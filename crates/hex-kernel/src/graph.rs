//! Compiled, immutable graph IR.
//!
//! A run executes an exact snapshot of this IR. Roles ("planner", "reviewer")
//! are *metadata* on an [`NodeKind::Agent`] node and interactivity is a policy
//! flag — never new kinds. Keeping the kind set tiny, and bounding every cycle
//! by construction (the loader gives every non-terminal node a visit bound),
//! are core design rules.

use std::collections::BTreeMap;

use hex_proto::Disposition;

/// The kind of a schedulable node (four, intentionally tiny).
///
/// There is deliberately no `Gate` kind: a gate is a *role*, not a kind — any
/// [`NodeKind::Command`] whose signal appears in the graph's `accept` contract
/// is acting as a gate. The two used to be separate variants with identical
/// fields, one validation arm and one executor; collapsing them removed the
/// duplication without losing any expressiveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Invoke one opaque external agent (a coding-agent CLI) via a worker.
    Agent,
    /// Run a deterministic executable/script producing `passed`/`failed`.
    Command,
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
            NodeKind::Human => "human",
            NodeKind::Terminal => "terminal",
        }
    }
}

/// Per-node context policy: whether each attempt gets a fresh worker session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Context {
    /// A new worker session per attempt (the default).
    #[default]
    Fresh,
    /// Resume the prior worker session for this node, so a loop's later rounds
    /// keep what the earlier ones established instead of re-deriving it. Requires
    /// the bound worker to declare [`hex_proto::Capability::SessionResume`].
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
        /// The routing events this agent is allowed to propose (as the
        /// `VERDICT:` line of its final message).
        may_propose: Vec<String>,
        /// Context policy.
        context: Context,
        /// Whether the agent should not modify the workspace (a reviewer).
        /// Advisory only: a hard read-only sandbox would also stop the agent
        /// writing the final message the run routes on, so this is conveyed via
        /// the node's prompt, not an OS boundary (see the worker).
        read_only: bool,
    },
    /// Run one or more deterministic commands; produces `passed`/`failed`. Acts
    /// as a *gate* when its signal is named in the graph's `accept` contract.
    Command {
        /// The steps to run, in declared order (never empty).
        steps: Vec<CommandStep>,
        /// Whether the steps run in sequence or concurrently.
        mode: CommandMode,
    },
    /// Explicit terminal outcome.
    Terminal {
        /// The disposition this node records.
        disposition: Disposition,
    },
    /// Suspend for a human decision or input, answered with `hex respond`.
    Human {
        /// Message shown to the operator.
        prompt: String,
    },
}

/// One command a [`NodeSpec::Command`] node runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStep {
    /// Argv, executed directly (never a shell string). Always resolved: a graph
    /// naming a project check the project has not configured is refused at
    /// compile time rather than silently passing.
    pub argv: Vec<String>,
    /// The `checks:` entry this resolved from, for diagnostics and log naming.
    /// `None` for a literal argv written in the graph.
    pub check: Option<String>,
}

impl CommandStep {
    /// A short label for logs and notes: the check name, else the program.
    #[must_use]
    pub fn label(&self) -> &str {
        self.check
            .as_deref()
            .or_else(|| self.argv.first().map(String::as_str))
            .unwrap_or("command")
    }
}

/// How a command node's steps execute. The two modes differ in *failure*
/// semantics as much as in concurrency, which is the whole point of having both:
/// `ordered` is for a pipeline where a later step is pointless once an earlier
/// one fails; `parallel` is for independent checks where you want every failure
/// in one round, so the agent can fix them all in one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandMode {
    /// Run in sequence and **stop at the first failure**.
    #[default]
    Ordered,
    /// Run concurrently and **run every step**, failing if any failed.
    Parallel,
}

impl CommandMode {
    /// The canonical lowercase name.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            CommandMode::Ordered => "ordered",
            CommandMode::Parallel => "parallel",
        }
    }
}

