//! Rule corpus: `rules/*.toml` embedded via `include_str!` and parsed at
//! runtime (SPEC-006 "Rule ids and severity", `rules/README.md`).
//!
//! A malformed rule file drops THAT rule with a stderr line; the rest run
//! and nothing blocks — the fail-open matrix in SPEC-006. Parse errors are
//! therefore impossible to observe in a shipped binary (checked by `cargo
//! test`), but the drop path exists because the matrix promises it.

use std::collections::BTreeMap;

use serde::Deserialize;

pub const COMMENT_HISTORY: &str = include_str!("../../rules/comment-history.toml");
pub const COMMENT_META: &str = include_str!("../../rules/comment-meta.toml");
pub const REVIEW_JARGON: &str = include_str!("../../rules/review-jargon.toml");
pub const COMMENT_PARAPHRASE: &str = include_str!("../../rules/comment-paraphrase.toml");
pub const TODO_NO_CONDITION: &str = include_str!("../../rules/todo-no-condition.toml");
pub const PERF_CLAIM: &str = include_str!("../../rules/perf-claim.toml");
pub const UNSOURCED_CONSTRAINT: &str = include_str!("../../rules/unsourced-constraint.toml");
pub const COMMENT_LANGUAGE: &str = include_str!("../../rules/comment-language.toml");
pub const REJECTED_ALTERNATIVE: &str = include_str!("../../rules/rejected-alternative.toml");
pub const MANUAL_DDL: &str = include_str!("../../rules/manual-ddl.toml");
pub const BARE_REFERENCE: &str = include_str!("../../rules/bare-reference.toml");
pub const COMMENT_HEDGING: &str = include_str!("../../rules/comment-hedging.toml");
pub const COMMENT_STEPS: &str = include_str!("../../rules/comment-steps.toml");
pub const DOCSTRING_RESTATES_NAME: &str = include_str!("../../rules/docstring-restates-name.toml");
pub const SELF_JUSTIFICATION: &str = include_str!("../../rules/self-justification.toml");
pub const CONFIG_DEPLOY_NARRATION: &str = include_str!("../../rules/config-deploy-narration.toml");

