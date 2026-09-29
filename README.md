# hex

**A thin, deterministic control plane for agentic loops and graphs.**

* `hex` runs a graph of nodes. A node is one of four kinds: `agent`, `command`, `human` or `terminal`.
* A graph is one YAML file with one entry node and at least one reachable `terminal: succeeded`. A graph may have several terminals.
* Humans and agents drive it the same way, from a shell or from a driving agent.
* It calls Claude Code, Codex, Pi and opencode as workers, and journals every step and steer.

```sh
hex run critique-loop -p "Investigate problem with blocked google auth, fix bug and the flaky auth test"
```

## Quickstart

```bash
cargo build --workspace          # binary is `hex` (package `hex-cli`)
hex init                         # .hex/ + a starter config; safe to re-run
hex doctor                       # are codex/claude/pi installed? checks runnable?
hex list                         # runnable graphs: project > user > built-in
```

`hex run` blocks and owns the loop in-process. Background it with your shell
(`hex run … &`) or tmux.

A graph is one standard YAML file -- kind-as-key, edges co-located in `on:`,
prompts as inline block scalars. The loader uses `deny_unknown_fields`, so a
typo is an error:

```yaml
version: 1
name: implement-until-green

defaults:
  role: implementer
  budget: { elapsed: 30m, attempt: 10m }

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
    # Runs this project's `checks.test`. Declare none and the run is refused.
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

That validates and runs as-is. Reaching `clarify` blocks the run until you
answer it with `hex respond <run> "…"`; the answer becomes that node's result,
so a downstream prompt can reference `{{clarify.result}}` like an agent's.

## How it works

The kernel is two pure functions: `reduce(graph, state, event)` and
`schedule(graph, state, now_ms) -> Vec<Effect>`. `schedule` checks acceptance
and returns **effect intents** (`StartAttempt`, `RunCommand`, `RequestHuman`,
`RecordTerminal`) and never performs them. The runtime replays the journal,
asks `schedule` what to do, writes the intent *before* acting, executes it,
appends the result, and loops. Routing spends no tokens; `replay(journal)`
always reproduces the projection. Crates depend strictly inward
(`proto ← kernel, worker ← runtime ← cli`); the full table and the non-negotiable
rules are in [`AGENTS.md`](AGENTS.md).

**Four node kinds:** `agent` · `command` · `human` · `terminal`. Roles
("planner", "reviewer") are metadata on an `agent` node, never kinds. A
**gate is a role too**: any `command` node whose signal is named in
`accept.require` acts as one.

**Verdicts.** An agent talks back through its final message. A node with one
outcome just finishes -- the runtime routes a synthesized `done`. A node with
several ends its message with `VERDICT: <signal>`; the runtime generates that
instruction from the node's `may_propose` allow-list and appends it to the
prompt, so no graph author writes it and it cannot drift from the edges. A
missing or undeclared verdict is the reserved `unknown` signal: the attempt
fails closed naming the expected signals, unless the graph routes it
(`on: { unknown: … }`). Agents choose among given options; they never invent a
transition.

**Bounds are layered.** `budget.elapsed` bounds the run and `budget.attempt`
bounds one attempt (always set, so a hung agent cannot block forever).
`budget.output_tokens` bounds what the run *generates* (not cached input, which
dominates any raw total), and every non-terminal node carries a visit bound —
`budget: { visits: N }`, defaulting to 5 — so the run is bounded node by node
rather than by one retry count thrown at every loop. A node that exceeds its
visit bound ends the run `budget_exhausted`, naming the node. A graph with no
reachable `terminal: succeeded` is a validation error, as is a zero bound.
`accept.on_unmet: <node>` sends a run that reached success without the required
evidence back to earn it instead of dead-ending.

**Context is per node.** Each attempt gets a fresh session; `context: continue`
resumes *that node's* last one (codex `exec resume`, claude `--resume`, pi).
The handle comes from the journal, so it survives a crash and a `hex resume`.
A node asking for it on a worker that cannot resume is refused at compile time.
Presets continue the implementer and keep the reviewer `fresh` -- a critic
resuming its own session talks itself into approving what it already argued
about.

### Checks and config

Layered config, deep-merged per key, project wins: built-in
([`defaults.yaml`](crates/hex-runtime/src/defaults.yaml), a real YAML file
through the same loader) → `~/.config/hex/config.yaml` → `.hex/config.yaml`.

```yaml
# Workers are CLI adapters -- internal plumbing. A graph names a role.
workers:
  codex:  { kind: codex }       # kinds: codex | claude | pi | opencode | command
  claude: { kind: claude }
  pi:     { kind: pi }

