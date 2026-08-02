//! `hex-runtime` — the imperative shell around the pure kernel.
//!
//! Everything effectful lives here: the drive loop, effect execution, the
//! append-only JSONL journal, config + preset resolution, workspace layout,
//! and crash recovery. The kernel decides *what*; the runtime is the only
//! layer that *does*.
//!
//! Clients (CLI, MCP, dashboard) are thin peers over [`Runtime`]: they parse
//! arguments and render, and every fact they show is one this layer computed.
//! There was a `RuntimeClient` trait here for a future `Remote` client; it had
//! one implementation and no callers, so it was deleted — a trait is cheaper to
//! re-derive from a second implementation than to keep honest without one.

pub mod config;
pub mod control;
pub mod doctor;
pub mod driver;
pub mod error;
pub mod journal;
pub mod loader;
pub mod preset;
#[cfg(test)]
mod test_support;
pub mod workers;
pub mod worktree;

pub use control::{Inbox, Liveness};
pub use doctor::Report as DoctorReport;
pub use driver::{AttemptView, NodeProgress, NodeState, ProgressSink};
pub use error::{HexError, Result};
pub use hex_kernel::graph::NodeKind;
pub use hex_kernel::graph::{Budget, Context, Node, NodeSpec};
pub use hex_kernel::topology::{Cycle, EdgeClass, Topology, Transition};
pub use hex_kernel::{Graph, RunState, Status, Totals, Usage};
pub use hex_proto::{Actor, Command, Disposition, Event, EventBody, ModelUsage, PROTOCOL_VERSION};
pub use preset::{Entry as GraphEntry, Layer};
pub use workers::Workers;
pub use worktree::Isolation;

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use config::Config;
use driver::{Session, check_workers, graph_hash};
use hex_kernel::RunState as State;
use journal::Journal;

// Keys under which a run's worktree lease is recorded in `RunCreated.inputs`
// (written by `start`, read back by `resume`). Stringly-typed for now — a typed,
// validated journal record is a tracked follow-up (see TODO.md).
const WT_SLOT: &str = "worktree.slot";
const WT_BRANCH: &str = "worktree.branch";
const WT_BASE_REF: &str = "worktree.base_ref";

/// The outcome of starting or resuming a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// The run's id.
    pub run_id: String,
    /// Where the graph came from (path or `built-in:<name>`).
    pub origin: String,
    /// The terminal disposition reached, or `None` when an operator paused the
    /// run: a pause is not an outcome, and the run continues with `hex resume`.
    pub disposition: Option<Disposition>,
}

/// One row of `hex runs`: enough to pick a run out of a list without opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// The run's id.
    pub run_id: String,
    /// Lifecycle status, or `None` if the run could not be replayed.
    pub status: Option<Status>,
    /// The active node, if any.
    pub current: Option<String>,
    /// Attempts started so far.
    pub attempts: u32,
    /// Terminal disposition, if finished.
    pub disposition: Option<Disposition>,
    /// What can be concluded about the driving process (lock + heartbeat).
    pub liveness: Liveness,
    /// When the run was created (Unix epoch ms).
    pub created_at_ms: u64,
    /// When the journal last grew (Unix epoch ms) — the run's true "age".
    pub updated_at_ms: u64,
    /// Why the run could not be replayed, when `status` is `None`. A listing must
    /// still show a broken run: hiding it is how a run gets lost.
    pub error: Option<String>,
}

/// Whether a cancellation was applied directly or handed to a live driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancellation {
    /// No process was driving the run, so the terminal event was appended here.
    Recorded,
    /// A live driver holds the run lock; the command is queued in its control
    /// inbox and takes effect at the next attempt boundary.
    Requested,
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
    /// Times each node was entered — the per-node attempt count a spend
    /// breakdown is read against ("12 attempts on `implement`" is the number
    /// that explains the bill).
    pub visits: BTreeMap<String, u32>,
    /// What the run spent, as the kernel folded it from `AttemptReported`.
    pub usage: Usage,
    /// The attempt executing right now, if any — what an operator watching a live
    /// loop is actually asking about.
    pub in_flight: Option<InFlight>,
    /// Control commands sitting in the inbox that no driver has claimed yet.
    /// Distinct from [`Self::pending_steer`], which has already been journaled:
    /// between `hex steer` and the driver's next attempt boundary a command is
    /// real but invisible to the projection, and an operator asking "did that
    /// land?" needs to see it.
    pub queued: Vec<Command>,
    /// Operator guidance already journaled and awaiting the next *agent* attempt.
    /// Visible because a steer that lands at the next attempt boundary is
    /// otherwise indistinguishable from one that was dropped.
    pub pending_steer: Vec<String>,
    /// The `human` node blocking the run, if any.
    pub asked: Option<String>,
    /// That node's question, so the thing you have to answer is on screen with the
    /// fact that you have to answer it.
    pub question: Option<String>,
}

/// How far a follower has read each of an attempt's streams.
///
/// Opaque to the caller: it holds paths, which are the runtime's business. Reset
/// it (or make a new one) when moving to a different attempt.
#[derive(Debug, Clone, Default)]
pub struct StreamCursor {
    offsets: BTreeMap<PathBuf, u64>,
}

impl StreamCursor {
    /// A cursor positioned at the *end* of everything already written, so a
    /// follower joining a long-running attempt streams what happens next instead
    /// of replaying an hour of output it missed.
    ///
    /// # Errors
    /// Fails if the run id or attempt id is not a safe grammar.
    pub fn at_end(runtime: &Runtime, run_id: &str, attempt_id: &str) -> Result<Self> {
        let mut cursor = Self::default();
        runtime.read_streams(run_id, attempt_id, &mut cursor)?;
        Ok(cursor)
    }
}

