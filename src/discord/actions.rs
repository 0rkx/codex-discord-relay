use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Number, Value, json};

use crate::codex::MethodCapability;

pub const FIELDS_PER_PAGE: usize = 5;
const MAX_SCHEMA_DEPTH: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionSurface {
    Account,
    Connectors,
    ModelsAndModes,
    CommandsAndProcesses,
    Configuration,
    Environments,
    Files,
    Search,
    PluginsAndMarketplace,
    SkillsAndHooks,
    Memory,
    RemoteControl,
    Review,
    TasksAndTurns,
    Windows,
    System,
}

impl ActionSurface {
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Connectors => "connectors",
            Self::ModelsAndModes => "models-and-modes",
            Self::CommandsAndProcesses => "commands-and-processes",
            Self::Configuration => "configuration",
            Self::Environments => "environments",
            Self::Files => "files",
            Self::Search => "search",
            Self::PluginsAndMarketplace => "plugins-and-marketplace",
            Self::SkillsAndHooks => "skills-and-hooks",
            Self::Memory => "memory",
            Self::RemoteControl => "remote-control",
            Self::Review => "review",
            Self::TasksAndTurns => "tasks-and-turns",
            Self::Windows => "windows",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStrategy {
    NoInput,
    TypedSchemaForm,
    TaskTypedSchemaForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionAuthorization {
    Normal,
    Owner,
    God,
}

impl ActionAuthorization {
    #[must_use]
    pub const fn audit_label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Owner => "owner",
            Self::God => "god",
        }
    }

    /// User-facing risk summary for the authorization actually required after
    /// parameter-sensitive escalation.
    #[must_use]
    pub const fn risk_label(self) -> &'static str {
        match self {
            Self::Normal => "Read-only",
            Self::Owner => "Owner mutation — confirmation required",
            Self::God => "High risk — active task-scoped GOD mode required",
        }
    }

    /// User-facing description of the gate enforced by the executor.
    #[must_use]
    pub const fn requirement_label(self) -> &'static str {
        match self {
            Self::Normal => "Normal read-only session",
            Self::Owner => "Configured owner confirmation",
            Self::God => "Active task-scoped GOD mode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    None,
    Required,
    Danger,
}

