//! `hex-kernel` — the pure, deterministic kernel of hex.
//!
//! Owns the compiled graph IR (nodes, edges, bounded cycles), the projected
//! run state, IR validation, and the three pure functions the runtime drives:
//! [`reduce`], [`schedule`], and [`accept`]. Depends only on [`hex_proto`].
//!
//! The kernel is **pure**: no IO, no clock, no subprocesses, no worker
//! adapters, no rendering. It never performs external actions — it emits
//! [`Effect`] *intents* that only the runtime executes (intent-before-effect,
//! with idempotency keys). This keeps routing and completion logic testable
//! without any model or subprocess, and routing spends no tokens.
//!
//! The graph *surface syntax* (standard YAML) is parsed elsewhere (the
//! runtime); this crate models the compiled IR only.

use std::collections::BTreeMap;

use hex_proto::{Disposition, Event, EventBody};

pub mod graph;
pub mod template;
pub mod validate;

pub use graph::{Budget, Context, Edge, Graph, Node, NodeKind, NodeSpec, Requirement};
pub use validate::{Issue, check_journal, validate};

/// Status of a run, derived purely by folding [`reduce`] over the journal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// Created but scheduling has not started.
    #[default]
    Created,
    /// Actively scheduling attempts.
    Running,
    /// Scheduling suspended by an operator.
    Paused,
    /// Reached a terminal disposition.
    Finished(Disposition),
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Created => f.write_str("created"),
            Status::Running => f.write_str("running"),
            Status::Paused => f.write_str("paused"),
            Status::Finished(d) => write!(f, "finished:{d}"),
        }
    }
}

/// Projected state of one run: `state = fold(reduce, journal)`. Never
/// persisted as authority — always rebuildable from the journal.
#[derive(Debug, Clone, Default)]
pub struct RunState {
    /// Where the run is in its lifecycle.
    pub status: Status,
    /// The node currently active (about to run, or in-flight).
    pub current: Option<String>,
    /// The id of the in-flight attempt, if any. `Some` exactly when an attempt
    /// is running — so it doubles as the "awaiting" flag (see
    /// [`RunState::awaiting`]) and drives correlation + resume.
    pub current_attempt: Option<String>,
    /// Total attempts started across the whole run.
    pub attempts_total: u32,
    /// Times each node has been entered (cycle-visit accounting).
    pub visits: BTreeMap<String, u32>,
    /// The last routing signal each node produced (drives acceptance).
    pub signals: BTreeMap<String, String>,
    /// The last captured result text each node produced, for `{{node.result}}`
    /// handoff into a downstream node's prompt.
    pub results: BTreeMap<String, String>,
    /// When scheduling started (Unix epoch ms), for elapsed budgets.
    pub started_at_ms: u64,
}

impl RunState {
    /// The terminal disposition, if the run has finished.
    #[must_use]
    pub fn disposition(&self) -> Option<Disposition> {
        match self.status {
            Status::Finished(d) => Some(d),
            _ => None,
        }
    }

    /// Whether the run has reached any terminal state.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self.status, Status::Finished(_))
    }

    /// Whether an attempt is in flight for [`RunState::current`] — i.e. an
    /// attempt has started and has not yet produced a terminal event.
    #[must_use]
    pub fn awaiting(&self) -> bool {
        self.current_attempt.is_some()
    }
}

/// An *intent* describing one external action the kernel wants performed. The
/// kernel emits it; only the runtime executes it, writing intent-before-effect
/// with idempotency keys. Effects are lean (ids only) — the runtime already
/// holds the graph and reads the node's prompt/command/worker from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Start one attempt of an agent node via its worker.
    StartAttempt {
        /// Node to run.
        node_id: String,
        /// Fresh attempt id.
        attempt_id: String,
        /// Key deduplicating the attempt across restarts.
        idempotency_key: String,
    },
    /// Run a deterministic gate/command node.
    RunGate {
        /// Node to run.
        node_id: String,
        /// Fresh attempt id.
        attempt_id: String,
        /// Key deduplicating the attempt across restarts.
        idempotency_key: String,
    },
    /// Suspend and request a human decision or input (not implemented in the
    /// slim MVP — the critique loop uses no human node).
    RequestHuman {
        /// Node requesting the human.
        node_id: String,
    },
    /// Record the run's terminal disposition.
    RecordTerminal {
        /// The final outcome.
        disposition: Disposition,
    },
}

