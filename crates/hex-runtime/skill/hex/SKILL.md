---
name: hex
description: >-
  Run other coding agents as your own sub-agents with the hex CLI
  (github.com/k4black/hex): bounded, journaled, resumable graphs over Claude
  Code, Codex, pi and opencode. Implement → review loops, parallel research,
  N approaches from one context, test gates, and calling the same agent again
  later. Use when the operator types /hex or names hex: `hex run`, a preset
  (critique-loop, implement-until-green, tdd, review, checklist, autoresearch),
  `.hex/` config, roles and models, or a run to steer, read or resume. Manual
  only: if a task only looks loop-shaped, propose /hex and stop. Do not use from
  a sub-agent or inside a hex attempt (HEX_RUN_ID is set). Read this whole
  skill before the first hex command.
argument-hint: "[task]"
disable-model-invocation: true
license: MIT
---

# hex

You plan. hex runs other agents for you over a small YAML graph, records every step, enforces bounds and gates, and resumes after a crash. The agent's "done" is a proposal: checks and `accept.require` decide the outcome.

`hex --help` and `hex <verb> --help` are authoritative. If this file and the CLI disagree, trust the CLI and tell the operator.

## Setup

Run `hex --version`. If it is missing, tell the operator to install it (`brew install k4black/tap/hex-cli`, `uv tool install hex-agents-cli`, `npm i -g @k4black/hex-cli`, `cargo binstall hex-cli`). Do not install it unasked.

Then, from the repository root: `hex init`, `hex doctor`. hex reads `.hex/` in the current directory only.

## When to use it

