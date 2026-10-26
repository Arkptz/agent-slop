//! The plugin host: declare `[slop.plugins.<name>]`, run the process, fold
//! its findings into the shared model (owner decision 2026-10-25).
//!
//! One process per call, no daemon. Everything here is fail-open: a plugin
//! that is absent, slow, loud or broken is a *skip* keyed by its name and
//! never changes a command's exit code.

pub mod runner;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_slop_core::config::{ResolvedConfig, ResolvedPlugin, RuleLevel};
use agent_slop_core::finding::{Finding, Severity, Source, fingerprint};
use agent_slop_core::lang;
use agent_slop_protocol::{Cost, Input, Manifest, Request, RequestFile, Response, WireComment};

use crate::linters::normalize_path;

/// The host stages a plugin may be called from.
pub use agent_slop_protocol::Stage;

/// What the runner needs of a declared plugin.
#[derive(Debug)]
pub struct ResolvedPluginRef<'a> {
    pub command: &'a [String],
    pub env_passthrough: &'a [String],
}

impl<'a> From<&'a ResolvedPlugin> for ResolvedPluginRef<'a> {
    fn from(p: &'a ResolvedPlugin) -> Self {
        Self {
            command: &p.command,
            env_passthrough: &p.env_passthrough,
        }
    }
}

/// Findings one plugin may report per call; past that the run is recorded
/// as truncated.
pub const MAX_FINDINGS: usize = 5000;

/// A hook-stage call is advisory and on the model's critical path, so it
/// gets a hard ceiling no matter what `timeout_ms` says.
pub const HOOK_BUDGET_MS: u64 = 300;

/// Below this the call is not worth starting.
const MIN_BUDGET_MS: u64 = 50;

/// Why a plugin did not contribute, into `skipped[]` and the status map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub tool: String,
    pub reason: String,
}

/// `--plugins auto|none|<name,...>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Auto,
    None,
    Named(Vec<String>),
}

impl Selection {
    #[must_use]
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

    fn selects(&self, name: &str) -> bool {
        match self {
            Selection::Auto => true,
            Selection::None => false,
            Selection::Named(names) => names.iter().any(|n| n == name),
        }
    }

    fn names(&self, name: &str) -> bool {
        matches!(self, Selection::Named(names) if names.iter().any(|n| n == name))
    }
}

/// Manifests cached under `$XDG_CACHE_HOME/agent-slop/plugins/`, keyed by
/// the command line plus the size and mtime of every argv element that
/// resolves to an existing file (the executable and, for an interpreter
/// argv, the script), so a plugin upgrade or a changed argument is a new
/// key.
#[derive(Debug, Clone, Default)]
pub struct DescribeCache {
    dir: Option<PathBuf>,
}

impl DescribeCache {
    #[must_use]
    pub fn at(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    /// `$XDG_CACHE_HOME/agent-slop/plugins` (or `~/.cache/...`).
    #[must_use]
    pub fn from_env() -> Self {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| home::home_dir().map(|h| h.join(".cache")));
        Self::at(base.map(|b| b.join("agent-slop").join("plugins")))
    }

    fn key(command: &[String]) -> String {
        let mut hasher = blake3::Hasher::new();
        for part in command {
            hasher.update(part.as_bytes());
            hasher.update(b"\0");
            // An interpreter argv (`["sh", "/path/plugin.sh"]`) runs code
            // from every argv element that is a file, so each one's size
            // and mtime joins the key: editing or upgrading any of them
            // must invalidate the cached manifest.
            if let Some(meta) = resolve_exe(part).and_then(|p| std::fs::metadata(p).ok()) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                hasher.update(b"f");
                hasher.update(&meta.len().to_le_bytes());
                hasher.update(&mtime.to_le_bytes());
            }
        }
        hasher.finalize().to_hex()[..32].to_string()
    }

    fn describe(
        &self,
        plugin: &ResolvedPlugin,
        cwd: &Path,
        budget: Duration,
    ) -> Result<Manifest, String> {
        let key = Self::key(&plugin.command);
        let path = self.dir.as_ref().map(|d| d.join(format!("{key}.json")));
        if let Some(path) = &path
            && let Some(hit) = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
        {
            return Ok(hit);
        }
        let out = runner::run_process(&plugin.into(), "describe", &[], cwd, budget)?;
        let manifest: Manifest =
            serde_json::from_slice(&out).map_err(|_| "describe-failed".to_string())?;
        if let Some(path) = &path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_string(&manifest) {
                let _ = std::fs::write(path, json);
            }
        }
        Ok(manifest)
    }
}

fn resolve_exe(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        return Some(PathBuf::from(program));
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(program))
            .find(|p| p.is_file())
    })
}

