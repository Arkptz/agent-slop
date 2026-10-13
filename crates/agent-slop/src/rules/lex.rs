//! Lexical matchers: phrase and anchored-marker matching over a comment's
//! payload (markers stripped via `finding::normalize`-style anchored strip).
//!
//! These are deliberately not regexes: the corpus is data, reviewed as
//! phrases, and matching cost stays linear. Word-boundary handling is done
//! by padding, not by a regex engine, so "digital" never trips "nit" and
//! "everything" never trips "LGTM".

use super::corpus::MatchSection;

/// The comment payload: anchored marker strip (the same LEADING_MARKERS
/// table as `finding::normalize`, restated here because it is private
/// there — the values must stay in sync).
pub fn payload(text: &str) -> &str {
    let mut s = text.trim_start();
    const MARKERS: [&str; 8] = ["/**", "///", "//!", "//", "/*", "#", "--", ";"];
    while let Some(m) = MARKERS.iter().find(|m| s.starts_with(**m)) {
        s = &s[m.len()..];
    }
    let s = s.trim();
    let s = s.strip_suffix("*/").unwrap_or(s).trim();
    // Python docstrings arrive as the string literal, quotes included.
    s.trim_matches(|c| c == '"' || c == '\'').trim()
}

/// Lowercased payload with runs of whitespace folded to one space, so a
/// phrase broken across the lines of a docstring still matches.
fn padded_lower(s: &str) -> String {
    let folded = s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(" {} ", folded.to_lowercase())
}

/// Whole-word containment. A trailing `*` makes the phrase a stem, which
/// is how the Russian entries cover their inflections: `переименова*`
/// matches `переименовано` and `переименовали`.
fn contains_word(hay: &str, phrase: &str) -> bool {
    let phrase = phrase.to_lowercase();
    let (needle, stem) = match phrase.strip_suffix('*') {
        Some(n) => (n, true),
        None => (phrase.as_str(), false),
    };
    // `previously-generated` is a compound adjective, not change history.
    let joins = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '/');
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = hay[..start].chars().next_back().is_none_or(|c| !joins(c));
        let after_ok = stem || hay[end..].chars().next().is_none_or(|c| !joins(c));
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// First of `phrases` present in `text` as a word (or stem), if any.
pub fn any_phrase<'p>(phrases: &'p [String], text: &str) -> Option<&'p String> {
    let hay = padded_lower(&without_code_spans(payload(text)));
    phrases.iter().find(|p| contains_word(&hay, p))
}

/// Which phrase of `section.phrases` first matches `text`, if any. Returns
/// the phrase as written in the corpus (for the message).
pub fn phrase_hit(section: &MatchSection, text: &str) -> Option<String> {
    if section.word_boundaries {
        return any_phrase(&section.phrases, text).cloned();
    }
    let hay = padded_lower(payload(text));
    section
        .phrases
        .iter()
        .find(|p| hay.contains(&p.to_lowercase()))
        .cloned()
}

