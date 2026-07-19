# hex

**A thin, deterministic control plane for agentic loops and graphs.**

`hex` compiles a human-readable graph, runs existing agent CLIs (Claude Code,
Codex, Gemini, …) as opaque workers, records every transition in an append-only
journal, enforces hard limits and evidence gates, and exposes the *same* control
protocol to humans and agents.

It is deliberately narrow. The loop itself can be one shell line; the value is a
small, legible kernel that makes nondeterministic agents **programmable,
interruptible, resumable, verifiable, and easy to operate** — from a terminal or
from another agent.

> Make the graph deterministic, the workers replaceable, the journal
> authoritative, every loop bounded, completion evidence-based, and every human
> or agent intervention an explicit protocol event.

## What hex is not

An LLM/provider SDK · a coding agent · a memory/RAG system · an issue tracker ·
a multi-agent role-playing framework · a cloud workflow platform · a required
daemon. These may become optional adapters later; they never enter the kernel's
domain model.

## Core ideas

- **Deterministic kernel.** Models may *propose* events/routes; the kernel
  validates every state transition. Routing spends no tokens.
- **One canonical journal.** Every state change, control request, approval, and
  result is an append-only event. Status/graph views are rebuildable
  projections — never a second source of truth.
- **Human/agent symmetry.** A person at a TTY and an orchestrating agent send
  the same commands; authority is scoped per actor. Every UI (CLI, `--json`,
  MCP, dashboard) is a projection over one protocol.
- **Bounded by construction.** Every cycle must declare a bound (attempts,
  time, budget, or an evidence/human exit). Unbounded cycles are a validation
  error, not a warning.
- **Completion is provisional.** A worker's "done" is a proposal; required
  deterministic gates and acceptance rules decide the run's real outcome.
- **Fresh context by default.** Per-node context policy (`fresh`/`continue`/…)
  is explicit, never silently inherited from a provider session.

## Workspace layout

Layered Cargo workspace (edition 2024); dependencies point strictly inward.

| Crate | Role | Depends on |
|---|---|---|
| [`hex-proto`](crates/hex-proto) | Versioned control protocol: `Event`, `Command`, `Capability`. The one stable public surface. | — |
| [`hex-core`](crates/hex-core) | Domain model: compiled graph IR, journal, projections. | proto |
| [`hex-engine`](crates/hex-engine) | Deterministic reducer, scheduler, acceptance rules. | core |
| [`hex-backend`](crates/hex-backend) | `Backend` trait + capability manifest + mock/subprocess adapters. | proto, core |
| [`hex-cli`](crates/hex-cli) | The `hex` binary — operator surface for humans and agents. | all |
| [`hex-mcp`](crates/hex-mcp) | *(later)* MCP adapter — a thin transport over the protocol, no orchestration logic. | proto |
| [`hex-dashboard`](crates/hex-dashboard) | *(later)* Optional TUI/web viewer — a projection consumer only. | core |
| [`hex-bench`](crates/hex-bench) | Cross-crate criterion benchmarks. | core, engine |

Dependency direction: `proto ← core ← engine`, `proto,core ← backend`, all
`← cli`. The engine never imports backend adapters, rendering, or the CLI.

## Vocabulary

`Graph` · `Node` · `Edge` · `Run` · `Attempt` · `Event` · `Gate` · `Approval` ·
`Budget` · `Backend` · `Artifact` · `Projection` · `Operator`.

Node kinds are intentionally tiny: `agent`, `command`, `gate`, `human`,
`terminal`. Roles like "planner"/"reviewer" are *metadata* on an `agent` node,
never distinct kinds. See [`AGENTS.md`](AGENTS.md) for full definitions.

## Planned CLI surface

A representative slice — the full roadmap lives in [`TODO.md`](TODO.md).

```text
hex validate <graph>       check schema, references, bounded cycles
hex graph <graph>          render the graph (ascii/mermaid/dot)
hex run <graph>            execute a run
hex status <run>           projected run status
hex watch <run>            stream events (ndjson with --json)
hex pause|resume|cancel    operator control
hex step <run>             run exactly one ready attempt
hex emit <event>           agent-side scoped structured control
hex approve|reject <req>   human decisions on blocking gates
```

## Status

Early scaffold. Every crate compiles with placeholder types; there is no real
orchestration yet. The graph *surface syntax* (TOML vs YAML) is deliberately
still undecided — the core models a compiled IR and loading is a stub.

```bash
cargo build --workspace     # build everything
cargo test  --workspace     # run unit tests
cargo run   --bin hex       # print the planned command surface
cargo bench                 # run criterion benchmarks
```

## Design

The architecture is derived from a broad survey of the agentic-loop /
orchestration ecosystem (AutoLoop, Ralph/Hats, Microsoft Conductor, Gas
Town/City, LangGraph, Dagu, Ruflo, and more). The full research lives in
[`docs/design/`](docs/design/):

- [`gpt-research-1.md`](docs/design/gpt-research-1.md) — landscape, architecture, MVP boundary.
- [`gpt-research-2.md`](docs/design/gpt-research-2.md) — deep dives, failure modes, domain model.
