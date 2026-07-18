use std::collections::HashMap;

use anyhow::{Context, Result};
use serenity::{
    all::{
        ChannelId, ChannelType, GuildId, PermissionOverwrite, PermissionOverwriteType, Permissions,
        UserId,
    },
    builder::{
        CreateChannel, CreateEmbed, CreateEmbedFooter, CreateMessage, EditChannel, EditMessage,
    },
    http::Http,
};

use crate::{config::Config, discord::components, state::StateStore};

pub const CONTROL_CATEGORY: &str = "00 • CONTROL";
pub const RUNNING_CATEGORY: &str = "10 • RUNNING";
pub const NEEDS_USER_CATEGORY: &str = "20 • NEEDS-YOU";
pub const DONE_CATEGORY_PREFIX: &str = "30 • DONE";
pub const FAILED_CATEGORY: &str = "40 • FAILED";

/// Guild permissions the bot needs — the single source for setup's OAuth
/// invite, its preflight check, and `doctor --deep`. Deliberately excludes
/// `MANAGE_ROLES`: isolation is enforced with per-channel overwrites only.
#[must_use]
pub fn required_bot_permissions() -> Permissions {
    Permissions::VIEW_CHANNEL
        | Permissions::SEND_MESSAGES
        | Permissions::READ_MESSAGE_HISTORY
        | Permissions::MANAGE_CHANNELS
        | Permissions::MANAGE_MESSAGES
        | Permissions::EMBED_LINKS
        | Permissions::ATTACH_FILES
        | Permissions::ADD_REACTIONS
        | Permissions::USE_APPLICATION_COMMANDS
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub control_category: ChannelId,
    pub running_category: ChannelId,
    pub needs_user_category: ChannelId,
    pub done_category: ChannelId,
    pub failed_category: ChannelId,
    pub new_task_channel: ChannelId,
    pub existing_tasks_channel: ChannelId,
    pub runner_status_channel: ChannelId,
    pub audit_log_channel: ChannelId,
}

pub async fn ensure_layout(http: &Http, config: &Config) -> Result<Layout> {
    let guild = GuildId::new(config.guild_id);
    let bot_user = http.get_current_user().await?.id;
    let private_permissions =
        private_overwrites(guild, UserId::new(config.owner_user_id), bot_user);
    let mut channels = guild
        .channels(http)
        .await
        .context("cannot list Discord channels")?;

    let control = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::category(CONTROL_CATEGORY),
        &private_permissions,
    )
    .await?;
    let running = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::category(RUNNING_CATEGORY),
        &private_permissions,
    )
    .await?;
    let needs_user = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::category(NEEDS_USER_CATEGORY),
        &private_permissions,
    )
    .await?;
    let done_name = format!(
        "{DONE_CATEGORY_PREFIX} {}",
        chrono::Utc::now().format("%Y-%m")
    );
    let done = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::category(&done_name),
        &private_permissions,
    )
    .await?;
    let failed = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::category(FAILED_CATEGORY),
        &private_permissions,
    )
    .await?;

    let new_task = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::text(
            "new-task",
            control,
            "Start Codex tasks: /new, the ➕ button, or the control card.",
        ),
        &private_permissions,
    )
    .await?;
    let existing = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::text(
            "existing-tasks",
            control,
            "Browse, search, and reopen Codex Desktop tasks with /tasks.",
        ),
        &private_permissions,
    )
    .await?;
    let status = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::text(
            "runner-status",
            control,
            "Live relay health, Codex runner state, and GOD-mode status.",
        ),
        &private_permissions,
    )
    .await?;
    let audit = ensure_channel(
        http,
        guild,
        &mut channels,
        ChannelSpec::text(
            "audit-log",
            control,
            "Audit trail of relay actions and Codex notifications.",
        ),
        &private_permissions,
    )
    .await?;

    ensure_control_dashboard(http, new_task).await?;
    Ok(Layout {
        control_category: control,
        running_category: running,
        needs_user_category: needs_user,
        done_category: done,
        failed_category: failed,
        new_task_channel: new_task,
        existing_tasks_channel: existing,
        runner_status_channel: status,
        audit_log_channel: audit,
    })
}

