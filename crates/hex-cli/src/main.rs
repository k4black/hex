//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is one operator surface: argument parsing and rendering over
//! [`hex_runtime::Runtime`]. A human at a TTY and an agent (via injected
//! `hex emit`) share the same control protocol; every action becomes an event.
//!
//! Verbs: `init` · `validate` · `graph` · `run` · `resume` · `runs` · `status` ·
//! `watch` · `wait` · `logs` · `pause` · `steer` · `respond` · `cancel`, plus the
//! worker-side `emit`. Redoing work is a new `run`; there is no
//! `retry`/`replay`. The mid-run verbs are all thin writes to the run's control
//! inbox — the same transport a human and an agent use.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use hex_runtime::{
    Actor, Cancellation, Command as ControlCommand, Disposition, Isolation, Runtime,
};

#[macro_use]
mod out;
mod agent_stream;
mod graph_export;
mod graph_view;
mod preview;
#[cfg(test)]
mod test_support;
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
    /// Validate a graph: schema, references, bounded cycles
    Validate {
        /// Graph reference: a preset name or a path to a `.yaml` file
        #[arg(value_name = "GRAPH")]
        graph: String,
    },
    /// Render a graph: as text, JSON, mermaid, or graphviz DOT
    Graph {
        /// Graph reference: a preset name or a path to a `.yaml` file
        #[arg(value_name = "GRAPH")]
        graph: String,
        /// Output format (`mermaid` pastes into a GitHub comment; `dot` feeds graphviz)
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
        /// Also break the spend down per node and per model
        #[arg(long)]
        usage: bool,
    },
    /// Print a run's recorded event stream
    Watch {
        /// Run id, as printed by `hex run`
        run_id: String,
        /// Keep printing new events until the run finishes
        #[arg(long)]
        follow: bool,
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
    /// Propose a routing event (worker-side; called by an agent inside a run)
    Emit {
        /// Routing event to append (must be in the node's `may_propose`)
        event: String,
    },
}

/// How `hex graph` renders. Every one of these goes to stdout and exits 0.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum GraphFormat {
    /// The flow, for a terminal.
    Text,
    /// The compiled IR plus edge classification, for a machine.
    Json,
    /// A mermaid `flowchart`, for a Markdown comment.
    Mermaid,
    /// A graphviz `digraph`, for `dot -Tsvg`.
    Dot,
    /// The graph's YAML verbatim — the copy-and-customise path two shipped
    /// presets already tell you to use.
    Source,
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
            ui::Ui::stdout(cli.color, json),
        ),
        Command::Resume { run_id } => cmd_resume(&run_id, json, no_preview),
        Command::Runs => cmd_runs(json, ui::Ui::stdout(cli.color, json)),
        Command::Status { run_id, usage } => {
            cmd_status(&run_id, usage, json, ui::Ui::stdout(cli.color, json))
        }
        Command::Watch { run_id, follow } => cmd_watch(&run_id, follow, json),
        Command::Logs {
            run_id,
            node,
            full,
            tail,
            follow,
        } => cmd_logs(&run_id, node.as_deref(), full, tail, follow, json),
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

/// The starter `.hex/config.yaml`. Every key is commented out: the built-in layer
/// (`hex-runtime/src/defaults.yaml`) already supplies working workers and roles,
/// so an uncommented copy of them here would freeze this machine's defaults into
/// the repository and stop deep-merge doing its job.
const CONFIG_TEMPLATE: &str = "\
# hex project configuration.
#
# This is the last of three layers: the built-in defaults (embedded in the `hex`
# binary), then `~/.config/hex/config.yaml`, then this file. Layers deep-merge per
# key, so setting `roles.reviewer.model` here keeps the built-in worker, effort,
# read_only and prompt. Run `hex doctor` to see what the merged result resolves to.

# Project checks: name → argv. **Deliberately empty.**
#
# What \"green\" means is your decision, so hex autodetects nothing and ships no
# commands. Declare a check here and a graph can gate on it as
# `command: { check: test }`; naming an undeclared check is refused before the run
# starts, rather than passing silently. Two built-in presets (`tdd`,
# `implement-until-green`) are a gate, so they need `test` declared.
#
#   checks:
#     test: [cargo, test, --workspace]
#     lint: [cargo, clippy, --workspace, --all-targets]
checks: {}

# Roles are what a graph names (`role: reviewer`). Each binds a worker CLI to a
# model, a reasoning effort, a read-only policy, and a prompt preamble. Override
# only what should differ from the built-in layer; `prompt_append` extends the
# inherited preamble, `prompt` replaces it.
#
#   roles:
#     reviewer:
#       prompt_append: |
#         This repository's invariants are in AGENTS.md — read it before judging a
#         design choice.

