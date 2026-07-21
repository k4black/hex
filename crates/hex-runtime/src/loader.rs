//! Parse the standard-YAML graph surface into the kernel's compiled IR.
//!
//! Surface shape (slim MVP subset): kind-as-key nodes (`agent:`/`gate:`/
//! `terminal:`), co-located edges under `on:`, `defaults:` for repetition,
//! one operator `{{prompt}}` interpolation token, and an `accept.require` list.
//! The kernel only ever sees the flat IR this produces.

use std::collections::BTreeMap;

use hex_kernel::graph::{Budget, Context, Edge, Graph, Node, NodeSpec, Requirement};
use hex_proto::Disposition;
use serde::Deserialize;

use crate::config::DefaultsSpec;
use crate::error::{HexError, Result};

/// A parsed but uncompiled graph (raw YAML shape).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGraph {
    #[serde(default = "one")]
    version: u32,
    name: String,
    entry: String,
    #[serde(default)]
    defaults: RawDefaults,
    nodes: BTreeMap<String, RawNode>,
    #[serde(default)]
    accept: RawAccept,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    worker: Option<String>,
    context: Option<String>,
    budget: Option<RawBudget>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBudget {
    attempts: Option<u32>,
    elapsed: Option<String>,
    cycle_visits: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccept {
    #[serde(default)]
    require: Vec<String>,
}

/// One node's kind-as-key body plus its co-located edges.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    #[serde(default)]
    agent: Option<RawAgent>,
    #[serde(default)]
    gate: Option<RawRun>,
    #[serde(default)]
    command: Option<RawRun>,
    #[serde(default)]
    terminal: Option<String>,
    #[serde(default)]
    human: Option<RawHuman>,
    #[serde(default)]
    on: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    worker: Option<String>,
    prompt: String,
    #[serde(default)]
    may_propose: Vec<String>,
    #[serde(default)]
    context: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRun {
    run: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHuman {
    prompt: String,
}

/// Compile YAML `source` into the kernel IR, interpolating the operator
/// `prompt` into node prompts (`{{prompt}}`) and falling back to
/// `config_defaults` where the graph omits its own.
///
/// The operator supplies exactly one value — the prompt (`-p`/`-f`). Richer
/// per-node typed inputs/outputs are an internal graph-dataflow concern (see
/// TODO Phase 6), not part of this operator surface.
///
/// # Errors
/// Fails on malformed YAML, an unknown node kind, or an unparseable duration.
pub fn load(source: &str, prompt: Option<&str>, config_defaults: &DefaultsSpec) -> Result<Graph> {
    let raw: RawGraph = yaml_serde::from_str(source)?;
    if raw.version != 1 {
        return Err(HexError::new(format!(
            "unsupported graph version {} (expected 1)",
            raw.version
        )));
    }

    let default_worker = raw
        .defaults
        .worker
        .clone()
        .or_else(|| config_defaults.worker.clone());
    let default_context = raw
        .defaults
        .context
        .clone()
        .or_else(|| config_defaults.context.clone());
    let budget = compile_budget(raw.defaults.budget.as_ref())?;

    let mut nodes = BTreeMap::new();
    let mut edges = Vec::new();

    for (id, node) in &raw.nodes {
        let spec = compile_node(
            id,
            node,
            default_worker.as_deref(),
            default_context.as_deref(),
            prompt,
        )?;
        for (on, to) in &node.on {
            edges.push(Edge {
                from: id.clone(),
                on: on.clone(),
                to: to.clone(),
            });
        }
        nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                spec,
            },
        );
    }

    let accept = raw
        .accept
        .require
        .iter()
        .map(|r| compile_requirement(r))
        .collect::<Result<Vec<_>>>()?;

    Ok(Graph {
        name: raw.name,
        entry: raw.entry,
        nodes,
        edges,
        budget,
        accept,
    })
}

/// The single interpolation token the operator prompt fills.
const PROMPT_TOKEN: &str = "{{prompt}}";