impl ConfirmationPolicy {
    #[must_use]
    pub const fn audit_label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Required => "required",
            Self::Danger => "danger",
        }
    }

    #[must_use]
    pub const fn color(self) -> u32 {
        match self {
            Self::Danger => 0xED_4245,
            Self::Required => 0xFE_E75C,
            Self::None => 0x57_F287,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRenderer {
    Account,
    Connector,
    Configuration,
    Environment,
    File,
    Model,
    Plugin,
    Process,
    RemoteControl,
    Search,
    Skill,
    System,
    Task,
    Windows,
}

impl ActionRenderer {
    #[must_use]
    pub const fn result_title(self) -> &'static str {
        match self {
            Self::Account => "Account result",
            Self::Connector => "Connector result",
            Self::Configuration => "Configuration result",
            Self::Environment => "Environment result",
            Self::File => "File result",
            Self::Model => "Model result",
            Self::Plugin => "Plugin result",
            Self::Process => "Process result",
            Self::RemoteControl => "Remote-control result",
            Self::Search => "Search result",
            Self::Skill => "Skill result",
            Self::System => "System result",
            Self::Task => "Task result",
            Self::Windows => "Windows result",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedicatedActionSpec {
    pub method: &'static str,
    pub surface: ActionSurface,
    pub label: &'static str,
    pub description: &'static str,
    pub input: InputStrategy,
    pub authorization: ActionAuthorization,
    pub confirmation: ConfirmationPolicy,
    pub renderer: ActionRenderer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionExemption {
    pub method: &'static str,
    pub reason: &'static str,
}

macro_rules! action {
    ($method:literal, $surface:ident, $label:literal, $description:literal, $input:ident, $auth:ident, $confirm:ident, $renderer:ident) => {
        DedicatedActionSpec {
            method: $method,
            surface: ActionSurface::$surface,
            label: $label,
            description: $description,
            input: InputStrategy::$input,
            authorization: ActionAuthorization::$auth,
            confirmation: ConfirmationPolicy::$confirm,
            renderer: ActionRenderer::$renderer,
        }
    };
}

/// Audited, user-launchable Codex 0.145.0-alpha.18 actions. Every row is a deliberate
/// Discord contract: label, typed input, authorization, confirmation, and
/// result renderer. Schema-generated handling for unknown future methods is a
/// compatibility fallback and never enters this registry.
pub const DEDICATED_ACTIONS: &[DedicatedActionSpec] = &[
    action!(
        "account/login/cancel",
        Account,
        "Cancel sign-in",
        "Cancel the active ChatGPT sign-in attempt.",
        NoInput,
        Owner,
        Required,
        Account
    ),
    action!(
        "account/login/start",
        Account,
        "Start sign-in",
        "Begin the local ChatGPT sign-in flow.",
        TypedSchemaForm,
        Owner,
        Required,
        Account
    ),
    action!(
        "account/logout",
        Account,
        "Sign out",
        "Sign the local Codex installation out.",
        NoInput,
        Owner,
        Danger,
        Account
    ),
    action!(
        "account/rateLimitResetCredit/consume",
        Account,
        "Use reset credit",
        "Consume an available rate-limit reset credit.",
        TypedSchemaForm,
        Owner,
        Required,
        Account
    ),
    action!(
        "account/rateLimits/read",
        Account,
        "View rate limits",
        "Show current account rate limits and reset windows.",
        NoInput,
        Normal,
        None,
        Account
    ),
    action!(
        "account/read",
        Account,
        "View account",
        "Show the locally signed-in Codex account.",
        NoInput,
        Normal,
        None,
        Account
    ),
    action!(
        "account/sendAddCreditsNudgeEmail",
        Account,
        "Email credit reminder",
        "Ask OpenAI to send the account a credit reminder email.",
        NoInput,
        Owner,
        Required,
        Account
    ),
    action!(
        "account/usage/read",
        Account,
        "View usage",
        "Show current Codex account usage.",
        TypedSchemaForm,
        Normal,
        None,
        Account
    ),
    action!(
        "account/workspaceMessages/read",
        Account,
        "View workspace messages",
        "Read account workspace notices.",
        NoInput,
        Normal,
        None,
        Account
    ),
    action!(
        "app/list",
        Connectors,
        "List apps",
        "List apps available to the local Codex installation.",
        TypedSchemaForm,
        Normal,
        None,
        Connector
    ),
    action!(
        "collaborationMode/list",
        ModelsAndModes,
        "List collaboration modes",
        "Show available Codex collaboration modes.",
        NoInput,
        Normal,
        None,
        Model
    ),
    action!(
        "command/exec",
        CommandsAndProcesses,
        "Run raw command",
        "Run a raw local command with explicit GOD authorization.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "command/exec/resize",
        CommandsAndProcesses,
        "Resize command terminal",
        "Resize a live command terminal.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "command/exec/terminate",
        CommandsAndProcesses,
        "Terminate command",
        "Terminate a live raw command.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "command/exec/write",
        CommandsAndProcesses,
        "Write to command",
        "Write input to a live raw command.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "config/batchWrite",
        Configuration,
        "Write configuration batch",
        "Apply multiple local Codex configuration changes.",
        TypedSchemaForm,
        God,
        Danger,
        Configuration
    ),
    action!(
        "config/mcpServer/reload",
        Configuration,
        "Reload MCP servers",
        "Reload configured MCP servers.",
        NoInput,
        God,
        Danger,
        Configuration
    ),
    action!(
        "config/read",
        Configuration,
        "Read configuration",
        "Read effective local Codex configuration.",
        TypedSchemaForm,
        Normal,
        None,
        Configuration
    ),
    action!(
        "config/value/write",
        Configuration,
        "Write configuration value",
        "Write one local Codex configuration value.",
        TypedSchemaForm,
        God,
        Danger,
        Configuration
    ),
    action!(
        "configRequirements/read",
        Configuration,
        "Read configuration requirements",
        "Read enforced Codex configuration requirements.",
        NoInput,
        Normal,
        None,
        Configuration
    ),
    action!(
        "environment/add",
        Environments,
        "Add environment",
        "Add a Codex execution environment.",
        TypedSchemaForm,
        God,
        Danger,
        Environment
    ),
    action!(
        "environment/info",
        Environments,
        "View environment",
        "Show details for a Codex execution environment.",
        TypedSchemaForm,
        Normal,
        None,
        Environment
    ),
    action!(
        "environment/status",
        Environments,
        "Environment status",
        "Show the current connection status of a Codex execution environment.",
        TypedSchemaForm,
        Normal,
        None,
        Environment
    ),
    action!(
        "experimentalFeature/enablement/set",
        Configuration,
        "Set experimental feature",
        "Enable or disable a named experimental feature.",
        TypedSchemaForm,
        God,
        Danger,
        Configuration
    ),
    action!(
        "experimentalFeature/list",
        Configuration,
        "List experimental features",
        "List experimental features and enablement state.",
        NoInput,
        Normal,
        None,
        Configuration
    ),
    action!(
        "externalAgentConfig/detect",
        Environments,
        "Detect agent configuration",
        "Detect external agent configuration available for import.",
        TypedSchemaForm,
        Normal,
        None,
        Environment
    ),
    action!(
        "externalAgentConfig/import",
        Environments,
        "Import agent configuration",
        "Import selected external agent configuration.",
        TypedSchemaForm,
        God,
        Danger,
        Environment
    ),
    action!(
        "externalAgentConfig/import/readHistories",
        Environments,
        "Import agent histories",
        "Read and inject external agent history under GOD authorization.",
        TypedSchemaForm,
        God,
        Danger,
        Environment
    ),
    action!(
        "feedback/upload",
        System,
        "Upload feedback",
        "Upload a feedback report from this Codex installation.",
        TypedSchemaForm,
        Owner,
        Required,
        System
    ),
    action!(
        "fs/copy",
        Files,
        "Copy file",
        "Copy a local file or directory.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/createDirectory",
        Files,
        "Create directory",
        "Create a local directory.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/getMetadata",
        Files,
        "Get file metadata",
        "Read metadata for a local path.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/readDirectory",
        Files,
        "Read directory",
        "List a local directory.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/readFile",
        Files,
        "Read file",
        "Read a local text file.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/remove",
        Files,
        "Remove path",
        "Remove a local file or directory.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/unwatch",
        Files,
        "Stop watching path",
        "Stop an active local filesystem watch.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/watch",
        Files,
        "Watch path",
        "Start watching a local path for changes.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fs/writeFile",
        Files,
        "Write file",
        "Write content to a local file.",
        TypedSchemaForm,
        God,
        Danger,
        File
    ),
    action!(
        "fuzzyFileSearch",
        Search,
        "Search files",
        "Search workspace files by fuzzy name match.",
        TypedSchemaForm,
        Normal,
        None,
        Search
    ),
    action!(
        "fuzzyFileSearch/sessionStart",
        Search,
        "Start file-search session",
        "Start a streaming fuzzy file-search session.",
        TypedSchemaForm,
        Normal,
        None,
        Search
    ),
    action!(
        "fuzzyFileSearch/sessionStop",
        Search,
        "Stop file-search session",
        "Stop a streaming fuzzy file-search session.",
        TypedSchemaForm,
        Normal,
        None,
        Search
    ),
    action!(
        "fuzzyFileSearch/sessionUpdate",
        Search,
        "Update file search",
        "Update a streaming fuzzy file-search query.",
        TypedSchemaForm,
        Normal,
        None,
        Search
    ),
    action!(
        "hooks/list",
        SkillsAndHooks,
        "List hooks",
        "List configured Codex hooks.",
        NoInput,
        Normal,
        None,
        Skill
    ),
    action!(
        "marketplace/add",
        PluginsAndMarketplace,
        "Add marketplace",
        "Add a plugin marketplace source.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "marketplace/remove",
        PluginsAndMarketplace,
        "Remove marketplace",
        "Remove a plugin marketplace source.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "marketplace/upgrade",
        PluginsAndMarketplace,
        "Upgrade marketplace",
        "Refresh a plugin marketplace source.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "mcpServer/oauth/login",
        Connectors,
        "Sign in to MCP server",
        "Begin OAuth sign-in for an MCP server.",
        TypedSchemaForm,
        God,
        Danger,
        Connector
    ),
    action!(
        "mcpServer/resource/read",
        Connectors,
        "Read MCP resource",
        "Read a resource exposed by an MCP server.",
        TypedSchemaForm,
        Normal,
        None,
        Connector
    ),
    action!(
        "mcpServer/tool/call",
        Connectors,
        "Call MCP tool",
        "Invoke a configured MCP tool with typed fields.",
        TypedSchemaForm,
        God,
        Danger,
        Connector
    ),
    action!(
        "mcpServerStatus/list",
        Connectors,
        "List MCP status",
        "Show configured MCP server status.",
        TypedSchemaForm,
        Normal,
        None,
        Connector
    ),
    action!(
        "memory/reset",
        Memory,
        "Reset memory",
        "Reset Codex memory state.",
        TypedSchemaForm,
        Owner,
        Danger,
        System
    ),
    action!(
        "model/list",
        ModelsAndModes,
        "List models",
        "Show models available to the local Codex account.",
        TypedSchemaForm,
        Normal,
        None,
        Model
    ),
    action!(
        "modelProvider/capabilities/read",
        ModelsAndModes,
        "Read provider capabilities",
        "Show model-provider capabilities.",
        TypedSchemaForm,
        Normal,
        None,
        Model
    ),
    action!(
        "permissionProfile/list",
        ModelsAndModes,
        "List permission profiles",
        "Show available Codex permission profiles.",
        NoInput,
        Normal,
        None,
        Model
    ),
    action!(
        "plugin/install",
        PluginsAndMarketplace,
        "Install plugin",
        "Install a selected Codex plugin.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "plugin/installed",
        PluginsAndMarketplace,
        "List installed plugins",
        "Show installed Codex plugins.",
        NoInput,
        Normal,
        None,
        Plugin
    ),
    action!(
        "plugin/list",
        PluginsAndMarketplace,
        "Browse plugins",
        "List plugins available from configured marketplaces.",
        TypedSchemaForm,
        Normal,
        None,
        Plugin
    ),
    action!(
        "plugin/read",
        PluginsAndMarketplace,
        "Read plugin",
        "Show details for one plugin.",
        TypedSchemaForm,
        Normal,
        None,
        Plugin
    ),
    action!(
        "plugin/share/checkout",
        PluginsAndMarketplace,
        "Check out shared plugin",
        "Check out a shared plugin workspace.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "plugin/share/delete",
        PluginsAndMarketplace,
        "Delete shared plugin",
        "Delete a shared plugin record.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "plugin/share/list",
        PluginsAndMarketplace,
        "List shared plugins",
        "List shared plugin workspaces.",
        TypedSchemaForm,
        Normal,
        None,
        Plugin
    ),
    action!(
        "plugin/share/save",
        PluginsAndMarketplace,
        "Save shared plugin",
        "Save changes to a shared plugin.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "plugin/share/updateTargets",
        PluginsAndMarketplace,
        "Update plugin targets",
        "Update targets for a shared plugin.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "plugin/skill/read",
        PluginsAndMarketplace,
        "Read plugin skill",
        "Read a skill bundled with a plugin.",
        TypedSchemaForm,
        Normal,
        None,
        Plugin
    ),
    action!(
        "plugin/uninstall",
        PluginsAndMarketplace,
        "Uninstall plugin",
        "Uninstall a Codex plugin.",
        TypedSchemaForm,
        God,
        Danger,
        Plugin
    ),
    action!(
        "process/kill",
        CommandsAndProcesses,
        "Kill process",
        "Kill a raw local process under GOD authorization.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "process/resizePty",
        CommandsAndProcesses,
        "Resize process terminal",
        "Resize a raw process terminal under GOD authorization.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "process/spawn",
        CommandsAndProcesses,
        "Spawn process",
        "Spawn a raw local process under GOD authorization.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "process/writeStdin",
        CommandsAndProcesses,
        "Write process input",
        "Write to a raw local process under GOD authorization.",
        TypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "remoteControl/client/list",
        RemoteControl,
        "List remote clients",
        "List paired remote-control clients.",
        NoInput,
        Normal,
        None,
        RemoteControl
    ),
    action!(
        "remoteControl/client/revoke",
        RemoteControl,
        "Revoke remote client",
        "Revoke a paired remote-control client.",
        TypedSchemaForm,
        Owner,
        Danger,
        RemoteControl
    ),
    action!(
        "remoteControl/disable",
        RemoteControl,
        "Disable remote control",
        "Disable Codex remote control.",
        NoInput,
        Owner,
        Danger,
        RemoteControl
    ),
    action!(
        "remoteControl/enable",
        RemoteControl,
        "Enable remote control",
        "Enable Codex remote control.",
        NoInput,
        God,
        Danger,
        RemoteControl
    ),
    action!(
        "remoteControl/pairing/start",
        RemoteControl,
        "Start remote pairing",
        "Start pairing a remote-control client.",
        NoInput,
        God,
        Danger,
        RemoteControl
    ),
    action!(
        "remoteControl/pairing/status",
        RemoteControl,
        "View pairing status",
        "Show current remote pairing status.",
        NoInput,
        Normal,
        None,
        RemoteControl
    ),
    action!(
        "remoteControl/status/read",
        RemoteControl,
        "View remote status",
        "Show Codex remote-control status.",
        NoInput,
        Normal,
        None,
        RemoteControl
    ),
    action!(
        "review/start",
        Review,
        "Start review",
        "Start a Codex code-review turn for a task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "skills/config/write",
        SkillsAndHooks,
        "Write skill config",
        "Update local skill configuration.",
        TypedSchemaForm,
        God,
        Danger,
        Skill
    ),
    action!(
        "skills/extraRoots/set",
        SkillsAndHooks,
        "Set skill roots",
        "Set additional local skill roots.",
        TypedSchemaForm,
        God,
        Danger,
        Skill
    ),
    action!(
        "skills/list",
        SkillsAndHooks,
        "List skills",
        "List skills available to Codex.",
        TypedSchemaForm,
        Normal,
        None,
        Skill
    ),
    action!(
        "thread/approveGuardianDeniedAction",
        TasksAndTurns,
        "Override guardian denial",
        "Approve an action denied by the guardian under GOD authorization.",
        TaskTypedSchemaForm,
        God,
        Danger,
        Task
    ),
    action!(
        "thread/archive",
        TasksAndTurns,
        "Archive task",
        "Archive a Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/backgroundTerminals/clean",
        TasksAndTurns,
        "Clean background terminals",
        "Clean completed background terminals for a task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/backgroundTerminals/list",
        TasksAndTurns,
        "List background terminals",
        "List background terminals for a task.",
        TaskTypedSchemaForm,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/backgroundTerminals/terminate",
        TasksAndTurns,
        "Terminate background terminal",
        "Terminate a task background terminal.",
        TaskTypedSchemaForm,
        Owner,
        Danger,
        Task
    ),
    action!(
        "thread/compact/start",
        TasksAndTurns,
        "Compact task context",
        "Start task context compaction.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/delete",
        TasksAndTurns,
        "Delete task",
        "Permanently delete a Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Danger,
        Task
    ),
    action!(
        "thread/fork",
        TasksAndTurns,
        "Fork task",
        "Fork a Codex task into a new task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/goal/clear",
        TasksAndTurns,
        "Clear task goal",
        "Clear the active task goal.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/goal/get",
        TasksAndTurns,
        "View task goal",
        "Show the active task goal.",
        TaskTypedSchemaForm,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/goal/set",
        TasksAndTurns,
        "Set task goal",
        "Set the active task goal.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/inject_items",
        TasksAndTurns,
        "Inject task history",
        "Inject raw items into task history under GOD authorization.",
        TaskTypedSchemaForm,
        God,
        Danger,
        Task
    ),
    action!(
        "thread/items/list",
        TasksAndTurns,
        "List task items",
        "List items stored in a Codex task.",
        TaskTypedSchemaForm,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/list",
        TasksAndTurns,
        "List tasks",
        "List Codex tasks.",
        TypedSchemaForm,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/loaded/list",
        TasksAndTurns,
        "List loaded tasks",
        "List tasks loaded by the app server.",
        NoInput,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/memoryMode/set",
        TasksAndTurns,
        "Set task memory mode",
        "Set memory behavior for a task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/metadata/update",
        TasksAndTurns,
        "Update task metadata",
        "Update metadata for a Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/name/set",
        TasksAndTurns,
        "Rename task",
        "Set the display name for a Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/read",
        TasksAndTurns,
        "Read task",
        "Read a Codex task and its current state.",
        TaskTypedSchemaForm,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/realtime/appendAudio",
        TasksAndTurns,
        "Append realtime audio",
        "Append audio to a realtime task session.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/realtime/appendSpeech",
        TasksAndTurns,
        "Append realtime speech",
        "Append speech metadata to a realtime task session.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/realtime/appendText",
        TasksAndTurns,
        "Append realtime text",
        "Append text to a realtime task session.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/realtime/listVoices",
        TasksAndTurns,
        "List realtime voices",
        "List voices available for realtime tasks.",
        NoInput,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/realtime/start",
        TasksAndTurns,
        "Start realtime task",
        "Start a realtime session for a task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/realtime/stop",
        TasksAndTurns,
        "Stop realtime task",
        "Stop a realtime task session.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/resume",
        TasksAndTurns,
        "Resume task",
        "Resume a persisted Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/rollback",
        TasksAndTurns,
        "Roll back task",
        "Roll a task back to an earlier turn.",
        TaskTypedSchemaForm,
        Owner,
        Danger,
        Task
    ),
    action!(
        "thread/search",
        TasksAndTurns,
        "Search tasks",
        "Search Codex tasks.",
        TypedSchemaForm,
        Normal,
        None,
        Search
    ),
    action!(
        "thread/settings/update",
        TasksAndTurns,
        "Update task settings",
        "Update task sandbox and approval settings.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/shellCommand",
        TasksAndTurns,
        "Run task shell command",
        "Run an unrestricted task shell command under GOD authorization.",
        TaskTypedSchemaForm,
        God,
        Danger,
        Process
    ),
    action!(
        "thread/start",
        TasksAndTurns,
        "Start task",
        "Start a new Codex task.",
        TypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "thread/turns/list",
        TasksAndTurns,
        "List task turns",
        "List turns in a Codex task.",
        TaskTypedSchemaForm,
        Normal,
        None,
        Task
    ),
    action!(
        "thread/unarchive",
        TasksAndTurns,
        "Unarchive task",
        "Restore an archived Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "turn/interrupt",
        TasksAndTurns,
        "Interrupt turn",
        "Interrupt the active Codex turn.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "turn/start",
        TasksAndTurns,
        "Start turn",
        "Start a new turn in a Codex task.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "turn/steer",
        TasksAndTurns,
        "Steer turn",
        "Send steering input to the active Codex turn.",
        TaskTypedSchemaForm,
        Owner,
        Required,
        Task
    ),
    action!(
        "windowsSandbox/readiness",
        Windows,
        "Check Windows sandbox",
        "Check Windows sandbox readiness.",
        NoInput,
        Normal,
        None,
        Windows
    ),
    action!(
        "windowsSandbox/setupStart",
        Windows,
        "Set up Windows sandbox",
        "Start Windows sandbox setup.",
        TypedSchemaForm,
        Owner,
        Required,
        Windows
    ),
];

pub const ACTION_EXEMPTIONS: &[ActionExemption] = &[
    ActionExemption {
        method: "initialize",
        reason: "mandatory JSON-RPC handshake owned by the bridge, not a user action",
    },
    ActionExemption {
        method: "mock/experimentalMethod",
        reason: "protocol test fixture with no production user behavior",
    },
    ActionExemption {
        method: "thread/increment_elicitation",
        reason: "internal elicitation reference counter maintained by the app server",
    },
    ActionExemption {
        method: "thread/decrement_elicitation",
        reason: "internal elicitation reference counter maintained by the app server",
    },
    ActionExemption {
        method: "thread/unsubscribe",
        reason: "transport subscription cleanup owned by the bridge lifecycle",
    },
];

#[must_use]
pub fn dedicated_action(method: &str) -> Option<&'static DedicatedActionSpec> {
    DEDICATED_ACTIONS.iter().find(|spec| spec.method == method)
}

/// Capabilities that are equivalent to unrestricted local-device access.
///
/// This is the authoritative authorization boundary for both curated actions
/// and schema-discovered methods. Namespace defaults fail closed so a newly
/// added host-filesystem, configuration, marketplace, plugin, or skill
/// mutation cannot silently inherit an owner-only gate.
#[must_use]
pub fn capability_requires_god(method: &str) -> bool {
    if method.starts_with("fs/") || method.starts_with("marketplace/") {
        return true;
    }

    if method.starts_with("config/") {
        return method != "config/read";
    }

    if method.starts_with("plugin/") {
        return !matches!(
            method,
            "plugin/installed"
                | "plugin/list"
                | "plugin/read"
                | "plugin/share/list"
                | "plugin/skill/read"
        );
    }

    if method.starts_with("skills/") {
        return method != "skills/list";
    }

    matches!(
        method,
        "environment/add"
            | "experimentalFeature/enablement/set"
            | "externalAgentConfig/import"
            | "externalAgentConfig/import/readHistories"
            | "mcpServer/oauth/login"
            | "mcpServer/tool/call"
            | "remoteControl/enable"
            | "remoteControl/pairing/start"
    )
}

#[must_use]
pub fn action_exemption(method: &str) -> Option<&'static ActionExemption> {
    ACTION_EXEMPTIONS
        .iter()
        .find(|entry| entry.method == method)
}

/// Checks that a registry row can be driven by the installed method schema.
/// Coverage must count working Discord forms, not static registry entries.
#[must_use]
pub fn dedicated_action_form_error(capability: &MethodCapability) -> Option<String> {
    let spec = dedicated_action(&capability.method)?;
    let schema = capability.params_schema.as_ref();
    if spec.input != InputStrategy::NoInput && schema.is_none() {
        return Some("typed action has no parameter schema".to_owned());
    }
    let fields = schema.map(fields_from_schema).unwrap_or_default();
    if spec.input == InputStrategy::TaskTypedSchemaForm
        && !fields
            .iter()
            .any(|field| matches!(field.key().as_str(), "threadId" | "conversationId"))
    {
        return Some("task action schema has no task identifier".to_owned());
    }
    fields
        .iter()
        .find_map(|field| field_form_error(field).map(|error| format!("{}: {error}", field.key())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRisk {
    ReadOnly,
    Mutating,
}

impl ActionRisk {
    #[must_use]
    pub const fn audit_label(self) -> &'static str {
        match self {
            Self::ReadOnly => "ReadOnly",
            Self::Mutating => "Mutating",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveActionPolicy {
    pub authorization: ActionAuthorization,
    pub confirmation: ConfirmationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    String,
    Boolean,
    Integer,
    Number,
    Array,
    Object,
    /// A value whose schema cannot drive a typed line form: `true`/empty
    /// schemas that accept anything, or unions whose branches are structured
    /// objects (for example `sandboxPolicy`). The entered text is parsed as
    /// JSON when possible, kept as a plain string otherwise, and always
    /// validated against the live schema before dispatch.
    Json,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionField {
    pub path: Vec<String>,
    pub label: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: FieldKind,
    pub default: Option<Value>,
    pub enum_values: Vec<Value>,
    pub schema: Value,
}

impl ActionField {
    #[must_use]
    pub fn key(&self) -> String {
        self.path.join(".")
    }

    #[must_use]
    pub fn placeholder(&self) -> String {
        if !self.enum_values.is_empty() {
            return self
                .enum_values
                .iter()
                .map(compact_value)
                .collect::<Vec<_>>()
                .join(" | ")
                .chars()
                .take(100)
                .collect();
        }
        if self.kind == FieldKind::Object {
            let variants = schema_branches(&self.schema)
                .filter_map(tagged_type_value)
                .collect::<Vec<_>>();
            if !variants.is_empty() {
                return format!("type={}; then key=value fields", variants.join(" | "))
                    .chars()
                    .take(100)
                    .collect();
            }
        }
        if self.kind == FieldKind::Json {
            let variants = union_variant_names(&self.schema);
            if !variants.is_empty() {
                return format!("JSON or one of: {}", variants.join(" | "))
                    .chars()
                    .take(100)
                    .collect();
            }
        }
        self.description
            .as_deref()
            .unwrap_or(match self.kind {
                FieldKind::Boolean => "true or false",
                FieldKind::Integer => "whole number",
                FieldKind::Number => "number",
                FieldKind::Array => "Comma or newline-separated values",
                FieldKind::Object => "One key=value pair per line",
                FieldKind::String => "Text value",
                FieldKind::Json => "JSON value (plain text is kept as a string)",
            })
            .chars()
            .take(100)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ActionDraft {
    pub method: String,
    pub label: String,
    pub description: Option<String>,
    pub fields: Vec<ActionField>,
    pub params: Value,
    pub page: usize,
    pub risk: ActionRisk,
    pub authorization: ActionAuthorization,
    pub confirmation: ConfirmationPolicy,
    pub renderer: ActionRenderer,
    pub dedicated: bool,
    pub task_id: Option<String>,
    pub channel_id: u64,
    pub created_at: DateTime<Utc>,
}

impl ActionDraft {
    pub fn from_capability(
        capability: &MethodCapability,
        task_id: Option<String>,
        channel_id: u64,
        default_cwd: Option<&str>,
    ) -> Self {
        let spec = dedicated_action(&capability.method);
        let mut fields = capability
            .params_schema
            .as_ref()
            .map(fields_from_schema)
            .unwrap_or_default();
        let mut params = if capability
            .params_schema
            .as_ref()
            .and_then(|schema| schema.get("type"))
            .and_then(Value::as_str)
            == Some("null")
        {
            Value::Null
        } else {
            json!({})
        };
        if let Some(task_id) = task_id.as_deref() {
            for key in ["threadId", "conversationId"] {
                if fields.iter().any(|field| field.key() == key) {
                    set_path(
                        &mut params,
                        &[key.to_owned()],
                        Value::String(task_id.to_owned()),
                    );
                    fields.retain(|field| field.key() != key);
                }
            }
        }
        if let Some(cwd) = default_cwd {
            for field in &mut fields {
                if field.key() == "cwd" && field.default.is_none() {
                    field.default = Some(Value::String(cwd.to_owned()));
                }
            }
        }
        let authorization = if capability_requires_god(&capability.method) {
            ActionAuthorization::God
        } else {
            spec.map_or(ActionAuthorization::God, |spec| spec.authorization)
        };
        Self {
            method: capability.method.clone(),
            label: spec.map_or_else(|| capability.method.clone(), |spec| spec.label.to_owned()),
            description: spec
                .map(|spec| spec.description.to_owned())
                .or_else(|| capability.description.clone()),
            fields,
            params,
            page: 0,
            risk: spec.map_or_else(
                || classify_risk(&capability.method),
                |spec| {
                    if spec.authorization == ActionAuthorization::Normal {
                        ActionRisk::ReadOnly
                    } else {
                        ActionRisk::Mutating
                    }
                },
            ),
            authorization,
            confirmation: if authorization == ActionAuthorization::God {
                ConfirmationPolicy::Danger
            } else {
                spec.map_or(ConfirmationPolicy::Danger, |spec| spec.confirmation)
            },
            renderer: spec.map_or(ActionRenderer::System, |spec| spec.renderer),
            dedicated: spec.is_some(),
            task_id,
            channel_id,
            created_at: Utc::now(),
        }
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.fields.len().max(1).div_ceil(FIELDS_PER_PAGE)
    }

    #[must_use]
    pub fn page_fields(&self, page: usize) -> &[ActionField] {
        let start = page.saturating_mul(FIELDS_PER_PAGE).min(self.fields.len());
        let end = (start + FIELDS_PER_PAGE).min(self.fields.len());
        &self.fields[start..end]
    }

    pub fn apply_page_values(
        &mut self,
        page: usize,
        values: &BTreeMap<usize, String>,
    ) -> Result<()> {
        let start = page.saturating_mul(FIELDS_PER_PAGE);
        for (relative_index, raw) in values {
            let absolute_index = start + relative_index;
            let field = self
                .fields
                .get(absolute_index)
                .with_context(|| format!("unknown action field {absolute_index}"))?;
            let raw = raw.trim();
            if raw.is_empty() {
                if field.required && field.default.is_none() {
                    anyhow::bail!("{} is required", field.label);
                }
                if let Some(default) = field.default.clone() {
                    set_path(&mut self.params, &field.path, default);
                }
                continue;
            }
            let value = parse_field_value(field, raw, self.dedicated)
                .with_context(|| format!("invalid value for {}", field.label))?;
            set_path(&mut self.params, &field.path, value);
        }
        self.page = page;
        Ok(())
    }

    #[must_use]
    pub fn expired(&self) -> bool {
        Utc::now() - self.created_at > Duration::minutes(20)
    }

    #[must_use]
    pub fn redacted_preview(&self) -> Value {
        crate::security::redact_secrets(&self.params)
    }

    /// Parameter-sensitive escalation prevents a normally owner-authorized
    /// task method from smuggling `dangerFullAccess` or approval bypass through
    /// an otherwise safe typed form.
    #[must_use]
    pub fn effective_policy(&self) -> EffectiveActionPolicy {
        let authorization = if capability_requires_god(&self.method)
            || self.authorization == ActionAuthorization::God
            || contains_unrestricted_setting(&self.params)
        {
            ActionAuthorization::God
        } else {
            self.authorization
        };
        EffectiveActionPolicy {
            authorization,
            confirmation: if authorization == ActionAuthorization::God {
                ConfirmationPolicy::Danger
            } else {
                self.confirmation
            },
        }
    }
}

#[must_use]
pub fn method_category(method: &str) -> &str {
    if let Some(spec) = dedicated_action(method) {
        return spec.surface.slug();
    }
    match method.split('/').next().unwrap_or("other") {
        "account" => "account",
        "app" | "mcpServer" | "mcpServerStatus" => "connectors",
        "collaborationMode" | "model" | "modelProvider" | "permissionProfile" => "models-and-modes",
        "command" | "process" => "commands-and-processes",
        "config" | "configRequirements" | "experimentalFeature" => "configuration",
        "environment" | "externalAgentConfig" => "environments",
        "fs" => "files",
        "fuzzyFileSearch" => "search",
        "marketplace" | "plugin" => "plugins-and-marketplace",
        "hooks" | "skills" => "skills-and-hooks",
        "memory" => "memory",
        "remoteControl" => "remote-control",
        "review" => "review",
        "thread" | "turn" => "tasks-and-turns",
        "windows" | "windowsSandbox" => "windows",
        "feedback" | "mock" | "initialize" => "system",
        _ => "other",
    }
}

#[must_use]
pub fn classify_risk(method: &str) -> ActionRisk {
    let read_only = method.ends_with("/read")
        || method.ends_with("/list")
        || method.ends_with("/get")
        || method.ends_with("/status")
        || method.ends_with("/info")
        || method.ends_with("/detect")
        || method.ends_with("/readiness")
        || method.ends_with("/capabilities/read")
        || method.ends_with("/loaded/list")
        || method.ends_with("/turns/list")
        || method.ends_with("/items/list")
        || method == "fuzzyFileSearch"
        || method == "configRequirements/read";
    if read_only {
        ActionRisk::ReadOnly
    } else {
        ActionRisk::Mutating
    }
}

#[must_use]
pub fn fields_from_schema(schema: &Value) -> Vec<ActionField> {
    let mut fields = Vec::new();
    flatten_schema(schema, &mut Vec::new(), false, 0, &mut fields);
    fields
}

fn flatten_schema(
    schema: &Value,
    path: &mut Vec<String>,
    required: bool,
    depth: usize,
    fields: &mut Vec<ActionField>,
) {
    if !path.is_empty()
        && ["oneOf", "anyOf"].iter().any(|key| {
            schema
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|v| v.len() > 1)
        })
    {
        push_field(schema, path, required, fields);
        return;
    }
    let schema = select_schema_branch(schema);
    let properties = schema.get("properties").and_then(Value::as_object);
    if depth < MAX_SCHEMA_DEPTH {
        let Some(properties) = properties else {
            if !path.is_empty() {
                push_field(schema, path, required, fields);
            }
            return;
        };
        let required_names = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        for (name, child) in properties {
            path.push(name.clone());
            flatten_schema(
                child,
                path,
                required_names.contains(&name.as_str()),
                depth + 1,
                fields,
            );
            path.pop();
        }
        return;
    }
    if path.is_empty() {
        return;
    }
    push_field(schema, path, required, fields);
}

fn push_field(schema: &Value, path: &[String], required: bool, fields: &mut Vec<ActionField>) {
    let kind = schema_kind(schema);
    let label = path
        .last()
        .map(|value| humanize(value))
        .unwrap_or_else(|| "Value".to_owned());
    fields.push(ActionField {
        path: path.to_vec(),
        label,
        description: schema
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        required,
        kind,
        default: schema.get("default").cloned(),
        enum_values: schema
            .get("enum")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        schema: schema.clone(),
    });
}

fn select_schema_branch(schema: &Value) -> &Value {
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array)
            && let Some(branch) = branches.iter().find(|branch| {
                branch.get("type").and_then(Value::as_str) != Some("null")
                    && branch.get("const").is_none()
            })
        {
            return branch;
        }
    }
    schema
}

fn schema_kind(schema: &Value) -> FieldKind {
    // Draft-07 boolean schemas and keyword-free schemas accept any value;
    // only JSON input can express that (for example `arguments: true` on
    // `mcpServer/tool/call`).
    if schema_accepts_anything(schema) {
        return FieldKind::Json;
    }
    let kind = schema.get("type").and_then(|value| {
        value.as_str().or_else(|| {
            value
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .find(|kind| *kind != "null")
        })
    });
    match kind {
        Some("boolean") => FieldKind::Boolean,
        Some("integer") => FieldKind::Integer,
        Some("number") => FieldKind::Number,
        Some("array") => FieldKind::Array,
        Some("object") => FieldKind::Object,
        _ if schema_branches(schema).any(|branch| {
            branch.get("type").and_then(Value::as_str) == Some("object")
                || branch.get("properties").is_some()
        }) =>
        {
            FieldKind::Object
        }
        // Unions whose structure hides one level deeper (for example
        // `anyOf: [{oneOf: [tagged objects…]}, null]` on
        // `thread/settings/update`) cannot be expressed as key=value lines.
        _ if contains_structured_branch(schema, 0) => FieldKind::Json,
        _ => FieldKind::String,
    }
}

fn schema_accepts_anything(schema: &Value) -> bool {
    match schema {
        Value::Bool(accept) => *accept,
        Value::Object(object) => !object.keys().any(|key| {
            matches!(
                key.as_str(),
                "type"
                    | "properties"
                    | "enum"
                    | "const"
                    | "items"
                    | "oneOf"
                    | "anyOf"
                    | "allOf"
                    | "$ref"
                    | "required"
            )
        }),
        _ => false,
    }
}

/// True when any union branch, at any depth, is object- or array-shaped.
fn contains_structured_branch(schema: &Value, depth: usize) -> bool {
    if depth >= MAX_SCHEMA_DEPTH {
        return false;
    }
    if matches!(
        schema.get("type").and_then(Value::as_str),
        Some("object" | "array")
    ) || schema.get("properties").is_some()
        || schema.get("items").is_some()
    {
        return true;
    }
    ["oneOf", "anyOf", "allOf"]
        .iter()
        .filter_map(|key| schema.get(key).and_then(Value::as_array))
        .flatten()
        .any(|branch| contains_structured_branch(branch, depth + 1))
}

fn field_form_error(field: &ActionField) -> Option<&'static str> {
    if field.schema.get("$ref").is_some()
        && field.schema.get("type").is_none()
        && field.schema.get("properties").is_none()
        && !["oneOf", "anyOf", "allOf"]
            .iter()
            .any(|key| field.schema.get(key).is_some())
    {
        return Some("unresolved schema reference");
    }
    if field.kind == FieldKind::Array && field.schema.get("items").is_none() {
        return Some("array item schema missing");
    }
    None
}

fn parse_bool_token(raw: &str) -> Result<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => anyhow::bail!("expected true or false"),
    }
}

fn parse_field_value(field: &ActionField, raw: &str, dedicated: bool) -> Result<Value> {
    let value = match field.kind {
        FieldKind::String => Value::String(raw.to_owned()),
        FieldKind::Boolean => Value::Bool(parse_bool_token(raw)?),
        FieldKind::Integer => Value::Number(Number::from(raw.parse::<i64>()?)),
        FieldKind::Number => {
            let number = Number::from_f64(raw.parse::<f64>()?).context("number is not finite")?;
            Value::Number(number)
        }
        FieldKind::Array => {
            if dedicated && array_items_are_objects(&field.schema) {
                parse_typed_object_array(&field.schema, raw)?
            } else if raw.starts_with('[') && !dedicated {
                let parsed: Value = serde_json::from_str(raw)?;
                anyhow::ensure!(parsed.is_array(), "expected a JSON array");
                parsed
            } else {
                anyhow::ensure!(
                    !dedicated || !raw.starts_with('['),
                    "use comma or newline-separated values; raw JSON is disabled for dedicated actions"
                );
                Value::Array(
                    raw.split([',', '\n'])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(parse_loose_scalar)
                        .collect(),
                )
            }
        }
        FieldKind::Object => {
            if dedicated {
                anyhow::ensure!(
                    !raw.trim_start().starts_with('{'),
                    "use one key=value pair per line; raw JSON is disabled for dedicated actions"
                );
            }
            coerce_object_to_schema(parse_free_form_object(raw)?, &field.schema)?
        }
        // The schema itself is untyped or a structured union, so JSON is the
        // only faithful input; a non-JSON entry stays a plain string (covering
        // bare enum/const branches). Pre-dispatch validation still checks the
        // result against the live schema.
        FieldKind::Json => {
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
        }
    };
    if !field.enum_values.is_empty() {
        anyhow::ensure!(
            field.enum_values.contains(&value),
            "value must be one of {}",
            field
                .enum_values
                .iter()
                .map(compact_value)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(value)
}

pub fn parse_free_form_object(raw: &str) -> Result<Value> {
    if raw.trim_start().starts_with('{') {
        let parsed: Value = serde_json::from_str(raw)?;
        anyhow::ensure!(parsed.is_object(), "expected an object");
        return Ok(parsed);
    }
    let mut object = json!({});
    for entry in raw.split(['\n', ';']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (key, value) = entry
            .split_once('=')
            .with_context(|| format!("expected key=value, got {entry}"))?;
        let path = key
            .trim()
            .split('.')
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        anyhow::ensure!(!path.is_empty(), "object key cannot be empty");
        set_path(&mut object, &path, parse_loose_scalar(value.trim()));
    }
    anyhow::ensure!(
        object.as_object().is_some_and(|value| !value.is_empty()),
        "enter at least one key=value pair"
    );
    Ok(object)
}

fn parse_loose_scalar(raw: &str) -> Value {
    match raw.to_ascii_lowercase().as_str() {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if let Ok(integer) = raw.parse::<i64>() {
        return Value::Number(integer.into());
    }
    if let Ok(number) = raw.parse::<f64>()
        && let Some(number) = Number::from_f64(number)
    {
        return Value::Number(number);
    }
    Value::String(raw.to_owned())
}

fn set_path(root: &mut Value, path: &[String], value: Value) {
    if path.is_empty() {
        *root = value;
        return;
    }
    if !root.is_object() {
        *root = json!({});
    }
    let mut current = root;
    for segment in &path[..path.len() - 1] {
        let object = current.as_object_mut().expect("object created above");
        current = object.entry(segment.clone()).or_insert_with(|| json!({}));
        if !current.is_object() {
            *current = json!({});
        }
    }
    current
        .as_object_mut()
        .expect("object created above")
        .insert(path[path.len() - 1].clone(), value);
}

fn contains_unrestricted_setting(value: &Value) -> bool {
    match value {
        Value::String(value) => matches!(
            canonical_security_token(value).as_str(),
            "dangerfullaccess" | "unrestricted" | "bypass" | "never"
        ),
        Value::Array(values) => values.iter().any(contains_unrestricted_setting),
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                canonical_security_token(key).as_str(),
                "dangerfullaccess" | "unrestricted" | "bypassguardian"
            ) || contains_unrestricted_setting(value)
        }),
        _ => false,
    }
}

fn array_items_are_objects(schema: &Value) -> bool {
    let Some(items) = schema.get("items") else {
        return false;
    };
    schema_kind(items) == FieldKind::Object
        || ["oneOf", "anyOf"].iter().any(|key| {
            items
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|branches| {
                    branches
                        .iter()
                        .any(|branch| schema_kind(branch) == FieldKind::Object)
                })
        })
}

fn parse_typed_object_array(schema: &Value, raw: &str) -> Result<Value> {
    let items = schema.get("items").context("array item schema missing")?;
    let mut values = Vec::new();
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let object = if !line.contains('=') && user_input_text_branch(items).is_some() {
            let branch = user_input_text_branch(items).expect("checked above");
            let type_value = tagged_type_value(branch).unwrap_or_else(|| "text".to_owned());
            json!({"type": type_value, "text": line})
        } else if line.matches('=').count() == 1 && config_edit_shape(items) {
            let (key, value) = line.split_once('=').context("edit must be key=value")?;
            json!({
                "keyPath": key.trim(),
                "mergeStrategy": "replace",
                "value": parse_loose_scalar(value.trim())
            })
        } else {
            parse_record_line(line)?
        };
        values.push(coerce_object_to_schema(object, items)?);
    }
    Ok(Value::Array(values))
}

fn parse_record_line(line: &str) -> Result<Value> {
    let normalized = line.replace(';', "\n");
    parse_free_form_object(&normalized)
}

fn user_input_text_branch(schema: &Value) -> Option<&Value> {
    schema_branches(schema).find(|branch| {
        branch
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| {
                properties.contains_key("text") && properties.contains_key("type")
            })
    })
}

fn config_edit_shape(schema: &Value) -> bool {
    schema_branches(schema).any(|branch| {
        branch
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| {
                properties.contains_key("keyPath")
                    && properties.contains_key("mergeStrategy")
                    && properties.contains_key("value")
            })
    })
}

fn schema_branches(schema: &Value) -> impl Iterator<Item = &Value> {
    let branches = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array);
    branches
        .into_iter()
        .flatten()
        .chain(std::iter::once(schema))
}

