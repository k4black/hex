//! `hex-proto` — the versioned control protocol for hex.
//!
//! This is the one stable, public surface shared by every actor: the kernel,
//! the runtime, worker adapters, and every thin client (CLI, MCP, dashboard).
//! It defines the append-only [`Event`] envelope, operator [`Command`]s, and
//! worker [`Capability`] manifest entries.
//!
//! It has **no** dependencies on other hex crates — the whole workspace points
//! inward to here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Current protocol schema version. Every persisted [`Event`] records this so
/// old journals stay readable as the protocol evolves.
pub const PROTOCOL_VERSION: u32 = 1;

/// Who caused an event or issued a command. Authority is scoped per actor, not
/// per surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// The class of actor.
    pub kind: ActorKind,
    /// Stable identifier (e.g. `local`, `codex`, a human handle).
    pub id: String,
}

impl Actor {
    /// The deterministic runtime itself.
    #[must_use]
    pub fn runtime() -> Self {
        Self {
            kind: ActorKind::Runtime,
            id: "local".to_owned(),
        }
    }

    /// An agent worker, identified by its registry name.
    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Agent,
            id: id.into(),
        }
    }

    /// A human operator, identified by a handle (a login name, say). Every
    /// control command carries its issuer, so authority is scoped per actor
    /// rather than per surface and the journal records *who* paused or steered.
    #[must_use]
    pub fn human(id: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Human,
            id: id.into(),
        }
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.id)
    }
}

/// The class of an [`Actor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// The deterministic kernel/runtime.
    Runtime,
    /// An opaque external agent driven by a worker.
    Agent,
    /// A human operator.
    Human,
}

impl ActorKind {
    /// The canonical snake_case name — the same spelling serde uses on the wire.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorKind::Runtime => "runtime",
            ActorKind::Agent => "agent",
            ActorKind::Human => "human",
        }
    }
}

/// How one run ended — a single enum shared by scheduler, CLI, exit-code
/// mapping, and tests so a terminal outcome is never spelled two ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Completed and the acceptance contract passed.
    Succeeded,
    /// Reached a failure terminal or an unrecoverable error.
    Failed,
    /// An operator cancelled the run.
    Cancelled,
    /// A budget (attempts/visits) was exhausted.
    BudgetExhausted,
    /// A time budget elapsed.
    TimedOut,
}

impl Disposition {
    /// The canonical snake_case name — the *same* spelling serde uses on the
    /// wire, so every surface (CLI text, `--json`, journal) agrees. Adding a
    /// variant forces this match to be updated (compiler-checked).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Disposition::Succeeded => "succeeded",
            Disposition::Failed => "failed",
            Disposition::Cancelled => "cancelled",
            Disposition::BudgetExhausted => "budget_exhausted",
            Disposition::TimedOut => "timed_out",
        }
    }
}

impl std::fmt::Display for Disposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Append-only fact recorded in a run journal. The journal is authoritative;
/// all status/graph views are projections *computed* from these events, never
/// stored as a second source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Protocol version this event was written under.
    pub schema_version: u32,
    /// Monotonic per-run sequence number assigned by the runtime.
    pub seq: u64,
    /// Wall-clock time the event was recorded (Unix epoch milliseconds).
    pub at_ms: u64,
    /// Owning run identifier.
    pub run_id: String,
    /// Node this event concerns, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Attempt this event concerns, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// Who caused the event.
    pub actor: Actor,
    /// The typed body, flattened so the JSON carries a `kind` discriminator
    /// alongside the envelope fields.
    #[serde(flatten)]
    pub body: EventBody,
}

