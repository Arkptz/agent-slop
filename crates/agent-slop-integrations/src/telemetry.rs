//! The telemetry row (SPEC-006 "Telemetry row").
//!
//! One JSONL row appended to the existing feedback shard that
//! `files/claude-hooks/background-tasks-probe.sh` writes, so there is no
//! new subtree and no new Landlock grant. Every fixed value here is
//! load-bearing for the consumer (`pkgs/agent-feedback-miner/`):
//! `schema: 2` passes its check (`fold.rs:18`), `agent_behaviour` is the
//! last entry of `CAT_ORDER` (`model.rs:11-21`) so the row ranks rather
//! than landing in an unnamed tail, and `severity: "none"` with
//! `human_action_required: false` keeps slop out of the environment-problem
//! counters (`fold.rs:32-35`) while `defects[]` still folds.
//!
//! The comment text is never recorded: this store keeps data forever
//! (POL-024), and a finding's snippet is someone's source code.
//!
//! Writing telemetry never changes an exit code. Every failure is
//! swallowed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_slop_core::config::Mode;
use agent_slop_core::finding::Finding;

/// `fold.rs:96-100` weights `top_sessions` by defect count regardless of
/// severity, so an unbounded row would dominate the digest.
const MAX_DEFECTS: usize = 20;

/// A suppression reason is free text from the repository.
const REASON_CAP: usize = 160;

/// `leak_surface` (`fold.rs:104-118`) interns one surface per category, so
/// a per-rule path there buys nothing; the specific path rides in `detail`.
const NIX_SURFACE: &str = "pkgs/agent-slop/rules/ (rule corpus)";

#[derive(Debug)]
pub struct RowInput<'a> {
    pub event: &'a str,
    pub harness: &'a str,
    pub session_id: Option<&'a str>,
    pub parent_session_id: Option<&'a str>,
    pub subagent_type: Option<&'a str>,
    pub cwd: &'a Path,
    pub findings: &'a [Finding],
    pub added: &'a [Finding],
    pub mode: Mode,
    pub ref_: &'a str,
    pub blocked: bool,
    pub would_block: bool,
    pub files_scanned: usize,
    pub duration_ms: u64,
    pub linters: BTreeMap<String, String>,
}

fn truncate(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    s.chars().take(cap).collect()
}

// Closed prefix list: the ephemeral realms. $TMPDIR is added at runtime
// when set and absolute.
const EPHEMERAL_ROOTS: &[&str] = &["/tmp", "/var/tmp"];

// Project names become ~/reviews/<name>/ directory names (POL-024, forever)
// and the recorder enforces this set on basenames: keep all producers
// identical.
fn is_safe_project_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

// A `.git` pointer file is ambiguous: linked worktree or submodule. Only
// the worktree shape folds into the repo it belongs to; a submodule
// keeps its own basename.
fn gitdir_repo_component(gitdir: &str) -> Option<String> {
    let p = gitdir.trim();
    let i = p.find("/.git/worktrees/")?;
    let name = p[..i].rsplit('/').next()?;
    if name != "." && name != ".." && is_safe_project_name(name) {
        Some(name.to_string())
    } else {
        None
    }
}

// Exact match or a real descendant: "/tmp2" must not count as "/tmp".
fn under_prefix(s: &str, prefix: &str) -> bool {
    s == prefix || s.starts_with(&format!("{prefix}/"))
}

fn is_ephemeral(cwd: &Path, home: Option<&Path>, tmpdir: Option<&str>) -> bool {
    let s = cwd.to_string_lossy();
    if EPHEMERAL_ROOTS.iter().any(|p| under_prefix(&s, p)) {
        return true;
    }
    if let Some(tmpdir) = tmpdir {
        // Absolute and non-root only; a trailing slash would defeat the
        // descendant check.
        let t = tmpdir.trim_end_matches('/');
        if t.starts_with('/') && t.len() > 1 && under_prefix(&s, t) {
            return true;
        }
    }
    if let Some(h) = home {
        for dot in [".grok", ".cache"] {
            if cwd.starts_with(h.join(dot)) {
                return true;
            }
        }
    }
    false
}

