//! A failed parallel node stops its siblings. Its own test binary: the stop flag
//! is process-wide, so it would kill the attempts of other tests.

use common::{sh, temp_root, write_graph};
use hex_proto::{Disposition, EventBody};
use hex_runtime::config::Config;
use hex_runtime::{Isolation, Runtime, Workers};
use hex_worker::{CommandWorker, ResultCapture};

mod common;

const GRAPH: &str = r#"
version: 1
name: par
entry: plan
nodes:
  plan:
    agent: { role: planner_w, prompt: "plan" }
    on: { done: [left, right] }
  left:
    agent: { role: left_w, prompt: "left" }
    on: { done: merge }
  right:
    agent: { role: right_w, prompt: "right" }
    on: { done: merge }
  merge:
    join: all
    agent: { role: merge_w, prompt: "merge {{left.result}} {{right.result}}" }
    on: { done: done }
  done:
    terminal: succeeded
"#;

/// A worker that runs `script` and captures its stdout as the result.
fn text_worker(name: &str, script: &str) -> Box<CommandWorker> {
    Box::new(CommandWorker::new(name, sh(script)).with_result_capture(Some(ResultCapture::Text)))
}

fn workers(right: &str) -> Workers {
    let mut w = Workers::new();
    w.insert("planner_w", text_worker("planner_w", "echo planned"));
    // `left` waits for the marker `right` writes, so the run finishes only if
    // the two nodes really run at the same time.
    w.insert(
        "left_w",
        text_worker(
            "left_w",
            ": > l.up; for i in $(seq 50); do [ -f r.up ] && break; sleep 0.1; done; [ -f r.up ] && echo LEFT-SAW-RIGHT",
        ),
    );
    w.insert("right_w", text_worker("right_w", right));
    w.insert(
        "merge_w",
        text_worker("merge_w", "cat > merge.prompt; echo merged"),
    );
    w
}

#[test]
fn a_failed_node_stops_its_sibling_and_fails_the_run() {
    let root = temp_root("par-fail");
    write_graph(&root, "par", GRAPH);
    // `right` fails at once; `left` would wait 5 s for a marker that never comes.
    let rt = Runtime::with_workers(root, Config::builtin(), workers("echo boom >&2; exit 3"));
    let started = std::time::Instant::now();
    let report = rt
        .start("par", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Failed));
    assert!(
        started.elapsed().as_secs() < 4,
        "left was stopped, not awaited"
    );

    let events = rt.events(&report.run_id).unwrap();
    let interrupted = events
        .iter()
        .position(|e| matches!(e.body, EventBody::AttemptInterrupted))
        .expect("left closed as interrupted");
    let failed = events
        .iter()
        .position(|e| matches!(e.body, EventBody::AttemptFailed { .. }))
        .expect("right failed");
    assert!(interrupted < failed, "siblings close before the terminal");
    rt.status(&report.run_id).expect("the read path accepts it");
}
