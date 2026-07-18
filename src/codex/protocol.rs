use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type JsonObject = Map<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WireMessage {
    Response {
        id: Value,
        result: Result<Value, RpcErrorObject>,
    },
    Notification(Notification),
    ServerRequest(ServerRequest),
}

pub(crate) fn classify_message(value: &Value) -> Result<WireMessage, &'static str> {
    let object = value.as_object().ok_or("message is not an object")?;
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str);

    // JSON-RPC request IDs are strings or numbers. Rejecting null/boolean/
    // structured IDs here prevents an unreplyable server request from being
    // forwarded to Discord and makes response correlation deterministic.
    if id
        .as_ref()
        .is_some_and(|id| !matches!(id, Value::String(_) | Value::Number(_)))
    {
        return Err("invalid JSON-RPC request id");
    }

    if let Some(method) = method {
        if object.contains_key("result") || object.contains_key("error") {
            return Err("request/notification cannot contain response fields");
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        return Ok(match id {
            Some(id) => WireMessage::ServerRequest(ServerRequest {
                id,
                method: method.to_owned(),
                params,
            }),
            None => WireMessage::Notification(Notification {
                method: method.to_owned(),
                params,
            }),
        });
    }

    let id = id.ok_or("response has no id")?;
    if object.contains_key("error") && object.contains_key("result") {
        return Err("response cannot contain both result and error");
    }
    if let Some(error) = object.get("error") {
        let error = serde_json::from_value(error.clone()).map_err(|_| "invalid error object")?;
        return Ok(WireMessage::Response {
            id,
            result: Err(error),
        });
    }

    let result = object
        .get("result")
        .cloned()
        .ok_or("response has neither result nor error")?;
    Ok(WireMessage::Response {
        id,
        result: Ok(result),
    })
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_term: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kinds: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_turns: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_workspace_roots: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn_id: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIdParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNameSetParams {
    pub thread_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRollbackParams {
    pub thread_id: String,
    pub num_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum UserInput {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        text_elements: Vec<Value>,
    },
    #[serde(rename = "image")]
    Image {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename = "localImage")]
    LocalImage {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename = "skill")]
    Skill { name: String, path: String },
    #[serde(rename = "mention")]
    Mention { name: String, path: String },
}

