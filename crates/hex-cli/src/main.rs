//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is one operator surface: argument parsing and rendering over a
//! [`hex_runtime::RuntimeClient`]. A human at a TTY and an agent (via injected
//! `hex emit`) share the same control protocol; every action becomes an event.
//!
//! Verbs: `validate` · `graph` · `run` · `resume` · `runs` · `status` · `watch` ·
//! `wait` · `logs` · `pause` · `steer` · `respond` · `cancel`, plus the
//! worker-side `emit`. Redoing work is a new `run`; there is no
//! `retry`/`replay`. The mid-run verbs are all thin writes to the run's control
//! inbox — the same transport a human and an agent use.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use hex_runtime::{
    Actor, Cancellation, Command as ControlCommand, Disposition, Isolation, Runtime,
};

mod preview;
#[cfg(test)]
mod test_support;

/// Worked examples, shown under `hex --help`.
const EXAMPLES: &str = "\
Examples:
  hex list                             list runnable graphs
  hex validate critique-loop           check a graph before running it
  hex run critique-loop -p \"fix bug\"   start a run with an inline prompt
  hex run critique-loop -f task.md     read the prompt from a file
  hex run tdd -p \"fix bug\" --detach    start a run in the background
  hex runs                             list runs (ids, status, age)
  hex status <run-id>                  show a run's status
  hex logs <run-id> --node reviewer    show one node's agent output
  hex wait <run-id>                    block until a run finishes
  hex steer <run-id> \"use the v2 API\"  guide the next attempt
  hex respond <run-id> \"approved\"      answer a waiting human node
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
    /// Check that configured workers and checks are actually usable
    Doctor,
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
        /// Name this run (used in the run id; else <workflow>-<short-uuid>)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Run in an isolated git worktree, branched from BASE (default HEAD)
        #[arg(
            long,
            value_name = "BASE",
            num_args = 0..=1,
            default_missing_value = "",
            conflicts_with = "no_worktree"
        )]
        worktree: Option<String>,
        /// Force the shared workspace (project root), overriding any default
        #[arg(long)]
        no_worktree: bool,
        /// Warmup argv run once in a fresh/reclaimed worktree (no shell)
        #[arg(long, value_name = "CMD", requires = "worktree")]
        worktree_init: Option<String>,
        /// Run in the background: print the run id and return immediately
        #[arg(long)]
        detach: bool,
        /// Internal: drive the run id a `--detach` launcher reserved
        #[arg(long, value_name = "RUN_ID", hide = true, conflicts_with = "detach")]
        reserved_run_id: Option<String>,
    },
    /// Resume the SAME run from its journal (after a pause or crash)
    Resume {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// List runs (newest activity first)
    Runs,
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
    /// Show each attempt's final message (`--full` for full stdout/stderr)
    Logs {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// Show only this node's attempts
        #[arg(long, value_name = "NODE")]
        node: Option<String>,
        /// Show full captured stdout/stderr, not just each attempt's final message
        #[arg(long)]
        full: bool,
    },
    /// Block until a run finishes, exiting with its disposition code
    Wait {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// Pause a run at its next attempt boundary (`hex resume` continues it)
    Pause {
        /// Run id, as printed by `hex run`
        run_id: String,
    },
    /// Add operator guidance to the run's next attempt
    Steer {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// Guidance text, injected into the next attempt's prompt
        #[arg(value_name = "TEXT")]
        text: String,
    },
    /// Answer a `human` node that is blocking a run
    Respond {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// The answer, stored as the node's result
        #[arg(value_name = "TEXT")]
        text: String,
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
        Command::Doctor => cmd_doctor(json),
        Command::Validate { graph } => cmd_validate(&graph, json),
        Command::Graph { graph } => cmd_graph(&graph, json),
        Command::Run {
            graph,
            prompt,
            file,
            name,
            worktree,
            no_worktree,
            worktree_init,
            detach,
            reserved_run_id,
        } => cmd_run(
            graph.as_deref(),
            resolve_prompt(&prompt, &file)?,
            name.as_deref(),
            isolation_from(worktree.as_deref(), no_worktree, worktree_init.as_deref()),
            RunMode {
                detach,
                reserved: reserved_run_id,
            },
            json,
            no_preview,
        ),
        Command::Resume { run_id } => cmd_resume(&run_id, json, no_preview),
        Command::Runs => cmd_runs(json),
        Command::Status { run_id } => cmd_status(&run_id, json),
        Command::Watch { run_id } => cmd_watch(&run_id, json),
        Command::Logs { run_id, node, full } => cmd_logs(&run_id, node.as_deref(), full, json),
        Command::Wait { run_id } => cmd_wait(&run_id, json),
        Command::Pause { run_id } => cmd_control(&run_id, &ControlCommand::Pause, json),
        Command::Steer { run_id, text } => {
            cmd_control(&run_id, &ControlCommand::Steer { text }, json)
        }
        Command::Respond { run_id, text } => {
            cmd_control(&run_id, &ControlCommand::Respond { text }, json)
        }
        Command::Cancel { run_id } => cmd_cancel(&run_id, json),
        Command::Emit { event } => cmd_emit(&event),
    }
}

/// How `hex run` should execute: foreground (the default), detaching, or — when
/// the launcher re-execs us — driving the run id it reserved.
struct RunMode {
    detach: bool,
    reserved: Option<String>,
}

/// The actor a command from this CLI is issued as. A human at a TTY and an agent
/// share one protocol, so the *identity* is what distinguishes them; `$USER` is
/// the best local handle available without asking.
fn operator_actor() -> Actor {
    Actor::human(
        std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "local".to_owned()),
    )
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

/// Print the runnable graphs (shared by `hex list` and bare `hex run`). Each
/// entry shows its name + origin, a one-line description, and a ready-to-run
/// example invocation.
fn print_graph_list(runtime: &Runtime, json: bool) {
    let graphs = runtime.list_graphs();
    if json {
        let items: Vec<_> = graphs
            .iter()
            .map(|g| {
                serde_json::json!({
                    "name": g.name,
                    "origin": g.origin,
                    "description": g.description,
                    "example": g.example,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "graphs": items }));
        return;
    }
    if graphs.is_empty() {
        println!("no graphs found (add one to .hex/graphs/ or ~/.config/hex/graphs/)");
        return;
    }
    println!("available graphs:\n");
    for g in &graphs {
        println!("  {}  ({})", g.name, g.origin);
        if let Some(desc) = &g.description {
            println!("      {desc}");
        }
        let example = g.example.as_deref().unwrap_or("<prompt>");
        println!("      hex run {} -p \"{example}\"\n", g.name);
    }
}

/// Report whether every configured worker and check can actually run. Exit 1 if
/// anything is broken, so CI (or a driving agent) can gate on it.
fn cmd_doctor(json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let report = runtime.doctor();
    if json {
        let findings = report
            .findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "kind": f.kind,
                    "name": f.name,
                    "program": f.program,
                    "ok": f.ok,
                    "detail": f.detail,
                })
            })
            .collect::<Vec<_>>();
        let v = serde_json::json!({ "ok": report.ok(), "findings": findings });
        println!("{v}");
    } else if report.findings.is_empty() {
        println!("no workers or checks configured");
    } else {
        for f in &report.findings {
            let mark = if f.ok { "ok     " } else { "MISSING" };
            println!("  {mark} {:<7} {:<12} {}", f.kind, f.name, f.detail);
        }
        if report.ok() {
            println!("\nall good");
        } else {
            eprintln!(
                "\n{} unusable: {}\ninstall the missing tools, or fix `.hex/config.yaml`",
                report.broken().len(),
                report.broken().join(", ")
            );
        }
    }
    Ok(if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
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

/// Build the isolation policy from the CLI flags. `--no-worktree` (or neither
/// flag) → shared; `--worktree` → worktree from HEAD; `--worktree <base>` → from
/// that base. `--worktree-init "<argv>"` is whitespace-split (no shell).
fn isolation_from(worktree: Option<&str>, no_worktree: bool, init: Option<&str>) -> Isolation {
    if no_worktree {
        return Isolation::Shared;
    }
    match worktree {
        None => Isolation::Shared,
        Some(base) => Isolation::Worktree {
            base: (!base.is_empty()).then(|| base.to_owned()),
            init: init
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
        },
    }
}

fn cmd_run(
    reference: Option<&str>,
    prompt: Option<String>,
    name: Option<&str>,
    isolation: Isolation,
    mode: RunMode,
    json: bool,
    no_preview: bool,
) -> Result<ExitCode, String> {
    let runtime = open_runtime_streaming(json, no_preview)?;
    // `hex run` with no graph lists what you can run instead of erroring.
    let Some(reference) = reference else {
        print_graph_list(&runtime, json);
        return Ok(ExitCode::SUCCESS);
    };
    if mode.detach {
        return cmd_detach(
            &runtime,
            reference,
            prompt.as_deref(),
            name,
            &isolation,
            json,
        );
    }
    let report = match &mode.reserved {
        Some(run_id) => runtime.start_reserved(run_id, reference, prompt.as_deref(), &isolation),
        None => runtime.start(reference, prompt.as_deref(), name, &isolation),
    }
    .map_err(|e| e.to_string())?;
    print_outcome(&report, json, "run");
    Ok(exit_for_report(&report))
}

/// Launch a run in the background and return its id immediately.
///
/// The run id is reserved (and the graph validated) here, in the foreground, so a
/// bad graph or a missing agent CLI is reported to the operator instead of dying
/// unseen in the child. Then we re-exec ourselves to drive that reservation.
fn cmd_detach(
    runtime: &Runtime,
    reference: &str,
    prompt: Option<&str>,
    name: Option<&str>,
    isolation: &Isolation,
    json: bool,
) -> Result<ExitCode, String> {
    let (run_id, run_dir) = runtime
        .reserve(reference, prompt, name)
        .map_err(|e| e.to_string())?;

    let mut argv: Vec<String> = vec![
        "run".to_owned(),
        reference.to_owned(),
        "--reserved-run-id".to_owned(),
        run_id.clone(),
    ];
    if let Some(text) = prompt {
        // Pass the resolved text, not `-f`: the child must run the prompt the
        // launcher validated, even if the file changes underneath it.
        argv.push("--prompt".to_owned());
        argv.push(text.to_owned());
    }
    if let Isolation::Worktree { base, init } = isolation {
        argv.push("--worktree".to_owned());
        argv.push(base.clone().unwrap_or_default());
        if !init.is_empty() {
            argv.push("--worktree-init".to_owned());
            argv.push(init.join(" "));
        }
    }
    spawn_detached(&argv, &run_dir)?;

    if json {
        println!(
            "{}",
            serde_json::json!({ "run_id": run_id, "detached": true })
        );
    } else {
        println!("{run_id}");
        eprintln!("detached; follow with `hex wait {run_id}` or `hex watch {run_id}`");
    }
    Ok(ExitCode::SUCCESS)
}

/// Spawn `hex <argv>` as a run that outlives this process.
///
/// `spawn`, never `fork()` — forking a multithreaded Rust process is unsafe, and
/// this binary is multithreaded. `process_group(0)` puts the child in its own
/// process group so a Ctrl-C (or the shell reaping our group) does not kill the
/// run, and we deliberately never `wait()`: the parent exits at once, the child
/// is reparented, and no zombie is left behind.
#[cfg(unix)]
fn spawn_detached(argv: &[String], run_dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command as Proc, Stdio};

    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the hex binary: {e}"))?;
    let out = std::fs::File::create(run_dir.join("detached.out"))
        .map_err(|e| format!("cannot create detached.out: {e}"))?;
    let err = std::fs::File::create(run_dir.join("detached.err"))
        .map_err(|e| format!("cannot create detached.err: {e}"))?;
    Proc::new(exe)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot spawn the detached run: {e}"))?;
    Ok(())
}