/// Whether the acceptance contract (`accept.require`) is satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acceptance {
    /// All required signals observed.
    Accepted,
    /// Required `node.signal` evidence still missing.
    Missing(Vec<String>),
}

/// Fold one journal event into the projected run state:
/// `new_state = reduce(graph, old_state, event)`. Pure and deterministic; the
/// single source of every state transition, **including routing**.
#[must_use]
pub fn reduce(graph: &Graph, mut state: RunState, event: &Event) -> RunState {
    match &event.body {
        EventBody::RunCreated { .. } => {
            state.status = Status::Created;
        }
        EventBody::RunStarted => {
            state.status = Status::Running;
            state.current = Some(graph.entry.clone());
            *state.visits.entry(graph.entry.clone()).or_insert(0) += 1;
            state.started_at_ms = event.at_ms;
        }
        EventBody::AttemptStarted { .. } => {
            // Fail closed on an attempt-start that does not target the projected
            // current node, or that carries no attempt id: ignore it rather than
            // marking the wrong (or an anonymous) attempt in flight.
            if state.current.is_none()
                || event.node_id != state.current
                || event.attempt_id.is_none()
            {
                return state;
            }
            state.attempts_total += 1;
            state.current_attempt = event.attempt_id.clone();
            // A fresh attempt starts with no result: clear any prior one for this
            // node so a re-visit that captures nothing can't hand downstream the
            // previous attempt's stale text.
            if let Some(cur) = &state.current {
                state.results.remove(cur);
            }
        }
        EventBody::AttemptInterrupted => {
            // Orphaned attempt: clear the in-flight attempt so the same node is
            // re-scheduled fresh. Correlated like other attempt outcomes so a
            // forged interruption cannot desync the projection.
            if !correlated(&state, event) {
                return state;
            }
            state.current_attempt = None;
            // Drop any result the interrupted attempt captured before crashing,
            // so a re-attempt that produces none can't hand downstream stale text.
            if let Some(cur) = &state.current {
                state.results.remove(cur);
            }
        }
        EventBody::Signal { name } => {
            // Only a signal correlated to the in-flight attempt advances the
            // graph. This guards replay of a corrupt/forged journal record
            // whose node/attempt does not match the projected position.
            if !correlated(&state, event) {
                return state;
            }
            state.current_attempt = None;
            if let Some(cur) = state.current.clone() {
                state.signals.insert(cur.clone(), name.clone());
                match graph.route(&cur, name) {
                    Some(to) => {
                        state.current = Some(to.to_owned());
                        *state.visits.entry(to.to_owned()).or_insert(0) += 1;
                    }
                    None => {
                        // No legal edge for this signal — a defensive failure
                        // (validation guarantees `may_propose` events route).
                        state.status = Status::Finished(Disposition::Failed);
                    }
                }
            }
        }
        EventBody::AttemptFailed { disposition, .. } => {
            if !correlated(&state, event) {
                return state;
            }
            state.current_attempt = None;
            // Terminal in one atomic event: the failure and its disposition are
            // recorded together, so a crash can never leave a failed attempt
            // looking re-runnable. Fail closed: only a genuine failure
            // disposition is honored — anything else (e.g. a forged `Succeeded`)
            // collapses to `Failed`, never bypassing the acceptance contract.
            let disposition = match disposition {
                Disposition::Failed | Disposition::TimedOut => *disposition,
                _ => Disposition::Failed,
            };
            state.status = Status::Finished(disposition);
        }
        EventBody::NodeResult { text } => {
            // Recorded while the attempt is still in flight (before its signal),
            // so it must correlate to the current attempt like other outcomes.
            if !correlated(&state, event) {
                return state;
            }
            if let Some(cur) = state.current.clone() {
                state.results.insert(cur, text.clone());
            }
        }
        EventBody::RunFinished { disposition } => {
            state.status = Status::Finished(*disposition);
        }
        EventBody::BudgetExhausted { .. } | EventBody::Note { .. } => {}
    }
    state
}

