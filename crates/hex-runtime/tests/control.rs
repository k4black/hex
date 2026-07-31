//! The control inbox end to end: cancelling and pausing a *live* run, steering
//! the next attempt, and answering a `human` node.
//!
//! The commands are queued the way any operator queues them — one JSON file
//! written to `control/tmp/` and renamed into `control/inbox/`. Most tests have
//! the running "agent" queue the command from inside its own attempt, which
//! makes the timing deterministic (the file is in the inbox before the attempt
//! ends, so the very next boundary consumes it) *and* exercises the case core
//! rule 8 is about: a human and an agent share one control protocol.

use std::path::{Path, PathBuf};

use common::temp_root;
use hex_proto::{Actor, Command, Disposition, EventBody};
use hex_runtime::config::Config;
use hex_runtime::{Inbox, Isolation, Runtime, Status, Workers};
use hex_worker::{CommandWorker, ResultCapture};

mod common;

/// implement(agent) → review(agent) → done. Both agents are shell scripts, and
/// `attempt: 20s` bounds every wait so a broken test fails instead of hanging.
const LOOP_GRAPH: &str = r#"
version: 1
name: control-loop
entry: implement
defaults: { budget: { attempts: 8, attempt: 20s } }
nodes:
  implement:
    agent: { worker: implementer, prompt: "implement it", may_propose: [ready] }
    on: { ready: review }
  review:
    agent: { worker: reviewer, prompt: "review it", may_propose: [approved] }
    on: { approved: done }
  done:
    terminal: succeeded
accept: { require: [] }
"#;

/// plan(agent) → approve(human) → build(agent) → done: the plan-approve-implement
/// shape, with the operator's answer handed on as `{{approve.result}}`.
const APPROVAL_GRAPH: &str = r#"
version: 1
name: approval
entry: plan
defaults: { budget: { attempts: 4, attempt: 20s } }
nodes:
  plan:
    agent: { worker: planner, prompt: "make a plan" }
    on: { done: approve }
  approve:
    human: { prompt: "approve this plan: {{plan.result}}" }
    on: { done: build }
  build:
    agent: { worker: builder, prompt: "build per the operator: {{approve.result}}", may_propose: [ready] }
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

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

/// A shell snippet that queues `json` in the *running* run's control inbox,
/// temp-then-rename, exactly as `hex pause`/`hex cancel` do. The run dir is
/// derived from `HEX_EMIT_FILE` (`<run_dir>/attempts/<id>/emitted`), the only
/// path an attempt is handed.
fn queue(json: &str) -> String {
    format!(
        "d=$(dirname $(dirname $(dirname \"$HEX_EMIT_FILE\")))/control; \
         mkdir -p \"$d/tmp\" \"$d/inbox\"; \
         printf '%s' '{json}' > \"$d/tmp/c.json\"; \
         mv \"$d/tmp/c.json\" \"$d/inbox/0000000000001-c.json\"; "
    )
}

const FROM_OPERATOR: &str = r#""actor":{"kind":"human","id":"t"}"#;

fn workers(scripts: &[(&str, String)]) -> Workers {
    let mut ws = Workers::new();
    for (name, script) in scripts {
        ws.insert(*name, Box::new(CommandWorker::new(*name, sh(script))));
    }
    ws
}

/// The one run directory under `root` (these tests start exactly one run).
fn only_run_dir(root: &Path) -> PathBuf {
    let runs = root.join(".hex").join("runs");
    std::fs::read_dir(&runs)
        .expect("runs dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("one run")
}

/// A cancel queued while the run is live must actually stop it. Before the
/// inbox, `Runtime::cancel` refused outright: the driver holds the journal's
/// single write lock, so the only honest way in is to ask the holder.
#[test]
fn a_cancel_command_ends_a_live_run() {
    let root = temp_root("cancel");
    write_graph(&root, "cl", LOOP_GRAPH);
    let runtime = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(&[
            (
                "implementer",
                format!(
                    "{}printf ready > \"$HEX_EMIT_FILE\"",
                    queue(&format!("{{{FROM_OPERATOR},\"command\":\"cancel\"}}"))
                ),
            ),
            (
                "reviewer",
                "printf approved > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
        ]),
    );

    let report = runtime
        .start("cl", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Cancelled));

    let events = runtime.events(&report.run_id).expect("events");
    // The journal names who cancelled, not just that something did.
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::Note { text } if text.contains("cancelled by human:t")
        )),
        "{events:#?}"
    );
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::RunFinished {
            disposition: Disposition::Cancelled
        }
    )));
    // The cancel landed at the *boundary*: review never started.
    assert!(
        !events
            .iter()
            .any(|e| e.node_id.as_deref() == Some("review")),
        "cancel must stop scheduling, not interrupt an attempt: {events:#?}"
    );
}

