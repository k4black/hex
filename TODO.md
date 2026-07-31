# TODO / Roadmap

Phased so a working claude+codex critique loop ships **first**, then
pause/resume/evidence harden, then concurrency and remote control. Architecture
locked in the 2026-07-19 design session (see README.md + AGENTS.md; research:
`docs/design/gpt-research-{1,2}.md`).

## Decisions — resolved 2026-07-19

- [x] Crate shape: `hex-proto` / `hex-kernel` (pure: IR + journal +
      projections + reduce/schedule/accept) / `hex-worker` (adapter) /
      `hex-runtime` (owns the drive loop) / thin clients (cli, mcp,
      dashboard) over `RuntimeClient`.
- [x] Definition syntax: **standard YAML**, single file, inline prompts,
      kind-as-key + co-located `on:` edges, `defaults:`/`templates:`/`extends:`,
      run-level `gates:` + `accept.require`. No custom mini-grammar.
- [x] Routing: deterministic ordered edges **+** agent proposals validated
      against a per-node `may_propose` allow-list (agents choose among given
      options only).
- [x] Process ownership: **A3 hybrid** — foreground `InProcess` now; per-run
      background controller + `Remote` client later behind the same trait.
- [x] Worker→runtime channel: one `Command` protocol, two transports —
      injected `hex emit` CLI (floor) + MCP tool hooks (`finish_session()`).
- [x] Domain: 5 entities (Graph, Run, Event, Budget, Artifact); 5 node kinds;
      interactivity = policy flag on `agent`; approval = later `human` node.
- [x] Verbs: `run`/`resume` only execution verbs; **no** `retry`/`replay`/
      `skip` — redo is a new run. Plus pause/cancel/status/watch/logs/emit/
      respond/validate/graph, `--json` everywhere.
- [x] Isolation: runtime-owned; default `shared`, opt-in per-run `worktree`
      (feature work isolated from the main working copy); **no auto-merge**.
- [x] MVP shape: slim skeleton **with** journal/replay/resume (hex's core
      identity), claude + codex adapters, and a built-in critique-loop preset
      as the flagship demo.
- [x] Presets: named, parametrized graphs resolved **project
      (`.hex/graphs/`) > user (`~/.config/hex/graphs/`) > built-in** —
      `hex run critique-loop -p "…"`. ("Preset", not "pipeline" —
      the term pipeline stays banned as a Graph synonym.)
- [x] Config: layered `~/.config/hex/config.yaml` + project `.hex/config.yaml`
      (project wins) — worker registry (argv template, headless flags, model,
      declared capabilities) + default budgets/context/isolation in MVP;
      richer policy (permissions, notifications, cost) later.
- [x] Authoring automation: a shipped **authoring skill** (SKILL.md teaching
      an agent to write + `hex validate` graph YAML) right after MVP;
      `hex graph new` architect command and MCP prompts deferred.
- [x] Operator input surface (resolved 2026-07-20): the CLI takes **one**
      operator value — the prompt — via `-p/--prompt <text>` or `-f/--file
      <path>`, filling `{{prompt}}` in node prompts. No `--input k=v`. Named
      *typed* inputs/outputs live on **nodes** (internal graph dataflow), not
      the operator surface. Rationale: every surveyed workflow tool parametrizes
      with named/typed values, but the operator only needs to say "what to do";
      richer parametrization is graph-internal. Keep orchestration/run-config
      (budget, worker, isolation) on CLI flags, never on the prompt channel.
- [x] Dependency choices (resolved 2026-07-20): adopt small, well-maintained
      crates over hand-rolling where they cut real code — `clap` (CLI parsing,
      derive), `thiserror` (runtime error type), `yaml_serde` (maintained
      serde_yaml fork), `fs4` (advisory locks, fs2 successor), `uuid` (run-id
      suffixes), `humantime` (budget durations); dev: `assert_cmd` + `tempfile`
      (binary end-to-end tests). Nothing lands in `hex-kernel` — it stays pure
      (proto-only). Deferred crates are annotated on their phase items below
      (`schemars`/`jsonschema` P2, `clap_complete` P3, `rmcp` P4/P7, `tokio` P6,
      `ratatui` P7).

## Phase 0 — restructure the scaffold ✅

- [x] `hex-kernel` as the single pure crate (IR + journal model + projections
      + `reduce`/`schedule`/`accept` signatures + `Effect` intents; no
      IO/clock deps).
- [x] `hex-worker`: `Worker` trait + capability manifest (`structured_events`,
      `live_steering`, `session_resume`, `graceful_cancel`, `cost_reporting`,
      …) + mock/subprocess adapter stubs.
- [x] `hex-runtime`: `Runtime` API + `RuntimeClient` trait with an `InProcess`
      impl; `hex-cli`/`hex-mcp`/`hex-dashboard` are thin clients on it (the
      runtime re-exports `Event`/`Command`/`PROTOCOL_VERSION` as the narrow
      client surface).
- [x] Placeholder types wiring the new inward graph; build/test/clippy/bench
      green; binary prints the 11-verb surface.

## Phase 1 — slim MVP: claude + codex critique loop ✅

Goal (met): `hex run critique-loop -p "…"` runs end-to-end — codex
implements, claude critiques, loop until approved + gate passes — and a killed
run resumes from its journal. Workspace tests + clippy are green; verified end-to-end
against the real `hex` binary, and hardened through a cross-model review round
(journal torn-tail repair, snapshot-integrity verification, run-lock, budget
fail-closed, typed timeout disposition, evidence/correlation guards).

- [x] `hex-proto`: versioned `Event` envelope (schema, seq, at_ms, run/node/
      attempt, actor, typed `EventBody`) + `Command` + `Disposition`.
- [x] Config loading: `~/.config/hex/config.yaml` + `.hex/config.yaml`
      (project wins), layered over a built-in codex/claude registry.
