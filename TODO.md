# TODO

Forward list. Design decisions and invariants live in AGENTS.md (core rules);
research in `docs/design/gpt-research-{1,2}.md`.

## Landed

- **Phase 0–1 MVP**: pure kernel (`reduce` + `schedule`), append-only JSONL
  journal with torn-tail repair and snapshot hash checks, run lock, crash
  resume, claude + codex adapters, the `critique-loop` preset.
- **Operability**: project `checks:` (undeclared = compile error), `gate`
  merged into `command`, a per-attempt bound on every attempt, `hex doctor` +
  start-time preflight, disposition exit codes, `why` for every terminal.
- **Roles & gating**: `workers:` + `roles:` layered config deep-merged over
  `defaults.yaml`, multi-step `command` nodes (`ordered`/`parallel`),
  `accept.on_unmet` rerouting, `E-no-happy-path`.
- **Control**: file inbox (`pause`/`steer`/`respond`/`cancel`), `human` nodes,
  `hex runs`/`wait`, `cancel` on a live run, Ctrl-C lands the run in `paused`.
- **Payoff & cost**: end-of-run output (why, spend, failed-check tails, final
  message), salvaged partial output, process-group kill, usage journaled in
  micro-USD with honest lower bounds, `budget.output_tokens`, `context:
  continue`, `hex init`.
- **Live observability**: `hex status` in-flight view, two-stage steers,
  journal-driven `hex logs --follow`, `Runtime::read_streams` /
  `AttemptStreams`, live preview with a graph progress strip.
- **Simplification**: `VERDICT:` line replaces `hex emit`; `--detach`, `hex
  dash`, `hex watch`, graph exports, the MCP/dashboard stubs, the bench and
  four unused presets deleted; `hex stats`, `hex prune` and gate stall
  detection added.
- **Per-node bounding**: every non-terminal node is visit-bounded (default 5);
  run-wide retry budgets and cycle analysis deleted.
- **Ponytail passes**: `lifecycle::admissible` as the one guard table,
  acceptance inlined into `schedule`, `Usage::attempt_cost`, worker trait
  defaults + `&'static [Capability]`, `hex_runtime::init`, `StatusReport.why`,
  `worker:` as an alias of `role:`, `--no-worktree` deleted, preview reads
  streams through `AttemptStreams`, humantime durations in `hex graph`.

## Open

Review cost and quality:
- [ ] Feed the reviewer the diff, not the repo. A cross-model `checklist` run
      cost ≈ $40, mostly the reviewer re-reading the repo each round, and the
      holistic review looped on already-fixed points.
- [ ] A/B the reviewer/implementer pairing. Defaults: claude implements, codex
      reviews. One paper claims Claude reviewing Codex helps more than the
      reverse.
- [ ] A red gate at the branch base wedges a scoped run:
      scope gate checks to the branch diff, or refuse to start when the gate is
      already red at the base.
- [ ] tdd's red/green gates run the whole suite, so an unrelated failure reads
      as "red". Needs per-test targeting.
- [ ] Agent-result stall detection (gate-signature stall detection exists).

Skill:
- [ ] Embed `skill/hex/agents/openai.yaml` in `skill::FILES` once it lands.

Correctness:
- [ ] `gate_sigs` is in-memory: a pause/resume between two identical gate
      failures resets the stall count. Journal the signature if seen in practice.
- [ ] Pause + `--worktree`: a paused run's slot is unlocked, so another run can
      reclaim it (discarding uncommitted work) before `hex resume`.
- [ ] Forged-journal attempt-id reuse: `attempt_report_ok` dedupes only the
      in-flight occurrence, so a reused `att_N` bills twice. Fix: a seen-id set
      enforced by `attempt_start_ok`.
- [ ] A run ending on a `human` node shows the last agent result, since
      `Payoff` reads `logs` (keyed by attempt). Expose the verified projection's
      `results` instead.
- [ ] `Inbox::queued()` claim window: a command claimed between the fold and
      the inbox scan shows as neither queued nor pending.
- [ ] `VISITS` in the spend table is a visit count; a crash between
      `AttemptReported` and the terminal leaves two reports on one visit.
- [ ] fs4/flock flake: not root-caused; worktree slot
      leasing still hits it, and the concurrent leasing path is untested.

Tests:
- [ ] `budget.output_tokens` end-to-end test: no fake worker reports usage.
      A mock that emits a scripted `AttemptReported` closes it.
- [ ] Live smoke test of `effort` and the per-agent flags (needs agent auth).

Workers and permissions:
- [ ] Enforced read-only: needs a result path outside the sandboxed workspace.
- [ ] Configurable claude permission policy (operator override + an explicit
      unsafe `bypass` for use inside real isolation).
- [ ] opencode: a generated `opencode.json` `permission` block instead of
      `--auto`; parse its stream for usage and session id.
- [ ] codex reports no model name: two codex roles with no `model` merge into
      one `by_model` row.

Efficiency:
- [ ] `hex wait` re-verifies and re-folds the whole run every 500ms. Cache the
      verified compiled graph; add a liveness-only fast path.
- [ ] `hex runs` re-folds every finished run. A summary written at
      `RunFinished` would bound it.
- [ ] `Runtime::logs` loads every attempt's full output with no cap.

Journal and surface:
- [ ] Graph-surface versioning before 1.0: bump `version` on surface changes
      and keep a replay compiler per version.
- [ ] Canonical compiled snapshot (hash covers interpolated inputs + resolved
      defaults); decide whether `graph.sha256` (duplicates
      `RunCreated.graph_hash`) goes.
- [ ] Typed worktree record: `worktree.*` rides as strings in
      `RunCreated.inputs` (`WT_*` in `hex-runtime/src/lib.rs`).
- [ ] Roles and workers share one namespace. A
      `WorkerRef::Role | Direct` in the IR would remove the shadowing trap.
- [ ] `hex config show` with per-key provenance.
- [ ] `hex logs --json --follow` (needs a per-line event shape).
- [ ] Worktree follow-ups: integration verbs (`diff`/`merge`/`apply`), a
      concurrency cap, a graph-YAML `isolation:` field, a deadline on
      `--worktree-init`.
- [ ] `hex doctor`: CLI version probing; a claude model check if claude ever
      ships a free model catalog. `hex init`: write an example graph.

## On demand

Build only when a real need shows up.

- Phase 6 concurrency: `tokio`, `map`/`parallel`/`join`, subgraphs and
  watchdogs as graph constructs, per-node worktrees + an integration queue.
- Typed node inputs/outputs (`{{node.output}}`).
- `hex graph new` (an agent drafts a graph).
- `hex add` / `hex connect` builder commands.
- Actor authority scoping (observer/contributor/operator/approver).
- `triage` diagnostics + exportable run bundle.
- Richer policy layer (permissions, notifications, cost).
- Content-addressed artifact store; changed-file/diff artifacts.
- Approve/reject/edit `human` approval node.
- `context: compact`.
- `interactive: true` sessions and an interactive run UX.
- `templates:` / `extends:`.
- Dynamic shell completions.
- Structured tool-call feed in the preview.
- `--quiet` / `--verbose`; diagnostics with source spans.
- Published JSON Schema for the graph surface.
- `state.json` snapshot projection.
- Mock-worker contract suite; full crash/replay suite.
- Container/sandbox adapters, PTY workers, tracker/PR adapters, web viewer,
  distributed workers, SQLite projection, preset registry.