/// The corpus marker `label:` means a review-FINDING id frozen into a
/// comment (`M-01:`, `P2-6:`, `F5:`), not any leading `word:` label —
/// unbounded, it also fired on ordinary `NOTE:`, `Usage:`, `Safety:`
/// headings and on the scheme of a bare `https:` URL.
///
/// Accepted shape, over the lowercased payload: one to three letters,
/// then digit runs of at most two, at most one `-` group, at least one
/// digit, five characters total. Debt markers are never finding ids and
/// are exempted first, in both the bare (`todo:`) and dated
/// (`todo(2026-12-01):`) spellings.
fn review_finding_id(hay: &str) -> bool {
    let Some((head, rest)) = hay.split_once(':') else {
        return false;
    };
    let head = head.trim();
    // A label prefixing a self-sufficient sentence ("R5-2: run-scope is
    // unsupported for X") names the round, and the sentence carries the
    // fact. The jargon arm wants the label INSTEAD of the explanation.
    if rest.split_whitespace().count() >= 3 {
        return false;
    }
    const DEBT: [&str; 4] = ["todo", "fixme", "xxx", "hack"];
    if DEBT.contains(&head.split_once('(').map_or(head, |(root, _)| root)) {
        return false;
    }
    if head.is_empty() || head.len() > 5 {
        return false;
    }
    let (mut letters, mut digits, mut run, mut dashes) = (0usize, 0usize, 0usize, 0usize);
    for c in head.chars() {
        match c {
            'a'..='z' => {
                if digits > 0 || dashes > 0 {
                    return false;
                }
                letters += 1;
            }
            '0'..='9' => {
                digits += 1;
                run += 1;
                if run > 2 {
                    return false;
                }
            }
            '-' => {
                dashes += 1;
                run = 0;
                if dashes > 1 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    (1..=3).contains(&letters) && digits >= 1
}

/// Anchored review markers: the marker must start the comment (after the
/// comment marker), including the `LABEL: text` shape review findings are
/// frozen in (`M-01:`, `P2-6:`, `NIT:`) — never a bare substring inside
/// another word. The literal marker `label:` is the corpus's spelling for
/// that shape: it hits exactly a short leading `word...:` run.
pub fn anchored_marker_hit(section: &MatchSection, text: &str) -> Option<String> {
    let body = payload(text).trim_start_matches(['*', ' ']);
    let hay = body.to_lowercase();
    for marker in &section.anchored_markers {
        let m = marker.to_lowercase();
        let hit = if m == "label:" {
            review_finding_id(&hay)
        } else {
            let label = hay.split_once(':').map_or(hay.as_str(), |(l, _)| l.trim());
            hay.starts_with(&m) || (label.len() <= 12 && label.contains(&m))
        };
        if hit {
            return Some(marker.clone());
        }
    }
    None
}

/// `slop/rejected-alternative`: a contrast connective ("а не", "instead
/// of"), or a negation opening a clause (", not Y", "— не Y"). Code spans
/// are blanked first: a quoted `X, not Y` is documentation of the shape.
pub fn rejected_alternative_hit(section: &MatchSection, text: &str) -> Option<String> {
    let clean = without_code_spans(payload(text));
    if let Some(p) = any_phrase(&section.phrases, &clean) {
        return Some(p.clone());
    }
    let hay = padded_lower(&clean);
    for neg in &section.contrast_negations {
        for brk in [',', ':', ';', '—', '–'] {
            if hay.contains(&format!("{brk} {neg} ")) {
                return Some(format!("{brk} {neg}"));
            }
        }
    }
    None
}

/// `slop/bare-reference`: the first ticket or decision-doc id (`KAN-621`,
/// `POL-001`) in a comment that has fewer than `min_comment_words` words
/// of its own. Standards named in `stopwords` (`UTF-8`, `RFC-9110`) are
/// not references to a tracker, and neither is a one-digit suffix: that
/// shape is a product version (`GPT-6`), trackers number from 01 or 100.
pub fn bare_reference(section: &MatchSection, text: &str) -> Option<String> {
    let clean = without_code_spans(payload(text));
    let is_id = |tok: &str| is_doc_id(tok, section);
    let tokens: Vec<&str> = clean
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .filter(|t| !t.is_empty())
        .collect();
    let id = tokens.iter().find(|t| is_id(t))?;
    // Words outside the parenthesis group the id sits in; the id can close
    // any line of a multi-line block, so strip the parenthesised groups
    // instead of cutting the text at the first one.
    let words = tokens
        .iter()
        .filter(|t| !is_id(t) && t.chars().filter(|c| c.is_alphabetic()).count() > 1)
        .filter(|t| !inside_parens(t, &clean))
        .count();
    (words < section.min_comment_words.unwrap_or(4)).then(|| id.to_string())
}

/// True when `tok` occurs in `text` only inside a parenthesis group.
/// Positions of `(`..`)` spans are collected once per call.
fn inside_parens(tok: &str, text: &str) -> bool {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut open = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    open = i;
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    spans.push((open, i + 1));
                }
            }
            _ => {}
        }
    }
    let mut from = 0;
    while let Some(rel) = text[from..].find(tok) {
        let start = from + rel;
        if spans.iter().any(|(a, b)| start >= *a && start < *b) {
            return true;
        }
        from = start + tok.len();
    }
    false
}

/// `PREFIX-<digits>` naming a decision doc or ticket, the shape
/// `bare-reference` is about.
fn is_doc_id(tok: &str, section: &MatchSection) -> bool {
    let Some((prefix, num)) = tok.split_once('-') else {
        return false;
    };
    (2..=10).contains(&prefix.len())
        && prefix.starts_with(|c: char| c.is_ascii_uppercase())
        && prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && num.len() >= 2
        && num.chars().all(|c| c.is_ascii_digit())
        && !section.stopwords.iter().any(|s| s == prefix)
}

