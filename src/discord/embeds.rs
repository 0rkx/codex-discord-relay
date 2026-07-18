use serde_json::Value;
use serenity::builder::CreateEmbed;
use serenity::model::Timestamp;

use crate::{
    codex::coverage::{CoverageReport, EXCLUDED_SERVER_REQUESTS},
    models::{TaskMirror, TaskState},
};

pub fn task_card(task: &TaskMirror, description: Option<&str>) -> CreateEmbed {
    let status = match task.state {
        TaskState::Running => "🔵 Running",
        TaskState::NeedsUser => "🟡 Needs you",
        TaskState::Done => "🟢 Done",
        TaskState::Failed => "🔴 Failed",
        TaskState::Idle => "⚪ Ready",
    };
    let mut embed = CreateEmbed::new()
        .title(truncate(&task.title, 256))
        .description(description.unwrap_or_else(|| status_copy(task.state)))
        .color(task.state.color())
        .field("Status", status, true)
        .field("Task", format!("`{}`", task.thread_id), true)
        .timestamp(task.last_event_at.map_or_else(Timestamp::now, |when| {
            Timestamp::from_unix_timestamp(when.timestamp()).unwrap_or_else(|_| Timestamp::now())
        }));
    if let Some(cwd) = &task.cwd {
        embed = embed.field(
            "Working directory",
            format!("`{}`", truncate(cwd, 980)),
            false,
        );
    }
    if let Some(model) = &task.model {
        embed = embed.field(
            "Model override",
            format!("`{}`", truncate(model, 900)),
            false,
        );
    }
    embed.footer(serenity::builder::CreateEmbedFooter::new(
        task.turn_id.as_ref().map_or_else(
            || "Codex Discord Relay".to_owned(),
            |turn| format!("Turn {turn}"),
        ),
    ))
}

pub fn codex_answer(
    title: &str,
    text: &str,
    state: TaskState,
    page: usize,
    pages: usize,
) -> CreateEmbed {
    CreateEmbed::new()
        .author(serenity::builder::CreateEmbedAuthor::new("Codex"))
        .title(truncate(title, 256))
        .description(if text.is_empty() {
            "_No text returned._"
        } else {
            text
        })
        .color(state.color())
        .timestamp(Timestamp::now())
        .footer(serenity::builder::CreateEmbedFooter::new(format!(
            "Page {page}/{pages}"
        )))
}

pub fn runner_card(
    connected: bool,
    version: Option<&str>,
    tasks: &[TaskMirror],
    god: bool,
    dead_outbox: u64,
) -> CreateEmbed {
    let count = |state| tasks.iter().filter(|task| task.state == state).count();
    let mut embed = CreateEmbed::new()
        .title("Codex runner")
        .description(if connected {
            "🟢 Connected to local Codex app-server"
        } else {
            "🔴 Local runner unavailable. Discord messages remain queued."
        })
        .color(if connected { 0x57F287 } else { 0xED4245 })
        .field("Version", version.unwrap_or("Unknown"), true)
        .field("GOD mode", if god { "🔴 ACTIVE" } else { "Off" }, true)
        .field(
            "Tasks",
            format!(
                "🔵 Running: {}\n🟡 Needs you: {}\n🟢 Done: {}\n🔴 Failed: {}",
                count(TaskState::Running),
                count(TaskState::NeedsUser),
                count(TaskState::Done),
                count(TaskState::Failed),
            ),
            false,
        )
        .timestamp(Timestamp::now());
    if dead_outbox > 0 {
        embed = embed.field(
            "Delivery",
            format!("⚠️ {dead_outbox} undeliverable message(s) parked in the outbox"),
            false,
        );
    }
    embed
}

