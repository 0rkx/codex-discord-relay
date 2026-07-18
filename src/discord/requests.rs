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
/// capability advertisement. Receiving one is a protocol violation and gets
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
            Self::CurrentTimeRead => ServerRequestDisposition::HostHandled,
            Self::DynamicToolCall | Self::ChatgptAuthTokensRefresh | Self::AttestationGenerate => {
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

/// Decisions the Discord approval card can actually produce. The protocol
/// also accepts `cancel`, but no relay control emits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
}

/// Legacy decision vocabulary, likewise limited to what the card can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LegacyApprovalDecision {
    #[serde(rename = "approved")]
    Approved,
    #[serde(rename = "approved_for_session")]
    ApprovedForSession,
    #[serde(rename = "denied")]
    Denied,
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
        ServerRequestMethod::DynamicToolCall
        | ServerRequestMethod::ChatgptAuthTokensRefresh
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
        assert_eq!((interactive, host_handled, negotiated_off), (7, 1, 3));

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
        let reply = unsupported_reply(&request(json!("rpc-id"), "item/tool/call"));
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