/// A pause is not an outcome: it records no terminal, and `hex resume` continues
/// the *same* run (same id, same journal, budgets not reset).
#[test]
fn pause_returns_without_a_terminal_and_resume_continues_the_same_run() {
    let root = temp_root("pause");
    write_graph(&root, "cl", LOOP_GRAPH);
    let runtime = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(&[
            (
                "implementer",
                format!(
                    "{}printf ready > \"$HEX_EMIT_FILE\"",
                    queue(&format!("{{{FROM_OPERATOR},\"command\":\"pause\"}}"))
                ),
            ),
            (
                "reviewer",
                "printf approved > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
        ]),
    );

    let report = runtime
        .start("cl", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, None, "a pause has no disposition");
    let run_id = report.run_id.clone();

    let events = runtime.events(&run_id).expect("events");
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.body, EventBody::RunPaused)),
        "{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.body, EventBody::RunFinished { .. })),
        "a paused run must not record a terminal: {events:#?}"
    );
    let status = runtime.status(&run_id).expect("status");
    assert_eq!(status.status, Status::Paused);
    assert_eq!(
        status.current.as_deref(),
        Some("review"),
        "parked, not lost"
    );

    // Resume drives the same run to completion, through the same journal.
    let resumed = runtime.resume(&run_id).expect("resume");
    assert_eq!(resumed.run_id, run_id);
    assert_eq!(resumed.disposition, Some(Disposition::Succeeded));
    let events = runtime.events(&run_id).expect("events");
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.body, EventBody::RunResumed)),
        "the suspension is lifted on the record: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.attempt_id.as_deref() == Some("att_2")),
        "the second attempt continues the same run's numbering"
    );
}

/// Steering must reach the *next* attempt's prompt, fenced as operator input.
#[test]
fn steer_text_reaches_the_next_attempts_prompt() {
    let root = temp_root("steer");
    write_graph(&root, "cl", LOOP_GRAPH);
    let runtime = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(&[
            (
                "implementer",
                format!(
                    "{}printf ready > \"$HEX_EMIT_FILE\"",
                    queue(&format!(
                        "{{{FROM_OPERATOR},\"command\":{{\"steer\":{{\"text\":\"USE-THE-V2-API\"}}}}}}"
                    ))
                ),
            ),
            // No `{prompt}` in the argv, so the resolved prompt arrives on stdin.
            (
                "reviewer",
                "cat > review-prompt.txt; printf approved > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
        ]),
    );

    let report = runtime
        .start("cl", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    let prompt = std::fs::read_to_string(root.join("review-prompt.txt")).expect("review prompt");
    assert!(prompt.contains("USE-THE-V2-API"), "{prompt}");
    assert!(
        prompt.contains("operator guidance"),
        "guidance is fenced as operator input, not mixed into the node prompt: {prompt}"
    );
    assert!(
        prompt.contains("review it"),
        "the node's own prompt survives: {prompt}"
    );

    // Journaled, so a replay reproduces the prompt the attempt actually ran.
    let events = runtime.events(&report.run_id).expect("events");
    assert!(events.iter().any(|e| matches!(
        &e.body,
        EventBody::Steered { text } if text == "USE-THE-V2-API"
    )));
}

/// `human` nodes used to pass `hex validate` and then fail the run. Now the node
/// blocks on the inbox and an answer both routes the run and becomes the node's
/// result for the next prompt.
#[test]
fn a_human_node_blocks_until_answered_and_its_answer_becomes_the_node_result() {
    let root = temp_root("human");
    write_graph(&root, "ap", APPROVAL_GRAPH);

    let mut ws = Workers::new();
    ws.insert(
        "planner",
        Box::new(
            CommandWorker::new("planner", sh("printf 'PLAN-A' > \"$HEX_RESULT_FILE\""))
                .with_result_capture(Some(ResultCapture::File)),
        ),
    );
    ws.insert(
        "builder",
        Box::new(CommandWorker::new(
            "builder",
            sh("cat > build-prompt.txt; printf ready > \"$HEX_EMIT_FILE\""),
        )),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), ws);

    // The operator: waits for the question, then answers — the real sequence, and
    // proof the driver is genuinely blocked rather than racing ahead.
    let watched = root.clone();
    let operator = std::thread::spawn(move || {
        for _ in 0..200 {
            let run_dir = std::fs::read_dir(watched.join(".hex").join("runs"))
                .ok()
                .and_then(|d| {
                    d.filter_map(Result::ok)
                        .map(|e| e.path())
                        .find(|p| p.is_dir())
                });
            if let Some(dir) = run_dir
                && std::fs::read_to_string(dir.join("events.jsonl"))
                    .is_ok_and(|j| j.contains("human_requested"))
            {
                Inbox::new(&dir)
                    .send(
                        &Actor::human("t"),
                        &Command::Respond {
                            text: "SHIP-IT-BUT-RENAME-THE-FLAG".to_owned(),
                        },
                    )
                    .expect("send respond");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the run never asked for a human decision");
    });

    let report = runtime
        .start("ap", None, None, &Isolation::Shared)
        .expect("run");
    operator.join().expect("operator thread");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    let events = runtime.events(&report.run_id).expect("events");
    // The question carries the interpolated prompt (the plan being approved).
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::HumanRequested { prompt } if prompt.contains("PLAN-A")
        )),
        "{events:#?}"
    );
    // The answer carries the actor and the signal it routed on.
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::HumanResponded { text, signal }
                if text == "SHIP-IT-BUT-RENAME-THE-FLAG"
                    && signal == "done"
                    && e.actor == Actor::human("t")
        )),
        "{events:#?}"
    );
    // And it landed in `results`: the downstream prompt read it back.
    let prompt = std::fs::read_to_string(root.join("build-prompt.txt")).expect("build prompt");
    assert!(
        prompt.contains("SHIP-IT-BUT-RENAME-THE-FLAG"),
        "an operator answer hands on exactly like an agent result: {prompt}"
    );
}