/// Newly appended output from one of an attempt's streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamChunk {
    /// Which stream: `stdout`, `stderr`, or `3-test/stdout` for a command step.
    pub label: String,
    /// The bytes appended since the cursor's previous position, lossily decoded
    /// (a read boundary can land mid-codepoint).
    pub text: String,
}

/// The attempt currently executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    /// Its id.
    pub attempt_id: String,
    /// The node it is running.
    pub node_id: String,
    /// The worker running it (agent attempts only).
    pub worker: Option<String>,
    /// When it started (Unix epoch ms), for an elapsed clock.
    pub started_at_ms: u64,
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
    /// The attempt's captured final message (an agent's last message), if any.
    pub result: Option<String>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// One entry per step of a `command` node, in declared order. Empty for an
    /// agent attempt, which writes its capture to the attempt dir itself.
    pub steps: Vec<StepLog>,
}

/// The captured output of one step of a `command` attempt.
///
/// A multi-step command node writes nothing to the attempt dir — every byte
/// lands in a numbered per-step directory — so without this a failing check's
/// output was on disk and reachable from no CLI surface at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    /// The step's directory name (`3-test`), which carries its declared position.
    pub label: String,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// The step's exit status as the driver recorded it beside its logs (`"0"`,
    /// `"101"`, or `"signal"`). `None` for a step from before this was recorded,
    /// or one whose process never produced a status.
    ///
    /// This is what lets a reader point at *the* failing check: the journal names
    /// the failed steps for the attempt, but a step directory alone could not say
    /// whether it was the culprit.
    pub exit: Option<String>,
}

impl StepLog {
    /// Whether this step failed, as far as its recorded status can say. An
    /// unrecorded status is not treated as a failure — silence is not evidence.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.exit.as_deref().is_some_and(|code| code != "0")
    }
}