pub fn approval_card(method: &str, params: &Value) -> CreateEmbed {
    let (title, icon) = match method {
        "item/commandExecution/requestApproval" => ("Command approval", "🖥️"),
        "item/fileChange/requestApproval" => ("File change approval", "📝"),
        "item/permissions/requestApproval" => ("Permission approval", "🔐"),
        "item/tool/requestUserInput" => ("Codex needs input", "❓"),
        "mcpServer/elicitation/request" => ("Connector needs input", "🔌"),
        _ => ("Action required", "⚠️"),
    };
    let description = if method == "item/commandExecution/requestApproval" {
        let command = params
            .get("command")
            .or_else(|| params.pointer("/item/command"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown command");
        format!("```powershell\n{}\n```", truncate(command, 3000))
    } else if method == "mcpServer/elicitation/request"
        && params.get("mode").and_then(Value::as_str) == Some("url")
    {
        let server = params
            .get("serverName")
            .and_then(Value::as_str)
            .unwrap_or("connector");
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Open the secure page, complete the request, then confirm here.");
        format!(
            "**Connector:** {}\n\n{}\n\nUse **Open secure page**, then choose **I completed it**, **Cancel**, or **Decline**.",
            truncate(server, 200),
            truncate(message, 3000)
        )
    } else {
        truncate(
            &serde_json::to_string_pretty(params).unwrap_or_default(),
            3800,
        )
    };
    CreateEmbed::new()
        .title(format!("{icon} {title}"))
        .description(description)
        .color(0xFEE75C)
        .timestamp(Timestamp::now())
        .footer(serenity::builder::CreateEmbedFooter::new(
            "Review before approving. GOD mode bypasses prompts while active.",
        ))
}

pub fn god_warning(minutes: u64, scope: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title("🔴 GOD MODE ACTIVE")
        .description(
            "Codex has unrestricted local-device access and approvals are bypassed. Commands can read, change, or delete files and control local processes.",
        )
        .color(0xED4245)
        .field("Scope", scope, true)
        .field("Expires", format!("In {minutes} minutes"), true)
        .timestamp(Timestamp::now())
        .footer(serenity::builder::CreateEmbedFooter::new("Use /god_off immediately when finished"))
}

pub fn error_card(title: &str, detail: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(format!("❌ {}", truncate(title, 250)))
        .description(truncate(detail, 4000))
        .color(0xED4245)
}

pub fn warning_card(title: &str, detail: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(format!("⚠️ {}", truncate(title, 250)))
        .description(truncate(detail, 4000))
        .color(0xFEE75C)
        .timestamp(Timestamp::now())
}

pub fn info_card(title: &str, detail: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(truncate(title, 256))
        .description(truncate(detail, 4000))
        .color(0x5865F2)
        .timestamp(Timestamp::now())
}

/// Render a `ThreadGoal` object from `thread/goal/get|set` responses.
pub fn goal_card(goal: &Value) -> CreateEmbed {
    let objective = goal
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or("(no objective text)");
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("active");
    let status_icon = match status {
        "complete" => "✅",
        "paused" => "⏸",
        "blocked" => "🚫",
        "usageLimited" | "budgetLimited" => "⛔",
        _ => "🎯",
    };
    let tokens_used = goal.get("tokensUsed").and_then(Value::as_i64).unwrap_or(0);
    let budget_line = goal.get("tokenBudget").and_then(Value::as_i64).map_or_else(
        || format!("{tokens_used} tokens used (no budget)"),
        |budget| format!("{tokens_used} of {budget} budgeted tokens used"),
    );
    let minutes_used = goal
        .get("timeUsedSeconds")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        / 60;
    CreateEmbed::new()
        .title(format!("{status_icon} Task goal"))
        .description(truncate(objective, 3800))
        .field("Status", status, true)
        .field("Budget", budget_line, true)
        .field("Time", format!("{minutes_used} min"), true)
        .color(0x5865F2)
        .timestamp(Timestamp::now())
}

/// One digest line for a turn from `thread/turns/list`.
#[must_use]
pub fn describe_turn(turn: &Value) -> String {
    let status = turn.get("status").and_then(Value::as_str).unwrap_or("?");
    let icon = match status {
        "completed" => "✅",
        "failed" => "❌",
        "interrupted" => "⏹",
        "inProgress" => "🔄",
        _ => "▫️",
    };
    let items = turn
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let started = turn
        .get("startedAt")
        .and_then(Value::as_i64)
        .map(|seconds| format!(" · started <t:{seconds}:R>"))
        .unwrap_or_default();
    let duration = turn
        .get("durationMs")
        .and_then(Value::as_i64)
        .map(|ms| format!(" · {}s", ms / 1000))
        .unwrap_or_default();
    let error = turn
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(|message| {
            format!(
                "\n   ⚠️ {}",
                truncate(message.lines().next().unwrap_or(""), 160)
            )
        })
        .unwrap_or_default();
    format!("{icon} {status} · {items} item(s){started}{duration}{error}")
}

/// Rolling digest of item lifecycle events for the current turn.
pub fn activity_card(lines: &[String]) -> CreateEmbed {
    let mut description = String::new();
    for line in lines {
        if description.len() + line.len() > 3600 {
            break;
        }
        description.push_str(line);
        description.push('\n');
    }
    CreateEmbed::new()
        .title("🧭 Turn activity")
        .description(if description.is_empty() {
            "_Waiting for the first action…_".to_owned()
        } else {
            description
        })
        .color(0x5865F2)
        .timestamp(Timestamp::now())
}

/// One coalesced card for command, MCP, and patch progress. Callers provide
/// already-redacted bounded summaries; this final clamp preserves Discord's
/// per-embed description ceiling even if several operations update together.
pub fn live_operations_card(summaries: &[String]) -> CreateEmbed {
    let description = if summaries.is_empty() {
        "_Waiting for operation output…_".to_owned()
    } else {
        truncate(&summaries.join("\n\n"), 4_000)
    };
    CreateEmbed::new()
        .title("⚙️ Live operations")
        .description(description)
        .color(0x0058_65F2)
        .timestamp(Timestamp::now())
}

/// Coalesced live transcript for one experimental Codex realtime session.
pub fn realtime_card(
    transcript: &str,
    active: bool,
    failed: bool,
    version: Option<&str>,
    close_reason: Option<&str>,
) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(if active {
            "🎙️ Realtime transcript · live"
        } else if failed {
            "🎙️ Realtime transcript · error"
        } else {
            "🎙️ Realtime transcript · closed"
        })
        .description(if transcript.is_empty() {
            "_Waiting for transcript…_".to_owned()
        } else {
            truncate(transcript, 4_000)
        })
        .color(if active {
            0x0058_65F2
        } else if failed {
            0x00ED_4245
        } else {
            0x0057_F287
        })
        .timestamp(Timestamp::now());
    if let Some(version) = version {
        embed = embed.field("Protocol", format!("`{}`", truncate(version, 30)), true);
    }
    if let Some(reason) = close_reason {
        embed = embed.field("Closed", truncate(reason, 300), false);
    }
    embed
}

/// Render the current turn plan from `turn/plan/updated` params.
pub fn plan_card(params: &Value) -> CreateEmbed {
    let mut description = String::new();
    if let Some(explanation) = params.get("explanation").and_then(Value::as_str) {
        description.push_str(&truncate(explanation, 500));
        description.push_str("\n\n");
    }
    for step in params
        .get("plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(30)
    {
        let icon = match step.get("status").and_then(Value::as_str) {
            Some("completed") => "✅",
            Some("inProgress") => "🔄",
            _ => "⬜",
        };
        let text = step.get("step").and_then(Value::as_str).unwrap_or("…");
        description.push_str(&format!("{icon} {}\n", truncate(text, 180)));
    }
    CreateEmbed::new()
        .title("🗺 Plan")
        .description(if description.is_empty() {
            "_No plan steps yet._".to_owned()
        } else {
            truncate(&description, 4000)
        })
        .color(0x5865F2)
        .timestamp(Timestamp::now())
}

/// One digest line for a thread item lifecycle event. Returns `None` for item
/// kinds already carried by the coalesced answer stream or plan card.
#[must_use]
pub fn describe_item(item: &Value, completed: bool) -> Option<String> {
    let kind = item.get("type").and_then(Value::as_str)?;
    match kind {
        // The live answer stream renders agent text; user messages are echoes
        // of the owner's own input; reasoning/plan have dedicated surfaces.
        "userMessage" | "agentMessage" | "reasoning" | "plan" | "hookPrompt" | "sleep" => None,
        "commandExecution" => {
            let command = item.get("command").and_then(Value::as_str).unwrap_or("?");
            let command = truncate(command.lines().next().unwrap_or("?"), 120);
            if completed {
                let exit = item.get("exitCode").and_then(Value::as_i64);
                let icon = match exit {
                    Some(0) => "✅",
                    Some(_) => "❌",
                    None => "⏹",
                };
                let duration = item
                    .get("durationMs")
                    .and_then(Value::as_i64)
                    .map(|ms| format!(" · {:.1}s", ms as f64 / 1000.0))
                    .unwrap_or_default();
                let exit_note = match exit {
                    Some(code) if code != 0 => format!(" · exit {code}"),
                    _ => String::new(),
                };
                Some(format!("{icon} `{command}`{exit_note}{duration}"))
            } else {
                Some(format!("🖥️ `{command}`"))
            }
        }
        "fileChange" => {
            if !completed {
                return None;
            }
            let paths: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .take(4)
                .collect();
            let summary = if paths.is_empty() {
                item.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("updated")
                    .to_owned()
            } else {
                paths.join(", ")
            };
            Some(format!("📝 {}", truncate(&summary, 200)))
        }
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let status = if completed {
                item.get("status").and_then(Value::as_str).unwrap_or("done")
            } else {
                "running"
            };
            Some(format!("🔌 {server}.{tool} · {status}"))
        }
        "dynamicToolCall" => {
            let namespace = item
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("host");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let status = if completed {
                item.get("status").and_then(Value::as_str).unwrap_or("done")
            } else {
                "running"
            };
            let icon = if completed && item.get("success").and_then(Value::as_bool) == Some(false) {
                "❌"
            } else {
                "🧰"
            };
            let duration = item
                .get("durationMs")
                .and_then(Value::as_i64)
                .map(|ms| format!(" · {:.1}s", ms as f64 / 1000.0))
                .unwrap_or_default();
            Some(format!("{icon} {namespace}.{tool} · {status}{duration}"))
        }
        "webSearch" => {
            if !completed {
                return None;
            }
            let query = item.get("query").and_then(Value::as_str).unwrap_or("web");
            Some(format!("🔎 {}", truncate(query, 160)))
        }
        "imageGeneration" | "imageView" => completed.then(|| "🖼 Image processed".to_owned()),
        "enteredReviewMode" => (!completed).then(|| "🧐 Code review started".to_owned()),
        "exitedReviewMode" => completed.then(|| "🧐 Code review finished".to_owned()),
        "contextCompaction" => completed.then(|| "🗜 Context compacted".to_owned()),
        other => completed.then(|| format!("🧩 {other}")),
    }
}

/// Measured coverage report for `/capabilities`.
pub fn coverage_card(report: &CoverageReport, schema_source: &str) -> CreateEmbed {
    let client = &report.client;
    let server = &report.server_requests;
    let notifications = &report.notifications;
    let mut exclusions = String::new();
    for (method, reason) in EXCLUDED_SERVER_REQUESTS {
        exclusions.push_str(&format!("`{method}` — {reason}\n"));
    }
    let mut embed = CreateEmbed::new()
        .title("📊 Codex app-server coverage (measured)")
        .description(format!(
            "Computed against {schema_source}. Dedicated Discord UX covers **{:.2}%** of user-launchable client methods. Generic schema forms are compatibility fallbacks and do not count as dedicated UX.",
            report.dedicated_percent()
        ))
        .color(if report.dedicated_percent() >= 99.0 {
            0x57F287
        } else {
            0xFEE75C
        })
        .field(
            format!("Client requests · {}", client.total),
            format!(
                "Dedicated UX: **{}**\nGeneric compatibility forms: **{}**\nExempt protocol mechanics: **{}**\nMissing route: **{}**",
                client.dedicated_ux,
                client.generic,
                client.exempt,
                client.missing.len()
            ),
            false,
        )
        .field(
            format!("Server requests · {}", server.total),
            format!(
                "Interactive cards: **{}**\nHost-handled: **{}**\nNegotiated off: **{}**\nUnrouted: **{}**",
                server.interactive,
                server.host_handled,
                server.negotiated_off,
                server.unknown.len()
            ),
            false,
        )
        .field(
            format!("Notifications · {}", notifications.total),
            format!(
                "Rendered in Discord: **{}**\nReasoned ignore: **{}**\nUnclassified: **{}**",
                notifications.rendered,
                notifications.reasoned_ignored,
                notifications.unknown.len()
            ),
            false,
        )
        .field("Measured exclusions", truncate(&exclusions, 1000), false)
        .timestamp(Timestamp::now());
    let mut drift = String::new();
    for method in server.unknown.iter().chain(&notifications.unknown).take(10) {
        drift.push_str(&format!("`{method}`\n"));
    }
    if !drift.is_empty() {
        embed = embed.field("⚠️ Schema drift (unrouted)", truncate(&drift, 1000), false);
    }
    if !client.generic_methods.is_empty() {
        let generic = client
            .generic_methods
            .iter()
            .take(20)
            .map(|method| format!("`{method}`"))
            .collect::<Vec<_>>()
            .join("\n");
        embed = embed.field(
            "Future methods using generic fallback (score zero)",
            truncate(&generic, 1000),
            false,
        );
    }
    embed
}

#[must_use]
pub fn split_answer(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_owned()];
    }
    let mut pages = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= limit {
            pages.push(remaining.to_owned());
            break;
        }
        let mut end = floor_char_boundary(remaining, limit);
        if let Some(newline) = remaining[..end].rfind('\n').filter(|at| *at > limit / 2) {
            end = newline;
        } else if let Some(space) = remaining[..end].rfind(' ').filter(|at| *at > limit / 2) {
            end = space;
        }
        pages.push(remaining[..end].trim_end().to_owned());
        remaining = remaining[end..].trim_start();
    }
    pages
}

