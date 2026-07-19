//! `hex-worker` — the [`Worker`] trait, capability manifest, and adapters.
//!
//! A Worker is *our adapter*: it wraps one opaque external agent — a
//! coding-agent CLI, a plain subprocess, or a deterministic mock — behind a
//! common, capability-declaring interface. The kernel decides, the runtime
//! orchestrates; a worker only runs **one** agent and reports back. It never
//! coordinates: sub-agents, watchdogs, and fan-out are kernel-routed /
//! runtime-scheduled graph constructs, or the external agent's own internal
//! business — never logic in this crate.
//!
//! Depends on [`hex_proto`] and [`hex_kernel`], never the reverse.
//!
//! Status: scaffold.

use hex_kernel::graph::NodeKind;
use hex_proto::Capability;

/// The common contract every worker adapter implements.
///
/// (placeholder — start/events/steer/cancel/wait land here alongside the
/// [`CapabilityManifest`] the graph validator checks against node
/// requirements)
pub trait Worker {
    /// The capability manifest this worker advertises to the graph validator.
    fn capabilities(&self) -> CapabilityManifest;
}

/// The full set of [`Capability`] entries one worker advertises. The graph
/// validator rejects a graph whose nodes demand capabilities the assigned
/// worker does not declare.
///
/// (placeholder shape)
#[derive(Debug, Clone, Default)]
pub struct CapabilityManifest {
    /// Capabilities this worker supports.
    pub capabilities: Vec<Capability>,
}

impl CapabilityManifest {
    /// Whether the manifest declares `capability`.
    #[must_use]
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// The node kinds executed *through* a worker: `Agent` (one external agent)
/// and `Command` (a subprocess). `Gate`/`Human`/`Terminal` nodes are handled
/// by kernel + runtime, never a worker. Placeholder tying this layer to the
/// [`hex_kernel`] IR it consumes.
#[must_use]
pub fn executable_kinds() -> [NodeKind; 2] {
    [NodeKind::Agent, NodeKind::Command]
}

pub mod mock {
    //! Deterministic mock worker used to test kernel + runtime without any
    //! model (the contract suite runs against it).

    use super::{CapabilityManifest, Worker};
    use hex_proto::Capability;

    /// A worker that emits prescribed results for tests.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct MockWorker;

    impl Worker for MockWorker {
        fn capabilities(&self) -> CapabilityManifest {
            CapabilityManifest {
                capabilities: vec![Capability::StructuredEvents, Capability::FreshSessions],
            }
        }
    }
}

pub mod subprocess {
    //! Generic subprocess worker: run an argv (no shell by default), capture
    //! output, map exit status.
    //!
    //! (stub — coding-agent presets layer on top of this)

    use super::{CapabilityManifest, Worker};
    use hex_proto::Capability;

    /// A worker that drives one external process from an argv.
    ///
    /// (placeholder — spawn/stream/cancel land here)
    #[derive(Debug, Default, Clone)]
    pub struct SubprocessWorker {
        /// Program + arguments, executed directly — never a shell string.
        pub argv: Vec<String>,
    }

    impl Worker for SubprocessWorker {
        fn capabilities(&self) -> CapabilityManifest {
            CapabilityManifest {
                capabilities: vec![Capability::FreshSessions, Capability::GracefulCancel],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockWorker;
    use super::subprocess::SubprocessWorker;
    use super::{Worker, executable_kinds};
    use hex_kernel::graph::NodeKind;
    use hex_proto::Capability;

    #[test]
    fn mock_advertises_fresh_sessions() {
        assert!(
            MockWorker
                .capabilities()
                .supports(Capability::FreshSessions)
        );
    }

    #[test]
    fn subprocess_does_not_advertise_live_steering() {
        assert!(
            !SubprocessWorker::default()
                .capabilities()
                .supports(Capability::LiveSteering)
        );
    }

    #[test]
    fn executes_agent_and_command_kinds() {
        assert!(executable_kinds().contains(&NodeKind::Agent));
        assert!(executable_kinds().contains(&NodeKind::Command));
    }
}
