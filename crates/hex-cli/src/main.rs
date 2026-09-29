//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is one operator surface: argument parsing and rendering over
//! [`hex_runtime::Runtime`]. A human at a TTY and an agent driving hex from a
//! shell share the same control protocol; every action becomes an event.
//!
//! Verbs: `init` · `validate` · `graph` · `run` · `resume` · `runs` · `status` ·
//! `wait` · `logs` · `pause` · `steer` · `respond` · `cancel`. Redoing
//! work is a new `run`; there is no `retry`/`replay`. The mid-run verbs are all
//! thin writes to the run's control inbox — the same transport a human and an
//! agent use.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use hex_runtime::{
    Actor, Cancellation, Command as ControlCommand, Disposition, Isolation, Runtime,
};

#[macro_use]
mod out;
mod agent_stream;
mod feedback;
mod graph_view;
mod preview;
mod ui;
use hex_runtime::Layer;

/// Worked examples, shown under `hex --help`.
const EXAMPLES: &str = "\
Examples:
  hex init                             set this repository up for hex
  hex list                             list runnable graphs
  hex validate critique-loop           check a graph before running it
  hex run critique-loop -p \"fix bug\"   start a run with an inline prompt
  hex run critique-loop -f task.md     read the prompt from a file
  hex run critique-loop -p \"fix bug\" & run it in the background (your shell)
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

    /// Emit machine-readable JSON on stdout, for the commands that have a
    /// machine form (`hex graph` does not — a graph's machine form is its YAML,
    /// via `--format source`).
    #[arg(long, global = true)]
    json: bool,

    /// When to colour output
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    color: ui::When,
}

/// The operator/worker verbs. Names are stable public surface.
#[derive(Subcommand)]
enum Command {
    /// Set this repository up for hex: `.hex/`, a starter config, `.gitignore`
    Init,
    /// List runnable graphs (project, then user, then built-in)
    #[command(visible_alias = "ls")]
    List,
    /// Check that configured workers and checks are actually usable
    Doctor,
    /// Validate a graph: schema, references, a reachable success
    Validate {
        /// Graph reference: a preset name or a path to a `.yaml` file
        #[arg(value_name = "GRAPH")]
        graph: String,
    },
    /// Render a graph: as text, or its YAML source
    Graph {
        /// Graph reference: a preset name or a path to a `.yaml` file
        #[arg(value_name = "GRAPH")]
        graph: String,
        /// Output format (`source` prints the YAML, the copy-and-customise path)
        #[arg(long, value_enum, default_value_t = GraphFormat::Text)]
        format: GraphFormat,
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
        /// Name this run (used in the run id; else the graph name and a short uuid)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Run in an isolated git worktree, branched from BASE (default HEAD)
        #[arg(long, value_name = "BASE", num_args = 0..=1, default_missing_value = "")]
        worktree: Option<String>,
        /// Warmup argv run once in a fresh/reclaimed worktree (no shell)
        #[arg(long, value_name = "CMD", requires = "worktree")]
        worktree_init: Option<String>,
        /// Disable the live in-flight preview pane (plain line streaming instead)
        #[arg(long)]
        no_preview: bool,
    },
    /// Resume the SAME run from its journal (after a pause or crash)
    Resume {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// Disable the live in-flight preview pane (plain line streaming instead)
        #[arg(long)]
        no_preview: bool,
    },
    /// List runs (newest activity first)
    Runs,
    /// Show a run's projected status
    Status {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// Also break the spend down per node and per model
        #[arg(long)]
        usage: bool,
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
        /// Last N lines of an attempt still running (default 20)
        #[arg(long, value_name = "N")]
        tail: Option<usize>,
        /// Stream the attempt's output until the run finishes (streams every
        /// captured byte, so `--full` adds nothing; honours `--node`)
        #[arg(long, conflicts_with = "json")]
        follow: bool,
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
    /// Record feedback about hex to `~/.hex/feedback.jsonl` (issues, missing
    /// capabilities); auto-captures the run/graph/agent/project context
    Feedback {
        /// The feedback text
        #[arg(value_name = "MESSAGE")]
        message: String,
        /// Optional category, e.g. `issue`, `missing-capability`, `idea`
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
    },
    /// Show what this machine has asked hex to do, folded from `~/.hex/stats.jsonl`
    Stats,
    /// Remove old finished/interrupted run directories and release their worktree slots
    Prune {
        /// Only remove runs whose journal is older than this (humantime, e.g. `7d`)
        #[arg(long, value_name = "DUR")]
        older_than: Option<String>,
        /// Remove every non-live run, regardless of age
        #[arg(long)]
        all: bool,
    },
}

/// How `hex graph` renders. Both go to stdout and exit 0.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum GraphFormat {
    /// The flow, for a terminal.
    Text,
    /// The graph's YAML verbatim — the copy-and-customise path, and the only
    /// machine form a graph has: the exact text a run compiles.
    Source,
}

fn main() -> ExitCode {
    // clap handles `--help`/`-h`/`--version` and parse/usage errors itself
    // (usage errors exit 2, help/version exit 0), matching the old exit codes.
    // Parsed in two steps so the stats log gets clap's own canonical verb name
    // (aliases resolved) instead of a hand-kept match over every variant.
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    match dispatch(cli, matches.subcommand_name().unwrap_or_default()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("hex: {err}");
            ExitCode::from(2)
        }
    }
}

/// Dispatch a verb. Returns the process exit code; `Err` is a usage/setup
/// failure (exit 2).
fn dispatch(cli: Cli, verb: &str) -> Result<ExitCode, String> {
    let json = cli.json;
    let Some(command) = cli.command else {
        // Bare `hex` is a usage error, so render clap's own help — byte-for-byte
        // the same text `hex --help` prints — to stderr and exit 2. (clap's
        // `--help` uses the compact `render_help`, not the verbose long form.)
        eprint!("{}", Cli::command().render_help());
        return Ok(ExitCode::from(2));
    };

    // One usage line per invocation, with two exclusions. `init` creates the
    // project's `.hex/`, and a usage log must never create that directory first.
    // The observation verbs (`status`, `logs`, `wait`, `runs`, `stats`) are the
    // ones agents poll in a loop — logging them grows the file with poll
    // frequency instead of with work done, which breaks the fold-on-read ceiling.
    if !matches!(verb, "init" | "status" | "logs" | "wait" | "runs" | "stats") {
        hex_runtime::stats::record_cli(
            verb,
            caller_source(),
            &std::env::current_dir().unwrap_or_default(),
        );
    }

    match command {
        Command::Init => cmd_init(json),
        Command::List => cmd_list(json, ui::Ui::stdout(cli.color, json)),
        Command::Doctor => cmd_doctor(json, cli.color, ui::Ui::stdout(cli.color, json)),
        Command::Validate { graph } => cmd_validate(&graph, json),
        Command::Graph { graph, format } => {
            cmd_graph(&graph, format, json, ui::Ui::stdout(cli.color, json))
        }
        Command::Run {
            graph,
            prompt,
            file,
            name,
            worktree,
            worktree_init,
            no_preview,
        } => cmd_run(
            graph.as_deref(),
            resolve_prompt(prompt, file.as_deref())?,
            name.as_deref(),
            isolation_from(worktree.as_deref(), worktree_init.as_deref()),
            json,
            no_preview,
            ui::Ui::stdout(cli.color, json),
        ),
        Command::Resume { run_id, no_preview } => {
            cmd_resume(&run_id, json, no_preview, ui::Ui::stdout(cli.color, json))
        }
        Command::Runs => cmd_runs(json, ui::Ui::stdout(cli.color, json)),
        Command::Status { run_id, usage } => {
            cmd_status(&run_id, usage, json, ui::Ui::stdout(cli.color, json))
        }
        Command::Logs {
            run_id,
            node,
            full,
            tail,
            follow,
        } => cmd_logs(
            &run_id,
            node.as_deref(),
            full,
            tail,
            follow,
            json,
            ui::Ui::stdout(cli.color, json),
        ),
        Command::Wait { run_id } => cmd_wait(&run_id, json),
        Command::Pause { run_id } => cmd_control(&run_id, &ControlCommand::Pause, json),
        Command::Steer { run_id, text } => {
            cmd_control(&run_id, &ControlCommand::Steer { text }, json)
        }
        Command::Respond { run_id, text } => {
            cmd_control(&run_id, &ControlCommand::Respond { text }, json)
        }
        Command::Cancel { run_id } => cmd_cancel(&run_id, json),
        Command::Feedback { message, kind } => feedback::record(&message, kind.as_deref()),
        Command::Stats => cmd_stats(json, ui::Ui::stdout(cli.color, json)),
        Command::Prune { older_than, all } => cmd_prune(older_than.as_deref(), all, json),
    }
}

