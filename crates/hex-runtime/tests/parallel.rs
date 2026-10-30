//! Parallel nodes: a list edge starts several nodes at once, a `join: all` node
//! waits for all of them, and a failed node stops its siblings.

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
fn a_list_edge_runs_nodes_at_once_and_a_join_reads_both() {
    let root = temp_root("par-ok");
    write_graph(&root, "par", GRAPH);
    let rt = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(
            ": > r.up; for i in $(seq 50); do [ -f l.up ] && break; sleep 0.1; done; echo RIGHT-SAW-LEFT",
        ),
    );
    let report = rt
        .start("par", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    let merge = std::fs::read_to_string(root.join("merge.prompt")).unwrap();
    assert!(
        merge.contains("LEFT-SAW-RIGHT") && merge.contains("RIGHT-SAW-LEFT"),
        "{merge}"
    );
    let starts: Vec<String> = rt
        .events(&report.run_id)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.body, EventBody::AttemptStarted { .. }))
        .filter_map(|e| e.node_id.clone())
        .collect();
    assert_eq!(
        starts,
        ["plan", "left", "right", "merge"],
        "merge runs once"
    );
    let status = rt.status(&report.run_id).expect("the read path accepts it");
    assert_eq!(status.visits["merge"], 1);
}

/// A worker that owns sessions like an agent CLI, as program `.0`: it reports a
/// session id and writes what it was asked to resume or fork into its result.
struct SessionWorker(&'static str);

impl hex_worker::Worker for SessionWorker {
    fn program(&self) -> Option<&str> {
        Some(self.0) // on PATH, so preflight passes
    }
    fn capabilities(&self) -> &'static [hex_proto::Capability] {
        &[
            hex_proto::Capability::SessionResume,
            hex_proto::Capability::SessionFork,
        ]
    }
    fn run(&self, r: &hex_worker::WorkRequest) -> hex_worker::WorkOutcome {
        let how = match (&r.resume_session, r.fork_session) {
            (None, _) => "fresh".to_owned(),
            (Some(id), true) => format!("fork:{id}"),
            (Some(id), false) => format!("resume:{id}"),
        };
        let verdict = if r.prompt.contains("VERDICT") {
            // A review: request changes once, then approve.
            let again = r.attempt_id == "att_4";
            format!(
                "\nVERDICT: {}",
                if again {
                    "changes_requested"
                } else {
                    "approved"
                }
            )
        } else {
            String::new()
        };
        hex_worker::WorkOutcome::default()
            .with_result(Some(format!("{how}{verdict}")))
            .with_report(Some(hex_worker::AttemptReport {
                session_id: Some(format!("s-{}", r.attempt_id)),
                agent: Some(self.0.to_owned()),
                ..Default::default()
            }))
    }
}

const FORK_GRAPH: &str = r#"
version: 1
name: fork
entry: research
nodes:
  research:
    agent: { role: s, prompt: "research" }
    on: { done: [a, b] }
  a:
    agent: { role: s, context: { fork: research }, prompt: "a" }
    on: { done: review }
  b:
    agent: { role: s, context: { fork: research }, prompt: "b" }
    on: { done: review }
  review:
    join: all
    agent: { role: s, prompt: "review", may_propose: [approved, changes_requested] }
    on: { approved: done, changes_requested: [a, b] }
  done:
    terminal: succeeded
"#;

/// Round 1: both implementers fork the research session. Round 2: each one
/// continues its own fork, not the research session.
#[test]
fn a_fork_starts_from_the_source_then_continues_its_own_session() {
    let root = temp_root("par-fork");
    write_graph(&root, "fork", FORK_GRAPH);
    let mut w = Workers::new();
    w.insert("s", Box::new(SessionWorker("sh")));
    let rt = Runtime::with_workers(root, Config::builtin(), w);
    let report = rt
        .start("fork", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    let results: Vec<(String, String)> = rt
        .events(&report.run_id)
        .unwrap()
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::NodeResult { text } => Some((
                e.node_id.clone().unwrap_or_default(),
                text.lines().next().unwrap_or_default().to_owned(),
            )),
            _ => None,
        })
        .filter(|(n, _)| n == "a" || n == "b")
        .collect();
    let of = |node: &str| -> Vec<&str> {
        results
            .iter()
            .filter(|(n, _)| n == node)
            .map(|(_, t)| t.as_str())
            .collect()
    };
    assert_eq!(of("a"), ["fork:s-att_1", "resume:s-att_2"]);
    assert_eq!(of("b"), ["fork:s-att_1", "resume:s-att_3"]);
}

const CONTINUE_GRAPH: &str = r#"
version: 1
name: cont
entry: implement
nodes:
  implement:
    agent: { role: s, context: continue, prompt: "implement" }
    on: { done: done }
  done:
    terminal: succeeded
"#;

/// A new run with `--continue` starts the same-named node from the earlier
/// run's session, and records it in its own journal.
#[test]
fn a_new_run_continues_the_sessions_of_an_earlier_run() {
    let root = temp_root("par-continue");
    write_graph(&root, "cont", CONTINUE_GRAPH);
    let rt = |w: Box<dyn hex_worker::Worker>| {
        let mut workers = Workers::new();
        workers.insert("s", w);
        Runtime::with_workers(root.clone(), Config::builtin(), workers)
    };
    let first = rt(Box::new(SessionWorker("sh")))
        .start("cont", None, None, &Isolation::Shared)
        .expect("first run");
    let handle = rt(Box::new(SessionWorker("sh")))
        .status(&first.run_id)
        .expect("status")
        .sessions["implement"]
        .clone();
    assert_eq!(handle.id, "s-att_1");

    let runtime = rt(Box::new(SessionWorker("sh")));
    let second = runtime
        .start_from("cont", None, None, &Isolation::Shared, Some(&first.run_id))
        .expect("second run");
    let result = runtime
        .events(&second.run_id)
        .unwrap()
        .iter()
        .find_map(|e| match &e.body {
            EventBody::NodeResult { text } => Some(text.clone()),
            _ => None,
        });
    assert_eq!(result.as_deref(), Some("resume:s-att_1"));

    // The session now continues in the second run: the first is no source and
    // cannot be resumed, but the chain goes on from the second.
    let again = rt(Box::new(SessionWorker("sh")))
        .start_from("cont", None, None, &Isolation::Shared, Some(&first.run_id))
        .expect_err("already continued");
    assert!(again.to_string().contains("already continued"), "{again}");
    let resumed = rt(Box::new(SessionWorker("sh")))
        .resume(&first.run_id)
        .expect_err("taken over");
    assert!(resumed.to_string().contains("continued"), "{resumed}");
    let third = runtime
        .start_from("cont", None, None, &Isolation::Shared, Some(&second.run_id))
        .expect("the chain continues");
    assert_eq!(third.disposition, Some(Disposition::Succeeded));

    // The node's role now runs a different program: refused before the start.
    let err = rt(Box::new(SessionWorker("true")))
        .start_from("cont", None, None, &Isolation::Shared, Some(&first.run_id))
        .expect_err("refused");
    assert!(err.to_string().contains("E-import-agent"), "{err}");
}
