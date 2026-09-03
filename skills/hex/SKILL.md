---
name: hex
description: >-
  Runs a multi-round coding task as a bounded, resumable agent loop using the `hex` CLI: an
  implement/review/test graph over codex, claude or another agent CLI, with an append-only
  journal, hard budgets, deterministic gates and stable exit codes. Use this skill whenever work
  should run in a loop until it is verifiably done rather than in one pass: "keep iterating until
  the tests pass", "have a different model review this", "run it in the background and tell me
  when it is done", "TDD this", "research X until it is actually answered". Also use it for any
  mention of hex, `hex run`, `hex resume`, a hex preset (critique-loop, tdd, review),
  `.hex/config.yaml`, `.hex/graphs/`, or hex checks, roles and workers, and when starting,
  watching, steering, pausing or reading the result of a hex run. Do NOT use it for a single edit
  you can just make, for a task with no checkable definition of done, or from inside a running
  hex attempt.
license: MIT
---

# hex

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
`attempts × budget.attempt` — `attempts: 12` at the 30m default is six hours. Say the estimate
before starting.

## 1. Orient (never skip)

```bash
hex doctor              # are the agent CLIs and this project's checks runnable?
hex list                # graphs available here (project > user > built-in)
hex validate <graph>    # schema, references, bounded cycles, a reachable success
```

`doctor` matters because preflight **refuses to start** a run whose agent CLI is missing instead of
burning an attempt. `MISSING self hex` means the `hex` binary is not on `PATH` — fix that before
running any graph with a multi-outcome node, because the worker→hex channel is a plain `PATH`
lookup in the agent's own shell.

**Always run `hex` from the repository root.** It reads `.hex/` from the current directory and
**does not walk up**. From a subdirectory `hex list` silently shows nothing and `hex run` creates a
*second* `.hex/`.

## 2. Pick a graph

| Preset | Loop | Needs `checks.test` |
|---|---|---|
| `critique-loop` | implement → review, until approved | no |
| `checklist` | one `- [ ]` item per round, critique loop each, then one holistic review | no |
| `plan-build-review` | plan → implement → review | no |
| `review` | one reviewer over the current diff | no |
| `implement-until-green` | implement → test, until green | **yes** |
| `tdd` | write failing test → prove red → implement → prove green | **yes** |
| `autoresearch` | research → critic judges sufficiency → report | no |

Behaviour worth knowing: `critique-loop`, `plan-build-review`, `tdd` and `implement-until-green`
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
2. `hex run checklist -p "<file>: <one-line goal>" --detach` — the path goes **first** in
   the prompt; the graph's agents read and edit the file themselves.
3. `hex wait` (or poll `hex status`). Progress is the `[x]` marks in the file, so you can
   watch it, and a crash or `hex resume` continues exactly where the file says.
4. Each item is implemented then reviewed by a different model; after the last item a
   **fresh** reviewer judges the whole change. Its objections come back as new `- [ ]`
   items — check the file afterwards for scope the reviewer added.

Budgets fit ~8-10 items (`attempts: 30`, `review` visits 20). For a longer list, fork the
preset and raise both, or split into two runs.

## 3. Give it the task

One operator value only — the prompt. There is **no `--input k=v`**; `-p` and `-f` conflict.

```bash
hex run critique-loop -p "fix the flaky auth test"
hex run plan-build-review -f prompts/task.md
hex run tdd -p "add a --json flag" --name json-flag   # stable run id
```

**Reference files by path; never paste them.** The agent has filesystem access, so
`-p "fix the retry logic in src/http/client.rs; failing case at tests/retry.rs:88"` costs no tokens
and cannot go stale mid-run.

## 4. Run, watch, steer

| Command | Blocks | Does |
|---|---|---|
| `hex run <graph> --detach` | no | starts it, prints the run id |
| `hex wait <run>` | yes | exits with the disposition code |
| `hex status <run>` | no | current node, in-flight attempt + elapsed, queued steer, spend |
| `hex dash [--interval MS]` | until you quit | live full-screen table of all runs (`top` for hex); `q`/`Esc`/`Ctrl-C` quits; needs a TTY |
| `hex logs <run> [--node N] [--tail K] [--full] [--follow]` | `--follow` | what the agent said |
| `hex watch <run> [--follow]` | `--follow` | the event stream |
| `hex steer <run> "text"` | no | guidance for the **next** attempt |
| `hex respond <run> "text"` | no | answer a blocking `human` node |
| `hex pause <run>` / `hex resume <run>` | resume does | stop at next boundary / continue same run |
| `hex cancel <run>` | no | stop it |
| `hex feedback "text" [--kind K]` | no | log a hex issue/missing-capability to `~/.hex/feedback.jsonl` (see below) |

```bash
id=$(hex run critique-loop -p "…" --detach)
hex logs "$id" --follow
hex wait "$id"; echo "exit $?"
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
is worth more than a vague one. It is *not* for talking to the operator mid-run — that is `hex emit`
(routing) and the node's final message (results).

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
    attempts: 12            # run-wide
    elapsed: 30m            # run-wide wall clock
    attempt: 20m            # per attempt; always set, defaults to 30m
    output_tokens: 200000   # run-wide, GENERATION tokens only
nodes:
  review:
    budget: { visits: 3 }   # this loop only
```

`output_tokens` counts generation because a review reads millions of cached tokens to produce tens
of thousands — a total-token bound would track context size, not work. Whichever bound bites first
wins. Prefer `--detach` + `hex status` over fire-and-forget; tighten a node's `visits` before
raising `attempts`.

