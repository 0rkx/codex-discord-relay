# Changelog

All notable changes to Relay are documented here. This project follows Semantic Versioning once a
stable public API is declared.

## [Unreleased]

### Added

- Public architecture, Discord UX, setup, security, coverage, contribution, and release guides.

### Fixed

- Replaced the Fork button's non-emoji symbol with a valid Unicode emoji so Discord accepts task
  control payloads.

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
- Some high-volume command/realtime and generated-image item types have limited Discord rendering.
