# Windows setup

Relay is currently packaged for a private, single-owner Discord server on Windows.

## Requirements

- Windows 10 or Windows 11 on x86-64.
- Codex Desktop installed and signed in for the same Windows account that runs Relay.
- A private Discord server you own.
- A Discord application with a bot token.
- Message Content Intent enabled for that bot.
- A prebuilt `codex-discord.exe`, or Rust 1.88+ with the MSVC build tools for a source build.

No OpenAI API key, Node.js, npm, Python service, or standalone Codex OpenAI Bridge is required.

## 1. Create the Discord application

Discord does not allow a bot to create its own developer application or server.

1. Create an empty private server under the Discord account that will own Relay.
2. Open the [Discord Developer Portal](https://discord.com/developers/applications).
3. Create an application, add a bot, and enable **Message Content Intent**.
4. Reset/copy the bot token and keep it outside chat, screenshots, shell history, and source files.
5. Do not add untrusted members or other administrator bots to the server.

You do not need to assemble an OAuth permission integer by hand. Setup validates the token and, if
the bot is not installed, opens an invite containing Relay's exact required permissions.

## 2. Install Relay

From PowerShell in the release/source directory:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\install.ps1
```

The installer performs interactive Discord setup, stores the token in Windows Credential Manager,
copies the binary under `%LOCALAPPDATA%\CodexDiscordRelay\bin`, registers logon startup, and starts
the hidden runner. If Task Scheduler registration is denied by policy, it uses the current-user
Windows Run key.

Use elevated startup only when you intentionally want the relay and Codex child to inherit the
highest privileges available to your Windows account:

```powershell
.\scripts\install.ps1 -Highest
```

Highest-privilege startup increases impact of a compromised Discord account or GOD credential. It
is not needed for ordinary workspaces.

### Manual source flow

```powershell
cargo build --release --locked
.\target\release\codex-discord.exe setup
.\target\release\codex-discord.exe set-god-password
.\target\release\codex-discord.exe install
.\target\release\codex-discord.exe doctor --deep
```

`setup` can accept explicit values for unattended parts of provisioning:

```powershell
codex-discord.exe setup --guild-id 123456789012345678 `
  --owner-user-id 123456789012345678 `
  --default-cwd 'C:\Users\you\Documents\Codex'
```

The owner ID must equal the Discord server owner ID. The default working directory is
`%USERPROFILE%\Documents\Codex`; setup creates it when possible.

## 3. Set the GOD password locally

Stop the relay before replacing the password, then run:

```powershell
codex-discord.exe set-god-password
```

Password length is 12–512 characters. Relay stores only an Argon2id PHC hash and restricts its file
ACL. It refuses replacement while another `codex-discord.exe` process is running so an old password
cannot remain active in a live process.

For local automation, provide exactly two matching newline-delimited values on standard input:

```powershell
$secret = Read-Host 'New GOD password'
"$secret`n$secret" | codex-discord.exe set-god-password --password-stdin
Remove-Variable secret
```

Never put this password in a command-line argument, environment variable, Discord message, or
repository file.

## 4. Verify the installation

```powershell
codex-discord.exe doctor --deep
```

Deep doctor checks live Discord connectivity, owner membership, required bot permissions, private
channel overwrites, expected layout, registered commands, default working directory, GOD hash and
ACL, Codex app-server connectivity, startup registration, and a separate running relay process.

In Discord, confirm:

1. Control categories and channels are visible only to you and the bot.
2. `#runner-status` reports ready state.
3. `/new` creates a task channel.
4. A small test message receives a final answer.
5. `/god` opens a modal and `/god_off` revokes the session.

## Configuration and storage

Configuration and state are under `%LOCALAPPDATA%\Codex\CodexDiscordRelay`:

| Path | Contents |
|---|---|
| `config.json` | Guild ID, owner ID, working directory, pruning and history settings |
| `relay.sqlite3` plus WAL files | Task mirrors, cursors, approvals, audit, outbox, cleanup state |
| `god-password.argon2id` | Restricted Argon2id password hash |
| `runtime\codex.exe` | Hash-verified private copy of installed Desktop companion |
| `schemas\current` | Schema generated from installed Codex app-server |
| `attachments` | Bounded local attachment cache |

Token service: `codex-discord-relay`; account: `discord-bot-token` in Windows Credential Manager.

Supported environment overrides:

| Variable | Use | Security note |
|---|---|---|
| `CODEX_RELAY_CODEX_EXECUTABLE` | Absolute path to an existing Codex executable | Advanced override; Windows Desktop discovery is preferred |
| `CODEX_DISCORD_TOKEN` | Temporary token source for setup/run | Credential Manager is safer; deep doctor warns when this is set |
| `RUST_LOG` | Rust tracing filter, such as `info` or `codex_discord_relay=debug` | Debug logs may expose operational metadata; do not publish them blindly |

## Update

1. Stop the running relay.
2. Replace/install the new binary from a trusted, checksum-verified release.
3. Start Relay or sign out/in if using automatic startup.
4. Run `codex-discord.exe doctor --deep`.
5. Check `/capabilities` after Codex Desktop updates.

Setup and provisioning are idempotent; rerunning them repairs expected channels, overwrites,
dashboard, and guild commands without deleting Codex tasks.

## Uninstall

```powershell
.\scripts\uninstall.ps1
```

Preserve local configuration and state:

```powershell
.\scripts\uninstall.ps1 -KeepConfig
```

The default uninstall removes startup registration, installed binary, Relay database, cached
runtime, attachment cache, configuration, and GOD hash. It does not delete Discord channels,
the Discord application, or Codex Desktop tasks. Revoke/delete the bot token in the Developer
Portal when retiring the installation.

## Troubleshooting

| Symptom | Check |
|---|---|
| Bot token validation fails | Reset token in Developer Portal and rerun setup; never paste token into an issue |
| Bot lacks permissions | Raise/reinstall bot using setup's invite, then rerun setup |
| Isolation check fails | Remove broad channel overwrites or untrusted admin roles; rerun setup and deep doctor |
| Codex executable not found | Install/start Codex Desktop; use `CODEX_RELAY_CODEX_EXECUTABLE` only for a trusted explicit file |
| Commands missing | Rerun `setup` or restart Relay; commands are registered per guild |
| Task waits on approval with no card | Check logs/audit for a lagged server-request receiver and interrupt the affected task |
| Dead outbox count is nonzero | Inspect deleted/inaccessible task channels; reopen the task and retry relevant output |
| GOD password replacement refused | Stop the long-running Relay process, replace password, then restart |

Do not report a token, password, task content, local path, or raw unredacted config in a public issue.