/// Derive the next [`Effect`] intents from `(graph, state)` at time `now_ms`.
/// Deterministic. Returns at most one effect in the slim sequential MVP.
///
/// `now_ms` is injected so the kernel stays clock-free and testable.
#[must_use]
pub fn schedule(graph: &Graph, state: &RunState, now_ms: u64) -> Vec<Effect> {
    if state.status != Status::Running || state.awaiting() {
        return Vec::new();
    }
    let Some(cur) = state.current.clone() else {
        return Vec::new();
    };
    let Some(node) = graph.node(&cur) else {
        return vec![terminal(Disposition::Failed)];
    };

    // Terminal nodes settle the run *before* budget checks: reaching an outcome
    // does not spend an attempt, so a success that coincides with the last
    // attempt must not be flipped to `budget_exhausted`.
    if let NodeSpec::Terminal { disposition } = &node.spec {
        // A success terminal only succeeds if the acceptance contract holds.
        if *disposition == Disposition::Succeeded
            && !matches!(accept(graph, state), Acceptance::Accepted)
        {
            return vec![terminal(Disposition::Failed)];
        }
        return vec![terminal(*disposition)];
    }

    // Budgets are checked before spending an attempt, and fail closed.
    if let Some(max) = graph.budget.attempts
        && state.attempts_total >= max
    {
        return vec![terminal(Disposition::BudgetExhausted)];
    }
    if let Some(maxv) = graph.budget.cycle_visits
        && state.visits.get(&cur).copied().unwrap_or(0) > maxv
    {
        return vec![terminal(Disposition::BudgetExhausted)];
    }
    if let Some(maxe) = graph.budget.elapsed_ms {
        // Fail closed on the time budget, and treat a backward clock (now before
        // the recorded start) conservatively as exhausted rather than granting a
        // fresh budget.
        let over = now_ms < state.started_at_ms || now_ms - state.started_at_ms >= maxe;
        if over {
            return vec![terminal(Disposition::TimedOut)];
        }
    }

    let attempt_id = format!("att_{}", state.attempts_total + 1);
    let idempotency_key = format!("{cur}#{}", state.attempts_total + 1);
    match &node.spec {
        NodeSpec::Terminal { .. } => unreachable!("terminal handled above"),
        NodeSpec::Agent { .. } => vec![Effect::StartAttempt {
            node_id: cur,
            attempt_id,
            idempotency_key,
        }],
        NodeSpec::Gate { .. } | NodeSpec::Command { .. } => vec![Effect::RunGate {
            node_id: cur,
            attempt_id,
            idempotency_key,
        }],
        NodeSpec::Human { .. } => vec![Effect::RequestHuman { node_id: cur }],
    }
}

/// Decide whether the run's acceptance contract (`accept.require`) is met.
/// Deterministic evidence outranks any worker's "done" claim.
#[must_use]
pub fn accept(graph: &Graph, state: &RunState) -> Acceptance {
    let mut missing = Vec::new();
    for req in &graph.accept {
        if state.signals.get(&req.node).map(String::as_str) != Some(req.signal.as_str()) {
            missing.push(format!("{}.{}", req.node, req.signal));
        }
    }
    if missing.is_empty() {
        Acceptance::Accepted
    } else {
        Acceptance::Missing(missing)
    }
}

fn terminal(disposition: Disposition) -> Effect {
    Effect::RecordTerminal { disposition }
}

