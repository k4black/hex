//! `hex-runtime` — the imperative shell around the pure kernel.
//!
//! Everything effectful lives here: the drive loop, effect execution, the
//! append-only JSONL journal, config + preset resolution, workspace layout,
//! and crash recovery. The kernel decides *what*; the runtime is the only
//! layer that *does*.
//!
//! Clients (today, the CLI) are thin peers over [`Runtime`]: they parse
//! arguments and render, and every fact they show is one this layer computed.
//! (No `RuntimeClient` trait: one implementation, no callers — deleted 2026-08-01.)

pub mod config;
pub mod control;
pub mod doctor;
pub mod driver;
pub mod error;
mod init;
pub mod interrupt;
pub mod journal;
pub mod loader;
pub mod local_log;
pub mod preset;
pub mod stats;
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
pub use hex_kernel::topology::{EdgeClass, Topology, Transition};
pub use hex_kernel::{Graph, Status, Totals, Usage};
pub use hex_proto::{Actor, Command, Disposition, Event, EventBody, ModelUsage};
pub use init::{InitReport, init};
pub use preset::{Entry as GraphEntry, Layer};
pub use workers::Workers;
pub use worktree::Isolation;

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
/// The resolved graph origin (`built-in:<name>` or a path), recorded so a
/// resumed or cancelled run's stats line keeps the original classification.
const ORIGIN: &str = "origin";

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

/// What `hex prune` removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PruneReport {
    /// Run ids whose directories were removed.
    pub removed: Vec<String>,
    /// Run ids deliberately kept (still live, or younger than the cutoff).
    pub kept: Vec<String>,
    /// Bytes reclaimed from run directories.
    pub bytes: u64,
}

/// One row of `hex runs`: enough to pick a run out of a list without opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// The run's id.
    pub run_id: String,
    /// The one runtime-facing state: live, interrupted, finished, or unreadable.
    /// It folds in what used to be three separate fields (`status`, `disposition`,
    /// `error`) because they all answered the same question.
    pub state: Liveness,
    /// Whether a `live` run is still ticking. The lock proves a *process* is
    /// alive; only a fresh heartbeat proves it is progressing. Meaningless for
    /// any state other than [`Liveness::Live`].
    pub hung: bool,
    /// The active node, if any.
    pub current: Option<String>,
    /// Attempts started so far.
    pub attempts: u32,
    /// When the run was created (Unix epoch ms).
    pub created_at_ms: u64,
    /// When the journal last grew (Unix epoch ms) — the run's true "age".
    pub updated_at_ms: u64,
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
    /// Why a finished run ended, when the journal says: the `Note` recorded just
    /// before `RunFinished` (a budget bound, an unmet acceptance) or the
    /// terminal `AttemptFailed`'s reason. `None` while the run is unfinished.
    pub why: Option<String>,
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

/// One attempt's captured streams, as an opaque handle — what
/// [`Runtime::read_streams`] reads, for a client (the live preview) that holds an
/// [`AttemptView`] rather than a run id. The paths inside are the runtime's
/// business.
#[derive(Debug, Clone, Default)]
pub struct AttemptStreams {
    dir: PathBuf,
}

impl AttemptStreams {
    pub(crate) fn of(run_dir: &Path, attempt_id: &str) -> Self {
        Self {
            dir: run_dir.join("attempts").join(attempt_id),
        }
    }

    /// Everything appended since `cursor`: the attempt's own stdout/stderr
    /// **and** both streams of every numbered step directory a `command` node
    /// writes, in declared position.
    ///
    /// # Errors
    /// Fails if a stream exists but cannot be read.
    pub fn read(&self, cursor: &mut StreamCursor) -> Result<Vec<StreamChunk>> {
        let mut chunks = Vec::new();
        for path in attempt_streams(&self.dir) {
            let label = stream_label(&self.dir, &path);
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
            let mut bytes = read_span(&path, *offset, len - *offset)?;
            // A read boundary can split a multi-byte char; hold its first bytes
            // back for the next read instead of decoding them as U+FFFD.
            bytes.truncate(bytes.len() - incomplete_utf8_tail(&bytes));
            *offset += bytes.len() as u64;
            if !bytes.is_empty() {
                chunks.push(StreamChunk {
                    label,
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                });
            }
        }
        Ok(chunks)
    }
}

