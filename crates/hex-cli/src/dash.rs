//! `hex dash` — a live, full-screen view of all runs; a `top` for hex.
//!
//! The live counterpart of `hex runs`: same rows, same vocabulary, redrawn on a
//! timer until the operator quits. A thin [`Runtime`] client, exactly like
//! `hex runs` — every fact it shows the runtime computed.

use std::io::{IsTerminal, Stdout};
use std::process::ExitCode;
use std::time::Duration;

use anstyle::AnsiColor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use hex_runtime::{Liveness, RunSummary, Runtime};
use ratatui::Frame;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::ui;

/// Run the live dashboard: redraw a fresh `list_runs()` snapshot every
/// `interval_ms`, waking early on any keypress, until the operator quits with
/// `q`, `Esc`, or `Ctrl-C`. The RAII guard restores the terminal on every exit
/// path — the quit keys, an error, or a panic unwind.
pub fn run(runtime: &Runtime, interval_ms: u64, color: ui::When) -> Result<ExitCode, String> {
    // A full-screen TUI needs a terminal; a machine consumer uses `hex runs
    // --json`. Refuse before touching raw mode or the alternate screen, so a
    // pipe never sees escape codes.
    if !std::io::stdout().is_terminal() {
        eprintln!("hex: dash needs a terminal; use `hex runs --json` for machine-readable output");
        return Ok(ExitCode::from(2));
    }
    // Resolve the colour/charset policy *before* entering the alternate screen,
    // while stdout is still a plain terminal — the same policy `hex runs` uses.
    let ui = ui::Ui::stdout(color, false);
    let poll = Duration::from_millis(interval_ms);
    let mut guard = TermGuard::enter()?;
    loop {
        let runs = runtime.list_runs().map_err(|e| e.to_string())?;
        let rows = ordered(&runs);
        guard
            .term
            .draw(|frame| draw(frame, &rows, ui))
            .map_err(|e| e.to_string())?;
        // Block up to one interval for input; a keypress wakes the redraw early,
        // and a quit key ends the loop. A resize or other key just redraws.
        if event::poll(poll).map_err(|e| e.to_string())?
            && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
            && is_quit(key)
        {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The keys that end the dashboard: `q`, `Esc`, or `Ctrl-C`. In raw mode the tty
/// generates no SIGINT, so `Ctrl-C` arrives as a key event, not a signal.
fn is_quit(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

type Term = CrosstermBackend<Stdout>;

/// Owns the full-screen terminal and guarantees teardown: on drop (normal *or*
/// panic-unwind) it clears the frame, leaves the alternate screen, disables raw
/// mode, and re-shows the cursor — so the shell is left exactly as it was found.
struct TermGuard {
    term: ratatui::Terminal<Term>,
}

impl TermGuard {
    /// Enter raw mode + the alternate screen. Any failure restores what it had
    /// already changed, so a half-entered terminal never leaks to the shell.
    fn enter() -> Result<Self, String> {
        enable_raw_mode().map_err(|e| e.to_string())?;
        if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(e.to_string());
        }
        match ratatui::Terminal::new(CrosstermBackend::new(std::io::stdout())) {
            Ok(term) => Ok(Self { term }),
            Err(e) => {
                let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(e.to_string())
            }
        }
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        // Clear first so a frame can't survive a failed screen-leave during an
        // unwind, then leave the alternate screen, restore the cooked terminal,
        // re-show the cursor, and flush — every step best-effort.
        let _ = self.term.clear();
        let _ = execute!(self.term.backend_mut(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = self.term.show_cursor();
        let _ = Backend::flush(self.term.backend_mut());
    }
}

/// Render one frame: a title line (run count + how many are live), then a table
/// mirroring `hex runs` — the same columns, in model order. Every style obeys
/// the resolved `ui` policy (colour off under `--color never`, ASCII glyphs off
/// a UTF-8 locale); the mark carries the colour, the word carries the verdict.
fn draw(frame: &mut Frame, rows: &[&RunSummary], ui: ui::Ui) {
    // "Live" is the beating-process count (`Liveness::Live`); the PROCESS column
    // appears for *any* unfinished run, matching `hex runs`.
    let live = live_count(rows);
    let show_process = rows.iter().any(|r| is_unfinished(r));
    // Bold/dim are colour too: under `--color never` the table is plain text.
    let bold = if ui.colored() {
        Style::new().bold()
    } else {
        Style::new()
    };
    let dim = if ui.colored() {
        Style::new().dim()
    } else {
        Style::new()
    };

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(frame.area());
    let sep = if ui.unicode() { "·" } else { "-" };
    let title = format!("hex dash {sep} {} run(s) {sep} {live} live", rows.len());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(title, bold))),
        chunks[0],
    );

    // No table for an empty journal — a centred hint on how to start one.
    if rows.is_empty() {
        let hint = if ui.unicode() {
            "no runs yet — start one with `hex run <graph> -p \"…\"`"
        } else {
            "no runs yet - start one with `hex run <graph> -p \"...\"`"
        };
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), chunks[1]);
        return;
    }

    let mut headers = vec![
        Cell::from(""),
        Cell::from("RUN"),
        Cell::from("RESULT"),
        Cell::from("AGE"),
        Cell::from("LAST"),
    ];
    let mut widths = vec![
        Constraint::Length(1),
        Constraint::Min(20),
        Constraint::Length(16),
        Constraint::Length(4),
        Constraint::Min(10),
    ];
    if show_process {
        headers.push(Cell::from("PROCESS"));
        widths.push(Constraint::Length(9));
    }
    let header = Row::new(headers).style(bold);

    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            let (glyph, style) = mark_cell(ui, crate::mark_for(r));
            let mut cells = vec![
                Cell::from(Span::styled(glyph, style)),
                Cell::from(r.run_id.clone()).style(bold),
                Cell::from(crate::result_word(r)),
                Cell::from(crate::age(r.updated_at_ms)).style(dim),
                Cell::from(r.current.clone().unwrap_or_else(|| "-".to_owned())),
            ];
            if show_process {
                cells.push(Cell::from(r.liveness.to_string()).style(dim));
            }
            Row::new(cells)
        })
        .collect();

    frame.render_widget(Table::new(body, widths).header(header), chunks[1]);
}

