//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is one operator surface: argument parsing and rendering over a
//! [`hex_runtime::RuntimeClient`]. A human at a TTY and an agent (via injected
//! `hex emit`) share the same control protocol; every action becomes an event.
//!
//! Verbs (slim MVP): `validate` · `graph` · `run` · `resume` · `status` ·
//! `watch` · `cancel`, plus the worker-side `emit`. Redoing work is a new
//! `run`; there is no `retry`/`replay`.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use hex_runtime::{Disposition, Runtime};

mod preview;

/// Worked examples, shown under `hex --help`.
const EXAMPLES: &str = "\
Examples:
  hex list                             list runnable graphs
  hex validate critique-loop           check a graph before running it
  hex run critique-loop -p \"fix bug\"   start a run with an inline prompt
  hex run critique-loop -f task.md     read the prompt from a file
  hex status <run-id>                  show a run's status
  hex logs <run-id> --node reviewer    show one node's agent output
  hex resume <run-id>                  continue a run after a pause or crash

Add --json to any command for machine-readable output on stdout.
Docs: https://github.com/k4black/hex";

// The subcommand is optional so bare `hex` prints help to stderr (exit 2)
// rather than clap's default. The `about`/`after_help` below — not this
// comment — are what `hex --help` shows.
#[derive(Parser)]
#[command(
    name = "hex",
    version,
    propagate_version = true,
    about = "A thin, deterministic control plane for agentic loops and graphs.",
    after_help = EXAMPLES,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Emit machine-readable JSON on stdout instead of human-readable text
    #[arg(long, global = true)]
    json: bool,

    /// Disable the live in-flight preview pane (plain line streaming instead)
    #[arg(long, global = true)]
    no_preview: bool,
}

