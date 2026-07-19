//! `hex-kernel` — the pure, deterministic kernel of hex.
//!
//! Owns the compiled graph IR (nodes, edges, bounded cycles), the journal
//! *model*, projections, and the three pure functions the runtime drives:
//! [`reduce`], [`schedule`], and [`accept`]. Depends only on [`hex_proto`].
//!
//! The kernel is **pure**: no IO, no clock, no subprocesses, no worker
//! adapters, no rendering. It never performs external actions — it emits
//! [`Effect`] *intents* that only the runtime executes (intent-before-effect,
//! with idempotency keys). This keeps routing and completion logic testable
//! without any model or subprocess, and routing spends no tokens.
//!
//! The graph *surface syntax* (standard YAML) is loaded elsewhere; this crate
//! models the compiled IR only.
//!
//! Status: scaffold.

use hex_proto::{Event, PROTOCOL_VERSION};

pub mod graph {
    //! Compiled, immutable graph IR.
    //!
    //! Roles such as "planner" or "reviewer" are *metadata* on an
    //! [`NodeKind::Agent`] node, and interactivity is a *policy flag* on
    //! `agent` — never new kinds. Keeping the kind set tiny is a core design
    //! rule, as is rejecting unbounded cycles at validation time.
    //!
    //! (placeholder shapes)

    /// The kind of a schedulable node.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NodeKind {
        /// Invoke one opaque external agent (a coding-agent CLI) via a worker.
        Agent,
        /// Run a deterministic executable/script.
        Command,
        /// Run a deterministic validator producing pass/fail/escalate.
        Gate,
        /// Suspend durably for a human decision or input.
        Human,
        /// Explicit terminal outcome.
        Terminal,
    }

    /// A single node in the graph.
    #[derive(Debug, Clone)]
    pub struct Node {
        /// Stable node identifier.
        pub id: String,
        /// What this node does.
        pub kind: NodeKind,
    }

    /// A legal transition between nodes, taken on a named event and carrying
    /// an ordered condition — never model-chosen control flow.
    #[derive(Debug, Clone)]
    pub struct Edge {
        /// Source node id.
        pub from: String,
        /// Target node id.
        pub to: String,
        /// Event name that activates this edge.
        pub on: String,
    }

    /// A compiled, immutable graph: nodes plus the edges between them.
    #[derive(Debug, Clone, Default)]
    pub struct Graph {
        /// All nodes, keyed elsewhere by [`Node::id`].
        pub nodes: Vec<Node>,
        /// All legal transitions.
        pub edges: Vec<Edge>,
    }
}

pub mod journal {
    //! The journal *model*: the authoritative, append-only history of a run.
    //!
    //! This module defines what a journal is (versioned, sequenced, actored
    //! events; monotonic seq; one terminal per attempt) — the actual JSONL
    //! writer/reader with fsync and torn-tail tolerance is IO and therefore
    //! lives in `hex-runtime`, not here.
    //!
    //! (stub — replay/validation of event sequences lands here)
}

pub mod projection {
    //! Read models *computed* from the journal — never stored authority.
    //!
    //! `state = fold(reduce, journal)`; the atomic snapshot file is one kind
    //! of projection, an optimization the runtime may persist and must always
    //! be able to rebuild.
    //!
    //! (stub — status/graph/metrics projections land here)
}

/// Projected state of one run, computed by folding [`reduce`] over the
/// journal. Never persisted as authority.
///
/// (placeholder shape)
#[derive(Debug, Clone, Default)]
pub struct RunState {
    /// Sequence number of the last event folded in.
    pub last_seq: u64,
}

/// An *intent* describing one external action the kernel wants performed.
/// The kernel emits it; only the runtime executes it, writing
/// intent-before-effect with idempotency keys.
///
/// (placeholder set)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Start one attempt of a node via its worker.
    StartAttempt,
    /// Run a deterministic gate validator.
    RunGate,
    /// Suspend and request a human decision or input.
    RequestHuman,
    /// Cancel an in-flight attempt.
    CancelAttempt,
    /// Record the run's terminal disposition.
    RecordTerminal,
}

/// Provisional acceptance of a run outcome. A worker's "done" is a proposal;
/// required gates + acceptance rules decide — deterministic evidence outranks
/// model assertions.
///
/// (placeholder set)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Acceptance {
    /// Evidence still outstanding.
    #[default]
    Pending,
    /// Required gates passed; outcome accepted.
    Accepted,
    /// Required evidence failed; outcome rejected.
    Rejected,
}

/// Fold one journal event into the projected run state:
/// `new_state = reduce(old_state, event)`. Pure and deterministic.
///
/// (placeholder — only tracks the sequence number for now)
#[must_use]
pub fn reduce(state: RunState, event: &Event) -> RunState {
    RunState {
        last_seq: state.last_seq.max(event.seq),
    }
}

/// Derive the next batch of [`Effect`] intents from `(graph, state)`.
/// Deterministic: a model may *propose* a route only from its node's
/// `may_propose` allow-list; this function validates every transition.
///
/// (placeholder — always empty for now)
#[must_use]
pub fn schedule(_graph: &graph::Graph, _state: &RunState) -> Vec<Effect> {
    Vec::new()
}

/// Decide the provisional acceptance of the current state from required
/// gates + acceptance rules.
///
/// (placeholder — always pending for now)
#[must_use]
pub fn accept(_state: &RunState) -> Acceptance {
    Acceptance::Pending
}

/// Placeholder that also proves the `proto -> kernel` dependency direction
/// compiles. Returns the protocol version this build was compiled against.
#[must_use]
pub fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_proto::Event;

    fn event(seq: u64) -> Event {
        Event {
            schema_version: PROTOCOL_VERSION,
            seq,
            run_id: "run_0".to_owned(),
            kind: "run.created".to_owned(),
        }
    }

    #[test]
    fn reduce_advances_sequence() {
        let state = reduce(RunState::default(), &event(7));
        assert_eq!(state.last_seq, 7);
    }

    #[test]
    fn empty_graph_schedules_no_effects() {
        assert!(schedule(&graph::Graph::default(), &RunState::default()).is_empty());
    }

    #[test]
    fn acceptance_is_pending_by_default() {
        assert_eq!(accept(&RunState::default()), Acceptance::Pending);
    }
}
