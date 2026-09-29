# Worktree isolation (thin slice)

- **Status:** designed + implemented 2026-07-21 (thin slice; `hex-runtime/src/worktree.rs`).
- **Scope:** Phase-2 run isolation, the first slice. Relates to the AGENTS.md worktree gotcha,
  README "Workspace isolation", TODO.md Phase-2 isolation items.
- **One line:** opt into a per-run git worktree (from a **pool** of reusable slots
  keyed by an advisory lock) so agents work on a throwaway branch without touching
  the main working copy; no auto-merge, journal stays authoritative and outside the
  worktree.

## Context (what exists today)

- Isolation is a documented decision with **zero code**: no `isolation` field in
  config or the graph IR, no git shell-out anywhere.
- `workdir` (where the agent runs) == project root; `run_dir` == `<root>/.hex/runs/
  <run-id>`; the control-channel files (`emitted`, `result.txt`) and attempt logs
  live under `run_dir/attempts/<attempt-id>/` — i.e. *inside* the workspace today.
- Run ids are already path-safe: `yyyy-MM-dd-<workflow>-<short-uuid>` (or
  `-<name>`). `.gitignore` ignores `.hex/runs/` only.
- hex already has an `fs4` advisory-lock primitive: `RunLock::acquire(&run_dir)`
  (OS-released on crash). We reuse this pattern for slot leasing.
- Control-channel constraint: an enforced read-only / path-scoped sandbox blocks the
  agent from writing `HEX_EMIT_FILE`/`HEX_RESULT_FILE`. codex runs under
  `--sandbox workspace-write` (writes confined to cwd + tmp); claude/opencode have no
  path sandbox.

## Goals

- `hex run … --worktree` executes the whole run in an isolated git worktree on a
  fresh branch `hex/<run-id>`, leaving that branch for **manual** integration
  (no auto-merge).
- Reuse worktree directories across runs (a **pool**) so built, gitignored deps
  (`node_modules`, `target/`, `.venv`) stay warm — killing the cold-start cost.
- Parallel runs each get a distinct slot; the pool grows on demand.
- The agent's diff and the authoritative journal never collide: journal stays in the
  main checkout, the worktree holds only the agent's work.

## Non-goals (deferred follow-ups)

`hex worktree` cleanup/prune/list; integration verbs (diff/merge/apply); `git
worktree lock`; a concurrency cap; dep/build-cache ergonomics (`CARGO_TARGET_DIR`,
pnpm store, `.worktreeinclude`); per-node worktrees; OS/container sandbox as a second
isolation axis; a graph-YAML `isolation:` default field.

## CLI surface

Run-config flags (off the prompt channel):

- `--worktree [<base-branch>]` — enable worktree isolation. Base = `<base-branch>`
  if given, else current `HEAD`. hex cuts a new branch `hex/<run-id>` **from** that
  base.
- `--no-worktree` — force `shared` (overrides any future configured default). *(Deleted 2026-09-29: it did nothing, since shared is the default.)*
- Neither flag → `shared` (default; today's behavior, agent runs in project root).
- `--worktree` and `--no-worktree` are mutually exclusive (clap `conflicts_with`).
- `--worktree-init "<argv…>"` — optional **warmup command** run inside a freshly
  created/reclaimed slot before the first agent attempt (see below). Repeatable /
  space-split into argv (no shell). Only meaningful with `--worktree`.

`--worktree` takes an optional value (clap `num_args(0..=1)`, modeled as
`Option<Option<String>>`): present-without-value = HEAD base, present-with-value =
named base branch.

## Design

### One worktree per run (not per node)

The whole run — every node/attempt — shares one leased slot and one branch. Nodes do
not get their own worktrees (that is a later parallelism-phase item).

### Slot pool + leasing

Pool lives in-repo at `.hex/worktrees/<slot-N>/` (numbered, durable across runs).
`.hex/worktrees/` is gitignored (hex appends the entry on first worktree use if
absent; `.hex/runs/` is already ignored).

On a `--worktree` run start, **lease** a slot:

1. Enumerate existing slot dirs; for each, try to claim its `fs4` advisory lock
   (same primitive as `RunLock`). A slot whose lock is held by a live run is skipped
   (busy / parallel run).
2. First slot we can lock:
   - **Clean** working tree (`git status --porcelain` empty; gitignored deps do not
     count) → reuse directly: `git checkout -b hex/<run-id> <base>`. Tracked files
     reset to `<base>`; warm deps untouched.
   - **Dirty** (a *finished* prior run left uncommitted/untracked changes) →
     **reclaim, loudly** (see below), then `git checkout -b hex/<run-id> <base>`.
3. No lockable slot (all busy or none exist) → create a new slot `N` = next free
   index: `git worktree add .hex/worktrees/<N> -b hex/<run-id> <base>` (cold deps),
   then claim its lock.

Hold the slot lock for the run's duration; release on finish/crash (RAII, OS-backed).

### Reclaiming a dirty slot (reclaim + log)

Only slots hex can lock (finished runs) are ever reclaim candidates — a live run's
slot is never touched. Before wiping:

1. Capture `git status --porcelain` + `git diff --stat` of the slot.
2. Emit a clear **stderr** warning naming the prior run and the discarded diffstat,
   and record a `Note` event in the **new** run's journal with the same detail (the
   authoritative record shows exactly what was discarded and from which prior run).
3. `git reset --hard` + `git clean -fd` — reverts modified tracked files and removes
   untracked non-ignored files, but **keeps** gitignored deps. Only the prior run's
   *uncommitted* tracked+untracked source is lost; its **committed** branch history
   is untouched.

### Warmup (`init`) command

