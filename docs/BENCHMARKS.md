# Performance

Relay combines Discord gateway handling, SQLite persistence, rich rendering, authorization, and a
native Codex app-server client in one Rust process. It does not require an interpreter or a separate
localhost HTTP bridge.

## Current Windows sample

Measured 2026-07-18 on Windows x64. Exact deployed release SHA-256:
`0AFA3A4A3594060C5F81F38095F13256D40B66FE10370276D59C819729F8EA10`.

| Metric | Result |
| --- | ---: |
| Release executable | 13.18 MiB |
| Relay settled working set | 33.72 MiB |
| Relay settled private memory | 16.06 MiB |
| Codex companion settled working set | 46.39 MiB |
| Codex companion settled private memory | 20.85 MiB |
| Relay + companion working set | 80.11 MiB |
| Relay + companion private memory | 36.91 MiB |
| Relay CPU over a ten-second idle sample | 0.000% of one core |

Windows also created a console host using 10.14 MiB working set and 1.62 MiB private memory.
Active Discord cache entries, attachments, task output, SQLite pages, Codex tools, and conversation
context can raise memory use.

## Embedded bridge design

Relay talks directly to one persistent `codex.exe app-server` child through typed JSON-RPC over
stdio. Discord commands do not make a localhost HTTP hop through the standalone bridge. This keeps
approvals, notifications, active turns, cancellation, and task routing in one process.

## Reproduction protocol

1. Build with `cargo build --release --locked`.
2. Install that exact binary and start one `run` process.
3. Wait until `codex-discord doctor --deep` reports the Discord, app-server, runner, permissions,
   layout, commands, working-directory, and GOD-hash checks as `OK`.
4. Wait 15 seconds, then capture working/private bytes for Relay and its direct Codex child.
5. Capture process CPU time, wait ten seconds at idle, capture it again, and calculate
   `(cpu_seconds_delta / 10) * 100` as percent of one logical core.
6. Record the executable hash, machine, Codex Desktop version, and console-host process separately.
