# hex dash — small live dashboard

Implement `hex dash`: a live, full-screen terminal view of all runs — a `top`
for hex. It is the **live counterpart of `hex runs`**: same rows, same
vocabulary, but redrawn on a timer until the operator quits.

## Design (decided — do not re-litigate)

- **Where it lives:** a new `dash` module in **`hex-cli`** (`crates/hex-cli/src/dash.rs`)
  and a new `Dash` subcommand. It is a thin `Runtime` client, exactly like
  `hex runs`. Do **not** touch the `hex-dashboard` crate — that stub is reserved
  for a future remote/web viewer and `hex-cli` may not depend on it (see the
  dependency table in AGENTS.md: `hex-cli` depends only on `hex-runtime`).
- **Reuse, do not reinvent:** `cmd_runs` in `main.rs` already turns a
  `RunSummary` into a row. Reuse its helpers verbatim — `mark_for`,
  `result_word`, `age`, and the `ui` module — so `hex dash` and `hex runs` can
  never disagree about how a run looks. Make those helpers `pub(crate)` if the
  `dash` module needs them.
- **Data source:** `runtime.list_runs()` → `Vec<RunSummary>` each tick. It is
  cheap (sub-millisecond per run). Do not call `status()` per run in the core
  loop.
- **Terminal handling:** full-screen alternate screen + raw mode, restored on
  drop by an RAII guard — mirror the guard discipline in `preview.rs`
  (`TermGuard`: clear, restore cursor, flush on `Drop`; here also
  `disable_raw_mode` + `LeaveAlternateScreen`). The guard must restore the
  terminal even on a panic unwind.
- **Input:** `q`, `Esc`, or `Ctrl-C` quit. Redraw every `--interval`
  milliseconds (default 1000) and also immediately on a keypress.
- **Colour:** semantic only, via the existing `ui::Mark`/`ui::style` — never
  decorative. The mark carries the colour, the word carries the verdict (same
  rule `cmd_runs` documents).
- **Not a TUI target?** If stderr/stdout is not a terminal, refuse with a clear
  message on stderr and a non-zero exit — a machine consumer uses
  `hex runs --json`. Do not draw escape codes into a pipe.

## Checklist

- [x] **Scaffold the command and module.** Add a `Dash { interval: u64 }`
  variant to the `Command` enum in `crates/hex-cli/src/main.rs` with a doc
  comment ("Live full-screen view of all runs") and an `--interval <MS>` arg
  (`default_value_t = 1000`). Add `mod dash;` and a `crates/hex-cli/src/dash.rs`
  exposing `pub fn run(runtime: &Runtime, interval_ms: u64, color: ...) ->
  Result<ExitCode, String>`; for this item the body may just build the rows once
  (reusing `list_runs()`), print the count, and return `SUCCESS`. Wire
  `Command::Dash { interval } => dash::run(...)` into the dispatch match. It must
  build and `hex dash --help` must work.

- [x] **Enable keyboard events in crossterm.** In the workspace `Cargo.toml`,
  add the `events` feature to the `crossterm` dependency (it is currently
  `default-features = false` for `terminal::size` only). Update the inline
  comment to say why (`dash` needs key input). Confirm the workspace still builds
  and clippy is clean. This is the one dependency change the whole task needs.

- [x] **Build the sorted row model.** In `dash.rs`, add a pure function that
  takes `&[RunSummary]` and returns them ordered for display: not-finished runs
  first (a live loop is what the operator is watching), then by `updated_at_ms`
  descending (most-recent activity on top). Add a focused unit test with a few
  fabricated `RunSummary` values asserting the order. Keep the row *cells*
  produced by the shared `cmd_runs` helpers — this item only decides order.

- [x] **Render the table with ratatui.** In `dash.rs`, add the RAII terminal
  guard (enter alternate screen + raw mode on construction, restore on `Drop`)
  and a `draw` that renders a `ratatui::widgets::Table`: a title line showing the
  run count and how many are live, a header row (`RUN`, `RESULT`, `AGE`, `LAST`,
  and `PROCESS` when any run is unfinished — same columns as `hex runs`), and one
  styled row per run in model order. Draw once and return for now (the loop is
  the next item). Colours come from the shared `ui`/`Mark` semantics.

- [x] **Add the live loop and input handling.** Turn `dash::run` into a loop:
  redraw from a fresh `list_runs()` every `interval_ms`, and quit on `q`, `Esc`,
  or `Ctrl-C` (use `crossterm::event::poll(Duration::from_millis(interval_ms))`
  so a keypress also wakes it). The RAII guard restores the terminal on every
  exit path including the quit keys.

- [x] **Handle the edges.** Refuse to run when stdout is not a terminal
  (`std::io::stdout().is_terminal()` is false): print a one-line message to
  stderr pointing at `hex runs --json` and return a non-zero `ExitCode`, before
  touching the terminal. When `list_runs()` is empty, draw a centred empty-state
  line ("no runs yet — start one with `hex run <graph> -p \"…\"`") instead of a
  bare table. A run whose journal could not be replayed (`error.is_some()`) must
  still appear as a row, never crash the view.

- [x] **Document it.** Add a `hex dash` entry to `README.md` (near the
  `hex runs` / live-preview material) and to `skills/hex/SKILL.md`'s command
  table. If any sharp edge turned up while implementing (a crossterm/ratatui
  gotcha, a terminal-restore subtlety), add a numbered gotcha to `AGENTS.md`.
  Keep the prose in Simplified Technical English.

- [x] **Reject `hex dash --json`.** The global `--json` flag promises
  machine-readable output, but `dash` has no machine mode, so today it silently
  opens the TUI in a PTY. Refuse the combination in `dash::run` (or dispatch)
  before opening the runtime or touching the terminal: print a one-line stderr
  message pointing at `hex runs --json` and return a non-zero `ExitCode`. Update
  the global `--json` help text so the claim is no longer false, and add a CLI
  regression test (`tests/cli.rs`) asserting the refusal.

- [x] **Reject `--interval 0`.** A zero interval makes the poll return
  immediately, so the loop re-folds every journal with no wait — busy-spinning
  CPU and disk until quit. Require a nonzero value via a clap range
  (`value_parser = clap::value_parser!(u64).range(1..)`) on the `interval` arg,
  and add a CLI test asserting `hex dash --interval 0` is a usage error (exit 2).

- [x] **Record it in `TODO.md`.** Add `hex dash` to the Phase 3 CLI UX ledger,
  so the shipped command is reflected there as project instructions require.

## Definition of done

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero
warnings) and `cargo test --workspace` all pass. Running `hex dash` in a
terminal shows a live, self-refreshing table that quits cleanly on `q` and
leaves the terminal exactly as it was found.
