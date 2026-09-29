//! Subprocess agent adapters: one uniform [`Worker`] surface, one adapter per
//! external coding-agent CLI.
//!
//! Every adapter shares the same spawn/log/result plumbing (`run_agent`), but
//! each concrete worker ([`CodexWorker`], [`ClaudeWorker`], [`OpencodeWorker`],
//! [`PiWorker`]) encapsulates *its* agent's specifics — argv, final-message
//! capture, and the read-only flag it maps to. [`CommandWorker`] is the generic
//! escape hatch for a custom argv (and for tests). The runtime only ever sees
//! `dyn Worker`.
//!
//! An agent's only output channel is its terminating message. The runtime
//! injects `HEX_RESULT_FILE`/`{result}` (where a worker may write that message)
//! plus `HEX_RUN_ID`/`HEX_NODE_ID`/`HEX_ATTEMPT_ID`, and reads the node's routing
//! verdict out of the captured text — there is no `hex emit` subprocess and no
//! emit file, so nothing here needs `hex` on the agent's `PATH`.

use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Command as ProcCommand, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use hex_proto::{Capability, ModelUsage};

use crate::{AttemptReport, WorkOutcome, WorkRequest, Worker};

/// The env var (and `{result}` argv token) naming the file a worker writes its
/// final message to, for capture into `{{node.result}}`.
pub const RESULT_FILE_ENV: &str = "HEX_RESULT_FILE";

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
    /// stdout is JSONL whose last `type == "result"` line carries `result` /
    /// `is_error` (claude `--output-format stream-json`).
    JsonlResult,
    /// stdout is JSONL; take the `part.text` of the last `type == "text"` line
    /// (e.g. opencode `run --format json`).
    JsonlLastText,
    /// stdout is JSONL from `pi --mode json`; extract the final assistant
    /// message text and usage from `pi`'s structured event stream.
    PiJsonl,
    /// The bounded *tail* of stdout is the final message. The lowest-common-
    /// denominator mode, for a `kind: command` worker (or a test script) that
    /// simply prints its answer and its `VERDICT:` line last.
    Text,
}

/// Cap on the bounded log tail kept as a *partial* result for an attempt that
/// died before reporting one. Small: it feeds a downstream prompt and a human's
/// terminal, and the newest output is the informative part.
const MAX_PARTIAL_BYTES: u64 = 8 * 1024;

