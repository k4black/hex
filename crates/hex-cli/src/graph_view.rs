//! Rendering a compiled graph for a human.
//!
//! `hex graph` used to print nodes and edges in alphabetical order, which put
//! `tdd`'s entry node last and its terminal first — a listing of facts in the
//! order least likely to explain anything. This renders the graph as a *flow*:
//! entry first, each node with the policy it will run under, its outgoing
//! transitions labelled by the signal that takes them, loops marked with the
//! bound that stops them.
//!
//! Ordering and cycle structure come from [`hex_runtime::Topology`] in the
//! kernel, so this file decides how things look and never what is true.

use std::collections::BTreeMap;

use hex_runtime::{Budget, EdgeClass, Graph, NodeKind, NodeSpec, Topology, Transition};

use crate::glyphs::Glyphs;

/// Render `graph` as text. `origin` is where it came from (a path, or
/// `built-in:<name>`), shown because "which of the three layers am I looking
/// at" is a question a preset name alone cannot answer.
pub fn render(
    graph: &Graph,
    origin: &str,
    description: Option<&str>,
    bindings: &BTreeMap<String, String>,
    g: &Glyphs,
) -> String {
    let topo = Topology::of(graph);
    let mut out = String::new();

    header(&mut out, graph, origin, description, g);
    flow(&mut out, graph, &topo, bindings, g);
    cycles(&mut out, &topo, g);
    summary(&mut out, graph, &topo, g);
    out
}

/// Name, origin, entry, budget and the acceptance contract — `kubectl describe`
/// shape: a fixed key column, one fact per line, no box.
fn header(out: &mut String, graph: &Graph, origin: &str, description: Option<&str>, g: &Glyphs) {
    out.push_str(&format!("{:<10}{:<48} {origin}\n", "Graph", graph.name));
    if let Some(d) = description {
        out.push_str(&format!("{:<10}{d}\n", ""));
    }
    out.push_str(&format!("{:<10}{}\n", "Entry", graph.entry));
    out.push_str(&format!(
        "{:<10}{}\n",
        "Budget",
        budget_line(&graph.budget, g)
    ));

    if !graph.accept.require.is_empty() || graph.accept.on_unmet.is_some() {
        let mut first = true;
        for req in &graph.accept.require {
            let key = if first { "Accept" } else { "" };
            let label = if first { "require" } else { "" };
            out.push_str(&format!(
                "{key:<10}{label:<10}{}.{}\n",
                req.node, req.signal
            ));
            first = false;
        }
        if let Some(to) = &graph.accept.on_unmet {
            let key = if first { "Accept" } else { "" };
            out.push_str(&format!(
                "{key:<10}{:<10}{to}   a success terminal missing evidence reroutes here\n",
                "on_unmet"
            ));
        }
    }
}

fn budget_line(budget: &Budget, g: &Glyphs) -> String {
    let mut parts = Vec::new();
    if let Some(a) = budget.attempts {
        parts.push(format!("{a} attempts"));
    }
    if let Some(ms) = budget.elapsed_ms {
        parts.push(format!("{} elapsed", human_ms(ms)));
    }
    if let Some(ms) = budget.attempt_elapsed_ms {
        parts.push(format!("{} per attempt", human_ms(ms)));
    }
    if let Some(v) = budget.cycle_visits {
        parts.push(format!("{v} visits per node"));
    }
    if let Some(t) = budget.output_tokens {
        parts.push(format!("{t} generated tokens"));
    }
    if parts.is_empty() {
        return "unbounded".to_owned();
    }
    parts.join(&format!(" {} ", g.sep))
}

