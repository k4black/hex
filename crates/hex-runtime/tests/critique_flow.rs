//! End-to-end runtime tests on a deterministic mock worker: the critique loop
//! runs to `succeeded`, and a killed run resumes from its journal.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hex_proto::{Actor, Disposition, EventBody};
use hex_runtime::config::Config;
use hex_runtime::journal::Journal;
use hex_runtime::{Runtime, Status, Workers};
use hex_worker::MockWorker;

/// A critique loop whose agents are the `mock` worker and whose gate is `true`.
const GRAPH: &str = r#"
version: 1
name: test-critique
entry: implement
defaults:
  budget:
    attempts: 8
nodes:
  implement:
    agent: { worker: mock, prompt: "implement {{prompt}}", may_propose: [ready] }
    on: { ready: review }
  review:
    agent: { worker: mock, prompt: "review", may_propose: [approved, changes_requested] }
    on: { approved: test, changes_requested: implement }
  test:
    gate: { run: [true] }
    on: { passed: done, failed: implement }
  done:
    terminal: succeeded
accept:
  require: [test.passed]
"#;

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hex-e2e-{tag}-{}-{}",
        std::process::id(),
        hex_runtime::journal::now_ms()
    ));
    std::fs::create_dir_all(&root).expect("mkdir root");
    root
}

fn write_graph(root: &std::path::Path) -> PathBuf {
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir graphs");
    let path = dir.join("test-critique.yaml");
    std::fs::write(&path, GRAPH).expect("write graph");
    path
}

fn recorded_prompt() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("prompt".to_owned(), "the thing".to_owned());
    m
}

#[test]
fn critique_loop_runs_to_success() {
    let root = temp_root("success");
    write_graph(&root);

    // review requests changes once, then approves; implement always emits ready.
    let mock = MockWorker::new()
        .on("implement", &["ready", "ready"])
        .on("review", &["changes_requested", "approved"]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers);

    let report = runtime.start("test-critique", Some("the thing")).expect("run");
    assert_eq!(report.disposition, Disposition::Succeeded);

    // The acceptance evidence is really in the journal.
    let events = runtime.events(&report.run_id).expect("events");
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::Signal { name } if name == "passed"
    )));
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::RunFinished { disposition: Disposition::Succeeded }
    )));

    let status = runtime.status(&report.run_id).expect("status");
    assert_eq!(status.status, Status::Finished(Disposition::Succeeded));
}

#[test]
fn budget_exhaustion_fails_closed() {
    let root = temp_root("budget");
    write_graph(&root);

    // review never approves → the loop churns until the attempts budget stops it.
    let mock = MockWorker::new()
        .on("implement", &["ready"; 20])
        .on("review", &["changes_requested"; 20]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));
    let runtime = Runtime::with_workers(root, Config::builtin(), workers);

    let report = runtime.start("test-critique", Some("the thing")).expect("run");
    assert_eq!(report.disposition, Disposition::BudgetExhausted);
}

#[test]
fn rejects_unsafe_run_ids() {
    let root = temp_root("badid");
    let runtime = Runtime::with_workers(root, Config::builtin(), Workers::new());
    for bad in ["../escape", "run_/../x", "nope", "run_"] {
        assert!(runtime.status(bad).is_err(), "status accepted `{bad}`");
        assert!(runtime.resume(bad).is_err(), "resume accepted `{bad}`");
        assert!(runtime.events(bad).is_err(), "events accepted `{bad}`");
    }
}

#[test]
fn tampered_snapshot_is_rejected_on_resume() {
    let root = temp_root("tamper");
    let run_id = "run_tamper";
    let run_dir = root.join(".hex").join("runs").join(run_id);
    std::fs::create_dir_all(run_dir.join("attempts")).expect("mkdir");
    std::fs::write(run_dir.join("graph.yaml"), GRAPH).expect("graph.yaml");
    // A sha256 that does not match the graph text must block resume.
    std::fs::write(run_dir.join("graph.sha256"), "not_the_real_hash").expect("sha");
    {
        let mut j = Journal::create(run_dir.join("events.jsonl")).expect("journal");
        j.append(
            run_id,
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: "not_the_real_hash".to_owned(),
                inputs: recorded_prompt(),
                defaults: Default::default(),
            },
        )
        .unwrap();
    }
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(MockWorker::new()));
    let runtime = Runtime::with_workers(root, Config::builtin(), workers);
    let err = runtime.resume(run_id).unwrap_err();
    assert!(err.to_string().contains("sha256") || err.to_string().contains("snapshot"));
}

