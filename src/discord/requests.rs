use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::codex::{RpcErrorObject, ServerRequest};

pub const CURRENT_SERVER_REQUEST_METHODS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
    "item/permissions/requestApproval",
    "item/tool/call",
    "account/chatgptAuthTokens/refresh",
    "attestation/generate",
    "currentTime/read",
    "applyPatchApproval",
    "execCommandApproval",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRequestMethod {
    CommandExecutionApproval,
    FileChangeApproval,
    ToolUserInput,
    McpElicitation,
    PermissionsApproval,
    DynamicToolCall,
    ChatgptAuthTokensRefresh,
    AttestationGenerate,
    CurrentTimeRead,
    LegacyApplyPatchApproval,
    LegacyExecCommandApproval,
    Unknown,
}

/// How each server-initiated request is handled by this host.
///
/// `NegotiatedOff` methods are deliberately absent from the initialize
/// capability advertisement. Dynamic tools are registered per thread instead
/// and routed through the Rust `HostBroker`. Any remaining disabled method gets
/// a fail-closed `-32601` response without echoing its parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRequestDisposition {
    Interactive,
    HostHandled,
    NegotiatedOff,
    Unknown,
}

impl ServerRequestMethod {
    #[must_use]
    pub fn classify(method: &str) -> Self {
        match method {
            "item/commandExecution/requestApproval" => Self::CommandExecutionApproval,
            "item/fileChange/requestApproval" => Self::FileChangeApproval,
            "item/tool/requestUserInput" => Self::ToolUserInput,
            "mcpServer/elicitation/request" => Self::McpElicitation,
            "item/permissions/requestApproval" => Self::PermissionsApproval,
            "item/tool/call" => Self::DynamicToolCall,
            "account/chatgptAuthTokens/refresh" => Self::ChatgptAuthTokensRefresh,
            "attestation/generate" => Self::AttestationGenerate,
            "currentTime/read" => Self::CurrentTimeRead,
            "applyPatchApproval" => Self::LegacyApplyPatchApproval,
            "execCommandApproval" => Self::LegacyExecCommandApproval,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn disposition(self) -> ServerRequestDisposition {
        match self {
            Self::CommandExecutionApproval
            | Self::FileChangeApproval
            | Self::ToolUserInput
            | Self::McpElicitation
            | Self::PermissionsApproval
            | Self::LegacyApplyPatchApproval
            | Self::LegacyExecCommandApproval => ServerRequestDisposition::Interactive,
            Self::CurrentTimeRead | Self::DynamicToolCall => ServerRequestDisposition::HostHandled,
            Self::ChatgptAuthTokensRefresh | Self::AttestationGenerate => {
                ServerRequestDisposition::NegotiatedOff
            }
            Self::Unknown => ServerRequestDisposition::Unknown,
        }
    }
}

/// A JSON-RPC response that keeps the app-server's request id in its original
/// JSON representation. In particular, numeric ids must not be converted to
/// strings when they pass through a Discord component custom id.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerReply {
    Result { id: Value, result: Value },
    Error { id: Value, error: RpcErrorObject },
}

impl ServerReply {
    #[must_use]
    pub fn result(request: &ServerRequest, result: Value) -> Self {
        Self::Result {
            id: request.id.clone(),
            result,
        }
    }

    #[must_use]
    pub fn error(request: &ServerRequest, error: RpcErrorObject) -> Self {
        Self::Error {
            id: request.id.clone(),
            error,
        }
    }
}

/// Modern app-server approval decision. Amendment payloads must be copied from
/// the request's server-offered `availableDecisions`; Discord text must never
/// be used to synthesize a policy rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
    AcceptWithExecpolicyAmendment(Vec<String>),
    ApplyNetworkPolicyAmendment(NetworkPolicyAmendment),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyApprovalDecision {
    Approved,
    ApprovedForSession,
    Denied,
    TimedOut,
    Abort,
    ApprovedExecpolicyAmendment(Vec<String>),
    NetworkPolicyAmendment(NetworkPolicyAmendment),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkPolicyAction {
    Allow,
    Deny,
}

impl Serialize for ApprovalDecision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Accept => serializer.serialize_str("accept"),
            Self::AcceptForSession => serializer.serialize_str("acceptForSession"),
            Self::Decline => serializer.serialize_str("decline"),
            Self::Cancel => serializer.serialize_str("cancel"),
            Self::AcceptWithExecpolicyAmendment(amendment) => json!({
                "acceptWithExecpolicyAmendment": {
                    "execpolicy_amendment": amendment
                }
            })
            .serialize(serializer),
            Self::ApplyNetworkPolicyAmendment(amendment) => json!({
                "applyNetworkPolicyAmendment": {
                    "network_policy_amendment": amendment
                }
            })
            .serialize(serializer),
        }
    }
}

