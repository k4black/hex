//! Turning an agent's machine stream back into something a human can watch.
//!
//! Both supported agents stream JSONL so hex can read their session id and
//! usage — which means a raw tail of an attempt shows a 6 KB `system:init` line
//! enumerating every installed tool before the agent has said anything. Making
//! the log *visible* and making it *readable* are two different jobs; this is
//! the second.
//!
//! Presentation, so it lives in the client. It recognises the two streams hex
//! itself produces and passes anything else through untouched — a `kind:
//! command` worker printing plain text must not be swallowed by a JSON parser.

/// Render one line of an attempt's captured stdout for a human.
///
/// `None` means "carries nothing worth a line": session banners, token-estimate
/// pings, rate-limit notices, and the final result object (which the caller
/// already shows in full as the attempt's final message).
#[must_use]
pub fn humanize(line: &str) -> Option<String> {
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    // Not JSON: a plain-argv worker, or an agent writing prose. Pass it through
    // rather than deciding it is noise.
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return Some(line.to_owned());
    };
    let kind = v.get("type").and_then(serde_json::Value::as_str)?;
    match kind {
        // claude: the only lines carrying the agent's actual work.
        "assistant" => claude_message(&v),
        // codex: one completed item — a message, a command, a file edit.
        "item.completed" => codex_item(&v),
        // Everything else is bookkeeping the operator did not ask to watch.
        _ => None,
    }
}

/// claude's `assistant` line: prose blocks verbatim, tool calls as one line.
fn claude_message(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?.as_array()?;
    let mut out: Vec<String> = Vec::new();
    for block in content {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(serde_json::Value::as_str) {
                    let t = t.trim();
                    if !t.is_empty() {
                        out.push(t.to_owned());
                    }
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("tool");
                out.push(match tool_target(block) {
                    Some(target) => format!("· {name} {target}"),
                    None => format!("· {name}"),
                });
            }
            _ => {}
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// The one field of a tool call worth showing: what it acted on.
///
/// A whole `input` object would be a wall of JSON — often the entire contents of
/// a file being written — which is exactly the noise this module exists to stop.
fn tool_target(block: &serde_json::Value) -> Option<String> {
    let input = block.get("input")?;
    for key in [
        "file_path",
        "path",
        "command",
        "pattern",
        "url",
        "description",
    ] {
        if let Some(v) = input.get(key).and_then(serde_json::Value::as_str) {
            return Some(truncate(v, 100));
        }
    }
    None
}

/// codex's `item.completed`: the agent's message, or what it just did.
fn codex_item(v: &serde_json::Value) -> Option<String> {
    let item = v.get("item")?;
    let kind = item.get("type").and_then(serde_json::Value::as_str)?;
    match kind {
        "agent_message" => item
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty()),
        "reasoning" => None,
        other => {
            let detail = ["command", "path", "file_path"]
                .iter()
                .find_map(|k| item.get(*k).and_then(serde_json::Value::as_str));
            Some(match detail {
                Some(d) => format!("· {other} {}", truncate(d, 100)),
                None => format!("· {other}"),
            })
        }
    }
}

/// Cut at a char boundary so a multi-byte path cannot panic the renderer.
fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines a tail must not show.
    #[test]
    fn hidden() {
        for (case, line) in [
            // The line that made streaming worse than silence: 6 KB of session
            // banner listing every installed tool, before the agent did anything.
            (
                "session banner",
                r#"{"type":"system","subtype":"init","tools":["Task","Bash"],"session_id":"x"}"#,
            ),
            (
                "thinking-token ping",
                r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":50}"#,
            ),
            (
                "rate limit",
                r#"{"type":"rate_limit_event","rate_limit_info":{}}"#,
            ),
            (
                "codex thread start",
                r#"{"type":"thread.started","thread_id":"019f"}"#,
            ),
            (
                "codex turn usage",
                r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#,
            ),
            // The final message, which the caller prints in full — showing it
            // twice is not a tail, it is an echo.
            (
                "result object",
                r#"{"type":"result","result":"all done","total_cost_usd":0.1}"#,
            ),
            (
                "codex reasoning",
                r#"{"type":"item.completed","item":{"type":"reasoning","text":"..."}}"#,
            ),
            ("blank line", "   "),
        ] {
            assert_eq!(humanize(line), None, "{case}");
        }
    }

    /// Lines a tail shows, and as what.
    #[test]
    fn shown() {
        for (case, line, want) in [
            (
                "claude prose",
                r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"Reading the driver to see how attempts start."}]}}"#,
                "Reading the driver to see how attempts start.",
            ),
            (
                "codex agent message",
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"hi"}}"#,
                "hi",
            ),
            // A `kind: command` worker prints plain text and must not be mistaken
            // for an agent stream and dropped.
            ("plain output", "running 3 tests", "running 3 tests"),
        ] {
            assert_eq!(humanize(line).as_deref(), Some(want), "{case}");
        }
    }

    /// A tool call shows *what it touched*, never its whole input — a Write's
    /// input is the entire file being written.
    #[test]
    fn a_tool_call_shows_its_target_not_its_payload() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Write","input":{
                "file_path":"crates/hex-cli/src/graph_view.rs",
                "content":"a thousand lines of file content"}}]}}"#;
        let out = humanize(line).expect("a tool line");
        assert_eq!(out, "· Write crates/hex-cli/src/graph_view.rs");
        assert!(!out.contains("thousand"), "the payload stays out: {out}");
    }

    #[test]
    fn a_long_tool_target_is_cut_at_a_char_boundary() {
        let long = "é".repeat(300);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[
                {{"type":"tool_use","name":"Read","input":{{"file_path":"{long}"}}}}]}}}}"#
        );
        let out = humanize(&line).expect("a tool line");
        assert!(out.ends_with('…'), "{out}");
    }
}