- [x] YAML loader → Graph IR (kind-as-key, co-located `on:` map, `defaults:`,
      block-scalar prompts, single operator `{{prompt}}`). Templates/extends and
      reusable `gates:`/`use:` remain Phase 2.
- [x] Validator (minimum honest set): references, reachability, terminal
      reachability, **bounded-cycle enforcement**, `may_propose` coverage.
- [x] Kernel: `reduce` (owns routing) / `schedule` (→ `StartAttempt`,
      `RunGate`, `RequestHuman`, `RecordTerminal`) / `accept` — minimal but real.
- [x] Runtime drive loop: replay → schedule → intent-before-effect with
      idempotency keys → execute → append → reduce; crash recovery (orphaned
      attempts marked `interrupted`; redo = new run).
- [x] Append-only JSONL journal + replay (torn-tail tolerant, monotonic seq);
      run dir `.hex/runs/<id>/{graph.yaml, graph.sha256, events.jsonl,
      attempts/<id>/}`. Atomic `state.json` snapshot trails to Phase 2.
- [x] Workers: generic `AgentWorker` (argv template, fresh session, `hex emit`
      channel) drives claude/codex + a scripted `MockWorker` for tests.
- [x] Worker channel, transport 1: injected env (`HEX_RUN_ID`/`HEX_NODE_ID`/
      `HEX_ATTEMPT_ID`/`HEX_EMIT_FILE`/`HEX_MAY_PROPOSE`) + `hex emit`;
      per-node `may_propose` allow-list enforced at emit and at ingest.
- [x] Node kinds working: `agent`, `gate`/`command` (inline argv), `terminal`
      (`human` stubs to Phase 2, fails closed).
- [x] Budgets: attempts + elapsed time + per-node cycle visits; fail-closed.
- [x] Preset resolution: `.hex/graphs/` > `~/.config/hex/graphs/` > built-in;
      ships built-in `critique-loop`; operator prompt via `-p`/`-f` fills `{{prompt}}`.
- [x] Verbs: `list` `run` `resume` `status` `watch` `logs` `cancel` `validate`
      `graph` `emit`; `--json`/NDJSON; stable exit codes (0 success / 1
      non-success / 2 usage). `hex run` with no graph lists what's runnable;
      `hex logs [--node <id>]` shows per-attempt agent output.
- [x] Isolation: `shared` only (the run's workspace is the project cwd).
- [x] Tests: kernel property basics (terminal schedules nothing; replay ==
      projection; unbounded cycles rejected) + kill-and-resume integration +
      critique-loop e2e + budget-exhaustion e2e on the mock worker.

## Operability pass — landed 2026-07-31

Pulled forward out of Phases 2/3 because each one blocked *daily* use rather than
being a hardening nicety. Driven by an honest review: the tool had 4 recorded
runs ever, all against its own repo, because the presets only worked in a Rust
project and a hung agent could block forever.

- [x] **Project-defined `checks:`** — a graph names a check
      (`command: { check: test }`); `.hex/config.yaml`'s `checks:` map supplies the
      argv; **empty by default**, and an unconfigured check routes `passed` with a
      journaled `Note`. Presets stopped hardcoding `cargo test`, so the built-in
      library runs in a Python/Node/Go repo unedited. Resolved at compile time and
      recorded in `RunCreated.checks`, so a config edit cannot change what a
      *resumed* run executes.
- [x] **`gate` merged into `command`** — the two IR variants were identical
      (same fields, one validation arm, one executor). Four node kinds now; a gate
      is the *role* a `command` plays when `accept.require` names its signal
      (which already worked for agent signals). `Effect::RunGate` → `RunCommand`.
      Core rule 4 updated.
- [x] **Every attempt is bounded** — `Budget.attempt_elapsed_ms`, always set by
      the loader (default 30m, `budget: { attempt: … }` to override).
      **This was a real hang**: with no `elapsed` declared, `wait_bounded` fell
      back to a bare blocking `child.wait()`, and the shipped `review` preset had
      exactly that shape. `attempt_deadline()` now takes whichever bound bites
      first.
- [x] **`hex doctor` + start-time preflight** — probes every configured worker
      and check against `PATH`; `run`/`resume` now *refuse to start* when the
      graph needs an agent CLI that is missing, instead of surfacing it as a
      failed first attempt. Stdlib `which` (no new dep).
- [x] **Exit codes encode the disposition** — 0/1/3/4/5 (2 stays clap's usage
      error). Previously everything non-success was `1`, which the README already
      claimed otherwise.
- [x] **The kernel says *why* a run ended** — `Effect::RecordTerminal` carries
      `why`, journaled as a `Note`: which budget ran out (attempts vs cycle
      visits, previously indistinguishable) and which `accept.require` evidence
      was missing (previously a bare `failed` — the worst diagnostic in the
      tool). Gave the computed-then-discarded `Acceptance::Missing` a consumer.
- [x] **Doc honesty** — the README's flagship YAML example did not parse (unknown
      `gates:`, missing `entry:`, unimplemented `gate: { use: … }`,
      `interactive:`); it is replaced and **pinned by a test** that compiles the
      README's first `yaml` block. Removed claims for `hex pause`, `hex respond`,
      `graph --format mermaid|dot`, and an in-repo authoring SKILL.md that does
      not exist.
- [x] **Bug fixes from a cross-model audit** — `hex watch` panicked byte-slicing
      a multibyte `graph_hash` from the (unvalidated) journal; `status --json`
      silently dropped `disposition`; test temp dirs keyed on PID alone could read
      a previous run's leftovers (the likely cause of the `worktree.rs` flake,
      which did *not* reproduce in 25 workspace runs); the timeout test raced its
      own run budget and now exercises the per-attempt bound instead.

## Roles & gating pass — landed 2026-07-31

