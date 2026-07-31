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
   *effect intents* (`StartAttempt`, `RunCommand`, `RequestHuman`, …); only the
   runtime performs them, writing intent-before-effect with idempotency keys.
   A model may *propose* an event only from its node's `may_propose`
   allow-list; the kernel validates the transition. Routing spends no tokens.
4. **Node kinds stay tiny:** `agent`, `command`, `human`, `terminal` — **four**.
   Roles ("planner", "reviewer") are metadata on an `agent` node; interactivity
   is a *policy flag* on `agent` (`interactive: true`), never a new kind. A
   **gate is a role too**, not a kind (changed 2026-07-30): `gate` and `command`
   had identical fields, one validation arm and one executor, so they collapsed
   into `command`. What makes a node a gate is that `accept.require` names its
   signal — which already worked for *agent* signals too
   (`accept.require: [review.approved]`), proving gate-ness was never about the
   kind.
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
performed (`StartAttempt`, `RunCommand`, `RerouteUnmet`, …). The kernel emits it; the runtime
executes it. _Avoid_: command (reserved for operator `Command`s), task.

**Worker**: *Our adapter* in `hex-worker` — the code everyone calls with
params and gets results from; wraps one opaque external agent behind the
capability-declaring `Worker` trait. Internal plumbing: a graph names a **Role**,
which binds a worker. _Avoid_: backend (retired term), provider, driver.

**Agent**: The opaque external process a Worker drives (Claude Code, Codex, a
script). Also the node kind that invokes one. _Avoid_: calling our adapter an
agent, or the agent a worker.

**Gate**: A *role*, not a kind — a `command` node whose signal is named in
`accept.require`, so its deterministic `passed`/`failed` verdict feeds
acceptance. _Avoid_: calling it a node kind (it was one until 2026-07-30);
hook.

**Check**: A named argv in project config (`checks:` in `.hex/config.yaml`),
referenced by a graph as `command: { check: <name> }` and resolved at **compile**
time. **Empty by default** — what "green" means is per-project, so no built-in
preset gates on one. A graph naming a check the project has not declared is
**refused before the run starts**, with the key to add named in the error; it
never silently passes, because a gate whose verdict means nothing is worse than
no gate. _Avoid_: gate (a check is what a gate node *runs*).

**Role**: The user-facing unit a graph names (`role: reviewer`) —
implementer / reviewer / planner / researcher. A role binds a **worker** to a
`model`, `effort`, `read_only` policy and `prompt` preamble; two roles can share
one CLI and differ in everything else. Defined in layered config, deep-merged per
key, so a project overriding `roles.reviewer.model` inherits the rest. _Avoid_:
mode, persona, agent (the agent is the external process), and **orchestrator** —
routing is structural (core rule 9), never a role.

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
   still stubs or unbuilt. Node kinds `agent`/`command`/`terminal` work;
   All four node kinds now work, `human` included (answered with `hex respond`).
   Still unbuilt: `interactive`, `context: continue` (session resume),
   `templates:`/`extends:`, capability matching, cost/token accounting, stall
   detection, and a run digest. `hex-mcp`/`hex-dashboard` are empty stubs.
3. A node's routing token is an `EventBody::Signal { name }` — agent proposals
   *and* command verdicts (`passed`/`failed`) unify there; edges match on `name`.
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
8. Worktree isolation is per-run and opt-in (`hex run --worktree [<base>]` /
   `--no-worktree`), default `shared`; **no auto-merge** — the branch
   `hex/<run-id>` is left for explicit integration. Implemented as a thin slice
   (`hex-runtime/src/worktree.rs`): a **pooled** reusable slot under
   `.hex/worktrees/<n>/` (gitignored, fs4-locked like a run) — clean slots are
   reused (warm deps), dirty ones reclaimed-and-logged, parallel runs grow the
   pool. `workdir` = the slot; the journal + `emit`/`result` control files stay
   in the main `.hex/runs/<id>` (so the worktree diff stays clean and survives a
   discarded slot), and codex gets `--add-dir <run_dir>` so its `workspace-write`
   sandbox can still write them. hex never commits — the driver appends a banner
   asking the agent to. `--worktree-init "<argv>"` warms a fresh/reclaimed slot.
   Follow-ups (cleanup/prune verbs, `git worktree lock`, integration verbs,
   OS-sandbox read-only, graph-YAML field) are unbuilt. Design doc:
   `docs/design/2026-07-21-worktree-isolation.md`.