An optional operator-supplied command that primes a slot's environment (`npm ci`,
`cargo fetch`, `uv sync`, …). It runs **after the tree is placed at base** — i.e.
after a fresh `git worktree add` (cold deps) or after a reclaim (base changed, deps
may be stale) — and **before the first agent attempt**. It is **skipped on a clean
warm reuse** (deps already present, base unchanged from the agent's point of view;
the agent handles any drift).

- Source: `--worktree-init` for now; a project-config default can follow.
- Executed as **argv** (no shell), in the slot as cwd, via the same logged-subprocess
  machinery as a `command` node; stdout/stderr captured under the run's `.hex`.
- Failure **fails the run closed** with a clear error (the environment isn't ready).
- If unset, hex does nothing — the agent (or a `command` node in the graph) works out
  its own setup, exactly as today.

### State split & the control channel

`workdir` = the leased slot. `run_dir` (journal `events.jsonl`, graph snapshot, and
`attempts/<id>/{emitted,result.txt,stdout.log,stderr.log}`) **stays in the main
checkout** at `<root>/.hex/runs/<run-id>`. Effects:

- The worktree's `git status`/diff stays pristine (no `.hex` scratch inside it).
- The authoritative journal survives a discarded/reclaimed worktree.
- Control files are addressed by **absolute** paths (`HEX_EMIT_FILE` etc.), so
  claude/opencode (no path sandbox) write them fine from a worktree cwd.
- **codex** runs `--sandbox workspace-write`, which confines writes to cwd (= the
  slot) + tmp. The control files are under the main `.hex/runs/…`, *outside* the
  slot, so codex would be blocked. Fix: when isolation is active, the codex worker
  adds the run's `attempt_dir` (or `run_dir`) as an extra **writable root**
  (`-c 'sandbox_workspace_write.writable_roots=[…]'` or the equivalent additional-
  writable-dir flag). **Verify the exact codex flag at implementation** (see Risks).

### Agent commit banner

hex makes **no git commits itself** (the reclaim step only *discards*). Instead, when
isolation is active, the driver appends a fixed banner to each **agent** node's
resolved prompt at attempt-start (after `{{prompt}}`/`{{node.result}}`
interpolation):

> You are working in an isolated git worktree on branch `hex/<run-id>` (base
> `<base-ref>`). Your changes will not be merged automatically. When your task is
> complete, commit your work in this worktree with a clear message, and summarize
> what you changed in your final message.

Committing both preserves the work on the branch (survives slot reclaim/deletion) and
leaves the slot clean for zero-cost reuse. `git commit` is allowed by every worker's
permission policy; only `git push`/publish are denied.

### Resume

At creation hex records, in the journal, the slot path + branch + base ref + resolved
base SHA (as `RunCreated` inputs entries and/or a dedicated additive record — exact
form decided in the plan; must be integrity-bound like the graph hash). On `resume`:

- Reattach to the recorded slot + branch.
- Checkout dir missing but branch alive → `git worktree prune` then recreate the
  worktree from the **branch tip** (recovers committed work; uncommitted is already
  gone).
- Branch also gone → fail closed (never silently recreate from a different base).
- The orphaned attempt is marked `interrupted` as today.

### Errors / preconditions

- `--worktree` requires the cwd to be inside a git repo (and, for a named base, that
  the ref resolves) → otherwise fail closed with a clear message before any run
  state is created.
- A dirty **main** working tree is fine — a worktree is a clean checkout of the base
  commit, independent of the main tree's dirtiness.

### Dependencies on existing machinery

- `fs4` advisory lock (already used by `RunLock`) → slot leasing.
- Run-id (`yyyy-MM-dd-…`) → branch name `hex/<run-id>`.
- `WorkRequest.workdir` already threads cwd to the subprocess; only its value changes
  (slot vs project root). `attempt_dir` (control files) is unchanged (main `.hex`).

## Deps (unmanaged)

The pool keeps gitignored deps warm across reuses. Reused deps can be **stale** if the
new base changed `package.json`/`Cargo.toml`; it is the agent's or a `command` node's
job to re-install. hex manages no build cache this iteration.

## Testing

- Slot leasing: fresh pool creates slot 0; second concurrent lease (lock held)
  creates slot 1; releasing a clean slot lets the next run reuse it (assert same dir,
  new branch).
- Reclaim: a slot left dirty by a prior run is wiped to base, deps (an ignored file)
  survive, tracked change is gone, and a `Note` records the discarded diffstat.
- State split: with `workdir` = slot, `emitted`/`result.txt` are written under the
  main `.hex/runs/…` and routing still works (mock worker).
- Warmup: `--worktree-init` runs in a new/reclaimed slot before the first attempt;
  a non-zero exit fails the run; it is skipped on a clean warm reuse.
- Resume: delete the checkout dir, keep the branch → resume recreates from branch
  tip; delete the branch too → resume fails closed.
- Errors: `--worktree` outside a git repo fails closed; `--worktree`+`--no-worktree`
  is a CLI conflict.
- Non-git / `shared` path unchanged (regression).

## Risks / open items

- **codex writable-root flag** — the control channel under codex depends on adding
  the run dir as a writable root. Confirm the exact codex CLI incantation early; if
  unavailable, fall back to placing the per-attempt control dir *inside* the slot
  (under a base-gitignored path such as `<slot>/.hex/…`) so any sandbox can write it,
  keeping only the journal outside.
- **Nested worktree under `.hex/`** — git supports a worktree inside the main tree as
  long as the path is gitignored; ensure the `.gitignore` entry lands before
  `git worktree add`.
- **Pool growth** — no auto-cleanup this iteration; parked branches and (rarely)
  reclaim-resistant states accumulate. Disk grows until manual `git worktree remove`
  / the deferred `hex worktree cleanup`.
