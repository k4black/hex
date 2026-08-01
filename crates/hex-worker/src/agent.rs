//! Subprocess agent adapters: one uniform [`Worker`] surface, one adapter per
//! external coding-agent CLI.
//!
//! Every adapter shares the same spawn/log/emit/result plumbing (`run_agent`),
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

use hex_proto::{Capability, ModelUsage};

use crate::{AttemptReport, CapabilityManifest, WorkOutcome, WorkRequest, Worker};

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
///
/// Deserializable so project config names these modes directly, rather than a
/// parallel config-side enum plus a `From` bridge that had to be kept in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Cap on the bounded log tail kept as a *partial* result for an attempt that
/// died before reporting one. Small: it feeds a downstream prompt and a human's
/// terminal, and the newest output is the informative part.
const MAX_PARTIAL_BYTES: u64 = 8 * 1024;

/// Advertise a fresh session per attempt — what all blocking headless adapters
/// actually provide today. (Streaming is declared once implemented.)
fn fresh() -> CapabilityManifest {
    CapabilityManifest::from(&[Capability::FreshSessions])
}

/// A fresh session per attempt, plus resumption of a prior one — what an adapter
/// that can honour `context: continue` and report its spend declares.
fn resumable() -> CapabilityManifest {
    CapabilityManifest::from(&[
        Capability::FreshSessions,
        Capability::SessionResume,
        Capability::CostReporting,
    ])
}

/// Where an adapter's session id and usage come from. Deliberately separate from
/// [`ResultCapture`]: a `CommandWorker` may well capture its result from a file
/// like codex does without emitting codex's event stream, so the capture mode
/// cannot stand in for the agent's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    /// `codex exec --json` JSONL on stdout: `thread.started` carries the resume
    /// id, `turn.completed.usage` the tokens. No model, no cost.
    CodexJsonl,
    /// `claude -p --output-format json`: one object with `session_id`,
    /// `total_cost_usd`, `duration_ms` and a per-model `modelUsage` map.
    ClaudeJson,
}

// ---------------------------------------------------------------------------
// Concrete adapters — each owns its agent's argv, capture, and read-only flag.
// ---------------------------------------------------------------------------

/// The OpenAI Codex CLI (`codex exec`).
#[derive(Debug, Clone, Default)]
pub struct CodexWorker {
    /// `--model` override, if any.
    pub model: Option<String>,
    /// Reasoning effort, passed as a `-c model_reasoning_effort=…` override.
    pub effort: Option<String>,
}

impl CodexWorker {
    #[must_use]
    pub fn new(model: Option<String>) -> Self {
        Self {
            model,
            effort: None,
        }
    }

    /// Set the reasoning effort (builder style).
    #[must_use]
    pub fn with_effort(mut self, effort: Option<String>) -> Self {
        self.effort = effort;
        self
    }

    /// Build the argv template. `read_only` is advisory only: a true read-only
    /// sandbox (`--sandbox read-only`) would also block the agent from writing
    /// `HEX_EMIT_FILE`/`HEX_RESULT_FILE` under the workspace, breaking the
    /// control channel — so we always use `workspace-write` and rely on the
    /// node's prompt to keep a reviewer from editing. Enforced read-only awaits a
    /// non-workspace control transport (see TODO).
    #[must_use]
    pub fn command(&self, extra_writable: Option<&Path>, resume: Option<&str>) -> Vec<String> {
        let mut argv = strs(&["codex", "exec"]);
        // `resume` is a *subcommand*, and it accepts a strictly smaller flag set
        // than `exec`: no `--sandbox`, no `--add-dir`, no `--cd`. Everything those
        // would have expressed has to travel as a `-c` config override instead.
        // (Verified against the CLI: `codex exec resume --help`.)
        if let Some(session) = resume {
            argv.push("resume".to_owned());
            argv.push(session.to_owned());
        }
        argv.extend(strs(&[
            // JSONL on stdout, which is the only place codex exposes its session
            // id (`thread.started`) and its token counts (`turn.completed`). It
            // composes with `--output-last-message`, so result capture is
            // untouched; the cost is that stdout.log is events rather than prose.
            "--json",
            "--skip-git-repo-check",
            "--output-last-message",
            "{result}",
        ]));
        // Under worktree isolation the control files live outside the workspace,
        // so the sandbox must be told that directory is writable, or `hex emit`/
        // result capture would be blocked.
        let extra_dir = extra_writable.map(|d| d.to_string_lossy().into_owned());
        if resume.is_some() {
            // The config-override spellings of the two flags `resume` lacks. Both
            // key names verified against the CLI with `--strict-config`, which
            // rejects an unknown field before contacting the model.
            argv.push("-c".to_owned());
            argv.push("sandbox_mode=\"workspace-write\"".to_owned());
            if let Some(dir) = &extra_dir {
                argv.push("-c".to_owned());
                argv.push(format!(
                    "sandbox_workspace_write.writable_roots=[\"{dir}\"]"
                ));
            }
        } else {
            argv.push("--sandbox".to_owned());
            argv.push("workspace-write".to_owned());
            push_flag(&mut argv, "--add-dir", extra_dir.as_deref());
        }
        push_flag(&mut argv, "--model", self.model.as_deref());
        // codex takes reasoning effort as a config override, not a flag.
        let effort = self
            .effort
            .as_ref()
            .map(|e| format!("model_reasoning_effort=\"{e}\""));
        push_flag(&mut argv, "-c", effort.as_deref());
        argv.push("{prompt}".to_owned());
        argv
    }
}

