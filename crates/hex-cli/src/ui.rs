//! The CLI's only styling surface: colour policy, glyphs, and one table.
//!
//! `main.rs` decides *what* to print; this decides *how*. Nothing here knows a
//! kernel type. It exists because the same three questions — may I colour this,
//! may I use a box-drawing character, how wide is the terminal — were previously
//! answered ad hoc at eighty-odd call sites, mostly by not asking.
//!
//! **Colour and charset are separate axes.** Colour is noise in a file, so it
//! follows TTY-ness. Charset is not: `hex graph x > design.md` in a UTF-8 shell
//! should keep its glyphs, because the same person reads the file in the same
//! terminal. Conflating them is why so many tools emit ASCII into a pipe that
//! could have rendered arrows.

use anstyle::{AnsiColor, Style};
use unicode_width::UnicodeWidthStr;

/// `--color`, in the spelling cargo, ripgrep and grep all established.
#[derive(Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum When {
    /// Colour when the stream is a terminal that wants it.
    #[default]
    Auto,
    /// Always, even into a pipe.
    Always,
    /// Never.
    Never,
}

/// What one output stream may do. `Copy`, computed once in `main`.
///
/// Once, deliberately: re-reading the environment or re-measuring the terminal
/// mid-render lets a table's header disagree with its own rows.
#[derive(Clone, Copy)]
pub struct Ui {
    color: bool,
    unicode: bool,
    width: Option<usize>,
}

impl Ui {
    /// Resolve for stdout.
    #[must_use]
    pub fn stdout(color: When, json: bool) -> Self {
        Self {
            color: paint(
                color,
                json,
                std::io::IsTerminal::is_terminal(&std::io::stdout()),
            ),
            unicode: unicode_ok(),
            width: width(),
        }
    }

    /// Resolve for stderr. Its own answer: piping stdout to another program is
    /// no reason to strip colour from a diagnostic the human still sees.
    #[must_use]
    pub fn stderr(color: When, json: bool) -> Self {
        Self {
            color: paint(
                color,
                json,
                std::io::IsTerminal::is_terminal(&std::io::stderr()),
            ),
            unicode: unicode_ok(),
            width: width(),
        }
    }

    /// A fixed policy for tests — no environment, no ioctl. Env mutation races
    /// across `cargo test` threads, so tests must never depend on it.
    #[cfg(test)]
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            color: false,
            unicode: true,
            width: None,
        }
    }

    /// Styled, at a fixed width, for tests.
    #[cfg(test)]
    #[must_use]
    pub const fn styled(width: usize) -> Self {
        Self {
            color: true,
            unicode: true,
            width: Some(width),
        }
    }

    /// Whether box-drawing and arrows are safe here.
    #[must_use]
    pub const fn unicode(self) -> bool {
        self.unicode
    }

    /// Terminal width, when one is knowable. `None` means never truncate.
    #[must_use]
    pub const fn width(self) -> Option<usize> {
        self.width
    }

    /// Wrap `s` in `style`, or return it untouched when colour is off.
    ///
    /// Returns `impl Display` so an uncoloured run allocates nothing — the old
    /// `grey()` built a `String` per line and was called once per line of a
    /// 5000-character agent message.
    pub fn paint(self, style: Style, s: &str) -> impl std::fmt::Display {
        Painted {
            style: if self.color { style } else { Style::new() },
            body: s,
        }
    }

    /// The symbol set this stream may draw with.
    #[must_use]
    pub const fn glyphs(self) -> Glyphs {
        if self.unicode {
            Glyphs::UNICODE
        } else {
            Glyphs::ASCII
        }
    }

    /// Paint `s` and pad it to `width` *visible* cells.
    ///
    /// `format!("{:<10}", painted)` pads by byte length, and a painted string
    /// carries ~9 bytes of escape, so every label collided with its value the
    /// moment colour was switched on.
    #[must_use]
    pub fn field(self, style: Style, s: &str, width: usize) -> String {
        format!(
            "{}{}",
            self.paint(style, s),
            " ".repeat(width.saturating_sub(cells(s)))
        )
    }

    /// A status glyph, coloured to match its meaning.
    #[must_use]
    pub fn mark(self, m: Mark) -> String {
        let (uni, ascii, style) = match m {
            Mark::Ok => ("✓", "+", style::OK),
            Mark::Fail => ("✗", "x", style::FAIL),
            Mark::Warn => ("!", "!", style::WARN),
            Mark::Running => ("▸", ">", style::RUN),
            Mark::Idle => ("·", ".", style::DIM),
        };
        self.paint(style, if self.unicode { uni } else { ascii })
            .to_string()
    }
}

