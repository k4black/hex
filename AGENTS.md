# hex — thin, deterministic control plane for agentic loops/graphs. Full architecture/setup → README.md

## Project in one glance

Rust CLI (edition 2024) that runs existing agent CLIs as opaque workers over a
deterministic, bounded graph, recording everything to an append-only journal.
Layered Cargo workspace, one binary (`hex`). No orchestration logic exists yet.
Design rationale: `docs/design/gpt-research-{1,2}.md`; the decisions below
(locked 2026-07-19) supersede the research where they differ.

## Where things live

Dependencies point **strictly inward** (a crate may only depend on ones above
it in this table). One-liner: *kernel decides · worker runs one agent ·
runtime orchestrates and records · cli/mcp/dashboard are windows.*

| Crate | Purpose | May depend on |
|---|---|---|
| `hex-proto` | Versioned protocol: `Event`, `Command`, `Capability`. Only stable public surface. | — |
| `hex-kernel` | **Pure**: Graph IR, journal model, projections, `reduce`/`schedule`/`accept`. No IO/subprocess/clock. | proto |
| `hex-worker` | `Worker` trait + capability manifest + adapters (mock, subprocess, coding-agent presets). Runs **one** worker; never coordinates. | proto, kernel |
| `hex-runtime` | Imperative shell: drive loop, effect execution, journal writer, control ingestion, workspace isolation, run supervision. Exposes `Runtime` + `RuntimeClient` trait (`InProcess` now, `Remote` later). | kernel, worker, proto |
| `hex-cli` | The `hex` binary — thin client over `RuntimeClient`; arg parsing + rendering only. | runtime |
| `hex-mcp` | *(later)* MCP transport — a peer client of the CLI; can start/control runs. | runtime |
| `hex-dashboard` | *(later)* TUI/web viewer — another thin client. | runtime |
| `hex-bench` | Cross-crate criterion benchmarks. | kernel, runtime |

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

1. **Dependency direction is inward, always.** `hex-kernel` must never import
   a worker adapter, rendering, or any client. The kernel stays testable
   without any model or subprocess.
2. **The journal is authoritative.** Every state change is an append-only
   `Event`. Current state is a projection *computed* from the journal
   (`fold(reduce, journal)`) — never stored as a second source of truth. No
   mutable-database state.
3. **The kernel is deterministic; the worker is not.** The kernel emits
   *effect intents* (`StartAttempt`, `RunGate`, `RequestHuman`, …); only the
   runtime performs them, writing intent-before-effect with idempotency keys.
   A model may *propose* an event only from its node's `may_propose`
   allow-list; the kernel validates the transition. Routing spends no tokens.
4. **Node kinds stay tiny:** `agent`, `command`, `gate`, `human`, `terminal`.
   Roles ("planner", "reviewer") are metadata on an `agent` node; interactivity
   is a *policy flag* on `agent` (`interactive: true`), never a new kind.
5. **Every cycle is bounded.** An unbounded cycle is a validation *error*.
6. **Completion is provisional.** A worker's "done" is a proposal; required
   gates + acceptance rules decide the run outcome. Deterministic evidence
   outranks model assertions.
7. **Two execution verbs only.** `run` starts a new run; `resume` continues
   the same run from its journal (after pause *or* crash). There is no
   `retry`/`replay`/`skip` — redoing work is a new run. Never silently rerun a
   side-effecting attempt.
8. **Human and agent share one control protocol**, with authority scoped per
   actor. One `Command` type, two worker transports (injected `hex emit` CLI +
   MCP tool hooks); every surface (CLI, `--json`, MCP, dashboard) is a thin
   client over `RuntimeClient` — never a parallel implementation.
9. **The worker adapter never coordinates.** Sub-agents, watchdogs, fan-out
   are kernel-routed / runtime-scheduled graph constructs, or the external
   agent's own internal business — never logic inside `hex-worker`.

## Code style

- Prefer `argv` execution, NOT shell strings, for subprocess workers.
- Machine output: stdout carries requested data only, diagnostics to stderr,
  stable exit codes, `--json`/NDJSON, no interactive prompts in machine mode.

## Terminology

Five kernel entities: **Graph, Run, Event, Budget, Artifact.** Everything else
is a node kind, an event type, an adapter, or a derived view.

