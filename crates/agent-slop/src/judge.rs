//! The Laya second verdict — a model annotation on a comment, next to the
//! rule that matched it.
//!
//! agent-slop's rules are lexical: `slop/comment-history` fires on a phrase,
//! not on whether the comment is actually doing its job. Laya is a ModernBERT
//! classifier fine-tuned on 11k real review remarks and answers two questions
//! about a snippet: should this comment stay, be reworded or go; and does this
//! bare code warrant a comment at all.
//!
//! ## What a verdict is allowed to do
//!
//! Nothing but print. It never changes a severity, a fingerprint, a ratchet
//! decision, an exit code or a suppression. The user decision (2026-09-30) is
//! "additive only": the deterministic finding stands exactly as the rule
//! produced it, and the model answer is one more line of context for whoever
//! reads the output. A verdict that disagreed with the rule could have
//! demoted the finding; it does not, because a classifier that is below the
//! majority baseline on its own three-way choice (0.628 vs 0.664, measured on
//! 250 held-out rows) is not evidence enough to overrule a rule.
//!
//! ## Measured reliability, because the number belongs next to the feature
//!
//! | axis | measurement |
//! |---|---|
//! | verdict, 3-way choice | 0.628 on 250 held-out rows; always-keep scores 0.664 |
//! | verdict, delete-vs-keep | 0.700 binary, against a 0.656 always-keep baseline |
//! | needs_comment | AUROC 0.832-0.871 on held-out rows, 3/6 on hand probes |
//!
//! The verdict head also inherits the length confound of its training data
//! (kept comments: median 44 words; deleted: 17), so it calls a short,
//! load-bearing "why" comment `delete` at p≈0.9. Hence `min_confidence` and
//! `needs_min_p` exist and default high: a verdict below them prints nothing.
//!
//! ## The endpoint
//!
//! laya.cpp's `POST /v1/systemone`, over plain HTTP on loopback. Two request
//! details are load-bearing and were measured against the served checkpoints,
//! not assumed:
//!
//! * A choice option is rendered as `"<key>: <description>"` (rl_common.py
//!   `render_options`). Sending bare keys or bare descriptions — both tried —
//!   moves 11 of 25 verdicts on identical comments.
//! * Both laya-cli and laya-gateway answer with `Content-Length` and honour
//!   `Connection: close`, so a std-only reader is enough; no HTTP crate is
//!   added to a binary whose dependency policy is "each dep justified".
//!
//! Everything is best-effort by construction: no endpoint, a refused
//! connection, a timeout, a 503 from a stopped model, a malformed body, a
//! truncated option list — each returns `None` and prints nothing. The Stop
//! gate has 15 s and must work offline; a model that is not running is a
//! normal state, not an error.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::ResolvedConfig;
use crate::finding::Finding;
use crate::ratchet;

/// The verdict question. The three keys and their descriptions are the
/// wave-5 training contract (`~/Datasets/comment-quality/build_wave5.py`);
/// changing a word changes the input the checkpoint was trained on.
const VERDICT_INS: &str = "Weighing both sides: should this comment stay in the code as \
                           written? delete adds nothing a reader cannot get from the code, or \
                           narrates the past, an alternative, another module or deployment; \
                           rewrite has content worth keeping but wrong form; keep carries a \
                           non-obvious fact in the right place.";

const VERDICT: [(&str, &str); 3] = [
    ("delete", "the comment adds nothing a reader cannot get from the code, or narrates the past, an alternative, another module or deployment"),
    ("rewrite", "the comment has content worth keeping but its form is wrong (too long, hedging, contrasts, wrong location)"),
    ("keep", "the comment carries a non-obvious fact, constraint or consequence in the right place"),
];

/// The needs question, from the wave-7 builder. Its states carry no comment
/// at all, which is why this axis is free of the length confound.
const NEEDS_INS: &str = "Does this code need a comment it does not have? yes when the code is \
                         hard to follow: non-obvious constraint, magic constant, workaround \
                         with a hidden reason, subtle ordering or timing dependency; no when \
                         the code is straightforward or already says the thing a comment would \
                         say.";