impl UserInput {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            text_elements: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    pub expected_turn_id: String,
    pub input: Vec<UserInput>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

/// Target of a `review/start` request, mirroring the generated protocol schema.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ReviewTarget {
    #[serde(rename = "uncommittedChanges")]
    UncommittedChanges,
    #[serde(rename = "baseBranch")]
    BaseBranch { branch: String },
    #[serde(rename = "commit")]
    Commit {
        sha: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename = "custom")]
    Custom { instructions: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStartParams {
    pub thread_id: String,
    pub target: ReviewTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwds: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_reload: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_refetch: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_layers: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<bool>,
}

pub(crate) fn thread_id_from_params(params: &Value) -> Option<&str> {
    params
        .get("threadId")
        .and_then(Value::as_str)
        .or_else(|| params.get("conversationId").and_then(Value::as_str))
        .or_else(|| params.pointer("/thread/id").and_then(Value::as_str))
        .or_else(|| params.pointer("/turn/threadId").and_then(Value::as_str))
}

pub(crate) fn turn_id_from_params(params: &Value) -> Option<&str> {
    params
        .get("turnId")
        .and_then(Value::as_str)
        .or_else(|| params.pointer("/turn/id").and_then(Value::as_str))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoalSetParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnsListParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items_view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_direction: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearchParams {
    pub search_term: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalsListParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalTerminateParams {
    pub thread_id: String,
    pub process_id: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatusListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchParams {
    pub query: String,
    pub roots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation_token: Option<String>,
}

/// Safe subset of `thread/settings/update`.
///
/// `approvalPolicy`, `sandboxPolicy`, and `permissions` are deliberately not
/// representable here: those fields stay owned by the GOD-mode lifecycle
/// (`normal_thread_settings_params`) so no ordinary command can widen a
/// task's privileges.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettingsUpdateParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationModeSetting>,
}

/// A collaboration-mode preset applied to subsequent turns.
#[derive(Debug, Clone, Serialize)]
pub struct CollaborationModeSetting {
    pub mode: String,
    pub settings: CollaborationModeSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollaborationModeSettings {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_all_wire_message_shapes() {
        assert!(matches!(
            classify_message(&json!({"id": 1, "result": {"ok": true}})).unwrap(),
            WireMessage::Response { result: Ok(_), .. }
        ));
        assert!(matches!(
            classify_message(&json!({"method": "thread/started", "params": {}})).unwrap(),
            WireMessage::Notification(_)
        ));
        assert!(matches!(
            classify_message(
                &json!({"id": "server-1", "method": "item/tool/requestUserInput", "params": {}})
            )
            .unwrap(),
            WireMessage::ServerRequest(_)
        ));
        assert!(
            classify_message(&json!({
                "id": null,
                "method": "item/tool/requestUserInput",
                "params": {}
            }))
            .is_err()
        );
        assert!(classify_message(&json!({"id": 1, "result": {}, "error": {}})).is_err());
        assert!(classify_message(&json!({"id": 1, "method": "x", "result": {}})).is_err());
    }

    #[test]
    fn serializes_codex_camel_case_params() {
        let params = TurnSteerParams {
            thread_id: "thread-1".into(),
            expected_turn_id: "turn-1".into(),
            input: vec![UserInput::text("continue")],
            extra: BTreeMap::new(),
        };
        let value = serde_json::to_value(params).unwrap();
        assert_eq!(value["threadId"], "thread-1");
        assert_eq!(value["expectedTurnId"], "turn-1");
        assert_eq!(value["input"][0]["type"], "text");
    }

    #[test]
    fn review_targets_match_tagged_schema_shapes() {
        assert_eq!(
            serde_json::to_value(ReviewTarget::UncommittedChanges).unwrap(),
            json!({"type":"uncommittedChanges"})
        );
        assert_eq!(
            serde_json::to_value(ReviewTarget::BaseBranch {
                branch: "main".into()
            })
            .unwrap(),
            json!({"type":"baseBranch", "branch":"main"})
        );
    }

    #[test]
    fn goal_params_serialize_camel_case_with_optional_budget() {
        let params = ThreadGoalSetParams {
            thread_id: "thr-1".into(),
            objective: Some("Ship the relay".into()),
            status: None,
            token_budget: Some(250_000),
        };
        assert_eq!(
            serde_json::to_value(params).unwrap(),
            json!({"threadId":"thr-1","objective":"Ship the relay","tokenBudget":250_000})
        );
    }

    #[test]
    fn settings_update_keeps_collaboration_settings_snake_case() {
        let params = ThreadSettingsUpdateParams {
            thread_id: "thr-1".into(),
            effort: Some("high".into()),
            collaboration_mode: Some(CollaborationModeSetting {
                mode: "plan".into(),
                settings: CollaborationModeSettings {
                    model: "gpt-5.1-codex".into(),
                    reasoning_effort: Some("high".into()),
                    developer_instructions: None,
                },
            }),
        };
        assert_eq!(
            serde_json::to_value(params).unwrap(),
            json!({
                "threadId": "thr-1",
                "effort": "high",
                "collaborationMode": {
                    "mode": "plan",
                    "settings": {"model": "gpt-5.1-codex", "reasoning_effort": "high"}
                }
            })
        );
    }

    #[test]
    fn settings_update_cannot_express_privilege_fields() {
        // The safe-params contract: serializing every field must never emit
        // approvalPolicy, sandboxPolicy, or permissions keys.
        let params = ThreadSettingsUpdateParams {
            thread_id: "thr-1".into(),
            effort: Some("low".into()),
            collaboration_mode: Some(CollaborationModeSetting {
                mode: "default".into(),
                settings: CollaborationModeSettings {
                    model: "m".into(),
                    reasoning_effort: Some("low".into()),
                    developer_instructions: Some("x".into()),
                },
            }),
        };
        let value = serde_json::to_value(params).unwrap();
        let rendered = value.to_string();
        for forbidden in ["approvalPolicy", "sandboxPolicy", "permissions"] {
            assert!(!rendered.contains(forbidden), "{forbidden} leaked");
        }
    }

    #[test]
    fn search_and_file_search_params_match_wire_shapes() {
        assert_eq!(
            serde_json::to_value(ThreadSearchParams {
                search_term: "login bug".into(),
                archived: Some(false),
                cursor: None,
                limit: Some(10),
            })
            .unwrap(),
            json!({"searchTerm":"login bug","archived":false,"limit":10})
        );
        assert_eq!(
            serde_json::to_value(FuzzyFileSearchParams {
                query: "main.rs".into(),
                roots: vec!["C:/work".into()],
                cancellation_token: None,
            })
            .unwrap(),
            json!({"query":"main.rs","roots":["C:/work"]})
        );
    }

    #[test]
    fn extracts_thread_and_turn_identity_from_all_wire_shapes() {
        for params in [
            json!({"threadId": "thread-1"}),
            json!({"conversationId": "thread-1"}),
            json!({"thread": {"id": "thread-1"}}),
            json!({"turn": {"threadId": "thread-1"}}),
        ] {
            assert_eq!(thread_id_from_params(&params), Some("thread-1"));
        }
        assert_eq!(
            turn_id_from_params(&json!({"turnId": "turn-1"})),
            Some("turn-1")
        );
        assert_eq!(
            turn_id_from_params(&json!({"turn": {"id": "turn-1"}})),
            Some("turn-1")
        );
        assert_eq!(thread_id_from_params(&json!({"unrelated": true})), None);
        assert_eq!(turn_id_from_params(&json!({"unrelated": true})), None);
    }
}
