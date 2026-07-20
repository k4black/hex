//! `hex-runtime` — the imperative shell around the pure kernel.
//!
//! Everything effectful lives here: the drive loop, effect execution, the
//! append-only JSONL journal, config + preset resolution, workspace layout,
//! and crash recovery. The kernel decides *what*; the runtime is the only
//! layer that *does*.
//!
//! Clients (CLI, MCP, dashboard) are thin peers over the [`RuntimeClient`]
//! trait — [`Runtime`] is the in-process implementation now; a `Remote`
//! client (per-run background controller) arrives later behind the same trait.

pub mod config;
pub mod driver;
pub mod error;
pub mod journal;
pub mod loader;
pub mod preset;
pub mod workers;

pub use error::{HexError, Result};
pub use hex_kernel::graph::NodeKind;
pub use hex_kernel::{Graph, RunState, Status};
pub use preset::Entry as GraphEntry;
pub use hex_proto::{Actor, Command, Disposition, Event, EventBody, PROTOCOL_VERSION};
pub use workers::Workers;

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use config::Config;
use driver::{Session, check_workers, graph_hash};
use hex_kernel::{RunState as State, reduce};
use journal::Journal;

/// The outcome of starting or resuming a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// The run's id.
    pub run_id: String,
    /// Where the graph came from (path or `built-in:<name>`).
    pub origin: String,
    /// The terminal disposition reached.
    pub disposition: Disposition,
}

/// A projected snapshot of a run's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    /// The run's id.
    pub run_id: String,
    /// Lifecycle status.
    pub status: Status,
    /// The active node, if any.
    pub current: Option<String>,
    /// Attempts started so far.
    pub attempts: u32,
    /// Terminal disposition, if finished.
    pub disposition: Option<Disposition>,
}

/// The captured output of one attempt (an agent's or gate's stdout/stderr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptLog {
    /// The attempt id.
    pub attempt_id: String,
    /// The node it ran.
    pub node_id: Option<String>,
    /// The worker that ran it (agent attempts only).
    pub worker: Option<String>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// The in-process runtime: owns config + the worker registry and executes runs
/// under a project root (the directory containing `.hex/`).
/// A callback invoked with each event as it is journaled during a run.
pub type EventObserver = Box<dyn Fn(&Event)>;

pub struct Runtime {
    root: PathBuf,
    config: Config,
    workers: Workers,
    observer: Option<EventObserver>,
}

impl Runtime {
    /// Build a runtime rooted at `root`, loading layered config and building
    /// agent workers from its registry.
    ///
    /// # Errors
    /// Fails if a present config file is malformed.
    pub fn new(root: PathBuf) -> Result<Self> {
        let config = Config::load(&root)?;
        let workers = Workers::from_config(&config);
        Ok(Self {
            root,
            config,
            workers,
            observer: None,
        })
    }

    /// Build a runtime with an explicit worker registry (used by tests to
    /// inject mocks instead of real agent CLIs).
    #[must_use]
    pub fn with_workers(root: PathBuf, config: Config, workers: Workers) -> Self {
        Self {
            root,
            config,
            workers,
            observer: None,
        }
    }

