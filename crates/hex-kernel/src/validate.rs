//! Pure IR validation — the minimum honest set for the slim MVP.
//!
//! Operates on the compiled [`Graph`] only (surface parsing lives in the
//! runtime). Catches the errors that make a run undefined *before* any worker
//! spends a token: dangling references, unreachable nodes, unbounded cycles,
//! and agent proposals that cannot route.

use std::collections::BTreeSet;

use hex_proto::{Disposition, Event, EventBody, PROTOCOL_VERSION};

use crate::graph::{Graph, NodeKind, NodeSpec};
use crate::lifecycle;

/// A single validation failure, located by node/edge where possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Machine-stable code, e.g. `E-unbounded-cycle`.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
}

impl Issue {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Validate a compiled graph. Returns every issue found (not just the first),
/// so an author can fix them in one pass. `Ok(())` means the graph is runnable.
///
/// # Errors
/// Returns the list of [`Issue`]s when the graph is not runnable.
pub fn validate(graph: &Graph) -> Result<(), Vec<Issue>> {
    let mut issues = Vec::new();

    if graph.nodes.is_empty() {
        issues.push(Issue::new("E-empty", "graph has no nodes"));
    }
    if !graph.nodes.contains_key(&graph.entry) {
        issues.push(Issue::new(
            "E-entry",
            format!("entry node `{}` does not exist", graph.entry),
        ));
    }

    // Edge references must resolve.
    for edge in &graph.edges {
        if !graph.nodes.contains_key(&edge.from) {
            issues.push(Issue::new(
                "E-edge-from",
                format!("edge from unknown node `{}`", edge.from),
            ));
        }
        if !graph.nodes.contains_key(&edge.to) {
            issues.push(Issue::new(
                "E-edge-to",
                format!("edge to unknown node `{}`", edge.to),
            ));
        }
    }

    check_reachability(graph, &mut issues);
    check_routing(graph, &mut issues);
    check_signal_names(graph, &mut issues);
    check_acceptance(graph, &mut issues);
    check_result_refs(graph, &mut issues);

    check_cycles(graph, &mut issues);

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn check_reachability(graph: &Graph, issues: &mut Vec<Issue>) {
    if !graph.nodes.contains_key(&graph.entry) {
        return; // already reported; reachability is meaningless
    }
    let reachable = reachable_from(graph, &graph.entry);
    for id in graph.nodes.keys() {
        if !reachable.contains(id.as_str()) {
            issues.push(Issue::new(
                "E-unreachable",
                format!("node `{id}` is unreachable from entry `{}`", graph.entry),
            ));
        }
    }
    // At least one terminal must be reachable so the run can end...
    let terminals: Vec<&NodeSpec> = graph
        .nodes
        .values()
        .filter(|n| reachable.contains(n.id.as_str()))
        .map(|n| &n.spec)
        .filter(|s| s.kind() == NodeKind::Terminal)
        .collect();
    if terminals.is_empty() {
        issues.push(Issue::new(
            "E-no-terminal",
            "no terminal node is reachable from entry",
        ));
        return;
    }
    // ...and at least one of them must be a *success*, or the graph has no happy
    // path: every run of it is predestined to fail, which is never what the
    // author meant and is invisible without tracing the edges by hand.
    let has_success = terminals.iter().any(|s| {
        matches!(
            s,
            NodeSpec::Terminal {
                disposition: Disposition::Succeeded
            }
        )
    });
    if !has_success {
        issues.push(Issue::new(
            "E-no-happy-path",
            "no `terminal: succeeded` is reachable from entry — this graph can only fail",
        ));
    }
}

fn check_routing(graph: &Graph, issues: &mut Vec<Issue>) {
    for node in graph.nodes.values() {
        let edge_signals: BTreeSet<&str> = graph.signals_from(&node.id).into_iter().collect();
        match &node.spec {
            NodeSpec::Agent { may_propose, .. } => {
                let proposable: BTreeSet<&str> = may_propose.iter().map(String::as_str).collect();
                // `done` is reserved for runtime-synthesized implicit completion;
                // an agent must not list it as something it proposes.
                if proposable.contains(DONE_SIGNAL) {
                    issues.push(Issue::new(
                        "E-done-reserved",
                        format!("agent `{}` lists reserved `done` in may_propose", node.id),
                    ));
                }
                // Every proposable event must have an edge...
                for sig in &proposable {
                    if !edge_signals.contains(sig) {
                        issues.push(Issue::new(
                            "E-proposal-no-edge",
                            format!(
                                "agent `{}` may propose `{sig}` but no edge handles it",
                                node.id
                            ),
                        ));
                    }
                }
                // ...and every outgoing edge must be proposable — except the
                // reserved `done`, synthesized by the runtime when an agent
                // completes cleanly without emitting (implicit completion).
                for sig in &edge_signals {
                    if *sig != DONE_SIGNAL && !proposable.contains(sig) {
                        issues.push(Issue::new(
                            "E-edge-not-proposable",
                            format!(
                                "agent `{}` has an edge on `{sig}` outside its may_propose list",
                                node.id
                            ),
                        ));
                    }
                }
            }
            NodeSpec::Command { .. } => {
                for sig in &edge_signals {
                    if *sig != "passed" && *sig != "failed" {
                        issues.push(Issue::new(
                            "E-gate-signal",
                            format!(
                                "command `{}` has an edge on `{sig}` (only `passed`/`failed`)",
                                node.id
                            ),
                        ));
                    }
                }
            }
            NodeSpec::Terminal { .. } => {
                if !edge_signals.is_empty() {
                    issues.push(Issue::new(
                        "E-terminal-edge",
                        format!("terminal `{}` must have no outgoing edges", node.id),
                    ));
                }
            }
            // A human answer is *text*, not a signal, so the runtime cannot pick
            // between two outgoing edges — it routes the node's single edge. Both
            // arms below used to pass validation and then fail (or dead-end) at
            // run time, which is exactly the dishonesty a validator exists to
            // prevent. Branch on the *content* of the answer downstream instead.
            NodeSpec::Human { .. } => match edge_signals.len() {
                0 => issues.push(Issue::new(
                    "E-human-no-edge",
                    format!(
                        "human `{}` has no outgoing edge, so an answer could not \
                         continue the run (add `on: {{ done: <node> }}`)",
                        node.id
                    ),
                )),
                1 => {}
                _ => issues.push(Issue::new(
                    "E-human-multi-edge",
                    format!(
                        "human `{}` has {} outgoing edges but an answer carries text, \
                         not a signal — keep one edge (`done`) and branch on the \
                         answer in a downstream node",
                        node.id,
                        edge_signals.len()
                    ),
                )),
            },
        }
    }
}

/// The reserved routing signal the runtime synthesizes when an agent finishes
/// cleanly without emitting (implicit completion). Not agent-proposable.
pub const DONE_SIGNAL: &str = "done";

/// Validate the template tokens in agent/human prompts: every `{{…}}` must be
/// terminated, and every `{{<node>.result}}` must name a node that actually
/// produces a result — an *agent* (its captured final message) or a *human*
/// (their answer) — so a downstream handoff can't silently interpolate to
/// nothing or to an unresolved token.
fn check_result_refs(graph: &Graph, issues: &mut Vec<Issue>) {
    for node in graph.nodes.values() {
        let prompt = match &node.spec {
            NodeSpec::Agent { prompt, .. } | NodeSpec::Human { prompt } => prompt.as_str(),
            _ => continue,
        };
        // Drive the shared template grammar (so validation and the runtime's
        // interpolation can never disagree on what a token is).
        for token in crate::template::tokens(prompt) {
            let reff = match token {
                crate::template::Token::Result(reff) => reff,
                crate::template::Token::Unterminated(_) => {
                    issues.push(Issue::new(
                        "E-result-ref",
                        format!(
                            "node `{}` has an unterminated `{{{{` template token",
                            node.id
                        ),
                    ));
                    continue;
                }
                _ => continue, // text or `{{prompt}}` — not a result ref
            };
            match graph.nodes.get(reff) {
                None => issues.push(Issue::new(
                    "E-result-ref",
                    format!(
                        "node `{}` references `{{{{{reff}.result}}}}` but no node `{reff}` exists",
                        node.id
                    ),
                )),
                // Only agents and humans produce a result; a command/terminal
                // reference would always interpolate to nothing.
                Some(n) if !produces_result(n.spec.kind()) => issues.push(Issue::new(
                    "E-result-ref",
                    format!(
                        "node `{}` references `{{{{{reff}.result}}}}` but `{reff}` is a {} (only agent and human nodes produce a result)",
                        node.id,
                        n.spec.kind().as_str()
                    ),
                )),
                Some(_) => {}
            }
        }
    }
}

/// Whether a node kind yields a `{{<node>.result}}` value: an agent's captured
/// final message, or a human's answer.
fn produces_result(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Agent | NodeKind::Human)
}

fn check_acceptance(graph: &Graph, issues: &mut Vec<Issue>) {
    if let Some(to) = &graph.accept.on_unmet {
        match graph.nodes.get(to) {
            None => issues.push(Issue::new(
                "E-accept-unmet-node",
                format!("`accept.on_unmet` names unknown node `{to}`"),
            )),
            Some(n) if n.spec.kind() == NodeKind::Terminal => issues.push(Issue::new(
                "E-accept-unmet-terminal",
                format!(
                    "`accept.on_unmet` points at terminal `{to}`; it must name a node \
                     that can produce the missing evidence"
                ),
            )),
            Some(_) => {}
        }
    }

    for req in &graph.accept.require {
        let Some(node) = graph.nodes.get(&req.node) else {
            issues.push(Issue::new(
                "E-accept-node",
                format!("acceptance requires unknown node `{}`", req.node),
            ));
            continue;
        };
        // The required signal must actually be producible by that node, or the
        // contract can never be satisfied and the run can never succeed.
        let producible = match &node.spec {
            // An agent produces its `may_propose` signals, plus the synthesized
            // `done` when it has a `done` edge (implicit completion).
            NodeSpec::Agent { may_propose, .. } => {
                may_propose.iter().any(|s| s == &req.signal)
                    || (req.signal == DONE_SIGNAL && graph.route(&req.node, DONE_SIGNAL).is_some())
            }
            NodeSpec::Command { .. } => req.signal == "passed" || req.signal == "failed",
            // An answered human node produces the signal of its single outgoing
            // edge, so requiring it (an approval as acceptance evidence) is
            // satisfiable exactly when that edge exists.
            NodeSpec::Human { .. } => graph.route(&req.node, &req.signal).is_some(),
            NodeSpec::Terminal { .. } => false,
        };
        if !producible {
            issues.push(Issue::new(
                "E-accept-unsatisfiable",
                format!(
                    "acceptance requires `{}.{}` but node `{}` can never emit `{}`",
                    req.node, req.signal, req.node, req.signal
                ),
            ));
        }
    }
}

/// Routing event names must fit a small grammar so they survive the
/// comma-delimited `HEX_MAY_PROPOSE` channel and stay unambiguous.
fn check_signal_names(graph: &Graph, issues: &mut Vec<Issue>) {
    let mut check = |name: &str, where_: &str| {
        if !is_valid_signal_name(name) {
            issues.push(Issue::new(
                "E-bad-signal-name",
                format!("signal `{name}` ({where_}) must match [a-z][a-z0-9_]*"),
            ));
        }
    };
    for edge in &graph.edges {
        check(&edge.on, &format!("edge {}->{}", edge.from, edge.to));
    }
    for node in graph.nodes.values() {
        if let NodeSpec::Agent { may_propose, .. } = &node.spec {
            for sig in may_propose {
                check(sig, &format!("{} may_propose", node.id));
            }
        }
    }
}

/// Validate the lifecycle ordering of a run's journal against `graph` *before*
/// folding it, so a malformed or forged sequence fails closed instead of
/// silently mutating the projection. Complements the byte-level integrity the
/// runtime's journal reader enforces (contiguous seq, schema, single run id).
///
/// It projects the run's position by calling [`reduce`](crate::reduce) itself,
/// rather than re-deriving it: this used to be a second hand-written state
/// machine, and the two drifted — an `AcceptanceUnmet` moved `reduce`'s current
/// node but not this one's, so a single legitimate `accept.on_unmet` reroute made
/// every later `status`/`logs`/`resume` reject the journal as invalid, forever.
/// Each rule is now one predicate in [`crate::lifecycle`], shared with `reduce`:
/// what `reduce` silently drops is exactly what this reports. Only *shape* facts
/// outside the projection (has `run_created` been seen at all) are tracked here;
/// the deliberate exceptions are enumerated in the `lifecycle` module doc.
///
/// Returns the projection it folded on the way through: the audit has to build
/// the state anyway to evaluate its guards, so handing it back spares every
/// caller a second fold of the same events.
///
/// # Errors
/// Returns the first lifecycle violation found.
pub fn check_journal(graph: &Graph, events: &[Event]) -> Result<crate::RunState, Issue> {
    let mut state = crate::RunState::default();
    // `RunState::default()` is indistinguishable from post-`run_created`, so the
    // "nothing yet" phase needs its own flag.
    let mut created = false;

    for (i, e) in events.iter().enumerate() {
        if e.schema_version != PROTOCOL_VERSION {
            return Err(bad(
                i,
                format!("unsupported schema_version {}", e.schema_version),
            ));
        }
        if state.is_finished() {
            // Only inert diagnostics may trail a terminal.
            if !matches!(e.body, EventBody::Note { .. }) {
                return Err(bad(i, "event after the run finished"));
            }
            continue;
        }
        match &e.body {
            EventBody::RunCreated { .. } => {
                if created {
                    return Err(bad(i, "run_created must be the first event"));
                }
                created = true;
            }
            EventBody::RunStarted => {
                if !created || !lifecycle::run_start_ok(&state) {
                    return Err(bad_at(i, &state, "run_started out of order"));
                }
            }
            EventBody::AcceptanceUnmet { to, missing } => {
                if !lifecycle::unmet_reroute_ok(graph, &state, to, missing) {
                    return Err(bad_at(
                        i,
                        &state,
                        format!(
                            "acceptance_unmet to `{to}` is not a reroute the kernel could have \
                             emitted here: it needs an idle running run parked on a success \
                             terminal, `to` equal to the graph's `accept.on_unmet`, and exactly \
                             the evidence acceptance actually reports missing"
                        ),
                    ));
                }
            }
            EventBody::AttemptStarted { .. } => {
                if e.node_id.is_none() || e.attempt_id.is_none() {
                    return Err(bad(i, "attempt_started missing node/attempt id"));
                }
                if !lifecycle::attempt_start_ok(graph, &state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "attempt_started must target an idle running run's current agent or \
                         command node",
                    ));
                }
            }
            // A routing signal is only meaningful from the attempt that produced
            // it. (`reduce` then routes; a signal with no legal edge is a
            // defensive run failure there, so it is accepted here.)
            EventBody::Signal { .. } => {
                if !lifecycle::correlated(&state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "signal does not match the in-flight attempt",
                    ));
                }
            }
            EventBody::AttemptFailed { disposition, .. } => {
                if !lifecycle::correlated(&state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "attempt_failed does not match the in-flight attempt",
                    ));
                }
                // Stricter than `reduce`, which collapses a forged success
                // disposition to `failed` rather than rejecting the journal.
                if !matches!(disposition, Disposition::Failed | Disposition::TimedOut) {
                    return Err(bad(i, "attempt_failed carries a non-failure disposition"));
                }
            }
            EventBody::AttemptInterrupted => {
                if !lifecycle::correlated(&state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "attempt_interrupted does not match the in-flight attempt",
                    ));
                }
            }
            EventBody::RunFinished { .. } => {
                if state.awaiting() {
                    return Err(bad(i, "run_finished while an attempt is still in flight"));
                }
            }
            // Pause/resume bracket a suspension. Both are recorded at attempt
            // boundaries, so an in-flight attempt makes them impossible.
            EventBody::RunPaused => {
                if !lifecycle::pause_ok(&state) {
                    return Err(bad_at(i, &state, "run_paused while not idle-running"));
                }
            }
            EventBody::RunResumed => {
                if !lifecycle::resume_ok(&state) {
                    return Err(bad_at(i, &state, "run_resumed without a pause"));
                }
            }
            // Steering is inert until an attempt reads it, but it must still come
            // from a position the driver could have journaled it in: it drains the
            // control inbox at an attempt boundary or while blocked on a human
            // node, never mid-attempt and never before the run exists.
            EventBody::Steered { .. } => {
                if !lifecycle::steer_ok(&state) {
                    return Err(bad_at(
                        i,
                        &state,
                        "steered outside an attempt boundary or a pause",
                    ));
                }
            }
            EventBody::HumanRequested { .. } => {
                if !lifecycle::human_request_ok(graph, &state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "human_requested must target an idle running run's current human node",
                    ));
                }
            }
            EventBody::HumanResponded { .. } => {
                if !lifecycle::human_response_ok(graph, &state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "human_responded does not answer an outstanding question on the node the \
                         run is parked on",
                    ));
                }
            }
            // NodeResult rides inside an in-flight attempt (before its signal).
            // Only an *agent* attempt produces a result, and at most one per
            // attempt — so a stray/forged/duplicate record can't slip through
            // and later surface as a bogus final message. Both halves live in the
            // shared guard: `reduce` drops exactly what this rejects.
            EventBody::NodeResult { .. } => {
                if !lifecycle::node_result_ok(graph, &state, e) {
                    return Err(bad_at(
                        i,
                        &state,
                        "node_result does not match an in-flight agent attempt that has not \
                         already recorded one",
                    ));
                }
            }
            EventBody::Note { .. } => {}
        }
        // The projection every guard above reads is `reduce`'s own, so the audit
        // and the driver can never disagree about where the run is.
        state = crate::reduce(graph, state, e);
    }
    Ok(state)
}

