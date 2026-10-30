# Dynamic workflows: research notes

Two research passes on 2026-10-17 for [the design](2026-10-17-dynamic-workflows.md). The facts come from the cited pages. A finding that says "not verified" is an inference.

## 1. Dynamic multi-agent systems

| System | Dynamic mechanism | What hex can take | Source |
|---|---|---|---|
| LangGraph | `Send` fans out with per-branch state. Reducers merge results. `Command(goto, update)` routes. `recursion_limit` is 1000. | Merge results by name. A hard step bound. | https://docs.langchain.com/oss/python/langgraph/graph-api |
| LangChain multi-agent | Compares subagents, handoffs, skills and router. Subagents use about 67% fewer tokens than skills on multi-domain tasks. | Isolated context per branch is the cheap path. | https://docs.langchain.com/oss/python/langchain/multi-agent |
| OpenAI Agents SDK | Handoffs, or agents called as tools. Orchestration "via LLM" or "via code". | Code orchestration when you need determinism. | https://openai.github.io/openai-agents-python/multi_agent/ |
| Google ADK | `ParallelAgent` has isolated branches. Each branch writes its own `output_key`, and a merger reads the keys. ADK 2.0 has a graph engine. | Join by named keys. Branches share no history. | https://adk.dev/agents/workflow-agents/parallel-agents/ |
| MS AutoGen GraphFlow / Agent Framework | DiGraph fan-out. A join uses `activation_condition` `all`/`any`. Execution runs in supersteps with barriers. | Ship `all` first, `any` later. A slow branch holds up the join. | https://learn.microsoft.com/en-us/agent-framework/workflows/workflows |
| Magentic-One | Task and progress ledgers. A stall counter (2 or less) triggers a replan. No round limit by default. | A stall threshold. Always set an explicit cap. | https://arxiv.org/abs/2411.04468 |
| Temporal | The workflow is deterministic and LLM calls are activities. Replay uses recorded LLM outputs. | Confirms the kernel/runtime split. Never re-ask a model on replay. | https://temporal.io/blog/of-course-you-can-build-dynamic-ai-agents-with-temporal |
| LLMCompiler | An LLM planner emits a DAG with placeholders. A unit with no LLM dispatches ready tasks. Up to 3.7x faster and 6.7x cheaper than ReAct. | Precedent for "the agent plans, the engine runs". | https://arxiv.org/abs/2312.04511 |
| Archon | YAML nodes with `depends_on`, `loop` + `until`, `fresh_context`, one worktree per run. Ships a workflow that writes workflows. | A workflow that writes workflows is a preset, not a feature. | https://github.com/coleam00/Archon |
| Ralph loop | One prompt in a `while` loop. Fresh context, a plan file, tests as backpressure. One builder at a time. | `context: fresh` + gates + one builder. | https://ghuntley.com/ralph/ |
| Anthropic research system | A lead spawns 3–5 subagents. Effort scales: 1 agent for a fact, 2–4 for a comparison, 10+ for complex work. | Effort rules in the skill. | https://www.anthropic.com/engineering/multi-agent-research-system |

Pitfalls:
- **Token cost.** A single agent uses about 4x chat tokens, and multi-agent about 15x. Token usage explains about 80% of the performance variance (Anthropic).
- **Coding parallelizes poorly.** Multi-agent variants degraded sequential tasks by 39–70% (https://arxiv.org/abs/2512.08296).
- **Conflicting decisions** between parallel writers (https://cognition.com/blog/dont-build-multi-agents).
- **Over-spawning:** early Anthropic versions spawned 50 subagents for simple queries.
- **Barrier stall:** one slow branch blocks the join.

Not verified: ADK 2.0 dynamic graphs, the full Magentic-One paper, CrewAI `@router`.

## 2. Claude Code and other CLIs

- **Subagents** (https://code.claude.com/docs/en/sub-agents): `.claude/agents/*.md` with `model`, `tools`, `effort`, `isolation: worktree`, `background`. The `Agent` tool takes `model` per call. Several calls in one turn run at once. Nesting depth 3, at most 20 at once.
- **Workflows** (https://code.claude.com/docs/en/workflows): the `Workflow` tool runs a JS script that Claude writes, with `agent()`, `parallel()`, `pipeline()`, `phase()`.
  - The operator approves a phase plan before the start.
  - `/workflows` shows progress, with pause, stop and restart.
  - Replay resume works only within the same session.
  - Limits: 16 agents at once, 1,000 per run. A warning appears above 25 agents.
  - No operator input mid-run.
- **Agent teams** (https://code.claude.com/docs/en/agent-teams): experimental. A shared task list and file mailboxes. `/resume` does not restore teammates.
- **`/goal`** (https://code.claude.com/docs/en/goal): a small model judges the transcript. It does not run commands.
- **Codex** subagents: TOML in `.codex/agents/` with `model` and a concurrency cap. **opencode**: `.opencode/agents/*.md` with `mode: subagent`. All of them use one vendor per CLI.

UX worth copying: the agent writes the structure at runtime, one-step launch with a preview, roles named once, fan-out as one primitive, results in variables and not in the parent's context, visible cost.

Gaps hex fills: workers from several vendors, a durable journal and crash resume, deterministic gates, real budgets, mid-run `steer`/`pause`/`human`.