/// Two commands queued together must both take effect. The driver returns from
/// the human wait as soon as the `respond` answers it, so a batch consumed up
/// front lost the `steer` behind it — marked done, never applied, in entirely
/// ordinary operation.
///
/// The steer is written first so the assertions hold whichever way the driver's
/// 250 ms poll interleaves; in practice both files are queued before it wakes, so
/// this exercises the batch. `control::claiming_takes_one_command_and_leaves_the_rest_queued`
/// pins the mechanism without any timing.
#[test]
fn a_respond_and_a_steer_queued_together_both_apply() {
    let root = temp_root("batch");
    write_graph(&root, "ap", APPROVAL_GRAPH);

    let mut ws = Workers::new();
    ws.insert(
        "planner",
        Box::new(
            CommandWorker::new("planner", sh("printf 'PLAN-A' > \"$HEX_RESULT_FILE\""))
                .with_result_capture(Some(ResultCapture::File)),
        ),
    );
    ws.insert(
        "builder",
        Box::new(CommandWorker::new(
            "builder",
            sh("cat > build-prompt.txt; printf ready > \"$HEX_EMIT_FILE\""),
        )),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), ws);

    let watched = root.clone();
    let operator = std::thread::spawn(move || {
        for _ in 0..200 {
            let run_dir = std::fs::read_dir(watched.join(".hex").join("runs"))
                .ok()
                .and_then(|d| {
                    d.filter_map(Result::ok)
                        .map(|e| e.path())
                        .find(|p| p.is_dir())
                });
            if let Some(dir) = run_dir
                && std::fs::read_to_string(dir.join("events.jsonl"))
                    .is_ok_and(|j| j.contains("human_requested"))
            {
                let inbox = Inbox::new(&dir);
                inbox
                    .send(
                        &Actor::human("t"),
                        &Command::Steer {
                            text: "USE-THE-V2-API".to_owned(),
                        },
                    )
                    .expect("send steer");
                inbox
                    .send(
                        &Actor::human("t"),
                        &Command::Respond {
                            text: "SHIP-IT".to_owned(),
                        },
                    )
                    .expect("send respond");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the run never asked for a human decision");
    });

    let report = runtime
        .start("ap", None, None, &Isolation::Shared)
        .expect("run");
    operator.join().expect("operator thread");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));

    let events = runtime.events(&report.run_id).expect("events");
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::Steered { text } if text == "USE-THE-V2-API"
        )),
        "the steer queued alongside the answer must not be swallowed: {events:#?}"
    );
    // And it reached the attempt after the answer, not just the journal.
    let prompt = std::fs::read_to_string(root.join("build-prompt.txt")).expect("build prompt");
    assert!(prompt.contains("USE-THE-V2-API"), "{prompt}");
    assert!(
        prompt.contains("SHIP-IT"),
        "the answer handed on too: {prompt}"
    );
}

