# TODO / Roadmap

Phased so a working claude+codex critique loop ships **first**, then
pause/resume/evidence harden, then concurrency and remote control. Architecture
locked in the 2026-07-19 design session (see README.md + AGENTS.md; research:
`docs/design/gpt-research-{1,2}.md`).

## Decisions — resolved 2026-07-19

- [x] Crate shape: `hex-proto` / `hex-kernel` (pure: IR + journal +
      projections + reduce/schedule/accept) / `hex-worker` (adapter) /
      `hex-runtime` (owns the drive loop) / thin clients (cli, mcp,
      dashboard) over the runtime (via a `RuntimeClient` trait until
      2026-08-01, now over `Runtime` itself). (Amended 2026-09-13: the
      `hex-mcp`/`hex-dashboard` stubs were deleted; the cli is the one client
      until a second one is actually built.)
- [x] Definition syntax: **standard YAML**, single file, inline prompts,
      kind-as-key + co-located `on:` edges, `defaults:`/`templates:`/`extends:`,
      run-level `gates:` + `accept.require`. No custom mini-grammar.
- [x] Routing: deterministic ordered edges **+** agent proposals validated
      against a per-node `may_propose` allow-list (agents choose among given
      options only).
- [x] Process ownership: **A3 hybrid** — foreground in-process now; per-run
      background controller + a remote client later. (Amended 2026-08-01: the
      `RuntimeClient` trait held open for that second implementation was deleted
      — one impl, no callers. The decision stands; the seam gets re-derived when
      there is something to derive it from.)
- [x] Worker→runtime channel: one `Command` protocol, two transports —
      injected `hex emit` CLI (floor) + MCP tool hooks (`finish_session()`).
      **Superseded 2026-09-13 (simplification):** the `hex emit` transport is
      deleted; an agent node's routing verdict is the `VERDICT: <signal>` line
      of its final message, instructed and parsed by the runtime
      (`docs/design/2026-09-13-simplification.md`). Operator control keeps the
      file-inbox `Command` protocol.
- [x] Domain: 5 entities (Graph, Run, Event, Budget, Artifact); 5 node kinds;
      interactivity = policy flag on `agent`; approval = later `human` node.
- [x] Verbs: `run`/`resume` only execution verbs; **no** `retry`/`replay`/
      `skip` — redo is a new run. Plus pause/cancel/status/watch/logs/emit/
      respond/validate/graph, `--json` everywhere. **Amended 2026-09-13
      (simplification):** `watch`, `emit` and `dash` deleted; `stats` and
      `prune` added.
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
      suffixes), `humantime` (budget durations), `which` (is this agent CLI
      installed?), `jiff` (the run-id date prefix), `anstream` (`strip_str`, to
      measure a painted cell); dev: `assert_cmd` + `tempfile`
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
- [x] `hex-runtime`: `Runtime` API + a `RuntimeClient` trait with an `InProcess`
      impl (the trait was deleted 2026-08-01, unused);
      `hex-cli`/`hex-mcp`/`hex-dashboard` are thin clients on it (the
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
      (**Superseded 2026-09-13**: the emit channel and its env vars are gone —
      the verdict line replaced them; `may_propose` is still the allow-list.)
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
      as a blanket backstop. *(Superseded 2026-09-14: the run-wide bounds were
      deleted and every non-terminal node is now visit-bounded by default — see
      "Per-node bounding" above.)*
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

- [~] `hex run --detach` (foreground stays the default): spawn with
      `process_group(0)`, never `fork()` (unsafe in a multithreaded Rust process),
      stdio to files, parent exits without `wait()`. Keep the advisory lock as the
      liveness signal — kernel-released on death, unlike a pidfile, and PID reuse
      is real — plus a journal heartbeat to tell "hung" from "crashed".
      (Shipped 2026-07-31; **deleted 2026-09-13 (simplification)** — `hex run`
      blocks, backgrounding is the shell or tmux. Lock + heartbeat liveness
      survives as the `Liveness` enum.)
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
- [~] A shipped SKILL.md teaching an agent to drive the CLI, and `hex-mcp` as a
      second thin client over the same runtime. (The `RuntimeClient` trait was
      kept for this reason and deleted 2026-08-01 having never gained a second
      implementation; `hex-mcp` binds to `Runtime` and re-derives a trait if it
      ever needs one.) (SKILL.md shipped in `skills/hex/`; the `hex-mcp` stub
      crate was **deleted 2026-09-13** — an MCP client would be recreated as a
      new thin client when it is actually built.)

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
      (**Deleted 2026-09-13 (simplification)** — one blocking execution mode;
      lock + heartbeat liveness kept.)
- [x] **New verbs** `runs`, `wait`, `pause`, `steer`, `respond`; `cancel` now works
      on a *live* run (inbox) as well as an idle one (direct append). Exit code
      **6 = paused** joins 0–5.
- [x] **34 new tests** (200 total), including a half-written file in `tmp/` never
      being consumed, and a detached run outliving its launcher.
- [x] **hex now reviews itself**: `.hex/config.yaml` (roles → codex reviews,
      claude implements; checks `test`/`fmt`/`clippy`) and
      `.hex/graphs/self-review.yaml`, a parallel-gated review-only graph.

### Known warts from this pass

- [x] ~~A human answer is fenced downstream as "untrusted agent output"~~ — stale
      when written, or fixed without the note being updated: `interpolate` labels
      it `operator input — the human answer to this node, follow it`, pinned by
      `a_human_answer_is_labelled_operator_input_not_agent_output`.
- [x] ~~`hex status` says "run not found" in the window between `--detach`
      reserving a run dir and the journal's first write~~ — stale when written:
      `status` already distinguished a missing *directory* from a missing journal
      ("has no journal yet (reserved, or never started)"), which is the distinction
      `wait_for_journal` now relies on.
- [ ] Pause + `--worktree`: `start` releases the slot lease when it returns, so a
      *paused* worktree run's slot can be reclaimed (its uncommitted work
      discarded-and-logged) by another run before `hex resume`. In-scope of the
      documented reclaim behaviour, but new now that pause can return mid-run.