Decisions locked in a design interview before implementing; each bullet is the
outcome, not a guess.

- [x] **Two-level config: `workers:` + `roles:`.** Workers are CLI adapters
      (internal); roles (`implementer`/`reviewer`/`planner`/`researcher`) are what a
      graph names, each binding a worker plus `model`/`effort`/`read_only`/`prompt`.
      `Workers::from_config` registers workers under their own names *and* roles
      under theirs (roles last, so they win a clash). Four roles ship; there is
      deliberately **no orchestrator** — Roo shipped one with zero tools, Kilo
      deprecated theirs, and LangGraph/Anthropic keep routing in the framework;
      all of them converge on hex's core rule 9.
- [x] **Layered deep merge**, built-in → user → project, per key: overriding
      `roles.reviewer.model` inherits the rest. `prompt:` replaces an inherited
      preamble, `prompt_append:` extends it. Preambles are prepended at *compile*
      time, so they are part of the IR a resumed run replays.
- [x] **Built-in defaults are `hex-runtime/src/defaults.yaml`**, embedded via
      `include_str!` and parsed by the same loader/merge path as user config —
      no hardcoded `Config::builtin()` to drift. Every surveyed tool that
      hardcoded defaults (Roo `DEFAULT_MODES`, Cline's mode union, Cursor's
      removed Custom Modes) forced all-or-nothing overrides; Cursor's team
      publicly conceded the cost.
- [x] **Undeclared checks are refused at compile time**, naming the key to add.
      Consequence, accepted deliberately: built-in presets ship **gate-free**
      (`critique-loop`, `plan-build-review`, `review`), while `tdd` and
      `implement-until-green` — whose gate *is* the preset — refuse to start until
      `checks.test` exists. This replaced the previous "pass with a note", which
      let a run reach `succeeded` having verified nothing.
- [x] **Multi-step command nodes with a mode**, one kind not two (kind count
      stays at four). The modes differ in *failure* semantics, not just
      concurrency: `ordered` stops at the first failure, `parallel` runs every
      step and fails if any did. Parallel buffers each step to
      `attempts/<id>/<n>-<label>/` in **declared** order, so concurrent evidence
      reads like sequential evidence and the journal stays deterministic.
- [x] **Per-node `budget: { visits: N }`** — bounds one loop (cap a review cycle
      at 3 without capping a cheap lint cycle). The run-wide `cycle_visits` stays
      as a blanket backstop.
- [x] **`accept.on_unmet: <node>`** reroutes a run that reached a success terminal
      without its required evidence, instead of dead-ending on `failed`. Carried by
      its own `EventBody::AcceptanceUnmet`, *not* a synthesized `Signal`, because
      `reduce` deliberately drops routing signals no in-flight attempt produced —
      a fail-closed guard that must not be relaxed to express this.
- [x] **`E-no-happy-path`**: a graph with no reachable `terminal: succeeded` is
      invalid. Reachability, terminal-exists and unbounded-cycle checks already
      existed; "any terminal counts" was the gap, so a graph that could only ever
      fail passed validation.
- [x] **Cleanups**: dropped the two ignored `_read_only` worker params; `effort`
      wired per agent (codex `-c model_reasoning_effort`, claude `--effort`) —
      which is exactly the per-worker divergence that justified keeping three
      adapter structs rather than collapsing them.

### Decided but NOT yet built — the agreed sequence

**(b) Detached runs + control inbox** — the prerequisite for driving hex from a
Claude session, and the largest remaining piece.

- [ ] `hex run --detach` (foreground stays the default): spawn with
      `process_group(0)`, never `fork()` (unsafe in a multithreaded Rust process),
      stdio to files, parent exits without `wait()`. Keep the advisory lock as the
      liveness signal — kernel-released on death, unlike a pidfile, and PID reuse
      is real — plus a journal heartbeat to tell "hung" from "crashed".
- [ ] Control inbox `.hex/runs/<id>/control/`: write temp then `rename()`
      (Maildir), polled at attempt boundaries. **cancel · pause/resume · steer**,
      each journaled with an actor so replay stays honest. Every daemonless job
      runner (GitHub Actions, GitLab, Buildkite) converges on polled shared state
      because there is no persistent listener to push to.
- [ ] `hex respond` over the same inbox — the **only** human-node transport, so it
      works foreground or detached, human or agent. Makes `human` nodes real;
      plan → approve → implement is the most universally shipped loop shape in the
      field survey (Cursor, Claude Code, aider, Cline, Roo).
- [ ] `hex runs` (list), `hex wait <id>`, `hex cancel` on a live run.
- [ ] A shipped SKILL.md teaching an agent to drive the CLI, and `hex-mcp` as a
      second thin client over the same `RuntimeClient` (kept for this reason).

## Control & detach pass — landed 2026-07-31

Phase (b) of the agreed sequence, implemented as specified.

- [x] **Control inbox** `.hex/runs/<id>/control/{tmp,inbox,done}/` — write to
      `tmp/`, `sync_all`, `rename()` into `inbox/`; the driver drains before each
      `schedule()` and renames into `done/` **before** applying, i.e. deliberately
      at-most-once (losing a `steer` beats double-applying a terminal). Files sort
      `{now_ms:013}-{uuid}.json`. `cancel` · `pause`/`resume` · `steer` ·
      `respond`, each journaled with its actor so replay reproduces the run.
- [x] **`pause` returns without a terminal** — `drive()` now returns
      `Option<Disposition>`; `None` = paused, no `RunFinished` written, `hex resume`
      continues the same run. `Status::Paused` finally has a producer.
- [x] **`human` nodes work** — `HumanRequested` (prompt interpolated) then blocks
      on the inbox at 250ms, bounded by `attempt_deadline()`; the answer is stored
      as the node result so `{{node.result}}` behaves exactly like an agent's.
      Validator honesty: `E-human-no-edge` / `E-human-multi-edge`.