/// Whether `event` refers to the currently in-flight node + attempt. Requires
/// an attempt to actually be in flight — a `Signal`/`AttemptFailed` arriving
/// while nothing is awaiting is spurious and must not alter the projection.
fn correlated(state: &RunState, event: &Event) -> bool {
    state.current_attempt.is_some()
        && event.node_id == state.current
        && event.attempt_id == state.current_attempt
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_proto::{Actor, PROTOCOL_VERSION};

    fn ev(seq: u64, body: EventBody) -> Event {
        Event {
            schema_version: PROTOCOL_VERSION,
            seq,
            at_ms: seq * 1000,
            run_id: "run_0".to_owned(),
            node_id: None,
            attempt_id: None,
            actor: Actor::runtime(),
            body,
        }
    }

    /// implement --ready--> test(gate) --passed--> done, --failed--> implement.
    fn loop_graph() -> Graph {
        Graph::builder("t", "implement")
            .agent("implement", "codex", "do it", &["ready"])
            .gate("test", &["true"])
            .terminal("done", Disposition::Succeeded)
            .edge("implement", "ready", "test")
            .edge("test", "passed", "done")
            .edge("test", "failed", "implement")
            .budget(Budget {
                attempts: Some(8),
                elapsed_ms: None,
                cycle_visits: None,
            })
            .require("test", "passed")
            .build()
    }

    /// Fold a list of event bodies, stamping node/attempt ids the way the real
    /// runtime does so correlation holds.
    fn drive_to(graph: &Graph, events: &[EventBody]) -> RunState {
        let mut state = RunState::default();
        let mut attempt_n = 0u32;
        for (i, body) in events.iter().enumerate() {
            let mut e = ev(i as u64, body.clone());
            match body {
                EventBody::AttemptStarted { .. } => {
                    attempt_n += 1;
                    e.node_id = state.current.clone();
                    e.attempt_id = Some(format!("att_{attempt_n}"));
                }
                EventBody::Signal { .. } | EventBody::AttemptFailed { .. } => {
                    e.node_id = state.current.clone();
                    e.attempt_id = state.current_attempt.clone();
                }
                _ => {}
            }
            state = reduce(graph, state, &e);
        }
        state
    }

    #[test]
    fn run_started_activates_entry() {
        let g = loop_graph();
        let s = drive_to(&g, &[EventBody::RunStarted]);
        assert_eq!(s.current.as_deref(), Some("implement"));
        assert_eq!(s.status, Status::Running);
    }

    #[test]
    fn signal_routes_along_matching_edge() {
        let g = loop_graph();
        let s = drive_to(
            &g,
            &[
                EventBody::RunStarted,
                EventBody::AttemptStarted {
                    idempotency_key: "k".to_owned(),
                    worker: None,
                },
                EventBody::Signal {
                    name: "ready".to_owned(),
                },
            ],
        );
        assert_eq!(s.current.as_deref(), Some("test"));
        assert!(!s.awaiting());
    }

    #[test]
    fn schedule_emits_one_effect_then_waits() {
        let g = loop_graph();
        let s = drive_to(&g, &[EventBody::RunStarted]);
        let effects = schedule(&g, &s, 0);
        assert_eq!(effects.len(), 1);
        // After an attempt starts, nothing new schedules until it ends.
        let mut started = ev(
            9,
            EventBody::AttemptStarted {
                idempotency_key: "k".to_owned(),
                worker: None,
            },
        );
        started.node_id = Some("implement".to_owned()); // must target the current node
        started.attempt_id = Some("att_1".to_owned()); // and carry an attempt id
        let s2 = reduce(&g, s, &started);
        assert!(schedule(&g, &s2, 0).is_empty());
    }

    #[test]
    fn terminal_run_schedules_nothing() {
        let g = loop_graph();
        let s = drive_to(
            &g,
            &[
                EventBody::RunStarted,
                EventBody::RunFinished {
                    disposition: Disposition::Succeeded,
                },
            ],
        );
        assert!(schedule(&g, &s, 0).is_empty());
    }

    #[test]
    fn attempts_budget_exhaustion_fails_closed() {
        let g = loop_graph();
        let mut s = drive_to(&g, &[EventBody::RunStarted]);
        s.attempts_total = 8; // at the limit
        assert_eq!(
            schedule(&g, &s, 0),
            vec![Effect::RecordTerminal {
                disposition: Disposition::BudgetExhausted
            }]
        );
    }

    #[test]
    fn success_terminal_requires_acceptance() {
        let g = loop_graph();
        // Reach `done` without ever seeing test.passed.
        let mut s = drive_to(&g, &[EventBody::RunStarted]);
        s.current = Some("done".to_owned());
        assert_eq!(
            schedule(&g, &s, 0),
            vec![Effect::RecordTerminal {
                disposition: Disposition::Failed
            }]
        );
        // With the evidence present it succeeds.
        s.signals.insert("test".to_owned(), "passed".to_owned());
        assert_eq!(
            schedule(&g, &s, 0),
            vec![Effect::RecordTerminal {
                disposition: Disposition::Succeeded
            }]
        );
    }

    #[test]
    fn exact_budget_success_is_not_flipped() {
        // At the attempts limit, a success terminal must still succeed — a
        // reached outcome does not spend an attempt.
        let g = loop_graph();
        let mut s = drive_to(&g, &[EventBody::RunStarted]);
        s.current = Some("done".to_owned());
        s.attempts_total = 8; // exactly at budget
        s.signals.insert("test".to_owned(), "passed".to_owned());
        assert_eq!(
            schedule(&g, &s, 0),
            vec![Effect::RecordTerminal {
                disposition: Disposition::Succeeded
            }]
        );
    }

    #[test]
    fn uncorrelated_signal_does_not_route() {
        let g = loop_graph();
        // Start an attempt on `implement` (att_1), then feed a Signal tagged
        // with a *different* attempt id — it must be ignored.
        let mut s = drive_to(
            &g,
            &[
                EventBody::RunStarted,
                EventBody::AttemptStarted {
                    idempotency_key: "k".to_owned(),
                    worker: None,
                },
            ],
        );
        // AttemptStarted in drive_to has no attempt_id in the envelope; set the
        // projection's expected attempt explicitly to model a real run.
        s.current_attempt = Some("att_1".to_owned());
        let mut forged = ev(
            50,
            EventBody::Signal {
                name: "ready".to_owned(),
            },
        );
        forged.node_id = Some("implement".to_owned());
        forged.attempt_id = Some("att_999".to_owned()); // wrong attempt
        let after = reduce(&g, s, &forged);
        assert_eq!(
            after.current.as_deref(),
            Some("implement"),
            "must not route"
        );
        assert!(
            after.awaiting(),
            "spurious signal leaves the attempt in-flight"
        );
    }

    #[test]
    fn attempt_failed_with_success_disposition_fails_closed() {
        let g = loop_graph();
        // Reach an in-flight attempt on `implement`.
        let mut s = drive_to(
            &g,
            &[
                EventBody::RunStarted,
                EventBody::AttemptStarted {
                    idempotency_key: "k".to_owned(),
                    worker: None,
                },
            ],
        );
        s.current_attempt = Some("att_1".to_owned());
        // A forged failure claiming success must collapse to Failed.
        let mut failed = ev(
            50,
            EventBody::AttemptFailed {
                reason: "forged".to_owned(),
                disposition: Disposition::Succeeded,
            },
        );
        failed.node_id = Some("implement".to_owned());
        failed.attempt_id = Some("att_1".to_owned());
        let after = reduce(&g, s, &failed);
        assert_eq!(after.status, Status::Finished(Disposition::Failed));
    }

    #[test]
    fn replay_is_deterministic() {
        let g = loop_graph();
        let events = [
            EventBody::RunStarted,
            EventBody::AttemptStarted {
                idempotency_key: "k".to_owned(),
                worker: None,
            },
            EventBody::Signal {
                name: "ready".to_owned(),
            },
        ];
        let a = drive_to(&g, &events);
        let b = drive_to(&g, &events);
        assert_eq!(a.current, b.current);
        assert_eq!(a.attempts_total, b.attempts_total);
        assert_eq!(a.visits, b.visits);
    }
}
