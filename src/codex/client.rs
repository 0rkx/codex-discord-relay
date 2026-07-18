#![allow(clippy::missing_errors_doc)]

use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[cfg(windows)]
use std::ffi::OsStr;

#[cfg(windows)]
use std::{fs::File, io::Read as _};

#[cfg(windows)]
use sha2::{Digest as _, Sha256};

#[cfg(windows)]
use sysinfo::System;

#[cfg(windows)]
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, broadcast, oneshot},
    task::JoinHandle,
    time::timeout,
};

use super::protocol::{
    AccountReadParams, AppListParams, BackgroundTerminalTerminateParams,
    BackgroundTerminalsListParams, ConfigReadParams, FuzzyFileSearchParams,
    McpServerStatusListParams, ModelListParams, Notification, PluginInstalledParams,
    PluginListParams, PluginLocatorParams, ReviewStartParams, RpcErrorObject, ServerRequest,
    SkillsListParams, ThreadForkParams, ThreadGoalSetParams, ThreadIdParams, ThreadListParams,
    ThreadNameSetParams, ThreadReadParams, ThreadResumeParams, ThreadRollbackParams,
    ThreadSearchParams, ThreadSettingsUpdateParams, ThreadStartParams, ThreadTurnsListParams,
    TurnInterruptParams, TurnStartParams, TurnSteerParams, WireMessage, classify_message,
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_APP_SERVER_JSONL_FRAME_BYTES: usize = 16 * 1_024 * 1_024;

/// Capabilities this relay intentionally advertises to Codex app-server.
///
/// Keep this list narrow. Server-initiated dynamic tools, `ChatGPT` token
/// refresh, and device attestation require host implementations that this
/// private Discord relay does not expose, so they remain negotiated off.
#[must_use]
pub fn advertised_client_capabilities() -> Value {
    json!({
        "experimentalApi": true,
        "mcpServerOpenaiFormElicitation": true
    })
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(
        "Codex Desktop executable was not found; install Codex Desktop or set CODEX_RELAY_CODEX_EXECUTABLE"
    )]
    CodexNotFound,
    #[error("failed to start Codex app-server: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("Codex app-server connection is closed: {0}")]
    ConnectionLost(String),
    #[error("Codex app-server protocol error: {0}")]
    Protocol(String),
    #[error("Codex app-server request {method} timed out after {seconds}s")]
    Timeout { method: String, seconds: u64 },
    #[error("Codex app-server returned {code} for {method}: {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("failed to encode Codex request: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("failed to write to Codex app-server: {0}")]
    Write(#[source] std::io::Error),
    #[error("Codex app-server write timed out after {millis}ms")]
    WriteTimeout { millis: u128 },
    #[error("failed to materialize the Codex Desktop companion: {0}")]
    DesktopMaterialize(String),
}

#[derive(Debug, Clone)]
pub struct CodexCommand {
    program: PathBuf,
    prefix_args: Vec<OsString>,
}

impl CodexCommand {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_prefix_args(
        program: impl Into<PathBuf>,
        prefix_args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        Self {
            program: program.into(),
            prefix_args: prefix_args.into_iter().collect(),
        }
    }

    pub fn discover() -> Result<Self, ClientError> {
        if let Some(explicit) = env::var_os("CODEX_RELAY_CODEX_EXECUTABLE") {
            let explicit = PathBuf::from(explicit);
            return explicit
                .is_file()
                .then(|| Self::new(explicit))
                .ok_or(ClientError::CodexNotFound);
        }

        #[cfg(windows)]
        {
            discover_windows_desktop()
        }

        #[cfg(not(windows))]
        {
            let path = env::var_os("PATH").unwrap_or_default();
            for directory in env::split_paths(&path) {
                let name = "codex";
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Ok(Self::new(candidate));
                }
            }
            Err(ClientError::CodexNotFound)
        }
    }

    pub(crate) fn process(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix_args);
        command
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

#[cfg(windows)]
fn discover_windows_desktop() -> Result<CodexCommand, ClientError> {
    discover_windows_desktop_from(
        running_desktop_companion(),
        installed_desktop_companion(),
        relay_runtime_directory().as_deref(),
        codex_desktop_install_candidates(),
    )
}

#[cfg(windows)]
fn discover_windows_desktop_from(
    running: Option<PathBuf>,
    installed: Option<PathBuf>,
    runtime: Option<&Path>,
    legacy_candidates: Vec<PathBuf>,
) -> Result<CodexCommand, ClientError> {
    let cached = runtime.map(|path| path.join("codex.exe"));
    let mut materialize_error = None;

    if let Some(runtime) = runtime {
        for source in [running, installed].into_iter().flatten() {
            if !is_windowsapps_package_executable(&source) {
                continue;
            }
            match materialize_desktop_companion_at(&source, runtime) {
                Ok(executable) => return Ok(CodexCommand::new(executable)),
                Err(error) => {
                    tracing::warn!(source = %source.display(), %error, "failed to refresh Codex Desktop runtime");
                    materialize_error = Some(error);
                }
            }
        }
    }

    if let Some(cached) = cached.filter(|candidate| candidate.is_file()) {
        return Ok(CodexCommand::new(cached));
    }

    if let Some(legacy) = legacy_candidates.into_iter().find(|candidate| {
        candidate.is_file()
            && is_legacy_desktop_companion(candidate)
            && !is_windowsapps_package_executable(candidate)
    }) {
        return Ok(CodexCommand::new(legacy));
    }

    match materialize_error {
        Some(error) => Err(error),
        None => Err(ClientError::CodexNotFound),
    }
}

#[cfg(windows)]
fn is_windowsapps_package_executable(candidate: &Path) -> bool {
    let normalized = candidate
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    normalized.contains("\\windowsapps\\openai.codex_")
        && normalized.ends_with("\\app\\resources\\codex.exe")
}

#[cfg(windows)]
fn is_legacy_desktop_companion(candidate: &Path) -> bool {
    let normalized = candidate
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    normalized.ends_with("\\openai\\codex\\bin\\codex.exe")
        && (normalized.contains("\\appdata\\local\\openai\\codex\\")
            || normalized.contains("\\appdata\\local\\programs\\openai\\codex\\"))
}

#[cfg(windows)]
fn running_desktop_companion() -> Option<PathBuf> {
    let system = System::new_all();
    newest_package_companion(
        system
            .processes()
            .values()
            .filter_map(|process| process.exe())
            .filter_map(package_companion_from_process),
    )
}

#[cfg(windows)]
fn newest_package_companion(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().max_by(|left, right| {
        match (package_version(left), package_version(right)) {
            (Some(left_version), Some(right_version)) => left_version
                .cmp(&right_version)
                .then_with(|| left.cmp(right)),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => left.cmp(right),
        }
    })
}

#[cfg(windows)]
fn package_version(path: &Path) -> Option<Vec<u64>> {
    let package = path.ancestors().find_map(|ancestor| {
        let name = ancestor.file_name()?.to_str()?;
        name.to_ascii_lowercase()
            .starts_with("openai.codex_")
            .then_some(name)
    })?;
    let prefix = "openai.codex_";
    if package.len() <= prefix.len() || !package[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return None;
    }
    let version = &package[prefix.len()..];
    let version = version.split('_').next()?;
    let mut components = version
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    while components.len() > 1 && components.last() == Some(&0) {
        components.pop();
    }
    (!components.is_empty()).then_some(components)
}

#[cfg(windows)]
fn package_companion_from_process(executable: &Path) -> Option<PathBuf> {
    let package_root = executable.ancestors().find(|ancestor| {
        ancestor
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.to_ascii_lowercase().starts_with("openai.codex_"))
    })?;
    let companion = package_root.join("app").join("resources").join("codex.exe");
    companion.is_file().then_some(companion)
}