- [~] **The fs4/flock flake is NOT root-caused, and it is not worktree-specific.**
      (**User-visible symptom closed 2026-09-13**: `cancel` reads the journal
      before probing the lock, so a finished run records directly and the flaky
      probe is never taken. The flake itself is still undiagnosed; the
      worktree-slot side still has it.)
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
- [ ] `RunReport.disposition` is now `Option<Disposition>`, and the client surface
      gained `list_runs`/`control` with a new `cancel` signature — a future MCP
      client would inherit these. (They lived on `RuntimeClient` until it was
      deleted 2026-08-01; they are `Runtime` methods now. The `hex-mcp` stub
      crate was deleted 2026-09-13.)

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

- [x] **Ctrl-C kills the agent and saves progress** (shipped 2026-08-02).
      SIGINT/SIGTERM set a process-global flag (`hex_worker::interrupt`) that the
      existing 25ms polling wait checks, so the in-flight attempt's whole process
      group is killed by the same proven `kill_group` path a deadline uses. The
      driver journals what the attempt spent and produced, then
      `AttemptInterrupted` + `RunPaused`: exit 6, resumable with `hex resume`.
      Second press exits 130. Handler is `signal-hook` (already in the tree via
      crossterm, and safe — no `unsafe`, which the workspace forbids).
      Verified under a pty: agent process gone, run paused, resume completes.

### OPEN BUGS — found by review round 3, introduced by round 2's cycle fix

Both were unbounded-loop holes in the `accept.on_unmet` cycle validation added
on 2026-07-31. **Both fixed 2026-08-08** in `check_cycles`: a node breaks a
cycle only when its bound is actually enforced there.

- [x] **A terminal's `budget: { visits: N }` was honoured by the validator but
      never enforced at runtime** (`schedule` settles terminals before budget
      checks). Fixed the recommended way: a terminal's `max_visits` no longer
      counts as a cycle breaker, so `implement → done → implement` (via
      `on_unmet`) bounded only on `done` is now `E-unbounded-cycle` instead of
      an infinite reroute. Pinned by
      `a_terminal_visit_bound_does_not_bound_a_reroute_cycle` (verified to fail
      pre-fix).
- [x] **An attempt budget did not bound a human-only cycle** (a human response
      spends no attempt). Fixed: `budget.attempts` now breaks only cycles
      containing an `agent`/`command` node; a human-only cycle needs a visit
      bound, and the error message says so. Pinned by
      `an_attempt_budget_does_not_bound_a_human_only_cycle` (verified to fail
      pre-fix). `Budget::bounds_cycles` (one caller, now-wrong semantics) was
      deleted.

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

- [x] `context: continue` — real session resume (codex `exec resume`, claude
      `--resume`), capability-gated on `SessionResume`. **Landed 2026-07-31** in the
      payoff/partials/cost pass below; a node whose worker cannot resume is refused
      at compile time rather than degrading, since a silent degrade costs exactly
      what `continue` buys.
- [x] Stall breaker: stop and report when a check fails with an identical
      signature N times, or a node is revisited with an identical result.
      **Shipped 2026-09-13 (simplification)**: a gate failing with the identical
      output signature twice ends the run `failed` with
      `why: gate <x> failed identically twice` (`driver.rs`, `gate_sigs`);
      a pass clears the record. Agent-result stall detection stays unbuilt.
      Open edge (flagged by two reviewers, 2026-09-15): `gate_sigs` is
      in-memory, so a pause/`hex resume` between two identical failures resets
      the count — journal the signature (an `EventBody` field, schema bump) if
      a resumed run is ever seen wedging on the same gate.
- [x] Cost/token accounting (`CostReporting`, kept for this) — **landed
      2026-07-31**: `AttemptReported` + a `RunState.usage` projection, parsed from
      codex's and claude's own structured output, in micro-USD.
- [~] Run digest + `hex watch --follow`; `hex config show` with per-key provenance;
      `hex graph <name> --source` to copy a preset out. **The digest landed as
      output, not a verb** (2026-07-31): the end of `run`/`resume` prints why the run
      stopped, what it spent, each failed check's tail and the final message, and
      `hex status` renders the per-node/per-model breakdown — both off the same
      projection, so a third surface would only be somewhere for them to disagree.
      **`watch --follow` landed 2026-07-31** in the live-observability pass below,
      together with `logs --follow` (`hex watch` was then **deleted 2026-09-13** —
      `logs --follow` is the follower). Still open: `config show`. (`graph --format source` shipped and is how a preset is forked.)

**(d) Presets & remaining cleanups**

- [ ] `autoresearch`: research → critique(`enough`/`more_needed`) → report, critic
      = the `reviewer` role reused, bounded by per-node visits. Matches
      open_deep_research's two-way exit (explicit signal OR hard cap); no surveyed
      research loop uses a deterministic content gate.