/// What an adapter that can honour `context: continue` and report its spend
/// declares. Everything else (a fresh session per attempt) is the baseline and
/// needs no capability.
const RESUMABLE: &[Capability] = &[Capability::SessionResume, Capability::CostReporting];

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
    /// `pi --mode json` JSONL on stdout: `messageEnd` events carry usage and
    /// cost; `message_update` with `textEnd` carries the final text.
    PiJsonl,
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
    pub fn new(model: Option<String>, effort: Option<String>) -> Self {
        Self { model, effort }
    }

    /// Build the argv template. `read_only` is advisory only: a true read-only
    /// sandbox (`--sandbox read-only`) would also block the agent from writing
    /// `HEX_RESULT_FILE` under the workspace, losing the final message the run
    /// routes on — so we always use `workspace-write` and rely on the node's
    /// prompt to keep a reviewer from editing. Enforced read-only awaits a
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
        // Under worktree isolation the result file lives outside the workspace,
        // so the sandbox must be told that directory is writable, or result
        // capture would be blocked.
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
    fn auth_probe(&self) -> Option<Vec<String>> {
        Some(strs(&["codex", "login", "status"]))
    }
    fn model_probe(&self) -> Option<Vec<String>> {
        self.model
            .as_ref()
            .map(|_| strs(&["codex", "debug", "models"]))
    }
    fn model_verdict(&self, catalog: &str) -> Result<String, String> {
        codex_model_verdict(
            catalog,
            self.model.as_deref().unwrap_or_default(),
            self.effort.as_deref(),
        )
    }
    fn capabilities(&self) -> &'static [Capability] {
        RESUMABLE
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
    pub fn new(model: Option<String>, effort: Option<String>) -> Self {
        Self { model, effort }
    }

    // Read-only is advisory here: the allow/deny classifier below still leaves
    // write-capable `Bash`, which a coding agent needs to build and test, so it
    // cannot be enforced without crippling the agent. The role's prompt keeps it
    // read-only. (Enforced read-only awaits a stronger sandbox.)
    #[must_use]
    pub fn command(&self, resume: Option<&str>) -> Vec<String> {
        // Codex confines effects with an OS sandbox (`workspace-write`); Claude
        // has no equivalent flag here, so we approximate an *auto classifier*
        // instead of bypassing every check:
        //   * `acceptEdits` auto-approves in-workspace edits (no stall on Edit/
        //     Write), while dangerous ops still route through the deny-list.
        //   * `--allowedTools` auto-approves the coding essentials (incl. `Bash`,
        //     which build/test commands need). Tools outside this set (e.g.
        //     `WebFetch`) get no approver in headless mode → fail closed.
        //   * `--disallowedTools` denies the genuinely destructive/exfil commands
        //     (deny rules outrank the mode). This is defense-in-depth, NOT a hard
        //     boundary — prefix matching is bypassable via shell chaining, so real
        //     containment still awaits worktree/OS isolation (Phase 2).
        // This never uses `--dangerously-skip-permissions`.
        let mut argv = strs(&[
            "claude",
            "-p",
            "{prompt}",
            // Streamed, not buffered. `--output-format json` emits one object at
            // the very end, so a claude attempt wrote nothing to its log until it
            // finished and `hex logs --follow` showed "(nothing captured yet)" for
            // ten minutes. `stream-json` emits each message as it happens and its
            // final `type: "result"` line carries exactly the same fields, so
            // result capture and usage accounting are unchanged. `--verbose` is
            // required to use it with `-p`.
            "--output-format",
            "stream-json",
            "--verbose",
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
/// (how an agent builds and tests). Anything outside this set has no approver in
/// `-p` mode, so it fails closed rather than stalling.
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
    fn auth_probe(&self) -> Option<Vec<String>> {
        Some(strs(&["claude", "auth", "status"]))
    }
    fn capabilities(&self) -> &'static [Capability] {
        RESUMABLE
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        run_agent(
            "claude",
            &self.command(request.resume_session.as_deref()),
            Some(ResultCapture::JsonlResult),
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
    fn auth_probe(&self) -> Option<Vec<String>> {
        // Local credential listing only — opencode has no per-provider check.
        Some(strs(&["opencode", "auth", "list"]))
    }
    fn model_probe(&self) -> Option<Vec<String>> {
        self.model.as_ref().map(|_| strs(&["opencode", "models"]))
    }
    fn model_verdict(&self, catalog: &str) -> Result<String, String> {
        let model = self.model.as_deref().unwrap_or_default();
        if catalog.lines().any(|line| line.trim() == model) {
            Ok(model.to_owned())
        } else {
            Err(format!("`{model}` is not listed by `opencode models`"))
        }
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

/// Pi coding agent (`pi -p`).
#[derive(Debug, Clone, Default)]
pub struct PiWorker {
    /// `--model provider/model` override, if any.
    pub model: Option<String>,
    /// Reasoning effort, mapped to pi's `--thinking` scale.
    pub effort: Option<String>,
}

impl PiWorker {
    #[must_use]
    pub fn new(model: Option<String>, effort: Option<String>) -> Self {
        Self { model, effort }
    }

    /// Build the argv template. `session_dir` is where pi stores its session
    /// state so that `context: continue` works across attempts and resumes.
    /// `resume` adds `--continue` to pick up the existing session in that dir.
    #[must_use]
    pub fn command(&self, session_dir: &Path, resume: bool) -> Vec<String> {
        let mut argv = strs(&["pi", "-p", "{prompt}", "--mode", "json", "--session-dir"]);
        argv.push(session_dir.to_string_lossy().into_owned());
        if resume {
            argv.push("--continue".to_owned());
        }
        push_flag(&mut argv, "--model", self.model.as_deref());
        push_flag(&mut argv, "--thinking", self.effort.as_deref());
        argv
    }
}

impl Worker for PiWorker {
    fn program(&self) -> Option<&str> {
        Some("pi")
    }
    fn auth_probe(&self) -> Option<Vec<String>> {
        // Per model on purpose: `pi auth check --model openrouter/x/y` verifies
        // the provider the configured model resolves to, so it also catches the
        // bare-model ambiguity trap: it prints `not_ready`. With no
        // model configured there is no provider to name, so nothing to probe.
        let model = self.model.as_deref()?;
        Some(strs(&["pi", "auth", "check", "--model", model]))
    }
    fn capabilities(&self) -> &'static [Capability] {
        RESUMABLE
    }
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        let session_dir = request
            .project_root
            .join(".hex")
            .join("runs")
            .join(&request.run_id)
            .join("pi-sessions")
            .join(&request.node_id);
        run_agent(
            "pi",
            &self.command(&session_dir, request.resume_session.is_some()),
            Some(ResultCapture::PiJsonl),
            Some(UsageSource::PiJsonl),
            self.model.as_deref(),
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
    /// How to capture the worker's final message, if at all.
    pub result_capture: Option<ResultCapture>,
}

impl CommandWorker {
    #[must_use]
    pub fn new(name: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            name: name.into(),
            command,
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
    /// Only when a `result:` mode is configured: with none, the worker's output
    /// is logged but no final message is captured, so a multi-outcome node bound
    /// to it could never produce a verdict.
    fn captures_result(&self) -> bool {
        self.result_capture.is_some()
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
/// no `{prompt}`), enforce the deadline, then capture the final message — the
/// runtime reads the routing verdict out of it.
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

    let result_file = request.attempt_dir.join("result.txt");
    // Start clean so a resumed attempt never reads a stale result.
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
        .env("HEX_GRAPH", &request.graph)
        .env("HEX_PROJECT_ROOT", &request.project_root)
        // Empty string when not a worktree run; `hex feedback` treats "" as absent.
        .env(
            "HEX_WORKTREE_BRANCH",
            request.worktree_branch.as_deref().unwrap_or(""),
        )
        // The program actually spawned, so `hex feedback` records the real agent
        // (matches `AttemptReported.agent`).
        .env("HEX_AGENT", &command[0])
        .env(RESULT_FILE_ENV, &result_file)
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
        // `wait_bounded` kills the group for both a blown deadline and an
        // operator interrupt; the flag is what tells them apart. Salvage and the
        // usage report are kept either way — an interrupted attempt spent real
        // tokens, and throwing that away is what "no logs and no notice" means.
        Ok(None) if crate::interrupt::requested() => {
            return WorkOutcome::interrupted()
                .with_result(salvage("the run was interrupted"))
                .with_report(report);
        }
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

    // Routing is deliberately NOT decided here. The runtime knows the node's
    // allowed outcomes — and therefore whether a missing verdict is an error
    // (multi-outcome) or implicit completion (single-outcome) — so it receives
    // the **uncapped** final message and decides. See `WorkOutcome`.
    WorkOutcome::default()
        .with_result(result)
        .with_report(report)
}

/// `.result` from a claude result object, failing closed on `.is_error`.
/// Find `model` in `codex debug models` JSON and, when an effort is set, check
/// it against that model's `supported_reasoning_levels`.
fn codex_model_verdict(catalog: &str, model: &str, effort: Option<&str>) -> Result<String, String> {
    let catalog: serde_json::Value = serde_json::from_str(catalog)
        .map_err(|e| format!("cannot read `codex debug models` output: {e}"))?;
    let entry = catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["slug"] == model)
        .ok_or_else(|| format!("`{model}` is not in codex's model catalog"))?;
    let Some(effort) = effort else {
        return Ok(model.to_owned());
    };
    let levels: Vec<&str> = entry["supported_reasoning_levels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["effort"].as_str())
        .collect();
    if levels.is_empty() || levels.contains(&effort) {
        Ok(format!("{model}, effort {effort}"))
    } else {
        Err(format!(
            "`{model}` does not support effort `{effort}` (supports {})",
            levels.join(", ")
        ))
    }
}

fn result_field(v: &serde_json::Value) -> Result<Option<String>, String> {
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
    Ok(v.get("result")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned))
}

/// The last `type == "result"` line of a claude stream.
///
/// The last one, not the first match: the stream also carries `system`,
/// `assistant` and `rate_limit_event` lines, and only the final result object
/// has the run's totals. A truncated stream (a killed attempt) simply has none.
fn last_result_line(path: &Path) -> Result<Option<serde_json::Value>, String> {
    let text = read_capped(path, MAX_STREAM_BYTES)
        .map_err(|e| format!("cannot read agent output: {e}"))?;
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .rfind(|v| v.get("type").and_then(serde_json::Value::as_str) == Some("result")))
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
        UsageSource::PiJsonl => pi_report(&stdout, model_hint),
    }
}

/// Parse `codex exec --json`'s JSONL.
///
/// Line-by-line rather than whole-file, because a killed attempt leaves a
/// half-written final line and the earlier lines are still perfectly good facts.
fn codex_report(path: &Path, model_hint: Option<&str>) -> AttemptReport {
    let mut report = AttemptReport::default();
    let mut usage = ModelUsage {
        model: model_hint.unwrap_or("codex").to_owned(),
        ..ModelUsage::default()
    };
    let mut saw_usage = false;
    for v in jsonl(path, MAX_STREAM_BYTES).filter_map(Result::ok) {
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
    let Ok(Some(v)) = last_result_line(path) else {
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

/// Parse `pi --mode json`'s JSONL.
///
/// Sums usage across all assistant `messageEnd` events and extracts the model
/// name from the last one. Line-by-line so a killed attempt's torn tail does
/// not lose the facts already written.
fn pi_report(path: &Path, model_hint: Option<&str>) -> AttemptReport {
    let mut usage = ModelUsage {
        model: model_hint.unwrap_or("pi").to_owned(),
        ..ModelUsage::default()
    };
    let mut saw_usage = false;
    let mut total_cost: f64 = 0.0;
    let mut model_name: Option<String> = None;
    for v in jsonl(path, MAX_STREAM_BYTES).filter_map(Result::ok) {
        if v.get("type").and_then(serde_json::Value::as_str) != Some("message_end") {
            continue;
        }
        let Some(role) = v
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if role != "assistant" {
            continue;
        }
        if let Some(model) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(serde_json::Value::as_str)
        {
            model_name = Some(model.to_owned());
        }
        let Some(u) = v.get("message").and_then(|m| m.get("usage")) else {
            continue;
        };
        saw_usage = true;
        let n = |key: &str| u.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
        usage.input_tokens = usage.input_tokens.saturating_add(n("input"));
        usage.output_tokens = usage.output_tokens.saturating_add(n("output"));
        usage.cache_read_tokens = usage.cache_read_tokens.saturating_add(n("cacheRead"));
        usage.cache_write_tokens = usage.cache_write_tokens.saturating_add(n("cacheWrite"));
        usage.reasoning_tokens = usage.reasoning_tokens.saturating_add(n("reasoning"));
        if let Some(cost) = u
            .get("cost")
            .and_then(|c| c.get("total"))
            .and_then(serde_json::Value::as_f64)
        {
            total_cost += cost;
        }
    }
    if !saw_usage {
        return AttemptReport::default();
    }
    usage.model = model_name.unwrap_or_else(|| model_hint.unwrap_or("pi").to_owned());
    let cost_micro_usd = micro_usd(total_cost);
    usage.cost_micro_usd = cost_micro_usd;
    AttemptReport {
        session_id: Some("pi-session".to_owned()),
        agent: None,
        models: vec![usage],
        cost_micro_usd,
        duration_ms: None,
    }
}

/// Extract the final assistant text from a `pi --mode json` JSONL stream.
/// Looks at `messageEnd` events with `role == "assistant"` and joins all
/// `content` items of `type == "text"`. Falls back to `message_update`
/// `textEnd` content if no full `messageEnd` is found (truncated stream).
fn pi_result(path: &Path, max: u64) -> Result<Option<String>, String> {
    let mut last_text: Option<String> = None;
    let mut fallback_text: Option<String> = None;
    for v in jsonl(path, max) {
        let v = v?;
        // Fallback: text_end in a message_update carries the full text.
        if let Some(event) = v.get("assistantMessageEvent")
            && event.get("type").and_then(serde_json::Value::as_str) == Some("text_end")
            && let Some(content) = event.get("content").and_then(serde_json::Value::as_str)
        {
            fallback_text = Some(content.to_owned());
        }
        if v.get("type").and_then(serde_json::Value::as_str) != Some("message_end") {
            continue;
        }
        let Some(role) = v
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if role != "assistant" {
            continue;
        }
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        let texts: Vec<String> = content
            .iter()
            .filter_map(|item| {
                let t = item.get("type")?.as_str()?;
                if t != "text" {
                    return None;
                }
                item.get("text")?.as_str().map(ToOwned::to_owned)
            })
            .collect();
        if !texts.is_empty() {
            last_text = Some(texts.join(""));
        }
    }
    Ok(last_text.or(fallback_text))
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
        Some(ResultCapture::JsonlResult) => {
            let Some(v) = last_result_line(&attempt_dir.join("stdout.log"))? else {
                return Ok(None);
            };
            result_field(&v)?
        }
        Some(ResultCapture::JsonlLastText) => {
            // Stream to EOF (bounded per line) so the *final* text event is never
            // missed by a mid-file cap — a stale earlier text would misrepresent
            // the run. An over-long single line fails closed.
            last_jsonl_text_file(&attempt_dir.join("stdout.log"), MAX_STREAM_BYTES)?
        }
        Some(ResultCapture::PiJsonl) => {
            pi_result(&attempt_dir.join("stdout.log"), MAX_STREAM_BYTES)?
        }
        // The tail, because the answer and its trailing `VERDICT:` line are the
        // newest output.
        Some(ResultCapture::Text) => read_tail(&attempt_dir.join("stdout.log"), MAX_STREAM_BYTES),
    };
    Ok(raw.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()))
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

/// The `part.text` of a JSONL event if it is a `type == "text"` event.
fn text_part(v: &serde_json::Value) -> Option<String> {
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
    let mut last = None;
    for v in jsonl(path, max) {
        if let Some(t) = text_part(&v?) {
            last = Some(t);
        }
    }
    Ok(last)
}

/// The JSON events of a JSONL file, read line by line with at most `max` bytes
/// per read so a single pathological line can't exhaust memory. A line that is
/// not UTF-8 JSON (a killed attempt's torn tail, prose) is skipped. `Err` for an
/// unopenable file or a read error (both end the stream), and for a line that
/// fills `max` without a newline (the stream goes on with its remainder). The
/// usage parsers drop the `Err`s — usage is an observation, not evidence; the
/// result parsers fail closed on them.
fn jsonl(path: &Path, max: u64) -> impl Iterator<Item = Result<serde_json::Value, String>> {
    use std::io::{BufRead as _, Read as _};
    let cap = usize::try_from(max).unwrap_or(usize::MAX);
    let (mut reader, mut failed) = match File::open(path) {
        Ok(f) => (Some(std::io::BufReader::new(f)), None),
        Err(e) => (None, Some(format!("cannot read agent output: {e}"))),
    };
    let mut line = Vec::new();
    std::iter::from_fn(move || {
        loop {
            let Some(r) = reader.as_mut() else {
                return failed.take().map(Err);
            };
            line.clear();
            match r.take(max).read_until(b'\n', &mut line) {
                Ok(0) => reader = None,
                Err(e) => {
                    reader = None;
                    failed = Some(format!("cannot read agent output: {e}"));
                }
                Ok(_) if line.len() >= cap && !line.ends_with(b"\n") => {
                    return Some(Err(format!("agent output line exceeds {max} bytes")));
                }
                Ok(_) => {
                    if let Some(v) = std::str::from_utf8(&line)
                        .ok()
                        .and_then(|t| serde_json::from_str(t.trim()).ok())
                    {
                        return Some(Ok(v));
                    }
                }
            }
        }
    })
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
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        // An operator interrupt kills the attempt exactly like a blown deadline
        // does — the agent is in its own process group, so this is the *only*
        // thing that reaches it (see `crate::interrupt`). Checked before the
        // deadline so an unbounded attempt is still interruptible; this loop is
        // also why `deadline_ms: None` no longer blocks in `child.wait()`.
        if crate::interrupt::requested() {
            kill_group(child)?;
            return Ok(None);
        }
        if let Some(budget) = deadline_ms
            && u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) >= budget
        {
            // Verify termination rather than assuming kill succeeded — and take
            // the agent's whole process group, not just the process we hold.
            kill_group(child)?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    fn a_model_is_judged_against_the_cli_catalog() {
        let codex = fs::read_to_string(fixture("codex-debug-models.json")).expect("fixture");
        let opencode = fs::read_to_string(fixture("opencode-models.txt")).expect("fixture");
        let codex_with = |model: &str, effort: Option<&str>| {
            CodexWorker::new(Some(model.to_owned()), effort.map(ToOwned::to_owned))
                .model_verdict(&codex)
        };
        let opencode_with =
            |model: &str| OpencodeWorker::new(Some(model.to_owned())).model_verdict(&opencode);
        let cases = [
            ("codex known model", codex_with("gpt-6-sol", None), true),
            (
                "codex supported effort",
                codex_with("gpt-6-sol", Some("medium")),
                true,
            ),
            ("codex unknown model", codex_with("gpt-sol-6", None), false),
            (
                "codex unsupported effort",
                codex_with("gpt-6-sol", Some("turbo")),
                false,
            ),
            (
                "opencode listed model",
                opencode_with("opencode/big-pickle"),
                true,
            ),
            (
                "opencode unlisted model",
                opencode_with("big-pickle"),
                false,
            ),
        ];
        for (case, verdict, ok) in cases {
            assert_eq!(verdict.is_ok(), ok, "{case}: {verdict:?}");
        }
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
        assert_eq!(m.cost_micro_usd, None, "codex reports no money");
        let unhinted = codex_report(&fixture("codex-exec-json.jsonl"), None);
        assert_eq!(
            unhinted.models[0].model, "codex",
            "no configured model falls back to the worker name"
        );
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
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

    /// The streamed form: the same fields, on the last `result` line of a JSONL
    /// stream rather than alone in the file. Switching to it is what lets an
    /// operator watch a claude attempt work instead of staring at an empty log.
    #[test]
    fn claude_report_reads_the_result_line_of_a_stream() {
        let r = claude_report(&fixture("claude-stream-json.jsonl"));
        assert!(r.session_id.is_some(), "session id from the result line");
        assert!(r.cost_micro_usd.is_some(), "cost from the result line");
        assert!(!r.models.is_empty(), "per-model usage from the result line");
    }

    /// A stream cut off mid-flight (a killed attempt) has no result line, and
    /// must read as "nothing reported" rather than as an error.
    #[test]
    fn a_truncated_claude_stream_reports_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let whole = fs::read_to_string(fixture("claude-stream-json.jsonl")).expect("fixture");
        let torn: String = whole.lines().take(3).collect::<Vec<_>>().join("\n");
        let path = dir.join("stdout.log");
        fs::write(&path, torn).expect("write");
        assert!(claude_report(&path).is_empty());
        assert_eq!(
            capture_result(Some(ResultCapture::JsonlResult), dir, &path).expect("no error"),
            None
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        assert!(codex_report(&dir.join("absent.log"), None).is_empty());
        assert!(claude_report(&dir.join("absent.log")).is_empty());
    }

    /// The head of a 1.8 MB reasoning log says nothing; the end is the review.
    #[test]
    fn a_partial_result_keeps_the_tail_and_says_it_is_partial() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let filler = "x".repeat(usize::try_from(MAX_PARTIAL_BYTES).expect("fits") * 2);
        fs::write(dir.join("stderr.log"), format!("{filler}\nTHE VERDICT")).expect("write");
        fs::write(dir.join("stdout.log"), "").expect("write");
        let partial = partial_from_logs(dir, "the attempt exceeded its time budget")
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        assert!(partial_from_logs(dir, "whatever").is_none());
    }

    /// A timeout used to leave the agent's children running: `child.kill()`
    /// signals only the direct child, so a `cargo test` or dev server it spawned
    /// was reparented to pid 1 and kept going. Every timeout leaked.
    #[cfg(unix)]
    #[test]
    fn a_timed_out_attempt_kills_the_whole_process_group() {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let pidfile = dir.join("grandchild.pid");
        // The grandchild outlives its parent's foreground work on purpose.
        let argv = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
        ];
        let mut cmd = logged_command(&argv, dir, dir).expect("prepare");
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

    fn request(dir: &std::path::Path) -> WorkRequest {
        crate::test_request(dir, "implement")
    }

    /// The worker's job is to hand the runtime the agent's final message; it is
    /// the runtime that reads a verdict out of it. So whichever capture mode a
    /// worker uses, a message it wrote — verdict line included — is the result.
    #[test]
    fn the_final_message_is_captured_per_mode() {
        let cases = [
            (
                "text: a verdict printed last",
                ResultCapture::Text,
                "echo 'the answer'; echo 'VERDICT: ready'",
                "the answer\nVERDICT: ready",
            ),
            (
                "file: a plain result",
                ResultCapture::File,
                "printf 'the plan' > \"$HEX_RESULT_FILE\"",
                "the plan",
            ),
            (
                "file: a result and a verdict together",
                ResultCapture::File,
                "printf 'summary\\nVERDICT: approved' > \"$HEX_RESULT_FILE\"",
                "summary\nVERDICT: approved",
            ),
        ];
        for (name, mode, script, want) in cases {
            let tmp = tempfile::tempdir().expect("tempdir");
            let outcome = CommandWorker::new("fake", strs(&["sh", "-c", script]))
                .with_result_capture(Some(mode))
                .run(&request(tmp.path()));
            assert_eq!(outcome.error, None, "{name}");
            assert_eq!(outcome.result.as_deref(), Some(want), "{name}");
        }
    }

    #[test]
    fn a_clean_exit_with_no_message_reports_no_result() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        // A clean exit with no capture is not an error — the runtime decides
        // whether a missing verdict is implicit completion or a failed attempt.
        let worker = CommandWorker::new("fake", strs(&["true"]));
        let outcome = worker.run(&request(dir));
        assert_eq!(outcome.result, None);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn injects_graph_agent_and_project_root_into_the_child_env() {
        // `hex feedback` reads these from the environment to record which
        // workflow/agent/project a run was under; they must reach the child.
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let worker = CommandWorker::new(
            "fake",
            strs(&[
                "sh",
                "-c",
                "printf '%s|%s|%s' \"$HEX_GRAPH\" \"$HEX_AGENT\" \"$HEX_PROJECT_ROOT\" > \"$HEX_RESULT_FILE\"",
            ]),
        )
        .with_result_capture(Some(ResultCapture::File));
        let outcome = worker.run(&request(dir));
        let got = outcome.result.expect("captured env");
        let parts: Vec<&str> = got.split('|').collect();
        assert_eq!(parts[0], "t", "HEX_GRAPH is the graph name");
        assert_eq!(
            parts[1], "sh",
            "HEX_AGENT is the spawned program (command[0])"
        );
        assert_eq!(
            parts[2],
            dir.to_string_lossy(),
            "HEX_PROJECT_ROOT is the request's project_root"
        );
    }

    /// The failure this whole path exists for: an attempt that did real work and
    /// then blew its deadline used to be recorded as producing nothing, so a
    /// finished review sat unreachable in a 1.8 MB log.
    #[test]
    fn a_timed_out_attempt_keeps_what_the_agent_already_wrote() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let worker = CommandWorker::new(
            "fake",
            strs(&["sh", "-c", "echo THE REVIEW IS DONE >&2; sleep 30"]),
        );
        let mut req = request(dir);
        req.deadline_ms = Some(300);
        let start = Instant::now();
        let outcome = worker.run(&req);
        assert!(start.elapsed().as_secs() < 5, "must not wait for the child");
        assert!(outcome.timed_out);
        assert!(
            outcome.error.expect("failed").contains("time budget"),
            "the error names the time budget"
        );
        let result = outcome.result.expect("the partial output is kept");
        assert!(result.contains("THE REVIEW IS DONE"), "{result}");
        assert!(result.starts_with("[partial output —"), "{result}");
    }

    /// Same salvage on the other failure path — a crashed agent's final message
    /// was discarded too.
    #[test]
    fn a_nonzero_exit_keeps_what_the_agent_already_wrote() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let worker = CommandWorker::new("fake", strs(&["sh", "-c", "echo PROGRESS; exit 3"]));
        let outcome = worker.run(&request(dir));
        assert!(outcome.error.expect("failed").contains("exit 3"));
        let result = outcome.result.expect("the partial output is kept");
        assert!(result.contains("PROGRESS"), "{result}");
    }

    #[test]
    fn codex_argv_shape() {
        let w = CodexWorker::new(Some("gpt-5".to_owned()), None);
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
        let w = CodexWorker::new(None, None);
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
        // root travels as config or the run dir's result file is unwritable
        // under isolation.
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
        let argv = ClaudeWorker::new(None, None)
            .command(Some("sess-1"))
            .join(" ");
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
            assert!(w.capabilities().contains(&Capability::SessionResume));
            assert!(w.capabilities().contains(&Capability::CostReporting));
        }
        for w in [
            Box::new(OpencodeWorker::default()) as Box<dyn Worker>,
            Box::new(CommandWorker::new("x", strs(&["true"]))),
        ] {
            assert!(!w.capabilities().contains(&Capability::SessionResume));
        }
    }

    #[test]
    fn claude_and_opencode_argv_shapes() {
        let c = ClaudeWorker::new(None, None).command(None).join(" ");
        assert!(
            c.contains("claude -p {prompt} --output-format stream-json --verbose"),
            "streamed so a live attempt is watchable: {c}"
        );
        let o = OpencodeWorker::new(None).command().join(" ");
        assert!(
            o.contains("opencode run {prompt} --auto --format json"),
            "{o}"
        );
    }

    #[test]
    fn claude_uses_an_allow_deny_classifier_not_a_blanket_bypass() {
        let c = ClaudeWorker::new(None, None).command(None).join(" ");
        // A real classifier — never the blanket bypass the operator rejected.
        assert!(!c.contains("--dangerously-skip-permissions"), "{c}");
        assert!(c.contains("--permission-mode acceptEdits"), "{c}");
        // `Bash` (how the agent builds and tests) must stay approvable.
        assert!(c.contains("--allowedTools"), "{c}");
        assert!(c.contains("Bash"), "{c}");
        // …while the destructive/exfil set is denied.
        assert!(c.contains("--disallowedTools"), "{c}");
        assert!(c.contains("Bash(sudo:*)"), "{c}");
        assert!(c.contains("Bash(git push:*)"), "{c}");
    }

    #[test]
    fn text_part_extracts_only_text_events() {
        let text = serde_json::json!({"type": "text", "part": {"text": "hi"}});
        assert_eq!(text_part(&text).as_deref(), Some("hi"));
        let tool = serde_json::json!({"type": "tool_use", "part": {}});
        assert_eq!(text_part(&tool), None);
    }

    #[test]
    fn capture_result_jsonl_uses_the_full_file_path() {
        // Exercise capture_result end-to-end: the final text lives past a big
        // prefix, so a prefix-only reader would return a stale value.
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let mut stream = String::new();
        stream.push_str("{\"type\":\"text\",\"part\":{\"text\":\"early\"}}\n");
        stream.push_str(&"{\"type\":\"tool_use\",\"part\":{}}\n".repeat(10_000));
        stream.push_str("{\"type\":\"text\",\"part\":{\"text\":\"the real final\"}}\n");
        std::fs::write(dir.join("stdout.log"), &stream).unwrap();
        let got = capture_result(Some(ResultCapture::JsonlLastText), dir, &dir.join("unused"));
        assert_eq!(got.unwrap().as_deref(), Some("the real final"));
    }

    #[test]
    fn jsonl_last_text_file_fails_closed_on_an_over_long_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let path = dir.join("stdout.log");
        // A single line far exceeding the (tiny) cap must error, not silently
        // return an earlier text.
        std::fs::write(&path, format!("{}\n", "x".repeat(200))).unwrap();
        assert!(last_jsonl_text_file(&path, 64).is_err());
    }

    #[test]
    fn read_capped_is_lossy_and_bounded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
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

    /// A real `pi --mode json` run produces JSONL with `messageEnd` carrying
    /// usage/cost and `message_update` `textEnd` carrying the final text.
    #[test]
    fn pi_report_reads_usage_and_cost_from_message_end() {
        let r = pi_report(&fixture("pi-mode-json.jsonl"), Some("config-model"));
        assert_eq!(r.session_id.as_deref(), Some("pi-session"));
        assert!(
            !r.models.is_empty(),
            "at least one assistant message reported usage"
        );
        let m = &r.models[0];
        assert_eq!(
            m.model, "moonshotai/kimi-k2.6",
            "the stream's own model wins over the config hint"
        );
        // The fixture should have positive output tokens and some cost.
        assert!(m.output_tokens > 0, "output tokens reported");
        assert!(m.cost_micro_usd.is_some(), "pi reports cost per message");
    }

    #[test]
    fn pi_result_extracts_final_assistant_text() {
        let text = pi_result(&fixture("pi-mode-json.jsonl"), MAX_STREAM_BYTES)
            .expect("parses")
            .expect("has text");
        assert!(
            text.to_lowercase().contains("hello"),
            "expected greeting in result, got: {text}"
        );
    }

    /// A torn JSONL tail still yields text from earlier message_update `text_end`
    /// events when the final `message_end` is missing.
    #[test]
    fn pi_result_falls_back_to_text_end_when_message_end_is_truncated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let whole = fs::read_to_string(fixture("pi-mode-json.jsonl")).expect("fixture");
        // Cut off after the text_end but before the message_end.
        let torn = &whole[..whole.rfind("text_end").unwrap() + 60];
        let path = dir.join("stdout.log");
        fs::write(&path, torn).expect("write");
        let text = pi_result(&path, MAX_STREAM_BYTES)
            .expect("parses")
            .expect("fallback text");
        assert!(
            text.to_lowercase().contains("hello"),
            "expected greeting in fallback, got: {text}"
        );
    }
}
