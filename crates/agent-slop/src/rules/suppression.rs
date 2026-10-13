//! Suppression parsing (SPEC-006 "Suppression"):
//! `<marker> agent-slop: allow <rule-id> -- <reason>`, line-scoped — the
//! same line, or the next non-empty line when the suppression stands alone.
//! No file-level or block-level suppressions in v1.
//!
//! The reason is mandatory and must carry a referent (URL, decision-doc id,
//! issue ref, path, or date). A missing `--`/short reason raises
//! `slop/suppression-empty` at error; a referent-less reason raises
//! `slop/suppression-vacuous` at error. Both ids are fixed vocabulary, not
//! corpus families, so they parse from constants.

use std::sync::OnceLock;

pub const EMPTY_RULE: &str = "slop/suppression-empty";
pub const VACUOUS_RULE: &str = "slop/suppression-vacuous";

/// The SPEC-006 referent grammar, hand-rolled: URL, `(ADR|POL|SPEC|INC|
/// TASK)-\d+`, issue ref `[\w./-]+#\d+`, a path containing a slash, or
/// `date:YYYY-MM-DD`. No regex crate — one dependency less, and the grammar
/// is small enough that a scanner over whitespace tokens stays readable.
pub struct Referent;

impl Referent {
    pub fn is_match(&self, s: &str) -> bool {
        if s.contains("://") {
            return true;
        }
        s.split_whitespace().any(is_referent_token)
    }
}

pub fn referent() -> &'static Referent {
    static R: OnceLock<Referent> = OnceLock::new();
    R.get_or_init(|| Referent)
}

fn is_referent_token(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| {
        !c.is_alphanumeric() && c != '/' && c != '-' && c != '_' && c != '.' && c != '#' && c != ':'
    });
    // date:YYYY-MM-DD
    if let Some(rest) = t.strip_prefix("date:") {
        let mut parts = rest.splitn(3, '-');
        if let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) {
            if y.len() == 4
                && m.len() == 2
                && d.len() == 2
                && y.bytes()
                    .chain(m.bytes())
                    .chain(d.bytes())
                    .all(|b| b.is_ascii_digit())
            {
                return true;
            }
        }
    }
    // Decision-doc id.
    for p in ["ADR-", "POL-", "SPEC-", "INC-", "TASK-"] {
        if let Some(rest) = t.to_uppercase().strip_prefix(p) {
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                return true;
            }
        }
    }
    // Issue ref: owner/repo#123 or #123.
    if let Some((left, right)) = t.rsplit_once('#') {
        if !right.is_empty()
            && right.bytes().all(|b| b.is_ascii_digit())
            && !left.is_empty()
            && left
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'/' | b'-' | b'_'))
        {
            return true;
        }
    }
    // A path containing a slash: docs/adr-064.md, src/rules/.
    if t.contains('/')
        && t.len() > 3
        && !t.starts_with("//")
        && t.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '#'))
    {
        return true;
    }
    false
}

/// A concrete `<producer>/<id>`: both halves non-empty and made of the
/// characters a rule id uses. `<rule-id>`, `slop/...` and a bare word are
/// placeholders, not ids.
fn is_rule_id(s: &str) -> bool {
    let Some((producer, id)) = s.split_once('/') else {
        return false;
    };
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    ok(producer) && ok(id)
}

/// One parsed suppression line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowLine {
    pub rule: String,
    pub reason: String,
    pub valid: bool,
    /// `slop/suppression-empty` | `slop/suppression-vacuous` when invalid.
    pub violation: Option<&'static str>,
}

/// True when the comment is NOTHING but an allow directive — such a line
/// suppresses the next non-empty line and is not itself content.
pub fn is_standalone_allow(text: &str) -> bool {
    let t = text.trim_start();
    let t = t
        .strip_prefix("//")
        .or_else(|| t.strip_prefix("#"))
        .or_else(|| t.strip_prefix("/*"))
        .or_else(|| t.strip_prefix("--"))
        .or_else(|| t.strip_prefix(";"))
        .unwrap_or(t);
    t.trim_start()
        .to_lowercase()
        .starts_with("agent-slop: allow")
}