- [~] Approved deletion batch — **done 2026-07-31**: `EventBody::BudgetExhausted`,
      `Journal::path()`, `hex_runtime::open()`, plus `Inbox::drain()` and
      `Builder::commands()` found dead by the simplify pass. **Resolved 2026-09-05** (ponytail pass): the never-constructed
      `Capability` variants (`StreamingOutput`/`GracefulCancel`/`ReadOnlyMode`)
      and `Command::Status` deleted (`LiveSteering` was kept for the designed
      interactive sessions, then deleted 2026-09-13 with `StructuredEvents` and
      `FreshSessions` — a capability nothing checks is dead weight; re-add it
      when interactive ships); `hex-worker`'s unused `hex-kernel`
      dep dropped. The hardcoded `HEX_EMIT_FILE`/`HEX_MAY_PROPOSE` literals
      went with the emit protocol (**deleted 2026-09-13**). **Still open**:
      drop `graph.sha256` (duplicates `RunCreated.graph_hash`, but it is what
      `verify_and_fold` checks today — needs a decision); promote the
      worktree lease out of `RunCreated.inputs` into a typed event (it stores an
      absolute path next to the operator prompt today). `supports()` and
      `Status::Paused` are **kept** — session resume and pause give them consumers.
      Decided to keep: all 8 crates and the three worker adapter structs.
      `RuntimeClient` was kept here ("MCP will use it") and **deleted 2026-08-01**:
      an unbuilt crate is not a caller.
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
- [~] **Run digest + `hex watch --follow`** — superseded: the end-of-run output
      and `hex status` carry the digest from one projection (a third surface
      would only disagree), and `hex watch` was deleted 2026-09-13 with the
      simplification pass (`hex logs --follow` is the live tail).
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
      the zero-call-site `RuntimeClient` trait (−71 lines; **done 2026-08-01**,
      ~90 lines by then); collapse the three
      near-identical typed workers into one table-driven adapter (−90, contradicts
      gotcha 5c); delete the 6 never-advertised `Capability` variants and
      `supports()`; delete `EventBody::BudgetExhausted` (never constructed);
      drop `graph.sha256` (duplicates `RunCreated.graph_hash`, violating
      journal-is-authoritative); promote the worktree lease out of
      `RunCreated.inputs`, where it is stringly-typed next to the operator prompt
      and stores an absolute path; and decide whether 8 crates earn their keep at
      ~9k LOC (`hex-mcp`/`hex-dashboard`/`hex-bench` are ~130 lines of scaffold).
      Audit total: ~-1370 lines, -7 deps available.

## Payoff, partials & cost pass — landed 2026-07-31

Design: `docs/superpowers/specs/2026-07-31-payoff-partials-cost-design.md`.
Driven by an audit of hex's own `self-review` runs whose central finding was
that they produced valuable reviews and reported `disposition: failed` and
nothing else — the evidence was on disk and no CLI surface reached it. 257 tests
(was 218), clippy clean.

- [x] **`run`/`resume` print what the run produced** — the kernel's terminal
      reason (`why:`), the spend, each failed check's label with its exit code
      and a 40-line tail of its output, and the final message; the same fields
      in `--json` (`why`, `result`, `failed_steps`, `usage`). Before this the
      end of a run was two lines, so a four-minute cross-model review's findings
      were reachable from no output at all.
- [x] **`hex logs` reaches command-step output at all** — `AttemptLog.steps`
      reads `attempts/<id>/<n>-<label>/` in declared order (position parsed
      numerically, so `10-` follows `9-`), and each step now records its exit
      status to an `exit` file beside its logs. `Runtime::logs` read only the
      attempt dir before, and a command node writes every byte into its step
      dirs — so no check output was retrievable from any surface, for any
      preset.
- [x] **`--full` writes the transcript to stdout**, not stderr: it is the
      requested data of that verb, not a diagnostic, so `hex logs <id> --full >
      out.txt` captures it now.
- [x] **Partial work is never discarded** — `run_agent` returned before
      `capture_result` on both the timeout and the nonzero-exit paths. Capture
      always runs now, and where there is no final message a bounded 8 KB
      **tail** of stdout *and* stderr (codex writes everything to stderr) is
      journaled as the node's result, prefixed `[partial output — …]` because a
      downstream `{{node.result}}` can interpolate it. This is what made a
      15-minute review that finished and *then* blew its deadline record nothing
      at all.
- [x] **A timed-out attempt kills its whole process group** — `process_group(0)`
      at spawn, then `killpg(SIGTERM)`, a 2s grace, `SIGKILL`, through `nix`'s
      safe wrapper (the workspace forbids `unsafe`). Grandchildren were
      reparented to pid 1 and survived every timeout before this.
- [x] **Usage & cost accounting** — `EventBody::AttemptReported { session_id,
      models, cost_micro_usd, duration_ms }` + `ModelUsage`, parsed from each
      agent's own structured output: codex now runs `exec --json`
      (`thread.started.thread_id` + `turn.completed.usage`; it reports no model
      and no cost, so tokens only and the label falls back to the role's
      configured model), claude reuses the JSON object result capture already
      parses. Money is integer **micro-USD** — an `Event` is `Eq` and a float
      cost could come back a different number on replay. `reduce` folds totals
      into `RunState.usage` (`by_node`/`by_model`/`total`), a projection, so
      every client reads one set of numbers; `hex status` renders it per node,
      per model and in total. Recorded on **every** outcome including a timeout,
      because an attempt that spent tokens and then died is the one whose cost
      matters. hex never estimates: no report, no money.
- [x] **Every run ends with a `RunFinished`** — `AttemptFailed` is deliberately
      self-terminating and already carried the disposition, so this is redundant
      as a *fact*. It exists so one event terminates every run whatever path it
      took: a consumer tailing for `run_finished` never saw a timed-out run end.
      `check_journal` accepts at most one, and only when its disposition
      **equals** the one already recorded (a mismatch is `E-journal-lifecycle`);
      absence stays legal, so the pre-change journals in `.hex/runs/` still
      read.
