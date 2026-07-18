use std::{
    io::{Read, stdin},
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use codex_discord_relay::{
    codex::CodexClient,
    config,
    discord::{bot, provision},
    security::{Argon2idPasswordManager, StoredPasswordHash},
    setup::{self, SetupOptions},
    windows,
};
use secrecy::SecretString;
use serenity::all::{ChannelType, GuildId};
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

#[derive(Debug, Parser)]
#[command(name = "codex-discord", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Configure Discord IDs/token and provision the server layout.
    Setup {
        #[arg(long)]
        guild_id: Option<u64>,
        #[arg(long)]
        owner_user_id: Option<u64>,
        #[arg(long)]
        default_cwd: Option<PathBuf>,
    },
    /// Start Discord gateway and local Codex app-server bridge.
    Run,
    /// Install this binary, register startup, and optionally launch the relay.
    Install {
        #[arg(long)]
        target_dir: Option<PathBuf>,
        /// Register startup with highest available user privilege.
        #[arg(long)]
        highest: bool,
        /// Do not launch the relay after installation.
        #[arg(long)]
        no_start: bool,
        /// Run interactive Discord setup and create GOD password if absent.
        #[arg(long)]
        configure: bool,
    },
    /// Set or replace the Argon2id GOD-mode password.
    SetGodPassword {
        /// Read the password and confirmation as two newline-delimited values from stdin.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Install automatic startup at Windows logon.
    InstallStartup {
        /// Run with highest available user privilege. May require admin approval.
        #[arg(long)]
        highest: bool,
    },
    /// Remove automatic Windows startup.
    UninstallStartup,
    /// Validate configuration, Discord, Codex, and startup state.
    Doctor {
        /// Verify live guild layout, privacy, commands, GOD hash, cwd, and runner.
        #[arg(long)]
        deep: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Setup {
            guild_id,
            owner_user_id,
            default_cwd,
        } => {
            setup::run(SetupOptions {
                guild_id,
                owner_user_id,
                default_cwd,
                token: None,
            })
            .await
        }
        Command::Run => codex_discord_relay::discord::run().await,
        Command::Install {
            target_dir,
            highest,
            no_start,
            configure,
        } => {
            if configure {
                setup::run(SetupOptions::default()).await?;
                if config::load_god_password_hash().is_err() {
                    set_god_password(false)?;
                }
            }
            let source = std::env::current_exe().context("cannot locate current executable")?;
            let installed = windows::install_binary(&source, target_dir.as_deref())?;
            windows::install_startup(&installed, highest)?;
            if !no_start {
                if windows::relay_process_count()? <= 1 {
                    windows::launch_relay(&installed)?;
                } else {
                    println!("Relay already running; skipped duplicate launch.");
                }
            }
            println!("Installed {}", installed.display());
            Ok(())
        }
        Command::SetGodPassword { password_stdin } => set_god_password(password_stdin),
        Command::InstallStartup { highest } => {
            let executable = std::env::current_exe().context("cannot locate current executable")?;
            windows::install_startup(&executable, highest)?;
            println!("Installed Windows startup task.");
            Ok(())
        }
        Command::UninstallStartup => {
            windows::uninstall_startup()?;
            println!("Removed Windows startup task.");
            Ok(())
        }
        Command::Doctor { deep } => doctor(deep).await,
    }
}

fn set_god_password(password_stdin: bool) -> Result<()> {
    ensure_relay_stopped_for_password_replacement()?;
    let (first, second) = if password_stdin {
        let mut input = Zeroizing::new(String::new());
        stdin()
            .lock()
            .take(4098)
            .read_to_string(&mut input)
            .context("failed to read password from stdin")?;
        parse_password_pair(&input)?
    } else {
        (
            Zeroizing::new(rpassword::prompt_password(
                "New GOD-mode password (12+ chars): ",
            )?),
            Zeroizing::new(rpassword::prompt_password("Confirm password: ")?),
        )
    };
    anyhow::ensure!(*first == *second, "passwords do not match");
    let hash = Argon2idPasswordManager::default().hash(SecretString::from(first.to_string()))?;
    ensure_relay_stopped_for_password_replacement()?;
    config::save_god_password_hash(hash.encoded())?;
    println!(
        "Stored Argon2id hash. Plaintext was not persisted. Start the relay to load the new password."
    );
    Ok(())
}

fn ensure_relay_stopped_for_password_replacement() -> Result<()> {
    validate_password_replacement_process_count(windows::relay_process_count()?)
}

fn validate_password_replacement_process_count(process_count: usize) -> Result<()> {
    anyhow::ensure!(
        process_count <= 1,
        "refusing to replace the GOD password while a separate relay process is running ({process_count} codex-discord processes detected). Stop the relay, rerun `codex-discord set-god-password`, then restart it so only the new password can remain active"
    );
    Ok(())
}

fn parse_password_pair(input: &str) -> Result<(Zeroizing<String>, Zeroizing<String>)> {
    let mut lines = input.lines();
    let first = Zeroizing::new(
        lines
            .next()
            .context("stdin omitted the GOD-mode password")?
            .trim_start_matches('\u{feff}')
            .to_owned(),
    );
    let second = Zeroizing::new(
        lines
            .next()
            .context("stdin omitted the password confirmation")?
            .to_owned(),
    );
    anyhow::ensure!(
        lines.all(str::is_empty),
        "stdin must contain only the password and confirmation"
    );
    Ok((first, second))
}

async fn doctor(deep: bool) -> Result<()> {
    let config = config::load()?;
    let token = config::read_token()?;
    let http = serenity::http::Http::new(&token);
    let user = http
        .get_current_user()
        .await
        .context("Discord connection failed")?;
    let application = http
        .get_current_application_info()
        .await
        .context("Discord application lookup failed")?;
    http.set_application_id(application.id);
    println!("Discord: OK ({})", user.name);
    let codex = if let Some(path) = &config.codex_executable {
        CodexClient::new(codex_discord_relay::codex::CodexCommand::new(path.clone()))
    } else {
        let command =
            tokio::task::spawn_blocking(codex_discord_relay::codex::CodexCommand::discover)
                .await??;
        CodexClient::new(command)
    };
    codex
        .connect()
        .await
        .context("Codex app-server connection failed")?;
    println!("Codex app-server: OK");
    println!("Startup: {}", windows::startup_status()?);
    println!("Data: {}", config::data_dir().display());
    if deep {
        deep_doctor(&config, &http, user.id).await?;
    }
    codex.close().await;
    Ok(())
}

async fn deep_doctor(
    config: &config::Config,
    http: &serenity::http::Http,
    bot_user: serenity::all::UserId,
) -> Result<()> {
    let mut failures = Vec::new();
    let guild = GuildId::new(config.guild_id);

    record_check(
        &mut failures,
        "owner membership",
        guild
            .member(http, config.owner_user_id)
            .await
            .map(|_| ())
            .map_err(Into::into),
    );

    let required = provision::required_bot_permissions();
    let bot_permissions = async {
        let member = guild.member(http, bot_user).await?;
        let partial = guild.to_partial_guild(http).await?;
        anyhow::ensure!(
            partial.member_permissions(&member).contains(required),
            "bot lacks required guild permissions"
        );
        Ok(())
    }
    .await;
    record_check(&mut failures, "bot permissions", bot_permissions);

    record_check(
        &mut failures,
        "guild isolation",
        provision::verify_guild_isolation(http, config).await,
    );

    match guild.channels(http).await {
        Ok(channels) => {
            let expected = [
                (provision::CONTROL_CATEGORY, ChannelType::Category, false),
                (provision::RUNNING_CATEGORY, ChannelType::Category, false),
                (provision::NEEDS_USER_CATEGORY, ChannelType::Category, false),
                (provision::DONE_CATEGORY_PREFIX, ChannelType::Category, true),
                (provision::FAILED_CATEGORY, ChannelType::Category, false),
                ("new-task", ChannelType::Text, false),
                ("existing-tasks", ChannelType::Text, false),
                ("runner-status", ChannelType::Text, false),
                ("audit-log", ChannelType::Text, false),
            ];
            for (name, kind, prefix) in expected {
                let found = channels.values().find(|channel| {
                    channel.kind == kind
                        && if prefix {
                            channel.name.starts_with(name)
                        } else {
                            channel.name == name
                        }
                });
                match found {
                    Some(channel) => {
                        let private = provision::is_private_channel(http, config, channel.id).await;
                        record_check(
                            &mut failures,
                            &format!("private channel {name}"),
                            if private {
                                Ok(())
                            } else {
                                Err(anyhow!("privacy overwrite mismatch"))
                            },
                        );
                    }
                    None => failures.push(format!("layout {name}: missing")),
                }
            }
        }
        Err(error) => failures.push(format!("guild layout: {error}")),
    }

    match http.get_guild_commands(guild).await {
        Ok(commands) => {
            let installed = commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let missing = bot::COMMAND_NAMES
                .iter()
                .copied()
                .filter(|name| !installed.contains(name))
                .collect::<Vec<_>>();
            record_check(
                &mut failures,
                "slash commands",
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(anyhow!("missing {}", missing.join(", ")))
                },
            );
        }
        Err(error) => failures.push(format!("slash commands: {error}")),
    }

    record_check(
        &mut failures,
        "default cwd",
        config.default_cwd.as_ref().map_or_else(
            || Err(anyhow!("not configured")),
            |path| {
                anyhow::ensure!(path.is_absolute(), "not absolute: {}", path.display());
                anyhow::ensure!(path.is_dir(), "not a directory: {}", path.display());
                Ok(())
            },
        ),
    );

    let hash_path = config::god_password_path();
    let god_hash = config::load_god_password_hash().and_then(|encoded| {
        StoredPasswordHash::parse(encoded.trim().to_owned()).map_err(Into::into)
    });
    record_check(&mut failures, "GOD Argon2id hash", god_hash.map(|_| ()));
    record_check(
        &mut failures,
        "GOD hash ACL",
        windows::verify_secret_file_acl(&hash_path),
    );

    record_check(
        &mut failures,
        "running relay process",
        windows::relay_process_count().and_then(|count| {
            anyhow::ensure!(
                count >= 2,
                "no separate `codex-discord run` process detected"
            );
            Ok(())
        }),
    );

    if std::env::var_os("CODEX_DISCORD_TOKEN").is_some() {
        println!("Deep: WARN (token loaded from environment; Credential Manager is safer)");
    }
    if failures.is_empty() {
        println!("Deep: OK");
        return Ok(());
    }
    for failure in &failures {
        eprintln!("Deep: FAIL ({failure})");
    }
    Err(anyhow!("deep doctor failed {} check(s)", failures.len()))
}

fn record_check(failures: &mut Vec<String>, label: &str, result: Result<()>) {
    match result {
        Ok(()) => println!("Deep: OK ({label})"),
        Err(error) => failures.push(format!("{label}: {error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_password_pair, validate_password_replacement_process_count};

    #[test]
    fn password_stdin_accepts_windows_bom_and_crlf() {
        let (first, second) = parse_password_pair(
            "\u{feff}correct horse battery staple\r\ncorrect horse battery staple\r\n",
        )
        .unwrap();
        assert_eq!(*first, *second);
    }

    #[test]
    fn password_stdin_rejects_extra_values() {
        assert!(parse_password_pair("one\ntwo\nthree\n").is_err());
    }

    #[test]
    fn password_replacement_refuses_a_running_relay() {
        assert!(validate_password_replacement_process_count(1).is_ok());
        let error = validate_password_replacement_process_count(2).unwrap_err();
        assert!(error.to_string().contains("refusing to replace"));
    }
}
