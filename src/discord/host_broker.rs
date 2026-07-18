//! Host-side implementations for Codex app dynamic tools.
//!
//! The app-server advertises dynamic tools per thread through
//! `thread/start.dynamicTools`, then invokes them with `item/tool/call` server
//! requests. This module intentionally exposes only the read-only operations it
//! can execute against the real app-server. Mutating tools belong here only
//! after they have owner approval, idempotency, and Discord state mirroring.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::time::timeout;

use crate::{
    codex::{
        CodexClient, DynamicToolNamespaceTool, DynamicToolSpec, RpcErrorObject, ServerRequest,
    },
    security::redact_secrets,
    state::StateStore,
};

use super::requests::ServerReply;

const DYNAMIC_TOOL_METHOD: &str = "item/tool/call";
const CODEX_APP_NAMESPACE: &str = "codex_app";
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_OUTPUT_BYTES: usize = 64 * 1_024;
const MAX_ARGUMENT_BYTES: usize = 64 * 1_024;
const MAX_IDENTIFIER_CHARS: usize = 256;
const MAX_CURSOR_CHARS: usize = 4_096;
const MAX_QUERY_CHARS: usize = 200;
const DEFAULT_LIST_LIMIT: u32 = 10;
const MAX_LIST_LIMIT: u32 = 50;
const DEFAULT_TURN_LIMIT: u32 = 5;
const MAX_TURN_LIMIT: u32 = 10;
const DEFAULT_ITEM_OUTPUT_CHARS: usize = 4_000;
const MAX_ITEM_OUTPUT_CHARS: usize = 20_000;

/// Executes the dynamic tools Relay advertises to Codex.
#[derive(Clone)]
pub struct HostBroker {
    codex: CodexClient,
    store: StateStore,
    call_timeout: Duration,
    max_output_bytes: usize,
}

impl HostBroker {
    #[must_use]
    pub fn new(codex: CodexClient, store: StateStore) -> Self {
        Self {
            codex,
            store,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            max_output_bytes: DEFAULT_OUTPUT_BYTES,
        }
    }

    /// Specs to place directly in `thread/start.dynamicTools`.
    #[must_use]
    pub fn dynamic_tool_specs() -> Vec<DynamicToolSpec> {
        vec![DynamicToolSpec::Namespace {
            name: CODEX_APP_NAMESPACE.to_owned(),
            description: "Read-only Codex task tools provided by Discord Relay.".to_owned(),
            tools: CodexAppTool::ALL.iter().map(|tool| tool.spec()).collect(),
        }]
    }

    /// Handle an `item/tool/call`, returning `None` for every other method.
    ///
    /// Protocol-envelope failures use JSON-RPC errors. Once a known tool has
    /// been selected, operational failures use the dynamic-tool response shape
    /// so Codex records a normal failed tool item instead of losing correlation.
    pub async fn handle_request(&self, request: &ServerRequest) -> Option<ServerReply> {
        if request.method != DYNAMIC_TOOL_METHOD {
            return None;
        }

        if serialized_len(&request.params) > MAX_ARGUMENT_BYTES {
            return Some(rpc_error(
                request,
                -32_602,
                "dynamic tool request exceeded Relay's argument limit",
            ));
        }

        let Ok(call) = serde_json::from_value::<DynamicToolCallParams>(request.params.clone())
        else {
            return Some(rpc_error(request, -32_602, "invalid dynamic tool request"));
        };
        if let Err(message) = call.validate_envelope() {
            return Some(rpc_error(request, -32_602, message));
        }
        if call.namespace.as_deref() != Some(CODEX_APP_NAMESPACE) {
            return Some(rpc_error(
                request,
                -32_601,
                "unknown dynamic tool namespace",
            ));
        }
        let Some(tool) = CodexAppTool::from_name(&call.tool) else {
            return Some(rpc_error(request, -32_601, "unknown dynamic tool"));
        };

        let origin = match self.store.task(&call.thread_id).await {
            Ok(Some(task)) => task,
            Ok(None) => {
                return Some(tool_result(
                    request,
                    DynamicToolCallResponse::failure("Calling Relay task was not found."),
                ));
            }
            Err(_) => {
                return Some(tool_result(
                    request,
                    DynamicToolCallResponse::failure("Relay task state is unavailable."),
                ));
            }
        };
        if origin.turn_id.as_deref() != Some(call.turn_id.as_str()) {
            let live_turn = timeout(
                self.call_timeout,
                live_active_turn_id(&self.codex, &call.thread_id),
            )
            .await;
            if !matches!(live_turn, Ok(Ok(Some(ref id))) if id == &call.turn_id) {
                return Some(tool_result(
                    request,
                    DynamicToolCallResponse::failure("This dynamic tool call is stale."),
                ));
            }
        }

        let target_thread_id = call
            .arguments
            .get("threadId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let operation = async {
            let value = tool
                .execute(&self.codex, &self.store, call.arguments)
                .await?;
            bounded_json_text(value, self.max_output_bytes).map_err(|()| ToolError::Output)
        };
        let response = match timeout(self.call_timeout, operation).await {
            Ok(Ok(text)) => DynamicToolCallResponse::success(text),
            Ok(Err(error)) => DynamicToolCallResponse::failure(error.message()),
            Err(_) => DynamicToolCallResponse::failure("Codex host operation timed out."),
        };
        let _ = self
            .store
            .audit(
                "dynamic_tool_call",
                None,
                None,
                origin.channel_id,
                Some(&call.thread_id),
                &json!({
                    "tool": tool.name(),
                    "targetThreadId": target_thread_id,
                    "success": response.success
                }),
            )
            .await;
        Some(tool_result(request, response))
    }
}

