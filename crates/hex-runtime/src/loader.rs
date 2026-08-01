//! Parse the standard-YAML graph surface into the kernel's compiled IR.
//!
//! Surface shape (slim MVP subset): kind-as-key nodes (`agent:`/`command:`/
//! `terminal:`), co-located edges under `on:`, `defaults:` for repetition,
//! one operator `{{prompt}}` interpolation token, and an `accept.require` list.
//! The kernel only ever sees the flat IR this produces.

use std::collections::BTreeMap;

use hex_kernel::graph::{
    Accept, Budget, CommandMode, CommandStep, Context, DEFAULT_ATTEMPT_ELAPSED_MS, Edge, Graph,
    Node, NodeSpec, Requirement,
};
use hex_proto::Disposition;
use serde::Deserialize;

use crate::config::{Config, DefaultsSpec, RoleSpec};
use crate::error::{HexError, Result};

/// A parsed but uncompiled graph (raw YAML shape).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGraph {
    #[serde(default = "one")]
    version: u32,
    name: String,
    /// One-line summary shown by `hex list`.
    #[serde(default)]
    description: Option<String>,
    /// A short example operator prompt shown by `hex list`.
    #[serde(default)]
    example: Option<String>,
    entry: String,
    #[serde(default)]
    defaults: RawDefaults,
    nodes: BTreeMap<String, RawNode>,
    #[serde(default)]
    accept: RawAccept,
}

/// A graph's presentation metadata for `hex list` — parsed without building or
/// validating the full IR, so a graph with a downstream error still lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphMeta {
    /// The graph's declared name.
    pub name: String,
    /// One-line summary, if declared.
    pub description: Option<String>,
    /// Example operator prompt, if declared.
    pub example: Option<String>,
}

/// Parse just a graph's `name`/`description`/`example` from its YAML source.
///
/// # Errors
/// Fails if the source is not a parseable graph surface.
pub fn metadata(source: &str) -> Result<GraphMeta> {
    let raw: RawGraph = yaml_serde::from_str(source)?;
    Ok(GraphMeta {
        name: raw.name,
        description: raw.description,
        example: raw.example,
    })
}

fn one() -> u32 {
    1
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    /// Default role for agent nodes (`role:`; `worker:` is accepted as an alias
    /// since a graph names one thing and needn't care that it resolves via the
    /// worker registry).
    role: Option<String>,
    worker: Option<String>,
    context: Option<String>,
    budget: Option<RawBudget>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBudget {
    attempts: Option<u32>,
    elapsed: Option<String>,
    /// Per-attempt wall-clock bound (e.g. `20m`). Defaults to
    /// [`DEFAULT_ATTEMPT_ELAPSED_MS`] so no attempt ever waits forever.
    attempt: Option<String>,
    cycle_visits: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccept {
    #[serde(default)]
    require: Vec<String>,
    /// Where to go when acceptance is unmet, instead of failing the run.
    #[serde(default)]
    on_unmet: Option<String>,
}

/// One node's kind-as-key body plus its co-located edges.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    #[serde(default)]
    agent: Option<RawAgent>,
    #[serde(default)]
    command: Option<RawRun>,
    #[serde(default)]
    terminal: Option<String>,
    #[serde(default)]
    human: Option<RawHuman>,
    #[serde(default)]
    on: BTreeMap<String, String>,
    /// Per-node bounds. Only `visits` today: how many times this node may be
    /// entered, so one loop can be capped without capping every loop.
    #[serde(default)]
    budget: Option<RawNodeBudget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNodeBudget {
    visits: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    /// The role this node runs as (`role:`, or `worker:` as an alias).
    role: Option<String>,
    worker: Option<String>,
    prompt: String,
    #[serde(default)]
    may_propose: Vec<String>,
    #[serde(default)]
    context: Option<String>,
    /// Read-only policy: the worker must not modify the workspace (a reviewer).
    #[serde(default)]
    read_only: bool,
}

/// A command node's body: either a literal `run:` argv or a `check:` reference
/// resolved from the project's `checks:` config.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRun {
    /// One literal argv, or several.
    #[serde(default)]
    run: Option<Argvs>,
    /// One project check name, or several.
    #[serde(default)]
    check: Option<Names>,
    /// `ordered` (stop at the first failure) or `parallel` (run everything).
    #[serde(default)]
    mode: Option<String>,
}

/// One argv or a list of them, so the single-command case stays terse.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Argvs {
    One(Vec<String>),
    Many(Vec<Vec<String>>),
}