/// The operator/worker verbs. Names are stable public surface.
#[derive(Subcommand)]
enum Command {
    /// List runnable graphs (project, then user, then built-in)
    #[command(visible_alias = "ls")]
    List,
    /// Validate a graph: schema, references, bounded cycles
    Validate {
        /// Graph reference: a preset name or a path to a `.yaml` file
        #[arg(value_name = "GRAPH")]
        graph: String,
    },
    /// Render a graph as text (its nodes and edges)
    Graph {
        /// Graph reference: a preset name or a path to a `.yaml` file
        #[arg(value_name = "GRAPH")]
        graph: String,
    },
    /// Start a NEW run of a graph
    Run {
        /// Graph reference; omit to list what's runnable
        #[arg(value_name = "GRAPH")]
        graph: Option<String>,
        /// Operator prompt, inline (fills `{{prompt}}` in the graph)
        #[arg(short, long, value_name = "TEXT")]
        prompt: Option<String>,
        /// Read the operator prompt from a file instead of `--prompt`
        #[arg(short, long, value_name = "PATH", conflicts_with = "prompt")]
        file: Option<String>,
    },
    /// Resume the SAME run from its journal (after a pause or crash)
    Resume {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// Show a run's projected status
    Status {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// Print a run's recorded event stream
    Watch {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// Show per-attempt agent stdout/stderr for a run
    Logs {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// Show only this node's attempts
        #[arg(long, value_name = "NODE")]
        node: Option<String>,
    },
    /// Cancel a run (records a terminal cancellation event)
    Cancel {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// Propose a routing event (worker-side; called by an agent inside a run)
    Emit {
        /// Routing event to append (must be in the node's `may_propose`)
        event: String,
    },
}

fn main() -> ExitCode {
    // clap handles `--help`/`-h`/`--version` and parse/usage errors itself
    // (usage errors exit 2, help/version exit 0), matching the old exit codes.
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("hex: {err}");
            ExitCode::from(2)
        }
    }
}

/// Dispatch a verb. Returns the process exit code; `Err` is a usage/setup
/// failure (exit 2).
fn dispatch(cli: Cli) -> Result<ExitCode, String> {
    let json = cli.json;
    let no_preview = cli.no_preview;
    let Some(command) = cli.command else {
        // Bare `hex` is a usage error, so render clap's own help — byte-for-byte
        // the same text `hex --help` prints — to stderr and exit 2. (clap's
        // `--help` uses the compact `render_help`, not the verbose long form.)
        eprint!("{}", Cli::command().render_help());
        return Ok(ExitCode::from(2));
    };

    match command {
        Command::List => cmd_list(json),
        Command::Validate { graph } => cmd_validate(&graph, json),
        Command::Graph { graph } => cmd_graph(&graph, json),
        Command::Run {
            graph,
            prompt,
            file,
        } => cmd_run(graph.as_deref(), &prompt, &file, json, no_preview),
        Command::Resume { run_id } => cmd_resume(&run_id, json, no_preview),
        Command::Status { run_id } => cmd_status(&run_id, json),
        Command::Watch { run_id } => cmd_watch(&run_id, json),
        Command::Logs { run_id, node } => cmd_logs(&run_id, node.as_deref(), json),
        Command::Cancel { run_id } => cmd_cancel(&run_id, json),
        Command::Emit { event } => cmd_emit(&event),
    }
}

/// Open a runtime rooted at the current directory.
fn open_runtime() -> Result<Runtime, String> {
    let root = hex_runtime::project_root().map_err(|e| e.to_string())?;
    Runtime::new(root).map_err(|e| e.to_string())
}

/// A runtime that streams each event to stderr as it happens, so a foreground
/// `run`/`resume` shows live progress instead of blocking silently. stdout is
/// left clean for the final summary / `--json`.
fn open_runtime_streaming(json: bool, no_preview: bool) -> Result<Runtime, String> {
    // The live pane owns stderr, so it only turns on for an interactive TTY run.
    // `--json` (machine output), `--no-preview`, or a piped stderr fall back to
    // plain line streaming — same behavior as before this feature.
    let preview = !json && !no_preview && std::io::stderr().is_terminal();
    Ok(open_runtime()?.with_progress(Box::new(preview::LivePreview::new(preview))))
}

/// A one-line rendering of an event for progress/watch output.
pub(crate) fn event_line(e: &hex_runtime::Event) -> String {
    let node = e
        .node_id
        .as_deref()
        .map_or(String::new(), |n| format!(" {n}"));
    format!("#{}{} {}", e.seq, node, event_summary(&e.body))
}

fn cmd_list(json: bool) -> Result<ExitCode, String> {
    print_graph_list(&open_runtime()?, json);
    Ok(ExitCode::SUCCESS)
}

/// Print the runnable graphs (shared by `hex list` and bare `hex run`).
fn print_graph_list(runtime: &Runtime, json: bool) {
    let graphs = runtime.list_graphs();
    if json {
        let items: Vec<_> = graphs
            .iter()
            .map(|g| serde_json::json!({"name": g.name, "origin": g.origin}))
            .collect();
        println!("{}", serde_json::json!({ "graphs": items }));
        return;
    }
    if graphs.is_empty() {
        println!("no graphs found (add one to .hex/graphs/ or ~/.config/hex/graphs/)");
        return;
    }
    println!("available graphs:");
    let width = graphs.iter().map(|g| g.name.len()).max().unwrap_or(0);
    for g in &graphs {
        println!("  {:<width$}  {}", g.name, g.origin, width = width);
    }
    println!("\nrun one with:  hex run <name> -p \"<prompt>\"");
}

fn cmd_validate(reference: &str, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    match runtime.validate(reference) {
        Ok(graph) => {
            if json {
                let v =
                    serde_json::json!({"ok": true, "name": graph.name, "nodes": graph.nodes.len()});
                println!("{v}");
            } else {
                println!(
                    "ok: `{}` is valid ({} nodes)",
                    graph.name,
                    graph.nodes.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            if json {
                let v = serde_json::json!({"ok": false, "error": e.to_string()});
                println!("{v}");
            } else {
                eprintln!("{e}");
            }
            Ok(ExitCode::from(2))
        }
    }
}

fn cmd_graph(reference: &str, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let graph = runtime.validate(reference).map_err(|e| e.to_string())?;
    if json {
        let nodes: Vec<_> = graph
            .nodes
            .values()
            .map(|n| serde_json::json!({"id": n.id, "kind": n.spec.kind().as_str()}))
            .collect();
        let edges: Vec<_> = graph
            .edges
            .iter()
            .map(|e| serde_json::json!({"from": e.from, "on": e.on, "to": e.to}))
            .collect();
        let v = serde_json::json!({"name": graph.name, "entry": graph.entry, "nodes": nodes, "edges": edges});
        println!("{v}");
        return Ok(ExitCode::SUCCESS);
    }
    println!("graph: {}   entry: {}", graph.name, graph.entry);
    println!("nodes:");
    for node in graph.nodes.values() {
        println!("  {} [{}]", node.id, node.spec.kind().as_str());
    }
    println!("edges:");
    for edge in &graph.edges {
        println!("  {} --{}--> {}", edge.from, edge.on, edge.to);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_run(
    reference: Option<&str>,
    prompt: &Option<String>,
    file: &Option<String>,
    json: bool,
    no_preview: bool,
) -> Result<ExitCode, String> {
    let runtime = open_runtime_streaming(json, no_preview)?;
    // `hex run` with no graph lists what you can run instead of erroring.
    let Some(reference) = reference else {
        print_graph_list(&runtime, json);
        return Ok(ExitCode::SUCCESS);
    };
    let prompt = resolve_prompt(prompt, file)?;
    let report = runtime
        .start(reference, prompt.as_deref())
        .map_err(|e| e.to_string())?;
    if json {
        let v = serde_json::json!({
            "run_id": report.run_id,
            "origin": report.origin,
            "disposition": report.disposition,
        });
        println!("{v}");
    } else {
        println!("run {} ({})", report.run_id, report.origin);
        println!("disposition: {}", report.disposition);
    }
    Ok(exit_for(report.disposition))
}

fn cmd_resume(run_id: &str, json: bool, no_preview: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime_streaming(json, no_preview)?;
    let report = runtime.resume(run_id).map_err(|e| e.to_string())?;
    if json {
        let v = serde_json::json!({
            "run_id": report.run_id,
            "disposition": report.disposition,
        });
        println!("{v}");
    } else {
        println!("resumed {}", report.run_id);
        println!("disposition: {}", report.disposition);
    }
    Ok(exit_for(report.disposition))
}

fn cmd_status(run_id: &str, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let s = runtime.status(run_id).map_err(|e| e.to_string())?;
    if json {
        let v = serde_json::json!({
            "run_id": s.run_id,
            "status": s.status.to_string(),
            "current": s.current,
            "attempts": s.attempts,
        });
        println!("{v}");
    } else {
        println!("run: {}", s.run_id);
        println!("status: {}", s.status);
        if let Some(c) = &s.current {
            println!("current: {c}");
        }
        println!("attempts: {}", s.attempts);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_watch(run_id: &str, json: bool) -> Result<ExitCode, String> {
    // Foreground MVP: runs finish synchronously, so `watch` prints the recorded
    // event stream. Live tailing arrives with the background controller.
    let runtime = open_runtime()?;
    let events = runtime.events(run_id).map_err(|e| e.to_string())?;
    for event in &events {
        if json {
            println!(
                "{}",
                serde_json::to_string(event).map_err(|e| e.to_string())?
            );
        } else {
            println!("{}", event_line(event));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_logs(run_id: &str, node: Option<&str>, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let logs = runtime.logs(run_id).map_err(|e| e.to_string())?;
    let logs: Vec<_> = logs
        .into_iter()
        .filter(|l| node.is_none_or(|n| l.node_id.as_deref() == Some(n)))
        .collect();

    if json {
        let items: Vec<_> = logs
            .iter()
            .map(|l| {
                serde_json::json!({
                    "attempt_id": l.attempt_id,
                    "node_id": l.node_id,
                    "worker": l.worker,
                    "stdout": l.stdout,
                    "stderr": l.stderr,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "attempts": items }));
        return Ok(ExitCode::SUCCESS);
    }

    for l in &logs {
        let node = l.node_id.as_deref().unwrap_or("?");
        let via = l
            .worker
            .as_deref()
            .map_or(String::new(), |w| format!(" via {w}"));
        println!("── {} [{node}]{via} ──", l.attempt_id);
        if !l.stdout.trim().is_empty() {
            print!("{}", l.stdout);
            if !l.stdout.ends_with('\n') {
                println!();
            }
        }
        if !l.stderr.trim().is_empty() {
            eprint!("{}", l.stderr);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_cancel(run_id: &str, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    runtime.cancel(run_id).map_err(|e| e.to_string())?;
    if json {
        let v = serde_json::json!({"run_id": run_id, "cancelled": true});
        println!("{v}");
    } else {
        println!("cancelled {run_id}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Worker-side control (transport 1): append a routing event to `$HEX_EMIT_FILE`
/// after checking it is allowed by `$HEX_MAY_PROPOSE`. Agents call this.
fn cmd_emit(event: &str) -> Result<ExitCode, String> {
    let file = std::env::var("HEX_EMIT_FILE")
        .map_err(|_| "hex emit must be run inside a hex attempt (HEX_EMIT_FILE unset)")?;
    let allowed = std::env::var("HEX_MAY_PROPOSE").unwrap_or_default();
    let permitted: Vec<&str> = allowed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !permitted.contains(&event) {
        return Err(format!(
            "event `{event}` is not in this node's may_propose ({allowed})"
        ));
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .map_err(|e| format!("cannot open emit file: {e}"))?;
    writeln!(f, "{event}").map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve the operator prompt from `-p` (inline) or `-f` (file). At most one
/// may be given (enforced at parse time; the both-arm is a defensive fallback).
fn resolve_prompt(
    prompt: &Option<String>,
    file: &Option<String>,
) -> Result<Option<String>, String> {
    match (prompt, file) {
        (Some(_), Some(_)) => Err("pass only one of -p/--prompt or -f/--file".to_owned()),
        (Some(text), None) => Ok(Some(text.clone())),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map(Some)
            .map_err(|e| format!("cannot read prompt file `{path}`: {e}")),
        (None, None) => Ok(None),
    }
}

fn exit_for(d: Disposition) -> ExitCode {
    match d {
        Disposition::Succeeded => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

fn event_summary(body: &hex_runtime::EventBody) -> String {
    use hex_runtime::EventBody as B;
    match body {
        B::RunCreated { graph_hash, .. } => {
            format!("run_created ({})", &graph_hash[..graph_hash.len().min(12)])
        }
        B::RunStarted => "run_started".to_owned(),
        B::AttemptStarted { worker, .. } => {
            format!(
                "attempt_started{}",
                worker
                    .as_deref()
                    .map_or(String::new(), |w| format!(" via {w}"))
            )
        }
        B::AttemptInterrupted => "attempt_interrupted".to_owned(),
        B::Signal { name } => format!("signal {name}"),
        B::AttemptFailed {
            reason,
            disposition,
        } => format!("attempt_failed [{disposition}]: {reason}"),
        B::BudgetExhausted { detail } => format!("budget_exhausted: {detail}"),
        B::RunFinished { disposition } => format!("run_finished: {disposition}"),
        B::Note { text } => format!("note: {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xs: &[&str]) -> Result<Cli, clap::Error> {
        let mut v = vec!["hex"];
        v.extend_from_slice(xs);
        Cli::try_parse_from(v)
    }

    #[test]
    fn parses_positional_prompt_and_json() {
        // `--json` is global, so it parses after the subcommand.
        let cli = parse(&["run", "critique-loop", "--json", "-p", "fix the bug"]).unwrap();
        assert!(cli.json);
        let Some(Command::Run {
            graph,
            prompt,
            file,
        }) = cli.command
        else {
            panic!("expected run command");
        };
        assert_eq!(graph.as_deref(), Some("critique-loop"));
        assert_eq!(
            resolve_prompt(&prompt, &file).unwrap().as_deref(),
            Some("fix the bug")
        );
    }

    #[test]
    fn global_json_also_parses_before_the_subcommand() {
        let cli = parse(&["--json", "run", "g"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn long_prompt_flag_works() {
        let cli = parse(&["run", "g", "--prompt", "do the thing"]).unwrap();
        assert!(!cli.json);
        let Some(Command::Run { prompt, file, .. }) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(
            resolve_prompt(&prompt, &file).unwrap().as_deref(),
            Some("do the thing")
        );
    }

    #[test]
    fn prompt_and_file_together_is_an_error() {
        // Mutually exclusive at parse time (clap `conflicts_with`).
        assert!(parse(&["run", "g", "-p", "x", "-f", "prompt.md"]).is_err());
    }

    #[test]
    fn prompt_flag_requires_a_value() {
        assert!(parse(&["run", "g", "-p"]).is_err());
    }

    #[test]
    fn no_prompt_resolves_to_none() {
        let cli = parse(&["run", "g"]).unwrap();
        let Some(Command::Run { prompt, file, .. }) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(resolve_prompt(&prompt, &file).unwrap(), None);
    }

    #[test]
    fn file_flag_reads_the_prompt_from_disk() {
        let dir = std::env::temp_dir().join(format!("hex-cli-p-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prompt.md");
        std::fs::write(&path, "prompt from file").unwrap();
        let cli = parse(&["run", "g", "-f", path.to_str().unwrap()]).unwrap();
        let Some(Command::Run { prompt, file, .. }) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(
            resolve_prompt(&prompt, &file).unwrap().as_deref(),
            Some("prompt from file")
        );
    }

    #[test]
    fn logs_node_filter_parses() {
        let cli = parse(&["logs", "run_1", "--node", "build", "--json"]).unwrap();
        assert!(cli.json);
        let Some(Command::Logs { run_id, node }) = cli.command else {
            panic!("expected logs command");
        };
        assert_eq!(run_id, "run_1");
        assert_eq!(node.as_deref(), Some("build"));
    }

    #[test]
    fn list_has_an_ls_alias() {
        assert!(matches!(
            parse(&["ls"]).unwrap().command,
            Some(Command::List)
        ));
    }

    #[test]
    fn bare_invocation_has_no_command() {
        assert!(parse(&[]).unwrap().command.is_none());
    }

    #[test]
    fn prompt_and_file_are_rejected_only_on_run() {
        // Structural verbs don't accept -p/-f at all.
        assert!(parse(&["validate", "g", "-p", "x"]).is_err());
    }
}
