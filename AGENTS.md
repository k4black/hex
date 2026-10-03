# hex — thin, deterministic control plane for agentic loops/graphs. Full architecture/setup → README.md

## Project in one glance

Rust CLI (edition 2024) that runs existing agent CLIs as opaque workers over a
deterministic, bounded graph, recording everything to an append-only journal.
Layered Cargo workspace, one binary (`hex`). Design rationale:
`docs/design/gpt-research-{1,2}.md`; the core rules below supersede the
research where they differ.

## Where things live

Dependencies point **strictly inward** (a crate may only depend on ones above
it in this table). One-liner: *kernel decides · worker runs one agent ·
runtime orchestrates and records · the cli is a window.*

| Crate | Purpose | May depend on |
|---|---|---|
| `hex-proto` | Versioned protocol: `Event`, `Command`, `Capability`. Only stable public surface. | — |
| `hex-kernel` | **Pure**: Graph IR, journal model, projections, `reduce` + `schedule` (acceptance is checked inside `schedule`). No IO/subprocess/clock. | proto |
| `hex-worker` | `Worker` trait + capability manifest + adapters (mock, subprocess, coding-agent presets). Runs **one** worker; never coordinates. | proto |
| `hex-runtime` | Imperative shell: drive loop, effect execution, journal writer, control ingestion, workspace isolation, run supervision, `init`, the embedded agent skill (`skill.rs`). Exposes the concrete `Runtime`, including `read_streams`, so tailing a live attempt needs no knowledge of `.hex/`. | kernel, worker, proto |
| `hex-cli` | The `hex` binary: thin client over `Runtime`; arg parsing + rendering only. | runtime |

A future transport or viewer is another thin client over `Runtime`, never a
second implementation.

