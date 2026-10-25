//! The generic matcher behind custom rules: dispatch on `match.kind`,
//! reusing the `lex` primitives. Embedded families keep their hard-coded
//! dispatch in `rules/mod.rs`.

use regex::{Regex, RegexBuilder};

use super::corpus::RuleFile;
use super::{condition, lex};
use crate::lang::Comment;

/// Every `match.kind` a custom rule may use.
pub const KNOWN_KINDS: [&str; 4] = [
    "phrase",
    "anchored-marker",
    "marker-without-condition",
    "regex",
];

/// Compiled-regex ceiling per pattern. Rust's engine is linear-time, so
/// this bounds memory, not CPU.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Check a rule's `[match]` section and compile its regexes.
///
/// # Errors
///
/// The reason the rule is dropped: an unknown `kind`, an empty match
/// list, a bad `unit`/`targets` value, or a rejected pattern.
pub fn compile(rule: &RuleFile) -> Result<Vec<Regex>, String> {
    let m = &rule.r#match;
    let kind = m.kind.as_deref().unwrap_or_default();
    if !KNOWN_KINDS.contains(&kind) {
        return Err(format!(
            "unknown match.kind `{kind}` (known: {})",
            KNOWN_KINDS.join(", ")
        ));
    }
    let (list, field) = match kind {
        "phrase" => (&m.phrases, "phrases"),
        "anchored-marker" => (&m.anchored_markers, "anchored_markers"),
        "marker-without-condition" => (&m.markers, "markers"),
        _ => (&m.patterns, "patterns"),
    };
    if list.is_empty() {
        return Err(format!(
            "match.kind `{kind}` needs a non-empty `{field}` list"
        ));
    }
    if kind == "marker-without-condition" && m.condition_patterns.is_empty() {
        return Err("match.kind `marker-without-condition` needs `condition_patterns`".into());
    }
    match m.unit.as_deref() {
        None | Some("comment" | "block") => {}
        Some(other) => return Err(format!("match.unit `{other}` is not comment|block")),
    }
    for target in &rule.meta.targets {
        if !matches!(target.as_str(), "comment" | "docstring") {
            return Err(format!("meta.targets `{target}` is not comment|docstring"));
        }
    }
    if kind != "regex" {
        return Ok(Vec::new());
    }
    m.patterns
        .iter()
        .map(|p| {
            RegexBuilder::new(p)
                .size_limit(REGEX_SIZE_LIMIT)
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("pattern `{p}` rejected: {e}"))
        })
        .collect()
}

/// Whether the rule's scope covers this file and comment.
#[must_use]
pub fn applies(rule: &RuleFile, lang: &str, file: &str, comment: &Comment) -> bool {
    let meta = &rule.meta;
    if !meta.langs.is_empty() && !meta.langs.iter().any(|l| l == lang) {
        return false;
    }
    if !meta.targets.is_empty() {
        let kind = if comment.is_docstring {
            "docstring"
        } else {
            "comment"
        };
        if !meta.targets.iter().any(|t| t == kind) {
            return false;
        }
    }
    if !meta.paths.is_empty() && !meta.paths.iter().any(|p| crate::config::glob_path(p, file)) {
        return false;
    }
    !meta
        .exclude_paths
        .iter()
        .any(|p| crate::config::glob_path(p, file))
}

/// True when the rule judges the whole comment paragraph.
#[must_use]
pub fn is_block(rule: &RuleFile) -> bool {
    rule.r#match.unit.as_deref() == Some("block")
}

/// The matched text when `text` trips the rule, `None` otherwise. `text`
/// is a comment, or a paragraph for `unit = "block"`.
#[must_use]
pub fn evaluate(rule: &RuleFile, regexes: &[Regex], text: &str) -> Option<String> {
    let m = &rule.r#match;
    let matched = match m.kind.as_deref()? {
        "phrase" => lex::phrase_hit(m, text)?,
        "anchored-marker" => lex::anchored_marker_hit(m, text)?,
        "marker-without-condition" => {
            let marker = lex::marker_present(m, text)?;
            if m.condition_patterns
                .iter()
                .any(|pat| condition::matches_condition(pat, text))
            {
                return None;
            }
            marker
        }
        "regex" => {
            let clean = lex::without_code_spans(lex::payload(text));
            regexes
                .iter()
                .find_map(|re| re.find(&clean).map(|hit| hit.as_str().to_string()))?
        }
        _ => return None,
    };
    if !m.requires_phrases.is_empty() && lex::any_phrase(&m.requires_phrases, text).is_none() {
        return None;
    }
    if lex::any_phrase(&m.why_markers, text).is_some() {
        return None;
    }
    if m.requires_no_digit && lex::has_digit(text) {
        return None;
    }
    if m.requires_no_referent && lex::has_referent(text) {
        return None;
    }
    Some(matched)
}