# Roles are what a graph names. Shipped defaults: claude implements, codex
# reviews -- cross-model, so a different model judges than wrote.
roles:
  implementer: { worker: claude, effort: high }
  reviewer:
    worker: codex
    read_only: true
    prompt_append: "Only flag correctness and security issues."

# What "green" means in THIS project. Empty by default.
checks:
  test: [pytest, -q]
  lint: [ruff, check, .]
```

Four roles ship: `implementer`, `reviewer`, `planner`, `researcher`. There is
deliberately **no orchestrator role** -- routing is structural.
`prompt:` replaces an inherited preamble, `prompt_append:` extends it.

A check you have **not** declared is a hard error at compile time naming the
key to add -- never a silent pass, because a run reporting `succeeded` having
verified nothing is the failure mode worth designing against. So most presets
ship gate-free and run in any repo. A `command` node runs one step or several:
`mode: ordered` (default) stops at the first failure; `mode: parallel` runs
every step and fails if any did, buffering each to `attempts/<id>/<n>-<label>/`
in declared order.

### Presets

A preset is a named graph resolved `.hex/graphs/` > `~/.config/hex/graphs/` >
built-in. Six ship:

| Preset | Loop | Needs a check? |
|---|---|---|
| `critique-loop` | implement → review, until approved (the flagship) | no |
| `checklist` | work a `- [ ]` file item by item, critique loop per item, then one holistic review | no |
| `review` | reviewer over the current `git diff`, no implementer | no |
| `autoresearch` | research → critic judges sufficiency → revise → report | no |
| `implement-until-green` | implement → test (the Ralph loop) | **`checks.test`** |
| `tdd` | write-failing-test → prove red → implement → prove green | **`checks.test`** |

The operator supplies exactly one thing -- the prompt -- which fills
`{{prompt}}` in the graph's node prompts:

```bash
hex run implement-until-green -p "fix the flaky auth test"
hex run critique-loop -f prompts/task.md
hex run tdd -p "add a --json flag" --name json-flag   # names the run
```

Run ids read `yyyy-MM-dd-<workflow>-<short-uuid>`, or `yyyy-MM-dd-<name>`.
`hex graph <name> --format source` prints the YAML -- the copy-and-customise
path.

### Workspace isolation

`hex run <graph> --worktree [<base>]` runs the whole run in a fresh git
worktree on branch `hex/<run-id>`, so agents work without touching your main
copy; without it (the default) a run uses the project root. Worktrees are a
reusable pool under `.hex/worktrees/` so built deps stay warm;
`--worktree-init "<argv>"` primes a fresh slot. **No auto-merge** -- hex asks
the agent to commit and leaves the branch for you to integrate.

### Driving and watching a run

Control is a file inbox at `.hex/runs/<id>/control/`, written
temp-then-rename and drained at attempt boundaries. No daemon; the same
commands work from your shell, from a driving agent's `Bash` call, and whether
the run is live or resumed later.

```bash
hex run critique-loop -p "fix the flaky auth test" &
hex runs                              # what's alive, interrupted, or finished
hex status <id>                       # in-flight attempt, elapsed, pending steers
hex logs   <id> --follow              # tail the agent until the run ends
hex steer  <id> "prefer the existing retry helper"
hex pause  <id>; hex resume <id>
hex respond <id> "approved, but skip the cache part"
hex wait   <id>                       # exits with the disposition
```

A `&`-backgrounded run belongs to your shell's session, so the shell exiting
can HUP it. For a run that must outlive the terminal, start it in tmux.

**Ctrl-C** stops the agent too and keeps the work: the in-flight attempt's
process group is killed (`SIGTERM`, 2s, `SIGKILL`), what it spent and produced
is journaled, and the run is left **paused** (exit 6), resumable with
`hex resume`. A second Ctrl-C exits 130 immediately.

A steer has two stages and `hex status` distinguishes them: `queued` (in the
inbox, unclaimed) versus `steer accepted` (journaled, waiting for the next
agent attempt). `hex steer` warns on stderr when an attempt is already running.
`hex logs --follow` is journal-driven, so an attempt that starts and ends
between two polls is still shown; it follows across attempts and a `command`
node's numbered step directories.

On a TTY, `hex run`/`hex resume` show a live preview -- a sticky footer tailing
the in-flight attempt, status on the top border, the graph on the bottom
(`▸` is where the run is, `×N` how often a node was entered). It auto-disables
for a non-TTY, `--json`, or `--no-preview`.

```text
┌ build · stepper · attempt 3/5 · ⠴ 0:05 · 0:54 left ──────────┐
│working 1                                                     │
│working 2                                                     │
└ ▸ build ×2 · check ×1 · ok ──────────────────────────────────┘
```

**Stall detection** bounds the loop budgets cannot: a gate that fails twice
with the identical output signature is not routed back -- the run ends `failed`
instead of burning the rest of the budget re-fixing nothing.

### What a run spent

Every number comes from the agent's own structured output. hex **never
estimates** and ships no price table: codex reports tokens only, claude reports
per-model cost, pi reports both. So a partly priced total says so (`≥ $X`, with
a closing lower-bound line) instead of passing half the spend off as the whole.
Money is integer micro-USD end to end, folded from journaled `AttemptReported`
events into one projection that `hex status`, the end-of-run output and `--json`
all read.

```text
NODE                   VISITS        IN       OUT   CACHE R   CACHE W   REASON         COST
implement                   3     12.4k      8.1k      1.9M     41.2k     6.0k      $0.4183
review                      2      3.1k      1.2k      2.9M         0      896            --
MODEL
claude-opus-5                     12.4k      8.1k      1.9M     41.2k     6.0k      $0.4183
gpt-5-codex                        3.1k      1.2k      2.9M         0      896            --
total                       5     15.5k      9.3k      4.8M     41.2k     6.9k    ≥ $0.4183