/// The in-process runtime: owns config + the worker registry and executes runs
/// under a project root (the directory containing `.hex/`).
pub struct Runtime {
    root: PathBuf,
    config: Config,
    workers: Workers,
    sink: Option<Box<dyn ProgressSink>>,
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
            sink: None,
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
            sink: None,
        }
    }

    /// Install a [`ProgressSink`] that observes each journaled event and the
    /// start/finish of every attempt during a `run`/`resume`, so a foreground
    /// caller can stream live progress and preview in-flight agent output.
    #[must_use]
    pub fn with_progress(mut self, sink: Box<dyn ProgressSink>) -> Self {
        self.sink = Some(sink);
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

    /// Allocate a readable, collision-resistant run id by exclusively creating
    /// its directory. Ids read `yyyy-MM-dd-<session>` when the operator names the
    /// run, else `yyyy-MM-dd-<workflow>-<short-uuid>`; a same-day clash gets a
    /// `-2`, `-3`, … suffix. Exclusive `create_dir` means two concurrent starts
    /// can never select the same directory and truncate each other.
    fn new_run(&self, workflow: &str, session: Option<&str>) -> Result<(String, PathBuf)> {
        let runs = self.runs_dir();
        std::fs::create_dir_all(&runs)?;
        let date = today_utc();
        let base = match session {
            Some(name) => {
                let s = slug(name);
                if s.is_empty() {
                    return Err(HexError::new(format!(
                        "run name `{name}` has no usable characters"
                    )));
                }
                format!("{date}-{s}")
            }
            None => match slug(workflow) {
                w if w.is_empty() => format!("{date}-{}", random_suffix()),
                w => format!("{date}-{w}-{}", short_uuid()),
            },
        };
        for n in 1..=10_000u32 {
            let id = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
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
        self.compile(&resolved.source)
    }

    /// Compile already-resolved YAML `source` to a validated IR, using this
    /// runtime's live config defaults. The operator prompt is not needed here —
    /// `{{prompt}}` is substituted at attempt-start, not at compile.
    fn compile(&self, source: &str) -> Result<Graph> {
        self.compile_with(
            source,
            &self.config.defaults,
            &self.config.roles,
            &self.config.checks,
        )
    }

    /// Compile with an explicit set of fallback defaults — used on resume so a
    /// run recompiles against the defaults it was *created* with, not whatever
    /// the mutable config happens to say now.
    fn compile_with(
        &self,
        source: &str,
        defaults: &config::DefaultsSpec,
        roles: &BTreeMap<String, config::RoleSpec>,
        checks: &BTreeMap<String, Vec<String>>,
    ) -> Result<Graph> {
        let graph = loader::load_with(source, defaults, roles, checks)?;
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
    fn verify_and_fold(&self, run_id: &str, events: &[Event]) -> Result<(Graph, State)> {
        let run_dir = self.run_dir(run_id)?;
        let source = std::fs::read_to_string(run_dir.join("graph.yaml"))
            .map_err(|_| HexError::new(format!("run `{run_id}` not found")))?;

        // The journal's own run id must match the directory/operator id.
        if let Some(first) = events.first()
            && first.run_id != run_id
        {
            return Err(HexError::new(
                "journal run id does not match the run directory",
            ));
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
        let (recorded_hash, inputs, defaults, checks) = run_created(events)
            .ok_or_else(|| HexError::new("journal has no run_created record to verify against"))?;
        if recorded_hash != computed {
            return Err(HexError::new(
                "graph.yaml does not match the hash recorded at run creation",
            ));
        }

        // Recompile against the defaults recorded at creation (integrity-bound
        // with the verified hash), not live config. The prompt is verified
        // present and re-substituted at attempt-start.
        let prompt = inputs.get("prompt").map(String::as_str);
        // Roles come from live config on resume: their preambles are already baked
        // into the recorded prompts via `checks`-style resolution at creation, and
        // re-resolving them cannot change the graph's shape.
        let graph = self.compile_with(&source, &defaults, &self.config.roles, &checks)?;
        if prompt.is_none() && loader::uses_prompt(&graph) {
            return Err(HexError::new(
                "run_created is missing the prompt required by graph.yaml",
            ));
        }

        // Fail closed on a malformed lifecycle, and take the projection the audit
        // folded on its way through rather than folding the same events again.
        let state = hex_kernel::check_journal(&graph, events)
            .map_err(|i| HexError::new(format!("journal is invalid: {i}")))?;
        Ok((graph, state))
    }

    /// Start a new run of `reference` with an optional operator prompt. Blocks
    /// until the run reaches a terminal disposition (or an operator pauses it).
    ///
    /// # Errors
    /// Fails on resolution/validation, missing workers, or IO errors.
    pub fn start(
        &self,
        reference: &str,
        prompt: Option<&str>,
        name: Option<&str>,
        isolation: &Isolation,
    ) -> Result<RunReport> {
        self.start_run(reference, prompt, name, isolation, None)
    }

    /// Reserve a run id and directory without driving anything.
    ///
    /// `hex run --detach` needs the id *before* the driving process exists: it is
    /// what the launcher prints, and it names the directory the child's stdio is
    /// redirected into. Resolving and compiling here also means a bad graph, a
    /// missing prompt, or an uninstalled agent CLI fails in the foreground rather
    /// than in a detached child nobody is watching.
    ///
    /// # Errors
    /// Fails on the same conditions as [`Runtime::start`], before any run exists.
    pub fn reserve(
        &self,
        reference: &str,
        prompt: Option<&str>,
        name: Option<&str>,
    ) -> Result<(String, PathBuf)> {
        let resolved = preset::resolve(reference, &self.root)?;
        let graph = self.prepare(&resolved.source, prompt)?;
        self.new_run(&graph.name, name)
    }

    /// Drive a run whose id and directory were already reserved by a launcher.
    ///
    /// # Errors
    /// Fails if the reservation is unusable, or on the same conditions as
    /// [`Runtime::start`].
    pub fn start_reserved(
        &self,
        run_id: &str,
        reference: &str,
        prompt: Option<&str>,
        isolation: &Isolation,
    ) -> Result<RunReport> {
        let run_dir = self.run_dir(run_id)?;
        if !run_dir.is_dir() {
            return Err(HexError::new(format!(
                "run `{run_id}` was not reserved (no run directory)"
            )));
        }
        if run_dir.join("events.jsonl").exists() {
            return Err(HexError::new(format!(
                "run `{run_id}` already has a journal — use `resume`, never a second start"
            )));
        }
        self.start_run(
            reference,
            prompt,
            None,
            isolation,
            Some((run_id.to_owned(), run_dir)),
        )
    }

    /// Compile `source` and refuse everything that must not reach a run: an
    /// invalid graph, a missing operator prompt, an unknown worker, an agent CLI
    /// that is not installed.
    fn prepare(&self, source: &str, prompt: Option<&str>) -> Result<Graph> {
        let graph = self.compile(source)?;
        // A graph that references {{prompt}} in a node prompt needs one at run
        // time (validate and graph stay lenient).
        if prompt.is_none() && loader::uses_prompt(&graph) {
            return Err(HexError::new(
                "this graph needs a prompt — pass -p/--prompt <text> or -f/--file <path>",
            ));
        }
        check_workers(&graph, &self.workers)?;
        doctor::preflight(&graph, &self.workers)?;
        Ok(graph)
    }

    fn start_run(
        &self,
        reference: &str,
        prompt: Option<&str>,
        name: Option<&str>,
        isolation: &Isolation,
        reserved: Option<(String, PathBuf)>,
    ) -> Result<RunReport> {
        // Resolve the source exactly once, then compile that same text — no
        // second resolution that could observe a changed file (TOCTOU).
        let resolved = preset::resolve(reference, &self.root)?;
        let graph = self.prepare(&resolved.source, prompt)?;

        let (run_id, run_dir) = match reserved {
            Some(pair) => pair,
            None => self.new_run(&graph.name, name)?,
        };
        std::fs::create_dir_all(run_dir.join("attempts"))?;
        let _lock = RunLock::acquire(&run_dir)?;

        // Isolation: lease a git worktree (held for the run) if requested. Its
        // metadata rides in the RunCreated inputs so `resume` reattaches to it.
        let mut inputs: BTreeMap<String, String> = prompt
            .map(|p| BTreeMap::from([("prompt".to_owned(), p.to_owned())]))
            .unwrap_or_default();
        let mut workdir = self.root.clone();
        let mut worktree_ctx = None;
        let mut slot = None;
        if let Isolation::Worktree { base, init } = isolation {
            let leased = self.setup_worktree(&run_id, base.as_deref(), init, &run_dir)?;
            workdir = leased.dir.clone();
            inputs.insert(
                WT_SLOT.to_owned(),
                leased.dir.to_string_lossy().into_owned(),
            );
            inputs.insert(WT_BRANCH.to_owned(), leased.branch.clone());
            inputs.insert(WT_BASE_REF.to_owned(), leased.base_ref.clone());
            worktree_ctx = Some(driver::WorktreeCtx {
                branch: leased.branch.clone(),
                base_ref: leased.base_ref.clone(),
            });
            slot = Some(leased);
        }

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
            workdir,
            journal,
            State::default(),
            prompt.map(ToOwned::to_owned),
            worktree_ctx,
            self.sink.as_deref(),
        );

        session.record(
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: hash,
                inputs,
                defaults: defaults_to_map(&self.config.defaults),
                checks: resolved_checks(&graph),
            },
        )?;
        // A reclaimed slot discarded a prior run's uncommitted work — record
        // exactly what, in this run's authoritative journal.
        if let Some(report) = slot.as_ref().and_then(|s| s.reclaimed.as_deref())
            && !report.is_empty()
        {
            session.record(
                None,
                None,
                Actor::runtime(),
                EventBody::Note {
                    text: format!(
                        "worktree slot reclaimed; discarded a prior run's uncommitted changes:\n{report}"
                    ),
                },
            )?;
        }
        session.record(None, None, Actor::runtime(), EventBody::RunStarted)?;

        let disposition = session.drive()?;
        drop(slot); // release the worktree lock only after the run finishes
        Ok(RunReport {
            run_id,
            origin: resolved.origin,
            disposition,
        })
    }

    /// Lease and prime a worktree slot for a run. Fails closed outside a git repo.
    fn setup_worktree(
        &self,
        run_id: &str,
        base: Option<&str>,
        init: &[String],
        run_dir: &Path,
    ) -> Result<worktree::Slot> {
        if !worktree::is_git_repo(&self.root) {
            return Err(HexError::new(
                "`--worktree` requires the project to be a git repository",
            ));
        }
        worktree::ensure_gitignored(&self.root, ".hex/worktrees/")?;
        let (base_ref, base_sha) = worktree::resolve_base(&self.root, base)?;
        let branch = format!("hex/{run_id}");
        let slot = worktree::lease(&self.root, &branch, &base_ref, &base_sha)?;
        if slot.warmup_needed && !init.is_empty() {
            worktree::run_warmup(&slot.dir, init, run_dir)?;
        }
        Ok(slot)
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
        // The operator prompt recorded at creation, re-applied at attempt-start.
        // Read the creation record once: the operator prompt plus any recorded
        // worktree lease to reattach.
        let created = run_created(&events);
        let inputs = created.as_ref().map(|(_, inputs, _, _)| inputs);
        let prompt = inputs.and_then(|i| i.get("prompt").cloned());
        let mut workdir = self.root.clone();
        let mut worktree_ctx = None;
        // Held for the run's duration (released on drop / crash); never read.
        let mut _slot_lock = None;
        if let Some(slot_dir) = inputs.and_then(|i| i.get(WT_SLOT)) {
            // A recorded worktree with no branch means a corrupt journal — fail
            // closed rather than handing an empty ref to `git`.
            let branch = inputs
                .and_then(|i| i.get(WT_BRANCH))
                .filter(|b| !b.is_empty())
                .ok_or_else(|| {
                    HexError::new("run recorded a worktree but no branch — journal is corrupt")
                })?
                .clone();
            let base_ref = inputs
                .and_then(|i| i.get(WT_BASE_REF))
                .cloned()
                .unwrap_or_default();
            _slot_lock = Some(worktree::reattach(
                &self.root,
                Path::new(slot_dir),
                &branch,
            )?);
            worktree_ctx = Some(driver::WorktreeCtx { branch, base_ref });
            workdir = PathBuf::from(slot_dir);
        }
        let (graph, state) = self.verify_and_fold(run_id, &events)?;
        check_workers(&graph, &self.workers)?;
        doctor::preflight(&graph, &self.workers)?;

        let mut session = Session::new(
            &graph,
            &self.workers,
            run_id.to_owned(),
            run_dir,
            workdir,
            journal,
            state,
            prompt,
            worktree_ctx,
            self.sink.as_deref(),
        );

        if session.state().is_finished() {
            let disposition = session.state().disposition().unwrap_or(Disposition::Failed);
            return Ok(RunReport {
                run_id: run_id.to_owned(),
                origin: format!("resume:{run_id}"),
                disposition: Some(disposition),
            });
        }

        // Crashed after RunCreated but before RunStarted: activate the entry.
        if matches!(session.state().status, Status::Created) {
            session.record(None, None, Actor::runtime(), EventBody::RunStarted)?;
        }

        // Paused by an operator: lift the pause before scheduling, so the journal
        // shows the suspension being ended rather than silently ignored.
        if matches!(session.state().status, Status::Paused) {
            session.record(None, None, Actor::runtime(), EventBody::RunResumed)?;
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
        let run_dir = self.run_dir(run_id)?;
        // The same distinction `hex runs` makes, and for the same reason: a
        // detached run is reserved a moment before its driver writes anything, and
        // reporting "not found" in that window sends an operator looking for a run
        // that exists. Only a missing *directory* means missing.
        if !run_dir.is_dir() {
            return Err(HexError::new(format!("run `{run_id}` not found")));
        }
        let events = journal::read_all(&run_dir.join("events.jsonl")).map_err(|_| {
            HexError::new(format!(
                "run `{run_id}` has no journal yet (reserved, or never started)"
            ))
        })?;
        let (_, state) = self.verify_and_fold(run_id, &events)?;
        // What is happening *right now*, which is what an operator watching a live
        // loop is asking. The projection knows an attempt is in flight; only the
        // journal knows when it started, so the elapsed clock comes from the
        // `AttemptStarted` event rather than being tracked anywhere.
        let in_flight = state.current_attempt.as_ref().and_then(|attempt| {
            let started = events.iter().rev().find(|e| {
                e.attempt_id.as_ref() == Some(attempt)
                    && matches!(e.body, EventBody::AttemptStarted { .. })
            })?;
            Some(InFlight {
                attempt_id: attempt.clone(),
                node_id: started.node_id.clone().unwrap_or_default(),
                worker: match &started.body {
                    EventBody::AttemptStarted { worker, .. } => worker.clone(),
                    _ => None,
                },
                started_at_ms: started.at_ms,
            })
        });
        // A run parked on a `human` node is *waiting on you*, and used to report a
        // bare `running` with the question visible only through `hex watch`.
        let question = state.asked.as_ref().and_then(|node| {
            events.iter().rev().find_map(|e| match &e.body {
                EventBody::HumanRequested { prompt } if e.node_id.as_ref() == Some(node) => {
                    Some(prompt.clone())
                }
                _ => None,
            })
        });
        Ok(StatusReport {
            run_id: run_id.to_owned(),
            status: state.status.clone(),
            current: state.current.clone(),
            attempts: state.attempts_total,
            disposition: state.disposition(),
            visits: state.visits.clone(),
            usage: state.usage.clone(),
            in_flight,
            // Two different "not yet applied" states, and an operator needs both:
            // `queued` is in the control inbox and no driver has looked at it,
            // `pending_steer` has been journaled and awaits the next agent attempt.
            // Propagated, not swallowed: an unreadable control directory means
            // hex cannot say whether a queued command exists, and reporting
            // "nothing queued" would be a claim it has no basis for.
            queued: Inbox::new(&run_dir)
                .queued()?
                .into_iter()
                .map(|e| e.command)
                .collect(),
            pending_steer: state.pending_steer.clone(),
            asked: state.asked.clone(),
            question,
        })
    }

    /// One attempt's captured streams, read from `cursor` onward.
    ///
    /// This is how a client tails a *running* attempt without learning the
    /// on-disk layout: it hands back the bytes appended since it last asked, plus
    /// an updated cursor. An attempt's streams are its own stdout/stderr **and**
    /// both streams of every numbered step directory a `command` node writes, in
    /// declared position — a gate is the slow thing an operator waits on, and
    /// without its steps a follower showed a header and nothing else.
    ///
    /// # Errors
    /// Fails if the run id or attempt id is not a safe grammar, or the run does
    /// not exist.
    pub fn read_streams(
        &self,
        run_id: &str,
        attempt_id: &str,
        cursor: &mut StreamCursor,
    ) -> Result<Vec<StreamChunk>> {
        // The attempt id comes from the journal, never from an operator, but it
        // still ends up in a path — so it gets the same grammar check as a run id.
        validate_run_id(attempt_id)?;
        let dir = self.run_dir(run_id)?.join("attempts").join(attempt_id);
        let mut chunks = Vec::new();
        for path in attempt_streams(&dir) {
            let label = stream_label(&dir, &path);
            let offset = cursor.offsets.entry(path.clone()).or_insert(0);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let len = meta.len();
            // A shorter file means it was truncated (a re-attempt clears its logs);
            // leaving the cursor past the end would go silent forever.
            if len < *offset {
                *offset = 0;
            }
            if len == *offset {
                continue;
            }
            // Read exactly the bytes we accounted for. Reading to EOF instead would
            // print anything a concurrent writer appended *after* the length was
            // sampled while advancing the cursor only to the sampled length — so
            // the next read would print those bytes a second time.
            let text = read_span(&path, *offset, len - *offset)?;
            *offset = len;
            if !text.is_empty() {
                chunks.push(StreamChunk { label, text });
            }
        }
        Ok(chunks)
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
    /// attempt's `stdout.log`/`stderr.log` plus any per-step captures, mapped to
    /// its node via the journal.
    ///
    /// # Errors
    /// Fails if the run does not exist.
    pub fn logs(&self, run_id: &str) -> Result<Vec<AttemptLog>> {
        let run_dir = self.run_dir(run_id)?;
        let attempts = run_dir.join("attempts");
        let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        let events = self.events(run_id)?;
        // Verify the journal's lifecycle (snapshot + `check_journal`) before
        // trusting it — so a forged/misplaced `NodeResult` can't surface here.
        self.verify_and_fold(run_id, &events)?;
        // Captured final messages, by attempt.
        let mut results: BTreeMap<String, String> = BTreeMap::new();
        for event in &events {
            if let EventBody::NodeResult { text } = &event.body
                && let Some(attempt_id) = &event.attempt_id
            {
                results.insert(attempt_id.clone(), text.clone());
            }
        }
        let mut logs = Vec::new();
        for event in &events {
            if let EventBody::AttemptStarted { worker, .. } = &event.body
                && let Some(attempt_id) = &event.attempt_id
            {
                let dir = attempts.join(attempt_id);
                logs.push(AttemptLog {
                    attempt_id: attempt_id.clone(),
                    node_id: event.node_id.clone(),
                    worker: worker.clone(),
                    result: results.get(attempt_id).cloned(),
                    stdout: read(dir.join("stdout.log")),
                    stderr: read(dir.join("stderr.log")),
                    steps: step_logs(&dir),
                });
            }
        }
        Ok(logs)
    }

    /// A graph's YAML exactly as written, resolved through the same
    /// project > user > built-in layers a run uses.
    ///
    /// Deliberately does not compile it: the reason to read a graph's source is
    /// usually that you are about to change it, or that it failed to compile.
    ///
    /// # Errors
    /// Fails if no graph resolves under `reference`.
    pub fn graph_source(&self, reference: &str) -> Result<String> {
        Ok(preset::resolve(reference, &self.root)?.source)
    }

    /// Registry name → the program each entry actually spawns.
    ///
    /// A graph names a *role*; which CLI that lands on comes from config and is
    /// the fact a reader of someone else's graph most wants — and the only way
    /// gotcha 19's role-shadows-worker trap is visible at all.
    #[must_use]
    pub fn worker_bindings(&self) -> BTreeMap<String, String> {
        self.workers
            .entries()
            .filter_map(|(name, worker)| Some((name.to_owned(), worker.program()?.to_owned())))
            .collect()
    }

    /// Probe every configured worker and check for usability — the preflight
    /// behind `hex doctor`.
    #[must_use]
    pub fn doctor(&self) -> DoctorReport {
        doctor::report(&self.workers, &self.config.checks)
    }

    /// Cancel a run, whether or not something is driving it.
    ///
    /// Two paths, one invariant (a single writer per journal): if nobody holds the
    /// run lock, the terminal event is appended here; if a live driver holds it,
    /// the cancel is queued in that run's control inbox and the driver records the
    /// terminal itself at the next attempt boundary.
    ///
    /// # Errors
    /// Fails if the run does not exist or cannot be appended to.
    pub fn cancel(&self, run_id: &str, actor: &Actor) -> Result<Cancellation> {
        let run_dir = self.run_dir(run_id)?;
        // Taking the same exclusive lock a driver holds proves no live process is
        // writing, and holding it makes the append atomic w.r.t. a concurrent
        // resume. `acquire` fails cleanly if the run is active.
        let Ok(lock) = RunLock::acquire(&run_dir) else {
            self.control(run_id, actor, &Command::Cancel)?;
            return Ok(Cancellation::Requested);
        };
        // A reserved run whose driver has not started yet has no journal to
        // append to, so the cancel waits in its inbox instead — the driver
        // consumes it at its very first boundary, before any attempt runs.
        if !run_dir.join("events.jsonl").exists() {
            drop(lock);
            self.control(run_id, actor, &Command::Cancel)?;
            return Ok(Cancellation::Requested);
        }
        let _lock = lock;
        // One scan: open the writer (repairs a torn tail, returns events), then
        // verify + fold those same events.
        let (mut journal, events) = Journal::open_append(run_dir.join("events.jsonl"))?;
        let (_, state) = self.verify_and_fold(run_id, &events)?;
        if state.is_finished() {
            return Ok(Cancellation::Recorded);
        }
        // Cancelling a *dead* run (Ctrl-C, crash) reaches here with an attempt
        // still open, and `RunFinished` on top of one is rejected by
        // `lifecycle` — which left the journal permanently unreadable, on the
        // very path an operator takes to clean up after Ctrl-C. Close the
        // orphan first, exactly as `resume` does.
        if state.awaiting() {
            journal.append(
                run_id,
                state.current.as_deref(),
                state.current_attempt.as_deref(),
                actor.clone(),
                EventBody::AttemptInterrupted,
            )?;
        }
        journal.append(
            run_id,
            None,
            None,
            actor.clone(),
            EventBody::RunFinished {
                disposition: Disposition::Cancelled,
            },
        )?;
        Ok(Cancellation::Recorded)
    }

    /// Queue an operator command in a run's control inbox, to be applied by
    /// whoever is (or will be) driving it. The command file is a transport; what
    /// happened is whatever the driver journals in response.
    ///
    /// # Errors
    /// Fails if the run does not exist, has already finished, or the inbox cannot
    /// be written.
    pub fn control(&self, run_id: &str, actor: &Actor, command: &Command) -> Result<()> {
        let run_dir = self.run_dir(run_id)?;
        // The *directory* is the existence test, not the journal: a detached run
        // is reserved a moment before its driver writes anything, and `hex steer`
        // one keystroke later must queue rather than claim the run is missing.
        if !run_dir.is_dir() {
            return Err(HexError::new(format!("run `{run_id}` not found")));
        }
        // Refuse up front rather than leaving a command nobody will ever read.
        // Cheap honesty: the projection comes from the journal either way.
        if let Ok(report) = self.status(run_id)
            && let Some(d) = report.disposition
        {
            return Err(HexError::new(format!(
                "run `{run_id}` already finished ({d}) — start a new run instead"
            )));
        }
        Inbox::new(&run_dir).send(actor, command)
    }

    /// Every run under the project, newest activity first.
    ///
    /// A run that cannot be replayed is still listed (with its error): the point
    /// of this verb is that a run id is findable, and hiding a broken run is how
    /// one gets lost.
    ///
    /// # Errors
    /// Fails only if `.hex/runs` exists but cannot be read.
    pub fn list_runs(&self) -> Result<Vec<RunSummary>> {
        let runs = self.runs_dir();
        let entries = match std::fs::read_dir(&runs) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries.filter_map(std::result::Result::ok) {
            let run_id = entry.file_name().to_string_lossy().into_owned();
            if validate_run_id(&run_id).is_err() || !entry.path().is_dir() {
                continue;
            }
            out.push(self.summarize(&run_id, &entry.path()));
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.updated_at_ms));
        Ok(out)
    }

    /// One run's summary, including whether a process is still driving it — what
    /// `hex wait` polls, and one row of `hex runs`.
    ///
    /// # Errors
    /// Fails if the run id is unsafe or the run does not exist.
    pub fn summary(&self, run_id: &str) -> Result<RunSummary> {
        let run_dir = self.run_dir(run_id)?;
        if !run_dir.is_dir() {
            return Err(HexError::new(format!("run `{run_id}` not found")));
        }
        Ok(self.summarize(run_id, &run_dir))
    }

    /// One run's listing row.
    fn summarize(&self, run_id: &str, run_dir: &Path) -> RunSummary {
        let events = journal::read_all(&run_dir.join("events.jsonl")).unwrap_or_default();
        let created_at_ms = events.first().map_or(0, |e| e.at_ms);
        let updated_at_ms = events.last().map_or(0, |e| e.at_ms);
        let (status, current, attempts, disposition, error) =
            match self.verify_and_fold(run_id, &events) {
                Ok((_, state)) => (
                    Some(state.status.clone()),
                    state.current.clone(),
                    state.attempts_total,
                    state.disposition(),
                    None,
                ),
                // A journal-less run is normal for a moment (a detached run between
                // reservation and its driver's first write), so say that rather than
                // reporting the generic replay failure.
                Err(_) if events.is_empty() => (
                    None,
                    None,
                    0,
                    None,
                    Some("no journal yet (reserved, or never started)".to_owned()),
                ),
                Err(e) => (None, None, 0, None, Some(e.to_string())),
            };
        RunSummary {
            run_id: run_id.to_owned(),
            liveness: liveness_of(run_dir, status.as_ref()),
            status,
            current,
            attempts,
            disposition,
            created_at_ms,
            updated_at_ms,
            error,
        }
    }
}

/// Classify a run's process from its lock and heartbeat.
///
/// The lock is the primary signal because the OS releases it when the holder
/// dies — a pidfile cannot promise that, and PID reuse is real. The heartbeat
/// only refines "a process is alive" into "and it is still ticking".
fn liveness_of(run_dir: &Path, status: Option<&Status>) -> Liveness {
    match status {
        Some(Status::Finished(_)) => return Liveness::Finished,
        Some(Status::Paused) => return Liveness::Paused,
        _ => {}
    }
    // Probing takes the lock for an instant; dropping it immediately is the whole
    // test ("could anyone else have it?").
    match try_lock_file(&run_dir.join("run.lock")) {
        Ok(Some(_free)) => Liveness::Abandoned,
        Ok(None) => match control::last_beat_ms(run_dir) {
            Some(beat) if journal::now_ms().saturating_sub(beat) <= control::HEARTBEAT_STALE_MS => {
                Liveness::Live
            }
            _ => Liveness::Hung,
        },
        // Unreadable lock: report the conservative answer rather than guessing
        // that nobody is driving (which would invite a second writer).
        Err(_) => Liveness::Hung,
    }
}

/// The run's creation record — hash, inputs, and effective defaults — read in a
/// single pass over the journal.
type RunCreation = (
    String,
    BTreeMap<String, String>,
    config::DefaultsSpec,
    BTreeMap<String, Vec<String>>,
);

fn run_created(events: &[Event]) -> Option<RunCreation> {
    events.iter().find_map(|e| match &e.body {
        EventBody::RunCreated {
            graph_hash,
            inputs,
            defaults,
            checks,
        } => Some((
            graph_hash.clone(),
            inputs.clone(),
            defaults_from_map(defaults),
            checks.clone(),
        )),
        _ => None,
    })
}

/// The project checks a compiled graph actually resolved, recorded at creation so
/// a resumed run re-executes the same argv even if `.hex/config.yaml` changed.
/// Every check a graph names is present: an undeclared one is refused at compile
/// time, so a run cannot exist with an unresolved check to record.
fn resolved_checks(graph: &Graph) -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    for node in graph.nodes.values() {
        if let hex_kernel::graph::NodeSpec::Command { steps, .. } = &node.spec {
            for step in steps {
                if let Some(name) = &step.check {
                    map.insert(name.clone(), step.argv.clone());
                }
            }
        }
    }
    map
}

/// Encode compile defaults as a stable string map for the RunCreated event.
fn defaults_to_map(defaults: &config::DefaultsSpec) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(role) = &defaults.role {
        map.insert("role".to_owned(), role.clone());
    }
    if let Some(context) = &defaults.context {
        map.insert("context".to_owned(), context.clone());
    }
    map
}