#[cfg(windows)]
fn installed_desktop_companion() -> Option<PathBuf> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let packages = current_user
        .open_subkey(
            r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages",
        )
        .ok()?;
    newest_package_companion(
        packages
            .enum_keys()
            .filter_map(Result::ok)
            .filter(|name| name.to_ascii_lowercase().starts_with("openai.codex_"))
            .filter_map(|name| {
                let package = packages.open_subkey(name).ok()?;
                let root: String = package.get_value("PackageRootFolder").ok()?;
                let companion = PathBuf::from(root)
                    .join("app")
                    .join("resources")
                    .join("codex.exe");
                companion.is_file().then_some(companion)
            }),
    )
}

#[cfg(windows)]
fn relay_runtime_directory() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA").map(|root| {
        PathBuf::from(root)
            .join("Codex")
            .join("CodexDiscordRelay")
            .join("runtime")
    })
}

#[cfg(windows)]
fn materialize_desktop_companion_at(source: &Path, runtime: &Path) -> Result<PathBuf, ClientError> {
    if !is_windowsapps_package_executable(source) {
        return Err(ClientError::DesktopMaterialize(
            "refusing to materialize an executable outside the Codex Desktop package".to_owned(),
        ));
    }
    std::fs::create_dir_all(runtime)
        .map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
    let destination = runtime.join("codex.exe");
    let source_hash = sha256_file(source)?;
    if destination.is_file() && sha256_file(&destination).is_ok_and(|hash| hash == source_hash) {
        return Ok(destination);
    }

    let mut temporary = tempfile::Builder::new()
        .prefix("codex-")
        .suffix(".exe.tmp")
        .tempfile_in(runtime)
        .map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
    let mut input =
        File::open(source).map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
    std::io::copy(&mut input, temporary.as_file_mut())
        .map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
    if sha256_file(temporary.path())? != source_hash {
        return Err(ClientError::DesktopMaterialize(
            "copied companion hash did not match the Desktop package".to_owned(),
        ));
    }
    temporary
        .persist(&destination)
        .map_err(|error| ClientError::DesktopMaterialize(error.error.to_string()))?;
    Ok(destination)
}

