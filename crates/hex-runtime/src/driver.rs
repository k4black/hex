//! The drive loop — the imperative shell around the pure kernel.
//!
//! Per iteration: ask [`hex_kernel::schedule`] for the next effect, journal the
//! intent *before* performing it (idempotency key), execute it via a worker or
//! a gate process, journal the result, fold it through [`hex_kernel::reduce`],
//! and repeat until the kernel schedules nothing (terminal or blocked).

use std::path::{Path, PathBuf};
use std::process::{Command as ProcCommand, Stdio};

use hex_kernel::graph::NodeSpec;
use hex_kernel::{Effect, Graph, RunState, reduce, schedule};
use hex_proto::{Actor, Disposition, EventBody};
use hex_worker::WorkRequest;

use crate::error::{HexError, Result};
use crate::journal::{Journal, now_ms};
use crate::workers::Workers;

/// One run's mutable execution context: the graph, its workers, the journal,
/// and the projected state folded from it.
pub struct Session<'a> {
    graph: &'a Graph,
    workers: &'a Workers,
    run_id: String,
    run_dir: PathBuf,
    workdir: PathBuf,
    journal: Journal,
    state: RunState,
}

impl<'a> Session<'a> {
    /// Build a session over an already-open journal and replayed state.
    pub fn new(
        graph: &'a Graph,
        workers: &'a Workers,
        run_id: String,
        run_dir: PathBuf,
        workdir: PathBuf,
        journal: Journal,
        state: RunState,
    ) -> Self {
        Self {
            graph,
            workers,
            run_id,
            run_dir,
            workdir,
            journal,
            state,
        }
    }

    /// The current projected state.
    #[must_use]
    pub fn state(&self) -> &RunState {
        &self.state
    }

    /// Journal one event and fold it into the projection.
    pub fn record(
        &mut self,
        node_id: Option<&str>,
        attempt_id: Option<&str>,
        actor: Actor,
        body: EventBody,
    ) -> Result<()> {
        let event = self
            .journal
            .append(&self.run_id, node_id, attempt_id, actor, body)?;
        self.state = reduce(self.graph, std::mem::take(&mut self.state), &event);
        Ok(())
    }

    /// Drive the loop to a terminal disposition. Guarantees that whenever it
    /// returns, the journal contains a matching terminal event — a disposition
    /// is never returned that the journal does not record.
    pub fn drive(&mut self) -> Result<Disposition> {
        // A generous ceiling so a pathological graph can never spin forever
        // even if a budget was mis-declared; budgets normally stop it first.
        for _ in 0..100_000 {
            let effects = schedule(self.graph, &self.state, now_ms());
            let Some(effect) = effects.into_iter().next() else {
                break;
            };
            self.execute(effect)?;
            if self.state.is_finished() {
                break;
            }
        }
        if !self.state.is_finished() {
            // Blocked (e.g. a human node) or hit the iteration ceiling: record
            // the halt so the outcome is always in the journal.
            self.record(
                None,
                None,
                Actor::runtime(),
                EventBody::Note {
                    text: "run halted without reaching a terminal node".to_owned(),
                },
            )?;
            self.record(
                None,
                None,
                Actor::runtime(),
                EventBody::RunFinished {
                    disposition: Disposition::Failed,
                },
            )?;
        }
        Ok(self.state.disposition().unwrap_or(Disposition::Failed))
    }

    /// Remaining time budget for the current attempt, if the run declares one.
    fn attempt_deadline(&self) -> Option<u64> {
        let budget = self.graph.budget.elapsed_ms?;
        let elapsed = now_ms().saturating_sub(self.state.started_at_ms);
        Some(budget.saturating_sub(elapsed))
    }

    fn execute(&mut self, effect: Effect) -> Result<()> {
        match effect {
            Effect::StartAttempt {
                node_id,
                attempt_id,
                idempotency_key,
            } => self.start_attempt(&node_id, &attempt_id, &idempotency_key),
            Effect::RunGate {
                node_id,
                attempt_id,
                idempotency_key,
            } => self.run_gate(&node_id, &attempt_id, &idempotency_key),
            Effect::RecordTerminal { disposition } => {
                self.record(None, None, Actor::runtime(), EventBody::RunFinished { disposition })
            }
            Effect::RequestHuman { node_id } => {
                // The slim MVP has no human transport; fail closed, legibly.
                self.record(
                    Some(&node_id),
                    None,
                    Actor::runtime(),
                    EventBody::Note {
                        text: "human node not supported in the slim MVP".to_owned(),
                    },
                )?;
                self.record(
                    None,
                    None,
                    Actor::runtime(),
                    EventBody::RunFinished {
                        disposition: Disposition::Failed,
                    },
                )
            }
        }
    }