impl Worker for CodexWorker {
    fn program(&self) -> Option<&str> {
        Some("codex")
    }
    fn capabilities(&self) -> CapabilityManifest {
        resumable()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "codex",
            &self.command(
                request.extra_writable_dir.as_deref(),
                request.resume_session.as_deref(),
            ),
            Some(ResultCapture::File),
            Some(UsageSource::CodexJsonl),
            // codex's event stream names no model, so the configured one is the
            // only label available for the per-model split.
            self.model.as_deref(),
            request,
        )
    }
}

/// Claude Code headless (`claude -p`).
#[derive(Debug, Clone, Default)]
pub struct ClaudeWorker {
    /// `--model` override, if any.
    pub model: Option<String>,
    /// Reasoning effort, mapped to claude's own effort scale.
    pub effort: Option<String>,
}

impl ClaudeWorker {
    #[must_use]
    pub fn new(model: Option<String>) -> Self {
        Self {
            model,
            effort: None,
        }
    }

    /// Set the reasoning effort (builder style).
    #[must_use]
    pub fn with_effort(mut self, effort: Option<String>) -> Self {
        self.effort = effort;
        self
    }

    // Read-only is advisory here: the allow/deny classifier below still leaves
    // write-capable `Bash` (needed for `hex emit`), so it can't be enforced
    // without breaking the control channel. The role's prompt keeps it
    // read-only. (Enforced read-only awaits a non-workspace control transport.)
    #[must_use]
    pub fn command(&self, resume: Option<&str>) -> Vec<String> {
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
        push_flag(&mut argv, "--model", self.model.as_deref());
        push_flag(&mut argv, "--effort", self.effort.as_deref());
        // Unlike codex's subcommand, claude resumes with a plain flag, so the
        // rest of the argv is unchanged.
        push_flag(&mut argv, "--resume", resume);
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
    fn program(&self) -> Option<&str> {
        Some("claude")
    }
    fn capabilities(&self) -> CapabilityManifest {
        resumable()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "claude",
            &self.command(request.resume_session.as_deref()),
            Some(ResultCapture::JsonResult),
            Some(UsageSource::ClaudeJson),
            // claude reports its own model per entry, so no hint is needed.
            None,
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
    pub fn command(&self) -> Vec<String> {
        // `--auto` so it never blocks on a permission prompt. Unlike Claude, this
        // is a blanket approve with no deny-list: opencode's classifier lives in
        // an `opencode.json` `permission` block (allow/ask/deny), which hex would
        // have to generate per-run — deferred (see TODO). `read_only` is advisory
        // only (not enforced here — see `CodexWorker::command`).
        let mut argv = strs(&["opencode", "run", "{prompt}", "--auto", "--format", "json"]);
        push_flag(&mut argv, "--model", self.model.as_deref());
        argv
    }
}

impl Worker for OpencodeWorker {
    fn program(&self) -> Option<&str> {
        Some("opencode")
    }
    fn capabilities(&self) -> CapabilityManifest {
        fresh()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "opencode",
            &self.command(),
            Some(ResultCapture::JsonlLastText),
            // opencode's JSON stream is not parsed for usage or a session id, so
            // it reports neither rather than reporting a guess.
            None,
            None,
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
    fn program(&self) -> Option<&str> {
        self.command.first().map(String::as_str)
    }
    fn capabilities(&self) -> CapabilityManifest {
        self.capabilities.clone()
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            &self.name,
            &self.command,
            self.result_capture,
            None,
            None,
            request,
        )
    }
}

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// Append `flag <value>` when `value` is set, and nothing when it is not — the
/// shape every optional argv pair here takes (`--model`, `--effort`, `--add-dir`,
/// codex's `-c <override>`).
fn push_flag(argv: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        argv.push(flag.to_owned());
        argv.push(value.to_owned());
    }
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
    usage: Option<UsageSource>,
    model_hint: Option<&str>,
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

    let waited = wait_bounded(&mut child, request.deadline_ms);

    // Harvest before branching on the outcome. Both failure paths used to return
    // above this point, which is how a 15-minute review that finished and then
    // blew its deadline was recorded as producing nothing at all — the tokens
    // were spent, the answer was on disk, and hex threw both away.
    // Stamped here rather than in each adapter: `command[0]` is literally the
    // program being spawned, so the recorded session owner cannot drift from the
    // process that created the session.
    let report = usage.map(|u| {
        read_report(u, &request.attempt_dir, model_hint)
            .owned_by(command.first().map(String::as_str))
    });
    let captured = capture_result(capture, &request.attempt_dir, &result_file);
    // A dead attempt has no final message of its own, so the tail of what it did
    // write stands in for one.
    let salvage = |why: &str| -> Option<String> {
        captured
            .clone()
            .ok()
            .flatten()
            .or_else(|| partial_from_logs(&request.attempt_dir, why))
    };

    let status = match waited {
        Ok(Some(status)) => status,
        Ok(None) => {
            return WorkOutcome::timed_out("attempt exceeded its time budget (killed)")
                .with_result(salvage("the attempt exceeded its time budget"))
                .with_report(report);
        }
        Err(e) => return WorkOutcome::error(format!("wait failed: {e}")).with_report(report),
    };

    // A nonzero exit is an infrastructure/agent failure, not a routing proposal.
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string());
        return WorkOutcome::error(format!("agent exited nonzero (exit {code})"))
            .with_result(salvage(&format!("the agent exited nonzero (exit {code})")))
            .with_report(report);
    }

    // On a *clean* exit a capture error is real evidence — a JSON-mode agent
    // reporting `is_error`, or output that is not the shape the mode promises —
    // so unlike the paths above it fails the attempt rather than being salvaged.
    let result = match captured {
        Ok(result) => result,
        Err(reason) => return WorkOutcome::error(reason).with_report(report),
    };

    // No emit on a clean exit is *implicit completion* (signal `None`); the
    // runtime synthesizes `done`. A disallowed/ambiguous emit is an error.
    match read_signal(&emit_file, &request.may_propose) {
        Ok(signal) => WorkOutcome {
            signal,
            result,
            error: None,
            timed_out: false,
            report: report.filter(|r| !r.is_empty()),
        },
        Err(reason) => WorkOutcome::error(reason)
            .with_result(result)
            .with_report(report),
    }
}

