use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::Utc;
use secrecy::SecretString;
use serde_json::{Value, json};
use serenity::{
    all::{
        ActionRowComponent, ActivityData, Attachment, ChannelId, CommandInteraction,
        CommandOptionType, ComponentInteraction, ComponentInteractionDataKind, Context,
        CreateCommand, CreateCommandOption, EventHandler, GatewayIntents, GuildId, Interaction,
        Message, ModalInteraction, Ready, UserId,
    },
    builder::{
        CreateActionRow, CreateAllowedMentions, CreateAttachment, CreateAutocompleteResponse,
        CreateCommand as CommandBuilder, CreateEmbed, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateMessage, CreateModal, EditInteractionResponse,
        EditMessage,
    },
    http::{Http, HttpError},
};
use tokio::{
    io::AsyncReadExt as _,
    sync::{Mutex, RwLock},
};
use tracing::{error, info, warn};
use zeroize::Zeroizing;

use crate::{
    codex::{
        AccountReadParams, AppListParams, BackgroundTerminalTerminateParams,
        BackgroundTerminalsListParams, CapabilityCatalog, CodexClient, CodexCommand,
        CollaborationModeSetting, CollaborationModeSettings, ConfigReadParams,
        FuzzyFileSearchParams, McpServerStatusListParams, ModelListParams, Notification,
        PluginInstalledParams, PluginListParams, PluginLocatorParams, ReviewStartParams,
        ReviewTarget, RpcErrorObject, ServerRequest, SkillsListParams, ThreadForkParams,
        ThreadGoalSetParams, ThreadListParams, ThreadSearchParams, ThreadSettingsUpdateParams,
        ThreadStartParams, ThreadTurnsListParams, TurnInterruptParams, TurnStartParams,
        TurnSteerParams, UserInput, coverage::CoverageReport,
    },
    config::{self, Config},
    discord::{
        actions::{self, ActionAuthorization, ActionDraft, ActionSurface},
        components, embeds,
        host_broker::HostBroker,
        notifications,
        projection::{CompletionState, OutputChunk, OutputStream, PatchState, ProjectionCore},
        provision::{self, Layout},
        requests::{
            self, ApprovalDecision, LegacyApprovalDecision, McpElicitationAction, PermissionGrant,
            PermissionScope, ServerReply, ServerRequestMethod,
        },
    },
    models::{TaskMirror, TaskState},
    security::{
        Argon2idPasswordManager, AuditEvent, AuditSink, GodModeService, PasswordSubmission,
        SecurityContext, SessionScope, StoredPasswordHash, SystemClock, TrustBoundary,
    },
    state::StateStore,
};

const RELAY_DEFAULT_MODEL: &str = "gpt-5.6-sol";
const RELAY_DEFAULT_REASONING_EFFORT: &str = "medium";
const RELAY_DEVELOPER_INSTRUCTIONS: &str = "Work as the installed Codex agent, not as a chat-only assistant. Inspect available apps, plugins, MCP servers, and tools before claiming a capability is unavailable. For current or public facts, search the web and open an authoritative source before concluding. Continue through relevant tool calls and verification until the requested outcome is complete or a genuine user/auth/external blocker remains. Never fabricate tool use, connector success, citations, or external actions.";

#[derive(Default)]
struct StreamBuffer {
    segments: Vec<StreamSegment>,
    message_id: Option<u64>,
    dirty: bool,
    final_state: Option<TaskState>,
}

/// One agent message item within a turn. Deltas accumulate per item so
/// separate messages never merge into one blob, and `item/completed` carries
/// the authoritative full text, healing any dropped or duplicated deltas.
struct StreamSegment {
    item_id: String,
    text: String,
    completed: bool,
}

impl StreamBuffer {
    fn append_delta(&mut self, item_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        match self
            .segments
            .iter_mut()
            .find(|segment| segment.item_id == item_id)
        {
            // The authoritative full text already arrived; a late or replayed
            // delta must not corrupt it.
            Some(segment) if segment.completed => {}
            Some(segment) => {
                segment.text.push_str(delta);
                self.dirty = true;
            }
            None => {
                self.segments.push(StreamSegment {
                    item_id: item_id.to_owned(),
                    text: delta.to_owned(),
                    completed: false,
                });
                self.dirty = true;
            }
        }
    }

    fn complete_item(&mut self, item_id: &str, text: &str) {
        if let Some(segment) = self
            .segments
            .iter_mut()
            .find(|segment| segment.item_id == item_id)
        {
            if segment.text != text {
                segment.text = text.to_owned();
                self.dirty = true;
            }
            segment.completed = true;
            return;
        }
        // Older Codex builds omit `itemId` on deltas; those accumulate under
        // an anonymous segment that the first completion adopts.
        if let Some(segment) = self
            .segments
            .iter_mut()
            .find(|segment| segment.item_id.is_empty() && !segment.completed)
        {
            segment.item_id = item_id.to_owned();
            segment.text = text.to_owned();
            segment.completed = true;
        } else {
            self.segments.push(StreamSegment {
                item_id: item_id.to_owned(),
                text: text.to_owned(),
                completed: true,
            });
        }
        self.dirty = true;
    }

    /// Render the answer: agent messages in arrival order, blank-line
    /// separated, skipping empty items.
    fn text(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            if segment.text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&segment.text);
        }
        out
    }
}

#[derive(Default)]
struct ActivityDigest {
    message_id: Option<u64>,
    lines: Vec<String>,
    dirty: bool,
}

#[derive(Default)]
struct PlanBoard {
    message_id: Option<u64>,
    params: Value,
    dirty: bool,
}

const MAX_REALTIME_TRANSCRIPT_CHARS: usize = 12_000;
// Discord's default per-file ceiling is 10 MiB. Keep room for multipart
// framing and the 44-byte WAV header instead of assuming a boosted guild.
const MAX_DISCORD_ATTACHMENT_BYTES: usize = 9 * 1024 * 1024;
const MAX_REALTIME_AUDIO_BYTES: usize = MAX_DISCORD_ATTACHMENT_BYTES - 44;
const MAX_REALTIME_AUDIO_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_REALTIME_AUDIO_CHUNK_BASE64: usize = MAX_REALTIME_AUDIO_CHUNK_BYTES.div_ceil(3) * 4;
const PENDING_REQUEST_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct RealtimeDigest {
    message_id: Option<u64>,
    segments: Vec<(String, String)>,
    partial_role: Option<String>,
    partial: String,
    version: Option<String>,
    close_reason: Option<String>,
    active: bool,
    failed: bool,
    dirty: bool,
    audio: Vec<u8>,
    audio_sample_rate: Option<u32>,
    audio_channels: Option<u16>,
    audio_truncated: bool,
    event_count: u32,
    last_item_kind: Option<String>,
    webrtc_ready: bool,
}

#[derive(Default)]
struct OperationsDigest {
    message_id: Option<u64>,
    entries: Vec<(String, String)>,
    dirty: bool,
}

impl OperationsDigest {
    fn upsert(&mut self, item_id: &str, summary: String) {
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(existing_id, _)| existing_id == item_id)
        {
            if *existing != summary {
                *existing = summary;
                self.dirty = true;
            }
            return;
        }
        self.entries.push((item_id.to_owned(), summary));
        if self.entries.len() > 6 {
            self.entries.remove(0);
        }
        self.dirty = true;
    }

    fn summaries(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|(_, summary)| summary.clone())
            .collect()
    }
}

impl RealtimeDigest {
    fn start(&mut self, version: Option<&str>) {
        self.segments.clear();
        self.partial.clear();
        self.partial_role = None;
        self.audio.clear();
        self.audio_sample_rate = None;
        self.audio_channels = None;
        self.audio_truncated = false;
        self.event_count = 0;
        self.last_item_kind = None;
        self.webrtc_ready = false;
        self.active = true;
        self.failed = false;
        self.version = version.map(str::to_owned);
        self.close_reason = None;
        self.dirty = true;
    }

    fn append_transcript(&mut self, role: &str, delta: &str) {
        if !self.active {
            return;
        }
        if self.partial_role.as_deref() != Some(role) {
            if !self.partial.is_empty() {
                let previous_role = self.partial_role.as_deref().unwrap_or("unknown").to_owned();
                let previous_text = std::mem::take(&mut self.partial);
                self.finish_transcript(previous_role, previous_text);
            }
            self.partial_role = Some(role.to_owned());
        }
        append_bounded(&mut self.partial, delta, MAX_REALTIME_TRANSCRIPT_CHARS);
        self.dirty = true;
    }

    fn finish_transcript(&mut self, role: String, text: String) {
        let text = truncate_chars(&text, 4_000);
        if !text.is_empty() {
            let replace = self
                .segments
                .iter_mut()
                .rev()
                .find(|(existing_role, existing_text)| {
                    existing_role == &role
                        && (text.starts_with(existing_text.as_str())
                            || existing_text.starts_with(text.as_str()))
                });
            if let Some((_, existing_text)) = replace {
                *existing_text = text;
            } else {
                self.segments.push((role.clone(), text));
            }
            if self.segments.len() > 20 {
                self.segments.remove(0);
            }
        }
        if self.partial_role.as_deref() == Some(role.as_str()) {
            self.partial.clear();
            self.partial_role = None;
        }
        self.dirty = true;
    }

    fn close(&mut self, reason: Option<&str>) {
        if !self.partial.is_empty() {
            let role = self.partial_role.as_deref().unwrap_or("unknown").to_owned();
            let text = std::mem::take(&mut self.partial);
            self.finish_transcript(role, text);
        }
        self.active = false;
        if let Some(reason) = reason {
            self.close_reason = Some(truncate_chars(reason, 300));
        }
        self.dirty = true;
    }

    fn note_item(&mut self, item: &Value) {
        if !self.active {
            return;
        }
        self.event_count = self.event_count.saturating_add(1);
        self.last_item_kind = Some(
            match item.get("type").and_then(Value::as_str) {
                Some("message") => "message",
                Some("response") => "response",
                Some("conversation") => "conversation",
                Some("function_call") => "function call",
                Some("function_call_output") => "function result",
                Some("tool_call") => "tool call",
                Some("tool_result") => "tool result",
                _ => "event",
            }
            .to_owned(),
        );
        self.dirty = true;
    }

    fn note_sdp(&mut self) {
        if !self.active {
            return;
        }
        self.webrtc_ready = true;
        self.dirty = true;
    }

    fn append_audio(&mut self, params: &Value) {
        if !self.active || self.audio_truncated {
            return;
        }
        let Some(audio) = params.get("audio") else {
            return;
        };
        let Some(data) = audio.get("data").and_then(Value::as_str) else {
            return;
        };
        let sample_rate = audio
            .get("sampleRate")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        let channels = audio
            .get("numChannels")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok());
        let samples_per_channel = audio
            .get("samplesPerChannel")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        if data.len() > MAX_REALTIME_AUDIO_CHUNK_BASE64
            || !sample_rate.is_some_and(|value| (8_000..=96_000).contains(&value))
            || !channels.is_some_and(|value| (1..=2).contains(&value))
            || self
                .audio_sample_rate
                .is_some_and(|value| Some(value) != sample_rate)
            || self
                .audio_channels
                .is_some_and(|value| Some(value) != channels)
        {
            self.audio_truncated = true;
            return;
        }
        let Ok(bytes) = BASE64_STANDARD.decode(data) else {
            self.audio_truncated = true;
            return;
        };
        let Some(block_align) = channels.map(|value| usize::from(value) * 2) else {
            self.audio_truncated = true;
            return;
        };
        let declared_len = samples_per_channel.and_then(|samples| samples.checked_mul(block_align));
        if bytes.len() > MAX_REALTIME_AUDIO_CHUNK_BYTES
            || bytes.len() % block_align != 0
            || declared_len.is_some_and(|expected| expected != bytes.len())
        {
            self.audio_truncated = true;
            return;
        }
        if self.audio.len().saturating_add(bytes.len()) > MAX_REALTIME_AUDIO_BYTES {
            self.audio_truncated = true;
            return;
        }
        self.audio_sample_rate = sample_rate;
        self.audio_channels = channels;
        self.audio.extend_from_slice(&bytes);
    }

    fn transcript_text(&self) -> String {
        let mut text = String::new();
        for (role, segment) in &self.segments {
            append_transcript_segment(&mut text, role, segment);
        }
        if !self.partial.is_empty() {
            append_transcript_segment(
                &mut text,
                self.partial_role.as_deref().unwrap_or("live"),
                &self.partial,
            );
        }
        truncate_chars(&text, 3_900)
    }

    fn take_wav(&mut self) -> Option<(Vec<u8>, bool)> {
        let sample_rate = self.audio_sample_rate.take()?;
        let channels = self.audio_channels.take()?;
        if self.audio.is_empty() {
            return None;
        }
        let pcm = std::mem::take(&mut self.audio);
        let truncated = std::mem::take(&mut self.audio_truncated);
        build_pcm16_wav(pcm, sample_rate, channels).map(|wav| (wav, truncated))
    }
}

fn append_bounded(target: &mut String, delta: &str, max_chars: usize) {
    let remaining = max_chars.saturating_sub(target.chars().count());
    target.extend(delta.chars().take(remaining));
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_owned()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn append_transcript_segment(target: &mut String, role: &str, text: &str) {
    if !target.is_empty() {
        target.push_str("\n\n");
    }
    target.push_str("**");
    target.push_str(&truncate_chars(role, 40));
    target.push_str(":** ");
    target.push_str(text);
}

fn build_pcm16_wav(pcm: Vec<u8>, sample_rate: u32, channels: u16) -> Option<Vec<u8>> {
    let data_len = u32::try_from(pcm.len()).ok()?;
    let block_align = channels.checked_mul(2)?;
    let byte_rate = sample_rate.checked_mul(u32::from(block_align))?;
    let riff_len = 36_u32.checked_add(data_len)?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);
    Some(wav)
}

#[derive(Clone)]
struct TaskBrowserDraft {
    search: Option<String>,
    archived: bool,
    /// Cursor used for each visited page. Index zero is always `None`.
    cursors: Vec<Option<String>>,
    page: usize,
    created_at: chrono::DateTime<Utc>,
}

#[derive(Clone)]
struct PluginBrowserDraft {
    rows: Vec<PluginCatalogRow>,
    scope: String,
    load_error_count: usize,
    channel_id: u64,
    owner_id: u64,
    created_at: chrono::DateTime<Utc>,
}

const TASK_BROWSER_TTL_MINUTES: i64 = 20;
const PLUGIN_BROWSER_TTL_MINUTES: i64 = 20;

#[derive(Clone, Copy)]
struct CodexExecutionPolicy {
    approval_policy: &'static str,
    thread_sandbox: &'static str,
    turn_sandbox_type: &'static str,
}

impl CodexExecutionPolicy {
    const NORMAL: Self = Self {
        approval_policy: "on-request",
        thread_sandbox: "workspace-write",
        turn_sandbox_type: "workspaceWrite",
    };
    const GOD: Self = Self {
        approval_policy: "never",
        thread_sandbox: "danger-full-access",
        turn_sandbox_type: "dangerFullAccess",
    };

    const fn for_god(active: bool) -> Self {
        if active { Self::GOD } else { Self::NORMAL }
    }

    fn thread_sandbox(self) -> Value {
        json!(self.thread_sandbox)
    }

    fn turn_sandbox(self) -> Value {
        json!({"type": self.turn_sandbox_type})
    }

    /// The one place `turn/start` parameters carry an execution policy.
    fn turn_start_params(
        self,
        thread_id: String,
        input: Vec<UserInput>,
        cwd: Option<String>,
        model: Option<String>,
    ) -> TurnStartParams {
        TurnStartParams {
            thread_id,
            input,
            cwd,
            model,
            approval_policy: Some(self.approval_policy.into()),
            sandbox_policy: Some(self.turn_sandbox()),
            extra: BTreeMap::new(),
        }
    }

    fn thread_settings(self, thread_id: &str) -> Value {
        json!({
            "threadId": thread_id,
            "approvalPolicy": self.approval_policy,
            "sandboxPolicy": self.turn_sandbox()
        })
    }
}

/// Serializes the short GOD state transitions that decide whether a dispatch
/// may start, while independently leasing slow cleanup work per task. Cleanup
/// performs Codex RPC retries and Discord I/O, so it must never retain the
/// global dispatch barrier.
#[derive(Default)]
struct GodLifecycle {
    dispatch_barrier: RwLock<()>,
    cleanup_tasks: Arc<dashmap::DashSet<String>>,
}

impl GodLifecycle {
    async fn dispatch(&self) -> tokio::sync::RwLockReadGuard<'_, ()> {
        self.dispatch_barrier.read().await
    }

    async fn transition(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.dispatch_barrier.write().await
    }

    fn cleanup_lease(&self, thread_id: &str) -> Option<GodCleanupLease> {
        if !self.cleanup_tasks.insert(thread_id.to_owned()) {
            return None;
        }
        Some(GodCleanupLease {
            thread_id: thread_id.to_owned(),
            cleanup_tasks: Arc::clone(&self.cleanup_tasks),
        })
    }
}

struct GodCleanupLease {
    thread_id: String,
    cleanup_tasks: Arc<dashmap::DashSet<String>>,
}

impl Drop for GodCleanupLease {
    fn drop(&mut self) {
        self.cleanup_tasks.remove(&self.thread_id);
    }
}

impl TaskBrowserDraft {
    fn expired(&self) -> bool {
        Utc::now() - self.created_at > chrono::Duration::minutes(TASK_BROWSER_TTL_MINUTES)
    }
}

impl PluginBrowserDraft {
    fn expired(&self) -> bool {
        Utc::now() - self.created_at > chrono::Duration::minutes(PLUGIN_BROWSER_TTL_MINUTES)
    }
}

struct BotState {
    config: Arc<Config>,
    store: StateStore,
    codex: CodexClient,
    host_broker: HostBroker,
    /// Subscribed in `run()` before the first Codex connect and consumed once
    /// by `spawn_codex_listeners`, so events emitted while the Discord gateway
    /// starts are buffered rather than lost.
    early_codex_receivers: std::sync::Mutex<
        Option<(
            tokio::sync::broadcast::Receiver<Notification>,
            tokio::sync::broadcast::Receiver<ServerRequest>,
        )>,
    >,
    god: RwLock<Arc<GodModeService>>,
    god_password_configured: AtomicBool,
    capabilities: Arc<CapabilityCatalog>,
    client_methods: Vec<String>,
    layout: RwLock<Option<Layout>>,
    streams: Mutex<HashMap<String, StreamBuffer>>,
    activity: Mutex<HashMap<String, ActivityDigest>>,
    plans: Mutex<HashMap<String, PlanBoard>>,
    realtime: Mutex<HashMap<String, RealtimeDigest>>,
    projection: Mutex<ProjectionCore>,
    operations: Mutex<HashMap<String, OperationsDigest>>,
    delivered_media: dashmap::DashSet<String>,
    ingestion_locks: dashmap::DashMap<u64, Arc<Mutex<()>>>,
    pending_server_requests: dashmap::DashMap<String, ServerRequest>,
    pending_request_started: dashmap::DashMap<String, std::time::Instant>,
    pending_request_messages: dashmap::DashMap<String, (u64, u64)>,
    resolved_request_tombstones: dashmap::DashMap<String, std::time::Instant>,
    action_drafts: dashmap::DashMap<String, ActionDraft>,
    task_browsers: dashmap::DashMap<String, TaskBrowserDraft>,
    plugin_browsers: dashmap::DashMap<String, PluginBrowserDraft>,
    runner_status_message_id: AtomicU64,
    god_warning_channel_id: AtomicU64,
    god_warning_message_id: AtomicU64,
    /// A task remains blocked until both its active turn is stopped and its
    /// persisted Codex permissions are normalized.
    god_quarantined_tasks: dashmap::DashMap<String, String>,
    /// Read guards cover turn dispatch; lifecycle transitions take the write
    /// guard so activation, expiry, and revoke cannot race a privileged RPC.
    god_lifecycle: GodLifecycle,
    startup_ready: AtomicBool,
    guild_isolation_verified: AtomicBool,
    bot_user_id: AtomicU64,
    listeners_started: AtomicBool,
}

pub const COMMAND_NAMES: &[&str] = &[
    "new",
    "tasks",
    "status",
    "actions",
    "god",
    "god_off",
    "email",
    "interrupt",
    "fork",
    "archive",
    "rename",
    "rollback",
    "compact",
    "review",
    "model",
    "skills",
    "apps",
    "plugins",
    "config",
    "account",
    "usage",
    "capabilities",
    "advanced",
    "goal",
    "terminals",
    "history",
    "find",
    "mcp",
    "mode",
    "effort",
    "files",
];

struct StoreAuditSink(StateStore);

#[async_trait]
impl AuditSink for StoreAuditSink {
    async fn emit(&self, event: AuditEvent) -> anyhow::Result<()> {
        let event = event.redacted();
        let detail =
            serde_json::to_value(&event).unwrap_or_else(|_| json!({"error":"audit serialization"}));
        self.0
            .audit(
                &format!("god.{:?}", event.action).to_lowercase(),
                event.owner_id,
                event.guild_id,
                event.channel_id,
                event.task_id.as_deref(),
                &detail,
            )
            .await
    }
}

struct Handler {
    state: Arc<BotState>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        self.state.startup_ready.store(false, Ordering::Release);
        self.state
            .bot_user_id
            .store(ready.user.id.get(), Ordering::Release);
        info!(user = %ready.user.name, "Discord gateway ready");
        ctx.set_activity(Some(ActivityData::watching("your Codex tasks")));
        if let Err(error) = provision::verify_guild_isolation(&ctx.http, &self.state.config).await {
            self.state
                .guild_isolation_verified
                .store(false, Ordering::Release);
            error!(%error, "Discord server isolation check failed; relay remains gated");
            return;
        }
        self.state
            .guild_isolation_verified
            .store(true, Ordering::Release);
        match provision::ensure_layout(&ctx.http, &self.state.config).await {
            Ok(layout) => *self.state.layout.write().await = Some(layout),
            Err(error) => {
                error!(%error, "Discord server provisioning failed");
                return;
            }
        }
        if let Err(error) = refresh_runner_status(&self.state, &ctx.http).await {
            warn!(%error, "runner status card update failed");
        }
        if let Err(error) = register_commands(&ctx.http, self.state.config.guild_id).await {
            error!(%error, "slash command registration failed");
        }
        if !self.state.listeners_started.swap(true, Ordering::AcqRel) {
            spawn_codex_listeners(Arc::clone(&self.state), Arc::clone(&ctx.http));
            spawn_stream_flusher(Arc::clone(&self.state), Arc::clone(&ctx.http));
            spawn_outbox_flusher(Arc::clone(&self.state), Arc::clone(&ctx.http));
            spawn_sensitive_deletion_flusher(Arc::clone(&self.state), Arc::clone(&ctx.http));
            spawn_god_monitor(Arc::clone(&self.state), Arc::clone(&ctx.http));
            spawn_guild_isolation_monitor(Arc::clone(&self.state), Arc::clone(&ctx.http));
        }
        if let Err(error) = reconcile(Arc::clone(&self.state), Arc::clone(&ctx.http)).await {
            error!(%error, "gateway-ready reconciliation failed; relay remains gated");
            return;
        }
        self.state.startup_ready.store(true, Ordering::Release);
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if !self.state.startup_ready.load(Ordering::Acquire) {
            let error = anyhow::anyhow!("relay startup safety reconciliation is still running");
            match &interaction {
                Interaction::Command(command) => {
                    send_command_error(&ctx.http, command, &error).await
                }
                Interaction::Component(component) => {
                    send_component_error(&ctx.http, component, &error).await;
                }
                Interaction::Modal(modal) => send_modal_error(&ctx.http, modal, &error).await,
                _ => {}
            }
            return;
        }
        match interaction {
            Interaction::Command(command) => {
                if let Err(error) = self.command(&ctx, command.clone()).await {
                    error!(%error, "command failed");
                    send_command_error(&ctx.http, &command, &error).await;
                }
            }
            Interaction::Component(component) => {
                if let Err(error) = self.component(&ctx, component.clone()).await {
                    error!(%error, "component failed");
                    send_component_error(&ctx.http, &component, &error).await;
                }
            }
            Interaction::Modal(modal) => {
                if let Err(error) = self.modal(&ctx, modal.clone()).await {
                    error!(%error, "modal failed");
                    send_modal_error(&ctx.http, &modal, &error).await;
                }
            }
            Interaction::Autocomplete(command) => {
                if let Err(error) = self.autocomplete(&ctx, &command).await {
                    warn!(%error, "autocomplete failed");
                }
            }
            _ => {}
        }
    }

    async fn message(&self, ctx: Context, message: Message) {
        if message.guild_id.map(GuildId::get) != Some(self.state.config.guild_id) {
            return;
        }
        if is_sensitive_god_message(&message.content) {
            self.handle_sensitive_god_message(&ctx.http, &message).await;
            return;
        }
        if message.author.bot {
            return;
        }
        if !self.is_relay_channel(message.channel_id).await {
            return;
        }
        if message.author.id.get() != self.state.config.owner_user_id {
            let _ = message.delete(&ctx.http).await;
            return;
        }
        if !self.state.startup_ready.load(Ordering::Acquire) {
            let _ = message
                .channel_id
                .say(
                    &ctx.http,
                    "Relay startup safety reconciliation is still running; retry shortly.",
                )
                .await;
            return;
        }
        if let Some((title, detail, show_controls)) =
            self.control_channel_guidance(message.channel_id).await
        {
            let mut response = CreateMessage::new().embed(embeds::info_card(title, detail));
            if show_controls {
                response = response.components(components::control_buttons());
            }
            let _ = message.channel_id.send_message(&ctx.http, response).await;
            return;
        }
        if let Err(error) = self.forward_task_message(&ctx, &message).await {
            let _ = message
                .channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new().embed(embeds::error_card(
                        "Could not send to Codex",
                        &error.to_string(),
                    )),
                )
                .await;
        }
    }
}

impl Handler {
    async fn handle_sensitive_god_message(&self, http: &Http, message: &Message) {
        let queue_error = self
            .state
            .store
            .queue_sensitive_deletion(message.channel_id.get(), message.id.get())
            .await
            .err()
            .map(|error| {
                error!(%error, "could not durably queue sensitive message deletion");
                error.to_string()
            });
        match delete_sensitive_discord_message(message, http).await {
            Ok(()) => {
                if queue_error.is_none()
                    && let Err(error) = self
                        .state
                        .store
                        .finish_sensitive_deletion(message.channel_id.get(), message.id.get())
                        .await
                {
                    warn!(%error, "sensitive deletion acknowledgement failed");
                }
                if message.author.id.get() == self.state.config.owner_user_id
                    && self.is_relay_channel(message.channel_id).await
                {
                    let _ = message.channel_id.send_message(
                        http,
                        CreateMessage::new().embed(embeds::error_card(
                            "Password message deleted",
                            "For safety it was not authenticated. Use `/god`; Discord modal input is never posted to channel history.",
                        )),
                    ).await;
                }
            }
            Err(delete_error) => {
                if let Some(queue_error) = queue_error {
                    report_unrecoverable_sensitive_deletion(
                        &self.state,
                        http,
                        message.channel_id,
                        message.id.get(),
                        &queue_error,
                        &delete_error.to_string(),
                    )
                    .await;
                } else {
                    warn!(
                        message_id = message.id.get(),
                        %delete_error,
                        "sensitive GOD message deletion will be retried during reconciliation"
                    );
                }
            }
        }
    }

    async fn control_channel_guidance(
        &self,
        channel_id: ChannelId,
    ) -> Option<(&'static str, &'static str, bool)> {
        let layout = self.state.layout.read().await;
        control_channel_guidance(layout.as_ref()?, channel_id)
    }

    async fn is_relay_channel(&self, channel_id: ChannelId) -> bool {
        let control_channel = self
            .state
            .layout
            .read()
            .await
            .as_ref()
            .is_some_and(|layout| {
                [
                    layout.new_task_channel,
                    layout.existing_tasks_channel,
                    layout.runner_status_channel,
                    layout.audit_log_channel,
                ]
                .contains(&channel_id)
            });
        control_channel
            || self
                .state
                .store
                .task_by_channel(channel_id.get())
                .await
                .ok()
                .flatten()
                .is_some()
    }

    async fn autocomplete(&self, ctx: &Context, command: &CommandInteraction) -> Result<()> {
        self.authorize_interaction(command.user.id.get(), command.guild_id.map(|id| id.get()))?;
        let Some(focused) = command.data.autocomplete() else {
            return Ok(());
        };
        let needle = focused.value.to_ascii_lowercase();
        let mut response = CreateAutocompleteResponse::new();
        if command.data.name == "effort" {
            let model = self
                .state
                .store
                .task_by_channel(command.channel_id.get())
                .await?
                .and_then(|task| task.model)
                .unwrap_or_else(|| RELAY_DEFAULT_MODEL.to_owned());
            if let Ok(Ok(result)) = tokio::time::timeout(
                Duration::from_secs(2),
                self.state.codex.model_list(ModelListParams {
                    cursor: None,
                    limit: Some(100),
                    include_hidden: Some(true),
                }),
            )
            .await
            {
                for effort in model_reasoning_efforts(&result, &model)
                    .into_iter()
                    .filter(|effort| effort.to_ascii_lowercase().contains(&needle))
                    .take(25)
                {
                    response = response.add_string_choice(&effort, effort.clone());
                }
            }
        } else {
            for method in self
                .state
                .client_methods
                .iter()
                .filter(|method| method.to_ascii_lowercase().contains(&needle))
                .take(25)
            {
                response = response.add_string_choice(method, method);
            }
        }
        command
            .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
            .await?;
        Ok(())
    }

    async fn command(&self, ctx: &Context, command: CommandInteraction) -> Result<()> {
        self.authorize_interaction(command.user.id.get(), command.guild_id.map(|id| id.get()))?;
        match command.data.name.as_str() {
            "new" => show_new_task_modal(&ctx.http, &command).await?,
            "tasks" => {
                let mut search = None;
                let mut archived = false;
                let mut page = 0_usize;
                for option in command.data.options() {
                    match (option.name, option.value) {
                        ("search", serenity::all::ResolvedValue::String(value)) => {
                            search = Some(value.to_owned());
                        }
                        ("archived", serenity::all::ResolvedValue::Boolean(value)) => {
                            archived = value;
                        }
                        ("page", serenity::all::ResolvedValue::Integer(value)) => {
                            page = value.clamp(1, 4) as usize - 1;
                        }
                        _ => {}
                    }
                }
                self.show_tasks(
                    &ctx.http,
                    CommandResponder::Command(&command),
                    search,
                    archived,
                    page,
                )
                .await?
            }
            "status" => self.show_status(&ctx.http, &command).await?,
            "actions" => {
                self.show_action_categories(&ctx.http, CommandResponder::Command(&command))
                    .await?
            }
            "god" => {
                show_god_modal(&ctx.http, &command, self.state.config.god_session_minutes).await?
            }
            "god_off" => {
                self.god_off(&ctx.http, CommandResponder::Command(&command))
                    .await?;
            }
            "email" => show_email_modal(&ctx.http, &command).await?,
            "interrupt" => self.interrupt(&ctx.http, &command).await?,
            "fork" => self.fork(&ctx.http, &command).await?,
            "archive" => self.archive(&ctx.http, &command).await?,
            "rename" => self.rename(&ctx.http, &command).await?,
            "rollback" => self.rollback(&ctx.http, &command).await?,
            "compact" => self.compact(&ctx.http, &command).await?,
            "review" => self.review(&ctx.http, &command).await?,
            "model" => self.models(&ctx.http, &command).await?,
            "skills" => self.skills(&ctx.http, &command).await?,
            "apps" => self.apps(&ctx.http, &command).await?,
            "plugins" => self.plugins(&ctx.http, &command).await?,
            "config" => self.config_summary(&ctx.http, &command).await?,
            "account" => self.account(&ctx.http, &command).await?,
            "usage" => self.usage(&ctx.http, &command).await?,
            "capabilities" => self.capabilities(&ctx.http, &command).await?,
            "advanced" => self.advanced(&ctx.http, &command).await?,
            "goal" => self.goal(&ctx.http, &command).await?,
            "terminals" => self.terminals(&ctx.http, &command).await?,
            "history" => self.history(&ctx.http, &command).await?,
            "find" => self.find_tasks(&ctx.http, &command).await?,
            "mcp" => self.mcp_status(&ctx.http, &command).await?,
            "mode" => self.collaboration_modes(&ctx.http, &command).await?,
            "effort" => self.set_effort(&ctx.http, &command).await?,
            "files" => self.find_files(&ctx.http, &command).await?,
            _ => {}
        }
        Ok(())
    }

