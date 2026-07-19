//! Truthful connector-health model for Codex apps, plugins, and MCP servers.
//!
//! Wire sources, verified against the installed app-server schema dump, a live
//! Codex app-server, and the official openai/codex source:
//!
//! - `app/list` (`AppInfo`) — the ChatGPT app directory. `isAccessible`
//!   (schema default `false`) is the app's *current accessibility* for this
//!   account; the official TUI labels `true` as "Installed" and `false` as
//!   "Can be installed". It is not a generic connected/disconnected flag.
//!   `isEnabled` (schema default `true`) mirrors `[apps.X] enabled` in
//!   config.toml; `installUrl` is the chatgpt.com page that installs the app;
//!   `pluginDisplayNames` lists local plugins projected into the entry.
//! - `plugin/installed` / `plugin/list` (`PluginSummary`) — local plugin
//!   state. `installed` and `enabled` are required booleans; `availability`
//!   defaults to `AVAILABLE` and may be `DISABLED_BY_ADMIN`; `installPolicy`
//!   may be `NOT_AVAILABLE`; `authPolicy` (`ON_INSTALL`/`ON_USE`) says *when*
//!   auth runs. None of these prove a credential is valid or that the
//!   product is currently usable.
//! - `mcpServerStatus/list` (`McpServerStatus`) — runtime auth truth.
//!   `authStatus` is required and one of `unsupported`, `notLoggedIn`,
//!   `bearerToken`, `oAuth`; `tools` is the live mounted tool inventory.
//!
//! A single product ("Gmail") can appear both as a directory app entry and as
//! an installed plugin that drives that app. Those are distinct facts and are
//! rendered as such; a plugin being installed does not make the app
//! accessible, and only app accessibility or an actual mounted tool inventory
//! proves usability. For an email send, a mounted MCP route must expose a
//! Gmail-scoped send tool; read/search tools alone are not send capability.
//! Every [`ConnectorHealth`] variant maps to an observed
//! field value, a schema-documented default, or an explicit unknown — never a
//! guess.

use serde_json::Value;

/// Health of one connector surface, derived from observed wire fields only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorHealth {
    /// App `isAccessible == true`. The official TUI shows this as
    /// "Installed".
    Accessible,
    /// MCP `authStatus` is `bearerToken`/`oAuth`.
    Authenticated,
    /// Negative auth signal: MCP `authStatus == notLoggedIn`.
    AuthRequired,
    /// App `isAccessible` is `false` (or absent — the schema default). The
    /// official TUI shows this as "Can be installed"; it is *not* proof that
    /// a plugin-based integration is missing.
    CanInstall,
    /// Plugin `installed == true` and enabled — a local install fact only.
    /// Plugin auth validity and product usability are not observable from
    /// `plugin/installed`, so nothing stronger is claimed.
    Installed,
    /// Plugin present in a marketplace catalog but `installed == false`.
    Available,
    /// Explicitly disabled: app `isEnabled == false` or plugin
    /// `enabled == false`.
    Disabled,
    /// Plugin `availability == DISABLED_BY_ADMIN`.
    AdminDisabled,
    /// Plugin `installPolicy == NOT_AVAILABLE`.
    Unavailable,
    /// MCP `authStatus == unsupported`: the server has no OAuth support. Not
    /// the same as "no credential needed" — it may authenticate out of band.
    AuthUnsupported,
    /// A schema-required field was missing, so no state can be asserted.
    Unknown,
}

impl ConnectorHealth {
    #[must_use]
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Accessible | Self::Authenticated => "🟢",
            Self::AuthRequired => "🔑",
            Self::CanInstall => "⚪",
            Self::Installed => "🧩",
            Self::Available => "⬇",
            Self::Disabled | Self::AdminDisabled => "⛔",
            Self::Unavailable => "🚫",
            Self::AuthUnsupported => "➖",
            Self::Unknown => "❔",
        }
    }

    /// User-facing state, aligned with the official TUI's app wording.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Accessible | Self::Installed => "installed",
            Self::Authenticated => "authenticated",
            Self::AuthRequired => "sign-in required",
            Self::CanInstall => "can be installed",
            Self::Available => "available",
            Self::Disabled => "disabled",
            Self::AdminDisabled => "disabled by admin",
            Self::Unavailable => "unavailable",
            Self::AuthUnsupported => "no OAuth support",
            Self::Unknown => "state unknown",
        }
    }

    /// Actionable states sort first so operators see problems before wins.
    #[must_use]
    pub fn sort_rank(self) -> u8 {
        match self {
            Self::AuthRequired => 0,
            Self::CanInstall => 1,
            Self::Disabled | Self::AdminDisabled | Self::Unavailable => 2,
            Self::Accessible | Self::Authenticated => 3,
            Self::Installed => 4,
            Self::AuthUnsupported => 5,
            Self::Available => 6,
            Self::Unknown => 7,
        }
    }
}

