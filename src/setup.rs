use std::{
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use serenity::{all::GuildId, http::Http};

use crate::{
    config::{self, Config},
    discord::{bot, provision},
    windows,
};

const GUILD_INSTALL_WAIT: std::time::Duration = std::time::Duration::from_secs(300);
const GUILD_INSTALL_POLL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
pub struct SetupOptions {
    pub guild_id: Option<u64>,
    pub owner_user_id: Option<u64>,
    pub token: Option<String>,
    pub default_cwd: Option<PathBuf>,
}

pub async fn run(options: SetupOptions) -> Result<()> {
    println!("Codex Discord Relay setup");
    println!("Needs a private Discord server and bot token. Discord apps cannot create servers.");

    let SetupOptions {
        guild_id: requested_guild_id,
        owner_user_id,
        token,
        default_cwd,
    } = options;

    let token = match token {
        Some(token) => token,
        None => match std::env::var("CODEX_DISCORD_TOKEN") {
            Ok(token) => token,
            Err(_) => rpassword::prompt_password(
                "Discord bot token (stored in Windows Credential Manager): ",
            )?,
        },
    };
    let http = Http::new(&token);
    let current_user = http
        .get_current_user()
        .await
        .context("Discord token validation failed")?;
    let application = http
        .get_current_application_info()
        .await
        .context("cannot inspect Discord application")?;
    http.set_application_id(application.id);
    let mut guilds = http
        .get_guilds(None, None)
        .await
        .context("cannot list bot guilds")?;
    let mut guild_id = match requested_guild_id {
        Some(id) => Some(id),
        None if guilds.len() == 1 => {
            let guild = &guilds[0];
            println!("Using the bot's only server: {} ({})", guild.name, guild.id);
            Some(guild.id.get())
        }
        None if guilds.len() > 1 => {
            println!("The bot is installed in multiple servers:");
            for guild in &guilds {
                println!("  {} — {}", guild.id, guild.name);
            }
            Some(prompt_u64("Discord server/guild ID: ")?)
        }
        None => None,
    };

    if guild_id.is_none_or(|id| !guilds.iter().any(|guild| guild.id.get() == id)) {
        let mut invite = format!(
            "https://discord.com/oauth2/authorize?client_id={}&permissions={}&scope=bot%20applications.commands",
            application.id,
            provision::required_bot_permissions().bits(),
        );
        if let Some(id) = guild_id {
            invite.push_str(&format!("&guild_id={id}&disable_guild_select=true"));
        }
        println!("Opening Discord authorization. Approve the bot for your private server.");
        println!("If the browser does not open, use:\n{invite}");
        if let Err(error) = windows::open_https_url(&invite) {
            tracing::warn!(%error, "could not open Discord authorization page");
        }
        let deadline = tokio::time::Instant::now() + GUILD_INSTALL_WAIT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "bot installation was not observed within five minutes; authorize it and rerun setup:\n{invite}"
                );
            }
            tokio::time::sleep(GUILD_INSTALL_POLL).await;
            guilds = http
                .get_guilds(None, None)
                .await
                .context("cannot refresh bot guild membership")?;
            let installed = match guild_id {
                Some(id) => guilds.iter().any(|guild| guild.id.get() == id),
                None => guilds
                    .first()
                    .map(|guild| {
                        guild_id = Some(guild.id.get());
                        true
                    })
                    .unwrap_or(false),
            };
            if installed {
                println!("Discord authorization detected; continuing automatic provisioning.");
                break;
            }
        }
    }
    let guild_id = guild_id.context("Discord authorization did not select a server")?;
    let guild = GuildId::new(guild_id);
    let bot_member = guild
        .member(&http, current_user.id)
        .await
        .context("cannot inspect bot permissions in the target guild")?;
    let partial_guild = guild
        .to_partial_guild(&http)
        .await
        .context("cannot inspect target guild roles")?;
    let owner_user_id = owner_user_id.unwrap_or_else(|| {
        println!("Using the Discord server owner as the relay owner.");
        partial_guild.owner_id.get()
    });
    anyhow::ensure!(
        owner_user_id == partial_guild.owner_id.get(),
        "relay owner must be the Discord server owner so private-channel and GOD-mode isolation cannot be bypassed by the guild owner"
    );
    guild.member(&http, owner_user_id).await.with_context(|| {
        format!("owner user {owner_user_id} is not a member of guild {guild_id}")
    })?;
    let required = provision::required_bot_permissions();
    anyhow::ensure!(
        partial_guild
            .member_permissions(&bot_member)
            .contains(required),
        "bot lacks required guild permissions; reinstall/raise its role, then rerun setup"
    );
    let config = Config {
        guild_id,
        owner_user_id,
        default_cwd: default_cwd.or_else(default_working_directory),
        codex_executable: None,
        god_session_minutes: 10,
        history_scan_limit: 1000,
        prune_at_channels: 450,
        prune_to_channels: 425,
    };
    config.validate()?;
    provision::ensure_layout(&http, &config).await?;
    bot::register_commands(&http, guild_id)
        .await
        .context("cannot register Discord slash commands")?;
    config::save_token(&token)?;
    config::save(&config)?;

    println!("Authenticated bot: {}", current_user.name);
    println!("Provisioned private control/task layout in guild {guild_id}.");
    println!("Next: run `codex-discord set-god-password`, then start the relay.");
    Ok(())
}

fn prompt_u64(label: &str) -> Result<u64> {
    print!("{label}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    input
        .trim()
        .parse()
        .with_context(|| format!("invalid number for {label}"))
}

fn default_working_directory() -> Option<PathBuf> {
    let user_profile = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    let codex = user_profile.join("Documents").join("Codex");
    std::fs::create_dir_all(&codex).ok()?;
    Some(codex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serenity::all::Permissions;

    #[test]
    fn provisioning_permissions_include_role_and_channel_management() {
        let permissions = provision::required_bot_permissions();
        assert!(!permissions.contains(Permissions::MANAGE_ROLES));
        assert!(permissions.contains(Permissions::MANAGE_CHANNELS));
        assert!(permissions.contains(Permissions::MANAGE_MESSAGES));
        assert!(permissions.contains(Permissions::USE_APPLICATION_COMMANDS));
    }
}
