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
      `hex run critique-loop --input task="…"`. ("Preset", not "pipeline" —
      the term pipeline stays banned as a Graph synonym.)
- [x] Config: layered `~/.config/hex/config.yaml` + project `.hex/config.yaml`
      (project wins) — worker registry (argv template, headless flags, model,
      declared capabilities) + default budgets/context/isolation in MVP;
      richer policy (permissions, notifications, cost) later.
- [x] Authoring automation: a shipped **authoring skill** (SKILL.md teaching
      an agent to write + `hex validate` graph YAML) right after MVP;
      `hex graph new` architect command and MCP prompts deferred.

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

## Phase 1 — slim MVP: claude + codex critique loop

Goal: `hex run critique-loop --input task="…"` works end-to-end — codex
implements, claude critiques, loop until approved + gate passes — and a killed
run resumes from its journal.

- [ ] `hex-proto`: versioned `Event` envelope (schema, seq, run/node/attempt,
      actor, type, payload) + `Command`.
- [ ] Config loading: `~/.config/hex/config.yaml` + `.hex/config.yaml`
      (project wins) — worker registry + default budgets/context.
- [ ] YAML loader → Graph IR (kind-as-key, co-located `on:` map, `defaults:`,
      block-scalar prompts, `inputs:` parametrization). Templates/extends and
      reusable `gates:`/`use:` are Phase 2.
- [ ] Validator (minimum honest set): references, reachability,
      **bounded-cycle (SCC) enforcement**, `may_propose` coverage.
- [ ] Kernel: `reduce` / `schedule` (→ `StartAttempt`, `RunGate`,
      `RequestHuman`, `CancelAttempt`, `RecordTerminal`) / `accept` — minimal
      but real.
- [ ] Runtime drive loop: replay → schedule → intent-before-effect with
      idempotency keys → execute → append → reduce; crash recovery (orphaned
      attempts marked `interrupted`; redo = new run).
- [ ] Append-only JSONL journal + replay; run dir
      `.hex/runs/<id>/{graph.yaml, graph.sha256, events.jsonl, artifacts/,
      attempts/<id>/}`. Atomic `state.json` snapshot may trail to Phase 2.
- [ ] Workers: **claude** + **codex** headless adapters (argv, fresh session
      per attempt) + a simple mock for tests.
- [ ] Worker channel, transport 1: injected env (ids + scoped token) +
      `hex emit`; per-node `may_propose` allow-list enforced.
- [ ] Node kinds working: `agent`, `gate` (inline command), `terminal`
      (`command`/`human` may stub to Phase 2).
- [ ] Budgets: attempts + elapsed time (per node + per run + per cycle
      visits); fail-closed.
- [ ] Preset resolution: `.hex/graphs/` > `~/.config/hex/graphs/` > built-in;
      ship built-in `critique-loop` (codex implements → claude critiques →
      gate) with `--input` parameters.
- [ ] Verbs: `run` `resume` `status` `watch` `cancel` `validate`; `--json`
      NDJSON on watch; stable exit codes.
- [ ] Isolation: `shared` only.
- [ ] Tests: kernel property basics (terminal runs schedule nothing; replay ==
      projection; unbounded cycles rejected) + one kill-and-resume
      integration test + critique-loop e2e on the mock worker.

## Phase 2 — hardening + authoring

Everything deliberately cut from the MVP.

- [ ] Full validator: schema + published JSON Schema, worker-capability
      matching; canonical `format`.
- [ ] `templates:`/`extends:`, run-level reusable `gates:` + `accept.require`.
- [ ] `command` node kind (shared executor with `gate`).
- [ ] Atomic `state.json` snapshot projection; torn-tail tolerance; monotonic
      seq audit.
- [ ] Mock-worker contract suite (every adapter passes the same tests);
      crash/replay suite (kill at every state transition).
- [ ] Repeated-failure circuit breaker + progress-signature stall detection.
- [ ] Verbs: `pause`, `logs`, `graph` (ascii/mermaid/dot); `capabilities`.
- [ ] Isolation: per-run `worktree` opt-in (branch left for manual
      integration; no auto-merge).
- [ ] **Authoring skill**: SKILL.md shipped in-repo teaching an agent to
      draft graph YAML from a task description and iterate against
      `hex validate` / `hex graph`.
- [ ] `hex init` (scaffold `.hex/` + example graph) + `hex doctor` (workers
      installed/authed/versions).

## Phase 3 — interactive sessions & MCP transport

- [ ] `interactive: true` agent policy: live human↔agent conversation
      (grill-me/Q&A), every turn journaled (`worker.message`/`human.message`),
      suspend/resume mid-attempt; validator requires `live_steering` +
      `session_resume`.
- [ ] `hex respond` / steering flow from any client; sessions resumable via
      `hex resume` after interruption.
- [ ] Worker channel, transport 2: MCP tool hooks (`propose(event)`,
      `finish_session(status)`) lowering to the same `Command`.
- [ ] `context: compact` (structured handoff then fresh).

## Phase 4 — approval + safe coding workflows

- [ ] Approval `human` node (blocking decision on a finished proposal:
      approve/reject/edit with actor + rationale) — e.g. plan → human approves
      → implement as two chained workflows.
- [ ] Actor authority scoping enforced end-to-end (observer / contributor /
      operator / approver; scoped worker tokens).
- [ ] Changed-file/diff artifacts; content-addressed artifact store.
- [ ] Worktree cleanup + manual integration helpers (still no auto-merge).
- [ ] Builder commands (`hex add`/`hex connect`) mutating the YAML in place.
- [ ] `triage`-style diagnostics + exportable run bundle; shell completion.
- [ ] Config, richer policy layer: permissions, notifications, cost policies.

## Phase 5 — explicit concurrency & sub-agents

- [ ] TODO(design): parallelism + sub-agent grammar in the YAML surface —
      revisit "even simpler YAML" at the same time.
- [ ] `map`/`parallel`/`join` as explicit scheduler constructs: declared
      capacity, isolation, write ownership, fail policy (fail-fast/continue/
      all-or-nothing), fan-in reducer/quorum.
- [ ] Sub-agents & watchdogs as kernel-routed graph constructs (subgraph
      nodes / observer gates) — never coordination inside `hex-worker`.
- [ ] Per-node worktrees; serialized integration queue + integration gate
      (generation parallel, acceptance conservative and serialized).
- [ ] Per-node cost/token budgets where workers report usage.

## Phase 6 — daemon & remote surfaces

- [ ] Per-run background controller; `RuntimeClient::Remote` over a run-local
      socket + scoped token (`hex run --background`, attach/detach).
- [ ] `hex-mcp` server: same verbs as MCP tools; clients can start new runs;
      authoring prompts/templates exposed over MCP.
- [ ] `hex graph new` architect command (worker drafts a graph from a
      description, validates, writes the file).
- [ ] `hex-dashboard` TUI: projection consumer + `RuntimeClient`; can also
      start/control runs.

## Only after demand

- [ ] Container/sandbox isolation adapters; PTY worker adapter tier.
- [ ] Issue tracker / PR adapters; web viewer; agent-generated graphs at
      runtime; distributed workers; SQLite query projection (deletable,
      rebuildable); preset marketplace/registry.