Use hex when the task needs more than one round, "done" is checkable (a test or a reviewer's verdict), and another model should judge the work or the work must survive a crash.

Do not use it when you can make the change yourself, or when every step needs a human decision.

**Cost is real.** Each node visit is a full agent session. Worst case ≈ visits × 30 min per attempt. Tell the operator the estimate before you start.

## Presets

```sh
hex list                                     # graphs: project > user > built-in
hex run critique-loop -p "fix the retry in src/http/client.rs; see tests/retry.rs:88" --name retry
```

| Preset | Loop | Needs `checks.test` |
|---|---|---|
| `critique-loop` | implement → review, until approved | no |
| `implement-until-green` | implement → test, until green | yes |
| `tdd` | failing test → red → implement → green | yes |
| `review` | one review of the current diff (changes requested exits 1) | no |
| `checklist` | one `- [ ]` item per round, then a final review. `-p "plan.md: <goal>"` | no |
| `autoresearch` | research → critic → report | no |

- `-p` (or `-f <file>`) is the only input. Name files by path; do not paste them.
- `--worktree [<base>]` isolates the run on branch `hex/<run-id>`. hex does not merge it.

## Write a graph for the task

When no preset fits, pipe a graph in. It names roles, so the operator's config still picks each CLI and model.

```sh
hex validate - <<'YAML'   # then: hex graph - (show the operator), then: hex run - -p "<task>"
version: 1
name: auth-fix
entry: research
nodes:
  research:
    agent: { role: scout, prompt: "Map the auth code for: {{prompt}}. Report file:line facts." }
    on: { done: [impl_a, impl_b] }
  impl_a:
    agent: { role: implementer, context: { fork: research }, prompt: "Approach A, in src/auth/a.rs only. Feedback: {{review.result}}" }
    on: { done: review }
  impl_b:
    agent: { role: implementer, context: { fork: research }, prompt: "Approach B, in src/auth/b.rs only. Feedback: {{review.result}}" }
    on: { done: review }
  review:
    join: all
    agent: { role: reviewer, prompt: "Compare {{impl_a.result}} and {{impl_b.result}} against: {{prompt}}", may_propose: [approved, changes_requested] }
    budget: { visits: 3 }
    on: { approved: done, changes_requested: [impl_a, impl_b] }
  done: { terminal: succeeded }
accept: { require: [review.approved] }
YAML
```

| Syntax | Meaning |
|---|---|
| `agent`, `command`, `human`, `terminal` | The four node kinds. `command: { check: test }` runs a `checks:` entry and emits `passed`/`failed`. |
| `may_propose: [a, b]` | The agent ends with `VERDICT: <signal>`; hex adds that instruction. No list: a clean exit emits `done`. |
| `on: { done: next }` | Edge. A list (`[a, b]`) starts every target at once. |
| `join: all` | Wait until the parallel nodes are done. Required where branches meet (`E-join-missing`). |
| `{{node.result}}`, `{{prompt}}` | Another node's final message (fenced as untrusted), the operator prompt. |
| `context: continue` | Next visit resumes this node's own session. |
| `context: { fork: research }` | First visit starts from a copy of `research`'s session, later visits continue its own. Same CLI only (claude or codex); the model may differ. |
| `budget: { visits: N }` | Visit bound (default 5). Attempts time out at 30m (`defaults: { budget: { attempt: 20m } }`). |
| `accept: { require: [test.passed] }` | Evidence a success terminal needs. |

- Parallel nodes share one workspace. Give writers separate files, or run each as its own `hex run --worktree`.
- A failed node stops the others and ends the run.
- Keep a graph that worked: save it to `.hex/graphs/<name>.yaml`, then `hex run <name>`. `hex graph <preset> --format source` prints a preset to start from.

## Call the same agent again

Each node's agent session outlives its run.

```sh
hex status <run> --json                      # "sessions": node -> { agent, id }
hex run critique-loop --continue <run> -p "Fix these review comments: ..."
```

- `--continue <run>` starts a **new** run: each `continue` or `fork` node starts from the same-named node's session in `<run>`. `hex resume <run>` continues the **same** run.
- Continue the newest run of a chain. hex refuses a live source, a source another run already continued, and a role that now runs another CLI.
- To re-review with comments, use a graph whose `review` has `context: continue`. Presets keep the reviewer fresh: a continuing reviewer defends its old verdict.

## Roles and config

A role binds a worker (claude, codex, pi, opencode, or a `kind: command` worker) to a `model`, `effort`, `read_only` and a prompt preamble. Layers, highest first: `.hex/config.yaml`, `~/.config/hex/config.yaml`, built-in; deep-merged per key.

| Built-in role | Runs | Use for |
|---|---|---|
| `implementer` | claude | changes |
| `reviewer` | codex, read-only | judging a diff |
| `planner` | codex, read-only | ordered plans |
| `researcher` | claude, read-only | open questions with sources |
| `scout` | claude `claude-sonnet-5-5`, read-only | fast, narrow lookups |

```yaml
# .hex/config.yaml
roles:
  implementer: { model: claude-opus-5-5, effort: high }
  reviewer:    { prompt_append: "Read AGENTS.md first." }
checks:
  test: [cargo, test, --workspace]
```

- `checks:` is empty by default. A graph naming an undeclared check does not start.
- Change a `worker` → also set its `model`. `hex doctor` checks it. pi needs `openrouter/<vendor>/<model>`.
- A graph cannot define a role. If none fits, propose a `.hex/config.yaml` entry and add it only after the operator approves.
- `read_only` is a prompt instruction, not a sandbox.
- A live run keeps its compiled roles: `hex pause`, edit, `hex resume`.

## Operate a run

```sh
hex run - -p "<task>" --name auth < graph.yaml > /tmp/hex-auth.log 2>&1 &
hex status <run>                             # active nodes, in-flight attempts, spend
hex logs <run> --follow                      # live output, [node] prefix when parallel
hex steer <run> "prefer the existing retry helper"
hex steer <run> --node impl_b "use the v2 API"
hex respond <run> "approved"                 # answer a human node
hex wait <run>; echo "exit $?"
hex pause <run>    hex resume <run>    hex cancel <run>
```

- `hex run` blocks; background it. A steer reaches the next attempt, not the one running.
- The run ends by printing why, failed checks, the final message and spend. `--json` for machine output.
- If hex blocks you: `hex feedback "<what happened>" --kind issue`.

## Best practices

- Review with a different model than the one that implements.
- Let a test decide "done": declare `checks:` and `accept.require`.
- Parallel reads, separate writes. Fan out research and alternatives; never two writers on one file.
- Research once, fork many: a `scout` node plus `context: { fork: … }` saves every implementer re-reading the code.
- Size it: 1 agent for a fact, 2–4 parallel nodes for a comparison.
- Steer a live run instead of restarting it. Fix a red base before a scoped run.

## Exit codes and errors

0 succeeded · 1 failed (also changes requested) · 2 usage/graph/config error, do not retry unchanged · 3 timed out · 4 budget exhausted · 5 cancelled · 6 paused.

| Symptom | Fix |
|---|---|
| Nothing listed, check undeclared | `cd` to the repo root. |
| `E-join-missing` | Add `join: all` where parallel branches meet. |
| `E-fork-agent` / `E-import-agent` | Fork source, or continued session, runs another CLI. Rebind the role. |
| No or undeclared verdict | The agent must end with `VERDICT: <signal>`. Steer it, or add `on: { unknown: … }`. |
| Two identical check failures | A stall. Read `hex logs`, fix the cause, start a new run. |
| Spend `—` or `≥ $X` | The agent reported no or partial price. hex never estimates. |
