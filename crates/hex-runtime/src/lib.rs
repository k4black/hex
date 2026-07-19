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

/// The in-process runtime: owns config + the worker registry and executes runs
/// under a project root (the directory containing `.hex/`).
pub struct Runtime {
    root: PathBuf,
    config: Config,
    workers: Workers,
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
        }
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

    /// Compile + validate a graph reference without running it.
    ///
    /// # Errors
    /// Fails on resolution, parse, or validation errors.
    pub fn validate(&self, reference: &str, inputs: &BTreeMap<String, String>) -> Result<Graph> {
        let resolved = preset::resolve(reference, &self.root)?;
        self.compile(&resolved.source, inputs)
    }

    /// Compile already-resolved YAML `source` to a validated IR.
    fn compile(&self, source: &str, inputs: &BTreeMap<String, String>) -> Result<Graph> {
        let graph = loader::load(source, inputs, &self.config.defaults)?;
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

    /// Start a new run of `reference`, parametrized by `inputs`. Blocks until
    /// the run reaches a terminal disposition (foreground MVP).
    ///
    /// # Errors
    /// Fails on resolution/validation, missing workers, or IO errors.
    pub fn start(
        &self,
        reference: &str,
        inputs: &BTreeMap<String, String>,
    ) -> Result<RunReport> {
        // Resolve the source exactly once, then compile that same text — no
        // second resolution that could observe a changed file (TOCTOU).
        let resolved = preset::resolve(reference, &self.root)?;
        let graph = self.compile(&resolved.source, inputs)?;
        check_workers(&graph, &self.workers)?;

        let (run_id, run_dir) = self.new_run()?;
        std::fs::create_dir_all(run_dir.join("attempts"))?;
        let _lock = RunLock::acquire(&run_dir)?;

        // Persist the exact graph snapshot + its hash for auditability/resume.
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
        );

        session.record(
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: hash,
                inputs: inputs.clone(),
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
        // Exclusive ownership: refuse to resume a run another process is driving.
        let _lock = RunLock::acquire(&run_dir)?;

        let source = std::fs::read_to_string(run_dir.join("graph.yaml"))?;
        let events = journal::read_all(&run_dir.join("events.jsonl"))?;

        // Snapshot integrity: the file on disk, the recorded sha256, and the
        // hash the run was created with must all agree before we replay.
        let computed = graph_hash(&source);
        let stored = std::fs::read_to_string(run_dir.join("graph.sha256"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if stored != computed {
            return Err(HexError::new(
                "graph.yaml does not match graph.sha256 — snapshot was modified",
            ));
        }
        if let Some(recorded) = recorded_graph_hash(&events)
            && recorded != computed
        {
            return Err(HexError::new(
                "graph.yaml does not match the hash recorded at run creation",
            ));
        }

        let inputs = recorded_inputs(&events);
        // Re-validate on resume: a graph that no longer validates must not run.
        let graph = self.compile(&source, &inputs)?;
        check_workers(&graph, &self.workers)?;

        // Replay the journal into the current projection.
        let mut state = State::default();
        for event in &events {
            state = reduce(&graph, state, event);
        }

        let journal = Journal::open_append(run_dir.join("events.jsonl"))?;
        let mut session = Session::new(
            &graph,
            &self.workers,
            run_id.to_owned(),
            run_dir,
            self.root.clone(),
            journal,
            state,
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
        if session.state().awaiting {
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
        let (_, state) = self.replay(run_id)?;
        Ok(StatusReport {
            run_id: run_id.to_owned(),
            status: state.status.clone(),
            current: state.current.clone(),
            attempts: state.attempts_total,
            disposition: state.disposition(),
        })
    }

    /// Read every event of a run (for `watch`/`logs`).
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

    /// Cancel a run: if it has not finished, record a terminal `Cancelled`.
    /// Refuses if the run is actively locked by a driving process — writing a
    /// second terminal from here would violate the single-writer invariant.
    ///
    /// # Errors
    /// Fails if the run does not exist, is active, or cannot be appended to.
    pub fn cancel(&self, run_id: &str) -> Result<()> {
        let (_, state) = self.replay(run_id)?;
        if state.is_finished() {
            return Ok(());
        }
        if self.run_dir(run_id)?.join("run.lock").exists() {
            return Err(HexError::new(
                "run is active (locked); external cancellation of a live foreground run \
                 is not supported in the slim MVP",
            ));
        }
        let mut journal = Journal::open_append(self.run_dir(run_id)?.join("events.jsonl"))?;
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

    /// Load a run's graph and replay its journal into projected state.
    fn replay(&self, run_id: &str) -> Result<(Graph, State)> {
        let run_dir = self.run_dir(run_id)?;
        let source = std::fs::read_to_string(run_dir.join("graph.yaml"))
            .map_err(|_| HexError::new(format!("run `{run_id}` not found")))?;
        let events = journal::read_all(&run_dir.join("events.jsonl"))?;
        let inputs = recorded_inputs(&events);
        let graph = self.compile(&source, &inputs)?;
        let mut state = State::default();
        for event in &events {
            state = reduce(&graph, state, event);
        }
        Ok((graph, state))
    }
}

/// Recover the `--input` values a run was created with from its journal.
fn recorded_inputs(events: &[Event]) -> BTreeMap<String, String> {
    for event in events {
        if let EventBody::RunCreated { inputs, .. } = &event.body {
            return inputs.clone();
        }
    }
    BTreeMap::new()
}

/// The graph hash recorded at run creation, if present.
fn recorded_graph_hash(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match &e.body {
        EventBody::RunCreated { graph_hash, .. } => Some(graph_hash.clone()),
        _ => None,
    })
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

/// A short, high-entropy suffix for run ids (no external RNG dependency).
fn random_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:08x}", nanos ^ pid.wrapping_mul(2_654_435_761))
}

/// An exclusive per-run lock enforcing the single-writer invariant: only one
/// process may drive (or append a terminal to) a run at a time. Released on drop.
struct RunLock {
    path: PathBuf,
}

impl RunLock {
    fn acquire(run_dir: &Path) -> Result<Self> {
        let path = run_dir.join("run.lock");
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(HexError::new(
                "run is already active (locked); refusing a concurrent writer",
            )),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The one control surface every client speaks. The CLI, MCP transport, and
/// dashboard are all thin clients over this trait — never parallel
/// implementations. Authority is scoped per actor, not per surface.
pub trait RuntimeClient {
    /// Start a new run.
    ///
    /// # Errors
    /// Propagates resolution, validation, and IO failures.
    fn start(&self, reference: &str, inputs: &BTreeMap<String, String>) -> Result<RunReport>;
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
    /// Cancel a run.
    ///
    /// # Errors
    /// Propagates IO failures.
    fn cancel(&self, run_id: &str) -> Result<()>;
}

impl RuntimeClient for Runtime {
    fn start(&self, reference: &str, inputs: &BTreeMap<String, String>) -> Result<RunReport> {
        Runtime::start(self, reference, inputs)
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