/// Which class of caller invoked this verb — the coarse fact a run journal
/// cannot hold. `subgraph` means it ran inside an attempt (the runtime injected
/// `HEX_RUN_ID`); otherwise a terminal is `interactive` and a pipe or an agent's
/// shell is `non-interactive`.
fn caller_source() -> &'static str {
    if std::env::var_os("HEX_RUN_ID").is_some() {
        "subgraph"
    } else if std::io::stdout().is_terminal() {
        "interactive"
    } else {
        "non-interactive"
    }
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
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
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
    // Ctrl-C must kill the agent, not just us: it runs in its own process group,
    // so the tty's SIGINT never reaches it and it would otherwise keep working
    // (and spending) unlogged and unnoticed. Installed for foreground `run` and
    // `resume` only — the two commands that own a live agent.
    let sink = std::sync::Arc::new(preview::LivePreview::new(preview));
    let banner = std::sync::Arc::clone(&sink);
    hex_runtime::interrupt::install(move || {
        // Tearing down a process group takes up to the SIGTERM grace period, so
        // say so; a silent pause reads as a hang. Through the preview, not a raw
        // stderr write: while the footer is live, its render thread owns the
        // terminal, and an interleaved write desyncs the inline viewport.
        banner.notice(
            "interrupting: stopping the agent and saving progress (press again to force-quit)",
        );
    });
    Ok(open_runtime()?.with_progress(sink))
}

/// A one-line rendering of an event for progress output.
pub(crate) fn event_line(e: &hex_runtime::Event) -> String {
    let node = e
        .node_id
        .as_deref()
        .map_or(String::new(), |n| format!(" {n}"));
    // Wall clock (UTC) so a run redirected to a log file carries its own
    // timeline — an agent tailing it can tell 2 minutes of silence from 20.
    let s = e.at_ms / 1000;
    format!(
        "{:02}:{:02}:{:02} #{}{} {}",
        (s / 3600) % 24,
        (s / 60) % 60,
        s % 60,
        e.seq,
        node,
        event_summary(&e.body)
    )
}