9. **Checks come from project config; naming an undeclared one is refused.** A
   graph names a check (`command: { check: test }`); `.hex/config.yaml`'s
   `checks:` map supplies the argv, and is **empty by default**. An undeclared
   check is a hard error at compile time naming the key to add — never a silent
   pass, since a run reaching `succeeded` having verified nothing is the failure
   mode worth designing against. Consequence, and it is deliberate: **built-in
   presets ship gate-free** so they run in any repo, except `tdd` and
   `implement-until-green` whose gate *is* the preset — those two refuse to start
   until you declare `checks.test`. Resolution happens at compile time and the
   resolved argv is recorded in `RunCreated.checks`, so editing config cannot
   change what a *resumed* run executes.
10. **Every attempt is bounded.** `Budget.attempt_elapsed_ms` is always set by
   the loader (`DEFAULT_ATTEMPT_ELAPSED_MS`, 30m; override with
   `budget: { attempt: 20m }`). Before this, a graph with no `elapsed` degraded
   to a bare blocking `child.wait()` and a hung agent blocked forever — the
   shipped `review` preset had exactly that shape. `attempt_deadline()` takes
   whichever of the run and attempt bounds bites first.
11. **A broken check is not a failing test.** `run_process` returns `Err` for an
   infrastructure failure (spawn/log/kill) and `Ok(false)` only for a real
   non-zero exit; only the latter routes `failed`. Routing infra failures as
   `failed` would spend agent tokens "fixing" code on evidence never gathered.
   `hex doctor` (and `doctor::preflight`, run by both `run` and `resume`) catches
   the common cases first: preflight *refuses to start* a run whose agent CLI is
   missing, rather than discovering it as a failed first attempt.
