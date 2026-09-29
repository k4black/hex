# hex simplification & agent-native redesign

- **Status:** design; decisions locked 2026-09-13 in a grilling session, amended after a
  glm-5.3 review. **This file is the implementation spec**; implemented 2026-09-13.
- **Scope:** every crate. Keeps AGENTS.md's core rules; changes rule 3's *transport*
  (a model still proposes an event from an allow-list — it now does so in its final
  message, not via a `hex emit` subprocess). Deletes one protocol, one process mode,
  four presets, two TUIs, and a duplicated state machine.
- **One line:** keep the operability surface that makes a run trustworthy; delete the
  accidental complexity around it; make the parent agent's contract one blocking call
  and one verdict.

## Why (the evidence this design answers to)

### LOC distribution (production / test)

| crate | prod | test | total |
|---|---:|---:|---:|
| hex-runtime | 4 967 | 3 248 | 8 215 |
| hex-cli | 4 565 | 1 808 | 6 373 |
| hex-kernel | 2 588 | 2 159 | 4 747 |
| hex-worker | 1 634 | 690 | 2 324 |
| hex-proto | 424 | 133 | 557 |
| hex-mcp / hex-dashboard (stubs) | 24 | 0 | 24 |
| **total** | **14 202** | **8 038** | **22 240** |

Hand-rolled code an existing crate could replace is **~250 lines total**. The rest
already uses clap, ratatui, serde, `yaml_serde`, `fs4`, `humantime`. The line count is
feature surface, not missing libraries.

### What the two real runs actually used

Two runs have ever executed here, both `checklist`, both `--detach` + worktree, both
ending without success (`cancelled`, `budget_exhausted`).

| exercised | never exercised |
|---|---|
| `checklist`; `implement`/`review`/`verify`/`final_review` | 8 of 10 presets |
| `item_done`, `approved`, `changes_requested`, `all_done`, `passed`, `failed` | `human`/`respond`, `dash`, mermaid/DOT, `graph --format json` |
| `steer` (both), `pause`+`cancel` (once), worktree isolation (both) | `hex wait` as a documented verb |

Feedback log: the reviewer re-reads the whole repo every round (one attempt hit
141 985 input tokens); review loops re-flag already-fixed points; no way to prune; a
pre-existing red check at the branch base wedged a scoped run.
**Both real runs wedged on exactly what stall detection bounds.**

### Library candidates, checked for maintenance

| crate | latest | last release | downloads | verdict |
|---|---|---|---|---|
| `clap` | 4.6.6 | 2026-08-06 | 1.13 B | already used |
| `tempfile` | 3.27.0 | 2026-03-11 | 804 M | take (already declared) |
| `anstream` | 1.0.0 | 2026-02-11 | 677 M | take only for `strip_str` |
| `humantime` | 2.4.0 | 2026-07-02 | 436 M | already used |
| `which` | 8.0.6 | 2026-08-26 | 416 M | take (replaces 65 lines) |
| `jiff` | 0.2.37 | 2026-09-12 | 187 M | take (replaces `civil_from_days`) |
| `comfy-table` | 8.0.0 | 2026-08-05 | 98 M | optional, only if `ui::Table` is replaced |
| `slug` | 0.1.6 | **2024-08-15** | 40 M | skip — stale, saves 15 lines |

## Locked decisions

### 1. Process model — one blocking execution

- **`hex run` blocks.** No `--detach`, no `--reserved-run-id`, no reserved-run
  machinery. Backgrounding is the caller's shell (`&`) or tmux.
- **`hex wait <run>` stays**, meaning *block on a run started elsewhere until the whole
  graph finishes and return the verdict*. `status`, `logs --node`, `steer`, `pause`,
  `cancel` observe and control a live run from another shell.
- Two output modes only: **interactive** (TTY) and **non-interactive** (pipe / `--json`).
- **Delete:** `--detach`, `--reserved-run-id`, `cmd_detach`, `spawn_detached`,
  `Runtime::start_reserved`, the reserved threading in `start`, the detached
  stdout/stderr files, and the followers' 15 s reserved-journal wait. `hex dash` and
  `hex watch` are deleted.
- **Rename `Status::Created` away** (no reserved run exists), but keep the fold that
  repairs a journal that stopped between `RunCreated` and `RunStarted` — `resume`
  depends on it (`Runtime::resume`).
- **SIGHUP caveat (document, do not fix in code):** a run backgrounded with `&` is no
  longer `process_group(0)`-detached, so the driving shell exiting can HUP it. tmux is
  the documented answer for humans; the skill and README must say so.

### 2. State model — one runtime view, kernel untouched (A1)

**Do NOT replace `hex_kernel::Status`.** It is the kernel's journal projection and is
load-bearing: `schedule` and every lifecycle guard match `Created`/`Running`/`Paused`
(`lifecycle.rs`). `live`/`interrupted`/`error` mix kernel state with process and IO
facts the pure kernel may not compute (core rule 1).

- Collapse only the runtime-facing pair into one enum:
  **`live · interrupted · finished(disposition) · error`**
  computed from `Status` + the run lock + the heartbeat.
