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
- [ ] `hex init` (scaffold `.hex/` + example graph) + `hex doctor` (workers
      installed/authed/versions).
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
