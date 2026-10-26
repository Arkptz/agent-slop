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
use crate::ratchet::{self, Cache, Repo, SkippedPath};
use agent_slop_core::config::{ResolvedConfig, excluded, scoped_out};
use agent_slop_core::finding::{Finding, Severity};
use agent_slop_core::rules::scan_source;
use agent_slop_integrations::judge;
use agent_slop_integrations::linters::{self, PathIndex, Selection, Skipped, Stage};
use agent_slop_integrations::plugins::Stage as PluginStage;

pub(crate) struct Report {
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
    if let Some(globs) = &args.rules
        && !globs.iter().any(|g| glob_matches(g, id))
    {
        return false;
    }
    if let Some(globs) = &args.no_rules
        && globs.iter().any(|g| glob_matches(g, id))
    {
        return false;
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

/// Every file the path arguments name, expanded and deduplicated; no
/// arguments means the whole root.
fn targets_of(paths: &[String], root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if paths.is_empty() {
        expand(root, &mut targets);
    }
    for p in paths {
        let path = Path::new(p);
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        expand(&full, &mut targets);
    }
    targets.sort();
    targets.dedup();
    targets
}

/// The eligible linter adapters over `rel_targets`: their findings, the
/// tools that skipped, and the per-tool status map.
fn run_linters(
    cfg: &ResolvedConfig,
    args: &ScanArgs,
    root: &Path,
    rel_targets: &[String],
) -> (Vec<Finding>, Vec<Skipped>, BTreeMap<String, String>) {
    let selection = args
        .linters
        .as_deref()
        .map_or(Selection::Auto, Selection::parse);
    let mut findings = Vec::new();
    let mut skipped_tools = Vec::new();
    let mut ran_tools: Vec<&str> = Vec::new();
    let index = PathIndex::from_env();
    for adapter in linters::ADAPTERS {
        if !linters::eligible(adapter, Stage::Scan, &selection, cfg) {
            continue;
        }
        match linters::run(adapter, root, rel_targets, &index) {
            Ok(stdout) => {
                let raws = (adapter.parse)(&stdout);
                findings.extend(linters::to_findings(cfg, adapter.tool, raws, root));
                ran_tools.push(adapter.tool);
            }
            Err(skip) => skipped_tools.push(skip),
        }
    }
    let status = linters::status_map(&ran_tools, &skipped_tools);
    (findings, skipped_tools, status)
}

/// What reading and rule-scanning the target files produced.
struct Walked {
    findings: Vec<Finding>,
    skipped_paths: Vec<SkippedPath>,
    truncated: bool,
    files_scanned: usize,
    /// Content per target, `None` where the path contributed nothing; the
    /// plugin host reads the same bytes the rules did.
    contents: Vec<Option<String>>,
}

fn walk(
    cfg: &ResolvedConfig,
    root: &Path,
    targets: &[PathBuf],
    deadline: Option<Instant>,
) -> Walked {
    let mut findings = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut truncated = false;
    let mut files_scanned = 0usize;
    let mut contents: Vec<Option<String>> = Vec::new();

    for path in targets {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            truncated = true;
            break;
        }
        let rel = relative(root, path);
        let skip = |reason: &'static str| SkippedPath {
            path: rel.clone(),
            reason,
        };
        if excluded(cfg, &rel) {
            skipped_paths.push(skip("excluded"));
            contents.push(None);
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            skipped_paths.push(skip("unreadable"));
            contents.push(None);
            continue;
        };
        if is_binary(&bytes) {
            skipped_paths.push(skip("binary"));
            contents.push(None);
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        contents.push(Some(content.clone().into_owned()));
        files_scanned += 1;
        findings.extend(
            scan_source(cfg, &rel, &content)
                .into_iter()
                .filter(|f| !scoped_out(cfg, &f.file, &f.rule)),
        );
    }
    Walked {
        findings,
        skipped_paths,
        truncated,
        files_scanned,
        contents,
    }
}

pub(crate) fn run(cfg: &ResolvedConfig, args: &ScanArgs, cwd: &Path, git: bool) -> Report {
    let deadline = args
        .deadline_ms
        .or(Some(cfg.timeout_ms))
        .and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
    let repo = if git { Repo::discover(cwd) } else { None };
    let root = repo
        .as_ref()
        .map_or_else(|| cwd.to_path_buf(), |r| r.root.clone());

    let mut targets = targets_of(&args.paths, &root, cwd);

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

    let Walked {
        mut findings,
        skipped_paths,
        truncated,
        files_scanned,
        contents,
    } = walk(cfg, &root, &targets, deadline);

    let rel_targets: Vec<String> = targets.iter().map(|p| relative(&root, p)).collect();
    let (linter_findings, mut skipped_tools, mut linter_status) =
        run_linters(cfg, args, &root, &rel_targets);
    findings.extend(linter_findings);

    let contents_named: Vec<(String, String)> = targets
        .iter()
        .zip(&contents)
        .filter_map(|(p, c)| c.clone().map(|text| (relative(&root, p), text)))
        .collect();
    let plugins = run_plugins(
        cfg,
        args.plugins.as_deref(),
        &PluginCtx {
            root: &root,
            repo: repo.as_ref(),
            base: base.as_deref(),
            deadline,
            stage: match args.stage {
                crate::cli::Stage::Check => PluginStage::Check,
                crate::cli::Stage::Scan => PluginStage::Scan,
            },
        },
        &contents_named,
    );
    findings.extend(plugins.findings.iter().cloned());
    skipped_tools.extend(plugins.skipped.iter().map(|s| linters::Skipped {
        tool: s.tool.clone(),
        reason: s.reason.clone(),
    }));
    for (tool, status) in &plugins.status {
        linter_status.insert(tool.clone(), status.clone());
    }

    findings.retain(|f| rule_selected(args, &f.rule) && severity_kept(args.severity, f.severity));
    findings.sort_by(|a, b| (&a.file, a.line, &a.rule).cmp(&(&b.file, b.line, &b.rule)));

    let added = added_of(
        cfg,
        repo.as_ref(),
        base.as_deref(),
        args.no_cache,
        &findings,
        deadline,
        &plugins,
    );

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

/// The plugin host over the scan's named files. `--plugins` mirrors
/// `--linters`; the stage is `Scan`, or `Check` for `agent-slop check` — the
/// Stop gate never runs plugins. Under `--since`, a deterministic comment
/// plugin also runs over the changed files as they were at the base, so its
/// inherited findings sit in the baseline instead of counting as added
/// (D3's spirit).
///
/// The two passes of a deterministic comments plugin split the deadline:
/// its head call may spend at most half of what is left when the plugin
/// pass starts, the baseline pass gets everything the head pass did not
/// use. Other plugins run once and keep the whole deadline. A plugin whose baseline pass did not run to the
/// end is not attributable: its findings are reported, never counted as
/// added, and the skip is surfaced as `baseline-<reason>`.
fn run_plugins(
    cfg: &ResolvedConfig,
    plugins_arg: Option<&str>,
    ctx: &PluginCtx<'_>,
    contents: &[(String, String)],
) -> agent_slop_integrations::plugins::PluginRun {
    use agent_slop_integrations::plugins::{DescribeCache, Opts, Selection, Skipped, run};
    let selection = plugins_arg.map_or(Selection::Auto, Selection::parse);
    let cache = DescribeCache::from_env();
    let baseline_pass = ctx.repo.zip(ctx.base);
    let attributable_deadline = baseline_pass.and_then(|_| half_of_remaining(ctx.deadline));
    let mut plugins = run(
        cfg,
        &Opts {
            selection: &selection,
            stage: ctx.stage,
            deadline: ctx.deadline,
            attributable_deadline,
        },
        ctx.root,
        contents,
        &cache,
    );
    if let Some((repo, base)) = baseline_pass
        && !plugins.attributable.is_empty()
    {
        let blobs: Vec<(String, String)> = contents
            .iter()
            .filter_map(|(p, _)| repo.show(base, p).map(|c| (p.clone(), c)))
            .collect();
        // No file existed at the base: there is nothing inherited to find.
        if blobs.is_empty() {
            return plugins;
        }
        let narrow = Selection::Named(plugins.attributable.iter().cloned().collect());
        let baseline = run(
            cfg,
            &Opts {
                selection: &narrow,
                stage: ctx.stage,
                deadline: ctx.deadline,
                attributable_deadline: None,
            },
            ctx.root,
            &blobs,
            &cache,
        );
        for name in std::mem::take(&mut plugins.attributable) {
            if baseline.settled.contains(&name) {
                plugins.attributable.insert(name);
                continue;
            }
            let reason = baseline
                .skipped
                .iter()
                .find(|s| s.tool == name)
                .map_or("incomplete", |s| s.reason.as_str());
            plugins.skipped.push(Skipped {
                reason: format!("baseline-{reason}"),
                tool: name,
            });
        }
        plugins.baseline = baseline.findings;
    }
    plugins
}

/// The instant halfway between now and `deadline` (the deadline itself when
/// it has passed or there is none).
fn half_of_remaining(deadline: Option<Instant>) -> Option<Instant> {
    let deadline = deadline?;
    let now = Instant::now();
    Some(
        now.checked_add(deadline.saturating_duration_since(now) / 2)
            .unwrap_or(deadline),
    )
}

/// What the plugin pass needs to know about where the scan runs.
struct PluginCtx<'a> {
    root: &'a Path,
    repo: Option<&'a Repo>,
    base: Option<&'a str>,
    deadline: Option<Instant>,
    stage: PluginStage,
}

/// The ratchet: multiset difference against the baseline, with the
/// baseline pass's plugin findings folded in. Findings from plugins that
/// cannot be attributed (file input, non-deterministic) are never counted
/// as added — unattributable debt never blocks (SPEC-006 rule 3's spirit).
fn added_of(
    cfg: &ResolvedConfig,
    repo: Option<&Repo>,
    base: Option<&str>,
    no_cache: bool,
    findings: &[Finding],
    deadline: Option<Instant>,
    plugins: &agent_slop_integrations::plugins::PluginRun,
) -> Vec<String> {
    let (Some(repo), Some(base)) = (repo, base) else {
        return Vec::new();
    };
    let attributable = |f: &Finding| match &f.source {
        agent_slop_core::finding::Source::Plugin { tool } => plugins.attributable.contains(tool),
        _ => true,
    };
    let cache = Cache::open(!no_cache);
    let paths: Vec<String> = findings.iter().map(|f| f.file.clone()).collect();
    let mut baseline = ratchet::baseline_at(cfg, repo, base, &paths, &cache, deadline);
    for f in &plugins.baseline {
        baseline.add(&f.fingerprint);
    }
    let current: Vec<Finding> = findings
        .iter()
        .filter(|f| attributable(f))
        .cloned()
        .collect();
    ratchet::added(&current, &baseline)
        .into_iter()
        .map(|f| f.fingerprint)
        .collect()
}

/// The stdout contract. `judged` is positionally aligned with
/// `report.findings` and carries the Laya second verdict where one was
/// printed-worthy; it is consumed by the human renderer only, so the JSON
/// contract in SPEC-006 stays byte-identical when the model is on.
pub(crate) fn render(
    report: &Report,
    fmt: Format,
    judged: &[Option<judge::Verdict>],
) -> Vec<String> {
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
pub(crate) fn envelope(report: &Report) -> String {
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
            paths: paths.iter().map(std::string::ToString::to_string).collect(),
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
        assert_eq!(
            report.findings,
            [] as [agent_slop_core::finding::Finding; 0]
        );
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
        assert_eq!(
            errors_only.findings,
            [] as [agent_slop_core::finding::Finding; 0]
        );
    }

    #[test]
    fn an_excluded_path_is_skipped_with_its_reason() {
        let s = scratch("exclude");
        std::fs::create_dir_all(s.0.join("vendor")).expect("dir");
        std::fs::write(s.0.join("vendor/a.py"), SLOP).expect("write");
        let mut cfg = builtin_config();
        cfg.exclude.paths = vec!["vendor/**".to_string()];
        let report = run(&cfg, &args(&[]), &s.0, false);
        assert_eq!(
            report.findings,
            [] as [agent_slop_core::finding::Finding; 0]
        );
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
        assert_eq!(
            report.findings,
            [] as [agent_slop_core::finding::Finding; 0]
        );
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
