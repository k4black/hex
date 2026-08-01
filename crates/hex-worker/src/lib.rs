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
//! Depends on [`hex_proto`] and [`hex_kernel`], never the reverse.

use std::path::PathBuf;

use hex_proto::{Capability, ModelUsage};

pub mod agent;
pub mod mock;

pub use agent::{
    ClaudeWorker, CodexWorker, CommandWorker, OpencodeWorker, ResultCapture, logged_command,
    wait_bounded,
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
    /// The node prompt (with the operator prompt already interpolated).
    pub prompt: String,
    /// Routing events the agent is allowed to emit (its `may_propose` list).
    pub may_propose: Vec<String>,
    /// Directory the agent operates in (the run's workspace).
    pub workdir: PathBuf,
    /// Per-attempt scratch directory for stdout/stderr and the emit file.
    pub attempt_dir: PathBuf,
    /// Wall-clock deadline for this attempt in milliseconds; the child is
    /// killed if it runs longer. `None` means no per-attempt time bound.
    pub deadline_ms: Option<u64>,
    /// Whether this node should not modify the workspace (e.g. a reviewer).
    /// Advisory only: a hard read-only sandbox would also block the agent from
    /// writing `HEX_EMIT_FILE`/`HEX_RESULT_FILE`, so workers do not enforce it —
    /// read-only intent is conveyed through the node's prompt. See `agent.rs`.
    pub read_only: bool,
    /// An extra directory *outside* the workspace the worker must keep writable —
    /// where the control files (`HEX_EMIT_FILE`/`HEX_RESULT_FILE`) live when they
    /// sit outside the run's cwd. `None` when they're already under the workspace.
    /// Only path-sandboxed workers (codex) act on it. This is a stopgap for the
    /// absent non-workspace control transport (a socket/MCP hook would retire it).
    pub extra_writable_dir: Option<PathBuf>,
    /// The agent session to continue, for a node declaring `context: continue`.
    /// `None` runs a fresh session — the default, and always the case on a
    /// node's first visit. Only a worker declaring
    /// [`hex_proto::Capability::SessionResume`] ever receives one.
    pub resume_session: Option<String>,
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
}

/// What a worker reports after one attempt. On success `signal` is the routing
/// event (or `None` for implicit completion — a clean finish with no emit); on
/// failure `error` is set. `result` is the captured final message (independent
/// of routing), fed to a downstream node as `{{node.result}}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkOutcome {
    /// The routing event the agent emitted (∈ `may_propose`), or `None` when the
    /// agent finished cleanly without emitting (the runtime synthesizes `done`).
    pub signal: Option<String>,
    /// The captured final message text, if the worker declares result capture.
    pub result: Option<String>,
    /// Execution failure reason (the agent crashed or emitted something invalid).
    pub error: Option<String>,
    /// Whether the failure was specifically a per-attempt timeout, so the
    /// runtime can record the `TimedOut` disposition rather than plain `Failed`.
    pub timed_out: bool,
    /// What the agent reported about this attempt — present on **every**
    /// outcome, including a timeout. An attempt that spent tokens and then died
    /// is precisely the one whose cost you need to see.
    pub report: Option<AttemptReport>,
}

impl WorkOutcome {
    /// A successful outcome carrying a routing signal.
    #[must_use]
    pub fn signal(name: impl Into<String>) -> Self {
        Self {
            signal: Some(name.into()),
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
    /// The capability manifest this worker advertises to the graph validator.
    fn capabilities(&self) -> CapabilityManifest;

    /// The executable this worker spawns, for preflight (`hex doctor`) — so a
    /// missing or unauthenticated agent CLI is reported *before* a run is
    /// created, instead of failing the first attempt. `None` for a worker that
    /// spawns nothing (the mock).
    fn program(&self) -> Option<&str> {
        None
    }

    /// Run one attempt to completion and report the outcome. Effectful (this
    /// is the adapter layer); the runtime journals around it.
    fn run(&self, request: &WorkRequest) -> WorkOutcome;
}

/// The set of [`Capability`] entries one worker advertises. The graph
/// validator rejects a graph whose nodes demand capabilities the assigned
/// worker does not declare.
#[derive(Debug, Clone, Default)]
pub struct CapabilityManifest {
    /// Capabilities this worker supports.
    pub capabilities: Vec<Capability>,
}

impl CapabilityManifest {
    /// Build a manifest from a list of capabilities.
    #[must_use]
    pub fn from(capabilities: &[Capability]) -> Self {
        Self {
            capabilities: capabilities.to_vec(),
        }
    }

    /// Whether the manifest declares `capability`.
    #[must_use]
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}