/// Text with inline code spans blanked out. A marker inside backticks is
/// being QUOTED — `slop/todo-no-condition`, `todo(2026-12-01):` — which is
/// what documentation about the rule looks like, not debt left in the code.
fn without_code_spans(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut inside = false;
    for c in s.chars() {
        if c == '`' {
            inside = !inside;
            out.push(' ');
        } else if inside {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// True when `hay` contains `needle` as a standalone word. The neighbours
/// that matter are the identifier-ish ones: `TODO-family` and
/// `slop/todo-no-condition` embed the marker in a longer name and are not
/// debt markers, while `TODO:`, `TODO.` and `TODO(owner):` are.
fn contains_marker_word(hay: &str, needle: &str) -> bool {
    let joins = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '/');
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = hay[..start].chars().next_back().is_none_or(|c| !joins(c));
        let after_ok = hay[end..].chars().next().is_none_or(|c| !joins(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Marker scan for TODO-family: any of `markers` present as a word.
pub fn marker_present(section: &MatchSection, text: &str) -> Option<String> {
    let hay = without_code_spans(payload(text)).to_lowercase();
    section
        .markers
        .iter()
        .find(|m| contains_marker_word(&hay, &m.to_lowercase()))
        .cloned()
}

/// True when the comment contains a digit anywhere (the perf-claim and
/// unsourced-constraint "requires_no_digit"/referent guards).
pub fn has_digit(text: &str) -> bool {
    payload(text).chars().any(|c| c.is_ascii_digit())
}

/// A referent, per SPEC-006 "Suppression": URL, (ADR|POL|SPEC|INC|TASK)-id,
/// issue ref, path with a slash, or date:YYYY-MM-DD. Also used to decide
/// whether an unsourced-constraint comment names its source.
pub fn has_referent(text: &str) -> bool {
    let s = payload(text);
    s.contains("://") || super::suppression::referent().is_match(&format!(" {s} "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(phrases: &[&str], markers: &[&str], anchored: &[&str]) -> MatchSection {
        MatchSection {
            phrases: phrases.iter().map(|s| s.to_string()).collect(),
            markers: markers.iter().map(|s| s.to_string()).collect(),
            anchored_markers: anchored.iter().map(|s| s.to_string()).collect(),
            word_boundaries: true,
            ..MatchSection::default()
        }
    }

    #[test]
    fn word_boundaries_keep_digital_out_of_nit() {
        // "digital" as a phrase must not match; a real nit marker must.
        let s = section(&["nit"], &[], &[]);
        assert!(phrase_hit(&s, "// the digital readout").is_none());
    }

    #[test]
    fn anchored_markers_do_not_substring_match() {
        let s = section(&[], &[], &["nit:", "lgtm"]);
        assert!(anchored_marker_hit(&s, "// everything is lgtm here sometimes").is_none());
        assert!(anchored_marker_hit(&s, "// nit: rename this").is_some());
        assert!(anchored_marker_hit(&s, "// LGTM after round 2").is_some());
        // Not a review marker: the word sits mid-prose after other text.
        assert!(anchored_marker_hit(&s, "// the pipeline says lgtm when green").is_none());
    }

    #[test]
    fn the_label_marker_hits_finding_ids_only() {
        let s = section(&[], &[], &["label:", "nit:"]);
        for hit in ["// M-01: survived OAuth", "// nit: rename this"] {
            assert!(anchored_marker_hit(&s, hit).is_some(), "must hit: {hit}");
        }
        for miss in [
            "// R5-2: run-scope is unsupported for a delegate_agent grant",
            "// GAP10: wire the shared role registry into the daemon config",
            "// NOTE: the socket is closed by the caller",
            "// Usage: agent-slop check --json",
            "// Safety: the pointer is non-null by construction",
            "// Purpose: parse the hook envelope",
            "// see https://example.com/x",
            "// TODO: split this",
            "// todo(2026-12-01): drop the shim",
            "// Invariant: len is non-zero",
        ] {
            assert!(anchored_marker_hit(&s, miss).is_none(), "must miss: {miss}");
        }
    }

    /// Documentation about the debt-marker rule quotes the marker rather than
    /// leaving debt. The scanner reads its own source, where all of these are
    /// prose.
    #[test]
    fn a_quoted_or_embedded_marker_is_not_debt() {
        let s = section(&[], &["TODO", "FIXME", "XXX", "HACK"], &[]);
        for miss in [
            "/// Full rule ids (`slop/todo-no-condition`, `ruff/ERA001`) to levels —",
            "//! (`todo-no-condition.toml`). No regex dependency: the shipped shapes are",
            "/// are exempted first, in both the bare (`todo:`) and dated",
            "/// (`todo(2026-12-01):`) spellings.",
            "/// Marker scan for TODO-family: any of `markers` present as a word.",
        ] {
            assert!(marker_present(&s, miss).is_none(), "must miss: {miss}");
        }
        for hit in [
            "// TODO: split this",
            "# FIXME handle the empty case",
            "// TODO(arkptz): drop the shim",
            "// XXX.",
        ] {
            assert!(marker_present(&s, hit).is_some(), "must hit: {hit}");
        }
    }

    #[test]
    fn payload_strips_only_anchored_markers() {
        assert_eq!(payload("// plain"), "plain");
        assert_eq!(payload("/// doc"), "doc");
        assert_eq!(payload("/** block */"), "block");
        assert_eq!(payload("# hash"), "hash");
        // Mid-comment markers are content.
        assert_eq!(payload("// a -- b"), "a -- b");
    }

    #[test]
    fn a_phrase_quoted_in_backticks_is_not_a_hit() {
        let s = section(&["переименова*"], &[], &[]);
        assert!(phrase_hit(&s, "// `переименова*` matches `переименовано`").is_none());
        assert!(phrase_hit(&s, "// переименовано из foo").is_some());
    }

    #[test]
    fn digit_and_referent_guards() {
        assert!(has_digit("// 3x faster"));
        assert!(!has_digit("// much faster"));
        assert!(has_referent("// blocked on owner/repo#12"));
        assert!(has_referent("// per ADR-064"));
        assert!(has_referent("// see https://example.com/x"));
        assert!(has_referent("// fixed in v0.16 (see docs/rules.md)"));
        assert!(!has_referent("// some providers require this"));
    }
}