/// The fields of an `app/list` `AppInfo` entry this relay reasons about.
///
/// Booleans stay `Option` so "absent on the wire" remains distinguishable from
/// an explicit value; health derivation applies the schema defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRecord {
    pub id: String,
    pub name: String,
    pub is_accessible: Option<bool>,
    pub is_enabled: Option<bool>,
    pub install_url: Option<String>,
    pub plugin_display_names: Vec<String>,
}

impl AppRecord {
    /// Extracts the record; returns `None` when the required `id`/`name`
    /// fields are missing (such an entry cannot be addressed or rendered).
    #[must_use]
    pub fn from_value(app: &Value) -> Option<Self> {
        let id = app.get("id").and_then(Value::as_str)?;
        let name = app.get("name").and_then(Value::as_str)?;
        Some(Self {
            id: id.to_owned(),
            name: name.to_owned(),
            is_accessible: app.get("isAccessible").and_then(Value::as_bool),
            is_enabled: app.get("isEnabled").and_then(Value::as_bool),
            install_url: app
                .get("installUrl")
                .and_then(Value::as_str)
                .map(str::to_owned),
            plugin_display_names: app
                .get("pluginDisplayNames")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        })
    }
}

/// Health of an app directory entry.
///
/// Schema defaults are applied where the wire omits a field: `isEnabled`
/// defaults to `true`, `isAccessible` defaults to `false`.
#[must_use]
pub fn app_health(app: &AppRecord) -> ConnectorHealth {
    if !app.is_enabled.unwrap_or(true) {
        return ConnectorHealth::Disabled;
    }
    if app.is_accessible.unwrap_or(false) {
        return ConnectorHealth::Accessible;
    }
    ConnectorHealth::CanInstall
}

/// The fields of a `PluginSummary` this relay reasons about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRecord {
    pub id: String,
    pub name: String,
    /// `interface.displayName` when the plugin publishes one (e.g. "Gmail").
    pub display_name: Option<String>,
    pub marketplace: String,
    /// Required by the schema; `None` means the server broke contract.
    pub installed: Option<bool>,
    /// Required by the schema; `None` means the server broke contract.
    pub enabled: Option<bool>,
    pub availability: Option<String>,
    pub install_policy: Option<String>,
    pub auth_policy: Option<String>,
    pub version: Option<String>,
}

impl PluginRecord {
    #[must_use]
    pub fn from_value(plugin: &Value, marketplace: &str) -> Option<Self> {
        let name = plugin
            .get("name")
            .or_else(|| plugin.get("id"))
            .and_then(Value::as_str)?;
        let id = plugin
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_owned();
        let string = |key: &str| plugin.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            id,
            name: name.to_owned(),
            display_name: plugin
                .pointer("/interface/displayName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            marketplace: marketplace.to_owned(),
            installed: plugin.get("installed").and_then(Value::as_bool),
            enabled: plugin.get("enabled").and_then(Value::as_bool),
            availability: string("availability"),
            install_policy: string("installPolicy"),
            auth_policy: string("authPolicy"),
            version: plugin
                .get("localVersion")
                .or_else(|| plugin.get("version"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// The name users see: `interface.displayName` when present, else `name`.
    #[must_use]
    pub fn shown_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    /// Flattens `plugin/installed` / `plugin/list` responses into records.
    #[must_use]
    pub fn from_marketplaces(response: &Value) -> Vec<Self> {
        let root = response.get("data").unwrap_or(response);
        root.get("marketplaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|marketplace| {
                let name = marketplace
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown marketplace");
                marketplace
                    .get("plugins")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(move |plugin| Self::from_value(plugin, name))
            })
            .collect()
    }
}

/// Health of a plugin entry. Precedence: admin/policy blocks, then the
/// explicit `enabled` flag, then installedness. Missing schema-required
/// fields yield [`ConnectorHealth::Unknown`] rather than a guess.
#[must_use]
pub fn plugin_health(plugin: &PluginRecord) -> ConnectorHealth {
    if plugin.availability.as_deref() == Some("DISABLED_BY_ADMIN") {
        return ConnectorHealth::AdminDisabled;
    }
    if plugin.install_policy.as_deref() == Some("NOT_AVAILABLE") {
        return ConnectorHealth::Unavailable;
    }
    match (plugin.installed, plugin.enabled) {
        (_, Some(false)) => ConnectorHealth::Disabled,
        (Some(true), Some(true)) => ConnectorHealth::Installed,
        (Some(false), Some(true)) => ConnectorHealth::Available,
        _ => ConnectorHealth::Unknown,
    }
}

/// Human phrasing for `authPolicy`; states when auth runs, not its validity.
#[must_use]
pub fn auth_policy_note(auth_policy: Option<&str>) -> Option<&'static str> {
    match auth_policy {
        Some("ON_INSTALL") => Some("auth at install"),
        Some("ON_USE") => Some("auth on first use"),
        _ => None,
    }
}

/// The fields of a `McpServerStatus` entry this relay reasons about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRecord {
    pub name: String,
    /// Required by the schema; `None` means the server broke contract.
    pub auth_status: Option<String>,
    /// Names of the currently mounted tools (map keys, or `name` fields for
    /// array-shaped inventories).
    pub tool_names: Vec<String>,
    pub tool_count: usize,
}

impl McpRecord {
    #[must_use]
    pub fn from_value(server: &Value) -> Option<Self> {
        let name = server.get("name").and_then(Value::as_str)?;
        let (tool_names, tool_count) = match server.get("tools") {
            Some(Value::Object(map)) => (map.keys().cloned().collect(), map.len()),
            Some(Value::Array(items)) => (
                items
                    .iter()
                    .filter_map(|item| item.get("name").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect(),
                items.len(),
            ),
            _ => (Vec::new(), 0),
        };
        Some(Self {
            name: name.to_owned(),
            auth_status: server
                .get("authStatus")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tool_names,
            tool_count,
        })
    }
}

/// Health of an MCP server from its `authStatus`.
///
/// `oauth` (lowercase) is tolerated alongside the schema's `oAuth` because
/// older relay code already accepted both spellings.
#[must_use]
pub fn mcp_health(server: &McpRecord) -> ConnectorHealth {
    match server.auth_status.as_deref() {
        Some("bearerToken" | "oAuth" | "oauth") => ConnectorHealth::Authenticated,
        Some("notLoggedIn") => ConnectorHealth::AuthRequired,
        Some("unsupported") => ConnectorHealth::AuthUnsupported,
        _ => ConnectorHealth::Unknown,
    }
}

/// Human phrasing for an MCP auth status without inventing state.
#[must_use]
pub fn mcp_auth_label(auth_status: Option<&str>) -> String {
    match auth_status {
        Some("notLoggedIn") => "sign-in required".to_owned(),
        Some("bearerToken") => "authenticated (bearer token)".to_owned(),
        Some("oAuth" | "oauth") => "authenticated (OAuth)".to_owned(),
        Some("unsupported") => "no OAuth support".to_owned(),
        Some(other) => other.to_owned(),
        None => "auth status unknown".to_owned(),
    }
}

/// One row of the merged connector overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewRow {
    pub name: String,
    pub health: ConnectorHealth,
    /// Short factual sentence fragments joined by " · ".
    pub detail: String,
    /// An https link to the app's install page, when one applies.
    pub connect_url: Option<String>,
}

fn matches_plugin(app: &AppRecord, plugin: &PluginRecord) -> bool {
    let shown = plugin.shown_name();
    app.name.eq_ignore_ascii_case(shown)
        || app.name.eq_ignore_ascii_case(&plugin.name)
        || app
            .plugin_display_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(shown))
}