- [x] **Detached runs** — re-exec `current_exe()` with a hidden
      `--reserved-run-id`, stdio to `detached.{out,err}`, `process_group(0)`,
      launcher exits without `wait()`. Never `fork()` (unsafe in a multithreaded
      Rust process). Liveness = run lock + 5s heartbeat → live / hung / abandoned.
- [x] **New verbs** `runs`, `wait`, `pause`, `steer`, `respond`; `cancel` now works
      on a *live* run (inbox) as well as an idle one (direct append). Exit code
      **6 = paused** joins 0–5.
- [x] **34 new tests** (200 total), including a half-written file in `tmp/` never
      being consumed, and a detached run outliving its launcher.
- [x] **hex now reviews itself**: `.hex/config.yaml` (roles → codex reviews,
      claude implements; checks `test`/`fmt`/`clippy`) and
      `.hex/graphs/self-review.yaml`, a parallel-gated review-only graph.

### Known warts from this pass

- [ ] A human answer is fenced downstream as "untrusted agent output" —
      `interpolate` does not know node kinds. Safe default, factually wrong label
      for an operator's own words. Cheap fix.
- [ ] `hex status` says "run not found" in the window between `--detach`
      reserving a run dir and the journal's first write; `hex runs` correctly says
      "no journal yet". Make `status` agree.
- [ ] Pause + `--worktree`: `start` releases the slot lease when it returns, so a
      *paused* worktree run's slot can be reclaimed (its uncommitted work
      discarded-and-logged) by another run before `hex resume`. In-scope of the
      documented reclaim behaviour, but new now that pause can return mid-run.
- [ ] **The fs4/flock flake is NOT root-caused, and it is not worktree-specific.**
      Now observed on the **run** lock too (`tests/control.rs::cancel_of_an_idle_run_is_recorded_directly`
      fails ~1 in 14 workspace runs: `RunLock::acquire` gets `WouldBlock` on a lock
      the just-returned `start` released, so `cancel` returns `Requested` instead
      of `Recorded`). That has a real user-facing analogue — `hex cancel` right
      after a run ends can spuriously queue instead of recording. Two independent
      sightings on two different lock files means this is in our locking or in fs4
      on macOS, not a worktree quirk. **Serializing the tests hid it rather than
      fixing it, and worse, the pool's concurrent path is now not exercised at
      all** — "parallel runs grow the pool" is an untested claim. Next step is a
      real diagnosis (log thread id + realpath + inode at every lock attempt),
      not another mutex. Original worktree symptom: Any concurrent in-process leasing made a
      just-released slot look busy to a sibling test — across *separate repos with
      distinct lock inodes*, which no obvious flock/fcntl semantics explain.
      serialized 8/8 pass, parallel ~1-in-3 failed, claiming slot 1 instead of the
      just-released slot 0. The safety invariant (no two live leases share a slot)
      is intact either way, so the cost is a needless extra slot, never
      corruption.
- [ ] `RunReport.disposition` is now `Option<Disposition>` and `RuntimeClient`
      gained `list_runs`/`control` with a new `cancel` signature — `hex-mcp` will
      inherit these when it is built.

### Quality pass — landed 2026-07-31 (`/simplify`, 4 parallel review angles)

- [x] **Two more guard-drift instances closed at the root.** `check_journal` was
      re-folding the journal after `reduce` already had (the read path folded
      twice); it now returns the projection it built. And `check_journal` rejected
      a duplicate `NodeResult` that `reduce` silently accepted — the *same* drift
      class as the `on_unmet` bug, hiding inside the refactor built to prevent it.
      The rule moved into `lifecycle::node_result_ok` (derivable from
      `state.results`, no new field), and `lifecycle.rs`'s doc now enumerates
      **all** remaining deliberate asymmetries instead of claiming there is one.
- [x] **One definition of a transition**: `Graph::implicit_reroutes()` /
      `implicit_reroute_from()` replace the `on_unmet` edge being derived
      independently by `schedule` and by cycle validation.
- [x] `ResultKind` deleted (was a duplicate of `hex_worker::ResultCapture` plus a
      zero-logic `From`); `preset::builtin()` now looks up `BUILTINS` instead of
      restating it; `push_flag` in `hex-worker`; two intentionally-different
      disposition renderers in `hex-cli` (JSON keeps `null`, humans get a word);
      `stage_then_rename` with a `Durability` enum; shared `test_support` modules.
- [x] Comment trims where the prose dwarfed the code — including my own
      17-line-comment/4-line-function `pool_shape_gate`.

### OPEN BUGS — found by review round 3, introduced by round 2's cycle fix

Both are unbounded-loop holes in the `accept.on_unmet` cycle validation added on
2026-07-31, i.e. the fix for one unbounded-cycle bug opened two more. Neither is
fixed. **Fix before trusting `on_unmet` or `human` nodes in an unattended run.**

- [ ] **A terminal's `budget: { visits: N }` is honoured by the validator but never
      enforced at runtime.** `check_cycles` (`hex-kernel/src/validate.rs:648`)
      treats *any* node carrying `max_visits` as breaking the cycle, but
      `schedule` (`hex-kernel/src/lib.rs:380`) settles terminal nodes **before** it
      checks visit budgets. So `implement → done → implement` (via `on_unmet`) with
      `budget.visits` on `done` alone passes validation and then reroutes forever.
      Fix: either check a terminal's visit bound before emitting `RerouteUnmet`, or
      stop counting terminal bounds as cycle breakers during validation. The
      second is probably right — a terminal spends no attempt, so bounding it is a
      confusing place to express a loop limit.
