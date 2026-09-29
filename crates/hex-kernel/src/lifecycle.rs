//! Lifecycle guards: the single definition of *when* each journal event is legal.
//!
//! The journal has two consumers that must agree: [`reduce`](crate::reduce) —
//! the projection the driver advances as it writes — and
//! [`check_journal`](crate::check_journal), the fail-closed audit every read
//! path (`status`, `logs`, `resume`) runs *before* folding. They used to state
//! the same rules twice, as two hand-written state machines, and they drifted:
//! an `AcceptanceUnmet` advanced `reduce`'s current node but not the audit's, so
//! after one legitimate `accept.on_unmet` reroute every later read of that run
//! failed with `attempt_started does not target the current node` — permanently,
//! because a journal is append-only.
//!
//! So each rule lives here exactly once, as a predicate answering: *could the
//! kernel itself have produced this event from this position?* [`admissible`]
//! maps each event to its predicate and rejection message, and both consumers
//! call it: [`reduce`] drops an event it rejects (a forged record never moves
//! the projection); [`check_journal`] reports the same rejection as an
//! `E-journal-lifecycle` issue. And `check_journal` folds *through* `reduce`
//! rather than tracking its own position, so the state both consult is by
//! construction the same one. Adding a guard means editing one function.
//!
//! Two asymmetries remain deliberate, each documented at its site — in both the
//! audit is the stricter one, so nothing `reduce` would apply survives a read:
//!
//! 1. `RunFinished`: `reduce` applies it unconditionally, `check_journal` refuses
//!    one recorded mid-attempt. Erring towards *stopping* a run cannot be abused,
//!    whereas ignoring a terminal would leave a finished run looking schedulable.
//! 2. `AttemptFailed` with a non-failure disposition: `reduce` collapses it to
//!    `Failed` (never bypassing the acceptance contract), `check_journal` rejects
//!    the journal outright.
//!
//! Beyond those, `check_journal` also enforces *shape* facts that are not
//! lifecycle rules at all and so have no predicate here: the protocol version,
//! `run_created` appearing exactly once and first, what may trail a terminal
//! (inert `Note`s, plus the single uniform `RunFinished` a self-terminating
//! `AttemptFailed` may be followed by — it must *agree* with the disposition
//! already recorded), and `attempt_started` carrying both ids (a clearer message
//! for what [`attempt_start_ok`] would reject anyway). Those stay as explicit
//! arms in `check_journal`, around its one call to [`admissible`].

use hex_proto::{Disposition, Event, EventBody};

use crate::graph::{Graph, NodeKind, NodeSpec};
use crate::{RunState, Status};

/// Idle-running: scheduling is live and no attempt is in flight. This is the
/// only position at which the driver reaches an attempt boundary, so it is the
/// only one that can produce a control-driven or routing record.
pub(crate) fn idle_running(state: &RunState) -> bool {
    state.status == Status::Running && state.current_attempt.is_none()
}

/// Whether scheduling may start here: exactly once, out of `Created`.
///
/// Guarded because a second `RunStarted` *moves* the run — back to the entry
/// node, spending another visit against its bound. (`RunCreated` needs no guard:
/// it carries no position, so replaying one is inert.)
pub(crate) fn run_start_ok(state: &RunState) -> bool {
    state.status == Status::Created
}

/// The kind of the node the run is parked on, if any.
fn current_kind(graph: &Graph, state: &RunState) -> Option<NodeKind> {
    graph.node(state.current.as_deref()?).map(|n| n.spec.kind())
}

/// Whether `event` refers to the currently in-flight node + attempt. Requires an
/// attempt to actually be in flight — a `Signal`/`AttemptFailed`/`NodeResult`
/// arriving while nothing is awaiting is spurious and must not be applied.
pub(crate) fn correlated(state: &RunState, event: &Event) -> bool {
    state.current_attempt.is_some()
        && event.node_id == state.current
        && event.attempt_id == state.current_attempt
}

/// Whether an `AttemptStarted` may begin here: an idle running run, on its own
/// current node, with an attempt id to correlate later outcomes to.
///
/// The kind check is a guard, not bookkeeping. Only `agent` and `command` nodes
/// run an attempt: a `human` node is answered (`HumanRequested`/
/// `HumanResponded`) and a `terminal` node is settled by the kernel. An attempt
/// forged on a human node is precisely how a journal could route *around* an
/// outstanding question — `human_requested → attempt_started → signal` — because
/// the signal then correlates to an attempt that should never have existed.
pub(crate) fn attempt_start_ok(graph: &Graph, state: &RunState, event: &Event) -> bool {
    idle_running(state)
        && event.attempt_id.is_some()
        && state.current.is_some()
        && event.node_id == state.current
        && matches!(
            current_kind(graph, state),
            Some(NodeKind::Agent | NodeKind::Command)
        )
}

