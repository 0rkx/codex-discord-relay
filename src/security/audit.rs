use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    AccessDenied,
    AuthenticationFailed,
    AuthenticationLocked,
    SessionStarted,
    SessionRevoked,
    SessionExpired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub action: AuditAction,
    pub occurred_at: DateTime<Utc>,
    pub owner_id: Option<u64>,
    pub guild_id: Option<u64>,
    pub channel_id: Option<u64>,
    pub task_id: Option<String>,
    /// A short non-capability prefix used for correlation, never the full session id.
    pub session_ref: Option<String>,
    pub detail: Value,
}

impl AuditEvent {
    #[must_use]
    pub fn redacted(mut self) -> Self {
        self.detail = redact_audit_value(self.detail, None);
        self
    }
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Persist one security event before privileged state is exposed.
    ///
    /// Implementations must return an error when durable persistence fails so
    /// GOD-mode activation can fail closed.
    async fn emit(&self, event: AuditEvent) -> anyhow::Result<()>;
}

#[derive(Clone, Default)]
pub struct InMemoryAuditSink {
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl InMemoryAuditSink {
    #[must_use]
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl AuditSink for InMemoryAuditSink {
    async fn emit(&self, event: AuditEvent) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.redacted());
        Ok(())
    }
}

#[must_use]
pub fn redact_detail(detail: Value) -> Value {
    redact_audit_value(detail, None)
}

#[must_use]
pub(crate) fn redact_secrets(value: &Value) -> Value {
    redact_secret_value(value, None)
}

fn redact_secret_value(value: &Value, parent_key: Option<&str>) -> Value {
    if parent_key.is_some_and(is_secret_key) {
        return Value::String("[REDACTED]".to_owned());
    }
    match value {
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let redacted = redact_secret_value(value, Some(key));
                    (key.clone(), redacted)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| redact_secret_value(value, parent_key))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn redact_audit_value(value: Value, parent_key: Option<&str>) -> Value {
    if parent_key.is_some_and(|key| is_secret_key(key) || is_audit_content_key(key)) {
        return Value::String("[REDACTED]".to_owned());
    }
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let redacted = redact_audit_value(value, Some(&key));
                    (key, redacted)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_audit_value(value, parent_key))
                .collect(),
        ),
        Value::String(value) if value.len() > 512 => {
            let prefix: String = value.chars().take(512).collect();
            Value::String(format!("{prefix}...[TRUNCATED]"))
        }
        other => other,
    }
}

fn is_secret_key(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "credential",
        "authorization",
        "cookie",
        "hash",
        "apikey",
    ]
    .iter()
    .any(|fragment| key.contains(fragment))
}

fn is_audit_content_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["content", "message", "body", "input"]
        .iter()
        .any(|fragment| key.contains(fragment))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn recursively_redacts_sensitive_detail() {
        let clean = redact_detail(json!({
            "password": "hunter2",
            "nested": {"api_token": "abc", "reason": "failed"},
            "api_key": "also-secret",
            "status": "denied"
        }));
        assert_eq!(clean["password"], "[REDACTED]");
        assert_eq!(clean["nested"]["api_token"], "[REDACTED]");
        assert_eq!(clean["api_key"], "[REDACTED]");
        assert_eq!(clean["nested"]["reason"], "failed");
    }

    #[test]
    fn preview_redaction_keeps_non_secret_content() {
        let clean = redact_secrets(&json!({
            "message": "safe preview",
            "nested": {"apiKey": "secret"}
        }));
        assert_eq!(clean["message"], "safe preview");
        assert_eq!(clean["nested"]["apiKey"], "[REDACTED]");
    }

    #[test]
    fn api_key_spelling_variants_share_one_policy() {
        for key in ["api_key", "api-key", "api key", "APIKey"] {
            let clean = redact_secrets(&json!({(key): "secret"}));
            assert_eq!(clean[key], "[REDACTED]", "variant {key}");
        }
    }
}