/// Read what the agent reported about the attempt from its structured output.
///
/// Never fails: usage is an observation, not evidence. If the stream is missing,
/// truncated (the ordinary case for a killed attempt) or unparseable, we report
/// whatever survived and stay silent about the rest — a broken bill must not
/// fail an attempt that otherwise succeeded.
fn read_report(source: UsageSource, attempt_dir: &Path, model_hint: Option<&str>) -> AttemptReport {
    let stdout = attempt_dir.join("stdout.log");
    match source {
        UsageSource::CodexJsonl => codex_report(&stdout, model_hint),
        UsageSource::ClaudeJson => claude_report(&stdout),
    }
}

/// Parse `codex exec --json`'s JSONL.
///
/// Line-by-line rather than whole-file, because a killed attempt leaves a
/// half-written final line and the earlier lines are still perfectly good facts.
fn codex_report(path: &Path, model_hint: Option<&str>) -> AttemptReport {
    let Ok(file) = File::open(path) else {
        return AttemptReport::default();
    };
    let mut report = AttemptReport::default();
    let mut usage = ModelUsage {
        model: model_hint.unwrap_or("codex").to_owned(),
        ..ModelUsage::default()
    };
    let mut saw_usage = false;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    let cap = usize::try_from(MAX_STREAM_BYTES).unwrap_or(usize::MAX);
    loop {
        line.clear();
        match read_line_bounded(&mut reader, &mut line, cap) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
            continue; // a torn tail line, or prose on a non-`--json` run
        };
        match v.get("type").and_then(serde_json::Value::as_str) {
            Some("thread.started") => {
                report.session_id = v
                    .get("thread_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
            }
            // Summed across turns: a resumed session reports one per turn.
            Some("turn.completed") => {
                if let Some(u) = v.get("usage") {
                    saw_usage = true;
                    let n = |key: &str| u.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
                    // codex follows the OpenAI convention where `input_tokens` is
                    // the whole prompt and `cached_input_tokens` is the part of it
                    // that hit cache. Adding both would count the cached tokens
                    // twice, so the fresh share is the difference.
                    // Saturating: these come straight off an agent's stream, and a
                    // garbled turn must not panic the attempt that produced it.
                    let cached = n("cached_input_tokens");
                    usage.input_tokens = usage
                        .input_tokens
                        .saturating_add(n("input_tokens").saturating_sub(cached));
                    usage.cache_read_tokens = usage.cache_read_tokens.saturating_add(cached);
                    usage.cache_write_tokens = usage
                        .cache_write_tokens
                        .saturating_add(n("cache_write_input_tokens"));
                    usage.output_tokens = usage.output_tokens.saturating_add(n("output_tokens"));
                    // Also a subset of `output_tokens`, so it is carried for
                    // information and never added into the total.
                    usage.reasoning_tokens = usage
                        .reasoning_tokens
                        .saturating_add(n("reasoning_output_tokens"));
                }
            }
            _ => {}
        }
    }
    if saw_usage {
        report.models.push(usage);
    }
    report
}

