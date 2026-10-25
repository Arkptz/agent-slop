//! The scan engine: comments from `lang::extract` → `Finding`s via the
//! corpus, with line-scoped suppressions applied (carried, never dropped).
//!
//! Severity comes from the resolved config (`severity_for`), not from the
//! TOML's shipped level — the TOML level is the built-in tier only.

pub mod condition;
pub mod corpus;
pub mod custom;
pub mod generic;
pub mod lex;
pub mod paraphrase;
pub mod suppression;
pub mod tune;

use crate::config::ResolvedConfig;
use crate::finding::{Finding, Severity, Source, Suppression, fingerprint};
use crate::lang::{self, Comment};
use corpus::RuleFile;

/// Scan one already-read file. `file` is the repo-relative display path.
/// Never fails: an unparsable source yields zero comments (tree-sitter
/// error nodes carry no comment kinds worth reporting) and unknown
/// languages yield nothing.
#[must_use]
pub fn scan_source(cfg: &ResolvedConfig, file: &str, source: &str) -> Vec<Finding> {
    let Some(spec) = lang::spec_for_path(file) else {
        return Vec::new();
    };
    let corpus = &cfg.ruleset.corpus;
    let customs = &cfg.ruleset.custom;
    let comments = lang::extract(spec, source);
    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();

    for (idx, comment) in comments.iter().enumerate() {
        let block = block_text(&comments, idx);
        // A comment carrying an allow directive is checked for validity; a
        // STANDALONE allow line is skipped as content (it suppresses the
        // next line), but a content comment with a trailing directive is
        // scanned normally and suppressed by its own line.
        if let Some(allow) = suppression::parse_allow(&comment.text) {
            if !allow.valid {
                findings.push(meta_finding(
                    file,
                    comment,
                    allow.violation.unwrap_or(suppression::VACUOUS_RULE),
                    &format!(
                        "invalid suppression: {} — reason must carry a referent \
                         (URL, ADR-064, owner/repo#1, path/to, date:2026-09-21)",
                        if allow.reason.is_empty() {
                            "no -- <reason> given".into()
                        } else {
                            allow.reason.clone()
                        }
                    ),
                    cfg,
                ));
            }
            if suppression::is_standalone_allow(&comment.text) {
                continue;
            }
        }
        let paraphrase = paraphrase_block(&comments, idx);
        for custom in customs {
            if let Some(finding) =
                evaluate_custom(cfg, custom, spec.name, file, comment, &block, &lines)
            {
                findings.push(finding);
            }
        }
        for rule in corpus.rules.values() {
            if rule.meta.severity == "off" && cfg.severity_for(&rule_id(&rule.meta.id)).is_none() {
                continue;
            }
            if let Some(finding) = evaluate_rule(
                cfg,
                rule,
                file,
                comment,
                &block,
                paraphrase.as_ref(),
                &lines,
            ) {
                findings.push(finding);
            }
        }
    }
    findings
}

fn rule_id(family: &str) -> String {
    format!("slop/{family}")
}

/// One custom rule against one comment: scope, severity (`off` skips),
/// the generic matcher, then line-scoped suppression like any other rule.
fn evaluate_custom(
    cfg: &ResolvedConfig,
    custom: &custom::LoadedRule,

    lang_name: &str,
    file: &str,
    comment: &Comment,
    block: &str,
    lines: &[&str],
) -> Option<Finding> {
    let severity = cfg.severity_for(&custom.id)?;
    if !generic::applies(&custom.rule, lang_name, file, comment) {
        return None;
    }
    let unit = if generic::is_block(&custom.rule) {
        block
    } else {
        &comment.text
    };
    let matched = generic::evaluate(&custom.rule, &custom.regexes, unit)?;
    let mut finding = Finding {
        rule: custom.id.clone(),
        severity,
        upstream_severity: None,
        file: file.to_string(),
        line: comment.line,
        col: comment.col,
        end_line: comment.end_line,
        message: generic::message(&custom.rule, &matched),
        snippet: comment.text.chars().take(512).collect(),
        fingerprint: fingerprint(&custom.id, &comment.text),
        source: Source::Slop,
        lang: None,
        suppression: None,
        fixable: true,
    };
    apply_suppression(&mut finding, lines);
    Some(finding)
}

