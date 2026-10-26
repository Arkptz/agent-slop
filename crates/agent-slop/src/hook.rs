//! Hook dispatch (SPEC-006 "CLI surface and exit codes", "The `PostToolUse`
//! advisory channel", "Payload key spellings", "Stop dispatch
//! preconditions").
//!
//! Every field is declared with both harness spellings: grok's envelope is
//! camelCase where Claude Code's is `snake_case`, and a parse written for
//! one reads `null` under the other — a hook that reports healthy while
//! scanning nothing.
//!
//! The only nonzero this module may produce is the deliberate Stop block,
//! exit 2. Unreadable stdin, unparsable JSON, an unknown event, no git, an
//! expired deadline: all exit 0.
//!
//! This module writes no files. A leg that has telemetry to record returns
//! the built row in [`Outcome::row`] and [`run`] appends it, so the whole
//! protocol stays assertable without a filesystem.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::elapsed_ms;

use crate::ratchet::{self, Cache, Repo};
use agent_slop_core::config::{self, ResolvedConfig};
use agent_slop_core::finding::{Finding, Severity};
use agent_slop_core::rules::{corpus::RuleSet, scan_source};
use agent_slop_integrations::telemetry::{self, RowInput};

/// At most three advice lines reach the model per tool call; more is a
/// wall of text an agent skims past.
const ADVICE_LINES: usize = 3;

/// Per-rule render ceiling inside the Stop message.
const PER_RULE_CEILING: usize = 5;

/// Both spellings are declared as SEPARATE fields, never as `#[serde(alias)]`
/// on one: grok's envelope carries BOTH — the real `PostToolUse` payload holds
/// `hookEventName` and `hook_event_name`, `toolName` and `tool_name`,
/// `sessionId` and `session_id` at once — and serde rejects a canonical field
/// and its alias in the same object with `duplicate field`. Under an alias the
/// whole parse fails, `dispatch` takes the error branch, and every leg becomes
/// a silent no-op at exit 0 on the harness the aliases were added for.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ToolInput {
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default, rename = "filePath")]
    pub file_path_camel: Option<String>,
}

impl ToolInput {
    fn path(&self) -> Option<&str> {
        self.file_path
            .as_deref()
            .or(self.file_path_camel.as_deref())
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HookPayload {
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default, rename = "hookEventName")]
    pub hook_event_name_camel: Option<String>,
    // Parsed and tested rather than consumed: the leg dispatches on the
    // presence of a file path, not on the tool's name, and the key
    // spellings are covered by their own test.
    #[allow(dead_code)]
    #[serde(default)]
    pub tool_name: Option<String>,
    #[allow(dead_code)]
    #[serde(default, rename = "toolName")]
    pub tool_name_camel: Option<String>,
    #[serde(default)]
    pub tool_input: Option<ToolInput>,
    #[serde(default, rename = "toolInput")]
    pub tool_input_camel: Option<ToolInput>,
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    #[serde(default, rename = "stopHookActive")]
    pub stop_hook_active_camel: Option<bool>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default, rename = "sessionId")]
    pub session_id_camel: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub subagent_type: Option<String>,
    #[serde(default, rename = "subagentType")]
    pub subagent_type_camel: Option<String>,
}

impl HookPayload {
    /// The event name under either spelling. grok fills both with DIFFERENT
    /// values — `hook_event_name` carries Claude's `PostToolUse`, while
    /// `hookEventName` carries grok's own `post_tool_use` — so the caller
    /// compares through [`event_is`], which is blind to that difference.
    #[cfg(test)]
    pub(crate) fn event(&self) -> &str {
        self.hook_event_name
            .as_deref()
            .or(self.hook_event_name_camel.as_deref())
            .unwrap_or_default()
    }

    /// Does the payload name `want` (spelled `posttooluse`, lowercase, no
    /// separators)? Compares BOTH fields, because only one of grok's two may
    /// be the vocabulary this binary knows, and folds away case and the
    /// underscores that make `post_tool_use` differ from `PostToolUse`.
    pub(crate) fn event_is(&self, want: &str) -> bool {
        [
            self.hook_event_name.as_deref(),
            self.hook_event_name_camel.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|name| squash(name) == want)
    }

