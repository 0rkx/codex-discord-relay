# Coverage and compatibility

Relay measures three different app-server protocol surfaces: client requests, server-initiated
requests, and notifications. Numbers are generated from the installed Codex schema at startup.

## What “coverage” means

For client requests, a method counts as schema-routed only when Relay has an audited registry entry
that defines:

- Discord family, label, and description;
- input strategy and compilable form;
- authorization policy and confirmation level;
- result rendering strategy;
- live method presence and schema validation before dispatch.

Coverage includes discovery, policy, form generation, validation, and dispatch routing. Bespoke
Discord component design, provider sign-in, and live side-effect testing are reported separately.

Complex object/union parameters can use validated JSON fields. Those methods are compatible and
routed, but their UI is not necessarily bespoke.

## Audited snapshot

Against codex-cli 0.145.0-alpha.18, the checked-in registry accounts for 123 client-schema entries:

| Bucket | Count | Meaning |
|---|---:|---|
| User-launchable, schema-routed | 118 | Audited policy, input strategy, validation, and dispatch |
| Protocol-only methods | 5 | Not user-launchable |
| Generic-only rows | 0 | Known method without audited routing |
| Missing rows | 0 | Schema method not represented |

The five protocol-only methods are the JSON-RPC `initialize` handshake, an experimental mock
fixture, two elicitation reference counters, and transport subscription cleanup.

This audit snapshot is versioned. Codex Desktop auto-updates, so Relay regenerates the schema and
recomputes the live report each time it starts.

## Forward compatibility

Unknown future methods can appear through the generic compatibility path and are reported
separately from schema-routed coverage. New user-launchable methods need an audited routing spec
before release; protocol-only methods receive an explicit classification.

Use `/capabilities` for current totals. Use `/capabilities method:<name>` to inspect a specific
installed method. A method absent from the installed build fails fast with a capability error.

## Server requests

Relay explicitly classifies currently known server-initiated requests:

| Disposition | Methods |
|---|---|
| Interactive Discord workflow | command approval, file-change approval, permission approval, legacy exec/patch approval, user input, MCP elicitation |
| Host handled | current time read; allowlisted `codex_app` dynamic tools |
| Negotiated off | ChatGPT token refresh, device attestation |

Relay-created tasks advertise the read-only `codex_app.list_threads` and `codex_app.read_thread`
tools through `thread/start.dynamicTools`. Unknown namespaces and tools fail closed. Other
negotiated-off capabilities are not advertised; if a peer sends one anyway, Relay rejects it
rather than fabricating a response.

Server-request classification coverage does not make its delivery queue durable. A lagged bounded
receiver can drop a pending approval and leave a turn waiting. Relay logs and audits this condition;
interrupt the affected task.

Once received, an interactive request is bound to its app-server process generation, externally
resolved requests are tombstoned across setup races, offered policy choices are echoed exactly,
and unanswered cards expire after 15 minutes with the protocol-specific terminal response.

## Notifications

Each installed notification must have an explicit semantic disposition before the live report is
fully accounted:

- rendered into answer, plan, activity, state, approval, or audit UX; or
- deliberately ignored with a reason because another event is authoritative or the event is only
  transport noise.

Only authoritative agent-message data enters the final answer accumulator. This prevents command
logs or unrelated deltas from corrupting assistant text. In the audited 70-notification snapshot,
39 methods feed rendered state and 31 are explicitly audit-only, transport-only, standalone, or
superseded by authoritative completion. Command/MCP/patch progress, realtime transcript/audio,
and completed image items have dedicated bounded paths.

## End-to-end evidence

Protocol routing and user-flow verification are separate. Public release checks should cover at
least:

| Flow | Evidence |
|---|---|
| Setup | Fresh private guild provisions expected layout and commands |
| New task | `/new` creates a Codex task and private mirror channel |
| Existing task | `/tasks` reopens active and archived tasks |
| Turn | Text and attachment reach Codex; final answer returns |
| Approval | Allow/deny response resolves the exact pending RPC request |
| Dynamic HostBroker | Real model tool call completes nested app-server RPC; cold resume and fork retain tools |
| User input | Modal response resumes the waiting turn |
| Lifecycle | Interrupt, fork, archive, rename, rollback, compact |
| Recovery | Offline message replay and outbox delivery after restart |
| Security | Wrong owner/guild/public channel fail; GOD activates, expires, revokes, and normalizes |

The repository's default unit/integration suite uses protocol fixtures and does not require Discord
or Codex Desktop. Ignored tests exercise the locally installed app-server and can drift with Desktop
updates.

```powershell
cargo test --all-targets
cargo test --lib -- --ignored --nocapture
codex-discord.exe doctor --deep
```

Public coverage label: **118/118 schema-routed user-launchable methods for the audited Codex
version**. Live end-to-end flow results are reported separately from protocol routing.