/// One check name or a list of them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Names {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHuman {
    prompt: String,
}

/// Compile YAML `source` into the kernel IR, falling back to `config_defaults`
/// where the graph omits its own. Node prompts keep their `{{prompt}}` and
/// `{{node.result}}` tokens verbatim; both are interpolated at attempt-start
/// (the operator prompt is substituted last, as opaque data).
///
/// # Errors
/// Fails on malformed YAML, an unknown node kind, or an unparseable duration.
pub fn load(source: &str, config: &Config) -> Result<Graph> {
    load_with(source, &config.defaults, &config.roles, &config.checks)
}

/// Compile with an explicit set of fallback defaults and resolved project
/// checks, rather than whatever live config says — used on resume, so a run is
/// bound to the values recorded when it was created.
///
/// # Errors
/// Fails on malformed YAML, an unknown node kind, or an unparseable duration.
pub fn load_with(
    source: &str,
    config_defaults: &DefaultsSpec,
    roles: &BTreeMap<String, RoleSpec>,
    checks: &BTreeMap<String, Vec<String>>,
) -> Result<Graph> {
    let raw: RawGraph = yaml_serde::from_str(source)?;
    if raw.version != 1 {
        return Err(HexError::new(format!(
            "unsupported graph version {} (expected 1)",
            raw.version
        )));
    }

    let default_role = raw
        .defaults
        .role
        .clone()
        .or_else(|| raw.defaults.worker.clone())
        .or_else(|| config_defaults.role.clone());
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
            default_role.as_deref(),
            default_context.as_deref(),
            roles,
            checks,
        )?;
        for (on, to) in &node.on {
            edges.push(Edge {
                from: id.clone(),
                on: on.clone(),
                to: to.clone(),
            });
        }
        let max_visits = node.budget.as_ref().and_then(|b| b.visits);
        if max_visits == Some(0) {
            return Err(HexError::new(format!(
                "node `{id}` has `budget.visits: 0`, so it could never run"
            )));
        }
        nodes.insert(
            id.clone(),
            Node::new(id.clone(), spec).with_max_visits(max_visits),
        );
    }

    let accept = Accept {
        require: raw
            .accept
            .require
            .iter()
            .map(|r| compile_requirement(r))
            .collect::<Result<Vec<_>>>()?,
        on_unmet: raw.accept.on_unmet.clone(),
    };

    Ok(Graph {
        name: raw.name,
        entry: raw.entry,
        nodes,
        edges,
        budget,
        accept,
    })
}

/// The single interpolation token the operator prompt fills. Substituted at
/// attempt-start (in the driver), *after* `{{node.result}}` interpolation and
/// as a plain replace, so the operator value is opaque data — never re-scanned
/// for template tokens.
pub(crate) const PROMPT_TOKEN: &str = "{{prompt}}";

/// Whether a compiled graph still references the operator prompt — i.e. a run
/// needs `-p`/`-f`. Only agent and human prompts support interpolation, so
/// comments and other YAML fields must not trigger this requirement.
#[must_use]
pub(super) fn uses_prompt(graph: &Graph) -> bool {
    use hex_kernel::template::{Token, tokens};
    graph.nodes.values().any(|node| {
        let prompt = match &node.spec {
            NodeSpec::Agent { prompt, .. } | NodeSpec::Human { prompt } => prompt,
            _ => return false,
        };
        tokens(prompt).any(|t| t == Token::Prompt)
    })
}