**Graph**: An immutable, versioned workflow definition (nodes + edges) a run
executes. Bounded cycles allowed. _Avoid_: workflow, DAG, pipeline.

**Node**: One schedulable unit with a single kind and one execution policy.
_Avoid_: step, stage, task, hat, role.

**Edge**: A legal transition from one node to another, activated by a named
event and carrying an ordered condition — never model-chosen control flow.
_Avoid_: link, arrow (a *transition* is the act; the edge is the rule).

**Run**: One execution of a graph, referencing an exact graph snapshot/hash,
ending in one terminal disposition. _Avoid_: job, session, loop.

**Attempt**: One execution of one node, with a unique id, a bound, and a start
+ terminal event. _Avoid_: run, try, iteration.

**Event**: An append-only fact in the journal (versioned, sequenced, actored).
The unit of truth. Approvals, gate results, budget spend, artifact refs are
all event types, not separate entities. _Avoid_: log line, message.

**Effect (intent)**: A description of an external action the kernel wants
performed (`StartAttempt`, `RunGate`, …). The kernel emits it; the runtime
executes it. _Avoid_: command (reserved for operator `Command`s), task.

**Worker**: *Our adapter* in `hex-worker` — the code everyone calls with
params and gets results from; wraps one opaque external agent behind the
capability-declaring `Worker` trait. _Avoid_: backend (retired term),
provider, driver.

**Agent**: The opaque external process a Worker drives (Claude Code, Codex, a
script). Also the node kind that invokes one. _Avoid_: calling our adapter an
agent, or the agent a worker.

**Gate**: A *node kind* running a deterministic validator returning
pass / fail / escalate; feeds acceptance. Reusable run-level gates are
declared once and referenced. _Avoid_: check, test (a test is *run by* a
gate), hook.

**Artifact**: A large or binary output/evidence/diff a node produces, stored
by content hash outside the event payload. _Avoid_: output, file, blob.

**Budget**: A durable limit and its consumption (attempts/time/cost); never
resets on resume. _Avoid_: quota, cap, limit (a limit is one field of a
budget).

**Projection**: A read model *computed* from the journal — never stored
authority. _Avoid_: state, cache, snapshot (the atomic snapshot file is one
*kind* of projection, an optimization).

**Operator**: Any actor (human or agent) issuing control commands, with
scoped authority. _Avoid_: user, supervisor, controller.

**Preset**: A named, parametrized graph in the library, resolved project
(`.hex/graphs/`) > user (`~/.config/hex/graphs/`) > built-in; invoked as
`hex run <preset> -p "<prompt>"`. _Avoid_: pipeline (banned Graph synonym),
template (reserved for `templates:` node reuse inside a graph).

**Interactive session**: An `agent` attempt with `interactive: true` — stays
open for live human↔agent conversation (grill-me/Q&A), journaled per turn,
resumable. Requires worker capabilities `live_steering` + `session_resume`.

**Approval**: A blocking human decision on a finished proposal — a `human`
boundary node (designed now, built later), recorded as
`human.requested`/`human.responded` events with actor + rationale. _Avoid_:
sign-off, confirmation.

## Gotchas

1. Binary is `hex`, package is `hex-cli` — use `cargo run --bin hex`, not `-p hex`.
2. Phase 1 (slim MVP) is implemented and green; Phase 2+ items in TODO.md are
   still stubs or unbuilt. Node kinds `agent`/`gate`/`command`/`terminal` work;
   `human` fails closed (no transport yet). `interactive`, `pause`, worktrees,
   `templates:`/`extends:`, reusable `gates:`, and capability matching are Phase 2+.
3. A node's routing token is an `EventBody::Signal { name }` — agent proposals
   *and* gate verdicts (`passed`/`failed`) unify there; edges match on `name`.
   `reduce(graph, state, event)` owns routing (it takes the graph); `schedule`
   only emits `Effect` intents; the runtime `Session` executes them.
4. Surface syntax is **standard YAML only** — kind-as-key + `on:` map,
   co-located edges, inline block-scalar prompts. No custom mini-grammar; the
   kernel models the compiled IR only, and the loader lives in `hex-runtime`
   (not the kernel). YAML via `yaml_serde` (the maintained serde_yaml fork).