    async fn component(&self, ctx: &Context, component: ComponentInteraction) -> Result<()> {
        self.authorize_interaction(
            component.user.id.get(),
            component.guild_id.map(|id| id.get()),
        )?;
        let id = component.data.custom_id.as_str();
        if id == components::NEW_TASK {
            component
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Modal(
                        CreateModal::new(components::NEW_TASK_MODAL, "Start Codex task")
                            .components(components::new_task_inputs()),
                    ),
                )
                .await?;
        } else if id == components::REFRESH_TASKS {
            self.show_tasks(
                &ctx.http,
                CommandResponder::Component(&component),
                None,
                false,
                0,
            )
            .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::TASK_BROWSER_PAGE) {
            let (token, direction) = rest
                .rsplit_once(':')
                .context("invalid task browser control")?;
            self.page_tasks(&ctx.http, &component, token, direction)
                .await?;
        } else if id == components::ACTION_BROWSER {
            self.show_action_categories(&ctx.http, CommandResponder::Component(&component))
                .await?;
        } else if id == components::PLUGIN_ACTIONS {
            self.show_action_methods(
                &ctx.http,
                &component,
                ActionSurface::PluginsAndMarketplace.slug(),
                0,
            )
            .await?;
        } else if let Some(token) = components::custom_id_arg(id, components::PLUGIN_SELECT) {
            let index = selected_value(&component)?.parse()?;
            self.show_plugin_detail(&ctx.http, &component, token, index)
                .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::PLUGIN_BACK) {
            let (token, index) = rest
                .rsplit_once(':')
                .context("invalid plugin back control")?;
            self.show_plugin_browser_page(&ctx.http, &component, token, index.parse()?)
                .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::PLUGIN_PAGE) {
            let (token, page) = rest
                .rsplit_once(':')
                .context("invalid plugin page control")?;
            self.show_plugin_browser_page_number(&ctx.http, &component, token, page.parse()?)
                .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::PLUGIN_INSTALL) {
            let (token, index) = rest
                .rsplit_once(':')
                .context("invalid plugin install control")?;
            self.begin_plugin_mutation(&ctx.http, &component, token, index.parse()?, true)
                .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::PLUGIN_UNINSTALL) {
            let (token, index) = rest
                .rsplit_once(':')
                .context("invalid plugin uninstall control")?;
            self.begin_plugin_mutation(&ctx.http, &component, token, index.parse()?, false)
                .await?;
        } else if id == components::ACTION_CATEGORY {
            let value = selected_value(&component)?;
            self.show_action_methods(&ctx.http, &component, value, 0)
                .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::ACTION_PAGE) {
            let (category, page) = rest.rsplit_once(':').context("invalid action page")?;
            self.show_action_methods(&ctx.http, &component, category, page.parse()?)
                .await?;
        } else if id == components::ACTION_METHOD {
            let method = selected_value(&component)?;
            self.begin_action(&ctx.http, &component, method).await?;
        } else if id == components::MODEL_SELECT {
            self.apply_model_selection(&ctx.http, &component).await?;
        } else if id == components::MODE_SELECT {
            self.apply_mode_selection(&ctx.http, &component).await?;
        } else if id == components::TERMINAL_KILL {
            self.terminate_terminal(&ctx.http, &component).await?;
        } else if id == components::TERMINAL_CLEAN {
            self.clean_terminals(&ctx.http, &component).await?;
        } else if id == components::REALTIME_START {
            self.begin_action(&ctx.http, &component, "thread/realtime/start")
                .await?;
        } else if id == components::REALTIME_TEXT {
            self.begin_action(&ctx.http, &component, "thread/realtime/appendText")
                .await?;
        } else if id == components::REALTIME_VOICES {
            self.begin_action(&ctx.http, &component, "thread/realtime/listVoices")
                .await?;
        } else if id == components::REALTIME_STOP {
            self.begin_action(&ctx.http, &component, "thread/realtime/stop")
                .await?;
        } else if let Some(rest) = components::custom_id_arg(id, components::ACTION_CONTINUE) {
            let (token, page) = rest
                .rsplit_once(':')
                .context("invalid action continuation")?;
            self.open_action_form(&ctx.http, &component, token, page.parse()?)
                .await?;
        } else if let Some(token) = components::custom_id_arg(id, components::ACTION_EXECUTE) {
            self.execute_action(&ctx.http, &component, token).await?;
        } else if let Some(token) = components::custom_id_arg(id, components::ACTION_CANCEL) {
            self.cancel_action(&ctx.http, &component, token).await?;
        } else if id == components::GOD_START {
            component
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Modal(
                        CreateModal::new(
                            components::GOD_MODAL,
                            format!(
                                "Unlock GOD mode ({} minutes)",
                                self.state.config.god_session_minutes
                            ),
                        )
                        .components(components::god_password_input()),
                    ),
                )
                .await?;
        } else if id == components::GOD_STOP {
            self.god_off(&ctx.http, CommandResponder::Component(&component))
                .await?;
        } else if id == components::TASK_CONTINUE {
            component
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Modal(
                        CreateModal::new(components::CONTINUE_MODAL, "Message Codex")
                            .components(components::continue_input()),
                    ),
                )
                .await?;
        } else if id == components::TASK_INTERRUPT {
            self.interrupt_component(&ctx.http, &component).await?;
        } else if id == components::TASK_FORK {
            self.fork_component(&ctx.http, &component).await?;
        } else if id == components::TASK_ARCHIVE {
            self.archive_component(&ctx.http, &component).await?;
        } else if id == components::EMAIL_START {
            component
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Modal(
                        CreateModal::new(components::EMAIL_MODAL, "Send email with Codex")
                            .components(components::email_inputs()),
                    ),
                )
                .await?;
        } else if id == components::OPEN_TASK {
            self.open_selected_task(&ctx.http, &component, false)
                .await?;
        } else if id == components::OPEN_TASK_ARCHIVED {
            self.open_selected_task(&ctx.http, &component, true).await?;
        } else if let Some(token) = components::custom_id_arg(id, components::ANSWER_REQUEST) {
            let request = self
                .state
                .pending_server_requests
                .get(token)
                .map(|request| request.clone())
                .context("request expired; wait for Codex to ask again")?;
            let fields = server_answer_form(&request)?;
            component
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Modal(
                        CreateModal::new(
                            format!("{}:{token}", components::SERVER_ANSWER_MODAL),
                            "Answer Codex",
                        )
                        .components(fields),
                    ),
                )
                .await?;
        } else if let Some(value) = components::custom_id_arg(id, components::APPROVE_OFFERED) {
            let (request_id, index) = value
                .rsplit_once(':')
                .context("offered approval control is malformed")?;
            let index = index
                .parse::<usize>()
                .context("invalid offered decision index")?;
            let decision = format!("offered:{index}");
            self.resolve_approval(&ctx.http, &component, request_id, &decision)
                .await?;
        } else if let Some((decision, request_id)) = parse_approval_component(id) {
            self.resolve_approval(&ctx.http, &component, request_id, decision)
                .await?;
        } else {
            ephemeral_component(
                &ctx.http,
                &component,
                embeds::error_card(
                    "Unknown control",
                    "This card is stale. Refresh the task dashboard.",
                ),
            )
            .await?;
        }
        Ok(())
    }

    async fn modal(&self, ctx: &Context, modal: ModalInteraction) -> Result<()> {
        self.authorize_interaction(modal.user.id.get(), modal.guild_id.map(|id| id.get()))?;
        match modal.data.custom_id.as_str() {
            components::NEW_TASK_MODAL => self.create_task(&ctx.http, &modal).await?,
            components::GOD_MODAL => self.enable_god(&ctx.http, &modal).await?,
            components::CONTINUE_MODAL => self.continue_from_modal(&ctx.http, &modal).await?,
            components::EMAIL_MODAL => self.send_email(&ctx.http, &modal).await?,
            custom_id if custom_id.starts_with(components::ACTION_FORM_MODAL) => {
                let rest = components::custom_id_arg(custom_id, components::ACTION_FORM_MODAL)
                    .context("missing action form token")?;
                let (token, page) = rest.rsplit_once(':').context("invalid action form token")?;
                self.save_action_form(&ctx.http, &modal, token, page.parse()?)
                    .await?;
            }
            custom_id if custom_id.starts_with(components::SERVER_ANSWER_MODAL) => {
                let token = components::custom_id_arg(custom_id, components::SERVER_ANSWER_MODAL)
                    .context("missing server request token")?;
                self.answer_server_request(&ctx.http, &modal, token).await?;
            }
            other => anyhow::bail!(
                "unrecognized form `{other}`; the card that opened it is stale — reopen it from the dashboard"
            ),
        }
        Ok(())
    }

    fn authorize_interaction(&self, user_id: u64, guild_id: Option<u64>) -> Result<()> {
        anyhow::ensure!(user_id == self.state.config.owner_user_id, "owner-only bot");
        anyhow::ensure!(
            guild_id == Some(self.state.config.guild_id),
            "wrong Discord server"
        );
        Ok(())
    }

    async fn create_task(&self, http: &Http, modal: &ModalInteraction) -> Result<()> {
        defer_modal(http, modal).await?;
        let prompt = modal_value(modal, "prompt").context("prompt missing")?;
        let cwd = modal_value(modal, "cwd")
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.state
                    .config
                    .default_cwd
                    .as_ref()
                    .map(|path| path.display().to_string())
            });
        anyhow::ensure!(
            cwd.as_ref()
                .is_some_and(|path| std::path::Path::new(path).is_absolute()),
            "working directory must be an absolute path (set it in modal or setup default)"
        );
        let title = prompt
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Codex task")
            .chars()
            .take(80)
            .collect::<String>();
        let thread = self
            .state
            .codex
            .thread_start(ThreadStartParams {
                cwd: cwd.clone(),
                model: Some(RELAY_DEFAULT_MODEL.to_owned()),
                approval_policy: Some(CodexExecutionPolicy::NORMAL.approval_policy.into()),
                sandbox: Some(CodexExecutionPolicy::NORMAL.thread_sandbox()),
                personality: None,
                developer_instructions: Some(RELAY_DEVELOPER_INSTRUCTIONS.to_owned()),
                runtime_workspace_roots: cwd.clone().map(|path| vec![path]),
                dynamic_tools: Some(HostBroker::dynamic_tool_specs()),
                extra: BTreeMap::from([("config".to_owned(), json!({"web_search": "live"}))]),
            })
            .await?;
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("Codex thread/start response missing thread.id")?
            .to_owned();
        self.state
            .codex
            .thread_settings_update(ThreadSettingsUpdateParams {
                thread_id: thread_id.clone(),
                effort: Some(RELAY_DEFAULT_REASONING_EFFORT.to_owned()),
                collaboration_mode: None,
            })
            .await?;
        let layout = self
            .state
            .layout
            .read()
            .await
            .clone()
            .context("Discord layout unavailable")?;
        let channel_id = provision::create_task_channel_for_state(
            http,
            &self.state.config,
            &self.state.store,
            &layout,
            &title,
            &thread_id,
            TaskState::Running,
        )
        .await?;
        let mut task = TaskMirror {
            thread_id: thread_id.clone(),
            channel_id: Some(channel_id.get()),
            title: title.clone(),
            cwd: cwd.clone(),
            state: TaskState::Running,
            turn_id: None,
            model: Some(RELAY_DEFAULT_MODEL.to_owned()),
            last_event_at: Some(Utc::now()),
        };
        self.state.store.upsert_task(&task).await?;
        channel_id
            .send_message(
                http,
                CreateMessage::new()
                    .content(format!("<@{}>", self.state.config.owner_user_id))
                    .embed(embeds::task_card(
                        &task,
                        Some("Task created. Codex is starting…"),
                    ))
                    .components(components::task_buttons(false)),
            )
            .await?;
        let turn = self
            .state
            .codex
            .turn_start(CodexExecutionPolicy::NORMAL.turn_start_params(
                thread_id.clone(),
                vec![UserInput::text(prompt)],
                cwd,
                Some(RELAY_DEFAULT_MODEL.to_owned()),
            ))
            .await?;
        task.turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        anyhow::ensure!(
            task.turn_id.is_some(),
            "Codex turn/start response missing turn.id"
        );
        self.state.store.upsert_task(&task).await?;
        refresh_task_card(http, &task, Some("Codex is working…")).await?;
        modal
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content(format!("Created <#{channel_id}> — Codex is working.")),
            )
            .await?;
        Ok(())
    }

    async fn enable_god(&self, http: &Http, modal: &ModalInteraction) -> Result<()> {
        anyhow::ensure!(
            self.state.god_password_configured.load(Ordering::Acquire),
            "GOD password is not configured locally; run `codex-discord set-god-password`, then restart the relay"
        );
        let password = Zeroizing::new(
            modal_value(modal, "password")
                .context("password missing")?
                .to_owned(),
        );
        defer_modal(http, modal).await?;
        let task = self
            .state
            .store
            .task_by_channel(modal.channel_id.get())
            .await?;
        let task = task.context("GOD mode is only available inside a private task channel")?;
        anyhow::ensure!(
            task.state != TaskState::Running,
            "stop the active turn before enabling GOD mode; it only applies to newly started turns"
        );
        let security_context = self
            .security_context(http, modal.user.id.get(), modal.guild_id, modal.channel_id)
            .await;
        let submission =
            PasswordSubmission::from_private_modal(SecretString::from(password.to_string()));
        let scope = SessionScope::Task(task.thread_id.clone());
        let god = self.state.god.read().await.clone();
        let _lifecycle = self.state.god_lifecycle.transition().await;
        anyhow::ensure!(
            !self
                .state
                .god_quarantined_tasks
                .contains_key(&task.thread_id)
                && !god.cleanup_required_for(&task.thread_id),
            "task is quarantined while GOD lifecycle cleanup is pending"
        );
        match god
            .authenticate_pending(&security_context, submission, scope)
            .await
        {
            Ok(pending) => {
                let previous = if let Some(expired) = god.expire_global_session().await {
                    Some(expired)
                } else {
                    let active = god.active_global_session().await;
                    if active.is_some() {
                        god.revoke_active(&security_context).await?;
                    }
                    active
                };
                if let Some(previous) = previous.as_ref() {
                    if let Some(previous_task) = previous.scope.task_id() {
                        self.state.god_quarantined_tasks.insert(
                            previous_task.to_owned(),
                            "replaced GOD session cleanup".to_owned(),
                        );
                        self.state
                            .store
                            .mark_god_dirty(previous_task, "replaced GOD session cleanup")
                            .await?;
                    }
                    cleanup_god_session(
                        &self.state,
                        http,
                        previous,
                        "replaced by a new GOD session",
                    )
                    .await
                    .context(
                        "previous GOD task could not be safely normalized; new session cancelled",
                    )?;
                }
                let scope_label = format!("Task `{}`", task.thread_id);
                let warning = match modal
                    .channel_id
                    .send_message(
                        http,
                        CreateMessage::new()
                            .content(format!("<@{}>", self.state.config.owner_user_id))
                            .embed(embeds::god_warning(
                                self.state.config.god_session_minutes,
                                &scope_label,
                            ))
                            .components(vec![serenity::builder::CreateActionRow::Buttons(vec![
                                serenity::builder::CreateButton::new(components::GOD_STOP)
                                    .label("Revoke now")
                                    .style(serenity::all::ButtonStyle::Danger),
                            ])]),
                    )
                    .await
                {
                    Ok(warning) => warning,
                    Err(error) => return Err(error.into()),
                };
                let issued = match god.activate_pending(&security_context, pending).await {
                    Ok(issued) => issued,
                    Err(error) => {
                        let _ = warning.delete(http).await;
                        return Err(error.into());
                    }
                };
                anyhow::ensure!(
                    issued.revoked.is_none(),
                    "GOD activation raced an unexpected active session"
                );
                let session = issued.session;
                // The new visible warning existed before the lease became active.
                retire_god_warning(&self.state, http, "GOD session replaced").await;
                self.state
                    .god_warning_channel_id
                    .store(warning.channel_id.get(), Ordering::Release);
                self.state
                    .god_warning_message_id
                    .store(warning.id.get(), Ordering::Release);
                refresh_runner_status(&self.state, http).await?;
                modal
                    .edit_response(
                        http,
                        EditInteractionResponse::new().embed(embeds::info_card(
                            "GOD mode active",
                            &format!(
                                "Active until <t:{}:R>. New turns in scope use `danger-full-access` + approvals `never`.",
                                session.expires_at.timestamp()
                            ),
                        )),
                    )
                    .await?;
            }
            Err(error) => {
                modal
                    .edit_response(
                        http,
                        EditInteractionResponse::new()
                            .embed(embeds::error_card("GOD mode denied", &error.to_string())),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn continue_from_modal(&self, http: &Http, modal: &ModalInteraction) -> Result<()> {
        let prompt = modal_value(modal, "prompt")
            .context("message missing")?
            .to_owned();
        defer_modal(http, modal).await?;
        let mut task = self.task_for_channel(modal.channel_id).await?;
        let security_context = self
            .security_context(http, modal.user.id.get(), modal.guild_id, modal.channel_id)
            .await;
        self.dispatch_task_input(
            http,
            &security_context,
            modal.channel_id,
            &mut task,
            vec![UserInput::text(prompt)],
            "GOD turn dispatch",
        )
        .await?;
        modal
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("Sent to Codex. Live response follows here."),
            )
            .await?;
        Ok(())
    }

    async fn send_email(&self, http: &Http, modal: &ModalInteraction) -> Result<()> {
        let to = modal_value(modal, "to")
            .context("recipient missing")?
            .trim()
            .to_owned();
        let subject = modal_value(modal, "subject")
            .context("subject missing")?
            .trim()
            .to_owned();
        let cc = modal_value(modal, "cc").unwrap_or("").trim().to_owned();
        let body = modal_value(modal, "body")
            .context("message missing")?
            .trim()
            .to_owned();
        anyhow::ensure!(!to.contains(['\r', '\n']), "recipient must be on one line");
        anyhow::ensure!(
            !subject.contains(['\r', '\n']),
            "subject must be on one line"
        );
        defer_modal(http, modal).await?;
        let mut task = self.task_for_channel(modal.channel_id).await?;
        let gmail_mention = collect_apps(&self.state.codex, Some(task.thread_id.clone()), true)
            .await?
            .into_iter()
            .find(|app| {
                app.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("gmail"))
            })
            .and_then(|app| app.get("id").and_then(Value::as_str).map(str::to_owned))
            .map(|id| format!("[$Gmail](app://{id})\n"))
            .unwrap_or_default();
        let cc_line = if cc.is_empty() {
            String::new()
        } else {
            format!("\nCc: {cc}")
        };
        let prompt = format!(
            "{gmail_mention}Use the Gmail connector to send this email. If Gmail needs installation, authorization, or confirmation, request it through the connector flow and continue after approval. Do not claim success without a successful connector result.\nTo: {to}{cc_line}\nSubject: {subject}\n\n{body}"
        );
        let security_context = self
            .security_context(http, modal.user.id.get(), modal.guild_id, modal.channel_id)
            .await;
        self.dispatch_task_input(
            http,
            &security_context,
            modal.channel_id,
            &mut task,
            vec![UserInput::text(prompt)],
            "GOD email turn dispatch",
        )
        .await?;
        modal
            .edit_response(
                http,
                EditInteractionResponse::new().content(
                    "Email request sent to Codex. Approval or connector setup may be required.",
                ),
            )
            .await?;
        Ok(())
    }

    async fn forward_task_message(&self, ctx: &Context, message: &Message) -> Result<()> {
        let lock = self
            .state
            .ingestion_locks
            .entry(message.channel_id.get())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        let Some(mut task) = self
            .state
            .store
            .task_by_channel(message.channel_id.get())
            .await?
        else {
            return Ok(());
        };
        let mut input = Vec::new();
        if !message.content.trim().is_empty() {
            input.push(UserInput::text(&message.content));
        }
        for attachment in &message.attachments {
            let local = cache_discord_attachment(attachment, message.id.get()).await?;
            if attachment
                .content_type
                .as_deref()
                .is_some_and(|kind| kind.starts_with("image/"))
            {
                input.push(UserInput::LocalImage {
                    path: local.display().to_string(),
                    detail: Some("auto".into()),
                });
            } else {
                input.push(UserInput::text(format!(
                    "Discord attachment cached locally: {} ({})",
                    attachment.filename,
                    local.display()
                )));
            }
        }
        if input.is_empty() {
            return Ok(());
        }
        let security_context = self
            .security_context(
                &ctx.http,
                message.author.id.get(),
                message.guild_id,
                message.channel_id,
            )
            .await;
        self.dispatch_task_input(
            &ctx.http,
            &security_context,
            message.channel_id,
            &mut task,
            input,
            "GOD message turn dispatch",
        )
        .await?;
        refresh_task_card(&ctx.http, &task, Some("Codex is working…")).await?;
        self.state
            .store
            .set_cursor(message.channel_id.get(), message.id.get())
            .await?;
        message.react(&ctx.http, '✅').await?;
        Ok(())
    }

    async fn dispatch_task_input(
        &self,
        http: &Http,
        security_context: &SecurityContext,
        channel_id: ChannelId,
        task: &mut TaskMirror,
        input: Vec<UserInput>,
        god_dirty_reason: &str,
    ) -> Result<()> {
        let _dispatch = self.state.god_lifecycle.dispatch().await;
        self.ensure_task_not_god_quarantined(&task.thread_id)
            .await?;
        let god = self.state.god.read().await.clone();
        let god_active = god
            .active_session(security_context, Some(&task.thread_id))
            .await
            .is_some();
        self.ensure_task_not_god_quarantined(&task.thread_id)
            .await?;
        if god_active {
            self.state
                .store
                .mark_god_dirty(&task.thread_id, god_dirty_reason)
                .await?;
        }

        let result = if task.state == TaskState::Running {
            self.state
                .codex
                .turn_steer(TurnSteerParams {
                    thread_id: task.thread_id.clone(),
                    expected_turn_id: task
                        .turn_id
                        .clone()
                        .context("running task has no active turn id yet; retry in a moment")?,
                    input,
                    extra: BTreeMap::new(),
                })
                .await?
        } else {
            let policy = CodexExecutionPolicy::for_god(god_active);
            self.state
                .codex
                .turn_start(policy.turn_start_params(
                    task.thread_id.clone(),
                    input,
                    task.cwd.clone(),
                    task.model.clone(),
                ))
                .await?
        };

        task.turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| task.turn_id.clone());
        task.state = TaskState::Running;
        task.last_event_at = Some(Utc::now());
        self.state.store.upsert_task(task).await?;
        provision::move_task_to_state(http, &self.state.config, channel_id, TaskState::Running)
            .await?;
        Ok(())
    }

    async fn show_action_categories(
        &self,
        http: &Http,
        responder: CommandResponder<'_>,
    ) -> Result<()> {
        responder.defer(http).await?;
        self.state.action_drafts.retain(|_, draft| !draft.expired());
        let mut categories = BTreeMap::<String, usize>::new();
        for method in self
            .state
            .capabilities
            .client_requests
            .iter()
            .filter(|method| actions::action_exemption(method).is_none())
        {
            *categories
                .entry(actions::method_category(method).to_owned())
                .or_default() += 1;
        }
        let embed = CreateEmbed::new()
            .title("Codex action center")
            .description(format!(
                "{} dedicated actions from the live app-server schema. Choose a family; every known method has curated policy and a typed form.",
                self.state
                    .capabilities
                    .client_requests
                    .iter()
                    .filter(|method| actions::dedicated_action(method).is_some())
                    .count()
            ))
            .field("Safety", "Read-only actions run normally. Mutations require owner confirmation. Raw processes, guardian overrides, history injection, and unrestricted shell access require task-scoped GOD mode.", false)
            .color(0x58_65F2);
        responder
            .respond(http, embed, components::action_category_select(categories))
            .await
    }

    async fn show_action_methods(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        category: &str,
        page: usize,
    ) -> Result<()> {
        defer_component(http, component).await?;
        let methods = self
            .state
            .capabilities
            .client_request_metadata
            .values()
            .filter(|capability| {
                actions::action_exemption(&capability.method).is_none()
                    && actions::method_category(&capability.method) == category
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(!methods.is_empty(), "unknown Codex action family");
        let page_count = methods.len().div_ceil(25);
        let page = page.min(page_count.saturating_sub(1));
        let options = methods.iter().skip(page * 25).take(25).map(|capability| {
            let spec = actions::dedicated_action(&capability.method);
            let description = spec.map_or_else(
                || "Compatibility form for a newly discovered Codex method".to_owned(),
                |spec| spec.description.to_owned(),
            );
            (
                capability.method.clone(),
                spec.map_or_else(
                    || format!("Future: {}", capability.method),
                    |spec| spec.label.to_owned(),
                ),
                description,
            )
        });
        component
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(
                        CreateEmbed::new()
                            .title(format!("{} actions", category))
                            .description(format!(
                                "Page {} of {}. Choose an action to build a typed form.",
                                page + 1,
                                page_count
                            ))
                            .color(0x58_65F2),
                    )
                    .components(components::action_method_picker(
                        category,
                        page,
                        options,
                        page > 0,
                        page + 1 < page_count,
                    )),
            )
            .await?;
        Ok(())
    }

    async fn begin_action(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        method: &str,
    ) -> Result<()> {
        let capability = self
            .state
            .capabilities
            .client_request(method)
            .with_context(|| format!("Codex no longer advertises {method}"))?;
        anyhow::ensure!(
            actions::action_exemption(method).is_none(),
            "{method} is a bridge-owned protocol mechanic, not a user action"
        );
        let task = self
            .state
            .store
            .task_by_channel(component.channel_id.get())
            .await?;
        let draft = ActionDraft::from_capability(
            capability,
            task.as_ref().map(|task| task.thread_id.clone()),
            component.channel_id.get(),
            task.as_ref()
                .and_then(|task| task.cwd.as_deref())
                .or_else(|| {
                    self.state
                        .config
                        .default_cwd
                        .as_deref()
                        .and_then(|path| path.to_str())
                }),
        );
        let token = uuid::Uuid::new_v4().simple().to_string();
        let fields = draft.page_fields(0).to_vec();
        self.state
            .action_drafts
            .insert(token.clone(), draft.clone());
        if fields.is_empty() {
            let (embed, controls) = action_confirmation(&draft, &token);
            component
                .create_response(
                    http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .embed(embed)
                            .components(controls)
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(());
        }
        component
            .create_response(
                http,
                CreateInteractionResponse::Modal(action_modal(&draft, &token, 0)),
            )
            .await?;
        Ok(())
    }

    async fn open_action_form(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
        page: usize,
    ) -> Result<()> {
        let draft = self
            .state
            .action_drafts
            .get(token)
            .context("action draft expired; reopen Actions")?;
        anyhow::ensure!(!draft.expired(), "action draft expired; reopen Actions");
        let _dispatch = self.state.god_lifecycle.dispatch().await;
        anyhow::ensure!(
            draft.channel_id == component.channel_id.get(),
            "action draft belongs to another channel"
        );
        anyhow::ensure!(page < draft.page_count(), "invalid action form page");
        component
            .create_response(
                http,
                CreateInteractionResponse::Modal(action_modal(&draft, token, page)),
            )
            .await?;
        Ok(())
    }

    async fn save_action_form(
        &self,
        http: &Http,
        modal: &ModalInteraction,
        token: &str,
        page: usize,
    ) -> Result<()> {
        defer_modal(http, modal).await?;
        let mut values = BTreeMap::new();
        for relative_index in 0..actions::FIELDS_PER_PAGE {
            if let Some(value) = modal_value(modal, &format!("f:{relative_index}")) {
                values.insert(relative_index, value.to_owned());
            }
        }
        let mut draft = self
            .state
            .action_drafts
            .get_mut(token)
            .context("action draft expired; reopen Actions")?;
        anyhow::ensure!(!draft.expired(), "action draft expired; reopen Actions");
        anyhow::ensure!(
            draft.channel_id == modal.channel_id.get(),
            "action draft belongs to another channel"
        );
        draft.apply_page_values(page, &values)?;
        let next_page = page + 1;
        if next_page < draft.page_count() {
            modal
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content(format!(
                            "Saved page {} of {} for `{}`.",
                            page + 1,
                            draft.page_count(),
                            draft.method
                        ))
                        .components(components::action_continue_button(token, next_page)),
                )
                .await?;
        } else {
            let (embed, controls) = action_confirmation(&draft, token);
            modal
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .embed(embed)
                        .components(controls),
                )
                .await?;
        }
        Ok(())
    }

    async fn execute_action(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
    ) -> Result<()> {
        defer_component(http, component).await?;
        let (_, draft) = self
            .state
            .action_drafts
            .remove(token)
            .context("action draft expired; reopen Actions")?;
        self.execute_action_inner(http, component, &draft).await
    }

    async fn execute_action_inner(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        draft: &ActionDraft,
    ) -> Result<()> {
        anyhow::ensure!(!draft.expired(), "action draft expired; reopen Actions");
        let _dispatch = self.state.god_lifecycle.dispatch().await;
        anyhow::ensure!(
            self.is_relay_channel(component.channel_id).await,
            "Codex actions are only available in private relay channels"
        );
        anyhow::ensure!(
            draft.channel_id == component.channel_id.get(),
            "action draft belongs to another channel"
        );
        if let Some(task_id) = draft.task_id.as_deref() {
            self.ensure_task_not_god_quarantined(task_id).await?;
        }
        let policy = draft.effective_policy();
        if policy.authorization == ActionAuthorization::Owner {
            anyhow::ensure!(
                component.user.id.get() == self.state.config.owner_user_id,
                "this action requires the configured owner"
            );
        }
        if policy.authorization == ActionAuthorization::God {
            let context = self
                .security_context(
                    http,
                    component.user.id.get(),
                    component.guild_id,
                    component.channel_id,
                )
                .await;
            let god = self.state.god.read().await.clone();
            anyhow::ensure!(
                god.active_session(&context, draft.task_id.as_deref())
                    .await
                    .is_some(),
                "this action requires active task-scoped GOD mode"
            );
            self.ensure_task_not_god_quarantined(
                draft
                    .task_id
                    .as_deref()
                    .context("GOD action requires a task")?,
            )
            .await?;
            self.state
                .store
                .mark_god_dirty(
                    draft
                        .task_id
                        .as_deref()
                        .context("GOD action requires a task")?,
                    "GOD schema action dispatch",
                )
                .await?;
        }
        let capability = self
            .state
            .capabilities
            .client_request(&draft.method)
            .context("action method is absent from the installed Codex schema")?;
        if let Some(schema) = capability.params_schema.as_ref() {
            let errors = crate::codex::params::validate(schema, &draft.params);
            anyhow::ensure!(
                errors.is_empty(),
                "parameters no longer match the installed schema:\n{}",
                errors.join("\n")
            );
        }
        let action_id = stable_value_hash(&json!({
            "method": draft.method,
            "params": draft.params,
            "channel": draft.channel_id
        }));
        self.state
            .store
            .audit(
                "schema_action.intent",
                Some(component.user.id.get()),
                component.guild_id.map(|id| id.get()),
                Some(component.channel_id.get()),
                draft.task_id.as_deref(),
                &json!({
                    "action_id": &action_id,
                    "method": draft.method,
                    "risk": draft.risk.audit_label(),
                    "authorization": policy.authorization.audit_label(),
                    "confirmation": policy.confirmation.audit_label(),
                    "params_hash": stable_value_hash(&draft.params)
                }),
            )
            .await?;
        let result = self
            .state
            .codex
            .request_value(&draft.method, draft.params.clone())
            .await?;
        let result_controls = if draft.method == "plugin/install" {
            components::plugin_auth_buttons(plugin_auth_links(&result))
        } else {
            Vec::new()
        };
        if let Err(error) = self
            .state
            .store
            .audit(
                "schema_action.outcome",
                Some(component.user.id.get()),
                component.guild_id.map(|id| id.get()),
                Some(component.channel_id.get()),
                draft.task_id.as_deref(),
                &json!({
                    "action_id": &action_id,
                    "method": draft.method,
                    "risk": draft.risk.audit_label(),
                    "authorization": policy.authorization.audit_label(),
                    "confirmation": policy.confirmation.audit_label(),
                    "params_hash": stable_value_hash(&draft.params),
                    "accepted": true
                }),
            )
            .await
        {
            error!(%error, %action_id, "action succeeded but outcome audit could not be persisted");
        }
        let redacted = crate::security::redact_detail(result);
        let pretty = serde_json::to_string_pretty(&redacted)?;
        let pages = embeds::split_answer(&pretty, 3800);
        let result_title = draft.renderer.result_title();
        let mut response = EditInteractionResponse::new()
            .embed(embeds::codex_answer(
                &format!("{}: {}", result_title, draft.label),
                pages.first().map(String::as_str).unwrap_or("{}"),
                TaskState::Idle,
                1,
                pages.len().max(1),
            ))
            .components(result_controls);
        if pages.len() > 1 {
            response = response.new_attachment(CreateAttachment::bytes(
                pretty.into_bytes(),
                "codex-action-result.redacted.json",
            ));
        }
        // The component was deferred ephemerally, so editing the original
        // interaction keeps both the embed and any attachment owner-only.
        component.edit_response(http, response).await?;
        Ok(())
    }

    async fn cancel_action(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
    ) -> Result<()> {
        self.state.action_drafts.remove(token);
        component
            .create_response(
                http,
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .content("Action cancelled.")
                        .components(Vec::new()),
                ),
            )
            .await?;
        Ok(())
    }

    async fn show_tasks(
        &self,
        http: &Http,
        responder: CommandResponder<'_>,
        search: Option<String>,
        archived: bool,
        page: usize,
    ) -> Result<()> {
        responder.defer(http).await?;
        let mut browser = TaskBrowserDraft {
            search,
            archived,
            cursors: vec![None],
            page: 0,
            created_at: Utc::now(),
        };
        for _ in 0..page {
            let (_, _, next) = self
                .task_page_view(
                    browser.cursors[browser.page].clone(),
                    browser.search.as_deref(),
                    browser.archived,
                    browser.page,
                )
                .await?;
            let Some(next) = next else { break };
            browser.cursors.push(Some(next));
            browser.page += 1;
        }
        let (embed, mut rows, next_cursor) = self
            .task_page_view(
                browser.cursors[browser.page].clone(),
                browser.search.as_deref(),
                browser.archived,
                browser.page,
            )
            .await?;
        if let Some(next) = next_cursor.clone()
            && browser.cursors.len() == browser.page + 1
        {
            browser.cursors.push(Some(next));
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let current_page = browser.page;
        // Every open browser costs one map entry until it is swept here;
        // page_tasks only checks expiry for tokens that are clicked again.
        self.state
            .task_browsers
            .retain(|_, browser| !browser.expired());
        self.state.task_browsers.insert(token.clone(), browser);
        rows.push(components::task_browser_navigation(
            &token,
            current_page > 0,
            next_cursor.is_some(),
        ));
        responder.respond(http, embed, rows).await
    }

    async fn page_tasks(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
        direction: &str,
    ) -> Result<()> {
        component
            .create_response(http, CreateInteractionResponse::Acknowledge)
            .await?;
        let mut browser = self
            .state
            .task_browsers
            .get(token)
            .map(|entry| entry.clone())
            .context("task browser expired; run /tasks again")?;
        anyhow::ensure!(!browser.expired(), "task browser expired; run /tasks again");
        browser.page = match direction {
            "next" => {
                anyhow::ensure!(
                    browser.page + 1 < browser.cursors.len(),
                    "no next task page is available"
                );
                browser.page + 1
            }
            "prev" => browser.page.saturating_sub(1),
            _ => anyhow::bail!("invalid task browser direction"),
        };
        let (embed, mut rows, next_cursor) = self
            .task_page_view(
                browser.cursors[browser.page].clone(),
                browser.search.as_deref(),
                browser.archived,
                browser.page,
            )
            .await?;
        if let Some(next) = next_cursor.clone()
            && browser.cursors.len() == browser.page + 1
        {
            browser.cursors.push(Some(next));
        }
        rows.push(components::task_browser_navigation(
            token,
            browser.page > 0,
            next_cursor.is_some(),
        ));
        self.state.task_browsers.insert(token.to_owned(), browser);
        component
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embed).components(rows),
            )
            .await?;
        Ok(())
    }

    async fn task_page_view(
        &self,
        cursor: Option<String>,
        search: Option<&str>,
        archived: bool,
        page: usize,
    ) -> Result<(CreateEmbed, Vec<CreateActionRow>, Option<String>)> {
        let result = self
            .state
            .codex
            .thread_list(ThreadListParams {
                cursor,
                limit: Some(25),
                search_term: search.map(str::to_owned),
                archived: Some(archived),
                ..Default::default()
            })
            .await?;
        let next_cursor = result
            .get("nextCursor")
            .or_else(|| result.get("next_cursor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let threads = result
            .get("data")
            .or_else(|| result.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let options = threads.iter().take(25).filter_map(|thread| {
            let id = thread.get("id")?.as_str()?.to_owned();
            let title = thread
                .get("name")
                .or_else(|| thread.get("preview"))
                .and_then(Value::as_str)
                .unwrap_or("Untitled Codex task")
                .chars()
                .take(90)
                .collect();
            let status = thread
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("Ready")
                .to_owned();
            Some((id, title, status))
        });
        let embed = CreateEmbed::new()
            .title("Existing Codex tasks")
            .description(format!(
                "{} tasks on page {}. Choose one to materialize its private channel.{}",
                threads.len(),
                page + 1,
                if next_cursor.is_some() {
                    " More tasks exist; use Next."
                } else {
                    ""
                }
            ))
            .color(0x5865F2);
        let components = if threads.is_empty() {
            Vec::new()
        } else {
            vec![components::task_select(options, archived)]
        };
        Ok((embed, components, next_cursor))
    }

    async fn show_status(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let tasks = self.state.store.tasks().await?;
        let context = self
            .security_context(
                http,
                command.user.id.get(),
                command.guild_id,
                command.channel_id,
            )
            .await;
        let god_service = self.state.god.read().await.clone();
        let task_id = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .map(|task| task.thread_id);
        let god = match task_id.as_deref() {
            Some(task_id) => god_service
                .active_session(&context, Some(task_id))
                .await
                .is_some(),
            None => god_service.active_global_session().await.is_some(),
        };
        let dead_outbox = self
            .state
            .store
            .dead_outbox_count()
            .await
            .unwrap_or_else(|error| {
                warn!(%error, "dead outbox count unavailable for status card");
                0
            });
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::runner_card(
                    self.state.codex.is_connected(),
                    Some(env!("CARGO_PKG_VERSION")),
                    &tasks,
                    god,
                    dead_outbox,
                )),
            )
            .await?;
        Ok(())
    }

    async fn open_selected_task(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        archived: bool,
    ) -> Result<()> {
        let ComponentInteractionDataKind::StringSelect { values } = &component.data.kind else {
            anyhow::bail!("task picker returned wrong component kind");
        };
        let thread_id = values.first().context("no task selected")?;
        component
            .create_response(
                http,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                ),
            )
            .await?;
        if archived {
            // The selection came from an archived listing: reopen the thread
            // in Codex first so newly started turns are accepted again.
            self.require_installed_method("thread/unarchive")?;
            self.state.codex.thread_unarchive(thread_id).await?;
        }
        if let Some(task) = self.state.store.task(thread_id).await?
            && let Some(channel_id) = task.channel_id
        {
            if ChannelId::new(channel_id).to_channel(http).await.is_ok() {
                return edit_component_card(
                    http,
                    component,
                    "Task ready",
                    &format!(
                        "{}Open <#{channel_id}>",
                        if archived { "Task unarchived. " } else { "" }
                    ),
                )
                .await;
            }
            self.state.store.detach_channel(thread_id).await?;
        }
        let read = self
            .state
            .codex
            .request_value(
                "thread/read",
                json!({"threadId":thread_id,"includeTurns":true}),
            )
            .await?;
        let thread = read.get("thread").unwrap_or(&read);
        let title = thread
            .get("name")
            .or_else(|| thread.get("preview"))
            .and_then(Value::as_str)
            .unwrap_or("Existing Codex task");
        let cwd = thread.get("cwd").and_then(Value::as_str).map(str::to_owned);
        let layout = self
            .state
            .layout
            .read()
            .await
            .clone()
            .context("layout unavailable")?;
        let channel = provision::create_task_channel_for_state(
            http,
            &self.state.config,
            &self.state.store,
            &layout,
            title,
            thread_id,
            TaskState::Idle,
        )
        .await?;
        let task = TaskMirror {
            thread_id: thread_id.clone(),
            channel_id: Some(channel.get()),
            title: title.to_owned(),
            cwd,
            state: TaskState::Idle,
            turn_id: None,
            model: thread
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            last_event_at: Some(Utc::now()),
        };
        self.state.store.upsert_task(&task).await?;
        channel
            .send_message(
                http,
                CreateMessage::new()
                    .embed(embeds::task_card(
                        &task,
                        Some("Existing Codex Desktop task opened here."),
                    ))
                    .components(components::task_buttons(false)),
            )
            .await?;
        component
            .edit_response(
                http,
                EditInteractionResponse::new().content(format!("Opened <#{channel}>")),
            )
            .await?;
        Ok(())
    }

    async fn resolve_approval(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        request_id: &str,
        decision: &str,
    ) -> Result<()> {
        let offered_index = decision
            .strip_prefix("offered:")
            .map(str::parse::<usize>)
            .transpose()
            .context("invalid offered approval index")?;
        anyhow::ensure!(
            offered_index.is_some()
                || matches!(
                    decision,
                    "accept" | "acceptForSession" | "decline" | "cancel"
                ),
            "unknown approval decision"
        );
        component
            .create_response(http, CreateInteractionResponse::Acknowledge)
            .await?;
        let (_, request) = self
            .state
            .pending_server_requests
            .remove(request_id)
            .context("request expired; wait for Codex to ask again")?;
        self.state.pending_request_started.remove(request_id);
        let card = self
            .state
            .pending_request_messages
            .remove(request_id)
            .map(|(_, value)| value)
            .unwrap_or((component.channel_id.get(), component.message.id.get()));
        let method = ServerRequestMethod::classify(&request.method);
        let reply = match method {
            ServerRequestMethod::CommandExecutionApproval
            | ServerRequestMethod::FileChangeApproval => {
                if let Some(index) = offered_index {
                    requests::approval_reply_from_offered(&request, index)?
                } else {
                    requests::approval_reply(
                        &request,
                        match decision {
                            "accept" => ApprovalDecision::Accept,
                            "acceptForSession" => ApprovalDecision::AcceptForSession,
                            "decline" => ApprovalDecision::Decline,
                            "cancel" => ApprovalDecision::Cancel,
                            _ => unreachable!("decision validated above"),
                        },
                    )?
                }
            }
            ServerRequestMethod::LegacyApplyPatchApproval
            | ServerRequestMethod::LegacyExecCommandApproval => {
                anyhow::ensure!(
                    offered_index.is_none(),
                    "legacy approval has no offered index"
                );
                requests::legacy_approval_reply(
                    &request,
                    match decision {
                        "accept" => LegacyApprovalDecision::Approved,
                        "acceptForSession" => LegacyApprovalDecision::ApprovedForSession,
                        "decline" => LegacyApprovalDecision::Denied,
                        "cancel" => LegacyApprovalDecision::Abort,
                        _ => unreachable!("decision validated above"),
                    },
                )?
            }
            ServerRequestMethod::PermissionsApproval => {
                anyhow::ensure!(
                    offered_index.is_none(),
                    "permission approval has no offered index"
                );
                let (permissions, scope) = match decision {
                    "decline" | "cancel" => (serde_json::Map::new(), PermissionScope::Turn),
                    "accept" => (
                        request
                            .params
                            .get("permissions")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default(),
                        PermissionScope::Turn,
                    ),
                    "acceptForSession" => (
                        request
                            .params
                            .get("permissions")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default(),
                        PermissionScope::Session,
                    ),
                    _ => unreachable!("decision validated above"),
                };
                requests::permissions_reply(
                    &request,
                    PermissionGrant {
                        permissions,
                        scope,
                        strict_auto_review: None,
                    },
                )?
            }
            ServerRequestMethod::McpElicitation => {
                anyhow::ensure!(offered_index.is_none(), "MCP approval has no offered index");
                match decision {
                    "decline" => requests::mcp_elicitation_reply(
                        &request,
                        McpElicitationAction::Decline,
                        None,
                    )?,
                    "cancel" => requests::mcp_elicitation_reply(
                        &request,
                        McpElicitationAction::Cancel,
                        None,
                    )?,
                    "accept"
                        if request.params.get("mode").and_then(Value::as_str) == Some("url") =>
                    {
                        requests::mcp_elicitation_reply(
                            &request,
                            McpElicitationAction::Accept,
                            None,
                        )?
                    }
                    "accept" if mcp_elicitation_has_empty_form_schema(&request) => {
                        requests::mcp_elicitation_reply(
                            &request,
                            McpElicitationAction::Accept,
                            Some(json!({})),
                        )?
                    }
                    "accept" => {
                        anyhow::bail!("MCP form requires an answer before it can be accepted")
                    }
                    "acceptForSession" => {
                        anyhow::bail!("MCP elicitation has no session-wide acceptance")
                    }
                    _ => unreachable!("decision validated above"),
                }
            }
            ServerRequestMethod::ToolUserInput => {
                anyhow::ensure!(offered_index.is_none(), "user input has no offered index");
                anyhow::ensure!(
                    matches!(decision, "decline" | "cancel"),
                    "typed user input must be answered through its form"
                );
                ServerReply::Error {
                    id: request.id.clone(),
                    error: RpcErrorObject {
                        code: -32_000,
                        message: "user declined the Discord input request".to_owned(),
                        data: None,
                    },
                }
            }
            _ => requests::unsupported_reply(&request),
        };
        if let Err(error) = send_server_reply(&self.state.codex, request.generation, reply).await {
            restore_pending_request(&self.state, request_id, &request, Some(card));
            component
                .channel_id
                .edit_message(
                    http,
                    component.message.id,
                    EditMessage::new().embed(embeds::error_card(
                        "Request still pending",
                        "Codex did not accept the decision. Try the button again.",
                    )),
                )
                .await?;
            return Err(error);
        }
        if let Err(error) = self.state.store.remove_pending_request(request_id).await {
            warn!(request_id, %error, "resolved pending request row could not be removed");
        }
        component
            .channel_id
            .edit_message(
                http,
                card.1,
                EditMessage::new()
                    .embed(
                        CreateEmbed::new()
                            .title("Request resolved")
                            .description(if offered_index.is_some() {
                                "Decision: **server-offered policy choice**".to_owned()
                            } else {
                                format!("Decision: **{decision}**")
                            })
                            .color(
                                if offered_index.is_none()
                                    && matches!(decision, "decline" | "cancel")
                                {
                                    0xED4245
                                } else {
                                    0x57F287
                                },
                            ),
                    )
                    .components(Vec::new()),
            )
            .await?;
        if let Some(thread_id) = request_thread_id(&request) {
            reactivate_unblocked_task(&self.state, http, &thread_id).await?;
        }
        Ok(())
    }

    async fn answer_server_request(
        &self,
        http: &Http,
        modal: &ModalInteraction,
        token: &str,
    ) -> Result<()> {
        defer_modal(http, modal).await?;
        let (_, request) = self
            .state
            .pending_server_requests
            .remove(token)
            .context("request expired; wait for Codex to ask again")?;
        self.state.pending_request_started.remove(token);
        let card = self
            .state
            .pending_request_messages
            .remove(token)
            .map(|(_, value)| value);
        let reply_result: Result<ServerReply> = (|| {
            Ok(match ServerRequestMethod::classify(&request.method) {
                ServerRequestMethod::ToolUserInput => {
                    let answers = parse_user_input_modal_answers(&request, modal)?;
                    requests::user_input_reply(&request, answers)?
                }
                ServerRequestMethod::McpElicitation => {
                    let content = parse_mcp_modal_content(&request, modal)?;
                    requests::mcp_elicitation_reply(
                        &request,
                        McpElicitationAction::Accept,
                        Some(content),
                    )?
                }
                _ => anyhow::bail!("this request does not accept a typed answer"),
            })
        })();
        let reply = match reply_result {
            Ok(reply) => reply,
            Err(error) => {
                restore_pending_request(&self.state, token, &request, card);
                return Err(error);
            }
        };
        if let Err(error) = send_server_reply(&self.state.codex, request.generation, reply).await {
            restore_pending_request(&self.state, token, &request, card);
            return Err(error);
        }
        if let Err(error) = self.state.store.remove_pending_request(token).await {
            warn!(token, %error, "answered pending request row could not be removed");
        }
        if let Some((channel_id, message_id)) = card.or_else(|| {
            modal
                .message
                .as_ref()
                .map(|message| (message.channel_id.get(), message.id.get()))
        }) {
            ChannelId::new(channel_id)
                .edit_message(
                    http,
                    message_id,
                    EditMessage::new()
                        .embed(
                            CreateEmbed::new()
                                .title("Request answered")
                                .description("Your answer was sent to Codex.")
                                .color(0x57F287),
                        )
                        .components(Vec::new()),
                )
                .await?;
        }
        if let Some(thread_id) = request_thread_id(&request) {
            reactivate_unblocked_task(&self.state, http, &thread_id).await?;
        }
        modal
            .edit_response(
                http,
                EditInteractionResponse::new().content("Answer sent to Codex."),
            )
            .await?;
        Ok(())
    }

    async fn interrupt(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        self.interrupt_task(&task).await?;
        edit_command_card(http, command, "Interrupt", "Turn interrupted.").await
    }
    async fn interrupt_component(
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<()> {
        defer_component(http, component).await?;
        let task = self.task_for_channel(component.channel_id).await?;
        self.interrupt_task(&task).await?;
        edit_component_card(http, component, "Interrupt", "Turn interrupted.").await
    }
    async fn interrupt_task(&self, task: &TaskMirror) -> Result<()> {
        let turn_id = task.turn_id.clone().context("task has no active turn")?;
        self.state
            .codex
            .turn_interrupt(TurnInterruptParams {
                thread_id: task.thread_id.clone(),
                turn_id,
            })
            .await?;
        Ok(())
    }

    async fn fork(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let channel = self.fork_task(http, command.channel_id).await?;
        edit_command_card(
            http,
            command,
            "Fork created",
            &format!("Fork opened in <#{channel}>"),
        )
        .await
    }
    async fn fork_component(&self, http: &Http, component: &ComponentInteraction) -> Result<()> {
        defer_component(http, component).await?;
        let channel = self.fork_task(http, component.channel_id).await?;
        edit_component_card(
            http,
            component,
            "Fork created",
            &format!("Fork opened in <#{channel}>"),
        )
        .await
    }
    async fn fork_task(&self, http: &Http, source_channel: ChannelId) -> Result<ChannelId> {
        let task = self.task_for_channel(source_channel).await?;
        let result = self
            .state
            .codex
            .thread_fork(ThreadForkParams {
                thread_id: task.thread_id.clone(),
                cwd: task.cwd.clone(),
                model: task.model.clone(),
                approval_policy: Some(CodexExecutionPolicy::NORMAL.approval_policy.into()),
                sandbox: Some(CodexExecutionPolicy::NORMAL.thread_sandbox()),
                last_turn_id: task.turn_id.clone(),
                extra: BTreeMap::new(),
            })
            .await?;
        let thread_id = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("fork response missing id")?;
        let layout = self
            .state
            .layout
            .read()
            .await
            .clone()
            .context("layout unavailable")?;
        let title = format!("Fork — {}", task.title);
        let channel = provision::create_task_channel_for_state(
            http,
            &self.state.config,
            &self.state.store,
            &layout,
            &title,
            thread_id,
            TaskState::Idle,
        )
        .await?;
        self.state
            .store
            .upsert_task(&TaskMirror {
                thread_id: thread_id.to_owned(),
                channel_id: Some(channel.get()),
                title: title.clone(),
                cwd: task.cwd,
                state: TaskState::Idle,
                turn_id: None,
                model: task.model,
                last_event_at: Some(Utc::now()),
            })
            .await?;
        channel
            .send_message(
                http,
                CreateMessage::new()
                    .embed(
                        CreateEmbed::new()
                            .title(title)
                            .description(format!("Forked from <#{source_channel}>."))
                            .color(0x5865F2),
                    )
                    .components(components::task_buttons(false)),
            )
            .await?;
        Ok(channel)
    }

    async fn archive(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        self.archive_task(http, command.channel_id).await?;
        edit_command_card(
            http,
            command,
            "Task archived",
            "Codex task archived. Discord mirror retained until pruning.",
        )
        .await
    }
    async fn archive_component(&self, http: &Http, component: &ComponentInteraction) -> Result<()> {
        defer_component(http, component).await?;
        self.archive_task(http, component.channel_id).await?;
        edit_component_card(
            http,
            component,
            "Task archived",
            "Codex task archived. Discord mirror retained until pruning.",
        )
        .await
    }
    async fn archive_task(&self, http: &Http, channel: ChannelId) -> Result<()> {
        let mut task = self.task_for_channel(channel).await?;
        self.state.codex.thread_archive(&task.thread_id).await?;
        task.state = TaskState::Done;
        self.state.store.upsert_task(&task).await?;
        provision::move_task_to_state(http, &self.state.config, channel, TaskState::Done).await?;
        refresh_task_card(http, &task, Some("Task archived.")).await
    }

    async fn rename(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let name = string_command_option(command, "name")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("name required")?
            .chars()
            .take(80)
            .collect::<String>();
        defer_command(http, command).await?;
        let mut task = self.task_for_channel(command.channel_id).await?;
        self.state
            .codex
            .thread_name_set(&task.thread_id, &name)
            .await?;
        provision::rename_task_channel(http, command.channel_id, &name, &task.thread_id).await?;
        task.title = name.clone();
        task.last_event_at = Some(Utc::now());
        self.state.store.upsert_task(&task).await?;
        refresh_task_card(http, &task, Some("Task renamed.")).await?;
        edit_command_card(
            http,
            command,
            "Task renamed",
            &format!("Task renamed to **{name}**."),
        )
        .await
    }

    async fn rollback(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let turns = integer_command_option(command, "turns").context("turns required")?;
        anyhow::ensure!(
            (1..=100).contains(&turns),
            "turns must be between 1 and 100"
        );
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        anyhow::ensure!(
            task.state != TaskState::Running,
            "stop the active turn first"
        );
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.state
            .codex
            .thread_rollback(&task.thread_id, turns as u32)
            .await?;
        edit_command_card(
            http,
            command,
            "Rollback complete",
            &format!("Dropped {turns} turn(s). Local file changes are not reverted."),
        )
        .await
    }

    async fn compact(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        self.state
            .codex
            .thread_compact_start(&task.thread_id)
            .await?;
        edit_command_card(http, command, "Compaction", "Context compaction started.").await
    }

    async fn review(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let kind = string_command_option(command, "target").unwrap_or("uncommitted");
        let reference = string_command_option(command, "ref").map(str::trim);
        let target = match kind {
            "uncommitted" => ReviewTarget::UncommittedChanges,
            "branch" => ReviewTarget::BaseBranch {
                branch: reference
                    .filter(|value| !value.is_empty())
                    .context("branch requires ref")?
                    .to_owned(),
            },
            "commit" => ReviewTarget::Commit {
                sha: reference
                    .filter(|value| !value.is_empty())
                    .context("commit requires ref")?
                    .to_owned(),
                title: None,
            },
            "custom" => ReviewTarget::Custom {
                instructions: reference
                    .filter(|value| !value.is_empty())
                    .context("custom review requires instructions in ref")?
                    .to_owned(),
            },
            _ => anyhow::bail!("unknown review target"),
        };
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        anyhow::ensure!(
            task.state != TaskState::Running,
            "wait for the active turn first"
        );
        let result = self
            .state
            .codex
            .review_start(ReviewStartParams {
                thread_id: task.thread_id,
                target,
                delivery: None,
                extra: BTreeMap::new(),
            })
            .await?;
        let detail = serde_json::to_string_pretty(&result)?;
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::info_card(
                    "Code review started",
                    &format!(
                        "```json\n{}\n```",
                        detail.chars().take(3200).collect::<String>()
                    ),
                )),
            )
            .await?;
        Ok(())
    }

    async fn models(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        let result = self
            .state
            .codex
            .model_list(ModelListParams {
                limit: Some(25),
                ..Default::default()
            })
            .await?;
        let models = first_result_array(&result, &["data", "models", "items"]);
        let mut choices = Vec::new();
        for model in models.iter().take(24) {
            let id = model
                .get("id")
                .or_else(|| model.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let name = model
                .get("displayName")
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(id);
            let description = model
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Codex model")
                .chars()
                .take(90)
                .collect::<String>();
            choices.push((id.to_owned(), name.chars().take(90).collect(), description));
        }
        anyhow::ensure!(!choices.is_empty(), "Codex returned no selectable models");
        command
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(
                        "Model for this task",
                        &format!(
                            "Current: **{}**. Selection applies to newly started turns.",
                            task.model.as_deref().unwrap_or("Codex default")
                        ),
                    ))
                    .components(vec![components::model_select(
                        choices,
                        task.model.as_deref(),
                    )]),
            )
            .await?;
        Ok(())
    }

    async fn apply_model_selection(
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<()> {
        let choice = selected_value(component)?.to_owned();
        defer_update_component(http, component).await?;
        if choice != "__default__" {
            // Re-read the installed model list at click time. A stale Discord
            // menu must not persist a model that Codex no longer exposes.
            let result = self
                .state
                .codex
                .model_list(ModelListParams {
                    limit: Some(100),
                    ..Default::default()
                })
                .await?;
            let valid = first_result_array(&result, &["data", "models", "items"])
                .iter()
                .any(|model| {
                    model
                        .get("id")
                        .or_else(|| model.get("model"))
                        .and_then(Value::as_str)
                        == Some(choice.as_str())
                });
            anyhow::ensure!(
                valid,
                "selected model is no longer available; run /model again"
            );
        }
        let mut task = self.task_for_channel(component.channel_id).await?;
        task.model = (choice != "__default__").then_some(choice);
        task.last_event_at = Some(Utc::now());
        self.state.store.upsert_task(&task).await?;
        component
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(
                        "Model updated",
                        &task.model.as_deref().map_or_else(
                            || "New turns use the Codex default model.".to_owned(),
                            |model| format!("New turns use **{model}**."),
                        ),
                    ))
                    .components(Vec::new()),
            )
            .await?;
        Ok(())
    }

    async fn skills(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let query = string_command_option(command, "query")
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_ascii_lowercase);
        let requested_page = integer_command_option(command, "page").unwrap_or(1).max(1) as usize;
        let cwd = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .and_then(|task| task.cwd);
        let result = self
            .state
            .codex
            .skills_list(SkillsListParams {
                cwds: cwd.map(|cwd| vec![cwd]),
                force_reload: None,
            })
            .await?;
        let mut skills = collect_skills(&result);
        if let Some(query) = query.as_deref() {
            skills.retain(|skill| {
                ["name", "id", "description", "path"]
                    .into_iter()
                    .filter_map(|key| skill.get(key).and_then(Value::as_str))
                    .any(|value| value.to_ascii_lowercase().contains(query))
            });
        }
        skills.sort_by_key(|skill| {
            skill
                .get("name")
                .or_else(|| skill.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase()
        });
        let (start, end, page, pages) = page_window(skills.len(), requested_page, 25);
        let mut lines = String::new();
        for skill in &skills[start..end] {
            let name = skill
                .get("name")
                .or_else(|| skill.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed");
            let description = skill
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            lines.push_str(&format!(
                "**{name}** — {}\n",
                description.chars().take(120).collect::<String>()
            ));
        }
        if lines.is_empty() {
            lines.push_str("No skills discovered.");
        }
        lines.push_str(&format!(
            "\n\nPage {page}/{pages} · {} skills",
            skills.len()
        ));
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::info_card("Codex skills", &lines)),
            )
            .await?;
        Ok(())
    }

    async fn apps(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let query = string_command_option(command, "query")
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_ascii_lowercase);
        let requested_page = integer_command_option(command, "page").unwrap_or(1).max(1) as usize;
        let thread_id = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .map(|task| task.thread_id);
        let mut apps = collect_apps(&self.state.codex, thread_id, true).await?;
        if let Some(query) = query.as_deref() {
            apps.retain(|app| {
                app.get("name")
                    .or_else(|| app.get("id"))
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.to_ascii_lowercase().contains(query))
            });
        }
        apps.sort_by_key(|app| {
            let accessible = app
                .get("isAccessible")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let enabled = app
                .get("isEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let name = app
                .get("name")
                .or_else(|| app.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            (!accessible, !enabled, name)
        });
        let (start, end, page, pages) = page_window(apps.len(), requested_page, 25);
        let mut lines = String::new();
        for app in &apps[start..end] {
            let name = app
                .get("name")
                .or_else(|| app.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed");
            let enabled = app
                .get("isEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let accessible = app
                .get("isAccessible")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = if !enabled {
                "disabled"
            } else if accessible {
                "connected"
            } else {
                "not connected"
            };
            lines.push_str(&format!("**{name}** · {status}\n"));
        }
        if lines.is_empty() {
            lines.push_str("No apps/connectors available.");
        }
        lines.push_str(&format!("\n\nPage {page}/{pages} · {} apps", apps.len()));
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::info_card("Codex apps", &lines)),
            )
            .await?;
        Ok(())
    }

    async fn plugins(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let scope = string_command_option(command, "scope").unwrap_or("installed");
        let method = match scope {
            "installed" => "plugin/installed",
            "catalog" => "plugin/list",
            _ => anyhow::bail!("unknown plugin scope `{scope}`"),
        };
        self.require_installed_method(method)?;
        defer_command(http, command).await?;
        let query = string_command_option(command, "query")
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_ascii_lowercase);
        let requested_page = integer_command_option(command, "page").unwrap_or(1).max(1) as usize;
        let cwd = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .and_then(|task| task.cwd);
        let result = if scope == "installed" {
            self.state
                .codex
                .plugin_installed(PluginInstalledParams {
                    cwds: cwd.map(|cwd| vec![cwd]),
                    install_suggestion_plugin_names: None,
                })
                .await?
        } else {
            self.state
                .codex
                .plugin_list(PluginListParams {
                    cwds: cwd.map(|cwd| vec![cwd]),
                    marketplace_kinds: None,
                })
                .await?
        };
        let load_error_count = plugin_load_error_count(&result);
        let mut rows = plugin_catalog_rows(&result);
        if scope == "installed" {
            rows.retain(|row| row.installed);
        }
        if let Some(query) = query.as_deref() {
            rows.retain(|row| {
                [&row.name, &row.id, &row.marketplace, &row.description]
                    .into_iter()
                    .any(|value| value.to_ascii_lowercase().contains(query))
            });
        }
        rows.sort_by_key(|row| {
            (
                !row.installed,
                !row.enabled,
                row.name.to_ascii_lowercase(),
                row.marketplace.to_ascii_lowercase(),
            )
        });
        self.state
            .plugin_browsers
            .retain(|_, draft| !draft.expired());
        let token = uuid::Uuid::new_v4().simple().to_string();
        let draft = PluginBrowserDraft {
            rows,
            scope: scope.to_owned(),
            load_error_count,
            channel_id: command.channel_id.get(),
            owner_id: command.user.id.get(),
            created_at: Utc::now(),
        };
        let (embed, controls) = plugin_browser_view(&draft, requested_page, &token);
        self.state.plugin_browsers.insert(token, draft);
        command
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embed)
                    .components(controls),
            )
            .await?;
        Ok(())
    }

    fn plugin_browser_row(
        &self,
        component: &ComponentInteraction,
        token: &str,
        index: usize,
    ) -> Result<PluginCatalogRow> {
        let draft = self
            .state
            .plugin_browsers
            .get(token)
            .map(|draft| draft.clone())
            .context("plugin browser expired; run /plugins again")?;
        anyhow::ensure!(
            !draft.expired(),
            "plugin browser expired; run /plugins again"
        );
        anyhow::ensure!(
            draft.channel_id == component.channel_id.get()
                && draft.owner_id == component.user.id.get(),
            "plugin browser belongs to another user or channel"
        );
        draft
            .rows
            .get(index)
            .cloned()
            .context("plugin selection is no longer available")
    }

    async fn show_plugin_detail(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
        index: usize,
    ) -> Result<()> {
        self.require_installed_method("plugin/read")?;
        let row = self.plugin_browser_row(component, token, index)?;
        defer_component(http, component).await?;
        let result = self
            .state
            .codex
            .plugin_read(PluginLocatorParams {
                plugin_name: row.name.clone(),
                marketplace_path: row.marketplace_path.clone(),
                remote_marketplace_name: row.remote_marketplace_name.clone(),
            })
            .await?;
        let detail = result.get("plugin").unwrap_or(&result);
        let summary = detail.get("summary").unwrap_or(detail);
        let auth = summary
            .get("authPolicy")
            .and_then(Value::as_str)
            .unwrap_or("unknown auth policy");
        let install_policy = summary
            .get("installPolicy")
            .and_then(Value::as_str)
            .unwrap_or("unknown install policy");
        let source = summary
            .pointer("/source/type")
            .and_then(Value::as_str)
            .unwrap_or("unknown source");
        let counts = [
            ("apps", detail.get("apps")),
            ("skills", detail.get("skills")),
            ("MCP servers", detail.get("mcpServers")),
            ("hooks", detail.get("hooks")),
            ("scheduled tasks", detail.get("scheduledTasks")),
        ]
        .into_iter()
        .map(|(label, value)| {
            format!(
                "{} {label}",
                value.and_then(Value::as_array).map_or(0, Vec::len)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
        let description = detail
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(&row.description)
            .replace('\n', " ");
        let lines = format!(
            "{}\n\n**Marketplace:** {}\n**Status:** {} · {}\n**Source:** {}\n**Capabilities:** {}",
            description.chars().take(900).collect::<String>(),
            row.marketplace,
            auth,
            install_policy,
            source,
            counts
        );
        component
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(&format!("Plugin: {}", row.name), &lines))
                    .components(components::plugin_detail_buttons(
                        token,
                        index,
                        plugin_available_mutation(&row),
                    )),
            )
            .await?;
        Ok(())
    }

    async fn show_plugin_browser_page(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
        index: usize,
    ) -> Result<()> {
        self.show_plugin_browser_page_number(http, component, token, index / 10 + 1)
            .await
    }

    async fn show_plugin_browser_page_number(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        token: &str,
        page: usize,
    ) -> Result<()> {
        let draft = self
            .state
            .plugin_browsers
            .get(token)
            .map(|draft| draft.clone())
            .context("plugin browser expired; run /plugins again")?;
        anyhow::ensure!(
            !draft.expired()
                && draft.channel_id == component.channel_id.get()
                && draft.owner_id == component.user.id.get(),
            "plugin browser expired or belongs to another user/channel"
        );
        let (embed, controls) = plugin_browser_view(&draft, page, token);
        component
            .create_response(
                http,
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(embed)
                        .components(controls),
                ),
            )
            .await?;
        Ok(())
    }

    async fn begin_plugin_mutation(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        browser_token: &str,
        index: usize,
        install: bool,
    ) -> Result<()> {
        let row = self.plugin_browser_row(component, browser_token, index)?;
        anyhow::ensure!(
            plugin_available_mutation(&row) == Some(install),
            "this plugin cannot be changed under its current marketplace policy"
        );
        anyhow::ensure!(
            row.installed != install,
            if install {
                "plugin is already installed"
            } else {
                "plugin is not installed"
            }
        );
        let method = if install {
            "plugin/install"
        } else {
            "plugin/uninstall"
        };
        let capability = self
            .state
            .capabilities
            .client_request(method)
            .with_context(|| format!("Codex no longer advertises {method}"))?;
        let task = self
            .state
            .store
            .task_by_channel(component.channel_id.get())
            .await?;
        let mut draft = ActionDraft::from_capability(
            capability,
            task.as_ref().map(|task| task.thread_id.clone()),
            component.channel_id.get(),
            task.as_ref()
                .and_then(|task| task.cwd.as_deref())
                .or_else(|| {
                    self.state
                        .config
                        .default_cwd
                        .as_deref()
                        .and_then(|path| path.to_str())
                }),
        );
        draft.params = plugin_mutation_params(&row, install);
        draft.fields.clear();
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.state
            .action_drafts
            .insert(token.clone(), draft.clone());
        let (embed, controls) = action_confirmation(&draft, &token);
        component
            .create_response(
                http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .embed(embed)
                        .components(controls)
                        .ephemeral(true),
                ),
            )
            .await?;
        Ok(())
    }

    async fn config_summary(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let cwd = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .and_then(|task| task.cwd);
        let result = self
            .state
            .codex
            .config_read(ConfigReadParams {
                cwd,
                include_layers: Some(false),
            })
            .await?;
        self.edit_redacted_json(
            http,
            command,
            "Effective Codex config",
            "codex-config.json",
            result,
        )
        .await
    }

    async fn account(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        let result = self
            .state
            .codex
            .account_read(AccountReadParams::default())
            .await?;
        self.edit_redacted_json(http, command, "Codex account", "codex-account.json", result)
            .await
    }

    async fn usage(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        // Each half is independently optional: the auto-updating installed
        // bundle may expose only one of these read methods.
        let rate_limits = if self.installed_method("account/rateLimits/read") {
            self.state
                .codex
                .account_rate_limits_read()
                .await
                .unwrap_or_else(|error| json!({"error": error.to_string()}))
        } else {
            json!({"unsupported": "not exposed by the installed Codex"})
        };
        let usage = if self.installed_method("account/usage/read") {
            self.state
                .codex
                .account_usage_read()
                .await
                .unwrap_or_else(|error| json!({"error": error.to_string()}))
        } else {
            json!({"unsupported": "not exposed by the installed Codex"})
        };
        let combined = json!({"rateLimits": rate_limits, "usage": usage});
        self.edit_redacted_json(http, command, "Codex usage", "codex-usage.json", combined)
            .await
    }

    async fn capabilities(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        defer_command(http, command).await?;
        if let Some(method) = string_command_option(command, "method") {
            let capability = self
                .state
                .capabilities
                .client_request(method)
                .context("unknown installed method")?;
            let mut detail = capability.description.clone().unwrap_or_default();
            if let Some(schema) = capability.params_schema.as_ref()
                && let Some(guidance) = crate::codex::params::guidance(schema)
            {
                detail.push_str("\n\n**Parameters**\n");
                detail.push_str(&guidance);
            }
            command
                .edit_response(
                    http,
                    EditInteractionResponse::new().embed(embeds::info_card(method, &detail)),
                )
                .await?;
        } else {
            let report = CoverageReport::compute(&self.state.capabilities);
            command
                .edit_response(
                    http,
                    EditInteractionResponse::new().embed(embeds::coverage_card(
                        &report,
                        "the installed Codex schema generated at startup",
                    )),
                )
                .await?;
        }
        Ok(())
    }

    fn installed_method(&self, method: &str) -> bool {
        self.state.capabilities.client_request(method).is_some()
    }

    /// Fail fast with a clear message when the auto-updating installed Codex
    /// bundle does not expose a method this command depends on.
    fn require_installed_method(&self, method: &str) -> Result<()> {
        anyhow::ensure!(
            self.installed_method(method),
            "the installed Codex app-server does not expose `{method}`; update Codex Desktop and restart the relay"
        );
        Ok(())
    }

    async fn goal(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let objective = string_command_option(command, "objective")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let budget = integer_command_option(command, "budget");
        let clear = boolean_command_option(command, "clear").unwrap_or(false);
        anyhow::ensure!(
            !(clear && (objective.is_some() || budget.is_some())),
            "clear cannot be combined with a new objective or budget"
        );
        anyhow::ensure!(
            budget.is_none() || objective.is_some(),
            "budget requires an objective"
        );
        self.require_installed_method(if clear {
            "thread/goal/clear"
        } else if objective.is_some() {
            "thread/goal/set"
        } else {
            "thread/goal/get"
        })?;
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        if clear {
            self.state.codex.thread_goal_clear(&task.thread_id).await?;
            return edit_command_card(http, command, "Task goal", "Task goal cleared.").await;
        }
        if let Some(objective) = objective {
            let result = self
                .state
                .codex
                .thread_goal_set(ThreadGoalSetParams {
                    thread_id: task.thread_id.clone(),
                    objective: Some(objective),
                    status: None,
                    token_budget: budget,
                })
                .await?;
            let goal = result.get("goal").unwrap_or(&result);
            command
                .edit_response(
                    http,
                    EditInteractionResponse::new().embed(embeds::goal_card(goal)),
                )
                .await?;
            return Ok(());
        }
        let result = self.state.codex.thread_goal_get(&task.thread_id).await?;
        match result.get("goal").filter(|goal| !goal.is_null()) {
            Some(goal) => {
                command
                    .edit_response(
                        http,
                        EditInteractionResponse::new().embed(embeds::goal_card(goal)),
                    )
                    .await?;
            }
            None => {
                edit_command_card(
                    http,
                    command,
                    "Task goal",
                    "No goal is set for this task. Use `/goal objective:<text>` to set one.",
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn terminals(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        self.require_installed_method("thread/backgroundTerminals/list")?;
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        let result = self
            .state
            .codex
            .background_terminals_list(BackgroundTerminalsListParams {
                thread_id: task.thread_id.clone(),
                cursor: None,
                limit: Some(25),
            })
            .await?;
        let terminals = first_result_array(&result, &["data", "items"]);
        let mut lines = String::new();
        let mut options = Vec::new();
        for terminal in terminals.iter().take(25) {
            let process_id = terminal
                .get("processId")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let terminal_command = terminal
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("(unknown command)");
            let os_pid = terminal
                .get("osPid")
                .and_then(Value::as_u64)
                .map(|pid| format!(" · pid {pid}"))
                .unwrap_or_default();
            let cpu = terminal
                .get("cpuPercent")
                .and_then(Value::as_f64)
                .map(|cpu| format!(" · {cpu:.0}% CPU"))
                .unwrap_or_default();
            let memory = terminal
                .get("rssKb")
                .and_then(Value::as_u64)
                .map(|kb| format!(" · {} MiB", kb / 1024))
                .unwrap_or_default();
            lines.push_str(&format!(
                "🖥 `{}`{os_pid}{cpu}{memory}\n",
                terminal_command.chars().take(160).collect::<String>()
            ));
            options.push((
                process_id.to_owned(),
                terminal_command.to_owned(),
                format!("{process_id}{os_pid}"),
            ));
        }
        if lines.is_empty() {
            lines.push_str("No background terminals are running for this task.");
        }
        push_more_indicator(&mut lines, terminals.len(), 25);
        // Controls appear only when the installed bundle can honor them.
        if !self.installed_method("thread/backgroundTerminals/terminate") {
            options.clear();
        }
        let can_clean = self.installed_method("thread/backgroundTerminals/clean");
        command
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card("Background terminals", &lines))
                    .components(components::terminal_rows(options, can_clean)),
            )
            .await?;
        Ok(())
    }

    async fn terminate_terminal(
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<()> {
        self.require_installed_method("thread/backgroundTerminals/terminate")?;
        let process_id = selected_value(component)?.to_owned();
        defer_update_component(http, component).await?;
        let task = self.task_for_channel(component.channel_id).await?;
        let result = self
            .state
            .codex
            .background_terminal_terminate(BackgroundTerminalTerminateParams {
                thread_id: task.thread_id,
                process_id: process_id.clone(),
            })
            .await?;
        let terminated = result
            .get("terminated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        component
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(
                        "Background terminal",
                        &if terminated {
                            format!("Terminated `{process_id}`. Run /terminals to refresh.")
                        } else {
                            format!(
                                "Codex reports `{process_id}` was not running anymore. Run /terminals to refresh."
                            )
                        },
                    ))
                    .components(Vec::new()),
            )
            .await?;
        Ok(())
    }

    async fn clean_terminals(&self, http: &Http, component: &ComponentInteraction) -> Result<()> {
        self.require_installed_method("thread/backgroundTerminals/clean")?;
        defer_update_component(http, component).await?;
        let task = self.task_for_channel(component.channel_id).await?;
        self.state
            .codex
            .background_terminals_clean(&task.thread_id)
            .await?;
        component
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(
                        "Background terminals",
                        "Finished terminals cleaned up. Run /terminals to refresh.",
                    ))
                    .components(Vec::new()),
            )
            .await?;
        Ok(())
    }

    async fn history(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        self.require_installed_method("thread/turns/list")?;
        let limit = integer_command_option(command, "turns")
            .unwrap_or(5)
            .clamp(1, 10);
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let result = self
            .state
            .codex
            .thread_turns_list(ThreadTurnsListParams {
                thread_id: task.thread_id.clone(),
                cursor: None,
                limit: Some(limit as u32),
                items_view: Some("summary".into()),
                sort_direction: Some("desc".into()),
            })
            .await?;
        let turns = first_result_array(&result, &["data", "items"]);
        let mut lines = String::new();
        for turn in turns {
            lines.push_str(&embeds::describe_turn(turn));
            lines.push('\n');
        }
        if lines.is_empty() {
            lines.push_str("This task has no recorded turns yet.");
        }
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::info_card(
                    &format!("Last {} turn(s), newest first", turns.len()),
                    &lines,
                )),
            )
            .await?;
        Ok(())
    }

    async fn find_tasks(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let query = string_command_option(command, "query")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("query required")?
            .to_owned();
        let archived = boolean_command_option(command, "archived").unwrap_or(false);
        self.require_installed_method("thread/search")?;
        defer_command(http, command).await?;
        let result = self
            .state
            .codex
            .thread_search(ThreadSearchParams {
                search_term: query.clone(),
                archived: Some(archived),
                cursor: None,
                limit: Some(10),
            })
            .await?;
        let matches = first_result_array(&result, &["data", "items"]);
        let mut lines = String::new();
        let mut options = Vec::new();
        for entry in matches.iter().take(10) {
            let thread = entry.get("thread").unwrap_or(entry);
            let Some(id) = thread.get("id").and_then(Value::as_str) else {
                continue;
            };
            let title = thread
                .get("name")
                .or_else(|| thread.get("preview"))
                .and_then(Value::as_str)
                .unwrap_or("Untitled Codex task");
            let snippet = entry
                .get("snippet")
                .and_then(Value::as_str)
                .unwrap_or("")
                .replace('\n', " ");
            lines.push_str(&format!(
                "**{}**\n   {}\n",
                title.chars().take(120).collect::<String>(),
                snippet.chars().take(240).collect::<String>()
            ));
            options.push((
                id.to_owned(),
                title.chars().take(90).collect::<String>(),
                snippet.chars().take(90).collect::<String>(),
            ));
        }
        if lines.is_empty() {
            lines = format!("No task content matched `{query}`.");
        }
        push_more_indicator(&mut lines, matches.len(), 10);
        let mut response = EditInteractionResponse::new().embed(embeds::info_card(
            &format!(
                "Content search: {}",
                query.chars().take(180).collect::<String>()
            ),
            &lines,
        ));
        if !options.is_empty() {
            response = response.components(vec![components::task_select(options, archived)]);
        }
        command.edit_response(http, response).await?;
        Ok(())
    }

    async fn mcp_status(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        self.require_installed_method("mcpServerStatus/list")?;
        defer_command(http, command).await?;
        let view = string_command_option(command, "view").unwrap_or("servers");
        let query = string_command_option(command, "query")
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_ascii_lowercase);
        let requested_page = integer_command_option(command, "page").unwrap_or(1).max(1) as usize;
        let thread_id = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .map(|task| task.thread_id);
        let servers = collect_mcp_servers(&self.state.codex, thread_id).await?;
        let mut rows = mcp_catalog_rows(&servers, view)?;
        if let Some(query) = query.as_deref() {
            rows.retain(|row| {
                [&row.primary, &row.secondary, &row.description]
                    .into_iter()
                    .any(|value| value.to_ascii_lowercase().contains(query))
            });
        }
        rows.sort_by_key(|row| {
            (
                row.secondary.to_ascii_lowercase(),
                row.primary.to_ascii_lowercase(),
            )
        });
        let (start, end, page, pages) = page_window(rows.len(), requested_page, 10);
        let mut lines = String::new();
        for row in &rows[start..end] {
            lines.push_str(&format!(
                "**{}** · {}\n   {}\n",
                row.primary.chars().take(120).collect::<String>(),
                row.secondary.chars().take(100).collect::<String>(),
                row.description.chars().take(120).collect::<String>()
            ));
        }
        if lines.is_empty() {
            lines.push_str("No matching MCP entries.");
        }
        lines.push_str(&format!(
            "\nPage {page}/{pages} · {} {}",
            rows.len(),
            mcp_view_label(view)
        ));
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::info_card(
                    &format!("MCP {}", mcp_view_label(view)),
                    &lines,
                )),
            )
            .await?;
        Ok(())
    }

    async fn collaboration_modes(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        self.require_installed_method("collaborationMode/list")?;
        self.require_installed_method("thread/settings/update")?;
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        let result = self.state.codex.collaboration_mode_list().await?;
        let presets = first_result_array(&result, &["data", "items"]);
        let mut options = Vec::new();
        for preset in presets.iter().take(25) {
            let Some(name) = preset.get("name").and_then(Value::as_str) else {
                continue;
            };
            let mode = preset
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let model = preset
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("task model");
            let effort = preset
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(|effort| format!(" · {effort}"))
                .unwrap_or_default();
            options.push((name.to_owned(), format!("{mode} · {model}{effort}")));
        }
        anyhow::ensure!(
            !options.is_empty(),
            "Codex returned no collaboration mode presets"
        );
        command
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(
                        "Collaboration mode",
                        &format!(
                            "Pick a preset for task **{}**. It applies to newly started turns.",
                            task.title.chars().take(120).collect::<String>()
                        ),
                    ))
                    .components(components::mode_select(options)),
            )
            .await?;
        Ok(())
    }

    async fn apply_mode_selection(
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<()> {
        self.require_installed_method("thread/settings/update")?;
        let choice = selected_value(component)?.to_owned();
        defer_update_component(http, component).await?;
        let task = self.task_for_channel(component.channel_id).await?;
        // Re-read presets at click time so a stale menu cannot apply settings
        // Codex no longer advertises.
        let result = self.state.codex.collaboration_mode_list().await?;
        let presets = first_result_array(&result, &["data", "items"]);
        let preset = presets
            .iter()
            .find(|preset| preset.get("name").and_then(Value::as_str) == Some(choice.as_str()))
            .context("preset is no longer available; run /mode again")?;
        let mode = preset
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned();
        let model = preset
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| task.model.clone())
            .context(
                "preset has no model and this task has no /model override; set /model first",
            )?;
        let reasoning_effort = preset
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.state
            .codex
            .thread_settings_update(ThreadSettingsUpdateParams {
                thread_id: task.thread_id.clone(),
                effort: None,
                collaboration_mode: Some(CollaborationModeSetting {
                    mode,
                    settings: CollaborationModeSettings {
                        model,
                        reasoning_effort,
                        developer_instructions: None,
                    },
                }),
            })
            .await?;
        component
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::info_card(
                        "Collaboration mode updated",
                        &format!("New turns use the **{choice}** preset."),
                    ))
                    .components(Vec::new()),
            )
            .await?;
        Ok(())
    }

    async fn set_effort(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        self.require_installed_method("thread/settings/update")?;
        let level = string_command_option(command, "level")
            .context("level required")?
            .to_owned();
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        let model = task
            .model
            .clone()
            .unwrap_or_else(|| RELAY_DEFAULT_MODEL.to_owned());
        let result = self
            .state
            .codex
            .model_list(ModelListParams {
                cursor: None,
                limit: Some(100),
                include_hidden: Some(true),
            })
            .await?;
        let supported = model_reasoning_efforts(&result, &model);
        anyhow::ensure!(
            supported.iter().any(|effort| effort == &level),
            "`{level}` is not supported by `{model}`; choose one of: {}",
            supported.join(", ")
        );
        self.state
            .codex
            .thread_settings_update(ThreadSettingsUpdateParams {
                thread_id: task.thread_id.clone(),
                effort: Some(level.clone()),
                collaboration_mode: None,
            })
            .await?;
        edit_command_card(
            http,
            command,
            "Reasoning effort",
            &format!("Reasoning effort for new turns set to **{level}**."),
        )
        .await
    }

    async fn find_files(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        self.require_installed_method("fuzzyFileSearch")?;
        let query = string_command_option(command, "query")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("query required")?
            .to_owned();
        defer_command(http, command).await?;
        let task = self.task_for_channel(command.channel_id).await?;
        let root = task
            .cwd
            .clone()
            .context("this task has no working directory to search")?;
        let result = self
            .state
            .codex
            .fuzzy_file_search(FuzzyFileSearchParams {
                query: query.clone(),
                roots: vec![root],
                cancellation_token: None,
            })
            .await?;
        let files = first_result_array(&result, &["files", "results", "data", "items"]);
        let mut lines = String::new();
        for file in files.iter().take(20) {
            let path = file
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("(unknown path)");
            lines.push_str(&format!(
                "`{}`\n",
                path.chars().take(180).collect::<String>()
            ));
        }
        if lines.is_empty() {
            lines = format!("No workspace files matched `{query}`.");
        }
        push_more_indicator(&mut lines, files.len(), 20);
        command
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::info_card(
                    &format!(
                        "File search: {}",
                        query.chars().take(180).collect::<String>()
                    ),
                    &lines,
                )),
            )
            .await?;
        Ok(())
    }

    async fn edit_redacted_json(
        &self,
        http: &Http,
        command: &CommandInteraction,
        title: &str,
        filename: &str,
        value: Value,
    ) -> Result<()> {
        let value = crate::security::redact_detail(value);
        let pretty = serde_json::to_string_pretty(&value)?;
        let mut response = EditInteractionResponse::new().embed(embeds::info_card(
            title,
            &format!(
                "Sensitive values are redacted.\n```json\n{}\n```",
                pretty.chars().take(3200).collect::<String>()
            ),
        ));
        if pretty.len() > 3200 {
            response =
                response.new_attachment(CreateAttachment::bytes(pretty.into_bytes(), filename));
        }
        command.edit_response(http, response).await?;
        Ok(())
    }

    async fn advanced(&self, http: &Http, command: &CommandInteraction) -> Result<()> {
        let context = self
            .security_context(
                http,
                command.user.id.get(),
                command.guild_id,
                command.channel_id,
            )
            .await;
        let task_id = self
            .state
            .store
            .task_by_channel(command.channel_id.get())
            .await?
            .map(|task| task.thread_id)
            .context("advanced RPC is only available inside a private task channel")?;
        let mut method = None;
        let mut params = json!({});
        for option in command.data.options() {
            match option.name {
                "method" => {
                    if let serenity::all::ResolvedValue::String(value) = option.value {
                        method = Some(value);
                    }
                }
                "params" => {
                    if let serenity::all::ResolvedValue::String(value) = option.value {
                        params = serde_json::from_str(value)?;
                    }
                }
                _ => {}
            }
        }
        let method = method.context("method required")?;
        let capability = self
            .state
            .capabilities
            .client_request(method)
            .context("method is absent from the installed Codex schema")?;
        bind_advanced_task_params(&mut params, capability.params_schema.as_ref(), &task_id)?;
        if let Some(schema) = capability.params_schema.as_ref() {
            let errors = crate::codex::params::validate(schema, &params);
            anyhow::ensure!(
                errors.is_empty(),
                "invalid parameters:\n{}",
                errors.join("\n")
            );
        }
        command
            .create_response(
                http,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                ),
            )
            .await?;
        let _dispatch = self.state.god_lifecycle.dispatch().await;
        self.ensure_task_not_god_quarantined(&task_id).await?;
        let god = self.state.god.read().await.clone();
        anyhow::ensure!(
            god.active_session(&context, Some(&task_id)).await.is_some(),
            "advanced RPC requires active task-scoped GOD mode in this private task channel"
        );
        self.ensure_task_not_god_quarantined(&task_id).await?;
        self.state
            .store
            .mark_god_dirty(&task_id, "GOD advanced RPC dispatch")
            .await?;
        let action_id = stable_value_hash(&json!({
            "method": method,
            "params": params,
            "channel": command.channel_id.get(),
            "task": task_id,
        }));
        self.state
            .store
            .audit(
                "advanced_rpc.intent",
                Some(command.user.id.get()),
                command.guild_id.map(|id| id.get()),
                Some(command.channel_id.get()),
                Some(&task_id),
                &json!({
                    "action_id": &action_id,
                    "method": method,
                    "authorization": "god",
                    "params_hash": stable_value_hash(&params),
                }),
            )
            .await?;
        let result = self.state.codex.request_value(method, params.clone()).await;
        if let Err(error) = self
            .state
            .store
            .audit(
                "advanced_rpc.outcome",
                Some(command.user.id.get()),
                command.guild_id.map(|id| id.get()),
                Some(command.channel_id.get()),
                Some(&task_id),
                &json!({
                    "action_id": &action_id,
                    "method": method,
                    "authorization": "god",
                    "params_hash": stable_value_hash(&params),
                    "accepted": result.is_ok(),
                }),
            )
            .await
        {
            error!(%error, %action_id, "advanced RPC outcome audit could not be persisted");
        }
        let result = result?;
        drop(_dispatch);
        self.edit_redacted_json(
            http,
            command,
            &format!("Advanced RPC: {method}"),
            "codex-rpc.redacted.json",
            result,
        )
        .await
    }

    async fn god_off(&self, http: &Http, responder: CommandResponder<'_>) -> Result<()> {
        responder.defer(http).await?;
        let context = self
            .security_context(
                http,
                responder.user_id(),
                responder.guild_id(),
                responder.channel_id(),
            )
            .await;
        let _lifecycle = self.state.god_lifecycle.transition().await;
        let god = self.state.god.read().await.clone();
        let previous = god.active_global_session().await;
        if let Some(thread_id) = previous
            .as_ref()
            .and_then(|session| session.scope.task_id())
        {
            self.state
                .god_quarantined_tasks
                .insert(thread_id.to_owned(), "owner revoking GOD mode".to_owned());
            self.state
                .store
                .mark_god_dirty(thread_id, "owner revoking GOD mode")
                .await?;
        }
        let revoke_result = god.revoke_active(&context).await;
        let still_active = god.active_global_session().await;
        let revoked = previous.as_ref().is_some_and(|previous| {
            still_active
                .as_ref()
                .is_none_or(|active| active.session_id != previous.session_id)
        });
        if revoked {
            if let Some(session) = previous.as_ref() {
                cleanup_god_session(&self.state, http, session, "owner revoked GOD mode").await?;
            }
            retire_god_warning(&self.state, http, "GOD mode revoked").await;
        }
        revoke_result?;
        refresh_runner_status(&self.state, http).await?;
        responder
            .respond(
                http,
                embeds::info_card(
                    "GOD mode",
                    if revoked {
                        "GOD mode revoked."
                    } else {
                        "No active GOD session."
                    },
                ),
                Vec::new(),
            )
            .await
    }

    async fn task_for_channel(&self, channel: ChannelId) -> Result<TaskMirror> {
        self.state
            .store
            .task_by_channel(channel.get())
            .await?
            .context("this is not a Codex task channel")
    }

    async fn ensure_task_not_god_quarantined(&self, thread_id: &str) -> Result<()> {
        if let Some(reason) = self.state.god_quarantined_tasks.get(thread_id) {
            anyhow::bail!(
                "task is quarantined while GOD cleanup retries: {}",
                reason.value()
            );
        }
        let god = self.state.god.read().await.clone();
        anyhow::ensure!(
            !god.cleanup_required_for(thread_id),
            "task is quarantined while GOD lifecycle cleanup is pending"
        );
        Ok(())
    }

    async fn security_context(
        &self,
        http: &Http,
        owner_id: u64,
        guild_id: Option<GuildId>,
        channel_id: ChannelId,
    ) -> SecurityContext {
        let control_channel = self
            .state
            .layout
            .read()
            .await
            .as_ref()
            .is_some_and(|layout| {
                [
                    layout.new_task_channel,
                    layout.existing_tasks_channel,
                    layout.runner_status_channel,
                    layout.audit_log_channel,
                ]
                .contains(&channel_id)
            });
        let task_channel = self
            .state
            .store
            .task_by_channel(channel_id.get())
            .await
            .ok()
            .flatten()
            .is_some();
        let bot_user_id = self.state.bot_user_id.load(Ordering::Acquire);
        let private = self.state.guild_isolation_verified.load(Ordering::Acquire)
            && bot_user_id != 0
            && (control_channel || task_channel)
            && provision::is_private_channel_for_bot(
                http,
                &self.state.config,
                channel_id,
                UserId::new(bot_user_id),
            )
            .await;
        SecurityContext {
            owner_id,
            guild_id: guild_id.map(|id| id.get()),
            channel_id: channel_id.get(),
            is_private_channel: private,
        }
    }
}

