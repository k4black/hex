//! Compiled, immutable graph IR.
//!
//! A run executes an exact snapshot of this IR. Roles ("planner", "reviewer")
//! are *metadata* on an [`NodeKind::Agent`] node and interactivity is a policy
//! flag — never new kinds. Keeping the kind set tiny, and rejecting unbounded
//! cycles at validation time, are core design rules.

use std::collections::BTreeMap;

use hex_proto::Disposition;

/// The kind of a schedulable node (five, intentionally tiny).
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

impl NodeKind {
    /// The canonical lowercase name of this kind.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Agent => "agent",
            NodeKind::Command => "command",
            NodeKind::Gate => "gate",
            NodeKind::Human => "human",
            NodeKind::Terminal => "terminal",
        }
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-node context policy: whether each attempt gets a fresh worker session.
/// (The slim MVP only implements `Fresh`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Context {
    /// A new worker session per attempt (the default).
    #[default]
    Fresh,
    /// Resume the prior worker session.
    Continue,
}

/// Kind-specific configuration of a node. The variant *is* the kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeSpec {
    /// Invoke an external agent; it may propose only the listed routing events.
    Agent {
        /// Worker registry name.
        worker: String,
        /// Node prompt (with the operator prompt already interpolated).
        prompt: String,
        /// The routing events this agent is allowed to emit.
        may_propose: Vec<String>,
        /// Context policy.
        context: Context,
    },
    /// Run a deterministic validator; produces `passed`/`failed`.
    Gate {
        /// Argv, executed directly (never a shell string).
        command: Vec<String>,
    },
    /// Run a deterministic command; produces `passed`/`failed`.
    Command {
        /// Argv, executed directly (never a shell string).
        command: Vec<String>,
    },
    /// Explicit terminal outcome.
    Terminal {
        /// The disposition this node records.
        disposition: Disposition,
    },
    /// Suspend for a human (unimplemented in the slim MVP).
    Human {
        /// Message shown to the operator.
        prompt: String,
    },
}

impl NodeSpec {
    /// The [`NodeKind`] of this spec.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        match self {
            NodeSpec::Agent { .. } => NodeKind::Agent,
            NodeSpec::Gate { .. } => NodeKind::Gate,
            NodeSpec::Command { .. } => NodeKind::Command,
            NodeSpec::Terminal { .. } => NodeKind::Terminal,
            NodeSpec::Human { .. } => NodeKind::Human,
        }
    }
}

/// A single node: a stable id plus its kind-specific spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Stable node identifier.
    pub id: String,
    /// What this node does.
    pub spec: NodeSpec,
}

/// A legal transition, activated by a named routing event. Never model-chosen
/// control flow — the agent proposes an event, the kernel matches the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Source node id.
    pub from: String,
    /// Routing event name that activates this edge.
    pub on: String,
    /// Target node id.
    pub to: String,
}

/// Durable limits bounding the run. A limit is one field of a budget; a budget
/// never resets on resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Budget {
    /// Maximum attempts across the whole run.
    pub attempts: Option<u32>,
    /// Maximum wall-clock time (milliseconds).
    pub elapsed_ms: Option<u64>,
    /// Maximum visits to any single node (per-cycle bound).
    pub cycle_visits: Option<u32>,
}

impl Budget {
    /// Whether the budget bounds cycles at all (attempts or visit cap set).
    #[must_use]
    pub fn bounds_cycles(&self) -> bool {
        self.attempts.is_some() || self.cycle_visits.is_some()
    }
}

/// One clause of the acceptance contract: `node` must have last emitted
/// `signal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// The node that must have produced the evidence.
    pub node: String,
    /// The signal it must have last produced.
    pub signal: String,
}

/// A compiled, immutable graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// Human-readable name.
    pub name: String,
    /// Entry node id.
    pub entry: String,
    /// Nodes, keyed by id.
    pub nodes: BTreeMap<String, Node>,
    /// Legal transitions.
    pub edges: Vec<Edge>,
    /// Durable limits.
    pub budget: Budget,
    /// Acceptance contract.
    pub accept: Vec<Requirement>,
}

impl Graph {
    /// Look up a node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The target of the edge leaving `from` on `signal`, if any (first match).
    #[must_use]
    pub fn route(&self, from: &str, signal: &str) -> Option<&str> {
        self.edges
            .iter()
            .find(|e| e.from == from && e.on == signal)
            .map(|e| e.to.as_str())
    }

    /// Routing event names leaving `from`.
    #[must_use]
    pub fn signals_from(&self, from: &str) -> Vec<&str> {
        self.edges
            .iter()
            .filter(|e| e.from == from)
            .map(|e| e.on.as_str())
            .collect()
    }

    /// Start building a graph with a name and entry node.
    #[must_use]
    pub fn builder(name: &str, entry: &str) -> Builder {
        Builder {
            graph: Graph {
                name: name.to_owned(),
                entry: entry.to_owned(),
                ..Graph::default()
            },
        }
    }
}

/// Ergonomic builder for graphs (tests and the loader both use it).
#[derive(Debug)]
pub struct Builder {
    graph: Graph,
}

impl Builder {
    /// Add an agent node.
    #[must_use]
    pub fn agent(mut self, id: &str, worker: &str, prompt: &str, may_propose: &[&str]) -> Self {
        self.insert(
            id,
            NodeSpec::Agent {
                worker: worker.to_owned(),
                prompt: prompt.to_owned(),
                may_propose: may_propose.iter().map(|s| (*s).to_owned()).collect(),
                context: Context::Fresh,
            },
        );
        self
    }

    /// Add a gate node.
    #[must_use]
    pub fn gate(mut self, id: &str, command: &[&str]) -> Self {
        self.insert(
            id,
            NodeSpec::Gate {
                command: command.iter().map(|s| (*s).to_owned()).collect(),
            },
        );
        self
    }

    /// Add a terminal node.
    #[must_use]
    pub fn terminal(mut self, id: &str, disposition: Disposition) -> Self {
        self.insert(id, NodeSpec::Terminal { disposition });
        self
    }

    /// Add an edge `from --on--> to`.
    #[must_use]
    pub fn edge(mut self, from: &str, on: &str, to: &str) -> Self {
        self.graph.edges.push(Edge {
            from: from.to_owned(),
            on: on.to_owned(),
            to: to.to_owned(),
        });
        self
    }

    /// Set the budget.
    #[must_use]
    pub fn budget(mut self, budget: Budget) -> Self {
        self.graph.budget = budget;
        self
    }

    /// Add an acceptance requirement `node.signal`.
    #[must_use]
    pub fn require(mut self, node: &str, signal: &str) -> Self {
        self.graph.accept.push(Requirement {
            node: node.to_owned(),
            signal: signal.to_owned(),
        });
        self
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> Graph {
        self.graph
    }

    fn insert(&mut self, id: &str, spec: NodeSpec) {
        self.graph.nodes.insert(
            id.to_owned(),
            Node {
                id: id.to_owned(),
                spec,
            },
        );
    }
}