- Fold `RunSummary.error` and `RunSummary.status: Option<Status>` into it.
- `interrupted` covers Ctrl-C, `hex pause`, and a crash; all resumable.
- **Keep `Heartbeat`.** It is the only signal that separates a live attempt from a
  crashed one — a run mid-attempt writes nothing to the journal (`AttemptStarted` is
  the last event). `Hung` becomes a *diagnostic on `live`* (lock held, heartbeat stale),
  not a fifth state.
- **Fix the fs4 lock flake where it actually bites:** in `cancel`, read the
  journal **before** probing the lock. A finished run then returns `Recorded` and never
  reaches the flaky probe. The `cancel_of_an_idle_run_is_recorded_directly` reproducer
  disappears because the path is not taken.

### 3. Verdict — the final message replaces `hex emit`

An agent node's only output channel is its terminating message.

- **Marker:** an explicit last line `VERDICT: <signal>` (e.g. `VERDICT: approved`).
- **The instruction is generated, never authored.** At attempt start the runtime appends
  it to the interpolated node prompt, from that node's allowed signals. No graph author
  writes it; no preset can forget it; it cannot drift from the edges. Single-outcome
  nodes get nothing appended.
- **Parsed in the runtime**, from the captured result — one place, every worker kind.
  `WorkOutcome` stops carrying a signal; `read_signal` and the per-adapter emit plumbing
  are deleted.
- **(A2) Parse the verdict from the RAW capture, before `cap_result`.** `cap_result`
  keeps the head and appends `…` (`MAX_RESULT_BYTES = 16 KiB`), so a >16 KiB review
  message would lose its last line and turn a compliant verdict into `unknown`. Test a
  >16 KiB message with a trailing marker.
- **(A3) Add a plain-stdout result mode** (`result: text` — the tail of `stdout.log`,
  which is already captured). Without it a `kind: command` worker printing
  `VERDICT: approved` has no capture mode at all.
- **A node with exactly one outcome** completes implicitly with `done` — unchanged.
- **`unknown`** (no marker, or a marker that is not a declared signal):
  - fail closed — the attempt fails with
    `why: "no verdict in the final message (expected: approved | changes_requested)"`;
  - a graph **may** declare `on: { unknown: <node> }`.
- **(A4) `unknown` is a second RESERVED signal**, exactly like `done`: exempt from
  `E-edge-not-proposable`, and rejected inside `may_propose` with a `done`-style error
  (it is never an expected verdict, so the generated instruction must not list it).
- **(A4) Forbid a mix of `may_propose` and a `done` edge** at compile time (no preset
  uses it). Today such a node falls back to `done`; under the new rule a clean
  no-marker exit would flip to fail, so the ambiguity must not be representable.
- A **multi-outcome node of `kind: command`** must declare a result-capturing `result:`
  mode: `E-multi-outcome-no-result`. This check lives in the runtime's `check_workers`
  (the kernel may not see workers) behind a `Worker::captures_result()` method
  (~5 lines, default false).
- `may_propose` stays: it is the allowed verdict set. `E-proposal-no-edge` /
  `E-edge-not-proposable` stay.
- **Delete:** the `hex emit` verb, `HEX_EMIT_FILE`, `HEX_MAY_PROPOSE`, the emitted-file
  read/validation, and — as a consequence — `hex doctor`'s `self` row, the preflight
  `self` exception, and the skill's "put `hex` on `PATH`" trap. Retires the old `hex emit` PATH gotcha
  (`hex emit` returned 127 in **3 of 3** real runs).

Core rule 3 survives: a model still proposes an event only from its allow-list and the
kernel still validates the transition. Only the transport changed.

### 4. Presets

Keep six: `critique-loop`, `checklist`, `implement-until-green`, `tdd`, `review`,
`autoresearch`. Delete four: `code`, `plan-build-review`, `pr`, `research`.

