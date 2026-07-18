# Release process

Relay releases must be reproducible, secret-free, and tested against both protocol fixtures and a
real installed Codex Desktop runtime.

## 1. Set release scope

1. Choose the semantic version and update `version` in `Cargo.toml`.
2. Move relevant entries from `Unreleased` in `CHANGELOG.md` to a dated version section.
3. Update public docs when commands, permissions, config, security policy, retained data, or live
   coverage semantics change.
4. Regenerate `Cargo.lock` only through Cargo and review dependency changes.

## 2. Run static and fixture gates

```powershell
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings -A clippy::pedantic
```

Release profile uses fat LTO, one codegen unit, abort-on-panic, and symbol stripping. The crate
forbids unsafe code in Relay's source.

## 3. Run live gates

Use a test Discord application and private server, never production credentials in CI logs.

```powershell
cargo test --lib --locked -- --ignored --nocapture
cargo build --release --locked
.\target\release\codex-discord.exe doctor --deep
```

Then exercise the end-to-end matrix in [COVERAGE.md](COVERAGE.md), including one normal turn, one
approval, restart recovery, and a GOD activate/revoke/cleanup cycle.

Record exact Codex Desktop/app-server version used for the live run. A green fixture suite alone is
not proof that a newer auto-updated Desktop schema remains compatible.

## 4. Security and secret audit

Before packaging:

```powershell
git status --short
git grep -n -I -E 'CODEX_DISCORD_TOKEN|discord(.{0,16})token|!god|password' -- . `
  ':(exclude)Cargo.lock' ':(exclude)docs/**' ':(exclude)README.md'
```

Review every match manually. Confirm release archives exclude:

- `.env` and local config overrides;
- `relay.sqlite3`, WAL/SHM files, logs, and audit exports;
- GOD password hashes;
- attachment/runtime/schema caches;
- `target` except the intended release executable;
- bot tokens, Discord IDs from private environments, task content, and user paths.

If any real token ever entered a message, terminal transcript, commit, artifact, or CI log, revoke
it in Discord Developer Portal before publishing. Removing the text is not enough.

## 5. Build and inspect artifact

```powershell
cargo build --release --locked
$exe = '.\target\release\codex-discord.exe'
Get-Item $exe | Select-Object Name,Length,LastWriteTimeUtc
Get-FileHash $exe -Algorithm SHA256
```

Run the final executable—not a debug build—through `--version`, `doctor --deep`, setup repair, and a
real turn. Test installer and uninstaller in a disposable Windows user profile or VM.

## 6. Performance reporting

Performance release notes include:

1. document CPU model, RAM, Windows build, Rust version, Relay commit, Codex version, Discord state,
   and measurement commands;
2. measure the release binary after a fixed idle settle period;
3. report Relay and Codex child separately and combined;
4. separate working set, private memory, binary size, startup-to-ready time, idle CPU, and active
   turn latency;
5. repeat runs and publish sample count plus median/range;
6. compare against a named baseline commit using the same machine, config, task, and measurement
   method.

Network/model latency and local Relay overhead are reported as separate measurements.

## 7. Package and publish

Recommended Windows release contents:

```text
codex-discord.exe
scripts/install.ps1
scripts/uninstall.ps1
README.md
LICENSE
CHANGELOG.md
SECURITY.md
docs/
SHA256SUMS.txt
```

Publish the checksum beside the archive. Sign the executable/archive when a trusted code-signing
identity is available. Create the release from an annotated tag matching `Cargo.toml` version.

Release notes must state:

- supported Windows architecture and Rust MSRV for source builds;
- required Codex Desktop and Discord setup;
- security-impacting changes and migration steps;
- audited Codex schema version and coverage terminology from [COVERAGE.md](COVERAGE.md);
- known limits and any unverified live flow;
- measured performance only when accompanied by reproducible methodology.

## 8. Post-release check

1. Download the public artifact as a new user would.
2. Verify its SHA-256 checksum.
3. Install into a clean private Discord server.
4. Run `doctor --deep` and one harmless task.
5. Verify uninstall behavior with and without `-KeepConfig`.
6. Keep prior artifacts available until the new release is proven recoverable.
