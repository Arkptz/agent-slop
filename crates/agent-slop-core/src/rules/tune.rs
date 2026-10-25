//! `[slop.tune.<family>]`: per-family adjustments to the embedded corpus.
//!
//! List keys extend an embedded list (`phrases_extend`, ...) or remove
//! exact entries from the phrase list (`phrases_remove`); both concatenate
//! across tiers, lowest first. Numeric keys replace per key and are
//! range-checked against a per-family allowlist. Nothing here replaces a
//! whole list: that path is a custom rule plus the family set to `off`,
//! which is visible as a demotion.
//!
//! A bad family, key, value or entry records a note for the tier that
//! wrote it and is ignored; the rest of the table still applies.

use std::collections::BTreeMap;

use toml::Value;

use super::corpus::{EMBEDDED, RuleFile, RuleSet};
use crate::config::{ConfigNote, KNOWN_FAMILIES, Tier};

/// The accumulated adjustments for one family.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tune {
    pub phrases_extend: Vec<String>,
    pub phrases_remove: Vec<String>,
    pub anchored_markers_extend: Vec<String>,
    pub markers_extend: Vec<String>,
    pub why_markers_extend: Vec<String>,
    pub stopwords_extend: Vec<String>,
    pub why_exemption_cancels_extend: Vec<String>,
    pub min_shared_identifiers: Option<usize>,
    pub min_shared_ratio: Option<f64>,
    pub max_block_lines: Option<usize>,
    pub min_comment_words: Option<usize>,
    pub min_head_words: Option<usize>,
}

impl Tune {
    /// What makes the family match less: removed phrases and any
    /// threshold change. `self-test` fails a frozen blocking family that
    /// carries any of these.
    #[must_use]
    pub fn weakening(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.phrases_remove.is_empty() {
            out.push(format!(
                "phrases_remove ({})",
                self.phrases_remove.join(", ")
            ));
        }
        let numeric: [(&str, Option<String>); 5] = [
            (
                "min_shared_identifiers",
                self.min_shared_identifiers.map(|v| v.to_string()),
            ),
            (
                "min_shared_ratio",
                self.min_shared_ratio.map(|v| v.to_string()),
            ),
            (
                "max_block_lines",
                self.max_block_lines.map(|v| v.to_string()),
            ),
            (
                "min_comment_words",
                self.min_comment_words.map(|v| v.to_string()),
            ),
            ("min_head_words", self.min_head_words.map(|v| v.to_string())),
        ];
        for (key, value) in numeric {
            if let Some(value) = value {
                out.push(format!("{key} = {value}"));
            }
        }
        out
    }

    /// Entries the tune adds to embedded lists, for the info line.
    #[must_use]
    pub fn extensions(&self) -> usize {
        self.phrases_extend.len()
            + self.anchored_markers_extend.len()
            + self.markers_extend.len()
            + self.why_markers_extend.len()
            + self.stopwords_extend.len()
            + self.why_exemption_cancels_extend.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Tune::default()
    }
}

/// Inclusive bounds of a numeric key for a family, or `None` when the
/// family does not expose it.
fn numeric_range(family: &str, key: &str) -> Option<(f64, f64)> {
    Some(match (family, key) {
        ("comment-paraphrase", "min_shared_identifiers") => (1.0, 10.0),
        ("comment-paraphrase", "min_shared_ratio") => (0.0, 1.0),
        ("comment-paraphrase", "max_block_lines" | "min_comment_words") => (1.0, 50.0),
        ("bare-reference", "min_comment_words") | ("docstring-restates-name", "min_head_words") => {
            (1.0, 20.0)
        }
        _ => return None,
    })
}

const LIST_KEYS: [&str; 7] = [
    "phrases_extend",
    "phrases_remove",
    "anchored_markers_extend",
    "markers_extend",
    "why_markers_extend",
    "stopwords_extend",
    "why_exemption_cancels_extend",
];

const NUMERIC_KEYS: [&str; 5] = [
    "min_shared_identifiers",
    "min_shared_ratio",
    "max_block_lines",
    "min_comment_words",
    "min_head_words",
];

/// The embedded list a list key refers to.
fn base_list<'a>(rule: &'a RuleFile, key: &str) -> &'a [String] {
    let m = &rule.r#match;
    match key {
        "phrases_extend" | "phrases_remove" => &m.phrases,
        "anchored_markers_extend" => &m.anchored_markers,
        "markers_extend" => &m.markers,
        "why_markers_extend" => &m.why_markers,
        "stopwords_extend" => &m.stopwords,
        "why_exemption_cancels_extend" => &m.why_exemption_cancels,
        _ => &[],
    }
}

