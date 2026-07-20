//! End-to-end tests of the `hex` binary itself: they run the compiled
//! executable (`CARGO_BIN_EXE_hex`) against a temp project whose "agents" are
//! plain shell commands, so no real coding-agent CLI is needed. Fast and
//! hermetic — `HOME` is pointed at the temp dir so the user's real config
//! never leaks in.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh project dir with `.hex/graphs/`, plus a config whose `builder`
/// worker just writes a `ready` signal to the emit file (no external agent).
fn project(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("hex-cli-it-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(root.join(".hex").join("graphs")).expect("mkdir");
    std::fs::write(
        root.join(".hex").join("config.yaml"),
        "workers:\n  builder:\n    command: [sh, -c, 'printf ready > \"$HEX_EMIT_FILE\"']\n",
    )
    .expect("config");
    std::fs::write(
        root.join(".hex").join("graphs").join("demo.yaml"),
        r#"
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
    root
}

/// Run the built `hex` binary in `dir` with an isolated HOME.
fn hex(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hex"))
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
fn no_args_prints_usage_and_exits_2() {
    let dir = project("usage");
    let out = hex(&dir, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("usage: hex"));
}

#[test]
fn list_shows_builtin_and_project_graphs() {
    let dir = project("list");
    let out = hex(&dir, &["list"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("demo"), "project graph listed: {s}");
    assert!(s.contains("critique-loop"), "built-in listed: {s}");
}

#[test]
fn validate_and_graph_need_no_inputs() {
    let dir = project("validate");
    // Regression: structural commands must not demand runtime inputs.
    assert!(hex(&dir, &["validate", "critique-loop"]).status.success());
    assert!(hex(&dir, &["graph", "critique-loop"]).status.success());
}

#[test]
fn run_drives_to_success_and_streams_progress() {
    let dir = project("run");
    let out = hex(&dir, &["run", "demo"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("succeeded"));
    // Progress is streamed to stderr as events are journaled.
    assert!(stderr(&out).contains("run_started"), "live progress on stderr");
    assert!(stderr(&out).contains("run_finished"));
}

#[test]
fn run_json_is_machine_readable_on_stdout() {
    let dir = project("json");
    let out = hex(&dir, &["run", "demo", "--json"]);
    assert!(out.status.success());
    // stdout is exactly one JSON object (progress went to stderr).
    let line = stdout(&out);
    let v: serde_json::Value = serde_json::from_str(line.trim()).expect("stdout is json");
    assert_eq!(v["disposition"], "succeeded");
    assert!(v["run_id"].as_str().unwrap().starts_with("run_"));
}

#[test]
fn run_without_required_input_fails_clearly() {
    let dir = project("missing");
    // critique-loop declares `task` required; run must refuse before executing.
    let out = hex(&dir, &["run", "critique-loop"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("missing required input"));
}

#[test]
fn unsafe_run_id_is_rejected() {
    let dir = project("traversal");
    let out = hex(&dir, &["status", "../../etc/passwd"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("invalid run id"));
}

#[test]
fn status_and_watch_reflect_a_finished_run() {
    let dir = project("status");
    let run = hex(&dir, &["run", "demo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&run).trim()).unwrap();
    let run_id = v["run_id"].as_str().unwrap();

    let st = hex(&dir, &["status", run_id]);
    assert!(stdout(&st).contains("finished:succeeded"));

    let watch = hex(&dir, &["watch", run_id]);
    assert!(stdout(&watch).contains("run_finished"));
}
