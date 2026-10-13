//! `slop/comment-paraphrase` — the structural family. Deciding that a
//! comment restates the statement below it requires comparing the comment's
//! tokens against the neighbouring AST node's identifiers; a phrase list
//! cannot do that, and that is why this engine is ast-grep-based.
//!
//! The comparison: tokenize the comment into lowercase words, drop
//! stopwords and short tokens, and count how many distinct tokens equal
//! (or are contained in) identifiers of the next code construct. At
//! `min_shared_identifiers` or more shared tokens over a comment of at
//! least `min_comment_words` words, the comment is a restatement.

use super::corpus::RuleFile;
use crate::lang::Comment;

/// Words too generic to count as overlap with the code below.
fn is_stopword(w: &str, stopwords: &[String]) -> bool {
    stopwords.iter().any(|s| s == w) || w.len() < 3
}

fn tokens(text: &str, stopwords: &[String]) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty() && !is_stopword(w, stopwords))
        .collect()
}

/// Split identifiers into lowercase sub-tokens on camelCase and `_` so
/// `parse_request` matches "parse the request".
fn ident_tokens(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in ids {
        let mut cur = String::new();
        for c in id.chars() {
            if c == '_' || c == '-' || c == '.' {
                if !cur.is_empty() {
                    out.push(cur.to_lowercase());
                    cur = String::new();
                }
            } else if c.is_uppercase() {
                if !cur.is_empty() {
                    out.push(cur.to_lowercase());
                    cur = String::new();
                }
                cur.push(c);
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            out.push(cur.to_lowercase());
        }
    }
    out
}

/// Rust `//!` / `/*! ... */` inner doc comments document the enclosing item,
/// not the statement below.
fn is_inner_doc(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("//!") || t.starts_with("/*!")
}

/// Map a comment word through the glossary pairs: "порт" -> "port" when the
/// rule carries `["порт", "port", ...]`. Unpaired words pass through.
/// Prefix match down to 5 Cyrillic characters covers most inflections
/// ("сервиса"/"сервису" both start with "сервис"), which is also why the
/// pairs are ordered longest-first in the TOML.
fn gloss(word: &str, glossary: &[String]) -> String {
    glossary
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .find_map(|pair| {
            let ru = &pair[0];
            let n = ru.chars().count();
            let stem: String = word.chars().take(n.min(6)).collect();
            let head: String = ru.chars().take(n.min(6)).collect();
            (stem == head && n >= 3).then(|| pair[1].clone())
        })
        .unwrap_or_else(|| word.to_string())
}

/// True when the comment restates the code below it, per the rule's
/// thresholds. Returns the shared tokens (for the message) when it fires.
#[must_use]
pub fn paraphrase_hit(rule: &RuleFile, comment: &Comment) -> Option<Vec<String>> {
    let m = &rule.r#match;
    let min_shared = m.min_shared_identifiers.unwrap_or(2);
    let min_words = m.min_comment_words.unwrap_or(3);

    // `//!` documents the ENCLOSING module, not the statement below it.
    if is_inner_doc(&comment.text) {
        return None;
    }

    // A comment giving a cause, a consequence or a source explains why the
    // code is shaped this way; overlap with the names below is then
    // expected. Identifier overlap alone flagged exactly these comments.
    // The exemption is cancelled when the why is about something this
    // module cannot know (deploy mechanics, another service) or argues
    // permission: those comments were flagged by reviewers regardless of
    // their why-content (gpt4 corpus, FINDINGS.md §10.2).
    if super::lex::any_phrase(&m.why_exemption_cancels, &comment.text).is_none()
        && (super::lex::any_phrase(&m.why_markers, &comment.text).is_some()
            || super::lex::has_referent(&comment.text))
    {
        return None;
    }

    let words: Vec<String> = tokens(&comment.text, &m.stopwords)
        .iter()
        .map(|w| gloss(w, &m.ru_glossary))
        .collect();
    if words.len() < min_words || comment.next_code_ids.is_empty() {
        return None;
    }

    let code = ident_tokens(&comment.next_code_ids);
    let mut shared: Vec<String> = Vec::new();
    for w in &words {
        if code.iter().any(|c| c == w) && !shared.contains(w) {
            shared.push(w.clone());
        }
    }
    if shared.len() >= min_shared {
        Some(shared)
    } else {
        None
    }
}

/// `slop/docstring-restates-name`: a docstring whose content is already
/// the name of the declaration it documents. Two shapes fire:
/// (a) the whole comment's substantive words are all words of the name;
/// (b) the HEAD — the clause before the first `:`, the definition slot
/// ("Провайдер эффектов Higgsfield: адаптер к пресетам") — is all name
/// words even when an elaborating tail follows. Headings at or below
/// `min_comment_words` are conventional (Go doc comments open with the
/// name) and stay out.
#[must_use]
pub fn name_only_docstring(rule: &RuleFile, comment: &Comment) -> Option<Vec<String>> {
    let m = &rule.r#match;
    let min_words = m.min_comment_words.unwrap_or(2);
    let min_head = m.min_head_words.unwrap_or(3);

    if is_inner_doc(&comment.text) {
        return None;
    }
    if comment.next_code_ids.is_empty() {
        return None;
    }
    // A `#` heading over an assignment is a section banner — a table of
    // contents line, conventional in Nix and YAML files; the rule judges
    // docstrings (python `"""` string statements) and colon-headed
    // definition slots, where the restatement verdict is unambiguous.
    let is_docstring = comment.text.starts_with("\"\"\"") || comment.text.starts_with("'''");
    // A dashed banner ("── Section: detail ──") is a table-of-contents
    // line; the rule judges prose, not file layout.
    let banner = comment.text.trim_start().starts_with("──")
        || comment.text.trim_start().starts_with("--")
        || comment.text.trim_start().starts_with("==");
    if banner || (!is_docstring && !comment.text.contains(':')) {
        return None;
    }
    let code = ident_tokens(&comment.next_code_ids);
    let glossed = |text: &str| -> Vec<String> {
        tokens(text, &m.stopwords)
            .iter()
            .map(|w| gloss(w, &m.ru_glossary))
            .collect()
    };

    let shared_with = |words: &[String]| -> Option<Vec<String>> {
        // `effect` matches the identifier's `effects`: English plurals
        // otherwise defeat exact-token equality for one-word gaps.
        let same_word = |w: &str, c: &str| c == w || c.strip_suffix('s') == Some(w);
        let all_in_name = words.iter().all(|w| code.iter().any(|c| same_word(w, c)));
        let mut shared: Vec<String> = Vec::new();
        for w in words {
            if !shared.contains(w) {
                shared.push(w.clone());
            }
        }
        (all_in_name && !shared.is_empty()).then_some(shared)
    };

    // (b) the definition slot before the first colon.
    let head = comment
        .text
        .split(':')
        .next()
        .unwrap_or(&comment.text)
        .to_string();
    let head_words = glossed(&head);
    if head_words.len() >= min_head
        && let Some(shared) = shared_with(&head_words)
    {
        return Some(shared);
    }

    // (a) the whole comment is nothing but the name.
    let words = glossed(&comment.text);
    if words.len() > min_words
        && let Some(shared) = shared_with(&words)
    {
        return Some(shared);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Comment;

    fn rule() -> RuleFile {
        toml::from_str(include_str!("../../rules/comment-paraphrase.toml")).unwrap()
    }

    fn comment(text: &str, ids: &[&str]) -> Comment {
        Comment {
            text: text.to_string(),
            line: 1,
            col: 1,
            end_line: 1,
            next_code_ids: ids.iter().map(std::string::ToString::to_string).collect(),
        }
    }

    #[test]
    fn restatement_of_the_statement_below_fires() {
        // The shape that forced the engine choice: the comment names the
        // very identifiers the code below declares.
        let c = comment(
            "// Iterate over the users and filter the active users list",
            &["iterate_users", "filter_active_users"],
        );
        assert!(paraphrase_hit(&rule(), &c).is_some());
    }

    #[test]
    fn why_comment_does_not_fire() {
        // Explains a constraint, shares only one token with the code.
        let c = comment(
            "// The upstream API rejects captions over 1024 bytes, so truncate before send",
            &["send_caption"],
        );
        assert!(paraphrase_hit(&rule(), &c).is_none());
    }

    /// Real 'why' comments from a PR scan that the overlap count flagged.
    #[test]
    fn a_cause_or_a_source_exempts_the_comment() {
        for (text, ids) in [
            (
                "# without a persisted session every acquire gets a new exit IP",
                &["acquire", "session", "persisted"][..],
            ),
            (
                "# из-за h2c upgrade uvicorn теряет body, и FastAPI отвечает 422 на request body",
                &["request", "body", "uvicorn"][..],
            ),
            (
                "# proxy pool size is capped per POL-001 proxy pool rules",
                &["proxy_pool_size", "pool_rules"][..],
            ),
            (
                "# editable install needs the hook, see editables#20 editable install",
                &["editable_install", "hook"][..],
            ),
        ] {
            assert!(
                paraphrase_hit(&rule(), &comment(text, ids)).is_none(),
                "{text}"
            );
        }
    }

    /// A Russian comment naming the English identifiers below is the same
    /// restatement; the glossary pairs translate the overlap back.
    #[test]
    fn russian_restating_comment_fires_through_the_glossary() {
        let r = rule();
        for (text, ids) in [
            ("# порт сервиса для прокси", &["port", "service"][..]),
            (
                "# лимит запросов в секунду",
                &["rate_limit", "request_limit"][..],
            ),
            ("# имя сервиса биллинга", &["service_name"][..]),
        ] {
            let c = comment(text, ids);
            assert!(paraphrase_hit(&r, &c).is_some(), "{text}");
        }
        // A Russian comment with a cause is still exempt.
        let c = comment("# порт, иначе проксируется мимо", &["port"][..]);
        assert!(paraphrase_hit(&r, &c).is_none());
        // A Russian word not in the glossary does not fake overlap.
        let c = comment("# расписание запуска", &["port"][..]);
        assert!(paraphrase_hit(&r, &c).is_none());
    }

    #[test]
    fn no_code_below_never_fires() {
        let c = comment("// Returns the parsed request from the parser", &[]);
        assert!(paraphrase_hit(&rule(), &c).is_none());
    }

    #[test]
    fn short_comments_never_fire() {
        let c = comment("// parse request", &["parse_request"]);
        assert!(paraphrase_hit(&rule(), &c).is_none());
    }

    #[test]
    fn camel_case_identifiers_split_into_tokens() {
        let c = comment(
            "// Parse the incoming request payload here",
            &["parseIncomingRequestPayload"],
        );
        // shared: parse, incoming(?), request, payload — at least parse,
        // request, payload >= 2.
        assert!(paraphrase_hit(&rule(), &c).is_some());
    }
}
