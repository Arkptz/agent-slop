//! CLI parsing for the full SPEC-006 surface. No argument-parsing crate —
//! same hand-rolled loop as agent-feedback-miner, because the surface is
//! small and the exit codes are a published contract: 3 is a usage error
//! (unknown flag, missing or invalid value), 0 is `--help`. Nothing here
//! panics: a missing value is a usage error, never an `expect`.

use std::path::PathBuf;

use agent_slop_core::finding::Severity;

/// Exit code for a usage error (SPEC-006 "CLI surface and exit codes").
pub(crate) const EXIT_USAGE: u8 = 3;

/// The usage text is the spec's fenced block, verbatim: it is the
/// published surface, not prose about it.
pub(crate) fn usage() -> String {
    "\
agent-slop scan [PATHS...]
    --since <ref>                       scan only files changed since merge-base(ref, HEAD), ratchet against it
    --format human|json|jsonl|github    default: human on tty, json otherwise
    --severity error|warning|hint       minimum reported, default hint
    --fail-on error|warning|hint|never  default never
    --rules <glob,...> / --no-rules <glob,...>
    --linters auto|none|<name,...>      default auto (cost class A+B only)
    --plugins auto|none|<name,...>      default auto (external plugins, [slop.plugins])
    --comment-lang en|ru|auto
    --strict-suppressions
    --no-cache / --deadline-ms <n>
    --max-verdicts <n>                 0 = every comment; also audits needs_comment
                                       over the --since diff (human output only)
    --emit-telemetry / --telemetry-root <dir>
  exit: 0 nothing at or above --fail-on | 1 something | 3 usage error

agent-slop check <FILE>...     single advisory scan, no git needed; always 0
agent-slop hook                stdin: harness JSON; dispatches on hookEventName
                               PostToolUse → always 0, advice on STDOUT
                               SessionStart → always 0, pins the baseline
                               Stop → 0 allow | 2 block, reason on stderr
agent-slop explain <rule-id>   rule text, exception, ready suppression line; 0
agent-slop rules --format json machine-readable rule list and levels; 0
agent-slop rules lint [FILES...]  check custom rule files (every discovered tier when none
                               are named): drop reasons, strict-lint gaps, red fixtures; 0 | 1
agent-slop rules test [FILES...]  run the [[fixture]] cases of custom rules through the engine; 0 | 1
agent-slop baseline --since <ref>  print the baseline multiset (debug); 0
agent-slop self-test           embedded fixtures; 0 | 1    ← POL-026 evidence"
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Human,
    Json,
    Jsonl,
    Github,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailOn {
    Error,
    Warning,
    Hint,
    Never,
}

impl FailOn {
    /// `never` is satisfied by nothing, so a scan with the default
    /// threshold always exits 0.
    pub(crate) fn trips(self, severity: Severity) -> bool {
        match self {
            FailOn::Error => severity == Severity::Error,
            FailOn::Warning => matches!(severity, Severity::Error | Severity::Warning),
            FailOn::Hint => true,
            FailOn::Never => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScanArgs {
    pub paths: Vec<String>,
    pub since: Option<String>,
    /// `None`: human on tty, json otherwise — the SPEC-006 default.
    pub format: Option<Format>,
    /// Minimum reported severity. `None` means the spec default, `hint`.
    pub severity: Option<Severity>,
    /// `None` means the spec default, `never`.
    pub fail_on: Option<FailOn>,
    pub rules: Option<Vec<String>>,
    pub no_rules: Option<Vec<String>>,
    /// `auto|none|<name,...>`; `None` means `auto`.
    pub linters: Option<String>,
    /// `auto|none|<name,...>`; `None` means `auto` (external plugins).
    pub plugins: Option<String>,
    /// `en|ru|auto`; `None` leaves the config's `language` standing.
    pub comment_lang: Option<String>,
    pub strict_suppressions: bool,
    pub no_cache: bool,
    pub deadline_ms: Option<u64>,
    /// How many comments to put to the Laya judge. `None` takes
    /// `slop.judge.max_verdicts`, which is the per-hook-leg cap; `0` means
    /// every eligible comment, for an audit run. A repo has thousands of
    /// comments and a tool call has one file, so the cap that protects the
    /// hook would silently truncate a whole-repo pass.
    pub max_verdicts: Option<usize>,
    pub emit_telemetry: bool,
    pub telemetry_root: Option<PathBuf>,
    /// `Check` for `agent-slop check`, so plugins see `stage = "check"`.
    pub stage: Stage,
}

/// The command a [`ScanArgs`] was built for; plugins see it as `stage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Stage {
    /// `agent-slop scan` and every default-constructed set (tests).
    #[default]
    Scan,
    /// `agent-slop check`.
    Check,
}

pub(crate) enum Cli {
    /// `scan`, and also the bare invocation: no subcommand is a scan of
    /// the whole repo, the common human-shell case.
    Scan(Box<ScanArgs>),
    Check {
        files: Vec<String>,
    },
    Hook,
    Explain {
        rule_id: String,
    },
    /// `rules` without `--format` prints a human table.
    Rules {
        format: Option<Format>,
    },
    /// `rules lint [FILES...]`; no files means every discovered tier.
    RulesLint {
        files: Vec<String>,
    },
    /// `rules test [FILES...]`; no files means every discovered tier.
    RulesTest {
        files: Vec<String>,
    },
    /// `baseline --since <ref>`: the ref is required, enforced by the
    /// parser (a bare `baseline` is a usage error).
    Baseline {
        since: Option<String>,
    },
    SelfTest,
}

/// Parse `std::env::args()`. Exits 0 on `--help`/`-h`, 3 on a usage
/// error. Never returns an `Err`: usage errors exit directly, matching
/// the miner's pattern.
pub(crate) fn parse() -> Cli {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        return Cli::Scan(Box::default());
    };
    match cmd.as_str() {
        "scan" => Cli::Scan(Box::new(parse_scan(&mut args))),
        "check" => Cli::Check {
            files: args.collect(),
        },
        "hook" => Cli::Hook,
        "explain" => Cli::Explain {
            rule_id: required(&mut args, "explain <rule-id>"),
        },
        "rules" => parse_rules(args.collect()),
        "baseline" => Cli::Baseline {
            since: parse_baseline(&mut args),
        },
        "self-test" => Cli::SelfTest,
        "--help" | "-h" | "help" => {
            print!("{}", usage());
            std::process::exit(0);
        }
        other => usage_error(&format!("unknown subcommand: {other}")),
    }
}

fn parse_scan(args: &mut impl Iterator<Item = String>) -> ScanArgs {
    let mut o = ScanArgs::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--since" => o.since = Some(value(args, "--since")),
            "--format" => o.format = Some(parse_format(&value(args, "--format"))),
            "--severity" => o.severity = Some(parse_severity(&value(args, "--severity"))),
            "--fail-on" => o.fail_on = Some(parse_fail_on(&value(args, "--fail-on"))),
            "--rules" => o.rules = Some(comma_list(&value(args, "--rules"))),
            "--no-rules" => o.no_rules = Some(comma_list(&value(args, "--no-rules"))),
            "--linters" => o.linters = Some(value(args, "--linters")),
            "--plugins" => o.plugins = Some(value(args, "--plugins")),
            "--comment-lang" => {
                o.comment_lang = Some(parse_comment_lang(&value(args, "--comment-lang")));
            }
            "--strict-suppressions" => o.strict_suppressions = true,
            "--no-cache" => o.no_cache = true,
            "--deadline-ms" => {
                o.deadline_ms = Some(parse_u64(&value(args, "--deadline-ms"), "--deadline-ms"));
            }
            "--max-verdicts" => {
                o.max_verdicts = Some(
                    usize::try_from(parse_u64(&value(args, "--max-verdicts"), "--max-verdicts"))
                        .unwrap_or(usize::MAX),
                );
            }
            "--emit-telemetry" => o.emit_telemetry = true,
            "--telemetry-root" => {
                o.telemetry_root = Some(PathBuf::from(value(args, "--telemetry-root")));
            }
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                usage_error(&format!("unknown flag: {other}"));
            }
            other => o.paths.push(other.to_string()),
        }
    }
    o
}

