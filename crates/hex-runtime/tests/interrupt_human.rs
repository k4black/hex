//! Ctrl-C while blocked on a `human` node: pause now, not after the answer.
//!
//! Its own test binary, like `interrupt.rs` and for the same reason: the
//! interrupt flag is process-global, so a test that sets it must not share a
//! process with tests that run subprocesses. One flag-setting test per binary.

use std::path::Path;
use std::time::Instant;

use common::temp_root;
use hex_proto::EventBody;
use hex_runtime::config::Config;
use hex_runtime::{Isolation, Runtime, Status, Workers};
use hex_worker::CommandWorker;

mod common;

/// plan finishes fast, then the run blocks on the human node — where the
/// interrupt lands. The 60s attempt bound means a regression fails the elapsed
/// assertion instead of hanging the suite.
const ASK_GRAPH: &str = r#"
version: 1
name: ask
entry: plan
defaults: { budget: { attempts: 4, attempt: 60s } }
nodes:
  plan:
    agent: { worker: fast, prompt: "plan" }
    on: { done: approve }
  approve:
    human: { prompt: "approve the plan?" }
    on: { done: done }
  done:
    terminal: succeeded
accept: { require: [] }
"#;

fn write_graph(root: &Path, name: &str, source: &str) {
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir graphs");
    std::fs::write(dir.join(format!("{name}.yaml")), source).expect("write graph");
}

/// The human wait is the one blocking path that never reaches `wait_bounded`,
/// so it needs (and now has) its own interrupt check. Before it, the request
/// sat unnoticed until the human answered — and the stale flag then paused the
/// run, or killed the next attempt, for no operator-visible reason.
#[test]
fn an_interrupt_during_a_human_wait_pauses_the_run_promptly() {
    let root = temp_root("interrupt-human");
    write_graph(&root, "ask", ASK_GRAPH);
    let mut ws = Workers::new();
    ws.insert(
        "fast",
        Box::new(CommandWorker::new(
            "fast",
            vec!["sh".to_owned(), "-c".to_owned(), "echo planned".to_owned()],
        )),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), ws);

    // Land the interrupt while the run is blocked at `approve` (the plan
    // attempt takes well under a second).
    let ticker = std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(700));
        hex_worker::interrupt::request();
    });
    let started = Instant::now();
    let report = runtime.start("ask", None, None, &Isolation::Shared);
    ticker.join().expect("ticker");
    hex_worker::interrupt::reset();
    let report = report.expect("run");

    // Unfixed, the wait ignores the flag and blocks the full 60s attempt bound.
    assert!(
        started.elapsed().as_secs() < 30,
        "the human wait ignored the interrupt: took {:?}",
        started.elapsed()
    );
    assert_eq!(report.disposition, None, "paused runs have no disposition");

    let status = runtime
        .status(&report.run_id)
        .expect("status stays readable");
    assert_eq!(status.status, Status::Paused);

    let events = runtime.events(&report.run_id).expect("events");
    let bodies: Vec<&EventBody> = events.iter().map(|e| &e.body).collect();
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, EventBody::HumanRequested { .. })),
        "the run never reached the human node: {bodies:?}"
    );
    assert!(
        bodies.iter().any(|b| matches!(b, EventBody::RunPaused)),
        "no run_paused: {bodies:?}"
    );
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, EventBody::RunFinished { .. })),
        "a paused run has no terminal: {bodies:?}"
    );
}
