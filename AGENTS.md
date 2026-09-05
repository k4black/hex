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
| `hex-worker` | `Worker` trait + capability manifest + adapters (mock, subprocess, coding-agent presets). Runs **one** worker; never coordinates. | proto |
| `hex-runtime` | Imperative shell: drive loop, effect execution, journal writer, control ingestion, workspace isolation, run supervision. Exposes the concrete `Runtime` — including `read_streams`, so tailing a live attempt needs no knowledge of `.hex/`. (The `RuntimeClient` trait it also exposed was deleted 2026-08-01: one impl, no callers.) | kernel, worker, proto |
| `hex-cli` | The `hex` binary — thin client over `Runtime`; arg parsing + rendering only. | runtime |
| `hex-mcp` | *(later)* MCP transport — a peer client of the CLI; can start/control runs. | runtime |
| `hex-dashboard` | *(later)* TUI/web viewer — another thin client. | runtime |

## Commands

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets   # workspace lints: unsafe forbidden, clippy::all warn
cargo run   --bin hex                     # NOT `cargo run -p hex` — package is hex-cli, binary is hex
cargo bench                               # criterion, in hex-runtime/benches
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
   client over the runtime — it parses arguments and renders, and every fact it
   shows the runtime computed. That is the substance, and it does not require a
   trait: `RuntimeClient` (nine signatures, one implementation, zero callers)
   was deleted 2026-08-01 because a trait is cheaper to re-derive from a second
   implementation than to keep honest without one. The rule a client must not
   break is *no parallel implementation*, not *implement this trait*.
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
   still stubs or unbuilt. All four node kinds work, `human` included (answered
   with `hex respond`), as do `context: continue` (gotcha 33) and token/cost
   accounting (gotchas 27-29). Still unbuilt: `interactive`,
   `templates:`/`extends:`, capability matching *for anything but session resume*,
   and stall detection. `hex-mcp`/`hex-dashboard` are empty stubs.
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
   (`file`|`jsonl_result`|`jsonl_last_text`|`pi_jsonl`) says how to capture its
   final message (codex `--output-last-message {result}`, claude stream-json's
   last `result` line; `json_result` — a single JSON object — was deleted
   2026-09-05 with no producer left in-tree).
   The capture is recorded as `EventBody::NodeResult` and folded into
   `RunState.results`; a downstream prompt references it as `{{node.result}}`,
   interpolated **at attempt-start** in the driver (not compile-time) and wrapped
   as untrusted. An agent that exits cleanly *without* emitting gets a
   runtime-synthesized reserved `done` signal (implicit completion), so a
   single-outcome node needs no `hex emit`; it must have an `on: { done: … }`
   edge (the validator allows `done` without a `may_propose` entry). Nodes with
   >1 outcome still emit. `read_signal` returning `Ok(None)` is *not* an error.
5c. **Workers are typed adapters behind one trait.** Each agent CLI has its own
   `Worker` impl in `hex-worker` (`CodexWorker`/`ClaudeWorker`/`PiWorker`/`OpencodeWorker`)
   owning its argv and result-capture mode; `CommandWorker` is the generic argv
   escape hatch (+ `MockWorker`). Config selects one via `kind:`
   (`codex`/`claude`/`pi`/`opencode`/`command`, default `command`); the runtime/CLI
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
7. `hex-mcp`/`hex-dashboard` are deliberate stubs; they become thin clients over
   `Runtime` — a transport/projection, never orchestration.
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
17. **`accept.on_unmet` routes instead of dead-ending.** Its two former
   unbounded-loop holes were closed 2026-08-08 in `check_cycles`: a node now
   breaks a cycle only when its bound is *enforced* there — a terminal's
   `budget.visits` no longer counts (`schedule` settles terminals before budget
   checks, so it was never enforced), and `budget.attempts` breaks only cycles
   containing an `agent`/`command` node (a human response spends no attempt).
   Both graphs that used to spin forever are now `E-unbounded-cycle` at compile
   time, with the human-only case named in the message. Reaching a success
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
   never be chosen between. Downstream the answer is fenced as *operator input*,
   not "untrusted agent output" (`driver.rs`, pinned by
   `a_human_answer_is_labelled_operator_input_not_agent_output`) — an earlier
   version of this gotcha claimed otherwise and was stale.
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
   mutex. **There is now a reproducer in the suite**: under full-workspace
   parallel load, `cancel_of_an_idle_run_is_recorded_directly` fails roughly
   1-in-4 at the `Cancellation::Recorded` assertion — `cancel` took the
   `WouldBlock` path and queued the command instead of appending the terminal.
   Verified 2026-08-02 to reproduce identically at `adc173f`, so it is not new;
   `cargo test -p hex-runtime --test control` alone always passes, which is why
   it hid for so long. Original detail:
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
27. **Money is integer micro-USD, never `f64`.** `Event`/`EventBody` derive `Eq`,
   which a float field would break, and a cost that round-trips through JSON as a
   float is a recorded fact that can change value on replay. `$0.1778` is
   `177_800`. `hex-proto`'s `ModelUsage.cost_micro_usd` and the kernel's
   `Totals.cost_micro_usd` are the only spellings; the CLI's `usd()` renders them
   to four decimals (two would print `$0.00` for a real cheap attempt).
