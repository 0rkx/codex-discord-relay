use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

pub const KEYRING_SERVICE: &str = "codex-discord-relay";
pub const TOKEN_ACCOUNT: &str = "discord-bot-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub guild_id: u64,
    pub owner_user_id: u64,
    pub default_cwd: Option<PathBuf>,
    pub codex_executable: Option<PathBuf>,
    #[serde(default = "default_god_minutes")]
    pub god_session_minutes: u64,
    #[serde(default = "default_history_limit")]
    pub history_scan_limit: u64,
    #[serde(default = "default_prune_at")]
    pub prune_at_channels: usize,
    #[serde(default = "default_prune_to")]
    pub prune_to_channels: usize,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.guild_id == 0 || self.owner_user_id == 0 {
            bail!("guild_id and owner_user_id must be non-zero");
        }
        if !(1..=10).contains(&self.god_session_minutes) {
            bail!("god_session_minutes must be between 1 and 10");
        }
        if self.prune_to_channels >= self.prune_at_channels || self.prune_at_channels >= 500 {
            bail!("channel pruning must satisfy prune_to < prune_at < 500");
        }
        if self
            .default_cwd
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            bail!("default_cwd must be an absolute path");
        }
        Ok(())
    }
}

#[must_use]
pub fn data_dir() -> PathBuf {
    ProjectDirs::from("dev", "Codex", "CodexDiscordRelay")
        .expect("Windows always exposes a local data directory")
        .data_local_dir()
        .to_path_buf()
}

#[must_use]
pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

#[must_use]
pub fn god_password_path() -> PathBuf {
    data_dir().join("god-password.argon2id")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "missing config at {}. Run `codex-discord setup`",
            path.display()
        )
    })?;
    let config: Config = serde_json::from_slice(&bytes).context("invalid config JSON")?;
    config.validate()?;
    Ok(config)
}

pub fn save(config: &Config) -> Result<()> {
    config.validate()?;
    let path = config_path();
    atomic_write(&path, &serde_json::to_vec_pretty(config)?, false)
}

pub fn read_token() -> Result<String> {
    if let Ok(token) = std::env::var("CODEX_DISCORD_TOKEN") {
        return Ok(token);
    }
    read_token_from_store()
}

#[cfg(windows)]
fn read_token_from_store() -> Result<String> {
    token_entry()?
        .get_password()
        .context("Discord token absent from environment and Windows Credential Manager")
}

#[cfg(not(windows))]
fn read_token_from_store() -> Result<String> {
    bail!("persistent Discord token storage requires Windows Credential Manager")
}

#[cfg(windows)]
pub fn save_token(token: &str) -> Result<()> {
    token_entry()?
        .set_password(token)
        .context("failed to store Discord token in Windows Credential Manager")
}

#[cfg(not(windows))]
pub fn save_token(_token: &str) -> Result<()> {
    bail!("persistent Discord token storage requires Windows Credential Manager")
}

#[cfg(windows)]
fn token_entry() -> Result<keyring_core::Entry> {
    use keyring_core::api::CredentialStoreApi as _;

    windows_native_keyring_store::Store::new()?
        .build(KEYRING_SERVICE, TOKEN_ACCOUNT, None)
        .context("failed to open Windows Credential Manager entry")
}

pub fn save_god_password_hash(encoded: &str) -> Result<()> {
    let path = god_password_path();
    atomic_write(&path, encoded.as_bytes(), true)
}

pub fn load_god_password_hash() -> Result<String> {
    let path = god_password_path();
    fs::read_to_string(&path).with_context(|| {
        format!(
            "missing GOD password hash at {}; run `codex-discord set-god-password`",
            path.display()
        )
    })
}

fn atomic_write(path: &Path, contents: &[u8], secret: bool) -> Result<()> {
    let parent = path.parent().context("atomic write path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".codex-write-")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temporary file beside {}", path.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("failed to write temporary file for {}", path.display()))?;
    temporary
        .flush()
        .with_context(|| format!("failed to flush temporary file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to fsync temporary file for {}", path.display()))?;
    if secret {
        crate::windows::lock_down_secret_file(temporary.path())?;
    }
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to atomically replace {}", path.display()))?;
    if secret {
        crate::windows::lock_down_secret_file(path)?;
    }
    Ok(())
}

const fn default_god_minutes() -> u64 {
    10
}
const fn default_history_limit() -> u64 {
    1000
}
const fn default_prune_at() -> usize {
    450
}
const fn default_prune_to() -> usize {
    425
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_existing_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        atomic_write(&path, b"first", false).unwrap();
        atomic_write(&path, b"second", false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            1,
            "temporary files must not survive a successful replacement"
        );
    }
}
