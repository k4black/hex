# hex as the operator's sub-agents

- **Status:** built and tested, 2026-10-30, branch `feat/dynamic-workflows` (uncommitted). Replaces the `parallel:` part of [dynamic workflows](2026-10-17-dynamic-workflows.md).
- **One line:** a main Claude Code or Codex session plans the work and calls hex to run bounded graphs of other agents. Nodes run in parallel when an edge lists several targets. A node can fork the session of another node. A new run can continue the sessions of an earlier run.

## Vocabulary

- **Operator session:** the main Claude Code or Codex conversation that plans and calls hex. It is never inside a hex attempt (`HEX_RUN_ID` is not set).
- **Agent session:** the conversation handle of one agent CLI inside a run: `SessionHandle { agent, id }` in `RunState.sessions[node]`.
- `hex resume <run>` continues **the same run** after a pause or a crash.
- `hex run <graph> --continue <run>` starts **a new run** that takes over the agent sessions of an earlier run.

## Goal

- Agents from several vendors in one graph: sonnet researches, opus implements, codex reviews.
- The operator writes a graph on the fly (`hex run -`) or reuses one (a preset, `.hex/graphs/`).
- Several nodes run at once. Each node keeps its own bound, verdict and session.
- A node forks the session of another node: research once, then N implementers start from that context.
- The operator calls the same implementer or reviewer again in a new run, with its context.
- The design stays small: easy to maintain, few bugs, easy to use.

## The model in one example

```yaml
version: 1
name: three-ways
entry: research
nodes:
  research:
    agent: { role: researcher, prompt: "Map the auth module for: {{prompt}}" }
    on: { done: [impl_a, impl_b, impl_c] }          # list edge: all three start
  impl_a:
    agent: { role: implementer, context: { fork: research }, prompt: "Approach A. {{review.result}}" }
    on: { done: review }
  impl_b:
    agent: { role: implementer, context: { fork: research }, prompt: "Approach B. {{review.result}}" }
    on: { done: review }
  impl_c:
    agent: { role: implementer, context: { fork: research }, prompt: "Approach C. {{review.result}}" }
    on: { done: review }
  review:
    join: all                                        # waits until the branches are done
    agent: { role: reviewer, prompt: "Compare A, B, C: {{impl_a.result}} {{impl_b.result}} {{impl_c.result}}", may_propose: [approved, changes_requested] }
    on: { approved: done, changes_requested: [impl_a, impl_b, impl_c] }
  done: { terminal: succeeded }
```

- Round 1: each implementer forks the researcher session. Round 2: each implementer continues **its own** forked session.
- Writers share one workspace unless the caller isolates them. Per-node worktrees come later.

## Decisions

### 1. List edges

- `on: { <signal>: [a, b] }` enters every target. A single target stays a string.
- `Graph::route` returns the target list. `reduce` enters each one.

### 2. Kernel state: a set of active nodes

- `RunState.current: Option<String>` and `current_attempt: Option<String>` become one map: `active: BTreeMap<node, Option<attempt_id>>`.
- `reduce` reads the node from `event.node_id`, not from `state.current`. Every event already carries `node_id` and `attempt_id`. `results`, `signals`, `visits` and `sessions` are already per node.
- `reported_attempt` becomes a set of attempt ids.
- `lifecycle` keeps one predicate per rule. "Idle" becomes "this node is active with no attempt". `check_journal` folds through `reduce`, so it changes with the predicates.
- `schedule` returns one effect for each active node that has no attempt. Attempt ids are `att_<n>` in order.

### 3. Join = quiescence

- A join, a `human` node and a success terminal wait until the run is quiet: nothing in flight and every active node waiting.
- Then every join starts. With no join active, one `human` node is asked first, else one success terminal settles. So waiters never deadlock.
- A node entered twice while it is active counts one visit. So branches that meet must meet at `join: all`: validation refuses the graph otherwise (`E-join-missing`). `join` is allowed only on agent and command nodes (`E-join-kind`).
- A failure terminal and an `AttemptFailed` end the run at once.
- Ceiling: a join waits for every other active node, also for the branches of an unrelated fan-out. Add reachability only when a real graph needs it.