    fn start_attempt(&mut self, node_id: &str, attempt_id: &str, idk: &str) -> Result<()> {
        let Some(node) = self.graph.node(node_id) else {
            return self.fail_attempt(node_id, attempt_id, "unknown node");
        };
        let NodeSpec::Agent {
            worker,
            prompt,
            may_propose,
            ..
        } = &node.spec
        else {
            return self.fail_attempt(node_id, attempt_id, "not an agent node");
        };
        let worker_name = worker.clone();

        // intent-before-effect: the attempt is on the record before it runs.
        self.record(
            Some(node_id),
            Some(attempt_id),
            Actor::runtime(),
            EventBody::AttemptStarted {
                idempotency_key: idk.to_owned(),
                worker: Some(worker_name.clone()),
            },
        )?;

        let Some(adapter) = self.workers.get(&worker_name) else {
            return self.fail_attempt(node_id, attempt_id, &format!("unknown worker `{worker_name}`"));
        };

        let attempt_dir = self.attempt_dir(attempt_id)?;
        let request = WorkRequest {
            run_id: self.run_id.clone(),
            node_id: node_id.to_owned(),
            attempt_id: attempt_id.to_owned(),
            prompt: prompt.clone(),
            may_propose: may_propose.clone(),
            workdir: self.workdir.clone(),
            attempt_dir,
            deadline_ms: self.attempt_deadline(),
        };
        let outcome = adapter.run(&request);

        match outcome.signal {
            Some(signal) if may_propose.contains(&signal) => self.record(
                Some(node_id),
                Some(attempt_id),
                Actor::agent(worker_name),
                EventBody::Signal { name: signal },
            ),
            Some(signal) => {
                self.fail_attempt(node_id, attempt_id, &format!("emitted disallowed `{signal}`"))
            }
            None => {
                let reason = outcome.error.unwrap_or_else(|| "no signal".to_owned());
                self.fail_attempt(node_id, attempt_id, &reason)
            }
        }
    }

    fn run_gate(&mut self, node_id: &str, attempt_id: &str, idk: &str) -> Result<()> {
        let Some(node) = self.graph.node(node_id) else {
            return self.fail_attempt(node_id, attempt_id, "unknown node");
        };
        let command = match &node.spec {
            NodeSpec::Gate { command } | NodeSpec::Command { command } => command.clone(),
            _ => return self.fail_attempt(node_id, attempt_id, "not a gate/command node"),
        };

        self.record(
            Some(node_id),
            Some(attempt_id),
            Actor::runtime(),
            EventBody::AttemptStarted {
                idempotency_key: idk.to_owned(),
                worker: None,
            },
        )?;

        let attempt_dir = self.attempt_dir(attempt_id)?;
        // An infrastructure failure (spawn/log/timeout) is NOT a `failed`
        // verdict — routing it as `failed` would feed a token-spending loop on
        // false evidence. Only a real exit status yields passed/failed.
        match run_process(&command, &self.workdir, &attempt_dir, self.attempt_deadline()) {
            Ok(passed) => {
                let signal = if passed { "passed" } else { "failed" };
                self.record(
                    Some(node_id),
                    Some(attempt_id),
                    Actor::runtime(),
                    EventBody::Signal {
                        name: signal.to_owned(),
                    },
                )
            }
            Err(reason) => self.fail_attempt(node_id, attempt_id, &reason),
        }
    }

    fn fail_attempt(&mut self, node_id: &str, attempt_id: &str, reason: &str) -> Result<()> {
        self.record(
            Some(node_id),
            Some(attempt_id),
            Actor::runtime(),
            EventBody::AttemptFailed {
                reason: reason.to_owned(),
            },
        )
    }

    fn attempt_dir(&self, attempt_id: &str) -> Result<PathBuf> {
        let dir = self.run_dir.join("attempts").join(attempt_id);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// Run a gate/command argv, capturing output to the attempt dir.
/// `Ok(true/false)` is a genuine pass/fail verdict from the process exit status;
/// `Err(_)` is an infrastructure failure (bad command, spawn/log error, or
/// deadline kill) that must not be treated as a `failed` verdict.
fn run_process(
    command: &[String],
    workdir: &Path,
    attempt_dir: &Path,
    deadline_ms: Option<u64>,
) -> std::result::Result<bool, String> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "gate/command has an empty argv".to_owned())?;
    let stdout = std::fs::File::create(attempt_dir.join("stdout.log"))
        .map_err(|e| format!("cannot open gate stdout log: {e}"))?;
    let stderr = std::fs::File::create(attempt_dir.join("stderr.log"))
        .map_err(|e| format!("cannot open gate stderr log: {e}"))?;
    let mut child = ProcCommand::new(program)
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("spawn `{program}` failed: {e}"))?;
    match hex_worker::wait_bounded(&mut child, deadline_ms) {
        Ok(Some(status)) => Ok(status.success()),
        Ok(None) => Err("gate exceeded its time budget (killed)".to_owned()),
        Err(e) => Err(format!("gate wait failed: {e}")),
    }
}

/// SHA-256 of the graph source, hex-encoded — the exact-snapshot identity a run
/// records so later edits to the source never change what already ran.
#[must_use]
pub fn graph_hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(source.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Ensure every agent node references a worker present in the registry.
///
/// # Errors
/// Returns the first missing worker reference.
pub fn check_workers(graph: &Graph, workers: &Workers) -> Result<()> {
    for node in graph.nodes.values() {
        if let NodeSpec::Agent { worker, .. } = &node.spec
            && workers.get(worker).is_none()
        {
            return Err(HexError::new(format!(
                "node `{}` references unknown worker `{worker}`",
                node.id
            )));
        }
    }
    Ok(())
}
