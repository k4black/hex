//! Subprocess agent adapters: one uniform [`Worker`] surface, one adapter per
//! external coding-agent CLI.
//!
//! Every adapter shares the same spawn/log/emit/result plumbing ([`run_agent`]),
//! but each concrete worker ([`CodexWorker`], [`ClaudeWorker`], [`OpencodeWorker`])
//! encapsulates *its* agent's specifics — argv, final-message capture, and the
//! read-only flag it maps to. [`CommandWorker`] is the generic escape hatch for a
//! custom argv (and for tests). The runtime only ever sees `dyn Worker`.
//!
//! Control channels the runtime injects: `HEX_EMIT_FILE` (the agent's routing
//! signal via `hex emit`), `HEX_RESULT_FILE`/`{result}` (where a worker writes
//! its final message), plus `HEX_RUN_ID`/`HEX_NODE_ID`/`HEX_ATTEMPT_ID`/
//! `HEX_MAY_PROPOSE`.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Command as ProcCommand, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use hex_proto::Capability;

use crate::{CapabilityManifest, WorkOutcome, WorkRequest, Worker};

/// The env var naming the file an agent appends its routing signal to.
pub const EMIT_FILE_ENV: &str = "HEX_EMIT_FILE";

/// The env var (and `{result}` argv token) naming the file a worker writes its
/// final message to, for capture into `{{node.result}}`.
pub const RESULT_FILE_ENV: &str = "HEX_RESULT_FILE";

/// Cap on the *extracted* result value: it feeds a downstream prompt, so a
/// runaway output must not bloat it. Truncated with `…`.
const MAX_RESULT_BYTES: usize = 16 * 1024;

/// Cap on the *raw* stream we read to find the result. Generous so a normal
/// tool-heavy JSONL stream's final answer (which comes last) isn't lost, while
/// still bounding memory; beyond this, JSON parsing fails closed.
const MAX_STREAM_BYTES: u64 = 8 * 1024 * 1024;

/// How a worker's final message is extracted from a completed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCapture {
    /// The worker wrote its final message to the `{result}` file /
    /// `HEX_RESULT_FILE` (e.g. codex `--output-last-message`); read that file.
    File,
    /// stdout is a single JSON object with `result`/`is_error` (e.g. claude
    /// `--output-format json`); take `.result`, and fail on `.is_error`.
    JsonResult,
    /// stdout is JSONL; take the `part.text` of the last `type == "text"` line
    /// (e.g. opencode `run --format json`).
    JsonlLastText,
}

/// Advertise a fresh session per attempt — what all blocking headless adapters
/// actually provide today. (Streaming/resume are declared once implemented.)
fn fresh() -> CapabilityManifest {
    CapabilityManifest::from(&[Capability::FreshSessions])
}

// ---------------------------------------------------------------------------
// Concrete adapters — each owns its agent's argv, capture, and read-only flag.
// ---------------------------------------------------------------------------

/// The OpenAI Codex CLI (`codex exec`).
#[derive(Debug, Clone, Default)]
pub struct CodexWorker {
    /// `--model` override, if any.
    pub model: Option<String>,
}

impl CodexWorker {
    #[must_use]
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }

    /// Build the argv template. `read_only` is advisory only: a true read-only
    /// sandbox (`--sandbox read-only`) would also block the agent from writing
    /// `HEX_EMIT_FILE`/`HEX_RESULT_FILE` under the workspace, breaking the
    /// control channel — so we always use `workspace-write` and rely on the
    /// node's prompt to keep a reviewer from editing. Enforced read-only awaits a
    /// non-workspace control transport (see TODO).
    #[must_use]
    pub fn command(&self, _read_only: bool) -> Vec<String> {
        let mut argv = strs(&[
            "codex",
            "exec",
            "--sandbox",
            "workspace-write",
            "--skip-git-repo-check",
            "--output-last-message",
            "{result}",
        ]);
        if let Some(model) = &self.model {
            argv.push("--model".to_owned());
            argv.push(model.clone());
        }
        argv.push("{prompt}".to_owned());
        argv
    }
}