fn meta_finding(
    file: &str,
    comment: &Comment,
    rule: &str,
    message: &str,
    cfg: &ResolvedConfig,
) -> Finding {
    let severity = cfg.severity_for(rule).unwrap_or(Severity::Error);
    Finding {
        rule: rule.to_string(),
        severity,
        upstream_severity: None,
        file: file.to_string(),
        line: comment.line,
        col: comment.col,
        end_line: comment.end_line,
        message: message.to_string(),
        snippet: comment.text.chars().take(512).collect(),
        fingerprint: fingerprint(rule, &comment.text),
        source: Source::Slop,
        lang: None,
        suppression: None,
        fixable: true,
    }
}

/// Two comments belong to one paragraph when the second starts on the line
/// the first ends on (or the next) in the same column.
fn adjacent(a: &Comment, b: &Comment) -> bool {
    // `end_line` counts the trailing newline, and grammars disagree on
    // whether a one-line comment's node ends on its own line or the next:
    // rust `///` ends on the next, python `#` on its own. Accept both.
    (b.line == a.end_line || b.line == a.end_line + 1) && a.col == b.col
}

/// The text of the run of adjacent line comments containing `comments[idx]`.
/// Grammars that emit one node per `#` line split a paragraph into lines, and
/// a rule counting a comment's own words must see the whole paragraph.
fn block_text(comments: &[Comment], idx: usize) -> String {
    let mut start = idx;
    while start > 0 && adjacent(&comments[start - 1], &comments[start]) {
        start -= 1;
    }
    let mut end = idx;
    while end + 1 < comments.len() && adjacent(&comments[end], &comments[end + 1]) {
        end += 1;
    }
    comments[start..=end]
        .iter()
        .map(|c| lex::payload(&c.text))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The comment paragraph `comment-paraphrase` judges as one unit.
struct ParaphraseBlock {
    text: String,
    lines: usize,
}

/// The paragraph led by `comments[idx]`, or `None` when `idx` is not its
/// first line: the verdict is one finding per paragraph, anchored at the
/// leader. Standalone allow lines never join a paragraph; a python
/// docstring is a paragraph of its own.
fn paraphrase_block(comments: &[Comment], idx: usize) -> Option<ParaphraseBlock> {
    let lead = &comments[idx];
    if lead.is_docstring {
        return Some(ParaphraseBlock {
            text: lex::payload(&lead.text).to_string(),
            lines: lead.text.trim_end().lines().count(),
        });
    }
    let member = |c: &Comment| !c.is_docstring && !suppression::is_standalone_allow(&c.text);
    if !member(lead) {
        return None;
    }
    if idx > 0 && member(&comments[idx - 1]) && adjacent(&comments[idx - 1], &comments[idx]) {
        return None;
    }
    let mut end = idx;
    while end + 1 < comments.len()
        && member(&comments[end + 1])
        && adjacent(&comments[end], &comments[end + 1])
    {
        end += 1;
    }
    let run = &comments[idx..=end];
    Some(ParaphraseBlock {
        text: run
            .iter()
            .map(|c| lex::payload(&c.text))
            .collect::<Vec<_>>()
            .join(" "),
        lines: run.iter().map(|c| c.text.trim_end().lines().count()).sum(),
    })
}

fn evaluate_rule(
    cfg: &ResolvedConfig,
    rule: &RuleFile,
    file: &str,
    comment: &Comment,
    block: &str,
    paraphrase: Option<&ParaphraseBlock>,
    lines: &[&str],
) -> Option<Finding> {
    let id = rule_id(&rule.meta.id);
    let severity = cfg.severity_for(&id)?;
    let m = &rule.r#match;

    let hit_message: Option<String> = match rule.meta.id.as_str() {
        "comment-history" => lex::phrase_hit(m, &comment.text)
            .map(|p| format!("change-history residue '{p}' — narrates the edit, not the code")),
        "comment-meta" => lex::phrase_hit(m, &comment.text)
            .map(|p| format!("session residue '{p}' — deferral with no owner")),
        "review-jargon" => lex::anchored_marker_hit(m, &comment.text)
            .map(|p| format!("review marker '{p}' frozen into the source")),
        "comment-paraphrase" => paraphrase
            .and_then(|b| paraphrase::paraphrase_hit(rule, comment, &b.text, b.lines))
            .map(|shared| format!("restates the code below ({})", shared.join(", "))),
        "todo-no-condition" => {
            let marker = lex::marker_present(m, &comment.text)?;
            let has_condition = m
                .condition_patterns
                .iter()
                .any(|pat| condition::matches_condition(pat, &comment.text));
            if has_condition {
                None
            } else {
                Some(format!(
                    "{marker} with no removal condition — name one or delete it"
                ))
            }
        }
        "perf-claim" => {
            let phrase = lex::phrase_hit(m, &comment.text)?;
            if m.requires_no_digit && lex::has_digit(&comment.text) {
                None
            } else {
                Some(format!(
                    "perf claim '{phrase}' without a measurement — cite it or drop it"
                ))
            }
        }
        "unsourced-constraint" => {
            let phrase = lex::phrase_hit(m, &comment.text)?;
            if m.requires_no_referent && lex::has_referent(&comment.text) {
                None
            } else {
                Some(format!(
                    "constraint '{phrase}' names no source — name the product, bug, or version"
                ))
            }
        }
        "rejected-alternative" => lex::rejected_alternative_hit(m, &comment.text)
            .map(|p| format!("'{p}' defines the code by an alternative that is not there")),
        "manual-ddl" => {
            // The marker and the statement are often on different lines
            // of one paragraph (`TODO manual migration:` then `RENAME TABLE a TO b`).
            lex::marker_present(m, block)
                .or_else(|| lex::any_phrase(&m.phrases, block).cloned())?;
            lex::any_phrase(&m.ddl, block).map(|stmt| {
                format!(
                    "'{}' left for a human — the deploy never runs it",
                    stmt.to_uppercase()
                )
            })
        }
        "bare-reference" => {
            // Counting only the words outside the trailing parens is what
            // makes this a "say it in words" verdict; `block` matters when
            // the id ends its own line and the words sit on earlier ones.
            lex::bare_reference(m, block)
                .map(|id| format!("'{id}' stands in for the explanation — say the rule in words"))
        }
        "comment-hedging" => lex::phrase_hit(m, &comment.text)
            .map(|p| format!("hedging '{p}' — doubt about shipped code, not a fact")),
        "comment-steps" => lex::anchored_marker_hit(m, &comment.text)
            .map(|p| format!("step narration '{p}' — the control flow is the code")),
        "docstring-restates-name" => {
            // Token overlap against the following declaration decides:
            // prose that survives only on that declaration's own
            // vocabulary carries nothing new.
            paraphrase::name_only_docstring(rule, comment).map(|shared| {
                format!(
                    "restates the name below ({}) — add a fact or delete",
                    shared.join(", ")
                )
            })
        }
        "self-justification" => lex::phrase_hit(m, &comment.text)
            .map(|p| format!("self-justification '{p}' — argues permission nobody asked for")),
        "config-deploy-narration" => lex::phrase_hit(m, &comment.text)
            .map(|p| format!("deploy narration '{p}' — mechanics this file cannot enforce")),
        // comment-language is reserved: no script-detection matcher ships.
        _ => None,
    };

    let message = hit_message?;
    let mut finding = Finding {
        rule: id.clone(),
        severity,
        upstream_severity: None,
        file: file.to_string(),
        line: comment.line,
        col: comment.col,
        end_line: comment.end_line,
        message,
        snippet: comment.text.chars().take(512).collect(),
        fingerprint: fingerprint(&id, &comment.text),
        source: Source::Slop,
        lang: None,
        suppression: None,
        fixable: true,
    };

    apply_suppression(&mut finding, lines);
    Some(finding)
}

/// Line-scoped suppression: the same line, or the previous non-empty line
/// when a standalone allow line precedes this comment. A suppressed
/// finding is carried with its `Suppression`, never dropped.
pub fn apply_suppression(finding: &mut Finding, lines: &[&str]) {
    let target = finding.line as usize; // 1-based
    let candidates = [
        lines.get(target.wrapping_sub(1)).copied(),
        prev_non_empty(lines, target.wrapping_sub(1)),
    ];
    for line in candidates.into_iter().flatten() {
        let Some(allow) = suppression::parse_allow(line) else {
            continue;
        };
        if allow.rule == finding.rule {
            finding.suppression = Some(Suppression {
                rule: allow.rule.clone(),
                reason: allow.reason.clone(),
                valid: allow.valid,
            });
            return;
        }
    }
}

fn prev_non_empty<'a>(lines: &[&'a str], before_1based: usize) -> Option<&'a str> {
    let mut i = before_1based;
    while i > 0 {
        i -= 1;
        let line = lines.get(i)?;
        if !line.trim().is_empty() {
            return Some(line);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::builtin_config;

    #[test]
    fn history_finding_on_harvested_comment() {
        let cfg = builtin_config();
        let f = scan_source(
            &cfg,
            "a.py",
            "# We previously always sent a Referer — that was a bug.\nreferer = None\n",
        );
        assert!(
            f.iter()
                .any(|x| x.rule == "slop/comment-history" && x.severity == Severity::Error)
        );
    }

    #[test]
    fn valid_suppression_carries_the_finding() {
        let cfg = builtin_config();
        let src = "# agent-slop: allow slop/comment-history -- migration shim owned by ADR-031\n\
                   # We previously always sent a Referer.\nreferer = None\n";
        let f = scan_source(&cfg, "a.py", src);
        let hist: Vec<_> = f
            .iter()
            .filter(|x| x.rule == "slop/comment-history")
            .collect();
        assert_eq!(hist.len(), 1, "carried, not dropped");
        assert!(hist[0].suppression.as_ref().is_some_and(|s| s.valid));
    }

    /// Calibration S1: a two-line restating block is one finding anchored at
    /// its first line, not one per physical line.
    #[test]
    fn paraphrase_is_one_finding_per_comment_block() {
        let cfg = builtin_config();
        let src =
            "// retry after header\n// parse retry after value\nfn parse_retry_after_header() {}\n";
        let hits: Vec<_> = scan_source(&cfg, "a.rs", src)
            .into_iter()
            .filter(|x| x.rule == "slop/comment-paraphrase")
            .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 1);
    }

    /// Calibration S1: the why-marker on the second line exempts the block.
    #[test]
    fn paraphrase_why_marker_on_a_later_line_exempts_the_block() {
        let cfg = builtin_config();
        let fires = |src: &str| {
            scan_source(&cfg, "a.rs", src)
                .iter()
                .any(|x| x.rule == "slop/comment-paraphrase")
        };
        assert!(fires(
            "// retry after header parse\nfn parse_retry_after_header() {}\n"
        ));
        assert!(!fires(
            "// retry after header parse\n// because the proxy drops it\nfn parse_retry_after_header() {}\n"
        ));
    }

    #[test]
    fn vacuous_suppression_raises_meta_rule() {
        let cfg = builtin_config();
        let src = "# agent-slop: allow slop/comment-history -- because I say so okay\n\
                   # We previously always sent a Referer.\nreferer = None\n";
        let f = scan_source(&cfg, "a.py", src);
        assert!(f.iter().any(|x| x.rule == "slop/suppression-vacuous"));
        let hist: Vec<_> = f
            .iter()
            .filter(|x| x.rule == "slop/comment-history")
            .collect();
        assert_ne!(hist, [] as [&crate::finding::Finding; 0]);
        assert!(hist[0].suppression.as_ref().is_some_and(|s| !s.valid));
    }

    #[test]
    fn same_line_suppression_applies() {
        let cfg = builtin_config();
        let src = "# For now serial-only (agent-slop: allow slop/comment-meta -- bounded by ADR-041)\nx = 1\n";
        let f = scan_source(&cfg, "a.py", src);
        let meta: Vec<_> = f.iter().filter(|x| x.rule == "slop/comment-meta").collect();
        assert_eq!(meta.len(), 1);
        assert!(meta[0].suppression.as_ref().is_some_and(|s| s.valid));
    }

    /// Docstrings carry the same prose as comments; a history phrase there
    /// was invisible because only `#` comment nodes were walked.
    #[test]
    fn a_docstring_is_scanned_like_a_comment() {
        let cfg = builtin_config();
        let src = "def mark_unhealthy(p):\n    \"\"\"Mark the proxy.\n\n    Previously this also reset\n    the cooldown.\n    \"\"\"\n    p.bad = True\n";
        let f = scan_source(&cfg, "a.py", src);
        let hist: Vec<_> = f
            .iter()
            .filter(|x| x.rule == "slop/comment-history")
            .collect();
        assert_eq!(hist.len(), 1, "{f:?}");
        assert_eq!(hist[0].line, 2);
        // A string that is not the first statement is data, not a docstring.
        let data = "def f():\n    x = 1\n    \"previously cached\"\n";
        assert_eq!(
            scan_source(&cfg, "a.py", data),
            [] as [crate::finding::Finding; 0]
        );
    }

    #[test]
    fn russian_history_and_deferral_inflections_fire() {
        let cfg = builtin_config();
        for (text, rule) in [
            (
                "# раньше возвращал None, теперь пустой список",
                "slop/comment-history",
            ),
            ("# переименовано из proxy_bind", "slop/comment-history"),
            ("# Раньше этот импорт ронял бота", "slop/comment-history"),
            (
                "# --- original 6 solver tests (behavioral assertions unchanged) ---",
                "slop/comment-history",
            ),
            ("# Пока оставим так, потом разберёмся", "slop/comment-meta"),
        ] {
            let f = scan_source(&cfg, "a.py", &format!("{text}\nx = 1\n"));
            assert!(f.iter().any(|x| x.rule == rule), "{text}: {f:?}");
        }
        let clean = scan_source(
            &cfg,
            "a.py",
            "# раньше полуночи задача не стартует\nx = 1\n",
        );
        assert!(
            clean.iter().all(|x| x.rule != "slop/comment-history"),
            "{clean:?}"
        );
    }

    fn with_family(family: &str, level: crate::config::RuleLevel) -> ResolvedConfig {
        let mut cfg = builtin_config();
        cfg.rules.insert(family.to_string(), level);
        cfg
    }

    /// Real comments from gpt4telegram PRs, where the project rules forbid
    /// the contrast shape; the negatives are affirmative rewrites and a
    /// quoted example.
    #[test]
    fn rejected_alternative_fires_when_a_project_enables_it() {
        let cfg = with_family("rejected-alternative", crate::config::RuleLevel::Warning);
        let fires = |src: &str| {
            scan_source(&cfg, "a.py", &format!("{src}\nx = 1\n"))
                .iter()
                .any(|f| f.rule == "slop/rejected-alternative")
        };
        for hit in [
            "# По два count-запроса на элемент enum вместо group-by: эндпоинт низкочастотный",
            "# Повтор списывает по версии исходной задачи, а не по текущему выбору",
            "# Same tag replaces the shown notification instead of stacking another one.",
            "# Fixed on desktop, not a minimum: switching the mode must not move the row.",
            "# Правила аллокации — конфигурация, а не секрет",
        ] {
            assert!(fires(hit), "{hit}");
        }
        for miss in [
            "# Чтение обрывается на потолке: размер тела задаёт чужой сервер",
            "# Forms like `X, not Y` describe a state that is not in the code",
            "# Keys are revoked and never deleted, so audit rows keep their owner",
            "# Это не баг: апстрим отдаёт 201 без тела",
        ] {
            assert!(!fires(miss), "{miss}");
        }
        assert!(
            scan_source(&builtin_config(), "a.py", "# X вместо Y\nx = 1\n")
                .iter()
                .all(|f| f.rule != "slop/rejected-alternative"),
            "ships off"
        );
    }

    #[test]
    fn manual_ddl_needs_a_human_step_and_a_statement() {
        let cfg = builtin_config();
        let rules = |src: &str| -> Vec<String> {
            scan_source(&cfg, "m.py", &format!("{src}\nx = 1\n"))
                .into_iter()
                .map(|f| f.rule)
                .collect()
        };
        assert!(
            rules("# TODO ручная миграция: RENAME TABLE a TO b")
                .contains(&"slop/manual-ddl".into())
        );
        assert!(
            rules("# Run manually before deploy: alter table x add column y int")
                .contains(&"slop/manual-ddl".into())
        );
        // Marker and statement on different lines of one paragraph.
        assert!(
            rules("# TODO manual migration before deploy:\n#   RENAME TABLE a TO b")
                .contains(&"slop/manual-ddl".into())
        );
        assert!(
            !rules("# TODO: drop the fallback once v2 ships").contains(&"slop/manual-ddl".into())
        );
        assert!(
            !rules("# The migrator runs CREATE TABLE IF NOT EXISTS itself")
                .contains(&"slop/manual-ddl".into())
        );
    }

    #[test]
    fn bare_reference_needs_own_words_not_just_an_id() {
        let cfg = builtin_config();
        let fires = |src: &str| {
            scan_source(&cfg, "a.py", &format!("{src}\nx = 1\n"))
                .iter()
                .any(|f| f.rule == "slop/bare-reference")
        };
        assert!(fires("# see POL-002"));
        assert!(!fires(
            "# Fernet keys never leave the host and are rotated\n# yearly (ADR-003)."
        ));
        assert!(fires("# Step-by-step generation (KAN-621)"));
        assert!(!fires(
            "# Keys are Fernet-encrypted at rest, never logged (ADR-003)"
        ));
        assert!(!fires("# UTF-8 only"));
        // The exception the rule declares: the sentence states the rule,
        // the id only points at the doc (exarch store/mod.rs:1938 shape).
        // A Rust doc paragraph: the id sits on its own trailing line, and
        // the words live on the lines above (the adjacency case).
        assert!(!fires(
            "# an asserted operator only ever touches a run it owns (ADR-018 C4)"
        ));
        let doc = "/// The relay-asserted READ/WRITE dispatch\n/// verbs ownership-gate against this so an asserted operator touches\n/// a run it owns (ADR-018 C4).\nfn f() {}\n";
        assert!(
            scan_source(&cfg, "a.rs", doc)
                .iter()
                .all(|f| f.rule != "slop/bare-reference")
        );
        assert!(!fires("# GPT-6 Luna"));
        assert!(!fires("# no ids here at all"));
    }

    #[test]
    fn comment_language_ships_off() {
        let cfg = builtin_config();
        let f = scan_source(
            &cfg,
            "a.py",
            "# Привет, это русский комментарий без референса\nx = 1\n",
        );
        assert!(f.is_empty(), "off means no findings: {f:?}");
    }
}
