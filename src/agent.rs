//! Agent loop: talks to the Claude Messages API (raw HTTP + SSE streaming),
//! executes tool calls locally and reports progress to the UI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use futures::channel::mpsc::Sender;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};

use crate::tools;

pub const MODEL: &str = "claude-opus-5-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2026-10-21";
/// Server-side refusal fallback (opt-in beta).
const BETAS: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 64_000;
const MAX_ITERATIONS: usize = 60;

/// Full conversation sent to the API. Append-only: assistant turns are stored
/// exactly as received (thinking blocks and signatures included).
pub type History = Arc<Mutex<Vec<Value>>>;

#[derive(Clone)]
pub struct Config {
    pub api_key: String,
    pub root: PathBuf,
    pub effort: String,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// A tool_use block started streaming (input not complete yet).
    ToolStarted { id: String, name: String },
    /// The tool call is complete and about to run.
    ToolCall { id: String, name: String, input: Value },
    ToolResult { id: String, output: String, is_error: bool, diff: Option<String> },
    Usage { input: u64, output: u64, cache_read: u64 },
    Error(String),
    Done,
}

/// Runs one user turn to completion (possibly many API round trips).
/// Always ends by sending `AgentEvent::Done`.
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

    let client = reqwest::Client::new();
    let system = system_prompt(cfg);
    let tool_defs = tools::definitions();

    for _ in 0..MAX_ITERATIONS {
        let messages = history.lock().unwrap().clone();
        let body = json!({
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "system": system,
            "tools": tool_defs,
            "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": cfg.effort },
            "fallbacks": "default",
            "cache_control": { "type": "ephemeral" },
            "messages": messages,
        });

        let resp = client
            .post(API_URL)
            .header("x-api-key", &cfg.api_key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", BETAS)
            .json(&body)
            .send()
            .await
            .context("requête vers l'API Anthropic impossible")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("API {status} : {text}");
        }

        let msg = read_stream(resp, out).await?;
        let tool_uses: Vec<Value> = msg
            .content
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .cloned()
            .collect();

        history.lock().unwrap().push(json!({ "role": "assistant", "content": msg.content }));

        match msg.stop_reason.as_deref() {
            Some("tool_use") => {}
            Some("refusal") => {
                close_tool_uses(history, &tool_uses, "Not executed: the response was declined.");
                let why = msg
                    .stop_details
                    .as_ref()
                    .and_then(|d| d["explanation"].as_str())
                    .unwrap_or("le modèle a refusé la demande");
                bail!("Refus : {why}");
            }
            Some("max_tokens") if !tool_uses.is_empty() => {
                // A truncated tool input must never be executed.
                close_tool_uses(
                    history,
                    &tool_uses,
                    "Not executed: the tool input was truncated by max_tokens. Retry with smaller steps (e.g. several edit_file calls instead of one huge write_file).",
                );
                continue;
            }
            Some("pause_turn") => continue,
            _ => return Ok(()),
        }

        let mut results = Vec::new();
        for tu in &tool_uses {
            let id = tu["id"].as_str().unwrap_or_default().to_string();
            let name = tu["name"].as_str().unwrap_or_default().to_string();
            let input = tu["input"].clone();

            let output = if let Some(raw) = msg.invalid_inputs.get(&id) {
                tools::ToolOutput::error(json!({ "INVALID_JSON": raw }).to_string())
            } else {
                out.send(AgentEvent::ToolCall { id: id.clone(), name: name.clone(), input: input.clone() })
                    .await
                    .ok();
                execute_tool(cfg, &name, &input).await
            };

            out.send(AgentEvent::ToolResult {
                id: id.clone(),
                output: output.content.clone(),
                is_error: output.is_error,
                diff: output.diff.clone(),
            })
            .await
            .ok();

            results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": output.content,
                "is_error": output.is_error,
            }));
        }
        // All results of one assistant turn go back in a single user message.
        history.lock().unwrap().push(json!({ "role": "user", "content": results }));
    }

    Err(anyhow!("limite de {MAX_ITERATIONS} itérations atteinte"))
}