fn control_channel_guidance(
    layout: &provision::Layout,
    channel_id: ChannelId,
) -> Option<(&'static str, &'static str, bool)> {
    if channel_id == layout.new_task_channel {
        return Some((
            "Start a Codex task",
            "Messages in this control channel are not forwarded accidentally. Click **New task** or run `/new`; after creation, drop text and attachments directly into the private task channel.",
            true,
        ));
    }
    if channel_id == layout.existing_tasks_channel {
        return Some((
            "Open an existing task",
            "Click **Browse tasks** or run `/tasks` to search, reopen, fork, archive, and continue Codex Desktop tasks.",
            true,
        ));
    }
    if channel_id == layout.runner_status_channel {
        return Some((
            "Runner status",
            "Run `/status` for live relay, Codex, queue, and GOD-mode health. Start work from `#new-task` or a private task channel.",
            false,
        ));
    }
    if channel_id == layout.audit_log_channel {
        return Some((
            "Audit log is read-only",
            "This channel records security and relay events. Send prompts in a private task channel, or use `#new-task` to create one.",
            false,
        ));
    }
    None
}

#[derive(Clone, Copy)]
enum CommandResponder<'a> {
    Command(&'a CommandInteraction),
    Component(&'a ComponentInteraction),
}
impl CommandResponder<'_> {
    fn user_id(self) -> u64 {
        match self {
            Self::Command(value) => value.user.id.get(),
            Self::Component(value) => value.user.id.get(),
        }
    }

    fn guild_id(self) -> Option<GuildId> {
        match self {
            Self::Command(value) => value.guild_id,
            Self::Component(value) => value.guild_id,
        }
    }

    fn channel_id(self) -> ChannelId {
        match self {
            Self::Command(value) => value.channel_id,
            Self::Component(value) => value.channel_id,
        }
    }

    async fn defer(self, http: &Http) -> Result<()> {
        let response = CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new().ephemeral(true),
        );
        match self {
            Self::Command(value) => value.create_response(http, response).await?,
            Self::Component(value) => value.create_response(http, response).await?,
        }
        Ok(())
    }

    async fn respond(
        self,
        http: &Http,
        embed: CreateEmbed,
        components: Vec<serenity::builder::CreateActionRow>,
    ) -> Result<()> {
        let message = EditInteractionResponse::new()
            .embed(embed)
            .components(components);
        match self {
            Self::Command(value) => value.edit_response(http, message).await?,
            Self::Component(value) => value.edit_response(http, message).await?,
        };
        Ok(())
    }
}