28. **hex never estimates a cost.** Usage comes from each agent's own structured
   output, parsed by that agent's adapter: codex from `codex exec --json`
   (`thread.started.thread_id`, `turn.completed.usage`), claude from the
   `--output-format json` object it already parses (`modelUsage` per model,
   `total_cost_usd`). codex reports **no model name and no cost**, so its rows are
   tokens only, labelled with the role's configured model or `"codex"`. There is
   deliberately no price table — prices drift, and a wrong number is worse than an
   absent one. `cost_micro_usd` stays `Option` so one could be added later without
   a schema change. Parsers are tested against **captured real output** in
   `crates/hex-worker/tests/fixtures/`: the first draft of the codex parser looked
   for a `token_count` event that does not exist, and only a real fixture caught it.
28b. **codex's `input_tokens` includes its cached tokens; claude's does not.**
   codex follows the OpenAI convention (`input_tokens` is the whole prompt,
   `cached_input_tokens` the part that hit cache), so the fresh share is the
   *difference* — adding both bills the cached tokens twice. Anthropic reports
   `cacheReadInputTokens`/`cacheCreationInputTokens` *beside* `inputTokens`, so
   nothing is subtracted there. `reasoning_tokens` is a subset of output on both
   and is carried for information only, never added into `tokens()`.
29. **`AttemptReported` is recorded before anything that can end the attempt**, and
   at most once per attempt. `reduce` clears `current_attempt` when a signal
   routes, so a report written after it fails `correlated` and is silently dropped.
   The at-most-once guard (`lifecycle::attempt_report_ok` + `RunState
   .reported_attempt`) exists because usage is *summed*, not overwritten: unlike a
   duplicate `NodeResult`, a duplicate report inflates the bill rather than
   replacing a value. Cost per node comes from the attempt's own total when the
   agent reported one, else the sum of its per-model costs — never both, or claude
   (which reports both) would be billed twice.
30. **Every run ends with `RunFinished`, and `AttemptFailed` still carries the
   disposition.** The duplication is deliberate: `AttemptFailed` stays
   self-terminating (one atomic durable fact), and the trailing `RunFinished` gives
   every consumer *one* terminator to tail for — before it, a timed-out run's
   journal simply stopped. `check_journal` allows at most one trailing
   `RunFinished` and only when its disposition **equals** the recorded one;
   absence stays legal, which is what keeps pre-change journals readable.
31. **A killed attempt takes its process group.** `logged_command` spawns with
   `process_group(0)` and `wait_bounded`'s deadline path sends `killpg(SIGTERM)`,
   waits 2s, then `SIGKILL`, through `nix`'s safe wrapper (the workspace forbids
   `unsafe`, so `libc::killpg` is not an option). `child.kill()` alone reparented
   an agent's grandchildren — a build, a test run, a dev server — to pid 1, and
   they survived every timeout. Pinned by
   `a_timed_out_attempt_kills_the_whole_process_group`, which was verified to fail
   against the old behaviour before being kept.
32. **A dead attempt's output is salvaged, not discarded.** `run_agent` captures
   the result *and* the usage report before branching on the outcome; both failure
   paths (timeout **and** nonzero exit) used to return above that point. With no
   final message, the bounded 8 KB **tail** of stdout *and* stderr becomes the
   node's result, prefixed `[partial output — …]`. The tail, not the head: an
   agent's newest output is the informative part. It is a real `NodeResult`, so it
   shows up in `hex logs` and the end-of-run output — but note it does **not**
   reach a downstream `{{node.result}}` today, because the same paths call
   `fail_attempt`, which is terminal. The marker earns its keep for the human
   reading it; the "downstream handoff" justification an earlier version of this
   gotcha gave was wrong, and would only apply if a failed attempt ever routed on.