/// Human hints for a union field: tagged-object type names plus bare
/// enum/const string branches, in schema order and deduplicated.
fn union_variant_names(schema: &Value) -> Vec<String> {
    fn collect(schema: &Value, depth: usize, names: &mut Vec<String>) {
        if depth >= MAX_SCHEMA_DEPTH {
            return;
        }
        if let Some(tag) = tagged_type_value(schema) {
            names.push(tag);
        } else if let Some(value) = schema.get("const").and_then(Value::as_str) {
            names.push(value.to_owned());
        } else if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            names.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        for key in ["oneOf", "anyOf"] {
            for branch in schema
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                collect(branch, depth + 1, names);
            }
        }
    }
    let mut names = Vec::new();
    collect(schema, 0, &mut names);
    let mut seen = std::collections::BTreeSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

fn tagged_type_value(schema: &Value) -> Option<String> {
    let type_schema = schema.get("properties")?.get("type")?;
    type_schema
        .get("const")
        .and_then(Value::as_str)
        .or_else(|| {
            type_schema
                .get("enum")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn coerce_object_to_schema(value: Value, schema: &Value) -> Result<Value> {
    let mut object = value
        .as_object()
        .cloned()
        .context("expected key=value object")?;
    let selected = select_object_branch(schema, &object);
    if let Some(properties) = selected.get("properties").and_then(Value::as_object) {
        for (key, value) in &mut object {
            if let Some(property) = properties.get(key) {
                *value = coerce_scalar_to_schema(value.clone(), property)?;
            }
        }
        if !object.contains_key("type")
            && let Some(type_value) = tagged_type_value(selected)
        {
            object.insert("type".to_owned(), Value::String(type_value));
        }
    }
    Ok(Value::Object(object))
}

fn select_object_branch<'a>(
    schema: &'a Value,
    object: &serde_json::Map<String, Value>,
) -> &'a Value {
    let requested = object.get("type").and_then(Value::as_str);
    schema_branches(schema)
        .find(|branch| {
            requested
                .is_some_and(|requested| tagged_type_value(branch).as_deref() == Some(requested))
        })
        .or_else(|| schema_branches(schema).find(|branch| schema_kind(branch) == FieldKind::Object))
        .unwrap_or(schema)
}

fn coerce_scalar_to_schema(value: Value, schema: &Value) -> Result<Value> {
    let Value::String(raw) = value else {
        return Ok(value);
    };
    Ok(match schema_kind(schema) {
        FieldKind::Boolean => Value::Bool(parse_bool_token(&raw)?),
        FieldKind::Integer => Value::Number(Number::from(raw.parse::<i64>()?)),
        FieldKind::Number => {
            Value::Number(Number::from_f64(raw.parse::<f64>()?).context("number is not finite")?)
        }
        _ => Value::String(raw),
    })
}

fn canonical_security_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn compact_value(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn humanize(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 8);
    for (index, character) in value.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            output.push(' ');
        }
        output.push(character);
    }
    let mut chars = output.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX_0_145_0_CLIENT_METHODS: &[&str] = &[
        "account/login/cancel",
        "account/login/start",
        "account/logout",
        "account/rateLimitResetCredit/consume",
        "account/rateLimits/read",
        "account/read",
        "account/sendAddCreditsNudgeEmail",
        "account/usage/read",
        "account/workspaceMessages/read",
        "app/list",
        "collaborationMode/list",
        "command/exec",
        "command/exec/resize",
        "command/exec/terminate",
        "command/exec/write",
        "config/batchWrite",
        "config/mcpServer/reload",
        "config/read",
        "config/value/write",
        "configRequirements/read",
        "environment/add",
        "environment/info",
        "environment/status",
        "experimentalFeature/enablement/set",
        "experimentalFeature/list",
        "externalAgentConfig/detect",
        "externalAgentConfig/import",
        "externalAgentConfig/import/readHistories",
        "feedback/upload",
        "fs/copy",
        "fs/createDirectory",
        "fs/getMetadata",
        "fs/readDirectory",
        "fs/readFile",
        "fs/remove",
        "fs/unwatch",
        "fs/watch",
        "fs/writeFile",
        "fuzzyFileSearch",
        "fuzzyFileSearch/sessionStart",
        "fuzzyFileSearch/sessionStop",
        "fuzzyFileSearch/sessionUpdate",
        "hooks/list",
        "initialize",
        "marketplace/add",
        "marketplace/remove",
        "marketplace/upgrade",
        "mcpServer/oauth/login",
        "mcpServer/resource/read",
        "mcpServer/tool/call",
        "mcpServerStatus/list",
        "memory/reset",
        "mock/experimentalMethod",
        "model/list",
        "modelProvider/capabilities/read",
        "permissionProfile/list",
        "plugin/install",
        "plugin/installed",
        "plugin/list",
        "plugin/read",
        "plugin/share/checkout",
        "plugin/share/delete",
        "plugin/share/list",
        "plugin/share/save",
        "plugin/share/updateTargets",
        "plugin/skill/read",
        "plugin/uninstall",
        "process/kill",
        "process/resizePty",
        "process/spawn",
        "process/writeStdin",
        "remoteControl/client/list",
        "remoteControl/client/revoke",
        "remoteControl/disable",
        "remoteControl/enable",
        "remoteControl/pairing/start",
        "remoteControl/pairing/status",
        "remoteControl/status/read",
        "review/start",
        "skills/config/write",
        "skills/extraRoots/set",
        "skills/list",
        "thread/approveGuardianDeniedAction",
        "thread/archive",
        "thread/backgroundTerminals/clean",
        "thread/backgroundTerminals/list",
        "thread/backgroundTerminals/terminate",
        "thread/compact/start",
        "thread/decrement_elicitation",
        "thread/delete",
        "thread/fork",
        "thread/goal/clear",
        "thread/goal/get",
        "thread/goal/set",
        "thread/increment_elicitation",
        "thread/inject_items",
        "thread/items/list",
        "thread/list",
        "thread/loaded/list",
        "thread/memoryMode/set",
        "thread/metadata/update",
        "thread/name/set",
        "thread/read",
        "thread/realtime/appendAudio",
        "thread/realtime/appendSpeech",
        "thread/realtime/appendText",
        "thread/realtime/listVoices",
        "thread/realtime/start",
        "thread/realtime/stop",
        "thread/resume",
        "thread/rollback",
        "thread/search",
        "thread/settings/update",
        "thread/shellCommand",
        "thread/start",
        "thread/turns/list",
        "thread/unarchive",
        "thread/unsubscribe",
        "turn/interrupt",
        "turn/start",
        "turn/steer",
        "windowsSandbox/readiness",
        "windowsSandbox/setupStart",
    ];

    #[test]
    fn registry_is_unique_reachable_and_curated() {
        let mut methods = std::collections::BTreeSet::new();
        for spec in DEDICATED_ACTIONS {
            assert!(methods.insert(spec.method), "duplicate {}", spec.method);
            assert_eq!(dedicated_action(spec.method), Some(spec));
            assert_eq!(method_category(spec.method), spec.surface.slug());
            assert!(!spec.label.trim().is_empty());
            assert!(!spec.description.trim().is_empty());
            if capability_requires_god(spec.method) {
                assert_eq!(
                    spec.authorization,
                    ActionAuthorization::God,
                    "{} registry authorization is weaker than central GOD policy",
                    spec.method
                );
                assert_eq!(
                    spec.confirmation,
                    ConfirmationPolicy::Danger,
                    "{} registry confirmation is weaker than central GOD policy",
                    spec.method
                );
            }
        }
        assert_eq!(methods.len(), 118);
    }

    #[test]
    fn pinned_0_145_0_schema_has_exact_dedicated_accounting() {
        assert_eq!(CODEX_0_145_0_CLIENT_METHODS.len(), 123);
        let mut methods = std::collections::BTreeSet::new();
        let mut dedicated = 0;
        let mut exempt = 0;
        for method in CODEX_0_145_0_CLIENT_METHODS {
            assert!(methods.insert(*method), "duplicate pinned method {method}");
            match (dedicated_action(method), action_exemption(method)) {
                (Some(_), None) => dedicated += 1,
                (None, Some(entry)) => {
                    exempt += 1;
                    assert!(!entry.reason.trim().is_empty(), "{method}");
                }
                pair => panic!("{method} has invalid registry accounting: {pair:?}"),
            }
        }
        assert_eq!(dedicated, 118);
        assert_eq!(exempt, 5);
        assert_eq!(dedicated * 100 / (methods.len() - exempt), 100);
    }

    #[test]
    fn dangerous_invocation_contract_is_exact() {
        let capability = MethodCapability {
            method: "command/exec".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({
                "type":"object",
                "required":["command"],
                "properties":{"command":{"type":"string"}}
            })),
        };
        let mut draft = ActionDraft::from_capability(&capability, None, 7, None);
        draft
            .apply_page_values(0, &BTreeMap::from([(0, "whoami".to_owned())]))
            .unwrap();
        assert_eq!(draft.method, "command/exec");
        assert_eq!(draft.params, json!({"command":"whoami"}));
        assert_eq!(draft.authorization, ActionAuthorization::God);
        assert_eq!(draft.confirmation, ConfirmationPolicy::Danger);
        assert_eq!(draft.renderer, ActionRenderer::Process);
        assert!(draft.dedicated);
    }

    #[test]
    fn risk_policy_separates_read_owner_and_god_actions() {
        assert_eq!(
            dedicated_action("fs/readFile").unwrap().authorization,
            ActionAuthorization::God
        );
        assert_eq!(
            dedicated_action("fs/writeFile").unwrap().authorization,
            ActionAuthorization::God
        );
        for method in [
            "command/exec",
            "process/spawn",
            "thread/shellCommand",
            "thread/approveGuardianDeniedAction",
            "thread/inject_items",
            "externalAgentConfig/import/readHistories",
        ] {
            let spec = dedicated_action(method).unwrap();
            assert_eq!(spec.authorization, ActionAuthorization::God, "{method}");
            assert_eq!(spec.confirmation, ConfirmationPolicy::Danger, "{method}");
        }
    }

    #[test]
    fn equivalent_local_device_capabilities_fail_closed_to_god() {
        for method in [
            "fs/readFile",
            "fs/writeFile",
            "fs/remove",
            "mcpServer/tool/call",
            "remoteControl/enable",
            "remoteControl/pairing/start",
            "config/value/write",
            "config/futureMutation",
            "environment/add",
            "experimentalFeature/enablement/set",
            "externalAgentConfig/import",
            "marketplace/add",
            "marketplace/futureMutation",
            "plugin/install",
            "plugin/futureMutation",
            "skills/extraRoots/set",
            "skills/futureMutation",
        ] {
            assert!(
                capability_requires_god(method),
                "privileged capability {method} escaped GOD authorization"
            );
            if let Some(spec) = dedicated_action(method) {
                let capability = MethodCapability {
                    method: method.to_owned(),
                    description: None,
                    title: None,
                    params_schema: Some(json!({"type":"object"})),
                };
                let draft = ActionDraft::from_capability(&capability, Some("task".into()), 7, None);
                assert_eq!(
                    draft.authorization,
                    ActionAuthorization::God,
                    "{}",
                    spec.method
                );
                assert_eq!(
                    draft.confirmation,
                    ConfirmationPolicy::Danger,
                    "{}",
                    spec.method
                );
            }
        }

        for method in [
            "config/read",
            "plugin/list",
            "plugin/read",
            "plugin/share/list",
            "plugin/skill/read",
            "skills/list",
            "remoteControl/pairing/status",
            "remoteControl/disable",
            "mcpServer/resource/read",
        ] {
            assert!(
                !capability_requires_god(method),
                "read-only or risk-reducing capability {method} was over-gated"
            );
        }
    }

    #[test]
    fn unrestricted_typed_parameters_escalate_to_god() {
        let draft = |params| ActionDraft {
            method: "thread/settings/update".to_owned(),
            label: "Update task settings".to_owned(),
            description: None,
            fields: Vec::new(),
            params,
            page: 0,
            risk: ActionRisk::Mutating,
            authorization: ActionAuthorization::Owner,
            confirmation: ConfirmationPolicy::Required,
            renderer: ActionRenderer::Task,
            dedicated: true,
            task_id: Some("thread-1".to_owned()),
            channel_id: 1,
            created_at: Utc::now(),
        };
        let safe =
            draft(json!({"sandboxPolicy":{"type":"workspaceWrite"},"approvalPolicy":"on-request"}));
        let safe_policy = safe.effective_policy();
        assert_eq!(safe_policy.authorization, ActionAuthorization::Owner);
        assert_eq!(safe_policy.confirmation, ConfirmationPolicy::Required);
        assert_eq!(
            safe_policy.authorization.requirement_label(),
            "Configured owner confirmation"
        );
        let unrestricted = draft(json!({"sandboxPolicy":{"type":"dangerFullAccess"}}));
        let unrestricted_policy = unrestricted.effective_policy();
        assert_eq!(unrestricted_policy.authorization, ActionAuthorization::God);
        assert_eq!(unrestricted_policy.confirmation, ConfirmationPolicy::Danger);
        assert_eq!(
            unrestricted_policy.authorization.risk_label(),
            "High risk — active task-scoped GOD mode required"
        );
        let bypass = draft(json!({"approvalPolicy":"never"}));
        assert_eq!(
            bypass.effective_policy().authorization,
            ActionAuthorization::God
        );
        for spelling in [
            "danger-full-access",
            "danger_full_access",
            "dangerFullAccess",
            "DANGER FULL ACCESS",
        ] {
            let unrestricted = draft(json!({"sandbox": spelling}));
            assert_eq!(
                unrestricted.effective_policy().authorization,
                ActionAuthorization::God,
                "unrestricted spelling {spelling:?} bypassed GOD authorization"
            );
        }
    }

    #[test]
    fn action_rendering_policy_has_one_stable_mapping() {
        assert_eq!(ActionRenderer::Process.result_title(), "Process result");
        assert_eq!(ActionRenderer::Task.result_title(), "Task result");
        assert_eq!(ConfirmationPolicy::Danger.color(), 0xED_4245);
        assert_eq!(ConfirmationPolicy::Danger.audit_label(), "danger");
        assert_eq!(ActionAuthorization::God.audit_label(), "god");
        assert_eq!(ActionRisk::ReadOnly.audit_label(), "ReadOnly");
        assert_eq!(ActionRisk::Mutating.audit_label(), "Mutating");
    }

    #[test]
    fn nullable_scalar_schema_uses_the_non_null_control_type() {
        let fields = fields_from_schema(&json!({
            "type":"object",
            "properties": {
                "enabled":{"type":["boolean", "null"]},
                "limit":{"type":["integer", "null"]},
                "label":{"type":["string", "null"]}
            }
        }));
        assert_eq!(
            fields
                .iter()
                .map(|field| (field.key(), field.kind))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("enabled".to_owned(), FieldKind::Boolean),
                ("limit".to_owned(), FieldKind::Integer),
                ("label".to_owned(), FieldKind::String)
            ])
        );
    }

    #[test]
    fn turn_input_lines_synthesize_typed_user_input_objects() {
        let capability = MethodCapability {
            method: "turn/start".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({
                "type":"object",
                "required":["threadId", "input"],
                "properties": {
                    "threadId":{"type":"string"},
                    "input": {
                        "type":"array",
                        "items": {
                            "oneOf":[{
                                "type":"object",
                                "required":["type", "text"],
                                "properties": {
                                    "type":{"type":"string", "enum":["text"]},
                                    "text":{"type":"string"}
                                }
                            }]
                        }
                    }
                }
            })),
        };
        let mut draft = ActionDraft::from_capability(&capability, Some("task-1".into()), 7, None);
        let input_index = draft
            .fields
            .iter()
            .position(|field| field.key() == "input")
            .unwrap();
        draft
            .apply_page_values(
                0,
                &BTreeMap::from([(input_index, "First line\nSecond line".to_owned())]),
            )
            .unwrap();
        assert_eq!(
            draft.params,
            json!({
                "threadId":"task-1",
                "input":[
                    {"type":"text", "text":"First line"},
                    {"type":"text", "text":"Second line"}
                ]
            })
        );
        assert!(
            crate::codex::params::validate(
                capability.params_schema.as_ref().unwrap(),
                &draft.params
            )
            .is_empty()
        );
    }

    #[test]
    fn config_batch_lines_synthesize_typed_edits() {
        let capability = MethodCapability {
            method: "config/batchWrite".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({
                "type":"object",
                "required":["edits"],
                "properties": {
                    "edits": {
                        "type":"array",
                        "items": {
                            "type":"object",
                            "required":["keyPath", "mergeStrategy", "value"],
                            "properties": {
                                "keyPath":{"type":"string"},
                                "mergeStrategy":{"type":"string", "enum":["replace", "upsert"]},
                                "value":true
                            }
                        }
                    }
                }
            })),
        };
        let mut draft = ActionDraft::from_capability(&capability, None, 7, None);
        draft
            .apply_page_values(
                0,
                &BTreeMap::from([(0, "model=gpt-5\nfeatures.fast=true".to_owned())]),
            )
            .unwrap();
        assert_eq!(
            draft.params,
            json!({"edits":[
                {"keyPath":"model", "mergeStrategy":"replace", "value":"gpt-5"},
                {"keyPath":"features.fast", "mergeStrategy":"replace", "value":true}
            ]})
        );
        assert!(
            crate::codex::params::validate(
                capability.params_schema.as_ref().unwrap(),
                &draft.params
            )
            .is_empty()
        );
    }

    #[test]
    fn tagged_union_is_one_typed_record_with_explicit_variant_help() {
        let fields = fields_from_schema(&json!({
            "type":"object",
            "required":["target"],
            "properties": {
                "target": {
                    "oneOf":[
                        {
                            "type":"object",
                            "required":["type"],
                            "properties":{"type":{"type":"string", "enum":["uncommittedChanges"]}}
                        },
                        {
                            "type":"object",
                            "required":["type", "branch"],
                            "properties":{
                                "type":{"type":"string", "enum":["baseBranch"]},
                                "branch":{"type":"string"}
                            }
                        }
                    ]
                }
            }
        }));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].key(), "target");
        assert_eq!(fields[0].kind, FieldKind::Object);
        assert!(fields[0].placeholder().contains("uncommittedChanges"));
        assert!(fields[0].placeholder().contains("baseBranch"));
    }

    #[test]
    fn untyped_tool_arguments_accept_structured_json() {
        // Mirrors the installed `mcpServer/tool/call` schema, where
        // `arguments` is the draft-07 boolean schema `true`.
        let capability = MethodCapability {
            method: "mcpServer/tool/call".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({
                "type":"object",
                "required":["server", "tool"],
                "properties":{
                    "server":{"type":"string"},
                    "tool":{"type":"string"},
                    "arguments": true
                }
            })),
        };
        let mut draft = ActionDraft::from_capability(&capability, None, 7, None);
        let arguments = draft
            .fields
            .iter()
            .position(|field| field.key() == "arguments")
            .unwrap();
        assert_eq!(draft.fields[arguments].kind, FieldKind::Json);
        draft
            .apply_page_values(
                0,
                &BTreeMap::from([
                    (arguments, r#"{"path":"src/main.rs","line":3}"#.to_owned()),
                    (
                        draft
                            .fields
                            .iter()
                            .position(|field| field.key() == "server")
                            .unwrap(),
                        "docs".to_owned(),
                    ),
                    (
                        draft
                            .fields
                            .iter()
                            .position(|field| field.key() == "tool")
                            .unwrap(),
                        "search".to_owned(),
                    ),
                ]),
            )
            .unwrap();
        assert_eq!(
            draft.params,
            json!({
                "server":"docs",
                "tool":"search",
                "arguments":{"path":"src/main.rs","line":3}
            })
        );
        assert!(
            crate::codex::params::validate(
                capability.params_schema.as_ref().unwrap(),
                &draft.params
            )
            .is_empty()
        );
    }

    #[test]
    fn structured_policy_unions_accept_json_and_keep_bare_strings() {
        // Mirrors the installed `thread/settings/update` sandboxPolicy shape:
        // an optional union of tagged object variants.
        let sandbox_policy = json!({
            "anyOf":[
                {
                    "oneOf":[
                        {
                            "type":"object",
                            "required":["type"],
                            "properties":{"type":{"type":"string","enum":["dangerFullAccess"]}}
                        },
                        {
                            "type":"object",
                            "required":["type"],
                            "properties":{
                                "type":{"type":"string","enum":["workspaceWrite"]},
                                "networkAccess":{"type":"boolean"}
                            }
                        }
                    ]
                },
                {"type":"null"}
            ]
        });
        let fields = fields_from_schema(&json!({
            "type":"object",
            "required":["threadId"],
            "properties":{
                "threadId":{"type":"string"},
                "sandboxPolicy": sandbox_policy
            }
        }));
        let field = fields
            .iter()
            .find(|field| field.key() == "sandboxPolicy")
            .unwrap();
        assert_eq!(field.kind, FieldKind::Json);
        assert!(field.placeholder().contains("dangerFullAccess"));
        assert!(field.placeholder().contains("workspaceWrite"));
        let value = parse_field_value(
            field,
            r#"{"type":"workspaceWrite","networkAccess":true}"#,
            true,
        )
        .unwrap();
        assert_eq!(value, json!({"type":"workspaceWrite","networkAccess":true}));
        assert!(
            crate::codex::params::validate(&field.schema, &value).is_empty(),
            "typed JSON entry must satisfy the union schema"
        );
        // Bare text stays a string so const/enum branches remain reachable.
        assert_eq!(
            parse_field_value(field, "on-request", true).unwrap(),
            json!("on-request")
        );
    }

    #[test]
    fn json_fields_still_escalate_unrestricted_settings_to_god() {
        let capability = MethodCapability {
            method: "thread/settings/update".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({
                "type":"object",
                "properties":{
                    "threadId":{"type":"string"},
                    "sandboxPolicy":{"anyOf":[
                        {"oneOf":[{"type":"object","required":["type"],
                            "properties":{"type":{"type":"string","enum":["dangerFullAccess"]}}}]},
                        {"type":"null"}
                    ]}
                }
            })),
        };
        let mut draft = ActionDraft::from_capability(&capability, Some("task-9".into()), 7, None);
        let index = draft
            .fields
            .iter()
            .position(|field| field.key() == "sandboxPolicy")
            .unwrap();
        draft
            .apply_page_values(
                0,
                &BTreeMap::from([(index, r#"{"type":"dangerFullAccess"}"#.to_owned())]),
            )
            .unwrap();
        assert_eq!(
            draft.effective_policy().authorization,
            ActionAuthorization::God
        );
    }

    #[test]
    fn flattens_nested_schema_and_marks_required_fields() {
        let fields = fields_from_schema(&json!({
            "type":"object",
            "required":["threadId", "config"],
            "properties":{
                "threadId":{"type":"string"},
                "config":{
                    "type":"object",
                    "required":["enabled"],
                    "properties":{"enabled":{"type":"boolean"}}
                }
            }
        }));
        assert_eq!(fields.len(), 2);
        let thread_id = fields
            .iter()
            .find(|field| field.key() == "threadId")
            .unwrap();
        let enabled = fields
            .iter()
            .find(|field| field.key() == "config.enabled")
            .unwrap();
        assert!(thread_id.required);
        assert!(enabled.required);
    }

    #[test]
    fn injects_task_id_and_parses_scalar_values() {
        let capability = MethodCapability {
            method: "thread/settings/update".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({
                "type":"object",
                "required":["threadId", "enabled"],
                "properties":{
                    "threadId":{"type":"string"},
                    "enabled":{"type":"boolean"},
                    "count":{"type":"integer"}
                }
            })),
        };
        let mut draft = ActionDraft::from_capability(&capability, Some("task-1".into()), 7, None);
        assert_eq!(draft.fields.len(), 2);
        let values = draft
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                (
                    index,
                    if field.key() == "enabled" { "yes" } else { "4" }.to_owned(),
                )
            })
            .collect();
        draft.apply_page_values(0, &values).unwrap();
        assert_eq!(
            draft.params,
            json!({"threadId":"task-1", "enabled":true, "count":4})
        );
    }

    #[test]
    fn risk_classifier_fails_closed_for_unknown_mutations() {
        assert_eq!(classify_risk("config/read"), ActionRisk::ReadOnly);
        assert_eq!(classify_risk("thread/delete"), ActionRisk::Mutating);
        assert_eq!(classify_risk("future/newMethod"), ActionRisk::Mutating);
    }

    #[test]
    fn preview_redacts_nested_secrets() {
        let capability = MethodCapability {
            method: "config/value/write".to_owned(),
            description: None,
            title: None,
            params_schema: None,
        };
        let mut draft = ActionDraft::from_capability(&capability, None, 7, None);
        draft.params = json!({"nested":{"apiToken":"abc"},"safe":"ok"});
        assert_eq!(
            draft.redacted_preview(),
            json!({"nested":{"apiToken":"[REDACTED]"},"safe":"ok"})
        );
    }

    #[test]
    fn null_parameter_methods_emit_json_null() {
        let capability = MethodCapability {
            method: "account/logout".to_owned(),
            description: None,
            title: None,
            params_schema: Some(json!({"type":"null"})),
        };
        let draft = ActionDraft::from_capability(&capability, None, 7, None);
        assert!(draft.fields.is_empty());
        assert_eq!(draft.params, Value::Null);
    }

    #[test]
    fn free_form_objects_do_not_require_json() {
        let field = ActionField {
            path: vec!["config".into()],
            label: "Config".into(),
            description: None,
            required: true,
            kind: FieldKind::Object,
            default: None,
            enum_values: Vec::new(),
            schema: json!({"type":"object"}),
        };
        assert_eq!(
            parse_field_value(&field, "enabled=true\nretries=3\nnested.name=relay", false,)
                .unwrap(),
            json!({"enabled":true,"retries":3,"nested":{"name":"relay"}})
        );
    }

    #[test]
    fn dedicated_forms_reject_raw_json_inputs() {
        let object = ActionField {
            path: vec!["config".into()],
            label: "Config".into(),
            description: None,
            required: true,
            kind: FieldKind::Object,
            default: None,
            enum_values: Vec::new(),
            schema: json!({"type":"object"}),
        };
        assert!(parse_field_value(&object, r#"{"enabled":true}"#, true).is_err());
        let array = ActionField {
            kind: FieldKind::Array,
            schema: json!({"type":"array","items":{"type":"string"}}),
            ..object
        };
        assert!(parse_field_value(&array, r#"["one","two"]"#, true).is_err());
        assert_eq!(
            parse_field_value(&array, "one, two", true).unwrap(),
            json!(["one", "two"])
        );
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_schema_compiles_all_121_interactive_actions() {
        use crate::codex::{CapabilityCatalog, CodexCommand};

        let output = tempfile::tempdir().unwrap();
        let catalog =
            CapabilityCatalog::generate_and_load(&CodexCommand::discover().unwrap(), output.path())
                .await
                .unwrap();
        // Totals drift with the auto-updating installed bundle; the invariant
        // is that EVERY discovered interactive method compiles into a valid
        // paginated form and null-root schemas send `params: null`.
        assert!(catalog.client_requests.len() >= 100);
        let interactive = catalog
            .client_request_metadata
            .values()
            .filter(|capability| action_exemption(&capability.method).is_none())
            .collect::<Vec<_>>();
        assert_eq!(interactive.len(), catalog.client_requests.len() - 5);
        let mut null_roots = 0;
        for capability in interactive {
            assert!(
                dedicated_action(&capability.method).is_some(),
                "live method lacks dedicated spec: {}",
                capability.method
            );
            let draft = ActionDraft::from_capability(capability, Some("task".into()), 7, None);
            for page in 0..draft.page_count() {
                assert!(draft.page_fields(page).len() <= FIELDS_PER_PAGE);
            }
            if capability
                .params_schema
                .as_ref()
                .and_then(|schema| schema.get("type"))
                .and_then(Value::as_str)
                == Some("null")
            {
                null_roots += 1;
                assert_eq!(draft.params, Value::Null);
            }

            if capability.method.starts_with("fs/")
                || capability.method.starts_with("config/") && capability.method != "config/read"
                || capability.method.starts_with("marketplace/")
                || capability.method.starts_with("plugin/")
                    && !matches!(
                        capability.method.as_str(),
                        "plugin/installed"
                            | "plugin/list"
                            | "plugin/read"
                            | "plugin/share/list"
                            | "plugin/skill/read"
                    )
            {
                assert!(
                    capability_requires_god(&capability.method),
                    "live privileged method escaped GOD policy: {}",
                    capability.method
                );
            }
        }
        assert!(null_roots >= 5, "found {null_roots} null-root methods");
    }
}
