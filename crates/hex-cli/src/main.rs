//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is one operator surface: argument parsing and rendering over a
//! [`hex_runtime::RuntimeClient`]. A human at a TTY and an agent (via injected
//! `hex emit`) share the same control protocol; every action becomes an event.
//!
//! Verbs (slim MVP): `validate` · `graph` · `run` · `resume` · `status` ·
//! `watch` · `cancel`, plus the worker-side `emit`. Redoing work is a new
//! `run`; there is no `retry`/`replay`.

use std::collections::BTreeMap;
use std::process::ExitCode;

use hex_runtime::{Disposition, Runtime, Status};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("hex: {err}");
            ExitCode::from(2)
        }
    }
}

/// Dispatch a verb. Returns the process exit code; `Err` is a usage/setup
/// failure (exit 2).
fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some((verb, rest)) = args.split_first() else {
        print_usage();
        return Ok(ExitCode::from(2));
    };

    match verb.as_str() {
        "emit" => cmd_emit(rest),
        "validate" => cmd_validate(rest),
        "graph" => cmd_graph(rest),
        "run" => cmd_run(rest),
        "resume" => cmd_resume(rest),
        "status" => cmd_status(rest),
        "watch" => cmd_watch(rest),
        "cancel" => cmd_cancel(rest),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}` (try `hex help`)")),
    }
}

/// Open a runtime rooted at the current directory.
fn open_runtime() -> Result<Runtime, String> {
    let root = hex_runtime::project_root().map_err(|e| e.to_string())?;
    Runtime::new(root).map_err(|e| e.to_string())
}

fn cmd_validate(args: &[String]) -> Result<ExitCode, String> {
    let parsed = Parsed::from(args);
    let reference = parsed.positional.first().ok_or("usage: hex validate <graph>")?;
    let runtime = open_runtime()?;
    match runtime.validate(reference, &parsed.inputs) {
        Ok(graph) => {
            println!("ok: `{}` is valid ({} nodes)", graph.name, graph.nodes.len());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(2))
        }
    }
}

fn cmd_graph(args: &[String]) -> Result<ExitCode, String> {
    let parsed = Parsed::from(args);
    let reference = parsed.positional.first().ok_or("usage: hex graph <graph>")?;
    let runtime = open_runtime()?;
    let graph = runtime.validate(reference, &parsed.inputs).map_err(|e| e.to_string())?;
    println!("graph: {}   entry: {}", graph.name, graph.entry);
    println!("nodes:");
    for node in graph.nodes.values() {
        println!("  {} [{}]", node.id, kind_name(node.spec.kind()));
    }
    println!("edges:");
    for edge in &graph.edges {
        println!("  {} --{}--> {}", edge.from, edge.on, edge.to);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_run(args: &[String]) -> Result<ExitCode, String> {
    let parsed = Parsed::from(args);
    let reference = parsed.positional.first().ok_or("usage: hex run <graph> [--input k=v]")?;
    let runtime = open_runtime()?;
    let report = runtime.start(reference, &parsed.inputs).map_err(|e| e.to_string())?;
    if parsed.json {
        let v = serde_json::json!({
            "run_id": report.run_id,
            "origin": report.origin,
            "disposition": disposition_name(report.disposition),
        });
        println!("{v}");
    } else {
        println!("run {} ({})", report.run_id, report.origin);
        println!("disposition: {}", disposition_name(report.disposition));
    }
    Ok(exit_for(report.disposition))
}

fn cmd_resume(args: &[String]) -> Result<ExitCode, String> {
    let parsed = Parsed::from(args);
    let run_id = parsed.positional.first().ok_or("usage: hex resume <run-id>")?;
    let runtime = open_runtime()?;
    let report = runtime.resume(run_id).map_err(|e| e.to_string())?;
    if parsed.json {
        let v = serde_json::json!({
            "run_id": report.run_id,
            "disposition": disposition_name(report.disposition),
        });
        println!("{v}");
    } else {
        println!("resumed {}", report.run_id);
        println!("disposition: {}", disposition_name(report.disposition));
    }
    Ok(exit_for(report.disposition))
}

fn cmd_status(args: &[String]) -> Result<ExitCode, String> {
    let parsed = Parsed::from(args);
    let run_id = parsed.positional.first().ok_or("usage: hex status <run-id>")?;
    let runtime = open_runtime()?;
    let s = runtime.status(run_id).map_err(|e| e.to_string())?;
    if parsed.json {
        let v = serde_json::json!({
            "run_id": s.run_id,
            "status": status_name(&s.status),
            "current": s.current,
            "attempts": s.attempts,
        });
        println!("{v}");
    } else {
        println!("run: {}", s.run_id);
        println!("status: {}", status_name(&s.status));
        if let Some(c) = &s.current {
            println!("current: {c}");
        }
        println!("attempts: {}", s.attempts);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_watch(args: &[String]) -> Result<ExitCode, String> {
    // Foreground MVP: runs finish synchronously, so `watch` prints the recorded
    // event stream. Live tailing arrives with the background controller.
    let parsed = Parsed::from(args);
    let run_id = parsed.positional.first().ok_or("usage: hex watch <run-id>")?;
    let runtime = open_runtime()?;
    let events = runtime.events(run_id).map_err(|e| e.to_string())?;
    for event in &events {
        if parsed.json {
            println!("{}", serde_json::to_string(event).map_err(|e| e.to_string())?);
        } else {
            let node = event.node_id.as_deref().map_or(String::new(), |n| format!(" {n}"));
            println!("#{}{} {}", event.seq, node, event_summary(&event.body));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_cancel(args: &[String]) -> Result<ExitCode, String> {
    let parsed = Parsed::from(args);
    let run_id = parsed.positional.first().ok_or("usage: hex cancel <run-id>")?;
    let runtime = open_runtime()?;
    runtime.cancel(run_id).map_err(|e| e.to_string())?;
    println!("cancelled {run_id}");
    Ok(ExitCode::SUCCESS)
}

/// Worker-side control (transport 1): append a routing event to `$HEX_EMIT_FILE`
/// after checking it is allowed by `$HEX_MAY_PROPOSE`. Agents call this.
fn cmd_emit(args: &[String]) -> Result<ExitCode, String> {
    let event = args.first().ok_or("usage: hex emit <event>")?;
    let file = std::env::var("HEX_EMIT_FILE")
        .map_err(|_| "hex emit must be run inside a hex attempt (HEX_EMIT_FILE unset)")?;
    let allowed = std::env::var("HEX_MAY_PROPOSE").unwrap_or_default();
    let permitted: Vec<&str> = allowed.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if !permitted.iter().any(|p| p == event) {
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

/// Parsed CLI arguments: positionals, `--input k=v` pairs, and `--json`.
struct Parsed {
    positional: Vec<String>,
    inputs: BTreeMap<String, String>,
    json: bool,
}

impl Parsed {
    fn from(args: &[String]) -> Self {
        let mut positional = Vec::new();
        let mut inputs = BTreeMap::new();
        let mut json = false;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--json" => json = true,
                "--input" => {
                    if let Some(pair) = args.get(i + 1)
                        && let Some((k, v)) = pair.split_once('=')
                    {
                        inputs.insert(k.to_owned(), v.to_owned());
                    }
                    i += 1;
                }
                other if other.starts_with("--input=") => {
                    if let Some((k, v)) = other.trim_start_matches("--input=").split_once('=') {
                        inputs.insert(k.to_owned(), v.to_owned());
                    }
                }
                other => positional.push(other.to_owned()),
            }
            i += 1;
        }
        Self {
            positional,
            inputs,
            json,
        }
    }
}

fn exit_for(d: Disposition) -> ExitCode {
    match d {
        Disposition::Succeeded => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

fn disposition_name(d: Disposition) -> &'static str {
    match d {
        Disposition::Succeeded => "succeeded",
        Disposition::Failed => "failed",
        Disposition::Cancelled => "cancelled",
        Disposition::BudgetExhausted => "budget_exhausted",
        Disposition::TimedOut => "timed_out",
    }
}

fn status_name(s: &Status) -> String {
    match s {
        Status::Created => "created".to_owned(),
        Status::Running => "running".to_owned(),
        Status::Paused => "paused".to_owned(),
        Status::Finished(d) => format!("finished:{}", disposition_name(*d)),
    }
}

fn kind_name(kind: hex_runtime::NodeKind) -> &'static str {
    use hex_runtime::NodeKind as K;
    match kind {
        K::Agent => "agent",
        K::Command => "command",
        K::Gate => "gate",
        K::Human => "human",
        K::Terminal => "terminal",
    }
}

fn event_summary(body: &hex_runtime::EventBody) -> String {
    use hex_runtime::EventBody as B;
    match body {
        B::RunCreated { graph_hash, .. } => format!("run_created ({})", &graph_hash[..graph_hash.len().min(12)]),
        B::RunStarted => "run_started".to_owned(),
        B::AttemptStarted { worker, .. } => {
            format!("attempt_started{}", worker.as_deref().map_or(String::new(), |w| format!(" via {w}")))
        }
        B::AttemptInterrupted => "attempt_interrupted".to_owned(),
        B::Signal { name } => format!("signal {name}"),
        B::AttemptFailed { reason } => format!("attempt_failed: {reason}"),
        B::BudgetExhausted { detail } => format!("budget_exhausted: {detail}"),
        B::RunFinished { disposition } => format!("run_finished: {}", disposition_name(*disposition)),
        B::Note { text } => format!("note: {text}"),
    }
}

fn print_usage() {
    eprintln!(
        "hex {} (protocol v{}) — thin control plane for agentic loops\n",
        env!("CARGO_PKG_VERSION"),
        hex_runtime::PROTOCOL_VERSION
    );
    for line in [
        "usage: hex <command> [args]",
        "",
        "  validate <graph> [--input k=v]   check schema, references, bounded cycles",
        "  graph <graph> [--input k=v]      render the graph (ascii)",
        "  run <graph> [--input k=v] [--json]   start a new run",
        "  resume <run-id> [--json]         continue the same run from its journal",
        "  status <run-id> [--json]         projected run status",
        "  watch <run-id> [--json]          print the run's event stream",
        "  cancel <run-id>                  record a terminal cancellation",
        "  emit <event>                     (worker-side) propose a routing event",
    ] {
        eprintln!("{line}");
    }
}