fn https_only(url: Option<&str>) -> Option<String> {
    url.filter(|url| url.starts_with("https://") && url.len() <= 2_048)
        .map(str::to_owned)
}

/// Merges the three wire sources into one deduplicated health view.
///
/// Directory apps appear only when they carry a signal worth surfacing:
/// accessible, explicitly disabled, or backed by a local plugin. Installed or
/// disabled plugins that match no app entry get their own rows, as do all MCP
/// servers. Catalog-only plugins (`installed == false`) are `/plugins`
/// material and are skipped here.
///
/// A row's health is always the app's own observed state; an installed
/// backing plugin is reported as an additional fact in the detail text and
/// never upgrades the app's accessibility.
#[must_use]
pub fn connector_overview(
    apps: &[AppRecord],
    plugins: &[PluginRecord],
    servers: &[McpRecord],
) -> Vec<OverviewRow> {
    let mut rows = Vec::new();
    let mut matched_plugins = vec![false; plugins.len()];

    for app in apps {
        let health = app_health(app);
        let backing = plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| {
                plugin_health(plugin) != ConnectorHealth::Available && matches_plugin(app, plugin)
            })
            .collect::<Vec<_>>();
        if health == ConnectorHealth::CanInstall && backing.is_empty() {
            // Directory noise: thousands of not-installed entries.
            continue;
        }
        let mut facts = vec![match health {
            ConnectorHealth::Accessible => "app accessible".to_owned(),
            ConnectorHealth::Disabled => "disabled in config.toml".to_owned(),
            _ => "app not accessible yet".to_owned(),
        }];
        for (index, plugin) in &backing {
            matched_plugins[*index] = true;
            let plugin_state = plugin_health(plugin);
            let version = plugin
                .version
                .as_deref()
                .map(|version| format!(" v{version}"))
                .unwrap_or_default();
            facts.push(format!(
                "plugin `{}`{version} {} ({})",
                plugin.name,
                plugin_state.label(),
                plugin.marketplace
            ));
        }
        let connect_url = if health == ConnectorHealth::CanInstall {
            https_only(app.install_url.as_deref())
        } else {
            None
        };
        rows.push(OverviewRow {
            name: app.name.clone(),
            health,
            detail: facts.join(" · "),
            connect_url,
        });
    }

    for (plugin, matched) in plugins.iter().zip(matched_plugins) {
        if matched {
            continue;
        }
        let health = plugin_health(plugin);
        if health == ConnectorHealth::Available {
            continue;
        }
        let mut facts = vec![format!("plugin ({})", plugin.marketplace)];
        if let Some(version) = plugin.version.as_deref() {
            facts.push(format!("v{version}"));
        }
        if let Some(note) = auth_policy_note(plugin.auth_policy.as_deref()) {
            facts.push(note.to_owned());
        }
        rows.push(OverviewRow {
            name: plugin.shown_name().to_owned(),
            health,
            detail: facts.join(" · "),
            connect_url: None,
        });
    }

    for server in servers {
        let health = mcp_health(server);
        let auth = mcp_auth_label(server.auth_status.as_deref());
        let tools = format!(
            "{} tool{}",
            server.tool_count,
            if server.tool_count == 1 { "" } else { "s" }
        );
        // The health label already carries the auth story for most states;
        // repeat it only when it adds detail (e.g. bearer token vs OAuth).
        let detail = if auth == health.label() {
            format!("MCP server · {tools}")
        } else {
            format!("MCP server · {auth} · {tools}")
        };
        rows.push(OverviewRow {
            name: server.name.clone(),
            health,
            detail,
            connect_url: None,
        });
    }

    rows.sort_by(|a, b| {
        (a.health.sort_rank(), a.name.to_ascii_lowercase())
            .cmp(&(b.health.sort_rank(), b.name.to_ascii_lowercase()))
    });
    rows
}