/// Whether a `NodeResult` may be recorded here: inside an in-flight *agent*
/// attempt that has not already recorded one. A command node yields a
/// `passed`/`failed` verdict, never a captured final message, so a result
/// attributed to one is forged.
///
/// At most one result per attempt, and the projection already says so: an
/// `AttemptStarted` clears `results` for the node it starts, so "this node has no
/// result" *is* "this attempt has not recorded one". Without the clause a second
/// record silently overwrote the first, which is how a forged trailing
/// `node_result` could replace what the agent actually reported downstream.
pub(crate) fn node_result_ok(graph: &Graph, state: &RunState, event: &Event) -> bool {
    correlated(state, event)
        && current_kind(graph, state) == Some(NodeKind::Agent)
        && state
            .current
            .as_deref()
            .is_some_and(|cur| !state.results.contains_key(cur))
}

/// Whether an `AttemptReported` may be recorded here: inside an in-flight
/// *agent* attempt that has not already reported.
///
/// The "not already" clause is not symmetry with [`node_result_ok`] for its own
/// sake — usage is *summed* into the projection, so a duplicate record does not
/// overwrite a fact, it inflates one. A replayed report would bill the run
/// twice, and a cost you cannot trust is worse than no cost at all.
pub(crate) fn attempt_report_ok(graph: &Graph, state: &RunState, event: &Event) -> bool {
    correlated(state, event)
        && current_kind(graph, state) == Some(NodeKind::Agent)
        && state.reported_attempt != state.current_attempt
}

/// Whether a `HumanRequested` may be recorded here: an idle running run parked
/// on the very `human` node doing the asking. A re-ask after a crash is
/// legitimate, so this stays true for an already-outstanding question (it
/// re-arms it).
pub(crate) fn human_request_ok(graph: &Graph, state: &RunState, event: &Event) -> bool {
    idle_running(state)
        && state.current.is_some()
        && event.node_id == state.current
        && current_kind(graph, state) == Some(NodeKind::Human)
}

/// Whether a `HumanResponded` may be applied here: it must answer an
/// *outstanding* question on the node the run is parked on.
///
/// [`RunState::asked`] is the human transport's analogue of [`correlated`]: a
/// human node runs no attempt, so without it a response could not be correlated
/// to anything and "the current node is a human node" was the whole check. That
/// let an answer nobody asked for route the run.
pub(crate) fn human_response_ok(graph: &Graph, state: &RunState, event: &Event) -> bool {
    idle_running(state)
        && state.asked.is_some()
        && event.node_id == state.asked
        && state.asked == state.current
        && current_kind(graph, state) == Some(NodeKind::Human)
}

/// Whether an `AcceptanceUnmet` reroute is one [`schedule`](crate::schedule)
/// could have emitted: idle-running, parked on a **success** terminal, routing to
/// the graph's declared `accept.on_unmet`, and carrying exactly the evidence
/// [`accept`](crate::accept) reports missing.
///
/// Every clause earns its keep. With only "running, nothing in flight, target
/// exists" (what this used to be), a forged record could jump the run from any
/// idle node to any node in the graph, and a *duplicate* record counted a second
/// visit against the target — spending a cycle bound on a transition the kernel
/// never made.
pub(crate) fn unmet_reroute_ok(
    graph: &Graph,
    state: &RunState,
    to: &str,
    missing: &[String],
) -> bool {
    idle_running(state)
        && graph.accept.on_unmet.as_deref() == Some(to)
        && graph.nodes.contains_key(to)
        && matches!(
            state
                .current
                .as_deref()
                .and_then(|c| graph.node(c))
                .map(|n| &n.spec),
            Some(NodeSpec::Terminal {
                disposition: Disposition::Succeeded
            })
        )
        && missing_evidence(graph, state) == missing
}

