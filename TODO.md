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

## Phase 2 — hardening & correctness

The robustness cut from the MVP: make the kernel/runtime trustworthy before
adding surface. (Split out from the old mega "Phase 2"; UX/authoring is Phase 3.)

- [ ] Full validator: schema + published JSON Schema, worker-capability
      matching; canonical `format`.
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
- [ ] Isolation: per-run `worktree` opt-in (branch left for manual
      integration; no auto-merge).

## Phase 3 — operator experience, authoring & presets

Make hex pleasant to drive and to author for. All build on the hardened
Phase-2 kernel; none change kernel semantics.

- [ ] Verbs: `pause`, `capabilities`; `graph --format mermaid|dot` (ascii
      shipped in Phase 1).
- [ ] Polished CLI UX: aligned tables, TTY-aware color with `--no-color`,
      `--quiet`/`--verbose`, human-friendly diagnostics with source spans, and
      progress while a run drives. `hex watch --follow` live-tails a run's
      journal as events append (poll the file; works from a second terminal
      while the run executes).
- [ ] Proper ASCII graph rendering for `hex graph`: a real laid-out diagram
      (boxes + arrows, cycles visible), not today's flat node/edge list; keep
      `--format ascii|mermaid|dot` so the same IR renders to each.
- [ ] Live agent-output preview during a run: under the streamed event lines,
      show the last ~8–12 lines of the *currently in-flight attempt's* stdout,
      refreshing in place as the agent prints (tail `attempts/<id>/stdout.log`;
      TTY-only, collapses to the final event line when the attempt ends).
- [ ] Dynamic shell completions (bash/zsh/fish): Tab-complete graph names from
      `hex list`, run-ids, verbs, and flags.
- [ ] Strict argument parsing: reject unknown flags and enforce per-verb arity
      instead of silently folding extras into positionals (today `hex list x`
      or a `--jsonn` typo pass quietly).
- [ ] **Authoring skill**: SKILL.md shipped in-repo teaching an agent to
      draft graph YAML from a task description and iterate against
      `hex validate` / `hex graph`.
- [ ] `hex init` (scaffold `.hex/` + example graph) + `hex doctor` (workers
      installed/authed/versions).
- [ ] **Grow the built-in preset library** beyond `critique-loop`, using the
      role vocabulary the research surveys — Ralph "hats" and AutoLoop roles:
      planner / implementer / reviewer / tester as *topology + prompts*, not
      separate processes (roles stay node metadata, never new kinds). Ship a
      small, opinionated set of bounded graphs for the task types autonomous
      loops actually work on (greenfield, mechanical changes, dependency bumps,
      well-specified defects, TDD):
      - `fix-until-green` — implement → test loop, tests the only backpressure,
        no reviewer (the pure Ralph pattern).
      - `tdd` — write-failing-test → implement → test-green loop.
      - `plan-then-build` — plan → (human-approve gate, Phase 5) → implement →
        test → review.
      - `review-only` — reviewer + gate over the current diff, no implementer
        (CI-style check; pairs with the review-only critique-loop flow).
      Factor shared role prompts into reusable node `templates:` (Phase 2) so
      presets compose one planner/reviewer definition. Resolution + authoring
      already exist (`hex list`, 3-layer lookup); this is content, not mechanism.

## Phase 4 — interactive sessions & MCP transport

- [ ] `interactive: true` agent policy: live human↔agent conversation
      (grill-me/Q&A), every turn journaled (`worker.message`/`human.message`),
      suspend/resume mid-attempt; validator requires `live_steering` +
      `session_resume`.
- [ ] `hex respond` / steering flow from any client; sessions resumable via
      `hex resume` after interruption.
- [ ] Worker channel, transport 2: MCP tool hooks (`propose(event)`,
      `finish_session(status)`) lowering to the same `Command`.
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
      revisit "even simpler YAML" at the same time.
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

## Only after demand

- [ ] Container/sandbox isolation adapters; PTY worker adapter tier.
- [ ] Issue tracker / PR adapters; web viewer; agent-generated graphs at
      runtime; distributed workers; SQLite query projection (deletable,
      rebuildable); preset marketplace/registry.