    pub(crate) fn stop_hook_active(&self) -> Option<bool> {
        self.stop_hook_active.or(self.stop_hook_active_camel)
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id
            .as_deref()
            .or(self.session_id_camel.as_deref())
    }

    pub(crate) fn subagent_type(&self) -> Option<&str> {
        self.subagent_type
            .as_deref()
            .or(self.subagent_type_camel.as_deref())
    }

    pub(crate) fn file_path(&self) -> Option<&str> {
        self.tool_input
            .as_ref()
            .and_then(ToolInput::path)
            .or_else(|| self.tool_input_camel.as_ref().and_then(ToolInput::path))
    }
}

/// Lowercase and drop `_`/`-`, so one event is one token whichever harness
/// spelled it: `PostToolUse`, `post_tool_use` and `posttooluse` all fold to
/// `posttooluse`.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A dispatch result, decoupled from the process so the whole protocol is
/// testable: exit code plus exactly what would be written where. `row` is
/// built but not written — [`run`] is the only place that touches disk,
/// and its absence is how "this leg did not scan" is asserted.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub code: u8,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub row: Option<serde_json::Value>,
}

impl Outcome {
    fn allow() -> Outcome {
        Outcome::default()
    }
}

/// Read stdin, dispatch, write, and return the process exit code.
pub(crate) fn run() -> u8 {
    let mut buf = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).is_err() {
        return 0; // unreadable stdin carries no event
    }
    let outcome = dispatch(&buf, &std::env::current_dir().unwrap_or_default());
    for line in &outcome.stdout {
        println!("{line}");
    }
    for line in &outcome.stderr {
        eprintln!("{line}");
    }
    // A failed write is swallowed inside `telemetry::write`: the gate's
    // verdict never depends on the recording of it.
    if let Some(row) = &outcome.row {
        let root = std::env::var_os("AGENT_SLOP_TELEMETRY_ROOT").map(std::path::PathBuf::from);
        let _ = telemetry::write(row, root.as_deref());
    }
    outcome.code
}

pub(crate) fn dispatch(payload_text: &str, cwd: &std::path::Path) -> Outcome {
    // A payload with no recognisable shape is not an error.
    let Ok(payload) = serde_json::from_str::<HookPayload>(payload_text) else {
        return Outcome::allow();
    };
    let cfg = config::load();

    if payload.event_is("posttooluse") {
        return post_tool_use(&cfg, &payload, cwd);
    }
    if payload.event_is("sessionstart") {
        return session_start(&cfg, &payload, cwd);
    }
    if payload.event_is("stop") {
        return stop(&cfg, &payload, cwd);
    }
    Outcome::allow()
}

fn deadline(cfg: &ResolvedConfig) -> Option<Instant> {
    Instant::now().checked_add(Duration::from_millis(cfg.timeout_ms))
}

