# Changelog

All notable changes to Relay are documented here. This project follows Semantic Versioning once a
stable public API is declared.

## [Unreleased]

### Added

- Wire-grounded connector health model (`discord::connectors`) distinguishing accessible
  ("installed" in official TUI wording), can-be-installed, authenticated, sign-in-required,
  plugin-installed, available, disabled/admin-disabled, unavailable, no-OAuth-support, and
  unknown states — every state maps to an observed `app/list`, `plugin/installed`, or
  `mcpServerStatus/list` field, a schema-documented default, or an explicit unknown.
- `/apps` now defaults to a merged connector-health overview (apps ⋈ installed plugins ⋈ MCP
  auth) with problems sorted first and https-only "Install" link buttons; it serves Codex's
  app cache by default with an explicit `refresh:true` option (a fresh fetch takes ~60–90s),
  and `/apps scope:directory` keeps the full catalog browse.
- `/email` verifies Gmail before dispatch and claims usability only on observed proof — app
  accessibility or an actually mounted Gmail send-tool inventory; an installed plugin is
  acknowledged as an unverified fact with the install link offered. Accessible Gmail is sent to
  Codex as a typed `mention` input instead of Markdown-shaped text.
- `app/list` calls that fail with RPC -32600 "thread not found" for a stored task thread
  (stale after a Relay restart) retry once globally instead of failing the command.
- Plugin detail embeds show humanized auth timing, non-default availability, and the ChatGPT
  apps a plugin drives; the plugin browser surfaces admin-disabled and policy-unavailable states.
- Read-only live harness (`connectors::live_tests`) that validates the documented wire
  contract and overview classification against the locally installed Codex app-server,
  never prints local connector names, IDs, tools, accounts, or paths, and asserts nothing
  machine-specific (Gmail checks run only where the plugin exists).
- Installed-Codex model harnesses now prove a native `webSearch` item plus sourced answer and the
  production existing-task resume path across a fresh app-server process and second turn.

- Native Rust HostBroker with real `codex_app.list_threads` and `codex_app.read_thread` dynamic
  tools, bounded concurrent routing, rich activity rendering, and live-tested cold resume/fork
  inheritance.
- First-class `/plugins` catalog, detail, policy-aware install/uninstall, and authentication UX.
- Bounded live-operation and realtime projections, validated PCM16 WAV delivery, generated/viewed
  image attachments, and a durable retrying media outbox.
- Dedicated realtime start/text/voice/stop controls plus privacy-safe terminal interaction,
  hook lifecycle, auto-approval review, model-safety, WebRTC, and realtime-item projections.
- Process-generation-bound approvals, server-offered policy amendment controls, cancel/timeout
  parity, resolved-request race tombstones, and 15-minute live request expiry.

### Fixed

- Gmail (and any plugin-backed product) is no longer reported with an invented
  connected/disconnected state: app accessibility now uses the official installed /
  can-be-installed wording, and the plugin install state is a separate fact on the same row
  that never upgrades the app's accessibility.
- `/apps` and `/email` no longer force a full app re-fetch (60–90s) on every invocation.
- `/tasks` explicitly calls `thread/resume` before reopening a stored task, including when its
  Discord channel already exists, preventing fresh app-server `thread not found` failures.
- MCP servers with a missing `authStatus` are labeled "auth status unknown" instead of the
  invented "no auth needed", and `unsupported` renders as "no OAuth support".
- Realtime audio now latches at the first rejected chunk instead of splicing later audio, preserves
  error state through close, heals interleaved transcript completion, and marks lagged sessions.
- Digest delivery retries after Discord failures instead of consuming the terminal dirty state.
- Server-request transport now applies bounded MPSC backpressure instead of dropping approvals when
  a broadcast consumer lags.
- Task-bound standalone command/process output is now generation-correlated into live operations;
  goal, settings, status, MCP startup, and external-config import notifications have Discord state.

## [0.1.1] - 2026-07-18

### Added

- Public architecture, Discord UX, setup, security, coverage, contribution, and release guides.
- New Relay tasks use GPT-5.6 Sol, medium reasoning, live web search, and a compact verification
  contract while preserving Codex as the underlying agent runtime.
- `/apps` paginates the Codex catalog, supports name filtering, and reports actual connector access.

### Fixed

- Replaced the Fork button's non-emoji symbol with a valid Unicode emoji so Discord accepts task
  control payloads.
- Empty MCP connector-install forms now use direct Accept/Decline controls, while forms above
  Discord's five-input limit use one lossless JSON or key-value field.
- `/email` explicitly attaches the discovered Gmail connector so Codex can run its installation,
  authorization, approval, and send workflow.

## [0.1.0] - 2026-07-18

Initial public preview.

### Added

- Native Rust Discord command center for local Codex Desktop tasks.
- Embedded typed Codex app-server v2 client with runtime discovery, schema generation, request
  correlation, protocol validation, reconnect guards, and child-process cleanup.
- Thirty guild slash commands covering task discovery, lifecycle, configuration, reviews,
  integrations, protocol actions, and task-scoped GOD mode.
- Private task-channel materialization, state categories, control dashboard, runner status, and
  audit channel.
- Rich embeds, buttons, selects, modals, autocomplete, attachment ingestion, coalesced streaming,
  plan/activity updates, and Markdown export for long answers.
- SQLite WAL state for mirrors, cursors, pending requests, redacted audit, durable answer outbox,
  sensitive-message deletion retries, and post-GOD cleanup markers.
- Offline owner-message replay and bounded local Discord attachment cache.
- Typed approvals, user-input requests, MCP elicitation, and connector-driven email flow.
- Live schema coverage registry with 117/117 user-launchable methods routed in the codex-cli 0.144.2
  audit snapshot and five protocol-only methods that are not user-launchable.
- Windows installer, Credential Manager token storage, Codex Desktop companion materialization,
  Task Scheduler startup, Run-key fallback, uninstaller, and deep doctor.
- Argon2id-protected, single-session, owner/guild/channel/task-bound GOD mode with expiry,
  revocation, cleanup quarantine, and guild-wide `!god` deletion handling.

### Security

- Central GOD classification covers raw RPC and capabilities equivalent to unrestricted local
  access, including host filesystem, arbitrary MCP, remote pairing, and extension/config mutation.
- Password rotation refuses a concurrently running Relay so stale in-memory credentials cannot
  remain valid.
- Guild isolation and private overwrites are verified before Relay accepts work.

### Known limits

- Relay is single-owner and private-guild only.
- Schema routing coverage is not the same as bespoke end-to-end Discord UX coverage.
- Bounded in-memory Codex server-request delivery can lag; Relay logs/audits the condition, but the
  affected task may require interruption.
- Long answers include complete Markdown but do not provide interactive page-by-page navigation.
- Connection-scoped standalone output cannot be rendered until Codex supplies safe task correlation.
