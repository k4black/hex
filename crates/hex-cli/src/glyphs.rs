//! Charset selection and the symbol vocabulary the renderers draw with.
//!
//! Charset is decided **independently of whether stdout is a terminal**, unlike
//! colour: `hex graph x > design.md` in a UTF-8 shell should keep its glyphs,
//! because a file is read by the same person in the same terminal. Colour is the
//! opposite — it is noise in a file — so the two questions never share an answer.
//!
//! Every glyph here is BMP with default *text* presentation (no emoji variation
//! selector), which is why the ticks are U+2713/U+2717 rather than the
//! heavier U+2714/U+2718 that many fonts render as emoji.

/// Which symbol set to draw with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// Box-drawing, arrows and geometric shapes.
    Unicode,
    /// Plain ASCII, for a terminal or locale that cannot be trusted with more.
    Ascii,
}

impl Charset {
    /// Resolve from an explicit choice, then the environment.
    ///
    /// `auto` reads the POSIX locale precedence (`LC_ALL`, then `LC_CTYPE`, then
    /// `LANG`) and asks only whether it names UTF-8. With none of them set, a
    /// POSIX locale is the C locale, so ASCII is the honest answer.
    #[must_use]
    pub fn resolve(explicit: Option<&str>) -> Self {
        match explicit {
            Some("utf8") => return Self::Unicode,
            Some("ascii") => return Self::Ascii,
            _ => {}
        }
        if let Some(forced) = std::env::var_os("HEX_CHARSET") {
            return match forced.to_string_lossy().as_ref() {
                "ascii" => Self::Ascii,
                _ => Self::Unicode,
            };
        }
        // Windows terminals have been UTF-8 by default since Windows 10; probing
        // the console code page is not worth the branch.
        if cfg!(windows) {
            return Self::Unicode;
        }
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
        match locale {
            Some(v) if v.to_ascii_lowercase().replace('-', "").contains("utf8") => Self::Unicode,
            _ => Self::Ascii,
        }
    }

    /// The symbol set for this charset.
    #[must_use]
    pub fn glyphs(self) -> Glyphs {
        match self {
            Self::Unicode => Glyphs::UNICODE,
            Self::Ascii => Glyphs::ASCII,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_choice_beats_the_environment() {
        assert_eq!(Charset::resolve(Some("ascii")), Charset::Ascii);
        assert_eq!(Charset::resolve(Some("utf8")), Charset::Unicode);
    }

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
}
