//! Rendering a compiled graph as a diagram someone else's tool draws.
//!
//! `hex graph` renders for a terminal; this renders for a GitHub comment
//! (mermaid) and for graphviz (DOT). Both read exactly what the text renderer
//! reads — [`Topology`] decides which transitions close a loop, and
//! `accept.on_unmet`'s reroute is a transition no `Edge` describes — so a
//! pasted diagram cannot disagree with the listing it came from.

use std::collections::{BTreeMap, BTreeSet};

use hex_runtime::{Context, EdgeClass, Graph, Node, NodeKind, NodeSpec, Topology, Transition};

/// Render `graph` as a mermaid `flowchart`.
///
/// A flowchart rather than `stateDiagram-v2`: hex has four node kinds and two
/// terminal dispositions, and only a flowchart gives each kind its own shape.
/// The delimiters are the classic bracket forms on purpose — GitHub's mermaid
/// trails upstream, and the `A@{ shape: … }` syntax needs 11.3+.
pub fn to_mermaid(graph: &Graph, topo: &Topology) -> String {
    let ids = mermaid_ids(graph);
    let mut out = String::from("flowchart TD\n  start(( ))\n");

    for node in nodes(graph, topo) {
        let Some(id) = ids.get(&node.id) else {
            continue;
        };
        let (open, close) = match node.spec.kind() {
            NodeKind::Agent => ("[\"", "\"]"),
            NodeKind::Command => ("[[\"", "\"]]"),
            NodeKind::Human => ("{{\"", "\"}}"),
            NodeKind::Terminal => ("([\"", "\"])"),
        };
        let lines = label(graph, node).map(|l| mermaid_escape(&l));
        out.push_str(&format!("  {id}{open}{}{close}\n", lines.join("<br/>")));
    }

    if let Some(entry) = ids.get(&graph.entry) {
        out.push_str(&format!("  start --> {entry}\n"));
    }
    for t in transitions(graph, topo) {
        let (Some(from), Some(to)) = (ids.get(&t.from), ids.get(&t.to)) else {
            continue;
        };
        // The reroute is asked about *before* the edge class, because it is not
        // always a back edge: the DFS calls it `Forward` whenever its target is
        // first discovered through the terminal it leaves. Either way it is no
        // step forward — nothing but unmet acceptance takes it.
        let arrow = if t.is_reroute() || t.class == EdgeClass::Back {
            "-.->"
        } else {
            "-->"
        };
        out.push_str(&format!(
            "  {from} {arrow}|{}| {to}\n",
            mermaid_escape(&signal(t))
        ));
    }
    out
}

/// Render `graph` as a graphviz `digraph`.
pub fn to_dot(graph: &Graph, topo: &Topology) -> String {
    let mut out = format!(
        "digraph \"{}\" {{\n  rankdir=TB;\n",
        dot_escape(&graph.name)
    );

    for node in nodes(graph, topo) {
        let shape = match node.spec.kind() {
            NodeKind::Agent => "shape=box, style=rounded",
            NodeKind::Command => "shape=box3d",
            NodeKind::Human => "shape=parallelogram",
            NodeKind::Terminal => "shape=box, peripheries=2",
        };
        // Escape each line, then join: `\n` is DOT's line break, so escaping it
        // as data would print a literal backslash instead of breaking the label.
        let lines = label(graph, node).map(|l| dot_escape(&l));
        out.push_str(&format!(
            "  \"{}\" [label=\"{}\", {shape}];\n",
            dot_escape(&node.id),
            lines.join("\\n")
        ));
    }

    for t in transitions(graph, topo) {
        // `constraint=false` is the attribute that makes the drawing readable:
        // without it graphviz ranks the loop too and the spine bends around it,
        // so the happy path — the thing a reader follows — stops being a line.
        // The reroute takes it whatever the DFS classified it as (see the arrow
        // in `to_mermaid`): it is an acceptance fallback, never part of the spine.
        let attrs = if t.is_reroute() {
            ", style=dotted, constraint=false"
        } else if t.class == EdgeClass::Back {
            ", style=dashed, constraint=false"
        } else {
            ""
        };
        out.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{}\"{attrs}];\n",
            dot_escape(&t.from),
            dot_escape(&t.to),
            dot_escape(&signal(t))
        ));
    }
    out.push_str("}\n");
    out
}

/// Every node in reading order, unreachable ones last. Validation rejects those,
/// but a diagram that drops a node the author wrote is a diagram that lies.
fn nodes<'a>(graph: &'a Graph, topo: &Topology) -> Vec<&'a Node> {
    topo.order
        .iter()
        .chain(topo.unreachable.iter())
        .filter_map(|id| graph.node(id))
        .collect()
}