impl Serialize for LegacyApprovalDecision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Approved => serializer.serialize_str("approved"),
            Self::ApprovedForSession => serializer.serialize_str("approved_for_session"),
            Self::Denied => serializer.serialize_str("denied"),
            Self::TimedOut => serializer.serialize_str("timed_out"),
            Self::Abort => serializer.serialize_str("abort"),
            Self::ApprovedExecpolicyAmendment(amendment) => json!({
                "approved_execpolicy_amendment": {
                    "proposed_execpolicy_amendment": amendment
                }
            })
            .serialize(serializer),
            Self::NetworkPolicyAmendment(amendment) => json!({
                "network_policy_amendment": {
                    "network_policy_amendment": amendment
                }
            })
            .serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionScope {
    Turn,
    Session,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionGrant {
    pub permissions: Map<String, Value>,
    pub scope: PermissionScope,
    pub strict_auto_review: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReplyBuildError {
    #[error("response builder does not match server request method {method}")]
    WrongMethod { method: String },
    #[error("accepted MCP elicitation requires structured content")]
    AcceptedElicitationMissingContent,
    #[error("declined or cancelled MCP elicitation must not include content")]
    RejectedElicitationHasContent,
    #[error("URL MCP elicitation responses must not include content")]
    UrlElicitationHasContent,
    #[error("approval decision index was not offered by the app-server")]
    DecisionNotOffered,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OfferedApprovalDecision {
    pub index: usize,
    pub label: &'static str,
    value: Value,
}

/// Parse only the installed modern decision union. Unknown future objects are
/// omitted instead of being echoed blindly. Original indexes are retained so
/// Discord can select the exact server-owned value without synthesizing it.
#[must_use]
pub fn offered_approval_decisions(request: &ServerRequest) -> Option<Vec<OfferedApprovalDecision>> {
    let values = request.params.get("availableDecisions")?.as_array()?;
    Some(
        values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                offered_decision_label(value).map(|label| OfferedApprovalDecision {
                    index,
                    label,
                    value: value.clone(),
                })
            })
            .collect(),
    )
}

pub fn approval_reply_from_offered(
    request: &ServerRequest,
    index: usize,
) -> Result<ServerReply, ReplyBuildError> {
    match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::CommandExecutionApproval | ServerRequestMethod::FileChangeApproval => {
        }
        _ => return Err(wrong_method(request)),
    }
    let decision = offered_approval_decisions(request)
        .and_then(|decisions| {
            decisions
                .into_iter()
                .find(|decision| decision.index == index)
        })
        .ok_or(ReplyBuildError::DecisionNotOffered)?;
    Ok(ServerReply::result(
        request,
        json!({"decision": decision.value}),
    ))
}

