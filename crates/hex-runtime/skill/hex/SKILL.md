---
name: hex
description: >-
  Run a multi-round coding task as a bounded, resumable loop with the hex CLI
  (github.com/k4black/hex). hex runs the agent CLIs you have (Claude Code, Codex,
  pi, opencode, any command) as workers over a small YAML graph. One agent
  implements. A different model reviews. Tests decide when the work is done. hex
  journals each step, bounds each loop and resumes after a crash. Use when the
  operator types /hex or names hex: `hex run`, a preset (critique-loop,
  implement-until-green, tdd, review, checklist, autoresearch), `.hex/` config,
  roles and models, or a run to steer, read or resume. Manual only: if a task
  only looks loop-shaped, propose /hex and stop. Do not use from a sub-agent or
  inside a hex attempt (HEX_RUN_ID is set). When loaded, read this whole skill
  before the first hex command and follow it.
argument-hint: "[task]"
disable-model-invocation: true
license: MIT
---

# hex

hex runs agent CLIs as workers in a bounded loop over a small YAML graph: https://github.com/k4black/hex.
It journals every step. Checks and `accept.require` decide when the run succeeds. The agent's "done" is only a proposal.
`hex --help` and `hex <verb> --help` are authoritative. If this file and the CLI disagree, trust the CLI and tell the operator.

## Install if missing

Run `hex --version`. If `hex` is not on PATH, tell the operator to install it. Do not install it unasked.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/k4black/hex/releases/latest/download/hex-cli-installer.sh | sh
brew install k4black/tap/hex-cli
uv tool install hex-agents-cli   # or: pipx install hex-agents-cli
npm install -g @k4black/hex-cli
cargo binstall hex-cli           # or: cargo install hex-cli
mise use -g github:k4black/hex
```

Then, from the repository root: `hex init`, then `hex doctor`. `hex skill install` keeps this skill in sync with the binary.

## When to use it

Use hex only on the operator's direct request, and only when all of these hold:
- The task takes more than one round.
- "Done" is checkable: a test, or a reviewer's verdict.
- A different model should judge the work, or the work must survive a crash or outlive one context window.

Do not use it when:
- You can make the change directly.
- Every step needs a human decision.
- You are a sub-agent, or you are inside a hex attempt (`HEX_RUN_ID` is set). A nested run is a loop nobody budgeted.

**Cost is real.** One 10-attempt cross-model run cost more than $9.70 over 40 minutes and accepted nothing.
Worst case is about (total node visits) × attempt bound (default 30m per attempt, 5 visits per node). Tell the operator the estimate before you start.

## List and run graphs

Run hex from the repository root. hex reads `.hex/` in the current directory and does not walk up.

```sh
hex list                # graphs: project > user > built-in
hex graph <graph>       # nodes, edges, bounds
hex validate <graph>    # schema, references, a reachable success
```

| Preset | Loop | Needs `checks.test` |
|---|---|---|
| `critique-loop` | implement → review, until approved | no |
| `implement-until-green` | implement → test, until green | yes |
| `tdd` | failing test → prove red → implement → prove green | yes |
| `review` | one reviewer over the current diff | no |
| `checklist` | one `- [ ]` item per round, critique loop each, then one holistic review | no |
| `autoresearch` | research → critic judges sufficiency → report | no |

```sh
hex run critique-loop -p "fix the retry in src/http/client.rs; see tests/retry.rs:88" --name retry
```

- The prompt is the only operator input: `-p "<text>"` or `-f <file>`. Reference files by path. Do not paste their content.
- `review` routes `changes_requested` to a failure terminal. A useful review exits 1.
- `checklist`: write a Markdown file of `- [ ]` items, one bounded change each. Put the path first: `hex run checklist -p "plan.md: <goal>"`. Progress is the `[x]` marks. The final reviewer can add items.
- `--worktree [<base>]` runs in an isolated git worktree on branch `hex/<run-id>`. hex does not merge it.

## How graphs work

- Four node kinds: `agent`, `command`, `human`, `terminal`. A role (implementer, reviewer) is metadata on an `agent` node.
- A multi-outcome agent ends its final message with `VERDICT: <signal>`. The signal must be in the node's `may_propose` list. hex adds this instruction itself. Edges (`on:`) pick the next node.
- A node with no `may_propose` emits `done` on a clean exit, so it needs an `on: { done: … }` edge.
- A `command` node runs a declared check and emits `passed` or `failed`. `accept.require` (for example `[test.passed]`) names the evidence a success terminal needs.
- Each node has a visit bound (default 5, `budget: { visits: N }` on the node). Each attempt has a time bound (default 30m, `defaults: { budget: { attempt: 20m } }`).
- `hex graph critique-loop --format source` prints a full working example.

## Override and extend

- Layers, highest first: project `.hex/config.yaml` and `.hex/graphs/`, then user `~/.config/hex/config.yaml` and `~/.config/hex/graphs/`, then built-in. Config deep-merges per key: set only the keys that differ.
- A graph with the same name overrides the lower one. To customize a preset: `hex graph <name> --format source > .hex/graphs/<name>.yaml`, edit, then `hex validate <name>`.
- `checks:` maps a name to an argv, for example `test: [cargo, test, --workspace]`. It is empty by default. A graph that names an undeclared check does not start.

## Choose agents and models per role

A graph names a role. A role binds a `worker` (claude, codex, pi, opencode, or a `kind: command` worker) to a `model`, an `effort`, a `read_only` policy and a prompt preamble.

```yaml
roles:
  implementer: { worker: claude, model: claude-opus-5-5, effort: high }
  reviewer:    { worker: codex, model: gpt-6-sol, prompt_append: "Read AGENTS.md first." }
