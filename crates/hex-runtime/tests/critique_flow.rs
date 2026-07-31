//! End-to-end runtime tests on a deterministic mock worker: the critique loop
//! runs to `succeeded`, and a killed run resumes from its journal.

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::temp_root;
use hex_proto::{Actor, Disposition, EventBody};
use hex_runtime::config::Config;
use hex_runtime::journal::Journal;
use hex_runtime::{Isolation, Runtime, Status, Workers};
use hex_worker::{CommandWorker, MockWorker, ResultCapture};

mod common;

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
    command: { run: ["true"] }
    on: { passed: done, failed: implement }
  done:
    terminal: succeeded
accept:
  require: [test.passed]
"#;

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

/// A two-node handoff graph: `plan` finishes with no emit (implicit `done`) and
/// captures a result; `build` references it via `{{plan.result}}`.
const HANDOFF_GRAPH: &str = r#"
version: 1
name: handoff
entry: plan
defaults: { budget: { attempts: 4 } }
nodes:
  plan:
    agent: { worker: planner, prompt: "make a plan" }
    on: { done: build }
  build:
    agent: { worker: builder, prompt: "build using {{plan.result}}", may_propose: [ready] }
    on: { ready: done }
  done:
    terminal: succeeded
accept: { require: [] }
"#;

#[test]
fn node_result_is_captured_and_handed_to_the_downstream_prompt() {
    let root = temp_root("handoff");
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir graphs");
    std::fs::write(dir.join("handoff.yaml"), HANDOFF_GRAPH).expect("write graph");

    // planner: writes a result to $HEX_RESULT_FILE and emits nothing (implicit
    // completion). builder: has no {prompt} in argv, so its (interpolated)
    // prompt is piped to stdin — capture it to a file to inspect the handoff.
    let mut workers = Workers::new();
    workers.insert(
        "planner",
        Box::new(
            CommandWorker::new(
                "planner",
                sh("printf 'STEP-ONE-DID-X' > \"$HEX_RESULT_FILE\""),
            )
            .with_result_capture(Some(ResultCapture::File)),
        ),
    );
    workers.insert(
        "builder",
        Box::new(CommandWorker::new(
            "builder",
            sh("cat > got-prompt.txt; printf ready > \"$HEX_EMIT_FILE\""),
        )),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers);
    let report = runtime
        .start("handoff", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(
        report.disposition,
        Some(Disposition::Succeeded),
        "run reaches done"
    );

    // The builder's prompt contains the planner's captured result, wrapped as
    // untrusted data — proving capture + implicit completion + interpolation.
    let got = std::fs::read_to_string(root.join("got-prompt.txt")).expect("builder prompt");
    assert!(
        got.contains("STEP-ONE-DID-X"),
        "plan result handed to builder: {got}"
    );
    assert!(
        got.contains("untrusted"),
        "result is wrapped as untrusted: {got}"
    );

    // The result is also in the journal as a NodeResult for `plan`.
    let events = runtime.events(&report.run_id).expect("events");
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::NodeResult { text } if text == "STEP-ONE-DID-X"
    )));
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

/// A graph whose acceptance can only be met on the *second* pass: `implement`
/// routes to the success terminal either way, and `accept.on_unmet` sends the run
/// back when the required evidence is missing. Edge-acyclic on purpose — the loop
/// exists only through the reroute.
const REROUTE_GRAPH: &str = r#"
version: 1
name: reroute
entry: implement
defaults: { budget: { attempts: 4, attempt: 20s } }
nodes:
  implement:
    agent: { worker: builder, prompt: "implement it", may_propose: [ready, blocked] }
    on: { ready: done, blocked: done }
  done:
    terminal: succeeded
accept:
  require: [implement.ready]
  on_unmet: implement
"#;