impl NodeSpec {
    /// The [`NodeKind`] of this spec.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        match self {
            NodeSpec::Agent { .. } => NodeKind::Agent,
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
    /// How many times this node may be entered (`budget: { visits: N }`).
    ///
    /// The only visit bound there is: the loader defaults every non-terminal
    /// node to [`DEFAULT_NODE_VISITS`], so a node with no declared `visits` is
    /// still bounded. Bounds *one* loop rather than every loop: a graph with a
    /// cheap lint cycle and an expensive review cycle can cap the review at 3
    /// without also capping the lint.
    pub max_visits: Option<u32>,
}

impl Node {
    /// A node with no per-node visit bound.
    #[must_use]
    pub fn new(id: impl Into<String>, spec: NodeSpec) -> Self {
        Self {
            id: id.into(),
            spec,
            max_visits: None,
        }
    }
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
///
/// There is deliberately no run-wide retry count: bounding one node
/// ([`Node::max_visits`]) says *which* loop is allowed to churn, whereas a
/// whole-run attempt budget only said how long the burn lasts. `elapsed_ms`,
/// `attempt_elapsed_ms` and `output_tokens` remain run-wide because time and
/// generation are genuinely global resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Budget {
    /// Maximum wall-clock time for the whole run (milliseconds).
    pub elapsed_ms: Option<u64>,
    /// Maximum wall-clock time for a *single* attempt (milliseconds).
    ///
    /// Always set by the loader (which applies
    /// [`DEFAULT_ATTEMPT_ELAPSED_MS`] when a graph declares neither bound), so
    /// no attempt can ever wait on a hung agent forever. Without it, one stuck
    /// process consumed the entire run budget — or blocked indefinitely when the
    /// graph declared no `elapsed` at all.
    pub attempt_elapsed_ms: Option<u64>,
    /// Maximum **generation** tokens across the whole run — what the agents
    /// produced, summed from `AttemptReported`.
    ///
    /// Generation only, deliberately. A bound over *every* reported token is
    /// dominated by cached input (93% of a real review run here), so it would
    /// have to be tuned to context size rather than to work done, and enabling
    /// `context: continue` would silently move it. Output tokens are the one
    /// measure immune to that.
    ///
    /// Checked at an attempt boundary like every other budget: a count only
    /// exists once an attempt reports, so the attempt that crosses the line is
    /// paid for and the next one never starts.
    pub output_tokens: Option<u64>,
}

/// Per-attempt wall-clock bound applied when a graph declares no attempt bound
/// (30 minutes). A backstop against an agent that hangs forever, not a tuning
/// knob — declare `budget.attempt` to override.
pub const DEFAULT_ATTEMPT_ELAPSED_MS: u64 = 30 * 60 * 1000;

/// How many times a non-terminal node may be entered when it declares no
/// `budget: { visits: N }`.
///
/// Applied by the loader, so every cycle is bounded by construction. Terminal
/// nodes keep `None`: `schedule` settles a terminal before any budget check, so
/// a bound there is recorded but never enforced.
pub const DEFAULT_NODE_VISITS: u32 = 5;