/// One host invocation.
#[derive(Debug, Clone, Copy)]
pub struct Opts<'a> {
    pub selection: &'a Selection,
    pub stage: Stage,
    /// The caller's whole-command deadline, if it has one. Recomputed
    /// before every call: what one plugin spends, the next one does not
    /// get again.
    pub deadline: Option<Instant>,
}

/// The host's result for one invocation.
#[derive(Debug, Default)]
pub struct PluginRun {
    pub findings: Vec<Finding>,
    pub skipped: Vec<Skipped>,
    /// Per-plugin status for the telemetry row (`{"acme": "ok"}`).
    pub status: BTreeMap<String, String>,
    /// Plugins whose findings can be attributed to a baseline: a
    /// deterministic `input = "comments"` manifest. Everything else is
    /// reported but never counted as added.
    pub attributable: BTreeSet<String>,
    /// Plugins whose call reached a definite answer: it ran to the end
    /// uncut (no truncation, no deadline while normalising) or had no
    /// matching file. A baseline built from a plugin outside this set is
    /// incomplete and cannot be compared against.
    pub settled: BTreeSet<String>,
    /// Findings of the baseline pass (the same plugins over the files as
    /// they were at the comparison ref); the caller folds their
    /// fingerprints into the baseline multiset.
    pub baseline: Vec<Finding>,
}

impl PluginRun {
    fn skip(&mut self, name: &str, reason: impl Into<String>) {
        let reason = reason.into();
        self.status
            .insert(name.to_string(), format!("skipped:{reason}"));
        self.skipped.push(Skipped {
            tool: name.to_string(),
            reason,
        });
    }
}

/// May this manifest run at this stage? Hooks run cost A only; scan and
/// check run A–C, and D only when the plugin is named.
fn cost_allows(cost: Cost, stage: Stage, named: bool) -> bool {
    match stage {
        Stage::Hook => cost == Cost::A,
        Stage::Scan | Stage::Check => cost != Cost::D || named,
    }
}

/// Run every eligible plugin over `targets` (repo-relative path, content).
/// Stop never calls this: the baseline there is read from git blobs and a
/// plugin cannot be attributed without them.
#[must_use]
pub fn run(
    cfg: &ResolvedConfig,
    opts: &Opts<'_>,
    repo_root: &Path,
    targets: &[(String, String)],
    cache: &DescribeCache,
) -> PluginRun {
    let mut result = PluginRun::default();
    let started = Instant::now();
    // The hook's ceiling is cumulative: every plugin and every call shares
    // it, so N plugins cannot cost N times 300 ms on the model's path.
    let hook_cutoff = (opts.stage == Stage::Hook)
        .then(|| started.checked_add(Duration::from_millis(HOOK_BUDGET_MS)))
        .flatten();
    let stop = [opts.deadline, hook_cutoff].into_iter().flatten().min();
    for (name, plugin) in &cfg.plugins {
        if !plugin.enable || !opts.selection.selects(name) {
            continue;
        }
        let Some(budget) = budget_for(plugin, opts.deadline, hook_cutoff) else {
            result.skip(name, "deadline");
            continue;
        };

        let manifest = match describe_and_check(cache, plugin, repo_root, budget, name, opts) {
            Ok(m) => m,
            Err(reason) => {
                result.skip(name, reason);
                continue;
            }
        };

        let mine = parse_targets(&manifest, targets);
        if mine.is_empty() {
            result.skip(name, "no-targets");
            result.settled.insert(name.clone());
            continue;
        }
        // What `describe` spent is gone: the check call gets what is left.
        let Some(budget) = budget_for(plugin, opts.deadline, hook_cutoff) else {
            result.skip(name, "deadline");
            continue;
        };
        let request = build_request(
            opts.stage,
            repo_root,
            &mine,
            plugin,
            u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
        );
        let Ok(payload) = serde_json::to_vec(&request) else {
            result.skip(name, "bad-request");
            continue;
        };
        let stdout = match runner::run_process(&plugin.into(), "check", &payload, repo_root, budget)
        {
            Ok(out) => out,
            Err(reason) => {
                result.skip(name, reason);
                continue;
            }
        };
        let Ok(mut response) = serde_json::from_slice::<Response>(&stdout) else {
            result.skip(name, "bad-response");
            continue;
        };
        if !agent_slop_protocol::is_supported(response.protocol) {
            result.skip(name, "protocol-mismatch");
            continue;
        }

        // Truncate BEFORE normalising: the cap is the bound on host work.
        let truncated = response.findings.len() > MAX_FINDINGS;
        response.findings.truncate(MAX_FINDINGS);
        if truncated {
            result.skipped.push(Skipped {
                tool: name.clone(),
                reason: format!("truncated at {MAX_FINDINGS} findings"),
            });
        }
        let (mut found, status) =
            normalize(cfg, name, &manifest, repo_root, &mine, &response, stop);
        if status.partial {
            result.skipped.push(Skipped {
                tool: name.clone(),
                reason: "deadline".to_string(),
            });
        }
        result.status.insert(name.clone(), status.text());
        if !truncated && !status.partial {
            result.settled.insert(name.clone());
        }
        if manifest.input == Input::Comments && manifest.deterministic {
            result.attributable.insert(name.clone());
        }
        result.findings.append(&mut found);
    }
    result
}