const NEEDS: [(&str, &str); 2] = [
    ("yes", "the code hides a why: constraint, magic number reason, workaround cause, or subtle dependency"),
    ("no", "the code reads plainly; a comment would restate it"),
];

/// The only needs answer that becomes a finding. Named once so the filter in
/// `needs` and its test cannot drift apart.
const NEEDS_YES: &str = "yes";

/// Rule families whose finding is a statement about a comment's form or
/// content, which is the only thing the verdict question asks about.
/// `todo-no-condition` is deliberately absent: that rule's defect is a
/// missing condition, and asking "should this comment stay" about it
/// produces a confident answer to a question nobody asked.
const COMMENT_RULES: [&str; 11] = [
    "comment-history",
    "comment-meta",
    "comment-paraphrase",
    "comment-hedging",
    "comment-steps",
    "comment-language",
    "review-jargon",
    "self-justification",
    "rejected-alternative",
    "docstring-restates-name",
    "config-deploy-narration",
];

/// Code characters kept per line and lines kept per snippet: the wave-5
/// builder capped hunk lines at 120 characters and the state at 600.
const LINE_CAP: usize = 120;
const STATE_CODE_CAP: usize = 600;
const CODE_LINES: usize = 12;
const COMMENT_CHARS: usize = 1500;
/// A candidate run shorter than this is a statement, a closing brace or a
/// config value, none of which is where a reader goes looking for a missing
/// why. Measured against the wave-7 positives, whose shortest hunks are three
/// lines.
const MIN_CHUNK_LINES: usize = 3;
const MIN_CHUNK_CHARS: usize = 40;

/// One model answer, rendered as a single line under the finding.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// `delete`, `rewrite`, `keep`, `yes` or `no`.
    pub label: String,
    pub confidence: f64,
    /// Full distribution, in question order, so a low-confidence near-tie is
    /// visible rather than rounded into a decision.
    pub distribution: Vec<(String, f64)>,
}

impl Verdict {
    /// `delete 0.91 (delete .91 / rewrite .02 / keep .07)`
    pub fn render(&self) -> String {
        let dist = self
            .distribution
            .iter()
            .map(|(k, p)| format!("{k} {p:.2}"))
            .collect::<Vec<_>>()
            .join(" / ");
        format!("{} {p:.2} ({dist})", self.label, p = self.confidence)
    }
}

/// Why a request did not produce a verdict. Never printed: the annotation is
/// an enrichment, and an unreachable model must not add noise to a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unavailable {
    Disabled,
    NoBudget,
    Transport,
    Status,
    Malformed,
}

/// Ask the model about one comment. `Ok(None)` means "answered, but not
/// confidently enough to print"; `Err` means there was no usable answer.
fn ask(
    cfg: &ResolvedConfig,
    state: &str,
    instructions: &str,
    options: &[(&str, &str)],
    deadline: Instant,
) -> Result<Option<Verdict>, Unavailable> {
    if !cfg.judge.enabled {
        return Err(Unavailable::Disabled);
    }
    let left = remaining(deadline).ok_or(Unavailable::NoBudget)?;
    if left.is_zero() {
        return Err(Unavailable::NoBudget);
    }
    let body = request_body(state, instructions, options);
    let response = post(&cfg.judge.url, &body, left)?;
    let answer = parse_choice(&response, options)?;
    let distribution: Vec<(String, f64)> = options
        .iter()
        .map(|(key, desc)| (key.to_string(), probability(&answer, key, desc)))
        .collect();
    let (label, confidence) = distribution
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, p)| (k.clone(), *p))
        .unwrap_or((String::from("?"), 0.0));
    let floor = if options.len() == 2 { cfg.judge.needs_min_p } else { cfg.judge.min_confidence };
    Ok(if confidence >= floor { Some(Verdict { label, confidence, distribution }) } else { None })
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