    /// Install a callback invoked with every event as it is journaled during a
    /// `run`/`resume`, so a foreground caller can stream live progress.
    #[must_use]
    pub fn on_event(mut self, observer: EventObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// The `.hex/runs` directory under the project root.
    fn runs_dir(&self) -> PathBuf {
        self.root.join(".hex").join("runs")
    }

    /// The directory for `run_id`, after validating the id is a safe grammar
    /// (no path traversal — operator-supplied ids reach this).
    fn run_dir(&self, run_id: &str) -> Result<PathBuf> {
        validate_run_id(run_id)?;
        Ok(self.runs_dir().join(run_id))
    }

    /// Allocate a collision-resistant run id by exclusively creating its
    /// directory (retrying on the rare clash), so two concurrent starts can
    /// never select the same directory and truncate each other.
    fn new_run(&self) -> Result<(String, PathBuf)> {
        let runs = self.runs_dir();
        std::fs::create_dir_all(&runs)?;
        for _ in 0..1000 {
            let id = format!("run_{}_{}", journal::now_ms(), random_suffix());
            let dir = runs.join(&id);
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok((id, dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(HexError::new("could not allocate a unique run id"))
    }

    /// List every runnable graph (project > user > built-in).
    #[must_use]
    pub fn list_graphs(&self) -> Vec<GraphEntry> {
        preset::list(&self.root)
    }

    /// Compile + validate a graph reference without running it. Structural only:
    /// no prompt is required (any `{{prompt}}` is left unsubstituted).
    ///
    /// # Errors
    /// Fails on resolution, parse, or validation errors.
    pub fn validate(&self, reference: &str) -> Result<Graph> {
        let resolved = preset::resolve(reference, &self.root)?;
        self.compile(&resolved.source, None)
    }

    /// Compile already-resolved YAML `source` to a validated IR, using this
    /// runtime's live config defaults.
    fn compile(&self, source: &str, prompt: Option<&str>) -> Result<Graph> {
        self.compile_with(source, prompt, &self.config.defaults)
    }

    /// Compile with an explicit set of fallback defaults — used on resume so a
    /// run recompiles against the defaults it was *created* with, not whatever
    /// the mutable config happens to say now.
    fn compile_with(
        &self,
        source: &str,
        prompt: Option<&str>,
        defaults: &config::DefaultsSpec,
    ) -> Result<Graph> {
        let graph = loader::load(source, prompt, defaults)?;
        hex_kernel::validate(&graph).map_err(|issues| {
            let joined = issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            HexError::new(format!("graph is invalid:\n{joined}"))
        })?;
        Ok(graph)
    }

    /// Verify already-scanned `events` against their on-disk snapshot and fold
    /// them into state — the single verified path behind `resume`, `status`, and
    /// `cancel`. Checks the run id, the snapshot hash triple, the required
    /// creation hash, the recorded defaults, and the journal lifecycle before
    /// folding. Taking `events` the caller already holds means one journal scan
    /// per operation (`resume`/`cancel` get them from the writer they open).
    fn verify_and_fold(&self, run_id: &str, events: Vec<Event>) -> Result<(Graph, State)> {
        let run_dir = self.run_dir(run_id)?;
        let source = std::fs::read_to_string(run_dir.join("graph.yaml"))
            .map_err(|_| HexError::new(format!("run `{run_id}` not found")))?;

        // The journal's own run id must match the directory/operator id.
        if let Some(first) = events.first()
            && first.run_id != run_id
        {
            return Err(HexError::new("journal run id does not match the run directory"));
        }

        // Snapshot integrity: file hash == recorded sha256 == creation hash.
        let computed = graph_hash(&source);
        let stored = std::fs::read_to_string(run_dir.join("graph.sha256"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if stored != computed {
            return Err(HexError::new(
                "graph.yaml does not match graph.sha256 — snapshot was modified",
            ));
        }
        // The run's creation record carries the hash, prompt, and defaults to
        // verify + recompile against (one pass, not three).
        let (recorded_hash, inputs, defaults) = run_created(&events)
            .ok_or_else(|| HexError::new("journal has no run_created record to verify against"))?;
        if recorded_hash != computed {
            return Err(HexError::new(
                "graph.yaml does not match the hash recorded at run creation",
            ));
        }

        // Recompile against the prompt + defaults recorded at creation
        // (integrity-bound with the verified hash), not live config.
        let prompt = inputs.get("prompt").map(String::as_str);
        let graph = self.compile_with(&source, prompt, &defaults)?;
        if prompt.is_none() && loader::uses_prompt(&graph) {
            return Err(HexError::new(
                "run_created is missing the prompt required by graph.yaml",
            ));
        }

        // Fail closed on a malformed lifecycle before folding it into state.
        hex_kernel::check_journal(&graph, &events)
            .map_err(|i| HexError::new(format!("journal is invalid: {i}")))?;

        let mut state = State::default();
        for event in &events {
            state = reduce(&graph, state, event);
        }
        Ok((graph, state))
    }

    /// Start a new run of `reference` with an optional operator prompt. Blocks
    /// until the run reaches a terminal disposition (foreground MVP).
    ///
    /// # Errors
    /// Fails on resolution/validation, missing workers, or IO errors.
    pub fn start(&self, reference: &str, prompt: Option<&str>) -> Result<RunReport> {
        // Resolve the source exactly once, then compile that same text — no
        // second resolution that could observe a changed file (TOCTOU).
        let resolved = preset::resolve(reference, &self.root)?;
        let graph = self.compile(&resolved.source, prompt)?;
        // A graph that references {{prompt}} in a node prompt needs one at run
        // time (validate and graph stay lenient).
        if prompt.is_none() && loader::uses_prompt(&graph) {
            return Err(HexError::new(
                "this graph needs a prompt — pass -p/--prompt <text> or -f/--file <path>",
            ));
        }
        check_workers(&graph, &self.workers)?;

        let (run_id, run_dir) = self.new_run()?;
        std::fs::create_dir_all(run_dir.join("attempts"))?;
        let _lock = RunLock::acquire(&run_dir)?;

        // Persist the exact graph snapshot + its hash. The effective defaults
        // are recorded in the RunCreated event (below), not a separate unbound
        // file, so resume is bound to them and independent of later config edits.
        std::fs::write(run_dir.join("graph.yaml"), &resolved.source)?;
        let hash = graph_hash(&resolved.source);
        std::fs::write(run_dir.join("graph.sha256"), &hash)?;

        let journal = Journal::create(run_dir.join("events.jsonl"))?;
        let mut session = Session::new(
            &graph,
            &self.workers,
            run_id.clone(),
            run_dir,
            self.root.clone(),
            journal,
            State::default(),
            self.observer.as_deref(),
        );

        session.record(
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: hash,
                inputs: prompt
                    .map(|p| BTreeMap::from([("prompt".to_owned(), p.to_owned())]))
                    .unwrap_or_default(),
                defaults: defaults_to_map(&self.config.defaults),
            },
        )?;
        session.record(None, None, Actor::runtime(), EventBody::RunStarted)?;

        let disposition = session.drive()?;
        Ok(RunReport {
            run_id,
            origin: resolved.origin,
            disposition,
        })
    }

    /// Resume an existing run from its journal — after a pause or a crash.
    /// An interrupted attempt is marked and its node re-attempted.
    ///
    /// # Errors
    /// Fails if the run does not exist or cannot be replayed.
    pub fn resume(&self, run_id: &str) -> Result<RunReport> {
        let run_dir = self.run_dir(run_id)?;
        if !run_dir.join("graph.yaml").exists() {
            return Err(HexError::new(format!("run `{run_id}` not found")));
        }
        // Exclusive ownership: refuse to resume a run another live process is
        // driving. A crashed run's advisory lock is released by the OS, so this
        // succeeds after a real crash.
        let _lock = RunLock::acquire(&run_dir)?;

        // One scan: the writer hands back the events it read (torn tail already
        // repaired), which we verify + fold rather than reading the journal again.
        let (journal, events) = Journal::open_append(run_dir.join("events.jsonl"))?;
        let (graph, state) = self.verify_and_fold(run_id, events)?;
        check_workers(&graph, &self.workers)?;

        let mut session = Session::new(
            &graph,
            &self.workers,
            run_id.to_owned(),
            run_dir,
            self.root.clone(),
            journal,
            state,
            self.observer.as_deref(),
        );

        if session.state().is_finished() {
            let disposition = session.state().disposition().unwrap_or(Disposition::Failed);
            return Ok(RunReport {
                run_id: run_id.to_owned(),
                origin: format!("resume:{run_id}"),
                disposition,
            });
        }

        // Crashed after RunCreated but before RunStarted: activate the entry.
        if matches!(session.state().status, Status::Created) {
            session.record(None, None, Actor::runtime(), EventBody::RunStarted)?;
        }

        // Orphaned attempt (started, no terminal): mark it interrupted — with
        // its exact node + attempt id — then re-attempt, never silently rerun.
        if session.state().awaiting() {
            let node = session.state().current.clone();
            let attempt = session.state().current_attempt.clone();
            session.record(
                node.as_deref(),
                attempt.as_deref(),
                Actor::runtime(),
                EventBody::AttemptInterrupted,
            )?;
        }

        let disposition = session.drive()?;
        Ok(RunReport {
            run_id: run_id.to_owned(),
            origin: format!("resume:{run_id}"),
            disposition,
        })
    }

    /// Compute the projected status of a run from its journal.
    ///
    /// # Errors
    /// Fails if the run does not exist or cannot be replayed.
    pub fn status(&self, run_id: &str) -> Result<StatusReport> {
        let events = journal::read_all(&self.run_dir(run_id)?.join("events.jsonl"))
            .map_err(|_| HexError::new(format!("run `{run_id}` not found")))?;
        let (_, state) = self.verify_and_fold(run_id, events)?;
        Ok(StatusReport {
            run_id: run_id.to_owned(),
            status: state.status.clone(),
            current: state.current.clone(),
            attempts: state.attempts_total,
            disposition: state.disposition(),
        })
    }

    /// Read every event of a run (for `watch`).
    ///
    /// # Errors
    /// Fails if the run does not exist.
    pub fn events(&self, run_id: &str) -> Result<Vec<Event>> {
        let path = self.run_dir(run_id)?.join("events.jsonl");
        if !path.exists() {
            return Err(HexError::new(format!("run `{run_id}` not found")));
        }
        journal::read_all(&path)
    }

    /// The captured per-attempt output of a run, in attempt order — each
    /// attempt's `stdout.log`/`stderr.log`, mapped to its node via the journal.
    ///
    /// # Errors
    /// Fails if the run does not exist.
    pub fn logs(&self, run_id: &str) -> Result<Vec<AttemptLog>> {
        let run_dir = self.run_dir(run_id)?;
        let attempts = run_dir.join("attempts");
        let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        let mut logs = Vec::new();
        for event in self.events(run_id)? {
            if let EventBody::AttemptStarted { worker, .. } = &event.body
                && let Some(attempt_id) = &event.attempt_id
            {
                let dir = attempts.join(attempt_id);
                logs.push(AttemptLog {
                    attempt_id: attempt_id.clone(),
                    node_id: event.node_id.clone(),
                    worker: worker.clone(),
                    stdout: read(dir.join("stdout.log")),
                    stderr: read(dir.join("stderr.log")),
                });
            }
        }
        Ok(logs)
    }

    /// Cancel a run: if it has not finished, record a terminal `Cancelled`.
    /// Refuses if the run is actively locked by a driving process — writing a
    /// second terminal from here would violate the single-writer invariant.
    ///
    /// # Errors
    /// Fails if the run does not exist, is active, or cannot be appended to.
    pub fn cancel(&self, run_id: &str) -> Result<()> {
        let run_dir = self.run_dir(run_id)?;
        // Take the same exclusive lock a driver holds: acquiring it proves no
        // live process is writing, and holding it makes the append atomic w.r.t.
        // a concurrent resume. `acquire` fails cleanly if the run is active.
        let _lock = RunLock::acquire(&run_dir).map_err(|_| {
            HexError::new(
                "run is active (locked by a live process); external cancellation of a live \
                 foreground run is not supported in the slim MVP",
            )
        })?;
        // One scan: open the writer (repairs a torn tail, returns events), then
        // verify + fold those same events.
        let (mut journal, events) = Journal::open_append(run_dir.join("events.jsonl"))?;
        let (_, state) = self.verify_and_fold(run_id, events)?;
        if state.is_finished() {
            return Ok(());
        }
        journal.append(
            run_id,
            None,
            None,
            Actor::runtime(),
            EventBody::RunFinished {
                disposition: Disposition::Cancelled,
            },
        )?;
        Ok(())
    }

}

/// The run's creation record — hash, inputs, and effective defaults — read in a
/// single pass over the journal.
fn run_created(events: &[Event]) -> Option<(String, BTreeMap<String, String>, config::DefaultsSpec)> {
    events.iter().find_map(|e| match &e.body {
        EventBody::RunCreated {
            graph_hash,
            inputs,
            defaults,
        } => Some((graph_hash.clone(), inputs.clone(), defaults_from_map(defaults))),
        _ => None,
    })
}

/// Encode compile defaults as a stable string map for the RunCreated event.
fn defaults_to_map(defaults: &config::DefaultsSpec) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(worker) = &defaults.worker {
        map.insert("worker".to_owned(), worker.clone());
    }
    if let Some(context) = &defaults.context {
        map.insert("context".to_owned(), context.clone());
    }
    map
}

/// Decode compile defaults from the RunCreated event's map.
fn defaults_from_map(map: &BTreeMap<String, String>) -> config::DefaultsSpec {
    config::DefaultsSpec {
        worker: map.get("worker").cloned(),
        context: map.get("context").cloned(),
    }
}

/// Validate an operator-supplied run id: `run_` followed by ASCII alphanumerics
/// and underscores only. Rejects path separators, `..`, and anything that could
/// escape `.hex/runs`.
fn validate_run_id(run_id: &str) -> Result<()> {
    let ok = run_id.len() > 4
        && run_id.starts_with("run_")
        && run_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(HexError::new(format!("invalid run id `{run_id}`")))
    }
}

/// A high-entropy, hyphen-free suffix for run ids. `simple()` keeps it
/// `[0-9a-f]` only, satisfying [`validate_run_id`]'s path-safe grammar.
fn random_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// An exclusive per-run lock enforcing the single-writer invariant: only one
/// process may drive (or append a terminal to) a run at a time.
///
/// It is an **OS advisory lock** on an open file (`fs4`), so the kernel
/// releases it automatically if the holder is SIGKILLed — a crashed run can be
/// resumed, while a live run cannot be double-driven. The `run.lock` file
/// merely anchors the lock; its presence alone never blocks anyone.
struct RunLock {
    _file: std::fs::File,
}

impl RunLock {
    fn acquire(run_dir: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(run_dir.join("run.lock"))?;
        // Call the fs4 trait method by path: on a Rust >= 1.89 toolchain the
        // inherent `File::try_lock` (stabilized then) would otherwise shadow it,
        // and this crate targets rust 1.85 where only fs4 provides locking.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {
                let _ = (&file).write_all(format!("{}\n", std::process::id()).as_bytes());
                Ok(Self { _file: file })
            }
            Err(fs4::TryLockError::WouldBlock) => Err(HexError::new(
                "run is already active (locked by a live process); refusing a concurrent writer",
            )),
            Err(fs4::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// The one control surface every client speaks. The CLI, MCP transport, and
/// dashboard are all thin clients over this trait — never parallel
/// implementations. Authority is scoped per actor, not per surface.
pub trait RuntimeClient {
    /// List every runnable graph.
    fn list_graphs(&self) -> Vec<GraphEntry>;
    /// Start a new run with an optional operator prompt.
    ///
    /// # Errors
    /// Propagates resolution, validation, and IO failures.
    fn start(&self, reference: &str, prompt: Option<&str>) -> Result<RunReport>;
    /// Resume an existing run.
    ///
    /// # Errors
    /// Propagates replay and IO failures.
    fn resume(&self, run_id: &str) -> Result<RunReport>;
    /// Projected status of a run.
    ///
    /// # Errors
    /// Propagates replay failures.
    fn status(&self, run_id: &str) -> Result<StatusReport>;
    /// Every event of a run.
    ///
    /// # Errors
    /// Fails if the run does not exist.
    fn events(&self, run_id: &str) -> Result<Vec<Event>>;
    /// Per-attempt captured output of a run.
    ///
    /// # Errors
    /// Fails if the run does not exist.
    fn logs(&self, run_id: &str) -> Result<Vec<AttemptLog>>;
    /// Cancel a run.
    ///
    /// # Errors
    /// Propagates IO failures.
    fn cancel(&self, run_id: &str) -> Result<()>;
}

impl RuntimeClient for Runtime {
    fn list_graphs(&self) -> Vec<GraphEntry> {
        Runtime::list_graphs(self)
    }
    fn start(&self, reference: &str, prompt: Option<&str>) -> Result<RunReport> {
        Runtime::start(self, reference, prompt)
    }
    fn resume(&self, run_id: &str) -> Result<RunReport> {
        Runtime::resume(self, run_id)
    }
    fn status(&self, run_id: &str) -> Result<StatusReport> {
        Runtime::status(self, run_id)
    }
    fn events(&self, run_id: &str) -> Result<Vec<Event>> {
        Runtime::events(self, run_id)
    }
    fn logs(&self, run_id: &str) -> Result<Vec<AttemptLog>> {
        Runtime::logs(self, run_id)
    }
    fn cancel(&self, run_id: &str) -> Result<()> {
        Runtime::cancel(self, run_id)
    }
}

/// The project root for a runtime: the current working directory.
///
/// # Errors
/// Fails if the current directory cannot be read.
pub fn project_root() -> Result<PathBuf> {
    std::env::current_dir().map_err(Into::into)
}

/// Convenience: build a [`Runtime`] rooted at `root`.
///
/// # Errors
/// Propagates config load failures.
pub fn open(root: &Path) -> Result<Runtime> {
    Runtime::new(root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The advisory lock is real (a no-op would let the second acquire succeed)
    /// and is released when the holder drops — the same fd-close path the OS
    /// takes when a run's process is SIGKILLed, which is what makes a crashed
    /// run resumable while a live one cannot be double-driven.
    #[test]
    fn run_lock_rejects_a_second_holder_and_releases_on_drop() {
        let dir = std::env::temp_dir().join(format!("hex-runlock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");

        let held = RunLock::acquire(&dir).expect("first acquire");
        assert!(
            RunLock::acquire(&dir).is_err(),
            "a second concurrent writer must be refused while the lock is held"
        );

        drop(held);
        RunLock::acquire(&dir).expect("acquire succeeds once the holder releases");

        std::fs::remove_dir_all(&dir).ok();
    }
}
