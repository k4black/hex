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

**Context** is per node. Each attempt gets a fresh agent session by default;
`context: continue` resumes *that node's* last one, so a reviewer's second round
keeps what the first established instead of re-reading a 5k-line diff cold (codex
`exec resume <id>`, claude `--resume <id>`). The handle comes from the journal, so
it survives a crash and a `hex resume`. It is scoped per node deliberately: a
reviewer resuming the implementer's session would inherit its reasoning and stop
being an independent judge. A node asking for it on a worker that cannot resume is
refused at **compile** time rather than silently degrading to a fresh session every
round — the exact cost `continue` exists to avoid; codex and claude can resume,
opencode and the generic `command` adapter cannot.

The shipped presets apply that per node, and the asymmetry is the point:
`implement` continues its own session in `critique-loop`, `implement-until-green`,
`plan-build-review` and `tdd` (plus `tdd`'s `spec`, which `red` sends back when the
new test did not actually fail), so a revisited node keeps what it already worked
out instead of re-deriving the task every round; `review` stays `fresh`, because a
reviewer continuing its own session carries its earlier verdict into the next one,
which is how a critic talks itself into approving what it already argued about.
`autoresearch` keeps every attempt fresh on purpose — its continuity is on disk in
`.hex/research-notes.md`. The trade is the compile-time refusal above: rebind the
`implementer` role to a `kind: command` worker and those presets refuse to start
until you set `context: fresh` on the named node, which the error says.

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