/// The finding message: the rule's `message` with `{match}` substituted,
/// or a generic line naming the rule.
#[must_use]
pub fn message(rule: &RuleFile, matched: &str) -> String {
    match rule.r#match.message.as_deref() {
        Some(template) => template.replace("{match}", matched),
        None => format!("{} matched '{matched}'", rule.meta.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(body: &str) -> RuleFile {
        let text = format!("[meta]\nid = \"acme/t\"\nschema = 1\nseverity = \"warning\"\n{body}");
        let mut rule: RuleFile = toml::from_str(&text).expect("test rule parses");
        rule.r#match.word_boundaries = true;
        rule
    }

    fn hit(rule: &RuleFile, text: &str) -> Option<String> {
        let regexes = compile(rule).expect("compiles");
        evaluate(rule, &regexes, text)
    }

    #[test]
    fn phrase_kind_hits_and_misses() {
        let r = rule("[match]\nkind = \"phrase\"\nphrases = [\"placeholder implementation\"]\n");
        assert!(hit(&r, "# placeholder implementation, real one later").is_some());
        assert!(hit(&r, "# a real implementation").is_none());
    }

    #[test]
    fn anchored_marker_kind_hits_and_misses() {
        let r = rule("[match]\nkind = \"anchored-marker\"\nanchored_markers = [\"wip:\"]\n");
        assert!(hit(&r, "# wip: half done").is_some());
        assert!(hit(&r, "# this is wip: nothing").is_none());
    }

    #[test]
    fn marker_without_condition_kind_hits_and_misses() {
        let r = rule(
            "[match]\nkind = \"marker-without-condition\"\nmarkers = [\"TODO\"]\n\
             condition_patterns = [\"#\\\\d+\"]\n",
        );
        assert!(hit(&r, "# TODO: tidy this").is_some());
        assert!(hit(&r, "# TODO(#12): tidy this").is_none());
    }

    #[test]
    fn regex_kind_hits_and_misses() {
        let r = rule("[match]\nkind = \"regex\"\npatterns = ['\\bfor now\\b']\n");
        assert_eq!(hit(&r, "# Kept FOR NOW").as_deref(), Some("FOR NOW"));
        assert!(hit(&r, "# fornow is a word").is_none());
    }

    #[test]
    fn regex_over_the_size_limit_is_rejected() {
        let r = rule("[match]\nkind = \"regex\"\npatterns = ['(\\w{100}){100}']\n");
        let err = compile(&r).expect_err("size limit must reject");
        assert!(err.contains("rejected"), "{err}");
    }

    #[test]
    fn kind_dispatch_rejects_unknown_kind() {
        let r = rule("[match]\nkind = \"fuzzy\"\nphrases = [\"x\"]\n");
        let err = compile(&r).expect_err("unknown kind must be rejected");
        assert!(err.contains("unknown match.kind"), "{err}");
    }

    #[test]
    fn guards_exempt_a_hit() {
        let r = rule(
            "[match]\nkind = \"phrase\"\nphrases = [\"faster\"]\nrequires_no_digit = true\n\
             why_markers = [\"because\"]\n",
        );
        assert!(hit(&r, "# faster").is_some());
        assert!(hit(&r, "# 3x faster").is_none());
        assert!(hit(&r, "# faster because it skips the copy").is_none());
    }

    #[test]
    fn requires_phrases_is_a_second_list_that_must_also_match() {
        let r = rule(
            "[match]\nkind = \"phrase\"\nphrases = [\"manual\"]\nrequires_phrases = [\"migration\"]\n",
        );
        assert!(hit(&r, "# manual migration step").is_some());
        assert!(hit(&r, "# manual step").is_none());
    }

    #[test]
    fn langs_scoping_excludes_another_language() {
        let mut r = rule("[match]\nkind = \"phrase\"\nphrases = [\"x\"]\n");
        r.meta.langs = vec!["python".into()];
        let comment = Comment {
            text: "# x".into(),
            line: 1,
            col: 1,
            end_line: 1,
            next_code_ids: Vec::new(),
            is_docstring: false,
        };
        assert!(applies(&r, "python", "a.py", &comment));
        assert!(!applies(&r, "rust", "a.rs", &comment));
    }

    #[test]
    fn message_substitutes_the_match() {
        let mut r = rule("[match]\nkind = \"phrase\"\nphrases = [\"x\"]\n");
        r.r#match.message = Some("found '{match}'".into());
        assert_eq!(message(&r, "x"), "found 'x'");
    }
}