#[cfg(windows)]
fn sha256_file(path: &Path) -> Result<[u8; 32], ClientError> {
    let mut file =
        File::open(path).map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| ClientError::DesktopMaterialize(error.to_string()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

#[cfg(windows)]
fn codex_desktop_install_candidates() -> Vec<PathBuf> {
    let Some(local_app_data) = env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let local_app_data = PathBuf::from(local_app_data);
    [
        local_app_data
            .join("OpenAI")
            .join("Codex")
            .join("bin")
            .join("codex.exe"),
        local_app_data
            .join("Programs")
            .join("OpenAI")
            .join("Codex")
            .join("bin")
            .join("codex.exe"),
    ]
    .into_iter()
    .filter(|candidate| candidate.is_file())
    .collect()
}

type PendingResult = Result<Value, ClientError>;

struct Inner {
    command: CodexCommand,
    request_timeout: Duration,
    connect_lock: Mutex<()>,
    // Serializes generation/child/stdin replacement independently from
    // `connect_lock`. The write path can invalidate a generation while an
    // initialize request still holds the connect lock.
    lifecycle: Mutex<()>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    // JSON-RPC permits string or number request IDs. Keep the serialized ID
    // as the correlation key rather than assuming every peer uses u64s.
    pending: Mutex<HashMap<String, oneshot::Sender<PendingResult>>>,
    next_id: AtomicU64,
    generation: AtomicU64,
    initialized_generation: AtomicU64,
    connected: AtomicBool,
    closed: AtomicBool,
    notifications: broadcast::Sender<Notification>,
    server_requests: broadcast::Sender<ServerRequest>,
}

#[derive(Clone)]
pub struct CodexClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CodexClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexClient")
            .field("program", &self.inner.command.program())
            .field("connected", &self.inner.connected.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl CodexClient {
    pub fn discover() -> Result<Self, ClientError> {
        Ok(Self::new(CodexCommand::discover()?))
    }

    #[must_use]
    pub fn new(command: CodexCommand) -> Self {
        Self::with_timeout(command, DEFAULT_REQUEST_TIMEOUT)
    }

    #[must_use]
    pub fn with_timeout(command: CodexCommand, request_timeout: Duration) -> Self {
        let (notifications, _) = broadcast::channel(4_096);
        // A dropped server request is an approval Codex waits on forever, so
        // this buffer is sized far beyond any realistic pending-request burst.
        let (server_requests, _) = broadcast::channel(1_024);
        Self {
            inner: Arc::new(Inner {
                command,
                request_timeout,
                connect_lock: Mutex::new(()),
                lifecycle: Mutex::new(()),
                stdin: Mutex::new(None),
                child: Mutex::new(None),
                reader: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                generation: AtomicU64::new(0),
                initialized_generation: AtomicU64::new(0),
                connected: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                notifications,
                server_requests,
            }),
        }
    }

    #[must_use]
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.inner.notifications.subscribe()
    }

    #[must_use]
    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<ServerRequest> {
        self.inner.server_requests.subscribe()
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    pub async fn connect(&self) -> Result<(), ClientError> {
        self.ensure_initialized().await
    }

    pub async fn reconnect(&self) -> Result<(), ClientError> {
        self.disconnect("explicit reconnect").await;
        self.inner.closed.store(false, Ordering::Release);
        self.ensure_initialized().await
    }

    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.disconnect("client closed").await;
    }

    pub async fn request<P>(&self, method: &str, params: P) -> Result<Value, ClientError>
    where
        P: Serialize,
    {
        self.ensure_initialized().await?;
        let params = serde_json::to_value(params)?;
        self.request_raw(method, params).await
    }

    pub async fn request_value(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.ensure_initialized().await?;
        self.request_raw(method, params).await
    }

    pub async fn notify<P>(&self, method: &str, params: P) -> Result<(), ClientError>
    where
        P: Serialize,
    {
        self.ensure_initialized().await?;
        self.write_json(&json!({"method": method, "params": params}))
            .await
    }

    pub async fn respond_result(&self, id: Value, result: Value) -> Result<(), ClientError> {
        if request_id_key(&id).is_none() {
            return Err(ClientError::Protocol(
                "cannot respond with an invalid JSON-RPC request id".into(),
            ));
        }
        self.write_json(&json!({"id": id, "result": result})).await
    }

    pub async fn respond_error(&self, id: Value, error: RpcErrorObject) -> Result<(), ClientError> {
        if request_id_key(&id).is_none() {
            return Err(ClientError::Protocol(
                "cannot respond with an invalid JSON-RPC request id".into(),
            ));
        }
        self.write_json(&json!({"id": id, "error": error})).await
    }

    /// Reply only to the exact app-server process that emitted the request.
    /// JSON-RPC ids may be reused after reconnect, so an id alone is not a
    /// safe correlation key across process generations.
    pub async fn respond_result_for_generation(
        &self,
        generation: u64,
        id: Value,
        result: Value,
    ) -> Result<(), ClientError> {
        if request_id_key(&id).is_none() {
            return Err(ClientError::Protocol(
                "cannot respond with an invalid JSON-RPC request id".into(),
            ));
        }
        self.write_json_for_generation(generation, &json!({"id": id, "result": result}))
            .await
    }

    pub async fn respond_error_for_generation(
        &self,
        generation: u64,
        id: Value,
        error: RpcErrorObject,
    ) -> Result<(), ClientError> {
        if request_id_key(&id).is_none() {
            return Err(ClientError::Protocol(
                "cannot respond with an invalid JSON-RPC request id".into(),
            ));
        }
        self.write_json_for_generation(generation, &json!({"id": id, "error": error}))
            .await
    }

    pub async fn thread_list(&self, params: ThreadListParams) -> Result<Value, ClientError> {
        self.request("thread/list", params).await
    }

    pub async fn thread_read(&self, params: ThreadReadParams) -> Result<Value, ClientError> {
        self.request("thread/read", params).await
    }

    pub async fn thread_start(&self, params: ThreadStartParams) -> Result<Value, ClientError> {
        self.request("thread/start", params).await
    }

    pub async fn thread_resume(&self, params: ThreadResumeParams) -> Result<Value, ClientError> {
        self.request("thread/resume", params).await
    }

    pub async fn thread_fork(&self, params: ThreadForkParams) -> Result<Value, ClientError> {
        self.request("thread/fork", params).await
    }

    pub async fn thread_archive(&self, thread_id: impl Into<String>) -> Result<Value, ClientError> {
        self.request(
            "thread/archive",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn thread_unarchive(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/unarchive",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn thread_delete(&self, thread_id: impl Into<String>) -> Result<Value, ClientError> {
        self.request(
            "thread/delete",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn thread_compact_start(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/compact/start",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn thread_name_set(
        &self,
        thread_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/name/set",
            ThreadNameSetParams {
                thread_id: thread_id.into(),
                name: name.into(),
            },
        )
        .await
    }

    pub async fn thread_rollback(
        &self,
        thread_id: impl Into<String>,
        num_turns: u32,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/rollback",
            ThreadRollbackParams {
                thread_id: thread_id.into(),
                num_turns,
            },
        )
        .await
    }

    pub async fn turn_start(&self, params: TurnStartParams) -> Result<Value, ClientError> {
        self.request("turn/start", params).await
    }

    pub async fn turn_steer(&self, params: TurnSteerParams) -> Result<Value, ClientError> {
        self.request("turn/steer", params).await
    }

    pub async fn turn_interrupt(&self, params: TurnInterruptParams) -> Result<Value, ClientError> {
        self.request("turn/interrupt", params).await
    }

    pub async fn review_start(&self, params: ReviewStartParams) -> Result<Value, ClientError> {
        self.request("review/start", params).await
    }

    pub async fn model_list(&self, params: ModelListParams) -> Result<Value, ClientError> {
        self.request("model/list", params).await
    }

    pub async fn skills_list(&self, params: SkillsListParams) -> Result<Value, ClientError> {
        self.request("skills/list", params).await
    }

    pub async fn app_list(&self, params: AppListParams) -> Result<Value, ClientError> {
        self.request("app/list", params).await
    }

    pub async fn config_read(&self, params: ConfigReadParams) -> Result<Value, ClientError> {
        self.request("config/read", params).await
    }

    pub async fn account_read(&self, params: AccountReadParams) -> Result<Value, ClientError> {
        self.request("account/read", params).await
    }

    pub async fn account_rate_limits_read(&self) -> Result<Value, ClientError> {
        self.request("account/rateLimits/read", Value::Null).await
    }

    pub async fn account_usage_read(&self) -> Result<Value, ClientError> {
        self.request("account/usage/read", Value::Null).await
    }

    pub async fn thread_goal_get(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/goal/get",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn thread_goal_set(&self, params: ThreadGoalSetParams) -> Result<Value, ClientError> {
        self.request("thread/goal/set", params).await
    }

    pub async fn thread_goal_clear(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/goal/clear",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn thread_turns_list(
        &self,
        params: ThreadTurnsListParams,
    ) -> Result<Value, ClientError> {
        self.request("thread/turns/list", params).await
    }

    pub async fn thread_search(&self, params: ThreadSearchParams) -> Result<Value, ClientError> {
        self.request("thread/search", params).await
    }

    pub async fn thread_settings_update(
        &self,
        params: ThreadSettingsUpdateParams,
    ) -> Result<Value, ClientError> {
        self.request("thread/settings/update", params).await
    }

    pub async fn background_terminals_list(
        &self,
        params: BackgroundTerminalsListParams,
    ) -> Result<Value, ClientError> {
        self.request("thread/backgroundTerminals/list", params)
            .await
    }

    pub async fn background_terminals_clean(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<Value, ClientError> {
        self.request(
            "thread/backgroundTerminals/clean",
            ThreadIdParams {
                thread_id: thread_id.into(),
            },
        )
        .await
    }

    pub async fn background_terminal_terminate(
        &self,
        params: BackgroundTerminalTerminateParams,
    ) -> Result<Value, ClientError> {
        self.request("thread/backgroundTerminals/terminate", params)
            .await
    }

    pub async fn collaboration_mode_list(&self) -> Result<Value, ClientError> {
        self.request("collaborationMode/list", json!({})).await
    }

    pub async fn mcp_server_status_list(
        &self,
        params: McpServerStatusListParams,
    ) -> Result<Value, ClientError> {
        self.request("mcpServerStatus/list", params).await
    }

    pub async fn plugin_list(&self, params: PluginListParams) -> Result<Value, ClientError> {
        self.request("plugin/list", params).await
    }

    pub async fn plugin_installed(
        &self,
        params: PluginInstalledParams,
    ) -> Result<Value, ClientError> {
        self.request("plugin/installed", params).await
    }

    pub async fn plugin_read(&self, params: PluginLocatorParams) -> Result<Value, ClientError> {
        self.request("plugin/read", params).await
    }

    pub async fn fuzzy_file_search(
        &self,
        params: FuzzyFileSearchParams,
    ) -> Result<Value, ClientError> {
        self.request("fuzzyFileSearch", params).await
    }

    async fn ensure_initialized(&self) -> Result<(), ClientError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ClientError::ConnectionLost("client is closed".into()));
        }
        let _guard = self.inner.connect_lock.lock().await;
        // Close may win while this caller is queued on connect_lock. Recheck
        // after acquiring it so a stale connect cannot spawn a new child.
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ClientError::ConnectionLost("client is closed".into()));
        }
        let generation = if self.inner.connected.load(Ordering::Acquire) {
            self.inner.generation.load(Ordering::Acquire)
        } else {
            self.spawn_locked().await?
        };

        if self.inner.initialized_generation.load(Ordering::Acquire) == generation {
            return Ok(());
        }

        self.request_raw("initialize", initialize_params()).await?;
        self.write_json(&json!({"method": "initialized", "params": {}}))
            .await?;
        self.inner
            .initialized_generation
            .store(generation, Ordering::Release);
        Ok(())
    }

    async fn spawn_locked(&self) -> Result<u64, ClientError> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        // A previous generation may have reached EOF without being reaped.
        // Reap it before replacing the child handle so a crashed app-server
        // cannot accumulate as a zombie across reconnects.
        let previous_child = self.inner.child.lock().await.take();
        if let Some(mut child) = previous_child {
            terminate_child(&mut child).await;
        }
        if let Some(reader) = self.inner.reader.lock().await.take() {
            reader.abort();
        }
        let mut command = self.inner.command.process();
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(ClientError::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::ConnectionLost("spawned process has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::ConnectionLost("spawned process has no stdout".into()))?;
        let stderr = child.stderr.take();
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;

        *self.inner.stdin.lock().await = Some(stdin);
        *self.inner.child.lock().await = Some(child);
        self.inner.connected.store(true, Ordering::Release);

        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "codex_app_server", "{line}");
                }
            });
        }

        let inner = Arc::clone(&self.inner);
        let reader = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                match read_bounded_line(&mut stdout, &mut line, MAX_APP_SERVER_JSONL_FRAME_BYTES)
                    .await
                {
                    Ok(true) => match std::str::from_utf8(&line) {
                        Ok(line) if line.trim().is_empty() => {}
                        Ok(line) => handle_stdout_line(&inner, generation, line).await,
                        Err(error) => {
                            lose_connection(
                                &inner,
                                generation,
                                format!("app-server stdout is not valid UTF-8: {error}"),
                            )
                            .await;
                            break;
                        }
                    },
                    Ok(false) => {
                        lose_connection(&inner, generation, "app-server reached EOF".into()).await;
                        break;
                    }
                    Err(error) => {
                        lose_connection(
                            &inner,
                            generation,
                            format!("failed reading app-server stdout: {error}"),
                        )
                        .await;
                        break;
                    }
                }
            }
        });
        *self.inner.reader.lock().await = Some(reader);
        Ok(generation)
    }

    async fn request_raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        let id_value = json!(id);
        let id_key = request_id_key(&id_value).expect("generated request ID is valid");
        self.inner
            .pending
            .lock()
            .await
            .insert(id_key.clone(), sender);

        if let Err(error) = self
            .write_json(&json!({"id": id_value, "method": method, "params": params}))
            .await
        {
            self.inner.pending.lock().await.remove(&id_key);
            return Err(error);
        }

        match timeout(self.inner.request_timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(ClientError::Rpc {
                code,
                message,
                data,
                ..
            }))) => Err(ClientError::Rpc {
                method: method.to_owned(),
                code,
                message,
                data,
            }),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(ClientError::ConnectionLost(
                "response channel closed".into(),
            )),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id_key);
                Err(ClientError::Timeout {
                    method: method.to_owned(),
                    seconds: self.inner.request_timeout.as_secs(),
                })
            }
        }
    }

    async fn write_json(&self, value: &Value) -> Result<(), ClientError> {
        if !self.inner.connected.load(Ordering::Acquire) {
            return Err(ClientError::ConnectionLost("not connected".into()));
        }
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        let generation = self.inner.generation.load(Ordering::Acquire);
        let write_timeout = self.inner.request_timeout.min(DEFAULT_WRITE_TIMEOUT);
        let result = write_frame(&self.inner.stdin, &bytes, write_timeout).await;
        if let Err(
            error @ (ClientError::Write(_)
            | ClientError::WriteTimeout { .. }
            | ClientError::ConnectionLost(_)),
        ) = &result
        {
            // A broken pipe is a transport-level event, not just a failed
            // request. Invalidate this generation and release every waiter so
            // the next operation can establish a fresh app-server process.
            lose_connection(&self.inner, generation, error.to_string()).await;
        }
        result
    }

    async fn write_json_for_generation(
        &self,
        expected_generation: u64,
        value: &Value,
    ) -> Result<(), ClientError> {
        if !self.inner.connected.load(Ordering::Acquire)
            || self.inner.generation.load(Ordering::Acquire) != expected_generation
        {
            return Err(ClientError::ConnectionLost(
                "server request belongs to a stale app-server generation".into(),
            ));
        }
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        let write_timeout = self.inner.request_timeout.min(DEFAULT_WRITE_TIMEOUT);
        let result = match timeout(write_timeout, async {
            let mut writer = self.inner.stdin.lock().await;
            if !self.inner.connected.load(Ordering::Acquire)
                || self.inner.generation.load(Ordering::Acquire) != expected_generation
            {
                return Err(ClientError::ConnectionLost(
                    "server request belongs to a stale app-server generation".into(),
                ));
            }
            let writer = writer
                .as_mut()
                .ok_or_else(|| ClientError::ConnectionLost("stdin is unavailable".into()))?;
            writer.write_all(&bytes).await.map_err(ClientError::Write)?;
            writer.flush().await.map_err(ClientError::Write)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ClientError::WriteTimeout {
                millis: write_timeout.as_millis(),
            }),
        };
        if let Err(
            error @ (ClientError::Write(_)
            | ClientError::WriteTimeout { .. }
            | ClientError::ConnectionLost(_)),
        ) = &result
            && self.inner.generation.load(Ordering::Acquire) == expected_generation
        {
            // A stale approval must never invalidate a newer app-server.
            lose_connection(&self.inner, expected_generation, error.to_string()).await;
        }
        result
    }

    async fn disconnect(&self, reason: &str) {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.inner.connected.store(false, Ordering::Release);
        self.inner
            .initialized_generation
            .store(0, Ordering::Release);
        // Release callers before process cleanup. In particular, close must not
        // wait for an initialize response while initialize owns connect_lock.
        fail_pending(&self.inner, reason).await;
        let child = self.inner.child.lock().await.take();
        if let Some(mut child) = child {
            terminate_child(&mut child).await;
        }
        if let Some(reader) = self.inner.reader.lock().await.take() {
            reader.abort();
        }
        // The write path has its own deadline, including time spent waiting
        // for this mutex. Therefore cleanup is bounded even if the OS pipe
        // stopped accepting bytes while another task held the writer.
        let write_timeout = self.inner.request_timeout.min(DEFAULT_WRITE_TIMEOUT);
        if let Ok(mut stdin) = timeout(write_timeout, self.inner.stdin.lock()).await {
            stdin.take();
        }
    }
}

fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "codex-discord-relay",
            "title": "Codex Discord Relay",
            "version": env!("CARGO_PKG_VERSION")
        },
        "capabilities": advertised_client_capabilities()
    })
}

async fn write_frame<W>(
    writer: &Mutex<Option<W>>,
    bytes: &[u8],
    write_timeout: Duration,
) -> Result<(), ClientError>
where
    W: AsyncWrite + Unpin,
{
    match timeout(write_timeout, async {
        let mut writer = writer.lock().await;
        let writer = writer
            .as_mut()
            .ok_or_else(|| ClientError::ConnectionLost("stdin is unavailable".into()))?;
        writer.write_all(bytes).await.map_err(ClientError::Write)?;
        writer.flush().await.map_err(ClientError::Write)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ClientError::WriteTimeout {
            millis: write_timeout.as_millis(),
        }),
    }
}

async fn terminate_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = timeout(PROCESS_SHUTDOWN_TIMEOUT, child.wait()).await;
}

async fn read_bounded_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<bool>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.unwrap_or(available.len());
        if chunk_len > max_bytes.saturating_sub(line.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("app-server JSONL frame exceeds {max_bytes} bytes"),
            ));
        }

        line.extend_from_slice(&available[..chunk_len]);
        let consumed = newline.map_or(chunk_len, |position| position + 1);
        reader.consume(consumed);

        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(true);
        }
    }
}

