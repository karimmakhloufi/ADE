//! Local tools the agent can call. Every path is confined to the project root.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_CHARS: usize = 30_000;
const MAX_SEARCH_MATCHES: usize = 200;
const MAX_LIST_ENTRIES: usize = 500;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    pub diff: Option<String>,
}

impl ToolOutput {
    fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false, diff: None }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true, diff: None }
    }
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "list_dir",
            "description": "List the entries of a directory in the project (directories end with '/'). Call this to discover the project layout before reading files.",
            "eager_input_streaming": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory relative to the project root. Use '.' for the root." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "read_file",
            "description": "Read a UTF-8 text file from the project. Call this before editing a file, and whenever you need its exact current content.",
            "eager_input_streaming": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the project root." },
                    "start_line": { "type": "integer", "description": "Optional first line to return (1-based)." },
                    "end_line": { "type": "integer", "description": "Optional last line to return (inclusive)." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "search",
            "description": "Search file contents with a regular expression (respects .gitignore). Call this to find where a symbol, string or pattern is used. Returns `path:line: text` matches.",
            "eager_input_streaming": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust regex syntax." },
                    "path": { "type": "string", "description": "Optional directory or file to restrict the search to." }
                },
                "required": ["pattern"]
            }
        },
        {
            "name": "write_file",
            "description": "Create a file or overwrite it entirely with new content (parent directories are created). Use for new files or full rewrites; prefer edit_file for small changes.",
            "eager_input_streaming": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the project root." },
                    "content": { "type": "string", "description": "The complete file content." }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "edit_file",
            "description": "Replace an exact snippet of text in an existing file. `old_str` must match the file exactly (whitespace included) and be unique unless replace_all is true. Read the file first.",
            "eager_input_streaming": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the project root." },
                    "old_str": { "type": "string", "description": "Exact text to replace." },
                    "new_str": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
                },
                "required": ["path", "old_str", "new_str"]
            }
        },
        {
            "name": "run_command",
            "description": "Run a shell command in the project root and return its exit code and output (stdout+stderr). Use it to build, run tests, or inspect git state.",
            "eager_input_streaming": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to execute." }
                },
                "required": ["command"]
            }
        }
    ])
}

