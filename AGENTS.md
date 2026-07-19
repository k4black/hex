# hex — thin, deterministic control plane for agentic loops/graphs. Full architecture/setup → README.md

## Project in one glance

Rust CLI (edition 2024) that runs existing agent CLIs as opaque workers over a
deterministic, bounded graph, recording everything to an append-only journal.
Layered Cargo workspace, one binary (`hex`). No orchestration logic exists yet —
this is a scaffold. Design rationale: `docs/design/gpt-research-{1,2}.md`.

## Where things live

Crates under `crates/`; dependencies point **strictly inward** (a crate may only
depend on ones above it in this table):

| Crate | Purpose | May depend on |
|---|---|---|
| `hex-proto` | Versioned protocol: `Event`, `Command`, `Capability`. Only stable public surface. | — |
| `hex-core` | Graph IR, journal, projections. | proto |
| `hex-engine` | Reducer, scheduler, acceptance. | core |
| `hex-backend` | `Backend` trait + capabilities + mock/subprocess adapters. | proto, core |
| `hex-cli` | The `hex` binary (operator surface). | all above |
| `hex-mcp` | *(stub, later)* MCP transport over the protocol. | proto |
| `hex-dashboard` | *(stub, later)* TUI/web viewer, projection consumer. | core |
| `hex-bench` | Cross-crate criterion benchmarks. | core, engine |

## Commands

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets   # workspace lints: unsafe forbidden, clippy::all warn
cargo run   --bin hex                     # NOT `cargo run -p hex` — package is hex-cli, binary is hex
cargo bench                               # criterion, in hex-bench
```

## Core rules

These are the design invariants that make hex *hex* — violating them turns it
into another agent framework. They are non-negotiable.

1. **Dependency direction is inward, always.** `hex-engine` must never import a
   backend adapter, rendering, or the CLI. The kernel stays testable without any
   model or subprocess.
2. **The journal is authoritative.** Every state change is an append-only
   `Event`. Status/graph views are projections *rebuildable from the journal* —
   never a second source of truth. Do not add mutable-database state.
3. **The kernel is deterministic; the worker is not.** A model may *propose* an
   event/route; the engine validates the transition. Routing spends no tokens.
4. **Node kinds stay tiny:** `agent`, `command`, `gate`, `human`, `terminal`.
   Roles ("planner", "reviewer") are metadata on an `agent` node, never new kinds.
5. **Every cycle is bounded.** An unbounded cycle is a validation *error*.
6. **Completion is provisional.** A worker's "done" is a proposal; required
   gates + acceptance rules decide the run outcome. Deterministic evidence
   outranks model assertions.
7. **Distinct operations get distinct events/verbs.** Never collapse
   register/start-process/model-call under one word like "spawn"; never make
   `resume`/`retry`/`replay` synonyms.
8. **Human and agent share one control protocol**, with authority scoped per
   actor. Every surface (CLI, `--json`, MCP, dashboard) is a projection over it —
   never a parallel implementation.

## Code style

- Prefer `argv` execution, NOT shell strings, for subprocess backends.
- Machine output: stdout carries requested data only, diagnostics to stderr,
  stable exit codes, `--json`/NDJSON, no interactive prompts in machine mode.

## Terminology

**Graph**: An immutable, versioned workflow definition (nodes + edges) a run
executes. Bounded cycles allowed. _Avoid_: workflow, DAG, pipeline.

**Node**: One schedulable unit with a single kind and one execution policy.
_Avoid_: step, stage, task, hat, role.

**Edge**: A legal transition from one node to another, activated by a named
event and carrying an ordered condition — never model-chosen control flow.
_Avoid_: link, arrow (a *transition* is the act; the edge is the rule).

**Attempt**: One execution of one node, with a unique id, a bound, and a start
+ terminal event. _Avoid_: run, try, iteration.

**Run**: One execution of a graph, referencing an exact graph snapshot/hash,
ending in one terminal disposition. _Avoid_: job, session, loop.

**Event**: An append-only fact in the journal (versioned, sequenced, actored).
The unit of truth. _Avoid_: log line, message.

**Gate**: A deterministic validator returning pass / fail / escalate.
_Avoid_: check, test (a test is *run by* a gate), hook.

**Backend**: An adapter wrapping an opaque external worker behind the
capability-declaring `Backend` trait. _Avoid_: agent (the worker), provider,
model, driver.

**Artifact**: A large or binary output/evidence/diff a node produces, stored by
content hash outside the event payload. _Avoid_: output, file, blob.

**Approval**: A durable human decision on a blocking gate (approve/reject/edit),
recorded with actor and rationale. _Avoid_: sign-off, confirmation.

**Budget**: A durable limit and its consumption (attempts/time/cost); never
resets on resume. _Avoid_: quota, cap, limit (a limit is one field of a budget).

**Projection**: A read model derived from and rebuildable from the journal.
_Avoid_: state, cache, snapshot (the atomic snapshot is one *kind* of projection).

**Operator**: Any actor (human or agent) issuing control commands.
_Avoid_: user, supervisor, controller.

## Gotchas

1. Binary is `hex`, package is `hex-cli` — use `cargo run --bin hex`, not `-p hex`.
2. Graph surface syntax (TOML vs YAML) is **undecided on purpose**; `hex-core`
   models the compiled IR only. Don't hardcode a format in the kernel.
3. `hex-mcp`/`hex-dashboard` are deliberate stubs; keep them thin — an MCP or UI
   surface is a transport/projection, never orchestration.
4. _add new gotchas here as they are discovered_