fn compile_node(
    id: &str,
    node: &RawNode,
    default_role: Option<&str>,
    default_context: Option<&str>,
    roles: &BTreeMap<String, RoleSpec>,
    checks: &BTreeMap<String, Vec<String>>,
) -> Result<NodeSpec> {
    let declared = [
        node.agent.is_some(),
        node.command.is_some(),
        node.terminal.is_some(),
        node.human.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if declared != 1 {
        return Err(HexError::new(format!(
            "node `{id}` must declare exactly one kind (agent/command/terminal/human)"
        )));
    }

    if let Some(agent) = &node.agent {
        let role_name = agent
            .role
            .clone()
            .or_else(|| agent.worker.clone())
            .or_else(|| default_role.map(ToOwned::to_owned))
            .ok_or_else(|| HexError::new(format!("agent `{id}` has no role and no default")))?;
        // A role the config does not define is refused here rather than at the
        // first attempt: the graph names a job, config says how to do it, and a
        // missing binding is a setup error the operator can fix in one edit.
        let role = roles.get(&role_name);
        // The role's preamble is prepended at *compile* time so the effective
        // prompt is part of the IR (and of what a resumed run replays), rather
        // than being re-derived from mutable config on every attempt.
        let prompt = match role.and_then(RoleSpec::preamble) {
            Some(preamble) => format!("{preamble}\n\n{}", agent.prompt),
            None => agent.prompt.clone(),
        };
        return Ok(NodeSpec::Agent {
            worker: role_name,
            // Keep `{{prompt}}`/`{{node.result}}` tokens as-authored; both are
            // interpolated at attempt-start.
            prompt,
            may_propose: agent.may_propose.clone(),
            context: parse_context(agent.context.as_deref().or(default_context))?,
            // A node may force read-only; otherwise the role's policy applies.
            read_only: agent.read_only || role.and_then(|r| r.read_only).unwrap_or(false),
        });
    }
    if let Some(command) = &node.command {
        return compile_command(id, command, checks);
    }
    if let Some(terminal) = &node.terminal {
        return Ok(NodeSpec::Terminal {
            disposition: parse_disposition(terminal)?,
        });
    }
    if let Some(human) = &node.human {
        return Ok(NodeSpec::Human {
            prompt: human.prompt.clone(),
        });
    }
    unreachable!("declared exactly one kind")
}

/// Compile a command node into its ordered steps: a literal `run:` argv, or a
/// `check:` name resolved against the project's `checks:` config.
///
/// A `check:` name is resolved **now**, so the recorded graph runs an exact argv
/// rather than a config reference that could drift. A name the project has not
/// declared is a hard error: passing silently
/// would let a run reach `succeeded` having verified nothing, and the whole point
/// of a gate is that its verdict means something.
fn compile_command(
    id: &str,
    raw: &RawRun,
    checks: &BTreeMap<String, Vec<String>>,
) -> Result<NodeSpec> {
    let mode = match raw.mode.as_deref() {
        None | Some("ordered") => CommandMode::Ordered,
        Some("parallel") => CommandMode::Parallel,
        Some(other) => {
            return Err(HexError::new(format!(
                "command `{id}` has unknown mode `{other}` (expected ordered/parallel)"
            )));
        }
    };

    let steps = match (&raw.run, &raw.check) {
        (Some(_), Some(_)) => {
            return Err(HexError::new(format!(
                "command `{id}` sets both `run` and `check` (use one)"
            )));
        }
        (None, None) => {
            return Err(HexError::new(format!(
                "command `{id}` must set `run: [argv]` or `check: <name>`"
            )));
        }
        (Some(argvs), None) => {
            let argvs = match argvs {
                Argvs::One(argv) => vec![argv.clone()],
                Argvs::Many(many) => many.clone(),
            };
            if argvs.is_empty() || argvs.iter().any(Vec::is_empty) {
                return Err(HexError::new(format!("command `{id}` has an empty `run`")));
            }
            argvs
                .into_iter()
                .map(|argv| CommandStep { argv, check: None })
                .collect()
        }
        (None, Some(names)) => {
            let names = match names {
                Names::One(name) => vec![name.clone()],
                Names::Many(many) => many.clone(),
            };
            if names.is_empty() {
                return Err(HexError::new(format!(
                    "command `{id}` has an empty `check`"
                )));
            }
            names
                .into_iter()
                .map(|name| match checks.get(&name) {
                    None => Err(HexError::new(format!(
                        "command `{id}` needs check `{name}`, which this project does not \
                         declare — add `checks.{name}` to .hex/config.yaml (see `hex doctor`)"
                    ))),
                    Some(argv) if argv.is_empty() => Err(HexError::new(format!(
                        "check `{name}` (used by `{id}`) is configured as an empty argv"
                    ))),
                    Some(argv) => Ok(CommandStep {
                        argv: argv.clone(),
                        check: Some(name),
                    }),
                })
                .collect::<Result<Vec<_>>>()?
        }
    };
    Ok(NodeSpec::Command { steps, mode })
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
        return Ok(Budget {
            attempt_elapsed_ms: Some(DEFAULT_ATTEMPT_ELAPSED_MS),
            ..Budget::default()
        });
    };
    Ok(Budget {
        attempts: raw.attempts,
        elapsed_ms: raw.elapsed.as_deref().map(parse_duration_ms).transpose()?,
        // Always bounded: an unbounded attempt let a hung agent block forever.
        attempt_elapsed_ms: Some(
            raw.attempt
                .as_deref()
                .map(parse_duration_ms)
                .transpose()?
                .unwrap_or(DEFAULT_ATTEMPT_ELAPSED_MS),
        ),
        cycle_visits: raw.cycle_visits,
    })
}

