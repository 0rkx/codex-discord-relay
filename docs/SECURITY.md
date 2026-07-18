# Security model

Relay is a remote-control surface for the Windows account running Codex Desktop. It is designed for
one trusted owner in one private Discord server. It is not a shared-team bot or a sandbox boundary
between mutually untrusted users.

## Trust boundary

Every protected interaction is checked against live Discord state:

- exact configured owner user ID;
- exact configured guild ID;
- a channel with the expected private overwrites;
- the Codex task bound to that channel;
- the action's normal, owner-confirmed, or GOD authorization policy.

Setup requires the configured Relay owner to also be the Discord server owner. This prevents a
different guild owner from bypassing channel isolation through server administration. Relay gates
startup event handling when the live guild-isolation check fails.

This design still trusts Discord, the owner's Discord account, the Windows account, installed
Codex Desktop, configured MCP/apps/connectors, and the local network/OS security posture.

## Execution modes

### Normal mode

Normal turns request Codex `workspace-write` sandboxing and `on-request` approvals. Actions that
expose equivalent unrestricted local capability—host filesystem access, arbitrary MCP calls,
remote pairing, configuration/plugin/marketplace mutation, or raw RPC—are centrally classified
and require GOD mode.

Codex and Windows remain final enforcement layers. Relay's policy classification reduces accidental
exposure but is not a substitute for OS account isolation or connector-side authorization.

### GOD mode

GOD mode requests `danger-full-access` with approval policy `never`. It can read, change, execute,
or delete anything the Windows account can access. It cannot bypass UAC or grant permissions the
Windows account does not have.

Controls:

- password entry is accepted through a Discord modal only;
- local password enrollment is required before Relay starts;
- Argon2id uses 64 MiB memory, three iterations, four lanes, and a 32-byte output;
- password length is 12–512 characters;
- five failures within five minutes cause a 15-minute lockout;
- only one process-local GOD grant exists at a time;
- grant is bound to owner, guild, private channel, and task;
- TTL is configurable from one to ten minutes and defaults to ten;
- `/god_off` revokes the active grant;
- restart or replacement removes the in-memory grant;
- task dispatch, expiration, revocation, and cleanup use lifecycle guards;
- a previously privileged task is quarantined until Codex permissions are normalized.

Any channel message whose first token is `!god`, case-insensitively, is rejected and queued for
deletion even outside normal Relay channels. Deletion retries survive restart and store only
channel/message IDs plus failure metadata—not message content. If deletion cannot be durably queued,
Relay emits a critical audit/owner alert. Discord deletion is best effort; assume a posted secret
may already exist in notifications, client caches, backups, or moderation logs and rotate it.

## Secret storage

| Secret/data | Storage |
|---|---|
| Discord bot token | Windows Credential Manager by default |
| GOD password | Never persisted as plaintext; Argon2id PHC hash in an ACL-restricted file |
| Relay config | Local JSON; contains IDs and paths, not the bot token |
| Task mirror/audit/outbox | Local SQLite WAL database |
| Attachments | Local bounded cache; not encrypted by Relay |
| Discord messages | Discord service and clients, subject to Discord retention |
| Codex tasks/connectors | Codex Desktop and configured provider storage |

`CODEX_DISCORD_TOKEN` is supported as an operational fallback but exposes the token to the process
environment. Prefer Credential Manager. Relay redacts account/config views and GOD audit values,
but logs and databases still contain operational metadata. Protect the Windows profile and disk.

## Discord permissions

Relay requests only:

- View Channels
- Send Messages
- Read Message History
- Manage Channels
- Manage Messages
- Embed Links
- Attach Files
- Add Reactions
- Use Application Commands

It deliberately does not request Manage Roles. Message Content Intent is required for task-channel
messages, attachment ingestion, offline replay, and emergency `!god` deletion.

## Data retention

- Audit storage is capped at 100,000 rows and 30 days.
- Outbox storage is capped at 10,000 rows; sent rows older than one day and dead rows older than 30
  days are pruned.
- Attachment cache prunes files older than seven days and reduces use after crossing 512 MiB.
- Discord and Codex provider retention are outside Relay's control.

These are operational bounds, not secure-erasure guarantees. Use full-disk encryption and normal
Windows account controls if local data confidentiality matters.

## Fail-closed behavior

- Missing/invalid config, owner mismatch, wrong guild, public channel, missing GOD hash, invalid
  schema parameters, or absent installed method rejects the action.
- Unadvertised server capabilities are negotiated off and rejected if a peer sends them anyway.
- Dynamic HostBroker calls require an exact `codex_app` namespace/tool allowlist and a matching
  Relay task plus active turn. Arguments, time, concurrency, returned fields, secrets, and output
  size are bounded; the first exposed tools are read-only.
- A failed post-GOD normalization keeps the task quarantined and retries cleanup.
- A poison Discord delivery dead-letters after ten attempts and becomes visible in runner status.

One current limit requires operator attention: Codex notifications and server requests enter
bounded in-memory broadcast receivers. Notification lag is logged. Server-request lag is logged and
audited because a dropped approval may leave a Codex turn waiting without a Discord card. Interrupt
that task and investigate resource pressure; do not assume those requests are durable.

## Deployment guidance

1. Use a dedicated private Discord server with no untrusted members or administrator bots.
2. Enable MFA and a strong unique password on the owner Discord account.
3. Keep the bot token and GOD password separate and rotate either after suspected exposure.
4. Avoid `--highest` unless the work needs elevated rights.
5. Keep Codex Desktop, Windows, and Relay updated.
6. Run `doctor --deep` after setup, updates, permission changes, or token rotation.
7. Review `#audit-log` and `#runner-status` for isolation, deletion, outbox, or cleanup alerts.
8. Do not expose Relay through public guilds, shared owner accounts, remote shells, or copied data
   directories.

For reporting instructions, see repository-level [SECURITY.md](../SECURITY.md).