## Commands

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets   # workspace lints: unsafe forbidden, clippy::all warn
cargo run   --bin hex                     # NOT `cargo run -p hex`: package is hex-cli, binary is hex
```

## Core rules

These are the design invariants that make hex *hex*. Violating them turns it
into another agent framework. They are non-negotiable.

1. **Dependency direction is inward.** `hex-kernel` must not import a worker
   adapter, rendering, or any client. The kernel stays testable without any
   model or subprocess.
2. **The journal is authoritative.** Every state change is an append-only
   `Event`. Current state is a projection *computed* from the journal
   (`fold(reduce, journal)`), never stored as a second source of truth. No
   mutable-database state.
3. **The kernel is deterministic; the worker is not.** The kernel emits
   *effect intents* (`StartAttempt`, `RunCommand`, `RequestHuman`, …); only the
   runtime performs them, writing intent-before-effect with idempotency keys.
   A model may *propose* an event only from its node's `may_propose`
   allow-list, via the `VERDICT: <signal>` line of its final message;
   the kernel still validates the transition. Routing spends no tokens.
4. **Node kinds stay tiny:** `agent`, `command`, `human`, `terminal`. Roles
   ("planner", "reviewer") are metadata on an `agent` node; interactivity is a
   *policy flag* on `agent` (`interactive: true`), not a new kind. A **gate is
   a role too**: a node is a gate because `accept.require` names its signal
   (`accept.require: [test.passed]`, and equally `[review.approved]` for an
   agent).
5. **Every cycle is bounded, by construction.** Every non-terminal node has a
   visit bound, and `schedule` checks it before acting. An
   unbounded cycle cannot be constructed, so there is no cycle analysis.
6. **Completion is provisional.** A worker's "done" is a proposal; required
   gates + acceptance rules decide the run outcome. Deterministic evidence
   outranks model assertions.
7. **Two execution verbs only.** `run` starts a new run; `resume` continues
   the same run from its journal (after pause *or* crash). There is no
   `retry`/`replay`/`skip`: redoing work is a new run. Never silently rerun a
   side-effecting attempt.
8. **Human and agent share one control protocol**, with authority scoped per
   actor. One `Command` type over the file inbox; an agent node's *routing*
   channel is its final message's verdict line (core rule 3). Every surface
   (CLI, `--json`, any later transport) is a thin client over the runtime: it
   parses arguments and renders, and the runtime computes every fact it shows.
   The rule is *no parallel implementation*; it needs no client trait (the
   one-impl `RuntimeClient` trait was deleted).
9. **The worker adapter never coordinates.** Sub-agents, watchdogs, fan-out
   are kernel-routed / runtime-scheduled graph constructs, or the external
   agent's own internal business. None of it is logic inside `hex-worker`.

## Code style

- Prefer `argv` execution, NOT shell strings, for subprocess workers.
- Machine output: stdout carries requested data only, diagnostics to stderr,
  stable exit codes, `--json`/NDJSON, no interactive prompts in machine mode.
- Comments and docs stand alone: never cite a gotcha by number. The gotcha
  list below is for agents; renumbering it must not break any other text.

## Terminology

Five kernel entities: **Graph, Run, Event, Budget, Artifact.** Everything else
is a node kind, an event type, an adapter, or a derived view.

**Graph**: An immutable, versioned workflow definition (nodes + edges) a run
executes. Bounded cycles allowed. _Avoid_: workflow, DAG, pipeline.

**Node**: One schedulable unit with a single kind and one execution policy.
_Avoid_: step, stage, task, hat, role.

**Edge**: A legal transition from one node to another, activated by a named
event and carrying an ordered condition. The model does not choose control
flow. _Avoid_: link, arrow (a *transition* is the act; the edge is the rule).

**Run**: One execution of a graph, referencing an exact graph snapshot/hash,
ending in one terminal disposition. _Avoid_: job, session, loop.

**Attempt**: One execution of one node, with a unique id, a bound, and a start
+ terminal event. _Avoid_: run, try, iteration.

**Event**: An append-only fact in the journal (versioned, sequenced, actored).
The unit of truth. Approvals, gate results, budget spend, artifact refs are
all event types, not separate entities. _Avoid_: log line, message.

**Effect (intent)**: A description of an external action the kernel wants
performed (`StartAttempt`, `RunCommand`, `RerouteUnmet`, …). The kernel emits
it; the runtime executes it. _Avoid_: command (reserved for operator
`Command`s), task.

**Worker**: *Our adapter* in `hex-worker`. It wraps one opaque external agent
behind the capability-declaring `Worker` trait. A graph names a **Role**,
which binds a worker. _Avoid_: backend (retired term), provider, driver.

**Agent**: The opaque external process a Worker drives (Claude Code, Codex, a
script). Also the node kind that invokes one. _Avoid_: calling our adapter an
agent, or the agent a worker.

**Gate**: A *role*, not a kind: a `command` node whose signal is named in
`accept.require`, so its deterministic `passed`/`failed` verdict feeds
acceptance. _Avoid_: calling it a node kind; hook.

**Check**: A named argv in project config (`checks:` in `.hex/config.yaml`),
referenced by a graph as `command: { check: <name> }` and resolved at
**compile** time. **Empty by default**: what "green" means is per-project. A
graph naming an undeclared check is **refused before the run starts**.
_Avoid_: gate (a check is what a gate node *runs*).

**Role**: The user-facing unit a graph names (`role: reviewer`):
implementer / reviewer / planner / researcher. A role binds a **worker** to a
`model`, `effort`, `read_only` policy and `prompt` preamble; two roles can
share one CLI and differ in everything else. Defined in layered config,
deep-merged per key, so a project overriding `roles.reviewer.model` inherits
the rest. _Avoid_: mode, persona, agent (the agent is the external process),
and **orchestrator** (routing is structural, core rule 9).

**Artifact**: A large or binary output/evidence/diff a node produces, stored
by content hash outside the event payload. _Avoid_: output, file, blob.

**Budget**: A durable limit and its consumption (visits/time/tokens); it does
not reset on resume. _Avoid_: quota, cap, limit (a limit is one field of a
budget).

**Projection**: A read model *computed* from the journal, never stored
authority. _Avoid_: state, cache, snapshot (the atomic snapshot file is one
*kind* of projection, an optimization).

**Operator**: Any actor (human or agent) issuing control commands, with
scoped authority. _Avoid_: user, supervisor, controller.

**Preset**: A named, parametrized graph in the library, resolved project
(`.hex/graphs/`) > user (`~/.config/hex/graphs/`) > built-in; invoked as
`hex run <preset> -p "<prompt>"`. _Avoid_: pipeline (banned Graph synonym),
template (reserved for `templates:` node reuse inside a graph).

**Interactive session**: An `agent` attempt with `interactive: true` that
stays open for live human↔agent conversation, journaled per turn, resumable.
Not built. Requires `session_resume` plus a `live_steering` capability (added
when this ships).

**Approval**: A blocking human decision on a finished proposal, recorded as
`human.requested`/`human.responded` events with actor + rationale. Today a
`human` node answered by `hex respond`; approve/reject routing is
not built. _Avoid_: sign-off, confirmation.

## Gotchas

1. Binary is `hex`, package is `hex-cli`: use `cargo run --bin hex`, not `-p hex`.
2. **What exists:** TODO.md lists what is open, what is broken and what is
   not built. The README's first ```yaml block must stay a valid graph:
   `the_readme_example_graph_is_valid` loads it. Not built: `interactive`,
   `templates:`/`extends:`, capability matching beyond session-resume and
   result capture.
