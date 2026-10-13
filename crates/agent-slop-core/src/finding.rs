//! Finding model and ratchet identity (SPEC-006 "Finding model" / "Rule ids
//! and severity" / "Ratchet").
//!
//! A suppressed finding is carried, never dropped — that is what keeps
//! "which rules get silenced, and why" observable. The fingerprint hashes
//! rule id and normalized snippet only; the path is excluded on purpose, so
//! moving a file does not turn inherited debt into "added". Marker stripping
//! is anchored, never a global replace: a global replace would eat the `--`
//! suppression separator, every SQL-style `--` and every em-dash, collapsing
//! unrelated comments onto one fingerprint.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Slop,
    Linter { tool: String },
}

/// `valid` is decided by the suppression parser (SPEC-006 "Suppression");
/// the model only carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suppression {
    pub rule: String,
    pub reason: String,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity, // ours, from config — not the linter's
    pub upstream_severity: Option<String>, // what the linter said, telemetry only
    pub file: String,       // repo-relative, '/' separated
    pub line: u32,          // 1-based
    pub col: u32,
    pub end_line: u32,
    pub message: String, // one line, imperative, no trailing period
    pub snippet: String, // raw match; the 512-byte cap is applied by the producer
    pub fingerprint: String,
    pub source: Source,
    pub lang: Option<String>,
    pub suppression: Option<Suppression>,
    pub fixable: bool,
}

/// Longest-first: `//` would otherwise eat the first half of `///`, `//!`,
/// `/*` and `/**`, leaving a stray `/` or `*` in the payload.
const LEADING_MARKERS: [&str; 8] = ["/**", "///", "//!", "//", "/*", "#", "--", ";"];

/// Returns the payload and whether a block form was opened: the closing
/// `*/` is stripped only as the counterpart of an opened block, so `x */`
/// (no `/*`) keeps its content.
fn strip_markers(mut s: &str) -> (&str, bool) {
    let mut block = false;
    while let Some(m) = LEADING_MARKERS.iter().find(|m| s.starts_with(**m)) {
        if matches!(*m, "/*" | "/**") {
            block = true;
        }
        s = &s[m.len()..];
    }
    let s = s.trim_end();
    if block {
        (s.strip_suffix("*/").unwrap_or(s), true)
    } else {
        (s, false)
    }
}

/// SPEC-006 normalize: anchored marker strip, whitespace runs (newlines
/// included) collapsed to one space, trim, NFC. NFC is load-bearing for
/// slop/comment-language: Russian "й" has a composed and a decomposed
/// spelling, and without it one comment yields two fingerprints.
pub fn normalize(snippet: &str) -> String {
    let (s, _) = strip_markers(snippet.trim_start());
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.nfc().collect()
}

