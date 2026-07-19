# Research report: a thin CLI for agentic loops and graphs

**TL;DR**: There is a clear gap between six-line Ralph scripts that provide almost no supervision and large “agent operating systems” that introduce task databases, role hierarchies, dashboards, MCP servers, memory systems, worktrees, and daemons. The strongest product opportunity is a **deterministic, local control plane for nondeterministic external workers**:

> A small CLI that compiles a human-readable graph, runs existing agent CLIs as opaque workers, records every transition in an append-only journal, enforces hard limits and evidence gates, and exposes the same control protocol to humans and agents.

For this product, **AutoLoop is the closest reference**, **Microsoft Conductor has the cleanest graph-authoring model**, **Ralph Orchestrator has the richest termination and recovery logic**, **AWS CAO has the best lessons for supervising real CLI processes**, and **Gas Town/Gas City have the strongest durable-work and reconciliation concepts**. Ruflo is useful mainly as a lesson in keeping “coordination” and “execution” semantics unambiguous. LangGraph, Pydantic Graph, Mastra, and Microsoft Agent Framework are valuable references for graph semantics, but they operate primarily inside application runtimes rather than supervising opaque command-line agents. ([GitHub][1])

This is a July 19, 2026 snapshot. I treated repository code and official documentation as primary evidence. Project blogs provide design intent; Reddit is included only as anecdotal evidence about usability and failure modes.

---

## 1. The project landscape

The current ecosystem falls into four architectural families.

### Ralph-style loop runners

These repeatedly invoke a coding agent, normally with fresh context, until a completion condition or safety limit fires. The original Ralph pattern, Ralph Orchestrator, AutoLoop, and dozens of smaller Ralph implementations sit here.

Their main strength is simplicity. Their main danger is that “repeat until done” easily becomes “repeat until the budget is gone.”

### Declarative graph and workflow runtimes

Microsoft Conductor, LangGraph, Pydantic Graph, Mastra, Microsoft Agent Framework, and CrewAI Flows define nodes, edges, branches, joins, checkpoints, and human gates.

They provide good control-flow semantics, but most are designed around model APIs, Python or TypeScript functions, and framework-managed state. They are not primarily shell-level supervisors for Claude Code, Codex, Gemini CLI, OpenCode, or arbitrary executables.

### CLI and workspace supervisors

AWS CLI Agent Orchestrator, Agent Orchestrator, Gas Town, Claude Squad-style tools, and many newer worktree managers supervise real terminal processes. They preserve each agent CLI’s native authentication, tools, and interactive behavior.

Their trade-off is infrastructure: tmux, PTYs, daemons, SQLite, web interfaces, Git worktrees, inboxes, and provider-specific terminal-state detection.

### Fleet control planes and “agent operating systems”

Gas Town, Gas City, and Ruflo extend orchestration into durable assignments, task ledgers, persistent memory, organizational roles, scheduled work, hooks, health monitoring, and multi-project operation.

These systems have important ideas, but they are the wrong initial scope for a thin loop CLI.

The opportunity is therefore not to build another full agent framework. It is to occupy the boundary between the first three families:

> **Graph semantics from workflow frameworks, process supervision from CLI orchestrators, and the operational simplicity of a Ralph loop.**

---

# 2. Primary repository studies

## 2.1 AutoLoop: the closest match to your product idea

### Main idea

AutoLoop is explicitly presented as a simpler, more opinionated extraction from Ralph Orchestrator. It retains reusable loop presets, multiple roles, graph topology, budgets, inspectable state, worktree isolation, and agent-operable commands, while trying to remove the broader experimental surface of Ralph. It also exposes an embeddable runtime with cancellation and event callbacks rather than making the CLI the only entry point. ([GitHub][1])

A preset is organized around configuration, topology, prompts, and optional harness instructions:

```text
preset/
├── autoloops.toml
├── topology.toml
├── harness.md
└── roles/
    ├── planner.md
    ├── implementer.md
    └── reviewer.md
```

This is a strong authoring model because configuration remains structured while long prompts remain ordinary Markdown files that both humans and agents can edit. ([GitHub][1])

### Repository structure

The repository is a TypeScript workspace with packages broadly separated by responsibility:

```text
packages/
├── core/
├── harness/
├── backend/
├── cli/
├── presets/
├── dashboard/
├── gh-sync/
├── issue-sync-core/
├── kanban/
└── linear-sync/
```

`core` owns topology, events, configuration schemas, journals, evidence, worktrees, concurrency, memory, profiles, run health, and task models. `harness` owns actual iteration behavior: prompt construction, backend invocation, hooks, completion checks, acceptance, progress and stall detection, fan-out, steering, suspension, file-change auditing, and postconditions. `backend` abstracts execution of one worker iteration. ([GitHub][2])

That separation is conceptually strong:

```text
definition and types     core
execution policy         harness
external worker call     backend
operator surface         cli
```

### Runtime logic

At a high level, the runtime works as follows:

1. Load a preset and normalize its roles, events, gates, stages, and completion event.
2. Validate that roles and events are reachable and routable.
3. Select the next role or stage from the current event.
4. Build the prompt and iteration context.
5. Invoke a backend through a common backend contract.
6. Collect worker output, structured events, usage, artifacts, and file changes.
7. Append agent, harness, and operator events to the journal.
8. Apply evidence gates, budgets, hooks, circuit breakers, and completion rules.
9. Route the resulting event to the next role or terminate.

The topology model supports per-role backends and concurrency, read-only or restricted roles, explicit completion events, fan-out stages, and evidence gates such as tests, lint, type checking, coverage, and mutation checks. Validation catches orphaned roles, unreachable events, unroutable outputs, and invalid completion paths. ([GitHub][3])

The iteration layer treats suspension as a durable boundary, clamps per-iteration timeouts to the remaining run budget, runs hooks before and after workers, audits file modifications, supports steering, and applies backoff or circuit-breaking behavior to transient and quota-related errors. It generally creates a fresh agent session or conversation for an iteration unless a backend explicitly implements persistent behavior. ([GitHub][4])

### Journal design

This is AutoLoop’s most transferable design.

It separately records:

* agent-generated events,
* harness-generated events,
* operator-generated events.

The JSONL reader tolerates malformed or partially written lines, while append operations can be flushed and synchronized. Journals can be repaired or quarantined, and events from multiple scopes can be merged in timestamp order. ([GitHub][5])

That gives a useful state model:

```text
immutable journal  →  derived run state  →  CLI/TUI/dashboard views
```

The journal is the evidence; the status screen is only a projection.

### Human and agent usability

AutoLoop has commands for running, listing, watching, inspecting, emitting events, managing worktrees and chains, checking health, triaging a run, and describing capabilities. It supports machine-readable forms, stable exit behavior, and “robot documentation” intended for agent consumption. ([GitHub][1])

This is precisely the right principle for your app:

> Do not create a human control plane and a separate agent control plane. Create one versioned control protocol, then expose human-readable and machine-readable renderers.

### What to copy

Copy the separation of topology, harness, and backend. Copy the append-only journal, capability introspection, structured diagnostics, explicit evidence gates, and the idea that human and agent actions are both recorded operator events.

### What not to copy initially

AutoLoop is already expanding into dashboards, issue synchronization, Kanban, profiles, memory, chains, tasks, and numerous harness policies. Those may be useful, but they demonstrate how quickly a “small loop runner” becomes a platform.