/// Parse one `agent-slop: allow <rule-id> -- <reason>` line. Returns `None`
/// for text that is not an allow line. Handles both the standalone form and
/// the trailing same-line form `(agent-slop: allow ... -- ...)`.
pub fn parse_allow(text: &str) -> Option<AllowLine> {
    let t = text.trim_start();
    let t = t
        .strip_prefix("//")
        .or_else(|| t.strip_prefix("#"))
        .or_else(|| t.strip_prefix("/*"))
        .or_else(|| t.strip_prefix("--"))
        .or_else(|| t.strip_prefix(";"))
        .unwrap_or(t);
    let trimmed = t.trim_start();
    let body = if let Some(rest) = trimmed.strip_prefix("agent-slop:") {
        rest.strip_prefix(" allow")?.trim_start().to_string()
    } else {
        // Trailing same-line form: `(agent-slop: allow ... -- reason)`.
        let i = trimmed.find("agent-slop: allow")?;
        trimmed[i + "agent-slop: allow".len()..].trim().to_string()
    };
    let body = body.strip_suffix("*/").unwrap_or(&body).to_string();

    let (rule, rest) = match body.split_once("--") {
        Some((rule, reason)) => (
            rule.trim().to_string(),
            reason.trim().trim_end_matches(')').trim().to_string(),
        ),
        None => (body.trim().to_string(), String::new()),
    };
    // Prose documenting the syntax carries a placeholder (`<rule-id>`, `...`)
    // where a directive carries a real id. Without this check the tool's own
    // documentation parses as a malformed suppression and raises an error.
    if !is_rule_id(&rule) {
        return None;
    }

    let (valid, violation) = if rest.chars().filter(|c| !c.is_whitespace()).count() < 12 {
        (false, Some(EMPTY_RULE))
    } else if !referent().is_match(&rest) {
        (false, Some(VACUOUS_RULE))
    } else {
        (true, None)
    };

    Some(AllowLine {
        rule,
        reason: rest,
        valid,
        violation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples_parse_and_validate() {
        let a = parse_allow(
            "// agent-slop: allow slop/comment-history -- rollback steps required by ADR-031",
        )
        .unwrap();
        assert_eq!(a.rule, "slop/comment-history");
        assert!(a.valid);
        assert!(a.violation.is_none());

        let b = parse_allow(
            "# agent-slop: allow slop/todo-no-condition -- blocked on astral-sh/ruff#12345",
        )
        .unwrap();
        assert!(b.valid);
    }

    /// Prose about the syntax is not a suppression. The scanner reads its own
    /// source, where these lines are documentation, and a placeholder parsed
    /// as a directive raised `slop/suppression-empty` at error on it.
    #[test]
    fn syntax_documentation_is_not_a_directive() {
        for prose in [
            "//! `<marker> agent-slop: allow <rule-id> -- <reason>`, line-scoped — the",
            "/// Parse one `agent-slop: allow <rule-id> -- <reason>` line. Returns `None`",
            "/// the trailing same-line form `(agent-slop: allow ... -- ...)`.",
            "// Trailing same-line form: `(agent-slop: allow ... -- reason)`.",
        ] {
            assert!(parse_allow(prose).is_none(), "must not parse: {prose}");
        }
    }

    #[test]
    fn missing_separator_is_empty_not_vacuous() {
        let a = parse_allow("// agent-slop: allow slop/comment-history").unwrap();
        assert!(!a.valid);
        assert_eq!(a.violation, Some(EMPTY_RULE));
    }

    #[test]
    fn short_reason_is_empty() {
        let a = parse_allow("// agent-slop: allow slop/comment-history -- too short").unwrap();
        assert!(!a.valid);
        assert_eq!(a.violation, Some(EMPTY_RULE));
    }

    #[test]
    fn referentless_reason_is_vacuous() {
        let a = parse_allow(
            "// agent-slop: allow slop/comment-history -- this is fine because reasons",
        )
        .unwrap();
        assert!(!a.valid);
        assert_eq!(a.violation, Some(VACUOUS_RULE));
    }

    #[test]
    fn url_path_and_date_count_as_referents() {
        for reason in [
            "documented at https://example.com/rule#why",
            "see docs/decisions/adr-064.md",
            "revisit date:2027-01-01",
            "tracked as Arkptz/dg#41",
        ] {
            let a = parse_allow(&format!(
                "// agent-slop: allow slop/comment-meta -- {reason}"
            ))
            .unwrap();
            assert!(a.valid, "reason {reason}");
        }
    }

    #[test]
    fn ordinary_prose_is_not_an_allow_line() {
        assert!(parse_allow("// agent-slop allows some things").is_none());
        assert!(parse_allow("// TODO: fix later").is_none());
    }
}