3. A node's routing token is an `EventBody::Signal { name }`. Agent proposals
   *and* command verdicts (`passed`/`failed`) unify there; edges match on
   `name`. `reduce(graph, state, event)` owns routing; `schedule(graph, state,
   now_ms)` only emits `Effect` intents and checks acceptance; the runtime
   `Session` executes them.
4. Surface syntax is **standard YAML only**: kind-as-key + `on:` map,
   co-located edges, inline block-scalar prompts. The kernel models the
   compiled IR only; the loader lives in `hex-runtime`. YAML via `yaml_serde`
   (the maintained serde_yaml fork). `worker:` is a serde alias of `role:` on
   a node and in `defaults:`; giving both is serde's duplicate-field error.
5. **Operator input is one prompt.** The CLI takes only `-p/--prompt <text>`
   or `-f/--file <path>`, filling `{{prompt}}` in node prompts. There is no
   `--input k=v`. Named typed inputs/outputs are a **node** concern (internal
   dataflow). Run-config (budget, worker, isolation) stays on its own flags.
6. **The agent→runtime routing channel is the verdict line.** A multi-outcome
   agent node ends its final message with `VERDICT: <signal>`.
   - The runtime generates the instruction from `may_propose`
     (`driver::verdict_instruction`, appended at attempt start). A graph never
     authors it.
   - `driver::verdict_of` parses it: last marker wins, case-insensitive,
     decoration-tolerant. A declared verdict routes; an undeclared or missing
     one fails the attempt closed, unless an `on: { unknown: … }` edge routes
     it.
   - `run_agent` parses the **raw** capture, before `cap_result`. `cap_result`
     keeps the head of a >16 KiB message and drops the tail, where the verdict
     lives. Pinned by `a_verdict_after_a_very_long_message_is_still_found`.
   - `unknown` and `done` are reserved: never in `may_propose`
     (`E-unknown-reserved`, `E-done-reserved`), an edge on them needs no
     declaration.
7. **Result capture & implicit completion.** The runtime injects
   `HEX_RESULT_FILE` and a `{result}` argv token. A worker's `result:`
   (`file`|`jsonl_result`|`jsonl_last_text`|`pi_jsonl`|`text`) says how to
   capture its final message; `text` is the tail of `stdout.log`, the mode a
   plain `kind: command` worker printing a verdict uses. The capture becomes
   `EventBody::NodeResult`, folded into `RunState.results`; a downstream prompt
   reads it as `{{node.result}}`, interpolated **at attempt start** and fenced
   as untrusted. A clean exit on a node with **no** declared outcomes yields
   the reserved `done` signal, so that node needs an `on: { done: … }` edge.
   Mixing `may_propose` with a `done` edge is `E-mixed-done-and-proposals`.
