//! API key storage and validation.
//!
//! The key comes from `ANTHROPIC_API_KEY` when set, otherwise from the user
//! config file (`%APPDATA%\ADE\config.json`, `~/.config/ADE/config.json`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

pub const KEYS_URL: &str = "https://platform.claude.com/settings/keys";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env,
    Saved,
}

pub fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("ADE").join("config.json"))
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

pub fn load_api_key() -> Option<(String, KeySource)> {
    if let Some(k) = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty()) {
        return Some((k.trim().to_string(), KeySource::Env));
    }
    read()["api_key"]
        .as_str()
        .filter(|k| !k.trim().is_empty())
        .map(|k| (k.trim().to_string(), KeySource::Saved))
}

pub fn save_api_key(key: &str) -> Result<PathBuf> {
    set("api_key", json!(key))
}

/// The folder that contains the user's projects.
pub fn load_workspace() -> Option<PathBuf> {
    read()["workspace"].as_str().map(PathBuf::from).filter(|p| p.is_dir())
}

pub fn save_workspace(dir: &std::path::Path) -> Result<PathBuf> {
    set("workspace", json!(dir.to_string_lossy()))
}

/// Checks the key with a free call (model listing): no tokens are billed.
pub async fn validate(key: String) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .get("https://api.anthropic.com/v1/models?limit=1")
        .header("x-api-key", &key)
        .header("anthropic-version", crate::agent::API_VERSION)
        .send()
        .await
        .map_err(|e| format!("Impossible de joindre l'API : {e}"))?;
    match resp.status().as_u16() {
        200..=299 => Ok(()),
        401 => Err("Clé API invalide.".into()),
        403 => Err("Cette clé n'a pas accès à l'API.".into()),
        s => {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("Réponse inattendue de l'API ({s}) : {body}"))
        }
    }
}