/// How Gmail usability was actually proven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmailRoute {
    /// The Gmail app is accessible for this account (`isAccessible == true`).
    App { app_id: String },
    /// Gmail send tools are actually mounted in the MCP inventory right now.
    McpTools { tool_count: usize },
}

/// What `/email` can truthfully claim about Gmail before dispatching a turn.
///
/// Only [`GmailReadiness::Ready`] asserts usability, and it requires an
/// observed proof: app accessibility or a live Gmail send-tool inventory. A plugin
/// being installed and enabled is reported, but never treated as that proof —
/// live wire captures showed the plugin state can exist while the app is not
/// accessible and no Gmail send tools are mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmailReadiness {
    /// Usability proven by an observed route.
    Ready { route: GmailRoute },
    /// The Gmail plugin is installed and enabled, but neither app
    /// accessibility nor a mounted tool proves Gmail currently works.
    PluginUnverified {
        plugin_name: String,
        connect_url: Option<String>,
    },
    /// A Gmail plugin exists locally but is disabled or blocked.
    PluginBlocked {
        plugin_name: String,
        health: ConnectorHealth,
    },
    /// Only a not-accessible directory entry was found.
    AppInstallable { connect_url: Option<String> },
    /// Nothing Gmail-shaped was observed.
    NotConfigured,
}

fn is_gmail(name: &str) -> bool {
    name.eq_ignore_ascii_case("gmail")
}

fn app_is_gmail(app: &AppRecord) -> bool {
    is_gmail(&app.name)
        || is_gmail(&app.id)
        || app.plugin_display_names.iter().any(|name| is_gmail(name))
}

fn gmail_send_tool_count(servers: &[McpRecord]) -> usize {
    servers
        .iter()
        .flat_map(|server| {
            let server_is_gmail = server.name.to_ascii_lowercase().contains("gmail");
            server.tool_names.iter().filter(move |tool| {
                let tool = tool.to_ascii_lowercase();
                let gmail_scoped = server_is_gmail || tool.contains("gmail");
                gmail_scoped && tool.contains("send")
            })
        })
        .count()
}

/// Derives Gmail readiness from all three observed sources.
///
/// Precedence: proven app accessibility, then proven send-tool inventory, then the
/// unproven-but-reportable plugin facts, then the installable directory
/// entry.
#[must_use]
pub fn gmail_readiness(
    plugins: &[PluginRecord],
    apps: &[AppRecord],
    servers: &[McpRecord],
) -> GmailReadiness {
    let app = apps.iter().find(|app| app_is_gmail(app));
    if let Some(app) = app
        && app_health(app) == ConnectorHealth::Accessible
    {
        return GmailReadiness::Ready {
            route: GmailRoute::App {
                app_id: app.id.clone(),
            },
        };
    }
    let gmail_tools = gmail_send_tool_count(servers);
    if gmail_tools > 0 {
        return GmailReadiness::Ready {
            route: GmailRoute::McpTools {
                tool_count: gmail_tools,
            },
        };
    }
    let connect_url = app.and_then(|app| https_only(app.install_url.as_deref()));
    let plugin = plugins
        .iter()
        .find(|plugin| is_gmail(plugin.shown_name()) || is_gmail(&plugin.name));
    if let Some(plugin) = plugin {
        return match plugin_health(plugin) {
            ConnectorHealth::Installed => GmailReadiness::PluginUnverified {
                plugin_name: plugin.name.clone(),
                connect_url,
            },
            ConnectorHealth::Available | ConnectorHealth::Unknown => match app {
                Some(_) => GmailReadiness::AppInstallable { connect_url },
                None => GmailReadiness::NotConfigured,
            },
            health => GmailReadiness::PluginBlocked {
                plugin_name: plugin.name.clone(),
                health,
            },
        };
    }
    match app {
        Some(_) => GmailReadiness::AppInstallable { connect_url },
        None => GmailReadiness::NotConfigured,
    }
}