/// Advice reaches the model as `additionalContext` on stdout at exit 0.
/// Exit 2 is the other spelling that reaches the model and is rejected
/// here: it keeps a reason but DROPS `additionalContext`, and 2 is
/// reserved for the Stop gate's deliberate block.
fn post_tool_use(cfg: &ResolvedConfig, payload: &HookPayload, cwd: &std::path::Path) -> Outcome {
    let started = Instant::now();
    // A Bash call edits no file; the latency budget is met by leaving now.
    let Some(rel) = payload.file_path() else {
        return Outcome::allow();
    };
    let path = std::path::Path::new(rel);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let Ok(content) = std::fs::read_to_string(&full) else {
        return Outcome::allow();
    };
    let mut all = scan_source(cfg, rel, &content);
    // Plugins are advisory here: cost A only, a 300 ms ceiling, and a
    // failure is a stderr note and a telemetry status, never advice and
    // never an exit code. The Stop gate does not run them.
    let plugins = hook_plugins(cfg, cwd, rel, &content, deadline(cfg));
    all.extend(plugins.findings);
    let findings: Vec<Finding> = all
        .iter()
        .filter(|f| f.suppression.is_none())
        .cloned()
        .collect();
    let plugin_notes: Vec<String> = plugins
        .skipped
        .iter()
        .map(|s| format!("agent-slop: plugin {} skipped: {}", s.tool, s.reason))
        .collect();

    // Every scanned file produces a row, findings or not: POL-026 rule 4
    // wants the absence of this leg to be observable, and a leg that only
    // records when it has something to say is indistinguishable from one
    // that never fires.
    let row = telemetry::build(&RowInput {
        event: "PostToolUse",
        harness: telemetry::harness(),
        session_id: payload.session_id(),
        parent_session_id: None,
        subagent_type: payload.subagent_type(),
        cwd,
        findings: &all,
        added: &findings,
        mode: cfg.mode,
        // The advisory leg compares against nothing; it reports the file
        // as it stands.
        ref_: "none",
        blocked: false,
        would_block: false,
        files_scanned: 1,
        duration_ms: elapsed_ms(started),
        linters: plugins.status,
    });

    if findings.is_empty() && !cfg.judge.enabled {
        return Outcome {
            row: Some(row),
            stderr: plugin_notes,
            ..Outcome::allow()
        };
    }

    let mut lines: Vec<String> = Vec::new();
    // One second verdict per comment the rules matched, computed before the
    // text is assembled so the annotation lands under its own finding. The
    // envelope format is unchanged: the model adds lines, not structure.
    let judged = agent_slop_integrations::judge::annotate(
        cfg,
        &findings,
        cwd,
        deadline(cfg).unwrap_or_else(Instant::now),
        None,
    );
    for (i, f) in findings.iter().take(ADVICE_LINES).enumerate() {
        lines.push(format!("{}:{} {} — {}", f.file, f.line, f.rule, f.message));
        if let Some(Some(v)) = judged.get(i) {
            lines.push(format!("  laya: {}", v.render()));
        }
    }
    if findings.len() > ADVICE_LINES {
        lines.push(format!(
            "+{} more — run agent-slop check {}",
            findings.len() - ADVICE_LINES,
            rel
        ));
    }
    // The second axis fires without a rule: code the agent just added that
    // reads as if it were hiding a why. It is advisory by construction — no
    // rule matched, so there is nothing to suppress and nothing to block on.
    lines.extend(needs_advice(cfg, cwd, rel));
    if lines.is_empty() {
        return Outcome {
            row: Some(row),
            stderr: plugin_notes,
            ..Outcome::allow()
        };
    }
    let advice = lines.join("\n");
    let envelope = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": advice,
        }
    });
    Outcome {
        code: 0,
        stdout: vec![envelope.to_string()],
        stderr: plugin_notes,
        row: Some(row),
    }
}

/// External plugins for the file the agent just wrote. Hook stage: cost A
/// only and a hard 300 ms ceiling on top of `timeout_ms` and the leg's
/// own deadline, so a slow plugin costs its annotation, never the turn.
fn hook_plugins(
    cfg: &ResolvedConfig,
    cwd: &std::path::Path,
    rel: &str,
    content: &str,
    leg_deadline: Option<Instant>,
) -> agent_slop_integrations::plugins::PluginRun {
    use agent_slop_integrations::plugins::{DescribeCache, Opts, Selection, Stage, run};
    if cfg.plugins.is_empty() {
        return agent_slop_integrations::plugins::PluginRun::default();
    }
    let root = Repo::discover(cwd).map_or_else(|| cwd.to_path_buf(), |r| r.root);
    // The path as the plugin sees it is repo-relative; the hook payload may
    // hand an absolute one.
    let named = std::path::Path::new(rel)
        .strip_prefix(&root)
        .map_or_else(|_| rel.to_string(), |p| p.to_string_lossy().into_owned());
    run(
        cfg,
        &Opts {
            selection: &Selection::Auto,
            stage: Stage::Hook,
            deadline: leg_deadline,
            attributable_deadline: None,
        },
        &root,
        &[(named, content.to_string())],
        &DescribeCache::from_env(),
    )
}

