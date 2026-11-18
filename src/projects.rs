//! Projects = the direct sub-folders of the user's workspace folder.

use std::path::{Path, PathBuf};

/// Sub-folders of `workspace`, hidden ones (starting with '.') excluded,
/// sorted case-insensitively.
pub fn list(workspace: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(workspace) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    dirs.sort_by_key(|p| name(p).to_lowercase());
    dirs
}

pub fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Creates the folder `name` in `workspace` and returns its path.
/// Errors are user-facing messages.
pub fn create(workspace: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Entrez un nom de projet.".into());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("Le nom ne peut pas commencer ni finir par un point.".into());
    }
    if let Some(c) = name.chars().find(|c| r#"<>:"/\|?*"#.contains(*c) || c.is_control()) {
        return Err(format!("Caractère interdit : {c:?}"));
    }
    let stem = name.split('.').next().unwrap_or(name).to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(format!("« {name} » est un nom réservé par Windows."));
    }
    let path = workspace.join(name);
    if path.exists() {
        return Err(format!("« {name} » existe déjà."));
    }
    std::fs::create_dir(&path).map_err(|e| format!("Création impossible : {e}"))?;
    Ok(path)
}

/// A sensible default to pre-fill the first-launch screen.
pub fn suggested_workspace() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?);
    ["Work", "Projects", "Projets", "dev", "src"]
        .iter()
        .map(|d| home.join(d))
        .find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_visible_dirs_sorted() {
        let ws = std::env::temp_dir().join(format!("ade-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        for d in ["beta", "Alpha", ".hidden", "gamma"] {
            std::fs::create_dir_all(ws.join(d)).unwrap();
        }
        std::fs::write(ws.join("file.txt"), "x").unwrap();
        let names: Vec<String> = list(&ws).iter().map(|p| name(p)).collect();
        assert_eq!(names, ["Alpha", "beta", "gamma"]);
    }

    #[test]
    fn creates_project_and_rejects_bad_names() {
        let ws = std::env::temp_dir().join(format!("ade-ws-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        let p = create(&ws, "  mon-projet ").unwrap();
        assert!(p.is_dir());
        assert_eq!(name(&p), "mon-projet");
        assert!(create(&ws, "mon-projet").is_err());
        for bad in ["", "   ", ".git", "a/b", "a:b", "con", "COM1.txt", "fin."] {
            assert!(create(&ws, bad).is_err(), "{bad:?} should be rejected");
        }
        let _ = std::fs::remove_dir_all(&ws);
    }
}