- `checklist`'s `review.visits` drops from 20 to 6; its `budget.attempts` stays.
- Preset prompts lose their `hex emit …` sentences.
- **(A8) The scoped-diff instruction is runtime-injected**, not authored: the run's
  base ref exists only under worktree isolation and is known at attempt start. Reuse the
  `worktree_banner` path in the driver, with a shared-run fallback ("diff against the
  working tree"). Reviewer prompts then say "review the change, not the repository".
- **No `extends:`.** Presets stay flat, standard YAML.

### 5. Stats — cross-repo facts only (A5)

`~/.hex/stats.jsonl`, append-only, **no counters, no database**: `hex stats` folds it on
read.

- **Writers:** one `run` line at `RunFinished` (runtime); one `cli` line per verb at
  dispatch (CLI).
- **Record only what the journal cannot hold** (the journal is per-project and `prune`
  deletes it, so it cannot answer "which graphs and verbs across my repos"):
  repo (full canonical path), graph, origin (`built-in:<name>` vs path), isolation
  (shared / worktree + branch), caller source, verb, disposition, attempts, per-node
  visit counts.
- **Do NOT duplicate** per-model usage, spend, or per-attempt detail — those stay
  folds over `.hex/runs`.
- **`source` ∈ `interactive` | `non-interactive` | `subgraph`** (`subgraph` = called
  from inside an attempt, i.e. `HEX_RUN_ID` is set). No env fingerprinting.
- Local only, **on by default, no setting**. A write failure warns on stderr and never
  fails a command.
- `hex stats` renders a `ui::Table`; `--json` emits the raw aggregate.
- **(A10) `hex_version` fix is one line in `Cargo.toml`** (`version = "0.0.0"` →
  a real version). `feedback.rs` currently stamps `"0.0.0"` on every line, forever.

### 6. Operator surface cleanup

- `hex prune [--older-than <dur>] [--all]` — removes finished/interrupted run dirs and
  releases their worktree slots; never touches a `live` run.
- `hex graph` prints **text**, plus `--format source`. Delete mermaid + DOT export and
  `--format json`.
- `hex feedback` and `hex doctor` stay, simpler: one shared append helper for feedback
  and stats; `doctor` reuses `ui::Table`.
- Delete the `hex-mcp` and `hex-dashboard` stub crates.

### 7. Bug-proneness fixes

- The lock-flake fix in `cancel` (section 2).
- **(A9) Build stall detection.** When the same gate fails twice with the same output
  signature, stop routing it back and end with
  `why: "gate <x> failed identically twice"`. ~50 lines in the kernel/runtime, and it
  bounds the `fmt` wedge and the review loop — the wedge both real runs hit.
- Journal a `Note` when a worker exits cleanly with non-empty output but the usage
  parser extracted nothing — today `budget.output_tokens` fails open and never bites.
- **(A6) Fix the false sentence in `topology.rs`** that claims the validator consumes
  `Topology`; it does not. Do NOT unify: the two walks answer different questions and
  `check_cycles` needs bound-awareness `Topology` cannot express; they already share
  `Graph::implicit_reroutes()`.
- **(A7) Defer the typed worktree lease.** A typed field changes `EventBody` (which
  derives `Eq`), i.e. a schema bump plus a dual-decode path; corruption is already
  detected fail-closed.

### 8. Deliberately NOT done

- **No base-state gate.** It breaks `implement-until-green` and `tdd`, whose job is to
  start red. The `fmt` wedge was a mis-scoped check (`--all` over a one-crate scope)
  plus an over-loose `visits`; both are existing knobs.
- **No `extends:`.**
- **No `hex wait` deletion** and no full-verdict `status`: observing a live run and
  blocking for a whole graph are separate capabilities.

## Change set

| area | change | est. lines |
|---|---|---:|
| emit transport | `hex emit`, `HEX_EMIT_FILE`, `HEX_MAY_PROPOSE`, `read_signal`, per-adapter plumbing, doctor `self` row | −300 |
| verdict | generated instruction + parse from raw capture + `result: text` + reserved `unknown` | +100 |
| detach | `--detach`, reserved machinery, reserved-journal waits | −250 |
| state | runtime enum collapse (kernel `Status` kept) | −60 |
| TUIs/exports | `dash`, `watch`, mermaid/DOT, `graph --format json` | −620 |
| presets | 4 deleted | −350 |
| stubs | `hex-mcp`, `hex-dashboard` | −24 |
| dedup | JSONL reader ×5, policy parts, `tail_lines` ×3, runtime dir-scan, resume/cancel block, `clap::ColorChoice` | −250 |
| lib swaps | `tempfile`, `which`, `jiff`, `anstream::strip_str` | −250 |
| stats | cross-repo log + `hex stats` | +150 |
| prune | `hex prune` | +65 |
| stall detection | gate-signature bound | +50 |
| usage Note | hardening | +15 |

_Ponytail ceilings, marked `ponytail:` in code:_ growing `stats.jsonl` folded on read;
`hex prune` deletes by mtime + run state; tail/stream helpers stay full-file reads until
a journal makes polling visible.

## Migration order (one writer at a time; the repo is the shared worktree)

1. **Verdict** — deletes a protocol; touches every `HEX_EMIT_FILE` site (39 across 7
   files) and the presets. Land alone; `cargo test --workspace` green.
2. **Process + state** — detach deletion, runtime enum collapse, `cancel` ordering.
3. **Surface** — delete `dash`/`watch`/export/stubs; add `prune`; add `stats`.
4. **Hardening** — stall detection, usage `Note`, `topology.rs` doc fix.
5. **Docs** — README, AGENTS.md (the verdict-line, fs4-flake, step-exit, `hex emit` and graph-export gotchas + terminology), the hex
   skill, `defaults.yaml`, preset comments.

## Risks

- **Verdict reliability** on long messages — closed by A2; without it the first real
  long review fails.
- **`&`-backgrounded runs and SIGHUP** — documented, not fixed.
- **Test churn** (39 emit sites + enum renames) is the largest mechanical cost and the
  likeliest place for a silent behaviour change.
- **Preset deletion** removes workflows some users want; `git graph --format source`
  and git history preserve them.

## Open items

- Whether `hex runs` shows a live run's in-flight node or stays a one-line row.
- `hex stats` columns and whether `--since` is worth it before anyone asks.
- Whether `unknown` deserves a `Note` as well as the `why` line.