/// The flow: every node in reading order with its policy and transitions.
fn flow(
    out: &mut String,
    graph: &Graph,
    topo: &Topology,
    bindings: &BTreeMap<String, String>,
    g: &Glyphs,
) {
    out.push_str("\nFlow\n");
    for (i, id) in topo.order.iter().enumerate() {
        let Some(node) = graph.node(id) else { continue };
        // The gutter asserts a real transition, so it breaks before a node that
        // does not follow from the previous one — a detached failure sink reads
        // as detached instead of as the next step.
        let continues = topo
            .order
            .get(i + 1)
            .is_some_and(|next| topo.flows_into(id, next));
        let rail = match (i, continues) {
            (0, _) => g.entry,
            (_, true) => g.rail,
            (_, false) => g.rail_end,
        };
        let gutter = if i > 0 && !flows_from_any_prior(topo, &topo.order[..i], id) {
            // Nothing above reaches it: do not draw it onto the rail at all.
            " "
        } else {
            rail
        };

        out.push_str(&format!(
            "  {gutter} {} {id:<12} {}\n",
            badge(graph, id, g),
            policy(graph, node, bindings, g)
        ));
        // Continuation rows keep the gutter only while more nodes follow *and*
        // this one is on the rail; a detached sink's details hang free.
        let more = i + 1 < topo.order.len();
        // Only a *continuing* rail carries on below a node. After `└` the path
        // has ended, so its own detail lines must not re-draw a gutter.
        let cont = if more && (gutter == g.rail || gutter == g.entry) {
            g.rail
        } else {
            " "
        };
        for step in steps(node) {
            out.push_str(&format!("  {cont}     {step}\n"));
        }
        for t in topo.from(id) {
            out.push_str(&format!("  {cont}     {}\n", transition(t, topo, g)));
        }
        if more {
            // A blank separator row is blank: `"  "` is trailing whitespace, and
            // it shows up in every diff of a rendering someone pasted into docs.
            out.push_str(&format!("{}\n", format!("  {cont}").trim_end()));
        }
    }

    if !topo.unreachable.is_empty() {
        out.push_str("\nUnreachable (validation rejects these)\n");
        for id in &topo.unreachable {
            out.push_str(&format!("    {} {id}\n", badge(graph, id, g)));
        }
    }
}

/// Whether anything already rendered flows into `id` — decides between drawing
/// it on the rail and leaving it detached.
fn flows_from_any_prior(topo: &Topology, prior: &[String], id: &str) -> bool {
    prior.iter().any(|p| topo.flows_into(p, id))
}

fn badge(graph: &Graph, id: &str, g: &Glyphs) -> &'static str {
    match graph.node(id).map(|n| n.spec.kind()) {
        Some(NodeKind::Agent) => g.agent,
        Some(NodeKind::Command) if graph.is_gate(id) => g.gate,
        Some(NodeKind::Command) => g.command,
        Some(NodeKind::Human) => g.human,
        Some(NodeKind::Terminal) => match graph.node(id).map(|n| &n.spec) {
            Some(NodeSpec::Terminal { disposition }) if disposition.as_str() == "succeeded" => g.ok,
            _ => g.fail,
        },
        None => " ",
    }
}