/// Parse `claude -p --output-format json`'s single object.
fn claude_report(path: &Path) -> AttemptReport {
    let Ok(text) = read_capped(path, MAX_STREAM_BYTES) else {
        return AttemptReport::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return AttemptReport::default();
    };
    let mut report = AttemptReport {
        session_id: v
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        cost_micro_usd: v
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .and_then(micro_usd),
        duration_ms: v.get("duration_ms").and_then(serde_json::Value::as_u64),
        models: Vec::new(),
        // Stamped by the shared plumbing, which knows the program it spawned.
        agent: None,
    };
    // `modelUsage` is keyed by model name and is the only place the per-model
    // split exists; claude's flat `usage` block aggregates them.
    if let Some(models) = v.get("modelUsage").and_then(serde_json::Value::as_object) {
        for (model, u) in models {
            let n = |key: &str| u.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
            report.models.push(ModelUsage {
                model: model.clone(),
                // Anthropic reports cache tokens *beside* `inputTokens` rather
                // than inside it, so unlike codex nothing is subtracted here.
                input_tokens: n("inputTokens"),
                output_tokens: n("outputTokens"),
                cache_read_tokens: n("cacheReadInputTokens"),
                cache_write_tokens: n("cacheCreationInputTokens"),
                reasoning_tokens: 0,
                cost_micro_usd: u
                    .get("costUSD")
                    .and_then(serde_json::Value::as_f64)
                    .and_then(micro_usd),
            });
        }
    }
    report
}

