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
  enforcement; returns **effect intents** (`StartAttempt`, `RunCommand`,
  `RequestHuman`, `RecordTerminal`), never performs them. `RecordTerminal`
  carries *why* the run ended, so `budget_exhausted` and an unmet
  `accept.require` explain themselves instead of arriving bare.
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

Four node kinds — roles ("planner", "reviewer") are metadata, never kinds:

`agent` · `command` · `human` · `terminal`

A **gate is a role, not a kind**: any `command` node whose signal is named in
`accept.require` is acting as one. (`gate` and `command` had identical fields and
one shared executor, so they collapsed into `command`.)

- Interactivity is a **policy flag** on `agent` (`interactive: true` = a live,
  resumable session the human converses with — grill-me/Q&A style; requires
  the worker to declare `live_steering` + `session_resume`). Not a new kind.
- Approval is a `human` boundary node: it journals `human.requested`, blocks on
  the control inbox, and records `human.responded` with the actor and text.
- A `command` node runs one or more argv steps (`mode: ordered|parallel`) and
  yields `passed`/`failed`; naming that signal in `accept.require` is what makes
  it a gate.

## Graph surface — single YAML file

Standard YAML only (no custom grammar — easy for humans *and* agents to
write). Edges co-located on the node via an `on:` map, prompts inline as block
scalars, `defaults:` for repetition. The loader uses `deny_unknown_fields`, so a
typo is an error rather than a silent no-op. (`templates:`/`extends:` and
reusable run-level `gates:` are *not* implemented.)

```yaml
version: 1
name: implement-until-green

defaults:
  worker: codex
  budget: { attempts: 8, elapsed: 30m, attempt: 10m }

entry: implement

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
    # Runs this project's `checks.test` (see Config below). If the project
    # declares none, the run is refused with the key to add.
    command: { check: test }
    on: { passed: done, failed: implement }

  clarify:
    human:
      prompt: "Discuss the blocker with the operator; agree on a decision."
    on: { resolved: implement }

  done:
    terminal: succeeded

accept:
  require: [test.passed]
```

> This example validates and runs as-is. Reaching `clarify` blocks the run until
> you answer it with `hex respond <run> "…"`, from any terminal or from a driving
> agent; the answer becomes that node's result, so a downstream prompt can
> reference `{{clarify.result}}` exactly like an agent's.


The kernel compiles any surface form to a flat, immutable Graph IR
(nodes + typed edges + bounds); a run records the exact snapshot + hash. Every
cycle must declare a bound — an unbounded cycle is a validation *error* — and so
is a graph with no reachable `terminal: succeeded`, which could only ever fail.

**Bounds** are layered: `budget.attempts` and `budget.elapsed` bound the run,
`budget.attempt` bounds a single attempt (always set, so a hung agent can never
block forever), and a per-node `budget: { visits: N }` bounds *one* loop — cap a
review cycle at 3 rounds without also capping a cheap lint cycle.

**Acceptance** can route rather than dead-end. `accept.on_unmet: <node>` sends a
run that reached a success terminal without the required evidence back to earn it;
without it the run fails, but the reason names the remedy.

## Config & presets

Layered config, project wins: `~/.config/hex/config.yaml` (user) ←
`.hex/config.yaml` (project). It holds the **worker registry** plus default
budgets/context. Each worker has a `kind`: a typed built-in adapter
(`codex`/`claude`/`opencode`) that encapsulates that agent's argv, output
parsing, and read-only flag — you only override its `model` — or the generic
`command` kind (an explicit argv template + a `result:` capture mode) for any
other CLI. The CLI/runtime only ever see a uniform `Worker`:

```yaml
# Workers are the CLI adapters — internal plumbing. A graph never names one.
workers:
  codex:  { kind: codex }
  claude: { kind: claude }

# Roles are what a graph names. Each binds a worker to a model, reasoning
# effort, a read-only policy and a prompt preamble.
roles:
  implementer: { worker: codex, effort: high }
  reviewer:
    worker: claude          # cross-model: a different model reviews than wrote
    read_only: true
    prompt_append: "Only flag correctness and security issues."

# What "green" means in THIS project. Empty by default.
checks:
  test: [pytest, -q]
  lint: [ruff, check, .]
```

Layers are **built-in → `~/.config/hex/config.yaml` → `.hex/config.yaml`**, and
they **deep-merge per key**: overriding `roles.reviewer.model` inherits that
role's worker, effort, policy and prompt. `prompt:` replaces an inherited
preamble; `prompt_append:` extends it. The built-in layer is a real YAML file
([`defaults.yaml`](crates/hex-runtime/src/defaults.yaml)) embedded in the binary
and parsed by the *same* loader — there is no hardcoded copy to drift from it.

