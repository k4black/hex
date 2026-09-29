---
name: hex
description: >-
  Manual only — invoke via /hex or when the operator explicitly asks for hex by name
  (`hex run`, a hex preset, `.hex/` config, or steering/reading a hex run). Runs a
  multi-round coding task as a bounded, resumable agent loop using the `hex` CLI: an
  implement/review/test graph over codex, claude or another agent CLI, with an append-only
  journal, hard budgets, deterministic gates and stable exit codes. Never auto-select this
  skill for a task that merely sounds loop-shaped, and never from a sub-agent.
disable-model-invocation: true
license: MIT
---

# hex

**Operator-invoked only.** Use this skill in an interactive session, on the operator's direct
request — never spawn a hex run because a task merely looks loop-shaped, and never from a
sub-agent (a hex run drives its own agents; a sub-agent starting one nests loops nobody
budgeted). If work seems to want a loop, propose `/hex` to the operator and stop.

Runs an existing coding-agent CLI in a loop over a graph you declare, journals every step, and
stops on bounds you set. **The agent does not decide when the loop ends** — a worker saying "done"
is a proposal; `accept.require` and the deterministic gates decide.

`hex --help` and `hex <verb> --help` are authoritative. hex is pre-1.0; when this file and the CLI
disagree, believe the CLI and say so.

## Use it when — and when not

**Use** when all hold: the task takes more than one round · "done" is checkable (tests, a
reviewer's verdict) · you want a different model to judge than wrote it, or the work to outlive one
context window, or to survive a crash.

**Do not use** when: you can just make the change · there is no checkable definition of done ·
every step needs a human decision · **you are already inside a hex attempt** (a worker has `hex` on
`PATH` and will recurse).

**Cost is real and uncapped in money.** A measured 10-attempt cross-model run in hex's own repo
spent **≥ $9.70 over ~40 min and ended `budget_exhausted` with nothing accepted.** Worst case is
roughly the total node visits (every non-terminal node defaults to 5, or its declared
`budget: { visits: N }`) × `budget.attempt` — 30m each by default. Say the estimate before
starting.

## 1. Orient (never skip)

```bash
hex doctor              # are the agent CLIs and this project's checks runnable?
hex list                # graphs available here (project > user > built-in)
hex validate <graph>    # schema, references, a reachable success
```

`doctor` matters because preflight **refuses to start** a run whose agent CLI is missing, or whose
model is not in the CLI's own catalog (codex, opencode, pi; claude has none), instead of burning an
attempt.

**Always run `hex` from the repository root.** It reads `.hex/` from the current directory and
**does not walk up**. From a subdirectory `hex list` silently shows nothing and `hex run` creates a
*second* `.hex/`.

## 2. Pick a graph

| Preset | Loop | Needs `checks.test` |
|---|---|---|
| `critique-loop` | implement → review, until approved | no |
| `checklist` | one `- [ ]` item per round, critique loop each, then one holistic review | no |
| `review` | one reviewer over the current diff | no |
| `implement-until-green` | implement → test, until green | **yes** |
| `tdd` | write failing test → prove red → implement → prove green | **yes** |
| `autoresearch` | research → critic judges sufficiency → report | no |

Behaviour worth knowing: `critique-loop`, `checklist`, `tdd` and `implement-until-green`
run their **implementer with `context: continue`** (it keeps what it worked out) and their
**reviewer fresh** (a reviewer continuing its own session talks itself into approving what it
already argued about). `autoresearch` is all-fresh because its continuity is on disk in
`.hex/research-notes.md`. With `review`, `changes_requested` routes to a **failure terminal**, so a
useful review exits 1.

Fork any of them: `hex graph <name> --format source > .hex/graphs/<name>.yaml`.

**Splitting a big job — the `checklist` workflow.** When a task is too big for one
critique-loop pass, decompose it yourself and let hex grind through the pieces:

1. Write the plan as a Markdown checklist file (`- [ ]` items, one bounded change each,
   ordered so earlier items never depend on later ones).
2. `hex run checklist -p "<file>: <one-line goal>" &` — the path goes **first** in
   the prompt; the graph's agents read and edit the file themselves. (`hex run`
   blocks; `&` backgrounds it in your shell.)
3. `hex wait` (or poll `hex status`). Progress is the `[x]` marks in the file, so you can
   watch it, and a crash or `hex resume` continues exactly where the file says.
4. Each item is implemented then reviewed by a different model; after the last item a
   **fresh** reviewer judges the whole change. Its objections come back as new `- [ ]`
   items — check the file afterwards for scope the reviewer added.

Budgets fit a handful of items (`review` visits 6, `implement` visits 12). For a longer
list, fork the preset and raise `review`'s bound (and `implement`'s with it), or split into two
runs.