/// How many trailing bytes of `buf` begin a UTF-8 sequence that is not complete
/// yet (0 when the buffer ends on a boundary).
fn incomplete_utf8_tail(buf: &[u8]) -> usize {
    for back in 1..=buf.len().min(3) {
        let b = buf[buf.len() - back];
        if b & 0xC0 == 0x80 {
            continue; // a continuation byte: keep looking for its lead
        }
        let need = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if back < need { back } else { 0 };
    }
    0
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
    sink: Option<Arc<dyn ProgressSink>>,
    /// Whether finished runs append to the user-global `~/.hex/stats.jsonl`.
    /// On for the real constructor, off for `with_workers`: the test suite runs
    /// thousands of mock attempts, and before this gate they all landed in the
    /// operator's own stats (found as `implement ×666` in a real `hex stats`).
    stats: bool,
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
            stats: true,
        })
    }

    /// Build a runtime with an explicit worker registry (used by tests to
    /// inject mocks instead of real agent CLIs). Never writes usage stats.
    #[must_use]
    pub fn with_workers(root: PathBuf, config: Config, workers: Workers) -> Self {
        Self {
            root,
            config,
            workers,
            sink: None,
            stats: false,
        }
    }

    /// Install a [`ProgressSink`] that observes each journaled event and the
    /// start/finish of every attempt during a `run`/`resume`, so a foreground
    /// caller can stream live progress and preview in-flight agent output.
    #[must_use]
    pub fn with_progress(mut self, sink: Arc<dyn ProgressSink>) -> Self {
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
        // Roles come from the CURRENT live config, by design: a live
        // run keeps the roles it compiled, and pause/stop + resume recompiles
        // with the updated `roles:` (worker, model, preamble). Role changes
        // cannot change the graph's shape.
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
        // Resolve the source exactly once, then compile that same text — no
        // second resolution that could observe a changed file (TOCTOU).
        let resolved = preset::resolve(reference, &self.root)?;
        let graph = self.prepare(&resolved.source, prompt)?;

        let (run_id, run_dir) = self.new_run(&graph.name, name)?;
        std::fs::create_dir_all(run_dir.join("attempts"))?;
        let _lock = RunLock::acquire(&run_dir)?;

        // Isolation: lease a git worktree (held for the run) if requested. Its
        // metadata rides in the RunCreated inputs so `resume` reattaches to it.
        let mut inputs: BTreeMap<String, String> = prompt
            .map(|p| BTreeMap::from([("prompt".to_owned(), p.to_owned())]))
            .unwrap_or_default();
        inputs.insert(ORIGIN.to_owned(), resolved.origin.clone());
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
            inputs.insert(WT_BASE_REF.to_owned(), leased.base_sha.clone());
            worktree_ctx = Some(driver::WorktreeCtx {
                branch: leased.branch.clone(),
                base_sha: leased.base_sha.clone(),
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

        let stats_inputs = inputs.clone();
        session.runtime_event(EventBody::RunCreated {
            graph_hash: hash,
            inputs,
            defaults: defaults_to_map(&self.config.defaults),
            checks: resolved_checks(&graph),
        })?;
        // A reclaimed slot discarded a prior run's uncommitted work — record
        // exactly what, in this run's authoritative journal.
        if let Some(report) = slot.as_ref().and_then(|s| s.reclaimed.as_deref())
            && !report.is_empty()
        {
            session.runtime_event(EventBody::Note {
                text: format!(
                    "worktree slot reclaimed; discarded a prior run's uncommitted changes:\n{report}"
                ),
            })?;
        }
        session.runtime_event(EventBody::RunStarted)?;

        let disposition = session.drive()?;
        self.record_stats(&graph, Some(&stats_inputs), disposition, session.state());
        drop(slot); // release the worktree lock only after the run finishes
        Ok(RunReport {
            run_id,
            origin: resolved.origin,
            disposition,
        })
    }

    /// Append the run's cross-repo stats line — at `RunFinished` only, so a
    /// paused run (`disposition` `None`) writes nothing and whichever resume
    /// finishes it writes the one line; visits are cumulative, so one finished
    /// run folds to one line. Origin and branch come from the `RunCreated`
    /// inputs, so a resumed or cancelled run keeps its original classification.
    /// Best-effort: telemetry must never fail a run.
    fn record_stats(
        &self,
        graph: &Graph,
        inputs: Option<&BTreeMap<String, String>>,
        disposition: Option<Disposition>,
        state: &State,
    ) {
        let Some(d) = disposition.filter(|_| self.stats) else {
            return;
        };
        let get = |key: &str| inputs.and_then(|i| i.get(key)).map(String::as_str);
        stats::record_run(
            &self.root,
            &graph.name,
            get(ORIGIN).unwrap_or("unknown"),
            get(WT_BRANCH),
            d,
            state,
        );
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
        worktree::ensure_gitignored(&self.root, &format!("{}/", worktree::WORKTREES_DIR))?;
        let base_sha = worktree::resolve_base(&self.root, base)?;
        let branch = format!("hex/{run_id}");
        let slot = worktree::lease(&self.root, &branch, &base_sha)?;
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
            let base_sha = inputs
                .and_then(|i| i.get(WT_BASE_REF))
                .cloned()
                .unwrap_or_default();
            _slot_lock = Some(worktree::reattach(
                &self.root,
                Path::new(slot_dir),
                &branch,
            )?);
            worktree_ctx = Some(driver::WorktreeCtx { branch, base_sha });
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
            session.runtime_event(EventBody::RunStarted)?;
        }

        // Paused by an operator: lift the pause before scheduling, so the journal
        // shows the suspension being ended rather than silently ignored.
        if matches!(session.state().status, Status::Paused) {
            session.runtime_event(EventBody::RunResumed)?;
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
        self.record_stats(&graph, inputs, disposition, session.state());
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
        // Look at the directory, not the journal: reporting "not found" for a run
        // whose first event has not landed yet sends an operator looking for a run
        // that exists. Only a missing *directory* means missing.
        if !run_dir.is_dir() {
            return Err(HexError::new(format!("run `{run_id}` not found")));
        }
        let events = journal::read_all(&run_dir.join("events.jsonl"))
            .map_err(|_| HexError::new(format!("run `{run_id}` has no journal yet")))?;
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
        // A run parked on a `human` node is *waiting on you*; surface the
        // question here rather than reporting a bare `running`.
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
            why: state.is_finished().then(|| terminal_why(&events)).flatten(),
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
        AttemptStreams::of(&self.run_dir(run_id)?, attempt_id).read(cursor)
    }

    /// Read every event of a run, unverified.
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
    /// the fact a reader of someone else's graph most wants — and the only place
    /// a role shadowing a same-named worker is visible at all.
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
        // Read the journal FIRST, before probing the lock. A finished run needs
        // no lock — the terminal event is already there — and the probe is the
        // flaky path: a just-released `fs4` lock intermittently still reads as
        // busy under load, which made `hex cancel` right after a run
        // ended queue a command instead of recording.
        let journal_path = run_dir.join("events.jsonl");
        if !journal_path.exists() {
            // No journal to append to (the writer died between creating the run
            // directory and its first event): the cancel waits in the inbox, where
            // a driver would consume it at its first boundary.
            self.control(run_id, actor, &Command::Cancel)?;
            return Ok(Cancellation::Requested);
        }
        let events = journal::read_all(&journal_path)?;
        // One fold answers both questions: is there anything left to do, and could
        // a driver be holding the lock?
        let folded = self.verify_and_fold(run_id, &events);
        if matches!(&folded, Ok((_, s)) if s.is_finished()) {
            return Ok(Cancellation::Recorded);
        }
        // Taking the same exclusive lock a driver holds proves no live process is
        // writing, and holding it makes the append atomic w.r.t. a concurrent
        // resume. A *paused* run has no driver left to hand the command to, so a
        // busy probe there is usually the fs4 flake — retry briefly
        // before believing it. If it stays busy, a real writer (a concurrent
        // `resume`) holds it: queue, never append beside another writer.
        let suspended = matches!(&folded, Ok((_, s)) if matches!(s.status, Status::Paused));
        let mut lock = RunLock::acquire(&run_dir);
        for _ in 0..4 {
            if lock.is_ok() || !suspended {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            lock = RunLock::acquire(&run_dir);
        }
        let _lock = match lock {
            Ok(lock) => lock,
            Err(_) => {
                self.control(run_id, actor, &Command::Cancel)?;
                return Ok(Cancellation::Requested);
            }
        };
        // One scan: open the writer (repairs a torn tail, returns events), then
        // verify + fold those same events.
        let (mut journal, events) = Journal::open_append(journal_path)?;
        let (graph, state) = self.verify_and_fold(run_id, &events)?;
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
        // This is the run's one RunFinished, so its stats line is written here.
        self.record_stats(
            &graph,
            run_created(&events)
                .map(|(_, inputs, _, _)| inputs)
                .as_ref(),
            Some(Disposition::Cancelled),
            &state,
        );
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
        // The *directory* is the existence test, not the journal: a run whose
        // first event has not landed yet still exists, and `hex steer` one
        // keystroke later must queue rather than claim the run is missing.
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
        let mut out: Vec<RunSummary> = self
            .run_dirs()?
            .iter()
            .map(|(run_id, dir)| self.summarize(run_id, dir))
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.updated_at_ms));
        Ok(out)
    }

    /// Every run directory under `.hex/runs`, as `(run id, path)`. A missing
    /// `runs` dir is empty, not an error; an entry whose name is not a legal run
    /// id is not ours. Order is the filesystem's — callers sort.
    fn run_dirs(&self) -> Result<Vec<(String, PathBuf)>> {
        let entries = match std::fs::read_dir(self.runs_dir()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(entries
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let run_id = entry.file_name().to_string_lossy().into_owned();
                let dir = entry.path();
                (validate_run_id(&run_id).is_ok() && dir.is_dir()).then_some((run_id, dir))
            })
            .collect())
    }

    /// Remove finished and interrupted run directories older than `older_than`,
    /// releasing any worktree slot their lease names. A `live` run is never
    /// touched; `all` additionally removes runs whose journal is unreadable.
    /// `older_than = None` means no age filter.
    ///
    /// # Errors
    /// Fails only if `.hex/runs` exists but cannot be read or a directory cannot
    /// be removed.
    pub fn prune(&self, older_than: Option<std::time::Duration>, all: bool) -> Result<PruneReport> {
        let now_ms = journal::now_ms();
        let max_age_ms = older_than.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let mut report = PruneReport::default();
        for (run_id, dir) in self.run_dirs()? {
            let summary = self.summarize(&run_id, &dir);
            let removable = match &summary.state {
                Liveness::Finished(_) | Liveness::Interrupted => true,
                Liveness::Error(_) => all,
                Liveness::Live => false,
            };
            let too_young =
                max_age_ms.is_some_and(|max| now_ms.saturating_sub(summary.updated_at_ms) < max);
            if !removable || too_young {
                report.kept.push(run_id);
                continue;
            }
            // Hold the run lock through deletion: a `resume` that started after
            // the liveness probe above would otherwise be writing a journal we
            // are removing. Also the only check an unreadable (`Error`) run gets.
            let Ok(_lock) = RunLock::acquire(&dir) else {
                report.kept.push(run_id);
                continue;
            };
            // Release the slot the run leased — only if it is still on this
            // run's branch and no live run holds it.
            if let Ok(events) = self.events(&run_id)
                && let Some((_, inputs, _, _)) = run_created(&events)
                && let Some(slot) = inputs.get(WT_SLOT)
                && let Some(branch) = inputs.get(WT_BRANCH)
            {
                let _ = worktree::release_slot(&self.root, Path::new(slot), branch);
            }
            report.bytes += dir_size(&dir);
            std::fs::remove_dir_all(&dir)?;
            report.removed.push(run_id);
        }
        Ok(report)
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
        // An unreadable journal is a *state*, not a missing field: a listing still
        // shows the run (hiding it is how a run gets lost) and `hex status` prints
        // why. A run directory with no journal at all — the writer died between
        // creating the directory and appending its first event — is the same case
        // with a plainer message.
        match self.verify_and_fold(run_id, &events) {
            Ok((_, kernel)) => {
                let (state, hung) = liveness_of(run_dir, &kernel.status);
                RunSummary {
                    run_id: run_id.to_owned(),
                    state,
                    hung,
                    current: kernel.current.clone(),
                    attempts: kernel.attempts_total,
                    created_at_ms,
                    updated_at_ms,
                }
            }
            Err(e) => RunSummary {
                run_id: run_id.to_owned(),
                state: Liveness::Error(if events.is_empty() {
                    "no journal yet (a run directory with no events)".to_owned()
                } else {
                    e.to_string()
                }),
                hung: false,
                current: None,
                attempts: 0,
                created_at_ms,
                updated_at_ms,
            },
        }
    }
}