12. **Exit codes encode the disposition:** 0 succeeded · 1 failed · 2 usage
   error (clap's, so dispositions start at 3) · 3 timed out · 4 budget
   exhausted · 5 cancelled · **6 paused** (a paused run has no terminal
   disposition, so it needs its own code; `hex wait` also uses 1 for an
   abandoned run). A driving agent can branch without parsing output.
13. **The kernel says *why* a run ended.** `Effect::RecordTerminal` carries
   `why: Option<String>`; the driver journals it as a `Note` before
   `RunFinished`. This is how `budget_exhausted` distinguishes attempts from
   cycle visits, and how a success terminal downgraded by an unmet
   `accept.require` names the missing evidence instead of a bare `failed`.
14. **Roles are the graph's vocabulary; workers are plumbing.** `roles:` in
   config (implementer/reviewer/planner/researcher) each bind a `worker` plus
   `model`/`effort`/`read_only`/`prompt`. A graph says `role: reviewer`; the
   registry resolves it (`Workers::from_config` registers every worker under its
   own name *and* every role under the role's name, roles last so they win a
   clash). A role's prompt preamble is prepended at **compile** time, so it is
   part of the IR a resumed run replays. `prompt:` replaces an inherited
   preamble; `prompt_append:` extends it — the common case for a project
   tightening a shipped role.
15. **The built-in config layer is `hex-runtime/src/defaults.yaml`**, embedded via
   `include_str!` and parsed by the *same* loader and merge path as user/project
   config. There is no hardcoded `Config::builtin()` value to drift from it. Every
   surveyed tool that hardcoded its defaults (Roo's `DEFAULT_MODES`, Cline's mode
   union, Cursor's removed Custom Modes) forced all-or-nothing overrides.
16. **A command node runs N steps with a mode**, and the modes differ in *failure*
   semantics, not just concurrency: `ordered` (default) stops at the first
   failure; `parallel` runs every step and fails if any did. Parallel buffers each
   step into `attempts/<id>/<n>-<label>/`, numbered in **declared** order, so
   concurrent evidence reads like sequential evidence and the journal stays
   deterministic. Infra failure in any step still fails the *attempt* rather than
   routing `failed` (gotcha 11).
17. **`accept.on_unmet` routes instead of dead-ending** — and has **two open
   unbounded-loop bugs** (see TODO.md "OPEN BUGS"): a terminal's `budget.visits`
   satisfies cycle validation but `schedule` settles terminals before checking
   visit budgets, and `budget.attempts` cannot bound a human-only cycle because a
   human response spends no attempt. Do not rely on `on_unmet` bounding itself yet. Reaching a success
   terminal with missing evidence used to end the run `failed`, which spent the
   whole budget and fixed nothing. Now the kernel emits `Effect::RerouteUnmet` and
   the runtime journals `EventBody::AcceptanceUnmet` — its own event, *not* a
   synthesized `Signal`, because `reduce` deliberately drops routing signals no
   in-flight attempt produced (a fail-closed guard that must not be relaxed to
   express this). Without `on_unmet` the run still fails, but the reason names the
   remedy.
18. **`budget: { visits: N }` bounds one node**, so an expensive review cycle can
   be capped at 3 without also capping a cheap lint cycle. The run-wide
   `cycle_visits` remains a blanket backstop; whichever is tighter bites first.
19. **A `roles:` entry shadows a same-named `workers:` entry.** `Workers::from_config`
   registers workers first, then roles, so a role wins the clash — that is
   intended (the role is the user-facing concept), but it bites: naming a scratch
   `kind: command` worker `implementer` silently runs the *role's* real agent CLI
   instead. Name test workers something that is not a role.
20. **The control inbox is `.hex/runs/<id>/control/{tmp,inbox,done}/`.** A
   controller writes to `tmp/`, `sync_all`s, then `rename()`s into `inbox/`;
   temp-then-rename is mandatory so a poller never reads a torn write. The driver
   drains the inbox before each `schedule()`, renaming a file into `done/`
   *before* applying it — deliberately **at-most-once**: losing a `steer` is
   cheaper than double-applying a terminal and corrupting the lifecycle. Files
   sort `{now_ms:013}-{uuid}.json` so lexical order ≈ arrival order. This polled
   design is what every daemonless job runner converges on; there is no
   persistent listener to push to, and it works whether the run is live or being
   `resume`d later.
21. **`pause` returns without a terminal.** `Session::drive()` returns
   `Option<Disposition>` — `None` means paused, and *no* `RunFinished` was
   written, so `hex resume` continues the same run. `Status::Paused` finally has a
   producer. Pause is only honored at an attempt boundary, never mid-attempt.
22. **`human` nodes work now.** `RequestHuman` journals `HumanRequested` (prompt
   interpolated), then blocks polling the inbox at 250ms bounded by
   `attempt_deadline()` (a timeout is `TimedOut`, so it cannot hang forever);
   `HumanResponded` stores the answer as the node's result so `{{node.result}}`
   works exactly like an agent's. `E-human-no-edge`/`E-human-multi-edge` keep the
   validator honest — an answer is text, not a signal, so >1 outgoing edge could
   never be chosen between. **Known wart:** the answer is fenced downstream as
   "untrusted agent output" because `interpolate` does not know node kinds — safe
   default, factually wrong label.
23. **Detach re-execs, never forks.** `--detach` spawns `current_exe()` with a
   hidden `--reserved-run-id`, stdio to `detached.{out,err}`, and
   `process_group(0)`; the launcher exits without `wait()`ing. `fork()` is unsafe
   in a multithreaded Rust process — do not reintroduce it. Liveness is the
   existing `RunLock` (kernel-released on death, unlike a pidfile — PID reuse is
   real) plus a 5s heartbeat file: lock+fresh beat = live, lock+stale = hung, no
   lock+no terminal = abandoned.
24. **An unexplained fs4/flock flake affects *both* lock kinds, and the tests
   that would catch it are serialized.** A just-released lock intermittently still
   reads as busy: worktree slot leasing claims slot 1 instead of the freed slot 0,
   and `RunLock::acquire` inside `cancel` gets `WouldBlock` on a lock `start` had
   already released (so `hex cancel` right after a run ends can queue instead of
   recording). Two different lock files, so it is our locking or fs4 on macOS —
   not a worktree quirk. `worktree.rs`'s leasing tests sit behind a test-only
   mutex, which makes them green **but stops the concurrent path being exercised
   at all** — treat "parallel runs grow the pool" as untested. Diagnose (thread id
   + realpath + inode at each lock attempt) before trusting it; do not add another
   mutex. Original detail:
   serialized runs pass 8/8, parallel ones failed ~1-in-3, even across separate
   repos with distinct lock inodes. The safety invariant (no two live leases share
   a slot) holds regardless, so the cost is a needless extra slot, not corruption.
25. **`hex-kernel/src/lifecycle.rs` is the single home for every "may this event
   apply here?" rule**, and `check_journal` folds *through* `reduce` rather than
   keeping its own projection — so the journal's two consumers cannot disagree
   about where a run is. Adding a guard means editing one predicate, not two state
   machines. Two real bugs came from the old drift (a run using `accept.on_unmet`
   became permanently unreadable; a duplicate `NodeResult` was rejected by the
   audit but silently folded by `reduce`). The module doc enumerates the remaining
   *deliberate* asymmetries — keep that list honest when you add one.
26. **A transition exists in exactly one place.** `accept.on_unmet`'s implicit
   success-terminal → node edge is `Graph::implicit_reroutes()` /
   `implicit_reroute_from()`, consulted by both `schedule` and cycle validation.
   It deliberately omits a `nodes.contains_key(to)` check because `schedule` never
   had one; target existence is enforced by `E-accept-unmet-node` and
   `lifecycle::unmet_reroute_ok`.
27. _add new gotchas here as they are discovered_