/// Hand-build a run whose journal ends mid-attempt (a crash after
/// `attempt_started`, before any terminal event for `implement`).
fn write_crashed_run(root: &std::path::Path, run_id: &str) {
    write_crashed_run_with_inputs(root, run_id, recorded_prompt());
}

fn write_crashed_run_with_inputs(
    root: &std::path::Path,
    run_id: &str,
    inputs: BTreeMap<String, String>,
) {
    let run_dir = root.join(".hex").join("runs").join(run_id);
    std::fs::create_dir_all(run_dir.join("attempts")).expect("mkdir run");
    std::fs::write(run_dir.join("graph.yaml"), GRAPH).expect("graph.yaml");
    let hash = hex_runtime::driver::graph_hash(GRAPH);
    std::fs::write(run_dir.join("graph.sha256"), &hash).expect("graph.sha256");
    let mut j = Journal::create(run_dir.join("events.jsonl")).expect("journal");
    j.append(
        run_id,
        None,
        None,
        Actor::runtime(),
        EventBody::RunCreated {
            graph_hash: hash,
            inputs,
                defaults: Default::default(),
        },
    )
    .unwrap();
    j.append(run_id, None, None, Actor::runtime(), EventBody::RunStarted)
        .unwrap();
    j.append(
        run_id,
        Some("implement"),
        Some("att_1"),
        Actor::runtime(),
        EventBody::AttemptStarted {
            idempotency_key: "implement#1".to_owned(),
            worker: Some("mock".to_owned()),
        },
    )
    .unwrap();
    // journal dropped here — the run "crashed" mid-attempt.
}

#[test]
fn resume_rejects_a_missing_recorded_prompt() {
    let root = temp_root("missing-prompt");
    let run_id = "run_missing_prompt";
    write_crashed_run_with_inputs(&root, run_id, BTreeMap::new());

    let err = finishing_runtime(root).resume(run_id).unwrap_err();
    assert!(err.to_string().contains("missing the prompt"), "{err}");
}

fn finishing_runtime(root: PathBuf) -> Runtime {
    let mock = MockWorker::new()
        .on("implement", &["ready"])
        .on("review", &["approved"]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));
    Runtime::with_workers(root, Config::builtin(), workers)
}

#[test]
fn interrupted_run_resumes_from_journal() {
    let root = temp_root("resume");
    let run_id = "run_crash";
    write_crashed_run(&root, run_id);

    let runtime = finishing_runtime(root);
    let report = runtime.resume(run_id).expect("resume");
    assert_eq!(report.disposition, Disposition::Succeeded);

    let events = runtime.events(run_id).expect("events");
    // The orphaned attempt was explicitly marked, not silently rerun.
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.body, EventBody::AttemptInterrupted)),
        "resume must record the interruption"
    );
    // And a fresh attempt (att_2) carried the run forward.
    assert!(
        events
            .iter()
            .any(|e| e.attempt_id.as_deref() == Some("att_2")),
        "resume must start a fresh attempt"
    );
}

#[test]
fn crash_right_after_attempt_failed_does_not_rerun() {
    // A journal ending exactly at a terminal AttemptFailed must resume as
    // finished — the failure and its disposition are one atomic record, so
    // there is no window in which the failed node looks re-runnable.
    let root = temp_root("failcrash");
    let run_id = "run_failcrash";
    let run_dir = root.join(".hex").join("runs").join(run_id);
    std::fs::create_dir_all(run_dir.join("attempts")).expect("mkdir");
    std::fs::write(run_dir.join("graph.yaml"), GRAPH).expect("graph.yaml");
    let hash = hex_runtime::driver::graph_hash(GRAPH);
    std::fs::write(run_dir.join("graph.sha256"), &hash).expect("sha");
    {
        let mut j = Journal::create(run_dir.join("events.jsonl")).expect("journal");
        j.append(
            run_id,
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: hash,
                inputs: recorded_prompt(),
                defaults: Default::default(),
            },
        )
        .unwrap();
        j.append(run_id, None, None, Actor::runtime(), EventBody::RunStarted)
            .unwrap();
        j.append(
            run_id,
            Some("implement"),
            Some("att_1"),
            Actor::runtime(),
            EventBody::AttemptStarted {
                idempotency_key: "implement#1".to_owned(),
                worker: Some("mock".to_owned()),
            },
        )
        .unwrap();
        j.append(
            run_id,
            Some("implement"),
            Some("att_1"),
            Actor::runtime(),
            EventBody::AttemptFailed {
                reason: "boom".to_owned(),
                disposition: Disposition::Failed,
            },
        )
        .unwrap();
    }

    // A runtime whose mock *would* emit if the node were re-run.
    let root_for_read = root.clone();
    let report = finishing_runtime(root).resume(run_id).expect("resume");
    assert_eq!(report.disposition, Disposition::Failed);
    let all = Runtime::with_workers(root_for_read, Config::builtin(), Workers::new())
        .events(run_id)
        .expect("events");
    assert!(
        !all.iter().any(|e| e.attempt_id.as_deref() == Some("att_2")),
        "a failed attempt must not be rerun on resume"
    );
}

