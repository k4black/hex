//! `hex-proto` — the versioned control protocol for hex.
//!
//! This is the one stable, public surface shared by every actor: the kernel,
//! the runtime, worker adapters, and every thin client (CLI, MCP, dashboard).
//! It defines the append-only [`Event`] envelope, operator [`Command`]s, and
//! worker [`Capability`] manifest entries.
//!
//! It has **no** dependencies on other hex crates — the whole workspace points
//! inward to here.
//!
//! Status: scaffold. Every type below is a placeholder pending the first real
//! design pass.

use serde::{Deserialize, Serialize};

/// Current protocol schema version. Every persisted [`Event`] records this so
/// old journals stay readable as the protocol evolves.
pub const PROTOCOL_VERSION: u32 = 1;

/// Append-only fact recorded in a run journal. The journal is authoritative;
/// all status/graph views are projections *computed* from these events, never
/// stored as a second source of truth.
///
/// (placeholder shape)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Protocol version this event was written under.
    pub schema_version: u32,
    /// Monotonic per-run sequence number assigned by the runtime.
    pub seq: u64,
    /// Owning run identifier.
    pub run_id: String,
    /// Dotted event type, e.g. `run.created`, `gate.failed`, `attempt.started`.
    pub kind: String,
}

/// An operator command issued over the shared control protocol. A human at a
/// TTY and an agent (via injected `hex emit` or MCP tool hooks) send the
/// *same* commands; authority is scoped per actor, not per surface.
///
/// (placeholder set — `run`/`resume` are the only execution verbs; there is no
/// retry/replay/skip, redoing work is a new run)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// Report the projected run status.
    Status,
    /// Stop scheduling new attempts.
    Pause,
    /// Continue the same run from its journal (after pause *or* crash).
    Resume,
    /// Cancel the run.
    Cancel,
}

/// A capability a worker adapter advertises in its manifest so the graph
/// validator can reject definitions the adapter cannot satisfy (e.g. an
/// `interactive: true` node on a worker without live steering).
///
/// (placeholder set)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Emits structured events rather than only prose on stdout.
    StructuredEvents,
    /// Streams output incrementally.
    StreamingOutput,
    /// Can start a fresh agent session per attempt.
    FreshSessions,
    /// Can resume a prior agent session.
    SessionResume,
    /// Accepts mid-attempt steering input (required for interactive sessions).
    LiveSteering,
    /// Supports graceful cancellation.
    GracefulCancel,
    /// Reports token/cost usage per attempt.
    CostReporting,
    /// Can run the agent in a read-only mode.
    ReadOnlyMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_pinned() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn event_serializes_to_json() {
        let event = Event {
            schema_version: PROTOCOL_VERSION,
            seq: 0,
            run_id: "run_0".to_owned(),
            kind: "run.created".to_owned(),
        };
        let json = serde_json::to_string(&event).expect("event serializes");
        assert!(json.contains("run.created"));
    }
}