pub async fn execute(root: &Path, name: &str, input: &Value) -> ToolOutput {
    let root = root.to_path_buf();
    let name = name.to_string();
    let input = input.clone();
    // File-system work is blocking: keep it off the async executor.
    let res = tokio::task::spawn_blocking(move || match name.as_str() {
        "list_dir" => list_dir(&root, &input),
        "read_file" => read_file(&root, &input),
        "search" => search(&root, &input),
        "write_file" => write_file(&root, &input),
        "edit_file" => edit_file(&root, &input),
        other => Err(anyhow!("Unknown tool `{other}`.")),
    })
    .await;
    match res {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => ToolOutput::error(format!("{e:#}")),
        Err(e) => ToolOutput::error(format!("Tool crashed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Input helpers
// ---------------------------------------------------------------------------

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input[key]
        .as_str()
        .ok_or_else(|| anyhow!("Missing required string field `{key}`."))
}

fn opt_u64(input: &Value, key: &str) -> Result<Option<u64>> {
    match &input[key] {
        Value::Null => Ok(None),
        v => v.as_u64().map(Some).ok_or_else(|| anyhow!("Field `{key}` must be a positive integer.")),
    }
}

/// Resolves a model-supplied path and guarantees it stays inside `root`
/// (rejects `..` escapes, absolute paths elsewhere and symlinks pointing out).
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let root = dunce::canonicalize(root).context("project root not found")?;
    let mut path = root.clone();
    for comp in Path::new(rel).components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(c) => path.push(c),
            Component::ParentDir => {
                if !path.pop() || !path.starts_with(&root) {
                    bail!("Path `{rel}` escapes the project root.");
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                let abs = PathBuf::from(rel);
                let abs = dunce::canonicalize(&abs).unwrap_or(abs);
                if abs.starts_with(&root) {
                    path = abs;
                    break;
                }
                bail!("Absolute path `{rel}` is outside the project root.");
            }
        }
    }
    if !path.starts_with(&root) {
        bail!("Path `{rel}` escapes the project root.");
    }
    // Follow symlinks on the deepest existing ancestor and re-check.
    let mut existing = path.as_path();
    while !existing.exists() {
        existing = existing.parent().unwrap_or(&root);
    }
    if !dunce::canonicalize(existing)?.starts_with(&root) {
        bail!("Path `{rel}` resolves outside the project root.");
    }
    Ok(path)
}

fn display(root: &Path, path: &Path) -> String {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    path.strip_prefix(&root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

fn unified_diff(old: &str, new: &str, path: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn list_dir(root: &Path, input: &Value) -> Result<ToolOutput> {
    let dir = resolve(root, req_str(input, "path")?)?;
    let mut entries: Vec<(bool, String)> = std::fs::read_dir(&dir)
        .with_context(|| format!("cannot read directory `{}`", display(root, &dir)))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (is_dir, e.file_name().to_string_lossy().to_string())
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let total = entries.len();
    let mut out: Vec<String> = entries
        .into_iter()
        .take(MAX_LIST_ENTRIES)
        .map(|(d, n)| if d { format!("{n}/") } else { n })
        .collect();
    if total > MAX_LIST_ENTRIES {
        out.push(format!("... ({} more entries not shown)", total - MAX_LIST_ENTRIES));
    }
    if out.is_empty() {
        return Ok(ToolOutput::ok("(empty directory)"));
    }
    Ok(ToolOutput::ok(out.join("\n")))
}

fn read_file(root: &Path, input: &Value) -> Result<ToolOutput> {
    let path = resolve(root, req_str(input, "path")?)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read `{}` as UTF-8 text", display(root, &path)))?;
    let start = opt_u64(input, "start_line")?;
    let end = opt_u64(input, "end_line")?;

    let total_lines = text.lines().count();
    let selected = if start.is_some() || end.is_some() {
        let s = start.unwrap_or(1).max(1) as usize;
        let e = end.map(|e| e as usize).unwrap_or(total_lines);
        text.lines().skip(s - 1).take(e.saturating_sub(s - 1)).collect::<Vec<_>>().join("\n")
    } else {
        text
    };

    if selected.len() > MAX_READ_BYTES {
        let mut cut = MAX_READ_BYTES;
        while !selected.is_char_boundary(cut) {
            cut -= 1;
        }
        let shown = selected[..cut].lines().count();
        return Ok(ToolOutput::ok(format!(
            "{}\n\n[File is {total_lines} lines; only the first {shown} lines are shown. Use start_line/end_line to read the rest.]",
            &selected[..cut]
        )));
    }
    Ok(ToolOutput::ok(selected))
}

fn search(root: &Path, input: &Value) -> Result<ToolOutput> {
    let re = regex::Regex::new(req_str(input, "pattern")?).context("invalid regex")?;
    let base = match input["path"].as_str() {
        Some(p) => resolve(root, p)?,
        None => resolve(root, ".")?,
    };
    let mut matches = Vec::new();
    let mut truncated = false;
    'walk: for entry in ignore::WalkBuilder::new(&base).build().filter_map(|e| e.ok()) {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if matches.len() >= MAX_SEARCH_MATCHES {
                    truncated = true;
                    break 'walk;
                }
                let line = if line.len() > 300 { format!("{}…", &line[..line.floor_char_boundary_compat(300)]) } else { line.to_string() };
                matches.push(format!("{}:{}: {}", display(root, entry.path()), i + 1, line.trim_end()));
            }
        }
    }
    if matches.is_empty() {
        return Ok(ToolOutput::ok("No matches."));
    }
    if truncated {
        matches.push(format!("[Stopped after {MAX_SEARCH_MATCHES} matches; narrow the pattern or path.]"));
    }
    Ok(ToolOutput::ok(matches.join("\n")))
}

fn write_file(root: &Path, input: &Value) -> Result<ToolOutput> {
    let rel = req_str(input, "path")?;
    let content = req_str(input, "content")?;
    let path = resolve(root, rel)?;
    if path.is_dir() {
        bail!("`{rel}` is a directory.");
    }
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content).with_context(|| format!("cannot write `{rel}`"))?;
    let shown = display(root, &path);
    Ok(ToolOutput {
        content: format!(
            "{} `{shown}` ({} lines).",
            if existed { "Overwrote" } else { "Created" },
            content.lines().count()
        ),
        is_error: false,
        diff: Some(unified_diff(&old, content, &shown)),
    })
}