```

- Defaults: claude implements and researches, codex reviews and plans. No role pins a model.
- `prompt` replaces the inherited preamble. `prompt_append` extends it. `read_only` is advisory.
- One CLI only: rebind the roles. Only claude: `reviewer: { worker: claude, model: <another claude model> }`, `planner: { worker: claude }`. Only codex: `implementer` and `researcher` to `{ worker: codex, model: <codex model> }`.
- When you change a `worker`, also set the `model`. A model from another layer may not exist in the new CLI. `hex doctor` reports it.
- pi model ids need the provider prefix: `openrouter/<vendor>/<model>`.
- A `roles:` entry hides a same-named `workers:` entry. Do not name a scratch worker `implementer`, `reviewer`, `planner` or `researcher`.

## Best practices

- Review with a different model from the one that implements, for example claude implements and codex reviews.
- Declare `checks:` so a test decides "done".
- Keep the default bounds.
- Steer a live run instead of restarting it.
- Fix a red base first. A check already red before the run (for example a workspace-wide `cargo fmt --check`) can wedge a scoped task.
- A live run keeps the roles it compiled. To change a model mid-task: `hex pause`, edit config, `hex resume`.

## Operate a run

```sh
hex run critique-loop -p "<task>" --name retry > /tmp/hex-retry.log 2>&1 &
tail -5 /tmp/hex-retry.log          # each line has a HH:MM:SS timestamp
hex runs                            # run ids and liveness
hex status <run>                    # node, in-flight attempt, queued steers, spend (--usage for detail)
hex logs <run> --follow             # stream agent output (--node N, --tail N, --full)
hex steer <run> "prefer the existing retry helper"
hex respond <run> "approved"        # answer a blocking human node
hex wait <run>; echo "exit $?"      # block until it ends
hex pause <run>                     # stop at the next attempt boundary
hex resume <run>                    # continue the same run after a pause or crash
hex cancel <run>
```

- `hex run` blocks. Background it with `&`. A shell exit can HUP it; use tmux if the run must outlive the terminal.
- A steer applies to the next attempt, not the one in flight. `hex status` shows `queued steer (not yet picked up)`, then `steer accepted`.
- `hex run` and `hex resume` end by printing `why`, the failed checks, the final message and the spend. Add `--json` for machine output.
- If hex blocks you or lacks a capability, record it: `hex feedback "<what happened>" --kind issue`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | succeeded |
| 1 | failed, including a reviewer requesting changes |
| 2 | usage, graph or config error: read stderr, do not retry unchanged |
| 3 | timed out |
| 4 | budget exhausted (a node's visit bound, elapsed or output tokens) |
| 5 | cancelled |
| 6 | paused: `hex resume` continues |

## Errors and fixes

| Symptom | Fix |
|---|---|
| `hex list` or `hex runs` shows nothing, or a check reads as undeclared | `cd` to the repo root. Do not create a second `.hex/`. |
| `hex doctor`: model not in the CLI's catalog | Set a model the role's worker lists, or change the worker. |
| A role change has no effect on a live run | `hex pause`, edit config, `hex resume`. |
| An attempt fails with no or an undeclared verdict | The agent must end with `VERDICT: <signal>` from `may_propose`. Steer it, or add an `on: { unknown: … }` edge. |
| The run fails after two identical check failures | A stall. Read `hex logs`, fix the cause, start a new run. |
| A scoped run cannot turn a check green | The base is red. Fix the base first. |
| `--json` with `--follow` is rejected | Use one of them. |
| Spend shows `—` or `≥ $X` | The agent reported no price, or part of it (codex reports tokens only). hex never estimates. |