8. **Workers are typed adapters behind one trait.** `CodexWorker`,
   `ClaudeWorker`, `PiWorker`, `OpencodeWorker` each own their argv and
   result-capture mode; `CommandWorker` is the generic argv escape hatch
   (+ `MockWorker`). Config selects one via `kind:` (default `command`); the
   runtime/CLI see only `dyn Worker`. Adding an agent = a new `Worker` impl +
   `WorkerKind`.
   - `read_only` is **advisory** (prompt-enforced). A real read-only sandbox
     would block the agent from writing `HEX_RESULT_FILE` and lose the verdict.
   - pi needs the provider in the model id
     (`openrouter/google/gemini-3.8-flash`): `PiWorker` passes only `--model`,
     and a bare id is ambiguous across providers.
9. **Headless permissioning is per-agent, never a blanket bypass.** codex runs
   under `--sandbox workspace-write`. claude runs with `--permission-mode
   acceptEdits` + `--allowedTools` (incl. `Bash`) + `--disallowedTools`
   denying destructive/exfil/publish commands; never
   `--dangerously-skip-permissions`. opencode is still `--auto` (TODO). The
   claude deny-list is prefix matching, so shell chaining bypasses it; it is
   defense-in-depth, not an OS boundary. Lists live in `ClaudeWorker`
   (`agent.rs`).
10. **Run ids** are `yyyy-MM-dd-<workflow>-<short-uuid>`, or `yyyy-MM-dd-<name>`
   with `hex run --name`; slugged, a same-day clash gets `-2`.
   `validate_run_id` allows `[a-z0-9-_]` only.
11. `hex run`'s workspace is the project cwd (shared isolation); run it from
   the repo root. `resume` marks an orphaned attempt `interrupted` before
   re-attempting it.
12. **Worktree isolation is per-run and opt-in** (`hex run --worktree
   [<base>]`), default shared. hex does not merge: the branch `hex/<run-id>`
   is left for explicit integration, and hex does not commit (the driver asks
   the agent to).
   - `hex-runtime/src/worktree.rs` leases a pooled slot under
     `.hex/worktrees/<n>/`, fs4-locked like a run. Clean slots are reused;
     dirty ones are reclaimed and logged. `hex prune` releases slots.
   - The journal, the result file and the control inbox stay in the main
     `.hex/runs/<id>`. codex gets `--add-dir <run_dir>` so its sandbox can
     still write them.
   - `--worktree-init "<argv>"` warms a fresh or reclaimed slot.
   - Design doc: `docs/design/2026-07-21-worktree-isolation.md`.
13. **Checks come from project config; naming an undeclared one is refused.**
   `.hex/config.yaml`'s `checks:` supplies the argv and is empty by default.
   An undeclared check is a compile error naming the key to add. Built-in
   presets ship gate-free, except `tdd` and `implement-until-green`, which
   refuse to start until you declare `checks.test`. The resolved argv is
   recorded in `RunCreated.checks`, so editing config does not change what a
   resumed run executes.
14. **Every attempt is bounded.** The loader sets `Budget.attempt_elapsed_ms`
   to `DEFAULT_ATTEMPT_ELAPSED_MS` (30m) unless `budget: { attempt: 20m }`.
   `attempt_deadline()` takes whichever of the run and attempt bounds bites
   first. `hex graph` prints durations via `humantime` (`1h 30m`).
15. **A broken check is not a failing test.** `run_process` returns `Err` for
   an infrastructure failure (spawn/log/kill) and `Ok(false)` only for a real
   non-zero exit; only the latter routes `failed`. `doctor::preflight` (run by
   `run` and `resume`) refuses to start a run whose agent CLI is missing, or
   whose model the CLI's own catalog does not list (`Worker::model_probe` +
   `model_verdict`: `codex debug models`, `opencode models`; pi's model check is
   its `auth_probe`). A catalog read spends no tokens. An unreadable catalog
   decides nothing. claude has no free catalog, so its model is not checked.
