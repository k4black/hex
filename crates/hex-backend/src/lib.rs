//! `hex-backend` — the backend contract and worker adapters.
//!
//! A [`Backend`] wraps an opaque external worker — a coding-agent CLI, a plain
//! subprocess, or a deterministic mock — behind a common, capability-declaring
//! interface. The engine schedules; backends execute. The core never learns
//! Claude/Codex/Gemini-specific details.
//!
//! Depends on [`hex_proto`] and [`hex_core`], never the reverse.
//!
//! Status: scaffold.

use hex_core::graph::NodeKind;
use hex_proto::Capability;

/// The common contract every worker adapter implements.
///
/// (placeholder — start/events/control/wait land here alongside a capability
/// manifest the graph validator can check against node requirements)
pub trait Backend {
    /// Capabilities this backend advertises to the graph validator.
    fn capabilities(&self) -> Vec<Capability>;
}

/// The node kinds a backend is asked to execute: `Agent` (a real worker) and
/// `Command` (a subprocess). `Gate`/`Human`/`Terminal` nodes are handled by the
/// engine, not a backend. Placeholder tying this layer to the [`hex_core`] IR it
/// consumes.
#[must_use]
pub fn executable_kinds() -> [NodeKind; 2] {
    [NodeKind::Agent, NodeKind::Command]
}

pub mod mock {
    //! Deterministic mock backend used to test the engine without any model.

    use super::Backend;
    use hex_proto::Capability;

    /// A backend that emits prescribed results for tests.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct MockBackend;

    impl Backend for MockBackend {
        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability::StructuredEvents, Capability::FreshSessions]
        }
    }
}

pub mod subprocess {
    //! Generic subprocess backend: run an argv (no shell by default), capture
    //! output, map exit status.
    //!
    //! (stub)
}

#[cfg(test)]
mod tests {
    use super::mock::MockBackend;
    use super::{Backend, executable_kinds};
    use hex_core::graph::NodeKind;
    use hex_proto::Capability;

    #[test]
    fn mock_advertises_fresh_sessions() {
        assert!(
            MockBackend
                .capabilities()
                .contains(&Capability::FreshSessions)
        );
    }

    #[test]
    fn executes_agent_and_command_kinds() {
        assert!(executable_kinds().contains(&NodeKind::Agent));
        assert!(executable_kinds().contains(&NodeKind::Command));
    }
}
