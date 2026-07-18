use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result};

const TASK_NAME: &str = "Codex Discord Relay";
const RUN_VALUE_NAME: &str = "CodexDiscordRelay";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn install_binary(source: &Path, destination_dir: Option<&Path>) -> Result<PathBuf> {
    let destination_dir = match destination_dir {
        Some(path) => path.to_path_buf(),
        None => default_install_dir()?,
    };
    fs::create_dir_all(&destination_dir).with_context(|| destination_dir.display().to_string())?;
    let destination = destination_dir.join("codex-discord.exe");
    let same_file = source
        .canonicalize()
        .ok()
        .zip(destination.canonicalize().ok())
        .is_some_and(|(source, destination)| source == destination);
    if !same_file {
        fs::copy(source, &destination).with_context(|| {
            format!(
                "failed to install {} to {}; stop the running relay before updating",
                source.display(),
                destination.display()
            )
        })?;
    }
    Ok(destination)
}

pub fn launch_relay(executable: &Path) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .spawn()
        .with_context(|| format!("failed to launch {} run", executable.display()))?;
    Ok(())
}

/// Open an HTTPS URL with the user's default browser without invoking a shell.
pub fn open_https_url(url: &str) -> Result<()> {
    anyhow::ensure!(
        url.starts_with("https://"),
        "refusing to open a non-HTTPS URL"
    );
    let mut command = Command::new("rundll32.exe");
    command
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .spawn()
        .context("failed to open the Discord authorization page")?;
    Ok(())
}

pub fn install_startup(executable: &Path, highest: bool) -> Result<()> {
    let command_line = format!("\"{}\" run", executable.display());
    let privilege = if highest { "HIGHEST" } else { "LIMITED" };
    let output = Command::new("schtasks.exe")
        .args([
            "/Create",
            "/F",
            "/SC",
            "ONLOGON",
            "/TN",
            TASK_NAME,
            "/TR",
            &command_line,
            "/RL",
            privilege,
        ])
        .output()
        .context("failed to launch Windows Task Scheduler")?;
    if !output.status.success() {
        tracing::warn!(
            error = %String::from_utf8_lossy(&output.stderr).trim(),
            "Task Scheduler denied startup install; using per-user Startup folder"
        );
        return install_run_key(executable);
    }
    remove_legacy_startup_launcher()?;
    Ok(())
}

pub fn uninstall_startup() -> Result<()> {
    let _ = Command::new("schtasks.exe")
        .args(["/Delete", "/F", "/TN", TASK_NAME])
        .output()
        .context("failed to launch Windows Task Scheduler")?;
    let _ = Command::new("reg.exe")
        .args([
            "DELETE",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/V",
            RUN_VALUE_NAME,
            "/F",
        ])
        .output();
    remove_legacy_startup_launcher()?;
    Ok(())
}

pub fn lock_down_secret_file(path: &Path) -> Result<()> {
    let identity = Command::new("whoami")
        .output()
        .context("failed to resolve current Windows identity")?;
    let identity = String::from_utf8_lossy(&identity.stdout).trim().to_owned();
    anyhow::ensure!(!identity.is_empty(), "current Windows identity is empty");
    let grant_user = format!("{identity}:(F)");
    let output = Command::new("icacls")
        .args([
            path.as_os_str(),
            "/inheritance:r".as_ref(),
            "/grant:r".as_ref(),
            grant_user.as_ref(),
            "/grant:r".as_ref(),
            "SYSTEM:(F)".as_ref(),
        ])
        .output()
        .with_context(|| format!("failed to launch icacls for {}", path.display()))?;
    anyhow::ensure!(
        output.status.success(),
        "icacls failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

pub fn startup_status() -> Result<String> {
    let output = Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME, "/FO", "LIST", "/V"])
        .output()
        .context("failed to query Windows Task Scheduler")?;
    if !output.status.success() {
        let run_key = Command::new("reg.exe")
            .args([
                "QUERY",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/V",
                RUN_VALUE_NAME,
            ])
            .output()
            .context("failed to query per-user Run key")?;
        return Ok(if run_key.status.success() {
            "installed via per-user Windows Run key".to_owned()
        } else {
            "not installed".to_owned()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Count relay processes visible to the current Windows session.
///
/// A `doctor` invocation counts as one, so deep doctor requires at least two
/// to prove a separate long-running relay is alive.
pub fn relay_process_count() -> Result<usize> {
    let output = Command::new("tasklist.exe")
        .args(["/FI", "IMAGENAME eq codex-discord.exe", "/FO", "CSV", "/NH"])
        .output()
        .context("failed to query running relay processes")?;
    anyhow::ensure!(
        output.status.success(),
        "tasklist failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains("codex-discord.exe"))
        .count())
}

/// Verify the GOD hash does not grant common broad Windows principals access.
pub fn verify_secret_file_acl(path: &Path) -> Result<()> {
    let output = Command::new("icacls.exe")
        .arg(path)
        .output()
        .with_context(|| format!("failed to inspect ACL for {}", path.display()))?;
    anyhow::ensure!(
        output.status.success(),
        "icacls failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let acl = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    for broad in [
        "everyone:",
        "builtin\\users:",
        "authenticated users:",
        "codexsandboxusers:",
    ] {
        anyhow::ensure!(
            !acl.contains(broad),
            "secret ACL grants broad principal {broad}"
        );
    }
    Ok(())
}

fn install_run_key(executable: &Path) -> Result<()> {
    let command = format!("\"{}\" run", executable.display());
    let output = Command::new("reg.exe")
        .args([
            "ADD",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/V",
            RUN_VALUE_NAME,
            "/T",
            "REG_SZ",
            "/D",
            &command,
            "/F",
        ])
        .output()
        .context("failed to install per-user Windows Run key")?;
    anyhow::ensure!(
        output.status.success(),
        "reg.exe failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    remove_legacy_startup_launcher()?;
    Ok(())
}

fn remove_legacy_startup_launcher() -> Result<()> {
    let Some(appdata) = env::var_os("APPDATA") else {
        return Ok(());
    };
    let launcher = PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("Codex Discord Relay.cmd");
    if launcher.exists() {
        fs::remove_file(&launcher).with_context(|| launcher.display().to_string())?;
    }
    Ok(())
}

fn default_install_dir() -> Result<PathBuf> {
    let local_app_data = env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local_app_data)
        .join("CodexDiscordRelay")
        .join("bin"))
}