pub async fn run() -> Result<()> {
    let config = Arc::new(config::load()?);
    let token = config::read_token()?;
    let store = StateStore::connect(&config::data_dir().join("relay.sqlite3")).await?;
    let (hash, god_password_configured) = match config::load_god_password_hash() {
        Ok(encoded) => (StoredPasswordHash::parse(encoded.trim().to_owned())?, true),
        Err(_) => {
            let unreachable = format!(
                "{}{}{}",
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            );
            (
                Argon2idPasswordManager::default().hash(SecretString::from(unreachable))?,
                false,
            )
        }
    };
    let god_audit: Arc<dyn AuditSink> = Arc::new(StoreAuditSink(store.clone()));
    let god = Arc::new(GodModeService::with_ttl(
        hash,
        TrustBoundary::new(config.owner_user_id, config.guild_id)?,
        Arc::clone(&god_audit),
        Arc::new(SystemClock),
        chrono::Duration::minutes(i64::try_from(config.god_session_minutes)?),
    )?);
    let codex_command = if let Some(path) = &config.codex_executable {
        CodexCommand::new(path)
    } else {
        // Discovery scans processes and hash-verifies a multi-hundred-MB
        // executable; keep that off the async workers.
        tokio::task::spawn_blocking(CodexCommand::discover).await??
    };
    let capabilities = CapabilityCatalog::generate_and_load(
        &codex_command,
        config::data_dir().join("schemas").join("current"),
    )
    .await
    .unwrap_or_else(|error| {
        warn!(%error, "capability schema generation failed; advanced autocomplete disabled");
        CapabilityCatalog::default()
    });
    let client_methods = capabilities.client_requests.iter().cloned().collect();
    let capabilities = Arc::new(capabilities);
    let codex = CodexClient::new(codex_command);
    let host_broker = HostBroker::new(codex.clone(), store.clone());
    // Subscribe before the first connect: notifications Codex emits while the
    // Discord gateway is still starting buffer in these receivers until
    // `spawn_codex_listeners` drains them at gateway-ready.
    let early_codex_receivers = std::sync::Mutex::new(Some((
        codex.subscribe_notifications(),
        codex.subscribe_server_requests(),
    )));
    codex.connect().await?;
    let state = Arc::new(BotState {
        config,
        store,
        codex,
        host_broker,
        early_codex_receivers,
        god: RwLock::new(god),
        god_password_configured: AtomicBool::new(god_password_configured),
        capabilities,
        client_methods,
        layout: RwLock::new(None),
        streams: Mutex::new(HashMap::new()),
        activity: Mutex::new(HashMap::new()),
        plans: Mutex::new(HashMap::new()),
        realtime: Mutex::new(HashMap::new()),
        projection: Mutex::new(ProjectionCore::new()),
        operations: Mutex::new(HashMap::new()),
        delivered_media: dashmap::DashSet::new(),
        ingestion_locks: dashmap::DashMap::new(),
        pending_server_requests: dashmap::DashMap::new(),
        pending_request_started: dashmap::DashMap::new(),
        pending_request_messages: dashmap::DashMap::new(),
        resolved_request_tombstones: dashmap::DashMap::new(),
        action_drafts: dashmap::DashMap::new(),
        task_browsers: dashmap::DashMap::new(),
        plugin_browsers: dashmap::DashMap::new(),
        runner_status_message_id: AtomicU64::new(0),
        god_warning_channel_id: AtomicU64::new(0),
        god_warning_message_id: AtomicU64::new(0),
        god_quarantined_tasks: dashmap::DashMap::new(),
        god_lifecycle: GodLifecycle::default(),
        startup_ready: AtomicBool::new(false),
        guild_isolation_verified: AtomicBool::new(false),
        bot_user_id: AtomicU64::new(0),
        listeners_started: AtomicBool::new(false),
    });
    let intents =
        GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::Client::builder(token, intents)
        .event_handler(Handler {
            state: Arc::clone(&state),
        })
        .await?;
    client.start().await?;
    state.codex.close().await;
    Ok(())
}

pub async fn register_commands(http: &Http, guild_id: u64) -> Result<()> {
    let commands: Vec<CreateCommand> = vec![
        CommandBuilder::new("new").description("Start a new Codex task"),
        CommandBuilder::new("tasks")
            .description("Search, browse, and reopen Codex Desktop tasks")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "search",
                    "Search task titles and previews",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "archived",
                    "Browse archived tasks instead of active tasks",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "page",
                    "Result page (1-4; use search to narrow further)",
                )
                .min_int_value(1)
                .max_int_value(4)
                .required(false),
            ),
        CommandBuilder::new("status").description("Show runner/task/GOD status"),
        CommandBuilder::new("actions")
            .description("Browse every Codex app-server action with generated forms"),
        CommandBuilder::new("god").description("Unlock short-lived task-scoped unrestricted mode"),
        CommandBuilder::new("god_off").description("Revoke active GOD mode immediately"),
        CommandBuilder::new("email")
            .description("Send an email through the configured Codex connector"),
        CommandBuilder::new("interrupt").description("Stop active turn in this task channel"),
        CommandBuilder::new("fork").description("Fork this Codex task into a new channel"),
        CommandBuilder::new("archive").description("Archive this Codex task"),
        CommandBuilder::new("rename")
            .description("Rename this Codex task and its channel")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "New task name")
                    .required(true),
            ),
        CommandBuilder::new("rollback")
            .description("Drop recent turns from this task history")
            .add_option(
                CreateCommandOption::new(CommandOptionType::Integer, "turns", "Turns to drop")
                    .min_int_value(1)
                    .max_int_value(100)
                    .required(true),
            ),
        CommandBuilder::new("compact").description("Compact this task context"),
        CommandBuilder::new("review")
            .description("Start a Codex code review")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "target", "Review target")
                    .add_string_choice("Uncommitted changes", "uncommitted")
                    .add_string_choice("Against base branch", "branch")
                    .add_string_choice("Commit", "commit")
                    .add_string_choice("Custom instructions", "custom")
                    .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "ref",
                    "Branch, SHA, or instructions",
                )
                .required(false),
            ),
        CommandBuilder::new("model").description("Choose the model for new turns in this task"),
        CommandBuilder::new("skills")
            .description("Search every skill available to this task")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "query",
                    "Filter by skill name or description",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(CommandOptionType::Integer, "page", "Result page")
                    .min_int_value(1)
                    .max_int_value(100)
                    .required(false),
            ),
        CommandBuilder::new("apps")
            .description("Search Codex apps and connectors")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "query",
                    "Filter by app or connector name",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(CommandOptionType::Integer, "page", "Result page")
                    .min_int_value(1)
                    .max_int_value(100)
                    .required(false),
            ),
        CommandBuilder::new("plugins")
            .description("Browse installed plugins or every configured marketplace")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "scope", "Plugin source")
                    .add_string_choice("Installed", "installed")
                    .add_string_choice("Marketplace catalog", "catalog")
                    .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "query",
                    "Filter by plugin, marketplace, id, or description",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(CommandOptionType::Integer, "page", "Result page")
                    .min_int_value(1)
                    .max_int_value(10_000)
                    .required(false),
            ),
        CommandBuilder::new("config").description("Show effective Codex config, redacted"),
        CommandBuilder::new("account").description("Show Codex account status, redacted"),
        CommandBuilder::new("usage").description("Show Codex usage and rate limits"),
        CommandBuilder::new("capabilities")
            .description("Measured coverage or details for one method")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "method", "Installed method")
                    .required(false)
                    .set_autocomplete(true),
            ),
        CommandBuilder::new("advanced")
            .description("Invoke any app-server method (GOD mode only)")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "method",
                    "Exact app-server method, e.g. config/read",
                )
                .required(true)
                .set_autocomplete(true),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "params",
                    "JSON object parameters",
                )
                .required(false),
            ),
        CommandBuilder::new("goal")
            .description("Show, set, or clear this task's goal")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "objective",
                    "New goal objective (omit to show the current goal)",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "budget",
                    "Token budget for the goal",
                )
                .min_int_value(1000)
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "clear",
                    "Clear the current goal",
                )
                .required(false),
            ),
        CommandBuilder::new("terminals")
            .description("List, terminate, or clean this task's background terminals"),
        CommandBuilder::new("history")
            .description("Summarize recent turns in this task")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "turns",
                    "How many recent turns to show (1-10)",
                )
                .min_int_value(1)
                .max_int_value(10)
                .required(false),
            ),
        CommandBuilder::new("find")
            .description("Full-text search across all Codex task content")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "query", "Text to search for")
                    .required(true),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "archived",
                    "Search archived tasks instead of active tasks",
                )
                .required(false),
            ),
        CommandBuilder::new("mcp")
            .description("Browse every MCP server, tool, resource, or template")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "view", "Catalog to browse")
                    .add_string_choice("Servers", "servers")
                    .add_string_choice("Tools", "tools")
                    .add_string_choice("Resources", "resources")
                    .add_string_choice("Resource templates", "templates")
                    .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "query",
                    "Filter by name, server, URI, or description",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(CommandOptionType::Integer, "page", "Result page")
                    .min_int_value(1)
                    .max_int_value(100)
                    .required(false),
            ),
        CommandBuilder::new("mode")
            .description("Choose a collaboration mode preset for new turns in this task"),
        CommandBuilder::new("effort")
            .description("Set reasoning effort for new turns in this task")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "level", "Reasoning effort")
                    .set_autocomplete(true)
                    .required(true),
            ),
        CommandBuilder::new("files")
            .description("Fuzzy-search files in this task's working directory")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "query",
                    "File name fragment, e.g. main.rs",
                )
                .required(true),
            ),
    ];
    GuildId::new(guild_id).set_commands(http, commands).await?;
    Ok(())
}

