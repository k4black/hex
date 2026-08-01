# The six built-in graphs

Read `SKILL.md` first. Fork any of these with
`hex graph <name> --format source > .hex/graphs/<name>.yaml` — a project graph of
the same name wins over the built-in.

## Contents

- [critique-loop](#critique-loop)
- [plan-build-review](#plan-build-review)
- [review](#review)
- [implement-until-green](#implement-until-green)
- [tdd](#tdd)
- [autoresearch](#autoresearch)
- [Which one](#which-one)
- [Adding a gate to a gate-free preset](#adding-a-gate-to-a-gate-free-preset)

## critique-loop

`implement → review`, looping on `changes_requested`. The flagship.

- `implement` — role `implementer`, **`context: continue`** (keeps what it worked
  out across rounds).
- `review` — role `reviewer`, **fresh every round on purpose**: a reviewer
  continuing its own session carries its earlier verdict forward, which is how a
  critic talks itself into approving what it already argued about. `visits ≤ 4`.
- `accept: require [review.approved]`, `on_unmet: implement`.

Cross-model by default — `reviewer` binds a different agent from `implementer`,
so the model judging the change is not the one that wrote it.

## plan-build-review

`plan → implement → review`, review loops back to implement. Use when the task
needs decomposing before any code is written. `planner` is read-only.

## review

One reviewer over the current working-tree diff. No implementer, nothing edited.

**`changes_requested` routes to a `failed` terminal**, so a review that finds real
problems exits 1. That is the graph's design, not a hex failure — read the final
message before concluding anything broke.

## implement-until-green

`implement → test`, test failure loops back. The Ralph shape.

**Requires `checks.test`.** Refuses to start without it.

## tdd

`spec → red → implement → green`. Writes a failing test, *proves* it fails, then
makes it pass and proves that.

- `spec` and `implement` both **continue** their sessions; `red` sends `spec` back
  when the new test did not actually fail.
- **Requires `checks.test`.**

## autoresearch

`research → critique → report`. The critic emits `enough` or `more_needed`;
`visits ≤ 4` is the backstop, because a model asked "is this enough?" will
eventually say yes for the wrong reason.

Continuity is **on disk**, in `.hex/research-notes.md`, not in the agent's
context — every attempt is a fresh session that reads and extends the notes. That
is what lets a bounded loop of stateless attempts do long-horizon work, and it is
why this preset alone stays all-`fresh`.

## Which one

| You want | Use |
|---|---|
| a change, reviewed by another model until it is good | `critique-loop` |
| the same, but the task needs planning first | `plan-build-review` |
| a second opinion on a diff you already have | `review` |
| a change driven to a green test suite | `implement-until-green` |
| a test written first and proven red, then green | `tdd` |
| a question answered with sourced evidence | `autoresearch` |

## Adding a gate to a gate-free preset

The three gate-free presets are that way so they run in any repo with no setup.
To add real backpressure:

```bash
hex graph critique-loop --format source > .hex/graphs/critique-loop.yaml
```

Declare the check in `.hex/config.yaml`:

```yaml
checks: { test: [pytest, -q] }
```

Then add the node and require its verdict:

```yaml
  test:
    command: { check: test }
    on: { passed: done, failed: implement }
accept:
  require: [review.approved, test.passed]
```