33. **`context: continue` resumes *that node's* session, and is refused at compile
   time when the worker cannot.** The handle is `RunState.sessions[node]`, folded
   from `AttemptReported.session_id`, so it survives a crash and a `hex resume`.
   Per node on purpose: a reviewer resuming the implementer's session inherits its
   reasoning and stops being an independent judge. `check_workers` (runtime, not
   kernel — only the runtime may see a worker) rejects the node unless its worker
   declares `Capability::SessionResume`, because silently degrading to a fresh
   session costs exactly what the policy exists to avoid. This is the capability
   manifest's **first real consumer**; before it, `Worker::capabilities()` had zero
   callers workspace-wide.
33b. **`codex exec resume` takes a smaller flag set than `codex exec`** — no
   `--sandbox`, no `--add-dir`, no `--cd`. Those become `-c
   sandbox_mode="workspace-write"` and
   `-c sandbox_workspace_write.writable_roots=[…]`. Both key names were verified
   against the CLI with `--strict-config`, which rejects an unknown field before
   contacting the model (so it costs nothing to check).
34. **A command step records its exit status to an `exit` file** beside its logs,
   because nothing else remembers it: the journal keeps the *attempt's* verdict and
   the names of the failed steps, so a reader of one step directory could not tell
   whether that step was the culprit. It is the recorded string (`"0"`, `"101"`,
   `"signal"`), not a typed status. `StepLog::failed()` treats an *unrecorded*
   status as not-failed — silence is not evidence.
35. **`hex doctor` probes `hex` itself, and nothing injects it into the agent's
   `PATH`.** `hex emit` is a plain PATH lookup in the agent's own shell; it
   returned 127 in 3 of 3 real agent runs, and two only survived because codex went
   hunting for `target/debug/hex` on its own. Placing the binary on PATH is
   deliberately the operator's job, so the fix is a `self` row that *reports* the
   gap (exit 1) rather than a silent environment edit. It is **not** in
   `preflight`: a single-outcome node completes implicitly without ever calling
   `hex`, so refusing every run would block work that would have succeeded.
36. **A steer has two "sent but not applied" states, and `Inbox::queued()` must stay
   read-only.** A command sits in `control/inbox/` until the driver claims it at an
   attempt boundary (`hex status`: `queued steer (not yet picked up)`), and only then
   becomes a journaled `Steered` in `RunState.pending_steer` (`steer accepted
   (applies to the next agent attempt)`). Conflating them is what made a steer look
   dropped: the projection could not see the first state and the inbox no longer
   holds the second. `Inbox::queued()` therefore reads the directory and moves
   **nothing** into `done/` — a read that consumed a command would delete the very
   steer the operator was checking on. `hex steer` also prints, on stderr, that an
   in-flight attempt will not see it, since drain happens between attempts.