Your first release should be considerably smaller than current AutoLoop.

---

## 2.2 Ralph Orchestrator: the richest safety and runtime reference

### Main idea

Ralph Orchestrator implements the Ralph pattern as an event-driven “hat” system. A hat supplies a role, prompt, subscriptions, and emitted events. The event loop repeatedly selects a hat, invokes a supported coding-agent backend, parses emitted events, and routes to the next hat until completion or a termination policy fires. The project supports multiple coding CLIs, worktrees, task and memory facilities, hooks, planning, a TUI, a web API, MCP, and Telegram control. ([GitHub][6])

An important source-level nuance is that hats should not automatically be interpreted as independently running agents. Current code retains a universal Ralph executor as the fallback and uses custom hats to define topology and prompt behavior around that executor. Depending on configuration, the system is closer to a sequential event-driven coordinator than a distributed collection of autonomous processes. ([GitHub][7])

That semantic distinction matters for your own terminology. “Agent,” “role,” “worker process,” and “graph node” should not be interchangeable terms.

### Repository structure

Ralph is a substantial Rust workspace:

```text
crates/
├── ralph-proto/
├── ralph-core/
├── ralph-adapters/
├── ralph-tui/
├── ralph-cli/
├── ralph-api/
├── ralph-telegram/
├── ralph-bench/
└── ralph-e2e/
```

The workspace uses separate protocol, core, adapter, CLI, TUI, API, benchmarking, and end-to-end testing crates. ([GitHub][8])

`ralph-core` contains the event loop, event parser and journal logic, hat registry, completion handling, context and history management, memory, tasks, merge queues, planning sessions, session recording, workspaces, worktrees, waves, hooks, and steering. `ralph-adapters` contains provider-specific integrations and PTY or protocol support. `ralph-cli` contains commands for runs, planning, loops, MCP, tasks, memory, presets, hooks, diagnostics, waves, and web operation. ([GitHub][9])

The protocol crate defines events, topics, hats, an event bus, JSON-RPC and robot-service messages, and TUI-facing protocol types. ([GitHub][10])

### Runtime logic

The conceptual path is:

```text
configuration
    ↓
hat registry and event topology
    ↓
event loop
    ↓
prompt/context construction
    ↓
backend adapter
    ↓
parsed output events
    ↓
event routing and completion validation
```

The configuration validator detects ambiguous trigger routing and protects reserved lifecycle triggers. It also performs backward-compatible configuration migration. ([GitHub][11])

Ralph has a notably rich termination taxonomy: normal completion, maximum iterations, runtime or cost exhaustion, repeated failures, thrashing, staleness, validation failures, user stop, interrupt, restart, workspace loss, and cancellation are represented separately. ([GitHub][7])

It also detects repeated event signatures as a form of stagnation and can refuse a claimed completion if required events have not occurred, reinserting a resume event instead. ([GitHub][7])

Those two ideas are especially valuable:

1. **Termination causes are typed data, not strings.**
2. **A worker’s “done” claim is subordinate to the run’s completion contract.**

### Backend layer

The current adapter configuration supports numerous coding-agent backends and an automatic selection mode. The important design is not the number of integrations; it is the common boundary between the event loop and each external executor. ([GitHub][11])

Ralph also supports importing an AutoLoop-style preset directory, suggesting convergence around a portable topology-plus-prompts package rather than a hard dependency on one runtime. ([GitHub][12])

### What to copy

Copy:

* the protocol/core/adapters boundary,
* typed termination reasons,
* required-event completion contracts,
* stale and thrash detection,
* backend capability abstraction,
* workspace-loss and interrupted-run handling,
* comprehensive end-to-end testing.

### What not to copy initially

Do not begin with a TUI, web dashboard, Telegram, MCP server, memory system, task system, wave scheduler, merge queue, planning subsystem, and compatibility support for several generations of configuration.

Ralph is a valuable implementation mine, but it is no longer a thin orchestrator.

---

## 2.3 Gas Town: durable work ownership rather than a simple graph loop

### Main idea

Gas Town is best understood as a multi-agent workspace and work-management system. It coordinates coding agents through durable assignments, Git worktrees, a task ledger called Beads, role-specific sessions, convoys of related work, and supervisory or merge-oriented roles. The focus is not one prompt loop; it is preserving work ownership and organizational state across many agent sessions and restarts. ([GitHub][13])

Its role vocabulary includes the Mayor, Deacon, Witness, Refinery, Polecats, Crew, and Dogs. Crew sessions are persistent and human-managed, while Polecats are transient workers associated with branches and supervised integration. Convoys group related work. ([GitHub][14])

The creator describes Gas Town as highly opinionated and explicitly expensive to operate at its intended scale. That admission is important context: it is designed for a very different operational envelope from a small local loop runner. ([Medium][15])

### Repository structure

The Go repository is organized around operational concepts:

```text
internal or package areas:
├── agent/
├── agentlog/
├── beads/
├── checkpoint/
├── convoy/
├── crew/
├── daemon/
├── deacon/
├── dog/
├── events/
├── formula/
├── health/
├── hooks/
├── mail/
├── mayor/
├── polecat/
├── refinery or merge facilities/
├── runtime/
├── session/
├── state/
├── telemetry/
├── tmux/
└── worktree/
```

This is not a generic graph engine with a few adapters. The domain model itself is a virtual organization. ([GitHub][16])

### Runtime logic

The high-level flow is roughly:

1. Work is represented durably in the Beads ledger.
2. A human or coordinator assigns a work item to an agent.
3. Gas Town associates the assignment with a session and normally a Git worktree.
4. The agent executes in that workspace.
5. Supervisory processes track liveness, status, ownership, and follow-up work.
6. Completed branches enter review or merge processing.
7. Attribution and task history remain available after sessions end.

“Hooks” act as durable assignments: an agent can restart and recover what it was meant to do. Git and worktrees supply much of the persistent workspace identity. ([GitHub][14])

The system intentionally uses loose provider coupling through terminal sessions and environment conventions rather than requiring every worker to use one model SDK. That makes it compatible with arbitrary command-line agents, but also ties the runtime to terminal and workspace management. ([GitHub][17])

One documented caveat is that capability-based automatic routing has been described as planned rather than fully implemented; assignments have historically remained explicit. This is a useful reminder to distinguish deployed behavior from roadmap language. ([GitHub][18])

### Gas City: the extracted control-plane architecture

Gas City is strategically more relevant to your design than many of Gas Town’s themed roles.

It extracts a smaller set of primitives:

* Agent
* Bead
* Formula
* Rig
* Pack
* Event

Its architecture uses a TOML configuration, an append-only event bus, a universal work store, provider-backed sessions, prompt templates, and a controller that reconciles desired and actual runtime state. Role behavior is supplied through configuration and Markdown templates rather than being permanently encoded in the orchestration engine. ([GitHub][19])

The event bus records immutable, monotonically ordered events and supports tailing and audit-style observation. The controller composes configuration, sessions, events, work records, and prompts into the orchestration loop. ([GitHub][20])

This is essentially a control-plane pattern:

```text
desired state in city.toml
          ↓
controller/reconciler
          ↓
actual worker sessions
          ↓
immutable event stream
          ↓
updated observed state
```

