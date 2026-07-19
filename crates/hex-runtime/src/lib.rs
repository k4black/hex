//! `hex-runtime` — the imperative shell around the pure kernel.
//!
//! Everything effectful lives here: the drive loop, effect execution, the
//! append-only JSONL journal writer (single writer, fsync, torn-tail
//! tolerance), control ingestion, workspace isolation (default `shared`,
//! opt-in per-run `worktree`, never auto-merged), and run supervision with
//! crash recovery. The kernel decides *what*; the runtime is the only layer
//! that *does*.
//!
//! The drive loop, per iteration:
//!
//! 1. **replay** — fold the journal through [`hex_kernel::reduce`] to the
//!    current [`hex_kernel::RunState`] projection;
//! 2. **schedule** — ask [`hex_kernel::schedule`] for the next
//!    [`hex_kernel::Effect`] intents;
//! 3. **intent-before-effect** — persist each intent with an idempotency key
//!    *before* performing it, so a crash never silently reruns a
//!    side-effecting attempt (orphans are marked `interrupted`; redo is a new
//!    run);
//! 4. **execute** — perform the effect via a [`hex_worker::Worker`] adapter,
//!    a command/gate executor, or a human request;
//! 5. **append** — record the resulting [`Event`]s to the journal;
//! 6. **reduce** — fold them in and go again.
//!
//! Clients never touch [`Runtime`] internals: the CLI, MCP, and dashboard are
//! thin peers over the [`RuntimeClient`] trait — [`InProcess`] now, a `Remote`
//! client (per-run background controller) later behind the same trait.
//!
//! Status: scaffold.

pub use hex_proto::{Command, Event, PROTOCOL_VERSION};

use hex_kernel::graph::Graph;
use hex_kernel::{Effect, RunState, reduce, schedule};
use hex_worker::Worker;

/// Owns one run end to end: the journal, the projected state, and the worker
/// adapters the drive loop executes effects through.
///
/// (placeholder — journal is an in-memory `Vec` until the JSONL writer lands)
#[derive(Default)]
pub struct Runtime {
    graph: Graph,
    state: RunState,
    journal: Vec<Event>,
    workers: Vec<Box<dyn Worker>>,
}

impl Runtime {
    /// Create a runtime for one run over a compiled graph snapshot.
    #[must_use]
    pub fn new(graph: Graph) -> Self {
        Self {
            graph,
            ..Self::default()
        }
    }

    /// Register a worker adapter effects may be executed through.
    ///
    /// (placeholder — capability validation against the graph lands here)
    pub fn register_worker(&mut self, worker: Box<dyn Worker>) {
        self.workers.push(worker);
    }

    /// One drive-loop iteration: derive the next effect intents from the
    /// kernel. Executing them (intent-before-effect, idempotency keys) is the
    /// runtime's job and is not implemented yet.
    ///
    /// (placeholder — returns the kernel's intents untouched)
    pub fn tick(&mut self) -> Vec<Effect> {
        schedule(&self.graph, &self.state)
    }

    /// Append one event to the journal and fold it into the projection.
    ///
    /// (placeholder — the real writer assigns seq, fsyncs, and is the single
    /// writer for the run)
    pub fn append(&mut self, event: Event) {
        self.state = reduce(std::mem::take(&mut self.state), &event);
        self.journal.push(event);
    }

    /// Number of events journaled so far.
    #[must_use]
    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }
}

/// The one control surface every client speaks. The CLI, MCP transport, and
/// dashboard are all thin clients over this trait — never parallel
/// implementations. Authority is scoped per actor, not per surface.
///
/// (placeholder — subscribe/watch/query methods land here)
pub trait RuntimeClient {
    /// Submit one operator [`Command`] over the shared control protocol.
    fn submit(&mut self, command: Command);
}

/// In-process client: drives a [`Runtime`] in the foreground of the calling
/// process. A `Remote` client (run-local socket + scoped token) arrives later
/// behind the same trait.
///
/// (placeholder — commands are parked, not yet dispatched)
#[derive(Default)]
pub struct InProcess {
    runtime: Runtime,
    pending: Vec<Command>,
}

impl InProcess {
    /// Create a client owning a runtime for one run.
    #[must_use]
    pub fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            pending: Vec::new(),
        }
    }

    /// Commands accepted but not yet dispatched.
    #[must_use]
    pub fn pending(&self) -> &[Command] {
        &self.pending
    }

    /// The owned runtime (placeholder accessor for the drive loop).
    pub fn runtime_mut(&mut self) -> &mut Runtime {
        &mut self.runtime
    }
}

impl RuntimeClient for InProcess {
    fn submit(&mut self, command: Command) {
        self.pending.push(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_worker::mock::MockWorker;

    #[test]
    fn empty_run_ticks_no_effects() {
        let mut runtime = Runtime::new(Graph::default());
        runtime.register_worker(Box::new(MockWorker));
        assert!(runtime.tick().is_empty());
    }

    #[test]
    fn append_folds_into_projection() {
        let mut runtime = Runtime::default();
        runtime.append(Event {
            schema_version: PROTOCOL_VERSION,
            seq: 1,
            run_id: "run_0".to_owned(),
            kind: "run.created".to_owned(),
        });
        assert_eq!(runtime.journal_len(), 1);
    }

    #[test]
    fn in_process_client_accepts_commands() {
        let mut client = InProcess::default();
        client.submit(Command::Status);
        assert_eq!(client.pending(), [Command::Status]);
    }
}