/// Runs whose driver is actually beating — distinct from *unfinished*: a paused
/// or abandoned run is unfinished but not live.
fn live_count(rows: &[&RunSummary]) -> usize {
    rows.iter()
        .filter(|r| matches!(r.liveness, Liveness::Live))
        .count()
}

/// A run the driver has not finished — the ones the PROCESS column is about.
fn is_unfinished(r: &RunSummary) -> bool {
    !matches!(r.liveness, Liveness::Finished)
}

/// A [`ui::Mark`] as a glyph + ratatui style, both derived from the shared
/// `ui` semantics: the glyph from `Mark::glyph` in the stream's charset, the
/// colour from `Mark::hue` and only when the `ui` policy permits colour.
fn mark_cell(ui: ui::Ui, m: ui::Mark) -> (&'static str, Style) {
    let glyph = m.glyph(ui.unicode());
    let style = match (ui.colored(), m.hue()) {
        (true, Some(hue)) => Style::new().fg(hue_to_ratatui(hue)),
        (true, None) => Style::new().dim(),
        (false, _) => Style::new(),
    };
    (glyph, style)
}

/// Map a four-bit ANSI hue (the backend-neutral colour `ui` exposes) to its
/// ratatui equivalent, so `hex dash` and `hex runs` render the same palette.
fn hue_to_ratatui(c: AnsiColor) -> Color {
    match c {
        AnsiColor::Black => Color::Black,
        AnsiColor::Red => Color::Red,
        AnsiColor::Green => Color::Green,
        AnsiColor::Yellow => Color::Yellow,
        AnsiColor::Blue => Color::Blue,
        AnsiColor::Magenta => Color::Magenta,
        AnsiColor::Cyan => Color::Cyan,
        AnsiColor::White => Color::Gray,
        AnsiColor::BrightBlack => Color::DarkGray,
        AnsiColor::BrightRed => Color::LightRed,
        AnsiColor::BrightGreen => Color::LightGreen,
        AnsiColor::BrightYellow => Color::LightYellow,
        AnsiColor::BrightBlue => Color::LightBlue,
        AnsiColor::BrightMagenta => Color::LightMagenta,
        AnsiColor::BrightCyan => Color::LightCyan,
        AnsiColor::BrightWhite => Color::White,
    }
}

