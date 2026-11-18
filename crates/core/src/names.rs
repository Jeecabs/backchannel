//! Names, board paths and subscription patterns.

/// Names nobody can register, so they can never be mistaken for system roles.
pub const RESERVED: &[&str] = &["all", "admin", "everyone", "here", "mod", "relay", "smoko", "system", "backchannel", "webhook"];

const MAX_DEPTH: usize = 4;

fn is_lower_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// 2-32 chars of a-z 0-9 and "-", starting with a letter or digit. Not reserved.
pub fn is_name(s: &str) -> bool {
    name_shape(s) && !RESERVED.contains(&s)
}

fn name_shape(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=32).contains(&b.len()) && is_lower_alnum(b[0]) && b.iter().all(|&c| is_lower_alnum(c) || c == b'-')
}

/// 1-40 chars of a-z 0-9 . _ -, starting with a letter or digit.
pub fn is_agent_label(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len()) && is_lower_alnum(b[0]) && b.iter().all(|&c| is_lower_alnum(c) || b"._-".contains(&c))
}

fn is_segment(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len()) && is_lower_alnum(b[0]) && b.iter().all(|&c| is_lower_alnum(c) || c == b'_' || c == b'-')
}

/// "lachlan" or "lachlan/frontend", with or without a leading "@".
pub fn parse_address(s: &str) -> Option<(String, Option<String>)> {
    let s = s.strip_prefix('@').unwrap_or(s);
    let mut parts = s.split('/');
    let name = parts.next()?;
    let agent = parts.next();
    if parts.next().is_some() || !name_shape(name) {
        return None;
    }
    match agent {
        Some(a) if !is_agent_label(a) => None,
        a => Some((name.to_string(), a.map(str::to_string))),
    }
}

/// Direct-message boards are named "@<name>".
pub fn dm_owner(board: &str) -> Option<&str> {
    board.strip_prefix('@').filter(|n| name_shape(n))
}

pub fn is_board_name(s: &str) -> bool {
    if dm_owner(s).is_some() {
        return true;
    }
    let parts: Vec<&str> = s.split('/').collect();
    parts.len() <= MAX_DEPTH && parts.iter().all(|p| is_segment(p))
}

/// A board path where a segment may be `*` (exactly one) or `**` (zero or more).
pub fn is_pattern(s: &str) -> bool {
    if dm_owner(s).is_some() {
        return true;
    }
    let parts: Vec<&str> = s.split('/').collect();
    parts.len() <= MAX_DEPTH + 1 && parts.iter().all(|p| *p == "*" || *p == "**" || is_segment(p))
}

pub fn matches(pattern: &str, board: &str) -> bool {
    if pattern == board {
        return true;
    }
    // Wildcards never reach into someone's direct messages.
    if board.starts_with('@') {
        return false;
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let b: Vec<&str> = board.split('/').collect();
    match_parts(&p, &b)
}

fn match_parts(p: &[&str], b: &[&str]) -> bool {
    match p.split_first() {
        None => b.is_empty(),
        Some((&"**", rest)) => (0..=b.len()).any(|i| match_parts(rest, &b[i..])),
        Some((head, rest)) => !b.is_empty() && (*head == "*" || *head == b[0]) && match_parts(rest, &b[1..]),
    }
}

/// Pulls `@name` and `@name/agent` out of free text.
pub fn extract_mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices('@') {
        if i > 0 && !matches!(bytes[i - 1], b' ' | b'\n' | b'\t' | b'(' | b',') {
            continue;
        }
        let token: String = text[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '/' | '.' | '_'))
            .collect();
        let token = token.trim_end_matches(['.', '/', '-', '_']).to_string();
        if parse_address(&token).is_some() && !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(matches("ops/*", "ops/deploys"));
        assert!(!matches("ops/*", "ops/deploys/prod"));
        assert!(matches("ops/**", "ops"));
        assert!(matches("ops/**", "ops/deploys/prod"));
        assert!(matches("**", "anything/at/all"));
        assert!(!matches("**", "@lachlan"));
        assert!(matches("@lachlan", "@lachlan"));
        assert!(matches("*/builds", "web/builds"));
    }

    #[test]
    fn names() {
        assert!(is_name("lachlan"));
        assert!(!is_name("admin"));
        assert!(!is_name("L"));
        assert!(is_board_name("ops/deploys"));
        assert!(is_board_name("@lachlan"));
        assert!(!is_board_name("ops//x"));
        assert!(!is_board_name("Ops"));
        assert_eq!(parse_address("@lachlan/frontend"), Some(("lachlan".into(), Some("frontend".into()))));
    }

    #[test]
    fn mentions() {
        assert_eq!(extract_mentions("hey @bob and @lachlan/frontend. email a@b.c"), vec!["bob", "lachlan/frontend"]);
    }
}
