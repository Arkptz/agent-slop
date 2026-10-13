//! Tiny pattern interpreter for the corpus's `condition_patterns`
//! (`todo-no-condition.toml`). No regex dependency: the shipped shapes are
//! few and fixed — literal substrings, `#\d+`, `https?://`,
//! `\b(word|…)\b` alternations, dotted versions, and `(ADR|…)-\d+` ids —
//! and each gets a hand-rolled arm. A pattern outside these shapes simply
//! does not match (fail-open: the marker keeps its warning, nothing
//! blocks).

/// Interpret one corpus pattern against `text`.
#[must_use]
pub fn matches_condition(pat: &str, text: &str) -> bool {
    let (ci, pat) = match pat.strip_prefix("(?i)") {
        Some(rest) => (true, rest),
        None => (false, pat),
    };
    let hay = if ci {
        text.to_lowercase()
    } else {
        text.to_string()
    };
    let needle = if ci {
        pat.to_lowercase()
    } else {
        pat.to_string()
    };

    if !needle.contains('\\') && !needle.contains('(') && !needle.contains('?') {
        return hay.contains(&needle);
    }
    if needle == "#\\d+" {
        return hay
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b'#' && w[1].is_ascii_digit());
    }
    if needle == "https?://" {
        return hay.contains("http://") || hay.contains("https://");
    }
    // `\b(word|word|...)\b` — any whole word from the set (words only:
    // alternations with meta-characters fall through).
    if needle.starts_with("\\b(") && needle.ends_with(")\\b") {
        let inner = &needle[3..needle.len() - 3];
        if !inner.contains('\\') && !inner.contains('?') {
            let words: Vec<&str> = inner.split('|').collect();
            return hay
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| words.contains(&w));
        }
    }
    if needle.contains("\\d+\\.\\d+") || needle.contains("v?\\d+") {
        return has_dotted_version(&hay);
    }
    // `(...)-\d+` — id-prefix alternation plus digits.
    if let Some(stem) = needle.strip_suffix("-\\d+")
        && let Some(stem) = stem.strip_prefix('(').and_then(|s| s.strip_suffix(')'))
    {
        let prefixes: Vec<String> = stem.split('|').map(str::to_lowercase).collect();
        return hay
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .any(|t| {
                prefixes.iter().any(|p| {
                    t.to_lowercase().starts_with(&format!("{p}-"))
                        && t.len() > p.len() + 1
                        && t[p.len() + 1..].chars().all(|c| c.is_ascii_digit())
                })
            });
    }
    false
}

/// A digit, a dot, a digit — `0.16`, `2.0`, `1.2.3` all qualify.
fn has_dotted_version(hay: &str) -> bool {
    let b = hay.as_bytes();
    (0..b.len().saturating_sub(2))
        .any(|i| b[i].is_ascii_digit() && b[i + 1] == b'.' && b[i + 2].is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_gate_counts_as_condition() {
        assert!(matches_condition(
            "(?i)\\b(v?\\d+\\.\\d+)\\b",
            "# TODO: drop once ships >= 0.16"
        ));
        assert!(matches_condition(
            "(?i)\\b(v?\\d+\\.\\d+)\\b",
            "// TODO drop in 2.0"
        ));
        assert!(!matches_condition(
            "(?i)\\b(v?\\d+\\.\\d+)\\b",
            "// TODO: fix later"
        ));
    }

    #[test]
    fn doc_ids_and_issue_refs_count() {
        assert!(matches_condition(
            "(?i)(ADR|POL|SPEC|INC|TASK)-\\d+",
            "blocked on ADR-064"
        ));
        assert!(matches_condition("#\\d+", "tracked as #123"));
        assert!(!matches_condition("#\\d+", "no ref here"));
    }

    #[test]
    fn word_alternation_is_whole_word() {
        assert!(matches_condition(
            "(?i)\\b(until|before|after|once|when)\\b",
            "drop BEFORE the flip"
        ));
        assert!(!matches_condition(
            "(?i)\\b(until|before|after|once|when)\\b",
            "the untockenizable word"
        ));
    }
}