/// Set the current repository up for hex: the runtime creates the layout, this
/// renders what it made.
fn cmd_init(json: bool) -> Result<ExitCode, String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let report = hex_runtime::init(&root).map_err(|e| e.to_string())?;
    if json {
        let mut v = serde_json::to_value(&report).map_err(|e| e.to_string())?;
        v["root"] = root.display().to_string().into();
        outln!("{v}");
    } else {
        for name in &report.created {
            outln!("created  {name}");
        }
        for name in &report.existed {
            outln!("exists   {name}");
        }
        outln!(
            "\ndeclare your checks in .hex/config.yaml, pin models in \
             ~/.config/hex/config.yaml, then `hex list` to see what you can run"
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// A path as written relative to the project root, for reporting what was made.
fn relative(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn cmd_list(json: bool, ui: ui::Ui) -> Result<ExitCode, String> {
    print_graph_list(&open_runtime()?, json, ui);
    Ok(ExitCode::SUCCESS)
}

/// Print the runnable graphs (shared by `hex list` and bare `hex run`). Each
/// entry shows its name + origin, a one-line description, and a ready-to-run
/// example invocation.
fn print_graph_list(runtime: &Runtime, json: bool, ui: ui::Ui) {
    let graphs = runtime.list_graphs();
    if json {
        outln!("{}", serde_json::json!({ "graphs": graphs }));
        return;
    }
    if graphs.is_empty() {
        outln!("no graphs found (add one to .hex/graphs/ or ~/.config/hex/graphs/)");
        return;
    }
    // Grouped by layer, highest precedence first: which graph actually runs is
    // decided by the layer, so a flat alphabetical list buried the one fact the
    // reader needs. Within a group, alphabetical.
    let root = std::env::current_dir().unwrap_or_default();
    let mut first_group = true;
    for layer in [Layer::Project, Layer::User, Layer::BuiltIn] {
        let group: Vec<&hex_runtime::GraphEntry> =
            graphs.iter().filter(|g| g.layer == layer).collect();
        if group.is_empty() {
            continue;
        }
        // The directory belongs in the heading: one line per group beats one
        // line per graph, and it answers "where would I put a new one".
        let dir = group
            .first()
            .map(|g| std::path::Path::new(&g.origin))
            .filter(|_| layer != Layer::BuiltIn)
            .and_then(std::path::Path::parent)
            .map(|d| format!("   {}", relative(&root, d)))
            .unwrap_or_default();
        // No empty escape pair when a layer has no directory to name.
        if first_group {
            first_group = false;
        } else {
            outln!();
        }
        if dir.is_empty() {
            outln!("{}", ui.paint(ui::style::HEADER, layer.label()));
        } else {
            outln!(
                "{}{}",
                ui.paint(ui::style::HEADER, layer.label()),
                ui.paint(ui::style::DIM, &dir)
            );
        }
        let mut table = ui::Table::new(&["", "GRAPH", "DESCRIPTION"], &[false; 3]).flex(2);
        for g in group {
            table.row(vec![
                // A project graph hiding a built-in of the same name is a
                // surprise; mark it rather than letting the built-in vanish.
                if g.shadows {
                    ui.paint(ui::style::WARN, "*").to_string()
                } else {
                    " ".to_owned()
                },
                ui.paint(ui::style::ID, &g.name).to_string(),
                ui.paint(ui::style::DIM, g.description.as_deref().unwrap_or("—"))
                    .to_string(),
            ]);
        }
        for line in table.render(ui) {
            outln!("  {line}");
        }
    }
    if graphs.iter().any(|g| g.shadows) {
        outln!(
            "\n{}",
            ui.paint(ui::style::DIM, "* shadows a lower layer of the same name")
        );
    }
    outln!(
        "\n{}",
        ui.paint(
            ui::style::DIM,
            "hex graph <name>   to read one   ·   hex run <name> -p \"…\"   to start it"
        )
    );
}

/// Report whether every configured worker and check can actually run. Exit 1 if
/// anything is broken, so CI (or a driving agent) can gate on it.
fn cmd_doctor(json: bool, color: ui::When, ui: ui::Ui) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let report = runtime.doctor();
    if json {
        let v = serde_json::json!({ "ok": report.ok(), "findings": report.findings });
        outln!("{v}");
    } else if report.findings.is_empty() {
        outln!("no workers or checks configured");
    } else {
        // The remediation text does not go in a cell: one 200-character
        // explanation set the DETAIL column width for all eleven rows and wrapped
        // three times on an 80-column terminal. Failures repeat it below, wrapped.
        let mut table = ui::Table::new(&["", "KIND", "NAME", "DETAIL"], &[false; 4]).flex(3);
        for f in &report.findings {
            table.row(vec![
                ui.mark(if f.ok { ui::Mark::Ok } else { ui::Mark::Fail }),
                ui.paint(ui::style::DIM, f.kind).to_string(),
                ui.paint(ui::style::ID, &f.name).to_string(),
                ui.paint(ui::style::DIM, first_line(&f.detail)).to_string(),
            ]);
        }
        for line in table.render(ui) {
            outln!("{line}");
        }
        if !report.ok() {
            let err = ui::Ui::stderr(color, json);
            eprintln!(
                "\nhex: {} of {} unusable.",
                report.broken().len(),
                report.findings.len()
            );
            for f in report.findings.iter().filter(|f| !f.ok) {
                eprintln!("\n  {}  {}", err.paint(ui::style::ID, &f.name), f.detail);
            }
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
                outln!("{v}");
            } else {
                outln!(
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
                outln!("{v}");
            } else {
                eprintln!("{e}");
            }
            Ok(ExitCode::from(2))
        }
    }
}

fn cmd_graph(
    reference: &str,
    format: GraphFormat,
    json: bool,
    ui: ui::Ui,
) -> Result<ExitCode, String> {
    // `--json` is refused, not ignored: a graph's machine form is its YAML.
    if json {
        return Err(
            "`hex graph` has no machine form — the graph's machine form is its YAML \
             (`hex graph <name> --format source`)"
                .to_owned(),
        );
    }
    let runtime = open_runtime()?;
    // Source is the one format that must work on a graph that does not compile:
    // you reach for it precisely to fix one.
    if matches!(format, GraphFormat::Source) {
        out!(
            "{}",
            runtime.graph_source(reference).map_err(|e| e.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }
    let graph = runtime.validate(reference).map_err(|e| e.to_string())?;
    // Which layer this resolved from, and its one-line description — "which of
    // the three graphs named this am I looking at" is a question the reference
    // alone cannot answer.
    let entry = runtime
        .list_graphs()
        .into_iter()
        .find(|e| e.name == reference);
    let origin = entry
        .as_ref()
        .map_or_else(|| reference.to_owned(), |e| e.origin.clone());
    let description = entry.as_ref().and_then(|e| e.description.clone());
    out!(
        "{}",
        graph_view::render(
            &graph,
            &origin,
            description.as_deref(),
            &runtime.worker_bindings(),
            ui
        )
    );
    Ok(ExitCode::SUCCESS)
}

fn isolation_from(worktree: Option<&str>, init: Option<&str>) -> Isolation {
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
    json: bool,
    no_preview: bool,
    ui: ui::Ui,
) -> Result<ExitCode, String> {
    let runtime = open_runtime_streaming(json, no_preview)?;
    // `hex run` with no graph lists what you can run instead of erroring.
    let Some(reference) = reference else {
        print_graph_list(&runtime, json, ui);
        return Ok(ExitCode::SUCCESS);
    };
    let report = runtime
        .start(reference, prompt.as_deref(), name, &isolation)
        .map_err(|e| e.to_string())?;
    print_outcome(&runtime, &report, json, "run", ui)
}

fn cmd_resume(run_id: &str, json: bool, no_preview: bool, ui: ui::Ui) -> Result<ExitCode, String> {
    let runtime = open_runtime_streaming(json, no_preview)?;
    let report = runtime.resume(run_id).map_err(|e| e.to_string())?;
    print_outcome(&runtime, &report, json, "resumed", ui)
}

/// Render the end of a `run`/`resume`. A paused run has no disposition — saying
/// `succeeded` (or nothing) would misreport a run that is merely suspended.
fn print_outcome(
    runtime: &Runtime,
    report: &hex_runtime::RunReport,
    json: bool,
    verb: &str,
    ui: ui::Ui,
) -> Result<ExitCode, String> {
    let disposition = disposition_label(report.disposition, "paused");
    let payoff = Payoff::of(runtime, &report.run_id);
    if json {
        let v = serde_json::json!({
            "run_id": report.run_id,
            "origin": report.origin,
            "disposition": disposition,
            "paused": report.disposition.is_none(),
            "why": payoff.why,
            "result": payoff.result,
            "failed_steps": payoff.failed_steps.iter().map(|s| &s.label).collect::<Vec<_>>(),
            "usage": payoff.usage.as_ref().map(totals_json),
        });
        outln!("{v}");
    } else {
        outln!("{verb} {} ({})", report.run_id, report.origin);
        outln!("disposition: {disposition}");
        payoff.print(ui);
        if report.disposition.is_none() {
            // Distinguish the two ways a run pauses: an operator `pause` stopped
            // it cleanly at a boundary, an interrupt killed a live agent. Both
            // resume the same way, but only one of them left a corpse.
            // "any live agent": an interrupt during a human wait kills nothing.
            let how = if hex_runtime::interrupt::requested() {
                "interrupted; any live agent was killed and progress saved"
            } else {
                "paused"
            };
            eprintln!("{how}; continue with `hex resume {}`", report.run_id);
        }
    }
    Ok(exit_for_report(report))
}

/// How many lines of a failed check's output the end-of-run summary shows. Enough
/// to carry a test failure and its assertion; `hex logs --node <id> --full` has
/// the rest.
const FAILED_STEP_TAIL_LINES: usize = 40;

/// What a finished run actually produced — the part an operator came for.
///
/// This exists because `run` used to end at `disposition: failed` and stop. Every
/// artefact of a four-minute cross-model review was on disk and named by no
/// output: the reviewer's findings, the reason the kernel stopped, and which check
/// went red. A run that reports only its verdict makes you go digging to learn
/// anything, which is the same as not having run it.
struct Payoff {
    /// The last result captured in the run — an agent's final message, or a
    /// human's answer.
    result: Option<String>,
    /// The kernel's reason for stopping, when it recorded one.
    why: Option<String>,
    /// Steps whose recorded exit status says they failed.
    failed_steps: Vec<hex_runtime::StepLog>,
    /// What the run spent, when anything reported it.
    usage: Option<hex_runtime::Totals>,
}

impl Payoff {
    /// Project it from the journal. Every field is best-effort: a run that ends
    /// badly enough to be unreadable must still print its disposition, so a
    /// failure here degrades to silence rather than replacing the outcome with an
    /// error about fetching the outcome.
    fn of(runtime: &Runtime, run_id: &str) -> Self {
        let logs = runtime.logs(run_id).unwrap_or_default();
        // The *last* attempt to capture anything: for a review loop that is the
        // review, and for a partial it is the salvaged tail.
        let result = logs.iter().rev().find_map(|a| a.result.clone());
        let failed_steps = logs
            .last()
            .map(|a| a.steps.iter().filter(|s| s.failed()).cloned().collect())
            .unwrap_or_default();
        let status = runtime.status(run_id).ok();
        let why = status.as_ref().and_then(|s| s.why.clone());
        let usage = status.map(|s| s.usage.total).filter(|t| t.tokens() > 0);
        Self {
            result,
            why,
            failed_steps,
            usage,
        }
    }

    fn print(&self, ui: ui::Ui) {
        if let Some(why) = &self.why {
            outln!("why: {why}");
        }
        if let Some(usage) = &self.usage {
            // `cost_cell` marks a partly priced total `≥`, as `hex status` does.
            let cost = if usage.cost_micro_usd > 0 {
                format!(", {}", cost_cell(usage))
            } else {
                String::new()
            };
            outln!("spent: {} tokens{cost}", tokens(usage.tokens()));
        }
        for step in &self.failed_steps {
            let code = step.exit.as_deref().unwrap_or("?");
            outln!("\n── {} failed (exit {code}) ──", step.label);
            // A check's diagnosis is at the end of its output, not the start.
            let combined = format!("{}{}", step.stdout, step.stderr);
            let lines: Vec<&str> = combined.lines().collect();
            print_last(&lines, FAILED_STEP_TAIL_LINES, ui);
        }
        if let Some(result) = &self.result {
            outln!("\n── final message ──");
            outln!("{}", ui.paint(ui::style::DIM, result.trim_end()));
        }
    }
}

fn cmd_runs(json: bool, ui: ui::Ui) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let runs = runtime.list_runs().map_err(|e| e.to_string())?;
    if json {
        let items: Vec<_> = runs
            .iter()
            .map(|r| {
                serde_json::json!({
                    "run_id": r.run_id,
                    "state": r.state.as_str(),
                    "hung": r.hung,
                    "disposition": disposition_json(r.state.disposition()),
                    "current": r.current,
                    "attempts": r.attempts,
                    "created_at_ms": r.created_at_ms,
                    "updated_at_ms": r.updated_at_ms,
                    "error": match &r.state {
                        hex_runtime::Liveness::Error(why) => Some(why),
                        _ => None,
                    },
                })
            })
            .collect();
        outln!("{}", serde_json::json!({ "runs": items }));
        return Ok(ExitCode::SUCCESS);
    }
    if runs.is_empty() {
        outln!("no runs yet (start one with `hex run <graph> -p \"…\"`)");
        return Ok(ExitCode::SUCCESS);
    }
    // One table, one width policy, one place that knows how a state looks.
    // `finished:failed` in STATE beside `finished` in PROCESS said "finished"
    // twice and nothing else. The mark carries the colour, the word carries the
    // verdict, and the process column appears only while it still means
    // something: an *unreadable* run has no disposition but is not a process
    // anyone is waiting on, and letting it force the column back means the column
    // never disappears.
    let live = runs
        .iter()
        .any(|r| !matches!(r.state, hex_runtime::Liveness::Finished(_)));
    let mut headers: Vec<&str> = vec!["", "RUN", "RESULT", "AGE", "LAST"];
    if live {
        headers.push("PROCESS");
    }
    let right = [false, false, false, true, false, false];
    let mut table = ui::Table::new(&headers, &right[..headers.len()]).flex(1);
    for r in &runs {
        let mut row = vec![
            ui.mark(mark_for(r)),
            ui.paint(ui::style::ID, &r.run_id).to_string(),
            result_word(r),
            ui.paint(ui::style::DIM, &age(r.updated_at_ms)).to_string(),
            r.current.clone().unwrap_or_else(|| "-".to_owned()),
        ];
        if live {
            // `live` alone cannot say whether the process is *working*, so the
            // stale-heartbeat case is spelled out here rather than becoming a
            // state of its own: the operator's next move is identical.
            let process = if r.hung {
                format!("{} (not beating)", r.state.as_str())
            } else {
                r.state.as_str().to_owned()
            };
            row.push(ui.paint(ui::style::DIM, &process).to_string());
        }
        table.row(row);
    }
    for line in table.render(ui) {
        outln!("{line}");
    }
    // Buffered to one line: four 130-character yaml errors interleaved with the
    // table on a terminal and vanished entirely when it was redirected.
    let broken = runs
        .iter()
        .filter(|r| matches!(r.state, hex_runtime::Liveness::Error(_)))
        .count();
    if broken > 0 {
        eprintln!(
            "hex: {broken} run(s) could not be replayed (a graph from an older schema); \
             `hex status <run>` prints why"
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Milliseconds since `at_ms`, floored at zero — clocks and journals disagree by
/// a few ms, and a negative "elapsed" is worse than a zero one.
fn elapsed_ms(at_ms: u64) -> u64 {
    hex_runtime::journal::now_ms().saturating_sub(at_ms)
}

/// The first line of a detail string — the rest is remediation prose that
/// belongs under the table, not inside a column.
fn first_line(s: &str) -> &str {
    s.split(" — ")
        .next()
        .unwrap_or(s)
        .lines()
        .next()
        .unwrap_or(s)
}

/// The outcome as one coloured word, for a record header.
fn result_line(s: &hex_runtime::StatusReport) -> String {
    s.disposition
        .map_or_else(|| s.status.to_string(), |d| d.to_string().replace('_', " "))
}

/// Tokens and money on one line: what a run cost, without a table.
fn spend_line(t: &hex_runtime::Totals, ui: ui::Ui) -> String {
    let mut parts = vec![
        format!(
            "{} in",
            tokens(t.input_tokens + t.cache_read_tokens + t.cache_write_tokens)
        ),
        format!("{} out", tokens(t.output_tokens)),
    ];
    if t.cost_micro_usd > 0 || t.cost_is_partial() {
        parts.push(cost_cell(t));
    }
    let line = parts.join(" · ");
    if t.cost_is_partial() {
        format!(
            "{line}   {}",
            ui.paint(
                ui::style::DIM,
                &format!("({} attempt(s) reported no price)", t.unpriced_reports)
            )
        )
    } else {
        line
    }
}

/// The glyph for a run's state — the column you scan before reading anything.
fn mark_for(r: &hex_runtime::RunSummary) -> ui::Mark {
    use hex_runtime::Liveness;
    match &r.state {
        Liveness::Error(_) => ui::Mark::Warn,
        Liveness::Finished(d) => match d {
            hex_runtime::Disposition::Succeeded => ui::Mark::Ok,
            hex_runtime::Disposition::Failed => ui::Mark::Fail,
            hex_runtime::Disposition::TimedOut | hex_runtime::Disposition::BudgetExhausted => {
                ui::Mark::Warn
            }
            hex_runtime::Disposition::Cancelled => ui::Mark::Idle,
        },
        Liveness::Live => ui::Mark::Running,
        Liveness::Interrupted => ui::Mark::Idle,
    }
}

/// The outcome in words, without the `finished:` ceremony.
///
/// Left uncoloured on purpose: the mark in the first column already carries the
/// colour, and colour must never be the only thing saying what happened.
fn result_word(r: &hex_runtime::RunSummary) -> String {
    use hex_runtime::Liveness;
    match &r.state {
        Liveness::Error(_) => "unreadable".to_owned(),
        Liveness::Finished(d) => d.to_string().replace('_', " "),
        Liveness::Live => "running".to_owned(),
        // Paused, Ctrl-C'd and crashed are one state on purpose: the next move
        // is `hex resume` in all three cases.
        Liveness::Interrupted => "interrupted".to_owned(),
    }
}

/// A compact "how long ago" for a listing (`3m`, `2h`, `4d`).
fn age(at_ms: u64) -> String {
    if at_ms == 0 {
        return "-".to_owned();
    }
    let secs = elapsed_ms(at_ms) / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// How long `wait` tolerates an `interrupted` run before concluding nobody is
/// coming back for it. A grace period, because a run started in another shell a
/// moment ago may not have taken its lock yet.
const WAIT_INTERRUPTED_GRACE_MS: u64 = 5_000;

/// `hex stats` — what this machine has asked hex to do, folded from
/// `~/.hex/stats.jsonl`. Read-only: no counters, no database, the log is the
/// only source and the fold is the only number.
fn cmd_stats(json: bool, ui: ui::Ui) -> Result<ExitCode, String> {
    let path = hex_runtime::local_log::path(hex_runtime::stats::FILE)?;
    let agg = hex_runtime::stats::fold(&path)?;
    if json {
        let mut v = serde_json::to_value(&agg).map_err(|e| e.to_string())?;
        v["path"] = path.display().to_string().into();
        outln!("{v}");
        return Ok(ExitCode::SUCCESS);
    }
    if agg.lines == 0 {
        outln!("no stats recorded yet ({} is empty)", path.display());
        return Ok(ExitCode::SUCCESS);
    }
    outln!("{} ({} lines)", path.display(), agg.lines);
    // Four of the five sections are a name and one count.
    for (title, headers, map) in [
        ("COMMANDS", ["VERB", "COUNT"], &agg.verbs),
        ("NODES", ["NODE", "ATTEMPTS"], &agg.nodes),
        ("REPOS", ["REPO", "LINES"], &agg.repos),
        ("OUTCOMES", ["DISPOSITION", "RUNS"], &agg.dispositions),
    ] {
        stats_table(
            ui,
            title,
            &headers,
            &[false, true],
            map.iter().map(|(k, n)| vec![k.clone(), n.to_string()]),
        );
    }
    stats_table(
        ui,
        "GRAPHS",
        &["GRAPH", "RUNS", "CUSTOM"],
        &[false, true, true],
        agg.graphs
            .iter()
            .map(|(g, u)| vec![g.clone(), u.runs.to_string(), u.custom.to_string()]),
    );
    outln!(
        "\nisolation: {} worktree · {} shared",
        agg.worktree_runs,
        agg.shared_runs
    );
    Ok(ExitCode::SUCCESS)
}

/// One titled table of the stats report. An empty section prints nothing:
/// a header over no rows reads like a bug.
fn stats_table<I>(ui: ui::Ui, title: &str, headers: &[&str], right: &[bool], rows: I)
where
    I: IntoIterator<Item = Vec<String>>,
{
    let mut table = ui::Table::new(headers, right);
    let mut any = false;
    for row in rows {
        any = true;
        table.row(row);
    }
    if !any {
        return;
    }
    outln!("\n{title}");
    for line in table.render(ui) {
        outln!("{line}");
    }
}

/// `hex prune` — remove old finished/interrupted runs and release their slots.
fn cmd_prune(older_than: Option<&str>, all: bool, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let dur = match older_than {
        Some(raw) => Some(
            humantime::parse_duration(raw)
                .map_err(|e| format!("cannot parse `{raw}` as a duration: {e}"))?,
        ),
        None if all => None,
        // Default: keep a week of history. Always printed, so the policy is
        // never a surprise.
        None => Some(std::time::Duration::from_secs(7 * 24 * 3600)),
    };
    let report = runtime.prune(dur, all).map_err(|e| e.to_string())?;
    if json {
        outln!(
            "{}",
            serde_json::to_value(&report).map_err(|e| e.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(d) = dur {
        outln!(
            "pruning runs untouched for over {} (pass --all to ignore age)",
            humantime::format_duration(d)
        );
    }
    for id in &report.removed {
        outln!("removed {id}");
    }
    outln!(
        "{} removed, {} kept, {} reclaimed",
        report.removed.len(),
        report.kept.len(),
        human_bytes(report.bytes)
    );
    Ok(ExitCode::SUCCESS)
}

/// Bytes in the unit a human would read ("how much did that free").
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // Whole bytes take no decimal ("512 B", "1.5 KiB").
    format!("{value:.*} {}", usize::from(unit > 0), UNITS[unit])
}

/// Block until a run reaches an outcome, then exit with its disposition code.
///
/// Also returns when the run *cannot* finish on its own — interrupted by a pause,
/// a Ctrl-C or a crash — rather than waiting forever for a process that is not
/// coming back. `hex resume` continues it.
fn cmd_wait(run_id: &str, json: bool) -> Result<ExitCode, String> {
    use hex_runtime::Liveness;
    let runtime = open_runtime()?;
    let mut interrupted_since: Option<std::time::Instant> = None;
    loop {
        let summary = runtime.summary(run_id).map_err(|e| e.to_string())?;
        let verdict = match &summary.state {
            Liveness::Finished(d) => Some((disposition_label(Some(*d), "failed"), exit_for(*d))),
            // Exit 6 (paused) covers a crash too: both continue with
            // `hex resume`, so a second code would distinguish nothing.
            Liveness::Interrupted => {
                let since = interrupted_since.get_or_insert_with(std::time::Instant::now);
                let waited = u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX);
                (waited >= WAIT_INTERRUPTED_GRACE_MS)
                    .then(|| ("interrupted".to_owned(), ExitCode::from(PAUSED)))
            }
            Liveness::Error(why) => Some((
                format!("unreadable: {}", first_line(why)),
                ExitCode::from(1),
            )),
            Liveness::Live => {
                interrupted_since = None;
                None
            }
        };
        if let Some((state, code)) = verdict {
            if json {
                outln!(
                    "{}",
                    serde_json::json!({
                        "run_id": summary.run_id,
                        "state": state,
                        "disposition": disposition_json(summary.state.disposition()),
                    })
                );
            } else {
                outln!("{state}");
                if state == "interrupted" {
                    eprintln!("`{run_id}` is not being driven — continue it with `hex resume`");
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
        outln!(
            "{}",
            serde_json::json!({ "run_id": run_id, "queued": command.as_str() })
        );
    } else {
        // Queued, not applied: the driver picks it up at its next attempt
        // boundary, and the journal is where the effect shows up.
        outln!("queued {} for {run_id}", command.as_str());
        // Say *when* it lands. A steer is drained at the next attempt boundary, so
        // an attempt already in flight will not see it — without that sentence the
        // operator reasonably expects the running agent to change course, and reads
        // the unchanged output as the steer having been lost.
        if matches!(command, ControlCommand::Steer { .. })
            && let Ok(s) = runtime.status(run_id)
            && let Some(f) = s.in_flight
        {
            eprintln!(
                "note: {} is mid-attempt ({} on {}); the steer applies to the NEXT attempt",
                run_id, f.attempt_id, f.node_id
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_status(run_id: &str, usage: bool, json: bool, ui: ui::Ui) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let s = runtime.status(run_id).map_err(|e| e.to_string())?;
    if json {
        let by_node: serde_json::Map<_, _> = s
            .usage
            .by_node
            .iter()
            .map(|(node, t)| {
                let mut v = totals_json(t);
                // Spend per node only means something next to how many attempts
                // produced it, and `visits` is where the projection keeps that.
                v["attempts"] = s.visits.get(node).copied().unwrap_or(0).into();
                (node.clone(), v)
            })
            .collect();
        let by_model: serde_json::Map<_, _> = s
            .usage
            .by_model
            .iter()
            .map(|(model, t)| (model.clone(), totals_json(t)))
            .collect();
        let v = serde_json::json!({
            "run_id": s.run_id,
            "status": s.status.to_string(),
            "current": s.current,
            "attempts": s.attempts,
            "disposition": disposition_json(s.disposition),
            "in_flight": s.in_flight.as_ref().map(|f| serde_json::json!({
                "attempt_id": f.attempt_id,
                "node_id": f.node_id,
                "worker": f.worker,
                "started_at_ms": f.started_at_ms,
                "elapsed_ms": elapsed_ms(f.started_at_ms),
            })),
            "queued": s.queued.iter().map(ControlCommand::as_str).collect::<Vec<_>>(),
            "pending_steer": s.pending_steer,
            "waiting_for_human": s.asked,
            "question": s.question,
            "usage": {
                "by_node": by_node,
                "by_model": by_model,
                "total": totals_json(&s.usage.total),
            },
        });
        outln!("{v}");
    } else {
        // Four questions, answered before anything else: which run, is it
        // going, where did it stop, and what did it cost in aggregate.
        outln!("{}", ui.paint(ui::style::ID, &s.run_id));
        let mut line = vec![result_line(&s)];
        if let Some(c) = &s.current {
            line.push(format!("at {c}"));
        }
        line.push(format!(
            "{} attempt{}",
            s.attempts,
            if s.attempts == 1 { "" } else { "s" }
        ));
        outln!("{}", line.join(" · "));

        // What is happening right now, when something is.
        if let Some(f) = &s.in_flight {
            let via = f
                .worker
                .as_deref()
                .map_or(String::new(), |w| format!(" via {w}"));
            outln!(
                "{}{} on {}{via}, running {}",
                ui.field(ui::style::HEADER, "In flight", 11),
                f.attempt_id,
                f.node_id,
                ui.paint(ui::style::DIM, &age(f.started_at_ms))
            );
        }
        if let Some(node) = &s.asked {
            outln!(
                "{}{}",
                ui.field(ui::style::HEADER, "Waiting", 11),
                format_args!("`hex respond {} \"…\"`   ({node})", s.run_id)
            );
            if let Some(q) = &s.question {
                outln!("           {}", ui.paint(ui::style::DIM, q.trim_end()));
            }
        }
        for command in &s.queued {
            match command {
                ControlCommand::Steer { text } => outln!(
                    "{}{text}   {}",
                    ui.field(ui::style::HEADER, "Queued", 11),
                    ui.paint(ui::style::DIM, "(not yet picked up)")
                ),
                other => outln!(
                    "{}{}",
                    ui.field(ui::style::HEADER, "Queued", 11),
                    other.as_str()
                ),
            }
        }
        for text in &s.pending_steer {
            outln!(
                "{}{text}   {}",
                ui.field(ui::style::HEADER, "Steer", 11),
                ui.paint(ui::style::DIM, "(applies to the next agent attempt)")
            );
        }

        // One aggregate line. The per-node/per-model breakdown is accounting,
        // not status, and printing it always made a two-attempt run look like a
        // billing report — it lives behind `--usage`.
        if s.usage.total.tokens() > 0 {
            outln!(
                "{}{}",
                ui.field(ui::style::HEADER, "Usage", 11),
                spend_line(&s.usage.total, ui)
            );
        }
        if usage {
            print_usage(&s.usage, &s.visits, ui);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Render what the run spent: per node, then per model, then the total.
///
/// Silent when nothing reported usage — a `command`-only graph, or an agent whose
/// CLI reports no accounting, would otherwise grow a table of zeroes that reads
/// like a run that cost nothing rather than one that never said.
fn print_usage(
    usage: &hex_runtime::Usage,
    visits: &std::collections::BTreeMap<String, u32>,
    ui: ui::Ui,
) {
    if usage.total.tokens() == 0 {
        return;
    }
    // Every category the agents report, because they are not interchangeable: a
    // cached read costs a fraction of a fresh one, and collapsing them hides the
    // fact that 93% of a review run is cache. REASON is a *subset* of OUT, carried
    // for information and never added into the totals.
    let row = |left: &str, visits: String, t: &hex_runtime::Totals| {
        outln!(
            "{left:<22} {visits:>6} {:>9} {:>9} {:>9} {:>9} {:>8} {:>12}",
            tokens(t.input_tokens),
            tokens(t.output_tokens),
            tokens(t.cache_read_tokens),
            tokens(t.cache_write_tokens),
            tokens(t.reasoning_tokens),
            cost_cell(t),
        );
    };
    // `VISITS`, not `ATTEMPTS`: this is the projection's per-node visit count, and
    // a crash between `AttemptReported` and the attempt's terminal leaves two
    // reports against one visit. Calling it attempts would be a number that
    // occasionally disagrees with itself.
    let head = format!(
        "{:<22} {:>6} {:>9} {:>9} {:>9} {:>9} {:>8} {:>12}",
        "NODE", "VISITS", "IN", "OUT", "CACHE R", "CACHE W", "REASON", "COST"
    );
    outln!("\n{}", ui.paint(ui::style::HEADER, &head));
    for (node, t) in &usage.by_node {
        row(node, visits.get(node).copied().unwrap_or(0).to_string(), t);
    }
    outln!("{}", ui.paint(ui::style::HEADER, "MODEL"));
    for (model, t) in &usage.by_model {
        row(model, String::new(), t);
    }
    // The attempts that produced this spend, not the run's total: a `command` node
    // spends no tokens, so counting its attempts here would explain nothing.
    let attempts: u32 = usage
        .by_node
        .keys()
        .filter_map(|n| visits.get(n))
        .sum::<u32>();
    row("total", attempts.to_string(), &usage.total);
    if usage.total.cost_is_partial() {
        outln!(
            "\ncost is a lower bound: {} attempt(s) reported tokens but no price",
            usage.total.unpriced_reports
        );
    }
}

/// A cost cell: `\u{2014}` when nothing reported one, `\u{2265} $X` when only part of the
/// work was priced, plain `$X` when all of it was.
///
/// The marker is the whole point. codex reports tokens and no money, so a run
/// mixing it with claude produced a figure covering half the work and labelled it
/// the total — the one number an operator uses to decide whether a loop was worth
/// it. hex now never shows a complete-looking cost it cannot back.
fn cost_cell(t: &hex_runtime::Totals) -> String {
    match (t.cost_micro_usd, t.cost_is_partial()) {
        (0, _) => "\u{2014}".to_owned(),
        (micro, true) => format!("\u{2265} {}", usd(micro)),
        (micro, false) => usd(micro),
    }
}

/// One [`hex_runtime::Totals`] as JSON: every token class the agents distinguish,
/// plus the summed `tokens` a one-line summary uses, plus money as integer
/// micro-USD (a float would make the recorded cost inexact).
fn totals_json(t: &hex_runtime::Totals) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": t.input_tokens,
        "output_tokens": t.output_tokens,
        "cache_read_tokens": t.cache_read_tokens,
        "cache_write_tokens": t.cache_write_tokens,
        "reasoning_tokens": t.reasoning_tokens,
        "tokens": t.tokens(),
        "cost_micro_usd": t.cost_micro_usd,
        // A machine consumer needs the same caveat the table shows: a cost
        // covering only part of the work is a lower bound, not a total.
        "cost_is_partial": t.cost_is_partial(),
        "unpriced_reports": t.unpriced_reports,
    })
}

/// How often a follower re-reads a run. Fast enough that a loop's transitions
/// feel live, slow enough that watching one costs nothing — the same polled shape
/// the control inbox uses, for the same reason: there is no daemon to push.
const FOLLOW_POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// How long a follower tolerates a run whose journal has not appeared yet.
///
/// A run directory and its first event are written by the same process a few
/// microseconds apart, so this only covers a run started in another shell an
/// instant ago. A follower's whole job is to wait, but not for long: a run that
/// has existed for seconds with no journal is a broken run, not a slow one.
const JOURNAL_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Block until `run_id` has a readable journal.
///
/// "Not started yet" and "does not exist" are told apart by the run *directory*:
/// `summary` succeeds for a directory with no events and fails for a missing one,
/// so a typo still fails fast.
fn wait_for_journal(runtime: &Runtime, run_id: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + JOURNAL_WAIT;
    loop {
        if runtime.events(run_id).is_ok() {
            return Ok(());
        }
        // A missing run must not be retried: fail with the runtime's own message.
        runtime.summary(run_id).map_err(|e| e.to_string())?;
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "run `{run_id}` still has no journal after {JOURNAL_WAIT:?}"
            ));
        }
        std::thread::sleep(FOLLOW_POLL);
    }
}

/// Stream an attempt's captured output until the run ends.
///
/// Driven by the **journal**, not by polling "what is in flight". Sampling the
/// projection every 400ms missed any attempt that started and finished between
/// two polls — a quick check that wrote its failure and exited was never
/// attached to, so the follower printed a disposition and none of the evidence
/// for it. Every `AttemptStarted` is seen exactly once, whenever it lands.
fn follow_logs(
    runtime: &Runtime,
    run_id: &str,
    node: Option<&str>,
    tail_lines: usize,
    ui: ui::Ui,
) -> Result<ExitCode, String> {
    wait_for_journal(runtime, run_id)?;
    let mut open: Option<(String, hex_runtime::StreamCursor)> = None;
    let mut seen = 0usize;
    // The attempt already running when we attach is joined at its tail; anything
    // starting later streams from its first byte.
    let mut attaching = true;
    loop {
        let events = runtime.events(run_id).map_err(|e| e.to_string())?;
        for event in &events[seen.min(events.len())..] {
            let hex_runtime::EventBody::AttemptStarted { worker, .. } = &event.body else {
                continue;
            };
            let (Some(attempt_id), Some(node_id)) = (&event.attempt_id, &event.node_id) else {
                continue;
            };
            if node.is_some_and(|want| want != node_id) {
                continue;
            }
            drain(runtime, run_id, &mut open, ui)?;
            let via = worker
                .as_deref()
                .map_or(String::new(), |w| format!(" via {w}"));
            outln!("\u{2500}\u{2500} {attempt_id} [{node_id}]{via} \u{2500}\u{2500}");
            let cursor = if attaching {
                print_last(&stream_lines(runtime, run_id, attempt_id), tail_lines, ui);
                hex_runtime::StreamCursor::at_end(runtime, run_id, attempt_id)
                    .map_err(|e| e.to_string())?
            } else {
                hex_runtime::StreamCursor::default()
            };
            open = Some((attempt_id.clone(), cursor));
        }
        seen = events.len();
        attaching = false;
        drain(runtime, run_id, &mut open, ui)?;

        let status = runtime.status(run_id).map_err(|e| e.to_string())?;
        if status.status.is_finished() {
            outln!(
                "\u{2500}\u{2500} {} \u{2500}\u{2500}",
                disposition_label(status.disposition, "finished")
            );
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(FOLLOW_POLL);
    }
}

/// Print whatever the open attempt has written since the last look. Called on
/// every poll *and* before switching attempts: an attempt's final bytes land
/// after the last poll that could still see it running, and those are the
/// interesting ones — a check's failure, an agent's last word.
fn drain(
    runtime: &Runtime,
    run_id: &str,
    open: &mut Option<(String, hex_runtime::StreamCursor)>,
    ui: ui::Ui,
) -> Result<(), String> {
    let Some((attempt_id, cursor)) = open.as_mut() else {
        return Ok(());
    };
    for chunk in runtime
        .read_streams(run_id, attempt_id, cursor)
        .map_err(|e| e.to_string())?
    {
        for line in chunk.text.lines() {
            if let Some(shown) = agent_stream::humanize(line) {
                outln!("{}", ui.paint(ui::style::DIM, &shown));
            }
        }
    }
    Ok(())
}

/// Every line an attempt has written so far, humanized, across all of its
/// streams — its own two and each command step's — so codex (all stderr) and a
/// gate (all step dirs) are not silent.
fn stream_lines(runtime: &Runtime, run_id: &str, attempt_id: &str) -> Vec<String> {
    let mut cursor = hex_runtime::StreamCursor::default();
    runtime
        .read_streams(run_id, attempt_id, &mut cursor)
        .unwrap_or_default()
        .iter()
        .flat_map(|c| {
            c.text
                .lines()
                .filter_map(agent_stream::humanize)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The last `n` of `lines`, dimmed, after a count of what was cut.
fn print_last<S: AsRef<str>>(lines: &[S], n: usize, ui: ui::Ui) {
    let start = lines.len().saturating_sub(n);
    if start > 0 {
        outln!(
            "{}",
            ui.paint(ui::style::DIM, &format!("… {start} earlier line(s)"))
        );
    }
    for line in &lines[start..] {
        outln!("{}", ui.paint(ui::style::DIM, line.as_ref()));
    }
}

/// Lines of a still-running attempt shown by default. Enough to see what the
/// agent is doing without replaying its whole transcript.
const DEFAULT_TAIL_LINES: usize = 20;

fn cmd_logs(
    run_id: &str,
    node: Option<&str>,
    full: bool,
    tail: Option<usize>,
    follow: bool,
    json: bool,
    ui: ui::Ui,
) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let tail_lines = tail.unwrap_or(DEFAULT_TAIL_LINES);
    if follow {
        return follow_logs(&runtime, run_id, node, tail_lines, ui);
    }
    let in_flight = runtime
        .status(run_id)
        .ok()
        .and_then(|s| s.in_flight.map(|f| f.attempt_id));
    let logs = runtime.logs(run_id).map_err(|e| e.to_string())?;
    let logs: Vec<_> = logs
        .into_iter()
        .filter(|l| node.is_none_or(|n| l.node_id.as_deref() == Some(n)))
        .collect();

    if json {
        let items: Vec<_> = logs
            .iter()
            .map(|l| {
                let steps: Vec<_> = l
                    .steps
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "label": s.label,
                            "stdout": s.stdout,
                            "stderr": s.stderr,
                            // Without these a machine consumer can see every
                            // step's output but not which one went red — the
                            // question a driving agent is actually asking.
                            "exit": s.exit,
                            "failed": s.failed(),
                        })
                    })
                    .collect();
                serde_json::json!({
                    "attempt_id": l.attempt_id,
                    "node_id": l.node_id,
                    "worker": l.worker,
                    "result": l.result,
                    "stdout": l.stdout,
                    "stderr": l.stderr,
                    "steps": steps,
                })
            })
            .collect();
        outln!("{}", serde_json::json!({ "attempts": items }));
        return Ok(ExitCode::SUCCESS);
    }

    // Captured output is the *requested data* of this verb, so all of it goes to
    // stdout — including the stderr half, which `eprint!` used to send back out of
    // the pipe (`hex logs --full > out.txt` captured almost nothing). One target
    // stream, so one TTY decides the colouring.
    for l in &logs {
        let node = l.node_id.as_deref().unwrap_or("?");
        let via = l
            .worker
            .as_deref()
            .map_or(String::new(), |w| format!(" via {w}"));
        outln!("── {} [{node}]{via} ──", l.attempt_id);
        if full {
            print_captured(&l.stdout, ui);
            print_captured(&l.stderr, ui);
        } else {
            // Default: just the attempt's final message, dimmed.
            match &l.result {
                Some(text) => outln!("{}", ui.paint(ui::style::DIM, text)),
                // An attempt still running has no final message *yet*, and saying
                // "(no final message captured)" over ten lines of live output reads
                // as "nothing happened". Show its tail instead, and say it is live.
                None if in_flight.as_deref() == Some(l.attempt_id.as_str()) => {
                    let lines = stream_lines(&runtime, run_id, &l.attempt_id);
                    if lines.is_empty() {
                        outln!(
                            "{}",
                            ui.paint(ui::style::DIM, "(running; nothing captured yet)")
                        );
                    } else {
                        print_last(&lines, tail_lines, ui);
                        outln!("{}", ui.paint(ui::style::DIM, "(still running)"));
                    }
                }
                None => outln!(
                    "{}",
                    ui.paint(ui::style::DIM, "(no final message captured)")
                ),
            }
        }
        // A `command` node writes every byte into its numbered step dirs and
        // nothing to the attempt dir, so a check's output was reachable from no
        // surface at all. Labels always (they say what ran, and an attempt with no
        // final message would otherwise render as one blank line); the captured
        // bytes under `--full`, which is the flag that means "all of it".
        for step in &l.steps {
            outln!("   · {}", step.label);
            if full {
                print_captured(&step.stdout, ui);
                print_captured(&step.stderr, ui);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Print one captured stream, dimmed, skipping it when it holds nothing worth a
/// blank line. A capture that does not end in a newline gets one, so the next
/// header starts at column zero.
fn print_captured(text: &str, ui: ui::Ui) {
    if text.trim().is_empty() {
        return;
    }
    out!("{}", ui.paint(ui::style::DIM, text));
    if !text.ends_with('\n') {
        outln!();
    }
}

/// Micro-USD as dollars, to four decimals — a single cheap attempt costs
/// fractions of a cent, and two decimals would print `$0.00` for real money.
fn usd(micro_usd: u64) -> String {
    format!(
        "${}.{:04}",
        micro_usd / 1_000_000,
        (micro_usd % 1_000_000) / 100
    )
}

/// A token count at a glance: exact when small, else `34.8k` / `1.2M`.
fn tokens(n: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "display only; f64 is exact well past any real token count"
    )]
    match n {
        n if n < 10_000 => n.to_string(),
        n if n < 1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => format!("{:.2}M", n as f64 / 1_000_000.0),
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
        outln!("{v}");
    } else if requested {
        outln!("cancel queued for {run_id} (a live driver will stop at its next boundary)");
    } else {
        outln!("cancelled {run_id}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolve the operator prompt from `-p` (inline) or `-f` (file). clap's
/// `conflicts_with` guarantees at most one is given.
fn resolve_prompt(prompt: Option<String>, file: Option<&str>) -> Result<Option<String>, String> {
    match file {
        Some(path) => std::fs::read_to_string(path)
            .map(Some)
            .map_err(|e| format!("cannot read prompt file `{path}`: {e}")),
        None => Ok(prompt),
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
        B::AttemptReported {
            models,
            cost_micro_usd,
            ..
        } => {
            // An explicit total prints even at zero; a per-model sum only when priced.
            let cost = hex_runtime::Usage::attempt_cost(models, *cost_micro_usd);
            let spent = if cost_micro_usd.is_some() || cost > 0 {
                format!(", {}", usd(cost))
            } else {
                String::new()
            };
            let names: Vec<&str> = models.iter().map(|m| m.model.as_str()).collect();
            format!(
                "usage {} tokens{spent}{}",
                tokens(
                    models
                        .iter()
                        .map(hex_runtime::ModelUsage::tokens)
                        .fold(0u64, u64::saturating_add)
                ),
                if names.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", names.join(", "))
                }
            )
        }
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

    /// Every way an operator supplies the one prompt channel (`-p`, `--prompt`,
    /// `-f`, or nothing), through the same parse → `resolve_prompt` path.
    #[test]
    fn a_prompt_comes_from_a_flag_a_file_or_nowhere() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.md");
        std::fs::write(&path, "prompt from file").unwrap();

        for (case, args, want) in [
            (
                "-p",
                vec!["run", "critique-loop", "-p", "fix the bug"],
                Some("fix the bug"),
            ),
            (
                "--prompt",
                vec!["run", "g", "--prompt", "do the thing"],
                Some("do the thing"),
            ),
            (
                "-f",
                vec!["run", "g", "-f", path.to_str().unwrap()],
                Some("prompt from file"),
            ),
            ("none", vec!["run", "g"], None),
        ] {
            let cli = parse(&args).unwrap_or_else(|e| panic!("{case}: {e}"));
            let Some(Command::Run { prompt, file, .. }) = cli.command else {
                panic!("{case}: expected run command");
            };
            assert_eq!(
                resolve_prompt(prompt, file.as_deref()).unwrap().as_deref(),
                want,
                "{case}"
            );
        }
    }
}