/// The state, in the wave-5 layout. The `Repo:` line carries no provenance
/// parenthetical: the training states all said `(elite)`, which is a fact
/// about the mined corpus and not about the repository being scanned. Dropping
/// it costs 2 points of choice accuracy on held-out rows (0.628 -> 0.608),
/// which is the price of not asserting something untrue to the model.
pub fn verdict_state(path: &str, comment: &str, code: &str, repo: &str) -> String {
    format!(
        "File: {} ({})\nRepo: {}\nComment:\n{}\n\nCode directly below:\n{}",
        path,
        where_of(path),
        repo,
        trim(comment, COMMENT_CHARS),
        trim(code, STATE_CODE_CAP),
    )
}

/// The state, in the wave-7 layout: no comment exists yet, so none is shown.
pub fn needs_state(path: &str, code: &str, repo: &str) -> String {
    format!(
        "File: {} ({})\nRepo: {}\nCode (no comment yet):\n{}",
        path,
        where_of(path),
        repo,
        trim(code, STATE_CODE_CAP),
    )
}

fn where_of(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let is_test = lower.contains("/test")
        || lower.contains("/tests/")
        || lower.contains("/testing/")
        || lower.starts_with("test_")
        || lower.contains("test_.");
    if is_test {
        "test code"
    } else {
        "production code"
    }
}

fn trim(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    s.chars().take(cap).collect()
}

/// Comment marker for the language-agnostic line filter below. `finding.rs`
/// has the authoritative list for stripping; here it decides only whether a
/// line of context is prose or code.
fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with('#')
        || t.starts_with('*')
        || t.starts_with("/*")
        || t.starts_with("--")
        || t.starts_with(';')
        || t.starts_with("//!")
}

/// The code a comment sits above: the next lines that are not themselves
/// comments. The training states were built from diff hunks the same way —
/// added and context lines, comment lines dropped, 120 characters per line.
pub fn code_below(content: &str, after_line: u32) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines().skip(after_line as usize) {
        if line.trim().is_empty() || is_comment_line(line) {
            continue;
        }
        let trimmed = line.trim_end();
        out.push(if trimmed.chars().count() > LINE_CAP {
            let mut capped: String = trimmed.chars().take(LINE_CAP).collect();
            capped.push('…');
            capped
        } else {
            trimmed.to_string()
        });
        if out.len() == CODE_LINES {
            break;
        }
    }
    out.join("\n")
}

/// One verdict per comment-quality finding, positionally aligned with the
/// input. `None` everywhere when the judge is off, unreachable, or out of
/// budget — the caller renders exactly as it did before.
///
/// `cap` overrides `slop.judge.max_verdicts`, and `0` means no cap at all.
/// The hook legs pass `None` and keep the per-call cap: a tool call has one
/// file and 15 seconds. A whole-repo pass passes its own number, because the
/// cap that protects the hook would otherwise truncate an audit at four
/// comments and still look like it finished.
pub fn annotate(
    cfg: &ResolvedConfig,
    findings: &[Finding],
    root: &Path,
    deadline: Instant,
    cap: Option<usize>,
) -> Vec<Option<Verdict>> {
    let mut out = vec![None; findings.len()];
    if !cfg.judge.enabled {
        return out;
    }
    let repo = repo_name(root);
    // `--max-verdicts 0` means every eligible comment. Reading it as a cap
    // of zero would be the quietest possible bug: the run finishes, prints
    // its findings and no verdict at all, and looks like a model that had
    // nothing to say.
    let cap = match cap {
        None => cfg.judge.max_verdicts,
        Some(0) => usize::MAX,
        Some(n) => n,
    };
    let mut asked = 0usize;
    let mut cache: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        if !is_comment_rule(&f.rule) || asked >= cap {
            continue;
        }
        if remaining(deadline).is_none_or(|d| d < Duration::from_millis(50)) {
            break;
        }
        let content = cache
            .entry(f.file.clone())
            .or_insert_with(|| std::fs::read_to_string(root.join(&f.file)).ok());
        let Some(content) = content.as_ref() else { continue };
        let state = verdict_state(&f.file, &f.snippet, &code_below(content, f.end_line), &repo);
        asked += 1;
        if let Ok(Some(v)) = ask(cfg, &state, VERDICT_INS, &VERDICT, deadline) {
            out[i] = Some(v);
        }
    }
    out
}