- [x] **`context: continue`** — the loader accepted only `fresh`. A node
      declaring it resumes *that node's* last session; the handle comes from the
      journal (`RunState.sessions`, per node), so it survives a crash and a `hex
      resume`. Per node deliberately: a reviewer resuming the implementer's
      session would inherit its reasoning and stop being an independent judge.
      `codex exec resume <id>` is a subcommand taking no
      `--sandbox`/`--add-dir`, so those travel as `-c sandbox_mode=…` / `-c
      sandbox_workspace_write.writable_roots=[…]`; claude is a plain `--resume
      <id>`. `check_workers` **refuses at compile time** a `continue` node on a
      worker without `Capability::SessionResume` — its first real consumer —
      rather than silently running fresh every round.
      `.hex/graphs/self-review.yaml` now uses it on the review node.
- [x] **`hex init`** — creates `.hex/` and `.hex/graphs/`, writes a commented
      starter `.hex/config.yaml` whose `checks:` is empty with the examples
      commented out (**no autodetection**, deliberately: what "green" means is
      the operator's call), and appends `.hex/runs/` + `.hex/worktrees/` to
      `.gitignore` only when absent. Never overwrites an existing config;
      idempotent, so a second run leaves the tree byte-for-byte as it found it.
- [x] **`hex doctor` probes `hex` itself** — a `self` row reporting whether the
      binary resolves on `PATH`, because `hex emit` is a plain PATH lookup in
      the agent's own shell and its absence broke routing in 3 of 3 real agent
      runs, surfacing as a 15-minute timeout. A missing `hex` makes `doctor`
      exit 1. Deliberately **not** in `preflight`: a single-outcome node
      completes implicitly without ever calling `hex`, so refusing every run
      would block work that would have succeeded. No `PATH` is injected anywhere
      — placing the binary on it is the operator's job.
      (**Retired 2026-09-13** with the emit protocol: an agent never calls
      `hex`, so the `self` row is gone.)

### Known warts from this pass

- [ ] **opencode reports neither usage nor a session id** — its JSON stream is
      not parsed for either, so an opencode node shows no spend and `context:
      continue` bound to it is refused at compile time. The stream carries the
      facts; nobody has written the parser.
- [ ] **codex reports no model name**, so its per-model split is labelled with
      the role's configured model, else the literal `"codex"`. Two codex roles
      that both leave `model` unset therefore merge into one `by_model` row.
- [~] **No price table, so a token-only agent shows no money.** codex's
      `cost_micro_usd` stays `None` and its cost cell renders `—`, meaning
      "nothing reported it" rather than "free" — honest, but not the answer an
      operator wants from a 12-attempt loop. A `prices:` config layer could
      populate it without a schema change; the event shape already allows it.
      Deliberately out of scope, because a table of prices in-tree is wrong the
      week a vendor changes one. **Half-addressed 2026-08-01**: the decision to
      ship no prices stands, but a *total* mixing a priced and an unpriced agent
      now renders `≥ $X` with a lower-bound line instead of passing half the spend
      off as the whole.
- [ ] `StepLog.exit` is the string the driver wrote (`"0"` / `"101"` /
      `"signal"`), not a typed exit status.

### Found by hex reviewing this pass, deliberately not fixed

`hex run self-review` over this change returned `changes_requested` with nine
findings; six were fixed in the same pass (unchecked `u64` addition in the usage
projection, an unbounded parsed cost, `hex init` truncating an unreadable
`.gitignore`, a stale `Note` printed as a successful run's `why`, `killpg` errors
other than `ESRCH` being swallowed, and a session id resumable by the wrong
worker). These remain:

- [ ] **The duplicate-report latch does not enforce unique attempt ids.**
      `attempt_report_ok` deduplicates only within the current in-flight
      occurrence, so a *forged* journal reusing `att_1` in a bounded cycle resets
      the latch and bills the attempt twice; two starts also collapse into one
      attempt directory. Pre-existing shape (`node_result_ok` has the same
      property), driver-generated ids are unique, so this is forged-journal-only —
      the fix is a seen-attempt-id set in the projection, enforced by
      `attempt_start_ok`.
- [ ] **A run ending on a `human` node shows the last *agent* result**, not the
      operator's answer: `Payoff` derives the result from `logs`, which is built
      from `NodeResult` events keyed by attempt, and a `HumanResponded` has no
      attempt. Reading raw events instead would bypass the `check_journal` audit
      that `logs` runs, so the honest fix is exposing the verified projection's
      `results` rather than re-scanning the journal. No shipped preset ends on a
      human node.
- [ ] **`VISITS` in the spend table is a visit count, not an attempt count.** A
      crash between `AttemptReported` and the attempt's terminal leaves two
      reports against one visit. The column is labelled `VISITS` to stay honest;
      an `attempts_by_node` projection incremented by valid `AttemptStarted`
      events is the real fix.

## Live observability & session policy pass — landed 2026-07-31

Driven by trying to *use* a detached run: `hex status` printed
`running / work / attempts: 1` with no clock, `hex logs` said `(no final message
captured)` while ten lines of the agent's live output sat in `stdout.log`, a queued
`hex steer` was visible on no surface at all, and `hex watch` was a one-shot
snapshot. 263 tests (was 257), clippy + fmt clean.

- [x] **`hex status` shows the live picture** — new `StatusReport` fields
      (`in_flight { attempt_id, node_id, worker, started_at_ms }`, `queued`,
      `pending_steer`, `asked`, `question`) rendered as
      `in flight: att_3 on work via implementer, running 12s`, a `waiting for you:
      hex respond <id> "…"` line carrying the blocking `human` node's question text,
      and the two steer stages; the same fields in `--json`. The elapsed clock is
      read off the `AttemptStarted` event, because the projection knows an attempt is
      in flight and only the journal knows when it began.
- [x] **Two distinct "sent but not applied" states**, and conflating them is what
      made a steer look lost. `Inbox::queued()` (new, and strictly **read-only** —
      moving anything into `done/` would mean checking on a steer consumed it) lists
      commands no driver has claimed; `RunState.pending_steer` is guidance already
      *journaled*, waiting for the next agent attempt to read it. Printed as
      `queued steer (not yet picked up)` vs `steer accepted (applies to the next
      agent attempt)`. Verified live: queued mid-attempt → journaled at the driver's
      next boundary → gone once an agent attempt consumed it.
- [x] **`hex steer` says when it lands** — `note: <id> is mid-attempt (att_1 on
      work); the steer applies to the NEXT attempt`, on stderr. Drain happens at an
      attempt boundary, so without that line the operator expects the running agent
      to change course and reads the unchanged output as a dropped command.
- [x] **`hex logs` tails a live attempt** instead of claiming nothing happened: the
      last `--tail N` lines (default 20) across *both* streams — codex writes
      everything to stderr and nothing to stdout, so either alone is silent for one
      of the two agents — then `(still running)`.
- [x] **`hex logs --follow`** streams the in-flight attempt until the run ends, then
      closes with `── <disposition> ──`. It follows *across* attempts and into a
      `command` node's numbered step dirs (`attempt_streams`, declared order so `10-`
      follows `9-`) — a gate is exactly the slow thing you wait on. It attaches at
      each stream's **tail**, not its head, and **drains a stopped attempt** before
      detaching: an attempt stops being in-flight the instant it terminates, so its
      most interesting line (a check's failure, an agent's last word) is written
      after the last poll that could still see it. (Attach mechanism superseded
      2026-08-01: sampling `in_flight` missed any attempt that fitted between two
      polls, so the follower is journal-driven now.)