/// Classify a run from its journal, its lock and its beacon.
///
/// The lock is the primary signal because the OS releases it when the holder
/// dies — a pidfile cannot promise that, and PID reuse is real. The heartbeat
/// only refines "a process is alive" into "and it is still ticking", which is
/// why `hung` is a diagnostic beside [`Liveness::Live`] rather than a state of
/// its own: the operator action is identical either way.
///
/// A paused run needs no special case: `hex pause` returns the driver, so the lock
/// is free and the run is `Interrupted` by the same rule that catches a crash.
fn liveness_of(run_dir: &Path, status: &Status) -> (Liveness, bool) {
    if let Status::Finished(d) = status {
        return (Liveness::Finished(*d), false);
    }
    // Probing takes the lock for an instant; dropping it immediately is the whole
    // test ("could anyone else have it?").
    match try_lock_file(&run_dir.join("run.lock")) {
        // Nobody holds it and the run is unfinished: paused, Ctrl-C'd, or crashed.
        Ok(Some(_free)) => (Liveness::Interrupted, false),
        // A process holds it — live, whether or not it is still ticking.
        Ok(None) => {
            let fresh = control::last_beat_ms(run_dir).is_some_and(|beat| {
                journal::now_ms().saturating_sub(beat) <= control::HEARTBEAT_STALE_MS
            });
            (Liveness::Live, !fresh)
        }
        // Unreadable lock: report the conservative answer rather than guessing
        // that nobody is driving (which would invite a second writer).
        Err(_) => (Liveness::Live, true),
    }
}