37. **An attempt's streams are more than its two logs, and a follower waits for a
   reserved run's journal.** `attempt_streams` (now in `hex-runtime`) collects the
   attempt's own stdout/stderr plus each numbered step dir's, in **declared**
   position order (`10-` after `9-`), because a `command` node writes nothing to
   the attempt dir and a gate is what you actually wait on. Attach at each stream's
   *tail*, not its head. Both followers call `wait_for_journal` (15s, `FOLLOW_POLL`
   400ms): `--detach` reserves the run dir before the driver's first write, so
   `hex logs --follow "$(hex run … --detach)"` would otherwise fail instantly —
   "not started yet" vs "does not exist" is told apart via `summary`, so a typo
   still fails fast. `Runtime::read_streams(run, attempt, &mut StreamCursor)`
   (`validate_run_id`'d, since an attempt id ends up in a path) hands back chunks,
   so no client learns the on-disk layout; it replaced the `attempt_dir` accessor,
   which handed one out. Attach/drain mechanics are gotcha 40.
38. **Preset session policy: the implementer continues, the reviewer stays fresh.**
   `context: continue` is declared on the node a loop revisits (`implement` in
   `critique-loop`/`implement-until-green`/`plan-build-review`/`tdd`/`checklist`,
   plus `tdd`'s
   `spec`, which `red` sends back), and deliberately *not* on `review` — a reviewer
   continuing its own session carries its earlier verdict into the next round, which
   is how a critic talks itself into approving what it already argued about.
   `autoresearch` stays all-`fresh` because its continuity is on disk
   (`.hex/research-notes.md`); `checklist` combines both patterns — a continuing
   implementer, but the loop state itself is the `[x]` marks in the checklist
   file, and its `final_review` is a *separate fresh node* from the per-item
   `review`, so the whole-change verdict comes from eyes that saw none of the
   per-item arguments. Consequence to remember before rebinding a role:
   `check_workers` **refuses at compile time** a `continue` node whose worker lacks
   `Capability::SessionResume` (gotcha 33), so pointing `implementer` at a
   `kind: command` worker makes these presets refuse to start until that node says
   `context: fresh`. Preset comments say so at the point of use — keep them there.
39. **A session's identity is the *program*, not the worker name.** The first
   version of this guard compared `SessionHandle.worker` (the registry name) with
   the node's worker, which looked right and was worthless: a role registers under
   its **own alias**, so `"implementer" == "implementer"` still holds after
   rebinding `roles.implementer.worker` from codex to claude — waving through
   `claude --resume <codex-thread-id>`, the exact bug the check existed for.
   `AttemptReported` now carries `agent` (the program, from `Worker::program()`,
   stamped by the shared plumbing in `run_agent` from `command[0]` — the argv
   actually spawned, so it cannot drift from the process that opened the session),
   `SessionHandle` is `{ agent, id }`, and the comparison is the testable
   `driver::resumable_id`. A mismatch runs **fresh**, not an error: the operator
   changed the binding, so a fresh session is what they asked for. A report naming
   no agent (an adapter that spawns nothing, e.g. the mock) is not resumable at
   all. General lesson: when a check compares two names, verify they cannot be
   equal for the wrong reason.
40. **A follower is driven by the journal, drains before switching, and reads
   exactly the bytes it accounts for.** Three separate bugs, one surface:
   (a) sampling `status().in_flight` every 400ms missed any attempt that started
   *and* finished between polls — a fast check wrote its failure and exited unseen,
   so the follower printed a disposition and none of the evidence. Iterate
   `AttemptStarted` events instead: append-only means each is seen exactly once,
   whenever it lands. (b) An attempt's last bytes are written after the last poll
   that could see it running, so `drain` runs both per-poll *and* before switching
   attempts. (c) The old cursor sampled a file's length, read to **EOF**, then
   stored the sampled length — anything a writer appended in between was printed
   and printed again next poll; `read_span(path, offset, len - offset)` reads the
   span it will account for. The attempt in flight at attach time joins at its tail
   (`StreamCursor::at_end`); later ones stream from byte zero
   (`StreamCursor::default`). `--follow` also honours `--node` and is
   `conflicts_with = "json"` — a flag a command accepts and ignores is worse than
   one it rejects.
41. **Every fold over reported usage saturates — no exceptions left.** These are
   numbers an agent printed and a journal replayed, so nothing here is trusted
   arithmetic, and a projection recomputed on every read turns one debug panic (or
   release wrap) into a permanently unreadable run. The first cost pass hardened
   `Usage::add` and missed three: `ModelUsage::tokens`, codex's multi-turn
   accumulator in `codex_report`, and both folds in the CLI's `event_summary`.
   If you add an arithmetic operator to a path fed by `AttemptReported`, it is
   `saturating_add`.
42. **A partly priced total says so.** `Totals.unpriced_reports` +
   `cost_is_partial()`; an attempt counts as under-priced when it reports **no**
   authoritative attempt total **and** at least one of its models named no price.
   The `and` is the point: it catches the *mixed* attempt where one model priced
   its share and another did not, because summing only the priced half and calling
   it the total is precisely the misreport. Rendering (`cost_cell`): `—` when
   nothing was priced, `≥ $X` when part was, plain `$X` when all was, plus a
   closing `cost is a lower bound: N attempt(s) reported tokens but no price`;
   `--json` carries `cost_is_partial`/`unpriced_reports` so a machine consumer gets
   the same caveat. This is the normal case in this repo, not an edge: this
   project's `.hex/config.yaml` binds reviewer→codex (tokens only) and
   implementer→claude (money). Still **no price table** — see gotcha 28.
43. **`budget.output_tokens` counts generation only.** Run-wide, so it is declared
   under `defaults: { budget: … }` (a node's `budget:` still takes `visits` only).
   A bound over every reported
   token is dominated by cached input (4.84M cache-read vs 209k fresh input and
   20.5k generated in a measured run of this repo), so it would be tuned to context
   size rather than to work done — and enabling `context: continue` would silently
   move it. Checked in `schedule` at the attempt boundary like every other budget,
   ending the run `budget_exhausted` (exit 4, no new disposition); worst-case
   overshoot is one attempt, itself bounded by `budget.attempt`. `output_tokens: 0`
   is `E-budget-zero`: spent before the first attempt, it would end a run that did
   nothing, which reads as a hex bug rather than a typo. **Untested end-to-end**:
   no fake worker reports usage, so enforcement is covered by kernel unit tests
   only (TODO.md).
44. **A diagram export must survive its renderer's syntax, not just hex's.**
   `hex graph --format mermaid|dot` (`hex-cli/src/graph_export.rs`) reads the same
   `Topology` the text renderer does, so the three renderings cannot disagree —
   but each target has a constraint the IR knows nothing about. Mermaid: node ids
   are unvalidated YAML map keys, so one can legally be `end` (a keyword that
   breaks the parser) or hold a dash or a space — every id is emitted as
   `n_<sanitized>` with the real id kept in the label, and a sanitizing clash
   takes a numeric suffix rather than silently merging two nodes into one box.
   Only the **classic** bracket delimiters (`[]`/`[[]]`/`{{}}`/`([])`) are used:
   GitHub's mermaid lags upstream and the `A@{ shape: … }` form needs 11.3+. A
   quote in a label is `#quot;`, since mermaid has no backslash escape. DOT: back
   edges and the `accept.on_unmet` reroute carry **`constraint=false`** — without
   it graphviz ranks the loop too and the happy-path spine bends around it.
   **A reroute is not always a back edge**, so both renderers ask `is_reroute()`
   *before* the edge class: the DFS classifies it `Forward` whenever its target is
   first discovered through the terminal it leaves (`done → fix`, `fix → implement`),
   and keying on `EdgeClass::Back` alone drew that reroute as an ordinary forward
   step. Gate-ness is derived once in `Graph::is_gate`; edge class once in
   `Topology` — but what a transition *means* still has to be read from the
   transition, not from where the traversal met it.
   Escaping is per *line*, before joining: escaping a DOT label's `\n` as data
   prints a literal backslash instead of breaking the line.
45. **Ctrl-C stops the run *and* the agent, and lands it in `paused`.** The
   agent runs in its own process group (gotcha 31), so the tty's SIGINT reaches
   only `hex` — before this, Ctrl-C left the agent spending tokens and writing
   the workspace with no journal, no logs and no notice, and `hex resume` would
   then run a *second* agent beside the live orphan. Now `hex-runtime`'s
   `interrupt::install` (signal-hook, already in the tree via crossterm; its
   `Signals` iterator delivers on a normal thread, so no `unsafe`) sets the
   process-global flag in `hex_worker::interrupt`, which the **existing polling
   wait** (`wait_bounded`) checks each 25 ms tick — the same tick that enforces
   deadlines, so the kill path is the proven `kill_group` one and no new
   plumbing carries it. A second press exits 130.
   The flag is in `hex-worker` because that is the lowest crate that must read
   it; the *policy* is in the runtime; the CLI only chooses to install it. What
   an interrupted attempt becomes: usage and salvaged output are journaled
   first, then `AttemptInterrupted` + `RunPaused` — **not** `AttemptFailed`,
   which would both misreport it (nothing failed) and make it unresumable (a
   failed attempt ends the run). `Paused` already had a producer, an exit code
   (6) and a continuation verb, so Ctrl-C invents no state. `wait_bounded` no
   longer blocks in `child.wait()` when `deadline_ms` is `None`, or an unbounded
   attempt would be uninterruptible. Pinned by
   `hex-runtime/tests/interrupt.rs`, in its **own** test binary because the flag
   is process-global — setting it in a shared file would kill unrelated tests'
   subprocesses (one flag-setting test per binary; the human-wait case below is
   `interrupt_human.rs` for the same reason).
   Two gaps a review closed after the first ship: (a) the **human-node wait**
   (`request_human`) is the one blocking path that never reaches `wait_bounded`,
   so it polls the flag itself and journals `RunPaused` directly (no
   `AttemptInterrupted` — a human node opens no attempt); unfixed, the stale
   flag survived the wait and paused the run — or killed the next attempt —
   *after* the human answered. (b) The CLI's interrupt banner routes through
   `LivePreview::notice` (the footer's own channel), never a raw stderr write
   from the signal thread: ratatui's inline viewport tracks the cursor from its
   own writes, so an interleaved foreign write desyncs the footer for the rest
   of the run.
   Related, and still true: `hex cancel` cannot stop an in-flight attempt (the
   inbox drains at attempt boundaries — gotcha 36), and cancelling a *crashed*
   run used to corrupt it, since `RunFinished` over an open attempt is rejected
   by `lifecycle`; `cancel` now writes `AttemptInterrupted` first, as `resume`
   does.
46. **A ratatui table renders cell text literally — it does not interpret ANSI.**
   `hex dash` (`hex-cli/src/dash.rs`) is a `Runtime` client like `hex runs`, but
   it cannot reuse `ui::mark`'s output: that returns an ANSI-escaped `String`,
   which ratatui would print as visible escape bytes. The shared semantics
   travel through the `Mark` enum instead — `Mark::glyph(unicode)` and
   `Mark::hue() -> Option<AnsiColor>` are the backend-neutral single source both
   the ANSI renderer (`ui::mark`) and the ratatui adapter (`dash::mark_cell` +
   `hue_to_ratatui`) derive from, so the two can never draw a different badge.
   Bold/dim are colour too: `dash` gates *every* style on `ui.colored()`, or
   `hex --color never dash` would still paint. Two lesser traps: the
   `CrosstermBackend<Stdout>` type alias must not be named `Backend` (it clashes
   with the `ratatui::backend::Backend` trait `flush` needs — alias is `Term`);
   and in raw mode the tty sends no SIGINT, so `Ctrl-C` arrives as a
   `KeyCode::Char('c')` + `CONTROL` key event, handled alongside `q`/`Esc`, not
   as a signal.
47. **`hex feedback` writes the *user-global* `~/.hex/`, not a project `.hex/`.**
   `hex-cli/src/feedback.rs` appends one JSON line per call to
   `$HOME/.hex/feedback.jsonl` so feedback from every project aggregates in one
   place — do not confuse it with the project-local `.hex/` (runs, graphs,
   config). It is a **pure CLI command with no Runtime**: only `$HOME`, the
   environment and the cwd, so it works from a worktree slot or a bare shell. It
   auto-captures run context from env the runtime injects into every agent
   attempt (`run_agent`, `agent.rs`): `HEX_RUN_ID`/`HEX_NODE_ID` already existed;
   this feature added `HEX_GRAPH` (from `WorkRequest.graph`), `HEX_PROJECT_ROOT`
   (from `WorkRequest.project_root`, which the driver derives as
   `run_dir.ancestors().nth(3)` since the journal is always at
   `<root>/.hex/runs/<id>` even under worktree isolation), `HEX_WORKTREE_BRANCH`
   (`hex/<run-id>` for a worktree run, else absent), and `HEX_AGENT` (the
   spawned `command[0]`, the same program `AttemptReported.agent` records —
   gotcha 39). The recorded **`location` is the real project root, never the
   worktree slot** — the slot is reclaimable, so a debugger would chase a path
   that no longer holds that run's code; the slot is recorded separately as
   `workdir`, and `branch` says what to `git checkout` in `location`. Paths are
   `canonicalize`d. Absent context is written as JSON `null`, never omitted, so
   the log has one fixed schema. The write is append-only, one `write_all` under
   `O_APPEND` — fine for short lines; add an fs4 lock only if long concurrent
   messages ever interleave.
48. **A red gate at the branch base wedges a scoped checklist run.** Dogfooded
   2026-09-05 (`2026-09-05-ponytail-cli`, claude-opus implements / pi
   gemini-3.8-flash reviews): the base commit carried unformatted
   `hex-worker/agent.rs`, so the worktree run's `verify` gate (fmt over the
   whole workspace) failed on files the checklist forbade touching; the
   implementer could not reconcile "fix the gate" with "touch no other crate"
   and the run burned to `budget_exhausted` — after all 11 items were
   implemented, reviewed and committed, so the branch merged fine anyway.
   Fix candidates are in `~/.hex/feedback.jsonl` and TODO.md: scope gate checks
   to the branch diff, or refuse to start a gated run whose gate is already red
   at the base. Cost datum from the same run: opus ≈ $2–3 per implemented item,
   gemini-flash ≈ $0.04–0.08 per review — the pi reviewer is ~50× cheaper than
   the codex reviewer this repo used before, with substantive verdicts.
   Reviewer binding note: pi needs the provider in the model pattern
   (`model: openrouter/google/gemini-3.8-flash`) because `PiWorker` passes only
   `--model` and bare `gemini-3.8-flash` is ambiguous across providers.
49. _add new gotchas here as they are discovered_