/// The read path is the regression: `accept.on_unmet` worked, and then `status`,
/// `logs` and `resume` all rejected the run's journal — permanently, since a
/// journal is append-only — because the lifecycle audit did not advance its
/// current node across the reroute the way `reduce` did. Driving the run was
/// never enough to catch it; *reading it afterwards* is.
#[test]
fn a_rerouted_run_stays_readable_by_status_logs_and_resume() {
    let root = temp_root("reroute");
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir graphs");
    std::fs::write(dir.join("reroute.yaml"), REROUTE_GRAPH).expect("write graph");

    // First attempt: `blocked` (acceptance unmet → reroute). Second: `ready`.
    let mut workers = Workers::new();
    workers.insert(
        "builder",
        Box::new(CommandWorker::new(
            "builder",
            sh(
                "if [ -f been-here ]; then printf ready > \"$HEX_EMIT_FILE\"; \
                else : > been-here; printf blocked > \"$HEX_EMIT_FILE\"; fi",
            ),
        )),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers);

    let report = runtime
        .start("reroute", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    // The reroute really happened (otherwise this test proves nothing).
    let events = runtime.events(&report.run_id).expect("events");
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::AcceptanceUnmet { to, missing }
                if to == "implement" && missing == &vec!["implement.ready".to_owned()]
        )),
        "{events:#?}"
    );

    // And every verified read path works on the finished run.
    let status = runtime
        .status(&report.run_id)
        .expect("status must not reject");
    assert_eq!(status.status, Status::Finished(Disposition::Succeeded));
    assert_eq!(status.attempts, 2, "the reroute earned a second attempt");
    let logs = runtime.logs(&report.run_id).expect("logs must not reject");
    assert_eq!(logs.len(), 2);
    let resumed = runtime
        .resume(&report.run_id)
        .expect("resume must not reject");
    assert_eq!(resumed.disposition, Some(Disposition::Succeeded));
}

/// A `ProgressSink` that records the driver's calls so a test can assert the
/// lifecycle (event → attempt_started → … → attempt_finished) and the contents
/// of each `AttemptView`.
#[derive(Clone, Default)]
struct Recorder {
    log: std::sync::Arc<std::sync::Mutex<Vec<Entry>>>,
}

#[derive(Debug, PartialEq)]
enum Entry {
    /// An `AttemptStarted` event was journaled for this node.
    EvStarted(String),
    /// `attempt_started` fired with this view.
    Start {
        node: String,
        number: u32,
        budget: Option<u32>,
        worker: Option<String>,
        stdout_log: PathBuf,
        attempt_id: String,
    },
    /// `attempt_finished` fired.
    Finish,
}

impl hex_runtime::ProgressSink for Recorder {
    fn event(&self, e: &hex_runtime::Event) {
        if matches!(e.body, EventBody::AttemptStarted { .. }) {
            let node = e.node_id.clone().unwrap_or_default();
            self.log.lock().unwrap().push(Entry::EvStarted(node));
        }
    }
    fn attempt_started(&self, v: &hex_runtime::AttemptView) {
        self.log.lock().unwrap().push(Entry::Start {
            node: v.node_id.clone(),
            number: v.attempt_number,
            budget: v.attempts_budget,
            worker: v.worker.clone(),
            stdout_log: v.stdout_log.clone(),
            attempt_id: v.attempt_id.clone(),
        });
    }
    fn attempt_finished(&self) {
        self.log.lock().unwrap().push(Entry::Finish);
    }
}

