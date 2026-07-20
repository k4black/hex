# hex

**A thin, deterministic control plane for agentic loops and graphs.**

`hex` compiles a human-readable graph, runs existing agent CLIs (Claude Code,
Codex, Gemini, …) as opaque workers, records every transition in an append-only
journal, enforces hard limits and evidence gates, and exposes the *same*
control protocol to humans and agents.

It is deliberately narrow. The loop itself can be one shell line; the value is
a small, legible kernel that makes nondeterministic agents **programmable,
interruptible, resumable, verifiable, and easy to operate** — from a terminal
or from another agent. Mental model: *Git-style local control for agent runs,
not Kubernetes for agents.*

> Make the graph deterministic, the workers replaceable, the journal
> authoritative, every loop bounded, completion evidence-based, and every human
> or agent intervention an explicit protocol event.

## What hex is not

An LLM/provider SDK · a coding agent · a memory/RAG system · an issue tracker ·
a multi-agent role-playing framework · a cloud workflow platform · a required
daemon. These may become optional adapters later; they never enter the kernel's
domain model.

## Architecture

One-liner: **kernel decides · worker runs one agent · runtime orchestrates and
records · cli / mcp / dashboard are windows.**

| Crate | Role | Depends on |
|---|---|---|
| [`hex-proto`](crates/hex-proto) | Versioned protocol: `Event`, `Command`, `Capability`. The one stable public surface, shared by kernel, workers, and clients. | — |
| [`hex-kernel`](crates/hex-kernel) | **Pure, deterministic.** Graph IR, journal model, projections, and the three functions `reduce` / `schedule` / `accept`. No IO, no subprocess, no wall clock. | proto |
| [`hex-worker`](crates/hex-worker) | Adapter for **one** opaque external agent/CLI behind the `Worker` trait + capability manifest (mock, subprocess/argv, coding-agent presets). Runs one worker, reports what happened. Never coordinates. | proto, kernel |
| [`hex-runtime`](crates/hex-runtime) | Orchestration — the imperative shell. Drive loop, effect execution, journal writer, control-command ingestion, workspace isolation, run supervision. Exposes the `Runtime` API and the `RuntimeClient` trait. | kernel, worker, proto |
| [`hex-cli`](crates/hex-cli) | The `hex` binary — a **thin client** over `RuntimeClient`. Arg parsing + rendering only. | runtime |
| [`hex-mcp`](crates/hex-mcp) | *(later)* MCP transport — a thin client/peer of the CLI over the same `RuntimeClient`. Can start and control runs. | runtime |
| [`hex-dashboard`](crates/hex-dashboard) | *(later)* TUI/web viewer — another thin client; also able to start runs. | runtime |
| [`hex-bench`](crates/hex-bench) | Criterion benchmarks. | kernel, runtime |

Dependency direction stays strictly inward: `proto ← kernel ← runtime`,
`proto,kernel ← worker`, `worker,kernel ← runtime`, all clients `← runtime`.
The kernel never imports a worker adapter, rendering, or any client.

### Execution model — functional core, imperative shell

The kernel is three pure functions:

- `reduce(state, event, graph) -> state` — all state-transition logic.
- `schedule(graph, state) -> Decision` — ready nodes, route evaluation, bound
  enforcement; returns **effect intents** (`StartAttempt`, `RunGate`,
  `RequestHuman`, `CancelAttempt`, `RecordTerminal`), never performs them.
- `accept(graph, state)` — the provisional-completion / acceptance contract.

The runtime replays the journal into a projection, asks `schedule` what to do,
writes each intent to the journal **before** acting (with an idempotency key),
executes it via a `Worker` adapter / gate executor / human wait, appends the
result events, feeds them back through `reduce`, and loops. On crash: replay
the journal; an attempt with no terminal event is `interrupted` — redo means a
**new run**, never a silent rerun.

Payoff: routing spends no tokens, the kernel is unit-testable with a mock
worker, and `replay(journal)` always reproduces the projection.

### Process & control model

- **Now:** foreground single process — `hex run` owns the loop via
  `RuntimeClient::InProcess`. No daemon.
- **Later:** a per-run background controller + `Remote` client behind the same
  `RuntimeClient` trait; the daemon is only a transport wrapper, never a second
  implementation.

Workers talk back over **one `Command` protocol with two transports**: an
injected `hex emit <event>` CLI (works for any subprocess — the universal
floor) and MCP tool hooks (e.g. `finish_session(success)`) for MCP-native
agents. The worker's capability manifest declares which it can use.

**Routing:** deterministic ordered edges evaluated by the kernel, plus
validated agent proposals — an agent node may only emit events from its
declared `may_propose` allow-list; the kernel validates the transition
(`route.rejected` feeds back otherwise). Agents can choose among *given*
options (e.g. a critic choosing "approve" vs "changes requested"); they can
never invent a transition.

## Domain model

Five kernel entities — everything else is a node kind, an event type, an
adapter, or a derived view:

**Graph** (Nodes + Edges) · **Run** (Attempts) · **Event** · **Budget** ·
**Artifact**