/// Describe the plugin and accept its manifest: protocol spoken, the name
/// matches the configured key, and the cost class may run at this stage.
/// `Err` is a skip reason.
fn describe_and_check(
    cache: &DescribeCache,
    plugin: &ResolvedPlugin,
    repo_root: &Path,
    budget: Duration,
    name: &str,
    opts: &Opts<'_>,
) -> Result<Manifest, String> {
    let manifest = cache.describe(plugin, repo_root, budget)?;
    if !agent_slop_protocol::is_supported(manifest.protocol) {
        return Err(format!(
            "protocol-mismatch (host speaks {}, plugin {})",
            agent_slop_protocol::PROTOCOL_VERSION,
            manifest.protocol
        ));
    }
    if manifest.name != name {
        return Err(format!("name-mismatch (manifest says `{}`)", manifest.name));
    }
    if !cost_allows(manifest.cost, opts.stage, opts.selection.names(name)) {
        return Err("cost".to_string());
    }
    Ok(manifest)
}

/// How a normalisation pass ended, for the status map.
struct NormStatus {
    dropped: usize,
    /// The deadline cut the pass short.
    partial: bool,
}

impl NormStatus {
    fn text(&self) -> String {
        if self.partial {
            "ok (partial: deadline reached while normalising)".to_string()
        } else if self.dropped == 0 {
            "ok".to_string()
        } else {
            format!(
                "ok ({} findings dropped: undeclared rule, foreign path or bad index)",
                self.dropped
            )
        }
    }
}

/// The time one call of `plugin` may take: `timeout_ms`, capped by what is
/// left of the caller's deadline less [`MIN_BUDGET_MS`] (plan §3.4) and by
/// what is left of the hook's cumulative ceiling. `None` = not worth
/// starting.
fn budget_for(
    plugin: &ResolvedPlugin,
    deadline: Option<Instant>,
    hook_cutoff: Option<Instant>,
) -> Option<Duration> {
    let left_ms = |at: Instant| {
        u64::try_from(at.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(0)
    };
    let mut ms = plugin.timeout_ms;
    if let Some(d) = deadline {
        ms = ms.min(left_ms(d).saturating_sub(MIN_BUDGET_MS));
    }
    if let Some(c) = hook_cutoff {
        ms = ms.min(left_ms(c));
    }
    (ms >= MIN_BUDGET_MS).then(|| Duration::from_millis(ms))
}

/// A target with its comments extracted once; request building and
/// normalisation both read it, so a file is parsed once per plugin call
/// however many findings come back.
struct Parsed<'a> {
    path: &'a str,
    lang: Option<&'static str>,
    comments: Vec<lang::Comment>,
    lines: Vec<&'a str>,
}

/// The files a manifest wants, parsed once: `langs` for comment plugins
/// (empty = every language the scanner knows), `exts` for file plugins.
fn parse_targets<'a>(manifest: &Manifest, targets: &'a [(String, String)]) -> Vec<Parsed<'a>> {
    targets
        .iter()
        .filter(|(path, _)| match manifest.input {
            Input::Comments => lang::spec_for_path(path).is_some_and(|s| {
                manifest.langs.is_empty() || manifest.langs.iter().any(|l| l == s.name)
            }),
            Input::Files => path
                .rsplit_once('.')
                .is_some_and(|(_, ext)| manifest.exts.iter().any(|e| e == ext)),
        })
        .map(|(path, content)| {
            let spec = lang::spec_for_path(path);
            let comments = match (manifest.input, spec) {
                (Input::Comments, Some(spec)) => lang::extract(spec, content),
                _ => Vec::new(),
            };
            Parsed {
                path,
                lang: spec.map(|s| s.name),
                comments,
                lines: content.lines().collect(),
            }
        })
        .collect()
}