fn offered_decision_label(value: &Value) -> Option<&'static str> {
    match value {
        Value::String(value) => match value.as_str() {
            "accept" => Some("Approve once"),
            "acceptForSession" => Some("Approve session"),
            "decline" => Some("Decline"),
            "cancel" => Some("Cancel and stop"),
            _ => None,
        },
        Value::Object(object) if object.len() == 1 => {
            if let Some(amendment) = object.get("acceptWithExecpolicyAmendment") {
                let rules = amendment
                    .get("execpolicy_amendment")
                    .and_then(Value::as_array)?;
                rules
                    .iter()
                    .all(|rule| rule.as_str().is_some_and(|rule| rule.len() <= 4_096))
                    .then_some("Approve + command rule")
            } else if let Some(amendment) = object.get("applyNetworkPolicyAmendment") {
                let policy = amendment.get("network_policy_amendment")?;
                let host = policy.get("host").and_then(Value::as_str)?;
                let action = policy.get("action").and_then(Value::as_str)?;
                (!host.is_empty() && host.len() <= 255 && matches!(action, "allow" | "deny"))
                    .then_some("Apply network rule")
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn approval_reply(
    request: &ServerRequest,
    decision: ApprovalDecision,
) -> Result<ServerReply, ReplyBuildError> {
    match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::CommandExecutionApproval | ServerRequestMethod::FileChangeApproval => {
            Ok(ServerReply::result(request, json!({"decision": decision})))
        }
        _ => Err(wrong_method(request)),
    }
}

pub fn legacy_approval_reply(
    request: &ServerRequest,
    decision: LegacyApprovalDecision,
) -> Result<ServerReply, ReplyBuildError> {
    match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::LegacyApplyPatchApproval
        | ServerRequestMethod::LegacyExecCommandApproval => {
            Ok(ServerReply::result(request, json!({"decision": decision})))
        }
        _ => Err(wrong_method(request)),
    }
}

pub fn user_input_reply(
    request: &ServerRequest,
    answers: BTreeMap<String, Vec<String>>,
) -> Result<ServerReply, ReplyBuildError> {
    if ServerRequestMethod::classify(&request.method) != ServerRequestMethod::ToolUserInput {
        return Err(wrong_method(request));
    }
    let answers = answers
        .into_iter()
        .map(|(question_id, answers)| (question_id, json!({"answers": answers})))
        .collect::<Map<_, _>>();
    Ok(ServerReply::result(request, json!({"answers": answers})))
}

pub fn mcp_elicitation_reply(
    request: &ServerRequest,
    action: McpElicitationAction,
    content: Option<Value>,
) -> Result<ServerReply, ReplyBuildError> {
    if ServerRequestMethod::classify(&request.method) != ServerRequestMethod::McpElicitation {
        return Err(wrong_method(request));
    }
    let url_mode = request.params.get("mode").and_then(Value::as_str) == Some("url");
    match (url_mode, action, content.as_ref()) {
        (false, McpElicitationAction::Accept, None) => {
            return Err(ReplyBuildError::AcceptedElicitationMissingContent);
        }
        (true, McpElicitationAction::Accept, Some(_)) => {
            return Err(ReplyBuildError::UrlElicitationHasContent);
        }
        (_, McpElicitationAction::Decline | McpElicitationAction::Cancel, Some(_)) => {
            return Err(ReplyBuildError::RejectedElicitationHasContent);
        }
        _ => {}
    }
    let mut result = Map::from_iter([("action".to_owned(), json!(action))]);
    if let Some(content) = content {
        result.insert("content".to_owned(), content);
    }
    Ok(ServerReply::result(request, Value::Object(result)))
}

pub fn permissions_reply(
    request: &ServerRequest,
    grant: PermissionGrant,
) -> Result<ServerReply, ReplyBuildError> {
    if ServerRequestMethod::classify(&request.method) != ServerRequestMethod::PermissionsApproval {
        return Err(wrong_method(request));
    }
    let mut result = Map::from_iter([
        ("permissions".to_owned(), Value::Object(grant.permissions)),
        ("scope".to_owned(), json!(grant.scope)),
    ]);
    if let Some(strict_auto_review) = grant.strict_auto_review {
        result.insert("strictAutoReview".to_owned(), json!(strict_auto_review));
    }
    Ok(ServerReply::result(request, Value::Object(result)))
}

pub fn current_time_reply(
    request: &ServerRequest,
    unix_seconds: i64,
) -> Result<ServerReply, ReplyBuildError> {
    if ServerRequestMethod::classify(&request.method) != ServerRequestMethod::CurrentTimeRead {
        return Err(wrong_method(request));
    }
    Ok(ServerReply::result(
        request,
        json!({"currentTimeAt": unix_seconds}),
    ))
}

/// Return the only safe default for a server-initiated capability this relay
/// does not implement. Parameters are deliberately omitted because auth and
/// dynamic-tool requests may contain secrets.
#[must_use]
pub fn unsupported_reply(request: &ServerRequest) -> ServerReply {
    ServerReply::error(
        request,
        RpcErrorObject {
            code: -32601,
            message: format!(
                "server request method {} is not implemented by the Discord relay",
                request.method
            ),
            data: None,
        },
    )
}

/// Resolve requests that never need Discord UI. All other methods are returned
/// for the caller to render as an approval, question, or elicitation form.
#[must_use]
pub fn immediate_reply(request: &ServerRequest, unix_seconds: i64) -> Option<ServerReply> {
    match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::CurrentTimeRead => current_time_reply(request, unix_seconds).ok(),
        ServerRequestMethod::ChatgptAuthTokensRefresh
        | ServerRequestMethod::AttestationGenerate
        | ServerRequestMethod::Unknown => Some(unsupported_reply(request)),
        _ => None,
    }
}