/// Why a run ended: the terminal cluster's `Note` (what `RecordTerminal` and
/// `finish` journal just before `RunFinished`) or `AttemptFailed` reason.
///
/// Only the *terminal cluster* counts. Scanning the whole journal for the last
/// note instead would surface a stale one: a run whose check failed on round one
/// and passed on round two would end `succeeded` while reporting "1 of 3 steps
/// failed" as its reason.
fn terminal_why(events: &[Event]) -> Option<String> {
    events
        .iter()
        .rev()
        .take_while(|e| {
            matches!(
                e.body,
                EventBody::Note { .. }
                    | EventBody::RunFinished { .. }
                    | EventBody::AttemptFailed { .. }
            )
        })
        .find_map(|e| match &e.body {
            EventBody::Note { text } => Some(text.clone()),
            EventBody::AttemptFailed { reason, .. } => Some(reason.clone()),
            _ => None,
        })
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

/// Today's UTC date as `yyyy-MM-dd`, for the run-id prefix. UTC, not local: the
/// prefix is a sort key, and a machine that changes timezone must not reorder
/// its runs.
fn today_utc() -> String {
    let ms = i64::try_from(journal::now_ms()).unwrap_or(i64::MAX);
    jiff::Timestamp::from_millisecond(ms)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .strftime("%Y-%m-%d")
        .to_string()
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
    step_dirs(attempt_dir)
        .into_iter()
        .map(|dir| StepLog {
            stdout: std::fs::read_to_string(dir.join("stdout.log")).unwrap_or_default(),
            stderr: std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default(),
            exit: std::fs::read_to_string(dir.join("exit"))
                .ok()
                .map(|s| s.trim().to_owned()),
            label: dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        })
        .collect()
}

/// The numbered step directories under one attempt, in declared order. See
/// [`step_logs`] for why the numeric prefix is parsed rather than sorted on.
fn step_dirs(attempt_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(attempt_dir) else {
        return Vec::new();
    };
    let mut steps: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let position = e
                .file_name()
                .to_string_lossy()
                .split_once('-')?
                .0
                .parse()
                .ok()?;
            Some((position, e.path()))
        })
        .collect();
    steps.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    steps.into_iter().map(|(_, dir)| dir).collect()
}