/// Decode compile defaults from the RunCreated event's map.
fn defaults_from_map(map: &BTreeMap<String, String>) -> config::DefaultsSpec {
    config::DefaultsSpec {
        // `worker` is the pre-roles spelling; still read so a run created by an
        // older hex resumes instead of losing its default.
        role: map.get("role").or_else(|| map.get("worker")).cloned(),
        context: map.get("context").cloned(),
    }
}

/// Validate an operator-supplied run id: ASCII alphanumerics, `-`, and `_` only.
/// Because `.` and `/` are disallowed, no `..` or path separator can appear, so
/// nothing can escape `.hex/runs`.
fn validate_run_id(run_id: &str) -> Result<()> {
    let ok = !run_id.is_empty()
        && run_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
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

/// A short (8-hex) uniqueness suffix appended after a workflow name.
fn short_uuid() -> String {
    random_suffix()[..8].to_owned()
}

/// Slugify a name for a run id: lowercase, non-alphanumeric runs collapse to a
/// single `-`, trimmed. `"Plan → Build"` → `"plan-build"`.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// Today's UTC date as `yyyy-MM-dd`, for the run-id prefix (dependency-free).
fn today_utc() -> String {
    let days = i64::try_from(journal::now_ms() / 1000 / 86_400).unwrap_or(0);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert days-since-Unix-epoch to a `(year, month, day)` civil date
/// (Howard Hinnant's algorithm — valid for the proleptic Gregorian calendar).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
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
        match try_lock_file(&run_dir.join("run.lock"))? {
            Some(file) => {
                let _ = (&file).write_all(format!("{}\n", std::process::id()).as_bytes());
                Ok(Self { _file: file })
            }
            None => Err(HexError::new(
                "run is already active (locked by a live process); refusing a concurrent writer",
            )),
        }
    }
}

