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
    std::fs::write(
        p.join(".hex").join("config.yaml"),
        "workers:\n  builder:\n    command: [sh, -c, 'printf ready > \"$HEX_EMIT_FILE\"']\n",
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
  test:  { gate: { run: [sh, -c, "exit 0"] }, on: { passed: done, failed: build } }
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
    root
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
    assert_eq!(err, stdout(&help), "bare-hex and --help must be identical text");
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
    assert!(hex(dir.path(), &["validate", "critique-loop"]).status.success());
    assert!(hex(dir.path(), &["graph", "critique-loop"]).status.success());
}

#[test]
fn run_drives_to_success_and_streams_progress() {
    let dir = project();
    let out = hex(dir.path(), &["run", "demo"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("succeeded"));
    // Progress is streamed to stderr as events are journaled.
    assert!(stderr(&out).contains("run_started"), "live progress on stderr");
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
    assert!(v["run_id"].as_str().unwrap().starts_with("run_"));
}

#[test]
fn run_without_a_needed_prompt_fails_clearly() {
    let dir = project();
    // promptdemo references {{prompt}}; run must refuse before executing.
    let out = hex(dir.path(), &["run", "promptdemo"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("needs a prompt"), "stderr: {}", stderr(&out));
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
    let out = hex(dir.path(), &["run", "promptdemo", "-f", pf.to_str().unwrap()]);
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
fn logs_show_per_attempt_output() {
    let dir = project();
    // A worker that prints something before emitting, so there's stdout to show.
    std::fs::write(
        dir.path().join(".hex").join("config.yaml"),
        "workers:\n  builder:\n    command: [sh, -c, 'echo HELLO-FROM-AGENT; printf ready > \"$HEX_EMIT_FILE\"']\n",
    )
    .expect("config");
    let run = hex(dir.path(), &["run", "demo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&run).trim()).unwrap();
    let run_id = v["run_id"].as_str().unwrap();

    let out = hex(dir.path(), &["logs", run_id]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("HELLO-FROM-AGENT"), "agent stdout shown: {s}");
    assert!(s.contains("[build]"), "attempt header shows the node: {s}");

    // --node filters to a single node's attempts.
    let only = hex(dir.path(), &["logs", run_id, "--node", "build"]);
    assert!(stdout(&only).contains("HELLO-FROM-AGENT"));
    assert!(!stdout(&only).contains("[test]"), "filtered to build only");
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