/// The one-line policy summary: what this node runs as, and under what bounds.
fn policy(
    graph: &Graph,
    node: &hex_runtime::Node,
    bindings: &BTreeMap<String, String>,
    g: &Glyphs,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    match &node.spec {
        NodeSpec::Agent {
            worker,
            context,
            read_only,
            ..
        } => {
            parts.push("agent".to_owned());
            // Both halves of the binding: a graph names a role, and which CLI
            // it lands on is exactly what you want when reading someone else's
            // graph — it is also the only way gotcha 19's shadowing is visible.
            parts.push(match bindings.get(worker) {
                Some(program) if program != worker => format!("{worker} {} {program}", g.binds),
                _ => worker.clone(),
            });
            parts.push(
                if *context == hex_runtime::Context::Continue {
                    "session continues"
                } else {
                    "fresh"
                }
                .to_owned(),
            );
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
        NodeSpec::Human { .. } => {
            parts.push("human".to_owned());
            parts.push("blocks for an operator answer".to_owned());
        }
        NodeSpec::Terminal { disposition } => {
            parts.push("terminal".to_owned());
            parts.push(disposition.as_str().to_owned());
        }
    }
    if let Some(v) = node.max_visits {
        parts.push(format!("visits {} {v}", g.le));
    }
    parts.join(&format!(" {} ", g.sep))
}

/// A command node's resolved argv, one per line.
///
/// The *resolved* argv, not the check name: gotcha 9's whole point is that a
/// gate whose verdict means nothing is worse than no gate, so the thing a reader
/// has to be able to audit is the command that will actually run.
fn steps(node: &hex_runtime::Node) -> Vec<String> {
    let NodeSpec::Command { steps, .. } = &node.spec else {
        return Vec::new();
    };
    steps
        .iter()
        .map(|s| format!("  {:<10} {}", s.label(), s.argv.join(" ")))
        .collect()
}

/// One outgoing transition, with its signal and — for a loop — its bound.
fn transition(t: &Transition, topo: &Topology, g: &Glyphs) -> String {
    let (arrow, note) = match (t.class, t.is_reroute()) {
        (EdgeClass::Back, true) => (g.reroute, "   accept.on_unmet".to_owned()),
        (EdgeClass::Back, false) => (g.back, cycle_note(topo, t, g)),
        (EdgeClass::Forward, _) => (g.forward, String::new()),
    };
    let signal = t.on.clone().unwrap_or_else(|| "accept unmet".to_owned());
    format!("{signal:<20}{arrow} {}{note}", t.to)
}

fn cycle_note(topo: &Topology, t: &Transition, g: &Glyphs) -> String {
    topo.cycles
        .iter()
        .position(|c| c.nodes.contains(&t.to) && c.nodes.contains(&t.from))
        .map_or_else(String::new, |i| {
            format!("   back edge {} cycle {}", g.binds, i + 1)
        })
}

/// Every loop and the bound that stops it.
///
/// The highest-value section in a design whose central rule is that every cycle
/// is bounded: the validator already knows this and used to throw it away, so a
/// reader could see that a graph loops but not what stops it looping.
fn cycles(out: &mut String, topo: &Topology, g: &Glyphs) {
    if topo.cycles.is_empty() {
        return;
    }
    let bounded = topo
        .cycles
        .iter()
        .filter(|c| c.bounded_by.is_some())
        .count();
    let all = topo.cycles.len();
    out.push_str(&format!(
        "\nCycles    {all}, {}\n",
        if bounded == all {
            "all bounded".to_owned()
        } else {
            format!("{} UNBOUNDED", all - bounded)
        }
    ));
    for (i, c) in topo.cycles.iter().enumerate() {
        let path = c.nodes.join(&format!(" {} ", g.step));
        let bound = c
            .bounded_by
            .clone()
            .unwrap_or_else(|| "nothing bounds it".to_owned());
        let via = if c.via_reroute {
            "   via accept.on_unmet"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}  {path} {} {}   {bound}{via}\n",
            i + 1,
            g.back.trim_start_matches(['-', '─']),
            c.nodes.first().map_or("", String::as_str)
        ));
    }
}

/// Counts and the legend.
fn summary(out: &mut String, graph: &Graph, topo: &Topology, g: &Glyphs) {
    let count = |k: NodeKind| graph.nodes.values().filter(|n| n.spec.kind() == k).count();
    let gates = graph.nodes.keys().filter(|id| graph.is_gate(id)).count();
    let reroutes = topo.transitions.iter().filter(|t| t.is_reroute()).count();
    let mut kinds = Vec::new();
    for (kind, label) in [
        (NodeKind::Agent, "agent"),
        (NodeKind::Command, "command"),
        (NodeKind::Human, "human"),
        (NodeKind::Terminal, "terminal"),
    ] {
        let n = count(kind);
        if n > 0 {
            kinds.push(format!("{n} {label}"));
        }
    }
    out.push_str(&format!(
        "\n{} nodes ({}) {} {} edges {} {reroutes} implicit {} {gates} {}\n",
        graph.nodes.len(),
        kinds.join(", "),
        g.sep,
        graph.edges.len(),
        g.sep,
        g.sep,
        if gates == 1 { "gate" } else { "gates" },
    ));
    out.push_str(&format!("{}\n", g.legend()));
}