cost is a lower bound: 2 attempt(s) reported tokens but no price
```

`REASON` is a subset of `OUT` and is never added into a total. `hex run` ends by
printing why the run stopped, what it spent, each failed check's tail, and the
final message; `--json` carries the same fields.

## CLI surface

Every verb takes `--json` where a machine form exists.

```text
hex init                 scaffold `.hex/` + a starter config in this repo
hex list                 list runnable graphs (project > user > built-in)
hex doctor               are the configured workers and checks usable?
hex validate <graph>     schema, references, a reachable success
hex graph <graph>        render a graph [--format text|source]
hex run [<graph>]        start a NEW run and block until it ends [--worktree]
                         [--name] [--no-preview]
hex resume <run>         continue the SAME run (after a pause or a crash)
hex runs                 list runs, newest activity first
hex status <run>         projected run status + what the run spent
hex wait <run>           block until it finishes; exit with its disposition
hex logs <run>           attempt output [--node <id>] [--full] [--tail N]
                         [--follow]
hex pause <run>          pause at the next attempt boundary
hex steer <run> <text>   add operator guidance to the next attempt
hex respond <run> <text> answer a blocking `human` node
hex cancel <run>         cancel, live or idle
hex stats                what this machine asked hex to do, across every repo
hex prune                remove old finished/interrupted run dirs
                         [--older-than <dur>] [--all]; never touches a live run
hex feedback <text>      log a hex issue to ~/.hex/feedback.jsonl
```

**Exit codes** encode the outcome, so a script branches without parsing output:
`0` succeeded · `1` failed · `2` usage error · `3` timed out · `4` budget
exhausted · `5` cancelled · `6` paused.

`hex stats` and `hex feedback` append to the user-global `~/.hex/` (distinct
from a project's `.hex/`), so usage and issue notes aggregate across repos and
survive `hex prune`. `hex feedback` auto-captures the run, node, graph, agent,
project root and worktree branch when it runs inside an attempt.

## Status

Phase 1 works and is dogfooded on this repo: the critique loop runs end-to-end
on real agent CLIs, journals every step, enforces budgets, resumes a killed
run, and reports why it stopped and what it cost. The
[simplification design](docs/design/2026-09-13-simplification.md) records what
was deleted and why. [`TODO.md`](TODO.md) is the forward list.

**Known broken.** An unexplained fs4/flock flake: a just-released lock still
reads as busy, on both worktree-slot and run locks. Its user-visible symptom
(`hex cancel` queuing right after a run ends) is closed; the flake is not
root-caused. The worktree leasing tests are serialized, so *"parallel runs grow
the pool" is an untested claim*.

**Not built** (designed, not shipped): `hex config show`, `interactive`
sessions, `templates:`/`extends:`, capability matching beyond session resume and
result capture, `hex logs --json --follow`. Followers poll at 400ms rather than
watching the filesystem.
