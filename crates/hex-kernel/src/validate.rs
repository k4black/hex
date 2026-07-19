//! Pure IR validation — the minimum honest set for the slim MVP.
//!
//! Operates on the compiled [`Graph`] only (surface parsing lives in the
//! runtime). Catches the errors that make a run undefined *before* any worker
//! spends a token: dangling references, unreachable nodes, unbounded cycles,
//! and agent proposals that cannot route.

use std::collections::BTreeSet;

use hex_proto::Disposition;

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
        if !graph.nodes.contains_key(&req.node) {
            issues.push(Issue::new(
                "E-accept-node",
                format!("acceptance requires unknown node `{}`", req.node),
            ));
        }
    }
    // A success terminal with an empty contract is legal but worth nothing;
    // the MVP does not warn on it.
    let _ = Disposition::Succeeded;
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
