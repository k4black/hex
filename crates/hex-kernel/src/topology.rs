//! Reading order and cycle structure of a compiled [`Graph`].
//!
//! A hex graph is cyclic by design (core rule 5 bounds cycles rather than
//! banning them), so a plain topological sort does not apply. This module runs
//! the standard "classify, break, layer" pipeline once, in the kernel, and hands
//! the result to its consumers: the `hex graph` text renderer and the runtime's
//! live progress strip.
//!
//! The validator does **not** consume this: with every non-terminal node
//! visit-bounded by the loader there is no cycle analysis left to run, and a
//! rendering classifier could not express one anyway. This module and
//! `schedule` read the same single source, [`Graph::implicit_reroutes`], so they
//! cannot disagree about which transitions are implicit.

use std::collections::BTreeMap;

use crate::graph::{Graph, NodeSpec};
use hex_proto::Disposition;

/// What a transition does to the reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeClass {
    /// Goes forward: its target is rendered below its source.
    Forward,
    /// Closes a loop: its target was already on the DFS stack. This is the edge
    /// a renderer marks rather than draws, and the edge a cycle is named by.
    Back,
}

/// One transition, with the class the traversal assigned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// Source node id.
    pub from: String,
    /// The signal that activates it — `None` for an implicit `accept.on_unmet`
    /// reroute, which no `Edge` describes and no signal triggers.
    pub on: Option<String>,
    /// Target node id.
    pub to: String,
    /// Forward or back.
    pub class: EdgeClass,
}

impl Transition {
    /// Whether this is the implicit reroute rather than an authored edge.
    #[must_use]
    pub fn is_reroute(&self) -> bool {
        self.on.is_none()
    }
}

/// A loop in the graph, and what stops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    /// The nodes on the loop, starting at the back edge's target.
    pub nodes: Vec<String>,
    /// Whether the loop is closed by an `accept.on_unmet` reroute rather than an
    /// authored edge — worth saying, because it is invisible in the YAML.
    pub via_reroute: bool,
    /// The tightest bound that stops it, already resolved to a sentence. `None`
    /// means no node on the loop carries a visit bound — only possible for a
    /// hand-built IR, since the loader defaults every non-terminal node to one.
    pub bounded_by: Option<String>,
}

/// The reading order and loop structure of a graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// Node ids in render order: entry first, then by distance from it, with
    /// terminals last within their rank and authored order breaking ties.
    pub order: Vec<String>,
    /// Every transition, classified.
    pub transitions: Vec<Transition>,
    /// Nodes the entry cannot reach. Validation rejects these
    /// (`E-unreachable`), but a renderer must still show them rather than
    /// silently dropping a node the author wrote.
    pub unreachable: Vec<String>,
    /// Every loop, with its bound.
    pub cycles: Vec<Cycle>,
}

impl Topology {
    /// Classify, rank and order `graph`.
    #[must_use]
    pub fn of(graph: &Graph) -> Self {
        let out = outgoing(graph);
        let mut walk = Walk::new(&out);
        walk.visit(&graph.entry);
        // Anything the entry cannot reach still gets a deterministic order.
        let unreachable: Vec<String> = graph
            .nodes
            .keys()
            .filter(|id| !walk.discovery.contains_key(*id))
            .cloned()
            .collect();

        let rank = rank_forward(&walk.transitions, &graph.entry, &walk.discovery);
        let mut order: Vec<String> = walk.discovery.keys().cloned().collect();
        order.sort_by_key(|id| {
            (
                rank.get(id).copied().unwrap_or(u32::MAX),
                // Terminals sink to the bottom of their rank: a failure sink is
                // an aside, not a step in the flow.
                terminal_weight(graph, id),
                walk.discovery.get(id).copied().unwrap_or(usize::MAX),
            )
        });

        let mut cycles: Vec<Cycle> = walk
            .transitions
            .iter()
            .filter(|t| t.class == EdgeClass::Back)
            .map(|t| {
                let nodes = walk.path_between(&t.to, &t.from);
                Cycle {
                    bounded_by: tightest_bound(graph, &nodes),
                    via_reroute: t.is_reroute(),
                    nodes,
                }
            })
            .collect();
        // Tightest loop first. DFS emits a cycle when it *closes*, so a longer
        // outer loop can be found before the inner one it contains; a reader
        // scanning "what can spin here" wants the small one first.
        cycles.sort_by(|a, b| {
            a.nodes
                .len()
                .cmp(&b.nodes.len())
                .then_with(|| a.nodes.cmp(&b.nodes))
        });

        Self {
            order,
            transitions: walk.transitions,
            unreachable,
            cycles,
        }
    }

    /// The transitions leaving `node`, in authored order.
    #[must_use]
    pub fn from(&self, node: &str) -> Vec<&Transition> {
        self.transitions.iter().filter(|t| t.from == node).collect()
    }