/// Every captured stream an attempt owns: its own two, then both of each
/// numbered step directory, ordered by declared position so `10-` follows `9-`.
fn attempt_streams(attempt_dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![
        attempt_dir.join("stdout.log"),
        attempt_dir.join("stderr.log"),
    ];
    for dir in step_dirs(attempt_dir) {
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

/// Exactly `len` bytes of `path` starting at `offset`.
fn read_span(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.take(len).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Total bytes under `dir`, best-effort (an unreadable entry counts as 0).
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.is_dir() {
            total += dir_size(&path);
        } else {
            total += meta.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read that lands mid-codepoint holds the lead bytes back, so a follower
    /// never prints U+FFFD for a char that was merely split across two polls.
    #[test]
    fn a_split_multibyte_char_is_held_for_the_next_read() {
        let dir = tempfile::tempdir().unwrap();
        let streams = AttemptStreams::of(dir.path(), "a1");
        std::fs::create_dir_all(&streams.dir).unwrap();
        let log = streams.dir.join("stdout.log");
        let mut cursor = StreamCursor::default();
        // 'é' is 0xC3 0xA9; the first read sees only its lead byte.
        std::fs::write(&log, [b'x', 0xC3]).unwrap();
        let first = streams.read(&mut cursor).unwrap();
        assert_eq!(first[0].text, "x");
        std::fs::write(&log, [b'x', 0xC3, 0xA9, b'\n']).unwrap();
        let second = streams.read(&mut cursor).unwrap();
        assert_eq!(second[0].text, "é\n");
    }

    #[test]
    fn slug_is_lowercase_kebab() {
        assert_eq!(slug("critique-loop"), "critique-loop");
        assert_eq!(slug("Plan → Build Review"), "plan-build-review");
        assert_eq!(slug("  weird__name!! "), "weird-name");
        assert_eq!(slug("---"), "");
    }

    /// The run-id prefix is a path segment and a sort key, so its shape matters
    /// more than today's value.
    #[test]
    fn today_utc_is_a_sortable_iso_date() {
        let today = today_utc();
        assert_eq!(today.len(), 10, "{today}");
        assert!(validate_run_id(&today).is_ok(), "{today}");
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

    /// Old journals may carry defaults keys that no longer exist (a run created
    /// before the run-wide retry budgets were deleted recorded `attempts`). The
    /// map is read by name, so an unknown key is ignored rather than rejecting
    /// the journal — a resumed run must stay readable.
    #[test]
    fn an_unknown_recorded_default_is_ignored_on_read() {
        let mut map = BTreeMap::new();
        map.insert("role".to_owned(), "reviewer".to_owned());
        map.insert("attempts".to_owned(), "8".to_owned());
        map.insert("cycle_visits".to_owned(), "4".to_owned());
        let defaults = defaults_from_map(&map);
        assert_eq!(defaults.role.as_deref(), Some("reviewer"));
        assert_eq!(defaults.context, None);
    }

    /// Ten steps is where a lexical sort silently reorders the evidence, so that
    /// is the case worth pinning: `10-x` must follow `9-x`.
    #[test]
    fn step_logs_are_ordered_by_declared_position_not_by_name() {
        let dir = test_support::temp_dir("steplogs");
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
        let dir = test_support::temp_dir("runlock");

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