/// The run's acceptance contract: the evidence a success terminal requires, and
/// optionally where to go when it is missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Accept {
    /// Evidence that must hold for a success terminal to actually succeed.
    pub require: Vec<Requirement>,
    /// Node to route to when acceptance is unmet, instead of failing the run.
    /// Without it, reaching a success terminal with missing evidence ends the
    /// run `failed` — a dead end that spends the whole run and fixes nothing.
    pub on_unmet: Option<String>,
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
    pub accept: Accept,
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

    /// Whether `id` is acting as a **gate**: a `command` node whose verdict the
    /// acceptance contract names.
    ///
    /// Gate-ness is a role, not a kind (core rule 4), so it is derived — and it
    /// is derived *here* rather than in each of the four renderers that need it,
    /// because a fact recomputed in four places is a fact that can disagree with
    /// itself.
    #[must_use]
    pub fn is_gate(&self, id: &str) -> bool {
        matches!(
            self.node(id).map(|n| &n.spec),
            Some(NodeSpec::Command { .. })
        ) && self.accept.require.iter().any(|r| r.node == id)
    }

    /// The transitions the kernel can take that no [`Edge`] describes: a
    /// **success** terminal back to `accept.on_unmet`, taken when the acceptance
    /// contract is unmet there.
    ///
    /// The one definition of that implicit edge, so its two consumers agree:
    /// `schedule` *takes* it (emitting `RerouteUnmet`), and [`Topology`] *draws*
    /// it — `a → done` with `on_unmet: a` is edge-acyclic yet loops for real. An
    /// empty `require` list is always satisfied, so it yields nothing. Whether
    /// `to` exists is not asked here: that is validation's `E-accept-unmet-node`
    /// and the reroute's own lifecycle guard.
    ///
    /// [`Topology`]: crate::Topology
    #[must_use]
    pub fn implicit_reroutes(&self) -> Vec<(&str, &str)> {
        let Some(to) = self.accept.on_unmet.as_deref() else {
            return Vec::new();
        };
        if self.accept.require.is_empty() {
            return Vec::new();
        }
        self.nodes
            .values()
            .filter(|n| {
                matches!(
                    n.spec,
                    NodeSpec::Terminal {
                        disposition: Disposition::Succeeded
                    }
                )
            })
            .map(|n| (n.id.as_str(), to))
            .collect()
    }

    /// Where an unmet acceptance contract reroutes from `from`, if anywhere — the
    /// single-node view of [`Graph::implicit_reroutes`].
    #[must_use]
    pub fn implicit_reroute_from(&self, from: &str) -> Option<&str> {
        self.implicit_reroutes()
            .into_iter()
            .find(|(f, _)| *f == from)
            .map(|(_, to)| to)
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
                read_only: false,
            },
        );
        self
    }

    /// Add a command node running a literal argv (a *gate* once its signal is
    /// named in `accept`).
    #[must_use]
    pub fn command(mut self, id: &str, command: &[&str]) -> Self {
        self.insert(
            id,
            NodeSpec::Command {
                steps: vec![CommandStep {
                    argv: command.iter().map(|s| (*s).to_owned()).collect(),
                    check: None,
                }],
                mode: CommandMode::Ordered,
            },
        );
        self
    }

    /// Cap how many times an already-added node may be entered.
    #[must_use]
    pub fn max_visits(mut self, id: &str, visits: u32) -> Self {
        if let Some(node) = self.graph.nodes.get_mut(id) {
            node.max_visits = Some(visits);
        }
        self
    }

    /// Add a terminal node.
    #[must_use]
    pub fn terminal(mut self, id: &str, disposition: Disposition) -> Self {
        self.insert(id, NodeSpec::Terminal { disposition });
        self
    }

    /// Add a human node.
    #[must_use]
    pub fn human(mut self, id: &str, prompt: &str) -> Self {
        self.insert(
            id,
            NodeSpec::Human {
                prompt: prompt.to_owned(),
            },
        );
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

    /// Add an acceptance requirement `node.signal`.
    #[must_use]
    pub fn require(mut self, node: &str, signal: &str) -> Self {
        self.graph.accept.require.push(Requirement {
            node: node.to_owned(),
            signal: signal.to_owned(),
        });
        self
    }

    /// Route to `node` when acceptance is unmet, instead of failing the run.
    #[must_use]
    pub fn on_unmet(mut self, node: &str) -> Self {
        self.graph.accept.on_unmet = Some(node.to_owned());
        self
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> Graph {
        self.graph
    }

    fn insert(&mut self, id: &str, spec: NodeSpec) {
        self.graph.nodes.insert(id.to_owned(), Node::new(id, spec));
    }
}