16. **Exit codes encode the disposition:** 0 succeeded · 1 failed · 2 usage
   error (clap) · 3 timed out · 4 budget exhausted · 5 cancelled · 6 paused.
   `hex wait` also uses 1 for an abandoned run.
17. **The kernel says why a run ended.** `Effect::RecordTerminal` carries
   `why: Option<String>`; the driver journals it as a `Note` before
   `RunFinished`, and `StatusReport.why` exposes it. It names the node whose
   visit bound ran out, or the evidence an unmet `accept.require` lacked.
18. **Roles are the graph's vocabulary; workers are plumbing.** `roles:` bind a
   `worker` plus `model`/`effort`/`read_only`/`prompt`. `prompt:` replaces an
   inherited preamble; `prompt_append:` extends it.
   - `Workers::from_config` registers workers first, then roles, so a role
     shadows a same-named worker. A scratch `kind: command` worker named
     `implementer` silently runs the role's real agent. Name test workers
     something that is not a role.
   - A role's preamble is prepended at compile time. `resume` recompiles
     against the **current** `roles:` config (`Runtime::verify_and_fold`). A
     live run keeps what it compiled; pause or stop it, then `hex resume`, to
     pick up changed roles.
19. **The built-in config layer is `hex-runtime/src/defaults.yaml`**, embedded
   via `include_str!` and parsed by the same loader and merge path as
   user/project config. There is no hardcoded `Config::builtin()`.
20. **A command node runs N steps with a mode.** `ordered` (default) stops at
   the first failure; `parallel` runs every step and fails if any did. Each
   step writes `attempts/<id>/<n>-<label>/`, numbered in **declared** order,
   plus an `exit` file with its status (`"0"`, `"101"`, `"signal"`).
   `StepLog::failed()` treats an unrecorded status as not-failed. Infra
   failure in any step fails the attempt.
21. **`accept.on_unmet` routes instead of dead-ending.** At a success terminal
   with missing evidence, the kernel emits `Effect::RerouteUnmet` and the
   runtime journals `EventBody::AcceptanceUnmet`, its own event. It is not a
   synthesized `Signal`: `reduce` drops routing signals no in-flight attempt
   produced, and that fail-closed guard stays. Without `on_unmet` the run
   fails with a reason naming the remedy.
   - The implicit terminal → node edge lives only in
     `Graph::implicit_reroutes()` / `implicit_reroute_from()`, used by
     `schedule` and `Topology`. Target existence is enforced by
     `E-accept-unmet-node` and `lifecycle::unmet_reroute_ok`.
   - Read whether a transition is a reroute from `is_reroute()`, never from
     where a traversal met it (`EdgeClass`).
22. **Every non-terminal node is visit-bounded; there is no run-wide retry
   budget.** The loader fills `max_visits` with `DEFAULT_NODE_VISITS` (5)
   unless the node declares `budget: { visits: N }`. Exceeding it ends the run
   `budget_exhausted` with a `why` naming the node. A terminal keeps
   `max_visits: None`; `schedule` settles terminals before budget checks.
   `elapsed`, `attempt` and `output_tokens` stay run-wide. `visits: 0` is
   `E-budget-zero`. `budget: { attempts: N }` is serde's unknown-field error. A
   run paused before that change with `attempts:` in its snapshot fails to
   resume; redo it as a new run.
23. **The control inbox is `.hex/runs/<id>/control/{tmp,inbox,done}/`.** A
   controller writes to `tmp/`, `sync_all`s, then `rename()`s into `inbox/`,
   so a poller never reads a torn write. The driver drains the inbox before
   each `schedule()`, moving a file into `done/` *before* applying it
   (at-most-once: losing a `steer` is cheaper than double-applying a
   terminal). Files sort as `{now_ms:013}-{seq:010}-{uuid}.json`; the
   per-process `seq` keeps one sender's same-millisecond commands in order.