/// The typed payload of an [`Event`]. The `kind` tag is the on-the-wire
/// discriminator.
///
/// A node's routing token is a [`EventBody::Signal`] — agent proposals
/// (from a node's `may_propose` allow-list) and gate verdicts (`passed` /
/// `failed`) unify into the same event so edges match one thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventBody {
    /// The run was created over an exact graph snapshot.
    RunCreated {
        /// Hash of the exact persisted graph source snapshot.
        graph_hash: String,
        /// Operator values used to compile the graph (currently `prompt`).
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        /// The effective compile defaults (e.g. `worker`, `context`) used at
        /// creation, recorded here so resume is bound to them rather than to
        /// mutable config or a separate unbound file.
        #[serde(default)]
        defaults: BTreeMap<String, String>,
        /// The project `checks:` this run resolved at creation (name → argv),
        /// for the same reason as `defaults`: editing `.hex/config.yaml` must
        /// never change what a resumed run executes as its gate.
        #[serde(default)]
        checks: BTreeMap<String, Vec<String>>,
    },
    /// A success terminal was reached but the acceptance contract was unmet, so
    /// the run rerouted to `to` (the graph's `accept.on_unmet`) instead of
    /// failing. Its own event rather than a synthesized `Signal`, because
    /// `reduce` deliberately drops routing signals that no in-flight attempt
    /// produced — a fail-closed guard against forged routing that must not be
    /// relaxed just to express this.
    AcceptanceUnmet {
        /// The `node.signal` evidence that was missing.
        missing: Vec<String>,
        /// The node the run rerouted to, to go and produce it.
        to: String,
    },
    /// Scheduling has begun; the entry node is active.
    RunStarted,
    /// An operator paused the run at an attempt boundary (a `pause` command from
    /// the control inbox). Deliberately *not* a terminal disposition: a paused
    /// run has no outcome yet and continues with `hex resume`.
    RunPaused,
    /// A paused run was un-paused and is scheduling again.
    RunResumed,
    /// An operator injected guidance for the *next* agent attempt (a `steer`
    /// command). Journaled so a replay reproduces the prompt the attempt
    /// actually ran with — steering that lived only in memory would make the
    /// journal an incomplete account of the run.
    Steered {
        /// The operator's guidance text, fenced into the next prompt as
        /// operator input (trusted instructions, unlike agent output).
        text: String,
    },
    /// A `human` node suspended the run and asked its question. The runtime then
    /// blocks on a `respond` command; a crash while blocked leaves this event
    /// unanswered, and resume asks again.
    HumanRequested {
        /// The node's prompt, with `{{prompt}}`/`{{node.result}}` resolved.
        prompt: String,
    },
    /// An operator answered a `human` node. Carries both the answer (stored as
    /// the node's result, so downstream `{{node.result}}` works exactly as for
    /// an agent) and the routing signal it activates — its own event rather than
    /// a synthesized `Signal`, because `reduce` deliberately drops routing
    /// signals no in-flight attempt produced.
    HumanResponded {
        /// The operator's answer.
        text: String,
        /// The signal the response routes on (the node's single outgoing edge).
        signal: String,
    },
    /// An attempt was scheduled and is about to execute. Written
    /// intent-before-effect with an idempotency key, so a crash between here
    /// and the terminal event marks the attempt interrupted, never a silent
    /// rerun.
    AttemptStarted {
        /// Stable key deduplicating this attempt across restarts.
        idempotency_key: String,
        /// Worker registry name, for agent attempts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker: Option<String>,
    },
    /// An attempt started but never produced a terminal event (found orphaned
    /// on resume). The node is re-scheduled as a fresh attempt.
    AttemptInterrupted,
    /// The routing token a node produced: an agent proposal or a gate verdict.
    Signal {
        /// The event name edges match on.
        name: String,
    },
    /// An attempt's captured final result text (an agent's last message). Not a
    /// routing token — it carries the node's output so a downstream node can
    /// reference it as `{{<node>.result}}`. Recorded before the routing signal.
    NodeResult {
        /// The captured result text (treated as untrusted data downstream).
        text: String,
    },
    /// An attempt failed to execute (worker crashed, timed out, emitted nothing
    /// valid). This is a *terminal* event: it carries the run's resulting
    /// disposition so a failure is one atomic durable fact — there is no window
    /// between recording the failure and recording the outcome.
    AttemptFailed {
        /// Human-readable reason.
        reason: String,
        /// The terminal disposition this failure produces. Only `Failed` or
        /// `TimedOut` are legal here; a non-failure value is rejected by
        /// lifecycle validation and coerced to `Failed` in the reducer, so a
        /// failed attempt can never fail *open* into success.
        disposition: Disposition,
    },
    /// The run reached a terminal disposition.
    RunFinished {
        /// The final outcome.
        disposition: Disposition,
    },
    /// A free-form diagnostic note (never load-bearing for routing).
    Note {
        /// The note text.
        text: String,
    },
}

