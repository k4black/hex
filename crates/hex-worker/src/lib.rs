//! `hex-worker` — the [`Worker`] trait, capability manifest, and adapters.
//!
//! A Worker is *our adapter*: it wraps one opaque external agent — a
//! coding-agent CLI or a deterministic mock — behind a common,
//! capability-declaring interface. The kernel decides, the runtime
//! orchestrates; a worker only runs **one** agent and reports back. It never
//! coordinates: sub-agents, watchdogs, and fan-out are kernel-routed /
//! runtime-scheduled graph constructs, or the external agent's own internal
//! business — never logic in this crate.
//!
//! Depends on [`hex_proto`] only, never the reverse.

use std::path::PathBuf;

use hex_proto::{Capability, ModelUsage};

pub mod agent;
pub mod interrupt;
pub mod mock;

pub use agent::{
    ClaudeWorker, CodexWorker, CommandWorker, OpencodeWorker, PiWorker, ResultCapture,
    logged_command, wait_bounded,
};
pub use mock::MockWorker;

/// What the runtime hands a worker to run one attempt.
#[derive(Debug, Clone)]
pub struct WorkRequest {
    /// Owning run id (injected into the agent's environment).
    pub run_id: String,
    /// Node being executed.
    pub node_id: String,
    /// This attempt's id.
    pub attempt_id: String,
    /// The node prompt (with the operator prompt already interpolated, and the
    /// generated verdict instruction appended when the node has one).
    pub prompt: String,
    /// Directory the agent operates in (the run's workspace).
    pub workdir: PathBuf,
    /// Per-attempt scratch directory for stdout/stderr and the result file.
    pub attempt_dir: PathBuf,
    /// Wall-clock deadline for this attempt in milliseconds; the child is
    /// killed if it runs longer. `None` means no per-attempt time bound.
    pub deadline_ms: Option<u64>,
    /// Whether this node should not modify the workspace (e.g. a reviewer).
    /// Advisory only: a hard read-only sandbox would also block the agent from
    /// writing `HEX_RESULT_FILE`, so workers do not enforce it — read-only
    /// intent is conveyed through the node's prompt. See `agent.rs`.
    pub read_only: bool,
    /// An extra directory *outside* the workspace the worker must keep writable —
    /// where the result file (`HEX_RESULT_FILE`) lives when it sits outside the
    /// run's cwd. `None` when it is already under the workspace.
    /// Only path-sandboxed workers (codex) act on it. This is a stopgap for the
    /// absent non-workspace control transport (a socket/MCP hook would retire it).
    pub extra_writable_dir: Option<PathBuf>,
    /// The agent session to continue, for a node declaring `context: continue`.
    /// `None` runs a fresh session — the default, and always the case on a
    /// node's first visit. Only a worker declaring
    /// [`hex_proto::Capability::SessionResume`] ever receives one.
    pub resume_session: Option<String>,
    /// The graph (workflow) name this run executes, injected as `HEX_GRAPH` so an
    /// agent's `hex feedback` can record which workflow it was running under.
    pub graph: String,
    /// The main project root (the directory containing `.hex/`), injected as
    /// `HEX_PROJECT_ROOT`. Distinct from `workdir`, which under worktree
    /// isolation is the slot, not the project — `hex feedback` records both.
    pub project_root: PathBuf,
    /// The git branch a worktree-isolated run commits to (`hex/<run-id>`),
    /// injected as `HEX_WORKTREE_BRANCH`. This is where a worktree run's code
    /// actually lives — the `workdir` slot is reclaimable, the branch is not —
    /// so feedback records it as the durable place to debug. `None` for a
    /// shared-workspace run.
    pub worktree_branch: Option<String>,
}

/// What the agent itself reported about an attempt: its session handle and what
/// it spent. Parsed from the agent's own structured output, so hex never
/// estimates — an agent that reports no money yields `cost_micro_usd: None`
/// rather than a guess.
///
/// The runtime journals this as `EventBody::AttemptReported`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttemptReport {
    /// The agent's session id, where it exposes one — the resume handle for a
    /// node declaring `context: continue`.
    pub session_id: Option<String>,
    /// The program that owns `session_id` ([`Worker::program`]) — the identity a
    /// later resume is checked against, because the registry name is a role alias
    /// that survives being rebound to a different agent.
    pub agent: Option<String>,
    /// Per-model usage; a list because one claude attempt bills several models.
    pub models: Vec<ModelUsage>,
    /// Attempt total in micro-USD, when the agent reports money.
    pub cost_micro_usd: Option<u64>,
    /// Wall time the agent reported.
    pub duration_ms: Option<u64>,
}

impl AttemptReport {
    /// Whether the agent reported anything worth journaling. A report with no
    /// session and no usage is silence, and silence should not become an event.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.session_id.is_none()
            && self.models.is_empty()
            && self.cost_micro_usd.is_none()
            && self.duration_ms.is_none()
    }

    /// Stamp the owning program onto a report. Called by the shared plumbing, so
    /// no adapter can forget it and leave a session that cannot be safely resumed.
    #[must_use]
    fn owned_by(mut self, agent: Option<&str>) -> Self {
        self.agent = agent.map(ToOwned::to_owned);
        self
    }
}