24. **`pause` returns without a terminal.** `Session::drive()` returns
   `Option<Disposition>`; `None` means paused and no `RunFinished` was
   written, so `hex resume` continues the run. Pause applies only at an
   attempt boundary.
25. **`human` nodes.** `RequestHuman` journals `HumanRequested`, then polls the
   inbox at 250ms bounded by `attempt_deadline()`. `HumanResponded` stores the
   answer as the node's result. A human node has exactly one outgoing edge
   (`E-human-no-edge`/`E-human-multi-edge`), since an answer is text, not a
   signal. Downstream the answer is fenced as operator input (pinned by
   `a_human_answer_is_labelled_operator_input_not_agent_output`).
26. **Liveness is the `RunLock`, and the lock has an unexplained fs4 flake.**
   - Liveness = `RunLock` (released by the kernel on process death, unlike a
     pidfile) + a 5s heartbeat file, folded into `Liveness`
     (`live`/`interrupted`/`finished`/`error`, `control.rs`). A stale heartbeat
     under a held lock is the *hung* diagnostic on `live`.
   - `hex run` blocks; background it with `&` or tmux. A background mode, if
     added, must re-exec, not `fork()` (unsafe in a multithreaded process).
   - The flake: a just-released lock intermittently reads as busy, on macOS,
     for both run locks and worktree slots. Not root-caused.
   - `Runtime::cancel` reads the journal before probing the lock, so a
     finished or paused run avoids the flaky probe.
   - Worktree slot leasing still has it: a run can take a needless extra slot.
     `worktree.rs`'s leasing tests run behind a test-only mutex, so the
     concurrent path is untested. Diagnose (thread id + realpath + inode per
     lock attempt) before trusting it; do not add another mutex.
27. **`hex-kernel/src/lifecycle.rs` is the single home for every "may this
   event apply here?" rule.** `lifecycle::admissible` returns each guard's
   verdict and reason; `reduce` drops an event on `Err`, and `check_journal`
   (which folds *through* `reduce`) reports it as `E-journal-lifecycle`. Add a
   guard in one predicate. The module doc lists the deliberate asymmetries;
   keep it current.
28. **Money is integer micro-USD, and usage arithmetic saturates.** `Event`
   derives `Eq`, which an `f64` breaks, and a float can change value on
   replay. `$0.1778` is `177_800`; `ModelUsage.cost_micro_usd` and
   `Totals.cost_micro_usd` are the only spellings, rendered by the CLI's
   `usd()` to four decimals. Every fold over `AttemptReported` data uses
   `saturating_add`: an overflow panic in a projection makes a run unreadable.
29. **hex does not estimate a cost.** Each adapter parses its agent's own
   output: codex `exec --json` (`thread.started.thread_id`,
   `turn.completed.usage`; tokens only, no model or cost), claude's
   `--output-format json` (`modelUsage`, `total_cost_usd`). There is no price
   table. Parsers are tested against captured real output in
   `crates/hex-worker/tests/fixtures/`. codex's `input_tokens` *includes*
   `cached_input_tokens`, so the fresh share is the difference; claude reports
   cache tokens beside `inputTokens`. `reasoning_tokens` is a subset of output
   and is not added into `tokens()`.
30. **`AttemptReported` is recorded before anything that can end the
   attempt**, at most once per attempt. `reduce` clears `current_attempt` when
   a signal routes, so a later report is dropped. A duplicate would inflate
   the summed bill, so `lifecycle::attempt_report_ok` + `RunState
   .reported_attempt` refuse it. `Usage::attempt_cost` takes the attempt total
   when reported, else the sum of per-model costs, never both.
31. **A partly priced total says so.** An attempt is under-priced when it
   reports no attempt total **and** at least one model named no price
   (`Totals.unpriced_reports`, `cost_is_partial()`). `cost_cell` renders `—`
   (nothing priced), `≥ $X` (part), `$X` (all); `--json` carries
   `cost_is_partial`/`unpriced_reports`. A codex role always produces this.