/// Dollars as an agent reported them, converted to exact micro-USD. `None` for a
/// value that is not a sane amount of money, so a garbled field reads as "not
/// reported" instead of an absurd bill.
fn micro_usd(dollars: f64) -> Option<u64> {
    // No attempt costs a million dollars. An `as` cast saturates rather than
    // wrapping, so an absurd `1e300` would otherwise land as `u64::MAX` and read
    // as a real (enormous) bill; rejecting it says "not reported" instead, which
    // is the honest answer for a value the agent cannot have meant.
    const MAX_PLAUSIBLE_USD: f64 = 1_000_000.0;
    if !dollars.is_finite() || !(0.0..=MAX_PLAUSIBLE_USD).contains(&dollars) {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "guarded above: finite and non-negative, and rounded before the cast"
    )]
    Some((dollars * 1_000_000.0).round() as u64)
}

/// The tail of what the agent actually wrote, as a stand-in result for an
/// attempt that died before producing one.
///
/// The *tail* specifically: an agent's newest output is its most informative,
/// and the head of a 1.8 MB reasoning log tells you nothing. Both streams are
/// considered because codex writes everything to stderr and nothing to stdout,
/// while claude does the reverse.
fn partial_from_logs(attempt_dir: &Path, why: &str) -> Option<String> {
    let mut parts = Vec::new();
    for name in ["stdout.log", "stderr.log"] {
        if let Some(text) = read_tail(&attempt_dir.join(name), MAX_PARTIAL_BYTES) {
            let text = text.trim();
            if !text.is_empty() {
                parts.push(format!("--- {name} (tail) ---\n{text}"));
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "[partial output — {why}, so the agent never reported a final message]\n\n{}",
        parts.join("\n\n")
    ))
}

/// The last `max` bytes of `path`, decoded lossily (the cut can land mid-
/// codepoint). `None` when the file is missing or empty.
fn read_tail(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > max {
        file.seek(SeekFrom::Start(len - max)).ok()?;
    }
    let mut buf = Vec::new();
    Read::read_to_end(&mut Read::take(file, max), &mut buf).ok()?;
    (!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned())
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
    // Its own process group, so a deadline can take down the whole tree the
    // agent spawned (see `wait_bounded`) rather than only the process we hold a
    // handle to. Agents shell out constantly — a build, a test run, a dev server
    // — and those are exactly what survived a kill before this.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    Ok(cmd)
}

/// How long a killed group gets to exit on `SIGTERM` before `SIGKILL`. Short:
/// the attempt has already blown its budget, and the agent's own shutdown is
/// not something we are waiting to be graceful.
const GROUP_TERM_GRACE: Duration = Duration::from_secs(2);

/// Terminate the whole process group `child` leads, politely then not.
///
/// `child.kill()` only signals the direct child, so an agent's grandchildren
/// (a `cargo test`, a server it started) were reparented to pid 1 and kept
/// running after every timeout. `logged_command` puts each attempt in its own
/// group precisely so this can address all of it.
#[cfg(unix)]
fn kill_group(child: &mut Child) -> std::io::Result<()> {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    // The group id equals the leader's pid, because `process_group(0)` made the
    // child its own leader.
    let pgid = Pid::from_raw(i32::try_from(child.id()).unwrap_or(i32::MAX));
    // Only `ESRCH` — "no such group" — is success here: it means everything is
    // already gone. Anything else (`EPERM`, say) means processes may still be
    // running, and swallowing it would report a clean kill while the agent's
    // children carried on, which is exactly the bug this function exists to fix.
    let tolerate_gone = |r: nix::Result<()>| match r {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(e) => Err(std::io::Error::other(format!(
            "cannot signal the attempt's process group: {e}"
        ))),
    };
    tolerate_gone(killpg(pgid, Signal::SIGTERM))?;
    let deadline = Instant::now() + GROUP_TERM_GRACE;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            // The leader is gone; sweep any group member that ignored SIGTERM.
            return tolerate_gone(killpg(pgid, Signal::SIGKILL));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    tolerate_gone(killpg(pgid, Signal::SIGKILL))?;
    child.kill()?;
    child.wait()?;
    Ok(())
}

/// Without process groups, the best available is the direct child.
#[cfg(not(unix))]
fn kill_group(child: &mut Child) -> std::io::Result<()> {
    child.kill()?;
    child.wait()?;
    Ok(())
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
            // Verify termination rather than assuming kill succeeded — and take
            // the agent's whole process group, not just the process we hold.
            kill_group(child)?;
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

    /// A per-call unique suffix for temp dirs. PIDs are recycled, so a name keyed
    /// on the PID alone can collide with a *previous* test run's leftovers and
    /// read stale files (these tests assert exact log contents).
    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            .wrapping_add(N.fetch_add(1, Ordering::Relaxed))
    }
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hex-agent-test-{tag}-{}-{}",
            std::process::id(),
            unique()
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// Captured from a live CLI run, not hand-written: a parser invented against
    /// a guessed shape passes its tests while disagreeing with the agent. The
    /// first draft of this one looked for a `token_count` event codex does not
    /// emit.
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn codex_report_reads_the_session_id_and_token_split() {
        let r = codex_report(&fixture("codex-exec-json.jsonl"), Some("gpt-5.6"));
        assert_eq!(
            r.session_id.as_deref(),
            Some("019fb9a2-d8cf-7a12-bd5e-92d060051f64")
        );
        let m = &r.models[0];
        assert_eq!(
            m.model, "gpt-5.6",
            "codex names no model; the hint labels it"
        );
        // The fixture reports input_tokens 17259 with 11008 of them cached, so the
        // fresh share is the difference — counting both would bill 28267.
        assert_eq!(m.input_tokens, 6_251);
        assert_eq!(m.cache_read_tokens, 11_008);
        assert_eq!(m.output_tokens, 5);
        assert_eq!(m.tokens(), 17_264);
        assert_eq!(m.cost_micro_usd, None, "codex reports no money");
    }

    #[test]
    fn codex_report_falls_back_to_the_worker_name_without_a_configured_model() {
        let r = codex_report(&fixture("codex-exec-json.jsonl"), None);
        assert_eq!(r.models[0].model, "codex");
    }

    /// A tool-calling run: still exactly one `turn.completed`, which is why the
    /// parser sums turns instead of taking a cumulative last value.
    #[test]
    fn codex_report_handles_a_run_with_tool_calls() {
        let r = codex_report(&fixture("codex-exec-json-tools.jsonl"), None);
        assert_eq!(r.models[0].output_tokens, 119);
        assert!(r.session_id.is_some());
    }

    /// The ordinary shape of a killed attempt: the stream stops mid-line. The
    /// facts already written are still facts.
    #[test]
    fn a_torn_jsonl_tail_still_yields_the_earlier_facts() {
        let dir = temp_dir("torn");
        let whole = fs::read_to_string(fixture("codex-exec-json.jsonl")).expect("fixture");
        let torn = &whole[..whole.len() - 40];
        let path = dir.join("stdout.log");
        fs::write(&path, torn).expect("write");
        let r = codex_report(&path, None);
        assert!(
            r.session_id.is_some(),
            "the session id was on the first line"
        );
        assert!(
            r.models.is_empty(),
            "the truncated usage line is not invented"
        );
    }

    #[test]
    fn claude_report_reads_per_model_usage_and_exact_cost() {
        let r = claude_report(&fixture("claude-p-json.json"));
        assert_eq!(
            r.session_id.as_deref(),
            Some("cf95788d-de0e-4c3e-9eeb-157782473e7a")
        );
        // $0.1778 exactly, with no float left in the recorded fact.
        assert_eq!(r.cost_micro_usd, Some(177_800));
        assert_eq!(r.duration_ms, Some(2611));
        let m = &r.models[0];
        assert_eq!(m.model, "claude-opus-5");
        // Anthropic reports cache tokens beside `inputTokens`, not inside it, so
        // nothing is subtracted here (unlike codex).
        assert_eq!(m.input_tokens, 2);
        assert_eq!(m.output_tokens, 4);
        assert_eq!(m.cache_write_tokens, 17_769);
        assert_eq!(m.cost_micro_usd, Some(177_800));
    }

    #[test]
    fn a_garbled_cost_reads_as_not_reported_rather_than_an_absurd_bill() {
        assert_eq!(micro_usd(0.1778), Some(177_800));
        assert_eq!(micro_usd(-1.0), None);
        assert_eq!(micro_usd(f64::NAN), None);
        assert_eq!(micro_usd(f64::INFINITY), None);
        // An `as` cast *saturates*, so without an upper bound this would land as
        // `u64::MAX` and read as a real, enormous bill instead of "not reported".
        assert_eq!(micro_usd(1e300), None);
        assert_eq!(micro_usd(2_000_000.0), None);
    }

    #[test]
    fn usage_from_a_missing_stream_is_silence_not_a_failure() {
        let dir = temp_dir("no-stream");
        assert!(codex_report(&dir.join("absent.log"), None).is_empty());
        assert!(claude_report(&dir.join("absent.log")).is_empty());
    }

    /// The head of a 1.8 MB reasoning log says nothing; the end is the review.
    #[test]
    fn a_partial_result_keeps_the_tail_and_says_it_is_partial() {
        let dir = temp_dir("partial");
        let filler = "x".repeat(usize::try_from(MAX_PARTIAL_BYTES).expect("fits") * 2);
        fs::write(dir.join("stderr.log"), format!("{filler}\nTHE VERDICT")).expect("write");
        fs::write(dir.join("stdout.log"), "").expect("write");
        let partial = partial_from_logs(&dir, "the attempt exceeded its time budget")
            .expect("something was written");
        assert!(partial.starts_with("[partial output —"), "{partial}");
        assert!(partial.contains("THE VERDICT"), "kept the tail");
        assert!(
            !partial.contains("stdout.log"),
            "an empty stream is not reported: {partial}"
        );
        assert!(
            partial.len() < usize::try_from(MAX_PARTIAL_BYTES).expect("fits") + 200,
            "bounded"
        );
    }

    #[test]
    fn nothing_written_yields_no_partial() {
        let dir = temp_dir("empty-partial");
        assert!(partial_from_logs(&dir, "whatever").is_none());
    }

    /// A timeout used to leave the agent's children running: `child.kill()`
    /// signals only the direct child, so a `cargo test` or dev server it spawned
    /// was reparented to pid 1 and kept going. Every timeout leaked.
    #[cfg(unix)]
    #[test]
    fn a_timed_out_attempt_kills_the_whole_process_group() {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        let dir = temp_dir("group-kill");
        let pidfile = dir.join("grandchild.pid");
        // The grandchild outlives its parent's foreground work on purpose.
        let argv = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
        ];
        let mut cmd = logged_command(&argv, &dir, &dir).expect("prepare");
        let mut child = cmd.spawn().expect("spawn");

        // Let the shell record the grandchild before the deadline fires.
        let grandchild = {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(text) = fs::read_to_string(&pidfile)
                    && let Ok(pid) = text.trim().parse::<i32>()
                {
                    break Pid::from_raw(pid);
                }
                assert!(
                    Instant::now() < deadline,
                    "grandchild never reported its pid"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        assert!(kill(grandchild, None).is_ok(), "grandchild should be alive");

        assert!(
            wait_bounded(&mut child, Some(100)).expect("wait").is_none(),
            "the deadline should have fired"
        );

        // Signal 0 only probes for existence; ESRCH means it is gone.
        let deadline = Instant::now() + Duration::from_secs(5);
        while kill(grandchild, None).is_ok() {
            assert!(
                Instant::now() < deadline,
                "grandchild {grandchild} survived the group kill"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
            extra_writable_dir: None,
            resume_session: None,
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

    /// The failure this whole path exists for: an attempt that did real work and
    /// then blew its deadline used to be recorded as producing nothing, so a
    /// finished review sat unreachable in a 1.8 MB log.
    #[test]
    fn a_timed_out_attempt_keeps_what_the_agent_already_wrote() {
        let dir = temp_dir("partial-timeout");
        let worker = CommandWorker::new(
            "fake",
            strs(&["sh", "-c", "echo THE REVIEW IS DONE >&2; sleep 30"]),
        );
        let mut req = request(&dir, &["ready"]);
        req.deadline_ms = Some(300);
        let outcome = worker.run(&req);
        assert!(outcome.timed_out);
        let result = outcome.result.expect("the partial output is kept");
        assert!(result.contains("THE REVIEW IS DONE"), "{result}");
        assert!(result.starts_with("[partial output —"), "{result}");
    }

    /// Same salvage on the other failure path — a crashed agent's final message
    /// was discarded too.
    #[test]
    fn a_nonzero_exit_keeps_what_the_agent_already_wrote() {
        let dir = temp_dir("partial-crash");
        let worker = CommandWorker::new("fake", strs(&["sh", "-c", "echo PROGRESS; exit 3"]));
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert!(outcome.error.expect("failed").contains("exit 3"));
        let result = outcome.result.expect("the partial output is kept");
        assert!(result.contains("PROGRESS"), "{result}");
    }

    #[test]
    fn codex_argv_shape() {
        let w = CodexWorker::new(Some("gpt-5".to_owned()));
        let argv = w.command(None, None).join(" ");
        assert!(argv.contains("--output-last-message {result}"), "{argv}");
        // `--json` is load-bearing, not cosmetic: it is the only place codex
        // exposes its session id and token counts, so usage accounting and
        // `context: continue` both break silently if it goes missing.
        assert!(argv.contains("--json"), "{argv}");
        assert!(
            argv.contains("--model gpt-5") && argv.ends_with("{prompt}"),
            "{argv}"
        );
        // Always workspace-write (read_only is advisory and must not block the
        // emit/result files); no extra writable dir in shared mode.
        assert!(argv.contains("--sandbox workspace-write"));
        assert!(!argv.contains("--add-dir"));
        // Under isolation the control dir outside the workspace is added writable.
        let iso = w
            .command(Some(std::path::Path::new("/tmp/run")), None)
            .join(" ");
        assert!(iso.contains("--add-dir /tmp/run"), "{iso}");
    }

    /// `codex exec resume` is a subcommand with a smaller flag set than `exec`,
    /// so the sandbox settings have to change shape rather than just tag along.
    #[test]
    fn codex_resume_argv_uses_the_subcommand_and_config_overrides() {
        let w = CodexWorker::new(None);
        let argv = w.command(None, Some("019fb9a2")).join(" ");
        assert!(argv.starts_with("codex exec resume 019fb9a2 "), "{argv}");
        assert!(argv.contains("--json"), "{argv}");
        assert!(argv.contains("--output-last-message {result}"), "{argv}");
        // The flag does not exist on `resume`; passing it would abort the attempt.
        assert!(!argv.contains("--sandbox "), "{argv}");
        assert!(
            argv.contains(r#"-c sandbox_mode="workspace-write""#),
            "{argv}"
        );
        // Same for the worktree case: `--add-dir` is unavailable, so the writable
        // root travels as config or the emit channel breaks under isolation.
        let iso = w
            .command(Some(std::path::Path::new("/tmp/run")), Some("abc"))
            .join(" ");
        assert!(!iso.contains("--add-dir"), "{iso}");
        assert!(
            iso.contains(r#"-c sandbox_workspace_write.writable_roots=["/tmp/run"]"#),
            "{iso}"
        );
    }

    #[test]
    fn claude_resume_argv_uses_a_plain_flag() {
        let argv = ClaudeWorker::new(None).command(Some("sess-1")).join(" ");
        assert!(argv.contains("--resume sess-1"), "{argv}");
        // Unchanged otherwise, unlike codex.
        assert!(argv.starts_with("claude -p {prompt}"), "{argv}");
    }

    #[test]
    fn only_the_resumable_adapters_declare_session_resume() {
        for w in [
            Box::new(CodexWorker::default()) as Box<dyn Worker>,
            Box::new(ClaudeWorker::default()),
        ] {
            assert!(w.capabilities().supports(Capability::SessionResume));
            assert!(w.capabilities().supports(Capability::CostReporting));
        }
        for w in [
            Box::new(OpencodeWorker::default()) as Box<dyn Worker>,
            Box::new(CommandWorker::new("x", strs(&["true"]))),
        ] {
            assert!(!w.capabilities().supports(Capability::SessionResume));
        }
    }

    #[test]
    fn claude_and_opencode_argv_shapes() {
        let c = ClaudeWorker::new(None).command(None).join(" ");
        assert!(c.contains("claude -p {prompt} --output-format json"), "{c}");
        let o = OpencodeWorker::new(None).command().join(" ");
        assert!(
            o.contains("opencode run {prompt} --auto --format json"),
            "{o}"
        );
    }

    #[test]
    fn claude_uses_an_allow_deny_classifier_not_a_blanket_bypass() {
        let c = ClaudeWorker::new(None).command(None).join(" ");
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
