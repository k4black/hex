# hex graph YAML — full schema and error codes

Read `SKILL.md` first. This is the reference for writing or fixing a graph.

## Contents

- [Top level](#top-level)
- [`defaults:`](#defaults)
- [`budget:`](#budget)
- [Nodes](#nodes)
  - [`agent`](#agent)
  - [`command`](#command)
  - [`human`](#human)
  - [`terminal`](#terminal)
  - [Per-node budget](#per-node-budget)
- [Edges: `on:`](#edges-on)
- [`accept:`](#accept)
- [Prompt interpolation](#prompt-interpolation)
- [Durations](#durations)
- [Validation error codes](#validation-error-codes)

The loader uses `deny_unknown_fields` on every struct below, so a misspelled key
is an error naming the field and the alternatives — not a silent no-op.

## Top level

| Key | Type | Required | Notes |
|---|---|---|---|
| `version` | int | no (default 1) | Only `1` is accepted. |
| `name` | string | **yes** | |
| `description` | string | no | One line, shown by `hex list`. |
| `example` | string | no | Example prompt, shown by `hex list`. |
| `entry` | string | **yes** | Node id the run starts on. |
| `defaults` | map | no | Fallbacks for every node. |
| `nodes` | map | **yes** | Node id → node body. |
| `accept` | map | no | The acceptance contract. |

## `defaults:`

| Key | Notes |
|---|---|
| `role` | Default role for agent nodes (`worker:` is an accepted alias). |
| `context` | `fresh` (default) or `continue`. |
| `budget` | The run-wide budget — see below. |

## `budget:`

Run-wide, declared under `defaults:`. A **node's** `budget:` takes `visits` only.

| Key | Type | Notes |
|---|---|---|
| `attempts` | int | Total attempts across the whole run. |
| `elapsed` | duration | Wall clock for the run. |
| `attempt` | duration | Per attempt. **Always set** — defaults to 30m, so no attempt can hang forever. |
| `cycle_visits` | int | Blanket per-node visit cap. |
| `output_tokens` | int | Run-wide **generation** tokens. `0` is `E-budget-zero`. |

Whichever bound bites first wins. `attempts × attempt` is the worst-case wall
clock: `attempts: 12` with the default is six hours.

## Nodes

Kind-as-key: exactly one of `agent`/`command`/`human`/`terminal` per node, plus
optional `on:` and `budget:`.

### `agent`

| Key | Type | Notes |
|---|---|---|
| `role` | string | The role to run as (`worker:` is an alias). Falls back to `defaults.role`. |
| `prompt` | string | **Required.** Block scalars are the norm. |
| `may_propose` | list | Signals this node may emit. Must match its outgoing edges exactly (except reserved `done`). |
| `context` | `fresh`\|`continue` | `continue` resumes *this node's* session; refused at compile time if the worker cannot resume. |
| `read_only` | bool | Advisory — conveyed through the prompt, not enforced by a sandbox. |

### `command`

Either `check:` (a name from `.hex/config.yaml`) or `run:` (a literal argv). One
or several of each.

```yaml
verify:
  command:
    check: [fmt, clippy, test]     # names from config
    mode: parallel                 # or `ordered` (default)
  on: { passed: review, failed: implement }

smoke:
  command:
    run: [sh, -c, "curl -sf localhost:8080/health"]     # one literal argv, no config needed
  on: { passed: done, failed: fix }

two-steps:
  command:
    run:                                                # or several
      - [cargo, fmt, --all, --check]
      - [cargo, clippy, --workspace]
  on: { passed: done, failed: fix }
```

`run:` is the escape hatch when a command is graph-specific and does not belong
in project config. It is **not** documented in the README; it exists and works.

`mode` differs in *failure* semantics, not just concurrency:

- `ordered` (default) — stop at the first failure.
- `parallel` — run every step, fail if any did. Output buffers to
  `attempts/<id>/<n>-<label>/` in **declared** order.

A command node emits `passed` or `failed`. It becomes a **gate** when
`accept.require` names its signal.

### `human`

```yaml
approve:
  human: { prompt: "Ship to prod, or hold?" }
  on: { resolved: deploy }        # exactly one outgoing edge
```

Blocks until `hex respond <run> "text"`. The answer becomes the node's result,
so `{{approve.result}}` works downstream. Exactly one outgoing edge — an answer
is text, not a signal, so hex could not choose between two.

### `terminal`

```yaml
done:   { terminal: succeeded }
report: { terminal: failed }
```

**Only `succeeded` and `failed` may be written.** The other dispositions a run
can end with (`cancelled`, `budget_exhausted`, `timed_out`) are outcomes hex
assigns; a graph cannot declare them. Terminals have no outgoing edges. At least one `succeeded` terminal must be
reachable from the entry.

### Per-node budget

```yaml
review:
  budget: { visits: 3 }
```

Bounds one loop without capping any other. `visits: 0` is rejected.

## Edges: `on:`

```yaml
on:
  approved: done
  changes_requested: implement
```

Signal → target node id. **Authored order is preserved**, so write the happy path
first — it is the order `hex graph` renders. Signal names must match
`[a-z][a-z0-9_]*`.

## `accept:`

```yaml
accept:
  require: [review.approved, test.passed]
  on_unmet: implement
```

`require` lists `node.signal` evidence that must hold for a success terminal to
actually succeed — a model's approval and a deterministic gate carry equal weight
in the syntax, and the gate always outranks the claim in spirit.

`on_unmet` reroutes instead of dead-ending. **It has two open unbounded-loop
bugs** — see SKILL.md.

## Prompt interpolation

| Token | Expands to |
|---|---|
| `{{prompt}}` | The operator's `-p`/`-f` text. |
| `{{<node>.result}}` | That node's last final message (or a human's answer). |

Interpolated at attempt start, not compile time. Agent output is fenced as
untrusted data; a human answer is fenced as operator input.

## Durations

humantime: `500ms`, `30s`, `20m`, `2h`, `1h30m`. **`m` is minutes, `M` is
months.**

## Validation error codes

`hex validate <graph>` reports these. Every one is refused before a run starts.

| Code | Means | Fix |
|---|---|---|
| `E-empty` | No nodes. | Add some. |
| `E-entry` | `entry` names no node. | Fix the id. |
| `E-edge-from` / `E-edge-to` | An edge names a node that does not exist. | Fix the id. |
| `E-unreachable` | A node the entry cannot reach. | Connect or delete it. |
| `E-no-terminal` | No terminal reachable. | Add one. |
| `E-no-happy-path` | No `terminal: succeeded` reachable — the graph can only fail. | Add a success terminal. |
| `E-terminal-edge` | A terminal has outgoing edges. | Remove them. |
| `E-unbounded-cycle` | A loop nothing stops. | Add `budget.attempts`, `cycle_visits`, or a node's `visits`. |
| `E-proposal-no-edge` | `may_propose` lists a signal with no edge. | Add the edge or drop the signal. |
| `E-edge-not-proposable` | An edge on a signal the agent may not emit. | Add it to `may_propose`. |
| `E-bad-signal-name` | Signal is not `[a-z][a-z0-9_]*`. | Rename. |
| `E-done-reserved` | `done` used where it is reserved for implicit completion. | Pick another name. |
| `E-gate-signal` | A command node's edge is not `passed`/`failed`. | Use those. |
| `E-human-no-edge` / `E-human-multi-edge` | A human node needs exactly one outgoing edge. | Fix the count. |
| `E-accept-node` | `accept.require` names a node that does not exist. | Fix the id. |
| `E-accept-unsatisfiable` | Required evidence no node can ever produce. | Fix the signal. |
| `E-accept-unmet-node` | `on_unmet` names a node that does not exist. | Fix the id. |
| `E-accept-unmet-terminal` | `on_unmet` points at a terminal. | Point at a node that can produce evidence. |
| `E-result-ref` | `{{x.result}}` references an unknown node. | Fix the id. |
| `E-budget-zero` | `output_tokens: 0` — spent before the first attempt. | Omit it, or give a real ceiling. |
| `E-journal-lifecycle` | A journal event could not have happened where it appears. | Not a graph error — the run's journal is corrupt or forged. |
