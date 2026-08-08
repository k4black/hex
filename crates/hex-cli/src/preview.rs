//! Live run preview: a sticky footer (ratatui inline viewport) that tails the
//! in-flight attempt's output while the driver blocks, with a status line
//! (node · worker · attempt N/budget · spinner elapsed · countdown).
//!
//! Rendering runs on a background thread so the driver/worker are never touched
//! — the preview is a pure consumer of the `attempts/<id>/{stdout,stderr}.log`
//! files the worker already writes. On a non-TTY (or with `--no-preview` /
//! `--json`) it degrades to the plain line-streaming used before this feature.
//!
//! Robustness: the render thread's terminal is wrapped in an RAII guard that
//! clears the viewport and restores the cursor on any exit (normal, `Stop`, or
//! an unwinding panic); a dropped `LivePreview` stops and joins any live thread;
//! and event lines fall back to a plain write whenever the pane is absent or its
//! channel has closed, so a line is never silently lost.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hex_runtime::{AttemptView, Event, NodeKind, ProgressSink};
use ratatui::Frame;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Position;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_width::UnicodeWidthStr;

/// Lines of agent output shown in the pane.
const TAIL_LINES: usize = 8;
/// Repaint cadence (and the timer/spinner resolution).
const TICK: Duration = Duration::from_millis(100);
/// Cap on a single not-yet-newline-terminated line, so a newline-less flood
/// (a long token stream, a huge JSON blob) can never grow `pending` unbounded.
const MAX_PENDING: usize = 8 * 1024;
/// Cap on a stored/rendered line's bytes — a single enormous newline-terminated
/// line is truncated (with `…`) rather than retained whole in the ring.
const MAX_LINE: usize = 2 * 1024;
/// Cap on bytes read from one source per poll, so a large delta between polls
/// can't allocate an unbounded transient buffer; the rest is read next tick.
const MAX_READ: u64 = 64 * 1024;
/// Braille spinner frames.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A [`ProgressSink`] that renders a live footer while an attempt runs, and
/// falls back to plain line streaming when disabled or between attempts.
pub struct LivePreview {
    enabled: bool,
    active: Mutex<Option<Active>>,
}

/// The render thread for the currently in-flight attempt.
struct Active {
    tx: Sender<Cmd>,
    handle: JoinHandle<()>,
}

enum Cmd {
    /// A completed event line to print *above* the footer.
    Line(String),
    /// Tear the footer down and exit the render thread.
    Stop,
}