fn parse_context(raw: Option<&str>) -> Result<Context> {
    match raw {
        None | Some("fresh") => Ok(Context::Fresh),
        Some("continue") => Ok(Context::Continue),
        Some(other) => Err(HexError::new(format!(
            "unknown context `{other}` (expected fresh or continue)"
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

#[cfg(test)]
mod tests {
    use super::*;

    const CRITIQUE: &str = include_str!("presets/critique-loop.yaml");

    fn no_defaults() -> Config {
        Config::default()
    }

    /// The real built-in layer, so tests exercise the shipped roles.
    fn builtin() -> Config {
        Config::builtin()
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
    fn load_keeps_the_prompt_token_for_runtime_substitution() {
        // The operator prompt is substituted at attempt-start, not at load, so
        // the compiled prompt keeps `{{prompt}}` verbatim.
        let g = load(CRITIQUE, &no_defaults()).expect("loads");
        let NodeSpec::Agent { prompt, .. } = &g.node("implement").unwrap().spec else {
            panic!("implement is an agent");
        };
        assert!(prompt.contains(PROMPT_TOKEN));
    }

    #[test]
    fn uses_prompt_detects_the_token() {
        let graph = load(CRITIQUE, &no_defaults()).expect("loads");
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
        let graph = load(source, &no_defaults()).expect("loads");
        assert!(!uses_prompt(&graph));
    }

    #[test]
    fn compiles_the_builtin_critique_loop() {
        let g = load(CRITIQUE, &builtin()).expect("loads");
        assert_eq!(g.entry, "implement");
        assert_eq!(g.budget.attempts, Some(12));
        assert_eq!(g.accept.require.len(), 1);
        assert_eq!(g.accept.on_unmet.as_deref(), Some("implement"));
        // And it passes kernel validation.
        hex_kernel::validate(&g).expect("valid graph");
    }

    /// The built-ins must run in a repo that has declared nothing at all — that
    /// is the whole reason they ship gate-free.
    #[test]
    fn every_builtin_preset_compiles_and_validates_with_no_project_config() {
        for entry in crate::preset::BUILTINS {
            let graph = match load(entry.source, &builtin()) {
                Ok(g) => g,
                Err(e) => {
                    // A preset whose gate IS the preset (tdd, implement-until-green)
                    // legitimately requires a check; it must say so actionably.
                    let msg = e.to_string();
                    assert!(
                        msg.contains("does not declare") && msg.contains("checks."),
                        "preset `{}` failed for an unexpected reason: {msg}",
                        entry.name
                    );
                    continue;
                }
            };
            hex_kernel::validate(&graph)
                .unwrap_or_else(|i| panic!("preset `{}` is invalid: {i:?}", entry.name));
        }
    }

    /// A role's preamble is prepended at compile time, and the role's read-only
    /// policy reaches the node without the graph restating it.
    #[test]
    fn a_role_contributes_its_preamble_and_read_only_policy() {
        let g = load(CRITIQUE, &builtin()).expect("loads");
        let NodeSpec::Agent {
            prompt,
            read_only,
            worker,
            ..
        } = &g.node("review").unwrap().spec
        else {
            panic!("review should be an agent");
        };
        assert_eq!(worker, "reviewer");
        assert!(*read_only, "the reviewer role is read-only");
        assert!(
            prompt.starts_with("You are reviewing, not implementing."),
            "role preamble comes first: {prompt:.60}"
        );
        assert!(
            prompt.contains("hex emit approved"),
            "the node's own prompt survives"
        );
    }

    #[test]
    fn a_project_can_append_to_a_shipped_role_prompt() {
        let mut config = builtin();
        config.merge(
            yaml_serde::from_str(
                "roles:\n  reviewer:\n    prompt_append: \"Only flag security issues.\"\n",
            )
            .expect("parses"),
        );
        let g = load(CRITIQUE, &config).expect("loads");
        let NodeSpec::Agent { prompt, .. } = &g.node("review").unwrap().spec else {
            panic!("agent");
        };
        assert!(prompt.starts_with("You are reviewing, not implementing."));
        assert!(prompt.contains("Only flag security issues."));
    }

    /// A check the project has not declared is refused, with the fix in the
    /// message — never silently passed.
    #[test]
    fn an_unconfigured_check_is_refused_with_an_actionable_message() {
        let src = r#"
version: 1
name: needs-check
entry: t
nodes:
  t: { command: { check: test }, on: { passed: done, failed: done } }
  done: { terminal: succeeded }
"#;
        let err = load(src, &builtin()).unwrap_err().to_string();
        assert!(err.contains("does not declare"), "got: {err}");
        assert!(err.contains("checks.test"), "names the key to add: {err}");
    }

    #[test]
    fn a_configured_check_resolves_to_its_argv_at_compile_time() {
        let src = r#"
version: 1
name: needs-check
entry: t
nodes:
  t: { command: { check: test }, on: { passed: done, failed: done } }
  done: { terminal: succeeded }
"#;
        let mut config = builtin();
        config.checks.insert(
            "test".to_owned(),
            vec!["pytest".to_owned(), "-q".to_owned()],
        );
        let g = load(src, &config).expect("loads");
        let NodeSpec::Command { steps, mode } = &g.node("t").unwrap().spec else {
            panic!("command");
        };
        assert_eq!(*mode, CommandMode::Ordered, "ordered is the default");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].argv, ["pytest", "-q"]);
        assert_eq!(steps[0].check.as_deref(), Some("test"));
    }

    #[test]
    fn a_command_node_takes_several_steps_and_a_mode() {
        let src = r#"
version: 1
name: multi
entry: t
nodes:
  t:
    command:
      run: [[sh, -c, "true"], [sh, -c, "false"]]
      mode: parallel
    on: { passed: done, failed: done }
  done: { terminal: succeeded }
"#;
        let g = load(src, &no_defaults()).expect("loads");
        let NodeSpec::Command { steps, mode } = &g.node("t").unwrap().spec else {
            panic!("command");
        };
        assert_eq!(*mode, CommandMode::Parallel);
        assert_eq!(steps.len(), 2);

        let bad = src.replace("mode: parallel", "mode: sideways");
        assert!(
            load(&bad, &no_defaults())
                .unwrap_err()
                .to_string()
                .contains("unknown mode")
        );
    }

    #[test]
    fn a_per_node_visit_bound_compiles_and_zero_is_rejected() {
        let src = r#"
version: 1
name: bounded
entry: a
nodes:
  a:
    agent: { prompt: "x", may_propose: [again, fin] }
    budget: { visits: 3 }
    on: { again: a, fin: done }
  done: { terminal: succeeded }
"#;
        let g = load(src, &builtin()).expect("loads");
        assert_eq!(g.node("a").unwrap().max_visits, Some(3));
        let zero = src.replace("visits: 3", "visits: 0");
        assert!(
            load(&zero, &builtin())
                .unwrap_err()
                .to_string()
                .contains("could never run")
        );
    }

    #[test]
    fn a_command_needs_exactly_one_of_run_or_check() {
        let both = r#"
version: 1
name: bad
entry: t
nodes:
  t: { command: { run: ["true"], check: test }, on: { passed: done, failed: done } }
  done: { terminal: succeeded }
"#;
        let neither = r#"
version: 1
name: bad
entry: t
nodes:
  t: { command: {}, on: { passed: done, failed: done } }
  done: { terminal: succeeded }
"#;
        assert!(
            load(both, &no_defaults())
                .unwrap_err()
                .to_string()
                .contains("both `run` and `check`")
        );
        assert!(
            load(neither, &no_defaults())
                .unwrap_err()
                .to_string()
                .contains("must set `run: [argv]` or `check: <name>`")
        );
    }

    /// Regression: an attempt with no bound blocked forever on a hung agent.
    #[test]
    fn every_graph_gets_a_per_attempt_bound_even_with_no_budget_block() {
        let source = r#"
version: 1
name: nobudget
entry: done
nodes:
  done:
    terminal: succeeded
"#;
        let g = load(source, &no_defaults()).expect("loads");
        assert_eq!(g.budget.elapsed_ms, None, "no run bound was declared");
        assert_eq!(
            g.budget.attempt_elapsed_ms,
            Some(hex_kernel::graph::DEFAULT_ATTEMPT_ELAPSED_MS),
            "but an attempt is always bounded"
        );
        // An explicit bound wins.
        let explicit = source.replace(
            "entry: done",
            "entry: done\ndefaults:\n  budget:\n    attempt: 90s",
        );
        let g = load(&explicit, &no_defaults()).expect("loads");
        assert_eq!(g.budget.attempt_elapsed_ms, Some(90_000));
    }

    /// The README's graph example must actually compile and validate. It did not
    /// for a long time (unknown `gates:`, missing `entry:`, an unimplemented
    /// `gate: { use: … }`), which is exactly the kind of rot a doc-only claim
    /// invites — so it is pinned here.
    #[test]
    fn the_readme_example_graph_is_valid() {
        let readme = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"),
        )
        .expect("README.md is readable");
        let block = readme
            .split("```yaml")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("README has a ```yaml block");
        // The example demonstrates a project check, so give it one — the point of
        // this test is that the YAML is valid, not that checks are optional.
        let mut config = builtin();
        config
            .checks
            .insert("test".to_owned(), vec!["true".to_owned()]);
        let graph = load(block, &config).expect("README example compiles");
        hex_kernel::validate(&graph).expect("README example validates");
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
    command: { run: ["true"] }
    on: { go: done }
  done:
    terminal: succeeded
"#;
        let err = load(src, &no_defaults()).unwrap_err();
        assert!(err.to_string().contains("exactly one kind"));
    }
}