fn parse_rules(args: Vec<String>) -> Cli {
    let sub = args.first().map(String::as_str);
    if matches!(sub, Some("lint" | "test")) {
        let lint = sub == Some("lint");
        let mut files = Vec::new();
        for a in &args[1..] {
            match a.as_str() {
                "--help" | "-h" => {
                    print!("{}", usage());
                    std::process::exit(0);
                }
                other if other.starts_with("--") => {
                    usage_error(&format!(
                        "unknown flag for rules {}: {other}",
                        sub.unwrap_or("")
                    ));
                }
                _ => files.push(a.clone()),
            }
        }
        return if lint {
            Cli::RulesLint { files }
        } else {
            Cli::RulesTest { files }
        };
    }
    let mut format = None;
    let mut rest = args.into_iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--format" => format = Some(parse_format(&value(&mut rest, "--format"))),
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => usage_error(&format!("unexpected argument for rules: {other}")),
        }
    }
    Cli::Rules { format }
}

fn parse_baseline(args: &mut impl Iterator<Item = String>) -> Option<String> {
    let mut since = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--since" => since = Some(value(args, "--since")),
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => usage_error(&format!("unexpected argument for baseline: {other}")),
        }
    }
    // `--since <ref>` is part of the command's synopsis, not an option.
    since.or_else(|| usage_error("baseline requires --since <ref>"))
}

/// A flag value: absent, or itself flag-like, is a missing value (e.g.
/// `--since --format json`), which is a usage error — not a guess.
fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    match args.next() {
        Some(v) if !v.starts_with("--") => v,
        Some(v) => usage_error(&format!("{flag} requires a value (got `{v}`)")),
        None => usage_error(&format!("{flag} requires a value")),
    }
}

fn required(args: &mut impl Iterator<Item = String>, what: &str) -> String {
    match args.next() {
        Some(v) if !v.starts_with("--") => v,
        Some(v) => usage_error(&format!("{what} requires an argument (got `{v}`)")),
        None => usage_error(&format!("{what} requires an argument")),
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "hint" => Severity::Hint,
        other => usage_error(&format!("invalid --severity value: {other}")),
    }
}

fn parse_fail_on(s: &str) -> FailOn {
    match s {
        "error" => FailOn::Error,
        "warning" => FailOn::Warning,
        "hint" => FailOn::Hint,
        "never" => FailOn::Never,
        other => usage_error(&format!("invalid --fail-on value: {other}")),
    }
}

fn parse_format(s: &str) -> Format {
    match s {
        "human" => Format::Human,
        "json" => Format::Json,
        "jsonl" => Format::Jsonl,
        "github" => Format::Github,
        other => usage_error(&format!("invalid --format value: {other}")),
    }
}

fn parse_comment_lang(s: &str) -> String {
    match s {
        "en" | "ru" | "auto" => s.to_string(),
        other => usage_error(&format!("invalid --comment-lang value: {other}")),
    }
}

fn parse_u64(s: &str, flag: &str) -> u64 {
    s.parse()
        .unwrap_or_else(|_| usage_error(&format!("invalid {flag} value: {s}")))
}

fn comma_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn usage_error(msg: &str) -> ! {
    eprintln!("usage error: {msg}");
    eprint!("{}", usage());
    std::process::exit(EXIT_USAGE.into());
}
