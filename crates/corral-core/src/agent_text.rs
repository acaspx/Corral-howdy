//! Text an agent wrote, cleaned before any surface draws it (security model T20).
//!
//! Answers, summaries, plan steps and error messages can carry anything a model or a web page
//! put there. Before it reaches a card it's cleaned the same way everywhere:
//! - control characters go (a tab becomes a space; line breaks stay, `\r\n` becomes `\n`, and
//!   more than one blank line in a row becomes one);
//! - bidi controls and invisible format characters go (U+061C, U+200B, U+200C, U+200E, U+200F,
//!   U+202A..U+202E, U+2060..U+2064, U+2066..U+2069, U+FEFF, U+FFF9..U+FFFB). The zero-width
//!   joiner stays, because emoji sequences need it;
//! - leading and trailing whitespace goes;
//! - the result is capped at a number of characters, and the cut is flagged so the card can say
//!   it's showing part of the text.
//!
//! Swift bounds the text again when it arrives (it trusts nothing from Rust).

/// The most of an answer a result card shows.
pub const ANSWER_MAX_CHARS: usize = 4_000;
/// The most of a prompt, error or other one-line text a card shows.
pub const PROMPT_MAX_CHARS: usize = 280;

/// Cleaned, capped agent text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentText {
    text: String,
    truncated: bool,
}

impl AgentText {
    /// Cleans `raw` and keeps at most `max_chars` characters of it.
    pub fn new(raw: &str, max_chars: usize) -> Self {
        let mut clean = String::with_capacity(raw.len().min(max_chars.saturating_mul(4)));
        let mut newlines = 0usize;
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            let c = match c {
                '\r' if chars.peek() == Some(&'\n') => continue,
                '\r' | '\n' => '\n',
                '\t' => ' ',
                c if c.is_control() || is_invisible(c) => continue,
                c => c,
            };
            if c == '\n' {
                newlines += 1;
                if newlines > 2 {
                    continue;
                }
            } else {
                newlines = 0;
            }
            clean.push(c);
        }
        let trimmed = clean.trim();
        let mut text: String = trimmed.chars().take(max_chars).collect();
        let truncated = text.len() < trimmed.len();
        if truncated {
            // Don't end on half a line break or a dangling space.
            let kept = text.trim_end().len();
            text.truncate(kept);
        }
        Self { text, truncated }
    }

    /// The cleaned text.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the text was cut to fit.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// Bidi controls and invisible format characters.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}