fn spawn_codex_listeners(state: Arc<BotState>, http: Arc<Http>) {
    // Prefer the receivers subscribed in `run()` before the first Codex
    // connect: anything Codex emitted while the Discord gateway was starting
    // is buffered there instead of lost.
    let (mut notifications, mut requests) = state
        .early_codex_receivers
        .lock()
        .ok()
        .and_then(|mut receivers| receivers.take())
        .unwrap_or_else(|| {
            (
                state.codex.subscribe_notifications(),
                state.codex.subscribe_server_requests(),
            )
        });
    let notification_state = Arc::clone(&state);
    let notification_http = Arc::clone(&http);
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(event) => {
                    if let Err(error) =
                        handle_notification(&notification_state, &notification_http, event).await
                    {
                        error!(%error, "notification handling failed");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    warn!(count, "Codex notification consumer lagged");
                    {
                        let mut projection = notification_state.projection.lock().await;
                        projection.mark_all_threads_lagged();
                        projection.mark_connection_lagged();
                    }
                    {
                        let mut realtime = notification_state.realtime.lock().await;
                        for digest in realtime.values_mut().filter(|digest| digest.active) {
                            digest.audio_truncated = true;
                            digest.dirty = true;
                        }
                    }
                    let mut operations = notification_state.operations.lock().await;
                    for digest in operations.values_mut() {
                        for (_, summary) in &mut digest.entries {
                            if !summary.contains("stream lagged") {
                                summary.push_str(" · ⚠ stream lagged");
                            }
                        }
                        digest.dirty = true;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    tokio::spawn(async move {
        let dynamic_tool_concurrency = Arc::new(tokio::sync::Semaphore::new(8));
        let interactive_concurrency = Arc::new(tokio::sync::Semaphore::new(32));
        loop {
            match requests.recv().await {
                Ok(request) => {
                    let state = Arc::clone(&state);
                    let http = Arc::clone(&http);
                    let concurrency = if ServerRequestMethod::classify(&request.method)
                        == ServerRequestMethod::DynamicToolCall
                    {
                        Arc::clone(&dynamic_tool_concurrency)
                    } else {
                        Arc::clone(&interactive_concurrency)
                    };
                    tokio::spawn(async move {
                        let Ok(_permit) = concurrency.acquire_owned().await else {
                            return;
                        };
                        if let Err(error) = handle_server_request(&state, &http, request).await {
                            error!(%error, "server request handling failed");
                        }
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    // A dropped server request leaves Codex waiting on an
                    // approval no card exists for; make that loudly visible.
                    error!(
                        count,
                        "Codex server-request consumer lagged; a pending approval may never render — interrupt the affected task"
                    );
                    let _ = state
                        .store
                        .audit(
                            "codex_server_request_lagged",
                            None,
                            Some(state.config.guild_id),
                            None,
                            None,
                            &json!({"dropped": count}),
                        )
                        .await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_stream_flusher(state: Arc<BotState>, http: Arc<Http>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            if let Err(error) = flush_streams(&state, &http).await {
                warn!(%error, "stream flush failed");
            }
            if let Err(error) = flush_activity(&state, &http).await {
                warn!(%error, "activity flush failed");
            }
            if let Err(error) = flush_plans(&state, &http).await {
                warn!(%error, "plan flush failed");
            }
            if let Err(error) = flush_realtime(&state, &http).await {
                warn!(%error, "realtime transcript flush failed");
            }
            if let Err(error) = flush_operations(&state, &http).await {
                warn!(%error, "live operations flush failed");
            }
            if let Err(error) = expire_live_pending_requests(&state, &http).await {
                warn!(%error, "pending request expiry sweep failed");
            }
        }
    });
}

fn spawn_god_monitor(state: Arc<BotState>, http: Arc<Http>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let god = state.god.read().await.clone();
            // Only expiration and synchronous quarantine publication are
            // protected by the dispatch barrier. The task is fail-closed as
            // soon as the barrier is released; slow cleanup is separately
            // leased by `cleanup_god_thread`.
            let expired = {
                let _transition = state.god_lifecycle.transition().await;
                let expired = god.expire_global_session().await;
                if let Some(thread_id) =
                    expired.as_ref().and_then(|session| session.scope.task_id())
                {
                    state.god_quarantined_tasks.insert(
                        thread_id.to_owned(),
                        "expired GOD session cleanup".to_owned(),
                    );
                }
                expired
            };
            let expired_thread = expired
                .as_ref()
                .and_then(|session| session.scope.task_id())
                .map(str::to_owned);
            if let Some(expired) = expired {
                if let Some(thread_id) = expired.scope.task_id()
                    && let Err(error) = state
                        .store
                        .mark_god_dirty(thread_id, "expired GOD session cleanup")
                        .await
                {
                    error!(thread_id, %error, "expired GOD quarantine could not be persisted");
                }
                if let Err(error) =
                    cleanup_god_session(&state, &http, &expired, "GOD mode expired").await
                {
                    error!(thread_id = ?expired.scope.task_id(), %error, "expired GOD task quarantined");
                    mark_god_warning_quarantined(&state, &http, &error.to_string()).await;
                } else {
                    retire_god_warning(&state, &http, "GOD mode expired").await;
                }
                if let Err(error) = refresh_runner_status(&state, &http).await {
                    warn!(%error, "expired GOD status refresh failed");
                }
            }

            let quarantined = state
                .god_quarantined_tasks
                .iter()
                .map(|entry| entry.key().clone())
                .collect::<Vec<_>>();
            for thread_id in quarantined {
                // The just-expired task already received one cleanup attempt
                // in this tick. Failed work remains quarantined for the next
                // interval instead of immediately doubling the RPC retries.
                if expired_thread.as_deref() == Some(thread_id.as_str()) {
                    continue;
                }
                if let Err(error) =
                    cleanup_god_thread(&state, &http, &thread_id, "automatic GOD quarantine retry")
                        .await
                {
                    warn!(thread_id, %error, "GOD quarantine retry still pending");
                } else if god.active_global_session().await.is_none() {
                    // The stored card belongs to this resolved incident only
                    // when no newer session has replaced it; an active session
                    // owns the card and must keep it red.
                    retire_god_warning(&state, &http, "GOD cleanup completed").await;
                }
            }
        }
    });
}

fn spawn_guild_isolation_monitor(state: Arc<BotState>, http: Arc<Http>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            match provision::verify_guild_isolation(&http, &state.config).await {
                Ok(()) => state
                    .guild_isolation_verified
                    .store(true, Ordering::Release),
                Err(error) => {
                    state
                        .guild_isolation_verified
                        .store(false, Ordering::Release);
                    error!(%error, "Discord server isolation check failed; secured actions disabled");
                }
            }
        }
    });
}

fn normal_thread_settings_params(thread_id: &str) -> Value {
    CodexExecutionPolicy::NORMAL.thread_settings(thread_id)
}

fn bind_advanced_task_params(
    params: &mut Value,
    schema: Option<&Value>,
    task_id: &str,
) -> Result<()> {
    let Some(schema) = schema else {
        return Ok(());
    };
    bind_schema_task_ids(params, schema, task_id)
}

fn bind_schema_task_ids(value: &mut Value, schema: &Value, task_id: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                return Ok(());
            };
            for (key, child_schema) in properties {
                let canonical = key
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .map(|character| character.to_ascii_lowercase())
                    .collect::<String>();
                if matches!(canonical.as_str(), "threadid" | "conversationid" | "taskid") {
                    match object.get(key) {
                        Some(Value::String(supplied)) => anyhow::ensure!(
                            supplied == task_id,
                            "advanced RPC task identifier `{key}` does not match this channel"
                        ),
                        Some(_) => {
                            anyhow::bail!("advanced RPC task identifier `{key}` must be a string")
                        }
                        None => {
                            object.insert(key.clone(), Value::String(task_id.to_owned()));
                        }
                    }
                } else if let Some(child) = object.get_mut(key) {
                    bind_schema_task_ids(child, child_schema, task_id)?;
                }
            }
        }
        Value::Array(values) => {
            if let Some(item_schema) = schema.get("items") {
                for value in values {
                    bind_schema_task_ids(value, item_schema, task_id)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn normalize_thread_permissions(state: &BotState, thread_id: &str) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=3 {
        match state
            .codex
            .request_value(
                "thread/settings/update",
                normal_thread_settings_params(thread_id),
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
                }
            }
        }
    }
    Err(anyhow::anyhow!(
        "Codex did not confirm normal permissions: {}",
        last_error.context("permission reset produced no result")?
    ))
}

async fn interrupt_god_turn(state: &BotState, thread_id: &str) -> Result<()> {
    let Some(turn_id) = live_active_turn_id(state, thread_id).await? else {
        return Ok(());
    };
    let mut last_error = None;
    for attempt in 1..=3 {
        match state
            .codex
            .turn_interrupt(TurnInterruptParams {
                thread_id: thread_id.to_owned(),
                turn_id: turn_id.clone(),
            })
            .await
        {
            Ok(_) => return confirm_god_turn_stopped(state, thread_id, &turn_id).await,
            Err(error) => {
                last_error = Some(error);
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
                }
            }
        }
    }
    Err(anyhow::anyhow!(
        "Codex did not confirm GOD turn interruption: {}",
        last_error.context("turn interruption produced no result")?
    ))
}

async fn confirm_god_turn_stopped(state: &BotState, thread_id: &str, turn_id: &str) -> Result<()> {
    for _ in 0..20 {
        if live_active_turn_id(state, thread_id).await?.as_deref() != Some(turn_id) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    anyhow::bail!("Codex acknowledged interruption but the GOD turn is still reported running")
}

async fn live_active_turn_id(state: &BotState, thread_id: &str) -> Result<Option<String>> {
    let read = state
        .codex
        .request_value(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        )
        .await?;
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

async fn cleanup_god_session(
    state: &BotState,
    http: &Http,
    session: &crate::security::GodSession,
    reason: &str,
) -> Result<()> {
    let Some(thread_id) = session.scope.task_id() else {
        return Ok(());
    };
    cleanup_god_thread(state, http, thread_id, reason).await
}

async fn cleanup_god_thread(
    state: &BotState,
    http: &Http,
    thread_id: &str,
    reason: &str,
) -> Result<()> {
    let _lease = state
        .god_lifecycle
        .cleanup_lease(thread_id)
        .with_context(|| format!("GOD cleanup already in progress for task `{thread_id}`"))?;
    state
        .god_quarantined_tasks
        .insert(thread_id.to_owned(), reason.to_owned());

    let interrupt = interrupt_god_turn(state, thread_id).await;
    let normalize = normalize_thread_permissions(state, thread_id).await;
    let detail = json!({
        "reason": reason,
        "interrupt_confirmed": interrupt.is_ok(),
        "permissions_normalized": normalize.is_ok()
    });
    let audit = state
        .store
        .audit(
            "god.cleanup",
            Some(state.config.owner_user_id),
            Some(state.config.guild_id),
            None,
            Some(thread_id),
            &detail,
        )
        .await;

    if interrupt.is_ok() && normalize.is_ok() && audit.is_ok() {
        state.store.clear_god_dirty(thread_id).await?;
        state.god.read().await.cleanup_finished(thread_id);
        state.god_quarantined_tasks.remove(thread_id);
        if let Some(layout) = state.layout.read().await.clone() {
            let _ = layout
                .audit_log_channel
                .send_message(
                    http,
                    CreateMessage::new().embed(
                        CreateEmbed::new()
                            .title("GOD permissions normalized")
                            .description(reason)
                            .field("Task", format!("`{thread_id}`"), false)
                            .color(0x57F287)
                            .timestamp(serenity::model::Timestamp::now()),
                    ),
                )
                .await;
        }
        return Ok(());
    }

    let error = format!(
        "cleanup incomplete (interrupt: {}; permission reset: {}; audit: {})",
        interrupt
            .as_ref()
            .err()
            .map_or("confirmed".to_owned(), ToString::to_string),
        normalize
            .as_ref()
            .err()
            .map_or("confirmed".to_owned(), ToString::to_string),
        audit
            .as_ref()
            .err()
            .map_or("persisted".to_owned(), ToString::to_string)
    );
    state
        .god_quarantined_tasks
        .insert(thread_id.to_owned(), error.clone());
    Err(anyhow::anyhow!(error))
}

async fn normalize_completed_god_turn(
    state: &BotState,
    http: &Http,
    thread_id: &str,
) -> Result<()> {
    let _lease = state
        .god_lifecycle
        .cleanup_lease(thread_id)
        .with_context(|| format!("GOD cleanup already in progress for task `{thread_id}`"))?;
    state.god_quarantined_tasks.insert(
        thread_id.to_owned(),
        "normalizing completed GOD turn".to_owned(),
    );
    let normalize = normalize_thread_permissions(state, thread_id).await;
    let audit = state
        .store
        .audit(
            "god.turn_permissions_normalized",
            Some(state.config.owner_user_id),
            Some(state.config.guild_id),
            None,
            Some(thread_id),
            &json!({"permissions_normalized": normalize.is_ok()}),
        )
        .await;
    if normalize.is_ok() && audit.is_ok() {
        state.store.clear_god_dirty(thread_id).await?;
        state.god.read().await.cleanup_finished(thread_id);
        state.god_quarantined_tasks.remove(thread_id);
        return Ok(());
    }
    let error = format!(
        "completed GOD turn permission reset failed: {}; audit: {}",
        normalize
            .as_ref()
            .err()
            .map_or("confirmed".to_owned(), ToString::to_string),
        audit
            .as_ref()
            .err()
            .map_or("persisted".to_owned(), ToString::to_string)
    );
    state
        .god_quarantined_tasks
        .insert(thread_id.to_owned(), error.clone());
    mark_god_warning_quarantined(state, http, &error).await;
    Err(anyhow::anyhow!(error))
}

async fn mark_god_warning_quarantined(state: &BotState, http: &Http, reason: &str) {
    let channel_id = state.god_warning_channel_id.load(Ordering::Acquire);
    let message_id = state.god_warning_message_id.load(Ordering::Acquire);
    if channel_id == 0 || message_id == 0 {
        return;
    }
    let description = format!(
        "New work is blocked while the relay retries interruption and permission reset.\n\n`{}`",
        reason.chars().take(800).collect::<String>()
    );
    let _ = ChannelId::new(channel_id)
        .edit_message(
            http,
            message_id,
            EditMessage::new().embed(
                CreateEmbed::new()
                    .title("GOD cleanup quarantined")
                    .description(description)
                    .color(0xED4245)
                    .timestamp(serenity::model::Timestamp::now()),
            ),
        )
        .await;
}

fn is_sensitive_god_message(content: &str) -> bool {
    content
        .trim_start()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("!god"))
}

fn is_absent_discord_message_response(status: u16, code: isize) -> bool {
    status == 404 || code == 10_008
}

fn is_absent_discord_message_error(error: &serenity::Error) -> bool {
    matches!(
        error,
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response))
            if is_absent_discord_message_response(
                response.status_code.as_u16(),
                response.error.code,
            )
    )
}

async fn delete_sensitive_discord_message(
    message: &Message,
    http: &Http,
) -> std::result::Result<(), serenity::Error> {
    let mut last_error = None;
    for attempt in 1..=5 {
        match message.delete(http).await {
            Ok(()) => return Ok(()),
            Err(error) if is_absent_discord_message_error(&error) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
        }
    }
    Err(last_error.expect("a failed Discord deletion records its error"))
}

async fn report_unrecoverable_sensitive_deletion(
    state: &BotState,
    http: &Http,
    channel_id: ChannelId,
    message_id: u64,
    queue_error: &str,
    delete_error: &str,
) {
    let queue_error = queue_error.chars().take(500).collect::<String>();
    let delete_error = delete_error.chars().take(500).collect::<String>();
    error!(
        channel_id = channel_id.get(),
        message_id,
        %queue_error,
        %delete_error,
        "CRITICAL: sensitive GOD message could not be deleted or durably queued"
    );

    if let Err(audit_error) = state
        .store
        .audit(
            "security.sensitive_message_deletion_critical",
            Some(state.config.owner_user_id),
            Some(state.config.guild_id),
            Some(channel_id.get()),
            None,
            &json!({
                "message_id": message_id,
                "queue_error": queue_error,
                "delete_error": delete_error,
            }),
        )
        .await
    {
        error!(%audit_error, "CRITICAL: sensitive deletion failure audit could not be persisted");
    }

    let Some(layout) = state.layout.read().await.clone() else {
        error!(
            owner_user_id = state.config.owner_user_id,
            "CRITICAL: audit channel unavailable for sensitive deletion owner alert"
        );
        return;
    };
    if let Err(alert_error) = layout
        .audit_log_channel
        .send_message(
            http,
            CreateMessage::new()
                .content(format!("<@{}>", state.config.owner_user_id))
                .embed(
                    CreateEmbed::new()
                        .title("CRITICAL: password message deletion failed")
                        .description(
                            "Discord deletion failed and no durable retry could be recorded. Delete the source message manually now.",
                        )
                        .field("Channel", format!("<#{channel_id}>"), true)
                        .field("Message ID", format!("`{message_id}`"), true)
                        .color(0xED_4245)
                        .timestamp(serenity::model::Timestamp::now()),
                ),
        )
        .await
    {
        error!(%alert_error, "CRITICAL: sensitive deletion owner alert failed");
    }
}

async fn retire_god_warning(state: &BotState, http: &Http, title: &str) {
    let channel_id = state.god_warning_channel_id.swap(0, Ordering::AcqRel);
    let message_id = state.god_warning_message_id.swap(0, Ordering::AcqRel);
    if channel_id == 0 || message_id == 0 {
        return;
    }
    let result = ChannelId::new(channel_id)
        .edit_message(
            http,
            message_id,
            EditMessage::new()
                .embed(
                    CreateEmbed::new()
                        .title(title)
                        .description("Privileged local execution has been stopped.")
                        .color(0x57F287)
                        .timestamp(serenity::model::Timestamp::now()),
                )
                .components(Vec::new()),
        )
        .await;
    if let Err(error) = result {
        warn!(%error, "could not retire GOD warning card");
    }
}

async fn handle_notification(state: &BotState, http: &Http, event: Notification) -> Result<()> {
    use notifications::NotificationDisposition;

    let thread_id = crate::codex::thread_id_from_params(&event.params);
    let disposition = notifications::classify(&event.method);
    match disposition {
        NotificationDisposition::AgentMessageDelta => {
            if let (Some(thread_id), Some(delta)) =
                (thread_id, event.params.get("delta").and_then(Value::as_str))
            {
                let item_id = event
                    .params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mut streams = state.streams.lock().await;
                streams
                    .entry(thread_id.to_owned())
                    .or_default()
                    .append_delta(item_id, delta);
            }
            return Ok(());
        }
        NotificationDisposition::HighVolumeStream => return Ok(()),
        NotificationDisposition::OperationProgress => {
            update_operation_progress(state, &event).await;
            return Ok(());
        }
        NotificationDisposition::TerminalInteraction => {
            persist_notification_audit(state, &event, thread_id).await?;
            project_terminal_interaction(state, &event).await;
            return Ok(());
        }
        NotificationDisposition::AutoApprovalReview => {
            persist_notification_audit(state, &event, thread_id).await?;
            project_auto_approval_review(state, &event).await;
            return Ok(());
        }
        NotificationDisposition::HookLifecycle => {
            persist_notification_audit(state, &event, thread_id).await?;
            project_hook_lifecycle(state, &event).await;
            return Ok(());
        }
        NotificationDisposition::ModelSafety => {
            persist_notification_audit(state, &event, thread_id).await?;
            project_model_safety(state, &event).await;
            return Ok(());
        }
        NotificationDisposition::StandaloneOutput => {
            project_standalone_output(state, &event).await;
            return Ok(());
        }
        NotificationDisposition::StandaloneLifecycle => {
            project_standalone_completion(state, &event).await;
            persist_notification_audit(state, &event, None).await?;
            return Ok(());
        }
        NotificationDisposition::PlanUpdated => {
            persist_notification_audit(state, &event, thread_id).await?;
            if let Some(thread_id) = thread_id {
                let mut plans = state.plans.lock().await;
                let board = plans.entry(thread_id.to_owned()).or_default();
                board.params = event.params;
                board.dirty = true;
            }
            return Ok(());
        }
        NotificationDisposition::ItemActivity => {
            persist_notification_audit(state, &event, thread_id).await?;
            if let (Some(thread_id), Some(item)) = (thread_id, event.params.get("item")) {
                // `item/completed` carries the authoritative full text of an
                // agent message; prefer it over accumulated deltas so lost or
                // duplicated deltas can never corrupt the final answer.
                if event.method == "item/completed"
                    && item.get("type").and_then(Value::as_str) == Some("agentMessage")
                    && let (Some(item_id), Some(text)) = (
                        item.get("id").and_then(Value::as_str),
                        item.get("text").and_then(Value::as_str),
                    )
                {
                    let mut streams = state.streams.lock().await;
                    streams
                        .entry(thread_id.to_owned())
                        .or_default()
                        .complete_item(item_id, text);
                }
                if event.method == "item/completed" {
                    complete_operation_item(state, thread_id, item).await;
                    deliver_completed_media(state, http, thread_id, item).await?;
                }
                if let Some(line) = embeds::describe_item(item, event.method == "item/completed") {
                    let mut activity = state.activity.lock().await;
                    let digest = activity.entry(thread_id.to_owned()).or_default();
                    digest.lines.push(line);
                    if digest.lines.len() > 14 {
                        digest.lines.remove(0);
                    }
                    digest.dirty = true;
                }
            }
            return Ok(());
        }
        NotificationDisposition::RealtimeTranscript => {
            let Some(thread_id) = thread_id else {
                return Ok(());
            };
            let role = event
                .params
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("realtime");
            {
                let mut realtime = state.realtime.lock().await;
                let digest = realtime.entry(thread_id.to_owned()).or_default();
                if !digest.active {
                    return Ok(());
                } else if event.method == "thread/realtime/transcript/done" {
                    let text = event
                        .params
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    digest.finish_transcript(role.to_owned(), text.to_owned());
                } else if let Some(delta) = event.params.get("delta").and_then(Value::as_str) {
                    digest.append_transcript(role, delta);
                }
            }
            if event.method == "thread/realtime/transcript/done" {
                persist_notification_audit(state, &event, Some(thread_id)).await?;
                flush_realtime(state, http).await?;
            }
            return Ok(());
        }
        NotificationDisposition::RealtimeAudio => {
            if let Some(thread_id) = thread_id {
                state
                    .realtime
                    .lock()
                    .await
                    .entry(thread_id.to_owned())
                    .or_default()
                    .append_audio(&event.params);
            }
            return Ok(());
        }
        NotificationDisposition::RealtimeLifecycle => {
            persist_notification_audit(state, &event, thread_id).await?;
            let Some(thread_id) = thread_id else {
                return Ok(());
            };
            let audio = {
                let mut realtime = state.realtime.lock().await;
                let digest = realtime.entry(thread_id.to_owned()).or_default();
                if event.method == "thread/realtime/started" {
                    digest.start(event.params.get("version").and_then(Value::as_str));
                    None
                } else if event.method == "thread/realtime/error" {
                    digest.failed = true;
                    if let Some(message) = event.params.get("message").and_then(Value::as_str) {
                        digest.close_reason = Some(truncate_chars(message, 300));
                    }
                    digest.dirty = true;
                    None
                } else {
                    let reason = event.params.get("reason").and_then(Value::as_str);
                    digest.close(reason);
                    digest.take_wav()
                }
            };
            flush_realtime(state, http).await?;
            if let Some((wav, truncated)) = audio {
                deliver_realtime_audio(state, http, thread_id, wav, truncated).await?;
            }
            return Ok(());
        }
        NotificationDisposition::RealtimeSignal => {
            persist_notification_audit(state, &event, thread_id).await?;
            let Some(thread_id) = thread_id else {
                return Ok(());
            };
            let mut realtime = state.realtime.lock().await;
            let digest = realtime.entry(thread_id.to_owned()).or_default();
            if event.method == "thread/realtime/sdp" {
                digest.note_sdp();
            } else if let Some(item) = event.params.get("item") {
                digest.note_item(item);
            }
            return Ok(());
        }
        NotificationDisposition::TokenUsage => {
            persist_notification_audit(state, &event, thread_id).await?;
            if let Some(thread_id) = thread_id
                && let Some(mut task) = state.store.task(thread_id).await?
            {
                task.last_event_at = Some(Utc::now());
                state.store.upsert_task(&task).await?;
            }
            return Ok(());
        }
        NotificationDisposition::RequestResolved => {
            persist_notification_audit(state, &event, thread_id).await?;
            retire_resolved_request(state, http, &event).await?;
            return Ok(());
        }
        NotificationDisposition::RunnerStatus => {
            persist_notification_audit(state, &event, thread_id).await?;
            refresh_runner_status(state, http).await?;
            return Ok(());
        }
        NotificationDisposition::UserAlert => {
            persist_notification_audit(state, &event, thread_id).await?;
            post_notification_alert(state, http, &event, thread_id).await?;
            return Ok(());
        }
        NotificationDisposition::AuditOnly => {
            persist_notification_audit(state, &event, thread_id).await?;
            return Ok(());
        }
        NotificationDisposition::Unknown => {
            persist_notification_audit(state, &event, thread_id).await?;
            post_notification_alert(state, http, &event, thread_id).await?;
            return Ok(());
        }
        NotificationDisposition::TaskLifecycle => {
            persist_notification_audit(state, &event, thread_id).await?;
        }
    }
    let Some(thread_id) = thread_id else {
        return Ok(());
    };
    let Some(mut task) = state.store.task(thread_id).await? else {
        return Ok(());
    };
    let turn_completed = event.method == "turn/completed";
    let state_changed = match event.method.as_str() {
        "turn/started" => {
            task.turn_id = event
                .params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            task.state = TaskState::Running;
            state.activity.lock().await.remove(thread_id);
            state.plans.lock().await.remove(thread_id);
            if let Some(channel) = task.channel_id.map(ChannelId::new) {
                provision::move_task_to_state(http, &state.config, channel, TaskState::Running)
                    .await?;
            }
            true
        }
        "turn/completed" => {
            task.state = if event
                .params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "failed")
            {
                TaskState::Failed
            } else {
                TaskState::Done
            };
            task.turn_id = None;
            if let Some(stream) = state.streams.lock().await.get_mut(thread_id) {
                stream.final_state = Some(task.state);
                stream.dirty = true;
            }
            flush_streams(state, http).await?;
            flush_activity(state, http).await?;
            flush_plans(state, http).await?;
            if let Some(channel) = task.channel_id.map(ChannelId::new) {
                provision::move_task_to_state(http, &state.config, channel, task.state).await?;
            }
            true
        }
        "thread/archived" | "thread/closed" | "thread/deleted" => {
            task.state = TaskState::Done;
            task.turn_id = None;
            if let Some(channel) = task.channel_id.map(ChannelId::new) {
                provision::move_task_to_state(http, &state.config, channel, TaskState::Done)
                    .await?;
            }
            true
        }
        "thread/unarchived" => {
            task.state = TaskState::Idle;
            if let Some(channel) = task.channel_id.map(ChannelId::new) {
                provision::move_task_to_state(http, &state.config, channel, TaskState::Idle)
                    .await?;
            }
            true
        }
        "thread/name/updated" => {
            if let Some(name) = event
                .params
                .get("threadName")
                .or_else(|| event.params.get("name"))
                .or_else(|| event.params.pointer("/thread/name"))
                .and_then(Value::as_str)
            {
                task.title = name.to_owned();
                if let Some(channel) = task.channel_id.map(ChannelId::new)
                    && let Err(error) =
                        provision::rename_task_channel(http, channel, &task.title, &task.thread_id)
                            .await
                {
                    warn!(%error, "could not follow Codex thread rename");
                }
            }
            true
        }
        "thread/compacted" => {
            let mut activity = state.activity.lock().await;
            let digest = activity.entry(thread_id.to_owned()).or_default();
            digest.lines.push("🗜 Context compacted".to_owned());
            digest.dirty = true;
            false
        }
        _ => false,
    };
    if !state_changed {
        return Ok(());
    }
    task.last_event_at = Some(Utc::now());
    state.store.upsert_task(&task).await?;
    if turn_completed {
        let god = state.god.read().await.clone();
        let active_for_task = god.active_global_session().await.is_some_and(|session| {
            matches!(session.scope, SessionScope::Task(ref task_id) if task_id == thread_id)
        });
        if active_for_task
            || god.cleanup_required_for(thread_id)
            || state.god_quarantined_tasks.contains_key(thread_id)
        {
            normalize_completed_god_turn(state, http, thread_id).await?;
        }
    }
    refresh_task_card(http, &task, None).await?;
    refresh_runner_status(state, http).await?;
    if matches!(
        event.method.as_str(),
        "thread/archived" | "thread/closed" | "thread/deleted"
    ) {
        state.realtime.lock().await.remove(thread_id);
        state.operations.lock().await.remove(thread_id);
        let media_prefix = format!("{thread_id}:");
        state
            .delivered_media
            .retain(|key| !key.starts_with(&media_prefix));
    }
    Ok(())
}

async fn persist_notification_audit(
    state: &BotState,
    event: &Notification,
    thread_id: Option<&str>,
) -> Result<()> {
    state
        .store
        .audit(
            "codex_notification",
            None,
            Some(state.config.guild_id),
            None,
            thread_id,
            &json!({
                "method": event.method,
                "params_hash": stable_value_hash(&event.params),
                "disposition": format!("{:?}", notifications::classify(&event.method))
            }),
        )
        .await
}

async fn post_notification_alert(
    state: &BotState,
    http: &Http,
    event: &Notification,
    thread_id: Option<&str>,
) -> Result<()> {
    let Some(layout) = state.layout.read().await.clone() else {
        return Ok(());
    };
    let message = event
        .params
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .params
                .pointer("/error/message")
                .and_then(Value::as_str)
        })
        .unwrap_or("Codex emitted an alert; sensitive parameters were redacted.")
        .chars()
        .take(1200)
        .collect::<String>();
    let mut embed = CreateEmbed::new()
        .title(format!("Codex: {}", event.method))
        .description(message)
        .color(0xED_4245)
        .timestamp(serenity::model::Timestamp::now());
    if let Some(thread_id) = thread_id {
        embed = embed.field("Task", format!("`{thread_id}`"), false);
    }
    layout
        .audit_log_channel
        .send_message(http, CreateMessage::new().embed(embed))
        .await?;
    Ok(())
}

async fn refresh_task_card(http: &Http, task: &TaskMirror, note: Option<&str>) -> Result<()> {
    let Some(channel_id) = task.channel_id.map(ChannelId::new) else {
        return Ok(());
    };
    let messages = channel_id
        .messages(http, serenity::builder::GetMessages::new().limit(100))
        .await?;
    let existing = messages.iter().find(|message| {
        message.author.bot
            && message.embeds.iter().any(|embed| {
                embed
                    .fields
                    .iter()
                    .any(|field| field.name == "Task" && field.value.contains(&task.thread_id))
            })
    });
    let done = matches!(
        task.state,
        TaskState::Done | TaskState::Failed | TaskState::Idle
    );
    let edit = EditMessage::new()
        .embed(embeds::task_card(task, note))
        .components(components::task_buttons(done));
    if let Some(message) = existing {
        channel_id.edit_message(http, message.id, edit).await?;
    } else {
        channel_id
            .send_message(
                http,
                CreateMessage::new()
                    .embed(embeds::task_card(task, note))
                    .components(components::task_buttons(done)),
            )
            .await?;
    }
    Ok(())
}

/// Re-register a pending server request (and its Discord card, when known)
/// after a failed reply so the owner can retry from the same card.
fn restore_pending_request(
    state: &BotState,
    token: &str,
    request: &crate::codex::ServerRequest,
    card: Option<(u64, u64)>,
) {
    state
        .pending_server_requests
        .insert(token.to_owned(), request.clone());
    state
        .pending_request_started
        .entry(token.to_owned())
        .or_insert_with(std::time::Instant::now);
    if let Some(card) = card {
        state
            .pending_request_messages
            .insert(token.to_owned(), card);
    }
}

fn expired_server_reply(request: &ServerRequest) -> ServerReply {
    match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::CommandExecutionApproval | ServerRequestMethod::FileChangeApproval => {
            requests::approval_reply(request, ApprovalDecision::Cancel)
                .unwrap_or_else(|_| requests::unsupported_reply(request))
        }
        ServerRequestMethod::LegacyApplyPatchApproval
        | ServerRequestMethod::LegacyExecCommandApproval => {
            requests::legacy_approval_reply(request, LegacyApprovalDecision::TimedOut)
                .unwrap_or_else(|_| requests::unsupported_reply(request))
        }
        ServerRequestMethod::McpElicitation => {
            requests::mcp_elicitation_reply(request, McpElicitationAction::Cancel, None)
                .unwrap_or_else(|_| requests::unsupported_reply(request))
        }
        ServerRequestMethod::PermissionsApproval => requests::permissions_reply(
            request,
            PermissionGrant {
                permissions: serde_json::Map::new(),
                scope: PermissionScope::Turn,
                strict_auto_review: None,
            },
        )
        .unwrap_or_else(|_| requests::unsupported_reply(request)),
        ServerRequestMethod::ToolUserInput => ServerReply::Error {
            id: request.id.clone(),
            error: RpcErrorObject {
                code: -32_000,
                message: "Discord input request expired".to_owned(),
                data: None,
            },
        },
        _ => requests::unsupported_reply(request),
    }
}

async fn expire_live_pending_requests(state: &BotState, http: &Http) -> Result<()> {
    let now = std::time::Instant::now();
    let expired = state
        .pending_request_started
        .iter()
        .filter(|entry| now.duration_since(*entry.value()) >= PENDING_REQUEST_TTL)
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for token in expired {
        state.pending_request_started.remove(&token);
        let Some((_, request)) = state.pending_server_requests.remove(&token) else {
            continue;
        };
        let card = state
            .pending_request_messages
            .remove(&token)
            .map(|(_, card)| card);
        if let Err(error) = send_server_reply(
            &state.codex,
            request.generation,
            expired_server_reply(&request),
        )
        .await
        {
            warn!(token, %error, "expired Codex request could not be answered");
        }
        if let Err(error) = state.store.remove_pending_request(&token).await {
            warn!(token, %error, "expired pending request row could not be removed");
        }
        if let Some((channel_id, message_id)) = card {
            ChannelId::new(channel_id)
                .edit_message(
                    http,
                    message_id,
                    EditMessage::new()
                        .embed(embeds::error_card(
                            "Request expired",
                            "Codex did not receive a decision within 15 minutes.",
                        ))
                        .components(Vec::new()),
                )
                .await
                .ok();
        }
        if let Some(thread_id) = request_thread_id(&request)
            && let Err(error) = reactivate_unblocked_task(state, http, &thread_id).await
        {
            warn!(thread_id, %error, "expired request task could not be reactivated");
        }
    }
    state
        .pending_request_started
        .retain(|token, _| state.pending_server_requests.contains_key(token));
    Ok(())
}

/// After a pending server request is answered or retired, put the task back
/// into `Running` (category move + card refresh included) unless other
/// requests are still waiting, in which case only its freshness is bumped.
async fn reactivate_unblocked_task(state: &BotState, http: &Http, thread_id: &str) -> Result<()> {
    let Some(mut task) = state.store.task(thread_id).await? else {
        return Ok(());
    };
    task.last_event_at = Some(Utc::now());
    if !has_pending_for_thread(state, thread_id) {
        task.state = TaskState::Running;
        state.store.upsert_task(&task).await?;
        if let Some(channel_id) = task.channel_id.map(ChannelId::new) {
            provision::move_task_to_state(http, &state.config, channel_id, TaskState::Running)
                .await?;
        }
    } else {
        state.store.upsert_task(&task).await?;
    }
    refresh_task_card(http, &task, None).await?;
    Ok(())
}

async fn retire_resolved_request(
    state: &BotState,
    http: &Http,
    event: &Notification,
) -> Result<()> {
    let Some(rpc_id) = event.params.get("requestId") else {
        return Ok(());
    };
    let thread_id = crate::codex::thread_id_from_params(&event.params);
    let token = state
        .pending_server_requests
        .iter()
        .find(|entry| {
            entry.value().id == *rpc_id
                && thread_id
                    .is_none_or(|id| request_thread_id(entry.value()).as_deref() == Some(id))
        })
        .map(|entry| entry.key().clone());
    let Some(token) = token else {
        let now = std::time::Instant::now();
        state
            .resolved_request_tombstones
            .retain(|_, created| now.duration_since(*created) < Duration::from_secs(5 * 60));
        state
            .resolved_request_tombstones
            .insert(resolved_request_key(event.generation, rpc_id), now);
        return Ok(());
    };
    state.pending_server_requests.remove(&token);
    state.pending_request_started.remove(&token);
    let card = state
        .pending_request_messages
        .remove(&token)
        .map(|(_, card)| card);
    state.store.remove_pending_request(&token).await?;
    if let Some((channel_id, message_id)) = card {
        ChannelId::new(channel_id)
            .edit_message(
                http,
                message_id,
                EditMessage::new()
                    .embed(embeds::info_card(
                        "Resolved in Codex Desktop",
                        "This request was answered from another Codex client.",
                    ))
                    .components(Vec::new()),
            )
            .await
            .ok();
    }
    if let Some(thread_id) = thread_id {
        reactivate_unblocked_task(state, http, thread_id).await?;
    }
    Ok(())
}

fn resolved_request_key(generation: u64, rpc_id: &Value) -> String {
    format!(
        "{generation}:{}",
        serde_json::to_string(rpc_id).unwrap_or_else(|_| "invalid".to_owned())
    )
}