impl Worker for CodexWorker {
    fn capabilities(&self) -> CapabilityManifest {
        fresh()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "codex",
            &self.command(request.read_only),
            Some(ResultCapture::File),
            request,
        )
    }
}

/// Claude Code headless (`claude -p`).
#[derive(Debug, Clone, Default)]
pub struct ClaudeWorker {
    /// `--model` override, if any.
    pub model: Option<String>,
}

impl ClaudeWorker {
    #[must_use]
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }

    // `_read_only` is advisory (see `CodexWorker::command`): the allow/deny
    // classifier below still leaves write-capable `Bash` (needed for `hex
    // emit`), so read-only can't be enforced here without breaking the control
    // channel. The reviewer's prompt keeps it read-only.
    #[must_use]
    pub fn command(&self, _read_only: bool) -> Vec<String> {
        // Codex confines effects with an OS sandbox (`workspace-write`); Claude
        // has no equivalent flag here, so we approximate an *auto classifier*
        // instead of bypassing every check:
        //   * `acceptEdits` auto-approves in-workspace edits (no stall on Edit/
        //     Write), while dangerous ops still route through the deny-list.
        //   * `--allowedTools` auto-approves the coding essentials (incl. `Bash`,
        //     which the `hex emit` control channel needs). Tools outside this set
        //     (e.g. `WebFetch`) get no approver in headless mode → fail closed.
        //   * `--disallowedTools` denies the genuinely destructive/exfil commands
        //     (deny rules outrank the mode). This is defense-in-depth, NOT a hard
        //     boundary — prefix matching is bypassable via shell chaining, so real
        //     containment still awaits worktree/OS isolation (Phase 2).
        // This never uses `--dangerously-skip-permissions`.
        let mut argv = strs(&[
            "claude",
            "-p",
            "{prompt}",
            "--output-format",
            "json",
            "--permission-mode",
            "acceptEdits",
            "--allowedTools",
            CLAUDE_ALLOWED_TOOLS,
            "--disallowedTools",
            CLAUDE_DENIED_TOOLS,
        ]);
        if let Some(model) = &self.model {
            argv.push("--model".to_owned());
            argv.push(model.clone());
        }
        argv
    }
}

/// Auto-approved tools for headless Claude: the coding essentials plus `Bash`
/// (the `hex emit` control channel is a Bash call). Anything outside this set
/// has no approver in `-p` mode, so it fails closed rather than stalling.
const CLAUDE_ALLOWED_TOOLS: &str = "Read,Grep,Glob,Edit,Write,Bash";

/// Denied Bash invocations (deny outranks the permission mode). A focused,
/// top-level classification of destructive / privilege-escalating / exfil /
/// publishing commands — defense-in-depth, not a security boundary.
const CLAUDE_DENIED_TOOLS: &str = concat!(
    "Bash(sudo:*),Bash(su:*),",
    "Bash(rm -rf:*),Bash(rm -fr:*),",
    "Bash(dd:*),Bash(mkfs:*),Bash(chmod 777:*),Bash(chown:*),",
    "Bash(git push:*),Bash(npm publish:*),Bash(cargo publish:*),",
    "Bash(curl:*),Bash(wget:*)"
);

impl Worker for ClaudeWorker {
    fn capabilities(&self) -> CapabilityManifest {
        fresh()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "claude",
            &self.command(request.read_only),
            Some(ResultCapture::JsonResult),
            request,
        )
    }
}

/// opencode (`opencode run`).
#[derive(Debug, Clone, Default)]
pub struct OpencodeWorker {
    /// `--model provider/model` override, if any.
    pub model: Option<String>,
}

