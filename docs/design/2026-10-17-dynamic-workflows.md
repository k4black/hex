# Dynamic workflows

- **Status:** superseded on 2026-10-30. `hex run -` stays. The `parallel:` field was removed in favour of list edges and `join: all` between separate nodes: see [operator sessions](2026-10-30-operator-sessions.md).
- **One line:** the driving agent writes a graph for the task from the configured roles, pipes it to `hex run -`, and hex runs it deterministically. Read-only work fans out in parallel inside one node.

## Goal

- An agent (Claude Code with the hex skill, or any other agent) builds a workflow for one task at runtime, with no preset file.
- Example: sonnet researchers in parallel, then opus implements, then codex reviews.
- The roles come from the same config layers as today: project, then user, then built-in.
- hex stays simple. The kernel decides, a worker runs one agent, the runtime orchestrates and records.

## What existed before this branch

- `hex run <path.yaml>` ran any graph file. So an agent could already author a workflow. It had to write a file first.
- `resume` reads the snapshot `.hex/runs/<id>/graph.yaml`, not the original file. So a temporary graph is safe to resume.
- The kernel runs one node and one attempt at a time. `RunState.current_attempt` is a single `Option`.
- A `command` node already runs steps in parallel inside one attempt (`mode: parallel`).
- The gap: no parallel agents, and no one-step launch.

## Research summary

Sources: two research passes on 2026-10-17, saved in [`2026-10-17-dynamic-workflows-research.md`](2026-10-17-dynamic-workflows-research.md). The key ones are listed here.