/// The `needs_comment` half of the second verdict, for the file the agent
/// just wrote. It needs a resolved base ref, so it runs only inside a
/// work tree; outside one, or with the model off, it contributes nothing.
///
/// The chunk budget is what keeps this bounded: the diff of a large file is
/// hundreds of hunks and each one is a round trip, so the run is capped at
/// `max_verdicts` candidates and stops at the leg's deadline.
fn needs_advice(cfg: &ResolvedConfig, cwd: &std::path::Path, rel: &str) -> Vec<String> {
    if !cfg.judge.enabled {
        return Vec::new();
    }
    let Some(repo) = ratchet::Repo::discover(cwd) else {
        return Vec::new();
    };
    let Some(base) = repo.resolve_ref(&cfg.ref_) else {
        return Vec::new();
    };
    let chunks = agent_slop_integrations::judge::added_code_chunks(
        &repo.root,
        &base,
        Some(rel),
        cfg.judge.max_verdicts,
    );
    agent_slop_integrations::judge::needs(
        cfg,
        &chunks,
        &agent_slop_integrations::judge::repo_name(&repo.root),
        deadline(cfg).unwrap_or_else(Instant::now),
    )
    .iter()
    .map(agent_slop_integrations::judge::NeedsHit::render)
    .collect()
}

/// Pin the baseline. Always exit 0, nothing on stdout: `SessionStart` cannot
/// block and is off the turn's critical path.
fn session_start(cfg: &ResolvedConfig, payload: &HookPayload, cwd: &std::path::Path) -> Outcome {
    let (Some(session_id), Some(repo)) = (payload.session_id(), Repo::discover(cwd)) else {
        return Outcome::allow();
    };
    ratchet::pin_session(cfg, &repo, &Cache::open(true), session_id, deadline(cfg));
    Outcome::allow()
}

fn stop(cfg: &ResolvedConfig, payload: &HookPayload, cwd: &std::path::Path) -> Outcome {
    // Precondition 1: the agent is already continuing because a stop hook
    // blocked earlier this turn. The verdict cannot change until it edits
    // something, so re-blocking is a loop.
    if payload.stop_hook_active() == Some(true) {
        return Outcome::allow();
    }
    // Precondition 2: a session-end Stop carries a reason other than
    // end_turn and has no turn left to continue. Claude Code sends no
    // reason at all, so absence must NOT skip the scan.
    if payload.reason.as_deref().is_some_and(|r| r != "end_turn") {
        return Outcome::allow();
    }
    // Precondition 3: a subagent's turn is not the session's turn.
    if payload.subagent_type().is_some() {
        return Outcome::allow();
    }

    let Some(repo) = Repo::discover(cwd) else {
        return Outcome {
            code: 0,
            stdout: Vec::new(),
            stderr: vec!["agent-slop: not a git repository, ratchet off".to_string()],
            row: None,
        };
    };

    let started = Instant::now();
    let cache = Cache::open(true);
    let Some(gate) = ratchet::evaluate(
        cfg,
        &repo,
        payload.session_id(),
        None,
        &cache,
        deadline(cfg),
    ) else {
        return Outcome::allow();
    };

    let row = telemetry::build(&RowInput {
        event: "Stop",
        harness: telemetry::harness(),
        session_id: payload.session_id(),
        parent_session_id: None,
        // Precondition 3 already returned for a subagent Stop, so this is
        // always the session's own turn.
        subagent_type: None,
        cwd,
        findings: &gate.findings,
        added: &gate.added,
        mode: cfg.mode,
        ref_: match gate.baseline_source {
            ratchet::BaselineSource::Session => "session",
            ratchet::BaselineSource::LiveHead => &cfg.ref_,
        },
        blocked: gate.blocked,
        would_block: gate.would_block,
        files_scanned: gate.files_scanned,
        duration_ms: elapsed_ms(started),
        // The Stop leg runs no linters (ADR-064: the baseline is read from
        // git blobs, which no linter can lint without materialising a
        // tree), so the map is empty rather than claiming a tool ran.
        linters: BTreeMap::new(),
    });

    if !gate.blocked {
        return Outcome {
            code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
            row: Some(row),
        };
    }
    // The block decision is already made and the verdict cannot reach it:
    // `gate.blocked` was computed above, and nothing below re-reads it. The
    // model only adds a line of context to a message that is going out
    // either way. The budget is the leg's own, so a slow endpoint costs the
    // annotation, never the gate.
    let judged = agent_slop_integrations::judge::annotate(
        cfg,
        &gate.blocking,
        cwd,
        deadline(cfg).unwrap_or_else(Instant::now),
        None,
    );
    Outcome {
        code: 2,
        stdout: Vec::new(),
        stderr: render_block(&cfg.ruleset, &gate.blocking, &gate.added, &judged),
        row: Some(row),
    }
}