#[test]
fn progress_sink_brackets_every_attempt_with_a_correct_view() {
    let root = temp_root("progress");
    write_graph(&root);

    // implement → review(approved) → test(gate) → done: three attempts.
    let mock = MockWorker::new()
        .on("implement", &["ready"])
        .on("review", &["approved"]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));

    let rec = Recorder::default();
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers)
        .with_progress(Box::new(rec.clone()));
    let report = runtime
        .start("test-critique", Some("the thing"), None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    let log = rec.log.lock().unwrap();

    // Starts and finishes are perfectly bracketed: the START/FINISH subsequence
    // alternates and is balanced (a synchronous attempt never overlaps another).
    let brackets: Vec<&Entry> = log
        .iter()
        .filter(|e| matches!(e, Entry::Start { .. } | Entry::Finish))
        .collect();
    assert_eq!(brackets.len(), 6, "3 attempts × (start, finish)");
    for (i, e) in brackets.iter().enumerate() {
        if i % 2 == 0 {
            assert!(
                matches!(e, Entry::Start { .. }),
                "position {i} must be a Start"
            );
        } else {
            assert_eq!(**e, Entry::Finish, "position {i} must be a Finish");
        }
    }

    // Each attempt's AttemptStarted event is journaled immediately before its
    // attempt_started hook — the sink sees the fact before the side effect.
    for (i, e) in log.iter().enumerate() {
        if let Entry::Start { node, .. } = e {
            assert_eq!(
                log[i - 1],
                Entry::EvStarted(node.clone()),
                "attempt_started for {node} must follow its AttemptStarted event",
            );
        }
    }

    // The three views carry the right node, 1-based ordinal, shared budget,
    // worker (None for the gate), and a log path under this attempt's dir.
    let starts: Vec<&Entry> = log
        .iter()
        .filter(|e| matches!(e, Entry::Start { .. }))
        .collect();
    let expected = [
        ("implement", 1u32, Some("mock")),
        ("review", 2, Some("mock")),
        ("test", 3, None), // command: no worker
    ];
    for (start, (exp_node, exp_num, exp_worker)) in starts.iter().zip(expected) {
        let Entry::Start {
            node,
            number,
            budget,
            worker,
            stdout_log,
            attempt_id,
        } = start
        else {
            unreachable!()
        };
        assert_eq!(node, exp_node);
        assert_eq!(*number, exp_num);
        assert_eq!(*budget, Some(8), "the graph's attempts budget");
        assert_eq!(worker.as_deref(), exp_worker);
        assert!(
            stdout_log.ends_with("stdout.log"),
            "log path: {stdout_log:?}"
        );
        assert_eq!(
            stdout_log
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some(attempt_id.as_str()),
            "the log lives under attempts/<attempt_id>/",
        );
    }
}

#[test]
fn progress_sink_pairs_start_and_finish_even_when_an_attempt_fails() {
    let root = temp_root("progress-fail");
    write_graph(&root);

    // implement emits a signal outside its may_propose → the attempt fails and
    // the run ends. The start hook must still be closed by a finish hook.
    let mock = MockWorker::new().on("implement", &["nope"]);
    let mut workers = Workers::new();
    workers.insert("mock", Box::new(mock));

    let rec = Recorder::default();
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers)
        .with_progress(Box::new(rec.clone()));
    let report = runtime
        .start("test-critique", Some("x"), None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Failed));

    let log = rec.log.lock().unwrap();
    let brackets: Vec<&Entry> = log
        .iter()
        .filter(|e| matches!(e, Entry::Start { .. } | Entry::Finish))
        .collect();
    assert_eq!(
        brackets.len(),
        2,
        "one failed attempt still brackets start+finish"
    );
    assert!(matches!(brackets[0], Entry::Start { .. }), "start first");
    assert_eq!(*brackets[1], Entry::Finish, "then finish");
}

/// A worker that panics inside `run`, to prove the finish hook still fires while
/// the run unwinds (a real coding-agent adapter is opaque and could panic).
struct PanicWorker;

impl hex_worker::Worker for PanicWorker {
    fn capabilities(&self) -> hex_worker::CapabilityManifest {
        hex_worker::CapabilityManifest::from(&[hex_proto::Capability::FreshSessions][..])
    }
    fn run(&self, _request: &hex_worker::WorkRequest) -> hex_worker::WorkOutcome {
        panic!("worker exploded");
    }
}