| System | Mechanism | Take for hex |
|---|---|---|
| Claude Code `Workflow` tool ([docs](https://code.claude.com/docs/en/workflows)) | Claude writes a JS script with `agent()`, `parallel()` and `pipeline()`. Replay resume works only within the same session. | One-step launch. The agent writes the structure. A preview of the phases before the start. |
| Claude Code subagents ([docs](https://code.claude.com/docs/en/sub-agents)) | `Agent` tool, model chosen per spawn, several calls in one turn run at once. | Choose the model per role. Fan-out is one primitive. |
| Anthropic research system ([post](https://www.anthropic.com/engineering/multi-agent-research-system)) | A lead spawns 3–5 subagents. Effort scales with the task. Multi-agent costs about 15x chat tokens. | Put the effort-scaling rules in the skill. |
| LLMCompiler ([paper](https://arxiv.org/abs/2312.04511)) | An LLM emits a DAG, and a non-LLM unit dispatches it. | Precedent for "the agent plans, the engine runs". |
| Google ADK `ParallelAgent` ([docs](https://adk.dev/agents/workflow-agents/parallel-agents/)) | Isolated branches, each writes a named key, and a merger reads the keys. | Join results by name, in a fixed order. |
| Cognition, "Don't build multi-agents" ([post](https://cognition.com/blog/dont-build-multi-agents)) | Parallel writers make conflicting decisions. | Parallel reads, sequential writes. |
| Scaling-agents study ([paper](https://arxiv.org/abs/2512.08296)) | Multi-agent degrades sequential tasks by 39–70%. A central checker limits error amplification. | Do not fan out implementation. Keep gates and `accept.require`. |

Gaps that hex fills and Claude Code does not:
- Workers from several vendors in one run (claude, codex, pi, opencode).
- A durable journal, and `hex resume` after a crash or in a new session.
- Deterministic gates (`accept.require: [test.passed]`).
- Real budgets: visits per node, time per attempt, run-wide output tokens.
- Mid-run control: `steer`, `pause`, `human` nodes.

## Design (prototype)

### 1. The graph on stdin

- `hex run -`, `hex validate -` and `hex graph -` read the graph from stdin (`preset::resolve`).
- The run records `origin: stdin`. The snapshot in the run dir makes resume work.
- The agent writes no file. To keep a graph that worked, it saves the graph to `.hex/graphs/<name>.yaml`.

### 2. Fan-out as a field on `agent`, not a node kind

```yaml
research:
  agent:
    role: researcher                  # default role for the branches
    prompt: "Task: {{prompt}}"        # shared: every branch gets it first
    parallel:
      - prompt: "Map the auth module."
      - { role: planner, prompt: "List the risks." }
  on: { done: implement }
```

- IR: `NodeSpec::Agent.parallel: Vec<Branch>`. A `Branch` holds `{ worker, prompt, read_only }`, compiled like a node: the role preamble, then the node prompt, then the branch prompt.
- The kernel does not change. `reduce`, `schedule` and `lifecycle` see one attempt, one report, one result and one `done`.
- The runtime (`Session::start_fan_out`) runs the branches on scoped threads, like `run_parallel` for command steps. `Worker` is now `Sync`.
- Each branch writes `attempts/<id>/<n>-<role>/`, the same layout as a command step. `hex logs` and `--follow` show branches with no new code.
- Results: one `NodeResult` with a `[branch <n>-<role>]` section per branch, in declared order. The 16 KiB cap is split between the branches.
- Usage: one `AttemptReported`. `models` are concatenated. The total cost is `Some` only when every branch reported one, so an unpriced branch still shows as partial. No session id is recorded.
- Failure: any failed branch fails the attempt, after the other branches' output is saved into the result. All branches timed out gives `timed_out`. An interrupt pauses the run.

### 3. Validation rules (kernel `check_parallel`)

| Code | Rule | Why |
|---|---|---|
| `E-parallel-writer` | Every branch has the `read_only` flag. | Branches share one workspace. Two writers overwrite each other's files. |
| `E-parallel-proposes` | No `may_propose`. The node emits `done`. | N verdicts have no single answer. A downstream node routes on a verdict. |
| `E-parallel-continue` | No `context: continue`. | N sessions do not fit the one session handle a node has. |

- **`read_only` is advisory.** It is a prompt instruction, not a sandbox. A claude branch still runs with `--permission-mode acceptEdits` and can edit files. So `E-parallel-writer` checks the flag only. A misbehaving branch can still write at the same time as another.
- `check_workers` and `doctor::preflight` check every branch role (`NodeSpec::workers()`). A missing CLI stops the run before it starts.
- Template checks walk every branch prompt (`NodeSpec::prompts()`).

### 4. The skill

- New section "Compose a workflow on the fly": a heredoc example, the fan-out rules, "parallel reads, sequential writes", effort sizing, preview with `hex graph -`, and how to save a graph.

## Evidence

- `crates/hex-runtime/tests/fan_out.rs`:
  - two branches that each wait for the other's marker file (this proves they run concurrently), with the joined result in the downstream prompt and one attempt;
  - a failed branch fails the run and keeps the other branch's output;
  - a writing branch is refused with `E-parallel-writer`.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets`: clean.
- Live run from stdin (`2026-10-17-dyn-smoke`): 2 claude-sonnet-5-5 researcher branches in parallel (11 s), then a reviewer merge. Result `succeeded`, $0.37.

## Not built (phase 2 candidates)

1. **Graph-level fan-out and join.** `on: { done: [a, b] }` plus a join node that waits for all of them. Each branch is then a full node with its own visit bound, verdict and retries. Cost: `current_attempt` becomes a set, and `reduce`, `lifecycle`, `schedule` and `drive` all change.
2. **Writing branches.** One worktree per branch, plus an integration step (merge, or pick the best). This builds on `worktree.rs`.
3. **`map` over a computed list.** A planner emits a list, and hex fans out one branch per item. This needs typed node outputs. Today the same result takes two runs: one run produces the list, then the agent writes the next graph.
4. **Graph-local `roles:` overlay.** A graph overrides one role key (for example a model) for this run only, as a fourth config layer.
5. **Per-branch `model:`.** Rejected for now: the role is the unit of choice.
6. **Partial tolerance.** `parallel_policy: all | any` (succeed when at least one branch succeeds).
7. **A concurrency cap** for wide fan-outs.

## Open questions (for the grill)

- Is node-level fan-out the right first step, or do we go straight to graph-level fan-out and join?
- Syntax: `parallel: [...]` on `agent`, or `mode: parallel` + `steps:` to mirror `command`?
- Failure policy: fail on any branch (prototype), or continue with partial results?
- Is the read-only rule acceptable for now, or must writing branches with worktrees come first?
- How does an agent pick a model for one run: only through config roles, or with a graph-local `roles:` overlay?
- Does the launch need a preview or approval step in hex, or does the skill own that?