fn edit_file(root: &Path, input: &Value) -> Result<ToolOutput> {
    let rel = req_str(input, "path")?;
    let old_str = req_str(input, "old_str")?;
    let new_str = req_str(input, "new_str")?;
    let replace_all = input["replace_all"].as_bool().unwrap_or(false);
    if old_str.is_empty() {
        bail!("`old_str` must not be empty.");
    }
    let path = resolve(root, rel)?;
    let old = std::fs::read_to_string(&path).with_context(|| format!("cannot read `{rel}`"))?;
    let count = old.matches(old_str).count();
    let new = match (count, replace_all) {
        (0, _) => bail!("`old_str` was not found in `{rel}`. Read the file again and copy the text exactly."),
        (1, _) | (_, true) => old.replace(old_str, new_str),
        (n, false) => bail!("`old_str` matches {n} times in `{rel}`; add surrounding context to make it unique, or set replace_all."),
    };
    std::fs::write(&path, &new).with_context(|| format!("cannot write `{rel}`"))?;
    let shown = display(root, &path);
    Ok(ToolOutput {
        content: format!("Edited `{shown}` ({count} replacement{}).", if count > 1 { "s" } else { "" }),
        is_error: false,
        diff: Some(unified_diff(&old, &new, &shown)),
    })
}

pub async fn run_command(root: &Path, command: &str) -> ToolOutput {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd.exe");
        c.arg("/C").raw_arg(command);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    let output = match tokio::time::timeout(COMMAND_TIMEOUT, cmd.output()).await {
        Err(_) => return ToolOutput::error(format!("Command timed out after {}s and was killed.", COMMAND_TIMEOUT.as_secs())),
        Ok(Err(e)) => return ToolOutput::error(format!("Failed to start command: {e}")),
        Ok(Ok(o)) => o,
    };

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let char_count = text.chars().count();
    if char_count > MAX_OUTPUT_CHARS {
        let tail: String = text.chars().skip(char_count - MAX_OUTPUT_CHARS).collect();
        text = format!("[output truncated: showing the last {MAX_OUTPUT_CHARS} of {char_count} characters]\n{tail}");
    }
    let code = output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "killed".into());
    let content = format!("exit code: {code}\n{}", if text.trim().is_empty() { "(no output)" } else { &text });
    ToolOutput {
        content,
        is_error: !output.status.success(),
        diff: None,
    }
}

trait FloorCharBoundary {
    fn floor_char_boundary_compat(&self, i: usize) -> usize;
}

impl FloorCharBoundary for str {
    fn floor_char_boundary_compat(&self, mut i: usize) -> usize {
        i = i.min(self.len());
        while !self.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ade-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    #[test]
    fn resolve_rejects_escapes() {
        let root = tmp_root("resolve");
        assert!(resolve(&root, "src/main.rs").is_ok());
        assert!(resolve(&root, "./src/../Cargo.toml").is_ok());
        assert!(resolve(&root, "../outside.txt").is_err());
        assert!(resolve(&root, "src/../../outside.txt").is_err());
        let outside = std::env::temp_dir().join("x.txt");
        assert!(resolve(&root, outside.to_str().unwrap()).is_err());
    }

    #[test]
    fn edit_file_requires_unique_match() {
        let root = tmp_root("edit");
        std::fs::write(root.join("a.txt"), "foo\nbar\nfoo\n").unwrap();
        let dup = edit_file(&root, &json!({"path": "a.txt", "old_str": "foo", "new_str": "baz"}));
        assert!(dup.is_err());
        let ok = edit_file(&root, &json!({"path": "a.txt", "old_str": "bar", "new_str": "qux"})).unwrap();
        assert!(ok.diff.unwrap().contains("+qux"));
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "foo\nqux\nfoo\n");
        let all = edit_file(&root, &json!({"path": "a.txt", "old_str": "foo", "new_str": "z", "replace_all": true}));
        assert!(all.is_ok());
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "z\nqux\nz\n");
    }

    #[test]
    fn write_then_read_and_search() {
        let root = tmp_root("write");
        write_file(&root, &json!({"path": "src/new/mod.rs", "content": "fn alpha() {}\nfn beta() {}\n"})).unwrap();
        let read = read_file(&root, &json!({"path": "src/new/mod.rs", "start_line": 2})).unwrap();
        assert_eq!(read.content, "fn beta() {}");
        let found = search(&root, &json!({"pattern": r"fn b\w+"})).unwrap();
        assert!(found.content.contains("src/new/mod.rs:2:"));
        assert!(list_dir(&root, &json!({"path": "src"})).unwrap().content.contains("new/"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_command_captures_output() {
        let root = tmp_root("cmd");
        let out = run_command(&root, "echo hello").await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
        let fail = run_command(&root, "exit 3").await;
        assert!(fail.is_error && fail.content.starts_with("exit code: 3"));
    }
}