fn bad(index: usize, why: impl Into<String>) -> Issue {
    Issue::new(
        "E-journal-lifecycle",
        format!("event {index}: {}", why.into()),
    )
}

/// A rejection that also reports the projected position, so the message says why
/// the event was impossible without restating the rule the guard owns.
fn bad_at(index: usize, state: &crate::RunState, why: impl Into<String>) -> Issue {
    bad(
        index,
        format!(
            "{} [status={}, current={}, in_flight={}, asked={}]",
            why.into(),
            state.status,
            state.current.as_deref().unwrap_or("none"),
            state.current_attempt.as_deref().unwrap_or("none"),
            state.asked.as_deref().unwrap_or("none"),
        ),
    )
}

/// A routing signal name: lowercase, starts with a letter, `[a-z0-9_]` after.
#[must_use]
pub fn is_valid_signal_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn reachable_from<'a>(graph: &'a Graph, start: &'a str) -> BTreeSet<&'a str> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for edge in graph.edges.iter().filter(|e| e.from == id) {
            stack.push(edge.to.as_str());
        }
    }
    seen
}

/// Core rule 5: every cycle is bounded, and an unbounded one is an *error*.
///
/// Two things make this more than back-edge detection over `graph.edges`:
///
/// 1. **`accept.on_unmet` is a transition no edge describes.** `schedule` moves a
///    run from a success terminal back to `on_unmet` when the contract is unmet,
///    so `a → done` with `on_unmet: a` is an edge-acyclic graph that loops for
///    real. It passed validation with no bound and spun until the driver's
///    100_000-iteration ceiling — exactly the failure this rule exists to
///    prevent, hiding from the check that enforces it.
/// 2. **A per-node `budget.visits` bounds the cycles through that node.** So the
///    question is not "is there a cycle?" but "is there a cycle that no bound
///    stops?" — answered by deleting the nodes that carry their own bound and
///    looking for a cycle in what is left. A cycle that survives that deletion
///    passes through no bounded node, so nothing stops it.
fn check_cycles(graph: &Graph, issues: &mut Vec<Issue>) {
    // A run-wide bound stops every cycle, so nothing more to prove.
    if graph.budget.bounds_cycles() {
        return;
    }
    let implicit = graph.implicit_reroutes();
    if !has_unbounded_cycle(graph, &implicit) {
        return;
    }
    // Name the implicit transition when it is what closes the loop: an author
    // staring at acyclic-looking edges has no other way to see it.
    let via_unmet = !has_unbounded_cycle(graph, &[]);
    let mut message = String::from(
        "graph contains a cycle that no bound stops (set budget.attempts, \
         budget.cycle_visits, or `budget: { visits: N }` on a node in the cycle)",
    );
    if via_unmet && let Some(to) = &graph.accept.on_unmet {
        message.push_str(&format!(
            " — the cycle is closed by `accept.on_unmet: {to}`, which sends a success \
             terminal back to `{to}` whenever the acceptance contract is unmet"
        ));
    }
    issues.push(Issue::new("E-unbounded-cycle", message));
}

