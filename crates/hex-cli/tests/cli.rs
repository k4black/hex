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
    // And it must match `hex --help` (which clap prints to stdout, exit 0).
    let help = hex(dir.path(), &["--help"]);
    assert!(help.status.success());
    assert_eq!(
        err,
        stdout(&help),
        "bare-hex and --help must be identical text"
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
    ] {
        let out = hex(dir.path(), args);
        assert_eq!(out.status.code(), Some(2), "expected exit 2 for {args:?}");
    }
}

#[test]
fn help_and_version_exit_0() {
    let dir = project();
    assert!(hex(dir.path(), &["--help"]).status.success());
    assert!(hex(dir.path(), &["--version"]).status.success());
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
fn run_with_prompt_flag_succeeds() {
    let dir = project();
    let out = hex(dir.path(), &["run", "promptdemo", "-p", "the task"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("succeeded"));
}

#[test]
fn run_with_prompt_file_succeeds() {
    let dir = project();
    let pf = dir.path().join("prompt.md");
    std::fs::write(&pf, "task from a file").expect("prompt file");
    let out = hex(
        dir.path(),
        &["run", "promptdemo", "-f", pf.to_str().unwrap()],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("succeeded"));
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
    assert!(s.contains("finished:succeeded"), "{s}");
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
    assert!(stdout(&st).contains("finished:succeeded"));

    let watch = hex(dir.path(), &["watch", run_id]);
    assert!(stdout(&watch).contains("run_finished"));
}