async fn refresh_runner_status(state: &BotState, http: &Http) -> Result<()> {
    let Some(layout) = state.layout.read().await.clone() else {
        return Ok(());
    };
    let tasks = state.store.tasks().await?;
    let god = state.god.read().await.clone();
    let god_active = god.active_global_session().await.is_some();
    let dead_outbox = state
        .store
        .dead_outbox_count()
        .await
        .unwrap_or_else(|error| {
            warn!(%error, "dead outbox count unavailable for runner card");
            0
        });
    let embed = embeds::runner_card(
        state.codex.is_connected(),
        Some(env!("CARGO_PKG_VERSION")),
        &tasks,
        god_active,
        dead_outbox,
    );
    let cached = state.runner_status_message_id.load(Ordering::Acquire);
    if cached != 0 {
        match layout
            .runner_status_channel
            .edit_message(http, cached, EditMessage::new().embed(embed.clone()))
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => {
                warn!(%error, "cached runner status message disappeared; rediscovering");
                state.runner_status_message_id.store(0, Ordering::Release);
            }
        }
    }
    let messages = layout
        .runner_status_channel
        .messages(http, serenity::builder::GetMessages::new().limit(25))
        .await?;
    if let Some(message) = messages.iter().find(|message| {
        message.author.bot
            && message
                .embeds
                .iter()
                .any(|embed| embed.title.as_deref() == Some("Codex runner"))
    }) {
        state
            .runner_status_message_id
            .store(message.id.get(), Ordering::Release);
        layout
            .runner_status_channel
            .edit_message(http, message.id, EditMessage::new().embed(embed))
            .await?;
    } else {
        let message = layout
            .runner_status_channel
            .send_message(http, CreateMessage::new().embed(embed))
            .await?;
        state
            .runner_status_message_id
            .store(message.id.get(), Ordering::Release);
        if let Err(error) = message.pin(http).await {
            warn!(%error, "could not pin runner status card");
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerRequestSetupFailure {
    TaskLookup,
    TaskMissing,
    MissingChannel,
    PendingSave,
    DiscordDelivery,
}

impl ServerRequestSetupFailure {
    const fn message(self) -> &'static str {
        match self {
            Self::TaskLookup => "could not look up the Codex task in Discord relay state",
            Self::TaskMissing => "Codex task has no Discord mirror",
            Self::MissingChannel => "Codex task has no Discord channel",
            Self::PendingSave => "could not persist the pending Codex request",
            Self::DiscordDelivery => "could not deliver the Codex request to Discord",
        }
    }
}

fn server_request_setup_failure_reply(
    request: &ServerRequest,
    failure: ServerRequestSetupFailure,
) -> ServerReply {
    ServerReply::error(
        request,
        RpcErrorObject {
            code: -32_000,
            message: failure.message().to_owned(),
            data: None,
        },
    )
}

/// Fail closed after a server-request setup error. The correlated JSON-RPC
/// reply is attempted before the local error is surfaced, so Codex never
/// waits forever merely because Discord state or persistence failed.
async fn reject_server_request_setup(
    client: &CodexClient,
    request: &ServerRequest,
    failure: ServerRequestSetupFailure,
    cause: Option<anyhow::Error>,
) -> Result<()> {
    let reply = send_server_reply(
        client,
        request.generation,
        server_request_setup_failure_reply(request, failure),
    )
    .await;
    match (cause, reply) {
        (None, Ok(())) => Ok(()),
        (None, Err(reply_error)) => Err(reply_error),
        (Some(cause), Ok(())) => Err(cause.context(failure.message())),
        (Some(cause), Err(reply_error)) => Err(anyhow::anyhow!(
            "{}: {cause}; correlated Codex error reply also failed: {reply_error}",
            failure.message()
        )),
    }
}

async fn handle_server_request(
    state: &BotState,
    http: &Http,
    request: ServerRequest,
) -> Result<()> {
    if let Some(reply) = state.host_broker.handle_request(&request).await {
        send_server_reply(&state.codex, request.generation, reply).await?;
        return Ok(());
    }
    if let Some(reply) = requests::immediate_reply(&request, Utc::now().timestamp()) {
        send_server_reply(&state.codex, request.generation, reply).await?;
        return Ok(());
    }
    if matches!(
        ServerRequestMethod::classify(&request.method),
        ServerRequestMethod::CommandExecutionApproval | ServerRequestMethod::FileChangeApproval
    ) && requests::offered_approval_decisions(&request)
        .is_some_and(|decisions| decisions.is_empty())
    {
        send_server_reply(
            &state.codex,
            request.generation,
            ServerReply::Error {
                id: request.id.clone(),
                error: RpcErrorObject {
                    code: -32_602,
                    message: "app-server offered no supported approval decisions".to_owned(),
                    data: None,
                },
            },
        )
        .await?;
        return Ok(());
    }
    let thread_id = crate::codex::thread_id_from_params(&request.params);
    let Some(thread_id) = thread_id else {
        send_server_reply(
            &state.codex,
            request.generation,
            requests::unsupported_reply(&request),
        )
        .await?;
        return Ok(());
    };
    let resolution_key = resolved_request_key(request.generation, &request.id);
    if state
        .resolved_request_tombstones
        .remove(&resolution_key)
        .is_some()
    {
        return Ok(());
    }
    let mut task = match state.store.task(thread_id).await {
        Ok(Some(task)) => task,
        Ok(None) => {
            return reject_server_request_setup(
                &state.codex,
                &request,
                ServerRequestSetupFailure::TaskMissing,
                None,
            )
            .await;
        }
        Err(error) => {
            return reject_server_request_setup(
                &state.codex,
                &request,
                ServerRequestSetupFailure::TaskLookup,
                Some(error),
            )
            .await;
        }
    };
    let Some(channel_id) = task.channel_id else {
        return reject_server_request_setup(
            &state.codex,
            &request,
            ServerRequestSetupFailure::MissingChannel,
            None,
        )
        .await;
    };
    let channel = ChannelId::new(channel_id);
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let persisted_params = crate::security::redact_detail(request.params.clone());
    if let Err(error) = state
        .store
        .save_pending_request(
            &request_id,
            thread_id,
            crate::codex::turn_id_from_params(&request.params),
            &request.method,
            &persisted_params,
            &request.id,
        )
        .await
    {
        return reject_server_request_setup(
            &state.codex,
            &request,
            ServerRequestSetupFailure::PendingSave,
            Some(error),
        )
        .await;
    }
    state
        .pending_server_requests
        .insert(request_id.clone(), request.clone());
    state
        .pending_request_started
        .insert(request_id.clone(), std::time::Instant::now());
    if state
        .resolved_request_tombstones
        .remove(&resolution_key)
        .is_some()
        || !state.pending_server_requests.contains_key(&request_id)
    {
        state.pending_server_requests.remove(&request_id);
        state.pending_request_started.remove(&request_id);
        state.store.remove_pending_request(&request_id).await?;
        return Ok(());
    }
    task.state = TaskState::NeedsUser;
    task.last_event_at = Some(Utc::now());
    if let Err(error) = state.store.upsert_task(&task).await {
        warn!(thread_id, %error, "pending request state could not be mirrored; delivering approval card anyway");
    }
    if let Err(error) =
        provision::move_task_to_state(http, &state.config, channel, TaskState::NeedsUser).await
    {
        warn!(thread_id, %error, "task category could not be updated; delivering approval card anyway");
    }
    if let Err(error) = refresh_task_card(http, &task, Some("Codex needs your decision.")).await {
        warn!(thread_id, %error, "task summary could not be refreshed; delivering approval card anyway");
    }
    let controls = match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::McpElicitation
            if request.params.get("mode").and_then(Value::as_str) == Some("url") =>
        {
            components::elicitation_url_buttons(
                &request_id,
                request.params.get("url").and_then(Value::as_str),
            )
        }
        ServerRequestMethod::McpElicitation if mcp_elicitation_has_empty_form_schema(&request) => {
            components::elicitation_confirmation_buttons(&request_id)
        }
        ServerRequestMethod::ToolUserInput | ServerRequestMethod::McpElicitation => {
            components::answer_buttons(&request_id)
        }
        ServerRequestMethod::CommandExecutionApproval | ServerRequestMethod::FileChangeApproval
            if requests::offered_approval_decisions(&request).is_some() =>
        {
            let choices = requests::offered_approval_decisions(&request)
                .unwrap_or_default()
                .into_iter()
                .map(|decision| (decision.index, decision.label));
            components::offered_approval_buttons(&request_id, choices)
        }
        _ => components::approval_buttons(&request_id),
    };
    let sent = match channel
        .send_message(
            http,
            CreateMessage::new()
                .content(format!("<@{}>", state.config.owner_user_id))
                .embed(embeds::approval_card(&request.method, &request.params))
                .components(controls),
        )
        .await
    {
        Ok(message) => message,
        Err(error) => {
            state.pending_server_requests.remove(&request_id);
            state.pending_request_started.remove(&request_id);
            if let Err(cleanup_error) = state.store.remove_pending_request(&request_id).await {
                warn!(request_id, %cleanup_error, "failed Discord request left a persisted pending row");
            }
            return reject_server_request_setup(
                &state.codex,
                &request,
                ServerRequestSetupFailure::DiscordDelivery,
                Some(error.into()),
            )
            .await;
        }
    };
    state
        .pending_request_messages
        .insert(request_id.clone(), (channel.get(), sent.id.get()));
    if !state.pending_server_requests.contains_key(&request_id) {
        state.pending_request_messages.remove(&request_id);
        state.store.remove_pending_request(&request_id).await?;
        channel
            .edit_message(
                http,
                sent.id,
                EditMessage::new()
                    .embed(embeds::info_card(
                        "Resolved in Codex Desktop",
                        "This request was answered from another Codex client.",
                    ))
                    .components(Vec::new()),
            )
            .await
            .ok();
        reactivate_unblocked_task(state, http, thread_id).await?;
        return Ok(());
    }
    if let Err(error) = state
        .store
        .set_pending_request_message(&request_id, channel.get(), sent.id.get())
        .await
    {
        warn!(request_id, %error, "pending request card location could not be persisted");
    }
    Ok(())
}

async fn project_standalone_output(state: &BotState, event: &Notification) {
    let process_id = event
        .params
        .get("processId")
        .or_else(|| event.params.get("processHandle"))
        .and_then(Value::as_str);
    let Some(process_id) = process_id else {
        return;
    };
    let Some(delta) = event.params.get("deltaBase64").and_then(Value::as_str) else {
        return;
    };
    let stream = if event.params.get("stream").and_then(Value::as_str) == Some("stderr") {
        OutputStream::Stderr
    } else {
        OutputStream::Stdout
    };
    if let Err(error) = state.projection.lock().await.push_process_output(
        process_id,
        stream,
        OutputChunk::base64(delta),
    ) {
        warn!(
            process_id,
            ?error,
            "standalone process projection rejected a chunk"
        );
    }
}

async fn project_standalone_completion(state: &BotState, event: &Notification) {
    let Some(process_id) = event
        .params
        .get("processHandle")
        .or_else(|| event.params.get("processId"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let exit_code = event
        .params
        .get("exitCode")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let completion = exit_code.map_or(CompletionState::Unknown, |code| {
        if code == 0 {
            CompletionState::Succeeded
        } else {
            CompletionState::Failed
        }
    });
    let stdout = event
        .params
        .get("stdout")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(OutputChunk::utf8);
    let stderr = event
        .params
        .get("stderr")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(OutputChunk::utf8);
    if let Err(error) = state
        .projection
        .lock()
        .await
        .complete_process(process_id, completion, exit_code, stdout, stderr)
    {
        warn!(
            process_id,
            ?error,
            "standalone process completion projection failed"
        );
    }
}

async fn update_operation_progress(state: &BotState, event: &Notification) {
    let Some(thread_id) = crate::codex::thread_id_from_params(&event.params) else {
        return;
    };
    let Some(item_id) = event.params.get("itemId").and_then(Value::as_str) else {
        return;
    };
    let summary = {
        let mut projection = state.projection.lock().await;
        match event.method.as_str() {
            "item/commandExecution/outputDelta" => {
                let Some(delta) = event.params.get("delta").and_then(Value::as_str) else {
                    return;
                };
                if let Err(error) = projection.push_command_output(
                    thread_id,
                    item_id,
                    OutputStream::Stdout,
                    OutputChunk::utf8(delta),
                ) {
                    warn!(
                        thread_id,
                        item_id,
                        ?error,
                        "command output projection rejected a chunk"
                    );
                }
                projection
                    .command(thread_id, item_id)
                    .map(format_command_projection)
            }
            "item/mcpToolCall/progress" => {
                let Some(message) = event.params.get("message").and_then(Value::as_str) else {
                    return;
                };
                if let Err(error) = projection.push_mcp_progress(thread_id, item_id, message) {
                    warn!(
                        thread_id,
                        item_id,
                        ?error,
                        "MCP progress projection rejected an update"
                    );
                }
                projection
                    .mcp(thread_id, item_id)
                    .map(format_mcp_projection)
            }
            "item/fileChange/outputDelta" | "item/fileChange/patchUpdated" => {
                let files = projection_u32(&event.params, &["filesChanged", "fileCount"]);
                let applied = projection_u32(&event.params, &["hunksApplied", "completedHunks"]);
                let total = projection_u32(&event.params, &["hunksTotal", "totalHunks"]);
                if let Err(error) = projection.update_patch(
                    thread_id,
                    item_id,
                    PatchState::Running,
                    files,
                    applied,
                    total,
                    false,
                ) {
                    warn!(
                        thread_id,
                        item_id,
                        ?error,
                        "patch projection rejected an update"
                    );
                }
                projection
                    .patch(thread_id, item_id)
                    .map(format_patch_projection)
            }
            _ => None,
        }
    };
    if let Some(summary) = summary {
        state
            .operations
            .lock()
            .await
            .entry(thread_id.to_owned())
            .or_default()
            .upsert(item_id, summary);
    }
}

async fn project_terminal_interaction(state: &BotState, event: &Notification) {
    let (Some(thread_id), Some(item_id)) = (
        crate::codex::thread_id_from_params(&event.params),
        event.params.get("itemId").and_then(Value::as_str),
    ) else {
        return;
    };
    let byte_count = event
        .params
        .get("stdin")
        .and_then(Value::as_str)
        .map_or(0, str::len);
    let command = state
        .projection
        .lock()
        .await
        .command(thread_id, item_id)
        .map(format_command_projection)
        .unwrap_or_else(|| "⌨️ **Interactive command**".to_owned());
    state
        .operations
        .lock()
        .await
        .entry(thread_id.to_owned())
        .or_default()
        .upsert(
            item_id,
            format!("{command}\n↳ terminal input sent · {byte_count} bytes · content hidden"),
        );
}

async fn project_auto_approval_review(state: &BotState, event: &Notification) {
    let (Some(thread_id), Some(review_id)) = (
        crate::codex::thread_id_from_params(&event.params),
        event.params.get("reviewId").and_then(Value::as_str),
    ) else {
        return;
    };
    let action = match event.params.pointer("/action/type").and_then(Value::as_str) {
        Some("command") => "command",
        Some("execve") => "process",
        Some("applyPatch") => "file changes",
        Some("networkAccess") => "network access",
        Some("mcpToolCall") => "MCP tool",
        Some("requestPermissions") => "permissions",
        _ => "action",
    };
    let status = match event
        .params
        .pointer("/review/status")
        .and_then(Value::as_str)
    {
        Some("inProgress") => "reviewing",
        Some("approved") => "approved",
        Some("denied") => "denied",
        Some("timedOut") => "timed out",
        Some("aborted") => "aborted",
        _ => "updated",
    };
    let risk = match event
        .params
        .pointer("/review/riskLevel")
        .and_then(Value::as_str)
    {
        Some("low") => Some("low risk"),
        Some("medium") => Some("medium risk"),
        Some("high") => Some("high risk"),
        Some("critical") => Some("critical risk"),
        _ => None,
    };
    let authorization = match event
        .params
        .pointer("/review/userAuthorization")
        .and_then(Value::as_str)
    {
        Some("low") => Some("low authorization"),
        Some("medium") => Some("medium authorization"),
        Some("high") => Some("high authorization"),
        _ => None,
    };
    let duration = event
        .params
        .get("completedAtMs")
        .and_then(Value::as_i64)
        .zip(event.params.get("startedAtMs").and_then(Value::as_i64))
        .and_then(|(completed, started)| completed.checked_sub(started))
        .filter(|millis| *millis >= 0)
        .map(|millis| format!("{millis} ms"));
    let mut details = vec![action, status];
    details.extend(risk);
    details.extend(authorization);
    let mut summary = format!("🛡️ **Auto-approval review** · {}", details.join(" · "));
    if let Some(duration) = duration {
        summary.push_str(&format!(" · {duration}"));
    }
    state
        .operations
        .lock()
        .await
        .entry(thread_id.to_owned())
        .or_default()
        .upsert(&format!("review:{review_id}"), summary);
}

async fn project_hook_lifecycle(state: &BotState, event: &Notification) {
    let (Some(thread_id), Some(run)) = (
        crate::codex::thread_id_from_params(&event.params),
        event.params.get("run"),
    ) else {
        return;
    };
    let Some(run_id) = run.get("id").and_then(Value::as_str) else {
        return;
    };
    let event_name = safe_hook_value(run.get("eventName").and_then(Value::as_str), HOOK_EVENTS);
    let status = safe_hook_value(run.get("status").and_then(Value::as_str), HOOK_STATUSES);
    let mode = safe_hook_value(
        run.get("executionMode").and_then(Value::as_str),
        &["sync", "async"],
    );
    let handler = safe_hook_value(
        run.get("handlerType").and_then(Value::as_str),
        &["command", "prompt", "agent"],
    );
    let entry_count = run
        .get("entries")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let duration = run
        .get("durationMs")
        .and_then(Value::as_u64)
        .map(|millis| format!(" · {millis} ms"))
        .unwrap_or_default();
    let summary = format!(
        "🪝 **Hook** · {event_name} · {status} · {mode}/{handler} · {entry_count} entries{duration}"
    );
    state
        .operations
        .lock()
        .await
        .entry(thread_id.to_owned())
        .or_default()
        .upsert(&format!("hook:{run_id}"), summary);
}

const HOOK_EVENTS: &[&str] = &[
    "preToolUse",
    "permissionRequest",
    "postToolUse",
    "preCompact",
    "postCompact",
    "sessionStart",
    "userPromptSubmit",
    "subagentStart",
    "subagentStop",
    "stop",
];
const HOOK_STATUSES: &[&str] = &["running", "completed", "failed", "blocked", "stopped"];

fn safe_hook_value<'a>(value: Option<&'a str>, allowed: &[&str]) -> &'a str {
    value
        .filter(|value| allowed.contains(value))
        .unwrap_or("unknown")
}

async fn project_model_safety(state: &BotState, event: &Notification) {
    let Some(thread_id) = crate::codex::thread_id_from_params(&event.params) else {
        return;
    };
    let line = if event.method == "model/safetyBuffering/updated" {
        let active = event
            .params
            .get("showBufferingUi")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let faster = event
            .params
            .get("fasterModel")
            .is_some_and(|value| !value.is_null());
        format!(
            "🛡️ Model safety buffering · {}{}",
            if active { "active" } else { "clear" },
            if faster {
                " · faster option available"
            } else {
                ""
            }
        )
    } else {
        let count = event
            .params
            .get("verifications")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        format!("✅ Model verification · {count} check(s) received")
    };
    let mut activity = state.activity.lock().await;
    let digest = activity.entry(thread_id.to_owned()).or_default();
    digest.lines.push(line);
    if digest.lines.len() > 14 {
        digest.lines.remove(0);
    }
    digest.dirty = true;
}

async fn complete_operation_item(state: &BotState, thread_id: &str, item: &Value) {
    let Some(item_id) = item.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(item_type) = item.get("type").and_then(Value::as_str) else {
        return;
    };
    let completion = completion_state(item.get("status").and_then(Value::as_str));
    let summary = {
        let mut projection = state.projection.lock().await;
        match item_type {
            "commandExecution" => {
                let exit_code = item
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok());
                let authoritative = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .map(OutputChunk::utf8);
                if let Err(error) = projection.complete_command(
                    thread_id,
                    item_id,
                    completion,
                    exit_code,
                    authoritative,
                    None,
                ) {
                    warn!(
                        thread_id,
                        item_id,
                        ?error,
                        "command completion projection failed"
                    );
                }
                projection
                    .command(thread_id, item_id)
                    .map(format_command_projection)
            }
            "mcpToolCall" => {
                let authoritative = item.pointer("/error/message").and_then(Value::as_str);
                if let Err(error) =
                    projection.complete_mcp(thread_id, item_id, completion, authoritative)
                {
                    warn!(
                        thread_id,
                        item_id,
                        ?error,
                        "MCP completion projection failed"
                    );
                }
                projection
                    .mcp(thread_id, item_id)
                    .map(format_mcp_projection)
            }
            "fileChange" => {
                let files = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .and_then(|changes| u32::try_from(changes.len()).ok());
                let state_value = match completion {
                    CompletionState::Succeeded => PatchState::Applied,
                    CompletionState::Cancelled => PatchState::Cancelled,
                    CompletionState::Failed | CompletionState::Unknown => PatchState::Failed,
                };
                if let Err(error) = projection.update_patch(
                    thread_id,
                    item_id,
                    state_value,
                    files,
                    None,
                    None,
                    true,
                ) {
                    warn!(
                        thread_id,
                        item_id,
                        ?error,
                        "patch completion projection failed"
                    );
                }
                projection
                    .patch(thread_id, item_id)
                    .map(format_patch_projection)
            }
            _ => None,
        }
    };
    if let Some(summary) = summary {
        state
            .operations
            .lock()
            .await
            .entry(thread_id.to_owned())
            .or_default()
            .upsert(item_id, summary);
    }
}

fn completion_state(status: Option<&str>) -> CompletionState {
    match status.unwrap_or_default() {
        "completed" | "success" | "succeeded" => CompletionState::Succeeded,
        "cancelled" | "canceled" | "declined" | "aborted" => CompletionState::Cancelled,
        "failed" | "error" => CompletionState::Failed,
        _ => CompletionState::Unknown,
    }
}

fn projection_u32(params: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        params
            .get(*key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    })
}

fn format_command_projection(command: &crate::discord::projection::CommandProjection) -> String {
    let icon = if command.active {
        "⏳"
    } else if command.completion == Some(CompletionState::Succeeded) {
        "✅"
    } else {
        "❌"
    };
    let mut output = String::new();
    if !command.output.stdout.is_empty() {
        output.push_str(&command.output.stdout);
    }
    if !command.output.stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&command.output.stderr);
    }
    let output = truncate_chars(&output.replace('`', "ˋ"), 700);
    let markers = operation_markers(
        command.output.capped,
        command.output.incomplete,
        command.output.lagged,
    );
    if output.is_empty() {
        format!("{icon} **Command** · waiting for output{markers}")
    } else {
        format!("{icon} **Command**{markers}\n```text\n{output}\n```")
    }
}

fn format_mcp_projection(progress: &crate::discord::projection::McpProgressProjection) -> String {
    let icon = if progress.active {
        "🧰"
    } else if progress.completion == Some(CompletionState::Succeeded) {
        "✅"
    } else {
        "❌"
    };
    let latest = progress
        .messages
        .back()
        .map_or("Waiting for tool progress", String::as_str);
    let markers = operation_markers(false, progress.incomplete, progress.lagged);
    format!(
        "{icon} **MCP tool** · {}{markers}",
        truncate_chars(latest, 600)
    )
}

fn format_patch_projection(patch: &crate::discord::projection::PatchProjection) -> String {
    let icon = match patch.state {
        PatchState::Running => "📝",
        PatchState::Applied => "✅",
        PatchState::Failed | PatchState::Cancelled => "❌",
    };
    let counts = match (patch.hunks_applied, patch.hunks_total, patch.files_changed) {
        (Some(applied), Some(total), _) => format!(" · {applied}/{total} hunks"),
        (_, _, Some(files)) => format!(" · {files} files"),
        _ => String::new(),
    };
    let markers = operation_markers(false, patch.incomplete, patch.lagged);
    format!("{icon} **File changes**{counts}{markers}")
}

fn operation_markers(capped: bool, incomplete: bool, lagged: bool) -> String {
    let mut markers = Vec::new();
    if capped {
        markers.push("truncated");
    }
    if incomplete {
        markers.push("incomplete");
    }
    if lagged {
        markers.push("stream lagged");
    }
    if markers.is_empty() {
        String::new()
    } else {
        format!(" · ⚠ {}", markers.join(", "))
    }
}

async fn flush_activity(state: &BotState, http: &Http) -> Result<()> {
    flush_digests(
        state,
        http,
        &state.activity,
        |digest| {
            std::mem::take(&mut digest.dirty)
                .then(|| DigestRender::embed(embeds::activity_card(&digest.lines)))
        },
        |digest| digest.dirty = true,
        |digest| &mut digest.message_id,
    )
    .await
}

async fn flush_plans(state: &BotState, http: &Http) -> Result<()> {
    flush_digests(
        state,
        http,
        &state.plans,
        |board| {
            std::mem::take(&mut board.dirty)
                .then(|| DigestRender::embed(embeds::plan_card(&board.params)))
        },
        |board| board.dirty = true,
        |board| &mut board.message_id,
    )
    .await
}

async fn flush_operations(state: &BotState, http: &Http) -> Result<()> {
    flush_digests(
        state,
        http,
        &state.operations,
        |digest| {
            std::mem::take(&mut digest.dirty)
                .then(|| DigestRender::embed(embeds::live_operations_card(&digest.summaries())))
        },
        |digest| digest.dirty = true,
        |digest| &mut digest.message_id,
    )
    .await
}

async fn flush_realtime(state: &BotState, http: &Http) -> Result<()> {
    flush_digests(
        state,
        http,
        &state.realtime,
        |digest| {
            std::mem::take(&mut digest.dirty).then(|| DigestRender {
                embed: embeds::realtime_card(
                    &digest.transcript_text(),
                    digest.active,
                    digest.failed,
                    digest.version.as_deref(),
                    digest.close_reason.as_deref(),
                    digest.event_count,
                    digest.last_item_kind.as_deref(),
                    digest.webrtc_ready,
                ),
                components: components::realtime_buttons(digest.active),
            })
        },
        |digest| digest.dirty = true,
        |digest| &mut digest.message_id,
    )
    .await
}

#[derive(Clone, Copy)]
struct ImageFormat {
    extension: &'static str,
}

async fn deliver_completed_media(
    state: &BotState,
    _http: &Http,
    thread_id: &str,
    item: &Value,
) -> Result<()> {
    let Some(item_type @ ("imageGeneration" | "imageView")) =
        item.get("type").and_then(Value::as_str)
    else {
        return Ok(());
    };
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| stable_value_hash(item));
    let delivery_key = format!("{thread_id}:{item_id}");
    if !state.delivered_media.insert(delivery_key.clone()) {
        return Ok(());
    }

    let result = async {
        let Some(task) = state.store.task(thread_id).await? else {
            return Ok(false);
        };
        let Some(channel_id) = task.channel_id else {
            return Ok(false);
        };
        let Some((bytes, format)) = load_completed_image(&task, item, item_type).await else {
            warn!(
                thread_id,
                item_id, "generated image had no safe bounded attachment source"
            );
            return Ok(false);
        };
        let filename = format!("codex-generated.{}", format.extension);
        let status =
            item.get("status")
                .and_then(Value::as_str)
                .unwrap_or(if item_type == "imageView" {
                    "viewed"
                } else {
                    "completed"
                });
        let successful = matches!(status, "completed" | "viewed" | "success" | "succeeded");
        let title = match (item_type, successful) {
            ("imageView", _) => "🖼️ Codex viewed an image",
            (_, true) => "🖼️ Codex generated an image",
            _ => "🖼️ Image generation did not complete",
        };
        let mut description = format!(
            "Status: `{}` · {} KiB",
            truncate_chars(status, 40),
            bytes.len().div_ceil(1024)
        );
        if let Some(prompt) = item.get("revisedPrompt").and_then(Value::as_str) {
            description.push_str("\n\n**Revised prompt**\n");
            description.push_str(&truncate_chars(prompt, 500));
        }
        enqueue_media_attachment(
            state,
            &format!("media:{thread_id}:{item_id}"),
            channel_id,
            title,
            &description,
            if successful { 0x0057_F287 } else { 0x00ED_4245 },
            &filename,
            true,
            bytes,
        )
        .await?;
        Ok::<_, anyhow::Error>(true)
    }
    .await;

    match result {
        Ok(true) => Ok(()),
        Ok(false) => {
            state.delivered_media.remove(&delivery_key);
            Ok(())
        }
        Err(error) => {
            state.delivered_media.remove(&delivery_key);
            Err(error)
        }
    }
}

async fn load_completed_image(
    task: &TaskMirror,
    item: &Value,
    item_type: &str,
) -> Option<(Vec<u8>, ImageFormat)> {
    if let Some(cwd) = task.cwd.as_deref() {
        let path = if item_type == "imageView" {
            item.get("path").and_then(Value::as_str)
        } else {
            item.get("savedPath").and_then(Value::as_str)
        };
        if let Some(path) = path
            && let Some(image) = read_workspace_image(Path::new(cwd), Path::new(path)).await
        {
            return Some(image);
        }
    }
    (item_type == "imageGeneration")
        .then(|| item.get("result").and_then(Value::as_str))
        .flatten()
        .and_then(decode_inline_image)
}

async fn read_workspace_image(
    workspace: &Path,
    candidate: &Path,
) -> Option<(Vec<u8>, ImageFormat)> {
    if !workspace.is_absolute() || !candidate.is_absolute() {
        return None;
    }
    let workspace = tokio::fs::canonicalize(workspace).await.ok()?;
    let candidate = tokio::fs::canonicalize(candidate).await.ok()?;
    if !candidate.starts_with(&workspace) {
        return None;
    }
    let mut file = tokio::fs::File::open(&candidate).await.ok()?;
    let metadata = file.metadata().await.ok()?;
    if !metadata.is_file() || metadata.len() > u64::try_from(MAX_DISCORD_ATTACHMENT_BYTES).ok()? {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
    (&mut file)
        .take(u64::try_from(MAX_DISCORD_ATTACHMENT_BYTES + 1).ok()?)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    validate_image_bytes(bytes)
}

fn decode_inline_image(result: &str) -> Option<(Vec<u8>, ImageFormat)> {
    let encoded = if let Some(data_url) = result.strip_prefix("data:") {
        let (metadata, encoded) = data_url.split_once(',')?;
        if !metadata.ends_with(";base64") || !metadata.starts_with("image/") {
            return None;
        }
        encoded
    } else {
        // The installed Codex schema permits raw base64 here. URLs and paths
        // fail strict base64 decoding and are deliberately never fetched/read.
        result
    };
    let max_encoded = MAX_DISCORD_ATTACHMENT_BYTES.div_ceil(3) * 4 + 4;
    if encoded.is_empty() || encoded.len() > max_encoded {
        return None;
    }
    let bytes = BASE64_STANDARD.decode(encoded).ok()?;
    validate_image_bytes(bytes)
}

fn validate_image_bytes(bytes: Vec<u8>) -> Option<(Vec<u8>, ImageFormat)> {
    if bytes.is_empty() || bytes.len() > MAX_DISCORD_ATTACHMENT_BYTES {
        return None;
    }
    let extension = if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.len() >= 4 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if bytes.len() >= 13 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        "gif"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "webp"
    } else {
        return None;
    };
    Some((bytes, ImageFormat { extension }))
}