- [ ] **An attempt budget does not bound a human-only cycle.** `Graph::implicit_reroutes`
      (`graph.rs:236`) and `check_cycles` (`validate.rs:615`) both assume
      `budget.attempts` stops every cycle, but a human response creates **no
      attempt** — so `human A → human B → human A` costs one attempt to enter and
      then spins forever with `attempts: 2` while validation passes. Fix: treat an
      attempt budget as bounding only cycles that contain an `agent` or `command`
      node; a human-only cycle must require a visit bound. (Only reachable now that
      `human` nodes actually run, i.e. it arrived with the control pass.)

### Deliberately deferred by the quality pass (real, but need a decision)

- [ ] **`hex wait` re-verifies the entire run every 500ms** — re-reads
      `graph.yaml`, recomputes its SHA-256, re-parses and re-validates the graph,
      and re-folds the journal, on every tick for the run's whole lifetime. The
      biggest efficiency finding. Needs a cache-invalidation decision (the
      recorded hash makes the compiled graph safe to cache once verified), plus a
      liveness-only fast path — `wait` needs finished/paused/live/hung, not full
      state, until the final tick.
- [ ] **`hex runs` re-folds every historical run's journal on every call**, including
      runs that finished long ago and can never change. A sidecar summary written
      once at `RunFinished` would bound listing cost to in-flight runs.
- [ ] `Runtime::logs` loads every attempt's full stdout/stderr into memory with no
      cap — fine today, bad against a very verbose long run.
- [ ] Structural splits declined as churn on just-rewritten files: `start_attempt`
      (164 lines), `check_journal`'s 9 repeated guard arms (a `reject_unless`
      helper), `driver.rs` and `hex-runtime/src/lib.rs` both >700 lines.
- [ ] **Roles and workers share one shadowing namespace** because the compiled IR
      erases the distinction (`NodeSpec::Agent.worker` always holds the *role*
      name). The loader knows which YAML key the author wrote, so a
      `WorkerRef::Role | Direct` in the IR would remove the footgun documented in
      gotcha 19 instead of relying on insertion order. Invasive; noted.

**(c) Loop quality & observability**

- [ ] `context: continue` — real session resume (codex `exec resume`, claude
      `--resume`), capability-gated on `SessionResume`, degrading if a session
      expired. The biggest quality gap: the reviewer currently re-reads the diff
      cold each round and cannot know it already raised an ask.
- [ ] Stall breaker: stop and report when a check fails with an identical
      signature N times, or a node is revisited with an identical result.
- [ ] Cost/token accounting (`CostReporting`, kept for this): `claude
      --output-format json` already returns usage and it is currently discarded.
- [ ] Run digest + `hex watch --follow`; `hex config show` with per-key provenance;
      `hex graph <name> --source` to copy a preset out.

**(d) Presets & remaining cleanups**

- [ ] `autoresearch`: research → critique(`enough`/`more_needed`) → report, critic
      = the `reviewer` role reused, bounded by per-node visits. Matches
      open_deep_research's two-way exit (explicit signal OR hard cap); no surveyed
      research loop uses a deterministic content gate.
- [~] Approved deletion batch — **done 2026-07-31**: `EventBody::BudgetExhausted`,
      `Journal::path()`, `hex_runtime::open()`, plus `Inbox::drain()` and
      `Builder::commands()` found dead by the simplify pass. **Still open**:
      the unadvertised `Capability` variants except `SessionResume`/`CostReporting`
      (kept — session resume and cost accounting will consume them),
      `hex-worker`'s unused `hex-kernel` dep, hardcoded
      `HEX_EMIT_FILE`/`HEX_MAY_PROPOSE` literals in the CLI;
      drop `graph.sha256` (duplicates `RunCreated.graph_hash`); promote the
      worktree lease out of `RunCreated.inputs` into a typed event (it stores an
      absolute path next to the operator prompt today). `supports()` and
      `Status::Paused` are **kept** — session resume and pause give them consumers.
      Decided to keep: all 8 crates, `RuntimeClient` (MCP will use it), and the
      three worker adapter structs.
- [ ] Re-test the reviewer/implementer asymmetry: a published experiment found
      Claude reviewing Codex lifts pass rate 71.6%→89.7% while the reverse shows
      no gain or a regression. `defaults.yaml` now pairs codex-implements with
      claude-reviews on that basis; worth one deliberate A/B rather than trusting
      one paper.
- [ ] `effort` needs a live smoke test per agent — the flag spellings could not be
      verified here without agent auth.

### Superseded by the roles pass (kept for history)

- [ ] **Detached runs + a control inbox** — the prerequisite for driving hex from
      a Claude session. Research settled the design: spawn (never `fork()`, unsafe
      in a multithreaded Rust process) with `process_group(0)` and stdio to files,
      parent exits without `wait()`; keep the existing advisory lock as the
      liveness signal (kernel-released on death, unlike a pidfile — PID reuse is
      real) plus a journal heartbeat to tell "hung" from "crashed"; steer/cancel
      via `.hex/runs/<id>/control/` written temp-then-`rename()` (Maildir) and
      polled at attempt boundaries. Every daemonless job runner (GitHub Actions,
      GitLab, Buildkite) converges on exactly this because there is no persistent
      listener to push to. Separate verbs over the id (`hex wait`, `hex logs -f`),
      per docker/kubectl convention — not more flags on `run`.
- [ ] **Run digest + `hex watch --follow`** — an end-of-run summary (what
      changed, what the reviewer said, why it stopped, what it cost) and a
      live tail from a second terminal. `hex status` is still four lines.
- [ ] **`hex cancel` on a live run** — currently refused, because it needs the
      lock the driver holds. Falls out of the control inbox.
- [ ] **Stall / oscillation circuit breaker** — an unattended loop with no
      progress detection is a budget-burning machine. The incumbent
      `critique-loop` *skill* already does this (same asks 3 rounds → stop and
      report); hex does not.
