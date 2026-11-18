//! User settings (`%APPDATA%\ADE\config.json`, `~/.config/ADE/config.json`)
//! and detection of Claude Code, through which ADE uses the Claude subscription.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::{json, Value};

#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// Shown in the setup screen.
pub const INSTALL_COMMAND: &str = if cfg!(windows) {
    "irm https://claude.ai/install.ps1 | iex"
} else {
    "curl -fsSL https://claude.ai/install.sh | bash"
};

pub fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))
    };
    base.map(|b| b.join("ADE").join("config.json"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

fn read() -> Value {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}))
}

/// Updates one field, keeping the others.
fn set(key: &str, value: Value) -> Result<PathBuf> {
    let path = path().context("dossier de configuration introuvable")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut cfg = read();
    cfg[key] = value;
    std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)
        .with_context(|| format!("écriture de {} impossible", path.display()))?;
    Ok(path)
}

/// The folder that contains the user's projects.
pub fn load_workspace() -> Option<PathBuf> {
    read()["workspace"].as_str().map(PathBuf::from).filter(|p| p.is_dir())
}

pub fn save_workspace(dir: &Path) -> Result<PathBuf> {
    set("workspace", json!(dir.to_string_lossy()))
}

/// Last subscription limits reported by Claude Code (`rate_limit_info`),
/// so they are shown at startup before the first message.
pub fn load_rate_limits() -> Value {
    read()["rate_limits"].clone()
}

pub fn save_rate_limits(info: &Value) -> Result<PathBuf> {
    set("rate_limits", info.clone())
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ClaudeCode {
    pub path: PathBuf,
    /// e.g. "2.0.14 (Claude Code)".
    pub version: String,
}

/// The `claude` executable: PATH first, then the usual install locations
/// (ADE may have been started before the installation updated the PATH).
pub fn find_claude() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd"] } else { &["claude"] };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(h) = home() {
        dirs.push(h.join(".local").join("bin"));
        dirs.push(h.join(".claude").join("local"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// Finds Claude Code and checks that it runs (`claude --version`).
pub async fn detect_claude() -> Result<ClaudeCode, String> {
    let path = find_claude().ok_or("Claude Code est introuvable sur ce poste.")?;
    let mut cmd = tokio::process::Command::new(&path);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(crate::agent::CREATE_NO_WINDOW);
    let out = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "Claude Code ne répond pas (`claude --version`).".to_string())?
        .map_err(|e| format!("Impossible de lancer {} : {e}", path.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`claude --version` a échoué : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(ClaudeCode { path, version })
}

/// Opens Claude Code in a terminal so the user can log in with their
/// Claude account (`/login`).
pub fn open_login(claude: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new(claude);
        if let Some(h) = home() {
            cmd.current_dir(h);
        }
        cmd.creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Impossible d'ouvrir Claude Code : {e}"))
    }
    #[cfg(not(windows))]
    {
        let _ = claude;
        Err("Lancez `claude` dans un terminal puis tapez /login.".into())
    }
}

/// Runs the official Claude Code installer in a new terminal.
pub fn open_installer() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("powershell")
            .args(["-NoExit", "-ExecutionPolicy", "Bypass", "-Command", INSTALL_COMMAND])
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Impossible de lancer PowerShell : {e}"))
    }
    #[cfg(not(windows))]
    {
        Err(format!("Dans un terminal : {INSTALL_COMMAND}"))
    }
}
