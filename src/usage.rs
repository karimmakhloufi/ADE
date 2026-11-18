//! Subscription limits (% of the 5-hour window and of the week), as reported
//! by Claude Code in the `rate_limit_event` of its stream-json output.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local};

/// One window of the subscription limits.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// 0.0 – 1.0
    pub utilization: f64,
    /// Unix seconds.
    pub resets_at: i64,
}

impl Window {
    fn parse(v: &serde_json::Value) -> Option<Self> {
        Some(Self { utilization: v["utilization"].as_f64()?, resets_at: v["resetsAt"].as_i64()? })
    }

    /// Percentage, or `None` once the window has reset (value unknown until
    /// the next message).
    pub fn current(&self, now: i64) -> Option<f64> {
        (now < self.resets_at).then_some(self.utilization * 100.0)
    }

    /// "27 % · réinitialisation mer. 18/11 15:00"
    pub fn describe(&self, now: i64) -> String {
        let reset = DateTime::from_timestamp(self.resets_at, 0)
            .map(|t| t.with_timezone(&Local).format("%d/%m %H:%M").to_string())
            .unwrap_or_default();
        match self.current(now) {
            Some(p) => format!("{p:.0} %  ·  réinitialisation le {reset}"),
            None => format!("réinitialisée le {reset}"),
        }
    }
}

/// Subscription limits as last reported by Claude Code.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl Limits {
    /// From the `rate_limit_info` of a `rate_limit_event`.
    pub fn parse(info: &serde_json::Value) -> Self {
        let w = &info["unifiedWindows"];
        Self { five_hour: Window::parse(&w["five_hour"]), seven_day: Window::parse(&w["seven_day"]) }
    }
}

/// Sends a minimal request through Claude Code (Haiku, no tools, session not
/// saved) only to read the `rate_limit_info` it reports. Stops Claude Code as
/// soon as the info is received. `None` on failure or after 60 s.
pub async fn probe_limits(claude: PathBuf) -> Option<serde_json::Value> {
    use tokio::io::AsyncBufReadExt;
    let mut cmd = tokio::process::Command::new(claude);
    cmd.args(["-p", "ok", "--output-format", "stream-json", "--verbose", "--model", "haiku"])
        .args(["--no-session-persistence", "--tools", ""])
        .current_dir(std::env::temp_dir())
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(crate::agent::CREATE_NO_WINDOW);
    let mut child = cmd.spawn().ok()?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take()?).lines();
    let read = async {
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if v["type"] == "rate_limit_event" && v["rate_limit_info"].is_object() {
                return Some(v["rate_limit_info"].clone());
            }
        }
        None
    };
    let info = tokio::time::timeout(Duration::from_secs(60), read).await.ok().flatten();
    let _ = child.kill().await;
    info
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_limits() {
        let info = serde_json::json!({"unifiedWindows": {
            "five_hour": {"utilization": 0.27, "resetsAt": 2_000_000_000i64},
            "seven_day": {"utilization": 0.03, "resetsAt": 1_000}}});
        let l = super::Limits::parse(&info);
        assert_eq!(l.five_hour.unwrap().current(1_900_000_000).map(|p| p.round()), Some(27.0));
        assert_eq!(l.seven_day.unwrap().current(1_900_000_000), None);
    }

    #[tokio::test]
    #[ignore]
    async fn probe_real() {
        let claude = crate::config::find_claude().expect("claude");
        let info = super::probe_limits(claude).await.expect("rate_limit_info");
        println!("{:?}", super::Limits::parse(&info));
    }
}