impl OpencodeWorker {
    #[must_use]
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }

    #[must_use]
    pub fn command(&self, _read_only: bool) -> Vec<String> {
        // `--auto` so it never blocks on a permission prompt. Unlike Claude, this
        // is a blanket approve with no deny-list: opencode's classifier lives in
        // an `opencode.json` `permission` block (allow/ask/deny), which hex would
        // have to generate per-run — deferred (see TODO). `read_only` is advisory
        // only (not enforced here — see `CodexWorker::command`).
        let mut argv = strs(&["opencode", "run", "{prompt}", "--auto", "--format", "json"]);
        if let Some(model) = &self.model {
            argv.push("--model".to_owned());
            argv.push(model.clone());
        }
        argv
    }
}

impl Worker for OpencodeWorker {
    fn capabilities(&self) -> CapabilityManifest {
        fresh()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "opencode",
            &self.command(request.read_only),
            Some(ResultCapture::JsonlLastText),
            request,
        )
    }
}

/// A generic worker driven by an explicit argv template (`{prompt}`/`{result}`
/// tokens). The escape hatch for a custom CLI, a shell command, or a test stub.
#[derive(Debug, Clone)]
pub struct CommandWorker {
    /// Registry name (for diagnostics).
    pub name: String,
    /// Argv template, executed directly — never a shell string.
    pub command: Vec<String>,
    /// Advertised capabilities.
    pub capabilities: CapabilityManifest,
    /// How to capture the worker's final message, if at all.
    pub result_capture: Option<ResultCapture>,
}

impl CommandWorker {
    #[must_use]
    pub fn new(name: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            name: name.into(),
            command,
            capabilities: fresh(),
            result_capture: None,
        }
    }

    /// Set how this worker's final message is captured.
    #[must_use]
    pub fn with_result_capture(mut self, capture: Option<ResultCapture>) -> Self {
        self.result_capture = capture;
        self
    }
}

impl Worker for CommandWorker {
    fn capabilities(&self) -> CapabilityManifest {
        self.capabilities.clone()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(&self.name, &self.command, self.result_capture, request)
    }
}

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Shared plumbing.
// ---------------------------------------------------------------------------

/// Run one attempt from an argv template: substitute `{prompt}`/`{result}`,
/// inject the control env, spawn (piping the prompt to stdin when the argv has
/// no `{prompt}`), enforce the deadline, then capture the final message and the
/// routing signal. `Ok(None)` from the emit channel is *implicit completion*.
fn run_agent(
    name: &str,
    command: &[String],
    capture: Option<ResultCapture>,
    request: &WorkRequest,
) -> WorkOutcome {
    if command.is_empty() {
        return WorkOutcome::error(format!("worker `{name}` has an empty command"));
    }

    let emit_file = request.attempt_dir.join("emitted");
    let result_file = request.attempt_dir.join("result.txt");
    // Start clean so a resumed attempt never reads a stale signal/result.
    let _ = fs::remove_file(&emit_file);
    let _ = fs::remove_file(&result_file);

    let uses_placeholder = command.iter().any(|a| a.contains("{prompt}"));
    let result_path = result_file.to_string_lossy();
    let rendered: Vec<String> = command
        .iter()
        .map(|a| {
            a.replace("{prompt}", &request.prompt)
                .replace("{result}", &result_path)
        })
        .collect();

    let mut cmd = match logged_command(&rendered, &request.workdir, &request.attempt_dir) {
        Ok(cmd) => cmd,
        Err(e) => return WorkOutcome::error(format!("could not prepare attempt: {e}")),
    };
    cmd.env("HEX_RUN_ID", &request.run_id)
        .env("HEX_NODE_ID", &request.node_id)
        .env("HEX_ATTEMPT_ID", &request.attempt_id)
        .env(EMIT_FILE_ENV, &emit_file)
        .env(RESULT_FILE_ENV, &result_file)
        .env("HEX_MAY_PROPOSE", request.may_propose.join(","))
        .stdin(if uses_placeholder {
            Stdio::null()
        } else {
            Stdio::piped()
        });

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return WorkOutcome::error(format!("spawn `{}` failed: {e}", rendered[0])),
    };

    if !uses_placeholder && let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(request.prompt.as_bytes());
        // drop closes stdin
    }

    let status = match wait_bounded(&mut child, request.deadline_ms) {
        Ok(Some(status)) => status,
        Ok(None) => return WorkOutcome::timed_out("attempt exceeded its time budget (killed)"),
        Err(e) => return WorkOutcome::error(format!("wait failed: {e}")),
    };

    // A nonzero exit is an infrastructure/agent failure, not a routing proposal.
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string());
        return WorkOutcome::error(format!("agent exited nonzero (exit {code})"));
    }

    // Capture the final message (a JSON-mode agent may report `is_error` even on
    // a clean exit — that's a failure).
    let result = match capture_result(capture, &request.attempt_dir, &result_file) {
        Ok(result) => result,
        Err(reason) => return WorkOutcome::error(reason),
    };

    // No emit on a clean exit is *implicit completion* (signal `None`); the
    // runtime synthesizes `done`. A disallowed/ambiguous emit is an error.
    match read_signal(&emit_file, &request.may_propose) {
        Ok(signal) => WorkOutcome {
            signal,
            result,
            error: None,
            timed_out: false,
        },
        Err(reason) => WorkOutcome::error(reason).with_result(result),
    }
}

