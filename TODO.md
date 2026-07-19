# TODO / Roadmap

Phased so pause/resume, evidence, and truthful state are solid **before**
concurrency or remote control. Architecture locked in the 2026-07-19 design
session (see README.md + AGENTS.md; research: `docs/design/gpt-research-{1,2}.md`).

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

## Phase 1 — MVP: honest single-run cyclic kernel

- [ ] `hex-proto`: versioned `Event` envelope (schema, seq, run/node/attempt,
      actor, type, payload), `Command`, `Capability`.
- [ ] Graph IR + YAML loader (co-located `on:` map, `defaults:`, block-scalar
      prompts; `templates:`/`extends:` + `gates:`/`use:` can trail slightly);
      canonical `format`; published JSON Schema. Full grammar spec → design doc.
- [ ] Static validator: schema, references, reachability, **bounded-cycle
      (SCC) enforcement**, worker-capability match.
- [ ] Kernel: `reduce` / `schedule` (→ effect intents: `StartAttempt`,
      `RunGate`, `RequestHuman`, `CancelAttempt`, `RecordTerminal`) / `accept`.
- [ ] Runtime drive loop: replay → schedule → intent-before-effect with
      idempotency keys → execute → append → reduce; crash recovery (orphaned
      attempts marked `interrupted`; redo = new run, never silent rerun).
- [ ] Append-only JSONL journal + atomic snapshot projection; single writer,
      fsync, torn-tail tolerance, monotonic seq.
- [ ] Run dir: `.hex/runs/<id>/{graph.yaml, graph.sha256, events.jsonl,
      state.json, control/, artifacts/<hash>/, attempts/<id>/}`.
- [ ] Workers: deterministic mock (contract suite) + generic subprocess (argv)
      + one real coding-agent adapter.
- [ ] Node kinds: `agent`, `command`, `gate`, `human`, `terminal`; shared
      command/gate executor; run-level reusable gates + `accept.require`.
- [ ] Routing: ordered deterministic edges + `may_propose` validation with
      `route.rejected` feedback.
- [ ] Worker channel, transport 1: injected env (run/node/attempt ids, scoped
      token) + `hex emit`; event allow-list enforced per node.
- [ ] Context policy per node: `fresh` (default) vs `continue`.
- [ ] Budgets: attempts/time, per-node + per-run + per-cycle visits;
      repeated-failure circuit breaker; progress-signature stall detection;
      fail-closed.
- [ ] Isolation: `shared` default; per-run `worktree` opt-in (create worktree
      + branch, run there, leave branch; no auto-merge).
- [ ] CLI verbs: `validate` `graph` `run` `resume` `pause` `cancel` `status`
      `watch` `logs` `emit` `respond`; `--json`/NDJSON, stable exit codes,
      `capabilities`.
- [ ] Crash/replay test suite (kill at every state transition); kernel
      property tests (terminal runs schedule nothing; seq monotonic; one
      terminal per attempt; replay == projection; unbounded cycles rejected).

## Phase 2 — interactive sessions & MCP transport

- [ ] `interactive: true` agent policy: live human↔agent conversation
      (grill-me/Q&A), every turn journaled (`worker.message`/`human.message`),
      suspend/resume mid-attempt; validator requires `live_steering` +
      `session_resume`.
- [ ] `hex respond` / steering flow from any client; sessions resumable via
      `hex resume` after interruption.
- [ ] Worker channel, transport 2: MCP tool hooks (`propose(event)`,
      `finish_session(status)`) lowering to the same `Command`.
- [ ] `context: compact` (structured handoff then fresh).

## Phase 3 — approval + safe coding workflows

- [ ] Approval `human` node (blocking decision on a finished proposal:
      approve/reject/edit with actor + rationale) — e.g. plan → human approves
      → implement as two chained workflows.
- [ ] Actor authority scoping enforced end-to-end (observer / contributor /
      operator / approver; scoped worker tokens).
- [ ] Changed-file/diff artifacts; content-addressed artifact store.
- [ ] Worktree cleanup + manual integration helpers (still no auto-merge).
- [ ] Builder commands (`hex add`/`hex connect`) mutating the YAML in place.
- [ ] `triage`-style diagnostics + exportable run bundle; shell completion.

## Phase 4 — explicit concurrency & sub-agents

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

## Phase 5 — daemon & remote surfaces

- [ ] Per-run background controller; `RuntimeClient::Remote` over a run-local
      socket + scoped token (`hex run --background`, attach/detach).
- [ ] `hex-mcp` server: same verbs as MCP tools; clients can start new runs.
- [ ] `hex-dashboard` TUI: projection consumer + `RuntimeClient`; can also
      start/control runs.

## Only after demand

- [ ] Container/sandbox isolation adapters; PTY worker adapter tier.
- [ ] Issue tracker / PR adapters; web viewer; agent-generated graphs;
      distributed workers; SQLite query projection (deletable, rebuildable).