/// Every transition, grouped by its source in reading order — so the arrows
/// arrive in the order a reader meets the nodes they leave.
fn transitions<'a>(graph: &Graph, topo: &'a Topology) -> Vec<&'a Transition> {
    nodes(graph, topo)
        .iter()
        .flat_map(|node| topo.from(&node.id))
        .collect()
}

/// Mermaid ids for every node: `n_` plus the id reduced to `[A-Za-z0-9_]`.
///
/// A node id is an unvalidated YAML map key, so the raw id can never be emitted:
/// `end` is a mermaid keyword, and a dash or a space is a syntax error. The
/// prefix also keeps a node named `start` clear of the synthetic entry marker.
/// Sanitizing can map two distinct ids onto one name — which would silently draw
/// them as a single node — so a clash takes a numeric suffix.
fn mermaid_ids(graph: &Graph) -> BTreeMap<String, String> {
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut ids = BTreeMap::new();
    for id in graph.nodes.keys() {
        let base: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let base = format!("n_{base}");
        let mut name = base.clone();
        let mut n = 2;
        while !taken.insert(name.clone()) {
            name = format!("{base}_{n}");
            n += 1;
        }
        ids.insert(id.clone(), name);
    }
    ids
}

/// A node's box as its two lines: the real id, then the policy it runs under.
/// Each format escapes them and joins them with its own line break.
///
/// Shorter than the text renderer's line — a box has room for what changes how
/// the node *runs*, and the shape already says which kind it is.
fn label(graph: &Graph, node: &Node) -> [String; 2] {
    let mut parts: Vec<String> = Vec::new();
    match &node.spec {
        NodeSpec::Agent {
            worker,
            context,
            read_only,
            ..
        } => {
            parts.push("agent".to_owned());
            parts.push(worker.clone());
            if *context == Context::Continue {
                parts.push("continue".to_owned());
            }
            if *read_only {
                parts.push("read-only".to_owned());
            }
        }
        NodeSpec::Command { steps, mode } => {
            parts.push("command".to_owned());
            if graph.is_gate(&node.id) {
                parts.push("gate".to_owned());
            }
            parts.push(mode.as_str().to_owned());
            parts.push(format!(
                "{} step{}",
                steps.len(),
                if steps.len() == 1 { "" } else { "s" }
            ));
        }
        NodeSpec::Human { .. } => parts.push("human".to_owned()),
        NodeSpec::Terminal { disposition } => {
            parts.push("terminal".to_owned());
            parts.push(disposition.as_str().to_owned());
        }
    }
    if let Some(v) = node.max_visits {
        parts.push(format!("visits ≤ {v}"));
    }
    [node.id.clone(), parts.join(" · ")]
}

/// The arrow's label. A reroute carries no signal — no event proposes it — and
/// borrows the text renderer's wording for the gap rather than inventing a
/// second name for a transition an operator has already read about elsewhere.
fn signal(t: &Transition) -> String {
    t.on.clone().unwrap_or_else(|| "accept unmet".to_owned())
}

/// Mermaid has no backslash escape inside a quoted label; a literal quote is
/// written as the entity code.
fn mermaid_escape(s: &str) -> String {
    s.replace('"', "#quot;")
}

fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_runtime::Disposition;

    /// The flagship shape: implement → review → done, review loops back, and
    /// `done` reroutes back when acceptance is unmet.
    fn critique_loop() -> Graph {
        let mut graph = Graph::builder("critique-loop", "implement")
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
            .max_visits("review", 4)
            .require("review", "approved")
            .on_unmet("implement")
            .build();
        // The builder has no context setter, and the preset's implementer
        // continues its session (gotcha 38) — a policy the label must show.
        if let Some(NodeSpec::Agent { context, .. }) =
            graph.nodes.get_mut("implement").map(|n| &mut n.spec)
        {
            *context = Context::Continue;
        }
        graph
    }

    /// A reroute the DFS classifies **forward**: `fix` is discovered through the
    /// terminal it reroutes from, so it is not on the stack when the reroute is
    /// walked. The shape is ordinary (a success terminal whose missing evidence
    /// sends the run to a repair node), and it is the case that catches a
    /// renderer keying on the edge class alone.
    fn forward_reroute() -> Graph {
        Graph::builder("forward-reroute", "implement")
            .agent("implement", "implementer", "p", &["done"])
            .terminal("done", Disposition::Succeeded)
            .agent("fix", "implementer", "p", &["done"])
            .edge("implement", "done", "done")
            .edge("fix", "done", "implement")
            .require("implement", "done")
            .on_unmet("fix")
            .build()
    }

    fn graph_with_ids(entry: &str, terminal: &str) -> Graph {
        Graph::builder("g", entry)
            .agent(entry, "implementer", "p", &["done"])
            .terminal(terminal, Disposition::Succeeded)
            .edge(entry, "done", terminal)
            .build()
    }

    #[test]
    fn mermaid_renders_the_flow_its_loop_and_its_reroute() {
        let g = critique_loop();
        let want = "\
flowchart TD
  start(( ))
  n_implement[\"implement<br/>agent · implementer · continue\"]
  n_review[\"review<br/>agent · reviewer · visits ≤ 4\"]
  n_done([\"done<br/>terminal · succeeded\"])
  start --> n_implement
  n_implement -->|ready_for_review| n_review
  n_review -->|approved| n_done
  n_review -.->|changes_requested| n_implement
  n_done -.->|accept unmet| n_implement
";
        assert_eq!(to_mermaid(&g, &Topology::of(&g)), want);
    }

    /// Node ids are unvalidated YAML keys: `end` is a mermaid keyword and a dash
    /// is a syntax error, so neither may reach the id — and both must survive in
    /// the label, which is the name the operator actually typed.
    #[test]
    fn a_reserved_or_punctuated_id_is_sanitized_but_still_shown() {
        let g = graph_with_ids("re-view", "end");
        let out = to_mermaid(&g, &Topology::of(&g));
        assert!(out.contains("n_re_view[\"re-view<br/>"), "{out}");
        assert!(out.contains("n_end([\"end<br/>"), "{out}");
        assert!(out.contains("  start --> n_re_view\n"), "{out}");
        assert!(
            !out.contains(" end("),
            "the raw id would break the flowchart"
        );
    }

    /// Without `constraint=false` graphviz ranks the loop and bends the spine
    /// around it, which is the whole reason to emit DOT rather than a bare list.
    #[test]
    fn every_back_edge_and_the_reroute_are_unconstrained_in_dot() {
        let g = critique_loop();
        let topo = Topology::of(&g);
        let dot = to_dot(&g, &topo);

        for t in topo
            .transitions
            .iter()
            .filter(|t| t.class == EdgeClass::Back)
        {
            let arrow = format!("\"{}\" -> \"{}\"", t.from, t.to);
            let line = dot
                .lines()
                .find(|l| l.contains(&arrow))
                .unwrap_or_else(|| panic!("{arrow} is missing from\n{dot}"));
            assert!(line.contains("constraint=false"), "{line}");
        }
        assert!(
            dot.contains("\"review\" -> \"implement\" [label=\"changes_requested\", style=dashed, constraint=false];"),
            "{dot}"
        );
        assert!(
            dot.contains(
                "\"done\" -> \"implement\" [label=\"accept unmet\", style=dotted, constraint=false];"
            ),
            "{dot}"
        );
        // A forward edge stays constrained, or the spine would not be a line.
        assert!(
            dot.contains("\"implement\" -> \"review\" [label=\"ready_for_review\"];"),
            "{dot}"
        );
        assert!(
            dot.starts_with("digraph \"critique-loop\" {\n  rankdir=TB;\n"),
            "{dot}"
        );
        assert!(
            dot.contains(
                "\"done\" [label=\"done\\nterminal · succeeded\", shape=box, peripheries=2];"
            ),
            "{dot}"
        );
    }

    /// The reroute is dotted and unconstrained because of what it *is*, not
    /// because of where the DFS happened to meet it.
    #[test]
    fn a_forward_classified_reroute_is_still_drawn_as_a_reroute() {
        let g = forward_reroute();
        let topo = Topology::of(&g);

        let reroute = topo
            .transitions
            .iter()
            .find(|t| t.is_reroute())
            .expect("the on_unmet reroute is a transition");
        assert_eq!(
            reroute.class,
            EdgeClass::Forward,
            "this graph exists to exercise a forward-classified reroute"
        );

        assert!(
            to_mermaid(&g, &topo).contains("  n_done -.->|accept unmet| n_fix\n"),
            "{}",
            to_mermaid(&g, &topo)
        );
        assert!(
            to_dot(&g, &topo).contains(
                "\"done\" -> \"fix\" [label=\"accept unmet\", style=dotted, constraint=false];"
            ),
            "{}",
            to_dot(&g, &topo)
        );
    }

    #[test]
    fn a_quote_in_a_label_is_escaped_in_both_formats() {
        let g = graph_with_ids("say \"hi\"", "done");
        let topo = Topology::of(&g);
        assert!(
            to_mermaid(&g, &topo).contains("n_say__hi_[\"say #quot;hi#quot;<br/>"),
            "{}",
            to_mermaid(&g, &topo)
        );
        assert!(
            to_dot(&g, &topo).contains("\"say \\\"hi\\\"\" [label=\"say \\\"hi\\\"\\n"),
            "{}",
            to_dot(&g, &topo)
        );
    }
}
