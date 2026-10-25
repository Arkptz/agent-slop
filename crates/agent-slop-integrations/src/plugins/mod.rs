//! The plugin host: declare `[slop.plugins.<name>]`, run the process, fold
//! its findings into the shared model (owner decision 2026-10-25).
//!
//! One process per call, no daemon. Everything here is fail-open: a plugin
//! that is absent, slow, loud or broken is a *skip* keyed by its name and
//! never changes a command's exit code.

pub mod runner;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
/// the command line and the resolved executable's size and mtime, so a
/// plugin upgrade or a changed argument is a new key.
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

    fn key(command: &[String]) -> Option<String> {
        let exe = resolve_exe(command.first()?)?;
        let meta = std::fs::metadata(&exe).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        let mut hasher = blake3::Hasher::new();
        for part in command {
            hasher.update(part.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(&meta.len().to_le_bytes());
        hasher.update(&mtime.to_le_bytes());
        Some(hasher.finalize().to_hex()[..32].to_string())
    }

    fn describe(
        &self,
        plugin: &ResolvedPlugin,
        cwd: &Path,
        budget: Duration,
    ) -> Result<Manifest, String> {
        let key = Self::key(&plugin.command);
        let path = self
            .dir
            .as_ref()
            .zip(key.as_ref())
            .map(|(d, k)| d.join(format!("{k}.json")));
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
#[derive(Debug, Clone)]
pub struct Opts<'a> {
    pub selection: &'a Selection,
    pub stage: Stage,
    /// Milliseconds left on the caller's deadline, if it has one.
    pub left_ms: Option<u64>,
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
    for (name, plugin) in &cfg.plugins {
        if !plugin.enable || !opts.selection.selects(name) {
            continue;
        }
        let mut budget_ms = opts.left_ms.map_or(plugin.timeout_ms, |left| {
            plugin.timeout_ms.min(left.saturating_sub(MIN_BUDGET_MS))
        });
        if opts.stage == Stage::Hook {
            budget_ms = budget_ms.min(HOOK_BUDGET_MS);
        }
        if budget_ms < MIN_BUDGET_MS {
            result.skip(name, "deadline");
            continue;
        }
        let budget = Duration::from_millis(budget_ms);

        let manifest = match cache.describe(plugin, repo_root, budget) {
            Ok(m) => m,
            Err(reason) => {
                result.skip(name, reason);
                continue;
            }
        };
        if !agent_slop_protocol::is_supported(manifest.protocol) {
            result.skip(
                name,
                format!(
                    "protocol-mismatch (host speaks {}, plugin {})",
                    agent_slop_protocol::PROTOCOL_VERSION,
                    manifest.protocol
                ),
            );
            continue;
        }
        if manifest.name != *name {
            result.skip(
                name,
                format!("name-mismatch (manifest says `{}`)", manifest.name),
            );
            continue;
        }
        if !cost_allows(manifest.cost, opts.stage, opts.selection.names(name)) {
            result.skip(name, "cost");
            continue;
        }

        let mine = targets_for(&manifest, targets);
        if mine.is_empty() {
            result.skip(name, "no-targets");
            continue;
        }
        let request = build_request(&manifest, opts.stage, repo_root, &mine, plugin, budget_ms);
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
        let Ok(response) = serde_json::from_slice::<Response>(&stdout) else {
            result.skip(name, "bad-response");
            continue;
        };
        if !agent_slop_protocol::is_supported(response.protocol) {
            result.skip(name, "protocol-mismatch");
            continue;
        }

        let (mut found, dropped) = normalize(cfg, name, &manifest, repo_root, &mine, &response);
        if found.len() > MAX_FINDINGS {
            found.truncate(MAX_FINDINGS);
            result.skipped.push(Skipped {
                tool: name.clone(),
                reason: format!("truncated at {MAX_FINDINGS} findings"),
            });
        }
        let status = if dropped == 0 {
            "ok".to_string()
        } else {
            format!("ok ({dropped} findings dropped: undeclared rule, foreign path or bad index)")
        };
        result.status.insert(name.clone(), status);
        if manifest.input == Input::Comments && manifest.deterministic {
            result.attributable.insert(name.clone());
        }
        result.findings.append(&mut found);
    }
    result
}

/// The files a manifest wants: `langs` for comment plugins (empty = every
/// language the scanner knows), `exts` for file plugins.
fn targets_for<'a>(
    manifest: &Manifest,
    targets: &'a [(String, String)],
) -> Vec<&'a (String, String)> {
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
        .collect()
}

fn build_request(
    manifest: &Manifest,
    stage: Stage,
    repo_root: &Path,
    targets: &[&(String, String)],
    plugin: &ResolvedPlugin,
    budget_ms: u64,
) -> Request {
    let files = targets
        .iter()
        .map(|(path, content)| {
            let spec = lang::spec_for_path(path);
            let comments = match (manifest.input, spec) {
                (Input::Comments, Some(spec)) => lang::extract(spec, content)
                    .into_iter()
                    .enumerate()
                    .map(|(index, c)| WireComment {
                        index,
                        text: c.text,
                        line: c.line,
                        col: c.col,
                        end_line: c.end_line,
                        next_code_ids: c.next_code_ids,
                        is_docstring: c.is_docstring,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            RequestFile {
                path: path.clone(),
                lang: spec.map_or_else(|| "unknown".to_string(), |s| s.name.to_string()),
                comments,
            }
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
    targets: &[&(String, String)],
    response: &Response,
) -> (Vec<Finding>, usize) {
    let by_path: BTreeMap<&str, &str> = targets
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for wire in &response.findings {
        let local = normalize_path(&wire.path, repo_root);
        let (Some(content), Some(decl)) = (
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
            lang::spec_for_path(&local).and_then(|spec| {
                let comment = lang::extract(spec, content)
                    .into_iter()
                    .nth(wire.comment?)?;
                Some((
                    comment.line,
                    comment.col,
                    comment.end_line,
                    comment.text.chars().take(512).collect::<String>(),
                ))
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
        let lines: Vec<&str> = content.lines().collect();
        agent_slop_core::rules::apply_suppression(&mut finding, &lines);
        out.push(finding);
    }
    (out, dropped)
}

/// The shared severity path for imported findings: an explicit
/// `overrides` entry first, then the plugin's configured severity, capped
/// at `warning`. `off` reports nothing. A plugin's own `default_level`
/// only applies when the plugin has no configured severity entry.
#[must_use]
pub fn imported_severity(
    cfg: &ResolvedConfig,
    rule_id: &str,
    declared: RuleLevel,
) -> Option<Severity> {
    if let Some(level) = cfg.overrides.get(rule_id) {
        return level.severity();
    }
    let producer = rule_id.split('/').next().unwrap_or_default();
    let level = cfg.plugins.get(producer).map_or(declared, |p| p.severity);
    let base = level.severity()?;
    Some(if base == Severity::Error {
        Severity::Warning
    } else {
        base
    })
}