### 4. Driver: one writer, N workers

- The main thread is the only journal writer. Each attempt runs on a scoped thread and sends its outcome back on a channel.
- A failed attempt stops its siblings: each sibling journals `AttemptInterrupted`, then the run journals `AttemptFailed` and `RunFinished`.
- Sibling stop uses one per-run stop flag in the 25 ms `wait_bounded` tick. Ctrl-C keeps the global interrupt flag.
- `pause` stops new attempts, lets the attempts in flight finish, then journals `RunPaused`.
- Control (`steer`, `cancel`, `pause`) is read each time the loop wakes: an attempt ends, or a 250 ms poll times out.
- A `steer` may now land while attempts run (`steer_ok`). The next agent attempt that starts reads it.
- A worker panic becomes a failed attempt. It no longer unwinds out of `hex run`.
- The live preview shows one attempt. Parallel attempts show as event lines.

### 5. Fork inside a run: `context: { fork: <node> }`

- `context` is `fresh`, `continue` or `{ fork: <node> }`.
- At attempt start: if the node has its own session, it continues it. Else it forks the session of `<node>`. Else the attempt fails: "`research` has no session yet".
- The forked attempt reports its new id into `sessions[self]`. So the next visit continues its own session.
- New `Capability::SessionFork`: claude (`--resume <id> --fork-session`), codex (`exec fork <id>`).
- Compile checks: the target exists, is an agent node, is not the node itself, and runs the same program (`E-fork-agent`). The worker declares `SessionFork`.
- Checked on 2026-10-30: `claude -p --resume <id> --fork-session --model sonnet` gives a new id, keeps the context, and runs the new model. codex `exec fork -m` exists as a flag. It is not tested.

### 6. Continue a run: `hex run <graph> --continue <run>`

- The new run reads the sessions of the source run. A node with `context: continue` (or a fork node) starts from the handle of the node with the same name.
- The new run records the seeds in `RunCreated.sessions`. `hex resume` of the new run reads only its own journal.
- A source run that is still live is refused. The error names `hex wait` and `hex pause`.
- A seed whose program differs from the node's worker is refused before the start (`E-import-agent`).
- `hex status <run> --json` shows `sessions`, so the operator can choose a run to continue.

### 7. `parallel:` is removed

- The prototype `parallel:` field on `agent` is deleted: `Branch`, `check_parallel`, `start_fan_out`, `merge_reports`, `tests/fan_out.rs`.
- Separate nodes do everything a branch did. They also have their own bound, verdict, session and logs.
- `Worker: Sync` and `hex run -` (graph on stdin) stay.

## Operator recipes (the skill)

| Need | How |
|---|---|
| Implement → review | `hex run critique-loop -p "<task>" --name auth` |
| Same implementer, new comments | `hex run critique-loop --continue <auth-run> -p "<comments>"` |
| Same reviewer, review again | a graph whose `review` has `context: continue`, with `--continue <run>` |
| Research → plan | research nodes in a list edge, then a `planner` node with `join: all` |
| Review with CI | `command: { check: test }` and `accept.require: [test.passed]` |
| N approaches from one context | the example above |
| Isolated features | N × `hex run ... --worktree &`, then `hex wait` each |

## Phases

Each phase ends with tests green and clippy clean.

1. Kernel: list edges, `active` map, join, lifecycle. Pure tests.
2. Driver: concurrent attempts, sibling stop, pause.
3. Fork: `context: { fork }`, `SessionFork`, claude and codex argv.
4. `--continue <run>`, `RunCreated.sessions`, `sessions` in status, the two refusals.
5. Delete `parallel:`.
6. Docs: README, skill, AGENTS.md, TODO.md.