That becomes useful once your runs must survive the invoking CLI process, maintain worker pools, or reconcile remote executors. It is unnecessary for an initial foreground CLI.

### What to copy

Copy:

* durable assignment identity,
* explicit ownership,
* run and worker attribution,
* recovery after session death,
* optional worktree isolation,
* later, a desired-state reconciler.

### What not to copy initially

Avoid:

* mandatory Git and tmux,
* a separate task database as the core runtime state,
* hard-coded organizational roles,
* multi-repository fleet concepts,
* merge-factory behavior,
* a large terminology layer.

A “planner” should be user configuration on an agent node, not a special species of runtime object.

---

## 2.4 Ruflo: broad meta-harness and an important semantic lesson

### Main idea

Ruflo presents itself as an agent meta-harness with routing, swarms, memory, hooks, MCP tools, commands, background facilities, provider integrations, and many named agent patterns. The current package surface is broad, and the top-level package has been closely tied to the `@claude-flow/cli` ecosystem. The v3 source tree exports areas for security, memory, swarms, integration, shared infrastructure, CLI behavior, neural facilities, testing, performance, and deployment. ([GitHub][21])

Its repository reflects this breadth through root-level plugin, harness, agent, service, data, documentation, test, verification, and versioned implementation areas. ([GitHub][22])

### The most important source-level finding

Ruflo’s own agent guidance distinguishes the orchestration ledger from the executor. Operations such as initializing a swarm or spawning an agent can create coordination records and shared state, while the hosting coding agent remains responsible for writing files and running commands. Merely creating the coordination objects does not necessarily launch independently executing subprocesses. ([GitHub][23])

The correct conclusion is not a simplistic claim that the system is “fake.” The architectural lesson is:

> A tool must define exactly whether “spawn agent” means creating a record, creating a logical role, starting an in-process model loop, or launching an independently supervised OS process.

Your protocol should make these different operations impossible to confuse.

For example:

```text
worker.registered     logical capability record created
worker.started        operating-system process or remote execution started
attempt.started       graph node attempt began
session.attached      existing interactive session attached
```

Never compress these into one vague `agent.spawned` event.

### What to copy

Copy the explicit distinction between orchestration and execution, plus the ideas of capability manifests, hook boundaries, and machine-operated coordination.

### What not to copy

Avoid feature-count-driven architecture. Hundreds of commands, agents, memory modes, coordination patterns, and integrations make it hard for users to determine what is actually executing, what is merely recorded, and what state is authoritative.

Community discussion around Ruflo is polarized. Some users report useful results but high consumption, while others describe the system as convoluted or difficult to verify. One critical audit focused precisely on the distinction between swarm records and independently executing workers. These reports are anecdotal, but they reinforce the need for unambiguous runtime semantics and inspectable process state. ([Reddit][24])

---

## 2.5 Microsoft Conductor: the strongest direct graph-DSL reference

### Main idea

Conductor is a newer Microsoft CLI for defining repeatable multi-agent workflows in a YAML file. Routing is deterministic: expressions and templates select the next step rather than spending an LLM call to decide control flow. It supports agent steps, script steps, value-setting steps, explicit termination, conditional routing, parallel groups, dynamic `for_each`, reusable sub-workflows, human gates, maximum iterations, timeouts, and pre-runtime validation. ([GitHub][25])

That is very close to your graph-authoring goal.

Its strongest design decision is that coordination remains ordinary code:

```text
LLM produces nondeterministic content
orchestrator deterministically decides where it goes next
```

### Repository structure

Conductor is a relatively clean Python package:

```text
src/conductor/
├── cli/
├── config/
├── engine/
├── executor/
├── gates/
├── interrupt/
├── mcp/
├── providers/
├── registry/
├── web/
├── events.py
├── templating.py
└── exceptions.py
```

The separation between configuration, engine, executor, providers, gates, interruptions, and presentation is a good model for a small implementation. ([GitHub][26])

### Difference from your proposed product

Conductor integrates model and agent SDK providers. Your proposed tool is more compelling if it treats existing CLI agents and arbitrary commands as opaque processes with declared capabilities.

That gives you a distinctive positioning:

```text
Conductor:
YAML graph → SDK agent call

Your tool:
TOML/YAML graph → arbitrary external worker protocol
```

### What to copy

Copy:

* one source-controlled workflow definition,
* deterministic routing,
* explicit script and human steps,
* first-class terminal status,
* reusable subgraphs,
* pre-runtime template and dependency validation.

### What to change

Conductor uses Jinja-style expression evaluation. For a small safety-oriented CLI, I would begin with a restricted condition language: event-name matching, existence checks, equality, numeric comparison, and explicit edge priority. A full templating language inside routing conditions increases coercion, debugging, and injection complexity.

---

## 2.6 AWS CLI Agent Orchestrator: supervising real agent processes

### Main idea

AWS CLI Agent Orchestrator, or CAO, runs real coding-agent CLIs in isolated tmux sessions. A supervisor agent can delegate to workers using MCP operations such as synchronous handoff, asynchronous assignment, and message delivery. Humans can attach to the same terminal sessions to inspect or steer the workers. ([GitHub][27])

This directly addresses your “easy to control by human and agent” objective. The supervisor is an agent operator; a person attaching to tmux is a human operator.

### Repository and process architecture

CAO has two primary source packages, one for orchestration and one for workflow behavior. Its documented internal architecture is:

```text
CLI commands / MCP server
             ↓
       FastAPI service
             ↓
 session, terminal, inbox and flow services
             ↓
  tmux client + SQLite client + providers
             ↓
       real agent CLI processes
```

Its service layer includes an event bus, FIFO terminal reader, status monitor, log writer, inbox delivery, session management, terminal management, and scheduled flow execution. Provider modules handle agent-specific prompt and trust behavior. ([GitHub][28])

### Important lesson: terminal scraping is a fallback

The codebase documents provider-specific status detection based partly on prompt symbols and terminal output patterns. This is sometimes unavoidable for interactive CLIs, but it is brittle: a cosmetic CLI update can become a runtime-state bug. ([GitHub][28])

Your backend preference order should therefore be:

1. structured process protocol such as ACP or JSON streaming,
2. documented noninteractive CLI mode,
3. ordinary subprocess with explicit exit status,
4. PTY plus terminal-state heuristics as the final fallback.

PTY support belongs in an adapter, never in the scheduler.

### What to copy

Copy:

* real process/session semantics,
* provider-specific adapters,
* inbox and steering behavior,
* attachability,
* supervisor and human using the same worker session,
* explicit synchronous versus asynchronous delegation.

### What not to copy initially

Do not make tmux, FastAPI, SQLite, MCP, a global service, and a supervisor-worker hierarchy mandatory.

A run-local process and local control socket can provide most of the important behavior with far less infrastructure.

---

## 2.7 Bernstein: deterministic scheduling and audit-grade ideas

Bernstein is a newer deterministic orchestrator for external coding-agent CLIs. Its project documentation describes plain-Python scheduling, per-task worktrees, lint/type/test gates, replay journals, artifact lineage, and optional HMAC- or Merkle-style tamper evidence. ([GitHub][29])

Its repository is extensive, including adapters, agents, bridges, compliance, evaluation, dashboards, plugins, MCP, GitHub and GitLab applications, TUI components, specification-driven-development facilities, and editor packages. ([GitHub][29])

