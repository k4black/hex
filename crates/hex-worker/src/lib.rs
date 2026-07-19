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

use hex_proto::Capability;

pub mod agent;
pub mod mock;

pub use agent::{AgentWorker, wait_bounded};
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
    /// The prompt (already input-interpolated).
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
}

/// What a worker reports after one attempt. Exactly one of `signal`/`error`
/// should be set: a routing signal on success, a reason on execution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkOutcome {
    /// The routing event the agent emitted (guaranteed ∈ `may_propose`).
    pub signal: Option<String>,
    /// Execution failure reason (the agent crashed or emitted nothing valid).
    pub error: Option<String>,
}

impl WorkOutcome {
    /// A successful outcome carrying a routing signal.
    #[must_use]
    pub fn signal(name: impl Into<String>) -> Self {
        Self {
            signal: Some(name.into()),
            error: None,
        }
    }

    /// A failed outcome carrying a reason.
    #[must_use]
    pub fn error(reason: impl Into<String>) -> Self {
        Self {
            signal: None,
            error: Some(reason.into()),
        }
    }
}

/// The common contract every worker adapter implements.
pub trait Worker {
    /// The capability manifest this worker advertises to the graph validator.
    fn capabilities(&self) -> CapabilityManifest;

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