#[test]
fn progress_sink_finishes_even_when_the_worker_panics() {
    let root = temp_root("progress-panic");
    write_graph(&root);

    let mut workers = Workers::new();
    workers.insert("mock", Box::new(PanicWorker));

    let rec = Recorder::default();
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers)
        .with_progress(Box::new(rec.clone()));

    // The worker panics inside adapter.run; the driver's RAII finish guard must
    // still fire attempt_finished while the run unwinds. (The panic prints a
    // backtrace to the test log; we deliberately don't touch the process-global
    // panic hook, which would race with other tests running in parallel.)
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.start("test-critique", Some("x"), None, &Isolation::Shared)
    }));
    assert!(
        outcome.is_err(),
        "the worker panic propagates out of start()"
    );

    let log = rec.log.lock().unwrap();
    let brackets: Vec<&Entry> = log
        .iter()
        .filter(|e| matches!(e, Entry::Start { .. } | Entry::Finish))
        .collect();
    assert_eq!(
        brackets.len(),
        2,
        "the panicking attempt still brackets start+finish"
    );
    assert!(matches!(brackets[0], Entry::Start { .. }), "start first");
    assert_eq!(*brackets[1], Entry::Finish, "then finish");
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

    let report = runtime
        .start("test-critique", Some("the thing"), None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    // The acceptance evidence is really in the journal.
    let events = runtime.events(&report.run_id).expect("events");
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::Signal { name } if name == "passed"
    )));
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::RunFinished {
            disposition: Disposition::Succeeded
        }
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

    let report = runtime
        .start("test-critique", Some("the thing"), None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::BudgetExhausted));
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
                checks: Default::default(),
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
            checks: Default::default(),
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
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

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
                checks: Default::default(),
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
    assert_eq!(report.disposition, Some(Disposition::Failed));
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
    let report = runtime
        .resume(run_id)
        .expect("resume despite stale lock file");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));
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
                checks: Default::default(),
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
    elapsed: 10s
    attempt: 200ms
nodes:
  implement:
    agent: { worker: mock, prompt: "x", may_propose: [ready] }
    on: { ready: slow }
  slow:
    command: { run: [sleep, "30"] }
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

    let report = runtime
        .start("timeout", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::TimedOut));
    // The disposition is backed by a single durable terminal event: a
    // disposition-bearing AttemptFailed (no separate RunFinished / crash window).
    let events = runtime.events(&report.run_id).expect("events");
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::AttemptFailed {
            disposition: Disposition::TimedOut,
            ..
        }
    )));
}

// ---------------------------------------------------------------------------
// Project-defined checks: "by default, no checks".
// ---------------------------------------------------------------------------

/// A loop whose gate is a *project check* rather than a literal argv.
const CHECK_GRAPH: &str = r#"
version: 1
name: check-graph
entry: implement
defaults:
  budget:
    attempts: 6
nodes:
  implement:
    agent: { worker: mock, prompt: "implement", may_propose: [ready] }
    on: { ready: test }
  test:
    command: { check: test }
    on: { passed: done, failed: implement }
  done:
    terminal: succeeded
accept:
  require: [test.passed]
"#;

fn check_runtime(tag: &str, check: Option<Vec<String>>) -> (Runtime, PathBuf) {
    let root = temp_root(tag);
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("cg.yaml"), CHECK_GRAPH).expect("write");
    let mut config = Config::builtin();
    if let Some(argv) = check {
        config.checks.insert("test".to_owned(), argv);
    }
    let mut workers = Workers::new();
    // Enough scripted turns for the loop to spend its whole attempt budget: the
    // mock pops one signal per attempt and errors when its queue runs dry.
    workers.insert(
        "mock",
        Box::new(MockWorker::new().on("implement", &["ready"; 6])),
    );
    (Runtime::with_workers(root.clone(), config, workers), root)
}