32. **`budget.output_tokens` counts generation only**, run-wide under
   `defaults: { budget: … }`. Cached input dominates total tokens, so a bound
   over all tokens would track context size. `schedule` checks it at the
   attempt boundary (overshoot ≤ one attempt); `0` is `E-budget-zero`. Covered
   by kernel unit tests only (TODO.md).
33. **Every run ends with `RunFinished`, and `AttemptFailed` still carries the
   disposition.** `AttemptFailed` stays one atomic terminal fact; the trailing
   `RunFinished` gives consumers one terminator to tail for. `check_journal`
   allows at most one trailing `RunFinished`, with a disposition equal to the
   recorded one; its absence stays legal for older journals.
34. **A killed attempt takes its process group.** `logged_command` spawns with
   `process_group(0)`; `wait_bounded`'s deadline path sends `killpg(SIGTERM)`,
   waits 2s, then `SIGKILL`, through `nix` (no `unsafe`). `child.kill()` alone
   left grandchildren running. Pinned by
   `a_timed_out_attempt_kills_the_whole_process_group`.
35. **A dead attempt's output is salvaged.** `run_agent` captures the result
   and usage before branching on the outcome. With no final message, the
   8 KB tail of stdout and stderr becomes the node's result, prefixed
   `[partial output — …]`. It shows in `hex logs`; it does not reach a
   downstream `{{node.result}}`, because `fail_attempt` ends the run.
36. **`context: continue` resumes *that node's* session.** The handle is
   `RunState.sessions[node]`, folded from `AttemptReported.session_id`, so it
   survives a crash and `hex resume`. It is per node so a reviewer never
   inherits the implementer's reasoning. `check_workers` refuses the node at
   compile time unless its worker declares `Capability::SessionResume` (codex,
   claude, pi). `codex exec resume` lacks `--sandbox`/`--add-dir`/`--cd`; they
   become `-c sandbox_mode="workspace-write"` and
   `-c sandbox_workspace_write.writable_roots=[…]` (verify keys with
   `--strict-config`, which costs no tokens).
37. **Preset session policy: the implementer continues, the reviewer stays
   fresh.** `context: continue` is on the node a loop revisits (`implement` in
   `critique-loop`/`implement-until-green`/`tdd`/`checklist`, plus `tdd`'s
   `spec`), not on `review`: a continuing reviewer argues itself into its old
   verdict. `autoresearch` is all-`fresh` (its state is
   `.hex/research-notes.md`). `checklist` keeps state in the checklist's `[x]`
   marks, and its `final_review` is a separate fresh node. Binding
   `implementer` to a `kind: command` worker makes these presets refuse to
   start until the node says `context: fresh`.
38. **A session's identity is the *program*, not the worker name.** A role
   registers under its own alias, so `"implementer" == "implementer"` holds
   after rebinding it from codex to claude. `AttemptReported.agent` records
   `command[0]` (stamped in `run_agent`); `SessionHandle` is `{ agent, id }`;
   `driver::resumable_id` compares them. A mismatch runs fresh. A report with
   no agent (the mock) is not resumable.
39. **A steer has two "sent but not applied" states.** In `control/inbox/`
   until the driver claims it (`hex status`: `queued steer (not yet picked
   up)`), then a journaled `Steered` in `RunState.pending_steer` (`steer
   accepted`). `Inbox::queued()` must stay read-only: moving a file to `done/`
   would delete the steer the operator is checking. An in-flight attempt does
   not see a steer; `hex steer` says so on stderr.