/// blake3(rule_id ‖ 0x00 ‖ normalize(snippet)), first 8 bytes as 16
/// lowercase hex chars. Truncation collisions are strictly fail-open
/// (SPEC-006 "Ratchet").
pub fn fingerprint(rule_id: &str, snippet: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(rule_id.as_bytes());
    hasher.update(&[0x00]);
    hasher.update(normalize(snippet).as_bytes());
    hasher.finalize().as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_the_suppression_separator() {
        assert_eq!(normalize("// TODO -- x"), "TODO -- x");
        assert_eq!(
            normalize(
                "# agent-slop: allow slop/todo-no-condition -- blocked on astral-sh/ruff#12345"
            ),
            "agent-slop: allow slop/todo-no-condition -- blocked on astral-sh/ruff#12345"
        );
        // SQL marker at the anchor strips; the em-dash inside prose stays.
        assert_eq!(normalize("-- prose — stays"), "prose — stays");
    }

    #[test]
    fn markers_strip_only_at_the_anchor() {
        assert_eq!(normalize("// x"), "x");
        // Rust doc forms must reach the same payload as the plain form.
        assert_eq!(normalize("/// x"), "x");
        assert_eq!(normalize("//! x"), "x");
        assert_eq!(normalize("/** x */"), normalize("/* x */"));
        assert_eq!(normalize("# x"), "x");
        assert_eq!(normalize("## x"), "x"); // a run, still anchored
        assert_eq!(normalize("; x"), "x");
        assert_eq!(normalize("---- x ----"), "x ----");
        // Mid-comment markers are content.
        assert_eq!(normalize("x // y"), "x // y");
        assert_eq!(normalize("a -- b # c"), "a -- b # c");
        assert_eq!(normalize("x */"), "x */");
    }

    #[test]
    fn whitespace_runs_collapse_to_one_space_and_trim() {
        assert_eq!(normalize("  a\n\n b \t c  "), "a b c");
        assert_eq!(normalize("//\t a\n * b"), "a * b");
        assert_eq!(normalize("//"), "");
    }

    #[test]
    fn marker_and_whitespace_variants_share_a_fingerprint() {
        let plain = fingerprint("slop/comment-history", "// TODO: fix");
        assert_eq!(plain, fingerprint("slop/comment-history", "/// TODO: fix"));
        assert_eq!(
            plain,
            fingerprint("slop/comment-history", "/*  TODO:\n fix  */")
        );
        assert_ne!(
            plain,
            fingerprint("slop/comment-history", "// TODO: fix later")
        );
        assert_ne!(plain, fingerprint("slop/comment-meta", "// TODO: fix"));
    }

    /// Decomposed and composed Russian must be one fingerprint, or the
    /// comment-language rule silently misses half its matches.
    #[test]
    fn nfc_folds_decomposed_and_composed_spellings() {
        let composed = "// \u{0439} \u{0451}";
        let decomposed = "// \u{0438}\u{0306} \u{0435}\u{0308}";
        assert_ne!(composed, decomposed, "inputs must differ byte-wise");
        assert_eq!(normalize(composed), normalize(decomposed));
        assert_eq!(
            fingerprint("slop/comment-language", composed),
            fingerprint("slop/comment-language", decomposed)
        );
    }

    #[test]
    fn fingerprint_is_16_lowercase_hex_chars() {
        let f = fingerprint("slop/review-jargon", "// LGTM");
        assert_eq!(f.len(), 16);
        assert!(
            f.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "not lowercase hex: {f}"
        );
    }

    #[test]
    fn severity_serializes_as_lowercase_words() {
        assert_eq!(
            serde_json::to_string(&Severity::Error).unwrap(),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&Severity::Warning).unwrap(),
            "\"warning\""
        );
        assert_eq!(serde_json::to_string(&Severity::Hint).unwrap(), "\"hint\"");
        assert_eq!(
            serde_json::from_str::<Severity>("\"hint\"").unwrap(),
            Severity::Hint
        );
    }

    #[test]
    fn finding_round_trips_with_spec_field_names() {
        let f = Finding {
            rule: "ruff/F401".into(),
            severity: Severity::Warning,
            upstream_severity: Some("error".into()),
            file: "src/a.py".into(),
            line: 3,
            col: 1,
            end_line: 3,
            message: "remove unused import".into(),
            snippet: "import os".into(),
            fingerprint: fingerprint("ruff/F401", "import os"),
            source: Source::Linter {
                tool: "ruff".into(),
            },
            lang: Some("python".into()),
            suppression: Some(Suppression {
                rule: "ruff/F401".into(),
                reason: "tracked in Arkptz/dg#41".into(),
                valid: true,
            }),
            fixable: false,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["upstream_severity"], "error");
        assert_eq!(json["end_line"], 3);
        assert_eq!(
            json["source"],
            serde_json::json!({"linter": {"tool": "ruff"}})
        );
        assert_eq!(json["suppression"]["reason"], "tracked in Arkptz/dg#41");
        assert_eq!(serde_json::from_value::<Finding>(json).unwrap(), f);
    }
}