fn target<'a>(tune: &'a mut Tune, key: &str) -> Option<&'a mut Vec<String>> {
    Some(match key {
        "phrases_extend" => &mut tune.phrases_extend,
        "phrases_remove" => &mut tune.phrases_remove,
        "anchored_markers_extend" => &mut tune.anchored_markers_extend,
        "markers_extend" => &mut tune.markers_extend,
        "why_markers_extend" => &mut tune.why_markers_extend,
        "stopwords_extend" => &mut tune.stopwords_extend,
        "why_exemption_cancels_extend" => &mut tune.why_exemption_cancels_extend,
        _ => return None,
    })
}

fn string_list(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// Fold one tier's `[slop.tune]` table into the accumulated tunes.
pub fn merge_tier(
    tunes: &mut BTreeMap<String, Tune>,
    raw: BTreeMap<String, Value>,
    tier: Tier,
    notes: &mut Vec<ConfigNote>,
) {
    let embedded = RuleSet::embedded();
    let mut note = |reason: String| notes.push(ConfigNote { tier, reason });

    for (family, table) in raw {
        if !KNOWN_FAMILIES.contains(&family.as_str()) {
            note(format!("[slop.tune.{family}] ignored: no such rule family"));
            continue;
        }
        let Some(table) = table.as_table() else {
            note(format!("[slop.tune.{family}] ignored: expected a table"));
            continue;
        };
        let Some(base) = embedded.corpus.rules.get(&family) else {
            continue;
        };
        let entry = tunes.entry(family.clone()).or_default();

        for (key, value) in table {
            if let Some(list) = LIST_KEYS
                .contains(&key.as_str())
                .then_some(())
                .and(Some(key))
            {
                let Some(items) = string_list(value) else {
                    note(format!(
                        "[slop.tune.{family}] {list} ignored: expected a list of strings"
                    ));
                    continue;
                };
                if base_list(base, list).is_empty() {
                    note(format!(
                        "[slop.tune.{family}] {list} ignored: the family has no such list"
                    ));
                    continue;
                }
                if list == "phrases_remove" {
                    for item in &items {
                        let known = base.r#match.phrases.contains(item)
                            || entry.phrases_extend.contains(item);
                        if !known {
                            note(format!(
                                "[slop.tune.{family}] phrases_remove entry `{item}` is not in \
                                 the family's phrase list"
                            ));
                        }
                    }
                }
                if let Some(dst) = target(entry, list) {
                    dst.extend(items);
                }
            } else if NUMERIC_KEYS.contains(&key.as_str()) {
                let Some((lo, hi)) = numeric_range(&family, key) else {
                    note(format!(
                        "[slop.tune.{family}] {key} ignored: not tunable for this family"
                    ));
                    continue;
                };
                #[allow(clippy::cast_precision_loss)]
                let number = match value {
                    Value::Integer(i) => Some(*i as f64),
                    Value::Float(f) => Some(*f),
                    _ => None,
                };
                let Some(number) = number.filter(|n| (lo..=hi).contains(n)) else {
                    note(format!(
                        "[slop.tune.{family}] {key} ignored: expected a number in {lo}..={hi}"
                    ));
                    continue;
                };
                set_numeric(entry, key, number);
            } else {
                note(format!("[slop.tune.{family}] unknown key `{key}` ignored"));
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn set_numeric(tune: &mut Tune, key: &str, n: f64) {
    match key {
        "min_shared_identifiers" => tune.min_shared_identifiers = Some(n as usize),
        "min_shared_ratio" => tune.min_shared_ratio = Some(n),
        "max_block_lines" => tune.max_block_lines = Some(n as usize),
        "min_comment_words" => tune.min_comment_words = Some(n as usize),
        "min_head_words" => tune.min_head_words = Some(n as usize),
        _ => {}
    }
}

fn extend_unique(dst: &mut Vec<String>, extra: &[String]) {
    for item in extra {
        if !dst.contains(item) {
            dst.push(item.clone());
        }
    }
}

/// Apply a tune to one parsed embedded rule.
pub fn apply(rule: &mut RuleFile, tune: &Tune) {
    let m = &mut rule.r#match;
    extend_unique(&mut m.phrases, &tune.phrases_extend);
    m.phrases.retain(|p| !tune.phrases_remove.contains(p));
    extend_unique(&mut m.anchored_markers, &tune.anchored_markers_extend);
    extend_unique(&mut m.markers, &tune.markers_extend);
    extend_unique(&mut m.why_markers, &tune.why_markers_extend);
    extend_unique(&mut m.stopwords, &tune.stopwords_extend);
    extend_unique(
        &mut m.why_exemption_cancels,
        &tune.why_exemption_cancels_extend,
    );
    if let Some(v) = tune.min_shared_identifiers {
        m.min_shared_identifiers = Some(v);
    }
    if let Some(v) = tune.min_shared_ratio {
        m.min_shared_ratio = Some(v);
    }
    if let Some(v) = tune.max_block_lines {
        m.max_block_lines = Some(v);
    }
    if let Some(v) = tune.min_comment_words {
        m.min_comment_words = Some(v);
    }
    if let Some(v) = tune.min_head_words {
        m.min_head_words = Some(v);
    }
}

/// The embedded sources, for rebuilding a tuned ruleset.
#[must_use]
pub fn embedded_sources() -> &'static [(&'static str, &'static str)] {
    &EMBEDDED
}

#[cfg(test)]
mod tests {
    use crate::config::{builtin_config, resolve};

    fn fires(user_tier: &str, comment: &str, family: &str) -> bool {
        let cfg = resolve(Some(user_tier), None);
        crate::rules::scan_source(&cfg, "m.py", &format!("{comment}\nx = 1\n"))
            .iter()
            .any(|f| f.rule == format!("slop/{family}"))
    }

    #[test]
    fn phrases_extend_adds_a_hit() {
        let comment = "# it must be hand run before deploy: RENAME TABLE a TO b";
        assert!(
            !fires("", comment, "manual-ddl"),
            "control: the phrase is not in the corpus"
        );
        assert!(fires(
            "[slop.tune.manual-ddl]\nphrases_extend = [\"hand run before deploy\"]\n",
            comment,
            "manual-ddl"
        ));
    }

    #[test]
    fn phrases_remove_removes_a_hit() {
        let comment = "# run by hand before the deploy: RENAME TABLE a TO b";
        assert!(fires("", comment, "manual-ddl"), "control: it fires");
        assert!(!fires(
            "[slop.tune.manual-ddl]\nphrases_remove = [\"run by hand\"]\n",
            comment,
            "manual-ddl"
        ));
    }

    #[test]
    fn threshold_out_of_range_is_noted_and_ignored() {
        let cfg = resolve(
            Some("[slop.tune.comment-paraphrase]\nmin_shared_ratio = 5.0\n"),
            None,
        );
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.reason.contains("expected a number in 0..=1")),
            "{:?}",
            cfg.notes
        );
        assert_eq!(
            cfg.ruleset.corpus.rules["comment-paraphrase"]
                .r#match
                .min_shared_ratio,
            Some(0.3),
            "the embedded value stands"
        );
    }

    #[test]
    fn extend_lists_concatenate_across_tiers() {
        let cfg = resolve(
            Some("[slop.tune.manual-ddl]\nphrases_extend = [\"alpha deploy step\"]\n"),
            Some("[slop.tune.manual-ddl]\nphrases_extend = [\"beta deploy step\"]\n"),
        );
        let phrases = &cfg.ruleset.corpus.rules["manual-ddl"].r#match.phrases;
        assert!(phrases.contains(&"alpha deploy step".to_string()));
        assert!(phrases.contains(&"beta deploy step".to_string()));
        assert!(
            cfg.notes.is_empty(),
            "valid tunes record nothing: {:?}",
            cfg.notes
        );
    }

    #[test]
    fn unknown_family_key_and_entry_are_noted() {
        let cfg = resolve(
            Some(
                "[slop.tune]\nno-such-family = { phrases_extend = [\"x\"] }\n\
                 [slop.tune.comment-history]\nno_such_key = 1\nphrases_remove = [\"not a phrase here\"]\n",
            ),
            None,
        );
        for needle in [
            "no such rule family",
            "unknown key `no_such_key`",
            "is not in the family's phrase list",
        ] {
            assert!(
                cfg.notes.iter().any(|n| n.reason.contains(needle)),
                "{needle}: {:?}",
                cfg.notes
            );
        }
    }

    /// A threshold change alters what a scan can produce, so it must
    /// alter the ruleset hash (the baseline cache key's rules half).
    #[test]
    fn a_tune_change_alters_the_ruleset_hash() {
        let base = builtin_config();
        let tuned = resolve(
            Some("[slop.tune.comment-paraphrase]\nmax_block_lines = 3\n"),
            None,
        );
        assert_ne!(base.ruleset.hash, tuned.ruleset.hash);
    }

    #[test]
    fn a_numeric_key_on_a_family_without_it_is_noted() {
        let cfg = resolve(
            Some("[slop.tune.perf-claim]\nmin_shared_ratio = 0.5\n"),
            None,
        );
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.reason.contains("not tunable for this family")),
            "{:?}",
            cfg.notes
        );
    }
}
