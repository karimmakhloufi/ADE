//! Persists each project's conversation so it survives restarts.
//!
//! One JSON file per project in `<config dir>/conversations/`, holding the
//! API history (the source of truth, the chat view is rebuilt from it), the
//! token counters and the diffs shown for file edits.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

pub struct Saved {
    pub root: PathBuf,
    pub history: Vec<Value>,
    pub tokens: (u64, u64, u64),
    /// tool_use id -> unified diff.
    pub diffs: HashMap<String, String>,
}

fn dir() -> Option<PathBuf> {
    Some(crate::config::path()?.parent()?.join("conversations"))
}

/// Stable 64-bit FNV-1a (std's hasher is not guaranteed stable across releases).
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn file_for(dir: &Path, root: &Path) -> PathBuf {
    let mut key = root.to_string_lossy().to_string();
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    let name: String = crate::projects::name(root)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(40)
        .collect();
    dir.join(format!("{name}-{:016x}.json", fnv1a(&key)))
}

/// Saves (or deletes, when the history is empty) the conversation of `root`.
pub fn save(root: &Path, history: &[Value], tokens: (u64, u64, u64), diffs: &HashMap<String, String>) -> Result<()> {
    let dir = dir().context("dossier de configuration introuvable")?;
    let path = file_for(&dir, root);
    if history.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("suppression de {} impossible", path.display()))?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    let doc = json!({
        "version": 1,
        "root": root.to_string_lossy(),
        "tokens": [tokens.0, tokens.1, tokens.2],
        "diffs": diffs,
        "history": history,
    });
    // Write then rename, so a crash never leaves a truncated file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&doc)?).with_context(|| format!("écriture de {} impossible", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("écriture de {} impossible", path.display()))?;
    Ok(())
}

/// Every saved conversation (unreadable files are skipped).
pub fn load_all() -> Vec<Saved> {
    let Some(dir) = dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| parse(&std::fs::read_to_string(p).ok()?))
        .collect()
}

fn parse(text: &str) -> Option<Saved> {
    let v: Value = serde_json::from_str(text).ok()?;
    let root = PathBuf::from(v["root"].as_str()?);
    let history = v["history"].as_array()?.clone();
    let t = |i: usize| v["tokens"][i].as_u64().unwrap_or(0);
    let diffs = v["diffs"]
        .as_object()
        .map(|m| m.iter().filter_map(|(k, d)| Some((k.clone(), d.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    Some(Saved { root, history, tokens: (t(0), t(1), t(2)), diffs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_parse() {
        let doc = json!({
            "root": "C:\\Work\\demo",
            "tokens": [1, 2, 3],
            "diffs": { "t1": "+a" },
            "history": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        });
        let s = parse(&doc.to_string()).unwrap();
        assert_eq!(s.root, PathBuf::from("C:\\Work\\demo"));
        assert_eq!(s.tokens, (1, 2, 3));
        assert_eq!(s.diffs["t1"], "+a");
        assert_eq!(s.history.len(), 1);
    }

    #[test]
    fn file_name_is_stable_and_safe() {
        let a = file_for(Path::new("x"), Path::new("C:\\Work\\my proj"));
        let b = file_for(Path::new("x"), Path::new("C:\\Work\\my proj"));
        assert_eq!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("my_proj-"));
    }
}