## 3. Give it the task

One operator value only — the prompt. There is **no `--input k=v`**; `-p` and `-f` conflict.

```bash
hex run critique-loop -p "fix the flaky auth test"
hex run critique-loop -f prompts/task.md
hex run tdd -p "add a --json flag" --name json-flag   # stable run id
```

**Reference files by path; never paste them.** The agent has filesystem access, so
`-p "fix the retry logic in src/http/client.rs; failing case at tests/retry.rs:88"` costs no tokens
and cannot go stale mid-run.

## 4. Run, watch, steer

`hex run` **blocks** until the run ends. To watch or steer it, background it with your shell
(`hex run … &`) and use the verbs below from the same or another terminal. A `&`-backgrounded
run can be HUP'd when your shell exits — start it in tmux if it must outlive the terminal.

| Command | Blocks | Does |
|---|---|---|
| `hex run <graph> -p "…" &` | no (shell backgrounds it) | starts a run; the run id is in `hex runs` |
| `hex wait <run>` | yes | exits with the disposition code |
| `hex runs` | no | list runs, newest activity first |
| `hex status <run>` | no | current node, in-flight attempt + elapsed, queued steer, spend |
| `hex logs <run> [--node N] [--tail K] [--full] [--follow]` | `--follow` | what the agent said |
| `hex steer <run> "text"` | no | guidance for the **next** attempt |
| `hex respond <run> "text"` | no | answer a blocking `human` node |
| `hex pause <run>` / `hex resume <run>` | resume does | stop at next boundary / continue same run |
| `hex cancel <run>` | no | stop it |
| `hex stats` | no | what this machine asked hex to do, across repos |
| `hex prune [--older-than 7d] [--all]` | no | delete old finished/interrupted run dirs |
| `hex feedback "text" [--kind K]` | no | log a hex issue/missing-capability to `~/.hex/feedback.jsonl` (see below) |

**Driving a run as an agent: redirect to a log file, read the tail.** Every
event line hex prints carries a `HH:MM:SS` (UTC) timestamp, so the tail of the
log IS the run's current state — what node it is on, when it got there, and how
long the silence has been. Cheaper and fresher than polling `hex status` in a
loop, and the file survives your own context window:

```bash
hex run critique-loop -p "…" --name mytask > /tmp/hex-mytask.log 2>&1 &
tail -5 /tmp/hex-mytask.log        # where is it now, since when?
hex status 2026-…-mytask           # deeper: in-flight attempt, queued steers, spend
hex logs   2026-…-mytask --tail 20 # what the agent is actually saying
hex wait   2026-…-mytask; echo "exit $?"
```

A steer has **two stages**, both shown by `hex status`: `queued steer (not yet picked up)` is in the
control inbox; `steer accepted (applies to the next agent attempt)` is journaled. An attempt already
in flight will not see it — steering lands between attempts.

## 5. Read the result

`hex run`/`resume` end by printing the terminal reason (`why:`), each failed check with its exit
code and output tail, the final message, and the spend. `--json` gives `disposition`, `why`,
`result`, `failed_steps`, `usage`, `paused`.

- `hex logs <run>` — each attempt's final message (`--node` narrows, `--full` gives every byte).
- `hex status <run> --json` — usage as `by_node` / `by_model` / `total`.
- `--json` with `--follow` is rejected, not silently ignored.

## Tell us what hex is missing — `hex feedback`

If hex blocks you, lacks a capability you needed, or behaves surprisingly **while you are
using it**, say so: `hex feedback "what happened or what was missing" --kind missing-capability`
(kinds are free-form — `issue`, `missing-capability`, `idea`). It appends one JSON line to the
user-global `~/.hex/feedback.jsonl` and auto-captures the context — timestamp, project, and
`location` (the real project root, a durable place to debug from), plus, when you run it from
inside an attempt, the `run_id`, `node`, `graph`, `agent`, the `workdir` it ran in, and — for a
worktree run — the `branch` the code is on (so `cd <location> && git checkout <branch>` reaches it;
the `workdir` slot itself is reclaimable). It needs no project and no live run, writes nothing to
stdout, and never fails a run. This is the channel for
improving hex; a specific note ("`hex resume` re-ran a finished gate", "no way to pass two prompts")
is worth more than a vague one. It is *not* for talking to the operator mid-run — that is `hex steer` (guidance to the next
attempt) and a node's final message (its result and its verdict).

Costs render `—` when the agent reported none (codex reports tokens only), and a mixed run's total
is `≥ $X` with a line saying how much is unpriced. hex ships **no price table and never estimates**.

## 6. Exit codes