/// Round milliseconds to the unit a human wrote them in.
fn human_ms(ms: u64) -> String {
    match ms {
        ms if ms % 3_600_000 == 0 => format!("{}h", ms / 3_600_000),
        ms if ms % 60_000 == 0 => format!("{}m", ms / 60_000),
        ms if ms % 1_000 == 0 => format!("{}s", ms / 1_000),
        ms => format!("{ms}ms"),
    }
}

/// The graph as a machine-readable document.
///
/// The value a client cannot compute for itself is **edge classification**:
/// which transitions close a loop, which loops exist, and what bounds each one.
/// Everything else is a faithful dump of the compiled IR — the previous version
/// emitted only node ids and kinds, dropping budgets, roles, acceptance and
/// every bound.
pub fn to_json(graph: &Graph, topo: &Topology, origin: &str) -> serde_json::Value {
    let nodes: Vec<_> = topo
        .order
        .iter()
        .chain(topo.unreachable.iter())
        .filter_map(|id| {
            let node = graph.node(id)?;
            let mut v = serde_json::json!({
                "id": id,
                "kind": node.spec.kind().as_str(),
                "entry": *id == graph.entry,
                "gate": graph.is_gate(id),
                "reachable": !topo.unreachable.contains(id),
                "rank": topo.rank.get(id),
                "max_visits": node.max_visits,
            });
            match &node.spec {
                NodeSpec::Agent {
                    worker,
                    may_propose,
                    context,
                    read_only,
                    ..
                } => {
                    v["agent"] = serde_json::json!({
                        "role": worker,
                        "may_propose": may_propose,
                        "context": if *context == hex_runtime::Context::Continue {
                            "continue"
                        } else {
                            "fresh"
                        },
                        "read_only": read_only,
                    });
                }
                NodeSpec::Command { steps, mode } => {
                    v["command"] = serde_json::json!({
                        "mode": mode.as_str(),
                        "steps": steps.iter().map(|s| serde_json::json!({
                            "label": s.label(),
                            "argv": s.argv,
                        })).collect::<Vec<_>>(),
                    });
                }
                NodeSpec::Human { prompt } => {
                    v["human"] = serde_json::json!({ "prompt": prompt });
                }
                NodeSpec::Terminal { disposition } => {
                    v["terminal"] = serde_json::json!({ "disposition": disposition.as_str() });
                }
            }
            Some(v)
        })
        .collect();

    serde_json::json!({
        "name": graph.name,
        "origin": origin,
        "entry": graph.entry,
        "budget": {
            "attempts": graph.budget.attempts,
            "elapsed_ms": graph.budget.elapsed_ms,
            "attempt_elapsed_ms": graph.budget.attempt_elapsed_ms,
            "cycle_visits": graph.budget.cycle_visits,
            "output_tokens": graph.budget.output_tokens,
        },
        "accept": {
            "require": graph.accept.require.iter()
                .map(|r| serde_json::json!({"node": r.node, "signal": r.signal}))
                .collect::<Vec<_>>(),
            "on_unmet": graph.accept.on_unmet,
        },
        "order": topo.order,
        "nodes": nodes,
        "transitions": topo.transitions.iter().map(|t| serde_json::json!({
            "from": t.from,
            "on": t.on,
            "to": t.to,
            "class": if t.class == EdgeClass::Back { "back" } else { "forward" },
            "implicit": t.is_reroute(),
        })).collect::<Vec<_>>(),
        "cycles": topo.cycles.iter().map(|c| serde_json::json!({
            "nodes": c.nodes,
            "via_reroute": c.via_reroute,
            "bounded_by": c.bounded_by,
        })).collect::<Vec<_>>(),
    })
}