- [ ] **Cost/token accounting** — `Capability::CostReporting` is declared and
      implemented by nobody; `claude --output-format json` already returns usage
      and `capture_result` throws it away. Unattended loops spend money silently.
- [ ] **`context: continue` (session resume)** — the reviewer currently re-reads
      the diff cold every round, so it cannot know it already raised an ask. This
      is the single biggest *quality* gap against the skill, which keeps one
      persistent navigator session. Would also give `Capability::SessionResume`
      its first real consumer instead of deleting it.
- [ ] **`human` approval node + `hex respond`** — plan → *operator approves* →
      implement is the most universally validated loop shape in the field survey
      (Cursor Plan Mode, Claude Code plan mode, aider architect, Cline Plan/Act,
      Roo Architect→Code). Today a `human` node passes `hex validate` and then
      fails the run.
- [ ] **An `autoresearch` preset** — every current preset is code-shaped with a
      test gate. Research has no deterministic gate; the field's substitutes are
      (a) a model self-assessment signal (maps onto `may_propose` as-is),
      (b) depth/breadth budget caps as the hard backstop (maps onto `Budget`),
      and (c) a post-hoc citation/grounding check as the one place a real
      deterministic gate fits.
- [ ] **Reconsider reviewer/implementer model assignment** — the presets pair
      claude-implements with codex-reviews. A published cross-model experiment
      found Claude reviewing Codex lifts pass rate 71.6%→89.7% while the reverse
      shows no gain or a regression, i.e. the presets may have the asymmetry
      backwards. Worth one deliberate A/B before flipping on one paper.
- [ ] **Structural cleanups an audit priced but that need a decision**: delete
      the zero-call-site `RuntimeClient` trait (−71 lines); collapse the three
      near-identical typed workers into one table-driven adapter (−90, contradicts
      gotcha 5c); delete the 6 never-advertised `Capability` variants and
      `supports()`; delete `EventBody::BudgetExhausted` (never constructed);
      drop `graph.sha256` (duplicates `RunCreated.graph_hash`, violating
      journal-is-authoritative); promote the worktree lease out of
      `RunCreated.inputs`, where it is stringly-typed next to the operator prompt
      and stores an absolute path; and decide whether 8 crates earn their keep at
      ~9k LOC (`hex-mcp`/`hex-dashboard`/`hex-bench` are ~130 lines of scaffold).
      Audit total: ~-1370 lines, -7 deps available.

## Phase 2 — hardening & correctness

The robustness cut from the MVP: make the kernel/runtime trustworthy before
adding surface. (Split out from the old mega "Phase 2"; UX/authoring is Phase 3.)

- [ ] Full validator: schema + published JSON Schema, worker-capability
      matching; canonical `format`. _Candidate crates:_ `schemars` (derive the
      JSON Schema from the loader's `Raw*` structs) + `jsonschema` (validate).
- [ ] `templates:`/`extends:`, run-level reusable `gates:` + `accept.require`.
- [ ] `command` node kind (shared executor with `gate`).
- [ ] Atomic `state.json` snapshot projection. (Torn-tail repair, monotonic-seq
      and lifecycle auditing, and a run-lock already landed in Phase 1's
      hardening pass.)
- [ ] Mock-worker contract suite (every adapter passes the same tests);
      full crash/replay suite (kill at every state transition, real SIGKILL of
      a child controller — beyond the Phase-1 resume/tamper/stale-lock tests).
- [ ] Canonical self-contained compiled snapshot (hash covers interpolated
      inputs + resolved defaults), superseding the source-hash + defaults-in-
      `RunCreated` integrity check shipped in Phase 1.
- [ ] Repeated-failure circuit breaker + progress-signature stall detection.
- [ ] Graph-surface versioning before the format is externally relied on: bump
      `version` on surface changes and keep a replay compiler per version, so a
      snapshot written by an older hex still resumes. Moot pre-release (no
      persisted runs, `.hex/runs` gitignored); required before 1.0. Pairs with
      the canonical compiled-snapshot item above.
- [x] Isolation: per-run `worktree` opt-in (shipped 2026-07-21, thin slice):
      `hex run --worktree [<base>]` / `--no-worktree` leases a **pooled**,
      reusable git-worktree slot (`.hex/worktrees/<n>/`, gitignored) on branch
      `hex/<run-id>` from HEAD/`<base>`; deps stay warm across reuse, dirty slots
      are reclaimed-and-logged, parallel runs grow the pool (fs4-locked). Journal
      + control channel stay in the main `.hex/runs`; codex gets `--add-dir` so
      its sandbox can still write them. `--worktree-init "<argv>"` warms a fresh/
      reclaimed slot. hex never commits — an injected banner asks the agent to.
      No auto-merge; branch left for manual integration. Design:
      `docs/design/2026-07-21-worktree-isolation.md`.
- [ ] Isolation follow-ups: `hex worktree` list/cleanup/prune (respecting
      `git worktree lock`, never removing unmerged work); integration verbs
      (`diff`/`merge`/`apply`); a concurrency cap; dep/build-cache ergonomics
      (`CARGO_TARGET_DIR`, pnpm store, `.worktreeinclude`); a graph-YAML
      `isolation:` default field; enforced (OS-sandbox) read-only as a second
      isolation axis; a bounded warmup (`--worktree-init`) with a deadline.
- [ ] **Typed isolation journal record.** Worktree metadata currently rides in
      `RunCreated.inputs` as stringly-typed `worktree.*` keys (see `WT_*` consts
      in `hex-runtime/src/lib.rs`); promote to a typed, fold-validated record on
      the versioned proto (like `graph_hash`) so it's integrity-bound, not an
      unvalidated map in the stable surface. Retires `WorkRequest.extra_writable_dir`
      too once a non-workspace control transport lands.

## Phase 3 — operator experience, authoring & presets

Make hex pleasant to drive and to author for. All build on the hardened
Phase-2 kernel; none change kernel semantics.

- [ ] Verbs: `pause`, `capabilities`; `graph --format mermaid|dot` (ascii
      shipped in Phase 1).
- [~] Polished CLI UX. **Shipped 2026-07-20 with the `clap` migration:** one
      unified help (bare `hex` renders the same clap help as `--help`, to stderr
      / exit 2), every command + argument documented, a global `--json` flag,
      value-name hints (`<TEXT>`/`<PATH>`/`<NODE>`), an examples block,
      `propagate_version`, and an `ls` alias for `list`. **Remaining:** aligned
      tables, TTY-aware color with `--no-color` (+ `NO_COLOR`), `--quiet`/
      `--verbose`, human-friendly diagnostics with source spans, and `hex watch
      --follow` to live-tail a run's journal as events append (poll the file;
      works from a second terminal while the run executes).