Five node kinds — roles ("planner", "reviewer") are metadata, never kinds:

`agent` · `command` · `gate` · `human` · `terminal`

- Interactivity is a **policy flag** on `agent` (`interactive: true` = a live,
  resumable session the human converses with — grill-me/Q&A style; requires
  the worker to declare `live_steering` + `session_resume`). Not a new kind.
- Approval (blocking human decision on a finished proposal) is a separate
  `human` boundary node — designed now, implemented in a later phase; it
  shares the same `human.requested` / `human.responded` event family.
- `command` and `gate` are distinct kinds (a gate yields pass/fail/escalate
  and feeds acceptance) sharing one executor in the runtime.

## Graph surface — single YAML file

Standard YAML only (no custom grammar — easy for humans *and* agents to
write). Edges co-located on the node via an `on:` map; prompts inline as block
scalars; `defaults:` + `templates:`/`extends:` kill repetition; reusable
run-level `gates:` feed `accept.require`.

```yaml
version: 1
name: implement-until-green

defaults:
  worker: codex
  context: fresh
  budget: { attempts: 8, elapsed: 30m }

gates:
  repo_tests:
    run: [npm, test]

nodes:
  implement:
    agent:
      prompt: |
        Implement the next unchecked item in TODO.md.
        Keep the diff small and focused.
      may_propose: [ready_for_test, needs_human]
    on:
      ready_for_test: test
      needs_human: clarify

  test:
    gate: { use: repo_tests }
    on: { passed: done, failed: implement }

  clarify:
    agent:
      interactive: true
      prompt: "Discuss the blocker with the operator; agree on a decision."
    on: { resolved: implement }

  done:
    terminal: succeeded

accept:
  require: [repo_tests.passed]
```

The kernel compiles any surface form to a flat, immutable Graph IR
(nodes + typed edges + bounds); a run records the exact snapshot + hash. Every
cycle must declare a bound — an unbounded cycle is a validation *error*.

## Config & presets

Layered config, project wins: `~/.config/hex/config.yaml` (user) ←
`.hex/config.yaml` (project). It holds the **worker registry** — how to invoke
each external agent (argv template, headless flags, model, declared
capabilities) — plus default budgets/context/isolation.

A **preset** is a named graph resolved through three layers: `.hex/graphs/`
(project) > `~/.config/hex/graphs/` (user) > built-in. hex ships `critique-loop`
(codex implements → claude critiques → gate) as the flagship built-in.

The operator supplies exactly one thing — the **prompt** — inline or from a
file; it fills `{{prompt}}` wherever the graph's node prompts reference it
(richer per-node typed inputs/outputs are internal graph dataflow, not an
operator flag):

```bash
hex run critique-loop -p "fix the flaky auth test"
hex run critique-loop -f prompts/task.md
```

An authoring skill (SKILL.md shipped in-repo) teaches coding agents to draft
graph YAML from a task description and iterate against `hex validate`.

## CLI surface

The same verbs work from the CLI, MCP, and dashboard; all support `--json` /
NDJSON, stable exit codes, and `capabilities` introspection.

```text
hex list                 list runnable graphs (project > user > built-in)
hex validate <graph>     schema, references, bounded cycles, capability match
hex graph <graph>        render (ascii/mermaid/dot)
hex run [<graph>]        start a NEW run (no graph → list what's runnable)
hex resume <run>         continue the SAME run (after pause or crash)
hex pause|cancel <run>   operator control
hex status <run>         projected run status
hex watch <run>          stream events (NDJSON with --json)
hex logs <run>           attempt output
hex emit <event>         worker→runtime, scoped-token control
hex respond <req>        human answer (interactive Q&A; later: approve/reject)
```

Deliberately absent: `retry`, `replay`, `skip`. Redoing work is always a new
`run` — the journal keeps the old one inspectable and resumable.

## Workspace isolation

Owned by the runtime, declared per graph: `isolation: shared` (default) or
`worktree` — one field flips the whole run into a fresh git worktree + branch
so codex/claude can work freely without touching your main working copy. No
auto-merge: the branch is left for you to inspect and integrate. Per-node
worktrees + a serialized integration queue arrive with parallelism.

## Status

**Phase 1 (slim MVP) works.** The critique loop runs end-to-end on real agent
CLIs, records everything to a JSONL journal, enforces budgets, and resumes a
killed run. `hex-mcp` / `hex-dashboard` are still stubs. Phased roadmap and
what's deferred: [`TODO.md`](TODO.md).

```bash
cargo build --workspace     # build everything
cargo test  --workspace
cargo clippy --workspace --all-targets
hex list
hex validate critique-loop
hex run critique-loop -p "fix the flaky auth test"
```

## Design

- [`docs/design/gpt-research-1.md`](docs/design/gpt-research-1.md) — landscape,
  architecture, MVP boundary.
- [`docs/design/gpt-research-2.md`](docs/design/gpt-research-2.md) — deep
  dives, failure modes, domain model.
- [`AGENTS.md`](AGENTS.md) — terminology + non-negotiable core rules.
- [`TODO.md`](TODO.md) — phased roadmap with every deferred decision.