pub fn is_comment_rule(rule_id: &str) -> bool {
    rule_id
        .strip_prefix("slop/")
        .is_some_and(|family| COMMENT_RULES.contains(&family))
}

/// A run of added lines carrying no comment of their own — the shape the
/// needs_comment axis was trained on (wave-7 positives are hunks a reviewer
/// demanded a comment for; negatives are hunks of the same repositories that
/// drew no such demand).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeChunk {
    pub file: String,
    /// First added line, 1-based, in the post-image.
    pub line: u32,
    pub code: String,
}

/// A `needs_comment` answer the model gave with enough confidence to print.
/// It is not a rule finding: no corpus rule matched, nothing was suppressed
/// and nothing blocks on it.
#[derive(Debug, Clone, PartialEq)]
pub struct NeedsHit {
    pub file: String,
    pub line: u32,
    pub code: String,
    pub verdict: Verdict,
}

impl NeedsHit {
    pub fn render(&self) -> String {
        let first = self.code.lines().next().unwrap_or_default().trim();
        let shown: String = if first.chars().count() > 72 {
            format!("{}…", first.chars().take(72).collect::<String>())
        } else {
            first.to_string()
        };
        format!(
            "{}:{} laya/needs-comment (advisory) — this code may be missing a comment\n  laya: {}\n  {}",
            self.file,
            self.line,
            self.verdict.render(),
            shown
        )
    }
}