- [ ] Proper ASCII graph rendering for `hex graph`: a real laid-out diagram
      (boxes + arrows, cycles visible), not today's flat node/edge list; keep
      `--format ascii|mermaid|dot` so the same IR renders to each.
- [x] Live agent-output preview during a run (shipped 2026-07-21): a sticky
      footer (ratatui inline viewport) tails the in-flight attempt's stdout and
      stderr (interleaved best-effort — per-stream order exact, cross-stream is
      poll order not chronological, stderr dimmed) while the driver blocks, with a
      status line (node · worker · attempt N/budget · spinner elapsed · deadline
      countdown), including the current not-yet-newline partial line.
      Runtime exposes a `ProgressSink` (`event`/`attempt_started`/
      `attempt_finished` + `AttemptView`); the CLI renders on a background
      thread — the worker/kernel are untouched. TTY-only; `--no-preview`/
      `--json`/non-TTY fall back to plain line streaming.
- [ ] **Structured tool-call feed** in the preview (and richer `hex logs`):
      parse each agent's line-delimited-JSON stream into a normalized
      `ToolCallEvent {kind, target, status, exit_code?}` and render the last N
      (commands run, files changed, tokens) instead of raw stdout. Per-agent
      streams (researched 2026-07-21): codex `exec --json` JSONL
      (`item.completed` `command_execution`/`file_change`/…), claude
      `--output-format stream-json --verbose` (`assistant.tool_use` /
      `user.tool_result`), opencode `run --format json` or `serve` + `GET /event`
      SSE. Common denominator = one JSON object per stdout line → normalize.
      Schemas are all vendor-unstable → tolerate unknown types.
- [x] **Typed worker adapters** (shipped 2026-07-21): `CodexWorker`/
      `ClaudeWorker`/`OpencodeWorker` each own their argv, result-capture, and
      permission policy behind the `Worker` trait; `CommandWorker` is the generic
      argv escape hatch. Config selects via `kind:`. Permissioning is per-agent
      and never a blanket bypass: **codex** runs under its OS sandbox
      (`--sandbox workspace-write`: repo+tmp writable, `.git`/network blocked) —
      a real boundary; **claude** uses an auto classifier (`--permission-mode
      acceptEdits` + `--allowedTools` for the coding essentials incl. `Bash` +
      `--disallowedTools` denying destructive/exfil/publish commands) — a
      defense-in-depth deny-list, not an OS boundary (prefix matching is
      bypassable via shell chaining); **opencode** still uses `--auto` (blanket
      approve — a proper `opencode.json` `permission` block is deferred, see
      below). `read_only` is advisory only (see "Enforced read-only"). opencode
      uses a `JsonlLastText` capture (`--auto --format json`).
- [ ] **opencode permission classifier**: generate a per-run `opencode.json`
      `permission` block (`bash` allow/deny mirroring the Claude deny-list,
      `edit` scoped, `external_directory: deny`) so opencode matches codex/claude
      instead of a blanket `--auto` approve.
- [ ] **Configurable Claude permission policy**: today the allow/deny lists are
      hardcoded in `ClaudeWorker`. Expose an operator override (worker config)
      and an explicit, clearly-unsafe `bypass` opt-in for use only inside real
      isolation (worktree/container).
- [ ] **Live-verify + finish worker flags** (needs agent auth — not smoke-tested
      here): confirm codex `--output-last-message`/`--sandbox`, claude
      `--output-format json`, opencode `run --format json`; opencode
      `event_server` capability (`serve` + SSE) for richer fidelity.
- [ ] **Enforced read-only** for reviewer nodes: today `read_only` is advisory
      (prompt-only) because a real read-only sandbox also blocks the `hex emit`
      file channel. Needs a control transport outside the sandboxed workspace
      (e.g. MCP tool hook, or an emit dir the sandbox whitelists) so a reviewer
      can be sandbox-enforced read-only *and* still route its verdict.
- [ ] tdd's red/green gates run the whole `cargo test` suite, so they can't
      isolate the *new* test (an unrelated pre-existing failure reads as "red").
      Per-test targeting once node I/O can pass the test name to the gate.
- [ ] Dynamic shell completions (bash/zsh/fish): Tab-complete graph names from
      `hex list`, run-ids, verbs, and flags. _Candidate crate:_ `clap_complete`
      (now trivial — the CLI is on clap derive) + `clap_complete` dynamic
      completers for the graph-name/run-id value hints.
- [x] Strict argument parsing (shipped 2026-07-20 with the `clap` migration):
      unknown flags/commands, extra positionals, and prompt-source conflicts now
      fail with exit 2 instead of folding into positionals. Pinned by
      `clap_rejects_bad_invocations_with_exit_2` in `crates/hex-cli/tests/cli.rs`.
- [ ] **Authoring skill**: SKILL.md shipped in-repo teaching an agent to
      draft graph YAML from a task description and iterate against
      `hex validate` / `hex graph`.
