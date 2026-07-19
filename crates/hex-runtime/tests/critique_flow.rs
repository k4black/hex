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
    agent: { worker: mock, prompt: "implement {{task}}", may_propose: [ready] }
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

fn inputs() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("task".to_owned(), "the thing".to_owned());
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

    let report = runtime.start("test-critique", &inputs()).expect("run");
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

    let report = runtime.start("test-critique", &inputs()).expect("run");
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
                inputs: inputs(),
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

#[test]
fn interrupted_run_resumes_from_journal() {
    let root = temp_root("resume");
    // Hand-build a run whose journal ends mid-attempt (a crash after
    // `attempt_started`, before any terminal event for `implement`).
    let run_id = "run_crash";
    let run_dir = root.join(".hex").join("runs").join(run_id);
    std::fs::create_dir_all(run_dir.join("attempts")).expect("mkdir run");
    std::fs::write(run_dir.join("graph.yaml"), GRAPH).expect("graph.yaml");
    // Snapshot integrity: persist the sha256 and record the same hash below.
    let hash = hex_runtime::driver::graph_hash(GRAPH);
    std::fs::write(run_dir.join("graph.sha256"), &hash).expect("graph.sha256");

    {
        let mut j = Journal::create(run_dir.join("events.jsonl")).expect("journal");
        j.append(
            run_id,
            None,
            None,
            Actor::runtime(),
            EventBody::RunCreated {
                graph_hash: hash.clone(),
                inputs: inputs(),
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

    // Resume with a mock that lets it finish cleanly.
    let mock = MockWorker::new()
        .on("implement", &["ready"])
        .on("review", &["approved"]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));
    let runtime = Runtime::with_workers(root, Config::builtin(), workers);

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