fn basename_or_unknown(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| is_safe_project_name(s))
        .unwrap_or("unknown")
        .to_string()
}

/// The digest's project key: the same attribution the `SessionEnd` recorder
/// and the miner's `discover.rs::project_name` apply, ported standalone, with
/// no shelling out. In rule order:
///  - cwd == $HOME is reserved as "home" (a dotfiles checkout in ~ must
///    not resurrect the username as a project name);
///  - a relative cwd (legacy grok session roots) is taken as-is, no walk;
///  - the closed ephemeral realms (/tmp, /var/tmp, $TMPDIR, ~/.grok,
///    ~/.cache) map to the literal "ephemeral", decided BEFORE the walk so
///    a degraded floor can never escape into a repo outside the realm;
///  - otherwise the nearest enclosing repo wins: a `.git` directory is a
///    repo (a nested repo never folds into an outer one); a `.git` FILE
///    folds into its repo only when the gitdir pointer has the linked-
///    worktree shape (`.../.git/worktrees/<name>`), a submodule pointer
///    keeps its own basename; the walk stops unexamined at $HOME for
///    paths under it, at / elsewhere, and falls back to the cwd's
///    basename. One deliberate tension mirrored from the miner: a live
///    worktree under ~/.grok/worktrees lands in "ephemeral", not its
///    parent repo; fold-then-escape is the failure the rule order
///    exists to kill, and the row's cwd keeps offline re-attribution possible.
#[must_use]
pub fn project_of(cwd: &Path) -> String {
    project_of_in(
        cwd,
        home::home_dir().as_deref(),
        std::env::var("TMPDIR").ok().as_deref(),
    )
}

pub(crate) fn project_of_in(cwd: &Path, home: Option<&Path>, tmpdir: Option<&str>) -> String {
    if home == Some(cwd) {
        return "home".to_string();
    }
    if !cwd.is_absolute() {
        return basename_or_unknown(cwd);
    }
    if is_ephemeral(cwd, home, tmpdir) {
        return "ephemeral".to_string();
    }
    let floor: &Path = match home {
        Some(h) if cwd.starts_with(h) => h,
        _ => Path::new("/"),
    };
    let mut cur = Some(cwd);
    while let Some(d) = cur {
        if d == floor {
            break;
        }
        let dotgit = d.join(".git");
        if dotgit.is_dir() {
            // Dotfiles-home guard: when $HOME resolves elsewhere (scratch
            // home) the walk can land on the real ~/.git checkout; if the
            // cwd lives in that repo's .grok/.cache, the realm wins.
            for dot in [".grok", ".cache"] {
                let realm = d.join(dot);
                if cwd.starts_with(&realm) && realm.is_dir() {
                    return "ephemeral".to_string();
                }
            }
            return basename_or_unknown(d);
        }
        if dotgit.is_file() {
            if let Some(name) = std::fs::read_to_string(&dotgit).ok().and_then(|ptr| {
                let gitdir = ptr.strip_prefix("gitdir:")?;
                if gitdir.contains("/worktrees/") {
                    gitdir_repo_component(gitdir)
                } else {
                    None
                }
            }) {
                return name;
            }
            return basename_or_unknown(d);
        }
        cur = d.parent();
    }
    basename_or_unknown(cwd)
}

fn defect(item: &str, detail: &str, count: usize, ts: &str) -> serde_json::Value {
    serde_json::json!({
        "category": "agent_behaviour",
        "item": item,
        "detail": detail,
        "source": "agent-slop",
        "first_ts": ts,
        "count": count,
        "nix_surface": NIX_SURFACE,
    })
}

/// Per-rule histogram plus the suppression counterpart. `item` is the rule
/// id alone: `canon()` (`vocab.rs:13-15`) lowercases and folds hyphens to
/// underscores while `/` and `:` survive, so `slop/comment-history` is one
/// bucket. A language suffix would double the rows without answering a
/// question anyone asked.
/// The command that reproduces the evidence, so `detail` names a way to
/// see it rather than only asserting it. The advisory leg compares
/// against no ref, so `--since none` would be a command that does not run.
fn probe(input: &RowInput) -> String {
    match input.ref_ {
        "none" => "agent-slop check <file>".to_string(),
        r => format!("agent-slop scan --since {r}"),
    }
}