    /// Whether `next` immediately follows `prev` in render order *and* is
    /// actually reachable from it in one forward step.
    ///
    /// This is what lets a renderer draw a continuous gutter down the happy path
    /// and break it before an unrelated sink: a rail that merely tracked reading
    /// order would assert a transition that does not exist.
    #[must_use]
    pub fn flows_into(&self, prev: &str, next: &str) -> bool {
        self.transitions
            .iter()
            .any(|t| t.from == prev && t.to == next && t.class == EdgeClass::Forward)
    }
}

/// Adjacency in authored order: explicit edges first, then the implicit
/// reroutes, which are conceptually the last thing a terminal does.
fn outgoing(graph: &Graph) -> BTreeMap<String, Vec<(Option<String>, String)>> {
    let mut out: BTreeMap<String, Vec<(Option<String>, String)>> = BTreeMap::new();
    for edge in &graph.edges {
        out.entry(edge.from.clone())
            .or_default()
            .push((Some(edge.on.clone()), edge.to.clone()));
    }
    for (from, to) in graph.implicit_reroutes() {
        out.entry(from.to_owned())
            .or_default()
            .push((None, to.to_owned()));
    }
    out
}

/// Depth-first traversal that colours nodes to classify back edges.
struct Walk<'a> {
    out: &'a BTreeMap<String, Vec<(Option<String>, String)>>,
    /// Discovery index per node — also the "visited" set.
    discovery: BTreeMap<String, usize>,
    /// Nodes currently on the stack (grey). A transition into one closes a loop.
    on_stack: Vec<String>,
    /// Tree parent, for recovering the path a cycle runs through.
    parent: BTreeMap<String, String>,
    transitions: Vec<Transition>,
    next_index: usize,
}

impl<'a> Walk<'a> {
    fn new(out: &'a BTreeMap<String, Vec<(Option<String>, String)>>) -> Self {
        Self {
            out,
            discovery: BTreeMap::new(),
            on_stack: Vec::new(),
            parent: BTreeMap::new(),
            transitions: Vec::new(),
            next_index: 0,
        }
    }

    fn visit(&mut self, node: &str) {
        if self.discovery.contains_key(node) {
            return;
        }
        self.discovery.insert(node.to_owned(), self.next_index);
        self.next_index += 1;
        self.on_stack.push(node.to_owned());

        for (on, to) in self.out.get(node).cloned().unwrap_or_default() {
            // Grey target ⇒ it is an ancestor on the current path ⇒ this closes
            // a loop. Everything else points forward or across, and either way
            // the target renders below this node.
            let class = if self.on_stack.iter().any(|n| n == &to) {
                EdgeClass::Back
            } else {
                EdgeClass::Forward
            };
            self.transitions.push(Transition {
                from: node.to_owned(),
                on,
                to: to.clone(),
                class,
            });
            if class == EdgeClass::Forward && !self.discovery.contains_key(&to) {
                self.parent.insert(to.clone(), node.to_owned());
                self.visit(&to);
            }
        }
        self.on_stack.pop();
    }

    /// The tree path from `from` down to `to`, inclusive — the nodes a back edge
    /// loops through.
    fn path_between(&self, from: &str, to: &str) -> Vec<String> {
        let mut back = vec![to.to_owned()];
        let mut cur = to.to_owned();
        while cur != from {
            let Some(up) = self.parent.get(&cur) else {
                break;
            };
            cur = up.clone();
            back.push(cur.clone());
        }
        back.reverse();
        back
    }
}