/// The drawing vocabulary. Two instances exist; renderers name fields, never
/// literals, so adding a charset never means hunting for stray box characters.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    /// Marks the entry node.
    pub entry: &'static str,
    /// The gutter running down the happy path.
    pub rail: &'static str,
    /// The gutter's last row.
    pub rail_end: &'static str,
    /// An `agent` node.
    pub agent: &'static str,
    /// A `command` node.
    pub command: &'static str,
    /// A `command` node whose signal is named in `accept.require` — a gate.
    pub gate: &'static str,
    /// A `human` node.
    pub human: &'static str,
    /// A `terminal: succeeded`.
    pub ok: &'static str,
    /// A `terminal: failed` (or any non-success disposition).
    pub fail: &'static str,
    /// A transition that moves the run forward.
    pub forward: &'static str,
    /// A transition that closes a loop.
    pub back: &'static str,
    /// The implicit `accept.on_unmet` reroute — dotted, because no `Edge`
    /// describes it and nothing in the YAML says it exists.
    pub reroute: &'static str,
    /// Separates facts on a metadata line.
    pub sep: &'static str,
    /// Joins nodes when printing a cycle path.
    pub step: &'static str,
    /// "at most", for visit bounds.
    pub le: &'static str,
    /// Binds a role to the worker it resolves to.
    pub binds: &'static str,
}

impl Glyphs {
    const UNICODE: Self = Self {
        entry: "▶",
        rail: "│",
        rail_end: "└",
        agent: "◆",
        command: "□",
        gate: "▣",
        human: "?",
        ok: "✓",
        fail: "✗",
        forward: "──▶",
        back: "──↺",
        reroute: "┈┈↺",
        sep: "·",
        step: "▸",
        le: "≤",
        binds: "→",
    };

    const ASCII: Self = Self {
        entry: ">",
        rail: "|",
        rail_end: "`",
        agent: "A",
        command: "C",
        gate: "G",
        human: "?",
        ok: "+",
        fail: "x",
        forward: "-->",
        back: "--^",
        reroute: "..^",
        sep: ",",
        step: ">",
        le: "<=",
        binds: "->",
    };

    /// The one-line legend. Printed every time, like terraform reprints its
    /// `+ ~ -` key: a symbol vocabulary nobody can look up is a puzzle.
    #[must_use]
    pub fn legend(&self) -> String {
        format!(
            "{} agent  {} command  {} gate  {} human  {} succeeded  {} failed  \
             {} back edge  {} implicit",
            self.agent,
            self.command,
            self.gate,
            self.human,
            self.ok,
            self.fail,
            self.back,
            self.reroute,
        )
    }
}

/// A glyph and its colour chosen *together*. A green `✓` beside the word
/// `failed` is the mistake a shared enum makes unrepresentable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Succeeded, passed, present.
    Ok,
    /// Failed, missing.
    Fail,
    /// Timed out, exhausted, unreadable — went wrong without being a plain no.
    Warn,
    /// Live, in flight.
    Running,
    /// Paused, queued, cancelled.
    Idle,
}

/// The palette. Four-bit ANSI only, so it inherits the user's theme instead of
/// fighting it, and survives ssh to anything.
pub mod style {
    use super::{AnsiColor, Style};

