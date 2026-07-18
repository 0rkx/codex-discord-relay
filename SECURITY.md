# Security policy

Relay can execute Codex actions on the local Windows account. Treat vulnerabilities involving
authorization, Discord isolation, GOD mode, secret handling, protocol validation, attachment paths,
or local execution as security-sensitive.

## Supported versions

Until the first stable release, only the newest published version receives security fixes.

| Version | Supported |
|---|---|
| Latest public preview | Yes |
| Older previews | No |

## Report a vulnerability

Do not open a public issue, discussion, pull request, or Discord message containing exploit details.

Use the repository host's private security-advisory feature. Include:

- affected Relay version/commit and Codex Desktop version;
- Windows and Discord configuration relevant to the issue;
- clear reproduction steps with secrets and private IDs removed;
- impact and expected trust-boundary behavior;
- whether a bot token, GOD password, task content, or local file may have been exposed;
- any proposed fix or temporary mitigation.

If private advisories are unavailable, contact the maintainer privately through the account or
release channel that published the artifact. Ask for a secure reporting channel before sending the
details.

Never send a live Discord bot token, GOD password, credential export, unredacted database, private
task content, or personally identifying path. Use synthetic replacements in reproductions.

## Immediate containment

If exposure may be active:

1. Stop `codex-discord.exe`.
2. Revoke/reset the Discord bot token in Developer Portal.
3. Revoke active connector/provider sessions when relevant.
4. Change the GOD password locally while Relay is stopped.
5. Review Discord audit logs, Relay `#audit-log`, local state, and affected workspaces.
6. Remove the bot from untrusted guilds and restore private channel overwrites.
7. Preserve sanitized evidence before deleting state needed for investigation.

Deleting a Discord message or commit does not prove the secret was not copied. Rotate it.

## Disclosure expectations

Maintainers should acknowledge a complete report, validate impact, prepare a fix and release note,
and coordinate disclosure with the reporter. Do not promise a fixed response time until the project
publishes a staffed security SLA.

Relay's full threat model and deployment guidance are in [docs/SECURITY.md](docs/SECURITY.md).