/// Extract the worker's final message per its [`ResultCapture`] mode, capped so
/// a runaway output can't bloat a downstream prompt. `Err` means the agent
/// reported an error (e.g. JSON `is_error`) even on a clean exit.
fn capture_result(
    mode: Option<ResultCapture>,
    attempt_dir: &Path,
    result_file: &Path,
) -> Result<Option<String>, String> {
    let raw: Option<String> = match mode {
        None => None,
        Some(ResultCapture::File) => match read_capped(result_file, MAX_STREAM_BYTES) {
            Ok(text) => Some(text),
            // A missing file is a legitimate "no result"; any other error (perms)
            // fails closed rather than becoming a silent success.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read result file: {e}")),
        },
        Some(ResultCapture::JsonResult) => {
            // A single JSON document must be intact to be meaningful, so read it
            // whole with strict UTF-8 and explicit truncation detection (an
            // over-cap or non-UTF-8 output fails closed, never silently becomes
            // `�` and then "not JSON").
            let out = read_bounded_utf8(&attempt_dir.join("stdout.log"), MAX_STREAM_BYTES)?;
            let v: serde_json::Value = serde_json::from_str(out.trim())
                .map_err(|e| format!("agent output is not JSON: {e}"))?;
            if v.get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                let msg = v
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("agent reported an error");
                return Err(format!("agent reported an error: {msg}"));
            }
            v.get("result")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        }
        Some(ResultCapture::JsonlLastText) => {
            // Stream to EOF (bounded per line) so the *final* text event is never
            // missed by a mid-file cap — a stale earlier text would misrepresent
            // the run. An over-long single line fails closed.
            last_jsonl_text_file(&attempt_dir.join("stdout.log"), MAX_STREAM_BYTES)?
        }
    };
    Ok(raw
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(cap_result))
}

/// Read at most `max` bytes of `path`, decoded lossily so a cut through a
/// multi-byte codepoint yields `�` rather than an error (the cap can land
/// mid-codepoint). Only IO errors (missing file, perms) propagate. Used for the
/// raw final-message file, where a lossy tail is preferable to failing.
fn read_capped(path: &Path, max: u64) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, max), &mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Read `path` fully as strict UTF-8, failing closed if it exceeds `max` bytes
/// (explicit truncation detection) or is not valid UTF-8 — so a document that
/// must be intact (a single JSON object) never silently loses its tail or gets
/// altered by lossy `�` substitution.
fn read_bounded_utf8(path: &Path, max: u64) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("cannot read agent output: {e}"))?;
    let mut buf = Vec::new();
    // Read one byte past the cap so "exactly max" is distinguishable from "over".
    std::io::Read::read_to_end(&mut std::io::Read::take(file, max + 1), &mut buf)
        .map_err(|e| format!("cannot read agent output: {e}"))?;
    if buf.len() as u64 > max {
        return Err(format!("agent output exceeds {max} bytes"));
    }
    String::from_utf8(buf).map_err(|_| "agent output is not valid UTF-8".to_owned())
}