/// Read-only harness against the locally installed Codex app-server.
///
/// Generic by design: it asserts the documented wire contract and the
/// model's classification, never that a particular product (e.g. Gmail) is
/// installed on the machine. Every RPC used here only reads state
/// (`app/list`, `plugin/installed`, `mcpServerStatus/list`); nothing is
/// installed, mutated, or logged in. The harness never prints connector,
/// plugin, server, tool, account, or path values from the local machine.
#[cfg(test)]
mod live_tests {
    use serde_json::Value;

    use super::*;
    use crate::codex::{
        AppListParams, CodexClient, McpServerStatusListParams, PluginInstalledParams,
    };

    async fn fetch_all_apps(client: &CodexClient) -> Vec<AppRecord> {
        let mut records = Vec::new();
        let mut raw_entries = 0_usize;
        let mut cursor: Option<String> = None;
        for _ in 0..200 {
            let result = client
                .app_list(AppListParams {
                    cursor: cursor.clone(),
                    limit: Some(100),
                    thread_id: None,
                    force_refetch: Some(false),
                })
                .await
                .expect("app/list failed");
            let data = result
                .get("data")
                .and_then(Value::as_array)
                .expect("app/list returned no data array");
            raw_entries += data.len();
            records.extend(data.iter().filter_map(AppRecord::from_value));
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                assert_eq!(
                    records.len(),
                    raw_entries,
                    "some app/list entries were missing required id/name fields"
                );
                return records;
            }
        }
        panic!("app/list exceeded 200 pages");
    }

    async fn fetch_installed_plugins(client: &CodexClient) -> Vec<PluginRecord> {
        let result = client
            .plugin_installed(PluginInstalledParams::default())
            .await
            .expect("plugin/installed failed");
        PluginRecord::from_marketplaces(&result)
    }

    async fn fetch_mcp_servers(client: &CodexClient) -> Vec<McpRecord> {
        let result = client
            .mcp_server_status_list(McpServerStatusListParams {
                cursor: None,
                limit: Some(100),
                detail: Some("toolsAndAuthOnly".to_owned()),
                thread_id: None,
            })
            .await
            .expect("mcpServerStatus/list failed");
        result
            .get("data")
            .and_then(Value::as_array)
            .expect("mcpServerStatus/list returned no data array")
            .iter()
            .filter_map(McpRecord::from_value)
            .collect()
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_connector_sources_honor_the_documented_contract() {
        let client = CodexClient::discover().unwrap();
        let plugins = fetch_installed_plugins(&client).await;
        let servers = fetch_mcp_servers(&client).await;

        for (index, plugin) in plugins.iter().enumerate() {
            assert!(
                plugin.installed.is_some() && plugin.enabled.is_some(),
                "plugin row {index} omitted schema-required installed/enabled"
            );
        }
        for (index, server) in servers.iter().enumerate() {
            assert!(
                server.auth_status.is_some(),
                "MCP server row {index} omitted schema-required authStatus"
            );
        }
        client.close().await;
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_connector_overview_classifies_every_row() {
        let client = CodexClient::discover().unwrap();
        let apps = fetch_all_apps(&client).await;
        let plugins = fetch_installed_plugins(&client).await;
        let servers = fetch_mcp_servers(&client).await;
        let rows = connector_overview(&apps, &plugins, &servers);

        assert!(
            !rows.is_empty(),
            "an installed Codex should expose at least one connector row"
        );
        for (index, row) in rows.iter().enumerate() {
            assert_ne!(
                row.health,
                ConnectorHealth::Unknown,
                "live data produced an unclassifiable row at index {index}"
            );
            if let Some(url) = &row.connect_url {
                assert!(url.starts_with("https://"), "connect URL must be https");
            }
        }
        client.close().await;
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_plugin_backed_apps_keep_both_facts_and_readiness_is_grounded() {
        let client = CodexClient::discover().unwrap();
        let apps = fetch_all_apps(&client).await;
        let plugins = fetch_installed_plugins(&client).await;
        let servers = fetch_mcp_servers(&client).await;
        let rows = connector_overview(&apps, &plugins, &servers);

        // Generic invariant: every installed plugin that matches a directory
        // app keeps its install fact visible on that app's row, while the
        // row's health stays the app's own observed accessibility.
        for plugin in &plugins {
            if plugin_health(plugin) != ConnectorHealth::Installed {
                continue;
            }
            let Some(app) = apps.iter().find(|app| matches_plugin(app, plugin)) else {
                continue;
            };
            let row = rows
                .iter()
                .find(|row| row.name == app.name)
                .expect("plugin-backed app lost its overview row");
            assert_eq!(row.health, app_health(app), "health must stay the app's");
            assert!(
                row.detail.contains(&format!("plugin `{}`", plugin.name)),
                "plugin fact missing from matched overview row"
            );
        }

        // Gmail-specific check only when this machine has the plugin: the
        // readiness verdict must be grounded in app/tool proof, never in the
        // plugin alone.
        let readiness = gmail_readiness(&plugins, &apps, &servers);
        let gmail_plugin_installed = plugins.iter().any(|plugin| {
            (is_gmail(plugin.shown_name()) || is_gmail(&plugin.name))
                && plugin_health(plugin) == ConnectorHealth::Installed
        });
        let gmail_app_accessible = apps
            .iter()
            .any(|app| app_is_gmail(app) && app_health(app) == ConnectorHealth::Accessible);
        let gmail_tools_mounted = gmail_send_tool_count(&servers) > 0;
        if let GmailReadiness::Ready { .. } = &readiness {
            assert!(
                gmail_app_accessible || gmail_tools_mounted,
                "Ready verdict without an observed app/tool proof"
            );
        } else if gmail_plugin_installed {
            assert!(
                matches!(readiness, GmailReadiness::PluginUnverified { .. }),
                "installed Gmail plugin must surface as PluginUnverified"
            );
        }
        client.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Genericized Gmail directory fixture matching the installed schema.
    fn live_gmail_app() -> Value {
        json!({
            "id": "connector_gmail_fixture",
            "name": "Gmail",
            "description": "Review your Gmail conversations…",
            "distributionChannel": "DEFAULT_OAI_CATALOG",
            "labels": {"consequential": "true", "retrievable": "true", "sync": "true"},
            "installUrl": "https://chatgpt.com/apps/gmail/connector_fixture",
            "isAccessible": false,
            "isEnabled": true,
            "pluginDisplayNames": []
        })
    }

    /// Genericized installed Gmail plugin fixture matching the schema.
    fn live_gmail_plugin() -> Value {
        json!({
            "id": "gmail@openai-curated",
            "name": "gmail",
            "installed": true,
            "enabled": true,
            "availability": "AVAILABLE",
            "installPolicy": "AVAILABLE",
            "authPolicy": "ON_INSTALL",
            "localVersion": "0.1.3",
            "interface": {"displayName": "Gmail", "capabilities": ["Interactive", "Write"],
                          "screenshotUrls": [], "screenshots": []}
        })
    }

    #[test]
    fn app_health_applies_schema_defaults_for_absent_fields() {
        let record = AppRecord::from_value(&json!({"id": "a", "name": "A"})).unwrap();
        assert_eq!(record.is_accessible, None);
        assert_eq!(record.is_enabled, None);
        // isEnabled defaults true, isAccessible defaults false per schema.
        assert_eq!(app_health(&record), ConnectorHealth::CanInstall);
    }

    #[test]
    fn app_labels_match_the_official_tui_wording() {
        // openai/codex TUI: isAccessible true → "Installed",
        // false → "Can be installed". Never "connected"/"disconnected".
        assert_eq!(ConnectorHealth::Accessible.label(), "installed");
        assert_eq!(ConnectorHealth::CanInstall.label(), "can be installed");
        for health in [
            ConnectorHealth::Accessible,
            ConnectorHealth::CanInstall,
            ConnectorHealth::Authenticated,
        ] {
            assert!(
                !health.label().contains("connect"),
                "app/MCP state must not use connected/disconnected wording"
            );
        }
    }

    #[test]
    fn app_health_orders_disabled_above_accessibility() {
        let record = AppRecord::from_value(
            &json!({"id": "a", "name": "A", "isEnabled": false, "isAccessible": true}),
        )
        .unwrap();
        assert_eq!(app_health(&record), ConnectorHealth::Disabled);
    }

    #[test]
    fn app_records_without_identity_are_rejected() {
        assert_eq!(AppRecord::from_value(&json!({"name": "x"})), None);
        assert_eq!(AppRecord::from_value(&json!({"id": "x"})), None);
    }

    #[test]
    fn plugin_health_covers_every_wire_state() {
        let base = live_gmail_plugin();
        let record = PluginRecord::from_value(&base, "openai-curated").unwrap();
        assert_eq!(plugin_health(&record), ConnectorHealth::Installed);

        let mut disabled = record.clone();
        disabled.enabled = Some(false);
        assert_eq!(plugin_health(&disabled), ConnectorHealth::Disabled);

        let mut admin = record.clone();
        admin.availability = Some("DISABLED_BY_ADMIN".to_owned());
        assert_eq!(plugin_health(&admin), ConnectorHealth::AdminDisabled);

        let mut unavailable = record.clone();
        unavailable.install_policy = Some("NOT_AVAILABLE".to_owned());
        assert_eq!(plugin_health(&unavailable), ConnectorHealth::Unavailable);

        let mut catalog = record.clone();
        catalog.installed = Some(false);
        assert_eq!(plugin_health(&catalog), ConnectorHealth::Available);

        let mut broken = record;
        broken.installed = None;
        assert_eq!(plugin_health(&broken), ConnectorHealth::Unknown);
    }

    #[test]
    fn mcp_health_never_claims_no_auth_needed_for_missing_status() {
        let unknown = McpRecord::from_value(&json!({"name": "s"})).unwrap();
        assert_eq!(mcp_health(&unknown), ConnectorHealth::Unknown);
        assert_eq!(mcp_auth_label(None), "auth status unknown");

        let cases = [
            ("bearerToken", ConnectorHealth::Authenticated),
            ("oAuth", ConnectorHealth::Authenticated),
            ("oauth", ConnectorHealth::Authenticated),
            ("notLoggedIn", ConnectorHealth::AuthRequired),
            ("unsupported", ConnectorHealth::AuthUnsupported),
            ("somethingNew", ConnectorHealth::Unknown),
        ];
        for (status, expected) in cases {
            let record =
                McpRecord::from_value(&json!({"name": "s", "authStatus": status})).unwrap();
            assert_eq!(mcp_health(&record), expected, "authStatus={status}");
        }
    }

    #[test]
    fn mcp_records_collect_tool_names_from_maps_and_arrays() {
        let map_shape = McpRecord::from_value(&json!({
            "name": "codex_apps", "authStatus": "bearerToken",
            "tools": {"gmail.search_email_ids": {}, "sites.create_site": {}}
        }))
        .unwrap();
        assert_eq!(map_shape.tool_count, 2);
        assert!(
            map_shape
                .tool_names
                .contains(&"gmail.search_email_ids".to_owned())
        );

        let array_shape = McpRecord::from_value(&json!({
            "name": "s", "authStatus": "unsupported",
            "tools": [{"name": "resolve-library-id"}, {"noName": true}]
        }))
        .unwrap();
        assert_eq!(array_shape.tool_count, 2);
        assert_eq!(array_shape.tool_names, ["resolve-library-id"]);
    }

    #[test]
    fn overview_reports_gmail_plugin_and_app_state_as_separate_facts() {
        let apps = [AppRecord::from_value(&live_gmail_app()).unwrap()];
        let plugins = [PluginRecord::from_value(&live_gmail_plugin(), "openai-curated").unwrap()];
        let rows = connector_overview(&apps, &plugins, &[]);
        assert_eq!(rows.len(), 1, "plugin and app must merge into one row");
        let row = &rows[0];
        assert_eq!(row.name, "Gmail");
        // The health stays the app's own observed accessibility; the plugin
        // install is an extra fact, never an upgrade to "usable".
        assert_eq!(row.health, ConnectorHealth::CanInstall);
        assert!(row.detail.contains("app not accessible yet"));
        assert!(row.detail.contains("plugin `gmail` v0.1.3 installed"));
        // The install link stays offered: linking the app is the documented
        // path to make it accessible.
        assert_eq!(
            row.connect_url.as_deref(),
            Some("https://chatgpt.com/apps/gmail/connector_fixture")
        );
    }

    #[test]
    fn overview_hides_directory_noise_and_catalog_only_plugins() {
        let apps = [AppRecord::from_value(&json!({
            "id": "noise", "name": "Random Directory App",
            "isAccessible": false, "isEnabled": true,
            "installUrl": "https://chatgpt.com/apps/random"
        }))
        .unwrap()];
        let plugins = [PluginRecord::from_value(
            &json!({"id": "x", "name": "x", "installed": false, "enabled": true}),
            "catalog",
        )
        .unwrap()];
        assert!(connector_overview(&apps, &plugins, &[]).is_empty());
    }

    #[test]
    fn overview_rejects_non_https_install_links() {
        let mut app = live_gmail_app();
        app["installUrl"] = json!("http://chatgpt.com/apps/gmail/x");
        let apps = [AppRecord::from_value(&app).unwrap()];
        let plugins = [PluginRecord::from_value(&live_gmail_plugin(), "openai-curated").unwrap()];
        let rows = connector_overview(&apps, &plugins, &[]);
        assert_eq!(rows[0].connect_url, None, "plain http must be rejected");
    }

    #[test]
    fn overview_ranks_problems_before_healthy_rows() {
        let servers = [
            McpRecord::from_value(&json!({"name": "ok", "authStatus": "bearerToken",
                "tools": {"a": {}}}))
            .unwrap(),
            McpRecord::from_value(&json!({"name": "broken", "authStatus": "notLoggedIn"})).unwrap(),
        ];
        let rows = connector_overview(&[], &[], &servers);
        assert_eq!(rows[0].name, "broken");
        assert_eq!(rows[0].health, ConnectorHealth::AuthRequired);
        // "sign-in required" is already the health label; detail stays terse.
        assert_eq!(rows[0].detail, "MCP server · 0 tools");
        assert_eq!(rows[1].name, "ok");
        assert_eq!(
            rows[1].detail,
            "MCP server · authenticated (bearer token) · 1 tool"
        );
    }

    #[test]
    fn overview_matches_plugins_via_app_plugin_display_names() {
        let apps = [AppRecord::from_value(&json!({
            "id": "doc", "name": "Codex Document Control",
            "isAccessible": true, "isEnabled": true,
            "pluginDisplayNames": ["Spreadsheets"]
        }))
        .unwrap()];
        let plugins = [PluginRecord::from_value(
            &json!({"id": "spreadsheets", "name": "spreadsheets", "installed": true,
                "enabled": true,
                "interface": {"displayName": "Spreadsheets", "capabilities": [],
                              "screenshotUrls": [], "screenshots": []}}),
            "openai-primary-runtime",
        )
        .unwrap()];
        let rows = connector_overview(&apps, &plugins, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].health, ConnectorHealth::Accessible);
        assert!(rows[0].detail.contains("app accessible"));
        assert!(rows[0].detail.contains("plugin `spreadsheets` installed"));
    }

    #[test]
    fn gmail_readiness_requires_observed_proof_not_the_plugin_alone() {
        // Live-captured combination: plugin installed+enabled, app not
        // accessible, no gmail tools mounted → usability is NOT proven.
        let apps = [AppRecord::from_value(&live_gmail_app()).unwrap()];
        let plugins = [PluginRecord::from_value(&live_gmail_plugin(), "openai-curated").unwrap()];
        let servers = [McpRecord::from_value(&json!({
            "name": "codex_apps", "authStatus": "bearerToken",
            "tools": {"sites.create_site": {}, "plugin_management.uninstall_app": {}}
        }))
        .unwrap()];
        assert_eq!(
            gmail_readiness(&plugins, &apps, &servers),
            GmailReadiness::PluginUnverified {
                plugin_name: "gmail".to_owned(),
                connect_url: Some("https://chatgpt.com/apps/gmail/connector_fixture".to_owned())
            }
        );
    }

    #[test]
    fn gmail_readiness_accepts_app_accessibility_as_proof() {
        let mut app = live_gmail_app();
        app["isAccessible"] = json!(true);
        let apps = [AppRecord::from_value(&app).unwrap()];
        assert_eq!(
            gmail_readiness(&[], &apps, &[]),
            GmailReadiness::Ready {
                route: GmailRoute::App {
                    app_id: "connector_gmail_fixture".to_owned()
                }
            }
        );
    }

    #[test]
    fn gmail_readiness_does_not_treat_read_only_gmail_tools_as_send_proof() {
        let plugins = [PluginRecord::from_value(&live_gmail_plugin(), "openai-curated").unwrap()];
        let servers = [McpRecord::from_value(&json!({
            "name": "codex_apps", "authStatus": "bearerToken",
            "tools": {"gmail.search_email_ids": {}, "gmail.batch_read_email": {}}
        }))
        .unwrap()];
        assert_eq!(
            gmail_readiness(&plugins, &[], &servers),
            GmailReadiness::PluginUnverified {
                plugin_name: "gmail".to_owned(),
                connect_url: None
            }
        );
    }

    #[test]
    fn gmail_readiness_accepts_mounted_send_tool_as_proof() {
        let servers = [McpRecord::from_value(&json!({
            "name": "codex_apps", "authStatus": "bearerToken",
            "tools": {"gmail.send_email": {}, "gmail.search_email_ids": {}}
        }))
        .unwrap()];
        assert_eq!(
            gmail_readiness(&[], &[], &servers),
            GmailReadiness::Ready {
                route: GmailRoute::McpTools { tool_count: 1 }
            }
        );
    }

    #[test]
    fn gmail_readiness_reports_installable_and_blocked_states() {
        let apps = [AppRecord::from_value(&live_gmail_app()).unwrap()];
        assert_eq!(
            gmail_readiness(&[], &apps, &[]),
            GmailReadiness::AppInstallable {
                connect_url: Some("https://chatgpt.com/apps/gmail/connector_fixture".to_owned())
            }
        );

        let mut blocked = PluginRecord::from_value(&live_gmail_plugin(), "openai-curated").unwrap();
        blocked.availability = Some("DISABLED_BY_ADMIN".to_owned());
        assert_eq!(
            gmail_readiness(std::slice::from_ref(&blocked), &apps, &[]),
            GmailReadiness::PluginBlocked {
                plugin_name: "gmail".to_owned(),
                health: ConnectorHealth::AdminDisabled
            }
        );

        // An accessible app outranks the blocked plugin.
        let mut accessible = live_gmail_app();
        accessible["isAccessible"] = json!(true);
        let accessible = [AppRecord::from_value(&accessible).unwrap()];
        assert_eq!(
            gmail_readiness(std::slice::from_ref(&blocked), &accessible, &[]),
            GmailReadiness::Ready {
                route: GmailRoute::App {
                    app_id: "connector_gmail_fixture".to_owned()
                }
            }
        );

        assert_eq!(
            gmail_readiness(&[], &[], &[]),
            GmailReadiness::NotConfigured
        );
    }
}
