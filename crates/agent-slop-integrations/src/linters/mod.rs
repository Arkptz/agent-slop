//! Linter adapters: the repo's own linters folded into one finding model
//! (SPEC-006 "Linter adapters").
//!
//! Parsing is a pure function over captured output, separate from
//! invocation, so every adapter test feeds a fixture recorded from the
//! real binary instead of spawning one. Exit status is never an error
//! signal — statix exits 1 on findings, go vet exits 0 with findings —
//! so only stdout is read.
//!
//! Adding an adapter is one `mod` line plus one entry in [`ADAPTERS`].

pub mod clippy;
pub mod deadnix;
pub mod govet;
pub mod ruff;
pub mod statix;
pub mod tsc;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use agent_slop_core::config::{ResolvedConfig, ResolvedLinter, RuleLevel};
use agent_slop_core::finding::{Finding, Severity, Source, fingerprint};

/// What a parse yields, before config decides our severity. `code` is the
/// linter's own id; deadnix emits none, so the adapter supplies its fixed
/// `dead-code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFinding {
    pub code: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub message: String,
    pub upstream_severity: Option<String>,
    pub fixable: bool,
    /// The adapter's read of the code class. `Hint` marks stylistic codes
    /// (SPEC-006 severity column); everything else is `Warning`. It can
    /// only ever weaken what config says, never reach `error`.
    pub class: Severity,
}

/// SPEC-006 cost classes. A: under 200 ms, no build. B: ~1 s process
/// start. C: seconds, needs a warm build cache. D: minutes, exclusive
/// lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cost {
    A,
    // No shipped adapter is class B yet; the class exists because
    // `eligible` compares against it and a gap in the scale would make
    // the comparison lie.
    #[allow(dead_code)]
    B,
    C,
    D,
}

/// Where the adapter is being asked to run. A hook runs without the user
/// in the loop; `scan` is someone typing the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    // Constructed only by `eligible`'s own tests in v1: the Stop leg runs
    // no linters, because its baseline is read from git blobs and a
    // linter cannot lint those without materialising a tree.
    #[allow(dead_code)]
    Hook,
    Scan,
}

/// `--linters auto|none|<name,...>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Auto,
    None,
    Named(Vec<String>),
}

impl Selection {
    pub fn parse(s: &str) -> Self {
        match s {
            "auto" => Selection::Auto,
            "none" => Selection::None,
            list => Selection::Named(
                list.split(',')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .collect(),
            ),
        }
    }
}

/// How the binary accepts targets, measured per tool: `statix check` takes
/// at most one TARGET, and the whole-project tools ignore paths entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathArity {
    Many,
    OnePerRun,
    WholeProject,
}

#[derive(Debug, Clone, Copy)]
pub struct Adapter {
    pub tool: &'static str,
    pub bin: &'static str,
    pub argv: &'static [&'static str],
    pub cost: Cost,
    /// True when the linter loads and evaluates configuration the
    /// repository supplies as code (`eslint.config.js`, `tsconfig.json`
    /// extends chains and plugins, `build.rs` and proc macros under
    /// clippy). Such an adapter must be named explicitly, whatever its
    /// cost class.
    pub evaluates_repo_code: bool,
    pub arity: PathArity,
    /// File extensions this linter can parse, without the dot. A target
    /// outside the set is never handed to it: `ruff check` given a `.nix`
    /// or `.rs` file parses it as Python and reports `invalid-syntax` per
    /// line, which on this repository alone produced ~198k fabricated
    /// findings — 98.9% of the report — and buried the real ones.
    pub exts: &'static [&'static str],
    pub parse: fn(&str) -> Vec<RawFinding>,
}

/// True when `path`'s extension is one this adapter can parse.
fn adapter_takes(a: &Adapter, path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| a.exts.contains(&ext))
}

pub static ADAPTERS: &[Adapter] = &[
    ruff::ADAPTER,
    deadnix::ADAPTER,
    statix::ADAPTER,
    clippy::ADAPTER,
    govet::ADAPTER,
    tsc::ADAPTER,
];

/// A tool or path left out of the run, carried to the `skipped[]` array of
/// the JSON envelope. Never an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub tool: String,
    pub reason: String,
}