fn defects(input: &RowInput, ts: &str) -> Vec<serde_json::Value> {
    let probe = probe(input);
    let mut by_rule: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in input.added {
        by_rule.entry(f.rule.as_str()).or_default().push(f);
    }
    let mut suppressed: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in input.findings.iter().filter(|f| f.suppression.is_some()) {
        suppressed.entry(f.rule.as_str()).or_default().push(f);
    }

    let mut out = Vec::new();
    for (rule, hits) in &by_rule {
        let files: std::collections::BTreeSet<&str> =
            hits.iter().map(|f| f.file.as_str()).collect();
        let top = hits
            .first()
            .expect("a rule key exists because a finding produced it");
        out.push(defect(
            rule,
            &format!(
                "{rule} {}x in {} files, top '{}:{}' [{probe}]",
                hits.len(),
                files.len(),
                top.file,
                top.line,
            ),
            hits.len(),
            ts,
        ));
    }
    for (rule, hits) in &suppressed {
        let top = hits
            .first()
            .expect("a rule key exists because a finding produced it");
        let reason = top
            .suppression
            .as_ref()
            .map(|s| truncate(&s.reason, REASON_CAP))
            .unwrap_or_default();
        let family = rule.strip_prefix("slop/").unwrap_or(rule);
        out.push(defect(
            &format!("slop/suppressed:{family}"),
            &format!(
                "suppressed {}x, reason '{reason}' at '{}:{}' [{probe}]",
                hits.len(),
                top.file,
                top.line,
            ),
            hits.len(),
            ts,
        ));
    }
    // Only when the gate actually returned exit 2 — `would_block` alone is
    // the observe-mode signal and lives in `telemetry`.
    if input.blocked {
        out.push(defect(
            "slop/gate-blocked",
            &format!("Stop returned exit 2 [{probe}]"),
            1,
            ts,
        ));
    }

    if out.len() > MAX_DEFECTS {
        let dropped = out.len() - (MAX_DEFECTS - 1);
        out.truncate(MAX_DEFECTS - 1);
        out.push(defect(
            "truncated",
            &format!("{dropped} further defect entries dropped at the {MAX_DEFECTS}-entry cap"),
            dropped,
            ts,
        ));
    }
    out
}

#[must_use]
pub fn row(input: &RowInput, ts: &str, date: &str) -> serde_json::Value {
    serde_json::json!({
        "record_kind": "slop_scan",
        "schema": 2,
        "ts": ts,
        "event": input.event,
        "harness": input.harness,
        "session_id": input.session_id,
        "parent_session_id": input.parent_session_id,
        "subagent_type": input.subagent_type,
        "cwd": input.cwd.to_string_lossy(),
        "project": project_of(input.cwd),
        "date": date,
        // Slop is not a tool failure: `fold.rs:32-35` increments the
        // environment-problem counters off these two fields.
        "severity": "none",
        "human_action_required": false,
        "defects": defects(input, ts),
        "telemetry": {
            // Not "hook-payload": POL-024 ties that value to the
            // dedup-winner rule, and this row is not a session record.
            "source": "agent-slop",
            "mode": match input.mode { Mode::Block => "block", Mode::Observe => "observe" },
            "ref": input.ref_,
            "blocked": input.blocked,
            // Without this, an observe-only rollout records no evidence
            // about blocking at all and TASK-019's "zero false blocks"
            // would be satisfied vacuously.
            "would_block": input.would_block,
            "files_scanned": input.files_scanned,
            "duration_ms": input.duration_ms,
            "findings_total": input.findings.len(),
            "suppressions_total": input.findings.iter().filter(|f| f.suppression.is_some()).count(),
            "linters": input.linters,
        },
        "self_report": serde_json::Value::Null,
        "human_annotation": serde_json::Value::Null,
        "mined": {"taxonomy": null, "action": null, "reviewed": false},
    })
}

fn utc_now() -> (String, String, String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    (
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z"),
        format!("{y:04}"),
        format!("{m:02}"),
        format!("{y:04}-{m:02}-{d:02}"),
    )
}