`hex init` writes the project layer: `.hex/` + `.hex/graphs/`, a commented starter
`.hex/config.yaml` with every key inert (the built-in layer already supplies
working workers and roles, and an uncommented copy here would freeze this
machine's defaults into the repository), and `.hex/runs/` + `.hex/worktrees/`
appended to `.gitignore` when they are absent. Its `checks:` is empty with the
examples commented out — hex autodetects nothing, because what "green" means is
your call. It never overwrites an existing config, so running it twice is safe.

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
#   ok      self    hex      /Users/me/.cargo/bin/hex
#   ok      worker  codex    /opt/homebrew/bin/codex
#   MISSING check   lint     `ruff` not found on PATH
```

The `self` row probes `hex` itself, and a missing one makes `doctor` exit 1: the
agent's `hex emit` channel is a plain `PATH` lookup in the agent's *own* shell and
hex deliberately injects no `PATH` (installing the binary is the operator's job),
so its absence leaves every node with more than one outcome unroutable — which
broke 3 of 3 real agent runs and showed up as a 15-minute timeout. It is
deliberately *not* part of the start-time preflight: a single-outcome node
completes implicitly without ever calling `hex`, so refusing every run would block
work that would have succeeded.

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
hex init                 scaffold `.hex/` + a starter config in this repo
hex list                 list runnable graphs (project > user > built-in)
hex doctor               are `hex`, the configured workers and checks usable?
hex validate <graph>     schema, references, bounded cycles, a reachable success
hex graph <graph>        render a graph as text
hex run [<graph>]        start a NEW run (no graph → list what's runnable)
                         [--detach] return a run id immediately
                         [--no-preview] disable the live in-flight pane
hex resume <run>         continue the SAME run (after a pause or a crash)
hex runs                 list runs, newest activity first
hex status <run>         projected run status + what the run spent
hex wait <run>           block until it finishes; exit with its disposition
hex watch <run>          print the event stream ([--follow] until the run ends)
hex logs <run> [--node <id>] [--full] [--tail N] [--follow]
                         each attempt's final message + every check's output; a
                         still-running attempt's tail, streamed with --follow
hex pause <run>          pause at the next attempt boundary
hex steer <run> <text>   add operator guidance to the next attempt
hex respond <run> <text> answer a blocking `human` node
hex cancel <run>         cancel, live or idle
hex emit <event>         worker→runtime, scoped-token control
```

**Exit codes** encode the outcome, so a script or a driving agent branches without
parsing output: `0` succeeded · `1` failed · `2` usage error · `3` timed out ·
`4` budget exhausted · `5` cancelled · `6` paused.

Still unbuilt: `interactive` sessions, stall detection, and
`graph --format mermaid|dot`.

### What a run reports when it ends

`hex run`/`hex resume` finish by printing what the run *produced*, not only its
verdict: the kernel's reason for stopping (`why:`), what it spent, each failed
check's label with its exit code and a 40-line tail of its output, and the final
message. `--json` carries the same fields (`why`, `result`, `failed_steps`,
`usage`). Before this the end of a run was two lines, so a four-minute
cross-model review reported `disposition: failed` and left its findings on disk,
named by no output at all.

`hex logs` reaches a `command` node's step output at all now. A multi-step node
writes every byte into `attempts/<id>/<n>-<label>/` (plus a recorded `exit` file),
listed with its attempt in **declared** order — numerically, so `10-` follows
`9-`. `--full` prints both captured streams, and it writes to **stdout**, because
a transcript is requested data rather than a diagnostic: `hex logs <id> --full >
out.txt` now captures it.

An attempt that timed out or exited nonzero no longer throws away what the agent
already wrote. Capture always runs, and with no final message the bounded **tail**
(8 KB, from stdout *and* stderr — codex writes everything to stderr) becomes the
node's result, prefixed `[partial output — …]` because a downstream
`{{node.result}}` can interpolate it. A timed-out attempt also takes its whole
process group down (`SIGTERM`, 2s grace, `SIGKILL`); before that, grandchildren —
the `cargo test` the agent shelled out to — were reparented to pid 1 and survived
every timeout.

### What a run spent

Every number comes from the agent's own structured output and nowhere else. codex
runs `codex exec --json` and yields tokens only — its stream names no model and no
cost, so the per-model split is labelled with the role's configured model; claude
reports `modelUsage` per model with `costUSD`, plus `total_cost_usd`,
`duration_ms` and a `session_id`. hex never estimates and ships **no price
table**: an agent that reports no money shows no money, because a made-up cost is
worse than an absent one.

Money is integer **micro-USD** end to end. An `Event` is `Eq` and the journal
compares facts, so a currency amount that round-trips through JSON as an `f64` is
a fact that can change value on replay.

Each attempt journals an `AttemptReported` — on *every* outcome, including a
timeout, because an attempt that burned tokens and then died is exactly the one
whose cost you need — and `reduce` sums them into `RunState.usage`
(`by_node` · `by_model` · `total`). It is a projection like every other read
model, so `hex status`, the end-of-run line and any later client read one set of
numbers computed one way. `hex status` shows per-node rows against the attempts
that produced them, then per-model rows, then the total, in text and in `--json`;
it prints nothing when nothing reported usage, since a table of zeroes reads like
a free run rather than a silent one.

### Driving hex from another agent, or from a second terminal

Control is a **file inbox** at `.hex/runs/<id>/control/`, written temp-then-rename
and drained at attempt boundaries — no daemon, and it works whether the run is
live or is resumed later. The same commands reach it from your shell, from a
driving agent's `Bash` call, or (later) from MCP: one protocol, many transports,
authority scoped per actor.

```bash
id=$(hex run critique-loop -p "fix the flaky auth test" --detach)
hex runs                              # what's alive, hung, or abandoned
hex status "$id"                      # in-flight attempt, elapsed, pending steers
hex logs   "$id" --follow             # tail the agent until the run ends
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

### Checking in on a live run, and steering it

`hex status` answers *what is happening now*, not only "is it running":

```text
status: running
current: work
attempts: 3
in flight: att_3 on work via implementer, running 12s
queued steer (not yet picked up): prefer the existing retry helper
steer accepted (applies to the next agent attempt): keep the diff small
```

The elapsed clock is read off the attempt's `AttemptStarted` event: the projection
knows an attempt is in flight, but only the journal knows when it began. A run
parked on a `human` node prints `waiting for you: hex respond <id> "…"` with the
question itself, so the thing you have to answer is on screen next to the fact that
you have to answer it. `--json` carries every one of these fields.

**A steer has two stages, and conflating them is what made one look lost.**
`queued` means the command is in the control inbox and no driver has claimed it;
`steer accepted` means it is already journaled and waiting for the next *agent*
attempt to read it. `hex steer` says which you are getting: with an attempt already
running it adds, on stderr, `note: <id> is mid-attempt (att_1 on work); the steer
applies to the NEXT attempt` — a steer is drained at an attempt boundary, and
without that line an operator expects the running agent to change course and reads
the unchanged output as a dropped command.

`hex logs --follow` streams the in-flight attempt's output until the run ends, then
closes with a `── succeeded ──` line. It follows *across* attempts, including a
`command` node's numbered step directories — a gate is exactly the slow thing you
wait on. It attaches at each stream's **tail**, so joining a long attempt shows what
it is doing now rather than replaying an hour, and it drains an attempt's final
bytes when that attempt stops being in flight, because the most interesting line —
a check's failure, an agent's last word — is written after the last poll that could
still see it. Without `--follow`, a still-running attempt shows its last `--tail N`
lines (default 20) and `(still running)`, instead of the `(no final message
captured)` it used to print over ten lines of live output. Both streams either way:
codex writes everything to stderr and nothing to stdout, so one stream alone is
silent for one of the two agents.

`hex watch --follow` prints new events until the run finishes, cursored by event
count — the journal is append-only, so "how many have I printed" is the whole
cursor. Both followers wait up to 15s for a just-reserved run's journal, so
`hex logs --follow "$(hex run … --detach)"` — the obvious thing to type — works
instead of failing on a run directory that exists a moment before its first event;
a typo still fails fast, because "not started yet" and "does not exist" are
distinguished.

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

**Phase 1 works, plus five follow-up passes.** The critique loop runs end-to-end
on real agent CLIs, records everything to a JSONL journal, enforces run *and*
per-attempt budgets, resumes a killed run, and reports why it stopped, what it
produced, and what it cost.

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

Landed 2026-07-31 — output, partials, cost: driven by hex's own `self-review`
runs, which produced real reviews and reported `disposition: failed` and nothing
else. `run`/`resume` now print the run's payoff (terminal reason, spend, each
failed check's tail, the final message); `hex logs` reaches command-step output and
`--full` writes to stdout; a timed-out or crashed attempt keeps a bounded tail of
what the agent wrote instead of discarding it, and takes its whole process group
with it; per-attempt usage is journaled in micro-USD and folded into a
`RunState.usage` projection that `hex status` renders per node and per model; every
run now ends with a `RunFinished` whatever path it took; `context: continue`
(per-node session resume, refused at compile time on a worker that cannot — hex's
own `self-review` review node uses it); `hex init`; and a `doctor` row for `hex`
itself, whose absence had broken the `hex emit` channel in 3 of 3 real agent runs.

Landed 2026-07-31 — a live run you can see and steer: checking in on one used to
tell you almost nothing, because `hex status` printed three lines with no clock,
`hex logs` said `(no final message captured)` while ten lines of the agent's output
sat in `stdout.log`, and a queued `hex steer` was invisible on every surface.
`hex status` now reports the in-flight attempt with an elapsed clock, the blocking
`human` question, and a steer's *two* stages (in the inbox vs journaled and waiting
for the next agent attempt); `hex steer` says when it will land; `hex logs` tails a
running attempt and `--follow` streams it across attempts and command steps;
`hex watch --follow` tails the journal. Both followers wait for a `--detach`ed run's
first event. Smaller honesty fixes in the same pass: `hex runs` sizes its columns
from the data (`finished:budget_exhausted` used to run into the next field),
`hex logs --json` reports each step's `exit`/`failed` so a driving agent can name
the check that went red, a cost of zero renders as an em dash rather than claiming
the work was free, and the spend column is labelled `VISITS` because that is what
the projection counts. Also: the shipped presets now declare `context: continue` on
the node a loop revisits and keep the reviewer `fresh`.

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

**Not built** (designed, decided, not yet shipped): stall detection, `hex config
show`, `graph --source`, `interactive` sessions, `templates:`/`extends:`,
capability matching beyond the `context: continue` check, and the MCP client.
The followers poll at 400ms rather than watching the filesystem, and there is no
`hex logs --json --follow` (streaming NDJSON) yet. `hex-mcp` / `hex-dashboard` are
still stubs. A **run digest** verb is off the list rather than pending: the end-of-run output and `hex status`
already carry the breakdown from the same projection, and a third surface for it
would only be somewhere for the three to disagree. Roadmap and the locked
decisions behind each: [`TODO.md`](TODO.md).

```bash
cargo build --workspace     # build everything
cargo test  --workspace
cargo clippy --workspace --all-targets
hex init                    # .hex/ + a starter config (safe to re-run)
hex list
hex doctor                  # is hex on PATH? codex/claude installed? checks runnable?
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