40. **Following a run.** `Runtime::read_streams(run, attempt, &mut
   StreamCursor)` hands back chunks, so no client learns the on-disk layout.
   The live preview holds no run id, so it reads the same streams through the
   `hex_runtime::AttemptStreams` handle (`AttemptView.streams`,
   `AttemptStreams::read`). A read that ends mid-codepoint holds the trailing
   partial UTF-8 bytes for the next read.
   - `attempt_streams` covers the attempt's stdout/stderr plus each step dir,
     in declared order (`10-` after `9-`); a `command` node writes only step
     dirs.
   - The follower iterates `AttemptStarted` events, not a sampled
     `status().in_flight`, so a fast attempt is not missed. It drains per poll
     and before switching attempts.
   - `read_span(path, offset, len - offset)` reads exactly the span it
     accounts for; reading to EOF double-prints concurrent appends.
   - The attempt in flight at attach joins at its tail
     (`StreamCursor::at_end`) and prints `… N earlier line(s)`; later ones
     stream from byte zero.
   - `wait_for_journal` (15s, `FOLLOW_POLL` 400ms) covers a run dir that exists
     before its first event. `--follow` honours `--node` and conflicts with
     `--json`.
41. **Ctrl-C stops the run and the agent, and lands the run in `paused`.** The
   agent has its own process group, so SIGINT reaches only `hex`.
   - `hex_runtime::interrupt::install` (signal-hook, no `unsafe`) sets the
     process-global flag in `hex_worker::interrupt`; `wait_bounded` checks it
     each 25ms tick and kills the group. A second press exits 130.
   - The attempt journals usage and salvaged output, then
     `AttemptInterrupted` + `RunPaused`. It is not `AttemptFailed`, which would
     end the run.
   - `request_human` polls the flag itself and journals `RunPaused` directly.
   - Tests that set the flag need their own test binary
     (`tests/interrupt.rs`, `tests/interrupt_human.rs`), or they kill other
     tests' subprocesses.
   - The CLI banner goes through `LivePreview::notice`. A raw stderr write
     desyncs ratatui's inline viewport. A ratatui cell renders ANSI escapes as
     visible bytes, so share styling through `ui::Mark`, not rendered strings.
   - `hex cancel` does not stop an in-flight attempt (the inbox drains at
     boundaries). On a crashed run it writes `AttemptInterrupted` first, since
     `lifecycle` rejects `RunFinished` over an open attempt.
42. **`hex feedback` writes the *user-global* `~/.hex/feedback.jsonl`**, not a
   project `.hex/`. `hex-cli/src/feedback.rs` uses no `Runtime`: only `$HOME`,
   env and cwd. It captures `HEX_RUN_ID`, `HEX_NODE_ID`, `HEX_GRAPH`,
   `HEX_PROJECT_ROOT`, `HEX_WORKTREE_BRANCH` and `HEX_AGENT`, which
   `run_agent` injects into every attempt. `location` is the real project
   root (the driver derives it as `run_dir.ancestors().nth(3)`), never the
   reclaimable worktree slot, which goes in `workdir`. Absent context is JSON
   `null`, so the schema is fixed. One `write_all` under `O_APPEND`.
43. **A red gate at the branch base wedges a scoped run.** If the gate check
   (e.g. `cargo fmt --check` over the workspace) already fails at the base,
   an implementer told to touch only its scope cannot turn it green. Stall
   detection (`failure_signature` in `driver.rs`) ends the run `failed` after
   two identical failing outputs. Fix candidates are in TODO.md.
44. **The agent skill is embedded and version-stamped.** `skill.rs` embeds
   `crates/hex-runtime/skill/hex/` (`skills/hex` is a symlink to it).
   `hex skill install` writes it to `~/.claude/skills/hex/` and
   `~/.agents/skills/hex/` and stamps `metadata.hex-version` into SKILL.md.
   - A stamped copy is hex-owned. `run`, `resume`, `init` and `doctor`
     rewrite one whose stamp differs from the binary. The refresh never
     installs and never fails the verb.
   - A symlink (the dev checkout) is never touched. An unstamped copy is
     skipped unless `install --force`.
   - A new skill file needs its own `include_str!` entry in `skill::FILES`.
   - doctor's `skill` rows are informational: `Report::ok` ignores them.
45. _add new gotchas here as they are discovered_