/// The `part.text` of a single JSONL line if it is a `type == "text"` event.
/// Non-JSON / non-text lines yield `None` and are skipped by the caller.
fn text_part(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(serde_json::Value::as_str) != Some("text") {
        return None;
    }
    v.get("part")
        .and_then(|p| p.get("text"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

/// The `part.text` of the last `type == "text"` line in a JSONL file (opencode),
/// streamed to EOF so a final event past any cap is never missed. Memory is
/// bounded to one line plus the retained text; a single line exceeding `max`
/// bytes fails closed rather than silently returning a stale earlier text.
fn last_jsonl_text_file(path: &Path, max: u64) -> Result<Option<String>, String> {
    let file = File::open(path).map_err(|e| format!("cannot read agent output: {e}"))?;
    let mut reader = std::io::BufReader::new(file);
    let cap = usize::try_from(max).unwrap_or(usize::MAX);
    let mut last = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = read_line_bounded(&mut reader, &mut line, cap)
            .map_err(|e| format!("cannot read agent output: {e}"))?;
        if n == 0 {
            break; // EOF
        }
        // Filled the cap without a terminating newline → the line is over-long.
        if line.len() >= cap && !line.ends_with(b"\n") {
            return Err(format!("agent output line exceeds {max} bytes"));
        }
        // A line that isn't valid UTF-8 is not our JSON event — skip it.
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if let Some(t) = text_part(text) {
            last = Some(t);
        }
    }
    Ok(last)
}

/// Append one newline-terminated line (or the trailing unterminated remainder)
/// from `reader` into `buf`, reading at most `cap` bytes so a single pathological
/// line can't exhaust memory. Returns bytes read (`0` = EOF).
fn read_line_bounded<R: std::io::BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<usize> {
    let mut read = 0usize;
    while read < cap {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break; // EOF
        }
        let room = cap - read;
        match available.iter().take(room).position(|&b| b == b'\n') {
            Some(pos) => {
                buf.extend_from_slice(&available[..=pos]);
                reader.consume(pos + 1);
                return Ok(read + pos + 1);
            }
            None => {
                let take = available.len().min(room);
                buf.extend_from_slice(&available[..take]);
                reader.consume(take);
                read += take;
            }
        }
    }
    Ok(read)
}

/// Truncate a captured result so the *final* value (including the `…` marker) is
/// at most [`MAX_RESULT_BYTES`] bytes, cut at a char boundary. Takes the string
/// by value and returns it untouched in the common (under-cap) case — no
/// re-allocation unless truncation is actually needed.
fn cap_result(s: String) -> String {
    if s.len() <= MAX_RESULT_BYTES {
        return s;
    }
    let mut end = MAX_RESULT_BYTES - '…'.len_utf8();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Build a [`ProcCommand`] for `argv` in `cwd`, capturing stdout/stderr to
/// `attempt_dir/{stdout,stderr}.log`. The caller adds any env/stdin and spawns.
/// Shared by the agent adapters and the runtime's gate executor.
///
/// # Errors
/// Fails on an empty argv or if a log file cannot be created.
pub fn logged_command(
    argv: &[String],
    cwd: &Path,
    attempt_dir: &Path,
) -> std::io::Result<ProcCommand> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    let stdout = File::create(attempt_dir.join("stdout.log"))?;
    let stderr = File::create(attempt_dir.join("stderr.log"))?;
    let mut cmd = ProcCommand::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    Ok(cmd)
}

/// Wait for `child`, killing it if it outlives `deadline_ms`. `Ok(None)` means
/// the deadline fired and the child was killed. Shared by the agent adapters and
/// the runtime's gate executor so both honor per-attempt time budgets.
pub fn wait_bounded(
    child: &mut Child,
    deadline_ms: Option<u64>,
) -> std::io::Result<Option<ExitStatus>> {
    let Some(budget) = deadline_ms else {
        return child.wait().map(Some);
    };
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) >= budget {
            // Verify termination rather than assuming kill succeeded.
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Read the emitted routing signal. `Ok(None)` = the agent emitted nothing (a
/// clean finish → implicit completion, the runtime synthesizes `done`).
/// `Ok(Some(name))` = exactly one distinct signal, and it is in `may_propose`.
/// `Err` = a disallowed signal or multiple ambiguous emissions.
fn read_signal(
    emit_file: &std::path::Path,
    may_propose: &[String],
) -> Result<Option<String>, String> {
    // Bound the read: a faulty agent must not exhaust memory. Signals are tiny.
    const MAX_EMIT_BYTES: u64 = 64 * 1024;
    let file = match fs::File::open(emit_file) {
        Ok(file) => file,
        // No file = nothing proposed (implicit completion). Any *other* open
        // error (perms, …) fails closed rather than silently routing `done`.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not open emit file: {e}")),
    };
    let mut contents = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(file, MAX_EMIT_BYTES),
        &mut contents,
    )
    .map_err(|e| format!("could not read emit file: {e}"))?;
    let distinct: BTreeSet<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match distinct.len() {
        0 => Ok(None),
        1 => {
            let signal = *distinct.iter().next().expect("one element");
            if may_propose.iter().any(|allowed| allowed == signal) {
                Ok(Some(signal.to_owned()))
            } else {
                Err(format!(
                    "agent emitted `{signal}` which is not in may_propose"
                ))
            }
        }
        _ => {
            let mut names: Vec<&str> = distinct.into_iter().collect();
            names.sort_unstable();
            Err(format!(
                "agent emitted multiple signals: {}",
                names.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hex-agent-test-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn request(dir: &std::path::Path, may: &[&str]) -> WorkRequest {
        WorkRequest {
            run_id: "run_0".to_owned(),
            node_id: "implement".to_owned(),
            attempt_id: "att_1".to_owned(),
            prompt: "hello".to_owned(),
            may_propose: may.iter().map(|s| (*s).to_owned()).collect(),
            workdir: dir.to_path_buf(),
            attempt_dir: dir.to_path_buf(),
            deadline_ms: None,
            read_only: false,
        }
    }

    #[test]
    fn reads_emitted_signal_from_the_child() {
        let dir = temp_dir("emit");
        let worker = CommandWorker::new(
            "fake",
            strs(&["sh", "-c", "printf ready > \"$HEX_EMIT_FILE\""]),
        );
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert_eq!(outcome, WorkOutcome::signal("ready"));
    }

    #[test]
    fn disallowed_signal_is_an_error() {
        let dir = temp_dir("disallowed");
        let worker = CommandWorker::new(
            "fake",
            strs(&["sh", "-c", "printf sneaky > \"$HEX_EMIT_FILE\""]),
        );
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert!(outcome.error.is_some());
    }

    #[test]
    fn no_emit_is_implicit_completion() {
        let dir = temp_dir("silent");
        // A clean exit with no emit is not an error — it's implicit completion.
        let worker = CommandWorker::new("fake", strs(&["true"]));
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert_eq!(outcome.signal, None);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn captures_the_final_result_from_the_result_file() {
        let dir = temp_dir("result-file");
        let worker = CommandWorker::new(
            "fake",
            strs(&["sh", "-c", "printf 'the plan' > \"$HEX_RESULT_FILE\""]),
        )
        .with_result_capture(Some(ResultCapture::File));
        let outcome = worker.run(&request(&dir, &[]));
        assert_eq!(outcome.signal, None, "no emit → implicit completion");
        assert_eq!(outcome.result.as_deref(), Some("the plan"));
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn result_and_signal_are_captured_together() {
        let dir = temp_dir("result-signal");
        let worker = CommandWorker::new(
            "fake",
            strs(&[
                "sh",
                "-c",
                "printf summary > \"$HEX_RESULT_FILE\"; printf approved > \"$HEX_EMIT_FILE\"",
            ]),
        )
        .with_result_capture(Some(ResultCapture::File));
        let outcome = worker.run(&request(&dir, &["approved"]));
        assert_eq!(outcome.signal.as_deref(), Some("approved"));
        assert_eq!(outcome.result.as_deref(), Some("summary"));
    }

    #[test]
    fn signal_from_a_failed_process_is_rejected() {
        let dir = temp_dir("nonzero");
        let worker = CommandWorker::new(
            "fake",
            strs(&["sh", "-c", "printf ready > \"$HEX_EMIT_FILE\"; exit 3"]),
        );
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert!(outcome.signal.is_none());
        assert!(outcome.error.unwrap().contains("nonzero"));
    }

    #[test]
    fn deadline_kills_a_slow_child() {
        let dir = temp_dir("deadline");
        let worker = CommandWorker::new("fake", strs(&["sh", "-c", "sleep 30"]));
        let mut req = request(&dir, &["ready"]);
        req.deadline_ms = Some(100);
        let start = std::time::Instant::now();
        let outcome = worker.run(&req);
        assert!(start.elapsed().as_secs() < 5, "must not wait for the child");
        assert!(outcome.error.unwrap().contains("time budget"));
    }

    #[test]
    fn codex_argv_shape() {
        let w = CodexWorker::new(Some("gpt-5".to_owned()));
        let argv = w.command(false).join(" ");
        assert!(argv.contains("--output-last-message {result}"), "{argv}");
        assert!(
            argv.contains("--model gpt-5") && argv.ends_with("{prompt}"),
            "{argv}"
        );
        // read_only is advisory: it must NOT switch to a sandbox that would block
        // writing the emit/result files — workspace-write in both cases.
        assert!(
            w.command(false)
                .join(" ")
                .contains("--sandbox workspace-write")
        );
        assert!(
            w.command(true)
                .join(" ")
                .contains("--sandbox workspace-write")
        );
    }

    #[test]
    fn claude_and_opencode_argv_shapes() {
        let c = ClaudeWorker::new(None).command(false).join(" ");
        assert!(c.contains("claude -p {prompt} --output-format json"), "{c}");
        let o = OpencodeWorker::new(None).command(false).join(" ");
        assert!(
            o.contains("opencode run {prompt} --auto --format json"),
            "{o}"
        );
    }

    #[test]
    fn claude_uses_an_allow_deny_classifier_not_a_blanket_bypass() {
        let c = ClaudeWorker::new(None).command(false).join(" ");
        // A real classifier — never the blanket bypass the operator rejected.
        assert!(!c.contains("--dangerously-skip-permissions"), "{c}");
        assert!(c.contains("--permission-mode acceptEdits"), "{c}");
        // The control channel (`hex emit`, a Bash call) must stay approvable.
        assert!(c.contains("--allowedTools"), "{c}");
        assert!(c.contains("Bash"), "{c}");
        // …while the destructive/exfil set is denied.
        assert!(c.contains("--disallowedTools"), "{c}");
        assert!(c.contains("Bash(sudo:*)"), "{c}");
        assert!(c.contains("Bash(git push:*)"), "{c}");
    }

    #[test]
    fn cap_result_bounds_the_final_value_including_the_marker() {
        let big = "x".repeat(MAX_RESULT_BYTES * 2);
        let capped = cap_result(big);
        assert!(
            capped.len() <= MAX_RESULT_BYTES,
            "capped len {}",
            capped.len()
        );
        assert!(capped.ends_with('…'));
        // A multibyte char straddling the cut must not panic or corrupt.
        let multi = "é".repeat(MAX_RESULT_BYTES);
        let capped = cap_result(multi);
        assert!(capped.len() <= MAX_RESULT_BYTES);
    }

    #[test]
    fn text_part_extracts_only_text_events() {
        assert_eq!(
            text_part("{\"type\":\"text\",\"part\":{\"text\":\"hi\"}}").as_deref(),
            Some("hi")
        );
        assert_eq!(text_part("{\"type\":\"tool_use\",\"part\":{}}"), None);
        assert_eq!(text_part("not json"), None);
    }

    #[test]
    fn jsonl_last_text_file_reads_final_text_after_a_large_prefix() {
        // A tool-heavy stream places the final answer well past any small cap;
        // streaming to EOF must still find it (not a stale earlier text).
        let dir = temp_dir("jsonl-tail");
        let path = dir.join("stdout.log");
        let mut stream = String::new();
        for i in 0..5000 {
            stream.push_str(&format!(
                "{{\"type\":\"tool_use\",\"part\":{{\"n\":{i}}}}}\n"
            ));
        }
        stream.push_str("{\"type\":\"text\",\"part\":{\"text\":\"the final answer\"}}\n");
        assert!(stream.len() > 100_000, "prefix is large: {}", stream.len());
        std::fs::write(&path, &stream).unwrap();
        assert_eq!(
            last_jsonl_text_file(&path, MAX_STREAM_BYTES)
                .unwrap()
                .as_deref(),
            Some("the final answer")
        );
    }

    #[test]
    fn capture_result_jsonl_uses_the_full_file_path() {
        // Exercise capture_result end-to-end: the final text lives past a big
        // prefix, so a prefix-only reader would return a stale value.
        let dir = temp_dir("jsonl-capture");
        let mut stream = String::new();
        stream.push_str("{\"type\":\"text\",\"part\":{\"text\":\"early\"}}\n");
        stream.push_str(&"{\"type\":\"tool_use\",\"part\":{}}\n".repeat(10_000));
        stream.push_str("{\"type\":\"text\",\"part\":{\"text\":\"the real final\"}}\n");
        std::fs::write(dir.join("stdout.log"), &stream).unwrap();
        let got = capture_result(
            Some(ResultCapture::JsonlLastText),
            &dir,
            &dir.join("unused"),
        );
        assert_eq!(got.unwrap().as_deref(), Some("the real final"));
    }

    #[test]
    fn jsonl_last_text_file_fails_closed_on_an_over_long_line() {
        let dir = temp_dir("jsonl-overlong");
        let path = dir.join("stdout.log");
        // A single line far exceeding the (tiny) cap must error, not silently
        // return an earlier text.
        std::fs::write(&path, format!("{}\n", "x".repeat(200))).unwrap();
        assert!(last_jsonl_text_file(&path, 64).is_err());
    }

    #[test]
    fn read_bounded_utf8_fails_closed_when_over_cap() {
        let dir = temp_dir("bounded-utf8");
        let path = dir.join("blob");
        std::fs::write(&path, "y".repeat(100)).unwrap();
        assert!(read_bounded_utf8(&path, 16).is_err());
        assert_eq!(read_bounded_utf8(&path, 1000).unwrap().len(), 100);
    }

    #[test]
    fn read_capped_is_lossy_and_bounded() {
        let dir = temp_dir("read-capped");
        let path = dir.join("blob");
        // A multi-byte char right at the cap must not error (lossy decode).
        let mut bytes = vec![b'x'; 15];
        bytes.extend_from_slice("é".as_bytes()); // 2 bytes, straddles the cap of 16
        std::fs::write(&path, &bytes).unwrap();
        let got = read_capped(&path, 16).expect("lossy read never errors mid-codepoint");
        assert!(
            got.starts_with(&"x".repeat(15)),
            "content preserved: {got:?}"
        );
        // 16 input bytes; a truncated codepoint becomes the 3-byte `�`.
        assert!(got.len() <= 16 + 3, "byte-bounded read: {}", got.len());
    }
}