- [x] **`hex watch --follow`** prints new events until the run finishes, cursored by
      event count — the journal is append-only, so "how many have I printed" is the
      whole cursor. (`hex watch` **deleted 2026-09-13**; `logs --follow` remains.)
- [x] **Both followers wait for a just-reserved run's journal** (`wait_for_journal`,
      15s): `--detach` reserves the run dir a moment before the driver's first write,
      so `hex logs --follow "$(hex run … --detach)"` — the obvious thing to type —
      used to fail instantly. "Not started yet" vs "does not exist" is told apart via
      `summary`, so a typo still fails fast.
- [x] **`Runtime::attempt_dir(run_id, attempt_id)`** hands a follower the path (run
      through `validate_run_id`, since an attempt id ends up in one) so no client
      learns the on-disk layout; **`Status::is_finished()`** added to the kernel for a
      client holding only a status.
- [x] **`hex runs` computes its column widths from the data** — the hardcoded
      `{:<12}`/`{:<34}` ran `finished:budget_exhausted` (25 chars) and legacy
      46-character run ids into the next field, in the first table a new user sees.
- [x] **`hex logs --json` step objects carry `exit` and `failed`**, so a driving
      agent can identify the failing check instead of reading every step's output.
- [x] **A cost of zero renders as an em dash, not `$0.0000`** — codex reports tokens
      and no money, so a dollar figure claimed the work was free. The spend column
      stays labelled `VISITS`, not `ATTEMPTS`, for the reason in the previous pass's
      warts (a crash between `AttemptReported` and the terminal leaves two reports
      against one visit).
- [x] Ticked the control pass's `hex status` reservation-window wart: `status`
      already reported "has no journal yet (reserved, or never started)" rather than
      "run not found", and that distinction is what `wait_for_journal` reads.

### Per-node session policy in the shipped presets

- [x] **`context: continue` where a loop revisits a node**: `implement` in
      `critique-loop`, `implement-until-green`, `plan-build-review` and `tdd`, plus
      `tdd`'s `spec` (revisited when `red` shows the new test did not actually fail).
      A revisited node keeps what it worked out instead of re-deriving the task and
      the codebase every round.
- [x] **`review` stays `fresh`, with a comment saying why** — a reviewer continuing
      its own session carries its earlier verdict into the next one, which is how a
      critic talks itself into approving what it already argued about. `autoresearch`
      is deliberately untouched: its continuity is on disk in
      `.hex/research-notes.md`, which its own header explains.
- [x] **The trade, documented at the point of use**: a `continue` node bound to a
      worker that cannot resume is **refused at compile time** (`check_workers`), so
      rebinding the `implementer` role to a `kind: command` worker makes these presets
      refuse to start until that node says `context: fresh` — the error names the
      node and the two workers that can resume.

### Still open after this pass

- [ ] `--follow` **polls at 400ms** (`FOLLOW_POLL`) rather than watching the
      filesystem. The same polled shape the control inbox uses, and fine for a loop's
      cadence, but a file watcher would be both cheaper and tighter.
- [ ] **No `hex logs --json --follow`** — `--follow` renders human lines only, so a
      driving agent streaming a live attempt has to poll `logs --json` /
      `status --json` instead of reading NDJSON. Needs a per-line event shape decided
      first (attempt header, stream, text), not just a flag. Since 2026-08-01 the
      combination is *rejected* by clap rather than silently ignored, so the gap is
      at least visible from the CLI.

## Spend honesty, generation budget & one fewer seam — landed 2026-08-01

Four decisions taken with the owner, then the findings of a second review round
(hex reviewing its own previous commit). 271 tests (was 263), clippy + fmt clean.

- [x] **`hex status` reports every token category** — `IN / OUT / CACHE R /
      CACHE W / REASON` instead of one collapsed number. They are not
      interchangeable: a cached read costs a fraction of a fresh input and
      dominates the volume — a real run of this repo measured 4.84M cache-read
      against 209k fresh input and 20.5k generated. `REASON` is a *subset* of
      `OUT`, carried for information and never added into a total.
