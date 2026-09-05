//! End-to-end tests of the `hex` binary itself: they run the compiled
//! executable (located by `assert_cmd`) against a temp project whose "agents"
//! are plain shell commands, so no real coding-agent CLI is needed. Fast and
//! hermetic — each test gets its own `tempfile::TempDir` (auto-cleaned) and
//! `HOME` points at it so the user's real config never leaks in.

use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

/// A fresh project dir with `.hex/graphs/`, plus a config whose `builder`
/// worker just writes a `ready` signal to the emit file (no external agent).
/// The returned `TempDir` owns the directory; keep it alive for the test.
fn project() -> TempDir {
    let root = TempDir::new().expect("tempdir");
    let p = root.path();
    std::fs::create_dir_all(p.join(".hex").join("graphs")).expect("mkdir");
    // `slowbuilder` takes long enough that a detached run is provably still
    // working after its launcher has exited.
    std::fs::write(
        p.join(".hex").join("config.yaml"),
        "workers:\n  builder:\n    command: [sh, -c, 'printf ready > \"$HEX_EMIT_FILE\"']\n  \
         slowbuilder:\n    command: [sh, -c, 'sleep 1; printf ready > \"$HEX_EMIT_FILE\"']\n",
    )
    .expect("config");
    std::fs::write(
        p.join(".hex").join("graphs").join("demo.yaml"),
        r#"
# A literal {{prompt}} in documentation must not require operator input.
version: 1
name: demo
entry: build
defaults: { budget: { attempts: 6 } }
nodes:
  build: { agent: { worker: builder, prompt: "x", may_propose: [ready] }, on: { ready: test } }
  test:  { command: { run: [sh, -c, "exit 0"] }, on: { passed: done, failed: build } }
  done:  { terminal: succeeded }
accept: { require: [test.passed] }
"#,
    )
    .expect("graph");
    // A second graph that references the operator prompt (`{{prompt}}`).
    std::fs::write(
        p.join(".hex").join("graphs").join("promptdemo.yaml"),
        r#"
version: 1
name: promptdemo
entry: build
defaults: { budget: { attempts: 4 } }
nodes:
  build: { agent: { worker: builder, prompt: "do {{prompt}}", may_propose: [ready] }, on: { ready: done } }
  done:  { terminal: succeeded }
accept: { require: [] }
"#,
    )
    .expect("prompt graph");
    // A graph whose gate runs two steps, each writing to both streams — the only
    // shape that exercises the numbered per-step capture dirs.
    std::fs::write(
        p.join(".hex").join("graphs").join("stepdemo.yaml"),
        r#"
version: 1
name: stepdemo
entry: build
defaults: { budget: { attempts: 4 } }
nodes:
  build: { agent: { worker: builder, prompt: "x", may_propose: [ready] }, on: { ready: verify } }
  verify:
    command:
      run:
        - [sh, -c, "echo STEP-ONE-OUT; echo STEP-ONE-ERR >&2"]
        - [sh, -c, "echo STEP-TWO-OUT"]
    on: { passed: done, failed: build }
  done:  { terminal: succeeded }
accept: { require: [verify.passed] }
"#,
    )
    .expect("step graph");
    // A graph slow enough to still be running when `--detach` returns.
    std::fs::write(
        p.join(".hex").join("graphs").join("slowdemo.yaml"),
        r#"
version: 1
name: slowdemo
entry: build
defaults: { budget: { attempts: 4, attempt: 30s } }
nodes:
  build: { agent: { worker: slowbuilder, prompt: "x", may_propose: [ready] }, on: { ready: test } }
  test:  { command: { run: [sh, -c, "exit 0"] }, on: { passed: done, failed: build } }
  done:  { terminal: succeeded }
accept: { require: [test.passed] }
"#,
    )
    .expect("slow graph");
    root
}