pub async fn create_task_channel_for_state(
    http: &Http,
    config: &Config,
    store: &StateStore,
    layout: &Layout,
    title: &str,
    thread_id: &str,
    state: crate::models::TaskState,
) -> Result<ChannelId> {
    prune_completed_mirrors(http, config, store, layout).await?;
    let name = unique_task_channel_name(title, thread_id);
    let category = target_category_for_state(http, config, state).await?;
    let bot_user = http.get_current_user().await?.id;
    let permissions = private_overwrites(
        GuildId::new(config.guild_id),
        UserId::new(config.owner_user_id),
        bot_user,
    );
    let channel = GuildId::new(config.guild_id)
        .create_channel(
            http,
            CreateChannel::new(name)
                .kind(ChannelType::Text)
                .category(category)
                .permissions(permissions)
                .topic(format!("Codex task {thread_id}")),
        )
        .await
        .context("cannot create private task channel")?;
    Ok(channel.id)
}

async fn move_task_channel(http: &Http, channel: ChannelId, category: ChannelId) -> Result<()> {
    channel
        .edit(http, EditChannel::new().category(category))
        .await?;
    Ok(())
}

pub async fn rename_task_channel(
    http: &Http,
    channel: ChannelId,
    title: &str,
    thread_id: &str,
) -> Result<()> {
    channel
        .edit(
            http,
            EditChannel::new().name(unique_task_channel_name(title, thread_id)),
        )
        .await?;
    Ok(())
}

pub async fn repair_private_channel(
    http: &Http,
    config: &Config,
    channel: ChannelId,
) -> Result<()> {
    let permissions = private_overwrites(
        GuildId::new(config.guild_id),
        UserId::new(config.owner_user_id),
        http.get_current_user().await?.id,
    );
    channel
        .edit(http, EditChannel::new().permissions(permissions))
        .await?;
    Ok(())
}

pub async fn is_private_channel(http: &Http, config: &Config, channel: ChannelId) -> bool {
    let Ok(bot) = http.get_current_user().await else {
        return false;
    };
    is_private_channel_for_bot(http, config, channel, bot.id).await
}

pub async fn verify_guild_isolation(http: &Http, config: &Config) -> Result<()> {
    let bot = http.get_current_user().await?;
    let guild_id = GuildId::new(config.guild_id);
    let guild = guild_id.to_partial_guild(http).await?;
    anyhow::ensure!(
        guild.owner_id.get() == config.owner_user_id,
        "configured relay owner is not Discord server owner"
    );
    let members = guild_id.members(http, None, None).await?;
    anyhow::ensure!(
        !members.iter().any(|member| {
            member.user.id != guild.owner_id
                && member.user.id != bot.id
                && guild
                    .member_permissions(member)
                    .contains(Permissions::ADMINISTRATOR)
        }),
        "Discord server has an administrator other than relay owner or bot"
    );
    Ok(())
}

pub async fn is_private_channel_for_bot(
    http: &Http,
    config: &Config,
    channel: ChannelId,
    bot: UserId,
) -> bool {
    let Ok(serenity::all::Channel::Guild(channel)) = channel.to_channel(http).await else {
        return false;
    };
    if channel.guild_id.get() != config.guild_id {
        return false;
    }
    let guild_id = GuildId::new(config.guild_id);
    let everyone = guild_id.everyone_role();
    let owner = UserId::new(config.owner_user_id);
    let mut everyone_denied = false;
    let mut owner_allowed = false;
    let mut bot_allowed = false;
    for overwrite in &channel.permission_overwrites {
        match overwrite.kind {
            PermissionOverwriteType::Role(id) if id == everyone => {
                everyone_denied = overwrite.deny.contains(Permissions::VIEW_CHANNEL)
                    && !overwrite.allow.contains(Permissions::VIEW_CHANNEL);
            }
            PermissionOverwriteType::Member(id) if id == owner => {
                owner_allowed = overwrite.allow.contains(Permissions::VIEW_CHANNEL);
            }
            PermissionOverwriteType::Member(id) if id == bot => {
                bot_allowed = overwrite.allow.contains(Permissions::VIEW_CHANNEL);
            }
            _ if overwrite.allow.contains(Permissions::VIEW_CHANNEL) => return false,
            _ => {}
        }
    }
    everyone_denied && owner_allowed && bot_allowed
}