#[cfg(not(unix))]
fn spawn_detached(_argv: &[String], _run_dir: &std::path::Path) -> Result<(), String> {
    Err("--detach needs a unix process group; run in the foreground".to_owned())
}

fn cmd_resume(run_id: &str, json: bool, no_preview: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime_streaming(json, no_preview)?;
    let report = runtime.resume(run_id).map_err(|e| e.to_string())?;
    print_outcome(&report, json, "resumed");
    Ok(exit_for_report(&report))
}

/// Render the end of a `run`/`resume`. A paused run has no disposition — saying
/// `succeeded` (or nothing) would misreport a run that is merely suspended.
fn print_outcome(report: &hex_runtime::RunReport, json: bool, verb: &str) {
    let disposition = disposition_label(report.disposition, "paused");
    if json {
        let v = serde_json::json!({
            "run_id": report.run_id,
            "origin": report.origin,
            "disposition": disposition,
            "paused": report.disposition.is_none(),
        });
        println!("{v}");
    } else {
        println!("{verb} {} ({})", report.run_id, report.origin);
        println!("disposition: {disposition}");
        if report.disposition.is_none() {
            eprintln!("paused; continue with `hex resume {}`", report.run_id);
        }
    }
}

fn cmd_runs(json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let runs = runtime.list_runs().map_err(|e| e.to_string())?;
    if json {
        let items: Vec<_> = runs
            .iter()
            .map(|r| {
                serde_json::json!({
                    "run_id": r.run_id,
                    "status": r.status.as_ref().map(ToString::to_string),
                    "current": r.current,
                    "attempts": r.attempts,
                    "disposition": disposition_json(r.disposition),
                    "liveness": r.liveness.to_string(),
                    "created_at_ms": r.created_at_ms,
                    "updated_at_ms": r.updated_at_ms,
                    "error": r.error,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "runs": items }));
        return Ok(ExitCode::SUCCESS);
    }
    if runs.is_empty() {
        println!("no runs yet (start one with `hex run <graph> -p \"…\"`)");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:<34} {:<12} {:<10} {:<14} PROCESS",
        "RUN", "STATE", "AGE", "NODE"
    );
    for r in &runs {
        let state = r
            .status
            .as_ref()
            .map_or("unreadable".to_owned(), ToString::to_string);
        println!(
            "{:<34} {:<12} {:<10} {:<14} {}",
            r.run_id,
            state,
            age(r.updated_at_ms),
            r.current.as_deref().unwrap_or("-"),
            r.liveness,
        );
        if let Some(err) = &r.error {
            eprintln!("  {}: {err}", r.run_id);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A compact "how long ago" for a listing (`3m`, `2h`, `4d`).
fn age(at_ms: u64) -> String {
    if at_ms == 0 {
        return "-".to_owned();
    }
    let secs = hex_runtime::journal::now_ms().saturating_sub(at_ms) / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// How long `wait` tolerates "nobody is driving this unfinished run" before
/// giving up. A grace period, because a just-detached run has not yet taken its
/// lock and would otherwise look abandoned the instant it was launched.
const WAIT_ABANDONED_GRACE_MS: u64 = 5_000;

/// Block until a run reaches an outcome, then exit with its disposition code.
///
/// Also returns when the run *cannot* finish on its own — paused, or abandoned by
/// its driver — rather than waiting forever for a process that is not coming
/// back.
fn cmd_wait(run_id: &str, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let mut abandoned_since: Option<std::time::Instant> = None;
    loop {
        let summary = runtime.summary(run_id).map_err(|e| e.to_string())?;
        let verdict = match summary.liveness {
            hex_runtime::Liveness::Finished => Some((
                disposition_label(summary.disposition, "failed"),
                summary.disposition.map_or(ExitCode::from(1), exit_for),
            )),
            hex_runtime::Liveness::Paused => Some(("paused".to_owned(), ExitCode::from(PAUSED))),
            hex_runtime::Liveness::Abandoned => {
                let since = abandoned_since.get_or_insert_with(std::time::Instant::now);
                let waited = u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX);
                (waited >= WAIT_ABANDONED_GRACE_MS)
                    .then(|| ("abandoned".to_owned(), ExitCode::from(1)))
            }
            hex_runtime::Liveness::Live | hex_runtime::Liveness::Hung => {
                abandoned_since = None;
                None
            }
        };
        if let Some((state, code)) = verdict {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "run_id": summary.run_id,
                        "state": state,
                        "disposition": disposition_json(summary.disposition),
                    })
                );
            } else {
                println!("{state}");
                if state == "abandoned" {
                    eprintln!("nothing is driving `{run_id}` — continue it with `hex resume`");
                }
            }
            return Ok(code);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Queue a control command (pause/steer/respond) for whoever is driving the run.
fn cmd_control(run_id: &str, command: &ControlCommand, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let actor = operator_actor();
    runtime
        .control(run_id, &actor, command)
        .map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "run_id": run_id, "queued": command.as_str() })
        );
    } else {
        // Queued, not applied: the driver picks it up at its next attempt
        // boundary, and the journal is where the effect shows up.
        println!("queued {} for {run_id}", command.as_str());
    }
    Ok(ExitCode::SUCCESS)
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
            "disposition": disposition_json(s.disposition),
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