The useful lesson is to record provenance:

* workflow specification hash,
* prompt hashes,
* Git revision,
* backend identity and version,
* attempt identifiers,
* artifact content hashes,
* gate results.

Do not begin with a cryptographically chained audit system unless your threat model actually includes malicious log modification. A simple checksummed append-only journal and content-addressed artifacts will cover debugging, reproducibility, and accidental corruption.

Also distinguish two meanings of replay:

* **State replay:** reconstruct the run state from recorded events.
* **Execution replay:** invoke the model again and expect the same output.

The first can be deterministic. The second generally cannot, because the model, provider, hidden context handling, and external environment may all be nondeterministic.

---

# 3. Adjacent graph frameworks

## LangGraph

LangGraph is a low-level runtime for stateful agents with checkpoints, durable execution, streaming, human interruption, and persistent thread state. Its `interrupt()` mechanism saves graph state, waits for input, and resumes through a command when a checkpointer and thread identifier are supplied. Its documentation also warns that side effects before an interrupt must be idempotent because execution can resume or repeat around that boundary. ([Docs by LangChain][30])

The transferable concepts are checkpoints, interrupts, resumable human gates, and explicit thread identity.

The unsuitable part is making a mutable application-state object the primary runtime abstraction. For an external-agent CLI, immutable events and referenced artifacts are easier to inspect and recover.

## Pydantic Graph

Pydantic Graph provides typed state-machine and graph construction with steps, decisions, broadcasting, joins, reducers, and step-by-step iteration. Its own documentation cautions that graph machinery may be unnecessary for simple cases. ([Pydantic Docs][31])

Use it as a reference for:

* typed node inputs and outputs,
* builder validation,
* join and reducer semantics,
* clear distinction between a step and a decision.

You do not need to reproduce its Python type system. JSON Schema at external boundaries and internal strongly typed structures are enough.

## Mastra

Mastra offers a useful workflow vocabulary: sequence, parallel, branch, map, loops, bounded iteration, shared workflow state, nested workflows, suspend/resume, and time-travel-style re-execution. ([Mastra][32])

The best idea to borrow is allowing several convenient authoring forms while compiling all of them to one normalized graph IR.

For example:

```text
sequence syntax
parallel syntax
branch syntax
repeat syntax
```

should all compile to:

```text
nodes + typed edges + explicit joins + explicit cycle limits
```

The scheduler should only understand the normalized representation.

## Microsoft Agent Framework

Microsoft Agent Framework explicitly separates agents from workflows and frames orchestration around who decides the next action: the model, deterministic code, or a human. It supports typed graph construction, events, checkpoints, human interaction, time travel, and observability. Its guidance recommends choosing the simplest sufficient decision-maker. ([Microsoft Learn][33])

That yields a useful node taxonomy for your tool:

```text
agent node       model or coding-agent worker decides content
command node     deterministic executable decides success/failure
router node      deterministic data expression selects an edge
human node       person supplies approval or input
join node        deterministic synchronization rule
```

## CrewAI and OpenAI Agents SDK

CrewAI’s distinction between autonomous “Crews” and precisely controlled “Flows” reinforces the idea that role-playing collaboration and deterministic workflow execution should not be the same runtime primitive. ([GitHub][34])

The OpenAI Agents SDK is a useful reference for lightweight handoffs, guardrails, sessions, interruptions, and tracing of generations, tool calls, handoffs, and custom events. It is an in-process agent library rather than an external CLI supervisor. ([GitHub][35])

## Temporal

Temporal provides the deepest production reference for separating deterministic orchestration from nondeterministic effects. Workflows derive state from an event history; external activities perform side effects; replay reconstructs execution after crashes. ([Temporal][36])

You should borrow the principle, not the platform:

```text
pure reducer and scheduler
        +
recorded effect requests and results
        +
external agent/command execution
```

Temporal itself would be excessive for a local CLI MVP.

---

# 4. What the original Ralph material actually teaches

The popular caricature of Ralph is:

```bash
while true; do
  agent < PROMPT.md
done
```

The original material describes a more disciplined method.

The creator emphasizes small increments, fresh context, specifications in files, tests and static analysis as backpressure, a maintained plan or fix list, Git checkpoints, and frequent correction when the process drifts. He identifies nondeterminism as the core weakness and is notably cautious about applying unconstrained Ralph loops to existing codebases. ([Geoffrey Huntley][37])

The HumanLayer historical analysis reaches the same conclusion: the useful part is not merely blocking an agent from stopping. It is dividing work into small independent context windows with a clear end state. Poor specifications, ambiguous completion, exploratory tasks, and large batches of changes cause the method to deteriorate. ([HumanLayer][38])

This implies two defaults for your runtime:

1. **Fresh worker context per attempt should be the default.**
2. **Persistent or resumable sessions should be an explicit backend capability and node policy.**

A graph node should not silently inherit an enormous prior conversation merely because a provider happens to support session continuation.

---

# 5. What broader agent-engineering guidance says

Anthropic distinguishes workflows—where code determines the path—from agents—where a model dynamically determines its actions. Its recommendation is to start with the simplest architecture and add complexity only where it improves outcomes. It identifies orchestrator-worker and evaluator-optimizer patterns as useful when task decomposition or refinement genuinely requires them, while stressing stopping conditions, checkpoints, and grounded evaluation. ([Anthropic][39])

Cognition argues that multi-agent systems lose information when agents exchange only summaries; actions and complete trajectories contain decisions that may not appear in natural-language messages. It also reports that a linear single-threaded agent often goes farther than designers expect. ([cognition.ai][40])

For your design, this means downstream workers should receive concrete artifacts—diffs, test output, plans, event payloads, file references—not merely an LLM-generated summary of what another worker supposedly did.

Anthropic’s later harness-design discussion adds another useful rule: every harness component encodes an assumption about a model limitation, and scaffolding can become stale as models improve. Components should therefore be removable and evaluated independently. ([Anthropic][41])

That supports a plugin-light core. Planning, reviewing, summarizing, memory retrieval, and model-based evaluation should be ordinary configurable graph nodes, not permanent scheduler behavior.

---

# 6. Reddit and practitioner reports

Reddit evidence is self-selected and cannot establish comparative performance. It is nevertheless useful for identifying repeated usability complaints.

### Ralph-style loops

Users commonly report that the first few review or repair iterations are productive, after which returns diminish sharply. Reports of runaway loops, redundant implementations, and large token consumption consistently recommend hard caps, tests, linting, types, and narrowly scoped tasks. ([Reddit][42])

### Gas Town

The most common complaints are cognitive overhead, maintenance of the orchestration system itself, and token expenditure. Positive reports often focus on the durable Beads task model or on coordinating only one or two agents rather than operating a large autonomous town. Other users report that agent-authored task records need validation because low-quality ledger entries can propagate confusion. ([Reddit][43])

### LangGraph

Discussion is polarized in a predictable way: users with simple workflows experience the framework as heavy, while users with complicated branching, retries, state inspection, and deployment requirements value its explicit graph and persistence model. ([Reddit][44])

### General conclusion

The cross-project message is consistent:

> Orchestration starts paying for itself only when it makes failures, state, retries, and human intervention more legible than the raw agent sessions were.

A graph that merely adds terminology and prompts but does not improve observability, recoverability, or boundedness is negative value.