/// Whether a compiled graph still references the operator prompt — i.e. a run
/// needs `-p`/`-f`. Only agent and human prompts support interpolation, so
/// comments and other YAML fields must not trigger this requirement.
#[must_use]
pub(super) fn uses_prompt(graph: &Graph) -> bool {
    graph.nodes.values().any(|node| match &node.spec {
        NodeSpec::Agent { prompt, .. } | NodeSpec::Human { prompt } => {
            prompt.contains(PROMPT_TOKEN)
        }
        _ => false,
    })
}

fn compile_node(
    id: &str,
    node: &RawNode,
    default_worker: Option<&str>,
    default_context: Option<&str>,
    prompt: Option<&str>,
) -> Result<NodeSpec> {
    let declared = [
        node.agent.is_some(),
        node.gate.is_some(),
        node.command.is_some(),
        node.terminal.is_some(),
        node.human.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if declared != 1 {
        return Err(HexError::new(format!(
            "node `{id}` must declare exactly one kind (agent/gate/command/terminal/human)"
        )));
    }

    if let Some(agent) = &node.agent {
        let worker = agent
            .worker
            .clone()
            .or_else(|| default_worker.map(ToOwned::to_owned))
            .ok_or_else(|| HexError::new(format!("agent `{id}` has no worker and no default")))?;
        return Ok(NodeSpec::Agent {
            worker,
            prompt: interpolate(&agent.prompt, prompt),
            may_propose: agent.may_propose.clone(),
            context: parse_context(agent.context.as_deref().or(default_context))?,
        });
    }
    if let Some(gate) = &node.gate {
        return Ok(NodeSpec::Gate {
            command: gate.run.clone(),
        });
    }
    if let Some(command) = &node.command {
        return Ok(NodeSpec::Command {
            command: command.run.clone(),
        });
    }
    if let Some(terminal) = &node.terminal {
        return Ok(NodeSpec::Terminal {
            disposition: parse_disposition(terminal)?,
        });
    }
    if let Some(human) = &node.human {
        return Ok(NodeSpec::Human {
            prompt: interpolate(&human.prompt, prompt),
        });
    }
    unreachable!("declared exactly one kind")
}

fn compile_requirement(raw: &str) -> Result<Requirement> {
    let (node, signal) = raw.split_once('.').ok_or_else(|| {
        HexError::new(format!(
            "acceptance requirement `{raw}` must be `node.signal`"
        ))
    })?;
    Ok(Requirement {
        node: node.to_owned(),
        signal: signal.to_owned(),
    })
}

fn compile_budget(raw: Option<&RawBudget>) -> Result<Budget> {
    let Some(raw) = raw else {
        return Ok(Budget::default());
    };
    Ok(Budget {
        attempts: raw.attempts,
        elapsed_ms: raw.elapsed.as_deref().map(parse_duration_ms).transpose()?,
        cycle_visits: raw.cycle_visits,
    })
}

fn parse_context(raw: Option<&str>) -> Result<Context> {
    match raw {
        None | Some("fresh") => Ok(Context::Fresh),
        // `continue` is a real IR variant but the slim MVP only runs fresh
        // sessions; accepting it would silently ignore the author's intent.
        Some("continue") => Err(HexError::new(
            "context `continue` is not supported yet (Phase 4); only `fresh`",
        )),
        Some(other) => Err(HexError::new(format!(
            "unknown context `{other}` (expected fresh)"
        ))),
    }
}

fn parse_disposition(raw: &str) -> Result<Disposition> {
    match raw {
        "succeeded" => Ok(Disposition::Succeeded),
        "failed" => Ok(Disposition::Failed),
        other => Err(HexError::new(format!(
            "unknown terminal disposition `{other}` (expected succeeded/failed)"
        ))),
    }
}

/// Parse a duration like `30m`, `45s`, `2h`, `500ms` into milliseconds.
///
/// Units follow `humantime`, which is wider than the old `ms|s|m|h` set: it also
/// accepts `d`/`w`/`M`(month)/`y` and compound forms (`1h30m`). Note `m` is
/// minutes and `M` is months. Budgets are millisecond-resolution, so a positive
/// sub-millisecond duration (e.g. `1ns`) is rejected rather than silently
/// floored to a 0ms budget that would time out instantly.
fn parse_duration_ms(raw: &str) -> Result<u64> {
    let dur = humantime::parse_duration(raw.trim())
        .map_err(|e| HexError::new(format!("duration `{raw}`: {e}")))?;
    let ms = u64::try_from(dur.as_millis())
        .map_err(|_| HexError::new(format!("duration `{raw}` is too large")))?;
    if ms == 0 && !dur.is_zero() {
        return Err(HexError::new(format!(
            "duration `{raw}` is below the 1ms resolution of budgets"
        )));
    }
    Ok(ms)
}

/// Replace the `{{prompt}}` token with the operator prompt. When no prompt is
/// supplied (e.g. `validate`/`graph`) the token is left as-is.
fn interpolate(template: &str, prompt: Option<&str>) -> String {
    match prompt {
        Some(value) => template.replace(PROMPT_TOKEN, value),
        None => template.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_kernel::graph::NodeKind;

    const CRITIQUE: &str = include_str!("presets/critique-loop.yaml");

    fn no_defaults() -> DefaultsSpec {
        DefaultsSpec::default()
    }

    #[test]
    fn parses_duration_units() {
        assert_eq!(parse_duration_ms("500ms").unwrap(), 500);
        assert_eq!(parse_duration_ms("45s").unwrap(), 45_000);
        assert_eq!(parse_duration_ms("30m").unwrap(), 1_800_000);
        assert_eq!(parse_duration_ms("2h").unwrap(), 7_200_000);
        assert!(parse_duration_ms("30x").is_err());
        // humantime widens the grammar: compound forms now parse.
        assert_eq!(parse_duration_ms("1h30m").unwrap(), 5_400_000);
        // A positive sub-millisecond value must not silently become a 0ms
        // (instantly-exhausted) budget.
        assert!(parse_duration_ms("1ns").is_err());
        // An explicit zero is fine.
        assert_eq!(parse_duration_ms("0s").unwrap(), 0);
    }

    #[test]
    fn interpolates_the_prompt_into_node_prompts() {
        let g = load(CRITIQUE, Some("fix the bug"), &no_defaults()).expect("loads");
        let NodeSpec::Agent { prompt, .. } = &g.node("implement").unwrap().spec else {
            panic!("implement is an agent");
        };
        assert!(prompt.contains("fix the bug"));
        assert!(!prompt.contains(PROMPT_TOKEN));
    }

    #[test]
    fn load_is_lenient_without_a_prompt() {
        // Structural load must succeed without a prompt (for validate/graph);
        // the token is simply left unsubstituted.
        let g = load(CRITIQUE, None, &no_defaults()).expect("lenient load");
        let NodeSpec::Agent { prompt, .. } = &g.node("implement").unwrap().spec else {
            panic!("implement is an agent");
        };
        assert!(prompt.contains(PROMPT_TOKEN));
    }

    #[test]
    fn uses_prompt_detects_the_token() {
        let graph = load(CRITIQUE, None, &no_defaults()).expect("loads");
        assert!(uses_prompt(&graph));
    }

    #[test]
    fn prompt_token_outside_a_node_prompt_does_not_require_input() {
        let source = r#"
# {{prompt}} is just documentation here.
version: 1
name: no-prompt
entry: done
nodes:
  done:
    terminal: succeeded
"#;
        let graph = load(source, None, &no_defaults()).expect("loads");
        assert!(!uses_prompt(&graph));
    }

    #[test]
    fn compiles_the_builtin_critique_loop() {
        let g = load(CRITIQUE, Some("x"), &no_defaults()).expect("loads");
        assert_eq!(g.entry, "implement");
        assert_eq!(g.node("test").unwrap().spec.kind(), NodeKind::Gate);
        assert_eq!(g.budget.attempts, Some(12));
        assert_eq!(g.accept.len(), 1);
        // And it passes kernel validation.
        hex_kernel::validate(&g).expect("valid graph");
    }

    #[test]
    fn node_with_two_kinds_is_rejected() {
        let src = r#"
version: 1
name: bad
entry: a
nodes:
  a:
    agent: { prompt: "hi", may_propose: [go] }
    gate: { run: [true] }
    on: { go: done }
  done:
    terminal: succeeded
"#;
        let err = load(src, None, &no_defaults()).unwrap_err();
        assert!(err.to_string().contains("exactly one kind"));
    }
}
