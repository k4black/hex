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

`hex` runs an existing coding-agent CLI in a loop over a graph you declare, records every step to
an append-only journal, and stops on bounds you set. **The one idea that makes it different: the
agent does not decide when the loop ends.** A worker saying "done" is a proposal; `accept.require`
and the deterministic gates decide the outcome.

`hex --help` and `hex <verb> --help` are authoritative. hex is pre-1.0 and moves — when this file
and the CLI disagree, believe the CLI, and say so.

## When to use hex — and when not to

Reach for hex when **all** of these hold:

- the task takes more than one round, and "done" is checkable (a test suite, a reviewer's verdict);
- you want a *different model* to judge the work than wrote it, or the work to outlive one context
  window, or to survive a crash and resume;
- a bound on spend and wall-clock is worth having.

Do **not** reach for hex when:

- you can just make the change — one edit through a loop is pure overhead;
- there is no checkable definition of done ("make it nicer");
- every step needs a human decision — that is a conversation, not a loop;
- **you are already running inside a hex attempt.** A worker has `hex` on `PATH` and will recurse.

**Cost is real and not capped in money.** A measured 10-attempt cross-model run in hex's own repo
spent **≥ $9.70 over ~40 minutes and ended `budget_exhausted` with no accepted result.** Worst case
is `attempts × budget.attempt` of wall clock — `attempts: 12` with the 30m default is six hours.
Say the estimate out loud before starting one.

## Orient first — never skip

```bash
hex doctor                 # are the agent CLIs and this project's checks runnable?
hex list                   # what graphs exist here (project > user > built-in)
hex validate <graph>       # schema, references, bounded cycles, a reachable success
```

`hex doctor` matters because preflight **refuses to start** a run whose agent CLI is missing,
rather than burning an attempt discovering it. If it reports `MISSING self hex`, put the `hex`
binary on `PATH` before running anything with a multi-outcome node: the worker→hex control channel
is a plain `PATH` lookup in the agent's own shell.

**Always run `hex` from the repository root.** hex reads `.hex/` from the current directory and
**does not walk up**. From a subdirectory `hex list` silently shows no project graphs and
`hex runs` reports "no runs yet" — and `hex run` will *create a second `.hex/`* there.

## Pick a preset

| Preset | Loop | Needs `checks.test`? |
|---|---|---|
| `critique-loop` | implement → review, until approved | no |
| `plan-build-review` | plan → implement → review | no |
| `review` | a reviewer over the current diff, no implementer | no |
| `implement-until-green` | implement → test, until green | **yes** |
| `tdd` | failing test → prove red → implement → prove green | **yes** |
| `autoresearch` | research → critic judges sufficiency → report | no |

The first three are gate-free and run in any repo with no setup. The two that need `checks.test`
refuse to start without it — that is the design, not a bug.

## Give it the task

hex takes **exactly one** operator value: the prompt.

```bash
hex run critique-loop -p "fix the flaky auth test"
hex run plan-build-review -f prompts/task.md        # same channel, from a file
hex run tdd -p "add a --json flag" --name json-flag # stable run id
```

There is **no `--input k=v`.** `-p` and `-f` are mutually exclusive.

**Reference files by path, do not paste them.** The agent has filesystem access, so
`-p "fix the retry logic in src/http/client.rs; the failing case is tests/retry.rs:88"` is better
than embedding either file — it costs no tokens and cannot go stale mid-run.

## Run, observe, steer

| Verb | Blocks? | Use |
|---|---|---|
| `hex run <graph> --detach` | no, prints the run id | start it and keep working |
| `hex wait <run>` | yes, until terminal | exits with the disposition code |
| `hex status <run>` | no | current node, in-flight attempt + elapsed, queued steer, spend |
| `hex logs <run> [--node N] [--full] [--tail K] [--follow]` | `--follow` does | what the agent actually said |
| `hex watch <run> [--follow]` | `--follow` does | the event stream |
| `hex steer <run> "text"` | no | guidance for the **next** attempt |
| `hex respond <run> "text"` | no | answer a blocking `human` node |
| `hex pause` / `hex resume <run>` | resume blocks | stop at the next boundary / continue the same run |
| `hex cancel <run>` | no | stop it |

The driving-agent recipe:

```bash
id=$(hex run critique-loop -p "…" --detach)
hex logs "$id" --follow      # watch it work
hex wait "$id"; echo "exit $?"
```

**A steer has two stages and `hex status` shows which.** `queued steer (not yet picked up)` means
it is in the control inbox; `steer accepted (applies to the next agent attempt)` means the driver
journaled it. An attempt already in flight will **not** see it — steering lands between attempts.

## Read the result

`hex run`/`hex resume` end by printing the terminal reason (`why:`), each failed check with its
exit code and a tail of its output, the final message, and what was spent. `--json` carries
`disposition`, `why`, `result`, `failed_steps`, `usage`, `paused`.

- `hex logs <run>` — each attempt's final message. `--node <id>` narrows it.
- `hex logs <run> --full` — every captured byte, on stdout.
- `hex status <run> --json` — the usage projection: `by_node`, `by_model`, `total`.

`--json` and `--follow` cannot be combined (there is no NDJSON log stream yet); hex rejects it
rather than quietly ignoring one.

## Exit codes

Branch on these instead of parsing output.

| Code | Meaning |
|---|---|
| 0 | succeeded |
| 1 | failed (**including a reviewer requesting changes** — see below) |
| 2 | usage error, unknown run, **or a graph/config problem** — read stderr; do not retry the same command |
| 3 | timed out |
| 4 | budget exhausted |
| 5 | cancelled |
| 6 | paused (`hex resume` continues it) |

Two traps. **Exit 1 does not distinguish "the reviewer found problems" from "hex broke"** — with
`review`, `changes_requested` routes to a failure terminal, so read the final message before
concluding anything went wrong. And **a failing check routes `failed` while a *broken* check fails
the whole attempt**: hex deliberately never routes an infrastructure failure as evidence, because
that would spend agent tokens fixing code on evidence never gathered.

## Bounds and cost

```yaml
defaults:
  budget:
    attempts: 12            # run-wide
    elapsed: 30m            # run-wide wall clock
    attempt: 20m            # per attempt — always set, defaults to 30m
    output_tokens: 200000   # run-wide, GENERATION tokens only
nodes:
  review:
    budget: { visits: 3 }   # this loop only
```

`output_tokens` counts generation, not total: a review reads millions of cached tokens to produce
tens of thousands, so a total-token bound would track context size rather than work.

**hex ships no price table and never estimates.** codex reports tokens and no money, so its cost
shows `—` and a mixed run's total renders `≥ $X` with a line saying how much is unpriced. Prefer
`--detach` plus `hex status` over fire-and-forget, and tighten a node's `visits` before loosening
`attempts`.

## Set the project up

```bash
hex init      # .hex/, .hex/graphs/, a commented config, .gitignore entries; idempotent
```

`.hex/config.yaml` holds three things:

```yaml
workers:                      # how to invoke an agent CLI — plumbing; a graph never names one
  codex:  { kind: codex }
roles:                        # what a graph names
  implementer: { worker: codex, effort: high }
  reviewer:    { worker: claude, read_only: true, prompt_append: "Only correctness and security." }
checks:                       # what "green" means HERE. Empty by default.
  test: [cargo, test, --workspace]
```

A graph naming a check you have not declared is **refused at compile time**, exit 2, naming the key
to add. That is deliberate: a run reporting `succeeded` having verified nothing is the failure mode
hex exists to prevent. Config layers built-in → `~/.config/hex/config.yaml` → `.hex/config.yaml`
and deep-merges per key.

## Write a custom graph

Start from a built-in rather than a blank file:

```bash
hex graph critique-loop --format source > .hex/graphs/my-loop.yaml
```

A complete, valid graph:

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
    command: { check: test }            # or a literal argv: run: [cargo, test]
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

- **Four node kinds only**: `agent`, `command`, `human`, `terminal`.
- A node with **one** outcome just finishes — hex synthesizes `done`. A node with **more than one**
  must call `hex emit <signal>`, and every signal must be in `may_propose` *and* have an edge.
- **Every cycle must be bounded** or validation fails. Bound the expensive node with `visits`.
- `{{prompt}}` is the operator's text; `{{node.result}}` is another node's final message.
- Validate before running: `hex validate <graph>`.

Full schema and every `E-*` error code → `references/graph-schema.md`.

## Known broken — do not rely on

Checked 2026-08-01; verify against `hex --help` and the README's "Known broken" rather than
trusting this list.

- **`accept.on_unmet` has two open unbounded-loop bugs.** A terminal's `visits` bound is validated
  but not enforced, and `attempts` cannot bound a human-only cycle. Avoid `on_unmet` and `human`
  cycles in an unattended run.
- **`budget.output_tokens` has no end-to-end test** — no fake worker reports usage, so enforcement
  is covered by unit tests only.
- `interactive: true`, `templates:`/`extends:` and stall detection are unbuilt.

## References

- `references/graph-schema.md` — every YAML field, defaults, and all validation error codes.
- `references/presets.md` — the six built-ins node by node, and how to fork one.
- `references/troubleshooting.md` — literal error text → cause → fix.