---

# 7. Recommended product definition

I would define the product as:

> **A local, deterministic graph runner and control protocol for opaque agent processes.**

It is not:

* an LLM SDK,
* a coding agent,
* a memory or RAG system,
* an issue tracker,
* a multi-agent role-playing framework,
* a general cloud workflow platform,
* an IDE.

The orchestrator should own only:

1. Parsing and validating graph definitions.
2. Deterministic routing and scheduling.
3. Worker process or session lifecycle.
4. Durable event recording and recovery.
5. Limits, gates, retries, and cancellation.
6. Human and agent control commands.
7. Optional workspace isolation.

The worker should own:

* reasoning,
* model interaction,
* tool calls,
* code editing,
* context compaction,
* domain-specific planning,
* provider authentication.

This keeps model and provider churn outside your core.

---

# 8. Proposed architecture

```text
 loop.toml + prompts + scripts
               │
               ▼
        parser and compiler
               │
               ▼
       normalized Graph IR
               │
               ▼
         static validator
               │
               ▼
    deterministic reducer/scheduler
          │              │
          │              └──────── operator commands
          │                         human CLI / agent CLI
          ▼
       backend adapter
          │
          ▼
 external CLI / command / session
          │
          ▼
      structured worker events
          │
          ▼
 append-only run journal
          │
          ├── status projection
          ├── graph projection
          ├── logs and artifacts
          └── metrics and diagnostics
```

## 8.1 Keep five state domains separate

Many large orchestrators become complicated because they mix several unrelated meanings of “state.”

Your implementation should distinguish:

### Definition state

The graph file, prompt files, schemas, and scripts. This is version-controlled input.

### Run history

The immutable sequence of lifecycle, worker, gate, and operator events.

### Derived run projection

Current node states, ready queue, remaining budget, active attempts, and pending human requests. It must be rebuildable from the journal.

### Workspace state

The mutable filesystem, Git worktree, container, or remote sandbox in which a worker operates.

### Long-term memory

Optional user or agent knowledge across runs. This should not be part of the first core runtime.

Do not put all five into one mutable database model.

---

## 8.2 Graph model

A loop should not be a separate execution engine. It is simply a directed graph containing a cycle.

Use these core entities:

```text
Graph
Node
Edge
Run
Attempt
Event
Artifact
Operator
Backend
```

A node should have a stable ID, kind, input contract, output events, backend requirements, timeout, retry policy, permissions, and workspace policy.

Recommended node kinds:

```text
agent       invoke an external nondeterministic worker
command     invoke a deterministic executable or script
router      evaluate a restricted deterministic condition
human       request approval or input
join        wait for all, any, or a quorum of predecessor results
subgraph    later: invoke a reusable graph definition
```

Do not make `planner`, `reviewer`, `mayor`, `researcher`, or `developer` runtime node kinds. Those are role metadata and prompt configuration on an `agent` node.

### Cycles

Detect strongly connected components during validation. Every cycle must have an explicit bound, such as:

* maximum visits,
* maximum attempts,
* maximum elapsed time,
* maximum cost,
* or an exit event that is itself backed by a run-level hard limit.

An unbounded cycle should be a validation error, not a warning.

### Dynamic fan-out

Defer dynamic fan-out until the sequential engine is trustworthy. When introduced, require:

* maximum item count,
* maximum concurrency,
* explicit join policy,
* cancellation policy,
* deterministic item identity.

---

## 8.3 Event model

Use a versioned envelope:

```json
{
  "schema_version": 1,
  "seq": 42,
  "timestamp": "2026-07-19T13:22:04.120Z",
  "run_id": "run_01K...",
  "node_id": "review",
  "attempt_id": "att_01K...",
  "actor": {
    "kind": "agent",
    "id": "claude-reviewer"
  },
  "type": "review.changes_requested",
  "payload": {
    "artifact": "artifacts/review.md"
  },
  "causation_id": "evt_01K...",
  "correlation_id": "run_01K..."
}
```

Useful lifecycle events include:

```text
run.created
run.started
run.paused
run.resumed
run.completed
run.failed
run.cancelled

node.ready
attempt.scheduled
attempt.started
attempt.output
attempt.interrupted
attempt.succeeded
attempt.failed

worker.event
worker.exited
artifact.created

gate.passed
gate.failed
gate.overridden

human.requested
human.responded

operator.command_received
operator.command_rejected
operator.command_applied

budget.warning
budget.exhausted
stall.detected
```

Events should have JSON Schema-validated payloads. Gas City’s append-only event substrate is a good model, but its documented caveats around event schema validation and linear scanning are exactly what you should address: validate at ingestion, and build disposable query projections rather than scanning an ever-growing global file. ([GitHub][20])

---

## 8.4 Persistence

For the first implementation, use one run directory:

```text
.agentloop/
└── runs/
    └── run_01K.../
        ├── manifest.json
        ├── events.jsonl
        ├── snapshot.json
        ├── artifacts/
        ├── transcripts/
        ├── control.json
        └── run.lock
```

`manifest.json` should record:

* graph and prompt hashes,
* source-control revision,
* backend versions,
* initial inputs,
* environment metadata,
* creation time,
* schema versions.

`events.jsonl` is authoritative.

`snapshot.json` is a replaceable optimization. Write it atomically and rebuild it when missing or invalid.

Use a single journal writer. Flush accepted operator commands and important state transitions. Tolerate a torn final JSONL record after a crash. Assign monotonic sequence numbers inside the runner process.

At larger scale, create an optional SQLite projection for querying runs. It must be deletable and rebuildable from journals; SQLite must not become a second source of truth.

---

## 8.5 Pure reducer and effect boundary

The heart of the runtime should be a deterministic reducer:

```text
new_state = reduce(old_state, event)
```

The scheduler derives ready work from the reduced state and graph:

```text
ready_attempts = schedule(graph, state, budgets)
```

External execution is an effect:

1. Append `attempt.scheduled`.
2. Start the worker with an immutable attempt ID and input snapshot.
3. Append `attempt.started`.
4. Stream output and structured events.
5. Append the terminal attempt event.
6. Reduce state and schedule the next work.

On a crash, replay the journal. Any attempt that started but has no terminal event becomes `interrupted` or `orphaned`. The backend may reattach only if it explicitly supports reattachment and can prove the target session identity. Otherwise, require a new attempt.

Do not silently rerun side-effecting commands.

---

## 8.6 Backend contract

The core must not know Claude-, Codex-, Gemini-, or OpenCode-specific details.

A conceptual interface:

```go
type Backend interface {
    Capabilities(ctx context.Context) (Capabilities, error)
    Start(ctx context.Context, invocation Invocation) (Handle, error)
    Events(ctx context.Context, handle Handle) (<-chan WorkerEvent, error)
    Control(ctx context.Context, handle Handle, signal ControlSignal) error
    Wait(ctx context.Context, handle Handle) (Result, error)
}
```

A capability manifest should report:

```text
structured_events
streaming_output
fresh_sessions
session_resume
live_steering
graceful_cancel
cost_reporting
token_reporting
tool_policy
read_only_mode
pty_required
workspace_isolation
reattach
```

The graph validator can then reject incompatible definitions. For example, a node that requires live steering cannot use a backend that supports only one-shot execution.

Begin with:

1. Generic subprocess backend.
2. Mock backend for deterministic testing.
3. One structured coding-agent adapter.
4. PTY adapter only after the core is stable.

---

## 8.7 Structured agent control

Do not derive graph events by searching arbitrary model prose for phrases such as `LOOP_COMPLETE`.

Inject a scoped control endpoint into each worker:

```text
AGENTLOOP_RUN_ID
AGENTLOOP_NODE_ID
AGENTLOOP_ATTEMPT_ID
AGENTLOOP_CONTROL_ENDPOINT
AGENTLOOP_CONTROL_TOKEN
```

The worker can then execute:

```bash
loop emit plan.ready \
  --payload '{"artifact":"artifacts/plan.md"}'
```

or:

```bash
loop emit implementation.blocked \
  --payload-file blocked.json
```

The token should restrict the worker to events declared by its node. A reviewer should not automatically have permission to cancel the entire run or emit an administrator override.

Stdout and stderr remain logs. Structured control uses a side channel.

---

## 8.8 Human and agent control should be identical underneath

A run process can expose a run-local Unix socket or Windows named pipe. This avoids requiring a global daemon.

Both a human CLI and an agent-invoked CLI send the same versioned commands:

```text
status
watch
pause
resume
step
cancel
retry
skip
emit
approve
reject
respond
inspect
```

The runner validates the command, appends an operator event, and returns a structured acknowledgment.

Human commands:

```bash
loop status run_01K...
loop watch run_01K...
loop pause run_01K...
loop step run_01K...
loop approve run_01K... req_01K...
```

Agent-friendly forms:

```bash
loop status run_01K... --json
loop watch run_01K... --since 104 --jsonl
loop capabilities --json
loop validate loop.toml --json
```

Machine-output requirements should be non-negotiable:

* versioned JSON schemas,
* stable exit codes,
* stdout only for requested data,
* diagnostics on stderr,
* `--no-color`,
* idempotent request IDs,
* no interactive prompts in machine mode,
* explicit “not supported” capability responses.

MCP can later wrap this control protocol. It should not become a separate implementation of orchestration logic.

---

## 8.9 Human-control semantics

Define control operations precisely.

### Pause

Stop scheduling new attempts. The current attempt follows an explicit policy:

```text
drain       allow it to finish
interrupt   request graceful cancellation
kill        force termination after escalation
```

### Step

Schedule exactly one currently ready attempt, wait for its terminal event, and return to paused state.

This is one of the most valuable human-control features for debugging a graph.

### Retry

Create a new attempt linked to the failed attempt. Require the caller to choose whether it uses:

* the original workspace snapshot,
* the current workspace,
* or a clean workspace.

### Skip

Append an explicit skip event and require an outgoing edge that handles it. Never pretend that skipping means success.

### Override

Allow a human to override a failed gate only with an actor identity and reason. Preserve both the original failure and the override.

---

## 8.10 Completion and evidence

Completion should have three layers:

```text
worker claim
    ↓
graph reaches terminal path
    ↓
completion contract passes
```

A worker may emit `implementation.ready`, but it should not directly make the whole run successful.

A completion contract can require:

* test gate passed,
* lint gate passed,
* type-check gate passed,
* required artifacts exist,
* reviewer event observed,
* no unresolved human request,
* workspace is clean or has an expected commit,
* budget has not failed closed.

This generalizes Ralph’s required-event completion logic and AutoLoop’s typed evidence gates. ([GitHub][7])

Deterministic checks should carry more authority than model assertions. “Reviewer agent approved” is evidence, but weaker evidence than executable tests.

---

## 8.11 Budgets and anti-thrashing

Support layered limits:

```text
maximum total attempts
maximum attempts per node
maximum visits per cycle
maximum wall time
maximum attempt time
maximum concurrent workers
maximum dynamic fan-out
maximum consecutive failures
maximum cost or tokens when reliably reported
maximum repeated progress signature
```

A progress signature can combine:

```text
emitted event type
workspace tree or diff hash
relevant artifact hashes
gate-result hash
```

Repeated prose alone is a poor stagnation detector. A worker may produce different wording while making no changes, or similar wording while legitimately progressing.

Default to fail-closed when a budget is exhausted. A human may extend a budget through a recorded operator command.

---

## 8.12 Workspace and security model

Support workspace policies as adapters:

```text
shared       current directory
worktree     isolated Git worktree
copy         copied directory
container    sandboxed container
remote       remote worker workspace
```

Do not make worktrees mandatory. They are useful isolation from other branches, but they are not a security sandbox: a process in a worktree can still read home-directory files, secrets, the network, and other paths.

Security policy should include:

* environment-variable allowlists,
* secret filtering,
* working-directory boundaries,
* read-only nodes,
* command allowlists or approval gates,
* network policy where sandboxing exists,
* explicit destructive-operation approval,
* graceful-to-forceful process termination.

A reviewer node can be read-only by policy rather than merely being told in a prompt not to edit files.

---

# 9. Proposed authoring format

I recommend **TOML plus Markdown prompts**, compiled into canonical JSON internally.

TOML is less permissive and surprising than YAML, produces clean diffs, and is already validated by the AutoLoop ecosystem. YAML support can be added later through the same Graph IR.

Example:

```toml
version = 1
name = "implement-review"

[run]
entry = "plan"
terminal_events = ["run.completed"]
max_attempts = 16
max_cycle_visits = 3
timeout = "45m"

[nodes.plan]
kind = "agent"
backend = "claude"
prompt_file = "prompts/plan.md"
session = "fresh"
emits = ["plan.ready", "input.required"]

[nodes.implement]
kind = "agent"
backend = "codex"
prompt_file = "prompts/implement.md"
session = "fresh"
workspace = "worktree"
emits = ["implementation.ready", "implementation.blocked"]

[nodes.verify]
kind = "command"
commands = [
  "npm test",
  "npm run lint",
  "npm run typecheck"
]
success_event = "verification.passed"
failure_event = "verification.failed"
timeout = "10m"

[nodes.review]
kind = "agent"
backend = "claude"
prompt_file = "prompts/review.md"
permissions = "read-only"
emits = ["review.approved", "review.changes_requested"]

[nodes.release]
kind = "human"
message = "Implementation is verified and approved. Merge?"
emits = ["run.completed", "review.changes_requested"]
timeout = "never"

[[edges]]
from = "plan"
on = "plan.ready"
to = "implement"

[[edges]]
from = "implement"
on = "implementation.ready"
to = "verify"

[[edges]]
from = "verify"
on = "verification.passed"
to = "review"

[[edges]]
from = "verify"
on = "verification.failed"
to = "implement"

[[edges]]
from = "review"
on = "review.approved"
to = "release"

[[edges]]
from = "review"
on = "review.changes_requested"
to = "implement"

[[edges]]
from = "release"
on = "review.changes_requested"
to = "implement"
```

This expresses a Ralph-style repair cycle without giving the model control over routing.

The directory can remain simple:

```text
implement-review/
├── loop.toml
├── prompts/
│   ├── plan.md
│   ├── implement.md
│   └── review.md
├── schemas/
└── scripts/
```

All imported files should be resolved and hashed before a run starts. The active graph should be immutable for that run.

An agent may propose a modified graph as an artifact, but it should not silently rewrite the active topology. Runtime graph patching can be added later as a versioned, journaled, policy-controlled operation.

