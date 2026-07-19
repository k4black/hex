//! `hex-proto` — the versioned control protocol for hex.
//!
//! This is the one stable, public surface shared by every actor: the engine,
//! the CLI, agent workers, an MCP adapter, and dashboards. It defines the
//! append-only [`Event`] envelope, operator [`Command`]s, and backend
//! [`Capability`] manifests.
//!
//! It has **no** dependencies on other hex crates — the whole workspace points
//! inward to here.
//!
//! Status: scaffold. Every type below is a placeholder pending the first real
//! design pass; the graph *surface syntax* (TOML vs YAML) is deliberately still
//! undecided.

use serde::{Deserialize, Serialize};

/// Current protocol schema version. Every persisted [`Event`] records this so
/// old journals stay readable as the protocol evolves.
pub const PROTOCOL_VERSION: u32 = 1;

/// Append-only fact recorded in a run journal. The journal is authoritative;
/// all status/graph views are projections derived from these events.
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
/// TTY and an orchestrating agent send the *same* commands; authority is scoped
/// separately.
///
/// (placeholder set)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// Report the projected run status.
    Status,
    /// Stop scheduling new attempts.
    Pause,
    /// Resume a paused run.
    Resume,
    /// Cancel the run.
    Cancel,
}

/// A capability a backend advertises so the graph validator can reject
/// definitions an adapter cannot satisfy (e.g. a node needing live steering on
/// a one-shot backend).
///
/// (placeholder set)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Emits structured events rather than only prose on stdout.
    StructuredEvents,
    /// Streams output incrementally.
    StreamingOutput,
    /// Can start a fresh worker session per attempt.
    FreshSessions,
    /// Can resume a prior worker session.
    SessionResume,
    /// Supports graceful cancellation.
    GracefulCancel,
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