fn cmd_logs(run_id: &str, node: Option<&str>, full: bool, json: bool) -> Result<ExitCode, String> {
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
                    "result": l.result,
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
        let out_tty = std::io::stdout().is_terminal();
        let err_tty = std::io::stderr().is_terminal();
        if full {
            // Full captured output, dimmed. Each stream is colored by its OWN
            // TTY, so redirecting one doesn't leak ANSI into the other.
            if !l.stdout.trim().is_empty() {
                print!("{}", grey(&l.stdout, out_tty));
                if !l.stdout.ends_with('\n') {
                    println!();
                }
            }
            if !l.stderr.trim().is_empty() {
                eprint!("{}", grey(&l.stderr, err_tty));
            }
        } else {
            // Default: just the attempt's final message, dimmed (on stdout).
            match &l.result {
                Some(text) => println!("{}", grey(text, out_tty)),
                None => println!("{}", grey("(no final message captured)", out_tty)),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Dim `s` to grey when its target stream `is_tty` (and NO_COLOR is unset).
fn grey(s: &str, is_tty: bool) -> String {
    if is_tty && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[90m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

fn cmd_cancel(run_id: &str, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let outcome = runtime
        .cancel(run_id, &operator_actor())
        .map_err(|e| e.to_string())?;
    // A live run is cancelled by its own driver (it holds the journal's single
    // write lock), so the honest report is "requested", not "cancelled".
    let requested = outcome == Cancellation::Requested;
    if json {
        let v = serde_json::json!({
            "run_id": run_id,
            "cancelled": !requested,
            "requested": requested,
        });
        println!("{v}");
    } else if requested {
        println!("cancel queued for {run_id} (a live driver will stop at its next boundary)");
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

/// Map a run's disposition to a distinct exit code, so a script (or a driving
/// agent) can branch on the outcome without parsing output. `2` is reserved by
/// clap for usage errors, so the dispositions start at `3`.
///
/// | code | meaning |
/// |---|---|
/// | 0 | succeeded |
/// | 1 | failed (or a hex error) |
/// | 2 | usage error (clap) |
/// | 3 | timed out |
/// | 4 | budget exhausted |
/// | 5 | cancelled |
/// | 6 | paused (no disposition yet — resumable) |
fn exit_for(d: Disposition) -> ExitCode {
    ExitCode::from(match d {
        Disposition::Succeeded => 0,
        Disposition::Failed => 1,
        Disposition::TimedOut => 3,
        Disposition::BudgetExhausted => 4,
        Disposition::Cancelled => 5,
    })
}

/// Exit code for a paused run. Deliberately *not* 0: a suspended run has not
/// succeeded, and a script that treated it as success would move on from work
/// that has not happened yet.
const PAUSED: u8 = 6;

/// A disposition for a `--json` field: `null` when the run has no terminal
/// disposition yet, **never** a stand-in word — a machine consumer must be able
/// to tell "not finished" from an outcome, and this field once did not exist at
/// all, forcing one to string-split `status`.
fn disposition_json(disposition: Option<Disposition>) -> Option<String> {
    disposition.map(|d| d.to_string())
}

/// A disposition as one display word, with the caller naming what *its* "no
/// disposition yet" means. The two callers genuinely differ, which is why the
/// fallback is a parameter rather than a constant: a `run`/`resume` that returned
/// without a terminal is **paused** (see [`print_outcome`]), whereas `wait` only
/// formats a disposition once liveness says `Finished`, so a missing one there is
/// a journal that lost its outcome — reported `failed`, fail-closed.
fn disposition_label(disposition: Option<Disposition>, if_none: &str) -> String {
    disposition.map_or_else(|| if_none.to_owned(), |d| d.to_string())
}

/// Exit code for a finished-or-paused `run`/`resume`.
fn exit_for_report(report: &hex_runtime::RunReport) -> ExitCode {
    report.disposition.map_or(ExitCode::from(PAUSED), exit_for)
}

fn event_summary(body: &hex_runtime::EventBody) -> String {
    use hex_runtime::EventBody as B;
    match body {
        B::RunCreated { graph_hash, .. } => {
            format!(
                "run_created ({})",
                graph_hash.chars().take(12).collect::<String>()
            )
        }
        B::RunStarted => "run_started".to_owned(),
        B::RunPaused => "run_paused".to_owned(),
        B::RunResumed => "run_resumed".to_owned(),
        B::Steered { text } => format!("steer: {text}"),
        B::HumanRequested { prompt } => format!("human_requested: {prompt}"),
        B::HumanResponded { text, signal } => format!("human_responded [{signal}]: {text}"),
        B::AcceptanceUnmet { missing, to } => format!(
            "acceptance unmet (missing {}) → back to {to}",
            missing.join(", ")
        ),
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
        B::NodeResult { text } => format!("result ({} chars)", text.chars().count()),
        B::AttemptFailed {
            reason,
            disposition,
        } => format!("attempt_failed [{disposition}]: {reason}"),
        B::RunFinished { disposition } => format!("run_finished: {disposition}"),
        B::Note { text } => format!("note: {text}"),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_support::unique;

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
            ..
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
        let dir =
            std::env::temp_dir().join(format!("hex-cli-p-{}-{}", std::process::id(), unique()));
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
    fn logs_flags_parse() {
        let cli = parse(&["logs", "run_1", "--node", "build", "--full", "--json"]).unwrap();
        assert!(cli.json);
        let Some(Command::Logs { run_id, node, full }) = cli.command else {
            panic!("expected logs command");
        };
        assert_eq!(run_id, "run_1");
        assert_eq!(node.as_deref(), Some("build"));
        assert!(full);
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
