# Discord experience

Relay treats Discord as a command center, not a plain text pipe. Commands, task state, approvals,
and long answers use native Discord surfaces where those surfaces add clarity.

## Provisioned layout

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

Setup applies private permission overwrites for the configured owner and bot. Relay gates startup
if its live isolation check fails. Each materialized Codex task receives one text channel with a
stable task binding. State changes move that channel between categories.

Discord has a 500-channel server limit. Relay starts pruning completed mirror channels at 450 and
returns to 425. Pruning detaches only the Discord mirror; it never deletes the underlying Codex
task. Opening that task again materializes a fresh private channel.

## Primary flow

1. Use `/new` or the control-card button.
2. Complete the modal and open the returned task-channel mention.
3. Send ordinary messages and attachments directly in that task channel.
4. Watch the plan and activity cards update while Codex works.
5. Respond to approvals, questions, or connector elicitation through buttons and modals.
6. Receive the final answer as a formatted embed. Long answers also include the complete
   `codex-answer.md` attachment.

Messages posted in control channels are not forwarded to Codex. Relay responds with guidance so
accidental control-channel text cannot silently become a task instruction.

## Task discovery

- `/tasks` browses active or archived Codex Desktop tasks, supports title/preview search, and keeps
  filters across Previous/Next pages.
- `/find` runs full-text search across Codex task content and opens results into task channels.
- `/history` shows a recent-turn digest for the current task.
- `/status` shows current task, runner, and GOD state.

Discord cannot force a client to navigate to a channel. Relay returns a clickable channel mention
after task creation or materialization.

## Command surface

Relay currently registers 31 guild slash commands.

| Area | Commands | Purpose |
|---|---|---|
| Tasks | `/new`, `/tasks`, `/find`, `/status`, `/history` | Create, discover, reopen, and inspect tasks |
| Turn control | `/interrupt`, `/fork`, `/archive`, `/rename`, `/rollback`, `/compact` | Control task lifecycle and history |
| Work setup | `/model`, `/mode`, `/effort`, `/goal`, `/files`, `/terminals` | Configure and inspect task execution |
| Codex features | `/review`, `/skills`, `/apps`, `/plugins`, `/mcp`, `/email` | Launch reviews and use installed integrations |
| Runtime inspection | `/config`, `/account`, `/usage`, `/capabilities` | Show redacted configuration and live capability state |
| Protocol access | `/actions`, `/advanced` | Browse schema-routed actions or invoke a raw method |
| Privileged mode | `/god`, `/god_off` | Start or revoke task-scoped unrestricted execution |

New tasks start with `gpt-5.6-sol` and `medium` reasoning. `/model` changes model for later turns.
`/effort` fetches the selected model's live reasoning-effort catalog for autocomplete and rejects
values that model does not support; GPT-5.6 Sol currently exposes `low` through `ultra`.

`/mcp` has searchable, paginated views for every configured server, tool, resource, and resource
template. It fetches every Codex cursor with full detail instead of stopping at Discord's first
25 choices.

`/plugins` searches either installed plugins or all configured marketplaces. Each result page uses
an opaque, owner-bound selection token; selecting a plugin reads live detail without exposing local
paths. Install and uninstall reuse Relay's typed GOD confirmation flow. Successful installs render
Codex-provided app authentication URLs as link buttons when authentication remains necessary.

Relay-created tasks also expose a native Rust `codex_app` namespace to the Codex model. Its
read-only `list_threads` and `read_thread` tools let an agent inspect asynchronous task state without
leaving Discord. Activity embeds show the namespace, tool, status, and duration instead of a generic
item label.

`/advanced` is task-bound, requires active GOD mode, validates JSON against the installed schema,
and exists for developer recovery—not routine use.

## Components and rendering

- Buttons handle task creation, opening, interruption, state transitions, confirmations, and GOD
  revocation.
- Select menus handle task lists, action families/methods, models, modes, skills, apps, plugins, and files.
- Modals collect new-task data, action parameters, approval input, elicitation input, and the GOD
  password without posting it to channel history.
- Autocomplete is backed by the installed app-server schema for methods and other live catalogs.
- Final answer, status, plan, activity, audit, approval, and warning cards use embeds with distinct
  state colors and compact fields.

Action forms may span several modal pages because Discord limits modal fields. JSON-shaped or
union-typed parameters use a validated JSON field when Discord controls cannot represent the
shape safely. That is compatibility UX, not a bespoke workflow.

## Attachments and offline replay

Attachments are cached locally before dispatch. Images are passed to Codex as image inputs; other
files are represented by their local cached paths. Relay refuses files larger than 25 MiB.

After a restart, Relay pages through messages after each channel's persisted cursor and ingests up
to the configured history scan limit (1,000 by default). Successfully accepted messages receive a
check-mark reaction. Very long outages can exceed that finite scan window; review the task channel
after recovery instead of assuming unlimited backlog replay.

## Current UX limits

- Schema routing coverage does not mean every method has unique handcrafted Discord presentation.
- Long final answers show the first page in the embed and attach complete Markdown; they do not yet
  provide interactive page navigation through every page.
- High-volume command/realtime item output is not a terminal emulator. Use `/terminals` for
  background-terminal lifecycle and the workspace for full generated artifacts.
- Generated images and other non-text Codex items do not all have equally rich Discord rendering.
- Provider sign-in, connector authorization, Discord application creation, and server creation
  remain provider-owned UI steps.

See [COVERAGE.md](COVERAGE.md) for the measured protocol matrix.