/// What a worker reports after one attempt. Deliberately **no** routing signal
/// here: `result` carries the final message *uncapped*, and the runtime — which
/// alone knows the node's allowed outcomes — reads the `VERDICT: <signal>` line
/// out of it and caps it for `{{node.result}}`. Uncapped, or a long review
/// would lose its trailing verdict line. On failure `error` is set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkOutcome {
    /// The captured final message text, **uncapped** — the runtime caps it when
    /// it journals `NodeResult`, after reading the verdict out of it.
    pub result: Option<String>,
    /// Execution failure reason (the agent crashed, exited nonzero, or timed out).
    pub error: Option<String>,
    /// Whether the failure was specifically a per-attempt timeout, so the
    /// runtime can record the `TimedOut` disposition rather than plain `Failed`.
    pub timed_out: bool,
    /// Whether the attempt was killed because the operator interrupted the run
    /// (Ctrl-C). Distinct from `timed_out`: nothing was exceeded and nothing
    /// failed, so the runtime pauses the run rather than recording a failure.
    pub interrupted: bool,
    /// What the agent reported about this attempt — present on **every**
    /// outcome, including a timeout. An attempt that spent tokens and then died
    /// is precisely the one whose cost you need to see.
    pub report: Option<AttemptReport>,
}

impl WorkOutcome {
    /// A successful outcome whose final message ends `VERDICT: <signal>`, so the
    /// mock and tests exercise the runtime's real parse path.
    #[must_use]
    pub fn verdict(signal: &str) -> Self {
        Self {
            result: Some(format!("(mock worker)\n{VERDICT_PREFIX} {signal}")),
            ..Self::default()
        }
    }

    /// An attempt killed by an operator interrupt.
    #[must_use]
    pub fn interrupted() -> Self {
        Self {
            error: Some("attempt interrupted by the operator (killed)".to_owned()),
            interrupted: true,
            ..Self::default()
        }
    }

    /// A failed outcome carrying a reason.
    #[must_use]
    pub fn error(reason: impl Into<String>) -> Self {
        Self {
            error: Some(reason.into()),
            ..Self::default()
        }
    }

    /// A failed outcome caused by exceeding the attempt's time budget.
    #[must_use]
    pub fn timed_out(reason: impl Into<String>) -> Self {
        Self {
            error: Some(reason.into()),
            timed_out: true,
            ..Self::default()
        }
    }

    /// Attach a captured result to this outcome.
    #[must_use]
    pub fn with_result(mut self, result: Option<String>) -> Self {
        self.result = result;
        self
    }

    /// Attach what the agent reported, dropping a report that says nothing.
    #[must_use]
    pub fn with_report(mut self, report: Option<AttemptReport>) -> Self {
        self.report = report.filter(|r| !r.is_empty());
        self
    }
}

/// The common contract every worker adapter implements.
pub trait Worker {
    /// The capabilities this worker advertises to the graph validator. A fresh
    /// session per attempt is the baseline and needs none.
    fn capabilities(&self) -> &'static [Capability] {
        &[]
    }

    /// The executable this worker spawns, for preflight (`hex doctor`) — so a
    /// missing or unauthenticated agent CLI is reported *before* a run is
    /// created, instead of failing the first attempt. `None` for a worker that
    /// spawns nothing (the mock).
    fn program(&self) -> Option<&str> {
        None
    }

    /// An argv that checks this worker's *credentials* without buying a
    /// completion (`codex login status`, `claude auth status`, …). `hex doctor`
    /// runs it: success is exit 0 with no `not_ready` in the output. `None` for
    /// a worker with nothing to authenticate.
    fn auth_probe(&self) -> Option<Vec<String>> {
        None
    }

    /// Whether this worker can produce a final-message `result` at all. A node
    /// with more than one outcome routes on a verdict read out of that message,
    /// so the runtime refuses such a node bound to a worker that captures
    /// nothing — see `check_workers`. The kernel cannot make this check, because
    /// only the runtime may see a worker.
    fn captures_result(&self) -> bool {
        true
    }

    /// Run one attempt to completion and report the outcome. Effectful (this
    /// is the adapter layer); the runtime journals around it.
    fn run(&self, request: &WorkRequest) -> WorkOutcome;
}

/// Prefix of the line that carries a node's routing verdict, e.g.
/// `VERDICT: approved`. The runtime generates the instruction that asks for it
/// and parses it back; a node's graph never spells it.
pub const VERDICT_PREFIX: &str = "VERDICT:";

/// A [`WorkRequest`] for tests: node `node`, prompt `hello`, every directory
/// `dir`, no deadline.
#[cfg(test)]
fn test_request(dir: &std::path::Path, node: &str) -> WorkRequest {
    WorkRequest {
        run_id: "run_0".to_owned(),
        node_id: node.to_owned(),
        attempt_id: "att_1".to_owned(),
        prompt: "hello".to_owned(),
        workdir: dir.to_path_buf(),
        attempt_dir: dir.to_path_buf(),
        deadline_ms: None,
        read_only: false,
        extra_writable_dir: None,
        resume_session: None,
        graph: "t".to_owned(),
        project_root: dir.to_path_buf(),
        worktree_branch: None,
    }
}