/// Howard Hinnant's civil-from-days, so the shard path matches the shell
/// writers' `date -u` without pulling in a date crate.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400).cast_signed();
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// `$HOME/reviews/<project>/agent-feedback/<YYYY>/<MM>/<YYYY-MM-DD>.jsonl`,
/// hardcoded the same way both shell writers hardcode it: three writers on
/// one shard need one canonical path.
#[must_use]
pub fn shard_path(root: &Path, project: &str, year: &str, month: &str, day: &str) -> PathBuf {
    root.join(project)
        .join("agent-feedback")
        .join(year)
        .join(month)
        .join(format!("{day}.jsonl"))
}

fn default_root() -> Option<PathBuf> {
    Some(home::home_dir()?.join("reviews"))
}

/// Which harness is running us, by the same env test the shell writers
/// use (`background-tasks-probe.sh:82-85`).
#[must_use]
pub fn harness() -> &'static str {
    match std::env::var_os("GROK_SESSION_ID") {
        Some(_) => "grok",
        None => "claude",
    }
}

/// The row as it will be written, stamped with the current UTC time.
/// Built separately from writing it so a caller can hold the row, assert
/// on it, and write it once at the process edge.
#[must_use]
pub fn build(input: &RowInput) -> serde_json::Value {
    let (ts, _, _, day) = utc_now();
    row(input, &ts, &day)
}

/// Append one built row. The shard is derived from the row's own
/// `project` and `date`, so a row always lands where its own fields say.
/// Returns the path written, or `None` when anything at all went wrong —
/// the caller's exit code never depends on this.
#[must_use]
pub fn write(row: &serde_json::Value, root: Option<&Path>) -> Option<PathBuf> {
    let root = match root {
        Some(r) => r.to_path_buf(),
        None => default_root()?,
    };
    let project = row["project"].as_str()?;
    let day = row["date"].as_str()?;
    let (year, month) = (day.get(0..4)?, day.get(5..7)?);
    let path = shard_path(&root, project, year, month, day);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let line = serde_json::to_string(row).ok()?;
    append_line(&path, &line)?;
    Some(path)
}

/// Build and append in one call, for callers with nothing to inspect.
#[cfg(test)]
#[must_use]
pub fn emit(input: &RowInput, root: Option<&Path>) -> Option<PathBuf> {
    write(&build(input), root)
}