/// Poll `hex status --json` until the run finishes, returning its disposition.
/// Detached runs are the only asynchronous surface in the CLI, so every test that
/// touches one needs a bounded wait rather than a sleep.
fn await_disposition(dir: &Path, run_id: &str) -> String {
    for _ in 0..200 {
        let out = hex(dir, &["status", run_id, "--json"]);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout(&out).trim())
            && let Some(d) = v["disposition"].as_str()
        {
            return d.to_owned();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("run {run_id} never finished");
}

fn run_id_of(out: &Output) -> String {
    let v: serde_json::Value =
        serde_json::from_str(stdout(out).trim()).expect("json run report on stdout");
    v["run_id"]
        .as_str()
        .expect("run_id in the report")
        .to_owned()
}

/// Run the built `hex` binary in `dir` with an isolated HOME.
fn hex(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("hex")
        .expect("locate hex binary")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .output()
        .expect("spawn hex")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn no_args_prints_help_to_stderr_and_exits_2() {
    let dir = project();
    let out = hex(dir.path(), &[]);
    assert_eq!(out.status.code(), Some(2));
    // Bare `hex` renders the same clap help as `--help`, but to stderr / exit 2.
    let err = stderr(&out);
    assert!(err.contains("Usage: hex"), "clap usage on stderr: {err}");
    assert!(err.contains("Commands:"), "command list shown: {err}");
    // Bare `hex` prints the *short* help, so it must match `-h`, not `--help`:
    // `--help` is clap's long form and expands per-variant documentation that
    // the summary deliberately omits.
    let short = hex(dir.path(), &["-h"]);
    assert!(short.status.success());
    assert_eq!(
        err,
        stdout(&short),
        "bare hex and -h must be identical text"
    );
    let long = hex(dir.path(), &["--help"]);
    assert!(long.status.success());
    assert!(
        stdout(&long).contains("Colour when the stream is a terminal"),
        "--help expands what -h summarises"
    );
}

#[test]
fn clap_rejects_bad_invocations_with_exit_2() {
    let dir = project();
    // Unknown subcommand, unknown flag, extra positional, and a prompt-source
    // conflict must all be rejected at parse time (exit 2), not folded silently.
    for args in [
        &["frobnicate"][..],
        &["list", "--jsonn"][..],
        &["list", "extra"][..],
        &["run", "demo", "-p", "x", "-f", "y"][..],
        // `dash` would busy-spin at a zero interval: reject it at parse time.
        &["dash", "--interval", "0"][..],
    ] {
        let out = hex(dir.path(), args);
        assert_eq!(out.status.code(), Some(2), "expected exit 2 for {args:?}");
    }
}

#[test]
fn dash_refuses_json_because_it_has_no_machine_mode() {
    let dir = project();
    // `--json` promises machine output, but `dash` is a TUI. It must refuse
    // (exit 2) and point a machine consumer at `hex runs --json`, not open the
    // TUI or silently ignore the flag.
    let out = hex(dir.path(), &["dash", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("no machine mode"), "stderr: {err}");
    assert!(err.contains("hex runs --json"), "stderr: {err}");
    // The exception must appear in the *short* help too, not only the long form,
    // so `-h` cannot claim `dash` emits machine output.
    let short = hex(dir.path(), &["dash", "-h"]);
    assert!(short.status.success());
    assert!(
        stdout(&short).contains("every verb but `dash`"),
        "short help states the json exception: {}",
        stdout(&short)
    );
}

#[test]
fn list_shows_builtin_and_project_graphs() {
    let dir = project();
    let out = hex(dir.path(), &["list"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("demo"), "project graph listed: {s}");
    assert!(s.contains("critique-loop"), "built-in listed: {s}");
}

#[test]
fn validate_and_graph_need_no_prompt() {
    let dir = project();
    // Regression: structural commands must not demand an operator prompt.
    assert!(
        hex(dir.path(), &["validate", "critique-loop"])
            .status
            .success()
    );
    assert!(
        hex(dir.path(), &["graph", "critique-loop"])
            .status
            .success()
    );
}

#[test]
fn run_drives_to_success_and_streams_progress() {
    let dir = project();
    let out = hex(dir.path(), &["run", "demo"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("succeeded"));
    // Progress is streamed to stderr as events are journaled.
    assert!(
        stderr(&out).contains("run_started"),
        "live progress on stderr"
    );
    assert!(stderr(&out).contains("run_finished"));
}

#[test]
fn run_json_is_machine_readable_on_stdout() {
    let dir = project();
    let out = hex(dir.path(), &["run", "demo", "--json"]);
    assert!(out.status.success());
    // stdout is exactly one JSON object (progress went to stderr).
    let line = stdout(&out);
    let v: serde_json::Value = serde_json::from_str(line.trim()).expect("stdout is json");
    assert_eq!(v["disposition"], "succeeded");
    // Run ids read `yyyy-MM-dd-<graph-name>`.
    let run_id = v["run_id"].as_str().unwrap();
    assert!(run_id.contains("-demo"), "readable run id: {run_id}");
    assert!(run_id.starts_with("20"), "date-prefixed run id: {run_id}");
}

#[test]
fn run_without_a_needed_prompt_fails_clearly() {
    let dir = project();
    // promptdemo references {{prompt}}; run must refuse before executing.
    let out = hex(dir.path(), &["run", "promptdemo"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("needs a prompt"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn run_takes_its_prompt_from_a_flag_or_a_file() {
    let dir = project();
    let pf = dir.path().join("prompt.md");
    std::fs::write(&pf, "task from a file").expect("prompt file");
    for args in [
        &["run", "promptdemo", "-p", "the task"][..],
        &["run", "promptdemo", "-f", pf.to_str().unwrap()][..],
    ] {
        let out = hex(dir.path(), args);
        assert!(out.status.success(), "{args:?} stderr: {}", stderr(&out));
        assert!(stdout(&out).contains("succeeded"), "{args:?}");
    }
}

#[test]
fn unsafe_run_id_is_rejected() {
    let dir = project();
    let out = hex(dir.path(), &["status", "../../etc/passwd"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("invalid run id"));
}

#[test]
fn logs_show_final_message_by_default_and_full_output_with_flag() {
    let dir = project();
    // A worker that prints to stdout, captures a final message, then emits.
    std::fs::write(
        dir.path().join(".hex").join("config.yaml"),
        "workers:\n  builder:\n    command: [sh, -c, 'echo HELLO-FROM-AGENT; printf FINAL-SUMMARY > \"$HEX_RESULT_FILE\"; printf ready > \"$HEX_EMIT_FILE\"']\n    result: file\n",
    )
    .expect("config");
    let run = hex(dir.path(), &["run", "demo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&run).trim()).unwrap();
    let run_id = v["run_id"].as_str().unwrap();

    // Default: the attempt's final message, not the full stdout.
    let out = hex(dir.path(), &["logs", run_id]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.contains("FINAL-SUMMARY"),
        "final message shown by default: {s}"
    );
    assert!(s.contains("[build]"), "attempt header shows the node: {s}");
    assert!(
        !s.contains("HELLO-FROM-AGENT"),
        "full stdout hidden by default: {s}"
    );

    // --full: the full captured stdout.
    let full = hex(dir.path(), &["logs", run_id, "--full"]);
    assert!(
        stdout(&full).contains("HELLO-FROM-AGENT"),
        "full stdout shown with --full"
    );

    // --node filters to a single node's attempts.
    let only = hex(dir.path(), &["logs", run_id, "--node", "build"]);
    assert!(stdout(&only).contains("FINAL-SUMMARY"));
    assert!(!stdout(&only).contains("[test]"), "filtered to build only");
}

/// `hex logs --full > out.txt` used to capture almost nothing: the captured
/// stderr went through `eprint!` and straight back out of the pipe. It is the
/// *requested data* of this verb, so it belongs on stdout.
#[test]
fn logs_full_puts_the_captured_stderr_on_stdout() {
    let dir = project();
    std::fs::write(
        dir.path().join(".hex").join("config.yaml"),
        "workers:\n  builder:\n    command: [sh, -c, 'echo HELLO-FROM-AGENT; echo AGENT-DIAGNOSTIC >&2; printf ready > \"$HEX_EMIT_FILE\"']\n",
    )
    .expect("config");
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));

    let full = hex(dir.path(), &["logs", &run_id, "--node", "build", "--full"]);
    assert!(full.status.success(), "stderr: {}", stderr(&full));
    let out = stdout(&full);
    assert!(out.contains("HELLO-FROM-AGENT"), "captured stdout: {out}");
    assert!(out.contains("AGENT-DIAGNOSTIC"), "captured stderr: {out}");
    assert!(
        !stderr(&full).contains("AGENT-DIAGNOSTIC"),
        "nothing requested may leak to stderr: {}",
        stderr(&full)
    );
}

/// A `command` node writes every byte into `attempts/<id>/<n>-<label>/`, so
/// before `AttemptLog::steps` a failing check's output was on disk and reachable
/// from no CLI surface at all.
#[test]
fn logs_reach_command_step_output() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "stepdemo", "--json"]));

    let full = hex(dir.path(), &["logs", &run_id, "--node", "verify", "--full"]);
    assert!(full.status.success(), "stderr: {}", stderr(&full));
    let out = stdout(&full);
    for marker in ["STEP-ONE-OUT", "STEP-ONE-ERR", "STEP-TWO-OUT"] {
        assert!(out.contains(marker), "{marker} shown by --full: {out}");
    }
    // Declared order, not finish order and not lexical order.
    assert!(
        out.find("STEP-ONE-OUT") < out.find("STEP-TWO-OUT"),
        "steps read in declared order: {out}"
    );

    let json = hex(dir.path(), &["logs", &run_id, "--node", "verify", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).expect("logs json");
    let steps = v["attempts"][0]["steps"].as_array().expect("steps array");
    assert_eq!(steps.len(), 2, "one entry per declared step: {v}");
    assert_eq!(steps[0]["label"], "1-sh");
    assert!(
        steps[0]["stderr"]
            .as_str()
            .unwrap()
            .contains("STEP-ONE-ERR"),
        "each stream is kept apart: {v}"
    );
}

/// `hex status` must say what the run spent. No fake shell "agent" reports usage,
/// and the journal is the authority — so the stimulus is the very event a real
/// worker writes, appended to a real run's journal.
#[test]
fn status_reports_what_the_run_spent() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));
    inject_usage(dir.path(), &run_id);

    // Default status is a summary, not a billing report.
    let brief = stdout(&hex(dir.path(), &["status", &run_id]));
    assert!(brief.contains("Usage"), "one aggregate line: {brief}");
    assert!(!brief.contains("NODE"), "no table by default: {brief}");

    let out = hex(dir.path(), &["status", &run_id, "--usage"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("NODE"), "a per-node table: {s}");
    assert!(s.contains("build"), "the node that spent it: {s}");
    assert!(s.contains("test-model"), "a per-model row: {s}");
    assert!(s.contains("total"), "a total row: {s}");
    // Categories are reported separately: a cached read is not a fresh input, and
    // collapsing them hides that most of a real run is cache.
    assert!(
        s.contains("IN") && s.contains("OUT"),
        "per-category columns: {s}"
    );
    assert!(s.contains("CACHE R"), "cache is its own column: {s}");
    assert!(
        s.contains("1000") && s.contains("200"),
        "the reported split: {s}"
    );
    // Money is exact micro-USD, shown to 4 places, and unqualified because this
    // attempt priced all of its work.
    assert!(s.contains("$0.1234"), "cost rendered: {s}");
    assert!(
        !s.contains("lower bound"),
        "a fully priced run states a real total: {s}"
    );

    let json = hex(dir.path(), &["status", &run_id, "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).expect("status json");
    assert_eq!(v["usage"]["total"]["tokens"], 1200);
    assert_eq!(v["usage"]["total"]["cost_micro_usd"], 123_456);
    assert_eq!(v["usage"]["by_node"]["build"]["attempts"], 1);
    assert_eq!(v["usage"]["total"]["cost_is_partial"], false);
    assert_eq!(v["usage"]["by_model"]["test-model"]["output_tokens"], 200);
}

/// A run nothing reported usage for must not grow a table of zeroes: that reads
/// like a run that was free rather than one that never said.
#[test]
fn status_omits_the_usage_table_when_nothing_reported_any() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));

    let out = hex(dir.path(), &["status", &run_id]);
    assert!(!stdout(&out).contains("TOKENS"), "{}", stdout(&out));

    let json = hex(dir.path(), &["status", &run_id, "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).expect("status json");
    // `--json` keeps the shape regardless, so a consumer never branches on absence.
    assert_eq!(v["usage"]["total"]["tokens"], 0);
    assert!(v["usage"]["by_node"].as_object().unwrap().is_empty(), "{v}");
}

/// Splice in the `AttemptReported` a usage-reporting worker writes, correlated to
/// the `build` attempt — the kernel accepts it only while that attempt is in
/// flight, so it goes directly after its `attempt_started`. `seq` is contiguous
/// per run, so every following event is renumbered.
/// Inject a report that spent tokens and named no price — codex's shape.
fn inject_usage_unpriced(dir: &Path, run_id: &str) {
    inject_report(
        dir,
        run_id,
        serde_json::json!({
            // The real mixed shape: claude prices its share, codex does not, and
            // the attempt reports no authoritative total.
            "models": [
                { "model": "claude", "input_tokens": 10, "output_tokens": 5, "cost_micro_usd": 90_000 },
                { "model": "codex", "input_tokens": 500, "output_tokens": 50 },
            ],
        }),
    );
}

/// Splice a fully priced `attempt_reported` into a finished run's journal.
///
/// The journal is the authority, and no shell-based fake worker reports usage, so
/// appending the exact event a real adapter writes is the honest stimulus for the
/// projection and its rendering. (The parsers themselves are tested against
/// captured real agent output in `hex-worker`.)
fn inject_usage(dir: &Path, run_id: &str) {
    inject_report(
        dir,
        run_id,
        serde_json::json!({
            "models": [{
                "model": "test-model",
                "input_tokens": 1000,
                "output_tokens": 200,
                "cost_micro_usd": 123_456,
            }],
            "cost_micro_usd": 123_456,
        }),
    );
}

/// Insert `body` as an `attempt_reported` right after the `build` attempt started,
/// renumbering `seq` (which `journal::scan` requires to be contiguous).
fn inject_report(dir: &Path, run_id: &str, body: serde_json::Value) {
    let path = dir
        .join(".hex")
        .join("runs")
        .join(run_id)
        .join("events.jsonl");
    let journal = std::fs::read_to_string(&path).expect("journal");
    let mut events: Vec<serde_json::Value> = journal
        .lines()
        .map(|l| serde_json::from_str(l).expect("journal line"))
        .collect();
    let at = events
        .iter()
        .position(|e| e["kind"] == "attempt_started" && e["node_id"] == "build")
        .expect("the build attempt is in the journal");
    let mut event = serde_json::json!({
        "schema_version": 1,
        "seq": 0,
        "at_ms": events[at]["at_ms"],
        "run_id": run_id,
        "node_id": "build",
        "attempt_id": events[at]["attempt_id"],
        "actor": { "kind": "agent", "id": "builder" },
        "kind": "attempt_reported",
    });
    for (k, v) in body.as_object().expect("object body") {
        event[k] = v.clone();
    }
    events.insert(at + 1, event);
    let mut out = String::new();
    for (seq, mut event) in events.into_iter().enumerate() {
        event["seq"] = seq.into();
        out.push_str(&event.to_string());
        out.push('\n');
    }
    std::fs::write(&path, out).expect("rewrite journal");
}

/// A run mixing an agent that prices its work with one that does not must say so.
/// Presenting half the spend as "the total" is the number an operator uses to
/// decide whether a loop was worth it.
#[test]
fn status_marks_a_partly_priced_total_as_a_lower_bound() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));
    inject_usage_unpriced(dir.path(), &run_id);

    let out = hex(dir.path(), &["status", &run_id, "--usage"]);
    let s = stdout(&out);
    assert!(s.contains("lower bound"), "the caveat is stated: {s}");
    assert!(
        s.contains('\u{2265}'),
        "the total is marked as a bound: {s}"
    );

    let json = hex(dir.path(), &["status", &run_id, "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).expect("json");
    assert_eq!(v["usage"]["total"]["cost_is_partial"], true);
    assert_eq!(v["usage"]["total"]["unpriced_reports"], 1);
}

/// `--follow` streams human text, so pairing it with `--json` would emit neither
/// one thing nor the other. Rejected at parse time rather than silently ignored.
#[test]
fn follow_and_json_are_rejected_rather_than_silently_ignored() {
    let dir = project();
    let out = hex(dir.path(), &["logs", "whatever", "--follow", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot be used with"),
        "clap explains the conflict: {}",
        stderr(&out)
    );
}

/// `hex init` is the first command run in a new repo, so it must be safe to run
/// twice — and must not silently glue its `.gitignore` entry onto a last line that
/// had no terminator, which ignores both patterns.
#[test]
fn init_sets_up_a_bare_repo_and_is_idempotent() {
    let root = TempDir::new().expect("tempdir");
    let p = root.path();
    std::fs::write(p.join(".gitignore"), "target/").expect("gitignore");

    let first = hex(p, &["init", "--json"]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    let v: serde_json::Value = serde_json::from_str(stdout(&first).trim()).expect("init json");
    let created: Vec<&str> = v["created"]
        .as_array()
        .expect("created array")
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(
        created,
        [
            ".hex/",
            ".hex/graphs/",
            ".hex/config.yaml",
            ".gitignore:.hex/runs/",
            ".gitignore:.hex/worktrees/",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(p.join(".gitignore")).expect("gitignore"),
        "target/\n.hex/runs/\n.hex/worktrees/\n"
    );

    // What "green" means is the operator's call, so the checks map ships empty
    // with its examples commented out — never autodetected.
    let config = std::fs::read_to_string(p.join(".hex").join("config.yaml")).expect("config");
    assert!(config.contains("checks: {}"), "{config}");
    assert!(config.contains("#     test: [cargo, test"), "{config}");

    let second = hex(p, &["init", "--json"]);
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    let v: serde_json::Value = serde_json::from_str(stdout(&second).trim()).expect("init json");
    assert!(
        v["created"].as_array().unwrap().is_empty(),
        "the second run creates nothing: {v}"
    );
    assert_eq!(v["existed"].as_array().unwrap().len(), 5, "{v}");
    assert_eq!(
        std::fs::read_to_string(p.join(".gitignore")).expect("gitignore"),
        "target/\n.hex/runs/\n.hex/worktrees/\n",
        "no duplicated entries"
    );
    assert_eq!(
        std::fs::read_to_string(p.join(".hex").join("config.yaml")).expect("config"),
        config
    );
}

/// The one file `init` must never touch: an operator's checks and role overrides
/// are exactly what a re-run would lose.
#[test]
fn init_never_overwrites_an_existing_config() {
    let dir = project();
    let path = dir.path().join(".hex").join("config.yaml");
    let before = std::fs::read_to_string(&path).expect("config");

    let out = hex(dir.path(), &["init"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("exists   .hex/config.yaml"),
        "{}",
        stdout(&out)
    );
    assert_eq!(std::fs::read_to_string(&path).expect("config"), before);
}

/// `--follow` must *end* when the run does. A follower that hangs on a finished
/// run is worse than no follower, because it looks like the run is still going.
#[test]
fn follow_returns_immediately_on_an_already_finished_run() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));

    for verb in [
        vec!["logs", &run_id, "--follow"],
        vec!["watch", &run_id, "--follow"],
    ] {
        let out = hex(dir.path(), &verb);
        assert!(out.status.success(), "{verb:?}: {}", stderr(&out));
        assert!(
            stdout(&out).contains("succeeded"),
            "{verb:?} closes by saying how the run ended: {}",
            stdout(&out)
        );
    }
}

/// The live fields exist on a finished run too, as empty/absent rather than
/// missing keys — a machine consumer polling a run should not have to special-case
/// the shape by lifecycle stage.
#[test]
fn status_json_carries_the_live_fields() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));

    let out = hex(dir.path(), &["status", &run_id, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert!(v["in_flight"].is_null(), "nothing runs on a finished run");
    assert_eq!(v["queued"].as_array().expect("queued").len(), 0);
    assert_eq!(v["pending_steer"].as_array().expect("steer").len(), 0);
    assert!(v["waiting_for_human"].is_null());
}

/// A `.gitignore` hex cannot *read* must never be replaced by one it writes.
/// Mapping every read failure to "empty" truncated a real file to two lines —
/// found by hex reviewing this change.
#[test]
fn init_preserves_a_gitignore_it_cannot_read_as_utf8() {
    let dir = project();
    let path = dir.path().join(".gitignore");
    // A latin-1 comment: valid in a .gitignore, not valid UTF-8.
    let original: Vec<u8> = b"# caf\xe9 build output\ntarget/\n".to_vec();
    std::fs::write(&path, &original).expect("write");

    let out = hex(dir.path(), &["init"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let after = std::fs::read(&path).expect("gitignore");
    assert!(
        after.starts_with(&original),
        "the original bytes survive verbatim: {after:?}"
    );
    let tail = String::from_utf8_lossy(&after);
    assert!(tail.contains(".hex/runs/"), "{tail}");
    assert!(tail.contains(".hex/worktrees/"), "{tail}");
}

/// The agent's `hex emit` channel is a plain PATH lookup in the agent's own
/// shell, so `hex` missing from PATH is a real defect — reported like any missing
/// worker, not as a note.
#[test]
fn doctor_probes_hex_itself() {
    let dir = project();
    let bin = assert_cmd::cargo::cargo_bin("hex");

    let without = Command::cargo_bin("hex")
        .expect("locate hex binary")
        .args(["doctor", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("PATH", "/nonexistent-for-this-test")
        .output()
        .expect("spawn hex");
    let v: serde_json::Value = serde_json::from_str(stdout(&without).trim()).expect("doctor json");
    let row = &v["findings"][0];
    assert_eq!(
        (row["kind"].as_str(), row["name"].as_str()),
        (Some("self"), Some("hex"))
    );
    assert_eq!(row["ok"], false, "{v}");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("put the hex binary on PATH"),
        "the message says what to do: {v}"
    );
    assert_eq!(without.status.code(), Some(1), "a real finding, so exit 1");

    let with = Command::cargo_bin("hex")
        .expect("locate hex binary")
        .args(["doctor", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("PATH", bin.parent().expect("bin dir"))
        .output()
        .expect("spawn hex");
    let v: serde_json::Value = serde_json::from_str(stdout(&with).trim()).expect("doctor json");
    assert_eq!(v["findings"][0]["ok"], true, "{v}");
}

/// The point of `--detach`: the launcher prints an id and exits, and the run
/// keeps going in a process of its own. Both halves are asserted — the run is
/// still unfinished when the launcher returns, and it finishes anyway.
#[test]
fn a_detached_run_outlives_its_launcher() {
    let dir = project();
    let out = hex(dir.path(), &["run", "slowdemo", "--detach", "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let run_id = run_id_of(&out);

    // The launcher is gone (we hold its Output) while the run is still working.
    let listed = hex(dir.path(), &["runs", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&listed).trim()).expect("runs json");
    let row = v["runs"]
        .as_array()
        .expect("runs array")
        .iter()
        .find(|r| r["run_id"] == run_id.as_str())
        .expect("the detached run is listed");
    assert!(
        row["disposition"].is_null(),
        "the run must still be unfinished when the launcher exits: {row}"
    );

    // And it completes without anyone driving it from this process.
    assert_eq!(await_disposition(dir.path(), &run_id), "succeeded");
    // Its stdio went to files under the run dir, so the child had somewhere to
    // stream progress once it was no longer attached to a terminal.
    let run_dir = dir.path().join(".hex").join("runs").join(&run_id);
    let child_err = std::fs::read_to_string(run_dir.join("detached.err")).expect("detached.err");
    assert!(
        child_err.contains("run_started"),
        "the detached child streamed its own progress: {child_err}"
    );
}

/// `hex wait` blocks on a detached run and exits with its disposition code.
#[test]
fn wait_blocks_until_a_detached_run_finishes_and_exits_with_its_code() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "slowdemo", "--detach", "--json"]));
    let out = hex(dir.path(), &["wait", &run_id]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("succeeded"), "{}", stdout(&out));
}

/// `hex steer` on a live detached run reaches that run's journal — the whole
/// control path through a real process boundary, which is what the inbox is for.
#[test]
fn steer_reaches_a_live_detached_run() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "slowdemo", "--detach", "--json"]));
    let steer = hex(dir.path(), &["steer", &run_id, "USE-THE-V2-API"]);
    assert!(steer.status.success(), "stderr: {}", stderr(&steer));
    assert!(
        stdout(&steer).contains("queued steer"),
        "{}",
        stdout(&steer)
    );

    assert_eq!(await_disposition(dir.path(), &run_id), "succeeded");
    let watch = hex(dir.path(), &["watch", &run_id]);
    assert!(
        stdout(&watch).contains("steer: USE-THE-V2-API"),
        "the guidance is on the record: {}",
        stdout(&watch)
    );
}

/// A finished run takes no more commands: queueing one nobody will read would
/// look like it worked.
#[test]
fn control_verbs_refuse_a_finished_run() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));
    for args in [
        vec!["pause", run_id.as_str()],
        vec!["steer", run_id.as_str(), "late guidance"],
        vec!["respond", run_id.as_str(), "yes"],
    ] {
        let out = hex(dir.path(), &args);
        assert_eq!(out.status.code(), Some(2), "expected refusal for {args:?}");
        assert!(
            stderr(&out).contains("already finished"),
            "stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn runs_lists_a_finished_run_with_its_state() {
    let dir = project();
    let run_id = run_id_of(&hex(dir.path(), &["run", "demo", "--json"]));
    let out = hex(dir.path(), &["runs"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains(&run_id), "the run id is findable: {s}");
    // The `finished:` prefix is gone: the mark carries the colour and RESULT
    // carries the word, so saying "finished" twice added nothing.
    assert!(s.contains("succeeded"), "{s}");
    assert!(!s.contains("finished:succeeded"), "no ceremony prefix: {s}");
}

#[test]
fn runs_on_a_project_with_no_runs_says_so() {
    let dir = project();
    let out = hex(dir.path(), &["runs"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("no runs yet"), "{}", stdout(&out));
}

#[test]
fn status_and_watch_reflect_a_finished_run() {
    let dir = project();
    let run = hex(dir.path(), &["run", "demo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&run).trim()).unwrap();
    let run_id = v["run_id"].as_str().unwrap();

    let st = hex(dir.path(), &["status", run_id]);
    assert!(stdout(&st).contains("succeeded"));

    let watch = hex(dir.path(), &["watch", run_id]);
    assert!(stdout(&watch).contains("run_finished"));
}

/// `hex feedback` inside a worktree run: the injected context is captured, and
/// crucially `location` is the real project root (a durable place to debug)
/// while `workdir` is the ephemeral slot and `branch` is where the code lives.
#[test]
fn feedback_records_a_json_line_with_run_context() {
    let dir = TempDir::new().expect("tempdir");
    let project = dir.path().join("myproj");
    let slot = project.join(".hex").join("worktrees").join("0");
    std::fs::create_dir_all(&slot).expect("mkdir slot");
    Command::cargo_bin("hex")
        .expect("bin")
        .args([
            "feedback",
            "reviewer re-reads the whole repo",
            "--kind",
            "missing-capability",
        ])
        // Run *from* the worktree slot, as an isolated attempt would.
        .current_dir(&slot)
        .env("HOME", dir.path())
        .env("HEX_RUN_ID", "2026-08-13-x")
        .env("HEX_NODE_ID", "implement")
        .env("HEX_GRAPH", "checklist")
        .env("HEX_AGENT", "claude")
        .env("HEX_PROJECT_ROOT", &project)
        .env("HEX_WORKTREE_BRANCH", "hex/2026-08-13-x")
        .assert()
        .success();

    let log = std::fs::read_to_string(dir.path().join(".hex").join("feedback.jsonl"))
        .expect("feedback.jsonl written");
    let v: serde_json::Value = serde_json::from_str(log.trim()).expect("one json line");
    assert_eq!(v["message"], "reviewer re-reads the whole repo");
    assert_eq!(v["kind"], "missing-capability");
    assert_eq!(v["run_id"], "2026-08-13-x");
    assert_eq!(v["node"], "implement");
    assert_eq!(v["graph"], "checklist");
    assert_eq!(v["agent"], "claude");
    assert_eq!(v["branch"], "hex/2026-08-13-x");
    assert_eq!(v["project"], "myproj", "named from the real project root");
    // `location` is the real project, NOT the reclaimable slot — the whole point.
    let location = v["location"].as_str().unwrap();
    assert!(
        location.ends_with("myproj"),
        "location is the project: {location}"
    );
    assert!(
        !location.contains("worktrees"),
        "location must not be the slot: {location}"
    );
    let workdir = v["workdir"].as_str().unwrap();
    assert!(
        workdir.contains("worktrees"),
        "workdir is where it ran: {workdir}"
    );
    assert!(v["ts_ms"].as_u64().unwrap() > 0, "timestamp recorded");
    assert!(v["hex_version"].as_str().is_some(), "version recorded");
}

/// `hex feedback` outside a run works with no injected context (run fields and
/// `branch` are `null`), and a second call appends rather than overwrites.
#[test]
fn feedback_works_outside_a_run_and_appends() {
    let dir = TempDir::new().expect("tempdir");
    for msg in ["first note", "second note"] {
        Command::cargo_bin("hex")
            .expect("bin")
            .args(["feedback", msg])
            .current_dir(dir.path())
            .env("HOME", dir.path())
            .env_remove("HEX_RUN_ID")
            .env_remove("HEX_GRAPH")
            .env_remove("HEX_WORKTREE_BRANCH")
            .assert()
            .success();
    }
    let log = std::fs::read_to_string(dir.path().join(".hex").join("feedback.jsonl")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "appends, not overwrites");
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["message"], "first note");
    assert!(first["run_id"].is_null(), "no run context outside a run");
    assert!(first["graph"].is_null());
    assert!(
        first["branch"].is_null(),
        "no branch outside a worktree run"
    );
    assert!(first["location"].as_str().is_some(), "cwd always recorded");
}

/// An empty message is a usage error, not a blank log line.
#[test]
fn feedback_rejects_an_empty_message() {
    let dir = TempDir::new().expect("tempdir");
    Command::cargo_bin("hex")
        .expect("bin")
        .args(["feedback", "   "])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .assert()
        .failure();
    assert!(
        !dir.path().join(".hex").join("feedback.jsonl").exists(),
        "no file created for a rejected empty message"
    );
}