---

# 10. Graph-design CLI

Files should remain the canonical authoring interface because they are easy for humans, agents, Git, review tools, and CI.

The CLI can provide editing conveniences that modify those files rather than introducing a hidden graph database:

```bash
loop init implement-review
loop add agent plan --prompt prompts/plan.md
loop add command verify --run "npm test"
loop connect plan plan.ready implement
loop fmt
loop validate
loop graph --format mermaid
```

The most important design commands are:

```bash
loop validate loop.toml
loop explain loop.toml
loop graph loop.toml
loop simulate loop.toml --events fixture.jsonl
```

`validate` should return source-located diagnostics:

```text
E103 unbounded cycle

  implement → verify → review → implement

Every cycle requires max_cycle_visits or a cycle-specific bound.
  at loop.toml:46
```

`explain` should answer questions such as:

```text
Why is review blocked?
Why did this edge win?
Which required evidence is missing?
Which budget stopped this attempt?
Which backend capability is unsupported?
```

This type of transparency is more valuable than an early graphical editor.

---

# 11. Recommended repository structure

For a genuinely thin initial implementation, I would use **Go with one module and one binary**. Go gives straightforward subprocess control, concurrency, static binaries, fast compilation, and a small deployment footprint. Rust is equally credible when stronger type-level guarantees and advanced PTY handling justify the development cost; Ralph demonstrates that structure well.

A Go repository could be:

```text
cmd/
└── loop/
    └── main.go

internal/
├── spec/
│   ├── parse.go
│   ├── migrate.go
│   ├── schema.go
│   └── validate.go
├── graph/
│   ├── ir.go
│   ├── cycles.go
│   ├── routing.go
│   └── joins.go
├── engine/
│   ├── reducer.go
│   ├── scheduler.go
│   ├── effects.go
│   ├── reconcile.go
│   └── budgets.go
├── journal/
│   ├── writer.go
│   ├── reader.go
│   ├── replay.go
│   ├── repair.go
│   └── snapshot.go
├── backend/
│   ├── backend.go
│   ├── capabilities.go
│   ├── process/
│   ├── mock/
│   └── adapters/
├── control/
│   ├── server.go
│   ├── client.go
│   ├── commands.go
│   └── authorization.go
├── workspace/
│   ├── workspace.go
│   ├── shared.go
│   └── worktree.go
├── artifact/
│   ├── store.go
│   └── hash.go
├── projection/
│   ├── status.go
│   ├── graph.go
│   └── metrics.go
└── cli/
    ├── root.go
    ├── run.go
    ├── inspect.go
    ├── control.go
    ├── text.go
    └── json.go

pkg/
└── protocol/
    ├── event.go
    ├── command.go
    └── capability.go

schemas/
├── loop.schema.json
├── event.schema.json
└── protocol.schema.json

examples/
├── ralph-loop/
├── plan-build-review/
└── human-approval/

testkit/
├── mockworker/
├── fixtures/
└── crashrunner/
```

Only `pkg/protocol` should be a promised public Go API. Keep everything else under `internal` until real extension requirements appear.

The core dependency direction should remain:

```text
protocol
   ↑
graph + journal
   ↑
engine
   ↑
backends + control
   ↑
CLI
```

The graph engine must not import provider adapters, terminal rendering, GitHub integrations, MCP, or model SDKs.

---

# 12. Testing strategy

The scheduler itself should be testable without any model.

### Pure tests

Test:

* graph reachability,
* ambiguous routing,
* cycle-bound enforcement,
* join behavior,
* budget transitions,
* event reduction,
* completion contracts,
* operator-command authorization.

### Property tests

Generate graph and event combinations and verify invariants:

```text
a terminal run never schedules new work
sequence numbers never decrease
one attempt has at most one terminal state
a blocked gate cannot become passed without evidence or override
replay yields the same projection
unbounded cycles are rejected
```

### Backend contract suite

Every backend adapter should pass the same tests for:

* startup,
* output streaming,
* cancellation,
* timeout,
* process death,
* unsupported capabilities,
* session reattachment where claimed.

### Crash tests

Terminate the runner:

* during journal append,
* after scheduling but before process creation,
* after process creation but before `attempt.started`,
* while a worker is running,
* while writing a snapshot,
* while accepting a human command.

Then verify replay and orphan handling.

### End-to-end tests

Use deterministic mock workers that emit prescribed events. Do not rely on paid model calls for basic runtime correctness.

### Agent evaluations

For actual agent quality, distinguish trajectory from outcome. Track:

* task success,
* deterministic gate success,
* wall time,
* cost and tokens where available,
* number of attempts,
* cycle visits,
* human interventions,
* retries,
* workspace conflicts,
* false completion claims.

Run multiple trials because model behavior varies. Anthropic’s evaluation guidance similarly distinguishes the agent harness from the evaluation harness and emphasizes outcome-oriented graders rather than accepting the agent’s own declaration of success. ([Anthropic][45])

The critical baseline is not another orchestrator. It is:

> **One competent agent, one clear task, no orchestration.**

Your graph should justify itself by improving success, recoverability, cost control, or human supervision over that baseline.

---

# 13. Recommended MVP boundary

A credible first release needs only:

```text
one binary
one TOML graph format
one canonical Graph IR
agent, command and human nodes
static edges and bounded cycles
sequential execution
generic subprocess backend
one real coding-agent adapter
append-only JSONL journal
validate, graph, run, status and watch
pause, resume, step, emit and cancel
hard attempt and time limits
structured JSON output
deterministic mock backend
crash/replay tests
```

Parallel fan-out, joins, and worktrees can follow once sequential recovery is reliable.

The following should remain outside the first release:

```text
vector memory
RAG
issue tracker synchronization
Kanban
agent marketplace
hard-coded organizational roles
global daemon
web dashboard
Telegram or Slack
remote worker fleet
automatic merge queue
model-based scheduler
automatic graph mutation
MCP as the primary protocol
cryptographic audit chains
```

A TUI can eventually render the same event projection as `loop watch`. An MCP server can eventually proxy the same control commands. A daemon can eventually reconcile detached or remote runs. None should define separate state or execution logic.

---

# 14. Final relevance ranking

For your particular idea, ranked by architectural relevance rather than GitHub stars:

1. **AutoLoop** — closest overall product shape; strongest journal and human/agent CLI ideas.
2. **Microsoft Conductor** — strongest clean workflow-definition and deterministic-routing reference.
3. **Ralph Orchestrator** — deepest source for completion contracts, termination taxonomy, adapters, and anti-thrashing.
4. **AWS CLI Agent Orchestrator** — best reference for real CLI sessions, steering, inboxes, and provider-specific process supervision.
5. **Gas City** — strongest future reference for a persistent controller, event substrate, and desired-state reconciliation.
6. **Gas Town** — strongest durable assignment, worktree ownership, attribution, and fleet-operation lessons.
7. **Bernstein** — useful provenance, verification, journal, and audit concepts; substantially broader than a thin CLI.
8. **LangGraph, Microsoft Agent Framework, Pydantic Graph, and Mastra** — excellent graph semantics and human-interrupt references, but a different runtime boundary.
9. **Ruflo** — useful capability and ledger/executor ideas, plus a strong warning about feature surface and ambiguous worker semantics.

The concise design thesis is:

> **Make the graph deterministic, the workers replaceable, the journal authoritative, every loop bounded, completion evidence-based, and every human or agent intervention an explicit protocol event.**

That combination is narrower than the large orchestrators, but materially more controllable than a shell loop—and it is a defensible product boundary rather than the beginning of another agent platform.

[1]: https://github.com/mikeyobrien/autoloop "https://github.com/mikeyobrien/autoloop"
[2]: https://github.com/mikeyobrien/autoloop/tree/main/packages "https://github.com/mikeyobrien/autoloop/tree/main/packages"
[3]: https://raw.githubusercontent.com/mikeyobrien/autoloop/main/packages/core/src/topology.ts "https://raw.githubusercontent.com/mikeyobrien/autoloop/main/packages/core/src/topology.ts"
[4]: https://raw.githubusercontent.com/mikeyobrien/autoloop/main/packages/harness/src/iteration.ts "https://raw.githubusercontent.com/mikeyobrien/autoloop/main/packages/harness/src/iteration.ts"
[5]: https://raw.githubusercontent.com/mikeyobrien/autoloop/main/packages/core/src/journal.ts "https://raw.githubusercontent.com/mikeyobrien/autoloop/main/packages/core/src/journal.ts"
[6]: https://github.com/mikeyobrien/ralph-orchestrator "https://github.com/mikeyobrien/ralph-orchestrator"
[7]: https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/crates/ralph-core/src/event_loop/mod.rs "https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/crates/ralph-core/src/event_loop/mod.rs"
[8]: https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/Cargo.toml "https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/Cargo.toml"
[9]: https://github.com/mikeyobrien/ralph-orchestrator/tree/main/crates/ralph-core/src "https://github.com/mikeyobrien/ralph-orchestrator/tree/main/crates/ralph-core/src"
[10]: https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/crates/ralph-proto/src/lib.rs "https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/crates/ralph-proto/src/lib.rs"
[11]: https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/crates/ralph-core/src/config.rs "https://raw.githubusercontent.com/mikeyobrien/ralph-orchestrator/main/crates/ralph-core/src/config.rs"
[12]: https://github.com/mikeyobrien/ralph-orchestrator/tree/main/presets "https://github.com/mikeyobrien/ralph-orchestrator/tree/main/presets"
[13]: https://github.com/gastownhall/gastown "https://github.com/gastownhall/gastown"
[14]: https://github.com/gastownhall/gastown/blob/main/docs/overview.md "https://github.com/gastownhall/gastown/blob/main/docs/overview.md"
[15]: https://steve-yegge.medium.com/welcome-to-gas-town-4f25ee16dd04 "https://steve-yegge.medium.com/welcome-to-gas-town-4f25ee16dd04"
[16]: https://github.com/gastownhall/gastown/tree/main/internal "https://github.com/gastownhall/gastown/tree/main/internal"
[17]: https://github.com/gastownhall/gastown/blob/main/docs/agent-provider-integration.md "https://github.com/gastownhall/gastown/blob/main/docs/agent-provider-integration.md"
[18]: https://github.com/gastownhall/gastown/blob/main/docs/why-these-features.md "https://github.com/gastownhall/gastown/blob/main/docs/why-these-features.md"
[19]: https://github.com/gastownhall/gascity/blob/main/engdocs/architecture/glossary.md "https://github.com/gastownhall/gascity/blob/main/engdocs/architecture/glossary.md"
[20]: https://github.com/gastownhall/gascity/blob/main/engdocs/architecture/event-bus.md "https://github.com/gastownhall/gascity/blob/main/engdocs/architecture/event-bus.md"
[21]: https://github.com/ruvnet/ruflo/tree/main/ruflo "https://github.com/ruvnet/ruflo/tree/main/ruflo"
[22]: https://github.com/ruvnet/ruflo "https://github.com/ruvnet/ruflo"
[23]: https://github.com/ruvnet/ruflo/blob/main/AGENTS.md "https://github.com/ruvnet/ruflo/blob/main/AGENTS.md"
[24]: https://www.reddit.com/r/ClaudeAI/comments/1sckiy8/do_not_install_ruflo_into_your_claude_code/ "https://www.reddit.com/r/ClaudeAI/comments/1sckiy8/do_not_install_ruflo_into_your_claude_code/"
[25]: https://github.com/microsoft/conductor "https://github.com/microsoft/conductor"
[26]: https://github.com/microsoft/conductor/tree/main/src/conductor "https://github.com/microsoft/conductor/tree/main/src/conductor"
[27]: https://github.com/awslabs/cli-agent-orchestrator "https://github.com/awslabs/cli-agent-orchestrator"
[28]: https://github.com/awslabs/cli-agent-orchestrator/blob/main/CODEBASE.md "https://github.com/awslabs/cli-agent-orchestrator/blob/main/CODEBASE.md"
[29]: https://github.com/sipyourdrink-ltd/bernstein "https://github.com/sipyourdrink-ltd/bernstein"
[30]: https://docs.langchain.com/oss/python/langgraph/overview "https://docs.langchain.com/oss/python/langgraph/overview"
[31]: https://ai.pydantic.dev/graph/ "https://ai.pydantic.dev/graph/"
[32]: https://mastra.ai/docs/workflows/control-flow "https://mastra.ai/docs/workflows/control-flow"
[33]: https://learn.microsoft.com/en-us/agent-framework/workflows/ "https://learn.microsoft.com/en-us/agent-framework/workflows/"
[34]: https://github.com/crewaiinc/crewai "https://github.com/crewaiinc/crewai"
[35]: https://github.com/openai/openai-agents-python "https://github.com/openai/openai-agents-python"
[36]: https://temporal.io/blog/of-course-you-can-build-dynamic-ai-agents-with-temporal "https://temporal.io/blog/of-course-you-can-build-dynamic-ai-agents-with-temporal"
[37]: https://ghuntley.com/ralph/ "https://ghuntley.com/ralph/"
[38]: https://www.humanlayer.dev/blog/brief-history-of-ralph "https://www.humanlayer.dev/blog/brief-history-of-ralph"
[39]: https://www.anthropic.com/engineering/building-effective-agents "https://www.anthropic.com/engineering/building-effective-agents"
[40]: https://cognition.ai/blog/dont-build-multi-agents "https://cognition.ai/blog/dont-build-multi-agents"
[41]: https://www.anthropic.com/engineering/harness-design-long-running-apps "https://www.anthropic.com/engineering/harness-design-long-running-apps"
[42]: https://www.reddit.com/r/ClaudeCode/comments/1to6hen/ralph_loop_overnight_91_codex_reviews_200_gone/ "https://www.reddit.com/r/ClaudeCode/comments/1to6hen/ralph_loop_overnight_91_codex_reviews_200_gone/"
[43]: https://www.reddit.com/r/ClaudeCode/comments/1qur3qq/spent_2_weeks_running_multiple_claude_code_agents/ "https://www.reddit.com/r/ClaudeCode/comments/1qur3qq/spent_2_weeks_running_multiple_claude_code_agents/"
[44]: https://www.reddit.com/r/LangChain/comments/1sgahzh/anyone_actually_enjoying_langgraph_for_simple/ "https://www.reddit.com/r/LangChain/comments/1sgahzh/anyone_actually_enjoying_langgraph_for_simple/"
[45]: https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents "https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents"

