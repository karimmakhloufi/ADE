//! Agent backend: Claude Code in headless mode (`claude -p`, stream-json).
//!
//! Claude Code uses the account it is logged in with, i.e. the user's Claude
//! subscription (Pro / Max), and runs its own tools (Read, Edit, Bash…) in the
//! project folder. ADE sends the prompt on stdin and turns the JSON events
//! printed by Claude Code into `AgentEvent`s for the UI.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use futures::channel::mpsc::Sender;
use futures::SinkExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Appended to Claude Code's own system prompt. Kept on one line without
/// shell metacharacters: on Windows, `claude.cmd` goes through cmd.exe.
const SYSTEM_PROMPT: &str = "You are running inside ADE, a minimalist agentic development environment. \
The user reads your replies in a narrow chat panel and sees your file edits as diffs, \
so keep messages short and concrete, without long preambles. Reply in the user's language.";

/// Transcript of the conversation in Messages API format (user prompts,
/// assistant blocks, tool results), used to rebuild the chat view after a
/// restart. Claude Code keeps the real context itself (see `session`).
pub type History = Arc<Mutex<Vec<Value>>>;

#[derive(Clone)]
pub struct Config {
    /// Path of the `claude` executable.
    pub claude: PathBuf,
    pub root: PathBuf,
    /// Claude Code session to resume (previous turns of this conversation).
    pub session: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Claude Code session id (to resume the conversation) and model in use.
    Session { id: String, model: Option<String> },
    TextDelta(String),
    ThinkingDelta(String),
    /// A tool_use block started streaming (input not complete yet).
    ToolStarted { id: String, name: String },
    /// The tool call is complete and about to run.
    ToolCall { id: String, name: String, input: Value },
    ToolResult { id: String, output: String, is_error: bool, diff: Option<String> },
    Usage { input: u64, output: u64, cache_read: u64 },
    /// Subscription limits (`rate_limit_info` of a `rate_limit_event`).
    RateLimit(Value),
    Error(String),
    Done,
}

/// Runs one user turn to completion. Always ends by sending `AgentEvent::Done`.
/// `images` are base64-encoded PNGs attached to the user message.
pub async fn run(
    cfg: Config,
    history: History,
    user_text: String,
    images: Vec<String>,
    mut out: Sender<AgentEvent>,
) {
    if let Err(e) = run_inner(&cfg, &history, user_text, images, &mut out).await {
        let _ = out.send(AgentEvent::Error(format!("{e:#}"))).await;
    }
    let _ = out.send(AgentEvent::Done).await;
}

async fn run_inner(
    cfg: &Config,
    history: &History,
    user_text: String,
    images: Vec<String>,
    out: &mut Sender<AgentEvent>,
) -> Result<()> {
    // Images first, then the text (recommended order for vision prompts).
    let mut content: Vec<Value> = images
        .into_iter()
        .map(|data| {
            json!({
                "type": "image",
                "source": { "type": "base64", "media_type": "image/png", "data": data },
            })
        })
        .collect();
    if !user_text.is_empty() {
        content.push(json!({ "type": "text", "text": user_text }));
    }
    history.lock().unwrap().push(json!({ "role": "user", "content": content }));
    let message = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null,
        "session_id": cfg.session.clone().unwrap_or_default(),
    })
    .to_string();

    let mut child = command(cfg)
        .spawn()
        .with_context(|| format!("impossible de lancer Claude Code ({})", cfg.claude.display()))?;
    let mut guard = KillTree(child.id());
    let mut stdin = child.stdin.take().context("stdin de Claude Code indisponible")?;
    let stdout = child.stdout.take().context("stdout de Claude Code indisponible")?;
    let mut stderr = child.stderr.take().context("stderr de Claude Code indisponible")?;

    let write = async move {
        stdin.write_all(message.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        // EOF: Claude Code exits once this message is answered.
        drop(stdin);
        Ok::<(), std::io::Error>(())
    };
    let read_stderr = async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    };
    let mut parser = Parser::new(&cfg.root);
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
            for ev in parser.handle(&v, history) {
                out.send(ev).await.ok();
            }
        }
        Ok::<(), std::io::Error>(())
    };
    // Written concurrently: a large image could fill the stdin pipe.
    let (_, read_result, err_text) = futures::join!(write, read, read_stderr);
    let status = child.wait().await.context("attente de Claude Code")?;
    guard.0 = None;
    read_result.context("lecture de la sortie de Claude Code")?;

    if !parser.finished {
        let err = err_text.trim();
        if err.contains("No conversation found") {
            bail!("La session Claude Code de cette conversation est introuvable : cliquez sur « Nouvelle conversation ».");
        }
        if err.is_empty() {
            bail!("Claude Code s'est arrêté sans répondre ({status})");
        }
        bail!("Claude Code s'est arrêté ({status}) : {err}");
    }
    Ok(())
}