async fn execute_tool(cfg: &Config, name: &str, input: &Value) -> tools::ToolOutput {
    if name == "run_command" {
        let Some(command) = input["command"].as_str() else {
            return tools::ToolOutput::error("Missing required string field `command`.");
        };
        tools::run_command(&cfg.root, command).await
    } else {
        tools::execute(&cfg.root, name, input).await
    }
}

/// Appends error results for tool calls that will not run, keeping the
/// conversation valid (every tool_use needs a matching tool_result).
fn close_tool_uses(history: &History, tool_uses: &[Value], reason: &str) {
    if tool_uses.is_empty() {
        return;
    }
    let results: Vec<Value> = tool_uses
        .iter()
        .map(|tu| json!({ "type": "tool_result", "tool_use_id": tu["id"], "content": reason, "is_error": true }))
        .collect();
    history.lock().unwrap().push(json!({ "role": "user", "content": results }));
}

/// Called after the user interrupts a run: closes any dangling tool calls.
pub fn repair(history: &History) {
    let pending: Vec<Value> = {
        let h = history.lock().unwrap();
        match h.last() {
            Some(last) if last["role"] == "assistant" => last["content"]
                .as_array()
                .map(|c| c.iter().filter(|b| b["type"] == "tool_use").cloned().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    close_tool_uses(history, &pending, "Interrupted by the user before completion.");
}

fn system_prompt(cfg: &Config) -> String {
    let shell = if cfg!(windows) { "cmd.exe /C" } else { "sh -c" };
    format!(
        "You are the coding agent embedded in ADE, a minimalist agentic development environment.\n\
         You work on the project rooted at `{root}` ({os}).\n\
         \n\
         - Use the tools to explore the code, make changes and verify them. All paths are relative to the project root; you cannot access files outside it.\n\
         - Read a file before editing it. Prefer edit_file for targeted changes; use write_file for new files or full rewrites.\n\
         - run_command executes through `{shell}` in the project root without user confirmation, so run what is useful (builds, tests, git status) and never run destructive commands unless explicitly asked.\n\
         - The user sees your file edits as diffs and your messages in a narrow chat panel: keep messages short and concrete, no long preambles.\n\
         - Reply in the user's language.",
        root = cfg.root.display(),
        os = std::env::consts::OS,
    )
}

// ---------------------------------------------------------------------------
// SSE streaming
// ---------------------------------------------------------------------------

struct StreamedMessage {
    content: Vec<Value>,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
    /// tool_use id -> raw input that failed to parse as JSON.
    invalid_inputs: BTreeMap<String, String>,
}

async fn read_stream(resp: reqwest::Response, out: &mut Sender<AgentEvent>) -> Result<StreamedMessage> {
    let mut blocks: BTreeMap<usize, Value> = BTreeMap::new();
    let mut partial_json: BTreeMap<usize, String> = BTreeMap::new();
    let mut invalid_inputs = BTreeMap::new();
    let mut stop_reason = None;
    let mut stop_details = None;
    let (mut in_tok, mut out_tok, mut cache_read) = (0u64, 0u64, 0u64);

    let mut bytes = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = bytes.next().await {
        buf.extend_from_slice(&chunk.context("flux SSE interrompu")?);

        // Events are separated by a blank line.
        while let Some(pos) = find_event_end(&buf) {
            let raw: Vec<u8> = buf.drain(..pos.0 + pos.1).collect();
            let raw = String::from_utf8_lossy(&raw[..pos.0]).to_string();
            let data: String = raw
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.trim_start())
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let ev: Value = serde_json::from_str(&data).context("événement SSE illisible")?;

            match ev["type"].as_str().unwrap_or_default() {
                "message_start" => {
                    let u = &ev["message"]["usage"];
                    in_tok = u["input_tokens"].as_u64().unwrap_or(0);
                    cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                }
                "content_block_start" => {
                    let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                    let block = ev["content_block"].clone();
                    match block["type"].as_str() {
                        Some("tool_use") => {
                            partial_json.insert(idx, String::new());
                            out.send(AgentEvent::ToolStarted {
                                id: block["id"].as_str().unwrap_or_default().into(),
                                name: block["name"].as_str().unwrap_or_default().into(),
                            })
                            .await
                            .ok();
                        }
                        Some("text") => {
                            if let Some(t) = block["text"].as_str().filter(|t| !t.is_empty()) {
                                out.send(AgentEvent::TextDelta(t.into())).await.ok();
                            }
                        }
                        _ => {}
                    }
                    blocks.insert(idx, block);
                }
                "content_block_delta" => {
                    let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                    let delta = &ev["delta"];
                    let Some(block) = blocks.get_mut(&idx) else { continue };
                    match delta["type"].as_str().unwrap_or_default() {
                        "text_delta" => {
                            let t = delta["text"].as_str().unwrap_or_default();
                            append_str(block, "text", t);
                            out.send(AgentEvent::TextDelta(t.into())).await.ok();
                        }
                        "thinking_delta" => {
                            let t = delta["thinking"].as_str().unwrap_or_default();
                            append_str(block, "thinking", t);
                            if !t.is_empty() {
                                out.send(AgentEvent::ThinkingDelta(t.into())).await.ok();
                            }
                        }
                        "signature_delta" => {
                            block["signature"] = delta["signature"].clone();
                        }
                        "input_json_delta" => {
                            if let Some(p) = partial_json.get_mut(&idx) {
                                p.push_str(delta["partial_json"].as_str().unwrap_or_default());
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                    if let (Some(raw), Some(block)) = (partial_json.remove(&idx), blocks.get_mut(&idx)) {
                        // Eager input streaming: the server does not validate the
                        // JSON, so parse strictly and never run a broken input.
                        let parsed = if raw.trim().is_empty() {
                            Ok(json!({}))
                        } else {
                            serde_json::from_str::<Value>(&raw)
                        };
                        match parsed {
                            Ok(v) if v.is_object() => block["input"] = v,
                            _ => {
                                block["input"] = json!({});
                                let id = block["id"].as_str().unwrap_or_default().to_string();
                                invalid_inputs.insert(id, raw);
                            }
                        }
                    }
                }
                "message_delta" => {
                    if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(r.to_string());
                    }
                    if !ev["delta"]["stop_details"].is_null() {
                        stop_details = Some(ev["delta"]["stop_details"].clone());
                    }
                    out_tok = ev["usage"]["output_tokens"].as_u64().unwrap_or(out_tok);
                }
                "error" => {
                    bail!(
                        "erreur API pendant le streaming : {}",
                        ev["error"]["message"].as_str().unwrap_or(&data)
                    );
                }
                _ => {} // ping, message_stop
            }
        }
    }

    out.send(AgentEvent::Usage { input: in_tok, output: out_tok, cache_read }).await.ok();

    if stop_reason.is_none() {
        bail!("flux terminé sans stop_reason (connexion coupée ?)");
    }

    Ok(StreamedMessage {
        content: blocks.into_values().collect(),
        stop_reason,
        stop_details,
        invalid_inputs,
    })
}

/// Returns (end of event, length of separator) for the first complete event.
fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn append_str(block: &mut Value, key: &str, s: &str) {
    let mut cur = block[key].as_str().unwrap_or_default().to_string();
    cur.push_str(s);
    block[key] = Value::String(cur);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_event_boundaries() {
        assert_eq!(find_event_end(b"event: a\ndata: {}\n\nrest"), Some((17, 2)));
        assert_eq!(find_event_end(b"data: {}\r\n\r\n"), Some((8, 4)));
        assert_eq!(find_event_end(b"data: {\"partial"), None);
    }

    #[test]
    fn repair_closes_dangling_tool_calls() {
        let history: History = Arc::new(Mutex::new(vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "read_file", "input": {"path": "a"}},
                {"type": "tool_use", "id": "t2", "name": "read_file", "input": {"path": "b"}}
            ]}),
        ]));
        repair(&history);
        let h = history.lock().unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[2]["role"], "user");
        assert_eq!(h[2]["content"][1]["tool_use_id"], "t2");
        assert_eq!(h[2]["content"][0]["is_error"], true);
    }
}