/// An answer nobody asked for is journaled as ignored rather than silently
/// dropped (or worse, leaked into a later prompt).
#[test]
fn a_respond_with_no_human_waiting_is_journaled_as_ignored() {
    let root = temp_root("stray-respond");
    write_graph(&root, "cl", LOOP_GRAPH);
    let runtime = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(&[
            (
                "implementer",
                format!(
                    "{}printf ready > \"$HEX_EMIT_FILE\"",
                    queue(&format!(
                        "{{{FROM_OPERATOR},\"command\":{{\"respond\":{{\"text\":\"yes\"}}}}}}"
                    ))
                ),
            ),
            (
                "reviewer",
                "cat > review-prompt.txt; printf approved > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
        ]),
    );

    let report = runtime
        .start("cl", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, Some(Disposition::Succeeded));
    let events = runtime.events(&report.run_id).expect("events");
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::Note { text } if text.contains("ignored a `respond`")
        )),
        "{events:#?}"
    );
    let prompt = std::fs::read_to_string(root.join("review-prompt.txt")).expect("review prompt");
    assert!(
        !prompt.contains("yes"),
        "a stray answer must not reach a prompt: {prompt}"
    );
}

/// `hex cancel` on a run nobody is driving still appends the terminal itself,
/// and reports which path it took.
#[test]
fn cancel_of_an_idle_run_is_recorded_directly() {
    let root = temp_root("idle-cancel");
    write_graph(&root, "cl", LOOP_GRAPH);
    let runtime = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(&[
            (
                "implementer",
                format!(
                    "{}printf ready > \"$HEX_EMIT_FILE\"",
                    queue(&format!("{{{FROM_OPERATOR},\"command\":\"pause\"}}"))
                ),
            ),
            (
                "reviewer",
                "printf approved > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
        ]),
    );
    // Pause it first, so the run exists, is unfinished, and has no driver.
    let report = runtime
        .start("cl", None, None, &Isolation::Shared)
        .expect("run");
    assert_eq!(report.disposition, None);

    let outcome = runtime
        .cancel(&report.run_id, &Actor::human("t"))
        .expect("cancel");
    assert_eq!(outcome, hex_runtime::Cancellation::Recorded);
    assert_eq!(
        runtime.status(&report.run_id).expect("status").disposition,
        Some(Disposition::Cancelled)
    );
    // Nothing was left queued for a driver that will never read it.
    assert!(
        Inbox::new(&only_run_dir(&root))
            .claim_next()
            .expect("claim")
            .is_none()
    );
}

/// A run summary tells "finished" from "paused" from "nobody is driving this",
/// which is what makes a parallel run findable and resumable.
#[test]
fn a_summary_reports_liveness_and_age() {
    let root = temp_root("summary");
    write_graph(&root, "cl", LOOP_GRAPH);
    let runtime = Runtime::with_workers(
        root.clone(),
        Config::builtin(),
        workers(&[
            (
                "implementer",
                "printf ready > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
            (
                "reviewer",
                "printf approved > \"$HEX_EMIT_FILE\"".to_owned(),
            ),
        ]),
    );
    let report = runtime
        .start("cl", None, None, &Isolation::Shared)
        .expect("run");

    let runs = runtime.list_runs().expect("list");
    assert_eq!(runs.len(), 1);
    let summary = &runs[0];
    assert_eq!(summary.run_id, report.run_id);
    assert_eq!(
        summary.status,
        Some(Status::Finished(Disposition::Succeeded))
    );
    assert_eq!(summary.liveness, hex_runtime::Liveness::Finished);
    assert!(summary.error.is_none());
    assert!(summary.updated_at_ms >= summary.created_at_ms);
    assert!(summary.attempts >= 2, "attempts counted: {summary:?}");

    // A finished run refuses new commands rather than queueing them for nobody.
    let err = runtime
        .control(&report.run_id, &Actor::human("t"), &Command::Pause)
        .expect_err("must refuse");
    assert!(err.to_string().contains("already finished"), "{err}");
}