/// DFS back-edge detection over the graph's transitions (`edges` plus `extra`),
/// ignoring nodes whose own `budget.visits` already bounds every cycle through
/// them. A back edge found among what remains is an unbounded cycle.
fn has_unbounded_cycle(graph: &Graph, extra: &[(&str, &str)]) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    fn bounded(graph: &Graph, id: &str) -> bool {
        graph.node(id).is_some_and(|n| n.max_visits.is_some())
    }
    fn visit<'a>(
        graph: &'a Graph,
        extra: &[(&'a str, &'a str)],
        id: &'a str,
        marks: &mut std::collections::BTreeMap<&'a str, Mark>,
    ) -> bool {
        marks.insert(id, Mark::Open);
        let targets = graph
            .edges
            .iter()
            .filter(|e| e.from == id)
            .map(|e| e.to.as_str())
            .chain(
                extra
                    .iter()
                    .filter(|(from, _)| *from == id)
                    .map(|(_, to)| *to),
            );
        for to in targets {
            if bounded(graph, to) {
                continue; // any cycle through `to` is bounded by its own budget
            }
            match marks.get(to) {
                Some(Mark::Open) => return true,
                Some(Mark::Done) => {}
                None => {
                    if visit(graph, extra, to, marks) {
                        return true;
                    }
                }
            }
        }
        marks.insert(id, Mark::Done);
        false
    }

    let mut marks = std::collections::BTreeMap::new();
    graph.nodes.keys().any(|id| {
        !bounded(graph, id)
            && !marks.contains_key(id.as_str())
            && visit(graph, extra, id, &mut marks)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Budget, Graph};
    use hex_proto::Disposition;

    fn cyclic(budget: Budget) -> Graph {
        Graph::builder("t", "implement")
            .agent("implement", "codex", "p", &["ready"])
            .command("test", &["true"])
            .terminal("done", Disposition::Succeeded)
            .edge("implement", "ready", "test")
            .edge("test", "passed", "done")
            .edge("test", "failed", "implement")
            .budget(budget)
            .require("test", "passed")
            .build()
    }

    #[test]
    fn bounded_cycle_is_valid() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    #[test]
    fn unbounded_cycle_is_rejected() {
        let g = cyclic(Budget::default());
        let issues = validate(&g).unwrap_err();
        assert!(issues.iter().any(|i| i.code == "E-unbounded-cycle"));
    }

    /// `implement --ready|blocked--> done`, acceptance requiring `implement.ready`
    /// and rerouting on unmet. **Edge-acyclic**, yet it loops: reaching `done`
    /// having emitted `blocked` sends the run back to `implement` via
    /// `accept.on_unmet`, a transition no `Edge` describes.
    fn unmet_loop_graph(budget: Budget, node_visits: Option<u32>) -> Graph {
        let mut builder = Graph::builder("t", "implement")
            .agent("implement", "w", "p", &["ready", "blocked"])
            .terminal("done", Disposition::Succeeded)
            .edge("implement", "ready", "done")
            .edge("implement", "blocked", "done")
            .budget(budget)
            .require("implement", "ready")
            .on_unmet("implement");
        if let Some(visits) = node_visits {
            builder = builder.max_visits("implement", visits);
        }
        builder.build()
    }

    /// Core rule 5, at the one place it was blind: the cycle `accept.on_unmet`
    /// creates is invisible to edge traversal, so an unbounded one used to pass
    /// validation and then spin until the driver's iteration ceiling.
    #[test]
    fn an_unbounded_on_unmet_cycle_is_rejected() {
        let g = unmet_loop_graph(Budget::default(), None);
        let issues = validate(&g).unwrap_err();
        let issue = issues
            .iter()
            .find(|i| i.code == "E-unbounded-cycle")
            .unwrap_or_else(|| panic!("{issues:?}"));
        assert!(
            issue.message.contains("accept.on_unmet"),
            "the message must name the transition that closes the loop: {issue}"
        );
    }

    #[test]
    fn a_bounded_on_unmet_cycle_is_valid() {
        let g = unmet_loop_graph(
            Budget {
                attempts: Some(4),
                ..Budget::default()
            },
            None,
        );
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    /// A per-node bound bounds the cycles through that node, so it is a bound for
    /// core rule 5's purposes even with no run-wide budget declared.
    #[test]
    fn a_per_node_visit_bound_bounds_its_cycle() {
        let g = unmet_loop_graph(Budget::default(), Some(3));
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    /// An `accept.require` that can never be unmet adds no implicit transition, so
    /// an `on_unmet` on an otherwise acyclic graph must not be reported as a loop.
    #[test]
    fn on_unmet_without_requirements_is_not_a_cycle() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["go"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "go", "done")
            .on_unmet("a")
            .build();
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    #[test]
    fn dangling_edge_target_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["go"])
            .edge("a", "go", "nowhere")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(issues.iter().any(|i| i.code == "E-edge-to"));
    }

    #[test]
    fn proposal_without_edge_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["go", "stop"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "go", "done")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(issues.iter().any(|i| i.code == "E-proposal-no-edge"));
    }

    fn ev(seq: u64, node: Option<&str>, attempt: Option<&str>, body: EventBody) -> Event {
        Event {
            schema_version: PROTOCOL_VERSION,
            seq,
            at_ms: 0,
            run_id: "run_0".to_owned(),
            node_id: node.map(ToOwned::to_owned),
            attempt_id: attempt.map(ToOwned::to_owned),
            actor: hex_proto::Actor::runtime(),
            body,
        }
    }

    fn started_journal() -> Vec<Event> {
        vec![
            ev(
                0,
                None,
                None,
                EventBody::RunCreated {
                    graph_hash: "h".to_owned(),
                    inputs: Default::default(),
                    defaults: Default::default(),
                    checks: Default::default(),
                },
            ),
            ev(1, None, None, EventBody::RunStarted),
            ev(
                2,
                Some("implement"),
                Some("att_1"),
                EventBody::AttemptStarted {
                    idempotency_key: "k".to_owned(),
                    worker: None,
                },
            ),
        ]
    }

    #[test]
    fn check_journal_accepts_a_well_formed_prefix() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        assert!(check_journal(&g, &started_journal()).is_ok());
    }

    #[test]
    fn check_journal_rejects_attempt_on_wrong_node() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = started_journal();
        // The graph's entry is `implement`; an attempt on `test` is impossible.
        events[2].node_id = Some("test".to_owned());
        assert!(check_journal(&g, &events).is_err());
    }

    #[test]
    fn check_journal_rejects_success_flavored_failure() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = started_journal();
        events.push(ev(
            3,
            Some("implement"),
            Some("att_1"),
            EventBody::AttemptFailed {
                reason: "forged".to_owned(),
                disposition: Disposition::Succeeded,
            },
        ));
        assert!(check_journal(&g, &events).is_err());
    }

    #[test]
    fn check_journal_accepts_one_node_result_on_an_agent_attempt() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = started_journal();
        events.push(ev(
            3,
            Some("implement"),
            Some("att_1"),
            EventBody::NodeResult {
                text: "did it".to_owned(),
            },
        ));
        assert!(check_journal(&g, &events).is_ok());
    }

    #[test]
    fn check_journal_rejects_two_node_results_for_one_attempt() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = started_journal();
        for seq in 3..=4 {
            events.push(ev(
                seq,
                Some("implement"),
                Some("att_1"),
                EventBody::NodeResult {
                    text: "dup".to_owned(),
                },
            ));
        }
        assert!(check_journal(&g, &events).is_err());
    }

    #[test]
    fn check_journal_rejects_node_result_on_a_gate_attempt() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = started_journal();
        // Route implement → test (a gate), start its attempt, then forge a result.
        events.push(ev(
            3,
            Some("implement"),
            Some("att_1"),
            EventBody::Signal {
                name: "ready".to_owned(),
            },
        ));
        events.push(ev(
            4,
            Some("test"),
            Some("att_2"),
            EventBody::AttemptStarted {
                idempotency_key: "k2".to_owned(),
                worker: None,
            },
        ));
        events.push(ev(
            5,
            Some("test"),
            Some("att_2"),
            EventBody::NodeResult {
                text: "gates don't produce results".to_owned(),
            },
        ));
        assert!(check_journal(&g, &events).is_err());
    }

    #[test]
    fn unsatisfiable_acceptance_is_rejected() {
        // Require a signal the referenced node can never emit.
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["go"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "go", "done")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .require("a", "nope")
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(issues.iter().any(|i| i.code == "E-accept-unsatisfiable"));
    }

    #[test]
    fn bad_signal_name_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["Go Now"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "Go Now", "done")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(issues.iter().any(|i| i.code == "E-bad-signal-name"));
    }

    #[test]
    fn agent_with_done_edge_and_no_proposals_is_valid() {
        // Implicit completion: an agent that emits nothing routes the reserved
        // `done`, which needs no may_propose entry.
        let g = Graph::builder("t", "implement")
            .agent("implement", "w", "do it", &[])
            .command("test", &["true"])
            .terminal("done", Disposition::Succeeded)
            .edge("implement", "done", "test")
            .edge("test", "passed", "done")
            .edge("test", "failed", "implement")
            .budget(Budget {
                attempts: Some(4),
                ..Budget::default()
            })
            .require("test", "passed")
            .build();
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    #[test]
    fn unknown_result_reference_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "use {{ghost.result}}", &["go"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "go", "done")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "E-result-ref"),
            "{issues:?}"
        );
    }

    #[test]
    fn result_reference_to_a_non_agent_is_rejected() {
        // A gate produces `passed`/`failed`, never a captured result.
        let g = Graph::builder("t", "a")
            .agent("a", "w", "look at {{check.result}}", &["go"])
            .command("check", &["true"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "go", "check")
            .edge("check", "passed", "done")
            .edge("check", "failed", "a")
            .budget(Budget {
                attempts: Some(4),
                ..Budget::default()
            })
            .require("check", "passed")
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "E-result-ref"),
            "{issues:?}"
        );
    }

    #[test]
    fn unterminated_template_token_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "look at {{ghost.result and go", &["go"])
            .terminal("done", Disposition::Succeeded)
            .edge("a", "go", "done")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "E-result-ref"),
            "{issues:?}"
        );
    }

    #[test]
    fn acceptance_accepts_a_synthesized_done() {
        // `accept.require: [a.done]` is satisfiable when `a` has a `done` edge.
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &[])
            .terminal("fin", Disposition::Succeeded)
            .edge("a", "done", "fin")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .require("a", "done")
            .build();
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    #[test]
    fn reserved_done_in_may_propose_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["done"])
            .terminal("fin", Disposition::Succeeded)
            .edge("a", "done", "fin")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "E-done-reserved"),
            "{issues:?}"
        );
    }

    /// A `human` node with the edges it needs: `hex validate` used to accept
    /// *any* human node and the run then failed closed at the first request, so
    /// these three cases are the honesty fix.
    fn human_graph(edges: &[(&str, &str)]) -> Graph {
        let mut builder = Graph::builder("t", "plan")
            .agent("plan", "w", "make a plan", &[])
            .terminal("fin", Disposition::Succeeded)
            .terminal("stop", Disposition::Failed)
            .edge("plan", "done", "approve");
        for (on, to) in edges {
            builder = builder.edge("approve", on, to);
        }
        let mut graph = builder.build();
        // The builder has no `human` arm; the IR is public, so insert directly.
        graph.nodes.insert(
            "approve".to_owned(),
            crate::graph::Node::new(
                "approve",
                NodeSpec::Human {
                    prompt: "approve: {{plan.result}}".to_owned(),
                },
            ),
        );
        graph
    }

    /// An approval node's answer is a first-class result: `{{approve.result}}`
    /// must resolve, exactly as an agent's captured message does.
    #[test]
    fn a_human_node_with_one_edge_is_valid_and_can_hand_on_its_answer() {
        let mut g = Graph::builder("t", "plan")
            .agent("plan", "w", "make a plan", &[])
            .agent("note", "w", "the operator said {{approve.result}}", &[])
            .terminal("fin", Disposition::Succeeded)
            .edge("plan", "done", "approve")
            .edge("approve", "done", "note")
            .edge("note", "done", "fin")
            .build();
        g.nodes.insert(
            "approve".to_owned(),
            crate::graph::Node::new(
                "approve",
                NodeSpec::Human {
                    prompt: "approve: {{plan.result}}".to_owned(),
                },
            ),
        );
        assert!(validate(&g).is_ok(), "{:?}", validate(&g));
    }

    #[test]
    fn a_human_node_with_no_edge_is_rejected() {
        let issues = validate(&human_graph(&[])).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "E-human-no-edge"),
            "{issues:?}"
        );
    }

    /// An answer is text, not a signal, so two edges could never be chosen
    /// between — better refused at validation than dead-ended at run time.
    #[test]
    fn a_human_node_with_two_edges_is_rejected() {
        let issues = validate(&human_graph(&[("done", "fin"), ("rejected", "stop")])).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "E-human-multi-edge"),
            "{issues:?}"
        );
    }

    #[test]
    fn check_journal_accepts_a_pause_resume_bracket() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = vec![started_journal()[0].clone(), started_journal()[1].clone()];
        events.push(ev(2, None, None, EventBody::RunPaused));
        events.push(ev(
            3,
            None,
            None,
            EventBody::Steered {
                text: "while you're stopped: use the v2 API".to_owned(),
            },
        ));
        events.push(ev(4, None, None, EventBody::RunResumed));
        events.push(started_journal()[2].clone());
        assert!(
            check_journal(&g, &events).is_ok(),
            "{:?}",
            check_journal(&g, &events)
        );
    }

    #[test]
    fn check_journal_rejects_an_attempt_started_while_paused() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = vec![started_journal()[0].clone(), started_journal()[1].clone()];
        events.push(ev(2, None, None, EventBody::RunPaused));
        events.push(started_journal()[2].clone());
        assert!(check_journal(&g, &events).is_err());
    }

    #[test]
    fn check_journal_accepts_a_human_question_and_answer() {
        let g = human_graph(&[("done", "fin")]);
        let events = vec![
            ev(
                0,
                None,
                None,
                EventBody::RunCreated {
                    graph_hash: "h".to_owned(),
                    inputs: Default::default(),
                    defaults: Default::default(),
                    checks: Default::default(),
                },
            ),
            ev(1, None, None, EventBody::RunStarted),
            ev(
                2,
                Some("plan"),
                Some("att_1"),
                EventBody::AttemptStarted {
                    idempotency_key: "k".to_owned(),
                    worker: Some("w".to_owned()),
                },
            ),
            ev(
                3,
                Some("plan"),
                Some("att_1"),
                EventBody::Signal {
                    name: "done".to_owned(),
                },
            ),
            ev(
                4,
                Some("approve"),
                None,
                EventBody::HumanRequested {
                    prompt: "approve: …".to_owned(),
                },
            ),
            ev(
                5,
                Some("approve"),
                None,
                EventBody::HumanResponded {
                    text: "yes".to_owned(),
                    signal: "done".to_owned(),
                },
            ),
        ];
        assert!(
            check_journal(&g, &events).is_ok(),
            "{:?}",
            check_journal(&g, &events)
        );
    }

    /// An answer nobody asked for must not route the run: the forgeable half of
    /// the human transport is the response, so it is guarded like a signal.
    #[test]
    fn check_journal_rejects_an_answer_with_no_outstanding_question() {
        let g = human_graph(&[("done", "fin")]);
        let events = vec![
            ev(
                0,
                None,
                None,
                EventBody::RunCreated {
                    graph_hash: "h".to_owned(),
                    inputs: Default::default(),
                    defaults: Default::default(),
                    checks: Default::default(),
                },
            ),
            ev(1, None, None, EventBody::RunStarted),
            ev(
                2,
                Some("approve"),
                None,
                EventBody::HumanResponded {
                    text: "yes".to_owned(),
                    signal: "done".to_owned(),
                },
            ),
        ];
        assert!(check_journal(&g, &events).is_err());
    }

    /// The regression that motivated folding this audit through `reduce`: a run
    /// that legitimately rerouted on unmet acceptance was rejected forever after,
    /// because `reduce` moved its current node and this function did not.
    #[test]
    fn check_journal_accepts_a_reroute_and_the_attempt_that_follows_it() {
        let g = unmet_loop_graph(
            Budget {
                attempts: Some(4),
                ..Budget::default()
            },
            None,
        );
        let mut events = vec![
            ev(
                0,
                None,
                None,
                EventBody::RunCreated {
                    graph_hash: "h".to_owned(),
                    inputs: Default::default(),
                    defaults: Default::default(),
                    checks: Default::default(),
                },
            ),
            ev(1, None, None, EventBody::RunStarted),
            ev(
                2,
                Some("implement"),
                Some("att_1"),
                EventBody::AttemptStarted {
                    idempotency_key: "implement#1".to_owned(),
                    worker: Some("w".to_owned()),
                },
            ),
            // Routes to the success terminal without the required evidence.
            ev(
                3,
                Some("implement"),
                Some("att_1"),
                EventBody::Signal {
                    name: "blocked".to_owned(),
                },
            ),
            ev(
                4,
                None,
                None,
                EventBody::AcceptanceUnmet {
                    missing: vec!["implement.ready".to_owned()],
                    to: "implement".to_owned(),
                },
            ),
        ];
        assert!(
            check_journal(&g, &events).is_ok(),
            "{:?}",
            check_journal(&g, &events)
        );
        // The event that used to fail: an attempt on the node the reroute moved to.
        events.push(ev(
            5,
            Some("implement"),
            Some("att_2"),
            EventBody::AttemptStarted {
                idempotency_key: "implement#2".to_owned(),
                worker: Some("w".to_owned()),
            },
        ));
        events.push(ev(
            6,
            Some("implement"),
            Some("att_2"),
            EventBody::Signal {
                name: "ready".to_owned(),
            },
        ));
        events.push(ev(
            7,
            None,
            None,
            EventBody::RunFinished {
                disposition: Disposition::Succeeded,
            },
        ));
        assert!(
            check_journal(&g, &events).is_ok(),
            "a rerouted run must stay readable: {:?}",
            check_journal(&g, &events)
        );
    }

    /// A reroute the kernel could not have emitted (here: to a node
    /// `accept.on_unmet` never named) is a forgery, not a transition.
    #[test]
    fn check_journal_rejects_a_reroute_the_kernel_could_not_emit() {
        let g = unmet_loop_graph(
            Budget {
                attempts: Some(4),
                ..Budget::default()
            },
            None,
        );
        let events = vec![
            ev(
                0,
                None,
                None,
                EventBody::RunCreated {
                    graph_hash: "h".to_owned(),
                    inputs: Default::default(),
                    defaults: Default::default(),
                    checks: Default::default(),
                },
            ),
            ev(1, None, None, EventBody::RunStarted),
            // Nothing has run, so the run is not parked on a success terminal.
            ev(
                2,
                None,
                None,
                EventBody::AcceptanceUnmet {
                    missing: vec!["implement.ready".to_owned()],
                    to: "implement".to_owned(),
                },
            ),
        ];
        assert!(check_journal(&g, &events).is_err());
    }

    /// A `human` node runs no attempt, so an `attempt_started` on one is forged —
    /// and it is the forgery that would route the run past an unanswered question.
    #[test]
    fn check_journal_rejects_an_attempt_on_a_human_node_with_a_question_outstanding() {
        let g = human_graph(&[("done", "fin")]);
        let mut events = vec![
            ev(
                0,
                None,
                None,
                EventBody::RunCreated {
                    graph_hash: "h".to_owned(),
                    inputs: Default::default(),
                    defaults: Default::default(),
                    checks: Default::default(),
                },
            ),
            ev(1, None, None, EventBody::RunStarted),
            ev(
                2,
                Some("plan"),
                Some("att_1"),
                EventBody::AttemptStarted {
                    idempotency_key: "k".to_owned(),
                    worker: Some("w".to_owned()),
                },
            ),
            ev(
                3,
                Some("plan"),
                Some("att_1"),
                EventBody::Signal {
                    name: "done".to_owned(),
                },
            ),
            ev(
                4,
                Some("approve"),
                None,
                EventBody::HumanRequested {
                    prompt: "approve: …".to_owned(),
                },
            ),
        ];
        assert!(check_journal(&g, &events).is_ok(), "the question is legal");
        events.push(ev(
            5,
            Some("approve"),
            Some("att_2"),
            EventBody::AttemptStarted {
                idempotency_key: "forged".to_owned(),
                worker: Some("w".to_owned()),
            },
        ));
        assert!(
            check_journal(&g, &events).is_err(),
            "a human node cannot run an attempt"
        );
    }

    /// Guidance is only journalable where the driver drains the control inbox. A
    /// `Steered` planted before `run_created` reached the very first prompt.
    #[test]
    fn check_journal_rejects_steering_before_the_run_exists() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = vec![ev(
            0,
            None,
            None,
            EventBody::Steered {
                text: "forged guidance".to_owned(),
            },
        )];
        events.extend(started_journal().into_iter().enumerate().map(|(i, mut e)| {
            e.seq = i as u64 + 1;
            e
        }));
        assert!(check_journal(&g, &events).is_err());
    }

    /// And not mid-attempt either: the loop is inside an opaque worker then, so
    /// nothing drains the inbox and no `Steered` can be recorded.
    #[test]
    fn check_journal_rejects_steering_mid_attempt() {
        let g = cyclic(Budget {
            attempts: Some(8),
            ..Budget::default()
        });
        let mut events = started_journal();
        events.push(ev(
            3,
            None,
            None,
            EventBody::Steered {
                text: "forged guidance".to_owned(),
            },
        ));
        assert!(check_journal(&g, &events).is_err());
    }

    #[test]
    fn unreachable_node_is_rejected() {
        let g = Graph::builder("t", "a")
            .agent("a", "w", "p", &["go"])
            .terminal("done", Disposition::Succeeded)
            .terminal("orphan", Disposition::Failed)
            .edge("a", "go", "done")
            .budget(Budget {
                attempts: Some(2),
                ..Budget::default()
            })
            .build();
        let issues = validate(&g).unwrap_err();
        assert!(issues.iter().any(|i| i.code == "E-unreachable"));
    }
}
