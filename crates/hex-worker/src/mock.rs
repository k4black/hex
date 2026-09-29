//! Deterministic mock worker used to test kernel + runtime without any model.
//!
//! Scripted per node: each call to [`Worker::run`] pops the next signal queued
//! for that node id, so a `review` node can be told to request changes twice
//! then approve.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use crate::{WorkOutcome, WorkRequest, Worker};

/// A worker that produces prescribed verdicts for tests.
#[derive(Debug, Default)]
pub struct MockWorker {
    scripts: Mutex<BTreeMap<String, VecDeque<String>>>,
}

impl MockWorker {
    /// A mock with no scripts (every node will error until scripted).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue the `signals` a node will emit, in order (builder style).
    #[must_use]
    pub fn on(self, node_id: &str, signals: &[&str]) -> Self {
        self.scripts
            .lock()
            .expect("mock scripts lock")
            .entry(node_id.to_owned())
            .or_default()
            .extend(signals.iter().map(|s| (*s).to_owned()));
        self
    }
}

impl Worker for MockWorker {
    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        let next = self
            .scripts
            .lock()
            .expect("mock scripts lock")
            .get_mut(&request.node_id)
            .and_then(VecDeque::pop_front);
        match next {
            Some(signal) => WorkOutcome::verdict(&signal),
            None => WorkOutcome::error(format!(
                "mock: no scripted signal for `{}`",
                request.node_id
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(node: &str) -> WorkRequest {
        crate::test_request(std::path::Path::new("."), node)
    }

    #[test]
    fn a_scripted_verdict_becomes_a_final_message() {
        let w = MockWorker::new().on("review", &["changes_requested", "approved"]);
        assert_eq!(
            w.run(&request("review")),
            WorkOutcome::verdict("changes_requested")
        );
        assert_eq!(w.run(&request("review")), WorkOutcome::verdict("approved"));
        assert!(w.run(&request("review")).error.is_some());
    }
}