impl LivePreview {
    /// `enabled` must be true only for an interactive TTY run (not `--json`,
    /// `--no-preview`, or a pipe); otherwise this streams plain lines.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            active: Mutex::new(None),
        }
    }

    /// Print a line plainly to stderr (the pre-feature behavior). Best-effort:
    /// a broken pipe (`hex run | head`) must not panic and abort the run.
    fn plain(line: &str) {
        let _ = writeln!(std::io::stderr(), "  {line}");
    }

    /// Print an out-of-band line (not a journal event) above the footer, or
    /// plainly when none is live. The interrupt banner comes through here: a
    /// raw stderr write from the signal thread would interleave with the
    /// render thread's cursor movements and desync the inline viewport's
    /// position bookkeeping for the rest of the run.
    pub fn notice(&self, line: &str) {
        let guard = self.active.lock().expect("preview lock");
        if let Some(a) = guard.as_ref()
            && a.tx.send(Cmd::Line(line.to_owned())).is_ok()
        {
            return;
        }
        drop(guard);
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

/// The preview shared between the runtime (which owns its sink as a `Box`) and
/// the interrupt handler (which must print through the same footer channel):
/// both hold one `Arc`, and this newtype gives the `Box` side its
/// [`ProgressSink`].
pub struct SharedPreview(pub std::sync::Arc<LivePreview>);

impl ProgressSink for SharedPreview {
    fn event(&self, e: &Event) {
        self.0.event(e);
    }
    fn attempt_started(&self, v: &AttemptView) {
        self.0.attempt_started(v);
    }
    fn attempt_finished(&self) {
        self.0.attempt_finished();
    }
}

impl ProgressSink for LivePreview {
    fn event(&self, e: &Event) {
        let line = crate::event_line(e);
        // While a footer owns stderr, route the line through it so it prints
        // above the pane. If there's no footer, or its thread has gone, fall
        // back to a plain write — a line is never dropped.
        let guard = self.active.lock().expect("preview lock");
        if let Some(a) = guard.as_ref()
            && a.tx.send(Cmd::Line(line.clone())).is_ok()
        {
            return;
        }
        drop(guard);
        Self::plain(&line);
    }

    fn attempt_started(&self, v: &AttemptView) {
        if !self.enabled {
            return; // plain mode: never show a footer
        }
        let (tx, rx) = mpsc::channel();
        let view = OwnedView::from(v);
        // Fallible spawn: if the OS can't give us a thread, stay in plain mode
        // rather than panicking.
        match std::thread::Builder::new()
            .name("hex-preview".to_owned())
            .spawn(move || render_loop(&view, &rx))
        {
            Ok(handle) => *self.active.lock().expect("preview lock") = Some(Active { tx, handle }),
            Err(_) => { /* no footer; event() falls back to plain */ }
        }
    }

    fn attempt_finished(&self) {
        let taken = self.active.lock().expect("preview lock").take();
        if let Some(a) = taken {
            let _ = a.tx.send(Cmd::Stop);
            // A panic in the render thread still unwinds through TermGuard::drop,
            // which restores the terminal, so a failed join needs no extra work.
            let _ = a.handle.join();
        }
    }
}

impl Drop for LivePreview {
    /// If a run unwinds (panic) before `attempt_finished`, still stop and join
    /// the render thread so its guard restores the terminal.
    fn drop(&mut self) {
        if let Some(a) = self.active.get_mut().ok().and_then(Option::take) {
            let _ = a.tx.send(Cmd::Stop);
            let _ = a.handle.join();
        }
    }
}

/// The subset of [`AttemptView`] the render thread needs, owned so it can move
/// across the thread boundary. Elapsed time is measured monotonically from the
/// thread's own start rather than the view's wall-clock stamp.
struct OwnedView {
    progress: Vec<hex_runtime::NodeProgress>,
    node_id: String,
    kind: NodeKind,
    worker: Option<String>,
    attempt_number: u32,
    attempts_budget: Option<u32>,
    deadline_ms: Option<u64>,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
}

impl From<&AttemptView> for OwnedView {
    fn from(v: &AttemptView) -> Self {
        Self {
            progress: v.progress.clone(),
            node_id: v.node_id.clone(),
            kind: v.kind,
            worker: v.worker.clone(),
            attempt_number: v.attempt_number,
            attempts_budget: v.attempts_budget,
            deadline_ms: v.deadline_ms,
            stdout_log: v.stdout_log.clone(),
            stderr_log: v.stderr_log.clone(),
        }
    }
}

type Term = CrosstermBackend<std::io::Stderr>;

/// Owns the inline terminal and guarantees teardown: on drop (normal *or*
/// panic-unwind) it clears the footer, parks the cursor at the viewport origin,
/// and re-shows it — so the shell never inherits a hidden/misplaced cursor and
/// later output doesn't begin inside the former footer.
struct TermGuard {
    term: Terminal<Term>,
    /// The viewport's top-left from the last `draw` (it moves as lines scroll
    /// above it). `None` until the first frame is drawn.
    origin: Option<Position>,
    /// The viewport width from the last `draw`, for wrapping inserted lines.
    width: u16,
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        // `clear` erases the footer but restores the pre-clear cursor position;
        // move to the viewport origin so subsequent output starts cleanly there.
        let _ = self.term.clear();
        if let Some(pos) = self.origin {
            let _ = self.term.set_cursor_position(pos);
        }
        // `draw` hides the cursor each frame — make it visible again.
        let _ = self.term.show_cursor();
        let _ = Backend::flush(self.term.backend_mut());
    }
}

