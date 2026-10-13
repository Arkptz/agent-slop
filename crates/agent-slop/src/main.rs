//! agent-slop — CLI dispatch (SPEC-006 "CLI surface and exit codes").
//! The exit-code contract this module owns: scan 0/1/3, check always 0,
//! `PostToolUse` always 0, Stop 0-or-2, 3 only for CLI usage errors. The
//! binary never exits nonzero on an internal error, which is what makes
//! the hook wrapper's "anything outside {0,2} means it crashed, so allow"
//! rule safe.

mod cli;
mod hook;
mod ratchet;
mod scan;

/// Milliseconds since `started`, saturating at `u64::MAX`.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

use agent_slop_core::{config, finding, rules};
use agent_slop_integrations::{judge, telemetry};

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cli::{Cli, FailOn, Format, ScanArgs};
use config::{ResolvedConfig, RuleLevel};

fn main() -> ExitCode {
    match cli::parse() {
        Cli::Scan(args) => run_scan(&args),
        Cli::Check { files } => run_check(&files),
        Cli::Hook => ExitCode::from(hook::run()),
        Cli::Explain { rule_id } => run_explain(&rule_id),
        Cli::Rules { format } => run_rules(format),
        Cli::Baseline { since } => run_baseline(since.as_deref()),
        Cli::SelfTest => run_self_test(),
    }
}

/// The wall clock the whole scan may take, judge included: the judge shares
/// the leg's budget rather than adding one of its own, so switching the
/// model on cannot make a scan overstay the time its caller allowed. An
/// overflowing `checked_add` yields "now", i.e. no budget left.
fn deadline(cfg: &ResolvedConfig) -> Instant {
    Instant::now()
        .checked_add(Duration::from_millis(cfg.timeout_ms))
        .unwrap_or_else(Instant::now)
}

/// The same clock, from the caller's own number when it gave one.
/// `--deadline-ms` is documented as bounding the command, and a judge that
/// quietly kept the 4 s default would make that flag lie on a long run.
fn deadline_of(cfg: &ResolvedConfig, args: &ScanArgs) -> Instant {
    let ms = args.deadline_ms.unwrap_or(cfg.timeout_ms);
    Instant::now()
        .checked_add(Duration::from_millis(ms))
        .unwrap_or_else(Instant::now)
}