- [x] **A partly priced total says so** — `Totals.unpriced_reports` +
      `cost_is_partial()`. An attempt is under-priced when it reports no
      authoritative attempt total **and** at least one of its models named no
      price; the `and` deliberately catches the *mixed* attempt, because summing
      only the priced half and calling it the total is the misreport being fixed.
      Renders `—` when nothing was priced, `≥ $X` when part was, plain `$X` when
      all was, plus `cost is a lower bound: N attempt(s) reported tokens but no
      price`; `--json` carries `cost_is_partial`/`unpriced_reports`. This repo's
      own config makes the mixed run the normal case (reviewer→codex, tokens only;
      implementer→claude, money). **Still no price table** — see the wart below.
- [x] **Run-wide `budget: { output_tokens: N }`**, counting **generation** tokens
      only. A bound over every reported token is dominated by cache (numbers
      above), so it would be tuned to context size rather than to work done, and
      `context: continue` would silently move it. Checked in the kernel at the
      attempt boundary, ending `budget_exhausted` (exit 4 — no new disposition);
      worst-case overshoot is one attempt, itself bounded by `budget.attempt`.
      `output_tokens: 0` is a validation error (`E-budget-zero`): spent before the
      first attempt, it would end a run that did nothing.
- [x] **`RuntimeClient` deleted** (~90 lines: nine signatures, one implementation,
      zero callers). What core rule 8 protects — one `Command` protocol, no
      parallel implementations — is delivered by the concrete `Runtime`; a trait
      is cheaper to re-derive from a second implementation than to keep honest
      without one. Reverses the "kept, MCP will use it" line in the deletion batch
      above. Stream access moved *into* the runtime as `Runtime::read_streams(run,
      attempt, &mut StreamCursor) -> Vec<StreamChunk>` (+ `StreamCursor::at_end`),
      so the CLI no longer walks `.hex/` directories: it renders only.

### Round-2 review fixes (hex reviewing the previous commit)

- [x] **A session was resumable by the wrong agent.** The previous pass compared
      the *worker registry name*, but a role registers under its own alias, so
      `"implementer" == "implementer"` still held after rebinding
      `roles.implementer.worker` from codex to claude — waving through `claude
      --resume <codex-thread-id>`, the exact bug the check was added for.
      `AttemptReported` now carries `agent` (the owning program, from
      `Worker::program()`, stamped by the shared plumbing from the argv actually
      spawned); `SessionHandle { agent, id }`; the comparison is the testable
      `driver::resumable_id`. A mismatch runs fresh rather than failing; a report
      naming no agent is not resumable at all.
- [x] **`hex logs --follow` missed attempts that started and finished between
      polls.** It sampled the `in_flight` projection every 400ms, so a fast check
      wrote its failure and exited unseen — a disposition printed with none of the
      evidence for it. It is now driven by the **journal**: every `AttemptStarted`
      is seen exactly once whenever it lands, the attempt in flight at attach time
      is joined at its tail, later ones stream from their first byte, and the open
      attempt is drained before switching away (its final bytes land after the last
      poll that could see it running).
- [x] **Saturating arithmetic completed** on the same untrusted data:
      `ModelUsage::tokens`, codex's multi-turn accumulator, and both folds in the
      CLI's `event_summary` still used unchecked `+`/`.sum()`, so a garbled or
      forged value panicked in debug and wrapped in release.
- [x] **A cursor reads exactly the bytes it accounts for.** The old follower
      sampled a file's length, read to EOF, then stored the sampled length —
      anything a writer appended in between was printed and then printed again on
      the next poll.
- [x] **`--follow` no longer ignores flags it accepts** — `--node` is honoured
      (attach only to that node), `--json --follow` is rejected by clap rather than
      silently emitting human text, and `--full` is documented as redundant under
      follow (which streams every captured byte).
- [x] **An unreadable control inbox is an error, not "nothing queued"** — `status`
      propagates the scan failure instead of `unwrap_or_default()`.

### Found by this round, deliberately not fixed

- [ ] **`queued()` has a claim window.** `status` folds the journal and then scans
      the inbox, so a command the driver claims between those two reads shows as
      neither queued (gone from `inbox/`) nor pending (not yet journaled). Harmless
      for a steer — the next `status` shows it accepted — but it is a real hole in
      the two-stage story gotcha 36 tells. A fold-after-scan ordering would trade
      it for a double-report, which is the better failure but needs a decision.
- [ ] **The token budget has no end-to-end test**, because **no fake worker reports
      usage**: the mock never writes an `AttemptReported`, so every usage test
      either injects the event into a journal by hand (CLI tests) or drives
      `reduce`/`schedule` directly (kernel tests). Enforcement of
      `budget.output_tokens` is therefore covered by kernel unit tests only — no
      integration run has ever been stopped by it. A mock that emits a scripted
      report would close this and the projection's other end-to-end gaps at once.