#[test]
fn stale_lock_file_does_not_block_resume() {
    // A crashed process leaves its `run.lock` file behind, but the OS advisory
    // lock it held is released — so resume must still succeed.
    let root = temp_root("stalelock");
    let run_id = "run_stale";
    write_crashed_run(&root, run_id);
    let run_dir = root.join(".hex").join("runs").join(run_id);
    std::fs::write(run_dir.join("run.lock"), "999999\n").expect("leftover lock");

    let runtime = finishing_runtime(root);
    let report = runtime.resume(run_id).expect("resume despite stale lock file");
    assert_eq!(report.disposition, Disposition::Succeeded);
}

#[test]
fn lifecycle_invalid_journal_is_rejected() {
    // A `signal` with no attempt in flight is an impossible lifecycle and must
    // be rejected before folding, not silently ignored.
    let root = temp_root("badlife");
    let run_id = "run_badlife";
    let run_dir = root.join(".hex").join("runs").join(run_id);
    std::fs::create_dir_all(run_dir.join("attempts")).expect("mkdir");
    std::fs::write(run_dir.join("graph.yaml"), GRAPH).expect("graph.yaml");
    let hash = hex_runtime::driver::graph_hash(GRAPH);
    std::fs::write(run_dir.join("graph.sha256"), &hash).expect("sha");
    {
        let mut j = Journal::create(run_dir.join("events.jsonl")).expect("journal");
        j.append(
            run_id,
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: hash,
                inputs: recorded_prompt(),
                defaults: Default::default(),
            },
        )
        .unwrap();
        j.append(run_id, None, None, Actor::runtime(), EventBody::RunStarted)
            .unwrap();
        // No attempt_started — this signal is impossible.
        j.append(
            run_id,
            Some("implement"),
            Some("att_1"),
            Actor::runtime(),
            EventBody::Signal {
                name: "ready".to_owned(),
            },
        )
        .unwrap();
    }
    let runtime = Runtime::with_workers(root, Config::builtin(), Workers::new());
    let err = runtime.status(run_id).unwrap_err();
    assert!(err.to_string().contains("journal is invalid"), "{err}");
}

#[test]
fn a_timed_out_gate_records_the_timed_out_disposition() {
    const TIMEOUT_GRAPH: &str = r#"
version: 1
name: timeout
entry: implement
defaults:
  budget:
    attempts: 4
    elapsed: 200ms
nodes:
  implement:
    agent: { worker: mock, prompt: "x", may_propose: [ready] }
    on: { ready: slow }
  slow:
    gate: { run: [sleep, "30"] }
    on: { passed: done, failed: done }
  done:
    terminal: succeeded
accept:
  require: []
"#;
    let root = temp_root("timeout");
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("timeout.yaml"), TIMEOUT_GRAPH).expect("write");

    let mock = MockWorker::new().on("implement", &["ready"]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));
    let runtime = Runtime::with_workers(root, Config::builtin(), workers);

    let report = runtime.start("timeout", None).expect("run");
    assert_eq!(report.disposition, Disposition::TimedOut);
    // The disposition is backed by a single durable terminal event: a
    // disposition-bearing AttemptFailed (no separate RunFinished / crash window).
    let events = runtime.events(&report.run_id).expect("events");
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::AttemptFailed { disposition: Disposition::TimedOut, .. }
    )));
}