/// One `O_APPEND` write of a single line under the page size is atomic on
/// Linux, which is the same guarantee the shell writers get from their
/// `printf >>` under `flock`: two writers never interleave a row.
fn append_line(path: &Path, line: &str) -> Option<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    writeln!(file, "{line}").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_slop_core::finding::{Severity, Source, Suppression};

    fn finding(rule: &str, line: u32, suppression: Option<Suppression>) -> Finding {
        Finding {
            rule: rule.to_string(),
            severity: Severity::Error,
            upstream_severity: None,
            file: "src/scan.rs".to_string(),
            line,
            col: 1,
            end_line: line,
            message: "change-history residue".to_string(),
            snippet: "# NEVER-RECORD-THIS-TEXT previously sent a Referer".to_string(),
            fingerprint: format!("fp{line}"),
            source: Source::Slop,
            lang: None,
            suppression,
            fixable: true,
        }
    }

    fn input<'a>(
        findings: &'a [Finding],
        added: &'a [Finding],
        cwd: &'a Path,
        blocked: bool,
    ) -> RowInput<'a> {
        RowInput {
            event: "Stop",
            harness: "grok",
            session_id: Some("s1"),
            parent_session_id: None,
            subagent_type: None,
            cwd,
            findings,
            added,
            mode: Mode::Block,
            ref_: "HEAD",
            blocked,
            would_block: blocked,
            files_scanned: 12,
            duration_ms: 214,
            linters: BTreeMap::from([("ruff".to_string(), "ok".to_string())]),
        }
    }

    #[test]
    fn the_row_carries_every_value_the_consumer_keys_on() {
        let f = vec![finding("slop/comment-history", 42, None)];
        let cwd = Path::new("/home/u/config_v3");
        let v = row(
            &input(&f, &f, cwd, false),
            "2026-09-21T14:03:11Z",
            "2026-09-21",
        );
        assert_eq!(v["record_kind"], "slop_scan");
        assert_eq!(v["schema"], 2);
        assert_eq!(v["severity"], "none");
        assert_eq!(v["human_action_required"], false);
        assert_eq!(v["telemetry"]["source"], "agent-slop");
        assert_eq!(v["project"], "config_v3");
        assert_eq!(v["defects"][0]["category"], "agent_behaviour");
        assert_eq!(v["defects"][0]["item"], "slop/comment-history");
        assert_eq!(v["defects"][0]["nix_surface"], NIX_SURFACE);
    }

    /// POL-024: the store keeps data forever, so the comment itself must
    /// not be in it.
    #[test]
    fn the_comment_text_never_reaches_the_row() {
        let f = vec![finding("slop/comment-history", 42, None)];
        let cwd = Path::new("/home/u/config_v3");
        let text = row(&input(&f, &f, cwd, true), "t", "d").to_string();
        assert!(!text.contains("NEVER-RECORD-THIS-TEXT"), "{text}");
    }

    /// `detail`'s first quoted token is `path:line`, so `short_item`
    /// (`fold.rs:123-133`) degrades gracefully if `item` is ever dropped.
    #[test]
    fn detail_quotes_path_and_line_first() {
        let f = vec![finding("slop/comment-history", 42, None)];
        let cwd = Path::new("/home/u/config_v3");
        let v = row(&input(&f, &f, cwd, false), "t", "d");
        let detail = v["defects"][0]["detail"].as_str().expect("detail");
        let quoted = detail.split('\'').nth(1).expect("a quoted token");
        assert_eq!(quoted, "src/scan.rs:42");
    }

    #[test]
    fn gate_blocked_appears_only_when_the_gate_blocked() {
        let f = vec![finding("slop/comment-history", 1, None)];
        let cwd = Path::new("/home/u/p");
        let items = |blocked: bool| {
            row(&input(&f, &f, cwd, blocked), "t", "d")["defects"]
                .as_array()
                .expect("defects")
                .iter()
                .map(|d| d["item"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        assert!(!items(false).contains(&"slop/gate-blocked".to_string()));
        assert!(items(true).contains(&"slop/gate-blocked".to_string()));
    }

    #[test]
    fn a_suppression_rides_the_same_row_with_its_reason_capped() {
        let long = "x".repeat(400);
        let f = vec![finding(
            "slop/todo-no-condition",
            7,
            Some(Suppression {
                rule: "slop/todo-no-condition".into(),
                reason: long,
                valid: true,
            }),
        )];
        let cwd = Path::new("/home/u/p");
        let v = row(&input(&f, &[], cwd, false), "t", "d");
        let item = v["defects"][0]["item"].as_str().expect("item");
        assert_eq!(item, "slop/suppressed:todo-no-condition");
        let detail = v["defects"][0]["detail"].as_str().expect("detail");
        let reason = detail.split('\'').nth(1).expect("quoted reason");
        assert_eq!(reason.chars().count(), REASON_CAP);
        assert_eq!(v["telemetry"]["suppressions_total"], 1);
    }

    #[test]
    fn the_defect_cap_collapses_the_tail() {
        let findings: Vec<Finding> = (0..40)
            .map(|i| finding(&format!("slop/rule-{i}"), i + 1, None))
            .collect();
        let cwd = Path::new("/home/u/p");
        let v = row(&input(&findings, &findings, cwd, false), "t", "d");
        let defects = v["defects"].as_array().expect("defects");
        assert_eq!(defects.len(), MAX_DEFECTS);
        assert_eq!(defects[MAX_DEFECTS - 1]["item"], "truncated");
    }

    #[test]
    fn observe_mode_records_would_block_without_blocking() {
        let f = vec![finding("slop/comment-history", 1, None)];
        let cwd = Path::new("/home/u/p");
        let mut i = input(&f, &f, cwd, false);
        i.mode = Mode::Observe;
        i.would_block = true;
        let v = row(&i, "t", "d");
        assert_eq!(v["telemetry"]["mode"], "observe");
        assert_eq!(v["telemetry"]["blocked"], false);
        assert_eq!(v["telemetry"]["would_block"], true);
    }

    #[test]
    fn the_shard_path_matches_the_shell_writers() {
        let p = shard_path(
            Path::new("/h/reviews"),
            "config_v3",
            "2026",
            "09",
            "2026-09-21",
        );
        assert!(
            p.ends_with("config_v3/agent-feedback/2026/09/2026-09-21.jsonl"),
            "{p:?}"
        );
    }

    /// The date arithmetic has to agree with `date -u`, or the row lands
    /// in a shard the miner reads on a different day.
    #[test]
    fn civil_from_unix_matches_known_timestamps() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        // Each expectation is `date -u -d @<secs>`, run separately from
        // this implementation — an oracle the subject did not produce
        // (POL-026 rule 3).
        // date -u -d @1789999391 → 2026-09-21T14:03:11Z
        assert_eq!(civil_from_unix(1_789_999_391), (2026, 9, 21, 14, 3, 11));
        // date -u -d @1709164800 → 2024-02-29T00:00:00Z, the leap day a
        // naive 365-day divisor gets wrong.
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn emit_appends_a_parseable_line_and_never_rewrites() {
        let root = std::env::temp_dir().join(format!("agent-slop-tel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = vec![finding("slop/comment-history", 1, None)];
        let cwd = Path::new("/home/u/config_v3");
        let path = emit(&input(&f, &f, cwd, false), Some(&root)).expect("write");
        let path2 = emit(&input(&f, &f, cwd, true), Some(&root)).expect("write");
        assert_eq!(path, path2);
        let body = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "append, never rewrite");
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
            assert_eq!(v["record_kind"], "slop_scan");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Fixture tree under /dev/shm: /tmp is an ephemeral realm and would
    /// collapse its own fixtures into "ephemeral".
    struct Fx {
        root: PathBuf,
    }

    impl Fx {
        fn new() -> Fx {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = PathBuf::from("/dev/shm")
                .join(format!("agent-slop-attr-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let fx = Fx { root };
            let mk = |p: &str| std::fs::create_dir_all(fx.root.join(p)).expect("mkdir");
            let ptr = |p: &str, body: String| {
                std::fs::write(fx.root.join(p).join(".git"), body).expect("pointer");
            };
            let r = fx.root.display();
            mk(".git");
            mk("main-repo/.git");
            mk("main-repo/src/deep");
            mk("wtree/src");
            ptr(
                "wtree",
                format!("gitdir: {r}/main-repo/.git/worktrees/wtree\n"),
            );
            mk("outer/.git/modules/subm");
            mk("outer/nested/.git");
            mk("outer/nested/deep");
            mk("subm/inner");
            ptr("subm", format!("gitdir: {r}/outer/.git/modules/subm\n"));
            mk("odd name/.git");
            mk("odd name/src");
            mk(".grok/x");
            mk(".grok/worktrees/mine-exarch/subagent-01a1");
            ptr(
                ".grok/worktrees/mine-exarch/subagent-01a1",
                format!("gitdir: {r}/main-repo/.git/worktrees/subagent-01a1\n"),
            );
            mk(".cache/x");
            mk("tmpdir/s1");
            mk("share/profiles/work-gpt4/2026-09-21-a28ccd6d");
            ptr(
                "share/profiles/work-gpt4/2026-09-21-a28ccd6d",
                format!("gitdir: {r}/main-repo/.git/worktrees/2026-09-21-a28ccd6d\n"),
            );
            fx
        }

        fn at(&self, rel: &str) -> PathBuf {
            if rel.is_empty() {
                self.root.clone()
            } else {
                self.root.join(rel)
            }
        }

        fn tmpdir(&self) -> String {
            self.at("tmpdir").to_string_lossy().into_owned()
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// (cwd relative to the fixture home, expected project). Every row
    /// differs from the old basename-of-cwd answer, so each one fails on it.
    const ATTRIBUTION: &[(&str, &str)] = &[
        ("", "home"),
        ("main-repo/src/deep", "main-repo"),
        ("wtree/src", "main-repo"),
        ("outer/nested/deep", "nested"),
        ("subm/inner", "subm"),
        ("odd name/src", "unknown"),
        (".grok/x", "ephemeral"),
        (".grok/worktrees/mine-exarch/subagent-01a1", "ephemeral"),
        (".cache/x", "ephemeral"),
        ("tmpdir/s1", "ephemeral"),
        ("share/profiles/work-gpt4/2026-09-21-a28ccd6d", "main-repo"),
    ];

    #[test]
    fn project_attribution_follows_the_recorder_table() {
        let fx = Fx::new();
        let tmp = fx.tmpdir();
        for (rel, want) in ATTRIBUTION {
            let cwd = fx.at(rel);
            let got = project_of_in(&cwd, Some(&fx.root), Some(&tmp));
            assert_eq!(got, *want, "cwd {}", cwd.display());
            let old = cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            assert_ne!(
                old.as_deref(),
                Some(*want),
                "row must not pass on basename: {rel}"
            );
        }
    }

    #[test]
    fn closed_realms_need_no_fixture() {
        for cwd in ["/tmp", "/tmp/a/b", "/var/tmp/x/y"] {
            assert_eq!(
                project_of_in(Path::new(cwd), None, None),
                "ephemeral",
                "{cwd}"
            );
        }
        assert_eq!(
            project_of_in(Path::new("/srv/t/s1"), None, Some("/srv/t/")),
            "ephemeral"
        );
    }

    /// Runs the recorder's own naming block (the `miner-parity` markers in
    /// the consumer's script) on the same cwds, so the two cannot drift
    /// without this failing. The cwd == $HOME rule sits outside the
    /// markers and is covered by the table only. The recorder belongs to
    /// the agent-feedback consumer, so the check runs only when its path
    /// is supplied; once requested, absence is a failure, never a skip.
    #[test]
    #[ignore = "agent-feedback consumer check: needs AGENT_SLOP_FEEDBACK_RECORD; run with --ignored"]
    fn project_attribution_matches_the_shipped_recorder_block() {
        let script = std::env::var_os("AGENT_SLOP_FEEDBACK_RECORD")
            .map(PathBuf::from)
            .expect("set AGENT_SLOP_FEEDBACK_RECORD to the consumer's feedback-record.sh");
        let text = std::fs::read_to_string(&script)
            .unwrap_or_else(|e| panic!("{}: {e}", script.display()));
        let begin = text
            .find("[begin miner-parity: project_name_in]")
            .expect("begin marker");
        let end = text
            .find("[end miner-parity: project_name_in]")
            .expect("end marker");
        let block = text[begin..end]
            .split_once('\n')
            .expect("block body")
            .1
            .rsplit_once('\n')
            .expect("block end")
            .0
            .to_string();
        let program = format!(
            "home_dir=\"${{HOME:-}}\"\n{block}\nfi\n\
             p=\"$(basename -- \"$d\")\"\n\
             case \"$p\" in \"\"|*[!A-Za-z0-9._-]*) p=unknown ;; esac\nprintf '%s' \"$p\"\n"
        );
        let fx = Fx::new();
        let tmp = fx.tmpdir();
        for (rel, want) in ATTRIBUTION.iter().filter(|(rel, _)| !rel.is_empty()) {
            let cwd = fx.at(rel);
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&program)
                .env("cwd", &cwd)
                .env("HOME", &fx.root)
                .env("TMPDIR", &tmp)
                .output()
                .expect("bash");
            let got = String::from_utf8_lossy(&out.stdout).into_owned();
            assert_eq!(
                got,
                *want,
                "recorder block on {}: {}",
                cwd.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                project_of_in(&cwd, Some(&fx.root), Some(&tmp)),
                got,
                "{rel}"
            );
        }
    }

    #[test]
    fn an_unwritable_root_is_swallowed() {
        let f = vec![finding("slop/comment-history", 1, None)];
        let cwd = Path::new("/home/u/p");
        assert!(
            emit(
                &input(&f, &f, cwd, false),
                Some(Path::new("/proc/nonexistent"))
            )
            .is_none()
        );
    }
}