- [~] `hex doctor` **shipped 2026-07-31** (workers + checks probed against
      `PATH`, plus a start-time preflight that refuses a run with a missing agent
      CLI). Still open: version/auth probing, and `hex init` to scaffold `.hex/`
      with an example graph and a `checks:` block.
- [x] **Built-in preset library** (shipped 2026-07-21), roles as topology +
      prompts (never new kinds): `implement-until-green` (Ralph: implement→test),
      `tdd` (spec→implement→test), `plan-build-review` (plan→implement→test→
      review), `review` (reviewer over the diff). claude implements/plans, codex
      reviews (cross-model); `critique-loop` realigned to match. Gates run
      `cargo test` (documented as a starting point to edit per stack). Prompts
      inlined (no `templates:` yet — Phase 2).
- [x] **Result-I/O handoff** (shipped 2026-07-21; a light slice of the Phase-6
      typed node I/O): the runtime captures each agent's final message via a
      `HEX_RESULT_FILE` sink (codex `--output-last-message`, claude json
      `.result`), stored as `NodeResult`; a downstream prompt references it as
      `{{node.result}}`, interpolated at attempt-start and wrapped as untrusted.
      An agent that finishes cleanly without emitting gets a synthesized reserved
      `done` signal (implicit completion); >1-outcome nodes still `hex emit`.
      Full typed inputs/outputs (`schemars`, `{{node.output}}` typed fields,
      per-node dataflow contracts) remain Phase 6.

## Phase 4 — interactive sessions & MCP transport

- [ ] `interactive: true` agent policy: live human↔agent conversation
      (grill-me/Q&A), every turn journaled (`worker.message`/`human.message`),
      suspend/resume mid-attempt; validator requires `live_steering` +
      `session_resume`.
- [ ] `hex respond` / steering flow from any client; sessions resumable via
      `hex resume` after interruption.
- [ ] **Interactive run UX**: combine the live preview (sticky footer, last-N
      lines + timer) with inbound steering — while a run executes (esp. in a
      worktree), the operator watches the agent's output *and* can type messages
      to the running agent (journaled `human.message`, delivered over the
      worker's `live_steering` channel). Needs the live-preview loop to accept
      stdin without tearing the viewport, and a worker transport that can inject
      a mid-attempt turn.
- [ ] Worker channel, transport 2: MCP tool hooks (`propose(event)`,
      `finish_session(status)`) lowering to the same `Command`. _Candidate
      crate:_ `rmcp` (the official Rust MCP SDK) — shared with the Phase 7
      `hex-mcp` server.
- [ ] `context: compact` (structured handoff then fresh).

## Phase 5 — approval + safe coding workflows

- [ ] Approval `human` node (blocking decision on a finished proposal:
      approve/reject/edit with actor + rationale) — e.g. plan → human approves
      → implement as two chained workflows.
- [ ] Actor authority scoping enforced end-to-end (observer / contributor /
      operator / approver; scoped worker tokens).
- [ ] Changed-file/diff artifacts; content-addressed artifact store.
- [ ] Worktree cleanup + manual integration helpers (still no auto-merge).
- [ ] Builder commands (`hex add`/`hex connect`) mutating the YAML in place.
- [ ] `triage`-style diagnostics + exportable run bundle.
- [ ] Config, richer policy layer: permissions, notifications, cost policies.

## Phase 6 — explicit concurrency & sub-agents

- [ ] TODO(design): parallelism + sub-agent grammar in the YAML surface —
      revisit "even simpler YAML" at the same time. _Candidate crate (if real
      concurrency is needed):_ `tokio` + `tokio::process` — a big commitment;
      the functional core stays sync, so only adopt when the scheduler must
      drive attempts concurrently. Defer until forced.
- [ ] `map`/`parallel`/`join` as explicit scheduler constructs: declared
      capacity, isolation, write ownership, fail policy (fail-fast/continue/
      all-or-nothing), fan-in reducer/quorum.
- [ ] Sub-agents & watchdogs as kernel-routed graph constructs (subgraph
      nodes / observer gates) — never coordination inside `hex-worker`.
- [ ] **Typed node inputs/outputs** (internal graph dataflow): a node produces
      typed output an edge maps into a downstream node's typed input, so a
      reviewer sees the implementer's concrete diff/plan — not a re-summary.
      Distinct from the operator prompt; interpolation grows from just
      `{{prompt}}` to `{{node.output}}`-style references. Pairs with `map`/
      `join` (a fan-out item is one typed value). Design the type set
      (string/enum/path/bool/list) with validation + defaults at this point.
- [ ] Per-node worktrees; serialized integration queue + integration gate
      (generation parallel, acceptance conservative and serialized).
- [ ] Per-node cost/token budgets where workers report usage.

## Phase 7 — daemon & remote surfaces

- [ ] Per-run background controller; `RuntimeClient::Remote` over a run-local
      socket + scoped token (`hex run --background`, attach/detach).
- [ ] `hex-mcp` server: same verbs as MCP tools; clients can start new runs;
      authoring prompts/templates exposed over MCP.
- [ ] `hex graph new` architect command (worker drafts a graph from a
      description, validates, writes the file).
- [ ] Live operator views (need the controller to know what's live): `hex ps`
      — active runs with current node, in-flight worker/agent, and remaining
      budget — plus a live multi-run online-log/agent view.
- [ ] `hex-dashboard` TUI: projection consumer + `RuntimeClient`; renders the
      live active-runs/agents/log views and can also start/control runs.
      _Candidate crate:_ `ratatui` (TUI); pairs with `rmcp` if the dashboard
      talks to a remote runtime.

## Only after demand

- [ ] Container/sandbox isolation adapters; PTY worker adapter tier.
- [ ] Issue tracker / PR adapters; web viewer; agent-generated graphs at
      runtime; distributed workers; SQLite query projection (deletable,
      rebuildable); preset marketplace/registry.