async fn handle_stdout_line(inner: &Arc<Inner>, generation: u64, line: &str) {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => handle_incoming(inner, generation, value).await,
        Err(error) => {
            tracing::warn!(
                %error,
                line = %line.chars().take(512).collect::<String>(),
                "ignoring non-JSON line from app-server stdout"
            );
        }
    }
}

async fn handle_incoming(inner: &Arc<Inner>, generation: u64, value: Value) {
    match classify_message(&value) {
        Ok(WireMessage::Response { id, result }) => {
            let Some(id) = request_id_key(&id) else {
                tracing::warn!(?id, "ignoring response with invalid JSON-RPC request id");
                return;
            };
            if let Some(sender) = inner.pending.lock().await.remove(&id) {
                let result = result.map_err(|error| ClientError::Rpc {
                    method: "pending request".into(),
                    code: error.code,
                    message: error.message,
                    data: error.data,
                });
                let _ = sender.send(result);
            }
        }
        Ok(WireMessage::Notification(mut notification)) => {
            notification.generation = generation;
            let _ = inner.notifications.send(notification);
        }
        Ok(WireMessage::ServerRequest(mut request)) => {
            request.generation = generation;
            let _ = inner.server_requests.send(request);
        }
        Err(error) => tracing::warn!(%error, "ignoring malformed app-server message"),
    }
}