/// Tail the logs and repaint until told to stop. If the inline terminal can't
/// be set up, fall back to printing forwarded lines plainly so no event line is
/// ever dropped.
fn render_loop(view: &OwnedView, rx: &Receiver<Cmd>) {
    let height = u16::try_from(TAIL_LINES).unwrap_or(8) + 2; // + top/bottom border
    let backend = CrosstermBackend::new(std::io::stderr());
    let term = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    );
    let Ok(term) = term else {
        drain_plain(rx);
        return;
    };
    let mut guard = TermGuard {
        term,
        origin: None,
        width: 80,
    };

    let mut tail = Tail::new(
        vec![
            (view.stdout_log.clone(), false),
            (view.stderr_log.clone(), true),
        ],
        TAIL_LINES,
    );
    let start = Instant::now();
    let mut ticks = 0usize;

    loop {
        match rx.recv_timeout(TICK) {
            Ok(Cmd::Line(line)) => {
                // Reserve as many rows as the (wrapped) line needs, so a message
                // wider than the terminal isn't clipped to a single row.
                let para =
                    Paragraph::new(Line::raw(format!("  {line}"))).wrap(Wrap { trim: false });
                let rows = para.line_count(guard.width);
                let height = u16::try_from(rows).unwrap_or(1).max(1);
                let _ = guard
                    .term
                    .insert_before(height, |buf| para.render(buf.area, buf));
            }
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        tail.poll();
        ticks = ticks.wrapping_add(1);
        let elapsed = start.elapsed();
        // Capture the viewport origin *inside* the closure: `Frame::area()` is
        // the inline viewport rect (it scrolls as lines insert above), whereas
        // `CompletedFrame::area` is the whole terminal. Set before rendering so
        // it's recorded even if rendering panics.
        let mut origin = None;
        let mut width = guard.width;
        let _ = guard.term.draw(|frame| {
            let area = frame.area();
            origin = Some(area.as_position());
            width = area.width;
            render_footer(frame, view, &tail, elapsed, ticks);
        });
        if origin.is_some() {
            guard.origin = origin;
        }
        guard.width = width;
    }
    // `guard` drops here → clears the footer, parks + restores the cursor.
}

/// Fallback when no inline terminal is available: just echo forwarded lines.
fn drain_plain(rx: &Receiver<Cmd>) {
    while let Ok(Cmd::Line(line)) = rx.recv() {
        LivePreview::plain(&line);
    }
}