/// May this adapter run here? Two independent gates, and the code-execution
/// one is stricter than cost alone: an adapter that evaluates repository
/// code is never selected by `auto` and never runs in a hook, because a
/// hook fires on a clone the user never inspected.
#[must_use]
pub fn eligible(a: &Adapter, stage: Stage, selection: &Selection, cfg: &ResolvedConfig) -> bool {
    if !cfg.linters.get(a.tool).is_none_or(|l| l.enable) {
        return false;
    }
    let named = match selection {
        Selection::None => return false,
        Selection::Auto => false,
        Selection::Named(names) => {
            if !names.iter().any(|n| n == a.tool) {
                return false;
            }
            true
        }
    };
    if a.evaluates_repo_code && !(named && stage == Stage::Scan) {
        return false;
    }
    match stage {
        Stage::Hook => a.cost == Cost::A,
        Stage::Scan => a.cost != Cost::D || named,
    }
}

/// PATH resolution, done once per run: an absent linter is a `skipped`
/// entry, not an error.
#[derive(Debug, Default)]
pub struct PathIndex {
    dirs: Vec<std::path::PathBuf>,
}

impl PathIndex {
    #[must_use]
    pub fn from_env() -> Self {
        let dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Self { dirs }
    }

    #[must_use]
    pub fn has(&self, bin: &str) -> bool {
        self.dirs.iter().any(|d| d.join(bin).is_file())
    }
}

/// Run an adapter from `repo_root` so the linter's own config discovery
/// fires, and hand it repo-relative paths. Stdout is returned whatever the
/// exit status; a spawn failure is a `Skipped`, never an error.
///
/// `OnePerRun` is honoured by invoking the tool once per target and
/// concatenating stdout: `statix check` takes at most one TARGET and
/// answers a second one with a clap usage error on stderr, exit 2, and an
/// EMPTY stdout — which parses as zero findings and records the tool as
/// `ok`. Handing it the whole list therefore turned every multi-file nix
/// change into a silent no-lint. The recorded clap error is
/// `tests/linters/fixtures/statix-two-targets.stderr`.
///
/// # Errors
///
/// Returns [`Skipped`] when the binary is absent, no target matches the
/// adapter's extensions, or the process cannot be spawned.
pub fn run(
    a: &Adapter,
    repo_root: &Path,
    paths: &[String],
    index: &PathIndex,
) -> Result<String, Skipped> {
    if !index.has(a.bin) {
        return Err(Skipped {
            tool: a.tool.to_string(),
            reason: "absent".to_string(),
        });
    }
    if a.arity == PathArity::WholeProject {
        return exec(a, repo_root, &[]);
    }
    let mine: Vec<&String> = paths.iter().filter(|p| adapter_takes(a, p)).collect();
    // A path-taking linter handed NO paths lints the working directory
    // instead of nothing, so an empty set must skip the exec entirely.
    if mine.is_empty() {
        return Err(Skipped {
            tool: a.tool.to_string(),
            reason: "no-targets".to_string(),
        });
    }
    // WholeProject returned above; the match needs both arms to stay total.
    match a.arity {
        PathArity::WholeProject => unreachable!("returned above"),
        PathArity::Many => {
            let owned: Vec<String> = mine.into_iter().cloned().collect();
            exec(a, repo_root, &owned)
        }
        PathArity::OnePerRun => {
            let mut out = String::new();
            for p in mine {
                out.push_str(&exec(a, repo_root, std::slice::from_ref(p))?);
            }
            Ok(out)
        }
    }
}

fn exec(a: &Adapter, repo_root: &Path, paths: &[String]) -> Result<String, Skipped> {
    let mut cmd = Command::new(a.bin);
    cmd.current_dir(repo_root).args(a.argv).args(paths);
    match cmd.output() {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(e) => Err(Skipped {
            tool: a.tool.to_string(),
            reason: format!("spawn failed: {e}"),
        }),
    }
}