/// Take the `fs4` advisory lock on `path`, creating it. `Ok(Some)` = held (keep
/// the handle alive for the duration of ownership; the OS releases it on
/// drop/crash), `Ok(None)` = another live process holds it. Shared by `RunLock`
/// and worktree-slot leasing.
pub(crate) fn try_lock_file(path: &Path) -> Result<Option<std::fs::File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    // Call the fs4 trait method by path: on a Rust >= 1.89 toolchain the inherent
    // `File::try_lock` (stabilized then) would otherwise shadow it, and our MSRV
    // (1.88) predates it, so only fs4 provides locking.
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(fs4::TryLockError::WouldBlock) => Ok(None),
        Err(fs4::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Read the per-step captures under one attempt directory, in declared order.
///
/// The driver names each step dir `<position>-<slug>`, so the position is
/// recovered by parsing the numeric prefix rather than sorting the names: `10-x`
/// sorts before `9-x` lexically, which would reorder the evidence of any node
/// with ten or more steps.
fn step_logs(attempt_dir: &Path) -> Vec<StepLog> {
    let Ok(entries) = std::fs::read_dir(attempt_dir) else {
        return Vec::new();
    };
    let mut steps: Vec<(u32, StepLog)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let label = e.file_name().to_string_lossy().into_owned();
            let position = label.split_once('-')?.0.parse().ok()?;
            Some((
                position,
                StepLog {
                    stdout: std::fs::read_to_string(e.path().join("stdout.log"))
                        .unwrap_or_default(),
                    stderr: std::fs::read_to_string(e.path().join("stderr.log"))
                        .unwrap_or_default(),
                    exit: std::fs::read_to_string(e.path().join("exit"))
                        .ok()
                        .map(|s| s.trim().to_owned()),
                    label,
                },
            ))
        })
        .collect();
    steps.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.cmp(&b.1.label)));
    steps.into_iter().map(|(_, s)| s).collect()
}

