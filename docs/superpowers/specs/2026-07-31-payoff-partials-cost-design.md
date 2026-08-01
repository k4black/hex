# Design — payoff output, partial capture, usage accounting, `init`, session resume

Date: 2026-07-31. Status: approved, implementing.

Six changes, driven by an audit whose central finding was that hex's own
`self-review` runs produced valuable reviews and reported only
`disposition: failed` — the evidence existed and no CLI surface reached it.

| # | Change | Why now |
|---|---|---|
| 1 | Print the payoff | A run's product was unreachable: `logs` never read command-step dirs, `--full` wrote to stderr, `print_outcome` dropped the kernel's terminal reason |
| 2 | Never discard partial work | A timed-out attempt threw away a *finished* review (run `62ce0118`: 1.86 MB on disk, `NodeResult` absent) |
| 3 | Usage accounting | `Capability::CostReporting` was declared with no event able to carry it; a 12-attempt loop's spend was unknowable |
| 4 | `hex init` | First run in a new repo needed a hand-written `.hex/config.yaml` |
| 5 | `context: continue` | Every attempt re-read the diff cold; the 15m timeout was a symptom |
| 6 | `doctor` probes `hex` | `hex emit` returned 127 in 3 of 3 real agent runs, unseen by preflight |

Out of scope, deliberately: injecting `hex` into the agent's `PATH` (the
operator places it on `PATH` themselves; packaging comes later), a price table
for agents that report no money, and a `hex digest` verb (the breakdown goes
into existing verbs).

## Verified CLI surfaces

Captured from live runs on 2026-07-31, not inferred. Fixtures live in
`crates/hex-worker/tests/fixtures/`.

`codex exec --json` emits JSONL:

```json
{"type":"thread.started","thread_id":"019fb9a2-d8cf-7a12-bd5e-92d060051f64"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hi"}}
{"type":"turn.completed","usage":{"input_tokens":17259,"cached_input_tokens":11008,
  "cache_write_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":0}}
```

One `turn.completed` per exec — verified against a run making two shell calls.
**No model name and no cost.** `--json` composes with `-o/--output-last-message`,
so result capture is unaffected.

`claude -p --output-format json` emits one object carrying `session_id`,
`total_cost_usd`, `duration_ms`, `num_turns`, `usage`, and `modelUsage` keyed by
model name:

```json
{"modelUsage":{"claude-opus-5":{"inputTokens":2,"outputTokens":4,
  "cacheReadInputTokens":0,"cacheCreationInputTokens":17769,"costUSD":0.1778}},
 "session_id":"cf95788d-…","total_cost_usd":0.1778,"duration_ms":2611}
```

Resume: `codex exec resume <SESSION_ID> [PROMPT]` and `claude -p --resume <id>`.
`codex exec resume` accepts `--json -o -m --skip-git-repo-check -c -i` but
**not `--sandbox`, `--add-dir` or `--cd`** — see the constraint in §5.

## 1. Protocol — one new event

```rust
/// What the worker reported about a finished attempt: the agent's own
/// accounting, never ours.
AttemptReported {
    session_id: Option<String>,   // resume handle for `context: continue`
    models: Vec<ModelUsage>,      // a list: claude bills several models per attempt
    cost_usd: Option<f64>,        // attempt total, when the agent reports money
    duration_ms: Option<u64>,
}

struct ModelUsage {
    model: String,                // reported, else the role's model, else worker kind
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,        // codex reports it; claude does not (0)
    cost_usd: Option<f64>,
}
```

`models` is a list rather than one model per event because claude's `modelUsage`
genuinely reports several per attempt. `cost_usd` stays `Option` so a future
`prices:` config can populate it for token-only agents without a schema change.

### Lifecycle

One new predicate in `hex-kernel/src/lifecycle.rs`:

```rust
attempt_report_ok = correlated(state, event) && current_kind == Agent
```