    /// Succeeded / passed.
    pub const OK: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
    /// Failed.
    pub const FAIL: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
    /// Timed out, exhausted, unreadable.
    pub const WARN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
    /// Running, live.
    pub const RUN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));
    /// A routing signal — the word an edge is taken on.
    pub const SIGNAL: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));
    /// A loop: the back edge and the bound that stops it.
    pub const LOOP: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
    /// An identifier the reader is looking for.
    pub const ID: Style = Style::new().bold();
    /// A column header or section title.
    pub const HEADER: Style = Style::new().bold();
    /// Secondary detail: defaults, argv, metadata, elided counts.
    ///
    /// `dimmed()` (SGR 2), not bright-black (SGR 90): SGR 2 is theme-relative
    /// and degrades to normal text where unsupported, while a fixed grey is
    /// unreadable on a light background.
    pub const DIM: Style = Style::new().dimmed();
}

struct Painted<'a> {
    style: Style,
    body: &'a str,
}

impl std::fmt::Display for Painted<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.style.render(),
            self.body,
            self.style.render_reset()
        )
    }
}

/// Colour policy, first match wins.
///
/// `NO_COLOR` beats `CLICOLOR_FORCE`, matching anstream, cargo and clap — hex's
/// help text is painted by clap, so following a different precedence would let
/// `hex --help` and `hex runs` disagree in the same terminal.
fn paint(when: When, json: bool, is_tty: bool) -> bool {
    match when {
        When::Never => return false,
        When::Always => return true,
        When::Auto => {}
    }
    // Machine output is never decorated, whatever the terminal says.
    if json {
        return false;
    }
    if anstyle_query::no_color() {
        return false;
    }
    if anstyle_query::clicolor_force() {
        return true;
    }
    is_tty && anstyle_query::term_supports_color()
}

/// Whether the locale claims UTF-8. Never a function of TTY-ness.
fn unicode_ok() -> bool {
    if let Some(forced) = std::env::var_os("HEX_CHARSET") {
        return forced.to_string_lossy() != "ascii";
    }
    if cfg!(windows) {
        return true;
    }
    // POSIX precedence. With none set the locale is C, and ASCII is the honest
    // answer rather than a hopeful one.
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.to_ascii_lowercase().replace('-', "").contains("utf8"))
}

/// Terminal width: `COLUMNS` first so a test can set it, then the ioctl.
fn width() -> Option<usize> {
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(n) = cols.parse::<usize>()
        && n > 0
    {
        return Some(n);
    }
    crossterm::terminal::size().ok().map(|(w, _)| w as usize)
}

/// Display width in terminal cells.
///
/// Not `chars().count()`: `…` and `≥` are East-Asian *Ambiguous* and occupy two
/// cells in a CJK locale, which silently shifts every column to their right.
#[must_use]
pub fn cells(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// A borderless space-padded table — the kubectl/docker shape.
///
/// Headers are kept when piped rather than switching to TSV: hex already has
/// `--json` as its machine channel, and a second machine shape that appears only
/// off-TTY means `hex runs | less` stops looking like `hex runs`.
pub struct Table {
    headers: Vec<String>,
    right: Vec<bool>,
    rows: Vec<Vec<String>>,
    /// The one column that yields when the terminal is too narrow.
    flex: Option<usize>,
}

impl Table {
    /// `right` marks the columns to right-align (counts, money).
    #[must_use]
    pub fn new(headers: &[&str], right: &[bool]) -> Self {
        Self {
            headers: headers.iter().map(|h| (*h).to_owned()).collect(),
            right: right.to_vec(),
            rows: Vec::new(),
            flex: None,
        }
    }

    /// The column that gives up space first. hex has exactly one long column per
    /// table (a run id, a node id), so naming it is six lines where proportional
    /// redistribution would be eighty.
    #[must_use]
    pub fn flex(mut self, col: usize) -> Self {
        self.flex = Some(col);
        self
    }

    /// Add a row. Cells may already be painted; widths are measured on the
    /// plain text, so a style never shifts a column.
    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    /// Render, returning the lines.
    #[must_use]
    pub fn render(&self, ui: Ui) -> Vec<String> {
        let cols = self.headers.len();
        let mut w: Vec<usize> = self.headers.iter().map(|h| cells(&strip(h))).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().take(cols).enumerate() {
                w[i] = w[i].max(cells(&strip(cell)));
            }
        }
        // Shrink the flex column until the table fits, never below 8 cells —
        // an id elided to nothing identifies nothing.
        if let (Some(flex), Some(max)) = (self.flex, ui.width()) {
            let total: usize = w.iter().sum::<usize>() + cols.saturating_sub(1);
            if total > max {
                w[flex] = w[flex].saturating_sub(total - max).max(8);
            }
        }

        let mut out = Vec::with_capacity(self.rows.len() + 1);
        let header: Vec<String> = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                // An empty header must stay empty, not become a bare escape pair.
                let painted = if h.is_empty() {
                    String::new()
                } else {
                    ui.paint(style::HEADER, h).to_string()
                };
                pad(&painted, h, w[i], self.right[i])
            })
            .collect();
        out.push(header.join(" ").trim_end().to_owned());
        for row in &self.rows {
            let line: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let plain = strip(c);
                    let (shown, painted) = if cells(&plain) > w[i] {
                        let cut = elide(&plain, w[i], ui);
                        // Re-wrap in the cell's own escapes: eliding the plain
                        // text and pasting it back unstyled silently dropped the
                        // colour from exactly the rows long enough to need it.
                        let styled = reclothe(c, &cut);
                        (cut, styled)
                    } else {
                        (plain, c.clone())
                    };
                    pad(&painted, &shown, w[i], self.right[i])
                })
                .collect();
            out.push(line.join(" ").trim_end().to_owned());
        }
        out
    }
}

