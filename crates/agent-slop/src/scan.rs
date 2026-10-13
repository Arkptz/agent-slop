//! The `scan` and `check` commands: walk the requested paths, run the
//! slop rules and the eligible linter adapters, apply the ratchet when a
//! repository is available, and render (SPEC-006 "CLI surface and exit
//! codes").
//!
//! `check` is the advisory subset: named files, no git, no ratchet, and
//! always exit 0.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::{Format, ScanArgs};
use agent_slop_core::config::{excluded, scoped_out, ResolvedConfig};
use agent_slop_core::finding::{Finding, Severity};
use agent_slop_integrations::judge;
use agent_slop_integrations::linters::{self, PathIndex, Selection, Skipped, Stage};
use crate::ratchet::{self, Cache, Repo, SkippedPath};
use agent_slop_core::rules::scan_source;

pub struct Report {
    pub findings: Vec<Finding>,
    pub added: Vec<String>,
    pub skipped_paths: Vec<SkippedPath>,
    pub skipped_tools: Vec<Skipped>,
    pub truncated: bool,
    /// Paths actually read and scanned, excluding every `skipped_paths`
    /// entry: the telemetry row's `files_scanned`.
    pub files_scanned: usize,
    /// Per-tool status for the telemetry row (`{"ruff": "ok"}`).
    pub linters: BTreeMap<String, String>,
}

/// Rule-id globs from `--rules`/`--no-rules`. Only a trailing `*` is
/// supported: the ids are `<producer>/<id>`, so prefix matching covers
/// every selection anyone has asked for, and a full glob engine would be
/// a dependency for nothing.
fn glob_matches(pattern: &str, id: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => id.starts_with(prefix),
        None => pattern == id,
    }
}

fn rule_selected(args: &ScanArgs, id: &str) -> bool {
    if let Some(globs) = &args.rules {
        if !globs.iter().any(|g| glob_matches(g, id)) {
            return false;
        }
    }
    if let Some(globs) = &args.no_rules {
        if globs.iter().any(|g| glob_matches(g, id)) {
            return false;
        }
    }
    true
}