| Code | Meaning |
|---|---|
| 0 | succeeded |
| 1 | failed — **including a reviewer requesting changes** |
| 2 | usage error, unknown run, **or a graph/config problem** — read stderr, do not retry unchanged |
| 3 | timed out |
| 4 | budget exhausted |
| 5 | cancelled |
| 6 | paused (`hex resume` continues) |

**A failing check routes `failed`; a *broken* check fails the whole attempt.** hex never routes an
infrastructure failure as evidence, because that spends agent tokens fixing code on evidence never
gathered.

## 7. Bounds

```yaml
defaults:
  budget:
    elapsed: 30m            # run-wide wall clock
    attempt: 20m            # per attempt; always set, defaults to 30m
    output_tokens: 200000   # run-wide, GENERATION tokens only
nodes:
  review:
    budget: { visits: 3 }   # this loop only; every non-terminal node defaults to 5
```

There is no run-wide retry count: a node's `visits` says *which* loop may churn, which a single
attempt budget never did. `output_tokens` counts generation because a review reads millions of
cached tokens to produce tens of thousands — a total-token bound would track context size, not
work. Whichever bound bites first wins; exceeding a node's visits ends the run `budget_exhausted`
naming the node. Prefer a backgrounded run you poll with `hex status` over fire-and-forget; tighten
a node's `visits` rather than the wall clock. A gate that fails twice with identical output ends the
run `failed` (stall detection), so an unfixable check stops burning budget on its own.

## 8. Project setup

`hex init` creates `.hex/`, `.hex/graphs/`, a commented config and `.gitignore` entries. Idempotent.

```yaml
# .hex/config.yaml
workers:                    # how to invoke a CLI — plumbing; a graph names a role
  pi:     { kind: pi }      # kind: codex | claude | pi | opencode | command
roles:                      # what a graph names. Shipped: claude implements, codex reviews
  implementer: { worker: claude, effort: high }
  reviewer:    { worker: pi, model: openrouter/z-ai/glm-5.3, prompt_append: "Only correctness and security." }
checks:                     # what "green" means HERE. Empty by default.
  test: [cargo, test, --workspace]
```

Layers built-in → `~/.config/hex/config.yaml` → `.hex/config.yaml`, deep-merged per key.
`prompt:` replaces a role's preamble, `prompt_append:` extends it. A graph naming an undeclared
check is **refused at compile time (exit 2)** naming the key to add — a run reporting `succeeded`
having verified nothing is the failure mode hex exists to prevent.

Trap: a `roles:` entry **shadows** a same-named `workers:` entry. Never name a scratch worker
`implementer`/`reviewer`/`planner`/`researcher`.

pi model ids need the provider prefix (`openrouter/…`); a bare id is ambiguous.

`resume` recompiles against the **current** `roles:`. A live run keeps the roles it started with;
to switch a role's worker or model mid-task, `hex pause`, edit config, then `hex resume`.

## 9. Write a graph

```yaml
version: 1
name: my-loop
entry: implement
defaults:
  role: implementer
  budget: { elapsed: 30m, output_tokens: 150000 }
nodes:
  implement:
    agent:
      context: continue                 # keep this node's session across rounds
      prompt: "{{prompt}}\n\nReviewer said:\n{{review.result}}"
    on: { done: test }                  # one outcome: finishing cleanly IS the signal
  test:
    command: { check: test }            # or a literal argv: run: [cargo, test, --workspace]
    on: { passed: review, failed: implement }
  review:
    agent:
      role: reviewer
      prompt: "Review the diff for: {{prompt}}. Run `git diff`. Then give your verdict: approved, or changes_requested with concrete findings."
      may_propose: [approved, changes_requested]
    budget: { visits: 3 }
    on: { approved: done, changes_requested: implement }
  done:
    terminal: succeeded
accept:
  require: [review.approved, test.passed]
  on_unmet: implement
```

Rules that bite:

- **Four node kinds**: `agent`, `command`, `human`, `terminal`. Exactly one per node.
- **One outcome** → the node just finishes and hex synthesizes `done`; nothing is appended to its
  prompt. **More than one → the agent ends its final message with a `VERDICT: <signal>` line.**
  hex appends the exact instruction itself, so the prompt never has to spell it out; every signal
  still needs both a `may_propose` entry and an edge.
- **Every cycle is bounded by default**: every non-terminal node carries a visit bound (5 unless
  the graph declares `budget: { visits: N }`). A node that exceeds it ends the run
  `budget_exhausted`.
- Authored edge order is preserved — write the happy path first.
- `hex validate <graph>` before running.

### Schema

