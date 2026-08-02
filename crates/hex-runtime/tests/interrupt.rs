//! Ctrl-C: kill the agent, keep the evidence, pause the run.
//!
//! The interrupt flag is **process-global**, so these tests live in their own
//! integration binary — setting it in a file that also runs unrelated tests
//! would kill their subprocesses too. For the same reason this file has exactly
//! one test that sets the flag, and clears it before returning.
//!
//! The signal *handler* is not exercised here (a test that SIGINTs its own
//! process would take the harness with it); it is a thin wrapper whose only job
//! is to set this flag. What is worth pinning is everything downstream: that a
//! set flag actually reaches the agent, and what the journal is left saying.

use std::path::Path;
use std::time::Instant;

use common::temp_root;
use hex_proto::EventBody;
use hex_runtime::config::Config;
use hex_runtime::{Isolation, Runtime, Status, Workers};
use hex_worker::CommandWorker;

mod common;

/// One agent node that would sleep far longer than the test's patience, so a
/// prompt return can only mean the interrupt killed it.
const SLOW_GRAPH: &str = r#"
version: 1
name: slow
entry: work
defaults: { budget: { attempts: 4, attempt: 120s } }
nodes:
  work:
    agent: { worker: sleeper, prompt: "work", may_propose: [ready] }
    on: { ready: done }
  done:
    terminal: succeeded
accept: { require: [] }
"#;

fn write_graph(root: &Path, name: &str, source: &str) {
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir graphs");
    std::fs::write(dir.join(format!("{name}.yaml")), source).expect("write graph");
}

/// An interrupt kills the in-flight agent and leaves the run **paused**, not
/// failed: nothing was exceeded and nothing went wrong, so `AttemptFailed` would
/// both misreport it and make it unresumable.
///
/// Before this, Ctrl-C killed `hex` alone — the agent ran on in its own process
/// group, unlogged and unnoticed, and the run was left with an open attempt that
/// `hex resume` would have run a *second* agent alongside.
#[test]
fn an_interrupt_kills_the_agent_and_pauses_the_run() {
    let root = temp_root("interrupt");
    write_graph(&root, "slow", SLOW_GRAPH);
    let mut ws = Workers::new();
    ws.insert(
        "sleeper",
        Box::new(CommandWorker::new(
            "sleeper",
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                // `exec` so the process we signal *is* the sleeper: if the group
                // kill regressed to `child.kill()`, a wrapping shell would die
                // and the sleep would survive, which is the original bug.
                "exec sleep 120".to_owned(),
            ],
        )),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), ws);

    // Request the stop *while the attempt runs*, which is the path that has to
    // kill something. (Setting it before `start` instead exercises the boundary
    // check, which pauses without ever opening an attempt.)
    let ticker = std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(500));
        hex_worker::interrupt::request();
    });
    let started = Instant::now();
    let report = runtime.start("slow", None, None, &Isolation::Shared);
    ticker.join().expect("ticker");
    hex_worker::interrupt::reset();
    let report = report.expect("run");

    // The agent would have slept for 120s; the SIGTERM grace is 2s.
    assert!(
        started.elapsed().as_secs() < 30,
        "the agent was not killed: took {:?}",
        started.elapsed()
    );
    assert_eq!(report.disposition, None, "paused runs have no disposition");

    let status = runtime
        .status(&report.run_id)
        .expect("status stays readable");
    assert_eq!(status.status, Status::Paused);

    // The attempt is *closed* — an open one is what made `resume` double-run.
    let events = runtime.events(&report.run_id).expect("events");
    let bodies: Vec<&EventBody> = events.iter().map(|e| &e.body).collect();
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, EventBody::AttemptInterrupted)),
        "no attempt_interrupted: {bodies:?}"
    );
    assert!(
        bodies.iter().any(|b| matches!(b, EventBody::RunPaused)),
        "no run_paused: {bodies:?}"
    );
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, EventBody::AttemptFailed { .. })),
        "an interrupt is not a failure: {bodies:?}"
    );
}
