//! `hex-core` — the deterministic domain model of hex.
//!
//! Owns the compiled graph IR (nodes, edges, bounded cycles), the run/attempt
//! model, the append-only journal, and rebuildable projections. Depends only on
//! [`hex_proto`].
//!
//! The graph *surface syntax* (TOML/YAML) is intentionally undecided: this crate
//! models the compiled IR, and parsing/loading is a stub to be filled in during
//! the first real implementation.
//!
//! Status: scaffold.

use hex_proto::PROTOCOL_VERSION;

pub mod graph {
    //! Compiled, immutable graph IR.
    //!
    //! Roles such as "planner" or "reviewer" are *metadata* on an
    //! [`NodeKind::Agent`] node, never distinct node kinds — keeping the kind
    //! set tiny is a core design rule.
    //!
    //! (placeholder shapes)

    /// The kind of a schedulable node.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NodeKind {
        /// Invoke an external, nondeterministic worker (a coding-agent CLI).
        Agent,
        /// Run a deterministic executable/script.
        Command,
        /// Run deterministic evidence and produce pass/fail/escalate.
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

    /// A legal transition between nodes, taken on a named event.
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
    //! Append-only JSONL journal: the authoritative history of a run.
    //!
    //! (stub — writer/reader/replay/repair land here)
}

pub mod projection {
    //! Read models derived from — and fully rebuildable from — the journal.
    //!
    //! (stub — status/graph/metrics projections land here)
}

/// Placeholder that also proves the `proto -> core` dependency direction
/// compiles. Returns the protocol version this build was compiled against.
#[must_use]
pub fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}