/// An operator command issued over the shared control protocol. A human at a
/// TTY and an agent send the *same* commands; authority is scoped per actor,
/// not per surface.
///
/// `run`/`resume` are the only execution verbs (there is no retry/replay/skip;
/// redoing work is a new run); these are the mid-run control commands.
///
/// A command is delivered by writing one of these (with its issuing [`Actor`])
/// into a run's control inbox, which the driver drains at attempt boundaries.
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
    /// Inject operator guidance into the next attempt's prompt.
    Steer {
        /// The guidance text.
        text: String,
    },
    /// Answer a `human` node that is blocking the run.
    Respond {
        /// The operator's answer.
        text: String,
    },
}

impl Command {
    /// The canonical verb name, for diagnostics and journal notes.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Command::Status => "status",
            Command::Pause => "pause",
            Command::Resume => "resume",
            Command::Cancel => "cancel",
            Command::Steer { .. } => "steer",
            Command::Respond { .. } => "respond",
        }
    }
}

/// A capability a worker adapter advertises in its manifest so the graph
/// validator can reject definitions the adapter cannot satisfy (e.g. an
/// `interactive: true` node on a worker without live steering).
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

    fn event(body: EventBody) -> Event {
        Event {
            schema_version: PROTOCOL_VERSION,
            seq: 1,
            at_ms: 0,
            run_id: "run_0".to_owned(),
            node_id: None,
            attempt_id: None,
            actor: Actor::runtime(),
            body,
        }
    }

    #[test]
    fn protocol_version_is_pinned() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn signal_event_roundtrips_flat() {
        let ev = event(EventBody::Signal {
            name: "passed".to_owned(),
        });
        let json = serde_json::to_string(&ev).expect("serializes");
        assert!(json.contains("\"kind\":\"signal\""));
        assert!(json.contains("\"name\":\"passed\""));
        let back: Event = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, ev);
    }

    #[test]
    fn run_finished_carries_disposition() {
        let ev = event(EventBody::RunFinished {
            disposition: Disposition::Succeeded,
        });
        let json = serde_json::to_string(&ev).expect("serializes");
        assert!(json.contains("\"disposition\":\"succeeded\""));
    }

    /// The control inbox persists commands as files, so their wire shape is a
    /// compatibility surface: a queued command must survive a hex upgrade.
    #[test]
    fn control_commands_roundtrip_including_their_payloads() {
        for cmd in [
            Command::Pause,
            Command::Resume,
            Command::Cancel,
            Command::Steer {
                text: "prefer the smaller diff".to_owned(),
            },
            Command::Respond {
                text: "approved".to_owned(),
            },
        ] {
            let json = serde_json::to_string(&cmd).expect("serializes");
            let back: Command = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back, cmd, "{json}");
        }
    }

    #[test]
    fn human_events_carry_actor_and_routing_signal() {
        let mut ev = event(EventBody::HumanResponded {
            text: "ship it".to_owned(),
            signal: "done".to_owned(),
        });
        ev.actor = Actor::human("kc");
        let json = serde_json::to_string(&ev).expect("serializes");
        assert!(json.contains("\"kind\":\"human_responded\""), "{json}");
        assert!(json.contains("\"signal\":\"done\""), "{json}");
        assert!(
            json.contains("\"kind\":\"human\""),
            "actor recorded: {json}"
        );
        let back: Event = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, ev);
    }

    #[test]
    fn optional_envelope_fields_are_omitted_when_absent() {
        let json = serde_json::to_string(&event(EventBody::RunStarted)).expect("serializes");
        assert!(!json.contains("node_id"));
        assert!(!json.contains("attempt_id"));
    }
}
