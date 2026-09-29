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

hex runs an agent CLI (codex, claude, pi, opencode) in a bounded loop over a graph.
It journals every step. Gates and `accept.require` decide when the run succeeds. The agent's "done" is only a proposal.

`hex --help` and `hex <verb> --help` are authoritative. If this file and the CLI disagree, trust the CLI and tell the operator.
`hex skill install` keeps this skill in sync with the installed binary.

## If `hex` is not on PATH

The skill can be installed before the binary (for example with `npx skills`). Then tell the operator to install hex.
Do not install it for them unless they ask.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/k4black/hex/releases/latest/download/hex-cli-installer.sh | sh
brew install k4black/tap/hex-cli
uv tool install hex-cli          # or: pipx install hex-cli
npm install -g @k4black/hex-cli
cargo binstall hex-cli           # or: cargo install hex-cli
mise use -g github:k4black/hex
```

After install, from the repository root: `hex init`, then `hex doctor`.

## When to use it

Use hex only on the operator's direct request. If a task looks loop-shaped, propose `/hex` and stop.

Use it when all of these hold:
- The task takes more than one round.
- "Done" is checkable: a test, or a reviewer's verdict.
- You want a different model to judge the work, or the work must survive a crash or outlive one context window.

Do not use it when:
- You can make the change directly.
- Every step needs a human decision.
- You are a sub-agent, or you are inside a hex attempt (`HEX_RUN_ID` is set). A nested run is a loop nobody budgeted.

**Cost is real.** One 10-attempt cross-model run cost more than $9.70 over 40 minutes and accepted nothing.
Worst case is about (total node visits) × `budget.attempt`. Tell the operator the estimate before you start.

## Orient

Run hex from the repository root. hex reads `.hex/` in the current directory and does not walk up.

```sh
hex doctor              # agent CLIs installed and logged in, models known, checks runnable
hex list                # graphs: project > user > built-in
hex graph <graph>       # nodes, edges, bounds
hex validate <graph>    # schema, references, a reachable success
```

## Choose a preset

| Preset | Loop | Needs `checks.test` |
|---|---|---|
| `critique-loop` | implement → review, until approved | no |
| `checklist` | one `- [ ]` item per round, critique loop each, then one holistic review | no |
| `review` | one reviewer over the current diff | no |
| `autoresearch` | research → critic judges sufficiency → report | no |
| `implement-until-green` | implement → test, until green | yes |
| `tdd` | failing test → prove red → implement → prove green | yes |

- `review` routes `changes_requested` to a failure terminal. A useful review exits 1.
- `checklist`: write a Markdown file of `- [ ]` items, one bounded change each. Put the path first in the prompt:
  `hex run checklist -p "plan.md: <goal>"`. Progress is the `[x]` marks. The final reviewer can add new items.
- To customize: `hex graph <name> --format source > .hex/graphs/<name>.yaml`, edit, then `hex validate <name>`.

## Operate a run

The prompt is the only operator input: `-p "<text>"` or `-f <file>`, not both.
Reference files by path. Do not paste their content.

```sh
hex run critique-loop -p "fix the retry in src/http/client.rs; see tests/retry.rs:88" --name retry \
  > /tmp/hex-retry.log 2>&1 &
tail -5 /tmp/hex-retry.log          # each line has a HH:MM:SS timestamp: the current state
hex runs                            # run ids and liveness
hex status <run>                    # node, in-flight attempt, queued steers, spend (--usage for detail)
hex logs <run> --tail 20            # what the agent says (--node N, --full, --follow)
hex steer <run> "prefer the existing retry helper"
hex respond <run> "approved"        # answer a blocking human node
hex wait <run>; echo "exit $?"      # block until it ends
hex pause <run>                     # stop at the next attempt boundary
hex resume <run>                    # continue the same run after a pause or crash
hex cancel <run>
```

- `hex run` blocks. Background it with `&`. A shell exit can HUP it; use tmux if the run must outlive the terminal.
- A steer applies to the next attempt, not the one in flight. `hex status` shows `queued steer (not yet picked up)`, then `steer accepted`.
- `hex run` and `hex resume` end by printing `why`, the failed checks, the final message and the spend. `--json` gives the same fields.
- If hex itself blocks you or lacks a capability, record it: `hex feedback "<what happened>" --kind issue`.

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

## Pitfalls

- **Wrong directory.** `hex list` or `hex runs` shows nothing, or a check reads as undeclared: `cd` to the repo root. Do not create a second `.hex/`.
- **Undeclared check.** A graph that names a check missing from `checks:` in `.hex/config.yaml` does not start. Add it.
- **Role shadows worker.** A `roles:` entry hides a same-named `workers:` entry. Do not name a scratch worker `implementer`, `reviewer`, `planner` or `researcher`.
- **pi model ids** need the provider prefix: `openrouter/<vendor>/<model>`.
- **Changed roles.** A live run keeps the roles it compiled. To switch a model mid-task: `hex pause`, edit config, `hex resume`.
- **No verdict.** A multi-outcome agent must end with `VERDICT: <signal>`. hex adds that instruction itself. A missing or undeclared verdict fails the attempt.
- **Stall.** A gate that fails twice with identical output ends the run `failed`.
- **Red base.** A gate already red before the run starts (for example a workspace-wide `cargo fmt --check`) can wedge a scoped task. Fix the base first.
- **`--json` with `--follow`** is rejected.
- **Cost display.** `—` means the agent reported no price (codex reports tokens only). `≥ $X` means part of the spend is unpriced. hex never estimates.