/// Longest-path ranking over forward transitions, so a node always sits below
/// *every* predecessor and no rendered forward arrow ever points upward.
fn rank_forward(
    transitions: &[Transition],
    entry: &str,
    discovery: &BTreeMap<String, usize>,
) -> BTreeMap<String, u32> {
    let mut rank: BTreeMap<String, u32> = BTreeMap::new();
    rank.insert(entry.to_owned(), 0);
    // Relax in discovery order, repeatedly: the forward subgraph is a DAG, so
    // |V| passes always converge, and these graphs are tiny.
    for _ in 0..discovery.len().max(1) {
        let mut changed = false;
        for t in transitions.iter().filter(|t| t.class == EdgeClass::Forward) {
            let Some(&from) = rank.get(&t.from) else {
                continue;
            };
            let want = from + 1;
            if rank.get(&t.to).is_none_or(|&cur| cur < want) {
                rank.insert(t.to.clone(), want);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    rank
}

/// Sort weight pushing terminals below live nodes of the same rank, and a
/// failure sink below a success one.
fn terminal_weight(graph: &Graph, id: &str) -> u8 {
    match graph.node(id).map(|n| &n.spec) {
        Some(NodeSpec::Terminal { disposition }) => match disposition {
            Disposition::Succeeded => 1,
            _ => 2,
        },
        _ => 0,
    }
}

/// The tightest bound stopping a loop through `nodes`, as a phrase.
///
/// Rendering *which* bound stops a loop is the highest-value line about a design
/// whose central rule is that every cycle is bounded — "bounded" alone tells a
/// reader nothing they can act on. The answer is always a per-node visit bound:
/// the only bound there is.
fn tightest_bound(graph: &Graph, nodes: &[String]) -> Option<String> {
    nodes
        .iter()
        .filter_map(|id| {
            let visits = graph.node(id)?.max_visits?;
            Some((visits, format!("{id} visits ≤ {visits}")))
        })
        .min_by_key(|(visits, _)| *visits)
        .map(|(_, phrase)| phrase)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// implement → review → done, review loops back, and `done` reroutes back on
    /// unmet acceptance: the flagship shape.
    fn critique_loop() -> Graph {
        Graph::builder("critique-loop", "implement")
            .agent("implement", "implementer", "p", &["ready_for_review"])
            .agent(
                "review",
                "reviewer",
                "p",
                &["approved", "changes_requested"],
            )
            .terminal("done", Disposition::Succeeded)
            .edge("implement", "ready_for_review", "review")
            .edge("review", "approved", "done")
            .edge("review", "changes_requested", "implement")
            .max_visits("implement", 5)
            .max_visits("review", 4)
            .require("review", "approved")
            .on_unmet("implement")
            .build()
    }

    #[test]
    fn order_starts_at_the_entry_and_sinks_terminals() {
        let t = Topology::of(&critique_loop());
        assert_eq!(t.order, ["implement", "review", "done"]);
    }

    /// Alphabetical order put the entry last and the terminal first, which is
    /// what made the old rendering unreadable.
    #[test]
    fn order_is_not_alphabetical() {
        let t = Topology::of(&critique_loop());
        let mut sorted = t.order.clone();
        sorted.sort();
        assert_ne!(t.order, sorted);
    }

    #[test]
    fn the_loop_edge_is_classified_back_and_the_rest_forward() {
        let t = Topology::of(&critique_loop());
        let back: Vec<_> = t
            .transitions
            .iter()
            .filter(|t| t.class == EdgeClass::Back)
            .map(|t| (t.from.as_str(), t.to.as_str()))
            .collect();
        // DFS order: `done`'s reroute closes while `review` is still on the stack.
        assert_eq!(back, [("done", "implement"), ("review", "implement")]);
    }

    /// The reroute closes a real loop that no `Edge` describes — the case that
    /// makes edge-only cycle detection wrong.
    #[test]
    fn the_implicit_reroute_is_a_transition_with_no_signal() {
        let t = Topology::of(&critique_loop());
        let reroute = t
            .transitions
            .iter()
            .find(|t| t.is_reroute())
            .expect("the on_unmet reroute is a transition");
        assert_eq!(
            (reroute.from.as_str(), reroute.to.as_str()),
            ("done", "implement")
        );
        assert_eq!(reroute.class, EdgeClass::Back);
    }

    #[test]
    fn cycles_name_the_bound_that_stops_them() {
        let t = Topology::of(&critique_loop());
        assert_eq!(t.cycles.len(), 2);
        assert_eq!(t.cycles[0].nodes, ["implement", "review"]);
        assert_eq!(t.cycles[0].bounded_by.as_deref(), Some("review visits ≤ 4"));
        assert!(t.cycles[1].via_reroute);
        assert_eq!(t.cycles[1].nodes, ["implement", "review", "done"]);
    }

    /// The tightest bound on the loop is the one named — naming a looser one
    /// would send a reader to the wrong knob.
    #[test]
    fn the_tightest_visit_bound_on_the_loop_is_named() {
        let mut g = critique_loop();
        g.nodes.get_mut("implement").expect("implement").max_visits = Some(2);
        let t = Topology::of(&g);
        assert_eq!(
            t.cycles[0].bounded_by.as_deref(),
            Some("implement visits ≤ 2")
        );
    }

    /// The gutter must not claim a transition that does not exist.
    #[test]
    fn flows_into_follows_real_forward_transitions_only() {
        let t = Topology::of(&critique_loop());
        assert!(t.flows_into("implement", "review"));
        assert!(t.flows_into("review", "done"));
        // Adjacent in render order is not the same as connected.
        assert!(!t.flows_into("done", "implement"), "that is the back edge");
    }

    #[test]
    fn an_unreachable_node_is_reported_rather_than_dropped() {
        let mut g = critique_loop();
        g.nodes.insert(
            "orphan".to_owned(),
            crate::graph::Node::new(
                "orphan",
                NodeSpec::Terminal {
                    disposition: Disposition::Failed,
                },
            ),
        );
        let t = Topology::of(&g);
        assert_eq!(t.unreachable, ["orphan"]);
        assert!(!t.order.contains(&"orphan".to_owned()));
    }
}