| Where | Key | Notes |
|---|---|---|
| top | `version` `name` `entry` `nodes` | required (`version` defaults to 1) |
| top | `description` `example` | shown by `hex list` |
| `defaults` | `role` `context` `budget` | `context`: `fresh` (default) or `continue`; `worker:` is an alias of `role:` (not both) |
| `budget` | `elapsed` `attempt` `output_tokens` | run-wide, under `defaults:` |
| node | `budget: { visits: N }` | the only per-node budget; defaults to 5 |
| `agent` | `prompt` (required) `role` `may_propose` `context` `read_only` | `read_only` is advisory, prompt-enforced; `worker:` is an alias of `role:` (not both) |
| `command` | `check:` or `run:`, `mode:` | `mode`: `ordered` (stop at first failure) or `parallel` (run all, fail if any did) |
| `human` | `prompt` | blocks until `hex respond`; **exactly one** outgoing edge |
| `terminal` | `succeeded` or `failed` **only** | the other dispositions are outcomes hex assigns |
| `accept` | `require: [node.signal]` `on_unmet: <node>` | evidence for a success terminal to count |

`check:` names an entry in config; `run:` is a literal argv needing no config — `run: [cargo, test]`
for one, or a list of lists for several. Both accept one name/argv or a list.

Interpolation: `{{prompt}}` is the operator's text; `{{<node>.result}}` is another node's final
message (or a human's answer), substituted at attempt start. Agent output is fenced as untrusted
data, a human answer as operator input.

Durations are humantime: `500ms` `30s` `20m` `2h` `1h30m`. **`m` is minutes, `M` is months.**

### Validation errors

| Code | Fix |
|---|---|
| `E-empty` / `E-entry` | no nodes / `entry` names none |
| `E-edge-from` `E-edge-to` `E-accept-node` `E-accept-unmet-node` `E-result-ref` | an id that does not exist |
| `E-unreachable` | connect or delete the node |
| `E-no-terminal` / `E-no-happy-path` | add a reachable `terminal: succeeded` |
| `E-terminal-edge` | terminals have no outgoing edges |
| `E-proposal-no-edge` / `E-edge-not-proposable` | `may_propose` and the edges must match |
| `E-bad-signal-name` | signals are `[a-z][a-z0-9_]*` |
| `E-gate-signal` | a `command` node's edges are `passed`/`failed` |
| `E-human-no-edge` / `E-human-multi-edge` | a human node needs exactly one |
| `E-accept-unsatisfiable` | required evidence no node can produce |
| `E-accept-unmet-terminal` | `on_unmet` must name a node that can produce evidence |
| `E-done-reserved` / `E-unknown-reserved` | `done` and `unknown` are reserved — never list them in `may_propose` (an `on: { unknown: … }` edge is allowed) |
| `E-mixed-done-and-proposals` | a node either completes implicitly (`done` edge only) or reports a verdict (`may_propose`) — never both |
| `E-budget-zero` | a zero bound (`output_tokens: 0`, or a node's `visits: 0`) is spent before the first attempt |
| `E-journal-lifecycle` | not a graph error — that run's journal is corrupt |

## 10. When something is wrong

| Symptom | Cause and fix |
|---|---|
| `hex list`/`runs`/`doctor` show nothing you expect | not in the repo root; hex does not walk up. `cd` there — do **not** add a second config |
| ``needs check `test`, which this project does not declare`` | add it to `checks:`, or use a literal `run:` step. If it *is* declared, you are in the wrong directory |
| ``worker `x` cannot resume a session`` | `context: continue` on a worker without sessions — use `fresh`, or bind the role to codex, claude or pi |
| ``unknown field `argv`, expected `kind`, `model`, `command`, `result``` | a config typo; every struct denies unknown fields |
| a worker runs the wrong agent | a `roles:` entry shadowed your `workers:` entry — rename the worker |
| ``no verdict in the final message (expected: …)`` | the agent did not end with `VERDICT: <signal>`; restate the outcome you expect in the prompt |
| ``agent reported `x`, which is not among this node's outcomes`` | it wrote a verdict the node does not declare — add the signal + an edge, or fix the prompt |
| a steer looks ignored | `hex status` — queued vs accepted; in-flight attempts never see it |
| `hex cancel` says "queued" and it keeps going | a live driver holds the lock; applies at the next attempt boundary |
| `(no final message captured)` | the attempt produced none; a timeout or crash leaves a `[partial output — …]` tail instead |
| `unreadable` in `hex runs` | that run's stored graph predates a schema change; history is intact but unreplayable |
| exit 1 but the work looks fine | with `review`, `changes_requested` *is* a failure terminal — read the final message |
| run is `running` but idle | `hex status` shows the in-flight attempt; if the process died, `hex resume` continues it |

## Known broken

- **`budget.output_tokens` has no end-to-end test** — no fake worker reports usage, so enforcement
  is covered by unit tests only.
- `interactive: true` and `templates:`/`extends:` are unbuilt.

Verify against `hex --help` and the README's "Known broken" rather than trusting this list.
