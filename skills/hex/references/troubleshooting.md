# hex errors: text → cause → fix

Read `SKILL.md` first. Every string below is a literal hex message.

## Contents

- [Nothing here is what I expect](#nothing-here-is-what-i-expect)
- [Refused before the run starts](#refused-before-the-run-starts)
- [Config problems](#config-problems)
- [During a run](#during-a-run)
- [Reading a finished run](#reading-a-finished-run)
- [Exit code says 2](#exit-code-says-2)

## Nothing here is what I expect

**`hex list` shows no project graphs · `hex doctor` shows no checks · `hex runs`
says "no runs yet"** — and you know they exist.

You are not in the repository root. hex reads `.hex/` from the **current
directory** and does not walk up. Worse, `hex run` here would create a *second*
`.hex/`. `cd` to the root and retry.

The nastiest form: `hex run implement-until-green` reporting ``add `checks.test`
to .hex/config.yaml`` in a repo where `checks.test` **is** declared — one
directory up. Do not add a duplicate config; change directory.

## Refused before the run starts

**``command `test` needs check `test`, which this project does not declare — add
`checks.test` to .hex/config.yaml``**
The graph names a check your project has not defined. Add it, or use a literal
`command: { run: [[...]] }` step instead. Never silently passes, by design.

**``node `implement` asks for `context: continue`, but worker `implementer`
cannot resume a session``**
You rebound the role to a worker that has no sessions (a `kind: command` worker).
Set `context: fresh` on that node, or bind the role to codex or claude.

**`graph contains a cycle that no bound stops`** (`E-unbounded-cycle`)
Add `budget.attempts`, `budget.cycle_visits`, or `budget: { visits: N }` on a node
in the loop.

**``no `terminal: succeeded` is reachable from entry — this graph can only fail``**
Every path ends in failure. Add a success terminal and an edge to it.

**`MISSING self hex` from `hex doctor`**
The `hex` binary is not on `PATH`. A worker calls `hex emit` as a plain `PATH`
lookup in its own shell, so any node with more than one outcome cannot route.
Install it, or add its directory to `PATH`.

**`MISSING worker codex` / `MISSING check lint`**
The agent CLI or the check's program is not installed.

## Config problems

**``yaml: workers.faker: unknown field `argv`, expected one of `kind`, `model`,
`command`, `result```**
A typo. Every config and graph struct denies unknown fields, so the message names
the alternatives.

**A worker runs the wrong agent entirely**
A `roles:` entry **shadows** a same-named `workers:` entry — roles are registered
last and win. Naming a scratch `kind: command` worker `implementer` silently runs
the real role's agent CLI. Name test workers something that is not a role.

## During a run

**``hex emit must be run inside a hex attempt (HEX_EMIT_FILE unset)``**
You ran `hex emit` yourself. It is worker-side only; the runtime injects the
environment.

**``agent emitted `x` which is not in may_propose``**
The agent invented a signal. Add it to the node's `may_propose` *and* give it an
edge, or fix the prompt to name the allowed signals.

**A steer seems to have been ignored**
Check `hex status`. `queued steer (not yet picked up)` means no driver has
claimed it; `steer accepted` means it is journaled and waiting. Either way an
attempt **already in flight will not see it** — steering lands between attempts.

**`hex cancel` says "cancel queued for …" and the run keeps going**
A live driver holds the journal's write lock, so the cancel is queued and applied
at the next attempt boundary. With a 30m attempt bound that can take a while.

**The run is `running` but nothing is happening**
`hex status` shows the in-flight attempt and how long it has been going. If the
process is gone, `hex runs` shows `hung` or `abandoned`; `hex resume <run>`
continues it and marks the orphaned attempt `interrupted`.

## Reading a finished run

**`(no final message captured)`**
The attempt produced no result. If it timed out or crashed, hex salvages a
bounded tail instead, prefixed `[partial output — …]`. If you see neither, the
attempt died before writing anything.

**A cost of `—`, or a total shown as `≥ $X`**
codex reports tokens and no money. hex ships no price table and never estimates,
so an unpriced attempt shows `—` and a mixed run's total is a lower bound with a
line saying how many attempts went unpriced.

**`unreadable` in `hex runs`, with a yaml error**
The run's stored graph was written by an older hex whose schema has since
changed. The run's history is intact but cannot be replayed by this binary.

**Exit 1 but the work looks fine**
With `review` (and any graph routing `changes_requested` to a failure terminal),
a reviewer finding real problems is exit 1. Read the final message before
concluding hex broke.

## Exit code says 2

Exit 2 covers three different things:

1. a malformed command line (clap rejected it),
2. an unknown run id,
3. **a graph or config problem** — an undeclared check, an invalid graph.

Read stderr. **Do not retry the same command** — cases 2 and 3 will never succeed
without a change on disk.