4b. **Operator input is one prompt.** The CLI takes only `-p/--prompt <text>`
   or `-f/--file <path>`, filling `{{prompt}}` in node prompts — no `--input
   k=v`. Named *typed* inputs/outputs are a **node** concern (internal graph
   dataflow, Phase 6), never an operator flag. Keep run-config (budget, worker,
   isolation) on their own CLI flags, off the prompt channel.
5. The worker↔runtime channel is `hex emit <event>`: the runtime injects
   `HEX_EMIT_FILE`/`HEX_MAY_PROPOSE` etc.; the agent's argv must be able to
   reach the `hex` binary. `may_propose` is enforced both at emit and at ingest.
5b. **Result capture & implicit completion.** The runtime also injects
   `HEX_RESULT_FILE` and a `{result}` argv token; a worker's `result:`
   (`file`|`json_result`) says how to capture its final message (codex
   `--output-last-message {result}`, claude `--output-format json` → `.result`).
   The capture is recorded as `EventBody::NodeResult` and folded into
   `RunState.results`; a downstream prompt references it as `{{node.result}}`,
   interpolated **at attempt-start** in the driver (not compile-time) and wrapped
   as untrusted. An agent that exits cleanly *without* emitting gets a
   runtime-synthesized reserved `done` signal (implicit completion), so a
   single-outcome node needs no `hex emit`; it must have an `on: { done: … }`
   edge (the validator allows `done` without a `may_propose` entry). Nodes with
   >1 outcome still emit. `read_signal` returning `Ok(None)` is *not* an error.
5c. **Workers are typed adapters behind one trait.** Each agent CLI has its own
   `Worker` impl in `hex-worker` (`CodexWorker`/`ClaudeWorker`/`OpencodeWorker`)
   owning its argv and result-capture mode; `CommandWorker` is the generic argv
   escape hatch (+ `MockWorker`). Config selects one via `kind:`
   (`codex`/`claude`/`opencode`/`command`, default `command`); the runtime/CLI
   only ever see `dyn Worker`. A node's `read_only: bool` flows to
   `WorkRequest.read_only`, but is **advisory** today (prompt-enforced): a true
   read-only sandbox (codex `--sandbox read-only`) would also block the agent
   from writing `HEX_EMIT_FILE`/`HEX_RESULT_FILE` under the workspace and break
   routing — enforced read-only needs a non-workspace control transport (TODO).
   Adding an agent = a new `Worker` impl + `WorkerKind`, not scattered flags.
5c-perm. **Headless permissioning is per-agent, never a blanket bypass.** codex
   runs under its OS sandbox (`--sandbox workspace-write`); claude uses an *auto
   classifier* (`--permission-mode acceptEdits` + `--allowedTools` incl. `Bash`
   for the `hex emit` channel + `--disallowedTools` denying destructive/exfil/
   publish commands) — **never `--dangerously-skip-permissions`**; opencode is
   still `--auto` (blanket approve — TODO to generate an `opencode.json`
   `permission` block). The claude deny-list is defense-in-depth, not an OS
   boundary (shell chaining bypasses prefix matching) — real containment awaits
   worktree/OS isolation (Phase 2). Lists live in `ClaudeWorker` (`agent.rs`).
5d. **Run ids** are `yyyy-MM-dd-<workflow>-<short-uuid>`, or `yyyy-MM-dd-<name>`
   when the operator passes `hex run --name`; slugged + path-safe, a same-day
   clash gets a `-2` suffix. `validate_run_id` allows `[a-z0-9-_]` only.
6. `hex run`'s workspace is the project cwd (shared isolation); run it from the
   repo root. Redo = new `run`; `resume` continues the same run and marks an
   orphaned attempt `interrupted` before re-attempting (never a silent rerun).
7. `hex-mcp`/`hex-dashboard` are deliberate stubs; they become thin
   `RuntimeClient` clients — a transport/projection, never orchestration.
8. Worktree isolation is per-run and opt-in (`isolation: worktree`, Phase 2),
   default `shared`; **no auto-merge** — the branch is left for explicit
   integration.
9. _add new gotchas here as they are discovered_