/// Pad `painted` to `width` cells, measuring `plain` — so ANSI bytes never count
/// toward a column.
fn pad(painted: &str, plain: &str, width: usize, right: bool) -> String {
    let gap = width.saturating_sub(cells(plain));
    if right {
        format!("{}{painted}", " ".repeat(gap))
    } else {
        format!("{painted}{}", " ".repeat(gap))
    }
}

/// Cut to `width` cells, marking the cut.
fn elide(s: &str, width: usize, ui: Ui) -> String {
    let mark = if ui.unicode() { "…" } else { ".." };
    let keep = width.saturating_sub(cells(mark));
    let mut out = String::new();
    for c in s.chars() {
        if cells(&out) + UnicodeWidthStr::width(c.to_string().as_str()) > keep {
            break;
        }
        out.push(c);
    }
    out.push_str(mark);
    out
}

/// Put `body` back inside the escapes `original` was wearing.
fn reclothe(original: &str, body: &str) -> String {
    let lead: String = original
        .chars()
        .scan(false, |in_esc, c| {
            if *in_esc {
                *in_esc = c != 'm';
                Some(Some(c))
            } else if c == '\u{1b}' {
                *in_esc = true;
                Some(Some(c))
            } else {
                None
            }
        })
        .flatten()
        .collect();
    if lead.is_empty() {
        return body.to_owned();
    }
    format!("{lead}{body}\u{1b}[0m")
}