/// Move a task channel into the category that mirrors `state`. Every state
/// transition routes through here so category placement stays in one place.
pub async fn move_task_to_state(
    http: &Http,
    config: &Config,
    channel: ChannelId,
    state: crate::models::TaskState,
) -> Result<()> {
    let category = target_category_for_state(http, config, state).await?;
    move_task_channel(http, channel, category).await
}

async fn target_category_for_state(
    http: &Http,
    config: &Config,
    state: crate::models::TaskState,
) -> Result<ChannelId> {
    let base = match state {
        crate::models::TaskState::Done | crate::models::TaskState::Idle => {
            format!(
                "{DONE_CATEGORY_PREFIX} {}",
                chrono::Utc::now().format("%Y-%m")
            )
        }
        crate::models::TaskState::Failed => FAILED_CATEGORY.to_owned(),
        crate::models::TaskState::NeedsUser => NEEDS_USER_CATEGORY.to_owned(),
        crate::models::TaskState::Running => RUNNING_CATEGORY.to_owned(),
    };
    let guild = GuildId::new(config.guild_id);
    let channels = guild.channels(http).await?;
    let mut categories: Vec<_> = channels
        .values()
        .filter(|channel| channel.kind == ChannelType::Category && channel.name.starts_with(&base))
        .collect();
    categories.sort_by_key(|channel| channel.name.clone());
    for category in categories {
        let children = channels
            .values()
            .filter(|channel| channel.parent_id == Some(category.id))
            .count();
        if children < 50 {
            return Ok(category.id);
        }
    }
    let shard = channels
        .values()
        .filter(|channel| channel.kind == ChannelType::Category && channel.name.starts_with(&base))
        .count()
        + 1;
    let name = if shard == 1 {
        base
    } else {
        format!("{base} • {shard}")
    };
    let created = guild
        .create_channel(
            http,
            CreateChannel::new(name)
                .kind(ChannelType::Category)
                .permissions(private_overwrites(
                    guild,
                    UserId::new(config.owner_user_id),
                    http.get_current_user().await?.id,
                )),
        )
        .await?;
    Ok(created.id)
}

/// The desired shape of one provisioned channel or category.
struct ChannelSpec<'a> {
    name: &'a str,
    kind: ChannelType,
    category: Option<ChannelId>,
    topic: Option<&'a str>,
}

impl<'a> ChannelSpec<'a> {
    fn category(name: &'a str) -> Self {
        Self {
            name,
            kind: ChannelType::Category,
            category: None,
            topic: None,
        }
    }

    fn text(name: &'a str, category: ChannelId, topic: &'a str) -> Self {
        Self {
            name,
            kind: ChannelType::Text,
            category: Some(category),
            topic: Some(topic),
        }
    }
}

async fn ensure_channel(
    http: &Http,
    guild: GuildId,
    channels: &mut HashMap<ChannelId, serenity::model::channel::GuildChannel>,
    spec: ChannelSpec<'_>,
    permissions: &[PermissionOverwrite],
) -> Result<ChannelId> {
    if let Some(channel) = channels
        .values()
        .find(|c| c.name == spec.name && c.kind == spec.kind)
    {
        let mut edit = EditChannel::new().permissions(permissions.iter().cloned());
        if let Some(parent) = spec.category {
            edit = edit.category(parent);
        }
        if let Some(topic) = spec.topic {
            edit = edit.topic(topic);
        }
        channel.id.edit(http, edit).await?;
        return Ok(channel.id);
    }
    let mut builder = CreateChannel::new(spec.name)
        .kind(spec.kind)
        .permissions(permissions.to_vec());
    if let Some(parent) = spec.category {
        builder = builder.category(parent);
    }
    if let Some(topic) = spec.topic {
        builder = builder.topic(topic);
    }
    let created = guild
        .create_channel(http, builder)
        .await
        .with_context(|| format!("cannot create Discord channel/category {}", spec.name))?;
    let id = created.id;
    channels.insert(id, created);
    Ok(id)
}