async fn live_active_turn_id(
    codex: &CodexClient,
    thread_id: &str,
) -> Result<Option<String>, ToolError> {
    let read = codex
        .request_value(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        )
        .await
        .map_err(|_| ToolError::Host)?;
    Ok(read
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns.iter().rev().find(|turn| {
                turn.get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| matches!(status, "inProgress" | "running" | "active"))
            })
        })
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexAppTool {
    ListThreads,
    ReadThread,
}

impl CodexAppTool {
    const ALL: [Self; 2] = [Self::ListThreads, Self::ReadThread];

    const fn name(self) -> &'static str {
        match self {
            Self::ListThreads => "list_threads",
            Self::ReadThread => "read_thread",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    fn spec(self) -> DynamicToolNamespaceTool {
        let (description, input_schema) = match self {
            Self::ListThreads => (
                "List recent Codex threads. Usually omit query; use it only to search older threads for a specific substring.",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Specific title substring to search for."
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_LIST_LIMIT,
                            "description": "Maximum number of thread summaries to return."
                        }
                    }
                }),
            ),
            Self::ReadThread => (
                "Read recent status and turn summaries for one Codex thread without opening it. Use the returned cursor to read older turns.",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "threadId": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Thread id to inspect."
                        },
                        "cursor": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Optional cursor for older turns."
                        },
                        "turnLimit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_TURN_LIMIT,
                            "description": "Maximum number of turns to return."
                        },
                        "includeOutputs": {
                            "type": "boolean",
                            "description": "Whether to include bounded tool and command outputs."
                        },
                        "maxOutputCharsPerItem": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": MAX_ITEM_OUTPUT_CHARS,
                            "description": "Maximum output characters retained per item."
                        }
                    },
                    "required": ["threadId"]
                }),
            ),
        };
        DynamicToolNamespaceTool::Function {
            name: self.name().to_owned(),
            description: description.to_owned(),
            input_schema,
            defer_loading: None,
        }
    }

    async fn execute(
        self,
        codex: &CodexClient,
        store: &StateStore,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        match self {
            Self::ListThreads => {
                let arguments = parse_list_arguments(arguments)?;
                let mut params = Map::from_iter([("limit".to_owned(), json!(arguments.limit))]);
                if let Some(query) = arguments.query {
                    params.insert("searchTerm".to_owned(), Value::String(query));
                }
                let raw = codex
                    .request_value("thread/list", Value::Object(params))
                    .await
                    .map_err(|_| ToolError::Host)?;
                Ok(sanitize_thread_list(&raw, arguments.limit))
            }
            Self::ReadThread => {
                let arguments = parse_read_arguments(arguments)?;
                let mirrored = store
                    .task(&arguments.thread_id)
                    .await
                    .map_err(|_| ToolError::Host)?
                    .is_some();
                let god_dirty = store
                    .god_dirty_tasks()
                    .await
                    .map_err(|_| ToolError::Host)?
                    .iter()
                    .any(|(thread_id, _)| thread_id == &arguments.thread_id);
                if !mirrored || god_dirty {
                    return Err(ToolError::Target);
                }
                let mut params = Map::from_iter([
                    ("threadId".to_owned(), json!(arguments.thread_id)),
                    ("limit".to_owned(), json!(arguments.turn_limit)),
                    ("sortDirection".to_owned(), json!("desc")),
                    (
                        "itemsView".to_owned(),
                        json!(if arguments.include_outputs {
                            "full"
                        } else {
                            "summary"
                        }),
                    ),
                ]);
                if let Some(cursor) = arguments.cursor {
                    params.insert("cursor".to_owned(), Value::String(cursor));
                }
                let raw = codex
                    .request_value("thread/turns/list", Value::Object(params))
                    .await
                    .map_err(|_| ToolError::Host)?;
                Ok(sanitize_turn_page(
                    &raw,
                    arguments.include_outputs,
                    arguments.max_output_chars_per_item,
                ))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DynamicToolCallParams {
    arguments: Value,
    call_id: String,
    namespace: Option<String>,
    thread_id: String,
    tool: String,
    turn_id: String,
}

impl DynamicToolCallParams {
    fn validate_envelope(&self) -> Result<(), &'static str> {
        for value in [
            self.call_id.as_str(),
            self.thread_id.as_str(),
            self.tool.as_str(),
            self.turn_id.as_str(),
        ] {
            if value.is_empty() || value.chars().count() > MAX_IDENTIFIER_CHARS {
                return Err("invalid dynamic tool identifier");
            }
        }
        if self
            .namespace
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.chars().count() > MAX_IDENTIFIER_CHARS)
        {
            return Err("invalid dynamic tool namespace");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListThreadsArguments {
    query: Option<String>,
    limit: Option<u32>,
}

struct ValidListThreadsArguments {
    query: Option<String>,
    limit: u32,
}

fn parse_list_arguments(arguments: Value) -> Result<ValidListThreadsArguments, ToolError> {
    let arguments: ListThreadsArguments =
        serde_json::from_value(arguments).map_err(|_| ToolError::Arguments)?;
    let query = arguments
        .query
        .map(|query| query.trim().to_owned())
        .filter(|query| !query.is_empty());
    if query
        .as_deref()
        .is_some_and(|query| query.chars().count() > MAX_QUERY_CHARS)
    {
        return Err(ToolError::Arguments);
    }
    let limit = arguments.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(ToolError::Arguments);
    }
    Ok(ValidListThreadsArguments { query, limit })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadThreadArguments {
    thread_id: String,
    cursor: Option<String>,
    turn_limit: Option<u32>,
    include_outputs: Option<bool>,
    max_output_chars_per_item: Option<usize>,
}

struct ValidReadThreadArguments {
    thread_id: String,
    cursor: Option<String>,
    turn_limit: u32,
    include_outputs: bool,
    max_output_chars_per_item: usize,
}

fn parse_read_arguments(arguments: Value) -> Result<ValidReadThreadArguments, ToolError> {
    let arguments: ReadThreadArguments =
        serde_json::from_value(arguments).map_err(|_| ToolError::Arguments)?;
    if arguments.thread_id.is_empty()
        || arguments.thread_id.chars().count() > MAX_IDENTIFIER_CHARS
        || arguments
            .cursor
            .as_deref()
            .is_some_and(|cursor| cursor.is_empty() || cursor.chars().count() > MAX_CURSOR_CHARS)
    {
        return Err(ToolError::Arguments);
    }
    let turn_limit = arguments.turn_limit.unwrap_or(DEFAULT_TURN_LIMIT);
    let max_output_chars_per_item = arguments
        .max_output_chars_per_item
        .unwrap_or(DEFAULT_ITEM_OUTPUT_CHARS);
    if !(1..=MAX_TURN_LIMIT).contains(&turn_limit)
        || max_output_chars_per_item > MAX_ITEM_OUTPUT_CHARS
    {
        return Err(ToolError::Arguments);
    }
    Ok(ValidReadThreadArguments {
        thread_id: arguments.thread_id,
        cursor: arguments.cursor,
        turn_limit,
        include_outputs: arguments.include_outputs.unwrap_or(false),
        max_output_chars_per_item,
    })
}

#[derive(Debug, PartialEq, Eq)]
enum ToolError {
    Arguments,
    Host,
    Output,
    Target,
}

impl ToolError {
    const fn message(&self) -> &'static str {
        match self {
            Self::Arguments => "Dynamic tool arguments were invalid.",
            Self::Host => "Codex host operation failed.",
            Self::Output => "Codex returned too much data.",
            Self::Target => "Target task is not available to Relay.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallResponse {
    pub success: bool,
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
}

impl DynamicToolCallResponse {
    fn success(text: String) -> Self {
        Self {
            success: true,
            content_items: vec![DynamicToolCallOutputContentItem::InputText { text }],
        }
    }

    fn failure(message: &str) -> Self {
        Self {
            success: false,
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: message.to_owned(),
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum DynamicToolCallOutputContentItem {
    #[serde(rename = "inputText")]
    InputText { text: String },
    #[serde(rename = "inputImage")]
    InputImage {
        #[serde(rename = "imageUrl")]
        image_url: String,
    },
}

fn rpc_error(request: &ServerRequest, code: i64, message: &str) -> ServerReply {
    ServerReply::Error {
        id: request.id.clone(),
        error: RpcErrorObject {
            code,
            message: message.to_owned(),
            data: None,
        },
    }
}

fn tool_result(request: &ServerRequest, response: DynamicToolCallResponse) -> ServerReply {
    ServerReply::Result {
        id: request.id.clone(),
        result: serde_json::to_value(response)
            .expect("DynamicToolCallResponse serialization is infallible"),
    }
}

fn sanitize_thread_list(raw: &Value, limit: u32) -> Value {
    let threads = raw
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit as usize)
        .map(|thread| {
            let mut summary = select_fields(
                thread,
                &[
                    "id",
                    "name",
                    "preview",
                    "status",
                    "createdAt",
                    "updatedAt",
                    "recencyAt",
                    "modelProvider",
                    "ephemeral",
                    "forkedFromId",
                    "parentThreadId",
                ],
            );
            clip_strings(&mut summary, 1_000);
            redact_secrets(&summary)
        })
        .collect::<Vec<_>>();
    json!({
        "threads": threads,
        "nextCursor": raw.get("nextCursor").cloned().unwrap_or(Value::Null)
    })
}

fn sanitize_turn_page(raw: &Value, include_outputs: bool, max_output_chars: usize) -> Value {
    let turns = raw
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|turn| {
            let mut turn = select_fields(
                turn,
                &[
                    "id",
                    "status",
                    "startedAt",
                    "completedAt",
                    "durationMs",
                    "error",
                    "itemsView",
                    "items",
                ],
            );
            strip_private_paths_and_outputs(&mut turn, include_outputs);
            clip_strings(&mut turn, max_output_chars);
            redact_secrets(&turn)
        })
        .collect::<Vec<_>>();
    json!({
        "turns": turns,
        "nextCursor": raw.get("nextCursor").cloned().unwrap_or(Value::Null),
        "backwardsCursor": raw.get("backwardsCursor").cloned().unwrap_or(Value::Null)
    })
}

fn select_fields(value: &Value, fields: &[&str]) -> Value {
    let mut selected = Map::new();
    if let Some(object) = value.as_object() {
        for field in fields {
            if let Some(value) = object.get(*field) {
                selected.insert((*field).to_owned(), value.clone());
            }
        }
    }
    Value::Object(selected)
}

fn strip_private_paths_and_outputs(value: &mut Value, include_outputs: bool) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| {
                let normalized = normalize_key(key);
                if ["cwd", "path", "savedpath", "sandboxpath", "rolloutpath"]
                    .contains(&normalized.as_str())
                {
                    return false;
                }
                include_outputs
                    || ![
                        "aggregatedoutput",
                        "stdout",
                        "stderr",
                        "output",
                        "rawoutput",
                        "result",
                        "contentitems",
                    ]
                    .contains(&normalized.as_str())
            });
            for child in object.values_mut() {
                strip_private_paths_and_outputs(child, include_outputs);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip_private_paths_and_outputs(child, include_outputs);
            }
        }
        _ => {}
    }
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn clip_strings(value: &mut Value, max_chars: usize) {
    match value {
        Value::String(text) if text.chars().count() > max_chars => {
            let prefix = text.chars().take(max_chars).collect::<String>();
            *text = format!("{prefix}...[TRUNCATED]");
        }
        Value::Array(values) => {
            for child in values {
                clip_strings(child, max_chars);
            }
        }
        Value::Object(object) => {
            for child in object.values_mut() {
                clip_strings(child, max_chars);
            }
        }
        _ => {}
    }
}

fn bounded_json_text(mut value: Value, max_bytes: usize) -> Result<String, ()> {
    if max_bytes < 64 {
        return Err(());
    }
    let mut truncated = false;
    loop {
        if truncated && let Value::Object(object) = &mut value {
            object.insert("truncated".to_owned(), Value::Bool(true));
        }
        let text = serde_json::to_string(&value).map_err(|_| ())?;
        if text.len() <= max_bytes {
            return Ok(text);
        }
        if !pop_largest_array(&mut value) {
            return Err(());
        }
        truncated = true;
    }
}

fn pop_largest_array(value: &mut Value) -> bool {
    fn locate(value: &Value, path: &mut Vec<PathPart>, best: &mut Option<(usize, Vec<PathPart>)>) {
        match value {
            Value::Array(values) => {
                if !values.is_empty() && best.as_ref().is_none_or(|(size, _)| values.len() > *size)
                {
                    *best = Some((values.len(), path.clone()));
                }
                for (index, child) in values.iter().enumerate() {
                    path.push(PathPart::Index(index));
                    locate(child, path, best);
                    path.pop();
                }
            }
            Value::Object(object) => {
                for (key, child) in object {
                    path.push(PathPart::Key(key.clone()));
                    locate(child, path, best);
                    path.pop();
                }
            }
            _ => {}
        }
    }

    let mut best = None;
    locate(value, &mut Vec::new(), &mut best);
    let Some((_, path)) = best else {
        return false;
    };
    let mut target = value;
    for part in path {
        match part {
            PathPart::Index(index) => {
                let Some(next) = target
                    .as_array_mut()
                    .and_then(|values| values.get_mut(index))
                else {
                    return false;
                };
                target = next;
            }
            PathPart::Key(key) => {
                let Some(next) = target
                    .as_object_mut()
                    .and_then(|object| object.get_mut(&key))
                else {
                    return false;
                };
                target = next;
            }
        }
    }
    target.as_array_mut().is_some_and(|values| {
        if values.is_empty() {
            return false;
        }
        values.truncate(values.len() / 2);
        true
    })
}

#[derive(Clone)]
enum PathPart {
    Index(usize),
    Key(String),
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use chrono::Utc;

    use crate::{
        codex::{
            CodexCommand, ThreadForkParams, ThreadResumeParams, ThreadStartParams, TurnStartParams,
            UserInput,
        },
        models::{TaskMirror, TaskState},
    };

    async fn run_live_tool_turn(
        client: &CodexClient,
        store: &StateStore,
        thread_id: &str,
        cwd: &str,
        prompt: String,
        expected_tool: &str,
    ) -> String {
        let mut requests = client.subscribe_server_requests();
        let mut notifications = client.subscribe_notifications();
        let turn = client
            .turn_start(TurnStartParams {
                thread_id: thread_id.to_owned(),
                input: vec![UserInput::text(prompt)],
                cwd: Some(cwd.to_owned()),
                model: Some("gpt-5.6-sol".to_owned()),
                approval_policy: Some("never".to_owned()),
                sandbox_policy: None,
                extra: BTreeMap::from([("effort".to_owned(), json!("medium"))]),
            })
            .await
            .unwrap();
        let turn_id = turn["turn"]["id"].as_str().unwrap().to_owned();
        store
            .upsert_task(&TaskMirror {
                thread_id: thread_id.to_owned(),
                channel_id: None,
                title: "HostBroker persistence test".to_owned(),
                cwd: Some(cwd.to_owned()),
                state: TaskState::Running,
                turn_id: Some(turn_id.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                last_event_at: Some(Utc::now()),
            })
            .await
            .unwrap();

        let request = timeout(Duration::from_secs(180), async {
            loop {
                let request = requests.recv().await.unwrap();
                if request.method == DYNAMIC_TOOL_METHOD {
                    break request;
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no item/tool/call for {expected_tool}; dynamicTools were not persisted or inherited"
            )
        });
        assert_eq!(request.params["threadId"], thread_id);
        assert_eq!(request.params["turnId"], turn_id);
        assert_eq!(request.params["namespace"], CODEX_APP_NAMESPACE);
        assert_eq!(request.params["tool"], expected_tool);
        let call_id = request.params["callId"].as_str().unwrap().to_owned();

        let broker = HostBroker::new(client.clone(), store.clone());
        let reply = timeout(Duration::from_secs(30), broker.handle_request(&request))
            .await
            .expect("nested HostBroker RPC deadlocked")
            .expect("HostBroker did not claim item/tool/call");
        let ServerReply::Result { id, result } = reply else {
            panic!("HostBroker rejected a valid inherited dynamic tool call")
        };
        assert_eq!(result["success"], true);
        client.respond_result(id, result).await.unwrap();

        timeout(Duration::from_secs(180), async {
            loop {
                let event = notifications.recv().await.unwrap();
                if event.method == "turn/completed"
                    && event.params["threadId"] == thread_id
                    && event.params["turn"]["id"] == turn_id
                {
                    break;
                }
            }
        })
        .await
        .expect("turn did not complete after inherited HostBroker response");
        call_id
    }

    #[test]
    fn advertised_tools_and_handlers_are_one_registry() {
        let specs = serde_json::to_value(HostBroker::dynamic_tool_specs()).unwrap();
        let tools = specs[0]["tools"].as_array().unwrap();
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["list_threads", "read_thread"]);
        assert_eq!(names.len(), CodexAppTool::ALL.len());
        assert!(
            names
                .iter()
                .all(|name| CodexAppTool::from_name(name).is_some())
        );
        assert_eq!(specs[0]["type"], "namespace");
        assert_eq!(specs[0]["name"], CODEX_APP_NAMESPACE);
    }

    #[test]
    fn schemas_are_closed_and_bounded() {
        let specs = serde_json::to_value(HostBroker::dynamic_tool_specs()).unwrap();
        for tool in specs[0]["tools"].as_array().unwrap() {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        assert_eq!(
            specs[0]["tools"][0]["inputSchema"]["properties"]["limit"]["maximum"],
            MAX_LIST_LIMIT
        );
        assert_eq!(
            specs[0]["tools"][1]["inputSchema"]["properties"]["turnLimit"]["maximum"],
            MAX_TURN_LIMIT
        );
    }

    #[test]
    fn argument_parsers_reject_unknown_and_unbounded_values() {
        assert!(parse_list_arguments(json!({"limit": 50})).is_ok());
        assert!(parse_list_arguments(json!({"limit": 51})).is_err());
        assert!(parse_list_arguments(json!({"extra": true})).is_err());
        assert!(parse_list_arguments(json!({"query": "x".repeat(MAX_QUERY_CHARS + 1)})).is_err());

        assert!(parse_read_arguments(json!({"threadId": "t", "turnLimit": 10})).is_ok());
        assert!(parse_read_arguments(json!({"threadId": "", "turnLimit": 1})).is_err());
        assert!(parse_read_arguments(json!({"threadId": "t", "turnLimit": 11})).is_err());
        assert!(parse_read_arguments(json!({"threadId": "t", "unknown": 1})).is_err());
    }

    #[test]
    fn typed_response_matches_installed_schema() {
        let response = DynamicToolCallResponse::success("{\"ok\":true}".to_owned());
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "success": true,
                "contentItems": [{"type": "inputText", "text": "{\"ok\":true}"}]
            })
        );
        let image = DynamicToolCallOutputContentItem::InputImage {
            image_url: "https://example.invalid/image.png".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(image).unwrap(),
            json!({"type": "inputImage", "imageUrl": "https://example.invalid/image.png"})
        );
    }

    #[test]
    fn thread_list_omits_device_paths_and_unneeded_history() {
        let clean = sanitize_thread_list(
            &json!({
                "data": [{
                    "id": "t1",
                    "name": "Task",
                    "cwd": "C:/private/device/path",
                    "path": "C:/private/rollout.jsonl",
                    "turns": [{"id": "turn"}],
                    "status": {"type": "idle"}
                }],
                "nextCursor": "next"
            }),
            10,
        );
        assert_eq!(clean["threads"][0]["id"], "t1");
        assert!(clean["threads"][0].get("cwd").is_none());
        assert!(clean["threads"][0].get("path").is_none());
        assert!(clean["threads"][0].get("turns").is_none());
        assert_eq!(clean["nextCursor"], "next");
    }

    #[test]
    fn turn_page_redacts_secret_keys_and_optional_outputs() {
        let raw = json!({
            "data": [{
                "id": "turn",
                "status": "completed",
                "items": [{
                    "type": "commandExecution",
                    "cwd": "C:/private",
                    "command": "echo safe",
                    "aggregatedOutput": "output",
                    "apiToken": "must-not-leak"
                }]
            }]
        });
        let without_outputs = sanitize_turn_page(&raw, false, 100);
        let item = &without_outputs["turns"][0]["items"][0];
        assert!(item.get("cwd").is_none());
        assert!(item.get("aggregatedOutput").is_none());
        assert_eq!(item["apiToken"], "[REDACTED]");

        let with_outputs = sanitize_turn_page(&raw, true, 100);
        assert_eq!(
            with_outputs["turns"][0]["items"][0]["aggregatedOutput"],
            "output"
        );
    }

    #[test]
    fn bounded_json_stays_valid_and_reports_truncation() {
        let text = bounded_json_text(
            json!({"threads": (0..100).map(|index| json!({"id": index, "name": "x".repeat(50)})).collect::<Vec<_>>() }),
            512,
        )
        .unwrap();
        assert!(text.len() <= 512);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["truncated"], true);
        assert!(value["threads"].as_array().unwrap().len() < 100);
    }

    #[test]
    fn envelope_validates_known_fields_and_tolerates_future_additions() {
        let call: DynamicToolCallParams = serde_json::from_value(json!({
            "arguments": {},
            "callId": "call",
            "namespace": "codex_app",
            "threadId": "thread",
            "tool": "list_threads",
            "turnId": "turn"
        }))
        .unwrap();
        assert!(call.validate_envelope().is_ok());
        assert!(
            serde_json::from_value::<DynamicToolCallParams>(json!({
                "arguments": {},
                "callId": "call",
                "namespace": "codex_app",
                "threadId": "thread",
                "tool": "list_threads",
                "turnId": "turn",
                "futureEnvelopeField": true
            }))
            .is_ok()
        );
    }

    #[tokio::test]
    async fn read_thread_requires_a_clean_relay_visible_target() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("relay.sqlite3"))
            .await
            .unwrap();
        let client = CodexClient::new(CodexCommand::new("unused-codex-test-binary"));
        let arguments = json!({"threadId": "target"});
        assert_eq!(
            CodexAppTool::ReadThread
                .execute(&client, &store, arguments.clone())
                .await
                .unwrap_err(),
            ToolError::Target
        );

        store
            .upsert_task(&TaskMirror {
                thread_id: "target".to_owned(),
                channel_id: Some(42),
                title: "Target".to_owned(),
                cwd: None,
                state: TaskState::Idle,
                turn_id: None,
                model: None,
                last_event_at: None,
            })
            .await
            .unwrap();
        store
            .mark_god_dirty("target", "test quarantine")
            .await
            .unwrap();
        assert_eq!(
            CodexAppTool::ReadThread
                .execute(&client, &store, arguments)
                .await
                .unwrap_err(),
            ToolError::Target
        );
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server and model access"]
    #[allow(clippy::too_many_lines)]
    async fn live_dynamic_tool_nested_rpc_round_trip() {
        let client = CodexClient::discover().unwrap();
        let mut requests = client.subscribe_server_requests();
        let mut notifications = client.subscribe_notifications();
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let thread = client
            .thread_start(ThreadStartParams {
                cwd: Some(cwd.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                approval_policy: Some("never".to_owned()),
                sandbox: Some(json!("read-only")),
                personality: None,
                developer_instructions: Some(
                    "This is a HostBroker protocol test. When explicitly asked, call the named dynamic tool exactly once before answering."
                        .to_owned(),
                ),
                runtime_workspace_roots: Some(vec![cwd.clone()]),
                dynamic_tools: Some(HostBroker::dynamic_tool_specs()),
                extra: BTreeMap::new(),
            })
            .await
            .unwrap();
        let thread_id = thread["thread"]["id"].as_str().unwrap().to_owned();
        let turn = client
            .turn_start(TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![UserInput::text(
                    "Call codex_app.list_threads exactly once with limit 1. You must use the dynamic tool. After it returns, reply only HOST_BROKER_OK.",
                )],
                cwd: Some(cwd.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                approval_policy: Some("never".to_owned()),
                sandbox_policy: None,
                extra: BTreeMap::from([("effort".to_owned(), json!("medium"))]),
            })
            .await
            .unwrap();
        let turn_id = turn["turn"]["id"].as_str().unwrap().to_owned();

        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("relay.sqlite3"))
            .await
            .unwrap();
        store
            .upsert_task(&TaskMirror {
                thread_id: thread_id.clone(),
                channel_id: None,
                title: "HostBroker live test".to_owned(),
                cwd: Some(cwd),
                state: TaskState::Running,
                turn_id: Some(turn_id.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                last_event_at: Some(Utc::now()),
            })
            .await
            .unwrap();
        let broker = HostBroker::new(client.clone(), store);

        let request = timeout(Duration::from_secs(180), async {
            loop {
                let request = requests.recv().await.unwrap();
                if request.method == DYNAMIC_TOOL_METHOD {
                    break request;
                }
            }
        })
        .await
        .expect("Codex did not issue item/tool/call for the advertised HostBroker tool");
        assert_eq!(request.params["threadId"], thread_id);
        assert_eq!(request.params["turnId"], turn_id);
        assert_eq!(request.params["namespace"], CODEX_APP_NAMESPACE);
        assert_eq!(request.params["tool"], "list_threads");
        assert_eq!(request.params["arguments"]["limit"], 1);
        assert!(
            request.params["callId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );

        let reply = timeout(Duration::from_secs(30), broker.handle_request(&request))
            .await
            .expect("nested thread/list deadlocked while app-server awaited the tool response")
            .expect("HostBroker did not claim item/tool/call");
        let ServerReply::Result { id, result } = reply else {
            panic!("HostBroker returned a JSON-RPC error for a valid tool call")
        };
        assert_eq!(id, request.id);
        assert_eq!(result["success"], true);
        let text = result["contentItems"][0]["text"].as_str().unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        assert!(
            value["threads"]
                .as_array()
                .is_some_and(|rows| rows.len() <= 1)
        );
        assert!(!text.contains("\"cwd\""));
        assert!(!text.contains("\"path\""));
        assert!(!text.contains("\"turns\""));
        client.respond_result(id, result).await.unwrap();

        timeout(Duration::from_secs(180), async {
            loop {
                let event = notifications.recv().await.unwrap();
                if event.method == "turn/completed"
                    && event.params["threadId"] == thread_id
                    && event.params["turn"]["id"] == turn_id
                {
                    break;
                }
            }
        })
        .await
        .expect("turn did not complete after the HostBroker response");

        client.thread_delete(thread_id).await.unwrap();
        client.close().await;
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server and model access"]
    async fn live_dynamic_tools_survive_cold_resume_and_fork() {
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("relay.sqlite3"))
            .await
            .unwrap();

        let first = CodexClient::discover().unwrap();
        let created = first
            .thread_start(ThreadStartParams {
                cwd: Some(cwd.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                approval_policy: Some("never".to_owned()),
                sandbox: Some(json!("read-only")),
                personality: None,
                developer_instructions: Some(
                    "When explicitly asked, call the named codex_app dynamic tool exactly once before answering."
                        .to_owned(),
                ),
                runtime_workspace_roots: Some(vec![cwd.clone()]),
                dynamic_tools: Some(HostBroker::dynamic_tool_specs()),
                extra: BTreeMap::new(),
            })
            .await
            .unwrap();
        let original_id = created["thread"]["id"].as_str().unwrap().to_owned();
        let initial_call = run_live_tool_turn(
            &first,
            &store,
            &original_id,
            &cwd,
            "Call codex_app.list_threads exactly once with limit 1, then reply only INITIAL_OK."
                .to_owned(),
            "list_threads",
        )
        .await;
        first.close().await;

        let resumed_client = CodexClient::discover().unwrap();
        let resumed = resumed_client
            .thread_resume(ThreadResumeParams {
                thread_id: original_id.clone(),
                cwd: Some(cwd.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                approval_policy: Some("never".to_owned()),
                sandbox: Some(json!("read-only")),
                extra: BTreeMap::new(),
            })
            .await
            .unwrap();
        assert_eq!(resumed["thread"]["id"], original_id);
        let resumed_call = run_live_tool_turn(
            &resumed_client,
            &store,
            &original_id,
            &cwd,
            format!(
                "Call codex_app.read_thread exactly once with threadId {original_id}, then reply only RESUME_OK."
            ),
            "read_thread",
        )
        .await;

        let forked = resumed_client
            .thread_fork(ThreadForkParams {
                thread_id: original_id.clone(),
                cwd: Some(cwd.clone()),
                model: Some("gpt-5.6-sol".to_owned()),
                approval_policy: Some("never".to_owned()),
                sandbox: Some(json!("read-only")),
                last_turn_id: None,
                extra: BTreeMap::new(),
            })
            .await
            .unwrap();
        let child_id = forked["thread"]["id"].as_str().unwrap().to_owned();
        assert_ne!(child_id, original_id);
        let forked_call = run_live_tool_turn(
            &resumed_client,
            &store,
            &child_id,
            &cwd,
            "Call codex_app.list_threads exactly once with limit 1, then reply only FORK_OK."
                .to_owned(),
            "list_threads",
        )
        .await;

        assert_ne!(initial_call, resumed_call);
        assert_ne!(initial_call, forked_call);
        assert_ne!(resumed_call, forked_call);
        resumed_client.thread_delete(child_id).await.unwrap();
        resumed_client.thread_delete(original_id).await.unwrap();
        resumed_client.close().await;
    }
}