/// Draw the bordered pane: the status line as the block title, the newest tail
/// lines inside (stderr dimmed), clipped to the pane height.
fn render_footer(
    frame: &mut Frame,
    view: &OwnedView,
    tail: &Tail,
    elapsed: Duration,
    ticks: usize,
) {
    // The graph strip rides the bottom border: it is persistent context, so it
    // must not eat a line of the tail, which is the part that changes.
    let block = Block::bordered()
        .title(status_line(view, elapsed, ticks))
        .title_bottom(progress_line(view, frame.area().width))
        .dim();
    let inner = block.inner(frame.area());
    frame.render_widget(&block, frame.area());

    let all = tail.display();
    let start = all.len().saturating_sub(inner.height as usize);
    let lines: Vec<Line> = all[start..]
        .iter()
        .map(|(text, is_err)| {
            let line = Line::raw(text.clone());
            if *is_err { line.dim() } else { line }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The one-line status: `node · worker · attempt N/budget · ⣟ M:SS · M:SS left`.
fn status_line(view: &OwnedView, elapsed: Duration, ticks: usize) -> String {
    let spin = SPINNER[ticks % SPINNER.len()];
    // Agent nodes show their worker; a gate/command shows its kind, not "gate".
    let actor = view.worker.as_deref().unwrap_or_else(|| view.kind.as_str());
    let mut s = format!(
        " {} · {} · attempt {}",
        view.node_id, actor, view.attempt_number
    );
    if let Some(budget) = view.attempts_budget {
        s.push_str(&format!("/{budget}"));
    }
    s.push_str(&format!(" · {spin} {}", fmt_mmss(elapsed)));
    if let Some(deadline_ms) = view.deadline_ms {
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let left = deadline_ms.saturating_sub(elapsed_ms);
        s.push_str(&format!(
            " · {} left",
            fmt_mmss(Duration::from_millis(left))
        ));
    }
    s.push(' ');
    s
}

/// The graph as one line, with each node marked by where it stands.
///
/// `attempt 7 on implement` cannot tell you whether a loop is advancing or
/// circling; the shape can. A visited node carries its round count, because
/// that is the number a bound is about to stop — and because the count is what
/// separates *visited* from *pending* in text, so the distinction survives a
/// terminal that renders no dim.
///
/// No colour and no `✓`: a node the run has been through is not a node that
/// succeeded (the strip above is drawn while a failing gate loops), so the only
/// hierarchy here is weight — bold for where the run is, dim for where it has
/// not been.
fn progress_line(view: &OwnedView, width: u16) -> Line<'static> {
    use hex_runtime::NodeState;
    let cells: Vec<(String, Style)> = view
        .progress
        .iter()
        .map(|n| {
            let (prefix, style) = match n.state {
                NodeState::Active => ("▸ ", Style::new().bold().not_dim()),
                NodeState::Visited => ("", Style::new().not_dim()),
                NodeState::Pending => ("", Style::new()),
            };
            let count = if n.state == NodeState::Pending {
                String::new()
            } else {
                format!(" ×{}", n.visits)
            };
            (format!("{prefix}{}{count}", n.id), style)
        })
        .collect();

    // Two border corners plus a space of padding at each end.
    let budget = usize::from(width).saturating_sub(4);
    let (from, elided) = fits_from(&cells, active_index(&view.progress), budget);

    let mut spans = vec![Span::raw(" ")];
    if elided {
        spans.push(Span::styled("… ", Style::new()));
    }
    for (i, (text, style)) in cells.iter().enumerate().skip(from) {
        if i > from {
            spans.push(Span::raw(" · "));
        }
        spans.push(Span::styled(text.clone(), *style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Index of the node the run is on, or 0 when none is (every node pending).
fn active_index(progress: &[hex_runtime::NodeProgress]) -> usize {
    progress
        .iter()
        .position(|n| n.state == hex_runtime::NodeState::Active)
        .unwrap_or(0)
}

/// The earliest node the strip can start at and still show `active` within
/// `budget` cells, plus whether anything was dropped off the front.
///
/// Nodes are dropped from the *front* because the strip's job is where the run
/// is and what is left; a graph long enough to overflow has already-visited
/// history that the journal keeps anyway.
fn fits_from(cells: &[(String, Style)], active: usize, budget: usize) -> (usize, bool) {
    let sep = 3; // " · "
    let width = |s: &str| UnicodeWidthStr::width(s);
    let total: usize =
        cells.iter().map(|(t, _)| width(t)).sum::<usize>() + sep * cells.len().saturating_sub(1);
    if total <= budget {
        return (0, false);
    }
    // Walk the start forward until the remainder fits; "… " costs 2.
    for from in 1..cells.len() {
        let kept = &cells[from..];
        let w: usize = kept.iter().map(|(t, _)| width(t)).sum::<usize>()
            + sep * kept.len().saturating_sub(1)
            + 2;
        if w <= budget && from <= active {
            return (from, true);
        }
    }
    (active, true)
}

/// Format a duration as `M:SS` (minutes may exceed 59).
fn fmt_mmss(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// A rolling tail of a run's log files. It keeps the last `cap` *complete* lines
/// in a ring and, per source, the current unterminated line (`pending`) which is
/// shown as a provisional trailing line — so a live token stream is visible
/// immediately, not withheld until its newline.
///
/// The two files (stdout/stderr) carry no cross-stream ordering metadata, so
/// interleaving is best-effort: lines within one stream are in exact order, but
/// across streams they appear in the tail's own poll order (stdout before
/// stderr within a tick) — unspecified relative to real time. stderr is dimmed
/// to keep the two distinguishable. Each attempt's logs are created once at
/// spawn and only appended, so a source only grows; the shrink check below is a
/// cheap defense, not general log-rotation support.
struct Tail {
    sources: Vec<Source>,
    ring: VecDeque<(String, bool)>, // (line, is_stderr)
    cap: usize,
}

struct Source {
    path: PathBuf,
    is_err: bool,
    offset: u64,
    pending: Vec<u8>,
}

impl Tail {
    fn new(paths: Vec<(PathBuf, bool)>, cap: usize) -> Self {
        let sources = paths
            .into_iter()
            .map(|(path, is_err)| Source {
                path,
                is_err,
                offset: 0,
                pending: Vec::new(),
            })
            .collect();
        Self {
            sources,
            ring: VecDeque::new(),
            cap,
        }
    }

    /// Read bytes appended since the last poll, push newly *completed* lines onto
    /// the ring, and keep the trailing partial in `pending`. Byte-based so a
    /// multi-byte char split across two reads never panics.
    fn poll(&mut self) {
        for src in &mut self.sources {
            let Ok(mut f) = File::open(&src.path) else {
                continue;
            };
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if len < src.offset {
                // The file shrank — it can't have been appended-to. Restart.
                src.offset = 0;
                src.pending.clear();
            }
            if len <= src.offset {
                continue;
            }
            // If we've fallen more than a read-window behind, jump to the newest
            // window: a preview shows the *tail*, so skip stale middle rather
            // than reading the oldest bytes and lagging further each tick. The
            // stale partial is dropped (a fresh partial starts after the skip).
            if len - src.offset > MAX_READ {
                src.offset = len - MAX_READ;
                src.pending.clear();
            }
            if f.seek(SeekFrom::Start(src.offset)).is_err() {
                continue;
            }
            let want = len - src.offset; // now <= MAX_READ
            let mut buf = Vec::new();
            if f.take(want).read_to_end(&mut buf).is_err() {
                continue;
            }
            src.offset += u64::try_from(buf.len()).unwrap_or(0);
            src.pending.extend_from_slice(&buf);
            while let Some(nl) = src.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = src.pending.drain(..=nl).collect();
                let text = String::from_utf8_lossy(&line)
                    .trim_end_matches(['\n', '\r'])
                    .to_string();
                self.ring.push_back((cap_line(text), src.is_err));
                while self.ring.len() > self.cap {
                    self.ring.pop_front();
                }
            }
            // Bound a newline-less partial: keep only its last MAX_PENDING bytes.
            if src.pending.len() > MAX_PENDING {
                let drop = src.pending.len() - MAX_PENDING;
                src.pending.drain(..drop);
            }
        }
    }

    /// The complete lines currently in the ring (no partials). Test-only: the
    /// renderer uses [`Self::display`], which also includes the live partial.
    #[cfg(test)]
    fn lines(&self) -> impl Iterator<Item = (&str, bool)> {
        self.ring.iter().map(|(t, e)| (t.as_str(), *e))
    }

    /// The lines to render: the ring plus each source's in-progress partial
    /// line, so streaming output is visible before its newline arrives.
    fn display(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = self.ring.iter().map(|(t, e)| (t.clone(), *e)).collect();
        for src in &self.sources {
            if !src.pending.is_empty() {
                let text = String::from_utf8_lossy(&src.pending)
                    .trim_end_matches(['\n', '\r'])
                    .to_string();
                if !text.is_empty() {
                    out.push((cap_line(text), src.is_err));
                }
            }
        }
        out
    }
}

/// Truncate a line to [`MAX_LINE`] bytes (at a char boundary) with a `…` marker,
/// so one enormous line can't sit in the ring — or be cloned each render tick —
/// unbounded. The pane only shows a terminal-width slice anyway.
fn cap_line(mut s: String) -> String {
    if s.len() > MAX_LINE {
        let mut end = MAX_LINE;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_support::unique;

    fn view(worker: Option<&str>, kind: NodeKind, deadline_ms: Option<u64>) -> OwnedView {
        OwnedView {
            progress: Vec::new(),
            node_id: "build".to_owned(),
            kind,
            worker: worker.map(str::to_owned),
            attempt_number: 2,
            attempts_budget: Some(8),
            deadline_ms,
            stdout_log: PathBuf::new(),
            stderr_log: PathBuf::new(),
        }
    }

    #[test]
    fn fmt_mmss_pads_and_rolls_over_minutes() {
        assert_eq!(fmt_mmss(Duration::from_secs(0)), "0:00");
        assert_eq!(fmt_mmss(Duration::from_secs(47)), "0:47");
        assert_eq!(fmt_mmss(Duration::from_secs(90)), "1:30");
        assert_eq!(fmt_mmss(Duration::from_secs(3600)), "60:00");
    }

    fn progress(nodes: &[(&str, hex_runtime::NodeState, u32)]) -> OwnedView {
        let mut v = view(Some("codex"), NodeKind::Agent, None);
        v.progress = nodes
            .iter()
            .map(|(id, state, visits)| hex_runtime::NodeProgress {
                id: (*id).to_owned(),
                state: *state,
                visits: *visits,
            })
            .collect();
        v
    }

    /// The whole point: whether the loop is advancing or circling, at a glance.
    /// The strip's text with styling dropped, for assertions.
    fn flat(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_strip_marks_where_every_node_stands() {
        use hex_runtime::NodeState::{Active, Pending, Visited};
        let line = progress_line(
            &progress(&[
                ("implement", Visited, 3),
                ("review", Active, 2),
                ("done", Pending, 0),
            ]),
            80,
        );
        assert_eq!(flat(&line), " implement ×3 · ▸ review ×2 · done ");
    }

    /// A visited node must not wear a `✓`: the strip is drawn while a failing
    /// gate loops, and a tick there reads as a verdict the run never reached.
    #[test]
    fn a_visited_node_is_not_marked_as_passed() {
        use hex_runtime::NodeState::Visited;
        let line = progress_line(&progress(&[("check", Visited, 1)]), 80);
        assert!(!flat(&line).contains('✓'), "got: {}", flat(&line));
        // ×1 is what separates visited from pending without relying on dim.
        assert_eq!(flat(&line), " check ×1 ");
    }

    /// The strip must never outgrow the border it rides on, and must keep the
    /// active node visible when it drops nodes to fit.
    #[test]
    fn a_long_strip_elides_from_the_front_and_keeps_the_active_node() {
        use hex_runtime::NodeState::{Active, Pending, Visited};
        let line = progress_line(
            &progress(&[
                ("gather-requirements", Visited, 1),
                ("draft-the-plan", Visited, 1),
                ("implement", Active, 2),
                ("verify", Pending, 0),
            ]),
            40,
        );
        let flat = flat(&line);
        assert!(flat.starts_with(" … "), "elision marker: {flat}");
        assert!(flat.contains("▸ implement ×2"), "active kept: {flat}");
        assert!(
            UnicodeWidthStr::width(flat.as_str()) <= 40 - 2,
            "fits inside the border: {flat}"
        );
    }

    #[test]
    fn status_line_shows_node_worker_attempt_timer_and_countdown() {
        let s = status_line(
            &view(Some("codex"), NodeKind::Agent, Some(300_000)),
            Duration::from_secs(47),
            0,
        );
        assert!(s.contains("build · codex · attempt 2/8"), "got: {s}");
        assert!(s.contains("0:47"), "elapsed timer: {s}");
        // 300_000ms budget − 47_000ms elapsed = 253s = 4:13.
        assert!(s.contains("4:13 left"), "countdown: {s}");
    }

    #[test]
    fn status_line_labels_a_workerless_node_by_its_kind() {
        // A command node has no worker, so the status line shows its kind there.
        let c = status_line(
            &view(None, NodeKind::Command, None),
            Duration::from_secs(5),
            0,
        );
        assert!(c.contains("build · command · attempt 2/8"), "got: {c}");
        assert!(!c.contains("left"), "no countdown without a deadline: {c}");
    }

    /// Append `bytes` to `path`, like an agent's stdout growing.
    fn append(path: &std::path::Path, bytes: &[u8]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(bytes).unwrap();
    }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hex-tail-{tag}-{}-{}",
            std::process::id(),
            unique()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("log")
    }

    #[test]
    fn tail_ring_holds_complete_lines_across_both_streams() {
        let out = tmp("merge-out");
        let err = tmp("merge-err");
        let mut tail = Tail::new(vec![(out.clone(), false), (err.clone(), true)], 8);

        append(&out, b"reading\nediting\n");
        append(&err, b"warning: slow\n");
        // A partial line (no newline) is NOT a complete ring line yet.
        append(&out, b"compil");
        tail.poll();
        let ring: Vec<_> = tail.lines().collect();
        assert_eq!(
            ring,
            vec![
                ("reading", false),
                ("editing", false),
                ("warning: slow", true)
            ],
        );

        // Completing the partial moves it into the ring.
        append(&out, b"ing\n");
        tail.poll();
        assert_eq!(tail.lines().last(), Some(("compiling", false)));
    }

    #[test]
    fn tail_display_shows_the_in_progress_partial_line() {
        let out = tmp("partial");
        let mut tail = Tail::new(vec![(out.clone(), false)], 8);
        // A live token stream with no newline yet must still be visible.
        append(&out, b"thinking");
        tail.poll();
        assert_eq!(tail.lines().count(), 0, "no complete line in the ring");
        assert_eq!(tail.display(), vec![("thinking".to_owned(), false)]);
    }

    #[test]
    fn tail_bounds_a_newlineless_flood() {
        let out = tmp("flood");
        let mut tail = Tail::new(vec![(out.clone(), false)], 8);
        append(&out, &vec![b'x'; 20_000]); // 20 KiB, no newline
        tail.poll();
        let partial = &tail.display()[0].0;
        assert!(
            partial.len() <= MAX_PENDING,
            "pending is bounded: {}",
            partial.len()
        );
    }

    #[test]
    fn tail_caps_an_enormous_complete_line() {
        let out = tmp("bigline");
        let mut tail = Tail::new(vec![(out.clone(), false)], 8);
        // A newline-terminated 50 KiB line must not be retained whole.
        let mut big = vec![b'x'; 50_000];
        big.push(b'\n');
        append(&out, &big);
        tail.poll();
        let (text, _) = tail.lines().last().unwrap();
        assert!(
            text.len() <= MAX_LINE + '…'.len_utf8(),
            "line capped: {}",
            text.len()
        );
        assert!(text.ends_with('…'), "truncation marker present");
    }

    #[test]
    fn tail_jumps_to_the_newest_window_when_more_than_a_read_behind() {
        let out = tmp("behind");
        let mut tail = Tail::new(vec![(out.clone(), false)], 8);
        // A backlog larger than MAX_READ, ending in a sentinel line. One poll
        // must reach the newest output rather than reading a stale oldest chunk.
        let mut flood = vec![b'x'; MAX_READ as usize + 50_000];
        flood.extend_from_slice(b"\nSENTINEL\n");
        append(&out, &flood);
        tail.poll();
        assert!(
            tail.lines().any(|(t, _)| t == "SENTINEL"),
            "the newest line is visible after a single poll",
        );
    }

    #[test]
    fn tail_keeps_only_the_last_cap_lines() {
        let out = tmp("cap");
        let mut tail = Tail::new(vec![(out.clone(), false)], 2);
        append(&out, b"a\nb\nc\nd\n");
        tail.poll();
        let seen: Vec<_> = tail.lines().collect();
        assert_eq!(seen, vec![("c", false), ("d", false)]);
    }

    #[test]
    fn tail_survives_a_multibyte_char_split_across_reads() {
        let out = tmp("utf8");
        let mut tail = Tail::new(vec![(out.clone(), false)], 8);
        // 'é' is 0xC3 0xA9; deliver the two bytes in separate polls.
        append(&out, &[0xC3]);
        tail.poll();
        assert_eq!(tail.lines().count(), 0, "no complete line yet");
        append(&out, &[0xA9, b'\n']);
        tail.poll();
        assert_eq!(tail.lines().last(), Some(("é", false)));
    }

    #[test]
    fn tail_restarts_if_a_file_unexpectedly_shrinks() {
        let out = tmp("trunc");
        let mut tail = Tail::new(vec![(out.clone(), false)], 8);
        append(&out, b"old line\n");
        tail.poll();
        // Rewrite shorter than the current offset (shouldn't happen for real
        // append-only logs, but the guard must not read stale bytes).
        std::fs::write(&out, b"fresh\n").unwrap();
        tail.poll();
        assert_eq!(tail.lines().last(), Some(("fresh", false)));
    }
}