async fn deliver_realtime_audio(
    state: &BotState,
    _http: &Http,
    thread_id: &str,
    wav: Vec<u8>,
    truncated: bool,
) -> Result<()> {
    let Some(task) = state.store.task(thread_id).await? else {
        return Ok(());
    };
    let Some(channel_id) = task.channel_id else {
        return Ok(());
    };
    let note = if truncated {
        "The session exceeded Relay's bounded audio buffer; this attachment contains the retained prefix."
    } else {
        "Complete realtime audio captured from Codex."
    };
    let content_hash = stable_bytes_hash(&wav);
    enqueue_media_attachment(
        state,
        &format!("realtime-audio:{thread_id}:{content_hash}"),
        channel_id,
        "🔊 Realtime audio",
        note,
        if truncated { 0x00FE_E75C } else { 0x0057_F287 },
        "codex-realtime.wav",
        false,
        wav,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn enqueue_media_attachment(
    state: &BotState,
    dedupe_key: &str,
    channel_id: u64,
    title: &str,
    description: &str,
    color: u32,
    filename: &str,
    display_image: bool,
    bytes: Vec<u8>,
) -> Result<()> {
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_DISCORD_ATTACHMENT_BYTES,
        "media attachment exceeds Relay's safe upload limit"
    );
    anyhow::ensure!(
        safe_media_name(filename),
        "media attachment filename is unsafe"
    );
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("bin");
    let spool_name = format!("{}.{}", stable_value_hash(&json!(dedupe_key)), extension);
    let spool_root = config::data_dir().join("outbox-media");
    tokio::fs::create_dir_all(&spool_root).await?;
    let spool_path = spool_root.join(&spool_name);
    if !spool_path.exists() {
        let temporary = spool_root.join(format!("{spool_name}.tmp"));
        tokio::fs::write(&temporary, &bytes).await?;
        match tokio::fs::rename(&temporary, &spool_path).await {
            Ok(()) => {}
            Err(error) if spool_path.exists() => {
                let _ = tokio::fs::remove_file(&temporary).await;
                drop(error);
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Err(error) = state
        .store
        .enqueue_outbox(
            dedupe_key,
            channel_id,
            "media_attachment",
            &json!({
                "file": spool_name,
                "filename": filename,
                "title": truncate_chars(title, 256),
                "description": truncate_chars(description, 2_000),
                "color": color,
                "display_image": display_image
            }),
        )
        .await
    {
        let _ = tokio::fs::remove_file(spool_path).await;
        return Err(error);
    }
    Ok(())
}

/// Shared per-task digest delivery: snapshot dirty embeds, edit the existing
/// digest message when possible, otherwise send a fresh one and remember it.
struct DigestRender {
    embed: CreateEmbed,
    components: Vec<CreateActionRow>,
}

impl DigestRender {
    fn embed(embed: CreateEmbed) -> Self {
        Self {
            embed,
            components: Vec::new(),
        }
    }
}

async fn flush_digests<T>(
    state: &BotState,
    http: &Http,
    digests: &Mutex<HashMap<String, T>>,
    mut take_dirty: impl FnMut(&mut T) -> Option<DigestRender>,
    mut mark_dirty: impl FnMut(&mut T),
    message_id: impl Fn(&mut T) -> &mut Option<u64>,
) -> Result<()> {
    let pending: Vec<(String, DigestRender, Option<u64>)> = {
        let mut map = digests.lock().await;
        map.iter_mut()
            .filter_map(|(thread, digest)| {
                take_dirty(digest).map(|render| (thread.clone(), render, *message_id(digest)))
            })
            .collect()
    };
    for (thread_id, render, existing) in pending {
        let Some(task) = state.store.task(&thread_id).await? else {
            if let Some(digest) = digests.lock().await.get_mut(&thread_id) {
                mark_dirty(digest);
            }
            continue;
        };
        let Some(channel) = task.channel_id.map(ChannelId::new) else {
            if let Some(digest) = digests.lock().await.get_mut(&thread_id) {
                mark_dirty(digest);
            }
            continue;
        };
        let delivered = if let Some(existing) = existing {
            match channel
                .edit_message(
                    http,
                    existing,
                    EditMessage::new()
                        .embed(render.embed.clone())
                        .components(render.components.clone())
                        .allowed_mentions(CreateAllowedMentions::new()),
                )
                .await
            {
                Ok(_) => Some(existing),
                Err(_) => send_digest_message(http, channel, render).await,
            }
        } else {
            send_digest_message(http, channel, render).await
        };
        if let Some(digest) = digests.lock().await.get_mut(&thread_id) {
            if let Some(delivered) = delivered {
                *message_id(digest) = Some(delivered);
            } else {
                mark_dirty(digest);
            }
        }
    }
    Ok(())
}

async fn send_digest_message(http: &Http, channel: ChannelId, render: DigestRender) -> Option<u64> {
    match channel
        .send_message(
            http,
            CreateMessage::new()
                .embed(render.embed)
                .components(render.components)
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await
    {
        Ok(message) => Some(message.id.get()),
        Err(error) => {
            warn!(channel_id = channel.get(), %error, "could not deliver digest card");
            None
        }
    }
}

async fn flush_streams(state: &BotState, http: &Http) -> Result<()> {
    let pending: Vec<_> = {
        let mut streams = state.streams.lock().await;
        streams
            .iter_mut()
            .filter_map(|(thread, stream)| {
                let text = stream.text();
                if !stream.dirty || text.is_empty() {
                    return None;
                }
                Some((thread.clone(), text, stream.message_id, stream.final_state))
            })
            .collect()
    };
    for (thread_id, text, message_id, final_state) in pending {
        let Some(task) = state.store.task(&thread_id).await? else {
            continue;
        };
        let Some(channel_id) = task.channel_id.map(ChannelId::new) else {
            continue;
        };
        if let Some(final_state) = final_state {
            state
                .store
                .enqueue_outbox(
                    &format!(
                        "final-answer:{thread_id}:{}:{}",
                        task.turn_id.as_deref().unwrap_or("no-turn"),
                        stable_value_hash(&json!(text))
                    ),
                    channel_id.get(),
                    "final_answer",
                    &json!({
                        "thread_id": thread_id,
                        "text": text,
                        "state": final_state,
                        "message_id": message_id
                    }),
                )
                .await?;
            state.streams.lock().await.remove(&thread_id);
            continue;
        }
        let pages = embeds::split_answer(&text, 3900);
        let display_page = pages.len().saturating_sub(1);
        let display_text = pages.get(display_page).map(String::as_str).unwrap_or("");
        let state_label = TaskState::Running;
        let embed = embeds::codex_answer(
            "Codex is working…",
            display_text,
            state_label,
            display_page + 1,
            pages.len(),
        );
        let new_id = if let Some(message_id) = message_id {
            channel_id
                .edit_message(http, message_id, EditMessage::new().embed(embed))
                .await?;
            message_id
        } else {
            channel_id
                .send_message(http, CreateMessage::new().embed(embed))
                .await?
                .id
                .get()
        };
        if let Some(stream) = state.streams.lock().await.get_mut(&thread_id) {
            stream.message_id = Some(new_id);
            stream.dirty = stream.text() != text;
        }
    }
    Ok(())
}

/// Final-answer card parts: first page embed, task buttons, and the full text
/// as an attachment when it spans multiple pages.
fn final_answer_message(
    text: &str,
    state_label: TaskState,
) -> (
    CreateEmbed,
    Vec<serenity::builder::CreateActionRow>,
    Option<CreateAttachment>,
) {
    let pages = embeds::split_answer(text, 3900);
    let embed = embeds::codex_answer(
        "Codex answer",
        pages.first().map(String::as_str).unwrap_or(""),
        state_label,
        1,
        pages.len().max(1),
    );
    let buttons = components::task_buttons(matches!(
        state_label,
        TaskState::Done | TaskState::Failed | TaskState::Idle
    ));
    let attachment = (pages.len() > 1)
        .then(|| CreateAttachment::bytes(text.as_bytes().to_vec(), "codex-answer.md"));
    (embed, buttons, attachment)
}

fn safe_media_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !value.starts_with('.')
        && !value.contains("..")
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

fn media_outbox_path(payload: &Value) -> Option<PathBuf> {
    let file = payload.get("file").and_then(Value::as_str)?;
    safe_media_name(file).then(|| config::data_dir().join("outbox-media").join(file))
}

fn spawn_outbox_flusher(state: Arc<BotState>, http: Arc<Http>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            let Ok(rows) = state.store.pending_outbox(25).await else {
                warn!("outbox read failed");
                continue;
            };
            for row in rows {
                let channel = ChannelId::new(row.channel_id);
                let result = match row.kind.as_str() {
                    "final_answer" => {
                        let text = row
                            .payload
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let state_label = outbox_task_state(&row.payload);
                        let message_id = row.payload.get("message_id").and_then(Value::as_u64);
                        let edited = if let Some(message_id) = message_id {
                            let (embed, buttons, attachment) =
                                final_answer_message(text, state_label);
                            let mut edit = EditMessage::new().embed(embed).components(buttons);
                            if let Some(attachment) = attachment {
                                edit = edit.new_attachment(attachment);
                            }
                            channel
                                .edit_message(&http, message_id, edit)
                                .await
                                .map(|_| ())
                        } else {
                            Err(serenity::Error::Other("stream message absent"))
                        };
                        match edited {
                            Ok(()) => Ok(()),
                            Err(_) => {
                                let (embed, buttons, attachment) =
                                    final_answer_message(text, state_label);
                                let mut create =
                                    CreateMessage::new().embed(embed).components(buttons);
                                if let Some(attachment) = attachment {
                                    create = create.add_file(attachment);
                                }
                                channel.send_message(&http, create).await.map(|_| ())
                            }
                        }
                    }
                    "media_attachment" => {
                        async {
                            let path = media_outbox_path(&row.payload)
                                .ok_or(serenity::Error::Other("media outbox path is invalid"))?;
                            let metadata = tokio::fs::metadata(&path).await.map_err(|_| {
                                serenity::Error::Other("media outbox file is missing")
                            })?;
                            if !metadata.is_file()
                                || metadata.len()
                                    > u64::try_from(MAX_DISCORD_ATTACHMENT_BYTES)
                                        .unwrap_or(u64::MAX)
                            {
                                Err(serenity::Error::Other("media outbox file is invalid"))?
                            }
                            let bytes = tokio::fs::read(&path).await.map_err(|_| {
                                serenity::Error::Other("media outbox file is unreadable")
                            })?;
                            let filename = row
                                .payload
                                .get("filename")
                                .and_then(Value::as_str)
                                .filter(|filename| safe_media_name(filename))
                                .unwrap_or("codex-media.bin");
                            let title = row
                                .payload
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or("Codex media");
                            let description = row
                                .payload
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("Codex produced a media attachment.");
                            let color = row
                                .payload
                                .get("color")
                                .and_then(Value::as_u64)
                                .and_then(|value| u32::try_from(value).ok())
                                .unwrap_or(0x0057_F287);
                            let mut embed = CreateEmbed::new()
                                .title(truncate_chars(title, 256))
                                .description(truncate_chars(description, 2_000))
                                .color(color)
                                .timestamp(serenity::model::Timestamp::now());
                            if row
                                .payload
                                .get("display_image")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                            {
                                embed = embed.image(format!("attachment://{filename}"));
                            }
                            channel
                                .send_message(
                                    &http,
                                    CreateMessage::new()
                                        .embed(embed)
                                        .allowed_mentions(CreateAllowedMentions::new())
                                        .add_file(CreateAttachment::bytes(bytes, filename)),
                                )
                                .await
                                .map(|_| ())
                        }
                        .await
                    }
                    // An unknown kind must not be acked as delivered: a row
                    // written by a newer relay build should survive until that
                    // build processes it, or dead-letter after the retry cap.
                    other => Err(serenity::Error::Other(if other.is_empty() {
                        "outbox row has no kind"
                    } else {
                        "unknown outbox row kind"
                    })),
                };
                match result {
                    Ok(()) => {
                        if let Err(error) = state.store.mark_outbox_sent(row.id).await {
                            warn!(row_id = row.id, %error, "outbox ack failed");
                        } else if row.kind == "media_attachment"
                            && let Some(path) = media_outbox_path(&row.payload)
                        {
                            let _ = tokio::fs::remove_file(path).await;
                        }
                    }
                    Err(error) => {
                        if let Err(mark_error) = state
                            .store
                            .mark_outbox_attempt(row.id, &error.to_string())
                            .await
                        {
                            warn!(row_id = row.id, %mark_error, "outbox failure record failed");
                        }
                    }
                }
            }
        }
    });
}

fn spawn_sensitive_deletion_flusher(state: Arc<BotState>, http: Arc<Http>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let rows = match state.store.pending_sensitive_deletions(50).await {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(%error, "sensitive deletion queue read failed");
                    continue;
                }
            };
            for row in rows {
                match ChannelId::new(row.channel_id)
                    .delete_message(&http, row.message_id)
                    .await
                {
                    Ok(()) => {
                        if let Err(error) = state
                            .store
                            .finish_sensitive_deletion(row.channel_id, row.message_id)
                            .await
                        {
                            warn!(%error, "sensitive deletion acknowledgement failed");
                        }
                    }
                    Err(error) if is_absent_discord_message_error(&error) => {
                        if let Err(error) = state
                            .store
                            .finish_sensitive_deletion(row.channel_id, row.message_id)
                            .await
                        {
                            warn!(%error, "absent sensitive deletion acknowledgement failed");
                        }
                    }
                    Err(error) => {
                        if let Err(store_error) = state
                            .store
                            .fail_sensitive_deletion(
                                row.channel_id,
                                row.message_id,
                                &error.to_string(),
                            )
                            .await
                        {
                            warn!(%store_error, "sensitive deletion retry could not be recorded");
                        }
                        warn!(
                            channel_id = row.channel_id,
                            message_id = row.message_id,
                            attempts = row.attempts + 1,
                            %error,
                            "sensitive GOD message deletion still pending"
                        );
                    }
                }
            }
        }
    });
}

fn outbox_task_state(payload: &Value) -> TaskState {
    payload
        .get("state")
        .and_then(Value::as_str)
        .and_then(|value| match value {
            "idle" => Some(TaskState::Idle),
            "running" => Some(TaskState::Running),
            "needs_user" => Some(TaskState::NeedsUser),
            "done" => Some(TaskState::Done),
            "failed" => Some(TaskState::Failed),
            _ => None,
        })
        .unwrap_or(TaskState::Done)
}

async fn reconcile(state: Arc<BotState>, http: Arc<Http>) -> Result<()> {
    expire_stale_pending_requests(&state, &http).await?;
    for (thread_id, reason) in state.store.god_dirty_tasks().await? {
        state
            .god_quarantined_tasks
            .insert(thread_id.clone(), format!("startup GOD recovery: {reason}"));
        if let Err(error) = cleanup_god_thread(
            &state,
            &http,
            &thread_id,
            "startup recovery of persisted GOD-dirty task",
        )
        .await
        {
            error!(%thread_id, %error, "startup GOD cleanup failed; task remains quarantined");
        }
    }
    let tasks = state.store.tasks().await?;
    for task in tasks {
        if let Err(error) = reconcile_task(&state, &http, task.clone()).await {
            warn!(thread_id = %task.thread_id, %error, "task reconciliation failed; continuing");
        }
    }
    Ok(())
}

/// JSON-RPC request ids belong to one app-server process. After a relay
/// restart, retire persisted Discord cards instead of leaving live-looking
/// buttons that can never answer the old process.
async fn expire_stale_pending_requests(state: &BotState, http: &Http) -> Result<()> {
    for row in state.store.pending_requests().await? {
        if state.pending_server_requests.contains_key(&row.request_id) {
            continue;
        }
        if let (Some(channel_id), Some(message_id)) = (row.channel_id, row.message_id)
            && let Err(error) = ChannelId::new(channel_id)
                .edit_message(
                    http,
                    message_id,
                    EditMessage::new()
                        .embed(embeds::warning_card(
                            "Request expired",
                            &format!(
                                "The relay restarted before `{}` was answered. Codex will ask again if it still needs a decision.",
                                row.method
                            ),
                        ))
                        .components(Vec::new()),
                )
                .await
        {
            warn!(channel_id, message_id, %error, "could not retire stale request card");
        }
        state.store.remove_pending_request(&row.request_id).await?;
        info!(
            request_id = %row.request_id,
            method = %row.method,
            thread_id = %row.thread_id,
            "retired stale pending request"
        );
    }
    Ok(())
}

async fn reconcile_task(state: &BotState, http: &Http, mut task: TaskMirror) -> Result<()> {
    let Some(channel_id) = task.channel_id.map(ChannelId::new) else {
        return Ok(());
    };
    let lock = state
        .ingestion_locks
        .entry(channel_id.get())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _guard = lock.lock().await;
    if channel_id.to_channel(http).await.is_err() {
        warn!(thread_id = %task.thread_id, channel_id = channel_id.get(), "Discord mirror missing; detaching it");
        state.store.detach_channel(&task.thread_id).await?;
        return Ok(());
    }
    provision::repair_private_channel(http, &state.config, channel_id).await?;
    let read = state
        .codex
        .request_value(
            "thread/read",
            json!({"threadId": task.thread_id, "includeTurns": true}),
        )
        .await?;
    let latest_turn = read
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .and_then(|turns| turns.last());
    let active_turn_id = latest_turn
        .and_then(|turn| {
            let active = turn
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "inProgress" | "running" | "active"));
            active
                .then(|| turn.get("id").and_then(Value::as_str))
                .flatten()
        })
        .map(str::to_owned);
    task.turn_id.clone_from(&active_turn_id);
    task.state = if active_turn_id.is_some() {
        TaskState::Running
    } else {
        TaskState::Idle
    };
    state.store.upsert_task(&task).await?;

    let mut after = state.store.cursor(channel_id.get()).await?.unwrap_or(1);
    let mut scanned = 0_u64;
    let limit = state.config.history_scan_limit.max(1);
    let mut queued = Vec::new();
    while scanned < limit {
        let page_limit = (limit - scanned).min(100) as u8;
        let mut page = channel_id
            .messages(
                http,
                serenity::builder::GetMessages::new()
                    .limit(page_limit)
                    .after(after),
            )
            .await?;
        if page.is_empty() {
            break;
        }
        page.sort_by_key(|message| message.id);
        scanned += page.len() as u64;
        after = page.last().map_or(after, |message| message.id.get());
        let short_page = page.len() < usize::from(page_limit);
        for message in page {
            if message.author.bot || message.author.id.get() != state.config.owner_user_id {
                continue;
            }
            if is_sensitive_god_message(&message.content) {
                anyhow::ensure!(
                    delete_sensitive_discord_message(&message, http)
                        .await
                        .is_ok(),
                    "sensitive GOD message {} could not be deleted; cursor not advanced",
                    message.id.get()
                );
                continue;
            }
            queued.push(message);
        }
        if short_page {
            break;
        }
    }
    if queued.is_empty() {
        state.store.set_cursor(channel_id.get(), after).await?;
        return Ok(());
    }
    let mut input = Vec::new();
    for message in &queued {
        if !message.content.trim().is_empty() {
            input.push(UserInput::text(format!(
                "[Queued Discord message {}]\n{}",
                message.id, message.content
            )));
        }
        for attachment in &message.attachments {
            let local = cache_discord_attachment(attachment, message.id.get()).await?;
            if attachment
                .content_type
                .as_deref()
                .is_some_and(|kind| kind.starts_with("image/"))
            {
                input.push(UserInput::LocalImage {
                    path: local.display().to_string(),
                    detail: Some("auto".into()),
                });
            } else {
                input.push(UserInput::text(format!(
                    "Queued attachment cached locally: {} ({})",
                    attachment.filename,
                    local.display()
                )));
            }
        }
    }
    if !input.is_empty() {
        if let Some(expected_turn_id) = active_turn_id {
            state
                .codex
                .turn_steer(TurnSteerParams {
                    thread_id: task.thread_id.clone(),
                    expected_turn_id,
                    input,
                    extra: BTreeMap::new(),
                })
                .await?;
        } else {
            let result = state
                .codex
                .turn_start(CodexExecutionPolicy::NORMAL.turn_start_params(
                    task.thread_id.clone(),
                    input,
                    task.cwd.clone(),
                    task.model.clone(),
                ))
                .await?;
            task.turn_id = result
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            task.state = TaskState::Running;
            state.store.upsert_task(&task).await?;
        }
        provision::move_task_to_state(http, &state.config, channel_id, TaskState::Running).await?;
    }
    if let Some(last) = queued.last() {
        state
            .store
            .set_cursor(channel_id.get(), last.id.get())
            .await?;
    }
    for message in queued {
        if let Err(error) = message.react(http, '✅').await {
            warn!(message_id = message.id.get(), %error, "could not mark queued message delivered");
        }
    }
    Ok(())
}

async fn show_new_task_modal(http: &Http, command: &CommandInteraction) -> Result<()> {
    command
        .create_response(
            http,
            CreateInteractionResponse::Modal(
                CreateModal::new(components::NEW_TASK_MODAL, "Start Codex task")
                    .components(components::new_task_inputs()),
            ),
        )
        .await?;
    Ok(())
}
async fn show_god_modal(http: &Http, command: &CommandInteraction, minutes: u64) -> Result<()> {
    command
        .create_response(
            http,
            CreateInteractionResponse::Modal(
                CreateModal::new(
                    components::GOD_MODAL,
                    format!("Unlock GOD mode ({minutes} minutes)"),
                )
                .components(components::god_password_input()),
            ),
        )
        .await?;
    Ok(())
}
async fn show_email_modal(http: &Http, command: &CommandInteraction) -> Result<()> {
    command
        .create_response(
            http,
            CreateInteractionResponse::Modal(
                CreateModal::new(components::EMAIL_MODAL, "Send email with Codex")
                    .components(components::email_inputs()),
            ),
        )
        .await?;
    Ok(())
}

fn selected_value(component: &ComponentInteraction) -> Result<&str> {
    let ComponentInteractionDataKind::StringSelect { values } = &component.data.kind else {
        anyhow::bail!("control returned the wrong component type");
    };
    values
        .first()
        .map(String::as_str)
        .context("no value selected")
}

fn action_modal(draft: &ActionDraft, token: &str, page: usize) -> CreateModal {
    let title = format!("{} ({}/{})", draft.label, page + 1, draft.page_count())
        .chars()
        .take(45)
        .collect::<String>();
    CreateModal::new(
        format!("{}:{token}:{page}", components::ACTION_FORM_MODAL),
        title,
    )
    .components(components::action_form_inputs(draft.page_fields(page)))
}

fn action_confirmation(draft: &ActionDraft, token: &str) -> (CreateEmbed, Vec<CreateActionRow>) {
    let preview =
        serde_json::to_string_pretty(&draft.redacted_preview()).unwrap_or_else(|_| "{}".to_owned());
    let preview = preview.chars().take(3200).collect::<String>();
    let policy = draft.effective_policy();
    let embed = CreateEmbed::new()
        .title("Review Codex action")
        .description(
            draft
                .description
                .as_deref()
                .unwrap_or("Schema-generated app-server request"),
        )
        .field("Method", format!("`{}`", draft.method), false)
        .field("Risk", policy.authorization.risk_label(), false)
        .field(
            "Authorization",
            policy.authorization.requirement_label(),
            false,
        )
        .field("Result view", draft.renderer.result_title(), true)
        .field("Parameters", format!("```json\n{preview}\n```"), false)
        .color(policy.confirmation.color());
    (
        embed,
        components::action_confirm_buttons(
            token,
            policy.confirmation == actions::ConfirmationPolicy::Danger,
        ),
    )
}

fn string_command_option<'a>(command: &'a CommandInteraction, name: &str) -> Option<&'a str> {
    command.data.options().into_iter().find_map(|option| {
        (option.name == name)
            .then_some(option.value)
            .and_then(|value| {
                if let serenity::all::ResolvedValue::String(value) = value {
                    Some(value)
                } else {
                    None
                }
            })
    })
}

fn integer_command_option(command: &CommandInteraction, name: &str) -> Option<i64> {
    command.data.options().into_iter().find_map(|option| {
        (option.name == name)
            .then_some(option.value)
            .and_then(|value| {
                if let serenity::all::ResolvedValue::Integer(value) = value {
                    Some(value)
                } else {
                    None
                }
            })
    })
}

fn boolean_command_option(command: &CommandInteraction, name: &str) -> Option<bool> {
    command.data.options().into_iter().find_map(|option| {
        (option.name == name)
            .then_some(option.value)
            .and_then(|value| {
                if let serenity::all::ResolvedValue::Boolean(value) = value {
                    Some(value)
                } else {
                    None
                }
            })
    })
}

async fn collect_apps(
    client: &CodexClient,
    thread_id: Option<String>,
    force_refetch: bool,
) -> Result<Vec<Value>> {
    const PAGE_SIZE: u32 = 100;
    const MAX_PAGES: usize = 100;

    let mut apps = Vec::new();
    let mut cursor = None;
    for page in 0..MAX_PAGES {
        let result = client
            .app_list(AppListParams {
                cursor: cursor.clone(),
                limit: Some(PAGE_SIZE),
                thread_id: thread_id.clone(),
                force_refetch: Some(force_refetch && page == 0),
            })
            .await?;
        apps.extend(
            first_result_array(&result, &["data", "apps", "items"])
                .iter()
                .cloned(),
        );
        let next = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if next.is_none() {
            return Ok(apps);
        }
        anyhow::ensure!(next != cursor, "Codex app/list returned a repeated cursor");
        cursor = next;
    }
    anyhow::bail!("Codex app/list exceeded {MAX_PAGES} pages")
}

async fn collect_mcp_servers(
    client: &CodexClient,
    thread_id: Option<String>,
) -> Result<Vec<Value>> {
    const PAGE_SIZE: u32 = 100;
    const MAX_PAGES: usize = 100;

    let mut servers = Vec::new();
    let mut cursor = None;
    for _ in 0..MAX_PAGES {
        let result = client
            .mcp_server_status_list(McpServerStatusListParams {
                cursor: cursor.clone(),
                limit: Some(PAGE_SIZE),
                detail: Some("full".to_owned()),
                thread_id: thread_id.clone(),
            })
            .await?;
        servers.extend(
            first_result_array(&result, &["data", "items"])
                .iter()
                .cloned(),
        );
        let next = result
            .get("nextCursor")
            .or_else(|| result.get("next_cursor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if next.is_none() {
            return Ok(servers);
        }
        anyhow::ensure!(
            next != cursor,
            "Codex mcpServerStatus/list returned a repeated cursor"
        );
        cursor = next;
    }
    anyhow::bail!("Codex mcpServerStatus/list exceeded {MAX_PAGES} pages")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpCatalogRow {
    primary: String,
    secondary: String,
    description: String,
}

fn mcp_catalog_rows(servers: &[Value], view: &str) -> Result<Vec<McpCatalogRow>> {
    anyhow::ensure!(
        matches!(view, "servers" | "tools" | "resources" | "templates"),
        "unknown MCP catalog view `{view}`"
    );
    let mut rows = Vec::new();
    for server in servers {
        let server_name = server
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed server");
        if view == "servers" {
            let auth = match server.get("authStatus").and_then(Value::as_str) {
                Some("notLoggedIn") => "not logged in",
                Some("bearerToken") => "bearer token",
                Some("oAuth") | Some("oauth") => "OAuth",
                Some(status) => status,
                None => "no auth needed",
            };
            rows.push(McpCatalogRow {
                primary: server_name.to_owned(),
                secondary: auth.to_owned(),
                description: format!(
                    "{} tools · {} resources · {} templates",
                    mcp_collection_len(server.get("tools")),
                    mcp_collection_len(server.get("resources")),
                    mcp_collection_len(server.get("resourceTemplates"))
                ),
            });
            continue;
        }

        let field = match view {
            "tools" => "tools",
            "resources" => "resources",
            "templates" => "resourceTemplates",
            _ => anyhow::bail!("unknown MCP catalog view `{view}`"),
        };
        for (fallback_name, item) in mcp_collection_items(server.get(field)) {
            let primary = item
                .get("name")
                .or_else(|| item.get("title"))
                .or_else(|| item.get("uri"))
                .or_else(|| item.get("uriTemplate"))
                .and_then(Value::as_str)
                .unwrap_or(&fallback_name)
                .to_owned();
            let description = item
                .get("description")
                .or_else(|| item.get("uri"))
                .or_else(|| item.get("uriTemplate"))
                .and_then(Value::as_str)
                .unwrap_or("No description supplied")
                .replace('\n', " ");
            rows.push(McpCatalogRow {
                primary,
                secondary: server_name.to_owned(),
                description,
            });
        }
    }
    Ok(rows)
}

fn mcp_collection_len(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(items)) => items.len(),
        _ => 0,
    }
}

fn mcp_collection_items(value: Option<&Value>) -> Vec<(String, &Value)> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (format!("item {}", index + 1), item))
            .collect(),
        Some(Value::Object(items)) => items
            .iter()
            .map(|(name, item)| (name.clone(), item))
            .collect(),
        _ => Vec::new(),
    }
}

fn mcp_view_label(view: &str) -> &'static str {
    match view {
        "servers" => "servers",
        "tools" => "tools",
        "resources" => "resources",
        "templates" => "resource templates",
        _ => "entries",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginCatalogRow {
    id: String,
    name: String,
    marketplace: String,
    marketplace_path: Option<String>,
    remote_marketplace_name: Option<String>,
    description: String,
    version: Option<String>,
    installed: bool,
    enabled: bool,
    availability: Option<String>,
    install_policy: Option<String>,
}

fn plugin_catalog_rows(value: &Value) -> Vec<PluginCatalogRow> {
    let root = value.get("data").unwrap_or(value);
    let marketplaces = root
        .get("marketplaces")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut rows = Vec::new();
    for marketplace in marketplaces {
        let marketplace_name = marketplace
            .get("name")
            .or_else(|| marketplace.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("unknown marketplace");
        let marketplace_path = marketplace.get("path").and_then(Value::as_str);
        if let Some(plugins) = marketplace.get("plugins").and_then(Value::as_array) {
            rows.extend(
                plugins
                    .iter()
                    .map(|plugin| plugin_catalog_row(plugin, marketplace_name, marketplace_path)),
            );
        }
    }
    if rows.is_empty()
        && let Some(plugins) = root.get("plugins").and_then(Value::as_array)
    {
        rows.extend(
            plugins
                .iter()
                .map(|plugin| plugin_catalog_row(plugin, "unknown marketplace", None)),
        );
    }
    rows
}

fn plugin_catalog_row(
    plugin: &Value,
    marketplace: &str,
    marketplace_path: Option<&str>,
) -> PluginCatalogRow {
    let name = plugin
        .get("name")
        .or_else(|| plugin.get("id"))
        .or_else(|| plugin.get("remotePluginId"))
        .and_then(Value::as_str)
        .unwrap_or("unnamed plugin");
    let id = plugin
        .get("id")
        .or_else(|| plugin.get("remotePluginId"))
        .and_then(Value::as_str)
        .unwrap_or(name);
    let description = plugin
        .get("description")
        .or_else(|| plugin.get("shortDescription"))
        .or_else(|| plugin.pointer("/interface/shortDescription"))
        .or_else(|| plugin.pointer("/interface/longDescription"))
        .and_then(Value::as_str)
        .unwrap_or("No description supplied")
        .replace('\n', " ");
    PluginCatalogRow {
        id: id.to_owned(),
        name: name.to_owned(),
        marketplace: marketplace.to_owned(),
        marketplace_path: marketplace_path.map(str::to_owned),
        remote_marketplace_name: marketplace_path.is_none().then(|| marketplace.to_owned()),
        description,
        version: plugin
            .get("localVersion")
            .or_else(|| plugin.get("version"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        installed: plugin
            .get("installed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        enabled: plugin
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        availability: plugin
            .get("availability")
            .and_then(Value::as_str)
            .map(str::to_owned),
        install_policy: plugin
            .get("installPolicy")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn plugin_browser_view(
    draft: &PluginBrowserDraft,
    requested_page: usize,
    token: &str,
) -> (CreateEmbed, Vec<CreateActionRow>) {
    let (start, end, page, pages) = page_window(draft.rows.len(), requested_page, 10);
    let mut lines = String::new();
    for row in &draft.rows[start..end] {
        let status = if !row.enabled {
            "disabled"
        } else if row.installed {
            "installed"
        } else {
            "available"
        };
        let version = row
            .version
            .as_deref()
            .map(|version| format!(" · v{version}"))
            .unwrap_or_default();
        lines.push_str(&format!(
            "**{}** · {} · {}{}\n   {}\n",
            row.name.chars().take(100).collect::<String>(),
            row.marketplace.chars().take(80).collect::<String>(),
            status,
            version,
            row.description.chars().take(140).collect::<String>()
        ));
    }
    if lines.is_empty() {
        lines.push_str("No matching plugins.");
    }
    lines.push_str(&format!(
        "\nPage {page}/{pages} · {} {} plugins",
        draft.rows.len(),
        draft.scope
    ));
    if draft.load_error_count > 0 {
        lines.push_str(&format!(
            "\n⚠ {} marketplace(s) failed to load; diagnostics remain redacted.",
            draft.load_error_count
        ));
    }
    let options = draft.rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            (
                start + offset,
                row.name.clone(),
                format!("{} · {}", row.marketplace, row.description),
            )
        });
    (
        embeds::info_card("Codex plugins", &lines),
        components::plugin_browser_controls(token, options, page, pages),
    )
}

fn plugin_auth_links(value: &Value) -> Vec<(String, String)> {
    value
        .get("appsNeedingAuth")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|app| {
            let name = app
                .get("name")
                .or_else(|| app.get("id"))
                .and_then(Value::as_str)?;
            let url = app.get("installUrl").and_then(Value::as_str)?;
            Some((name.to_owned(), url.to_owned()))
        })
        .collect()
}

fn plugin_load_error_count(value: &Value) -> usize {
    value
        .get("marketplaceLoadErrors")
        .or_else(|| value.pointer("/data/marketplaceLoadErrors"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn plugin_mutation_params(row: &PluginCatalogRow, install: bool) -> Value {
    if !install {
        return json!({"pluginId": row.id});
    }
    let mut params = json!({
        "pluginName": row.name,
        "marketplacePath": row.marketplace_path,
        "remoteMarketplaceName": row.remote_marketplace_name
    });
    if let Some(object) = params.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    params
}

fn plugin_available_mutation(row: &PluginCatalogRow) -> Option<bool> {
    if !row.enabled
        || row.availability.as_deref() == Some("DISABLED_BY_ADMIN")
        || row.install_policy.as_deref() == Some("NOT_AVAILABLE")
    {
        return None;
    }
    if row.installed {
        (row.install_policy.as_deref() != Some("INSTALLED_BY_DEFAULT")).then_some(false)
    } else {
        Some(true)
    }
}

fn first_result_array<'a>(value: &'a Value, keys: &[&str]) -> &'a [Value] {
    for key in keys {
        if let Some(values) = value.get(*key).and_then(Value::as_array) {
            return values;
        }
    }
    &[]
}

/// `skills/list` groups entries by requested cwd in current Codex builds.
/// Normalize the older flat response at the same boundary.
fn collect_skills(value: &Value) -> Vec<Value> {
    let direct = first_result_array(value, &["data", "skills", "items"]);
    let grouped = direct
        .iter()
        .flat_map(|entry| {
            entry
                .get("skills")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .cloned()
        .collect::<Vec<_>>();
    if grouped.is_empty() {
        direct.to_vec()
    } else {
        grouped
    }
}

fn page_window(
    total: usize,
    requested_page: usize,
    page_size: usize,
) -> (usize, usize, usize, usize) {
    let pages = total.max(1).div_ceil(page_size);
    let page = requested_page.clamp(1, pages);
    let start = (page - 1) * page_size;
    let end = (start + page_size).min(total);
    (start, end, page, pages)
}

fn model_reasoning_efforts(value: &Value, model_id: &str) -> Vec<String> {
    first_result_array(value, &["data", "models", "items"])
        .iter()
        .find(|model| {
            model
                .get("id")
                .or_else(|| model.get("model"))
                .and_then(Value::as_str)
                == Some(model_id)
        })
        .and_then(|model| model.get("supportedReasoningEfforts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| {
            option
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .or_else(|| option.as_str())
        })
        .map(str::to_owned)
        .collect()
}

fn modal_value<'a>(modal: &'a ModalInteraction, custom_id: &str) -> Option<&'a str> {
    modal
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|component| {
            if let ActionRowComponent::InputText(input) = component
                && input.custom_id == custom_id
            {
                return input.value.as_deref();
            }
            None
        })
}
fn parse_approval_component(custom_id: &str) -> Option<(&'static str, &str)> {
    [
        (components::APPROVE_ONCE, "accept"),
        (components::APPROVE_SESSION, "acceptForSession"),
        (components::CANCEL_REQUEST, "cancel"),
        (components::DENY, "decline"),
    ]
    .into_iter()
    .find_map(|(prefix, decision)| {
        components::custom_id_arg(custom_id, prefix).map(|request_id| (decision, request_id))
    })
}

async fn send_server_reply(
    client: &CodexClient,
    generation: u64,
    reply: ServerReply,
) -> Result<()> {
    match reply {
        ServerReply::Result { id, result } => {
            client
                .respond_result_for_generation(generation, id, result)
                .await?
        }
        ServerReply::Error { id, error } => {
            client
                .respond_error_for_generation(generation, id, error)
                .await?
        }
    }
    Ok(())
}

fn request_thread_id(request: &ServerRequest) -> Option<String> {
    crate::codex::thread_id_from_params(&request.params).map(str::to_owned)
}

fn has_pending_for_thread(state: &BotState, thread_id: &str) -> bool {
    state.pending_server_requests.iter().any(|entry| {
        request_thread_id(entry.value()).is_some_and(|request_thread| request_thread == thread_id)
    })
}

fn server_answer_form(request: &ServerRequest) -> Result<Vec<serenity::builder::CreateActionRow>> {
    match ServerRequestMethod::classify(&request.method) {
        ServerRequestMethod::ToolUserInput => {
            let questions = request
                .params
                .get("questions")
                .and_then(Value::as_array)
                .context("Codex input request has no questions")?;
            anyhow::ensure!(
                !questions.is_empty(),
                "Codex input request has no questions"
            );
            anyhow::ensure!(
                questions.iter().all(|question| !question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)),
                "secret input is blocked in Discord; answer from local Codex"
            );
            if questions.len() > 5 {
                return Ok(components::server_answer_inputs([(
                    "u:free".to_owned(),
                    "Answers by question id".to_owned(),
                    "JSON object or one question_id=value per line".to_owned(),
                    true,
                )]));
            }
            let mut fields = Vec::with_capacity(questions.len());
            for (index, question) in questions.iter().enumerate() {
                let label = question
                    .get("header")
                    .or_else(|| question.get("question"))
                    .and_then(Value::as_str)
                    .unwrap_or("Answer")
                    .to_owned();
                let options = question
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option.get("label").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                let placeholder = if options.is_empty() {
                    question
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or("Type your answer")
                        .to_owned()
                } else {
                    options.join(" | ")
                };
                fields.push((format!("u:{index}"), label, placeholder, true));
            }
            Ok(components::server_answer_inputs(fields))
        }
        ServerRequestMethod::McpElicitation => {
            anyhow::ensure!(
                request.params.get("mode").and_then(Value::as_str) != Some("url"),
                "URL connector requests use the link and completion buttons"
            );
            let schema = request.params.get("requestedSchema");
            let properties = schema
                .and_then(|schema| schema.get("properties"))
                .and_then(Value::as_object);
            let Some(properties) =
                properties.filter(|properties| (1..=5).contains(&properties.len()))
            else {
                return Ok(components::server_answer_inputs([(
                    "m:free".to_owned(),
                    "Form values".to_owned(),
                    "JSON object or one key=value pair per line".to_owned(),
                    true,
                )]));
            };
            let required = schema
                .and_then(|schema| schema.get("required"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let fields = properties
                .iter()
                .enumerate()
                .map(|(index, (name, field))| {
                    let label = field
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or(name)
                        .to_owned();
                    let choices = field.get("enum").and_then(Value::as_array).map(|values| {
                        values
                            .iter()
                            .map(|value| {
                                value
                                    .as_str()
                                    .map_or_else(|| value.to_string(), str::to_owned)
                            })
                            .collect::<Vec<_>>()
                            .join(" | ")
                    });
                    let placeholder = choices.unwrap_or_else(|| {
                        field
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("Type a value")
                            .to_owned()
                    });
                    (
                        format!("m:{index}"),
                        label,
                        placeholder,
                        required.contains(&name.as_str()),
                    )
                })
                .collect::<Vec<_>>();
            Ok(components::server_answer_inputs(fields))
        }
        _ => anyhow::bail!("this request does not accept typed input"),
    }
}

fn mcp_elicitation_has_empty_form_schema(request: &ServerRequest) -> bool {
    ServerRequestMethod::classify(&request.method) == ServerRequestMethod::McpElicitation
        && request.params.get("mode").and_then(Value::as_str) != Some("url")
        && request
            .params
            .get("requestedSchema")
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
}

fn parse_user_input_modal_answers(
    request: &ServerRequest,
    modal: &ModalInteraction,
) -> Result<BTreeMap<String, Vec<String>>> {
    let questions = request
        .params
        .get("questions")
        .and_then(Value::as_array)
        .context("Codex input request has no questions")?;
    if questions.len() > 5 {
        let raw = modal_value(modal, "u:free").context("question answers missing")?;
        let supplied = actions::parse_free_form_object(raw)?;
        let supplied = supplied
            .as_object()
            .context("question answers must be a JSON object")?;
        let mut answers = BTreeMap::new();
        for (index, question) in questions.iter().enumerate() {
            let id = question
                .get("id")
                .and_then(Value::as_str)
                .with_context(|| format!("question {index} has no id"))?;
            let value = supplied
                .get(id)
                .with_context(|| format!("answer missing for {id}"))?;
            let values = match value {
                Value::Array(values) => values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned)
                    })
                    .collect(),
                Value::String(value) => vec![value.clone()],
                value => vec![value.to_string()],
            };
            answers.insert(id.to_owned(), validate_user_answer(question, id, values)?);
        }
        return Ok(answers);
    }
    let mut answers = BTreeMap::new();
    for (index, question) in questions.iter().enumerate() {
        let id = question
            .get("id")
            .and_then(Value::as_str)
            .with_context(|| format!("question {index} has no id"))?;
        let raw = modal_value(modal, &format!("u:{index}"))
            .with_context(|| format!("answer missing for {id}"))?;
        let values = raw
            .split(['\n', ','])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        answers.insert(id.to_owned(), validate_user_answer(question, id, values)?);
    }
    anyhow::ensure!(!answers.is_empty(), "at least one answer is required");
    Ok(answers)
}

fn validate_user_answer(question: &Value, id: &str, values: Vec<String>) -> Result<Vec<String>> {
    let values = values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    anyhow::ensure!(!values.is_empty(), "answer for {id} is empty");
    let allowed = question
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| option.get("label").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let allows_other = question
        .get("isOther")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !allowed.is_empty() && !allows_other {
        anyhow::ensure!(
            values.iter().all(|value| allowed.contains(&value.as_str())),
            "answer for {id} must be one of {}",
            allowed.join(", ")
        );
    }
    Ok(values)
}

fn parse_mcp_modal_content(request: &ServerRequest, modal: &ModalInteraction) -> Result<Value> {
    let properties = request
        .params
        .get("requestedSchema")
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object);
    let Some(properties) = properties.filter(|properties| (1..=5).contains(&properties.len()))
    else {
        let raw = modal_value(modal, "m:free").context("MCP form values missing")?;
        return actions::parse_free_form_object(raw);
    };
    let mut content = serde_json::Map::new();
    for (index, (name, schema)) in properties.iter().enumerate() {
        let Some(raw) = modal_value(modal, &format!("m:{index}")) else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let value = match schema.get("type").and_then(Value::as_str) {
            Some("boolean") => Value::Bool(match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" | "on" => true,
                "false" | "no" | "0" | "off" => false,
                _ => anyhow::bail!("{name} must be true or false"),
            }),
            Some("integer") => Value::Number(raw.trim().parse::<i64>()?.into()),
            Some("number") => Value::Number(
                serde_json::Number::from_f64(raw.trim().parse::<f64>()?)
                    .context("number is not finite")?,
            ),
            Some("array") => Value::Array(
                raw.split(['\n', ','])
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| Value::String(value.to_owned()))
                    .collect(),
            ),
            Some("object") => actions::parse_free_form_object(raw)?,
            _ => Value::String(raw.to_owned()),
        };
        content.insert(name.clone(), value);
    }
    Ok(Value::Object(content))
}