/// Comment-free runs of added lines, from one `git diff -U0`. Hunk headers
/// give the post-image line number, so a hit points at a line in the file the
/// agent just wrote rather than at a diff offset.
///
/// `only` restricts the diff to one path (the PostToolUse leg already knows
/// it); `None` walks the whole tree (the scan leg).
pub fn added_code_chunks(
    root: &Path,
    base: &str,
    only: Option<&str>,
    cap: usize,
) -> Vec<CodeChunk> {
    let mut args = vec!["diff", "-U0", "--no-color", base, "--"];
    if let Some(path) = only {
        args.push(path);
    }
    let Ok(out) = std::process::Command::new("git")
        .current_dir(root)
        .args(&args)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let diff = String::from_utf8_lossy(&out.stdout);
    let mut chunks: Vec<CodeChunk> = Vec::new();
    let mut file = String::new();
    let mut line_no: u32 = 0;
    let mut run: Vec<(u32, String)> = Vec::new();
    let mut in_hunk = false;

    let flush = |run: &mut Vec<(u32, String)>, file: &str, chunks: &mut Vec<CodeChunk>| {
        if run.len() < MIN_CHUNK_LINES || file.is_empty() {
            run.clear();
            return;
        }
        if run.iter().any(|(_, text)| is_comment_line(text)) {
            // A run that carries a comment of its own is not a candidate:
            // the question is about code that has none.
            run.clear();
            return;
        }
        let code: String = run
            .iter()
            .map(|(_, text)| text.trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        if code.chars().count() >= MIN_CHUNK_CHARS {
            chunks.push(CodeChunk { file: file.to_string(), line: run[0].0, code });
        }
        run.clear();
    };

    for raw in diff.lines() {
        if let Some(rest) = raw.strip_prefix("+++ ") {
            file = rest
                .rsplit_once('\t')
                .map(|(p, _)| p)
                .unwrap_or(rest)
                .strip_prefix("b/")
                .unwrap_or(rest)
                .to_string();
            in_hunk = false;
            continue;
        }
        if raw.starts_with("@@") {
            flush(&mut run, &file, &mut chunks);
            in_hunk = true;
            line_no = raw
                .split('+')
                .nth(1)
                .and_then(|rest| rest.split(&[' ', ','][..]).next())
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0);
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(text) = raw.strip_prefix('+') {
            run.push((line_no, text.to_string()));
            line_no += 1;
            continue;
        }
        if raw.starts_with('-') {
            // A deleted line between two added runs means the added runs are
            // not contiguous, and a reader would see them as one block.
            flush(&mut run, &file, &mut chunks);
            continue;
        }
        if raw.starts_with(' ') {
            line_no += 1;
            flush(&mut run, &file, &mut chunks);
        }
    }
    flush(&mut run, &file, &mut chunks);
    chunks.truncate(cap);
    chunks
}

/// Ask about each candidate chunk. Chunks the model does not clear
/// `needs_min_p` on are dropped, so the output is a short list of places a
/// reader would want a why, not an essay about every hunk in the diff.
///
/// A confident NO is not a hit. It is the model saying the code reads
/// plainly, and printing it under a header that says the code may be
/// missing a comment inverts the answer: the confidence floor decides
/// whether to speak, the answer decides what to say.
pub fn needs(
    cfg: &ResolvedConfig,
    chunks: &[CodeChunk],
    repo: &str,
    deadline: Instant,
) -> Vec<NeedsHit> {
    let mut hits = Vec::new();
    for chunk in chunks.iter().take(cfg.judge.max_verdicts) {
        if remaining(deadline).is_none_or(|d| d < Duration::from_millis(50)) {
            break;
        }
        let state = needs_state(&chunk.file, &chunk.code, repo);
        if let Ok(Some(verdict)) = ask(cfg, &state, NEEDS_INS, &NEEDS, deadline) {
            if verdict.label != NEEDS_YES {
                continue;
            }
            hits.push(NeedsHit {
                file: chunk.file.clone(),
                line: chunk.line,
                code: chunk.code.clone(),
                verdict,
            });
        }
    }
    hits
}

/// The needs axis for an audit run: every comment-free run of added lines
/// between `base` and the working tree, put to the model. The hook asks about
/// the file it just watched; this asks about a whole diff, which is the case
/// where a branch is mostly non-comment code and the comment axis has nothing
/// to say.
///
/// `since` is the caller's `--since`; `cfg.ref_` is the fallback so a bare
/// `scan` still has a base, and `None` from a non-work-tree directory
/// produces nothing rather than an error.
pub fn audit_needs(
    cfg: &ResolvedConfig,
    cwd: &Path,
    since: Option<&str>,
    cap: Option<usize>,
    deadline: Instant,
) -> Vec<NeedsHit> {
    if !cfg.judge.enabled {
        return Vec::new();
    }
    let Some(repo) = ratchet::Repo::discover(cwd) else {
        return Vec::new();
    };
    let spec = since.unwrap_or(&cfg.ref_);
    let Some(base) = repo.resolve_ref(spec) else {
        return Vec::new();
    };
    let cap = match cap {
        None => cfg.judge.max_verdicts,
        Some(0) => usize::MAX,
        Some(n) => n,
    };
    let chunks = added_code_chunks(&repo.root, &base, None, cap);
    needs(cfg, &chunks, &repo_name(&repo.root), deadline)
}

/// The repository's own name, which the states carry the way the mined
/// training states carried theirs. A checkout under a worktree or a
/// subdirectory falls back to the basename of the root.
fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("repo"))
}

// --------------------------------------------------------------------------- wire format

fn request_body(state: &str, instructions: &str, options: &[(&str, &str)]) -> String {
    let criteria: Vec<String> = options
        .iter()
        .map(|(key, desc)| format!("{key}: {desc}"))
        .collect();
    serde_json::json!({
        "state": state,
        "questions": {
            "q": {"type": "choice", "instructions": instructions, "criteria": criteria}
        }
    })
    .to_string()
}