/// Every captured stream an attempt owns: its own two, then both of each
/// numbered step directory, ordered by declared position so `10-` follows `9-`.
fn attempt_streams(attempt_dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![
        attempt_dir.join("stdout.log"),
        attempt_dir.join("stderr.log"),
    ];
    let Ok(entries) = std::fs::read_dir(attempt_dir) else {
        return files;
    };
    let mut steps: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            Some((name.split_once('-')?.0.parse().ok()?, e.path()))
        })
        .collect();
    steps.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, dir) in steps {
        files.push(dir.join("stdout.log"));
        files.push(dir.join("stderr.log"));
    }
    files
}

/// A stream's name relative to its attempt (`stderr`, `3-test/stdout`) — enough
/// for a client to label a chunk without knowing where any of it lives.
fn stream_label(attempt_dir: &Path, path: &Path) -> String {
    path.strip_prefix(attempt_dir)
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .into_owned()
}

/// Exactly `len` bytes of `path` starting at `offset`, lossily decoded.
fn read_span(path: &Path, offset: u64, len: u64) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.take(len).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// The project root for a runtime: the current working directory.
///
/// # Errors
/// Fails if the current directory cannot be read.
pub fn project_root() -> Result<PathBuf> {
    std::env::current_dir().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_lowercase_kebab() {
        assert_eq!(slug("critique-loop"), "critique-loop");
        assert_eq!(slug("Plan → Build Review"), "plan-build-review");
        assert_eq!(slug("  weird__name!! "), "weird-name");
        assert_eq!(slug("---"), "");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1)); // Unix epoch
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 not a leap year
        assert_eq!(civil_from_days(20_454), (2026, 1, 1));
    }

    #[test]
    fn run_id_grammar_allows_readable_ids_and_rejects_traversal() {
        assert!(validate_run_id("2026-07-21-critique-loop").is_ok());
        assert!(validate_run_id("2026-07-21-tdd-2").is_ok());
        assert!(validate_run_id("../../etc/passwd").is_err());
        assert!(validate_run_id("a/b").is_err());
        assert!(validate_run_id("a.b").is_err());
        assert!(validate_run_id("").is_err());
    }

    /// Ten steps is where a lexical sort silently reorders the evidence, so that
    /// is the case worth pinning: `10-x` must follow `9-x`.
    #[test]
    fn step_logs_are_ordered_by_declared_position_not_by_name() {
        let dir = std::env::temp_dir().join(format!(
            "hex-steplogs-{}-{}",
            std::process::id(),
            test_support::unique()
        ));
        for (i, label) in ["9-clippy", "10-test", "1-fmt"].iter().enumerate() {
            let step = dir.join(label);
            std::fs::create_dir_all(&step).expect("mkdir");
            std::fs::write(step.join("stdout.log"), format!("out{i}")).expect("stdout");
        }
        // Not a step dir: it must not become a phantom step.
        std::fs::create_dir_all(dir.join("scratch")).expect("mkdir");

        let labels: Vec<String> = step_logs(&dir).iter().map(|s| s.label.clone()).collect();
        assert_eq!(labels, ["1-fmt", "9-clippy", "10-test"]);
        assert_eq!(step_logs(&dir)[1].stdout, "out0");
        std::fs::remove_dir_all(&dir).ok();
    }

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