/// The Stop message: every added error in full, then one line for the
/// rest. A block an agent cannot resolve teaches it to route around the
/// gate, so each error carries the rule's exception shape and a
/// ready-to-paste suppression line.
fn render_block(
    ruleset: &RuleSet,
    blocking: &[Finding],
    added: &[Finding],
    judged: &[Option<agent_slop_integrations::judge::Verdict>],
) -> Vec<String> {
    let mut lines =
        vec!["agent-slop: this turn added slop that blocks (ADR-064 ratchet)".to_string()];
    let mut shown_per_rule: BTreeMap<&str, usize> = BTreeMap::default();

    for (i, f) in blocking.iter().enumerate() {
        let shown = shown_per_rule.entry(f.rule.as_str()).or_insert(0);
        *shown += 1;
        if *shown > PER_RULE_CEILING {
            continue;
        }
        lines.push(format!(
            "  {}:{}:{} {} — {}",
            f.file, f.line, f.col, f.rule, f.message
        ));
        if let Some(Some(v)) = judged.get(i) {
            lines.push(format!("    laya: {}", v.render()));
        }
        if let Some(rule) = ruleset.find(&f.rule) {
            if !rule.exception.shape.is_empty() {
                lines.push(format!("    legitimate when: {}", rule.exception.shape));
            }
            if !rule.note.text.is_empty() {
                lines.push(format!("    instead: {}", rule.note.text));
            }
        }
        lines.push(format!(
            "    suppress: agent-slop: allow {} -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>",
            f.rule
        ));
    }
    for (rule, shown) in &shown_per_rule {
        let total = blocking.iter().filter(|f| f.rule == *rule).count();
        if total > PER_RULE_CEILING {
            lines.push(format!(
                "  +{} more {rule}",
                total - shown.min(&PER_RULE_CEILING)
            ));
        }
    }

    let rest = added
        .iter()
        .filter(|f| f.severity != Severity::Error)
        .count();
    if rest > 0 {
        lines.push(format!(
            "  +{rest} added warnings/hints — run agent-slop scan --since HEAD"
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both envelopes name the same event, tool, file and session; a parse
    /// written for one harness reads null under the other.
    #[test]
    fn claude_and_grok_spellings_parse_to_the_same_fields() {
        let claude = r#"{"hook_event_name":"PostToolUse","tool_name":"Edit",
            "tool_input":{"file_path":"a.py"},"session_id":"s1","stop_hook_active":false}"#;
        let grok = r#"{"hookEventName":"PostToolUse","toolName":"Edit",
            "toolInput":{"filePath":"a.py"},"sessionId":"s1","stopHookActive":false}"#;
        for text in [claude, grok] {
            let p: HookPayload = serde_json::from_str(text).expect("parses");
            assert_eq!(p.event(), "PostToolUse");
            assert!(p.event_is("posttooluse"));
            assert_eq!(p.file_path(), Some("a.py"));
            assert_eq!(p.session_id(), Some("s1"));
            assert_eq!(p.stop_hook_active(), Some(false));
        }
    }

    /// The single-spelling payloads above are a shape NO harness sends. grok
    /// emits both keys at once, with DIFFERENT values in each — verbatim from
    /// a live grok session (review artifact `f1-real-grok-envelopes.jsonl`,
    /// 2026-09-22). Under `#[serde(alias)]` this failed to parse at all
    /// ("duplicate field"), which made every leg a silent no-op at exit 0.
    const REAL_GROK_POST_TOOL_USE: &str = r#"{"hookEventName":"post_tool_use",
        "sessionId":"01a0c884-9296-7b90-bc8d-a79fd16301a5","toolName":"write",
        "toolInput":{"file_path":"a.py"},"hook_event_name":"PostToolUse",
        "session_id":"01a0c884-9296-7b90-bc8d-a79fd16301a5","tool_name":"write",
        "tool_input":{"file_path":"a.py"}}"#;

    const REAL_GROK_STOP: &str = r#"{"hookEventName":"stop",
        "sessionId":"01a0c884-9296-7b90-bc8d-a79fd16301a5","reason":"end_turn",
        "stopHookActive":false,"hook_event_name":"Stop",
        "session_id":"01a0c884-9296-7b90-bc8d-a79fd16301a5"}"#;

    #[test]
    fn a_real_grok_envelope_carrying_both_spellings_parses() {
        let p: HookPayload = serde_json::from_str(REAL_GROK_POST_TOOL_USE)
            .expect("grok sends both keys in one object; this must not be a duplicate field");
        assert_eq!(p.file_path(), Some("a.py"));
        assert_eq!(p.session_id(), Some("01a0c884-9296-7b90-bc8d-a79fd16301a5"));
    }

    /// grok's own vocabulary is `snake_case` (`post_tool_use`), so a
    /// case-insensitive compare against `posttooluse` is false — the
    /// underscores are real characters. Dispatch must recognise the event
    /// through either key.
    #[test]
    fn grok_snake_case_event_values_dispatch() {
        let post: HookPayload = serde_json::from_str(REAL_GROK_POST_TOOL_USE).expect("parses");
        assert!(post.event_is("posttooluse"), "grok spells it post_tool_use");
        let stop: HookPayload = serde_json::from_str(REAL_GROK_STOP).expect("parses");
        assert!(stop.event_is("stop"));
        assert_eq!(stop.stop_hook_active(), Some(false));
        assert_eq!(stop.reason.as_deref(), Some("end_turn"));
    }

    /// The advisory leg must actually produce advice under grok. Before the
    /// fix this returned an empty `Outcome` — exit 0, nothing on stdout,
    /// indistinguishable from a healthy run with nothing to say.
    #[test]
    fn the_advisory_leg_is_not_a_no_op_under_grok() {
        let dir = std::env::temp_dir().join(format!("agent-slop-grok-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("hist.py");
        std::fs::write(
            &file,
            "# We previously always sent a Referer -- that was a bug.\nx = 1\n",
        )
        .expect("write fixture");
        let payload = format!(
            r#"{{"hookEventName":"post_tool_use","hook_event_name":"PostToolUse",
               "toolName":"write","tool_name":"write",
               "toolInput":{{"filePath":"{p}"}},"tool_input":{{"file_path":"{p}"}},
               "sessionId":"s1","session_id":"s1"}}"#,
            p = file.display()
        );
        let o = dispatch(&payload, &dir);
        assert_eq!(o.code, 0, "the advisory leg never blocks");
        assert!(
            o.stdout.iter().any(|l| l.contains("slop/comment-history")),
            "grok must receive advice, got {:?}",
            o.stdout
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cwd() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn an_empty_payload_allows() {
        let o = dispatch("{}", &cwd());
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, [] as [std::string::String; 0]);
    }

    #[test]
    fn unparsable_stdin_allows() {
        assert_eq!(dispatch("not json", &cwd()).code, 0);
    }

    #[test]
    fn stop_with_the_reentry_flag_exits_zero_without_scanning() {
        let o = dispatch(r#"{"hookEventName":"Stop","stopHookActive":true}"#, &cwd());
        assert_eq!(o.code, 0);
        assert!(
            o.row.is_none(),
            "precondition 1 must skip the scan entirely"
        );
    }

    #[test]
    fn a_session_end_stop_exits_zero_without_scanning() {
        let o = dispatch(
            r#"{"hookEventName":"Stop","reason":"channel_closed"}"#,
            &cwd(),
        );
        assert_eq!(o.code, 0);
        assert!(o.row.is_none(), "the leg must not scan");
    }

    #[test]
    fn a_subagent_stop_exits_zero_without_scanning() {
        let o = dispatch(
            r#"{"hookEventName":"Stop","subagentType":"explore"}"#,
            &cwd(),
        );
        assert_eq!(o.code, 0);
        assert!(o.row.is_none(), "the leg must not scan");
    }

    #[test]
    fn a_bash_call_edits_no_file_and_returns_immediately() {
        let o = dispatch(
            r#"{"hookEventName":"PostToolUse","toolName":"Bash"}"#,
            &cwd(),
        );
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, [] as [std::string::String; 0]);
    }

    #[test]
    fn post_tool_use_advice_lands_on_stdout_as_additional_context() {
        let dir = std::env::temp_dir().join(format!("agent-slop-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(
            dir.join("a.py"),
            "# We previously always sent a Referer — that was a bug.\nreferer = None\n",
        )
        .expect("write");

        let o = dispatch(
            r#"{"hookEventName":"PostToolUse","toolName":"Edit","toolInput":{"filePath":"a.py"}}"#,
            &dir,
        );
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(o.code, 0, "the advisory leg never blocks");
        assert_eq!(o.stdout.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&o.stdout[0]).expect("json envelope");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let context = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("context");
        assert!(context.contains("slop/comment-history"), "got {context}");
    }

    #[test]
    fn a_clean_file_produces_no_advice() {
        let dir =
            std::env::temp_dir().join(format!("agent-slop-hook-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(dir.join("a.py"), "referer = None\n").expect("write");
        let o = dispatch(
            r#"{"hookEventName":"PostToolUse","toolInput":{"filePath":"a.py"}}"#,
            &dir,
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(o.stdout, [] as [std::string::String; 0]);
    }

    #[test]
    fn a_block_message_carries_the_exception_and_a_suppression_line() {
        let f = Finding {
            rule: "slop/comment-history".into(),
            severity: Severity::Error,
            upstream_severity: None,
            file: "a.py".into(),
            line: 1,
            col: 1,
            end_line: 1,
            message: "change-history residue".into(),
            snippet: "# previously".into(),
            fingerprint: "aa".into(),
            source: agent_slop_core::finding::Source::Slop,
            lang: None,
            suppression: None,
            fixable: true,
        };
        let ruleset = agent_slop_core::config::builtin_config().ruleset;
        let lines = render_block(
            &ruleset,
            std::slice::from_ref(&f),
            std::slice::from_ref(&f),
            &[None],
        );
        let text = lines.join("\n");
        assert!(text.contains("legitimate when:"), "{text}");
        assert!(
            text.contains("agent-slop: allow slop/comment-history --"),
            "{text}"
        );
    }

    #[test]
    fn the_render_ceiling_caps_at_five_per_rule() {
        let mk = |line: u32| Finding {
            rule: "slop/comment-meta".into(),
            severity: Severity::Error,
            upstream_severity: None,
            file: "a.py".into(),
            line,
            col: 1,
            end_line: line,
            message: "session residue".into(),
            snippet: "# for now".into(),
            fingerprint: format!("f{line}"),
            source: agent_slop_core::finding::Source::Slop,
            lang: None,
            suppression: None,
            fixable: true,
        };
        let blocking: Vec<Finding> = (1..=8).map(mk).collect();
        let ruleset = agent_slop_core::config::builtin_config().ruleset;
        let text = render_block(&ruleset, &blocking, &blocking, &[]).join("\n");
        assert_eq!(text.matches("session residue").count(), PER_RULE_CEILING);
        assert!(text.contains("+3 more slop/comment-meta"), "{text}");
    }
}
