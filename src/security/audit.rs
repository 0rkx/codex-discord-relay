use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex::Regex;
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
        Value::String(text) => Value::String(redact_secret_text(text)),
        other => other.clone(),
    }
}

fn redact_secret_text(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"(?i)\bsk-[A-Za-z0-9_-]{20,}\b",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            r"\b[A-Za-z0-9_-]{18,}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{20,}\b",
            r"(?i)\bBearer\s+[A-Za-z0-9._~-]{20,}",
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("built-in secret regex must compile"))
        .collect()
    });
    patterns.iter().fold(text.to_owned(), |value, pattern| {
        pattern.replace_all(&value, "[REDACTED]").into_owned()
    })
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
        .filter(char::is_ascii_alphanumeric)
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

    #[test]
    fn redacts_secret_shapes_inside_untrusted_output_strings() {
        let openai = format!("sk-{}", "A".repeat(32));
        let github = format!("ghp_{}", "B".repeat(36));
        let aws = format!("AKIA{}", "C".repeat(16));
        let jwt = format!(
            "eyJ{}.{}.{}",
            "d".repeat(20),
            "e".repeat(20),
            "f".repeat(20)
        );
        let discord = format!("{}.{}.{}", "1".repeat(18), "G".repeat(6), "h".repeat(30));
        let text = format!(
            "safe {openai} {github} {aws} {jwt} {discord} -----BEGIN PRIVATE KEY-----\nmaterial\n-----END PRIVATE KEY-----"
        );
        let clean = redact_secrets(&json!({"output": text}));
        let output = clean["output"].as_str().unwrap();
        assert!(output.starts_with("safe "));
        assert_eq!(output.matches("[REDACTED]").count(), 6);
        for secret in [openai, github, aws, jwt, discord] {
            assert!(!output.contains(&secret));
        }
        assert!(!output.contains("PRIVATE KEY"));
    }
}