/// Repo-relative, '/'-separated. Linters differ in what they echo back:
/// ruff prints the absolute path it was handed, deadnix prints `./a.nix`
/// for a directory walk and `a.nix` for named files.
#[must_use]
pub fn normalize_path(raw: &str, repo_root: &Path) -> String {
    let slashed = raw.replace('\\', "/");
    let rel = Path::new(&slashed)
        .strip_prefix(repo_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(slashed);
    rel.trim_start_matches("./").to_string()
}

/// `select`/`ignore` are applied HERE, after parsing: ruff's `--select`
/// replaces the project's own selection rather than adding to it
/// (measured, SPEC-006), so the flag is never passed and the filter lives
/// on our side. Matching is by prefix, which is how the linters' own code
/// families work (`F` selects `F401`).
fn selected(code: &str, linter: &ResolvedLinter) -> bool {
    if let Some(select) = &linter.select
        && !select.iter().any(|s| code.starts_with(s.as_str()))
    {
        return false;
    }
    if let Some(ignore) = &linter.ignore
        && ignore.iter().any(|s| code.starts_with(s.as_str()))
    {
        return false;
    }
    true
}

/// Imported findings never reach `error` on the linter's own say-so: the
/// only path to `error` is an explicit `overrides` entry, or a stranger's
/// `ruff.toml` would decide when the agent gets blocked.
fn severity_of(
    cfg: &ResolvedConfig,
    rule_id: &str,
    tool: &str,
    class: Severity,
) -> Option<Severity> {
    if let Some(level) = cfg.overrides.get(rule_id) {
        return level.severity();
    }
    let level = cfg
        .linters
        .get(tool)
        .map_or(RuleLevel::Warning, |l| l.severity);
    let base = level.severity()?;
    let capped = if base == Severity::Error {
        Severity::Warning
    } else {
        base
    };
    Some(if class == Severity::Hint {
        Severity::Hint
    } else {
        capped
    })
}

/// Default snippet for an imported finding: the linter's message. The
/// source line is not read here — the ratchet only needs a stable identity
/// per finding, and a message survives reindentation where a line does not.
#[must_use]
pub fn snippet_from_message(raw: &RawFinding) -> String {
    raw.message.chars().take(512).collect()
}

/// Raw findings to the shared model. Foreign ids are never rewritten, so
/// telemetry joins with upstream documentation.
#[must_use]
pub fn to_findings(
    cfg: &ResolvedConfig,
    tool: &str,
    raws: Vec<RawFinding>,
    repo_root: &Path,
) -> Vec<Finding> {
    let default_linter = ResolvedLinter::default();
    let linter = cfg.linters.get(tool).unwrap_or(&default_linter);
    let mut out = Vec::new();
    for raw in raws {
        if !selected(&raw.code, linter) {
            continue;
        }
        let rule = format!("{tool}/{}", raw.code);
        let Some(severity) = severity_of(cfg, &rule, tool, raw.class) else {
            continue;
        };
        let snippet = snippet_from_message(&raw);
        out.push(Finding {
            fingerprint: fingerprint(&rule, &snippet),
            rule,
            severity,
            upstream_severity: raw.upstream_severity,
            file: normalize_path(&raw.file, repo_root),
            line: raw.line,
            col: raw.col,
            end_line: raw.end_line,
            message: raw.message,
            snippet,
            source: Source::Linter {
                tool: tool.to_string(),
            },
            lang: None,
            suppression: None,
            fixable: raw.fixable,
        });
    }
    out
}

/// Per-tool status for the telemetry row (`linters: {"ruff": "ok"}`).
#[must_use]
pub fn status_map(ok: &[&str], skipped: &[Skipped]) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = ok
        .iter()
        .map(|t| (t.to_string(), "ok".to_string()))
        .collect();
    for s in skipped {
        m.insert(s.tool.clone(), format!("skipped:{}", s.reason));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_slop_core::config::builtin_config;

    fn raw(code: &str, class: Severity) -> RawFinding {
        RawFinding {
            code: code.to_string(),
            file: "a.py".to_string(),
            line: 1,
            col: 1,
            end_line: 1,
            message: "m".to_string(),
            upstream_severity: Some("error".to_string()),
            fixable: false,
            class,
        }
    }

    /// `agent-slop-core` cannot see `ADAPTERS` (the dependency points the
    /// other way), so its reserved-producer list is a copy. This ties the
    /// two: a new adapter must be reserved, or a custom rule or plugin
    /// could claim its `<tool>/<code>` namespace (owner decision Q6).
    #[test]
    fn reserved_producers_are_exactly_the_engine_and_the_adapters() {
        use agent_slop_core::rules::custom::RESERVED_PRODUCERS;
        let engine = ["slop", "suppression"];
        let mut expected: Vec<&str> = engine
            .iter()
            .copied()
            .chain(ADAPTERS.iter().map(|a| a.tool))
            .collect();
        let mut reserved = RESERVED_PRODUCERS.to_vec();
        expected.sort_unstable();
        reserved.sort_unstable();
        assert_eq!(
            reserved, expected,
            "RESERVED_PRODUCERS (core rules/custom.rs) and ADAPTERS drifted: \
             reserve every adapter's `tool`, and nothing else but the engine's own"
        );
    }

    #[test]
    fn auto_never_selects_an_adapter_that_evaluates_repo_code() {
        let cfg = builtin_config();
        let eslint = Adapter {
            tool: "eslint",
            bin: "eslint",
            argv: &[],
            cost: Cost::B,
            evaluates_repo_code: true,
            arity: PathArity::Many,
            exts: &["js", "ts"],
            parse: |_| Vec::new(),
        };
        assert!(!eligible(&eslint, Stage::Scan, &Selection::Auto, &cfg));
        assert!(!eligible(&eslint, Stage::Hook, &Selection::Auto, &cfg));
        assert!(!eligible(
            &eslint,
            Stage::Hook,
            &Selection::Named(vec!["eslint".into()]),
            &cfg
        ));
        assert!(eligible(
            &eslint,
            Stage::Scan,
            &Selection::Named(vec!["eslint".into()]),
            &cfg
        ));
    }

    #[test]
    fn only_cost_a_runs_in_a_hook() {
        let cfg = builtin_config();
        assert!(eligible(
            &ruff::ADAPTER,
            Stage::Hook,
            &Selection::Auto,
            &cfg
        ));
        let slow = Adapter {
            cost: Cost::B,
            ..cheap_clone(&ruff::ADAPTER)
        };
        assert!(!eligible(&slow, Stage::Hook, &Selection::Auto, &cfg));
        assert!(eligible(&slow, Stage::Scan, &Selection::Auto, &cfg));
    }

    fn cheap_clone(a: &Adapter) -> Adapter {
        Adapter {
            tool: a.tool,
            bin: a.bin,
            argv: a.argv,
            cost: a.cost,
            evaluates_repo_code: a.evaluates_repo_code,
            arity: a.arity,
            exts: a.exts,
            parse: a.parse,
        }
    }

    #[test]
    fn selection_none_disables_everything() {
        let cfg = builtin_config();
        assert!(!eligible(
            &ruff::ADAPTER,
            Stage::Scan,
            &Selection::None,
            &cfg
        ));
    }

    #[test]
    fn an_imported_finding_never_reaches_error_without_an_override() {
        let mut cfg = builtin_config();
        cfg.linters.insert(
            "ruff".to_string(),
            ResolvedLinter {
                severity: RuleLevel::Error,
                ..ResolvedLinter::default()
            },
        );
        let f = to_findings(
            &cfg,
            "ruff",
            vec![raw("F401", Severity::Warning)],
            Path::new("/r"),
        );
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].rule, "ruff/F401");

        cfg.overrides
            .insert("ruff/F401".to_string(), RuleLevel::Error);
        let f = to_findings(
            &cfg,
            "ruff",
            vec![raw("F401", Severity::Warning)],
            Path::new("/r"),
        );
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn stylistic_codes_land_as_hint() {
        let cfg = builtin_config();
        let f = to_findings(
            &cfg,
            "ruff",
            vec![raw("E501", Severity::Hint)],
            Path::new("/r"),
        );
        assert_eq!(f[0].severity, Severity::Hint);
    }

    #[test]
    fn select_and_ignore_filter_by_prefix_after_parsing() {
        let mut cfg = builtin_config();
        cfg.linters.insert(
            "ruff".to_string(),
            ResolvedLinter {
                select: Some(vec!["F".to_string()]),
                ignore: Some(vec!["F811".to_string()]),
                ..ResolvedLinter::default()
            },
        );
        let raws = vec![
            raw("F401", Severity::Warning),
            raw("F811", Severity::Warning),
            raw("E501", Severity::Hint),
        ];
        let got: Vec<String> = to_findings(&cfg, "ruff", raws, Path::new("/r"))
            .into_iter()
            .map(|f| f.rule)
            .collect();
        assert_eq!(got, vec!["ruff/F401"]);
    }

    #[test]
    fn a_disabled_linter_is_not_eligible() {
        let mut cfg = builtin_config();
        cfg.linters.insert(
            "ruff".to_string(),
            ResolvedLinter {
                enable: false,
                ..ResolvedLinter::default()
            },
        );
        assert!(!eligible(
            &ruff::ADAPTER,
            Stage::Scan,
            &Selection::Auto,
            &cfg
        ));
    }

    #[test]
    fn paths_normalize_to_repo_relative_slash_form() {
        let root = Path::new("/home/u/repo");
        assert_eq!(normalize_path("/home/u/repo/src/a.py", root), "src/a.py");
        assert_eq!(normalize_path("./a.nix", root), "a.nix");
        assert_eq!(normalize_path("a.nix", root), "a.nix");
    }

    #[test]
    fn an_absent_binary_is_skipped_not_an_error() {
        let index = PathIndex { dirs: Vec::new() };
        let err = run(&ruff::ADAPTER, Path::new("/"), &[], &index).unwrap_err();
        assert_eq!(err.reason, "absent");
        assert_eq!(err.tool, "ruff");
    }

    #[test]
    fn status_map_reports_ok_and_skip_reasons() {
        let skipped = vec![Skipped {
            tool: "clippy".into(),
            reason: "no-cargo".into(),
        }];
        let m = status_map(&["ruff"], &skipped);
        assert_eq!(m["ruff"], "ok");
        assert_eq!(m["clippy"], "skipped:no-cargo");
    }

    /// A stand-in for a `OnePerRun` tool, behaving as the real `statix
    /// check` does: one TARGET prints a JSON object, a second one is a clap
    /// usage error on stderr with EMPTY stdout
    /// (`tests/linters/fixtures/statix-two-targets.stderr`). Handing such a
    /// tool the whole path list yields zero findings while the run still
    /// looks `ok`, so the arity has to be honoured by the runner, not just
    /// declared by the adapter.
    ///
    /// The tool is `/bin/sh -c <script> faketix`, and nothing is written to
    /// disk. A `#!/bin/sh` file on disk was the obvious fixture and it was
    /// wrong twice over: the runner execs a `OnePerRun` tool once per target,
    /// and Linux refuses to exec a script whose previous execution has not
    /// finished tearing down (ETXTBSY). That is a race, not a rare state —
    /// it took roughly one run in five here and failed a NixOS rebuild on a
    /// machine with nothing wrong with it. A fixture that writes an
    /// executable tests the filesystem's teardown timing as a side effect of
    /// testing the arity; this one tests the arity and nothing else.
    fn fake_one_per_run(dir: &Path) -> (&'static str, PathIndex) {
        (
            "/bin/sh",
            PathIndex {
                dirs: vec![dir.to_path_buf()],
            },
        )
    }

    #[test]
    fn one_per_run_invokes_the_tool_once_per_target() {
        let dir = std::env::temp_dir().join(format!("agent-slop-arity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (bin, index) = fake_one_per_run(&dir);
        let adapter = Adapter {
            tool: "faketix",
            bin,
            argv: &[
                "-c",
                "if [ \"$#\" -gt 1 ]; then echo \"error: unexpected argument '$2' found\" >&2; exit 2; fi; echo \"{\\\"target\\\":\\\"$1\\\"}\"",
                "faketix",
            ],
            cost: Cost::A,
            evaluates_repo_code: false,
            arity: PathArity::OnePerRun,
            exts: &["nix"],
            parse: |_| Vec::new(),
        };
        let paths = vec!["one.nix".to_string(), "two.nix".to_string()];
        let out = run(&adapter, &dir, &paths, &index).expect("runs");
        assert!(
            out.contains("one.nix"),
            "first target must be linted: {out:?}"
        );
        assert!(
            out.contains("two.nix"),
            "the second target is lost when the whole list goes in one exec: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A target the tool cannot parse never reaches it. `ruff check` on a
    /// `.nix` file reports one `invalid-syntax` per line; unfiltered, that
    /// was 98.9% of a whole-repo report.
    #[test]
    fn an_adapter_only_receives_the_extensions_it_parses() {
        let dir = std::env::temp_dir().join(format!("agent-slop-exts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (bin, index) = fake_one_per_run(&dir);
        let adapter = Adapter {
            tool: "faketix",
            bin,
            argv: &["-c", "echo \"{\\\"target\\\":\\\"$1\\\"}\"", "faketix"],
            cost: Cost::A,
            evaluates_repo_code: false,
            arity: PathArity::OnePerRun,
            exts: &["nix"],
            parse: |_| Vec::new(),
        };
        let paths = vec![
            "one.nix".to_string(),
            "noise.rs".to_string(),
            "noise.yaml".to_string(),
        ];
        let out = run(&adapter, &dir, &paths, &index).expect("runs");
        assert!(out.contains("one.nix"), "its own extension: {out:?}");
        assert!(!out.contains("noise.rs"), "foreign extension: {out:?}");
        assert!(!out.contains("noise.yaml"), "foreign extension: {out:?}");

        // No parsable target at all: skip the exec, because a path-taking
        // linter given no paths lints the whole working directory.
        let err = run(&adapter, &dir, &["only.rs".to_string()], &index)
            .expect_err("no parsable target must skip");
        assert_eq!(err.reason, "no-targets");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