fn build_request(
    stage: Stage,
    repo_root: &Path,
    targets: &[Parsed<'_>],
    plugin: &ResolvedPlugin,
    budget_ms: u64,
) -> Request {
    let files = targets
        .iter()
        .map(|t| RequestFile {
            path: t.path.to_string(),
            lang: t.lang.map_or_else(|| "unknown".to_string(), str::to_string),
            comments: t
                .comments
                .iter()
                .enumerate()
                .map(|(index, c)| WireComment {
                    index,
                    text: c.text.clone(),
                    line: c.line,
                    col: c.col,
                    end_line: c.end_line,
                    next_code_ids: c.next_code_ids.clone(),
                    is_docstring: c.is_docstring,
                })
                .collect(),
        })
        .collect();
    Request {
        protocol: agent_slop_protocol::PROTOCOL_VERSION,
        stage,
        repo_root: repo_root.display().to_string(),
        deadline_ms: budget_ms,
        settings: serde_json::to_value(&plugin.settings).unwrap_or(serde_json::Value::Null),
        files,
    }
}

/// Wire response to shared findings. For `input = "comments"` the
/// position and snippet come from OUR comment (by index), so the
/// fingerprint has the same shape as a slop finding's. A `rule` the
/// manifest does not declare, a `path` that was not in the request or a
/// comment index out of range is dropped and counted.
fn normalize(
    cfg: &ResolvedConfig,
    plugin: &str,
    manifest: &Manifest,
    repo_root: &Path,
    targets: &[Parsed<'_>],
    response: &Response,
    stop: Option<Instant>,
) -> (Vec<Finding>, NormStatus) {
    let by_path: BTreeMap<&str, &Parsed<'_>> = targets.iter().map(|t| (t.path, t)).collect();
    let mut out = Vec::new();
    let mut dropped = 0usize;
    let mut out_of_time = false;
    for wire in &response.findings {
        if stop.is_some_and(|stop| Instant::now() >= stop) {
            // The budget bought what it bought; the rest of the flood is
            // not normalised (CWE-400: the cap must bound the host work,
            // and this runs outside any subprocess budget).
            out_of_time = true;
            break;
        }
        let local = normalize_path(&wire.path, repo_root);
        let (Some(t), Some(decl)) = (
            by_path.get(local.as_str()).copied(),
            manifest.rules.iter().find(|r| r.id == wire.rule),
        ) else {
            dropped += 1;
            continue;
        };
        let rule_id = format!("{plugin}/{}", wire.rule);
        let default_level = match decl.default_level {
            agent_slop_protocol::Level::Error => RuleLevel::Error,
            agent_slop_protocol::Level::Warning => RuleLevel::Warning,
            agent_slop_protocol::Level::Hint => RuleLevel::Hint,
            agent_slop_protocol::Level::Off => RuleLevel::Off,
        };

        let located = if manifest.input == Input::Comments {
            wire.comment.and_then(|i| t.comments.get(i)).map(|comment| {
                (
                    comment.line,
                    comment.col,
                    comment.end_line,
                    comment.text.chars().take(512).collect::<String>(),
                )
            })
        } else {
            let snippet = if wire.snippet.is_empty() {
                &wire.message
            } else {
                &wire.snippet
            };
            Some((
                wire.line.unwrap_or(1),
                wire.col.unwrap_or(1),
                wire.end_line.or(wire.line).unwrap_or(1),
                snippet.chars().take(512).collect(),
            ))
        };
        let Some((line, col, end_line, snippet)) = located else {
            dropped += 1;
            continue;
        };

        let Some(severity) = imported_severity(cfg, &rule_id, default_level) else {
            continue;
        };
        let mut finding = Finding {
            fingerprint: fingerprint(&rule_id, &snippet),
            rule: rule_id,
            severity,
            upstream_severity: wire.upstream_severity.clone(),
            file: local,
            line,
            col,
            end_line,
            message: wire.message.clone(),
            snippet,
            source: Source::Plugin {
                tool: plugin.to_string(),
            },
            lang: None,
            suppression: None,
            fixable: wire.fixable,
        };
        // Plugins have no native suppression, so ours applies to them.
        agent_slop_core::rules::apply_suppression(&mut finding, &t.lines);
        out.push(finding);
    }
    (
        out,
        NormStatus {
            dropped,
            partial: out_of_time,
        },
    )
}

/// The shared severity path for imported findings: an explicit
/// `overrides` entry first, then the plugin's configured severity if a
/// tier set one, else the rule's own `default_level` from the manifest,
/// capped at `warning`. `off` reports nothing.
#[must_use]
pub fn imported_severity(
    cfg: &ResolvedConfig,
    rule_id: &str,
    declared: RuleLevel,
) -> Option<Severity> {
    let overrides = &cfg.overrides;
    if let Some(level) = overrides.get(rule_id) {
        return level.severity();
    }
    let producer = rule_id.split('/').next().unwrap_or_default();
    let level = cfg
        .plugins
        .get(producer)
        .and_then(|p| p.severity)
        .unwrap_or(declared);
    let base = level.severity()?;
    Some(if base == Severity::Error {
        Severity::Warning
    } else {
        base
    })
}
