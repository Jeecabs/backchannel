//! Cleaning text that other people wrote, before it is stored or shown.
//!
//! Message text ends up in two places where stray characters do damage: an
//! agent's context, where a line break can make a message look like it ended,
//! and a terminal, where an escape sequence can rewrite what a person sees.
//! Everything here keeps printable text, `\n` and `\t`, and nothing else.

/// Characters that end a line somewhere, even though only `\n` should.
fn is_break(c: char) -> bool {
    matches!(c, '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Characters that reorder the text around them when displayed.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Control characters other than `\n` and `\t`, the Unicode line and paragraph
/// separators, and bidirectional overrides.
pub fn is_forbidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || is_break(c) || is_bidi_control(c)
}

/// True when `s` could be stored as it is: printable text, `\n` and `\t` only.
pub fn is_clean(s: &str) -> bool {
    !s.chars().any(is_forbidden)
}

/// `is_clean`, and all on one line.
pub fn is_clean_line(s: &str) -> bool {
    is_clean(s) && !s.contains('\n')
}

/// Turns every kind of line break into `\n` and drops the other forbidden
/// characters, so the result passes `is_clean`.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else if is_break(c) {
            out.push('\n');
        } else if !is_forbidden(c) {
            out.push(c);
        }
    }
    out
}

/// `clean`, folded onto one line.
pub fn clean_line(s: &str) -> String {
    clean(s).replace(['\n', '\t'], " ").trim().to_string()
}

/// The first `n` characters.
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks_become_newlines() {
        assert_eq!(clean("a\r\nb\rc\u{2028}d\u{85}e\u{0b}f"), "a\nb\nc\nd\ne\nf");
    }

    #[test]
    fn escapes_and_overrides_are_dropped() {
        assert_eq!(clean("a\u{1b}[2Kb\u{7}c\u{0}d\u{202e}e\u{9b}f"), "a[2Kbcdef");
        assert!(is_clean(&clean("\u{1b}]0;title\u{7}\r\u{2029}")));
    }

    #[test]
    fn tabs_and_newlines_survive() {
        assert_eq!(clean("a\tb\nc"), "a\tb\nc");
        assert!(is_clean("a\tb\nc"));
        assert!(!is_clean("a\rb"));
        assert!(!is_clean_line("a\nb"));
    }

    #[test]
    fn one_line() {
        assert_eq!(clean_line(" a\r\nb\tc\u{2028}d "), "a b c d");
    }
}