/// `--severity` is the minimum reported level; the default is `hint`,
/// which reports everything.
fn severity_kept(min: Option<Severity>, severity: Severity) -> bool {
    match min {
        Some(Severity::Error) => severity == Severity::Error,
        Some(Severity::Warning) => matches!(severity, Severity::Error | Severity::Warning),
        _ => true,
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

/// Expand a path argument: a file is itself, a directory is every file
/// under it whose extension maps to a supported language. Hidden
/// directories and `target`/`node_modules` are not descended — they hold
/// build output, not authored comments.
///
/// Symlinks are never followed during the walk. `is_dir()` resolves them,
/// so a `result` link left by `nix build` made the walk descend the whole
/// store closure: 29k files, 3.5 MB of argv against a 2 MB `ARG_MAX`, and
/// every path-taking linter died with `Argument list too long` while the
/// scan still exited 0. A link's target is either inside the tree (walked
/// on its own) or deliberately outside it.
fn expand(arg: &Path, out: &mut Vec<PathBuf>) {
    if arg.is_file() {
        out.push(arg.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(arg) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        // Reading the link itself (not its target) is the point: `is_dir`
        // would follow it.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            expand(&path, out);
        } else if agent_slop_core::lang::spec_for_path(&path.to_string_lossy()).is_some() {
            out.push(path);
        }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn run(cfg: &ResolvedConfig, args: &ScanArgs, cwd: &Path, git: bool) -> Report {
    let deadline = args
        .deadline_ms
        .or(Some(cfg.timeout_ms))
        .and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
    let repo = if git { Repo::discover(cwd) } else { None };
    let root = repo
        .as_ref()
        .map(|r| r.root.clone())
        .unwrap_or_else(|| cwd.to_path_buf());

    let mut targets = Vec::new();
    if args.paths.is_empty() {
        expand(&root, &mut targets);
    } else {
        for p in &args.paths {
            let path = Path::new(p);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            expand(&full, &mut targets);
        }
    }
    targets.sort();
    targets.dedup();

    // `--since` narrows the scan to what differs from the ref. Without this
    // a PR scan in a monorepo reported every file in the tree, and the
    // imported ruff findings on unrelated projects buried the PR's own.
    let base = repo
        .as_ref()
        .zip(args.since.as_deref())
        .and_then(|(repo, since)| repo.resolve_ref(since));
    if let (Some(repo), Some(base)) = (&repo, &base) {
        let mut changed = repo.changed_files(base);
        changed.extend(repo.untracked_files());
        let changed: std::collections::BTreeSet<PathBuf> =
            changed.iter().map(|rel| root.join(rel)).collect();
        targets.retain(|t| changed.contains(t));
    }

    let mut findings = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut truncated = false;
    let mut files_scanned = 0usize;

    for path in &targets {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            truncated = true;
            break;
        }
        let rel = relative(&root, path);
        if excluded(cfg, &rel) {
            skipped_paths.push(SkippedPath {
                path: rel,
                reason: "excluded",
            });
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            skipped_paths.push(SkippedPath {
                path: rel,
                reason: "unreadable",
            });
            continue;
        };
        if is_binary(&bytes) {
            skipped_paths.push(SkippedPath {
                path: rel,
                reason: "binary",
            });
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        files_scanned += 1;
        findings.extend(
            scan_source(cfg, &rel, &content)
                .into_iter()
                .filter(|f| !scoped_out(cfg, &f.file, &f.rule)),
        );
    }

    let selection = args
        .linters
        .as_deref()
        .map(Selection::parse)
        .unwrap_or(Selection::Auto);
    let mut skipped_tools = Vec::new();
    let mut ran_tools: Vec<&str> = Vec::new();
    let index = PathIndex::from_env();
    let rel_targets: Vec<String> = targets.iter().map(|p| relative(&root, p)).collect();
    for adapter in linters::ADAPTERS {
        if !linters::eligible(adapter, Stage::Scan, &selection, cfg) {
            continue;
        }
        match linters::run(adapter, &root, &rel_targets, &index) {
            Ok(stdout) => {
                let raws = (adapter.parse)(&stdout);
                findings.extend(linters::to_findings(cfg, adapter.tool, raws, &root));
                ran_tools.push(adapter.tool);
            }
            Err(skip) => skipped_tools.push(skip),
        }
    }
    let linter_status = linters::status_map(&ran_tools, &skipped_tools);

    findings.retain(|f| rule_selected(args, &f.rule) && severity_kept(args.severity, f.severity));
    findings.sort_by(|a, b| (&a.file, a.line, &a.rule).cmp(&(&b.file, b.line, &b.rule)));

    // The ratchet only has meaning against a ref: without one every
    // finding is simply reported.
    let added = match (&repo, &base) {
        (Some(repo), Some(base)) => {
            let cache = Cache::open(!args.no_cache);
            let paths: Vec<String> = findings.iter().map(|f| f.file.clone()).collect();
            let baseline = ratchet::baseline_at(cfg, repo, base, &paths, &cache, deadline);
            ratchet::added(&findings, &baseline)
                .into_iter()
                .map(|f| f.fingerprint)
                .collect()
        }
        _ => Vec::new(),
    };

    Report {
        findings,
        added,
        skipped_paths,
        skipped_tools,
        truncated,
        files_scanned,
        linters: linter_status,
    }
}

/// The stdout contract. `judged` is positionally aligned with
/// `report.findings` and carries the Laya second verdict where one was
/// printed-worthy; it is consumed by the human renderer only, so the JSON
/// contract in SPEC-006 stays byte-identical when the model is on.
pub fn render(report: &Report, fmt: Format, judged: &[Option<judge::Verdict>]) -> Vec<String> {
    match fmt {
        Format::Json => vec![envelope(report)],
        Format::Jsonl => report
            .findings
            .iter()
            .filter_map(|f| serde_json::to_string(f).ok())
            .collect(),
        // GitHub workflow-command syntax, so a scan in CI annotates the
        // diff instead of printing into the log only.
        Format::Github => report
            .findings
            .iter()
            .map(|f| {
                let level = match f.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Hint => "notice",
                };
                format!(
                    "::{level} file={},line={},col={},title={}::{}",
                    f.file, f.line, f.col, f.rule, f.message
                )
            })
            .collect(),
        Format::Human => human(report, judged),
    }
}

fn human(report: &Report, judged: &[Option<judge::Verdict>]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (i, f) in report.findings.iter().enumerate() {
        let suppressed = match &f.suppression {
            Some(s) if s.valid => " [suppressed]",
            Some(_) => " [suppression invalid]",
            None => "",
        };
        lines.push(format!(
            "{}:{}:{} {} — {}{}",
            f.file, f.line, f.col, f.rule, f.message, suppressed
        ));
        // Second verdict, indented under the finding it annotates. It reads
        // as an attribute of that finding, never as a verdict of its own: it
        // carries no rule id, no severity and no exit-code weight.
        if let Some(Some(v)) = judged.get(i) {
            lines.push(format!("  laya: {}", v.render()));
        }
    }
    for skip in &report.skipped_tools {
        lines.push(format!("skipped {}: {}", skip.tool, skip.reason));
    }
    if report.truncated {
        lines.push("truncated: the deadline expired before every path was scanned".to_string());
    }
    lines
}

/// The `--format json` stdout contract (SPEC-006), verbatim keys.
pub fn envelope(report: &Report) -> String {
    let suppressed: Vec<_> = report
        .findings
        .iter()
        .filter_map(|f| f.suppression.as_ref())
        .map(|s| serde_json::json!({"rule": s.rule, "reason": s.reason, "valid": s.valid}))
        .collect();
    let mut skipped: Vec<serde_json::Value> = report
        .skipped_paths
        .iter()
        .map(|s| serde_json::json!({"path": s.path, "reason": s.reason}))
        .collect();
    skipped.extend(
        report
            .skipped_tools
            .iter()
            .map(|s| serde_json::json!({"tool": s.tool, "reason": s.reason})),
    );
    serde_json::json!({
        "schema": 1,
        "findings": report.findings,
        "added": report.added,
        "suppressed": suppressed,
        "skipped": skipped,
        "truncated": report.truncated,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_slop_core::config::builtin_config;

    const SLOP: &str = "# We previously always sent a Referer — that was a bug.\nreferer = None\n";

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("agent-slop-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    fn args(paths: &[&str]) -> ScanArgs {
        ScanArgs {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            linters: Some("none".to_string()),
            ..ScanArgs::default()
        }
    }

    #[test]
    fn a_directory_walk_finds_slop_and_renders_json() {
        let s = scratch("walk");
        std::fs::write(s.0.join("a.py"), SLOP).expect("write");
        let report = run(&builtin_config(), &args(&[]), &s.0, false);
        assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
        let v: serde_json::Value =
            serde_json::from_str(&envelope(&report)).expect("valid envelope");
        assert_eq!(v["schema"], 1);
        assert_eq!(v["findings"][0]["rule"], "slop/comment-history");
        assert_eq!(v["truncated"], false);
    }

    #[test]
    fn build_output_directories_are_not_walked() {
        let s = scratch("target");
        std::fs::create_dir_all(s.0.join("target")).expect("dir");
        std::fs::write(s.0.join("target/a.py"), SLOP).expect("write");
        let report = run(&builtin_config(), &args(&[]), &s.0, false);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }

    /// A `result` symlink from `nix build` points at a store closure. The
    /// walk must not enter it: 29k paths became 3.5 MB of argv and every
    /// path-taking linter died with E2BIG while the scan still exited 0.
    #[cfg(unix)]
    #[test]
    fn a_directory_symlink_is_never_walked() {
        let s = scratch("symlink");
        let outside = s.0.join("outside");
        std::fs::create_dir_all(&outside).expect("dir");
        std::fs::write(outside.join("a.py"), SLOP).expect("write");

        let inside = s.0.join("tree");
        std::fs::create_dir_all(&inside).expect("dir");
        std::os::unix::fs::symlink(&outside, inside.join("result")).expect("symlink");

        let report = run(&builtin_config(), &args(&[]), &inside, false);
        assert!(
            report.findings.is_empty(),
            "the link target was walked: {:?}",
            report.findings
        );
        // Control: the same content reached through a real directory is found,
        // so the assertion above is about the link and not about the fixture.
        let control = run(&builtin_config(), &args(&[]), &outside, false);
        assert_eq!(control.findings.len(), 1, "{:?}", control.findings);
    }

    #[test]
    fn no_rules_removes_a_family_from_the_output() {
        let s = scratch("norules");
        std::fs::write(s.0.join("a.py"), SLOP).expect("write");
        let mut a = args(&[]);
        a.no_rules = Some(vec!["slop/comment-*".to_string()]);
        let report = run(&builtin_config(), &a, &s.0, false);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn severity_floor_drops_anything_below_it() {
        let s = scratch("sev");
        std::fs::write(s.0.join("a.py"), "# TODO: fix this\nx = 1\n").expect("write");
        let mut a = args(&[]);
        let all = run(&builtin_config(), &a, &s.0, false);
        assert!(all.findings.iter().any(|f| f.severity == Severity::Warning));
        a.severity = Some(Severity::Error);
        let errors_only = run(&builtin_config(), &a, &s.0, false);
        assert!(errors_only.findings.is_empty());
    }

    #[test]
    fn an_excluded_path_is_skipped_with_its_reason() {
        let s = scratch("exclude");
        std::fs::create_dir_all(s.0.join("vendor")).expect("dir");
        std::fs::write(s.0.join("vendor/a.py"), SLOP).expect("write");
        let mut cfg = builtin_config();
        cfg.exclude.paths = vec!["vendor/**".to_string()];
        let report = run(&cfg, &args(&[]), &s.0, false);
        assert!(report.findings.is_empty());
        assert_eq!(report.skipped_paths.len(), 1);
        assert_eq!(report.skipped_paths[0].reason, "excluded");
    }

    #[test]
    fn a_scoped_exclude_silences_one_rule_under_one_path() {
        let s = scratch("scoped");
        std::fs::create_dir_all(s.0.join("tests")).expect("dir");
        std::fs::write(s.0.join("tests/a.py"), SLOP).expect("write");
        let mut cfg = builtin_config();
        cfg.exclude.scoped = vec![agent_slop_core::config::ScopedExclude {
            paths: vec!["tests/**".to_string()],
            rules: vec!["slop/comment-history".to_string()],
        }];
        let report = run(&cfg, &args(&[]), &s.0, false);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn a_binary_file_is_skipped_and_listed() {
        let s = scratch("binary");
        std::fs::write(s.0.join("a.py"), [0u8, 1, 2, 3]).expect("write");
        let report = run(&builtin_config(), &args(&["a.py"]), &s.0, false);
        assert_eq!(report.skipped_paths[0].reason, "binary");
    }

    #[test]
    fn github_format_emits_workflow_commands() {
        let s = scratch("github");
        std::fs::write(s.0.join("a.py"), SLOP).expect("write");
        let report = run(&builtin_config(), &args(&[]), &s.0, false);
        let lines = render(&report, Format::Github, &[]);
        assert!(
            lines[0].starts_with("::error file=a.py,line=1"),
            "{}",
            lines[0]
        );
    }
}