fn command(cfg: &Config) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&cfg.claude);
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Same behaviour as before: the agent acts without asking.
        "--permission-mode",
        "bypassPermissions",
        "--append-system-prompt",
        SYSTEM_PROMPT,
    ]);
    if let Some(id) = &cfg.session {
        cmd.arg("--resume").arg(id);
    }
    cmd.current_dir(&cfg.root)
        // Otherwise Claude Code would bill this key instead of the subscription.
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    // On Windows the whole process tree is killed by `KillTree` instead.
    #[cfg(not(windows))]
    cmd.kill_on_drop(true);
    cmd
}

/// When a run is stopped (its future dropped), also stops what Claude Code
/// started (shell commands, `node` behind `claude.cmd`…).
struct KillTree(Option<u32>);

impl Drop for KillTree {
    fn drop(&mut self) {
        let Some(_pid) = self.0 else { return };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &_pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }
}

// ---------------------------------------------------------------------------
// stream-json events
// ---------------------------------------------------------------------------

struct Parser {
    root: PathBuf,
    /// Partial messages are streamed: text comes from the deltas, not from
    /// the complete assistant messages (which would duplicate it).
    streamed: bool,
    /// Tool calls already announced with `ToolStarted`.
    started: HashSet<String>,
    /// tool_use id -> (name, input), to build diffs from the results.
    tools: HashMap<String, (String, Value)>,
    last_text: String,
    /// The final `result` event was received.
    finished: bool,
}