The driver records `AttemptReported` → `NodeResult` → routing signal /
`AttemptFailed`, in that order. Anything after the signal fails `correlated`,
because `reduce` clears `current_attempt` when the signal routes.

### `RunFinished` after `AttemptFailed`

`AttemptFailed` is deliberately self-terminating (`hex-proto`: "a failure is one
atomic durable fact"), so the disposition was always in the journal — but a
consumer tailing for `run_finished` never saw a terminator. Every run now ends
with one.

The blocker is not `reduce`; it is a *shape* rule in `check_journal` that
`lifecycle.rs`'s module doc records: only inert `Note`s may trail a terminal.
Amendment, narrowly: **a single `RunFinished` may trail an `AttemptFailed` iff
its disposition is equal.** A mismatched pair is an `E-journal-lifecycle` error.
Absence stays legal, which keeps the four pre-change journals in `.hex/runs/`
readable.

## 2. Usage is a projection, not a stored total

`reduce` folds `AttemptReported` into `RunState.usage`:

```rust
Usage {
    by_node:  BTreeMap<String, Totals>,
    by_model: BTreeMap<String, Totals>,
    total:    Totals,
}
```

Pure arithmetic over events, so the kernel stays IO-free and clock-free, core
rule 2 holds (computed, never stored), and every client — the CLI now,
`hex-dashboard` later — gets the same numbers with no second implementation.

## 3. Worker surface

`WorkOutcome` gains `report: Option<AttemptReport>`; `WorkRequest` gains
`resume_session: Option<String>`. Each typed adapter parses its own structured
output, which is what typed adapters are for:

- **codex**: `--json` added unconditionally. `thread.started.thread_id` → session
  id; sum `turn.completed.usage` across turns. Model falls back to the role's
  configured model, else `"codex"`.
- **claude**: reuse the object already parsed for result capture. `modelUsage` →
  one `ModelUsage` per model; `total_cost_usd`, `duration_ms`, `session_id`.
- **opencode / command / mock**: `report: None`.

codex's stdout.log becomes JSONL instead of prose. Accepted: it is the same
change that makes both usage and resume possible, and the final message still
comes from `-o`.

### Partial capture

`run_agent` early-returns on timeout and on nonzero exit *before*
`capture_result`, so both paths discard the agent's work. Restructured so
capture always runs and both failure paths carry `result` and `report`. When
capture yields nothing on a failed attempt, the result is synthesized from a
bounded tail of `stderr.log` **and** `stdout.log` (codex writes everything to
stderr and 0 bytes to stdout), prefixed `[partial output — attempt timed out]`.

A partial is a real `NodeResult`, so it folds into `RunState.results` and a
downstream `{{node.result}}` can interpolate a log tail — hence the marker
prefix. The alternative, a separate event that `logs` shows and interpolation
ignores, hides evidence from the handoff that most needs it.

### Killing the tree

`wait_bounded` calls `child.kill()`, which orphans grandchildren at ppid 1
(verified: `sleep 25` survived its killed parent). The child is now spawned with
`process_group(0)` and the deadline path sends `killpg(SIGTERM)`, waits 2s, then
`SIGKILL`. Uses `nix`'s safe wrapper, since the workspace forbids `unsafe`.

## 4. Surfaces

**`hex logs`** — `AttemptLog` gains `steps: Vec<StepLog { label, exit_code,
stdout, stderr }>` read from `attempts/<id>/<n>-<label>/`. This is the only way
a failing check's output becomes reachable. `--full` writes both streams to
**stdout** (today `stderr` goes to `eprint!`, so `--full > out.txt` captures
nothing).

**End of `run`/`resume`** — the terminal node's result, each failed step's label
with a 40-line tail, the terminal `Note` (the kernel's *why*, currently
dropped), and the run's usage total.

**`hex status`** — per-node rows (attempts · wall · verdict · tokens · cost),
then by-model rows, then the total. Same structure in `--json`.

**`hex init`** — creates `.hex/` and `.hex/graphs/`, writes a commented
`.hex/config.yaml` whose `checks:` is empty with examples commented out (no
autodetection — what "green" means is the operator's call), and appends
`.hex/runs/` and `.hex/worktrees/` to `.gitignore` only when absent. Never
overwrites an existing config; idempotent, exit 0.

**`hex doctor`** — one row probing whether `hex` itself resolves on `PATH`, so
the failure that killed run `62ce0118` is caught by preflight instead of by a
15-minute timeout.

## 5. `context: continue`

The plumbing exists and is inert: `Context` in `graph.rs`, `NodeSpec::Agent
.context` unread, and the loader actively rejects `continue`. The loader stops
rejecting it; the driver fills `WorkRequest.resume_session` from the most recent
`AttemptReported.session_id` **for that node** in `RunState`. A first visit has
none and runs fresh; the handle survives a crash because it comes from the
journal.

This gives `Capability::SessionResume` its first consumer: a node declaring
`context: continue` on a worker that does not advertise resume is a validation
error. `CodexWorker`/`ClaudeWorker` declare it; the others do not.

**Constraint.** `codex exec resume` accepts no `--sandbox` or `--add-dir`, so a
resumed attempt carries them as `-c sandbox_mode="workspace-write"` and
`-c sandbox_workspace_write.writable_roots=[…]`. Those keys are verified against
the live CLI during implementation; if the writable-roots key cannot be
confirmed, `context: continue` together with `--worktree` is refused at compile
time rather than silently running unsandboxed or breaking the emit channel.

## 6. Testing

- Usage parsers against the **captured real fixtures**, not hand-written JSON —
  a parser written against invented shapes passes while disagreeing with the
  CLI, which is the mistake this design already avoided once.
- Kernel: `reduce` folding usage; the `RunFinished`-trailing-`AttemptFailed`
  guard accepting a matched pair, rejecting a mismatched one, tolerating
  absence.
- A `kind: command` fake that outlives its deadline and spawns a grandchild:
  assert the partial `NodeResult` exists and the grandchild is dead.
- CLI: step output through `logs`, `--full` on stdout, `status` totals, `init`
  idempotence.
- Regression: the four pre-change journals in `.hex/runs/` still read.

## As built — where it differed from this design

Four deviations, all found during implementation:

1. **Money is `cost_micro_usd: Option<u64>`, not `f64`.** `Event`/`EventBody`
   derive `Eq`, which a float breaks, and stripping `Eq` would ripple through the
   kernel and its tests. Integer micro-USD is also simply correct for an
   append-only journal: a float cost can come back a different number.
2. **codex has no `token_count` event.** The real shape is one
   `turn.completed.usage` per exec, and the JSONL carries **no model name and no
   cost** — only `thread.started.thread_id`. Confirmed against a tool-calling run
   too. The design's guessed event name would have produced a parser that passed
   its tests and reported nothing; the captured fixture is what caught it.
3. **`codex exec resume` needed no refusal.** The design planned to refuse
   `context: continue` together with `--worktree` if the writable-roots config key
   could not be verified. Both `sandbox_mode` and
   `sandbox_workspace_write.writable_roots` were confirmed against the CLI via
   `--strict-config`, which rejects an unknown field before contacting the model,
   so the combination is supported and the refusal was dropped.
4. **`StepLog.exit` is a recorded string, not a typed status.** Nothing recorded a
   per-step exit status at all, so the driver now writes one to an `exit` file
   beside each step's logs (`"0"`, `"101"`, `"signal"`). Typing it would mean a
   typed journal record for something only a human reads.

## Consequences for the audit's kill list

`context: continue` gives the capability manifest a real consumer, so
`Capability`/`CapabilityManifest` come **off** the deletion list. The `context`
field plumbing likewise stays. Everything else in that audit stands.