fn stable_value_hash(value: &Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn stable_bytes_hash(value: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

async fn cache_discord_attachment(attachment: &Attachment, message_id: u64) -> Result<PathBuf> {
    const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
    const CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
    anyhow::ensure!(
        u64::from(attachment.size) <= MAX_ATTACHMENT_BYTES,
        "attachment {} exceeds 25 MiB relay cache limit",
        attachment.filename
    );
    let safe_name = attachment
        .filename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let cache_root = config::data_dir().join("attachments");
    prune_attachment_cache(
        &cache_root,
        CACHE_MAX_BYTES,
        Duration::from_secs(7 * 24 * 60 * 60),
    )
    .await?;
    let directory = cache_root.join(message_id.to_string());
    tokio::fs::create_dir_all(&directory).await?;
    let path = directory.join(if safe_name.is_empty() {
        "attachment.bin"
    } else {
        &safe_name
    });
    if !path.exists() {
        let bytes = attachment.download().await?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= MAX_ATTACHMENT_BYTES,
            "downloaded attachment exceeds 25 MiB relay cache limit"
        );
        tokio::fs::write(&path, bytes).await?;
    }
    Ok(path)
}

async fn prune_attachment_cache(
    root: &std::path::Path,
    max_bytes: u64,
    max_age: Duration,
) -> Result<()> {
    let now = std::time::SystemTime::now();
    let mut entries = Vec::new();
    let mut directories = Vec::new();
    let mut top = match tokio::fs::read_dir(root).await {
        Ok(top) => top,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(directory) = top.next_entry().await? {
        if !directory.file_type().await?.is_dir() {
            continue;
        }
        directories.push(directory.path());
        let mut files = tokio::fs::read_dir(directory.path()).await?;
        while let Some(file) = files.next_entry().await? {
            if !file.file_type().await?.is_file() {
                continue;
            }
            let metadata = file.metadata().await?;
            let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
            if now.duration_since(modified).unwrap_or_default() > max_age {
                let _ = tokio::fs::remove_file(file.path()).await;
            } else {
                entries.push((file.path(), metadata.len(), modified));
            }
        }
    }
    let mut total = entries.iter().map(|(_, size, _)| *size).sum::<u64>();
    if total > max_bytes {
        entries.sort_by_key(|(_, _, modified)| *modified);
        for (path, size, modified) in entries {
            if total <= max_bytes.saturating_mul(3) / 4 {
                break;
            }
            // Never race a turn that has just received this path.
            if now.duration_since(modified).unwrap_or_default() < Duration::from_secs(60 * 60) {
                continue;
            }
            if tokio::fs::remove_file(path).await.is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
    for directory in directories {
        let _ = tokio::fs::remove_dir(directory).await;
    }
    Ok(())
}
async fn defer_command(http: &Http, command: &CommandInteraction) -> Result<()> {
    command
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}
async fn defer_component(http: &Http, component: &ComponentInteraction) -> Result<()> {
    component
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}
/// Acknowledge a component interaction immediately so the original message can
/// be updated in place after slow work. Discord voids interactions that are
/// not acknowledged within three seconds, so any handler that performs a Codex
/// round-trip before its `UpdateMessage` must call this first and then use
/// `edit_response`.
async fn defer_update_component(http: &Http, component: &ComponentInteraction) -> Result<()> {
    component
        .create_response(http, CreateInteractionResponse::Acknowledge)
        .await?;
    Ok(())
}
/// Make list truncation explicit: a capped view must never read as complete.
fn push_more_indicator(lines: &mut String, total: usize, shown: usize) {
    if total > shown {
        if !lines.is_empty() && !lines.ends_with('\n') {
            lines.push('\n');
        }
        lines.push_str(&format!("…and {} more not shown.", total - shown));
    }
}

async fn defer_modal(http: &Http, modal: &ModalInteraction) -> Result<()> {
    modal
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}
async fn edit_command_card(
    http: &Http,
    command: &CommandInteraction,
    title: &str,
    text: &str,
) -> Result<()> {
    command
        .edit_response(
            http,
            EditInteractionResponse::new().embed(embeds::info_card(title, text)),
        )
        .await?;
    Ok(())
}
async fn edit_component_card(
    http: &Http,
    component: &ComponentInteraction,
    title: &str,
    text: &str,
) -> Result<()> {
    component
        .edit_response(
            http,
            EditInteractionResponse::new().embed(embeds::info_card(title, text)),
        )
        .await?;
    Ok(())
}
async fn ephemeral_component(
    http: &Http,
    component: &ComponentInteraction,
    embed: CreateEmbed,
) -> Result<()> {
    component
        .create_response(
            http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(embed)
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}
async fn send_command_error(http: &Http, interaction: &CommandInteraction, error: &anyhow::Error) {
    let detail = error.to_string();
    let message = CreateInteractionResponseMessage::new()
        .embed(embeds::error_card("Command failed", &detail))
        .ephemeral(true);
    if interaction
        .create_response(http, CreateInteractionResponse::Message(message))
        .await
        .is_err()
    {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::error_card("Command failed", &detail)),
            )
            .await;
    }
}

async fn send_component_error(
    http: &Http,
    interaction: &ComponentInteraction,
    error: &anyhow::Error,
) {
    let detail = error.to_string();
    let message = CreateInteractionResponseMessage::new()
        .embed(embeds::error_card("Action failed", &detail))
        .ephemeral(true);
    if interaction
        .create_response(http, CreateInteractionResponse::Message(message))
        .await
        .is_err()
    {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().embed(embeds::error_card("Action failed", &detail)),
            )
            .await;
    }
}

async fn send_modal_error(http: &Http, interaction: &ModalInteraction, error: &anyhow::Error) {
    let detail = error.to_string();
    let message = CreateInteractionResponseMessage::new()
        .embed(embeds::error_card("Could not continue", &detail))
        .ephemeral(true);
    if interaction
        .create_response(http, CreateInteractionResponse::Message(message))
        .await
        .is_err()
    {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .embed(embeds::error_card("Could not continue", &detail)),
            )
            .await;
    }
}

#[cfg(test)]
mod control_channel_tests {
    use serenity::all::ChannelId;

    use super::{control_channel_guidance, provision::Layout};

    fn layout() -> Layout {
        Layout {
            control_category: ChannelId::new(1),
            running_category: ChannelId::new(2),
            needs_user_category: ChannelId::new(3),
            done_category: ChannelId::new(4),
            failed_category: ChannelId::new(5),
            new_task_channel: ChannelId::new(6),
            existing_tasks_channel: ChannelId::new(7),
            runner_status_channel: ChannelId::new(8),
            audit_log_channel: ChannelId::new(9),
        }
    }

    #[test]
    fn every_control_channel_returns_explicit_guidance() {
        let layout = layout();
        for channel in [
            layout.new_task_channel,
            layout.existing_tasks_channel,
            layout.runner_status_channel,
            layout.audit_log_channel,
        ] {
            assert!(control_channel_guidance(&layout, channel).is_some());
        }
        assert!(control_channel_guidance(&layout, ChannelId::new(10)).is_none());
    }
}

#[cfg(test)]
mod god_safety_tests {
    use serde_json::json;
    use std::time::Duration;

    use super::{
        CodexExecutionPolicy, GodLifecycle, bind_advanced_task_params,
        is_absent_discord_message_response, is_sensitive_god_message,
        normal_thread_settings_params,
    };

    #[test]
    fn god_password_message_detection_is_case_insensitive_and_whitespace_tolerant() {
        assert!(is_sensitive_god_message("!god hunter2"));
        assert!(is_sensitive_god_message("  \t!GoD hunter2"));
        assert!(!is_sensitive_god_message("please use /god"));
    }

    #[test]
    fn unknown_or_missing_discord_message_is_a_successful_deletion() {
        assert!(is_absent_discord_message_response(404, 0));
        assert!(is_absent_discord_message_response(400, 10_008));
        assert!(!is_absent_discord_message_response(403, 50_013));
    }

    #[test]
    fn permission_reset_serializes_exact_codex_app_server_shape() {
        assert_eq!(
            normal_thread_settings_params("thread-123"),
            json!({
                "threadId": "thread-123",
                "approvalPolicy": "on-request",
                "sandboxPolicy": {"type": "workspaceWrite"}
            })
        );
    }

    #[test]
    fn turn_policy_has_one_normal_and_god_serializer() {
        let normal = CodexExecutionPolicy::for_god(false);
        assert_eq!(normal.approval_policy, "on-request");
        assert_eq!(normal.thread_sandbox(), json!("workspace-write"));
        assert_eq!(normal.turn_sandbox(), json!({"type": "workspaceWrite"}));

        let god = CodexExecutionPolicy::for_god(true);
        assert_eq!(god.approval_policy, "never");
        assert_eq!(god.thread_sandbox(), json!("danger-full-access"));
        assert_eq!(god.turn_sandbox(), json!({"type": "dangerFullAccess"}));
    }

    #[test]
    fn turn_start_params_carry_the_selected_policy() {
        let params = CodexExecutionPolicy::for_god(true).turn_start_params(
            "thread-9".into(),
            vec![crate::codex::UserInput::text("go")],
            None,
            None,
        );
        let serialized = serde_json::to_value(&params).unwrap();
        assert_eq!(serialized["approvalPolicy"], "never");
        assert_eq!(
            serialized["sandboxPolicy"],
            json!({"type":"dangerFullAccess"})
        );
        assert_eq!(serialized["threadId"], "thread-9");
    }

    #[test]
    fn advanced_rpc_binds_missing_task_id_to_the_channel_task() {
        let schema = json!({
            "type": "object",
            "required": ["threadId"],
            "properties": {"threadId": {"type": "string"}}
        });
        let mut params = json!({});
        bind_advanced_task_params(&mut params, Some(&schema), "thread-9").unwrap();
        assert_eq!(params, json!({"threadId": "thread-9"}));
    }

    #[test]
    fn advanced_rpc_rejects_cross_task_ids_at_any_depth() {
        for (mut params, schema) in [
            (
                json!({"threadId": "thread-other"}),
                json!({"type":"object","properties":{"threadId":{"type":"string"}}}),
            ),
            (
                json!({"nested": {"conversation_id": "thread-other"}}),
                json!({
                    "type":"object",
                    "properties":{"nested":{"type":"object","properties":{
                        "conversation_id":{"type":"string"}
                    }}}
                }),
            ),
            (
                json!({"items": [{"taskId": "thread-other"}]}),
                json!({
                    "type":"object",
                    "properties":{"items":{"type":"array","items":{
                        "type":"object","properties":{"taskId":{"type":"string"}}
                    }}}
                }),
            ),
        ] {
            let error =
                bind_advanced_task_params(&mut params, Some(&schema), "thread-9").unwrap_err();
            assert!(
                error.to_string().contains("does not match this channel"),
                "{error}"
            );
        }

        let mut matching = json!({
            "threadId": "thread-9",
            "nested": {"conversation_id": "thread-9"}
        });
        let schema = json!({
            "type":"object",
            "properties":{
                "threadId":{"type":"string"},
                "nested":{"type":"object","properties":{
                    "conversation_id":{"type":"string"}
                }}
            }
        });
        bind_advanced_task_params(&mut matching, Some(&schema), "thread-9").unwrap();

        let mut tool_arguments = json!({
            "threadId": "thread-9",
            "arguments": {"taskId": "external-domain-task"}
        });
        let tool_schema = json!({
            "type":"object",
            "properties":{
                "threadId":{"type":"string"},
                "arguments": true
            }
        });
        bind_advanced_task_params(&mut tool_arguments, Some(&tool_schema), "thread-9").unwrap();
    }

    #[tokio::test]
    async fn cleanup_lease_never_holds_the_global_dispatch_barrier() {
        let lifecycle = GodLifecycle::default();
        let lease = lifecycle
            .cleanup_lease("thread-9")
            .expect("first cleanup owns the task lease");
        assert!(
            lifecycle.cleanup_lease("thread-9").is_none(),
            "the same task cannot be cleaned concurrently"
        );

        let transition = tokio::time::timeout(Duration::from_millis(50), lifecycle.transition())
            .await
            .expect("slow cleanup must not retain the dispatch barrier");
        drop(transition);

        drop(lease);
        assert!(
            lifecycle.cleanup_lease("thread-9").is_some(),
            "dropping the lease must make a failed cleanup retryable"
        );
    }
}

#[cfg(test)]
mod capability_list_tests {
    use serde_json::json;

    use super::{
        collect_skills, mcp_catalog_rows, model_reasoning_efforts, page_window, plugin_auth_links,
        plugin_available_mutation, plugin_catalog_rows, plugin_mutation_params,
    };

    #[test]
    fn current_grouped_skills_response_is_flattened() {
        let result = json!({
            "data": [
                {"cwd":"C:/one","skills":[{"name":"alpha"},{"name":"beta"}]},
                {"cwd":"C:/two","skills":[{"name":"gamma"}]}
            ]
        });
        let names = collect_skills(&result)
            .into_iter()
            .filter_map(|skill| skill["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn flat_legacy_skills_response_still_works() {
        let result = json!({"skills":[{"name":"alpha"},{"name":"beta"}]});
        assert_eq!(collect_skills(&result).len(), 2);
    }

    #[test]
    fn pagination_clamps_without_slicing_past_the_end() {
        assert_eq!(page_window(434, 99, 25), (425, 434, 18, 18));
        assert_eq!(page_window(0, 1, 25), (0, 0, 1, 1));
    }

    #[test]
    fn reasoning_efforts_come_from_the_selected_live_model() {
        let result = json!({"data":[
            {"id":"gpt-a","supportedReasoningEfforts":[
                {"reasoningEffort":"low"},{"reasoningEffort":"medium"}
            ]},
            {"id":"gpt-b","supportedReasoningEfforts":[
                {"reasoningEffort":"high"},{"reasoningEffort":"ultra"}
            ]}
        ]});
        assert_eq!(model_reasoning_efforts(&result, "gpt-b"), ["high", "ultra"]);
    }

    #[test]
    fn mcp_catalog_flattens_object_tools_and_array_resources() {
        let servers = json!([{
            "name":"codex_apps",
            "authStatus":"bearerToken",
            "tools":{
                "gmail_search":{"name":"gmail_search","description":"Search Gmail"},
                "gmail_send":{"description":"Send email"}
            },
            "resources":[
                {"name":"inbox","uri":"gmail://inbox","description":"Inbox messages"}
            ],
            "resourceTemplates":[
                {"name":"thread","uriTemplate":"gmail://thread/{id}"}
            ]
        }]);
        let servers = servers.as_array().unwrap();

        let tools = mcp_catalog_rows(servers, "tools").unwrap();
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|row| {
            row.primary == "gmail_send"
                && row.secondary == "codex_apps"
                && row.description == "Send email"
        }));

        let resources = mcp_catalog_rows(servers, "resources").unwrap();
        assert_eq!(resources[0].primary, "inbox");
        let templates = mcp_catalog_rows(servers, "templates").unwrap();
        assert_eq!(templates[0].description, "gmail://thread/{id}");

        let status = mcp_catalog_rows(servers, "servers").unwrap();
        assert_eq!(status[0].description, "2 tools · 1 resources · 1 templates");
    }

    #[test]
    fn plugin_catalog_flattens_marketplaces_and_preserves_status() {
        let result = json!({
            "marketplaces":[{
                "name":"official",
                "path":"C:/plugins",
                "plugins":[{
                    "id":"gmail",
                    "name":"Gmail",
                    "installed":true,
                    "enabled":false,
                    "availability":"AVAILABLE",
                    "installPolicy":"AVAILABLE",
                    "localVersion":"1.2.3",
                    "interface":{"shortDescription":"Read and send email"}
                }]
            }]
        });
        let rows = plugin_catalog_rows(&result);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "gmail");
        assert_eq!(rows[0].name, "Gmail");
        assert_eq!(rows[0].marketplace, "official");
        assert_eq!(rows[0].marketplace_path.as_deref(), Some("C:/plugins"));
        assert!(rows[0].remote_marketplace_name.is_none());
        assert_eq!(rows[0].description, "Read and send email");
        assert_eq!(rows[0].version.as_deref(), Some("1.2.3"));
        assert!(rows[0].installed);
        assert!(!rows[0].enabled);
        assert_eq!(plugin_available_mutation(&rows[0]), None);
        let mut available = rows[0].clone();
        available.enabled = true;
        available.installed = false;
        assert_eq!(plugin_available_mutation(&available), Some(true));
        available.installed = true;
        available.install_policy = Some("INSTALLED_BY_DEFAULT".to_owned());
        assert_eq!(plugin_available_mutation(&available), None);
        assert_eq!(
            plugin_mutation_params(&rows[0], true),
            json!({"pluginName":"Gmail","marketplacePath":"C:/plugins"})
        );
        assert_eq!(
            plugin_mutation_params(&rows[0], false),
            json!({"pluginId":"gmail"})
        );
    }

    #[test]
    fn plugin_install_auth_links_use_only_named_install_urls() {
        let result = json!({
            "appsNeedingAuth":[
                {"id":"gmail","name":"Gmail","installUrl":"https://example.test/auth"},
                {"id":"broken","name":"Broken"}
            ]
        });
        assert_eq!(
            plugin_auth_links(&result),
            [("Gmail".to_owned(), "https://example.test/auth".to_owned())]
        );
    }
}

#[cfg(test)]
mod server_request_setup_tests {
    use serde_json::json;

    use super::{
        ServerReply, ServerRequest, ServerRequestSetupFailure, expired_server_reply,
        mcp_elicitation_has_empty_form_schema, resolved_request_key, server_answer_form,
        server_request_setup_failure_reply,
    };

    fn request(id: serde_json::Value) -> ServerRequest {
        ServerRequest {
            id,
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({"threadId":"thread-1"}),
            generation: 1,
        }
    }

    #[test]
    fn every_setup_failure_reply_preserves_the_codex_request_id() {
        for failure in [
            ServerRequestSetupFailure::TaskLookup,
            ServerRequestSetupFailure::TaskMissing,
            ServerRequestSetupFailure::MissingChannel,
            ServerRequestSetupFailure::PendingSave,
            ServerRequestSetupFailure::DiscordDelivery,
        ] {
            for original_id in [json!(73), json!("approval-73")] {
                let ServerReply::Error { id, error } =
                    server_request_setup_failure_reply(&request(original_id.clone()), failure)
                else {
                    panic!("setup failures must be JSON-RPC errors");
                };
                assert_eq!(id, original_id);
                assert_eq!(error.code, -32_000);
                assert_eq!(error.message, failure.message());
                assert!(error.data.is_none());
            }
        }
    }

    #[test]
    fn required_pre_card_failures_have_specific_non_secret_messages() {
        assert_eq!(
            ServerRequestSetupFailure::TaskLookup.message(),
            "could not look up the Codex task in Discord relay state"
        );
        assert_eq!(
            ServerRequestSetupFailure::MissingChannel.message(),
            "Codex task has no Discord channel"
        );
        assert_eq!(
            ServerRequestSetupFailure::PendingSave.message(),
            "could not persist the pending Codex request"
        );
    }

    #[test]
    fn live_expiry_uses_each_protocols_terminal_response() {
        for (method, expected) in [
            (
                "item/commandExecution/requestApproval",
                json!({"decision":"cancel"}),
            ),
            ("execCommandApproval", json!({"decision":"timed_out"})),
            ("mcpServer/elicitation/request", json!({"action":"cancel"})),
            (
                "item/permissions/requestApproval",
                json!({"permissions":{},"scope":"turn"}),
            ),
        ] {
            let request = ServerRequest {
                id: json!(77),
                method: method.to_owned(),
                params: json!({"threadId":"thread-1"}),
                generation: 1,
            };
            let ServerReply::Result { id, result } = expired_server_reply(&request) else {
                panic!("{method} must expire with a result")
            };
            assert_eq!(id, json!(77));
            assert_eq!(result, expected);
        }

        let input = ServerRequest {
            id: json!(78),
            method: "item/tool/requestUserInput".to_owned(),
            params: json!({"threadId":"thread-1"}),
            generation: 1,
        };
        let ServerReply::Error { id, error } = expired_server_reply(&input) else {
            panic!("input expiry must be correlated error")
        };
        assert_eq!(id, json!(78));
        assert_eq!(error.code, -32_000);
    }

    #[test]
    fn resolved_request_tombstones_are_generation_and_id_scoped() {
        assert_ne!(
            resolved_request_key(1, &json!(77)),
            resolved_request_key(2, &json!(77))
        );
        assert_ne!(
            resolved_request_key(1, &json!(77)),
            resolved_request_key(1, &json!("77"))
        );
    }

    #[test]
    fn six_field_mcp_schema_uses_one_lossless_free_form_input() {
        let request = ServerRequest {
            id: json!("mcp-six"),
            method: "mcpServer/elicitation/request".to_owned(),
            params: json!({
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "one": {"type": "string"},
                        "two": {"type": "string"},
                        "three": {"type": "string"},
                        "four": {"type": "string"},
                        "five": {"type": "string"},
                        "six": {"type": "string"}
                    }
                }
            }),
            generation: 1,
        };

        let rows = server_answer_form(&request)
            .expect("large MCP schemas must use one free-form Discord input");
        let payload = serde_json::to_value(rows).unwrap();

        assert_eq!(payload.as_array().unwrap().len(), 1);
        assert_eq!(payload[0]["components"][0]["custom_id"], "m:free");
    }

    #[test]
    fn five_field_mcp_schema_keeps_typed_discord_inputs() {
        let request = ServerRequest {
            id: json!("mcp-five"),
            method: "mcpServer/elicitation/request".to_owned(),
            params: json!({
                "mode": "form",
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "one": {"type": "string"},
                        "two": {"type": "string"},
                        "three": {"type": "string"},
                        "four": {"type": "string"},
                        "five": {"type": "string"}
                    }
                }
            }),
            generation: 1,
        };

        let payload = serde_json::to_value(server_answer_form(&request).unwrap()).unwrap();
        assert_eq!(payload.as_array().unwrap().len(), 5);
        assert_eq!(payload[4]["components"][0]["custom_id"], "m:4");
    }

    #[test]
    fn empty_mcp_form_schema_is_direct_confirmation() {
        let request = ServerRequest {
            id: json!("gmail-install"),
            method: "mcpServer/elicitation/request".to_owned(),
            params: json!({
                "mode": "form",
                "requestedSchema": {"type": "object", "properties": {}}
            }),
            generation: 1,
        };

        assert!(mcp_elicitation_has_empty_form_schema(&request));
    }
}

#[cfg(test)]
mod stream_tests {
    use super::StreamBuffer;

    #[test]
    fn separate_agent_messages_never_merge_into_one_blob() {
        let mut stream = StreamBuffer::default();
        stream.append_delta("item-1", "Interim commentary.");
        stream.append_delta("item-2", "Final ");
        stream.append_delta("item-2", "answer.");
        assert_eq!(stream.text(), "Interim commentary.\n\nFinal answer.");
    }

    #[test]
    fn authoritative_completion_heals_dropped_deltas() {
        let mut stream = StreamBuffer::default();
        // Only a fragment of the message arrived as deltas.
        stream.append_delta("item-1", "Hello wo");
        stream.complete_item("item-1", "Hello world, complete answer.");
        assert_eq!(stream.text(), "Hello world, complete answer.");
        assert!(stream.dirty);
    }

    #[test]
    fn completion_with_no_prior_deltas_still_produces_the_answer() {
        let mut stream = StreamBuffer::default();
        stream.complete_item("item-1", "Answer that lost every delta.");
        assert_eq!(stream.text(), "Answer that lost every delta.");
    }

    #[test]
    fn late_or_replayed_deltas_cannot_corrupt_a_completed_item() {
        let mut stream = StreamBuffer::default();
        stream.append_delta("item-1", "partial");
        stream.complete_item("item-1", "final text");
        stream.append_delta("item-1", " stale replay");
        assert_eq!(stream.text(), "final text");
    }

    #[test]
    fn anonymous_legacy_deltas_are_adopted_by_the_first_completion() {
        let mut stream = StreamBuffer::default();
        // Older Codex builds omit itemId on deltas.
        stream.append_delta("", "chunk one ");
        stream.append_delta("", "chunk two");
        stream.complete_item("item-1", "chunk one chunk two");
        assert_eq!(stream.text(), "chunk one chunk two");
        // A second message still renders separately.
        stream.complete_item("item-2", "second message");
        assert_eq!(stream.text(), "chunk one chunk two\n\nsecond message");
    }

    #[test]
    fn identical_completion_does_not_mark_the_stream_dirty_again() {
        let mut stream = StreamBuffer::default();
        stream.append_delta("item-1", "same text");
        stream.dirty = false;
        stream.complete_item("item-1", "same text");
        assert!(!stream.dirty, "no visible change to flush");
        assert_eq!(stream.text(), "same text");
    }
}

#[cfg(test)]
mod realtime_media_tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::{
        BASE64_STANDARD, HOOK_EVENTS, RealtimeDigest, decode_inline_image, media_outbox_path,
        read_workspace_image, safe_hook_value, safe_media_name, validate_image_bytes,
    };

    fn minimal_png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(24, 0);
        bytes
    }

    #[test]
    fn realtime_transcript_done_replaces_partial_delta() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.append_transcript("assistant", "partial");
        digest.finish_transcript("assistant".to_owned(), "authoritative final".to_owned());
        let text = digest.transcript_text();
        assert!(text.contains("authoritative final"));
        assert!(!text.contains("partial"));
    }

    #[test]
    fn pcm16_audio_is_validated_and_wrapped_as_wav() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.append_audio(&json!({
            "audio": {
                "data": BASE64_STANDARD.encode([1_u8, 0, 2, 0]),
                "sampleRate": 24_000,
                "numChannels": 1,
                "samplesPerChannel": 2
            }
        }));
        let (wav, truncated) = digest.take_wav().expect("valid PCM must produce WAV");
        assert!(!truncated);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &[1, 0, 2, 0]);
    }

    #[test]
    fn inconsistent_audio_sample_count_is_rejected() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.append_audio(&json!({
            "audio": {
                "data": BASE64_STANDARD.encode([1_u8, 0, 2, 0]),
                "sampleRate": 24_000,
                "numChannels": 1,
                "samplesPerChannel": 9
            }
        }));
        assert!(digest.audio.is_empty());
        assert!(digest.audio_truncated);
        assert!(digest.take_wav().is_none());
    }

    #[test]
    fn rejected_audio_chunk_latches_to_a_clean_prefix() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.append_audio(&json!({"audio": {
            "data": BASE64_STANDARD.encode([1_u8, 0]),
            "sampleRate": 24_000,
            "numChannels": 1,
            "samplesPerChannel": 1
        }}));
        digest.append_audio(&json!({"audio": {
            "data": BASE64_STANDARD.encode([9_u8, 0]),
            "sampleRate": 24_000,
            "numChannels": 1,
            "samplesPerChannel": 99
        }}));
        digest.append_audio(&json!({"audio": {
            "data": BASE64_STANDARD.encode([2_u8, 0]),
            "sampleRate": 24_000,
            "numChannels": 1,
            "samplesPerChannel": 1
        }}));
        let (wav, truncated) = digest.take_wav().unwrap();
        assert!(truncated);
        assert_eq!(&wav[44..], &[1, 0]);
    }

    #[test]
    fn interleaved_transcript_done_heals_without_destroying_other_role() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.append_transcript("user", "hello");
        digest.append_transcript("assistant", "wor");
        digest.finish_transcript("user".to_owned(), "hello".to_owned());
        digest.append_transcript("assistant", "ld");
        digest.finish_transcript("assistant".to_owned(), "world".to_owned());
        let text = digest.transcript_text();
        assert_eq!(text.matches("hello").count(), 1);
        assert_eq!(text.matches("world").count(), 1);
        assert!(!text.contains("wor\n"));
    }

    #[test]
    fn close_without_reason_preserves_a_prior_realtime_error() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.failed = true;
        digest.close_reason = Some("provider disconnected".to_owned());
        digest.close(None);
        assert!(digest.failed);
        assert_eq!(
            digest.close_reason.as_deref(),
            Some("provider disconnected")
        );
    }

    #[test]
    fn new_realtime_session_clears_stale_transcript_and_audio() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v2"));
        digest.append_transcript("assistant", "old transcript");
        digest.append_audio(&json!({
            "audio": {
                "data": BASE64_STANDARD.encode([1_u8, 0]),
                "sampleRate": 24_000,
                "numChannels": 1,
                "samplesPerChannel": 1
            }
        }));
        digest.start(Some("v3"));
        assert!(digest.transcript_text().is_empty());
        assert!(digest.audio.is_empty());
        assert_eq!(digest.version.as_deref(), Some("v3"));
    }

    #[test]
    fn realtime_signals_keep_only_allowlisted_metadata() {
        let mut digest = RealtimeDigest::default();
        digest.start(Some("v3"));
        digest.note_item(&json!({
            "type": "message",
            "secret": "must never be retained"
        }));
        digest.note_item(&json!({"type": "invented-private-kind"}));
        digest.note_sdp();

        assert_eq!(digest.event_count, 2);
        assert_eq!(digest.last_item_kind.as_deref(), Some("event"));
        assert!(digest.webrtc_ready);
        assert!(!digest.transcript_text().contains("must never be retained"));
        assert_ne!(
            digest.last_item_kind.as_deref(),
            Some("must never be retained")
        );
        assert_eq!(
            safe_hook_value(Some("C:/private/hook.ps1"), HOOK_EVENTS),
            "unknown"
        );
    }

    #[test]
    fn inline_images_require_base64_and_real_image_magic() {
        let png = minimal_png();
        let raw = BASE64_STANDARD.encode(&png);
        assert_eq!(decode_inline_image(&raw).unwrap().1.extension, "png");
        assert_eq!(
            decode_inline_image(&format!("data:image/png;base64,{raw}"))
                .unwrap()
                .1
                .extension,
            "png"
        );
        assert!(decode_inline_image("https://example.test/image.png").is_none());
        assert!(decode_inline_image("C:/private/image.png").is_none());
        assert!(validate_image_bytes(b"not an image".to_vec()).is_none());
    }

    #[test]
    fn media_outbox_names_cannot_escape_the_spool_directory() {
        for rejected in [
            "",
            ".hidden",
            "../secret",
            "folder/file.png",
            "folder\\file.png",
            "C:secret.png",
        ] {
            assert!(
                !safe_media_name(rejected),
                "accepted unsafe name: {rejected}"
            );
            assert!(media_outbox_path(&json!({"file": rejected})).is_none());
        }

        let path = media_outbox_path(&json!({"file": "relay-image_42.png"}))
            .expect("safe basename must map into the media spool");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("relay-image_42.png")
        );
    }

    #[tokio::test]
    async fn saved_images_cannot_escape_the_task_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside_path = workspace.path().join("generated.png");
        let outside_path = outside.path().join("private.png");
        tokio::fs::write(&inside_path, minimal_png()).await.unwrap();
        tokio::fs::write(&outside_path, minimal_png())
            .await
            .unwrap();

        assert!(
            read_workspace_image(workspace.path(), &inside_path)
                .await
                .is_some()
        );
        assert!(
            read_workspace_image(workspace.path(), &outside_path)
                .await
                .is_none()
        );
    }
}