impl Parser {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            streamed: false,
            started: HashSet::new(),
            tools: HashMap::new(),
            last_text: String::new(),
            finished: false,
        }
    }

    fn handle(&mut self, v: &Value, history: &History) -> Vec<AgentEvent> {
        let mut evs = Vec::new();
        // The internals of sub-agents (Task tool) are not shown.
        if !v["parent_tool_use_id"].is_null() {
            return evs;
        }
        match v["type"].as_str().unwrap_or_default() {
            "rate_limit_event" if v["rate_limit_info"].is_object() => {
                evs.push(AgentEvent::RateLimit(v["rate_limit_info"].clone()));
            }
            "system" if v["subtype"] == "init" => {
                if let Some(id) = v["session_id"].as_str() {
                    evs.push(AgentEvent::Session { id: id.into(), model: v["model"].as_str().map(Into::into) });
                }
            }
            "stream_event" => {
                self.streamed = true;
                let ev = &v["event"];
                match ev["type"].as_str().unwrap_or_default() {
                    "content_block_start" if ev["content_block"]["type"] == "tool_use" => {
                        self.tool_started(&ev["content_block"], &mut evs);
                    }
                    "content_block_delta" => {
                        let d = &ev["delta"];
                        match d["type"].as_str().unwrap_or_default() {
                            "text_delta" => push_text(&mut evs, AgentEvent::TextDelta, &d["text"]),
                            "thinking_delta" => push_text(&mut evs, AgentEvent::ThinkingDelta, &d["thinking"]),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let Some(blocks) = v["message"]["content"].as_array() else { return evs };
                history.lock().unwrap().push(json!({ "role": "assistant", "content": blocks }));
                for b in blocks {
                    match b["type"].as_str().unwrap_or_default() {
                        "text" => {
                            self.last_text = b["text"].as_str().unwrap_or_default().to_string();
                            if !self.streamed {
                                push_text(&mut evs, AgentEvent::TextDelta, &b["text"]);
                            }
                        }
                        "thinking" if !self.streamed => push_text(&mut evs, AgentEvent::ThinkingDelta, &b["thinking"]),
                        "tool_use" => {
                            self.tool_started(b, &mut evs);
                            let id = b["id"].as_str().unwrap_or_default().to_string();
                            let name = b["name"].as_str().unwrap_or_default().to_string();
                            self.tools.insert(id.clone(), (name.clone(), b["input"].clone()));
                            evs.push(AgentEvent::ToolCall { id, name, input: b["input"].clone() });
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                let Some(blocks) = v["message"]["content"].as_array() else { return evs };
                let results: Vec<Value> = blocks.iter().filter(|b| b["type"] == "tool_result").cloned().collect();
                if results.is_empty() {
                    return evs;
                }
                history.lock().unwrap().push(json!({ "role": "user", "content": results }));
                // Structured details (e.g. the patch of an edit) of a single result.
                let extra = if results.len() == 1 { &v["tool_use_result"] } else { &Value::Null };
                for r in &results {
                    let id = r["tool_use_id"].as_str().unwrap_or_default().to_string();
                    let is_error = r["is_error"].as_bool().unwrap_or(false);
                    let diff = if is_error {
                        None
                    } else {
                        self.tools.get(&id).and_then(|(name, input)| diff_for(name, input, extra, &self.root))
                    };
                    evs.push(AgentEvent::ToolResult { id, output: result_text(&r["content"]), is_error, diff });
                }
            }
            "result" => {
                self.finished = true;
                if let Some(id) = v["session_id"].as_str() {
                    evs.push(AgentEvent::Session { id: id.into(), model: None });
                }
                let u = &v["usage"];
                evs.push(AgentEvent::Usage {
                    input: u["input_tokens"].as_u64().unwrap_or(0),
                    output: u["output_tokens"].as_u64().unwrap_or(0),
                    cache_read: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
                });
                let failed = v["is_error"].as_bool().unwrap_or(false)
                    || v["subtype"].as_str().is_some_and(|s| s != "success");
                if failed {
                    let msg = v["result"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("Claude Code : {}", v["subtype"].as_str().unwrap_or("erreur")));
                    // Already shown when it came as the assistant's text.
                    if msg.trim() != self.last_text.trim() {
                        evs.push(AgentEvent::Error(msg.clone()));
                    }
                    if msg.contains("/login") || msg.to_lowercase().contains("api key") {
                        evs.push(AgentEvent::Error(
                            "Claude Code n'est pas connecté à votre compte : cliquez sur « Claude Code » en haut à droite, puis « Se connecter ».".into(),
                        ));
                    }
                }
            }
            _ => {}
        }
        evs
    }

    fn tool_started(&mut self, block: &Value, evs: &mut Vec<AgentEvent>) {
        let id = block["id"].as_str().unwrap_or_default().to_string();
        if self.started.insert(id.clone()) {
            evs.push(AgentEvent::ToolStarted { id, name: block["name"].as_str().unwrap_or_default().into() });
        }
    }
}

fn push_text(evs: &mut Vec<AgentEvent>, make: fn(String) -> AgentEvent, t: &Value) {
    if let Some(t) = t.as_str().filter(|t| !t.is_empty()) {
        evs.push(make(t.to_string()));
    }
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|c| match c["type"].as_str() {
                Some("image") => "[image]".to_string(),
                _ => c["text"].as_str().unwrap_or_default().to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Project-relative path for display.
pub fn relative(root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string())
}

/// Diff shown for file edits: the patch reported by Claude Code when present,
/// otherwise rebuilt from the tool input.
fn diff_for(name: &str, input: &Value, extra: &Value, root: &Path) -> Option<String> {
    let path = relative(root, input["file_path"].as_str()?);
    let diff = match structured_patch(extra, &path) {
        Some(d) => d,
        None => match name {
            "Edit" => unified_diff(input["old_string"].as_str()?, input["new_string"].as_str()?, &path),
            "MultiEdit" => input["edits"]
                .as_array()?
                .iter()
                .filter_map(|e| Some(unified_diff(e["old_string"].as_str()?, e["new_string"].as_str()?, &path)))
                .collect(),
            "Write" => unified_diff(extra["originalFile"].as_str().unwrap_or(""), input["content"].as_str()?, &path),
            _ => return None,
        },
    };
    (!diff.trim().is_empty()).then_some(diff)
}

fn structured_patch(extra: &Value, path: &str) -> Option<String> {
    let hunks = extra["structuredPatch"].as_array().filter(|h| !h.is_empty())?;
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for h in hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h["oldStart"], h["oldLines"], h["newStart"], h["newLines"]
        ));
        for l in h["lines"].as_array()? {
            out.push_str(l.as_str().unwrap_or_default());
            out.push('\n');
        }
    }
    Some(out)
}

fn unified_diff(old: &str, new: &str, path: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

/// Closes the tool calls left without result when a run is interrupted, so
/// the chat shows them as failed instead of pending forever.
pub fn repair(history: &History) {
    let pending: Vec<Value> = {
        let h = history.lock().unwrap();
        let answered: HashSet<&str> = h
            .iter()
            .filter(|m| m["role"] == "user")
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter_map(|b| b["tool_use_id"].as_str())
            .collect();
        h.iter()
            .filter(|m| m["role"] == "assistant")
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter(|b| b["type"] == "tool_use" && !answered.contains(b["id"].as_str().unwrap_or_default()))
            .cloned()
            .collect()
    };
    if pending.is_empty() {
        return;
    }
    let results: Vec<Value> = pending
        .iter()
        .map(|tu| {
            json!({ "type": "tool_result", "tool_use_id": tu["id"], "content": "Interrupted by the user before completion.", "is_error": true })
        })
        .collect();
    history.lock().unwrap().push(json!({ "role": "user", "content": results }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[Value]) -> (Vec<AgentEvent>, History, Parser) {
        let history: History = Arc::new(Mutex::new(Vec::new()));
        let mut p = Parser::new(Path::new("C:\\proj"));
        let evs = lines.iter().flat_map(|l| p.handle(l, &history)).collect();
        (evs, history, p)
    }

    #[test]
    fn parses_a_turn_with_streaming_and_a_tool() {
        let (evs, history, p) = feed(&[
            json!({"type": "system", "subtype": "init", "session_id": "s1", "model": "claude-x"}),
            json!({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Je lis"}}}),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Je lis"}]}}),
            json!({"type": "stream_event", "event": {"type": "content_block_start", "content_block": {"type": "tool_use", "id": "t1", "name": "Edit"}}}),
            json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Edit",
                "input": {"file_path": "C:\\proj\\a.txt", "old_string": "a\n", "new_string": "b\n"}}]}}),
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}}),
            json!({"type": "assistant", "parent_tool_use_id": "t9", "message": {"content": [{"type": "text", "text": "sub-agent"}]}}),
            json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "s1",
                "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100}}),
        ]);
        assert!(p.finished);
        assert!(matches!(&evs[0], AgentEvent::Session { id, model: Some(m) } if id == "s1" && m == "claude-x"));
        // Text comes once (from the delta), not again from the complete message.
        let texts: Vec<_> = evs.iter().filter_map(|e| match e { AgentEvent::TextDelta(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(texts, ["Je lis"]);
        assert_eq!(evs.iter().filter(|e| matches!(e, AgentEvent::ToolStarted { .. })).count(), 1);
        let diff = evs.iter().find_map(|e| match e { AgentEvent::ToolResult { diff, .. } => diff.clone(), _ => None }).unwrap();
        assert!(diff.contains("a/a.txt") && diff.contains("-a") && diff.contains("+b"));
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::Usage { input: 10, output: 5, cache_read: 100 })));
        assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Error(_))));
        assert_eq!(history.lock().unwrap().len(), 3);
    }

    #[test]
    fn reports_failures_once() {
        let (evs, _, _) = feed(&[
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Invalid API key · Please run /login"}]}}),
            json!({"type": "result", "subtype": "success", "is_error": true, "result": "Invalid API key · Please run /login"}),
        ]);
        let errors = evs.iter().filter(|e| matches!(e, AgentEvent::Error(_))).count();
        assert_eq!(errors, 1, "only the login hint, the message itself was already shown");
    }

    #[test]
    fn uses_structured_patch_when_present() {
        let extra = json!({"structuredPatch": [{"oldStart": 3, "oldLines": 1, "newStart": 3, "newLines": 1, "lines": ["-x", "+y"]}]});
        let d = diff_for("Edit", &json!({"file_path": "C:\\proj\\src\\m.rs"}), &extra, Path::new("C:\\proj")).unwrap();
        assert_eq!(d, "--- a/src/m.rs\n+++ b/src/m.rs\n@@ -3,1 +3,1 @@\n-x\n+y\n");
    }

    #[test]
    fn repair_closes_dangling_tool_calls() {
        let history: History = Arc::new(Mutex::new(vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t2", "name": "Bash", "input": {}}]}),
        ]));
        repair(&history);
        let h = history.lock().unwrap();
        assert_eq!(h.len(), 5);
        assert_eq!(h[4]["content"].as_array().unwrap().len(), 1);
        assert_eq!(h[4]["content"][0]["tool_use_id"], "t2");
        assert_eq!(h[4]["content"][0]["is_error"], true);
    }
}