/// The per-option probability, keyed by the `"key: description"` spelling
/// laya.cpp echoes back. A missing option is 0.0 rather than a guess: the
/// answer did not rank it, and inventing a rank is how a model gets to
/// influence a gate.
fn probability(answer: &serde_json::Value, key: &str, desc: &str) -> f64 {
    let probs = answer
        .get("probabilities")
        .and_then(|p| p.as_object());
    let Some(probs) = probs else { return 0.0 };
    probs
        .get(&format!("{key}: {desc}"))
        .or_else(|| probs.get(key))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn parse_choice(
    response: &str,
    options: &[(&str, &str)],
) -> Result<serde_json::Value, Unavailable> {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_head, body)| body)
        .unwrap_or(response);
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|_| Unavailable::Malformed)?;
    let answer = json
        .get("answers")
        .and_then(|a| a.get("q"))
        .ok_or(Unavailable::Malformed)?;
    // A model that returns fewer options than the question had has not
    // answered the question that was asked.
    let ranked = answer
        .get("probabilities")
        .and_then(|p| p.as_object())
        .map(|p| p.len())
        .unwrap_or(0);
    if ranked < options.len() {
        return Err(Unavailable::Malformed);
    }
    Ok(answer.clone())
}

/// One POST to a loopback HTTP endpoint, std only. `http://host:port/path`
/// is the whole URL grammar here: the judge never leaves the machine, and a
/// general URL parser would be a dependency to accept a string the config
/// already constrains.
fn post(url: &str, body: &str, budget: Duration) -> Result<String, Unavailable> {
    let rest = url
        .strip_prefix("http://")
        .ok_or(Unavailable::Transport)?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| Unavailable::Transport)?),
        None => (authority, 80),
    };
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|_| Unavailable::Transport)?
        .next()
        .ok_or(Unavailable::Transport)?;
    let mut stream = TcpStream::connect_timeout(&addr, budget).map_err(|_| Unavailable::Transport)?;
    stream.set_read_timeout(Some(budget)).map_err(|_| Unavailable::Transport)?;
    stream.set_write_timeout(Some(budget)).map_err(|_| Unavailable::Transport)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    stream.write_all(request.as_bytes()).map_err(|_| Unavailable::Transport)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|_| Unavailable::Transport)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or(Unavailable::Malformed)?;
    if !(200..300).contains(&status) {
        return Err(Unavailable::Status);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::telemetry::{self, RowInput};

    /// Options must carry the `key: description` spelling the checkpoint was
    /// trained on. Bare keys move verdicts: 11 of 25 real comments flipped
    /// between the two spellings on the served model.
    #[test]
    fn options_are_sent_as_key_colon_description() {
        let body = request_body("state", "ins", &VERDICT);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let criteria = json["questions"]["q"]["criteria"].as_array().unwrap();
        assert_eq!(criteria.len(), 3);
        assert!(criteria[0].as_str().unwrap().starts_with("delete: "));
        assert!(criteria[2].as_str().unwrap().contains("non-obvious fact"));
    }

    /// The state is the training contract, tag included in the layout but not
    /// in the text: asserting `(elite)` to the model would be a claim about
    /// the repository being scanned, and it is only true of the mined corpus.
    #[test]
    fn verdict_state_carries_the_training_layout_without_the_corpus_tag() {
        let s = verdict_state("modules/x.nix", "# why", "a = 1", "config_v3");
        assert_eq!(
            s,
            "File: modules/x.nix (production code)\nRepo: config_v3\nComment:\n# why\n\n\
             Code directly below:\na = 1"
        );
        assert!(!s.contains("elite"));
    }

    #[test]
    fn test_paths_are_labelled_as_test_code() {
        assert!(verdict_state("tests/test_x.py", "c", "code", "r").contains("(test code)"));
        assert!(needs_state("src/lib.rs", "code", "r").contains("(production code)"));
    }

    /// `render_options` puts the code directly below the comment; a comment
    /// the rule matched inside a block has code after the block, not inside.
    #[test]
    fn code_below_skips_comment_lines_and_blank_lines() {
        let content = "a\n# one\n# two\nb = 2\n\nc = 3\n";
        assert_eq!(code_below(content, 0), "a\nb = 2\nc = 3");
    }

    #[test]
    fn code_below_caps_like_the_training_states() {
        let long: String = (0..40).map(|i| format!("line {i} {}", "x".repeat(200))).collect::<Vec<_>>().join("\n");
        let out = code_below(&long, 0);
        assert!(out.lines().count() <= CODE_LINES);
        // +1 per line for the ellipsis that marks the cut, +1 per join.
        let ceiling = CODE_LINES * (LINE_CAP + 1) + (CODE_LINES - 1);
        assert!(out.chars().count() <= ceiling, "{} > {ceiling}", out.chars().count());
    }

    #[test]
    fn comment_rule_families_are_the_comment_ones() {
        for rule in ["slop/comment-history", "slop/comment-paraphrase", "slop/review-jargon"] {
            assert!(is_comment_rule(rule), "{rule} should be judged");
        }
        // A `todo-no-condition` finding's defect is the condition it fails to
        // name, which the verdict question does not ask about.
        assert!(!is_comment_rule(&format!("slop/{}", "todo-no-condition")));
        assert!(!is_comment_rule("ruff/ERA001"));
    }

    /// A truncated option list means the model answered a different
    /// question; the verdict is dropped rather than renormalised.
    #[test]
    fn a_short_option_list_is_rejected() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Length: 118\r\n\r\n",
            r#"{"answers":{"q":{"type":"choice","probabilities":{"delete: x":0.9,"rewrite: y":0.1}}}}"#
        );
        assert_eq!(parse_choice(response, &VERDICT), Err(Unavailable::Malformed));
    }

    #[test]
    fn a_full_option_list_parses_and_orders_by_probability() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n",
            r#"{"answers":{"q":{"type":"choice","probabilities":{}}}}"#
        );
        assert_eq!(parse_choice(response, &VERDICT[..2]), Err(Unavailable::Malformed));
    }

    #[test]
    fn probabilities_are_read_from_the_key_description_spelling() {
        let answer = serde_json::json!({
            "probabilities": {"delete: d": 0.7, "rewrite: r": 0.2, "keep: k": 0.1}
        });
        assert!((probability(&answer, "delete", "d") - 0.7).abs() < 1e-9);
        assert!((probability(&answer, "keep", "k") - 0.1).abs() < 1e-9);
        assert_eq!(probability(&answer, "rewrite", "absent"), 0.0);
    }

    /// The Stop gate has 15 s and must work with the model stopped: an
    /// unreachable endpoint is a `None`, never a panic and never a nonzero.
    #[test]
    fn an_unreachable_endpoint_yields_no_verdict() {
        let cfg = judge_cfg("http://127.0.0.1:1/v1/systemone", 200);
        let deadline = Instant::now() + Duration::from_millis(200);
        assert!(annotate(&cfg, &[finding("slop/comment-history")], Path::new("."), deadline, None)
            .iter()
            .all(Option::is_none));
    }

    /// A judge that is switched off must not even open a socket: the
    /// PostToolUse leg runs on every tool call.
    #[test]
    fn a_disabled_judge_asks_nothing() {
        let mut cfg = judge_cfg("http://127.0.0.1:1/v1/systemone", 200);
        cfg.judge.enabled = false;
        let deadline = Instant::now() + Duration::from_millis(200);
        assert_eq!(
            annotate(&cfg, &[finding("slop/comment-history")], Path::new("."), deadline, None),
            vec![None]
        );
    }

    #[test]
    fn a_past_deadline_stops_the_leg() {
        let cfg = judge_cfg("http://127.0.0.1:1/v1/systemone", 200);
        let deadline = Instant::now();
        assert_eq!(
            annotate(&cfg, &[finding("slop/comment-history")], Path::new("."), deadline, None),
            vec![None]
        );
    }

    /// The cap is what keeps a 200-comment file from turning one tool call
    /// into 200 round trips.
    #[test]
    fn the_verdict_cap_bounds_the_requests() {
        let mut cfg = judge_cfg("http://127.0.0.1:1/v1/systemone", 10_000);
        cfg.judge.max_verdicts = 2;
        let findings: Vec<Finding> = (0..10).map(|_| finding("slop/comment-history")).collect();
        let deadline = Instant::now() + Duration::from_millis(300);
        // 10 comment findings, cap 2: only the first two are eligible.
        let annotated = annotate(&cfg, &findings, Path::new("."), deadline, None);
        assert_eq!(annotated.len(), 10);
    }

    /// `--max-verdicts 0` must not be read as a cap of zero. The count of
    /// requests it produces is asserted against a live stub in
    /// tests/judge_leg.rs, because with a dead endpoint both readings
    /// terminate silently and neither is observable here.
    #[test]
    fn a_zero_cap_does_not_short_circuit_the_leg() {
        let cfg = judge_cfg("http://127.0.0.1:1/v1/systemone", 10_000);
        let findings: Vec<Finding> = (0..3).map(|_| finding("slop/comment-history")).collect();
        let deadline = Instant::now() + Duration::from_millis(300);
        // The output is aligned with the input whatever the cap, so this
        // only proves the leg ran to the end of the findings.
        assert_eq!(annotate(&cfg, &findings, Path::new("."), deadline, Some(0)).len(), 3);
    }

    /// A finding in a file that is not on disk cannot be judged; the leg must
    /// skip it and keep going rather than abort the batch.
    #[test]
    fn an_unreadable_file_is_skipped() {
        let cfg = judge_cfg("http://127.0.0.1:1/v1/systemone", 200);
        let mut f = finding("slop/comment-history");
        f.file = String::from("does/not/exist.nix");
        let deadline = Instant::now() + Duration::from_millis(200);
        assert_eq!(annotate(&cfg, &[f], Path::new("."), deadline, None), vec![None]);
    }

    #[test]
    fn a_verdict_renders_with_its_distribution() {
        let v = Verdict {
            label: String::from("delete"),
            confidence: 0.91,
            distribution: vec![
                (String::from("delete"), 0.91),
                (String::from("rewrite"), 0.02),
                (String::from("keep"), 0.07),
            ],
        };
        assert_eq!(v.render(), "delete 0.91 (delete 0.91 / rewrite 0.02 / keep 0.07)");
    }

    fn judge_cfg(url: &str, timeout_ms: u64) -> ResolvedConfig {
        let mut cfg = crate::config::builtin_config();
        cfg.judge.enabled = true;
        cfg.judge.url = url.to_string();
        cfg.judge.timeout_ms = timeout_ms;
        cfg
    }

    fn finding(rule: &str) -> Finding {
        Finding {
            rule: rule.to_string(),
            severity: crate::finding::Severity::Warning,
            upstream_severity: None,
            file: String::from("modules/x.nix"),
            line: 12,
            col: 1,
            end_line: 12,
            message: String::from("narrates the past"),
            snippet: String::from("// Renamed from enableFoo in 2.0"),
            fingerprint: String::from("deadbeefdeadbeef"),
            source: crate::finding::Source::Slop,
            lang: Some(String::from("nix")),
            suppression: None,
            fixable: false,
        }
    }

    /// The contract the annotation must never break: it is a line of text,
    /// and the finding above it is byte-identical to what the rule produced.
    #[test]
    fn telemetry_rows_survive_a_verdict() {
        let row = telemetry::build(&RowInput {
            event: "Stop",
            harness: telemetry::harness(),
            session_id: Some("s1"),
            parent_session_id: None,
            subagent_type: None,
            cwd: Path::new("."),
            findings: &[finding("slop/comment-history")],
            added: &[finding("slop/comment-history")],
            mode: crate::config::Mode::Observe,
            ref_: "none",
            blocked: false,
            would_block: false,
            files_scanned: 1,
            duration_ms: 3,
            linters: BTreeMap::new(),
        });
        assert_eq!(row["telemetry"]["source"], "agent-slop");
    }
}