async fn lose_connection(inner: &Arc<Inner>, generation: u64, reason: String) {
    let _lifecycle = inner.lifecycle.lock().await;
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    inner.connected.store(false, Ordering::Release);
    inner.initialized_generation.store(0, Ordering::Release);
    // Fail waiters before cleanup, then terminate the peer before waiting for
    // its pipe handle. Killing the child unblocks an outstanding OS pipe write.
    fail_pending(inner, &reason).await;
    let child = inner.child.lock().await.take();
    if let Some(mut child) = child {
        // EOF usually means the process already exited; kill is harmless in
        // that case and guarantees cleanup if stdout closed unexpectedly.
        terminate_child(&mut child).await;
    }
    let write_timeout = inner.request_timeout.min(DEFAULT_WRITE_TIMEOUT);
    if let Ok(mut stdin) = timeout(write_timeout, inner.stdin.lock()).await {
        stdin.take();
    }
}

fn request_id_key(id: &Value) -> Option<String> {
    match id {
        Value::String(_) | Value::Number(_) => serde_json::to_string(id).ok(),
        _ => None,
    }
}

async fn fail_pending(inner: &Arc<Inner>, reason: &str) {
    let pending = std::mem::take(&mut *inner.pending.lock().await);
    for (_, sender) in pending {
        let _ = sender.send(Err(ClientError::ConnectionLost(reason.to_owned())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::duplex;

    fn test_inner() -> Arc<Inner> {
        let (notifications, _) = broadcast::channel(8);
        let (server_requests, _) = broadcast::channel(8);
        Arc::new(Inner {
            command: CodexCommand::new("codex"),
            request_timeout: Duration::from_secs(1),
            connect_lock: Mutex::new(()),
            lifecycle: Mutex::new(()),
            stdin: Mutex::new(None),
            child: Mutex::new(None),
            reader: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            generation: AtomicU64::new(1),
            initialized_generation: AtomicU64::new(0),
            connected: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            notifications,
            server_requests,
        })
    }

    #[tokio::test]
    async fn correlates_responses_and_broadcasts_events() {
        let inner = test_inner();
        let (sender, receiver) = oneshot::channel();
        inner.pending.lock().await.insert("7".into(), sender);
        handle_incoming(&inner, 1, json!({"id": 7, "result": {"ok": true}})).await;
        assert_eq!(receiver.await.unwrap().unwrap(), json!({"ok": true}));

        let mut notifications = inner.notifications.subscribe();
        handle_incoming(
            &inner,
            1,
            json!({"method": "turn/completed", "params": {"threadId": "t"}}),
        )
        .await;
        let notification = notifications.recv().await.unwrap();
        assert_eq!(notification.method, "turn/completed");
        assert_eq!(notification.generation, 1);

        let mut requests = inner.server_requests.subscribe();
        handle_incoming(
            &inner,
            1,
            json!({"id": "approval-1", "method": "item/commandExecution/requestApproval", "params": {}}),
        )
        .await;
        let request = requests.recv().await.unwrap();
        assert_eq!(request.id, "approval-1");
        assert_eq!(request.generation, 1);
    }

    #[test]
    fn request_id_keys_preserve_json_rpc_scalar_types() {
        assert_eq!(request_id_key(&json!(7)).as_deref(), Some("7"));
        assert_eq!(request_id_key(&json!("7")).as_deref(), Some("\"7\""));
        assert!(request_id_key(&json!(null)).is_none());
        assert!(request_id_key(&json!({"id": 7})).is_none());
    }

    #[tokio::test]
    async fn correlates_string_ids_for_future_protocols() {
        let inner = test_inner();
        let (sender, receiver) = oneshot::channel();
        inner
            .pending
            .lock()
            .await
            .insert("\"future-1\"".into(), sender);
        handle_incoming(&inner, 1, json!({"id": "future-1", "result": true})).await;
        assert_eq!(receiver.await.unwrap().unwrap(), json!(true));
    }

    #[tokio::test]
    async fn stale_server_request_cannot_reply_into_a_new_generation() {
        let inner = test_inner();
        inner.generation.store(2, Ordering::Release);
        let client = CodexClient {
            inner: Arc::clone(&inner),
        };
        let error = client
            .respond_result_for_generation(1, json!(7), json!({"decision":"accept"}))
            .await
            .unwrap_err();
        assert!(
            matches!(error, ClientError::ConnectionLost(message) if message.contains("stale app-server generation"))
        );
        assert_eq!(inner.generation.load(Ordering::Acquire), 2);
        assert!(inner.connected.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn non_json_stdout_line_does_not_tear_down_a_healthy_connection() {
        let inner = test_inner();
        let (sender, receiver) = oneshot::channel();
        inner.pending.lock().await.insert("9".into(), sender);

        handle_stdout_line(&inner, 1, "app-server diagnostic that is not JSON").await;
        assert!(inner.connected.load(Ordering::Acquire));
        assert_eq!(inner.pending.lock().await.len(), 1);

        handle_stdout_line(&inner, 1, r#"{"id":9,"result":{"alive":true}}"#).await;
        assert_eq!(receiver.await.unwrap().unwrap(), json!({"alive": true}));
        assert!(inner.connected.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn disconnect_fails_every_pending_request() {
        let inner = test_inner();
        let (sender, receiver) = oneshot::channel();
        inner.pending.lock().await.insert("1".into(), sender);
        fail_pending(&inner, "gone").await;
        assert!(matches!(
            receiver.await.unwrap(),
            Err(ClientError::ConnectionLost(message)) if message == "gone"
        ));
    }

    #[tokio::test]
    async fn write_failure_invalidation_fails_pending_requests() {
        let inner = test_inner();
        let (sender, receiver) = oneshot::channel();
        inner.pending.lock().await.insert("1".into(), sender);
        lose_connection(&inner, 1, "write deadline elapsed".into()).await;
        assert!(!inner.connected.load(Ordering::Acquire));
        assert!(matches!(
            receiver.await.unwrap(),
            Err(ClientError::ConnectionLost(message)) if message == "write deadline elapsed"
        ));
    }

    #[tokio::test]
    async fn close_does_not_wait_for_initialize_connect_lock() {
        let inner = test_inner();
        let client = CodexClient {
            inner: Arc::clone(&inner),
        };
        let connect_guard = inner.connect_lock.lock().await;
        timeout(Duration::from_millis(100), client.close())
            .await
            .expect("close must not wait behind initialization");
        drop(connect_guard);
        assert!(inner.closed.load(Ordering::Acquire));
        assert!(!inner.connected.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn connect_queued_before_close_cannot_spawn_after_close() {
        let inner = test_inner();
        let client = CodexClient {
            inner: Arc::clone(&inner),
        };
        let connect_guard = inner.connect_lock.lock().await;
        let connect_client = client.clone();
        let connect = tokio::spawn(async move { connect_client.connect().await });
        tokio::task::yield_now().await;
        assert!(!connect.is_finished());

        timeout(Duration::from_millis(100), client.close())
            .await
            .expect("close must complete while connect is queued");
        drop(connect_guard);

        assert!(matches!(
            connect.await.unwrap(),
            Err(ClientError::ConnectionLost(message)) if message == "client is closed"
        ));
        assert!(inner.child.lock().await.is_none());
        assert!(!inner.connected.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn write_deadline_covers_pipe_backpressure() {
        let (writer, _unread_peer) = duplex(1);
        let writer = Mutex::new(Some(writer));
        let result = write_frame(&writer, &[b'x'; 8 * 1_024], Duration::from_millis(25)).await;
        assert!(matches!(
            result,
            Err(ClientError::WriteTimeout { millis: 25 })
        ));
    }

    #[tokio::test]
    async fn write_deadline_includes_waiting_for_writer_mutex() {
        let (writer, _peer) = duplex(64);
        let writer = Mutex::new(Some(writer));
        let guard = writer.lock().await;
        let result = write_frame(&writer, b"{}\n", Duration::from_millis(25)).await;
        drop(guard);
        assert!(matches!(
            result,
            Err(ClientError::WriteTimeout { millis: 25 })
        ));
    }

    #[tokio::test]
    async fn bounded_line_reader_accepts_a_frame_at_the_limit() {
        let mut reader = BufReader::new(&b"1234\n"[..]);
        let mut line = Vec::new();

        let found = read_bounded_line(&mut reader, &mut line, 4).await.unwrap();

        assert_eq!((found, line), (true, b"1234".to_vec()));
    }

    #[tokio::test]
    async fn bounded_line_reader_rejects_a_frame_over_the_limit() {
        let mut reader = BufReader::with_capacity(2, &b"12345\n"[..]);
        let mut line = Vec::new();

        let error = read_bounded_line(&mut reader, &mut line, 4)
            .await
            .unwrap_err();

        assert_eq!(
            (error.kind(), error.to_string(), line.len()),
            (
                io::ErrorKind::InvalidData,
                "app-server JSONL frame exceeds 4 bytes".to_owned(),
                4,
            )
        );
    }

    #[test]
    fn handshake_identifies_the_discord_relay() {
        let params = initialize_params();
        assert_eq!(params["clientInfo"]["name"], "codex-discord-relay");
        assert_eq!(params["clientInfo"]["title"], "Codex Discord Relay");
    }

    #[tokio::test]
    async fn stale_generation_cannot_invalidate_new_connection() {
        let inner = test_inner();
        inner.generation.store(2, Ordering::Release);
        lose_connection(&inner, 1, "old process exited".into()).await;
        assert!(inner.connected.load(Ordering::Acquire));
        assert_eq!(inner.initialized_generation.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn response_helpers_reject_invalid_ids_before_writing() {
        let client = CodexClient::new(CodexCommand::new("codex"));
        assert!(matches!(
            client.respond_result(Value::Null, Value::Null).await,
            Err(ClientError::Protocol(message)) if message.contains("invalid")
        ));
        assert!(matches!(
            client
                .respond_error(Value::Bool(false), RpcErrorObject {
                    code: -1,
                    message: "bad".into(),
                    data: None,
                })
                .await,
            Err(ClientError::Protocol(message)) if message.contains("invalid")
        ));
    }

    #[test]
    fn explicit_command_wins_discovery() {
        let key = "CODEX_RELAY_CODEX_EXECUTABLE";
        // Environment mutation is process-global, so only validate the constructor here.
        let command = CodexCommand::new(PathBuf::from("C:/tools/codex.exe"));
        assert_eq!(command.program(), Path::new("C:/tools/codex.exe"));
        assert_ne!(key, "");
    }

    #[cfg(windows)]
    fn fake_package(root: &Path, version: &str, contents: &[u8]) -> PathBuf {
        let companion = root
            .join("WindowsApps")
            .join(format!("OpenAI.Codex_{version}_x64__id"))
            .join("app")
            .join("resources")
            .join("codex.exe");
        std::fs::create_dir_all(companion.parent().unwrap()).unwrap();
        std::fs::write(&companion, contents).unwrap();
        companion
    }

    #[cfg(windows)]
    #[test]
    fn running_package_wins_and_is_never_spawned_directly() {
        let root = tempfile::tempdir().unwrap();
        let running = fake_package(root.path(), "26.800.0.0", b"running");
        let installed = fake_package(root.path(), "26.700.0.0", b"installed");
        let runtime = root.path().join("relay-runtime");

        let command = discover_windows_desktop_from(
            Some(running.clone()),
            Some(installed),
            Some(runtime.as_path()),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(command.program(), runtime.join("codex.exe"));
        assert_ne!(command.program(), running);
        assert_eq!(std::fs::read(command.program()).unwrap(), b"running");
    }

    #[cfg(windows)]
    #[test]
    fn package_selection_compares_msix_versions_numerically() {
        let root = tempfile::tempdir().unwrap();
        let v_800_9 = fake_package(root.path(), "26.800.9.0", b"older");
        let v_800_10 = fake_package(root.path(), "26.800.10.0", b"newer");
        assert_eq!(
            newest_package_companion([v_800_10.clone(), v_800_9]),
            Some(v_800_10)
        );

        let v_800 = fake_package(root.path(), "26.800.99.0", b"older-major");
        let v_1000 = fake_package(root.path(), "26.1000.0.0", b"newer-major");
        assert_eq!(
            newest_package_companion([v_1000.clone(), v_800]),
            Some(v_1000)
        );
    }

    #[cfg(windows)]
    #[test]
    fn package_selection_prefers_valid_version_and_has_stable_parse_fallback() {
        let root = tempfile::tempdir().unwrap();
        let valid = fake_package(root.path(), "26.900.0.0", b"valid");
        let invalid_a = fake_package(root.path(), "preview-a", b"invalid-a");
        let invalid_b = fake_package(root.path(), "preview-b", b"invalid-b");
        assert_eq!(
            newest_package_companion([invalid_b.clone(), valid.clone()]),
            Some(valid)
        );
        assert_eq!(
            newest_package_companion([invalid_a, invalid_b.clone()]),
            Some(invalid_b)
        );
    }

    #[cfg(windows)]
    #[test]
    fn materializer_reuses_matching_hash_and_refreshes_changed_source() {
        let root = tempfile::tempdir().unwrap();
        let source = fake_package(root.path(), "26.800.0.0", b"version-one");
        let runtime = root.path().join("relay-runtime");
        let destination = materialize_desktop_companion_at(&source, &runtime).unwrap();

        let unchanged = std::fs::metadata(&destination).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            materialize_desktop_companion_at(&source, &runtime).unwrap(),
            destination
        );
        assert_eq!(
            std::fs::metadata(&destination).unwrap().modified().unwrap(),
            unchanged,
            "a matching cached hash must not rewrite the executable"
        );

        std::fs::write(&source, b"version-two").unwrap();
        materialize_desktop_companion_at(&source, &runtime).unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"version-two");
    }

    #[cfg(windows)]
    #[test]
    fn cached_runtime_falls_back_after_package_refresh_failure() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("relay-runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let cached = runtime.join("codex.exe");
        std::fs::write(&cached, b"last-known-good").unwrap();
        let inaccessible = root
            .path()
            .join("WindowsApps")
            .join("OpenAI.Codex_26.900.0.0_x64__id")
            .join("app")
            .join("resources")
            .join("codex.exe");

        let command = discover_windows_desktop_from(
            Some(inaccessible),
            None,
            Some(runtime.as_path()),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(command.program(), cached);
    }

    #[cfg(windows)]
    #[test]
    fn stale_legacy_companion_is_the_final_fallback() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root
            .path()
            .join("AppData")
            .join("Local")
            .join("OpenAI")
            .join("Codex")
            .join("bin")
            .join("codex.exe");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"legacy").unwrap();

        let command =
            discover_windows_desktop_from(None, None, None, vec![legacy.clone()]).unwrap();
        assert_eq!(command.program(), legacy);
    }

    #[cfg(windows)]
    #[test]
    fn windowsapps_candidate_is_not_accepted_as_a_legacy_spawn_target() {
        let root = tempfile::tempdir().unwrap();
        let packaged = fake_package(root.path(), "26.800.0.0", b"package");
        let result = discover_windows_desktop_from(None, None, None, vec![packaged]);
        assert!(matches!(result, Err(ClientError::CodexNotFound)));
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_app_server_handshake_and_thread_list() {
        let client = CodexClient::discover().unwrap();
        let result = client
            .thread_list(ThreadListParams {
                limit: Some(1),
                ..ThreadListParams::default()
            })
            .await
            .unwrap();
        assert!(result.is_object());
        client.close().await;
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_mcp_full_catalog_has_browsable_entries() {
        let client = CodexClient::discover().unwrap();
        let result = client
            .mcp_server_status_list(McpServerStatusListParams {
                cursor: None,
                limit: Some(100),
                detail: Some("full".to_owned()),
                thread_id: None,
            })
            .await
            .unwrap();
        let servers = result
            .get("data")
            .and_then(Value::as_array)
            .expect("mcpServerStatus/list returned no data array");
        assert!(
            !servers.is_empty(),
            "installed Codex returned no MCP servers"
        );
        assert!(servers.iter().all(|server| server.get("name").is_some()));
        assert!(
            servers.iter().any(|server| {
                server
                    .get("tools")
                    .and_then(Value::as_object)
                    .is_some_and(|tools| !tools.is_empty())
            }),
            "full MCP detail returned no browsable tools"
        );
        client.close().await;
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_installed_plugins_have_marketplace_entries() {
        let client = CodexClient::discover().unwrap();
        let result = client
            .plugin_installed(PluginInstalledParams::default())
            .await
            .unwrap();
        let marketplaces = result
            .get("marketplaces")
            .or_else(|| result.pointer("/data/marketplaces"))
            .and_then(Value::as_array)
            .expect("plugin/installed returned no marketplaces array");
        assert!(
            marketplaces.iter().any(|marketplace| {
                marketplace
                    .get("plugins")
                    .and_then(Value::as_array)
                    .is_some_and(|plugins| !plugins.is_empty())
            }),
            "installed Codex returned no plugin entries"
        );
        let (plugin_name, marketplace_path, remote_marketplace_name) = marketplaces
            .iter()
            .find_map(|marketplace| {
                let plugin = marketplace
                    .get("plugins")?
                    .as_array()?
                    .iter()
                    .find(|plugin| {
                        plugin
                            .get("installed")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                            && plugin.get("name").and_then(Value::as_str).is_some()
                    })?;
                let name = plugin.get("name")?.as_str()?.to_owned();
                let path = marketplace
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let remote_name = path.is_none().then(|| {
                    marketplace
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned()
                });
                Some((name, path, remote_name))
            })
            .expect("plugin/installed returned no installed plugin locator");
        let detail = client
            .plugin_read(PluginLocatorParams {
                plugin_name,
                marketplace_path,
                remote_marketplace_name,
            })
            .await
            .unwrap();
        assert!(
            detail.pointer("/plugin/summary").is_some(),
            "plugin/read returned no plugin summary"
        );
        let catalog = client
            .plugin_list(PluginListParams::default())
            .await
            .unwrap();
        assert!(
            catalog
                .get("marketplaces")
                .or_else(|| catalog.pointer("/data/marketplaces"))
                .and_then(Value::as_array)
                .is_some_and(|marketplaces| !marketplaces.is_empty()),
            "plugin/list returned no marketplace catalog"
        );
        client.close().await;
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Codex Desktop"]
    fn live_discovery_prefers_codex_desktop_bundle() {
        let command = CodexCommand::discover().unwrap();
        assert!(
            !is_windowsapps_package_executable(command.program())
                && command
                    .program()
                    .ends_with(Path::new("runtime").join("codex.exe")),
            "selected a non-materialized runtime at {}",
            command.program().display()
        );
    }
}