/// The `node.signal` evidence the acceptance contract still lacks, in the
/// contract's declared order.
///
/// The one definition behind [`accept`](crate::accept) *and* the
/// `AcceptanceUnmet` guard — deterministic order is what lets a journaled
/// `missing` list be compared against it element for element.
pub(crate) fn missing_evidence(graph: &Graph, state: &RunState) -> Vec<String> {
    let mut missing = Vec::new();
    for req in &graph.accept.require {
        if state.signals.get(&req.node).map(String::as_str) != Some(req.signal.as_str()) {
            missing.push(format!("{}.{}", req.node, req.signal));
        }
    }
    missing
}

/// Whether a pause may be recorded: only between attempts, where the driver
/// drains control commands. Pausing mid-attempt would orphan it.
pub(crate) fn pause_ok(state: &RunState) -> bool {
    idle_running(state)
}

/// Whether a resume may be recorded: only to lift an actual pause.
pub(crate) fn resume_ok(state: &RunState) -> bool {
    state.status == Status::Paused
}

/// Whether operator guidance may be queued here.
///
/// Steering is inert until an agent attempt reads it, but it must still be
/// *journalable*: the driver drains the control inbox only at an attempt
/// boundary or while blocked on a human node, so idle-running and paused are the
/// only positions that can produce a `Steered`. Without this, a hand-edited
/// journal could place guidance before `run_created` and have it reach the first
/// prompt as if an operator had sent it.
pub(crate) fn steer_ok(state: &RunState) -> bool {
    idle_running(state) || state.status == Status::Paused
}

/// Whether `event` may apply at `state`: every per-event guard, with the reason
/// it fails. [`reduce`](crate::reduce) drops an event on `Err`;
/// [`check_journal`](crate::check_journal) reports the reason as an
/// `E-journal-lifecycle` issue. `RunCreated`, `RunFinished` and `Note` carry no
/// guard here — the audit's stricter handling of `RunFinished` and of a
/// non-failure `AttemptFailed` are the deliberate asymmetries in the module doc.
pub(crate) fn admissible(graph: &Graph, state: &RunState, event: &Event) -> Result<(), String> {
    let (ok, why) = match &event.body {
        EventBody::RunCreated { .. } | EventBody::RunFinished { .. } | EventBody::Note { .. } => {
            return Ok(());
        }
        EventBody::RunStarted => (run_start_ok(state), "run_started out of order"),
        EventBody::AcceptanceUnmet { to, missing } => {
            if unmet_reroute_ok(graph, state, to, missing) {
                return Ok(());
            }
            return Err(format!(
                "acceptance_unmet to `{to}` is not a reroute the kernel could have emitted \
                 here: it needs an idle running run parked on a success terminal, `to` equal \
                 to the graph's `accept.on_unmet`, and exactly the evidence acceptance \
                 actually reports missing"
            ));
        }
        EventBody::AttemptStarted { .. } => (
            attempt_start_ok(graph, state, event),
            "attempt_started must target an idle running run's current agent or command node",
        ),
        // A routing signal is only meaningful from the attempt that produced it.
        // (`reduce` then routes; a signal with no legal edge is a defensive run
        // failure there, so it is admissible here.)
        EventBody::Signal { .. } => (
            correlated(state, event),
            "signal does not match the in-flight attempt",
        ),
        EventBody::AttemptFailed { .. } => (
            correlated(state, event),
            "attempt_failed does not match the in-flight attempt",
        ),
        EventBody::AttemptInterrupted => (
            correlated(state, event),
            "attempt_interrupted does not match the in-flight attempt",
        ),
        EventBody::RunPaused => (pause_ok(state), "run_paused while not idle-running"),
        EventBody::RunResumed => (resume_ok(state), "run_resumed without a pause"),
        EventBody::Steered { .. } => (
            steer_ok(state),
            "steered outside an attempt boundary or a pause",
        ),
        EventBody::HumanRequested { .. } => (
            human_request_ok(graph, state, event),
            "human_requested must target an idle running run's current human node",
        ),
        EventBody::HumanResponded { .. } => (
            human_response_ok(graph, state, event),
            "human_responded does not answer an outstanding question on the node the run is \
             parked on",
        ),
        EventBody::NodeResult { .. } => (
            node_result_ok(graph, state, event),
            "node_result does not match an in-flight agent attempt that has not already \
             recorded one",
        ),
        EventBody::AttemptReported { .. } => (
            attempt_report_ok(graph, state, event),
            "attempt_reported does not match an in-flight agent attempt that has not already \
             reported",
        ),
    };
    if ok { Ok(()) } else { Err(why.to_owned()) }
}