/// The lexical half of one rule family (`rules/README.md`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuleFile {
    pub meta: RuleMeta,
    #[serde(default)]
    pub r#match: MatchSection,
    #[serde(default)]
    pub exception: ExceptionSection,
    #[serde(default)]
    pub note: NoteSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuleMeta {
    pub id: String,
    pub severity: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MatchSection {
    #[serde(default)]
    pub phrases: Vec<String>,
    #[serde(default)]
    pub anchored_markers: Vec<String>,
    #[serde(default)]
    pub markers: Vec<String>,
    #[serde(default)]
    pub condition_patterns: Vec<String>,
    #[serde(default)]
    pub stopwords: Vec<String>,
    #[serde(default)]
    pub why_markers: Vec<String>,
    /// Flat `["ru_word", "en_word", ...]` pairs: a comment word matching an
    /// odd element counts as if it were the even element after it.
    #[serde(default)]
    pub ru_glossary: Vec<String>,
    #[serde(default)]
    pub contrast_negations: Vec<String>,
    #[serde(default)]
    pub ddl: Vec<String>,
    #[serde(default)]
    pub min_shared_identifiers: Option<usize>,
    #[serde(default)]
    pub min_comment_words: Option<usize>,
    #[serde(default)]
    pub word_boundaries: bool,
    #[serde(default)]
    pub requires_no_digit: bool,
    #[serde(default)]
    pub requires_no_referent: bool,
    /// `docstring-restates-name`: the definition-slot clause before the
    /// first colon must have at least this many substantive words.
    #[serde(default)]
    pub min_head_words: Option<usize>,
    /// Phrases whose presence in a why-marked comment CANCELS the why
    /// exemption (comment-paraphrase carve-outs, FINDINGS.md §10.2): a why
    /// about what the module has no right to know is not a why.
    #[serde(default)]
    pub why_exemption_cancels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExceptionSection {
    #[serde(default)]
    pub shape: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct NoteSection {
    #[serde(default)]
    pub text: String,
}

/// The full parsed corpus: family name → rule. Families whose file fails
/// to parse are absent and recorded in `dropped`.
#[derive(Debug)]
pub struct Corpus {
    pub rules: BTreeMap<String, RuleFile>,
    pub dropped: Vec<(String, String)>,
}

/// Family file names in corpus order (SPEC-006).
const FILES: [(&str, &str); 16] = [
    ("comment-history", COMMENT_HISTORY),
    ("comment-meta", COMMENT_META),
    ("review-jargon", REVIEW_JARGON),
    ("comment-paraphrase", COMMENT_PARAPHRASE),
    ("todo-no-condition", TODO_NO_CONDITION),
    ("perf-claim", PERF_CLAIM),
    ("unsourced-constraint", UNSOURCED_CONSTRAINT),
    ("comment-language", COMMENT_LANGUAGE),
    ("rejected-alternative", REJECTED_ALTERNATIVE),
    ("manual-ddl", MANUAL_DDL),
    ("bare-reference", BARE_REFERENCE),
    ("comment-hedging", COMMENT_HEDGING),
    ("comment-steps", COMMENT_STEPS),
    ("docstring-restates-name", DOCSTRING_RESTATES_NAME),
    ("self-justification", SELF_JUSTIFICATION),
    ("config-deploy-narration", CONFIG_DEPLOY_NARRATION),
];

impl Corpus {
    /// Parse every embedded file. Never fails: a bad file is dropped with
    /// its reason recorded.
    #[must_use]
    pub fn load() -> Corpus {
        let mut rules = BTreeMap::new();
        let mut dropped = Vec::new();
        for (family, text) in FILES {
            match toml::from_str::<RuleFile>(text) {
                Ok(rule) => {
                    rules.insert(family.to_string(), rule);
                }
                Err(err) => dropped.push((family.to_string(), err.to_string())),
            }
        }
        Corpus { rules, dropped }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every embedded rule parses — this is the check that makes the
    /// fail-open drop path unobservable in a shipped binary.
    #[test]
    fn every_rule_file_parses() {
        let corpus = Corpus::load();
        assert!(corpus.dropped.is_empty(), "dropped: {:?}", corpus.dropped);
        assert_eq!(corpus.rules.len(), 16);
    }

    /// SPEC-006: every `error` rule MUST fill `exception` — it is the
    /// definition of the level.
    #[test]
    fn every_error_rule_declares_an_exception() {
        let corpus = Corpus::load();
        for (family, rule) in &corpus.rules {
            if rule.meta.severity == "error" {
                assert!(
                    !rule.exception.shape.is_empty(),
                    "family {family} ships at error with an empty exception (SPEC-006)"
                );
                assert!(
                    !rule.note.text.is_empty(),
                    "family {family} has no actionable note"
                );
            }
        }
    }

    /// Every rule states what to do instead — a finding an agent cannot act
    /// on teaches it to route around the gate.
    #[test]
    fn every_rule_has_an_actionable_note() {
        let corpus = Corpus::load();
        for (family, rule) in &corpus.rules {
            assert!(
                rule.note.text.len() > 40,
                "family {family} note is too thin to act on: {:?}",
                rule.note.text
            );
        }
    }

    /// The shipped severities are the frozen set: three errors, four
    /// warnings, comment-language off; the 2026-09 corpus-research batch
    /// (FINDINGS.md §10) ships five more warnings.
    #[test]
    fn shipped_severities_match_the_frozen_v1_set() {
        let corpus = Corpus::load();
        let level = |f: &str| corpus.rules[f].meta.severity.as_str();
        assert_eq!(level("comment-history"), "error");
        // Demoted 2026-10-03 (TASK-019): zero would_block with these at
        // error over 12 days, and the phrase lists also match ordinary
        // prose. The ratchet's blocking set is comment-history plus the
        // suppression-validity rules.
        assert_eq!(level("comment-meta"), "warning");
        assert_eq!(level("review-jargon"), "warning");
        assert_eq!(level("comment-paraphrase"), "warning");
        assert_eq!(level("todo-no-condition"), "warning");
        assert_eq!(level("perf-claim"), "warning");
        assert_eq!(level("unsourced-constraint"), "warning");
        assert_eq!(level("comment-language"), "off");
        assert_eq!(level("comment-hedging"), "warning");
        assert_eq!(level("comment-steps"), "warning");
        assert_eq!(level("docstring-restates-name"), "warning");
        assert_eq!(level("self-justification"), "warning");
        assert_eq!(level("config-deploy-narration"), "warning");
    }
}
