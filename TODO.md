# TODO / Roadmap

Sequenced so pause/resume, evidence, and truthful state are solid **before**
concurrency or remote control. Derived from `docs/design/` (research §13, §17).

## Now — scaffold ✅

- [x] Layered Cargo workspace (edition 2024, resolver 3), 8 crates.
- [x] Placeholder types wiring the inward dependency graph.
- [x] README, TODO, research docs, agent files.
- [x] `cargo build` / `test` / `clippy` / `bench` all green.

## Open decisions (resolve at start of MVP)

- [ ] Definition syntax: constrained **TOML** vs **YAML** (both compile to one IR).
- [ ] Routing policy: deterministic-only, or deterministic + declared agent-selected nodes.
- [ ] Process ownership: foreground-only first, or a small per-run background controller.
- [ ] Control transport: Unix socket / named pipe vs append-only request journal.
- [ ] Isolation default: shared workspace vs git worktree for coding runs.

## MVP — honest single-run cyclic kernel

- [ ] Graph IR + loader for the chosen surface syntax; canonical `format`.
- [ ] Static validator: schema, references, reachability, **bounded-cycle** enforcement.
- [ ] Node kinds: `agent`, `command`, `gate`, `human`, `terminal`.
- [ ] Deterministic routes + allowed agent proposals.
- [ ] Pure reducer + sequential scheduler + effect boundary.
- [ ] Append-only JSONL journal + atomic snapshot; replay/recovery.
- [ ] Generic subprocess backend + deterministic mock backend (contract suite).
- [ ] One real coding-agent adapter.
- [ ] `fresh` vs `continue` context policy (explicit, per node).
- [ ] Completion contract: required evidence gates + acceptance rules.
- [ ] Budgets: attempts/time; repeated-failure circuit breaker; stall/progress-signature.
- [ ] CLI: `validate` `graph` `explain` `dry-run` `run` `status` `watch` `logs`
      `pause` `resume` `cancel` `step` `emit` `guide` `approve` `reject` `respond`.
- [ ] Readable console + stable `--json`/NDJSON, stable exit codes, `capabilities`.
- [ ] Crash/replay test suite (kill at every state transition).

## Next — safe coding workflows

- [ ] Git worktree isolation; changed-file/diff artifacts; content-addressed store.
- [ ] Cleanup + manual/automatic integration policy.
- [ ] Subgraphs; context compaction/handoff.
- [ ] `triage` + exportable run bundle; shell completion; agent-readable capability schema.

## Later — explicit concurrency

- [ ] `parallel`/`map`/`join` with declared capacity, isolation, and fan-in.
- [ ] fail-fast / continue / all-or-nothing policies; reducer/quorum.
- [ ] Serialized integration queue; per-node cost/token budgets.
- [ ] Optional TUI (`hex-dashboard`), consuming the same event stream.

## Only after demand

- [ ] Background controller / remote access; container & sandbox adapters.
- [ ] MCP server (`hex-mcp`) exposing the same control API.
- [ ] Issue tracker / PR adapters; web viewer; agent-generated graphs; distributed workers.