fn wrong_method(request: &ServerRequest) -> ReplyBuildError {
    ReplyBuildError::WrongMethod {
        method: request.method.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: Value, method: &str) -> ServerRequest {
        ServerRequest {
            id,
            method: method.to_owned(),
            params: json!({"secret":"must-not-leak"}),
            generation: 1,
        }
    }

    #[test]
    fn classifies_every_method_in_installed_schema() {
        for method in CURRENT_SERVER_REQUEST_METHODS {
            assert_ne!(
                ServerRequestMethod::classify(method),
                ServerRequestMethod::Unknown,
                "{method} lacks a route"
            );
        }
    }

    #[test]
    fn every_server_request_has_an_explicit_host_disposition() {
        let mut interactive = 0;
        let mut host_handled = 0;
        let mut negotiated_off = 0;
        for method in CURRENT_SERVER_REQUEST_METHODS {
            match ServerRequestMethod::classify(method).disposition() {
                ServerRequestDisposition::Interactive => interactive += 1,
                ServerRequestDisposition::HostHandled => host_handled += 1,
                ServerRequestDisposition::NegotiatedOff => negotiated_off += 1,
                ServerRequestDisposition::Unknown => panic!("{method} has no disposition"),
            }
        }
        assert_eq!((interactive, host_handled, negotiated_off), (7, 2, 2));

        let advertised = crate::codex::advertised_client_capabilities();
        let serialized = advertised.to_string().to_ascii_lowercase();
        for forbidden in ["tool", "auth", "token", "attestation"] {
            assert!(
                !serialized.contains(forbidden),
                "negotiated-off capability leaked into initialize: {forbidden}"
            );
        }
    }

    #[test]
    fn modern_approval_has_schema_shape_and_preserves_numeric_id() {
        let reply = approval_reply(
            &request(json!(42), "item/commandExecution/requestApproval"),
            ApprovalDecision::AcceptForSession,
        )
        .unwrap();
        assert_eq!(
            reply,
            ServerReply::Result {
                id: json!(42),
                result: json!({"decision":"acceptForSession"})
            }
        );
    }

    #[test]
    fn legacy_approval_uses_legacy_decision_vocabulary() {
        let reply = legacy_approval_reply(
            &request(json!("opaque-id"), "execCommandApproval"),
            LegacyApprovalDecision::ApprovedForSession,
        )
        .unwrap();
        assert_eq!(
            reply,
            ServerReply::Result {
                id: json!("opaque-id"),
                result: json!({"decision":"approved_for_session"})
            }
        );
    }

    #[test]
    fn every_modern_approval_variant_has_the_exact_installed_shape() {
        let request = request(json!(1), "item/commandExecution/requestApproval");
        for (decision, expected) in [
            (ApprovalDecision::Accept, json!("accept")),
            (
                ApprovalDecision::AcceptForSession,
                json!("acceptForSession"),
            ),
            (ApprovalDecision::Decline, json!("decline")),
            (ApprovalDecision::Cancel, json!("cancel")),
            (
                ApprovalDecision::AcceptWithExecpolicyAmendment(vec!["allow command".to_owned()]),
                json!({
                    "acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": ["allow command"]
                    }
                }),
            ),
            (
                ApprovalDecision::ApplyNetworkPolicyAmendment(NetworkPolicyAmendment {
                    host: "example.test".to_owned(),
                    action: NetworkPolicyAction::Allow,
                }),
                json!({
                    "applyNetworkPolicyAmendment": {
                        "network_policy_amendment": {
                            "host": "example.test",
                            "action": "allow"
                        }
                    }
                }),
            ),
        ] {
            let ServerReply::Result { result, .. } = approval_reply(&request, decision).unwrap()
            else {
                panic!("modern approval must return a result")
            };
            assert_eq!(result["decision"], expected);
        }
    }

    #[test]
    fn offered_decisions_are_indexed_and_echoed_exactly() {
        let mut request = request(json!(7), "item/commandExecution/requestApproval");
        request.params = json!({
            "availableDecisions": [
                "accept",
                "decline",
                {"acceptWithExecpolicyAmendment": {
                    "execpolicy_amendment": ["allow git status"]
                }},
                {"applyNetworkPolicyAmendment": {
                    "network_policy_amendment": {
                        "host": "example.test",
                        "action": "allow"
                    }
                }}
            ]
        });
        let choices = offered_approval_decisions(&request).unwrap();
        assert_eq!(choices.len(), 4);
        assert_eq!(choices[2].index, 2);
        assert_eq!(choices[2].label, "Approve + command rule");
        assert_eq!(
            approval_reply_from_offered(&request, 2).unwrap(),
            ServerReply::Result {
                id: json!(7),
                result: json!({"decision": {
                    "acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": ["allow git status"]
                    }
                }})
            }
        );
        assert_eq!(
            approval_reply_from_offered(&request, 99),
            Err(ReplyBuildError::DecisionNotOffered)
        );
    }

    #[test]
    fn unknown_or_malformed_offered_decisions_fail_closed() {
        let mut request = request(json!(8), "item/commandExecution/requestApproval");
        request.params = json!({"availableDecisions": [
            "futureDecision",
            {"applyNetworkPolicyAmendment": {
                "network_policy_amendment": {"host":"example.test","action":"execute"}
            }},
            {"futureObject": {"secret":"do not echo"}}
        ]});
        assert!(offered_approval_decisions(&request).unwrap().is_empty());
        assert_eq!(
            approval_reply_from_offered(&request, 0),
            Err(ReplyBuildError::DecisionNotOffered)
        );
    }

    #[test]
    fn every_legacy_approval_variant_has_the_exact_installed_shape() {
        let request = request(json!(1), "execCommandApproval");
        for (decision, expected) in [
            (LegacyApprovalDecision::Approved, json!("approved")),
            (
                LegacyApprovalDecision::ApprovedForSession,
                json!("approved_for_session"),
            ),
            (LegacyApprovalDecision::Denied, json!("denied")),
            (LegacyApprovalDecision::TimedOut, json!("timed_out")),
            (LegacyApprovalDecision::Abort, json!("abort")),
            (
                LegacyApprovalDecision::ApprovedExecpolicyAmendment(vec![
                    "allow command".to_owned(),
                ]),
                json!({
                    "approved_execpolicy_amendment": {
                        "proposed_execpolicy_amendment": ["allow command"]
                    }
                }),
            ),
            (
                LegacyApprovalDecision::NetworkPolicyAmendment(NetworkPolicyAmendment {
                    host: "example.test".to_owned(),
                    action: NetworkPolicyAction::Deny,
                }),
                json!({
                    "network_policy_amendment": {
                        "network_policy_amendment": {
                            "host": "example.test",
                            "action": "deny"
                        }
                    }
                }),
            ),
        ] {
            let ServerReply::Result { result, .. } =
                legacy_approval_reply(&request, decision).unwrap()
            else {
                panic!("legacy approval must return a result")
            };
            assert_eq!(result["decision"], expected);
        }
    }

    #[test]
    fn user_input_maps_question_ids_to_answer_objects() {
        let answers = BTreeMap::from([
            ("cwd".to_owned(), vec!["C:/work".to_owned()]),
            (
                "flags".to_owned(),
                vec!["fast".to_owned(), "safe".to_owned()],
            ),
        ]);
        let reply =
            user_input_reply(&request(json!(7), "item/tool/requestUserInput"), answers).unwrap();
        assert_eq!(
            reply,
            ServerReply::Result {
                id: json!(7),
                result: json!({
                    "answers": {
                        "cwd": {"answers":["C:/work"]},
                        "flags": {"answers":["fast", "safe"]}
                    }
                })
            }
        );
    }

    #[test]
    fn mcp_elicitation_enforces_action_content_contract() {
        let form_request = request(json!("mcp-1"), "mcpServer/elicitation/request");
        assert_eq!(
            mcp_elicitation_reply(&form_request, McpElicitationAction::Accept, None),
            Err(ReplyBuildError::AcceptedElicitationMissingContent)
        );
        let mut url_request = request(json!("mcp-url"), "mcpServer/elicitation/request");
        url_request.params = json!({"mode":"url","url":"https://example.test/consent"});
        assert_eq!(
            mcp_elicitation_reply(&url_request, McpElicitationAction::Accept, None).unwrap(),
            ServerReply::Result {
                id: json!("mcp-url"),
                result: json!({"action":"accept"})
            }
        );
        assert_eq!(
            mcp_elicitation_reply(&url_request, McpElicitationAction::Cancel, None).unwrap(),
            ServerReply::Result {
                id: json!("mcp-url"),
                result: json!({"action":"cancel"})
            }
        );
        assert_eq!(
            mcp_elicitation_reply(
                &url_request,
                McpElicitationAction::Accept,
                Some(json!({"invalid":true}))
            ),
            Err(ReplyBuildError::UrlElicitationHasContent)
        );
        assert_eq!(
            mcp_elicitation_reply(
                &form_request,
                McpElicitationAction::Decline,
                Some(json!({"token":"no"}))
            ),
            Err(ReplyBuildError::RejectedElicitationHasContent)
        );
        assert_eq!(
            mcp_elicitation_reply(
                &form_request,
                McpElicitationAction::Accept,
                Some(json!({"region":"us"}))
            )
            .unwrap(),
            ServerReply::Result {
                id: json!("mcp-1"),
                result: json!({"action":"accept","content":{"region":"us"}})
            }
        );
    }

    #[test]
    fn permissions_and_current_time_follow_exact_camel_case_schema() {
        let permissions = permissions_reply(
            &request(json!(9), "item/permissions/requestApproval"),
            PermissionGrant {
                permissions: Map::from_iter([("network".to_owned(), json!({"enabled":true}))]),
                scope: PermissionScope::Session,
                strict_auto_review: Some(true),
            },
        )
        .unwrap();
        assert_eq!(
            permissions,
            ServerReply::Result {
                id: json!(9),
                result: json!({
                    "permissions":{"network":{"enabled":true}},
                    "scope":"session",
                    "strictAutoReview":true
                })
            }
        );

        assert_eq!(
            current_time_reply(&request(json!(10), "currentTime/read"), 1_700_000_000).unwrap(),
            ServerReply::Result {
                id: json!(10),
                result: json!({"currentTimeAt":1_700_000_000_i64})
            }
        );
    }

    #[test]
    fn unsupported_error_is_safe_and_preserves_string_id() {
        let reply = unsupported_reply(&request(json!("rpc-id"), "unknown/serverRequest"));
        let ServerReply::Error { id, error } = reply else {
            panic!("expected error reply")
        };
        assert_eq!(id, json!("rpc-id"));
        assert_eq!(error.code, -32601);
        assert!(error.data.is_none());
        assert!(!error.message.contains("must-not-leak"));
    }

    #[test]
    fn response_builder_rejects_wrong_method() {
        let request = request(json!(1), "currentTime/read");
        assert_eq!(
            approval_reply(&request, ApprovalDecision::Accept),
            Err(ReplyBuildError::WrongMethod {
                method: "currentTime/read".to_owned()
            })
        );
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_schema_requests_match_the_explicit_router() {
        let output = tempfile::tempdir().unwrap();
        let catalog = crate::codex::CapabilityCatalog::generate_and_load(
            &crate::codex::CodexCommand::discover().unwrap(),
            output.path(),
        )
        .await
        .unwrap();
        // The router may know about methods an older installed bundle no
        // longer sends (they stay dormant); what must never happen is an
        // installed server request the host has no explicit disposition for.
        let unrouted: Vec<_> = catalog
            .server_requests
            .iter()
            .filter(|method| {
                ServerRequestMethod::classify(method).disposition()
                    == ServerRequestDisposition::Unknown
            })
            .collect();
        assert!(
            unrouted.is_empty(),
            "installed server requests missing a disposition: {unrouted:?}"
        );
        let dormant: Vec<_> = CURRENT_SERVER_REQUEST_METHODS
            .iter()
            .filter(|method| !catalog.server_requests.contains(**method))
            .collect();
        println!(
            "installed server requests: {}; router extras not in this Codex build: {dormant:?}",
            catalog.server_requests.len()
        );
    }
}