## 8. Project setup

`hex init` creates `.hex/`, `.hex/graphs/`, a commented config and `.gitignore` entries. Idempotent.

```yaml
# .hex/config.yaml
workers:                    # how to invoke a CLI — plumbing; a graph never names one
  codex:  { kind: codex }   # kind: codex | claude | opencode | command
roles:                      # what a graph names
  implementer: { worker: codex, effort: high }
  reviewer:    { worker: claude, read_only: true, prompt_append: "Only correctness and security." }
checks:                     # what "green" means HERE. Empty by default.
  test: [cargo, test, --workspace]
```

Layers built-in → `~/.config/hex/config.yaml` → `.hex/config.yaml`, deep-merged per key.
`prompt:` replaces a role's preamble, `prompt_append:` extends it. A graph naming an undeclared
check is **refused at compile time (exit 2)** naming the key to add — a run reporting `succeeded`
having verified nothing is the failure mode hex exists to prevent.

Trap: a `roles:` entry **shadows** a same-named `workers:` entry. Never name a scratch worker
`implementer`/`reviewer`/`planner`/`researcher`.

## 9. Write a graph

```yaml
version: 1
name: my-loop
entry: implement
defaults:
  role: implementer
  budget: { attempts: 8, elapsed: 30m, output_tokens: 150000 }
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
      prompt: "Review the diff for: {{prompt}}. Run `git diff`. Then `hex emit approved` or `hex emit changes_requested`."
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
- One outcome → the node just finishes and hex synthesizes `done`. **More than one → the agent must
  call `hex emit <signal>`**, and every signal needs both a `may_propose` entry and an edge.
- **Every cycle must be bounded** or validation fails.
- Authored edge order is preserved — write the happy path first.
- `hex validate <graph>` before running.

### Schema

| Where | Key | Notes |
|---|---|---|
| top | `version` `name` `entry` `nodes` | required (`version` defaults to 1) |
| top | `description` `example` | shown by `hex list` |
| `defaults` | `role` `context` `budget` | `context`: `fresh` (default) or `continue` |
| `budget` | `attempts` `elapsed` `attempt` `cycle_visits` `output_tokens` | run-wide, under `defaults:` |
| node | `budget: { visits: N }` | the only per-node budget |
| `agent` | `prompt` (required) `role` `may_propose` `context` `read_only` | `read_only` is advisory, prompt-enforced |
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
| `E-unbounded-cycle` | add `budget.attempts`, `cycle_visits`, or a node's `visits` |
| `E-proposal-no-edge` / `E-edge-not-proposable` | `may_propose` and the edges must match |
| `E-bad-signal-name` | signals are `[a-z][a-z0-9_]*` |
| `E-gate-signal` | a `command` node's edges are `passed`/`failed` |
| `E-human-no-edge` / `E-human-multi-edge` | a human node needs exactly one |
| `E-accept-unsatisfiable` | required evidence no node can produce |
| `E-accept-unmet-terminal` | `on_unmet` must name a node that can produce evidence |
| `E-done-reserved` | `done` is reserved for implicit completion |
| `E-budget-zero` | `output_tokens: 0` is spent before the first attempt |
| `E-journal-lifecycle` | not a graph error — that run's journal is corrupt |

## 10. When something is wrong

| Symptom | Cause and fix |
|---|---|
| `hex list`/`runs`/`doctor` show nothing you expect | not in the repo root; hex does not walk up. `cd` there — do **not** add a second config |
| ``needs check `test`, which this project does not declare`` | add it to `checks:`, or use a literal `run:` step. If it *is* declared, you are in the wrong directory |
| ``worker `x` cannot resume a session`` | `context: continue` on a worker without sessions — use `fresh`, or bind the role to codex/claude |
| `MISSING self hex` | put the `hex` binary on `PATH`, or multi-outcome nodes cannot route |
| ``unknown field `argv`, expected `kind`, `model`, `command`, `result``` | a config typo; every struct denies unknown fields |
| a worker runs the wrong agent | a `roles:` entry shadowed your `workers:` entry — rename the worker |
| ``hex emit must be run inside a hex attempt`` | worker-side only; you ran it yourself |
| ``agent emitted `x` which is not in may_propose`` | add the signal + an edge, or fix the prompt |
| a steer looks ignored | `hex status` — queued vs accepted; in-flight attempts never see it |
| `hex cancel` says "queued" and it keeps going | a live driver holds the lock; applies at the next attempt boundary |
| `(no final message captured)` | the attempt produced none; a timeout or crash leaves a `[partial output — …]` tail instead |
| `unreadable` in `hex runs` | that run's stored graph predates a schema change; history is intact but unreplayable |
| exit 1 but the work looks fine | with `review`, `changes_requested` *is* a failure terminal — read the final message |
| run is `running` but idle | `hex status` shows the in-flight attempt; if the process died, `hex resume` continues it |

## Known broken (checked 2026-08-01)

- **`accept.on_unmet` has two open unbounded-loop bugs** — a terminal's `visits` bound is validated
  but not enforced, and `attempts` cannot bound a human-only cycle. Avoid `on_unmet` and `human`
  cycles in unattended runs.
- **`budget.output_tokens` has no end-to-end test** — no fake worker reports usage, so enforcement
  is covered by unit tests only.
- `interactive: true`, `templates:`/`extends:` and stall detection are unbuilt.

Verify against `hex --help` and the README's "Known broken" rather than trusting this list.