- [ ] Carried over, still open: the forged-journal attempt-id reuse hole and a run
      ending on a `human` node showing the last *agent* result (both in the payoff
      pass's "deliberately not fixed" list above).

## Ponytail pass — landed 2026-09-05

Whole-repo over-engineering audit (3 parallel reviewers), applied, plus the
preset-library gaps. Dogfooded: the hex-cli half of the cuts was implemented by
`hex run checklist --worktree` itself (claude-opus implementer, pi/gemini-3.8-flash
reviewer via openrouter — the first real cross-model run on the pi worker).

- [x] **New presets `code` / `research` / `pr`** — one-shot implementer, one-pass
      sourced research, and critique-loop-then-open-a-PR (`gh`). All gate-free.
      (**Deleted 2026-09-13 (simplification)** together with
      `plan-build-review` — never run; six presets remain, git history keeps
      the four.)
- [x] **`hex-bench` deleted** — the one bench file moved to
      `hex-runtime/benches/`; the crate was 8 lines of doc pulling three
      non-dev deps.
- [x] **Dead protocol surface cut** — `Command::Status` (no producer),
      `Capability::{StreamingOutput,GracefulCancel,ReadOnlyMode}` (never
      constructed or read), `ResultCapture::JsonResult` (+ its reader; no
      in-tree producer since claude moved to stream-json), `hex-worker`'s
      unused `hex-kernel` dep, `thiserror` (one derive → 7 lines of stdlib).
- [x] **Test-suite shrink** — trivial/tautological tests deleted
      (`replay_is_deterministic` folds a pure fn twice; clap-behaviour tests
      assert clap, not hex), near-identical tests merged into tables
      (proto wire shapes, interpolate cases, prompt sources), shared
      helpers (`bounded()`/`rejects()`/`temp_dir()`/`answer_when_asked()`).
      The kernel cost-fallback test now actually exercises the per-model-sum
      path it names (it previously asserted 0 == 0).
- [x] **Doc honesty** — README "Known broken" no longer lists the two
      `on_unmet` cycle holes fixed 2026-08-08; `graph --format source` moved
      off the not-built list; banned "pipeline" wording removed from
      `plan-build-review`; gotcha 5b's result modes match the enum again.
- [x] Declined, with reasons: `preset::list` single-map rewrite (couples into
      `collect_yaml`, ~10 lines for real churn), tempfile drop-guards (leaked
      temp dirs are documented debug evidence), `Workers::new()` removal (~35 call
      sites), `hex wait`/`hex runs` re-fold caching (real but invisible at
      current journal sizes — still listed under deferred efficiency).

### Found dogfooding the ponytail pass (2026-09-05), not yet fixed

- [ ] **A red gate at the branch base wedges a scoped checklist run** (gotcha
      48): `verify` runs workspace-wide checks, so pre-existing debt outside the
      run's scope fails the gate on files the prompt forbids touching, and the
      loop burns attempts to `budget_exhausted`. Candidates: scope gate checks
      to the branch diff; or a preflight that runs the gate once at the base and
      refuses to start (or warns) when it is already red. (Bounded, not fixed,
      2026-09-13: gate-signature stall detection ends the identical-failure
      loop `failed` instead of burning to `budget_exhausted`.)

## Simplification pass — landed 2026-09-13

Design and rationale: `docs/design/2026-09-13-simplification.md` (the
implementation spec; decisions locked in a grilling session).

- [x] **Verdict replaces `hex emit`** — the routing instruction is
      runtime-generated from `may_propose`, appended to the prompt at attempt
      start, and parsed from the RAW capture before the 16 KiB `cap_result`;
      reserved `unknown` fails closed or routes via `on: { unknown: … }`;
      `E-mixed-done-and-proposals`; `result: text` capture mode +
      `Worker::captures_result()` compile check (gotchas 5, 49).
- [x] **One blocking execution** — `--detach` and the reserved-run machinery
      deleted; backgrounding is `&` or tmux (SIGHUP caveat documented).
- [x] **One runtime liveness state** — `Liveness` (live / interrupted /
      finished / error) replaces `RunSummary.error` + `Option<Status>`; kernel
      `Status` untouched; hung is a diagnostic on live.
- [x] **`cancel` reads the journal before probing the lock** — closes the
      user-visible fs4-flake symptom.
- [x] **Surface cuts** — `hex dash`, `hex watch`, mermaid/DOT/`--format json`
      exports, `hex-mcp`/`hex-dashboard` stubs, and the `code` /
      `plan-build-review` / `pr` / `research` presets (six remain).
- [x] **Additions** — `hex stats` (cross-repo `~/.hex/stats.jsonl`, folded on
      read), `hex prune [--older-than <dur>] [--all]`, gate-signature stall
      detection, and a journaled `Note` when a worker exits cleanly with output
      but no parsed usage (so `budget.output_tokens` no longer fails open
      silently).

## Per-node bounding — landed 2026-09-14

- [x] **Deleted the run-wide retry budgets** (`Budget.attempts`,
      `Budget.cycle_visits`, and their `RawBudget` fields). Every non-terminal
      node's `max_visits` defaults to `DEFAULT_NODE_VISITS` (5); a node that
      exceeds its bound ends the run `budget_exhausted` naming the node. A
      graph still writing `budget: { attempts: N }` is refused by
      `deny_unknown_fields`. `check_cycles`/`E-unbounded-cycle` deleted (the
      default bound makes an unbounded cycle unconstructible); `E-budget-zero`
      now also rejects a node's `visits: 0`. Recorded snapshots are replayed with
      a legacy `attempts`/`cycle_visits` budget stripped, so a pre-change run
      still resumes; checklist's `implement` got an explicit bound so `review`
      stays the long-list knob (gotchas 5, 17, 18, 50).

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
- [~] Repeated-failure circuit breaker + progress-signature stall detection.
      (Gate-signature stall detection **shipped 2026-09-13**; an agent-result
      progress signature stays open.)
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

- [~] Verbs: `pause`, `capabilities`; `graph --format mermaid|dot` (ascii
      shipped in Phase 1). **`pause` shipped with the control inbox**, and
      **`graph --format text|json|mermaid|dot` shipped 2026-08-01** — then the
      mermaid/DOT/JSON arms were **deleted 2026-09-13 (simplification)**, never
      having been used; `--format text|source` remain.
      **Remaining:** `capabilities`.
- [~] Polished CLI UX. **Shipped 2026-07-20 with the `clap` migration:** one
      unified help (bare `hex` renders the same clap help as `--help`, to stderr
      / exit 2), every command + argument documented, a global `--json` flag,
      value-name hints (`<TEXT>`/`<PATH>`/`<NODE>`), an examples block,
      `propagate_version`, and an `ls` alias for `list`. **`hex watch --follow` and
      `hex logs --follow` shipped 2026-07-31** (polled, from a second terminal, live
      or resumed), and `hex runs` now sizes its columns from the data.
      **Remaining:** aligned tables elsewhere, TTY-aware color with `--no-color`
      (+ `NO_COLOR`), `--quiet`/`--verbose`, and human-friendly diagnostics with
      source spans.
- [x] **`hex dash` shipped 2026-08-09** — a live, full-screen table of all runs
      (a `top` for hex), the live counterpart of `hex runs`: same rows and
      vocabulary, redrawn every `--interval` ms (default 1000, `≥ 1`), quitting
      on `q`/`Esc`/`Ctrl-C`. A thin `Runtime` client reusing `hex runs`' row
      helpers and the shared `ui::Mark` colour/charset policy; refuses a non-TTY
      and `--json` (no machine mode — `hex runs --json` instead).
      (**Deleted 2026-09-13 (simplification)** — `hex runs` is the listing.)
- [ ] **Cut cross-model review cost — feed the reviewer the diff, not the whole
      repo.** Found dogfooding `hex dash` (checklist preset, cross-model review):
      a ~14-item run cost ≈ $40, almost all of it the codex reviewer re-reading
      the entire repo *every* round, so cost scales with repo size × review
      rounds. Worse, the holistic `final_review` looped — twice emitting
      `changes_requested` on already-fixed points with stale line numbers —
      burning attempts until a human steered/paused it. Candidates: (a) hand the
      reviewer the change under review directly (a `git diff`/patch in the
      prompt or a captured artifact) instead of relying on it to re-read; (b)
      cap holistic-review visits harder and/or require it to cite current
      line numbers; (c) let a review node scope its read to changed files. The
      per-item commits are safe regardless, so the failure mode is cost, not
      lost work. See memory `checklist-dogfood-cost-and-reviewer-loop`.
      (**Partly bounded 2026-09-13**: `checklist`'s `review.visits` dropped
      20 → 6, and gate-signature stall detection ends an identical-failure loop;
      the diff-not-repo reviewer feed itself is still open.)
- [ ] Proper ASCII graph rendering for `hex graph`: a real laid-out diagram
      (boxes + arrows, cycles visible), not today's flat node/edge list.
      (The mermaid/DOT/JSON arms were deleted 2026-09-13; `text|source` remain.)
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
- [x] Graph progress strip in the live preview (shipped 2026-08-02): the bottom
      border carries the whole graph in `Topology` order — `▸` on the active
      node, `×N` on every node entered, dim for the not-yet-reached — so the
      footer answers *is this loop advancing or circling* without a second
      command. `Session::progress()` builds it from `state.visits`; it rides the
      border (costs no tail line) and elides from the front to fit, keeping the
      active node. Deliberately colourless and tick-free: the strip is drawn
      while a failing gate loops, so a `✓` there would read as a verdict.
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
      (prompt-only) because a real read-only sandbox also blocks the
      `HEX_RESULT_FILE` write, losing the final message and with it the verdict
      (amended 2026-09-13 — the emit channel is gone, but the result file has
      the same problem). Needs a result path outside the sandboxed workspace so
      a reviewer can be sandbox-enforced read-only *and* still route.
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
      CLI); a `self` row probing `hex` itself and `hex init` followed later the same
      day (see the payoff/partials/cost pass). Still open: version/auth probing, and
      the example graph — `init` creates `.hex/graphs/` but writes nothing into it.
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
      `done` signal (implicit completion); >1-outcome nodes still `hex emit`
      (superseded 2026-09-13: they end their final message with
      `VERDICT: <signal>` instead).
      Full typed inputs/outputs (`schemars`, `{{node.output}}` typed fields,
      per-node dataflow contracts) remain Phase 6.