Four roles ship: `implementer`, `reviewer`, `planner`, `researcher`. There is
deliberately **no orchestrator role** — routing is structural (the graph's edges),
never a token-spending agent.

### Checks: the gate is yours, and there is none by default

A graph names a check (`command: { check: test }`); your project supplies the
argv. A check you have **not** declared is a hard error naming the key to add —
never a silent pass, because a run reporting `succeeded` having verified nothing
is the failure mode worth designing against.

So the built-in presets ship **gate-free** and run in any repo with no setup. The
two exceptions are `tdd` and `implement-until-green`, whose gate *is* the preset;
they refuse to start until you declare `checks.test`.

A command node runs one step or several, and the modes differ in *failure*
semantics rather than only concurrency:

```yaml
  verify:
    command:
      check: [test, lint, typecheck]
      mode: parallel      # run everything, fail if any failed
    on: { passed: done, failed: implement }
```

- `ordered` (default) runs in sequence and **stops at the first failure** — a
  later step is usually pointless once an earlier one fails.
- `parallel` runs concurrently and **runs every step**, so one round surfaces
  every problem and the agent fixes them together. Each step's output is buffered
  to `attempts/<id>/<n>-<label>/`, numbered in **declared** order, so concurrent
  evidence reads exactly like sequential evidence.

`hex doctor` reports whether every configured worker and check can actually run,
and `hex run` refuses to start when the graph needs an agent CLI that is not on
`PATH` — rather than discovering it as a failed first attempt.

```bash
hex doctor
#   ok      worker  codex    /opt/homebrew/bin/codex
#   MISSING check   lint     `ruff` not found on PATH
```

A **preset** is a named graph resolved through three layers: `.hex/graphs/`
(project) > `~/.config/hex/graphs/` (user) > built-in. The built-in library
(the `reviewer` role runs a different agent from `implementer`, so a different
model reviews than wrote the code):

| Preset | Loop | Needs a check? |
|---|---|---|
| `critique-loop` | implement → review, until approved (the flagship) | no |
| `plan-build-review` | plan → implement → review | no |
| `review` | reviewer over the current `git diff`, no implementer | no |
| `implement-until-green` | implement → test (the Ralph loop) | **`checks.test`** |
| `tdd` | write-failing-test → prove red → implement → prove green | **`checks.test`** |

The first three are gate-free, so they run in any repo with no setup — the
reviewer is the backpressure. The last two exist *to* run your tests, so they
refuse to start until you declare `checks.test`. A preset is a starting point:
copy it out and edit it to change the topology, or just redefine a role in your
config to change how every preset behaves. `hex list` shows each graph with its origin, description, and an example
invocation; a custom graph can set optional top-level `description:` and
`example:` fields to appear the same way.

The operator supplies exactly one thing — the **prompt** — inline or from a
file; it fills `{{prompt}}` wherever the graph's node prompts reference it:

```bash
hex run implement-until-green -p "fix the flaky auth test"
hex run plan-build-review -f prompts/task.md
hex run tdd -p "add a --json flag" --name json-flag   # names the run
```

Run ids read `yyyy-MM-dd-<workflow>-<short-uuid>`, or `yyyy-MM-dd-<name>` when
you pass `--name`.

**Node-to-node handoff.** An agent's final message is captured as its result and
a downstream node can reference it as `{{node.result}}` (e.g. the planner's plan
reaches the implementer), interpolated at runtime and wrapped as untrusted data.
Heavy artifacts (code, diffs) stay in the workspace — the reviewer runs
`git diff`. An agent with a single outcome may simply **finish** (no `hex emit`);
the runtime routes a synthesized `done`. Nodes with more than one outcome still
`hex emit <signal>`. (Full typed per-node inputs/outputs are a later phase.)

## CLI surface

The same verbs work from the CLI, MCP, and dashboard; all support `--json` /
NDJSON, stable exit codes, and `capabilities` introspection.

```text
hex list                 list runnable graphs (project > user > built-in)
hex doctor               are the configured workers and checks usable?
hex validate <graph>     schema, references, bounded cycles, a reachable success
hex graph <graph>        render a graph as text
hex run [<graph>]        start a NEW run (no graph → list what's runnable)
                         [--detach] return a run id immediately
                         [--no-preview] disable the live in-flight pane
hex resume <run>         continue the SAME run (after a pause or a crash)
hex runs                 list runs, newest activity first
hex status <run>         projected run status
hex wait <run>           block until it finishes; exit with its disposition
hex watch <run>          print the recorded event stream
hex logs <run> [--node <id>] [--full]   per-attempt final message
hex pause <run>          pause at the next attempt boundary
hex steer <run> <text>   add operator guidance to the next attempt
hex respond <run> <text> answer a blocking `human` node
hex cancel <run>         cancel, live or idle
hex emit <event>         worker→runtime, scoped-token control
```

**Exit codes** encode the outcome, so a script or a driving agent branches without
parsing output: `0` succeeded · `1` failed · `2` usage error · `3` timed out ·
`4` budget exhausted · `5` cancelled · `6` paused.

Still unbuilt: `interactive` sessions, `context: continue` (a reviewer with memory
across rounds), cost/token accounting, stall detection, a run digest,
`watch --follow`, and `graph --format mermaid|dot`.

### Driving hex from another agent, or from a second terminal

Control is a **file inbox** at `.hex/runs/<id>/control/`, written temp-then-rename
and drained at attempt boundaries — no daemon, and it works whether the run is
live or is resumed later. The same commands reach it from your shell, from a
driving agent's `Bash` call, or (later) from MCP: one protocol, many transports,
authority scoped per actor.

```bash
id=$(hex run critique-loop -p "fix the flaky auth test" --detach)
hex runs                              # what's alive, hung, or abandoned
hex steer  "$id" "prefer the existing retry helper"
hex pause  "$id"; hex resume "$id"
hex respond "$id" "approved, but skip the cache part"
hex wait   "$id"                      # exits with the disposition
```

A detached run re-execs the binary in its own process group with stdio to files
and outlives the launcher. Liveness comes from the run lock (kernel-released on
death, unlike a pidfile) plus a heartbeat, so `hex runs` distinguishes *running*
from *hung* from *crashed* — and a crashed run is continued with `hex resume`,
which marks the orphaned attempt `interrupted` rather than silently rerunning it.

On an interactive terminal, `hex run`/`hex resume` show a **live preview**: a
sticky footer that tails the in-flight attempt's output with a status line
(node · worker · attempt N/budget · elapsed · deadline). It auto-disables for a
non-TTY, `--json`, or `--no-preview`, falling back to plain event-line streaming.

## Workspace isolation

Owned by the runtime, opt-in per run: `hex run <graph> --worktree [<base>]`
runs the whole run in a fresh git worktree on branch `hex/<run-id>` (from HEAD,
or `<base>`) so codex/claude can work freely without touching your main working
copy; `--no-worktree` (the default) runs in the project root. Worktrees are a
**reusable pool** under `.hex/worktrees/` (gitignored) so built deps stay warm
across runs; parallel runs each get their own slot. `--worktree-init "<argv>"`
primes a fresh slot (`npm ci`, `cargo fetch`, …). **No auto-merge:** the branch
is left for you to inspect and integrate — hex asks the agent to commit its work
but never commits or merges itself. Cleanup/integration verbs, per-node
worktrees, and a serialized integration queue arrive later.

## Status

**Phase 1 works, plus three follow-up passes.** The critique loop runs end-to-end
on real agent CLIs, records everything to a JSONL journal, enforces run *and*
per-attempt budgets, resumes a killed run, and reports why it stopped.

Landed 2026-07-30 — operability: project-defined `checks:`, `hex doctor` +
start-time preflight, disposition exit codes, terminal reasons in the journal, and
the `gate`→`command` kind merge.

Landed 2026-07-31 — roles & gating: layered `roles:` config (deep-merged, with
`prompt`/`prompt_append`); built-in defaults as embedded YAML through the same
loader; gate-free presets; undeclared checks refused at compile time; multi-step
`command` nodes with `ordered`/`parallel` modes; per-node `budget: { visits: N }`;
`accept.on_unmet` rerouting; a reachable-success validation check; and the
`autoresearch` preset.

Landed 2026-07-31 — control & detach: the file-based control inbox
(`cancel`/`pause`/`resume`/`steer`/`respond`), working `human` nodes, detached
runs with heartbeat liveness, and `hex runs`/`wait`. hex now reviews its own
diffs: see [`.hex/graphs/self-review.yaml`](.hex/graphs/self-review.yaml).

### Known broken

Two unbounded-loop holes, found by hex reviewing its own diff and **not yet
fixed** — both in the `accept.on_unmet` cycle validation added 2026-07-31, so
fixing one unbounded cycle opened two more. Avoid `on_unmet` and `human` cycles in
an unattended run until these land:

- A terminal's `budget: { visits: N }` satisfies cycle validation but is never
  enforced — `schedule` settles terminals before it checks visit budgets, so
  `implement → done → implement` with the bound on `done` alone loops forever.
- `budget.attempts` does not bound a **human-only** cycle: a human response spends
  no attempt, so `human A → human B → human A` spins forever while validating.

Also unexplained: an fs4/flock flake where a just-released lock still reads as
busy. Seen on both worktree-slot and run locks; its user-visible symptom is
`hex cancel` right after a run ends queuing instead of recording. The tests that
would catch it are serialized, so **"parallel runs grow the pool" is currently an
untested claim.**

**Not built** (designed, decided, not yet shipped): `context: continue` (a reviewer
with memory across rounds), stall detection, cost/token accounting, a run digest,
`hex config show`, `graph --source`, `watch --follow`, `interactive` sessions,
`templates:`/`extends:`, capability matching, and the MCP client. `hex-mcp` /
`hex-dashboard` are still stubs. A timed-out attempt also discards the agent's
partial output, which is the wrong shape for a tool built on bounded runs.
Roadmap and the locked decisions behind each: [`TODO.md`](TODO.md).

```bash
cargo build --workspace     # build everything
cargo test  --workspace
cargo clippy --workspace --all-targets
hex list
hex doctor                  # are codex/claude installed? are my checks runnable?
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
