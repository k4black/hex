//! Pure IR validation — the minimum honest set for the slim MVP.
//!
//! Operates on the compiled [`Graph`] only (surface parsing lives in the
//! runtime). Catches the errors that make a run undefined *before* any worker
//! spends a token: dangling references, unreachable nodes, unbounded cycles,
//! and agent proposals that cannot route.

use std::collections::BTreeSet;

use hex_proto::{Disposition, Event, EventBody, PROTOCOL_VERSION};

use crate::graph::{Graph, NodeKind, NodeSpec};

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

    // Unbounded cycles are an error, not a warning.
    if has_cycle(graph) && !graph.budget.bounds_cycles() {
        issues.push(Issue::new(
            "E-unbounded-cycle",
            "graph contains a cycle but declares no bound (set budget.attempts \
             or budget.cycle_visits)",
        ));
    }

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
    // At least one terminal must be reachable so the run can end.
    let has_terminal = graph
        .nodes
        .values()
        .any(|n| reachable.contains(n.id.as_str()) && n.spec.kind() == NodeKind::Terminal);
    if !has_terminal {
        issues.push(Issue::new(
            "E-no-terminal",
            "no terminal node is reachable from entry",
        ));
    }
}

fn check_routing(graph: &Graph, issues: &mut Vec<Issue>) {
    for node in graph.nodes.values() {
        let edge_signals: BTreeSet<&str> = graph.signals_from(&node.id).into_iter().collect();
        match &node.spec {
            NodeSpec::Agent { may_propose, .. } => {
                let proposable: BTreeSet<&str> = may_propose.iter().map(String::as_str).collect();
                // Every proposable event must have an edge...
                for sig in &proposable {
                    if !edge_signals.contains(sig) {
                        issues.push(Issue::new(
                            "E-proposal-no-edge",
                            format!("agent `{}` may propose `{sig}` but no edge handles it", node.id),
                        ));
                    }
                }
                // ...and every outgoing edge must be proposable.
                for sig in &edge_signals {
                    if !proposable.contains(sig) {
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
            NodeSpec::Gate { .. } | NodeSpec::Command { .. } => {
                for sig in &edge_signals {
                    if *sig != "passed" && *sig != "failed" {
                        issues.push(Issue::new(
                            "E-gate-signal",
                            format!(
                                "gate/command `{}` has an edge on `{sig}` (only `passed`/`failed`)",
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
            NodeSpec::Human { .. } => {}
        }
    }
}

fn check_acceptance(graph: &Graph, issues: &mut Vec<Issue>) {
    for req in &graph.accept {
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
            NodeSpec::Agent { may_propose, .. } => may_propose.iter().any(|s| s == &req.signal),
            NodeSpec::Gate { .. } | NodeSpec::Command { .. } => {
                req.signal == "passed" || req.signal == "failed"
            }
            NodeSpec::Terminal { .. } | NodeSpec::Human { .. } => false,
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
/// silently mutating the projection. Graph-aware: it tracks the current node
/// exactly as [`reduce`](crate::reduce) does (entry, then routing on signals),
/// and requires every attempt start to target that node — so verification and
/// reduction can never disagree. Complements the byte-level integrity the
/// runtime's journal reader enforces (contiguous seq, schema, single run id).
///
/// # Errors
/// Returns the first lifecycle violation found.
pub fn check_journal(graph: &Graph, events: &[Event]) -> Result<(), Issue> {
    #[derive(PartialEq)]
    enum Phase {
        Init,
        Created,
        Running,
        Finished,
    }
    let mut phase = Phase::Init;
    // The projected current node (by routing) and the in-flight attempt.
    let mut current: Option<&str> = None;
    let mut active: Option<(&str, &str)> = None;

    for (i, e) in events.iter().enumerate() {
        if e.schema_version != PROTOCOL_VERSION {
            return Err(bad(i, format!("unsupported schema_version {}", e.schema_version)));
        }
        if phase == Phase::Finished {
            // Only inert diagnostics may trail a terminal.
            if !matches!(e.body, EventBody::Note { .. }) {
                return Err(bad(i, "event after the run finished"));
            }
            continue;
        }
        match &e.body {
            EventBody::RunCreated { .. } => {
                if phase != Phase::Init {
                    return Err(bad(i, "run_created must be the first event"));
                }
                phase = Phase::Created;
            }
            EventBody::RunStarted => {
                if phase != Phase::Created {
                    return Err(bad(i, "run_started out of order"));
                }
                phase = Phase::Running;
                current = Some(graph.entry.as_str());
            }
            EventBody::AttemptStarted { .. } => {
                if phase != Phase::Running || active.is_some() {
                    return Err(bad(i, "attempt_started while not idle-running"));
                }
                match (e.node_id.as_deref(), e.attempt_id.as_deref()) {
                    (Some(n), Some(a)) => {
                        if Some(n) != current {
                            return Err(bad(i, "attempt_started does not target the current node"));
                        }
                        active = Some((n, a));
                    }
                    _ => return Err(bad(i, "attempt_started missing node/attempt id")),
                }
            }
            EventBody::Signal { name } => {
                if !attempt_matches(active, e) {
                    return Err(bad(i, "signal does not match the in-flight attempt"));
                }
                active = None;
                // Advance the current node exactly as the reducer routes; a
                // signal with no legal edge is a defensive run failure.
                match current.and_then(|c| graph.route(c, name)) {
                    Some(to) => current = Some(to),
                    None => phase = Phase::Finished,
                }
            }
            EventBody::AttemptFailed { disposition, .. } => {
                if !attempt_matches(active, e) {
                    return Err(bad(i, "attempt_failed does not match the in-flight attempt"));
                }
                if !matches!(disposition, Disposition::Failed | Disposition::TimedOut) {
                    return Err(bad(i, "attempt_failed carries a non-failure disposition"));
                }
                active = None;
                phase = Phase::Finished; // a disposition-bearing failure is terminal
            }
            EventBody::AttemptInterrupted => {
                if !attempt_matches(active, e) {
                    return Err(bad(i, "attempt_interrupted does not match the in-flight attempt"));
                }
                active = None; // current stays; the node is re-attempted
            }
            EventBody::RunFinished { .. } => {
                if active.is_some() {
                    return Err(bad(i, "run_finished while an attempt is still in flight"));
                }
                phase = Phase::Finished;
            }
            EventBody::BudgetExhausted { .. } | EventBody::Note { .. } => {}
        }
    }
    Ok(())
}

/// Whether `event`'s node/attempt ids match the in-flight attempt.
fn attempt_matches(active: Option<(&str, &str)>, event: &Event) -> bool {
    match active {
        Some((node, attempt)) => {
            event.node_id.as_deref() == Some(node) && event.attempt_id.as_deref() == Some(attempt)
        }
        None => false,
    }
}

fn bad(index: usize, why: impl Into<String>) -> Issue {
    Issue::new("E-journal-lifecycle", format!("event {index}: {}", why.into()))
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

/// DFS back-edge detection over the edge graph.
fn has_cycle(graph: &Graph) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    fn visit<'a>(
        graph: &'a Graph,
        id: &'a str,
        marks: &mut std::collections::BTreeMap<&'a str, Mark>,
    ) -> bool {
        marks.insert(id, Mark::Open);
        for edge in graph.edges.iter().filter(|e| e.from == id) {
            match marks.get(edge.to.as_str()) {
                Some(Mark::Open) => return true,
                Some(Mark::Done) => {}
                None => {
                    if visit(graph, edge.to.as_str(), marks) {
                        return true;
                    }
                }
            }
        }
        marks.insert(id, Mark::Done);
        false
    }

    let mut marks = std::collections::BTreeMap::new();
    graph
        .nodes
        .keys()
        .any(|id| !marks.contains_key(id.as_str()) && visit(graph, id, &mut marks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Budget, Graph};
    use hex_proto::Disposition;

    fn cyclic(budget: Budget) -> Graph {
        Graph::builder("t", "implement")
            .agent("implement", "codex", "p", &["ready"])
            .gate("test", &["true"])
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