fn floor_char_boundary(value: &str, requested: usize) -> usize {
    let mut boundary = requested.min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_owned()
    } else {
        let boundary = floor_char_boundary(value, max.saturating_sub(1));
        format!("{}…", &value[..boundary])
    }
}

const fn status_copy(state: TaskState) -> &'static str {
    match state {
        TaskState::Running => "Codex is working. Live updates appear here.",
        TaskState::NeedsUser => "Codex paused and needs your decision.",
        TaskState::Done => "Task completed.",
        TaskState::Failed => "Task stopped with an error.",
        TaskState::Idle => "Task is ready for your next message.",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn item_lines_cover_commands_files_and_tools() {
        let command = json!({
            "type": "commandExecution",
            "command": "cargo test --all-targets",
            "exitCode": 0,
            "durationMs": 2500
        });
        let line = describe_item(&command, true).unwrap();
        assert!(line.contains('✅'), "{line}");
        assert!(line.contains("cargo test"));
        assert!(line.contains("2.5s"));

        let failed = json!({"type": "commandExecution", "command": "cargo build", "exitCode": 101});
        let line = describe_item(&failed, true).unwrap();
        assert!(line.contains('❌'));
        assert!(line.contains("exit 101"));

        let file = json!({
            "type": "fileChange",
            "changes": [{"path": "src/main.rs"}, {"path": "src/lib.rs"}]
        });
        let line = describe_item(&file, true).unwrap();
        assert!(line.contains("src/main.rs"));

        let tool = json!({"type": "mcpToolCall", "server": "github", "tool": "search"});
        assert!(
            describe_item(&tool, true)
                .unwrap()
                .contains("github.search")
        );

        let dynamic = json!({
            "type": "dynamicToolCall",
            "namespace": "codex_app",
            "tool": "read_thread",
            "status": "completed",
            "success": true,
            "durationMs": 1250
        });
        let line = describe_item(&dynamic, true).unwrap();
        assert!(line.contains("codex_app.read_thread"));
        assert!(line.contains("completed"));
        assert!(line.contains("1.2s"));

        // Kinds carried by other surfaces stay out of the digest.
        assert!(describe_item(&json!({"type": "agentMessage", "text": "hi"}), true).is_none());
        assert!(describe_item(&json!({"type": "reasoning"}), true).is_none());

        // Unknown future kinds degrade to a generic line instead of vanishing.
        assert_eq!(
            describe_item(&json!({"type": "quantumTeleport"}), true).unwrap(),
            "🧩 quantumTeleport"
        );
    }

    #[test]
    fn plan_card_renders_step_icons() {
        let params = json!({
            "explanation": "Ship it",
            "plan": [
                {"step": "write code", "status": "completed"},
                {"step": "test", "status": "inProgress"},
                {"step": "docs", "status": "pending"}
            ],
            "threadId": "t",
            "turnId": "u"
        });
        // Rendering is exercised through the builder; ensure it does not panic
        // and the inputs above hit every status arm.
        let _ = plan_card(&params);
        let _ = activity_card(&["✅ `cargo test`".to_owned()]);
    }

    #[test]
    fn split_is_unicode_safe() {
        let text = "🦀 rust discord\n".repeat(500);
        let pages = split_answer(&text, 1000);
        assert!(pages.len() > 1);
        assert!(pages.iter().all(|page| page.len() <= 1000));
        assert_eq!(
            pages.concat().replace([' ', '\n'], ""),
            text.replace([' ', '\n'], "")
        );
    }

    #[test]
    fn turn_lines_render_status_timing_and_errors() {
        let completed = json!({
            "id": "turn-1",
            "status": "completed",
            "items": [{}, {}],
            "startedAt": 1_700_000_000,
            "durationMs": 12_500
        });
        let line = describe_turn(&completed);
        assert!(line.contains('✅'), "{line}");
        assert!(line.contains("2 item(s)"));
        assert!(line.contains("<t:1700000000:R>"));
        assert!(line.contains("12s"));

        let failed = json!({
            "id": "turn-2",
            "status": "failed",
            "items": [],
            "error": {"message": "sandbox denied"}
        });
        let line = describe_turn(&failed);
        assert!(line.contains('❌'), "{line}");
        assert!(line.contains("sandbox denied"));
    }

    #[test]
    fn goal_card_accepts_minimal_and_budgeted_goals() {
        let _ = goal_card(&json!({
            "objective": "Ship the relay",
            "status": "active",
            "tokensUsed": 1000,
            "tokenBudget": 250000,
            "timeUsedSeconds": 600
        }));
        let _ = goal_card(&json!({"objective": "No budget", "status": "complete"}));
    }
}