## Phase 4 — interactive sessions & MCP transport

- [ ] `interactive: true` agent policy: live human↔agent conversation
      (grill-me/Q&A), every turn journaled (`worker.message`/`human.message`),
      suspend/resume mid-attempt; validator requires `session_resume` plus a
      re-added `live_steering` capability (deleted 2026-09-13 while unused).
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

- [ ] Per-run background controller; a remote client over a run-local socket +
      scoped token (`hex run --background`, attach/detach). This is the second
      implementation that earns back a client trait — derive it then, from two
      real implementations, rather than keeping an empty one open for it.
- [ ] `hex-mcp` server: same verbs as MCP tools; clients can start new runs;
      authoring prompts/templates exposed over MCP. (The stub crate was deleted
      2026-09-13; recreate it as a thin `Runtime` client when built.)
- [ ] `hex graph new` architect command (worker drafts a graph from a
      description, validates, writes the file).
- [ ] Live operator views (need the controller to know what's live): `hex ps`
      — active runs with current node, in-flight worker/agent, and remaining
      budget — plus a live multi-run online-log/agent view.
- [ ] `hex-dashboard` TUI: projection consumer over the runtime; renders the
      live active-runs/agents/log views and can also start/control runs.
      _Candidate crate:_ `ratatui` (TUI); pairs with `rmcp` if the dashboard
      talks to a remote runtime.

## Only after demand

- [ ] Container/sandbox isolation adapters; PTY worker adapter tier.
- [ ] Issue tracker / PR adapters; web viewer; agent-generated graphs at
      runtime; distributed workers; SQLite query projection (deletable,
      rebuildable); preset marketplace/registry.
