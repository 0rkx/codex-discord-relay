# Relay

Discord command center for local Codex Desktop tasks. Native Rust, Tokio, Serenity, SQLite,
and Codex app-server v2. Relay has its own embedded Rust app-server client; it does not run or
depend on the standalone [Codex OpenAI Bridge](https://github.com/0rkx/codex-openai-bridge).

Public documentation:

- [Setup](docs/SETUP.md) and [Discord experience](docs/DISCORD_UX.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security model](docs/SECURITY.md) and [vulnerability reporting](SECURITY.md)
- [Codex method coverage](docs/COVERAGE.md)
- [Measured performance and reproducible methodology](docs/BENCHMARKS.md)
- [Contributing](CONTRIBUTING.md), [changelog](CHANGELOG.md), and [release runbook](docs/RELEASING.md)

## What works

- New Codex tasks from `/new` or control-center button
- Existing Desktop tasks from `/tasks` with search/archive/page filters, materialized into private task channels
- Previous/Next task browsing preserves search and archive filters across cursor pages
- Rich embeds, buttons, selects, modals, page-numbered long answers with full Markdown export, attachment support
- `/actions` provides schema-generated compatibility forms for every non-exempt installed Codex
  method; the audited codex-cli 0.145.0-alpha.18 snapshot covers 118 user-launchable methods. Schema-untyped
  or union-typed parameters (for example `mcpServer/tool/call` arguments and
  `thread/settings/update` policies) accept validated JSON instead of dead-ending as plain text
- Live response coalescing instead of raw token/log spam; answers track Codex's per-item
  authoritative text, so multiple agent messages stay separated and dropped deltas cannot
  corrupt or lose a final answer
- Editable plan and activity embeds summarize turn progress without flooding channels
- Interrupt, steer, fork, archive, and reopen flows; reopening an archived task unarchives
  it in Codex first so new turns are accepted again
- Dedicated `/model`, `/review`, `/compact`, `/rollback`, `/rename`, `/skills`, `/apps`, `/plugins`,
  `/config`, `/account`, `/usage`, and `/capabilities` commands backed by typed Rust helpers
- `/apps` reads the complete paginated catalog, accepts an optional name filter, and distinguishes
  connected, not-connected, and disabled integrations from Codex's real accessibility fields
- `/plugins` searches installed and marketplace catalogs, opens full capability details, and offers
  confirmation-gated install/uninstall controls plus authentication links returned by Codex
- Task productivity commands: `/goal` (objective/budget tracking), `/history` (recent turn
  digest), `/terminals` (list/terminate/clean background terminals), `/files` (fuzzy workspace
  file search), `/mode` (collaboration presets), and `/effort` (reasoning effort for new turns)
- `/find` full-text search across every Codex task with snippets; results open directly into
  private task channels
- `/mcp` searches and paginates the complete MCP server, tool, resource, and resource-template catalogs
- Relay-created tasks register a native Rust `codex_app` HostBroker. Codex can list and read tasks
  asynchronously through real `dynamicTools` calls; calls are turn-bound, allowlisted, redacted,
  timeout-limited, and rendered in the activity embed
- `/email` opens a native modal, attaches Gmail explicitly, and carries installation,
  authorization, approval, and send results through the Codex connector flow
- Typed command/file/permission approvals plus question-specific user-input and MCP elicitation
  controls; empty consent forms use direct buttons and large forms use lossless JSON/key-value input
- New tasks default to GPT-5.6 Sol, medium reasoning, and live web search, with instructions to
  verify sources and external actions before reporting completion; `/model` and the model-aware
  `/effort` autocomplete expose per-task controls through Discord
- Discord attachments are cached locally before dispatch so asynchronous replay never depends on expiring CDN URLs
- Paged offline message catch-up after Windows runner restarts; ingestion marked with ✅
- Audited `/advanced` raw RPC remains only as a privileged developer escape hatch
- Automatic categories, channels, topics, private overwrites, dashboard, monthly/sharded archives
- Channel-cap pruning at 450 → 425; underlying Codex tasks are never deleted
- Discord token in Windows Credential Manager; SQLite WAL state/outbox/audit
- Pending approval cards survive long enough to be retired safely after restarts; poisoned outbox
  rows dead-letter after ten attempts instead of blocking later deliveries
- Rust-native Codex bridge: typed JSON-RPC framing, scalar-ID correlation, stale-child
  reaping, reconnect generation guards, protocol validation, and generated schema metadata
- Codex Desktop-native runtime discovery: the current Windows Store package companion is
  hash-verified and atomically copied into the relay's private runtime cache, so WindowsApps
  ACLs never block startup and no Node/npm, separate Codex CLI, or OpenAI API key is needed
- Windows Task Scheduler startup, with a per-user Windows Run-key fallback when policy blocks it

## Security model

Normal turns use `workspace-write` and `on-request`. `/god` opens a modal; its secret is never
posted to Discord history or logged. Any `!god ...` channel message is immediately deleted and
rejected. Passwords are Argon2id (64 MiB, 3 iterations), five failures lock authentication for
15 minutes, and only the exact configured owner + guild + private relay channel can authenticate.

GOD mode uses `danger-full-access` and approval policy `never`. It is scoped to one task/channel,
only one session may exist globally, it expires after 10 minutes, and can be revoked immediately.
This means real local code execution. It cannot bypass Windows account permissions or UAC.

Set the GOD password locally before starting the relay:

```powershell
codex-discord.exe set-god-password
```

For secure automation, `set-god-password --password-stdin` accepts exactly two matching
newline-delimited values. The password is never accepted as a command-line argument or environment
variable, so it does not appear in process listings or persisted configuration.

First-use GOD enrollment is local-only. If the protected hash is absent, `/god` fails closed and
tells you to run that local command. Channel messages beginning with `!god` are
deleted and rejected; password entry stays modal-only.

## Unavoidable Discord steps

Discord bots cannot create a Discord server or developer application. Do these once:

1. Create an empty private server.
2. At [Discord Developer Portal](https://discord.com/developers/applications), create application
   and bot.
3. Enable **Message Content Intent** for bot.
4. Copy bot token, server ID, and your user ID. Never paste token into Discord.
5. Run installer below. If bot is not installed, setup prints exact OAuth invite URL; open it,
   install bot into private server, then rerun installer.

Everything after that—permissions, categories, channels, cards, commands, persistence, startup—is
idempotently provisioned by bot.

## Install on Windows

From PowerShell:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\install.ps1
```

Use `-Highest` only if you want runner launched with highest privileges available to your Windows
account. Windows may require administrator approval.

Manual developer flow:

```powershell
cargo build --release --locked
.\target\release\codex-discord.exe install --configure
.\target\release\codex-discord.exe doctor --deep
```

Installation, binary copy, startup registration, and hidden relay launch are implemented by the
Rust executable. PowerShell only bootstraps Rust/build when no prebuilt executable is present.
Building from source requires Rust 1.88 or newer, matching the locked Windows dependencies.

Config/state live under `%LOCALAPPDATA%\Codex\CodexDiscordRelay`. Token lives in Windows Credential
Manager under service `codex-discord-relay`.

On Windows, the relay discovers the current Codex Desktop package from a running app process or
the current user's Store package registration. It never executes the ACL-protected WindowsApps
path directly. Instead it hashes the bundled native `codex.exe` and atomically refreshes
`%LOCALAPPDATA%\Codex\CodexDiscordRelay\runtime\codex.exe`. If Desktop is temporarily unavailable,
the last verified cache is used; the older `%LOCALAPPDATA%\OpenAI\Codex\bin\codex.exe` companion is
only a final fallback. `CODEX_RELAY_CODEX_EXECUTABLE` remains an explicit advanced override and must
name an existing file. PATH/npm shims are deliberately ignored on Windows.

## Discord layout

```text
00 • CONTROL
  #new-task
  #existing-tasks
  #runner-status
  #audit-log
10 • RUNNING
20 • NEEDS-YOU
30 • DONE YYYY-MM [• shard]
40 • FAILED [• shard]
```

Each mirrored Codex task gets one private text channel. Discord cannot force-open a channel on your
client; bot responds with a clickable channel mention.

## Coverage

The schema is regenerated from the installed Codex at every startup, and Codex Desktop
auto-updates, so runtime support follows the installed build. Against codex-cli 0.145.0-alpha.18, the
audited registry accounts for all 123 schema entries (the `initialize` handshake plus 122
post-initialize methods): 118 user-launchable methods have explicit method-specific Discord specs
and five protocol-only methods are not user-launchable.
That is 118/118 schema-routed user-launchable methods, with zero generic-only or missing registry
rows. Coverage includes method discovery, authorization metadata, form generation, and dispatch
routing. Bespoke Discord presentation is reported separately. The protocol-only methods are the
JSON-RPC `initialize` handshake, the mock experimental test fixture, two elicitation reference
counters, and transport subscription cleanup.

Each spec defines its Discord family, human label and description, input strategy, authorization
policy, confirmation level, and result renderer. Complex or newly introduced shapes may still use
a schema-derived JSON field; that is compatibility UX, not bespoke UX. Future Codex methods remain
invokable through the generic compatibility fallback and are reported separately until they
receive an audited spec or protocol-only classification. Methods absent from an older installed
build fail fast with a clear capability message. Run `/capabilities` for the live routing numbers;
use the live user-flow matrix for end-to-end UX evidence.

Every installed notification has an explicit semantic disposition; only agent-message delta
enters the answer stream. Every installed server request has an explicit host disposition:
interactive approvals/input, host-handled time reads, and deliberately unadvertised host
capabilities that fail closed if a peer violates negotiation. Methods the router knows but the
installed build no longer sends stay dormant. Account/auth browser handoffs and provider
creation still require provider UI because neither API permits safe unattended creation.

## Verify

```powershell
cargo fmt --all -- --check
cargo test --all-targets
cargo test --lib -- --ignored --nocapture
cargo clippy --all-targets --all-features --locked -- -D warnings -A clippy::pedantic
codex-discord.exe doctor --deep
```

## Measured resources

One Windows release-build sample taken from the exact deployed binary on 2026-07-18 after a
15-second idle settle and a ten-second CPU sample:

- Optimized Rust binary: 13.22 MiB on disk
- Rust Discord relay: 30.80 MiB working set, 13.96 MiB private memory, 0.000% one-core CPU
- Codex Desktop companion child: 48.46 MiB working set, 20.74 MiB private memory
- Relay + companion: 79.26 MiB working set, 34.71 MiB private memory
- Windows console host: an additional 10.15 MiB working set and 1.62 MiB private memory

Active turns, attachments, Discord cache size, and Codex Desktop updates can change resource use.
See [performance](docs/BENCHMARKS.md) for machine details, executable hash, and methodology.