# Workers are the CLI adapters behind a role — internal plumbing a graph never
# names directly. `kind` picks the adapter: codex | claude | opencode | command.
#
#   workers:
#     codex:
#       kind: codex
";

/// Lines `hex init` adds to `.gitignore`: a run's journal and a worktree slot are
/// machine-local working state, not source.
const GITIGNORE_LINES: [&str; 2] = [".hex/runs/", ".hex/worktrees/"];

/// Set the current repository up for hex.
///
/// Idempotent by construction: every step reports `created` or `exists` and an
/// existing `.hex/config.yaml` is never rewritten — the operator's checks and role
/// overrides are exactly the content a second `hex init` must not be able to lose.
fn cmd_init(json: bool) -> Result<ExitCode, String> {
    let root = hex_runtime::project_root().map_err(|e| e.to_string())?;
    let mut created: Vec<String> = Vec::new();
    let mut existed: Vec<String> = Vec::new();

    for dir in [root.join(".hex"), root.join(".hex").join("graphs")] {
        let name = format!("{}/", relative(&root, &dir));
        if dir.is_dir() {
            existed.push(name);
        } else {
            std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {name}: {e}"))?;
            created.push(name);
        }
    }

    let config = root.join(".hex").join("config.yaml");
    let config_name = relative(&root, &config);
    if config.exists() {
        existed.push(config_name.clone());
    } else {
        std::fs::write(&config, CONFIG_TEMPLATE)
            .map_err(|e| format!("cannot write {config_name}: {e}"))?;
        created.push(config_name);
    }

    let gitignore = root.join(".gitignore");
    // Bytes, not a `String`, and a read failure is fatal rather than "empty".
    // Treating an unreadable file as empty and then writing our two lines over it
    // deletes whatever it held — a `.gitignore` with one non-UTF-8 byte in a
    // comment, or one we lack permission to read, was silently truncated to two
    // lines. Appending raw bytes also preserves the original exactly.
    let mut current = match std::fs::read(&gitignore) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(format!("cannot read {}: {e}", relative(&root, &gitignore)));
        }
    };
    // Decide what is missing before mutating, so the comparison view and the
    // buffer are never borrowed at once.
    let needed: Vec<&str> = {
        let existing = String::from_utf8_lossy(&current);
        GITIGNORE_LINES
            .iter()
            .copied()
            .filter(|line| {
                let present = existing.lines().any(|l| l.trim() == *line);
                if present {
                    existed.push(format!(".gitignore:{line}"));
                }
                !present
            })
            .collect()
    };
    let appended = !needed.is_empty();
    for line in needed {
        // A file whose last line has no terminator would otherwise get our entry
        // glued onto it, silently ignoring both patterns.
        if !current.is_empty() && !current.ends_with(b"\n") {
            current.push(b'\n');
        }
        current.extend_from_slice(line.as_bytes());
        current.push(b'\n');
        created.push(format!(".gitignore:{line}"));
    }
    // Only touch the file when we have something to add: a second `hex init` must
    // leave the tree byte-for-byte, mtime included, as it found it.
    if appended {
        std::fs::write(&gitignore, &current)
            .map_err(|e| format!("cannot write .gitignore: {e}"))?;
    }

    if json {
        outln!(
            "{}",
            serde_json::json!({
                "root": root.display().to_string(),
                "created": created,
                "existed": existed,
            })
        );
    } else {
        for name in &created {
            outln!("created  {name}");
        }
        for name in &existed {
            outln!("exists   {name}");
        }
        outln!(
            "\ndeclare your checks in .hex/config.yaml, then `hex list` to see what you can run"
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
        let items: Vec<_> = graphs
            .iter()
            .map(|g| {
                serde_json::json!({
                    "name": g.name,
                    "origin": g.origin,
                    "layer": g.layer.label(),
                    "shadows": g.shadows,
                    "description": g.description,
                    "example": g.example,
                })
            })
            .collect();
        outln!("{}", serde_json::json!({ "graphs": items }));
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
    let runtime = open_runtime()?;
    // Source is the one format that must work on a graph that does not compile:
    // you reach for it precisely to fix one.
    if matches!(format, GraphFormat::Source) && !json {
        out!(
            "{}",
            runtime.graph_source(reference).map_err(|e| e.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }
    let graph = runtime.validate(reference).map_err(|e| e.to_string())?;
    let topo = hex_runtime::Topology::of(&graph);
    // The global `--json` is an alias for `--format json` rather than a conflict:
    // it is documented as working on any command, so rejecting it here would
    // break the one habit every other verb teaches.
    let format = if json { GraphFormat::Json } else { format };
    match format {
        GraphFormat::Json => outln!("{}", graph_view::to_json(&graph, &topo, reference)),
        GraphFormat::Mermaid => out!("{}", graph_export::to_mermaid(&graph, &topo)),
        GraphFormat::Dot => out!("{}", graph_export::to_dot(&graph, &topo)),
        // Handled above, before compilation.
        GraphFormat::Source => unreachable!("source returns before the graph is compiled"),
        GraphFormat::Text => {
            let glyphs = ui.glyphs();
            // Which layer this resolved from, and its one-line description —
            // "which of the three graphs named this am I looking at" is a
            // question the reference alone cannot answer.
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
                    &glyphs,
                    ui
                )
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

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

#[allow(
    clippy::too_many_arguments,
    reason = "one verb's flags; a struct would only move them"
)]
fn cmd_run(
    reference: Option<&str>,
    prompt: Option<String>,
    name: Option<&str>,
    isolation: Isolation,
    mode: RunMode,
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
    print_outcome(&runtime, &report, json, "run")
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
        outln!(
            "{}",
            serde_json::json!({ "run_id": run_id, "detached": true })
        );
    } else {
        outln!("{run_id}");
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
    print_outcome(&runtime, &report, json, "resumed")
}

/// Render the end of a `run`/`resume`. A paused run has no disposition — saying
/// `succeeded` (or nothing) would misreport a run that is merely suspended.
fn print_outcome(
    runtime: &Runtime,
    report: &hex_runtime::RunReport,
    json: bool,
    verb: &str,
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
        payoff.print();
        if report.disposition.is_none() {
            eprintln!("paused; continue with `hex resume {}`", report.run_id);
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
        let events = runtime.events(run_id).unwrap_or_default();
        // `RecordTerminal` journals its reason as a `Note` immediately before the
        // terminal, so only the *terminal cluster* counts. Scanning the whole
        // journal for the last note instead would surface a stale one: a run whose
        // check failed on round one and passed on round two would end `succeeded`
        // while printing "1 of 3 steps failed" as its reason.
        let why = events
            .iter()
            .rev()
            .take_while(|e| {
                matches!(
                    e.body,
                    hex_runtime::EventBody::Note { .. }
                        | hex_runtime::EventBody::RunFinished { .. }
                        | hex_runtime::EventBody::AttemptFailed { .. }
                )
            })
            .find_map(|e| match &e.body {
                hex_runtime::EventBody::Note { text } => Some(text.clone()),
                hex_runtime::EventBody::AttemptFailed { reason, .. } => Some(reason.clone()),
                _ => None,
            });
        let usage = runtime
            .status(run_id)
            .ok()
            .map(|s| s.usage.total)
            .filter(|t| t.tokens() > 0);
        Self {
            result,
            why,
            failed_steps,
            usage,
        }
    }

    fn print(&self) {
        let tty = std::io::stdout().is_terminal();
        if let Some(why) = &self.why {
            outln!("why: {why}");
        }
        if let Some(usage) = &self.usage {
            let cost = if usage.cost_micro_usd > 0 {
                format!(", {}", usd(usage.cost_micro_usd))
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
            let start = lines.len().saturating_sub(FAILED_STEP_TAIL_LINES);
            if start > 0 {
                outln!("{}", grey(&format!("… {start} earlier line(s)"), tty));
            }
            for line in &lines[start..] {
                outln!("{}", grey(line, tty));
            }
        }
        if let Some(result) = &self.result {
            outln!("\n── final message ──");
            outln!("{}", grey(result.trim_end(), tty));
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
    // verdict, and liveness appears only while it still means something.
    // Liveness, not status: an *unreadable* run has no status but is not a
    // process anyone is waiting on, and letting it force the column back means
    // the column never disappears.
    let live = runs
        .iter()
        .any(|r| !matches!(r.liveness, hex_runtime::Liveness::Finished));
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
            row.push(
                ui.paint(ui::style::DIM, &r.liveness.to_string())
                    .to_string(),
            );
        }
        table.row(row);
    }
    for line in table.render(ui) {
        outln!("{line}");
    }
    // Buffered to one line: four 130-character yaml errors interleaved with the
    // table on a terminal and vanished entirely when it was redirected.
    let broken: Vec<&hex_runtime::RunSummary> = runs.iter().filter(|r| r.error.is_some()).collect();
    if !broken.is_empty() {
        eprintln!(
            "hex: {} run(s) could not be replayed (a graph from an older schema); \
             `hex status <run>` prints why",
            broken.len()
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
    use hex_runtime::{Disposition as D, Status};
    match (&r.status, r.disposition) {
        (None, _) => ui::Mark::Warn,
        (_, Some(D::Succeeded)) => ui::Mark::Ok,
        (_, Some(D::Failed)) => ui::Mark::Fail,
        (_, Some(D::TimedOut | D::BudgetExhausted)) => ui::Mark::Warn,
        (_, Some(D::Cancelled)) => ui::Mark::Idle,
        (Some(Status::Running), None) => ui::Mark::Running,
        _ => ui::Mark::Idle,
    }
}

/// The outcome in words, without the `finished:` ceremony.
///
/// Left uncoloured on purpose: the mark in the first column already carries the
/// colour, and colour must never be the only thing saying what happened.
fn result_word(r: &hex_runtime::RunSummary) -> String {
    use hex_runtime::{Disposition as D, Status};
    match (&r.status, r.disposition) {
        (None, _) => "unreadable".to_owned(),
        (_, Some(D::Succeeded)) => "succeeded".to_owned(),
        (_, Some(D::Failed)) => "failed".to_owned(),
        (_, Some(D::TimedOut)) => "timed out".to_owned(),
        (_, Some(D::BudgetExhausted)) => "budget exhausted".to_owned(),
        (_, Some(D::Cancelled)) => "cancelled".to_owned(),
        (Some(Status::Paused), _) => "paused".to_owned(),
        (Some(Status::Running), _) => "running".to_owned(),
        _ => "created".to_owned(),
    }
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
                outln!(
                    "{}",
                    serde_json::json!({
                        "run_id": summary.run_id,
                        "state": state,
                        "disposition": disposition_json(summary.disposition),
                    })
                );
            } else {
                outln!("{state}");
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
        line.push(format!("{} attempts", s.attempts));
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

/// How long a follower waits for a just-reserved run's journal to appear.
///
/// `--detach` reserves the run directory a moment before the driver writes its
/// first event, so `hex logs --follow "$(hex run … --detach)"` — the obvious thing
/// to type — would otherwise fail instantly with "no journal yet". A follower's
/// whole job is to wait.
const JOURNAL_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Block until `run_id` has a readable journal. Distinguishes "not started yet"
/// from "does not exist" via `summary`, which succeeds for a reserved run and
/// fails for a missing one — so a typo still fails fast.
fn wait_for_journal(runtime: &Runtime, run_id: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + JOURNAL_WAIT;
    loop {
        let summary = runtime.summary(run_id).map_err(|e| e.to_string())?;
        if summary.status.is_some() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(summary.error.unwrap_or_else(|| {
                format!("run `{run_id}` still has no journal after {JOURNAL_WAIT:?}")
            }));
        }
        std::thread::sleep(FOLLOW_POLL);
    }
}

fn cmd_watch(run_id: &str, follow: bool, json: bool) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    if follow {
        wait_for_journal(&runtime, run_id)?;
    }
    let print_from = |events: &[hex_runtime::Event], from: usize| -> Result<(), String> {
        for event in &events[from..] {
            if json {
                outln!(
                    "{}",
                    serde_json::to_string(event).map_err(|e| e.to_string())?
                );
            } else {
                outln!("{}", event_line(event));
            }
        }
        Ok(())
    };
    let events = runtime.events(run_id).map_err(|e| e.to_string())?;
    print_from(&events, 0)?;
    if !follow {
        return Ok(ExitCode::SUCCESS);
    }
    // Poll by event count. The journal is append-only, so "how many have I already
    // printed" is the whole cursor — no offsets to keep and nothing to miss.
    let mut printed = events.len();
    loop {
        if let Ok(summary) = runtime.summary(run_id)
            && summary
                .status
                .as_ref()
                .is_some_and(hex_runtime::Status::is_finished)
        {
            // Drain whatever the terminal write added before stopping.
            if let Ok(events) = runtime.events(run_id) {
                print_from(&events, printed.min(events.len()))?;
            }
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(FOLLOW_POLL);
        if let Ok(events) = runtime.events(run_id)
            && events.len() > printed
        {
            print_from(&events, printed)?;
            printed = events.len();
        }
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
) -> Result<ExitCode, String> {
    wait_for_journal(runtime, run_id)?;
    let tty = std::io::stdout().is_terminal();
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
            drain(runtime, run_id, &mut open, tty)?;
            let via = worker
                .as_deref()
                .map_or(String::new(), |w| format!(" via {w}"));
            outln!("\u{2500}\u{2500} {attempt_id} [{node_id}]{via} \u{2500}\u{2500}");
            let cursor = if attaching {
                for line in tail_of(runtime, run_id, attempt_id, tail_lines) {
                    outln!("{}", grey(&line, tty));
                }
                hex_runtime::StreamCursor::at_end(runtime, run_id, attempt_id)
                    .map_err(|e| e.to_string())?
            } else {
                hex_runtime::StreamCursor::default()
            };
            open = Some((attempt_id.clone(), cursor));
        }
        seen = events.len();
        attaching = false;
        drain(runtime, run_id, &mut open, tty)?;

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
    tty: bool,
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
                outln!("{}", grey(&shown, tty));
            }
        }
    }
    Ok(())
}

/// The last `lines` lines an attempt has written, across all of its streams.
fn tail_of(runtime: &Runtime, run_id: &str, attempt_id: &str, lines: usize) -> Vec<String> {
    let mut cursor = hex_runtime::StreamCursor::default();
    let all: Vec<String> = runtime
        .read_streams(run_id, attempt_id, &mut cursor)
        .unwrap_or_default()
        .iter()
        .flat_map(|c| {
            c.text
                .lines()
                .filter_map(agent_stream::humanize)
                .collect::<Vec<_>>()
        })
        .collect();
    all[all.len().saturating_sub(lines)..].to_vec()
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
) -> Result<ExitCode, String> {
    let runtime = open_runtime()?;
    let tail_lines = tail.unwrap_or(DEFAULT_TAIL_LINES);
    if follow {
        return follow_logs(&runtime, run_id, node, tail_lines);
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
    let tty = std::io::stdout().is_terminal();
    for l in &logs {
        let node = l.node_id.as_deref().unwrap_or("?");
        let via = l
            .worker
            .as_deref()
            .map_or(String::new(), |w| format!(" via {w}"));
        outln!("── {} [{node}]{via} ──", l.attempt_id);
        if full {
            print_captured(&l.stdout, tty);
            print_captured(&l.stderr, tty);
        } else {
            // Default: just the attempt's final message, dimmed.
            match &l.result {
                Some(text) => outln!("{}", grey(text, tty)),
                // An attempt still running has no final message *yet*, and saying
                // "(no final message captured)" over ten lines of live output reads
                // as "nothing happened". Show its tail instead, and say it is live.
                None if in_flight.as_deref() == Some(l.attempt_id.as_str()) => {
                    print_tail(&l.stdout, &l.stderr, tail_lines, tty);
                }
                None => outln!("{}", grey("(no final message captured)", tty)),
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
                print_captured(&step.stdout, tty);
                print_captured(&step.stderr, tty);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The last `lines` lines of a live attempt's output, across both streams —
/// codex writes everything to stderr and nothing to stdout, so either alone is
/// silent for one of the two agents.
fn print_tail(stdout: &str, stderr: &str, lines: usize, tty: bool) {
    let combined: Vec<String> = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(agent_stream::humanize)
        .collect();
    if combined.is_empty() {
        outln!("{}", grey("(running; nothing captured yet)", tty));
        return;
    }
    let start = combined.len().saturating_sub(lines);
    if start > 0 {
        outln!("{}", grey(&format!("… {start} earlier line(s)"), tty));
    }
    for line in &combined[start..] {
        outln!("{}", grey(line, tty));
    }
    outln!("{}", grey("(still running)", tty));
}

/// Print one captured stream, dimmed, skipping it when it holds nothing worth a
/// blank line. A capture that does not end in a newline gets one, so the next
/// header starts at column zero.
fn print_captured(text: &str, tty: bool) {
    if text.trim().is_empty() {
        return;
    }
    out!("{}", grey(text, tty));
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
        outln!("{v}");
    } else if requested {
        outln!("cancel queued for {run_id} (a live driver will stop at its next boundary)");
    } else {
        outln!("cancelled {run_id}");
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
        B::AttemptReported {
            models,
            cost_micro_usd,
            ..
        } => {
            let spent = cost_micro_usd
                .or_else(|| {
                    // Saturating like every other fold over reported usage: a
                    // garbled value must not panic the renderer of the event that
                    // carries it.
                    let per_model: u64 = models
                        .iter()
                        .filter_map(|m| m.cost_micro_usd)
                        .fold(0u64, u64::saturating_add);
                    (per_model > 0).then_some(per_model)
                })
                .map_or(String::new(), |c| format!(", {}", usd(c)));
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
        let Some(Command::Logs {
            run_id, node, full, ..
        }) = cli.command
        else {
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
