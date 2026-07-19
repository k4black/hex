//! Deterministic mock worker used to test kernel + runtime without any model.
//!
//! Scripted per node: each call to [`Worker::run`] pops the next signal queued
//! for that node id, so a `review` node can be told to request changes twice
//! then approve.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use hex_proto::Capability;

use crate::{CapabilityManifest, WorkOutcome, WorkRequest, Worker};

/// A worker that emits prescribed signals for tests.
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
    fn capabilities(&self) -> CapabilityManifest {
        CapabilityManifest::from(&[Capability::StructuredEvents, Capability::FreshSessions])
    }

    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        let next = self
            .scripts
            .lock()
            .expect("mock scripts lock")
            .get_mut(&request.node_id)
            .and_then(VecDeque::pop_front);
        match next {
            Some(signal) => WorkOutcome::signal(signal),
            None => WorkOutcome::error(format!("mock: no scripted signal for `{}`", request.node_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn request(node: &str) -> WorkRequest {
        WorkRequest {
            run_id: "run_0".to_owned(),
            node_id: node.to_owned(),
            attempt_id: "att_1".to_owned(),
            prompt: String::new(),
            may_propose: vec![],
            workdir: PathBuf::from("."),
            attempt_dir: PathBuf::from("."),
        }
    }

    #[test]
    fn emits_scripted_signals_in_order() {
        let w = MockWorker::new().on("review", &["changes_requested", "approved"]);
        assert_eq!(w.run(&request("review")), WorkOutcome::signal("changes_requested"));
        assert_eq!(w.run(&request("review")), WorkOutcome::signal("approved"));
        assert!(w.run(&request("review")).error.is_some());
    }
}