/// Order runs for display: not-finished first (a live loop is what the operator
/// is watching), then by most-recent activity. Pure — the row *cells* still
/// come from the shared `cmd_runs` helpers; this only decides order.
pub(crate) fn ordered(runs: &[RunSummary]) -> Vec<&RunSummary> {
    let mut rows: Vec<&RunSummary> = runs.iter().collect();
    let finished = |r: &RunSummary| matches!(r.liveness, Liveness::Finished);
    rows.sort_by(|a, b| {
        finished(a)
            .cmp(&finished(b))
            .then(b.updated_at_ms.cmp(&a.updated_at_ms))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, liveness: Liveness, updated_at_ms: u64) -> RunSummary {
        RunSummary {
            run_id: id.to_owned(),
            status: None,
            current: None,
            attempts: 0,
            disposition: None,
            liveness,
            created_at_ms: 0,
            updated_at_ms,
            error: None,
        }
    }

    #[test]
    fn unfinished_runs_sort_first_then_by_recency() {
        let runs = vec![
            summary("old-finished", Liveness::Finished, 100),
            summary("new-finished", Liveness::Finished, 400),
            summary("old-live", Liveness::Live, 200),
            summary("new-paused", Liveness::Paused, 300),
        ];
        let order: Vec<&str> = ordered(&runs).iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(
            order,
            ["new-paused", "old-live", "new-finished", "old-finished"]
        );
    }

    #[test]
    fn live_counts_only_beating_runs_but_process_shows_for_any_unfinished() {
        let runs = vec![
            summary("beating", Liveness::Live, 4),
            summary("paused", Liveness::Paused, 3),
            summary("abandoned", Liveness::Abandoned, 2),
            summary("done", Liveness::Finished, 1),
        ];
        let rows = ordered(&runs);
        assert_eq!(live_count(&rows), 1, "only the beating run is live");
        assert!(
            rows.iter().any(|r| is_unfinished(r)),
            "paused/abandoned keep the PROCESS column visible"
        );

        // A finished-only set is neither live nor unfinished.
        let done = ordered(std::slice::from_ref(&runs[3]));
        assert_eq!(live_count(&done), 0);
        assert!(!done.iter().any(|r| is_unfinished(r)));
    }

    /// The rendered frame as one flat string, for content assertions.
    fn rendered(rows: &[&RunSummary]) -> String {
        use ratatui::backend::TestBackend;
        let mut term = ratatui::Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|frame| draw(frame, rows, ui::Ui::plain()))
            .unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn an_empty_journal_shows_a_start_hint_not_a_bare_table() {
        let runs: Vec<RunSummary> = Vec::new();
        let text = rendered(&ordered(&runs));
        assert!(text.contains("no runs yet"), "empty-state hint: {text}");
        assert!(
            !text.contains("RESULT"),
            "no table header when empty: {text}"
        );
    }

    #[test]
    fn an_unreadable_run_still_renders_a_row() {
        let mut broken = summary("broken-run", Liveness::Finished, 1);
        broken.status = None; // could not be replayed
        broken.error = Some("graph from an older schema".to_owned());
        let runs = vec![broken];
        let text = rendered(&ordered(&runs));
        assert!(text.contains("broken-run"), "the row is shown: {text}");
        assert!(text.contains("unreadable"), "its verdict is shown: {text}");
    }

    #[test]
    fn quit_keys_are_q_esc_and_ctrl_c_only() {
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        )));
        assert!(is_quit(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        // A plain 'c' or any other key keeps the dashboard open.
        assert!(!is_quit(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
        assert!(!is_quit(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
    }
}
