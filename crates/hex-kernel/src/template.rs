//! The shared grammar for node-prompt templates: `{{prompt}}` (the operator's
//! prompt) and `{{<node>.result}}` (a handoff of another node's captured
//! result). The kernel owns this parser so validation ([`check_result_refs`])
//! and the runtime's attempt-start interpolation can never disagree on what a
//! token *is* — a graph that validates renders exactly as validated.
//!
//! [`check_result_refs`]: crate::validate

/// One span of a prompt template, yielded in source order by [`tokens`].
#[derive(Debug, PartialEq, Eq)]
pub enum Token<'a> {
    /// Verbatim text — includes any *unrecognized* `{{…}}` token, reproduced
    /// as-is so an unknown brace pair is never silently dropped.
    Text(&'a str),
    /// The operator prompt placeholder `{{prompt}}`.
    Prompt,
    /// A `{{<node>.result}}` handoff; the payload is the trimmed node id.
    Result(&'a str),
    /// An unterminated `{{` (no closing `}}`). Carries the raw remainder from
    /// `{{` onward so a renderer can echo it verbatim; no tokens follow.
    Unterminated(&'a str),
}

/// Parse `template` into its [`Token`]s, in order. Pure and allocation-free.
#[must_use]
pub fn tokens(template: &str) -> Tokens<'_> {
    Tokens { rest: template }
}

/// Iterator over a template's [`Token`]s (see [`tokens`]).
pub struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        if self.rest.is_empty() {
            return None;
        }
        // Text up to the next `{{` (or the whole remainder if there is none).
        let Some(open) = self.rest.find("{{") else {
            let all = self.rest;
            self.rest = "";
            return Some(Token::Text(all));
        };
        if open > 0 {
            let text = &self.rest[..open];
            self.rest = &self.rest[open..];
            return Some(Token::Text(text));
        }
        // `rest` starts with `{{`.
        let Some(close_rel) = self.rest[2..].find("}}") else {
            let raw = self.rest;
            self.rest = "";
            return Some(Token::Unterminated(raw));
        };
        let end = 2 + close_rel + 2; // one past the closing `}}`
        let whole = &self.rest[..end];
        let inner = self.rest[2..2 + close_rel].trim();
        self.rest = &self.rest[end..];
        if inner == "prompt" {
            Some(Token::Prompt)
        } else if let Some(node) = inner.strip_suffix(".result") {
            Some(Token::Result(node.trim()))
        } else {
            // Unrecognized token — reproduce it exactly.
            Some(Token::Text(whole))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Token, tokens};

    fn parse(s: &str) -> Vec<Token<'_>> {
        tokens(s).collect()
    }

    #[test]
    fn plain_text_is_one_token() {
        assert_eq!(parse("just text"), vec![Token::Text("just text")]);
        assert_eq!(parse(""), vec![]);
    }

    #[test]
    fn recognizes_prompt_and_result_with_surrounding_text() {
        assert_eq!(
            parse("do {{prompt}} using {{review.result}}!"),
            vec![
                Token::Text("do "),
                Token::Prompt,
                Token::Text(" using "),
                Token::Result("review"),
                Token::Text("!"),
            ]
        );
    }

    #[test]
    fn trims_whitespace_inside_a_token() {
        assert_eq!(parse("{{  prompt  }}"), vec![Token::Prompt]);
        assert_eq!(parse("{{ review .result }}"), vec![Token::Result("review")]);
    }

    #[test]
    fn unrecognized_token_is_preserved_verbatim() {
        assert_eq!(
            parse("a {{foo}} b"),
            vec![Token::Text("a "), Token::Text("{{foo}}"), Token::Text(" b"),]
        );
    }

    #[test]
    fn unterminated_carries_the_raw_remainder_and_stops() {
        assert_eq!(
            parse("tail {{prompt"),
            vec![Token::Text("tail "), Token::Unterminated("{{prompt"),]
        );
    }

    #[test]
    fn nested_braces_take_the_first_terminator() {
        // `{{ {{prompt}}` closes at the first `}}`, is unrecognized, and is
        // reproduced verbatim; the trailing ` }}` is plain text.
        assert_eq!(
            parse("{{ {{prompt}} }}"),
            vec![Token::Text("{{ {{prompt}}"), Token::Text(" }}"),]
        );
    }
}