/// Text without ANSI escapes, for measuring.
fn strip(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else if c == '\u{1b}' {
            in_escape = true;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph must occupy one column in a monospace terminal, or the
    /// metadata columns drift. Multi-character arrows are the deliberate
    /// exception and are not part of the aligned badge gutter.
    #[test]
    fn badge_glyphs_are_single_characters() {
        for g in [Glyphs::UNICODE, Glyphs::ASCII] {
            for badge in [g.entry, g.agent, g.command, g.gate, g.human, g.ok, g.fail] {
                assert_eq!(
                    badge.chars().count(),
                    1,
                    "badge {badge:?} must be one character wide"
                );
            }
        }
    }

    /// No glyph may carry emoji presentation: a font that substitutes a colour
    /// emoji takes two cells and breaks every column to its right.
    #[test]
    fn no_glyph_is_an_emoji_presentation_codepoint() {
        for g in [Glyphs::UNICODE, Glyphs::ASCII] {
            let all = format!(
                "{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
                g.entry,
                g.rail,
                g.rail_end,
                g.agent,
                g.command,
                g.gate,
                g.human,
                g.ok,
                g.fail,
                g.forward,
                g.back,
                g.reroute,
                g.sep,
                g.step,
                g.le,
                g.binds
            );
            for c in all.chars() {
                assert!(
                    !matches!(c, '\u{FE0F}' | '\u{FE0E}') && (c as u32) < 0x1_0000,
                    "{c:?} is outside the BMP or carries a variation selector"
                );
            }
        }
    }

    #[test]
    fn the_legend_names_every_badge() {
        let legend = Glyphs::UNICODE.legend();
        for badge in ["◆", "□", "▣", "?", "✓", "✗"] {
            assert!(legend.contains(badge), "legend omits {badge}: {legend}");
        }
    }

    #[test]
    fn an_explicit_choice_beats_everything() {
        assert!(!paint(When::Never, false, true));
        assert!(paint(When::Always, true, false), "even --json");
    }

    #[test]
    fn machine_output_and_pipes_are_never_coloured() {
        assert!(!paint(When::Auto, true, true), "--json");
        assert!(!paint(When::Auto, false, false), "not a tty");
    }

    /// Styling must never change a column's width, or a coloured table and a
    /// piped one stop lining up.
    #[test]
    fn ansi_bytes_do_not_count_toward_a_column() {
        let ui = Ui::styled(200);
        let painted = ui.paint(style::FAIL, "failed").to_string();
        assert!(painted.len() > "failed".len(), "it really is painted");
        assert_eq!(cells(&strip(&painted)), 6);

        let mut t = Table::new(&["STATE", "N"], &[false, true]);
        t.row(vec![painted, "1".to_owned()]);
        t.row(vec!["ok".to_owned(), "20".to_owned()]);
        let rows = t.render(ui);
        let plain: Vec<usize> = rows.iter().map(|r| cells(&strip(r))).collect();
        assert_eq!(plain[1], plain[2], "both rows occupy the same cells");
    }

    #[test]
    fn the_flex_column_gives_up_space_and_the_rest_do_not() {
        let mut t = Table::new(&["RUN", "STATE"], &[false, false]).flex(0);
        t.row(vec![
            "2026-08-01-a-very-long-run-identifier".to_owned(),
            "finished:budget_exhausted".to_owned(),
        ]);
        let rows = t.render(Ui {
            color: false,
            unicode: true,
            width: Some(44),
        });
        assert!(rows[1].contains("finished:budget_exhausted"), "{rows:?}");
        assert!(rows[1].contains('…'), "the long column elided: {rows:?}");
        assert!(cells(&rows[1]) <= 44, "fits: {}", cells(&rows[1]));
    }

    /// With no knowable width nothing is cut — `hex runs > file` must keep every
    /// id in full.
    #[test]
    fn nothing_is_truncated_without_a_width() {
        let long = "2026-08-01-a-very-long-run-identifier";
        let mut t = Table::new(&["RUN"], &[false]).flex(0);
        t.row(vec![long.to_owned()]);
        assert!(t.render(Ui::plain())[1].contains(long));
    }

    /// An elided cell must keep its colour: the rows long enough to be cut are
    /// exactly the ones a reader is scanning for.
    #[test]
    fn eliding_a_cell_preserves_its_style() {
        let ui = Ui::styled(20);
        let mut t = Table::new(&["D"], &[false]).flex(0);
        t.row(vec![
            ui.paint(style::DIM, "a very long detail string indeed")
                .to_string(),
        ]);
        let row = &t.render(ui)[1];
        assert!(row.contains('…'), "cut: {row:?}");
        assert!(row.contains('\u{1b}'), "still styled: {row:?}");
    }

    #[test]
    fn an_empty_header_stays_empty() {
        let t = Table::new(&["", "N"], &[false, false]);
        assert!(!t.render(Ui::styled(40))[0].starts_with('\u{1b}'));
    }

    #[test]
    fn a_mark_carries_its_own_colour_and_falls_back_to_ascii() {
        assert_eq!(Ui::plain().mark(Mark::Ok), "✓");
        let ascii = Ui {
            color: false,
            unicode: false,
            width: None,
        };
        assert_eq!(ascii.mark(Mark::Fail), "x");
        assert!(Ui::styled(80).mark(Mark::Fail).contains('\u{1b}'));
    }
}