fn run_scan(args: &ScanArgs) -> ExitCode {
    let cfg = config::load();
    print_notes(&cfg);

    let cwd = std::env::current_dir().unwrap_or_default();
    let started = std::time::Instant::now();
    let report = scan::run(&cfg, args, &cwd, true);
    let duration_ms = elapsed_ms(started);
    let fail_on = args.fail_on.unwrap_or(FailOn::Never);
    let tripped = report.findings.iter().any(|f| fail_on.trips(f.severity));

    let fmt = args.format.unwrap_or({
        // SPEC-006 default: human on tty, json otherwise.
        if std::io::stdout().is_terminal() {
            Format::Human
        } else {
            Format::Json
        }
    });
    for line in scan::render(
        &report,
        fmt,
        &judge::annotate(
            &cfg,
            &report.findings,
            &cwd,
            deadline_of(&cfg, args),
            args.max_verdicts,
        ),
    ) {
        out(&line);
    }
    // The second axis needs a diff, and a diff is what `scan --since` already
    // asks for. It is the axis that fires with no rule behind it, so on a
    // branch whose commits are mostly non-comment code it is the only thing
    // the model has to say — and until this existed, `scan` could not ask it
    // at all: the needs half lived in the PostToolUse leg alone. Gated on
    // `--max-verdicts` being passed, because that flag is the caller saying
    // "audit", and a plain scan's output must not grow a section.
    if args.max_verdicts.is_some() && fmt == Format::Human {
        let base = ratchet::Repo::discover(&cwd).and_then(|repo| {
            repo.resolve_ref(args.since.as_deref().unwrap_or(&cfg.ref_))
                .map(|base| (repo.root, base))
        });
        if let Some((root, base)) = base {
            for hit in judge::audit_needs(
                &cfg,
                &root,
                &base,
                args.max_verdicts,
                deadline_of(&cfg, args),
            ) {
                out(&hit.render());
            }
        }
    }

    if args.emit_telemetry {
        // `added` is fingerprints here; the rule histogram needs the
        // findings they name.
        let added: Vec<finding::Finding> = report
            .findings
            .iter()
            .filter(|f| report.added.contains(&f.fingerprint))
            .cloned()
            .collect();
        let row = telemetry::build(&telemetry::RowInput {
            event: "scan",
            harness: telemetry::harness(),
            session_id: std::env::var("GROK_SESSION_ID").ok().as_deref(),
            parent_session_id: None,
            subagent_type: None,
            cwd: &cwd,
            findings: &report.findings,
            added: &added,
            mode: cfg.mode,
            ref_: args.since.as_deref().unwrap_or("none"),
            blocked: false,
            would_block: false,
            files_scanned: report.files_scanned,
            duration_ms,
            linters: report.linters.clone(),
        });
        let _ = telemetry::write(&row, args.telemetry_root.as_deref());
    }

    if tripped {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `check` is advisory: named files, no git, no ratchet, always 0.
fn run_check(files: &[String]) -> ExitCode {
    let cfg = config::load();
    let cwd = std::env::current_dir().unwrap_or_default();
    let args = ScanArgs {
        paths: files.to_vec(),
        linters: Some("none".to_string()),
        ..ScanArgs::default()
    };
    let report = scan::run(&cfg, &args, &cwd, false);
    // `check` is where the PostToolUse advice sends an agent ("run
    // agent-slop check <path>"), so it is the surface where a second
    // verdict is most likely to be looked for. Judged like `scan`, under the
    // same wall clock: the advice line and the command it points at must not
    // disagree about what the model said.
    for line in scan::render(
        &report,
        Format::Human,
        &judge::annotate(&cfg, &report.findings, &cwd, deadline(&cfg), None),
    ) {
        out(&line);
    }
    ExitCode::SUCCESS
}

fn run_explain(rule_id: &str) -> ExitCode {
    let cfg = config::load();
    print_notes(&cfg);
    match level_of(&cfg, rule_id) {
        Some(level) => {
            out(&format!("{rule_id}: {}", level_str(level)));
            let corpus = rules::corpus::Corpus::load();
            if let Some(rule) = rule_id
                .strip_prefix("slop/")
                .and_then(|family| corpus.rules.get(family))
            {
                out(&format!("  do instead: {}", rule.note.text.trim()));
                if !rule.exception.shape.trim().is_empty() {
                    out(&format!("  exception: {}", rule.exception.shape.trim()));
                }
            }
            if rule_id.starts_with("slop/") {
                out(&format!(
                    "  suppress: // agent-slop: allow {rule_id} -- <reason with a referent: \
                     URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>"
                ));
            } else {
                out(
                    "  imported rules suppress via their own linter mechanism (noqa, allow, disable)",
                );
            }
        }
        None => out(&format!(
            "{rule_id}: no config entry (off, foreign, or unknown)"
        )),
    }
    ExitCode::SUCCESS
}

fn run_rules(format: Option<Format>) -> ExitCode {
    let cfg = config::load();
    print_notes(&cfg);

    // Families as full ids, then the overrides table on top.
    let mut levels: BTreeMap<String, RuleLevel> = cfg
        .rules
        .iter()
        .map(|(family, level)| (format!("slop/{family}"), *level))
        .collect();
    for (rule_id, level) in &cfg.overrides {
        levels.insert(rule_id.clone(), *level);
    }

    match format {
        Some(Format::Json) => {
            let rules: Vec<_> = levels
                .iter()
                .map(|(id, level)| serde_json::json!({"id": id, "level": level_str(*level)}))
                .collect();
            out(&serde_json::json!({"schema": 1, "rules": rules}).to_string());
        }
        _ => {
            for (id, level) in &levels {
                out(&format!("{id:<28} {}", level_str(*level)));
            }
        }
    }
    ExitCode::SUCCESS
}

/// The baseline multiset the Stop gate would subtract, for debugging a
/// verdict. Outside a repository, or for a ref that does not resolve, the
/// list is empty — this command diagnoses, it never fails.
fn run_baseline(since: Option<&str>) -> ExitCode {
    let cfg = config::load();
    let cwd = std::env::current_dir().unwrap_or_default();
    let fingerprints = ratchet::Repo::discover(&cwd)
        .zip(since)
        .and_then(|(repo, spec)| {
            let base = repo.resolve_ref(spec)?;
            let mut paths = repo.changed_files(&base);
            paths.extend(repo.untracked_files());
            paths.sort();
            paths.dedup();
            let cache = ratchet::Cache::open(true);
            Some(ratchet::baseline_at(&cfg, &repo, &base, &paths, &cache, None).fingerprints())
        })
        .unwrap_or_default();
    out(
        &serde_json::json!({"schema": 1, "since": since, "fingerprints": fingerprints}).to_string(),
    );
    ExitCode::SUCCESS
}

/// Which families the Stop gate may block on. Frozen by the owner
/// decision of 2026-09-21 and amended with the TASK-019 flip on
/// 2026-10-03, which demoted comment-meta and review-jargon to
/// `warning` after 12 days of telemetry; the reasoning lives in
/// SPEC-006.
const FROZEN_BLOCKING: [&str; 1] = ["slop/comment-history"];

/// What POL-026 wants observable here is the gate as it will actually
/// run, which is the RESOLVED config — the repo's `.agent-hooks.toml` can
/// demote a frozen rule, promote a fourth, or flip `mode` back to
/// `observe`, and against the built-in tier alone every one of those
/// passed. Each verdict names the tier that produced it. Embedded rule
/// fixtures arrive with the rule-corpus work package.
fn run_self_test() -> ExitCode {
    let (user_path, repo_path) = config::tier_paths();
    let builtin = config::builtin_config();
    let user = config::load_from_paths(user_path.as_deref(), None);
    let cfg = config::load_from_paths(user_path.as_deref(), repo_path.as_deref());
    let stages = [&builtin, &user, &cfg];
    let mut checks = 0usize;
    let mut failed = 0usize;

    for (label, path) in [("user", &user_path), ("repo", &repo_path)] {
        match path {
            Some(p) if p.exists() => out(&format!("self-test: {label} tier {}", p.display())),
            Some(p) => out(&format!("self-test: {label} tier absent ({})", p.display())),
            None => out(&format!("self-test: {label} tier absent")),
        }
    }
    print_notes(&cfg);

    for family in config::KNOWN_FAMILIES {
        checks += 1;
        if !cfg.rules.contains_key(family) {
            failed += 1;
            err(&format!("self-test: config missing family {family}"));
        }
    }

    checks += 1;
    let blocking = blocking_slop_rules(&cfg);
    if blocking.iter().map(String::as_str).ne(FROZEN_BLOCKING) {
        failed += 1;
        err("self-test: the effective blocking set is not the frozen one (SPEC-006)");
        for id in FROZEN_BLOCKING
            .iter()
            .map(|s| (*s).to_string())
            .chain(blocking.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let frozen = FROZEN_BLOCKING.contains(&id.as_str());
            let effective = blocking.contains(&id);
            if frozen == effective {
                continue;
            }
            let verb = if frozen { "demoted" } else { "promoted" };
            err(&format!(
                "self-test:   {id} {verb} by the {} tier (now {})",
                tier_of(&stages, |c| level_of(c, &id)),
                level_of(&cfg, &id).map_or("off", level_str)
            ));
        }
    }

    // `mode` is the gate's on/off switch: `observe` runs the Stop leg and
    // records telemetry but never exits 2. A higher tier turning a `block`
    // into `observe` is an inert gate, which is the state this command
    // exists to make visible.
    checks += 1;
    let weakened = cfg.mode == config::Mode::Observe
        && stages[..2].iter().any(|c| c.mode == config::Mode::Block);
    if weakened {
        failed += 1;
        err(&format!(
            "self-test: mode weakened to observe by the {} tier — the Stop gate never blocks",
            tier_of(&stages, |c| c.mode)
        ));
    } else {
        out(&format!(
            "self-test: mode {:?} from the {} tier",
            cfg.mode,
            tier_of(&stages, |c| c.mode)
        ));
    }

    out(&format!(
        "self-test: {checks} checks, {failed} failed (fixtures land with the rules corpus)"
    ));
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Every `slop/*` rule the resolved config blocks on, sorted: the family
/// table and the full-id overrides together, since either can carry
/// `error`.
fn blocking_slop_rules(cfg: &ResolvedConfig) -> Vec<String> {
    let mut ids: Vec<String> = cfg
        .rules
        .keys()
        .map(|family| format!("slop/{family}"))
        .chain(cfg.overrides.keys().cloned())
        .filter(|id| id.starts_with("slop/"))
        .filter(|id| level_of(cfg, id) == Some(RuleLevel::Error))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Which tier last CHANGED the value, walking down from the effective
/// one. The lowest stage holding the same value is not the answer: with
/// the user tier at `block` and the repo tier at `observe`, the built-in
/// default is also `observe`, and naming it would credit the weakening
/// to the tier that cannot weaken anything.
fn tier_of<T: PartialEq>(
    stages: &[&ResolvedConfig; 3],
    read: impl Fn(&ResolvedConfig) -> T,
) -> &'static str {
    if read(stages[2]) != read(stages[1]) {
        "repo"
    } else if read(stages[1]) != read(stages[0]) {
        "user"
    } else {
        "built-in"
    }
}

/// Same routing as `ResolvedConfig::severity_for` but keeping `off`
/// distinct from foreign — `explain` names the level, not just a finding.
fn level_of(cfg: &ResolvedConfig, rule_id: &str) -> Option<RuleLevel> {
    if let Some(level) = cfg.overrides.get(rule_id) {
        return Some(*level);
    }
    rule_id
        .strip_prefix("slop/")
        .and_then(|family| cfg.rules.get(family))
        .copied()
}

fn level_str(level: RuleLevel) -> &'static str {
    match level {
        RuleLevel::Error => "error",
        RuleLevel::Warning => "warning",
        RuleLevel::Hint => "hint",
        RuleLevel::Off => "off",
    }
}

fn print_notes(cfg: &ResolvedConfig) {
    for note in &cfg.notes {
        err(&format!(
            "config note ({:?} tier): {}",
            note.tier, note.reason
        ));
    }
    // A family whose TOML fails to parse is dropped rather than fatal, so
    // without this line a broken rule file is a rule that silently stops
    // firing (POL-026 rule 4).
    for (family, reason) in &rules::corpus::Corpus::load().dropped {
        err(&format!("rule corpus: {family} dropped, {reason}"));
    }
}

fn out(line: &str) {
    use std::io::Write;
    // writeln, not println: a closed pipe must not panic — the wrapper
    // maps rc only for `hook`, so a panic here would escape as 101.
    let _ = writeln!(std::io::stdout(), "{line}");
}

fn err(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}
