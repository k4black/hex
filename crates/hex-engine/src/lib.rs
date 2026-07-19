//! `hex-engine` — the deterministic reducer, scheduler, and acceptance rules.
//!
//! The heart of the runtime is pure: `new_state = reduce(old_state, event)`, and
//! the scheduler derives ready work from `(graph, state)`. Actually running a
//! worker is an *effect* handled by [`hex_backend`](../hex_backend/index.html),
//! not here.
//!
//! This crate must never import backend adapters, rendering, or the CLI —
//! keeping routing and completion logic testable without any model. Depends on
//! [`hex_core`] (and, transitively, `hex-proto`).
//!
//! Status: scaffold.

use hex_core::graph::Graph;

/// Deterministic scheduler over a compiled [`Graph`].
///
/// (placeholder — real readiness/route evaluation/acceptance land here. Takes
/// `&self`: this type will hold scheduler state, so callers must borrow it, not
/// copy it.)
#[derive(Debug, Default)]
pub struct Scheduler;

impl Scheduler {
    /// Create a scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Return the ids of nodes ready to run given the current graph.
    ///
    /// (placeholder — always empty for now)
    #[must_use]
    pub fn ready(&self, _graph: &Graph) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_core::graph::Graph;

    #[test]
    fn empty_graph_has_no_ready_nodes() {
        assert!(Scheduler::new().ready(&Graph::default()).is_empty());
    }
}