## Decided in the second grill (2026-10-30)

- Join scope stays a global barrier.
- Branches of one list edge that meet must meet at `join: all` (`E-join-missing`). The check walks from each target and stops at a join, a `human` node or a terminal.
- `hex steer --node <n>` steers one node. Without `--node`, the next agent attempt reads it.
- `hex cancel` stops the attempts in flight.
- `hex status` (`in_flight` is now a list in `--json`) and `hex logs --follow` (lines start with `[node]` while several attempts run) show every parallel attempt. The live preview still shows one.
- Billing: claude and codex report session totals on resume and fork. hex bills the difference to the session's last totals, also across `--continue`.
- Per-node worktrees: later.

## Codex review (2026-10-30)

`codex exec -s read-only`, high effort, over the whole diff. 9 findings, verdict CHANGES_REQUESTED. Fixed:

1. A command verdict with no edge ended the run inside `reduce` with siblings still running: `Session::signal` now fails the attempt.
2. `join` on a human or terminal bypassed their wait: `E-join-kind`.
3. `root → [join_a, join_b] → merge` passed validation: the walk now passes joins that only some branches reach.
4. Two runs could continue one session: `--continue` refuses a session another run already took over.
5. An error mid-run left attempts unjournaled: `drive` now drains them first.
6. Two success terminals, two humans, or a human and a terminal deadlocked: one quiet rule (joins, then one human, then one terminal).
8. A codex fork under another model billed in full: a one-model report subtracts a one-model base.

Kept as `shortcut:` comments: fork billing when the source reruns during the fork (7), and the process-wide stop flag (9: one run per `hex` process).

Second pass, 5 findings. Fixed: a join no longer starts before a parallel `human` node is answered (a quiet run asks the human first); `--continue` records `continued_from`, so A → B → C works, a second taker of A is refused, and `hex resume A` is refused once B took A's sessions. Kept as `shortcut:` comments: conditional routes can still pick joins that meet at a non-join (the static check sees "can reach"), and two `--continue` commands at the same instant can both pass the unlocked scan.

Third pass over those fixes: no findings, VERDICT: APPROVE.

## Evidence

- Kernel tests: a list edge starts both targets with distinct attempt ids; a join waits and is entered once; a success terminal waits and a failure terminal does not; `check_journal` reads a fan-out journal.
- `crates/hex-runtime/tests/parallel.rs`: two nodes that wait for each other's marker file (this proves they run at once) and a join that reads both; two nodes that fork one session and then continue their own; `--continue` resumes the earlier session, and a program mismatch is refused (`E-import-agent`).
- `crates/hex-runtime/tests/parallel_stop.rs`: a failed node stops its sibling in under 4 s, and `AttemptInterrupted` comes before `AttemptFailed`.
- `cargo test --workspace`: all pass. `cargo clippy --workspace --all-targets`: clean.
- Live, claude-haiku-5-5, total under $0.07:
  - `research` → `[alt_a, alt_b]` (both fork) → `judge` (join). Both forks named the secret word from the research session. The two forks ran in parallel (4 s and 6 s).
  - `hex run - --continue <run>` on `alt_a` with `context: continue`: the agent recalled its earlier answer.
  - Fork billing: fork `a` reported $0.00764 for the whole session and was billed $0.00132 (minus the source's $0.00632).
  - `hex steer --node b` while `research` ran: only `b` ended with `STEERED`. `--node nope` exits 2. `hex status` listed `a` and `b` in flight; `--follow` printed `[a]` and `[b]` lines.

## Roles

- Built-in `scout`: claude, `claude-sonnet-5-5`, read-only, preamble "fast, narrow lookup". The only built-in role that pins a model.
- New roles go in project `.hex/config.yaml`. The skill proposes a role, and adds it only after the user approves. There is no graph-local `roles:` overlay.

## Not in this pass

- Per-node worktrees for parallel writers.
- `join: any` and partial tolerance.
- A concurrency cap.