async fn ensure_control_dashboard(http: &Http, channel: ChannelId) -> Result<()> {
    let messages = channel
        .messages(http, serenity::builder::GetMessages::new().limit(25))
        .await?;
    if let Some(existing) = messages.into_iter().find(|message| {
        message.author.bot
            && message
                .embeds
                .iter()
                .any(|embed| embed.title.as_deref() == Some("Codex control center"))
    }) {
        channel
            .edit_message(
                http,
                existing.id,
                EditMessage::new()
                    .embed(control_dashboard_embed())
                    .components(components::control_buttons()),
            )
            .await?;
        if !existing.pinned
            && let Err(error) = existing.pin(http).await
        {
            tracing::warn!(%error, "could not pin control dashboard; grant Manage Messages to enable pinning");
        }
        return Ok(());
    }
    let dashboard = channel
        .send_message(
            http,
            CreateMessage::new()
                .embed(control_dashboard_embed())
                .components(components::control_buttons()),
        )
        .await?;
    if let Err(error) = dashboard.pin(http).await {
        tracing::warn!(%error, "could not pin control dashboard; grant Manage Messages to enable pinning");
    }
    Ok(())
}

fn control_dashboard_embed() -> CreateEmbed {
    CreateEmbed::new()
        .title("Codex control center")
        .description("Launch work, reopen any Codex Desktop task, or temporarily unlock GOD mode.")
        .color(0x5865F2)
        .field(
            "New task",
            "Creates a private channel and starts Codex.",
            true,
        )
        .field("Browse", "Search and reopen existing Desktop tasks.", true)
        .field(
            "Offline",
            "Messages queue here until your Windows runner returns.",
            false,
        )
        .footer(CreateEmbedFooter::new("Relay dashboard v1"))
}

async fn prune_completed_mirrors(
    http: &Http,
    config: &Config,
    store: &StateStore,
    _layout: &Layout,
) -> Result<()> {
    let guild = GuildId::new(config.guild_id);
    let channels = guild.channels(http).await?;
    if channels.len() < config.prune_at_channels {
        return Ok(());
    }
    let remove = channels.len().saturating_sub(config.prune_to_channels);
    let done_categories: Vec<_> = channels
        .values()
        .filter(|channel| {
            channel.kind == ChannelType::Category && channel.name.starts_with(DONE_CATEGORY_PREFIX)
        })
        .map(|channel| channel.id)
        .collect();
    let mut completed: Vec<_> = channels
        .values()
        .filter(|channel| {
            channel
                .parent_id
                .is_some_and(|parent| done_categories.contains(&parent))
                && channel.kind == ChannelType::Text
        })
        .collect();
    completed.sort_by_key(|channel| channel.id);
    for channel in completed.into_iter().take(remove) {
        channel.delete(http).await?;
        store.detach_channel_id(channel.id.get()).await?;
    }
    Ok(())
}

fn private_overwrites(guild: GuildId, owner: UserId, bot: UserId) -> Vec<PermissionOverwrite> {
    vec![
        PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            kind: PermissionOverwriteType::Role(guild.everyone_role()),
        },
        PermissionOverwrite {
            allow: Permissions::VIEW_CHANNEL
                | Permissions::SEND_MESSAGES
                | Permissions::READ_MESSAGE_HISTORY
                | Permissions::ATTACH_FILES
                | Permissions::EMBED_LINKS
                | Permissions::USE_APPLICATION_COMMANDS,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Member(owner),
        },
        PermissionOverwrite {
            allow: Permissions::VIEW_CHANNEL
                | Permissions::SEND_MESSAGES
                | Permissions::READ_MESSAGE_HISTORY
                | Permissions::ATTACH_FILES
                | Permissions::EMBED_LINKS
                | Permissions::ADD_REACTIONS
                | Permissions::MANAGE_CHANNELS
                | Permissions::MANAGE_MESSAGES
                | Permissions::USE_APPLICATION_COMMANDS,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Member(bot),
        },
    ]
}

fn unique_task_channel_name(title: &str, thread_id: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(7)
        .collect::<Vec<_>>()
        .join("-");
    let suffix: String = thread_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(6)
        .collect();
    let base = if slug.is_empty() { "codex-task" } else { &slug };
    format!("{}-{}", &base[..base.len().min(80)], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_are_safe_and_bounded() {
        let name = unique_task_channel_name("Build AMAZING Discord UX!!!", "019f-abcdef");
        assert_eq!(name, "build-amazing-discord-ux-019fab");
        assert!(name.len() <= 100);
    }
}