/// A graph naming a check the project has not declared is refused before the run
/// is created, with the fix named — never passed silently. A silent pass would
/// let a run reach `succeeded` having verified nothing.
#[test]
fn an_unconfigured_check_is_refused_before_the_run_starts() {
    let (runtime, root) = check_runtime("nocheck", None);
    let err = runtime
        .start("cg", None, None, &Isolation::Shared)
        .expect_err("must refuse");
    let msg = err.to_string();
    assert!(msg.contains("does not declare"), "got: {msg}");
    assert!(msg.contains("checks.test"), "names the key to add: {msg}");
    let runs = root.join(".hex").join("runs");
    assert!(
        !runs.exists() || std::fs::read_dir(&runs).into_iter().flatten().count() == 0,
        "nothing should be created for a refused run"
    );
}

/// The same graph in a project that *does* configure the check runs it for real.
#[test]
fn a_configured_check_is_executed_and_can_fail_the_loop() {
    let (runtime, _root) = check_runtime(
        "failcheck",
        Some(vec!["sh".to_owned(), "-c".to_owned(), "exit 1".to_owned()]),
    );
    let report = runtime
        .start("cg", None, None, &Isolation::Shared)
        .expect("run");
    // The check genuinely fails, so the loop re-implements until the budget ends.
    assert_eq!(report.disposition, Some(Disposition::BudgetExhausted));

    let events = runtime.events(&report.run_id).expect("events");
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::Signal { name } if name == "failed"
        )),
        "a real check failure routes `failed`"
    );
    assert!(
        !events.iter().any(|e| matches!(
            &e.body,
            EventBody::Note { text } if text.contains("not configured")
        )),
        "a configured check must not report itself as skipped"
    );
    // And the operator is told which budget ran out.
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::Note { text } if text.contains("attempt budget spent")
        )),
        "budget exhaustion must explain itself: {events:#?}"
    );
}

/// A check whose binary does not exist is an *infrastructure* failure, never a
/// `failed` verdict — routing it as `failed` would spend agent tokens fixing
/// code on the strength of evidence that was never gathered.
#[test]
fn a_check_that_cannot_start_is_not_a_failed_verdict() {
    let (runtime, _root) = check_runtime(
        "brokencheck",
        Some(vec!["definitely-not-a-real-binary-xyz".to_owned()]),
    );
    let report = runtime
        .start("cg", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Failed));

    let events = runtime.events(&report.run_id).expect("events");
    assert!(
        !events.iter().any(|e| matches!(
            &e.body,
            EventBody::Signal { name } if name == "failed"
        )),
        "a spawn failure must not be routed as a test failure: {events:#?}"
    );
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::AttemptFailed { reason, .. } if reason.contains("spawn command failed")
    )));
}

/// A run whose graph needs an agent CLI that is not installed must be refused
/// before anything is created — not discovered as a failed first attempt.
#[test]
fn preflight_refuses_a_missing_agent_cli_before_creating_a_run() {
    let root = temp_root("preflight");
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("cg.yaml"), CHECK_GRAPH).expect("write");

    let mut workers = Workers::new();
    workers.insert(
        "mock",
        Box::new(CommandWorker::new(
            "mock",
            vec!["definitely-not-a-real-agent-xyz".to_owned()],
        )),
    );
    // Declare the check, so compilation succeeds and the run reaches the *worker*
    // preflight this test is about.
    let mut config = Config::builtin();
    config
        .checks
        .insert("test".to_owned(), vec!["true".to_owned()]);
    let runtime = Runtime::with_workers(root.clone(), config, workers);

    let err = runtime
        .start("cg", None, None, &Isolation::Shared)
        .expect_err("must refuse");
    let msg = err.to_string();
    assert!(msg.contains("not on PATH"), "got: {msg}");
    assert!(msg.contains("hex doctor"), "points at the fix: {msg}");
    // Nothing was created, so no half-started run is left behind.
    let runs = root.join(".hex").join("runs");
    assert!(
        !runs.exists() || std::fs::read_dir(&runs).into_iter().flatten().count() == 0,
        "preflight must not leave a run directory behind"
    );
}
