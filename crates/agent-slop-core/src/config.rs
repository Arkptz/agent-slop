//! `[slop]` config model and three-tier resolution (SPEC-006
//! "Configuration").
//!
//! Tiers, lowest to highest: built-in defaults, the user file
//! `~/.config/agent-slop/config.toml` (generated from Nix), and the repo's
//! `.agent-hooks.toml` at the git toplevel. Resolution deep-merges per key:
//! a key present in a higher tier replaces the lower tier's value, an
//! absent key keeps it. Lists (`exclude.paths`, `select`, `ignore`) are
//! REPLACED whole, never concatenated — a repo must be able to cancel a
//! user-level default — and `exclude.paths_extend` is the one additive
//! list. Exactly one config per git toplevel; there is no per-directory
//! nesting.
//!
//! Resolution never fails: the binary's core contract is that it does not
//! exit nonzero on an internal error, so a malformed, mistyped or
//! unreadable tier is dropped with the reason recorded in
//! [`ResolvedConfig::notes`] for the caller to print, and the lower tiers
//! stand. A missing file — or a missing `$HOME`, which is normal for hooks
//! run under systemd — is not a degradation at all and records nothing;
//! `home` resolves the directory without panicking.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

use crate::finding::Severity;
use crate::rules::corpus::{EMBEDDED, RuleSet};
use crate::rules::custom::{self, LoadedRule};
use crate::rules::tune::{self, Tune};

/// Every rule family v1 knows. A name outside this set in `[slop.rules]`
/// is reported in `notes` (it looks like intent) but kept — dropping it
/// would discard a value a newer binary may understand.
pub const KNOWN_FAMILIES: [&str; 16] = [
    "comment-history",
    "comment-meta",
    "review-jargon",
    "comment-paraphrase",
    "todo-no-condition",
    "perf-claim",
    "unsourced-constraint",
    "comment-language",
    "rejected-alternative",
    "manual-ddl",
    "bare-reference",
    "comment-hedging",
    "comment-steps",
    "docstring-restates-name",
    "self-justification",
    "config-deploy-narration",
];

/// `observe` never exits 2; `block` makes Stop exit 2 on added errors.
/// The exit contract is frozen in SPEC-006 "Configuration".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Observe,
    Block,
}

/// Config-level severity: the finding-level [`Severity`] plus `off`, which
/// leaves the rule disabled (how `slop/comment-language` ships).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleLevel {
    Error,
    Warning,
    Hint,
    Off,
}

impl RuleLevel {
    /// The spelling used in config files and CLI output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RuleLevel::Error => "error",
            RuleLevel::Warning => "warning",
            RuleLevel::Hint => "hint",
            RuleLevel::Off => "off",
        }
    }

    /// `off` is no severity at all: the rule does not fire.
    #[must_use]
    pub fn severity(self) -> Option<Severity> {
        match self {
            RuleLevel::Error => Some(Severity::Error),
            RuleLevel::Warning => Some(Severity::Warning),
            RuleLevel::Hint => Some(Severity::Hint),
            RuleLevel::Off => None,
        }
    }
}

/// One `[[slop.exclude.scoped]]` entry: a path/rule pair exempting those
/// paths from those rules alone — narrower than a global exclusion. The
/// shape is frozen in SPEC-006 "Configuration".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ScopedExclude {
    pub paths: Vec<String>,
    pub rules: Vec<String>,
}

/// `[slop.linters.<name>]` after merge. Declaring a linter enables it;
/// `severity` defaults to `warning` because imported findings never map to
/// `error` without an explicit `overrides` promotion (SPEC-006 "Rule ids
/// and severity"). `select`/`ignore` stay `None` when no tier set them —
/// `None` means "the linter's own config decides", which is the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLinter {
    pub enable: bool,
    pub select: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,
    pub severity: RuleLevel,
    pub overrides: BTreeMap<String, RuleLevel>,
}

impl Default for ResolvedLinter {
    fn default() -> Self {
        Self {
            enable: true,
            select: None,
            ignore: None,
            severity: RuleLevel::Warning,
            overrides: BTreeMap::new(),
        }
    }
}

/// `[slop.plugins.<name>]` after merge. Only the user tier may supply a
/// `command` or an `env_passthrough`: a cloned repository must not be able
/// to make the binary execute anything, the same principle as
/// `evaluates_repo_code` for linters. The repo tier may lower the plugin
/// (`enable = false`, a lower `severity`) and pass `settings`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPlugin {
    /// argv; empty until the user tier declares it, and a plugin with no
    /// command is not a plugin.
    pub command: Vec<String>,
    pub enable: bool,
    /// Wall-clock budget of one call.
    pub timeout_ms: u64,
    /// Cap for what the plugin may report; an `error` here is still capped
    /// at `warning` unless an `overrides` entry names the rule.
    pub severity: RuleLevel,
    /// Opaque table handed to the plugin in the request.
    pub settings: toml::Value,
    /// Environment variables forwarded on top of the fixed allowlist.
    pub env_passthrough: Vec<String>,
}

impl Default for ResolvedPlugin {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            enable: true,
            timeout_ms: 2000,
            severity: RuleLevel::Warning,
            settings: toml::Value::Table(toml::map::Map::new()),
            env_passthrough: Vec::new(),
        }
    }
}

/// `[slop.exclude]` after merge: `paths` is the highest tier's whole list,
/// `paths_extend` is every tier's extension concatenated lowest-first, and
/// `scoped` is the highest tier's whole list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedExclude {
    pub paths: Vec<String>,
    pub paths_extend: Vec<String>,
    pub scoped: Vec<ScopedExclude>,
}

/// Which file a note came from. The built-in tier cannot degrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    User,
    Repo,
}

/// A recorded degradation or oddity, exposed so a caller can print it.
/// Notes are information, never errors: resolution always completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigNote {
    pub tier: Tier,
    pub reason: String,
}

/// `[slop.judge]` after merge — the Laya second verdict (see
/// `judge.rs`). Every field defaults to a value that annotates nothing:
/// the judge is off until a tier switches it on, because the endpoint is a
/// model this machine may or may not be running.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeSettings {
    pub enabled: bool,
    /// laya.cpp's Jev-compatible endpoint. The gateway port, not the model
    /// port: the gateway stays up when the model is stopped and answers 503,
    /// which is the failure the judge is built to absorb.
    pub url: String,
    /// Wall-clock budget for the whole leg, not per request. The Stop gate
    /// has 15 s and must work with the model off.
    pub timeout_ms: u64,
    /// Requests per leg. A file with 200 comments must not become 200 round
    /// trips inside one tool call.
    pub max_verdicts: usize,
    /// A 3-way verdict prints only above this probability.
    pub min_confidence: f64,
    /// A `needs_comment` answer prints only above this probability, and the
    /// floor is separate because the two questions are calibrated apart.
    pub needs_min_p: f64,
}

impl Default for JudgeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://127.0.0.1:8095/v1/systemone".to_string(),
            timeout_ms: 2500,
            max_verdicts: 4,
            // The checkpoint's own three-way choice scores 0.628 against a
            // 0.664 always-keep baseline, and it inherits a length confound
            // (kept comments: median 44 words, deleted: 17) that makes it
            // confidently wrong on short, load-bearing comments. A high floor
            // is the only honest default for an annotator that weak.
            min_confidence: 0.9,
            needs_min_p: 0.9,
        }
    }
}

/// The fully merged `[slop]` config. Field-by-field contract in SPEC-006
/// "Configuration". `Eq` is gone with the judge's float floors: two configs
/// that differ by 1e-18 in a threshold are the same config for every
/// decision this binary makes.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedConfig {
    /// `[slop] ref` — a Rust keyword, hence the trailing underscore.
    /// "auto" resolves to the merge-base of HEAD and the first of
    /// origin/HEAD, main, master.
    pub ref_: String,
    pub mode: Mode,
    /// `None` (unset) and `Some("")` both leave `slop/comment-language`
    /// off; see [`ResolvedConfig::comment_language`].
    pub language: Option<String>,
    pub timeout_ms: u64,
    /// Bare family names (`comment-history`) to levels. Starts from the
    /// frozen built-in set; tiers override per family key.
    pub rules: BTreeMap<String, RuleLevel>,
    /// Full rule ids (`slop/todo-no-condition`, `ruff/ERA001`) to levels —
    /// the promote/demote list, including imported-id promotions.
    pub overrides: BTreeMap<String, RuleLevel>,
    pub exclude: ResolvedExclude,
    pub linters: BTreeMap<String, ResolvedLinter>,
    /// Declared external plugins by name; only entries with a user-tier
    /// `command` survive resolution.
    pub plugins: BTreeMap<String, ResolvedPlugin>,
    pub judge: JudgeSettings,
    /// `[slop.custom] enable`: whether rule files from the user and repo
    /// rule directories are loaded.
    pub custom_enabled: bool,
    /// `[slop.tune.<family>]` accumulated lowest-tier first.
    pub tune: BTreeMap<String, Tune>,
    /// The rules a scan runs against. Compared by identity hash.
    pub ruleset: Arc<RuleSet>,
    pub notes: Vec<ConfigNote>,
}

impl ResolvedConfig {
    /// The effective comment language, or `None` when unset or empty —
    /// `""` leaves `slop/comment-language` off. Which language codes are
    /// accepted is listed in SPEC-006 "Configuration".
    // Unconsumed until the slop/comment-language matcher ships; that rule
    // is the only reader.
    #[allow(dead_code)]
    #[must_use]
    pub fn comment_language(&self) -> Option<&str> {
        match self.language.as_deref() {
            Some("") | None => None,
            Some(lang) => Some(lang),
        }
    }

    /// Level of every rule id the config knows: `slop/*` families, custom
    /// ids and full-id overrides, in id order. Together with the ruleset
    /// hash this is everything that decides which findings a scan can
    /// produce.
    #[must_use]
    pub fn effective_levels(&self) -> BTreeMap<String, RuleLevel> {
        let mut levels: BTreeMap<String, RuleLevel> = self
            .rules
            .iter()
            .map(|(family, level)| (family_id(family), *level))
            .collect();
        levels.extend(
            self.overrides
                .iter()
                .map(|(id, level)| (id.clone(), *level)),
        );
        levels
    }

    /// The configured level of a full rule id, keeping `off` distinct from
    /// an id the config has no entry for: `overrides` first, then the
    /// family table (`slop/<family>` or a custom `<producer>/<id>`).
    #[must_use]
    pub fn level_of(&self, rule_id: &str) -> Option<RuleLevel> {
        if let Some(level) = self.overrides.get(rule_id) {
            return Some(*level);
        }
        let key = rule_id.strip_prefix("slop/").unwrap_or(rule_id);
        if key.len() == rule_id.len() && !self.is_custom(rule_id) {
            return None;
        }
        self.rules.get(key).copied()
    }

    /// Whether `rule_id` names a loaded custom rule.
    #[must_use]
    pub fn is_custom(&self, rule_id: &str) -> bool {
        self.ruleset.custom.iter().any(|c| c.id == rule_id)
    }

    /// Severity for a full rule id: `overrides` first, then the family
    /// table for `slop/*` ids and loaded custom ids. `None` means the rule
    /// is off or the id is foreign and unoverridden — for the latter the
    /// caller applies the linter's own default, never `error`.
    #[must_use]
    pub fn severity_for(&self, rule_id: &str) -> Option<Severity> {
        self.level_of(rule_id).and_then(RuleLevel::severity)
    }
}

/// The full id of a family-table key: bare families are `slop/*`, a key
/// already carrying a producer is a custom id.
fn family_id(key: &str) -> String {
    if key.contains('/') {
        key.to_string()
    } else {
        format!("slop/{key}")
    }
}

/// The frozen built-in tier (SPEC-006 "The v1 blocking set", owner
/// decision 2026-09-21; amended 2026-10-03 by the TASK-019 harvest).
/// `comment-history` ships at `error`; comment-meta and review-jargon
/// were demoted to `warning` with the observe → block flip after 12
/// days of telemetry showed no earned block from either. Changing the
/// set is a spec amendment, not a config tweak.
#[must_use]
pub fn builtin_config() -> ResolvedConfig {
    let rules: BTreeMap<String, RuleLevel> = [
        ("comment-history", RuleLevel::Error),
        ("comment-meta", RuleLevel::Warning),
        ("review-jargon", RuleLevel::Warning),
        ("comment-paraphrase", RuleLevel::Warning),
        ("todo-no-condition", RuleLevel::Warning),
        ("perf-claim", RuleLevel::Warning),
        ("unsourced-constraint", RuleLevel::Warning),
        ("comment-language", RuleLevel::Off),
        ("rejected-alternative", RuleLevel::Off),
        ("manual-ddl", RuleLevel::Warning),
        ("bare-reference", RuleLevel::Warning),
        ("comment-hedging", RuleLevel::Warning),
        ("comment-steps", RuleLevel::Warning),
        ("docstring-restates-name", RuleLevel::Warning),
        ("self-justification", RuleLevel::Warning),
        ("config-deploy-narration", RuleLevel::Warning),
    ]
    .into_iter()
    .map(|(family, level)| (family.to_string(), level))
    .collect();

    ResolvedConfig {
        ref_: "auto".to_string(),
        mode: Mode::Observe,
        language: None,
        timeout_ms: 4000,
        rules,
        overrides: BTreeMap::new(),
        exclude: ResolvedExclude::default(),
        linters: BTreeMap::new(),
        plugins: BTreeMap::new(),
        judge: JudgeSettings::default(),
        custom_enabled: true,
        tune: BTreeMap::new(),
        ruleset: RuleSet::embedded(),
        notes: Vec::new(),
    }
}

/// Merge two config tiers from their TOML text. `None` means the tier is
/// absent. Malformed or mistyped TOML drops that tier with a recorded
/// reason; it never fails.
#[must_use]
pub fn resolve(user: Option<&str>, repo: Option<&str>) -> ResolvedConfig {
    resolve_with(user, repo, &[], Vec::new())
}

/// [`resolve`] plus already-loaded custom rules from the user and repo
/// rule directories. `[slop.custom] enable = false` in any tier drops the
/// whole layer; a repo rule replaces a user rule of the same id whole.
#[must_use]
pub fn resolve_with(
    user: Option<&str>,
    repo: Option<&str>,
    user_rules: &[LoadedRule],
    repo_rules: Vec<LoadedRule>,
) -> ResolvedConfig {
    let mut cfg = resolve_tiers(user, repo);
    let custom = if cfg.custom_enabled {
        custom::compile(user_rules, repo_rules, &mut cfg)
    } else {
        Vec::new()
    };
    let mut prune_notes = Vec::new();
    prune_plugins(&mut cfg, &custom, &mut prune_notes);
    cfg.notes.extend(prune_notes);
    finish_ruleset(&mut cfg, custom);
    cfg
}

/// Build the ruleset the scan runs: the embedded corpus with the
/// accumulated tunes applied, plus the surviving custom rules. The shared
/// embedded set stands when neither exists.
fn finish_ruleset(cfg: &mut ResolvedConfig, custom: Vec<LoadedRule>) {
    if custom.is_empty() && cfg.tune.values().all(Tune::is_empty) {
        return;
    }
    cfg.ruleset = Arc::new(RuleSet::build(&EMBEDDED, custom, &cfg.tune));
}

fn resolve_tiers(user: Option<&str>, repo: Option<&str>) -> ResolvedConfig {
    let mut cfg = builtin_config();
    let mut notes = Vec::new();

    for (tier, content) in [(Tier::User, user), (Tier::Repo, repo)] {
        let Some(content) = content else { continue };
        let parsed = match toml::from_str::<AgentHooksFile>(content) {
            Ok(file) => file,
            Err(err) => {
                notes.push(ConfigNote {
                    tier,
                    reason: format!("config dropped, malformed TOML or wrong types: {err}"),
                });
                continue;
            }
        };
        if let Some(slop) = parsed.slop {
            if let Some(rules) = &slop.rules {
                for family in rules.families.keys() {
                    if !family.contains('/') && !KNOWN_FAMILIES.contains(&family.as_str()) {
                        notes.push(ConfigNote {
                            tier,
                            reason: format!(
                                "unknown rule name in [slop.rules]: `{family}` \
                                 (kept, but no such rule exists in v1)"
                            ),
                        });
                    }
                }
            }
            apply_section(&mut cfg, slop, tier, &mut notes);
        }
    }

    cfg.notes = notes;
    cfg
}

/// The custom-rule directories that belong to the two config files:
/// `rules/` beside the user file, `.agent-slop/rules/` beside the repo
/// file (the git toplevel).
#[must_use]
pub fn rule_dirs_for(
    user: Option<&Path>,
    repo: Option<&Path>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    (
        user.and_then(|p| p.parent().map(|d| d.join("rules"))),
        repo.and_then(|p| p.parent().map(|d| d.join(".agent-slop").join("rules"))),
    )
}

/// Read both tiers from disk. A `NotFound` file is the common case and
/// records nothing; any other read failure drops that tier with a reason.
/// Custom rules are read from the directories [`rule_dirs_for`] names.
#[must_use]
pub fn load_from_paths(user: Option<&Path>, repo: Option<&Path>) -> ResolvedConfig {
    let (user_dir, repo_dir) = rule_dirs_for(user, repo);
    load_with_rule_dirs(user, repo, user_dir.as_deref(), repo_dir.as_deref())
}

/// [`load_from_paths`] with the rule directories named explicitly.
#[must_use]
pub fn load_with_rule_dirs(
    user: Option<&Path>,
    repo: Option<&Path>,
    user_rules_dir: Option<&Path>,
    repo_rules_dir: Option<&Path>,
) -> ResolvedConfig {
    let (user_text, mut notes) = read_tier(user, Tier::User);
    let (repo_text, repo_notes) = read_tier(repo, Tier::Repo);
    notes.extend(repo_notes);

    let mut cfg = resolve_tiers(user_text.as_deref(), repo_text.as_deref());
    let custom = if cfg.custom_enabled {
        let mut discovery = Vec::new();
        let user_rules = custom::discover(user_rules_dir, Tier::User, &mut discovery);
        let repo_rules = custom::discover(repo_rules_dir, Tier::Repo, &mut discovery);
        cfg.notes.extend(discovery);
        custom::compile(&user_rules, repo_rules, &mut cfg)
    } else {
        Vec::new()
    };
    let mut prune_notes = Vec::new();
    prune_plugins(&mut cfg, &custom, &mut prune_notes);
    cfg.notes.extend(prune_notes);
    finish_ruleset(&mut cfg, custom);
    notes.append(&mut cfg.notes);
    cfg.notes = notes;
    cfg
}

/// Full discovery: user path from `home` (no `$HOME` is normal, not an
/// error), repo path at the git toplevel of the current directory. Both
/// may come back absent; the built-ins always stand.
#[must_use]
pub fn load() -> ResolvedConfig {
    let (user, repo) = tier_paths();
    load_from_paths(user.as_deref(), repo.as_deref())
}

/// The two file paths `load` reads, user tier first. A caller that must
/// attribute an effective value to the tier that set it resolves the
/// tiers one at a time against these (`self-test`).
#[must_use]
pub fn tier_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    (
        user_config_path(),
        git_toplevel().map(|top| top.join(".agent-hooks.toml")),
    )
}

/// `~/.config/agent-slop/config.toml`, or `None` when `$HOME` cannot be
/// resolved (hooks run under systemd).
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    home::home_dir().map(|h| h.join(".config").join("agent-slop").join("config.toml"))
}

/// `git rev-parse --show-toplevel` of the current directory — the ONLY
/// place a repo config is read (no per-directory nesting). `None` when git
/// is absent or the directory is not a repository; both are normal.
#[must_use]
pub fn git_toplevel() -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top = String::from_utf8(output.stdout).ok()?;
    let top = top.trim();
    if top.is_empty() {
        None
    } else {
        Some(PathBuf::from(top))
    }
}

/// `[slop]` from one tier, every field optional: `None` means the tier did
/// not speak, so the lower tier's value stands. Unknown keys are ignored
/// for forward compatibility.
#[derive(Deserialize, Default, Clone, Debug)]
struct SlopSection {
    #[serde(rename = "ref")]
    ref_: Option<String>,
    mode: Option<Mode>,
    language: Option<String>,
    timeout_ms: Option<u64>,
    rules: Option<RawRules>,
    exclude: Option<RawExclude>,
    linters: Option<BTreeMap<String, RawLinter>>,
    judge: Option<RawJudge>,
    custom: Option<RawCustom>,
    tune: Option<BTreeMap<String, toml::Value>>,
    plugins: Option<BTreeMap<String, RawPlugin>>,
}

/// `[slop.plugins.<name>]`, every key optional per tier.
#[derive(Deserialize, Default, Clone, Debug)]
struct RawPlugin {
    command: Option<Vec<String>>,
    enable: Option<bool>,
    timeout_ms: Option<u64>,
    severity: Option<RuleLevel>,
    settings: Option<toml::Value>,
    env_passthrough: Option<Vec<String>>,
}

/// `[slop.custom]`: the one switch of the custom-rule layer.
#[derive(Deserialize, Default, Clone, Debug)]
struct RawCustom {
    enable: Option<bool>,
}

/// `[slop.judge]` — every field optional, so a tier that names one key keeps
/// the built-in value of the rest. An unknown key is ignored like any other,
/// which is what lets a newer binary read an older file.
#[derive(Deserialize, Default, Clone, Debug)]
struct RawJudge {
    enable: Option<bool>,
    url: Option<String>,
    timeout_ms: Option<u64>,
    max_verdicts: Option<usize>,
    min_confidence: Option<f64>,
    needs_min_p: Option<f64>,
}

/// `[slop.rules]` — bare `family = level` pairs plus one optional
/// `overrides` subtable of full rule ids. The two value shapes share a
/// table, so the split needs a hand-written visitor: any value that is
/// neither a level nor (under `overrides`) a level table is a parse error,
/// which drops the tier with the reason recorded.
#[derive(Default, Clone, Debug)]
struct RawRules {
    families: BTreeMap<String, RuleLevel>,
    overrides: Option<BTreeMap<String, RuleLevel>>,
}

struct RawRulesVisitor;

impl<'de> Visitor<'de> for RawRulesVisitor {
    type Value = RawRules;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("`family = level` pairs plus an optional `overrides` table")
    }

    fn visit_map<A>(self, mut map: A) -> Result<RawRules, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut rules = RawRules::default();
        while let Some(key) = map.next_key::<String>()? {
            if key == "overrides" {
                rules.overrides = Some(map.next_value()?);
            } else {
                let level = map.next_value::<RuleLevel>()?;
                rules.families.insert(key, level);
            }
        }
        Ok(rules)
    }
}

impl<'de> Deserialize<'de> for RawRules {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawRulesVisitor)
    }
}

#[derive(Deserialize, Default, Clone, Debug)]
struct RawExclude {
    paths: Option<Vec<String>>,
    paths_extend: Option<Vec<String>>,
    scoped: Option<Vec<ScopedExclude>>,
}

#[derive(Deserialize, Default, Clone, Debug)]
struct RawLinter {
    enable: Option<bool>,
    select: Option<Vec<String>>,
    ignore: Option<Vec<String>>,
    severity: Option<RuleLevel>,
    overrides: Option<BTreeMap<String, RuleLevel>>,
}

/// Both tiers share the `.agent-hooks.toml` envelope: `[slop]` is one
/// section among several; the others belong to different tools and are
/// ignored here.
#[derive(Deserialize)]
struct AgentHooksFile {
    slop: Option<SlopSection>,
}

/// Fold one tier into the running merge. Every branch is "set only when
/// the tier spoke", which is the whole per-key contract; lists replace,
/// `paths_extend` appends.
fn apply_section(
    cfg: &mut ResolvedConfig,
    section: SlopSection,
    tier: Tier,
    notes: &mut Vec<ConfigNote>,
) {
    if let Some(ref_) = section.ref_ {
        cfg.ref_ = ref_;
    }
    if let Some(mode) = section.mode {
        cfg.mode = mode;
    }
    if let Some(language) = section.language {
        cfg.language = Some(language);
    }
    if let Some(timeout_ms) = section.timeout_ms {
        cfg.timeout_ms = timeout_ms;
    }
    if let Some(rules) = section.rules {
        for (family, level) in rules.families {
            cfg.rules.insert(family, level);
        }
        for (rule_id, level) in rules.overrides.unwrap_or_default() {
            // Q1 lets the repo tier promote custom rules after strict lint;
            // plugins are user-tier only (Q2), so on a plugin's rules the
            // repo tier may only stay at or below what the user tier lets
            // the plugin report.
            let producer = rule_id.split('/').next().unwrap_or_default();
            if tier == Tier::Repo
                && let Some(plugin) = cfg.plugins.get(producer)
                && level_rank(level)
                    > level_rank(plugin.severity).min(level_rank(RuleLevel::Warning))
            {
                notes.push(ConfigNote {
                    tier,
                    reason: format!(
                        "[slop.rules.overrides] {rule_id} ignored: the repo tier may only lower \
                         a plugin rule"
                    ),
                });
                continue;
            }
            cfg.overrides.insert(rule_id, level);
        }
    }
    if let Some(exclude) = section.exclude {
        if let Some(paths) = exclude.paths {
            cfg.exclude.paths = paths;
        }
        if let Some(extend) = exclude.paths_extend {
            cfg.exclude.paths_extend.extend(extend);
        }
        if let Some(scoped) = exclude.scoped {
            cfg.exclude.scoped = scoped;
        }
    }
    if let Some(enable) = section.custom.and_then(|c| c.enable) {
        cfg.custom_enabled = enable;
    }
    if let Some(raw_tune) = section.tune {
        tune::merge_tier(&mut cfg.tune, raw_tune, tier, notes);
    }
    if let Some(plugins) = section.plugins {
        for (name, raw) in plugins {
            apply_plugin(cfg, &name, raw, tier, notes);
        }
    }
    if let Some(judge) = section.judge {
        if let Some(enable) = judge.enable {
            cfg.judge.enabled = enable;
        }
        if let Some(url) = judge.url {
            cfg.judge.url = url;
        }
        if let Some(timeout_ms) = judge.timeout_ms {
            cfg.judge.timeout_ms = timeout_ms;
        }
        if let Some(max_verdicts) = judge.max_verdicts {
            cfg.judge.max_verdicts = max_verdicts;
        }
        if let Some(min_confidence) = judge.min_confidence {
            cfg.judge.min_confidence = min_confidence;
        }
        if let Some(needs_min_p) = judge.needs_min_p {
            cfg.judge.needs_min_p = needs_min_p;
        }
    }
    if let Some(linters) = section.linters {
        for (name, linter) in linters {
            let entry = cfg.linters.entry(name).or_default();
            if let Some(enable) = linter.enable {
                entry.enable = enable;
            }
            if let Some(select) = linter.select {
                entry.select = Some(select);
            }
            if let Some(ignore) = linter.ignore {
                entry.ignore = Some(ignore);
            }
            if let Some(severity) = linter.severity {
                entry.severity = severity;
            }
            for (rule_id, level) in linter.overrides.unwrap_or_default() {
                entry.overrides.insert(rule_id, level);
            }
        }
    }
}

/// Rank used to let the repo tier only lower a plugin's severity.
fn level_rank(level: RuleLevel) -> u8 {
    match level {
        RuleLevel::Error => 3,
        RuleLevel::Warning => 2,
        RuleLevel::Hint => 1,
        RuleLevel::Off => 0,
    }
}

/// Fold one tier's `[slop.plugins.<name>]` entry. The repo tier is a
/// restricted voice: no `command`, no `env_passthrough`, no `timeout_ms`,
/// no `enable = true`, and a `severity` only when it is lower.
fn apply_plugin(
    cfg: &mut ResolvedConfig,
    name: &str,
    raw: RawPlugin,
    tier: Tier,
    notes: &mut Vec<ConfigNote>,
) {
    let mut note = |reason: String| notes.push(ConfigNote { tier, reason });
    if !crate::rules::custom::valid_producer(name) || name.len() < 2 {
        note(format!(
            "[slop.plugins.{name}] ignored: the name must match [a-z][a-z0-9-]{{1,31}}"
        ));
        return;
    }
    if crate::rules::custom::RESERVED_PRODUCERS.contains(&name) {
        note(format!(
            "[slop.plugins.{name}] ignored: `{name}` is a reserved producer"
        ));
        return;
    }
    let entry = cfg.plugins.entry(name.to_string()).or_default();
    match tier {
        Tier::User => {
            if let Some(command) = raw.command {
                entry.command = command;
            }
            if let Some(env) = raw.env_passthrough {
                entry.env_passthrough = env;
            }
            if let Some(enable) = raw.enable {
                entry.enable = enable;
            }
            if let Some(timeout_ms) = raw.timeout_ms {
                entry.timeout_ms = timeout_ms;
            }
            if let Some(severity) = raw.severity {
                entry.severity = severity;
            }
        }
        Tier::Repo => {
            for (present, key) in [
                (raw.command.is_some(), "command"),
                (raw.env_passthrough.is_some(), "env_passthrough"),
                (raw.timeout_ms.is_some(), "timeout_ms"),
            ] {
                if present {
                    note(format!(
                        "[slop.plugins.{name}] {key} ignored: only the user tier may set it \
                         (a cloned repository must not choose what executes)"
                    ));
                }
            }
            match raw.enable {
                Some(false) => entry.enable = false,
                Some(true) => note(format!(
                    "[slop.plugins.{name}] enable = true ignored: the repo tier may only disable"
                )),
                None => {}
            }
            if let Some(severity) = raw.severity {
                if level_rank(severity) <= level_rank(entry.severity) {
                    entry.severity = severity;
                } else {
                    note(format!(
                        "[slop.plugins.{name}] severity ignored: the repo tier may only lower it"
                    ));
                }
            }
        }
    }
    if let Some(settings) = raw.settings {
        entry.settings = settings;
    }
}

/// Drop plugin entries that are not plugins: no user-tier command, or a
/// name that collides with a loaded custom rule's producer (the two share
/// the `<producer>/<id>` namespace).
fn prune_plugins(cfg: &mut ResolvedConfig, custom: &[LoadedRule], notes: &mut Vec<ConfigNote>) {
    cfg.plugins.retain(|name, plugin| {
        if plugin.command.is_empty() {
            return false;
        }
        if custom
            .iter()
            .any(|c| c.id.split('/').next() == Some(name.as_str()))
        {
            notes.push(ConfigNote {
                tier: Tier::User,
                reason: format!(
                    "[slop.plugins.{name}] dropped: a custom rule already uses producer `{name}`"
                ),
            });
            return false;
        }
        true
    });
}

/// Read one tier's file. `None` path or `NotFound` file → tier absent, no
/// note; any other IO error → tier dropped with a note.
fn read_tier(path: Option<&Path>, tier: Tier) -> (Option<String>, Vec<ConfigNote>) {
    match path {
        None => (None, Vec::new()),
        Some(path) => match std::fs::read_to_string(path) {
            Ok(content) => (Some(content), Vec::new()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (None, Vec::new()),
            Err(err) => (
                None,
                vec![ConfigNote {
                    tier,
                    reason: format!("config dropped, unreadable: {err}"),
                }],
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPEC-006 acceptance criterion: the blocking set is asserted BY
    /// NAME. Adding a fourth `error` rule must fail here until the spec
    /// section "The v1 blocking set (frozen, owner decision 2026-09-21)"
    /// is amended.
    #[test]
    fn builtin_error_set_is_exactly_the_frozen_three() {
        let cfg = builtin_config();
        let mut errors: Vec<&str> = cfg
            .rules
            .iter()
            .filter(|(_, level)| **level == RuleLevel::Error)
            .map(|(family, _)| family.as_str())
            .collect();
        errors.sort_unstable();
        assert_eq!(
            errors,
            vec!["comment-history"],
            "SPEC-006 v1 blocking set is frozen (owner decision 2026-09-21, \
             amended 2026-10-03): comment-history alone ships at `error` in \
             the built-in tier. Adding an `error` rule fails this test until \
             SPEC-006 is amended."
        );
    }

    #[test]
    fn builtin_defaults_match_the_spec_configuration_section() {
        let cfg = builtin_config();
        assert_eq!(cfg.ref_, "auto");
        assert_eq!(cfg.mode, Mode::Observe);
        assert_eq!(cfg.language, None);
        assert_eq!(cfg.comment_language(), None);
        assert_eq!(cfg.timeout_ms, 4000);

        let expected: &[(&str, RuleLevel)] = &[
            ("comment-history", RuleLevel::Error),
            ("comment-meta", RuleLevel::Warning),
            ("review-jargon", RuleLevel::Warning),
            ("comment-paraphrase", RuleLevel::Warning),
            ("todo-no-condition", RuleLevel::Warning),
            ("perf-claim", RuleLevel::Warning),
            ("unsourced-constraint", RuleLevel::Warning),
            ("comment-language", RuleLevel::Off),
            ("rejected-alternative", RuleLevel::Off),
            ("manual-ddl", RuleLevel::Warning),
            ("bare-reference", RuleLevel::Warning),
            ("comment-hedging", RuleLevel::Warning),
            ("comment-steps", RuleLevel::Warning),
            ("docstring-restates-name", RuleLevel::Warning),
            ("self-justification", RuleLevel::Warning),
            ("config-deploy-narration", RuleLevel::Warning),
        ];
        assert_eq!(
            cfg.rules.len(),
            expected.len(),
            "built-in families changed: {:?}",
            cfg.rules.keys().collect::<Vec<_>>()
        );
        for (family, level) in expected {
            assert_eq!(cfg.rules.get(*family), Some(level), "family {family}");
        }

        assert!(cfg.overrides.is_empty());
        assert_eq!(cfg.exclude, ResolvedExclude::default());
        assert!(cfg.linters.is_empty());
        assert_eq!(cfg.notes, [] as [crate::config::ConfigNote; 0]);

        // Silencing is total: no finding, no advice, no gate entry.
        assert_eq!(RuleLevel::Off.severity(), None);
        assert_eq!(RuleLevel::Error.severity(), Some(Severity::Error));
        assert_eq!(RuleLevel::Hint.severity(), Some(Severity::Hint));
    }

    /// "Deep-merged per key" means both halves: a repo key wins, and a key
    /// the repo never mentions keeps the user tier's value — proven for
    /// scalars, rule families and linter fields alike.
    #[test]
    fn repo_overrides_user_per_key_and_absent_keys_keep_user_values() {
        let user = r#"
[slop]
mode = "block"
timeout_ms = 9000
ref = "origin/main"

[slop.rules]
comment-history = "warning"

[slop.linters.ruff]
severity = "warning"
select = ["F401"]
"#;
        let repo = r#"
[slop]
timeout_ms = 1000

[slop.rules]
todo-no-condition = "error"

[slop.linters.ruff]
enable = false
"#;
        let cfg = resolve(Some(user), Some(repo));

        // Repo wins where it spoke.
        assert_eq!(cfg.timeout_ms, 1000);
        assert_eq!(cfg.rules.get("todo-no-condition"), Some(&RuleLevel::Error));
        assert!(!cfg.linters["ruff"].enable);

        // User values survive keys the repo never mentioned.
        assert_eq!(cfg.mode, Mode::Block);
        assert_eq!(cfg.ref_, "origin/main");
        assert_eq!(cfg.rules.get("comment-history"), Some(&RuleLevel::Warning));
        assert_eq!(cfg.linters["ruff"].severity, RuleLevel::Warning);
        assert_eq!(
            cfg.linters["ruff"].select.as_deref(),
            Some(&["F401".to_string()][..])
        );

        // Built-ins survive keys neither tier mentioned.
        assert_eq!(cfg.rules.get("review-jargon"), Some(&RuleLevel::Warning));
        assert_eq!(cfg.rules.get("comment-language"), Some(&RuleLevel::Off));

        assert!(
            cfg.notes.is_empty(),
            "valid tiers record nothing: {:?}",
            cfg.notes
        );
    }

    #[test]
    fn lists_replace_across_tiers_and_paths_extend_appends() {
        let user = r#"
[slop.exclude]
paths = ["vendor/**", "gen/**"]
paths_extend = ["user-extra/**"]

[[slop.exclude.scoped]]
paths = ["tests/**"]
rules = ["slop/todo-no-condition"]

[slop.linters.ruff]
select = ["F401", "F811"]
ignore = ["E501"]
"#;
        let repo = r#"
[slop.exclude]
paths = ["third_party/**"]
paths_extend = ["repo-extra/**"]

[slop.linters.ruff]
select = ["ERA001"]
"#;
        let cfg = resolve(Some(user), Some(repo));

        // Lists REPLACE: the repo cancels the user's paths and select.
        assert_eq!(cfg.exclude.paths, vec!["third_party/**"]);
        assert_eq!(
            cfg.linters["ruff"].select.as_deref(),
            Some(&["ERA001".to_string()][..])
        );
        // Absent list keys keep the user tier's list.
        assert_eq!(
            cfg.linters["ruff"].ignore.as_deref(),
            Some(&["E501".to_string()][..])
        );
        assert_eq!(
            cfg.exclude.scoped,
            vec![ScopedExclude {
                paths: vec!["tests/**".to_string()],
                rules: vec!["slop/todo-no-condition".to_string()],
            }]
        );
        // Extending, not replacing, is the contract: user tier first,
        // repo tier appended after it.
        assert_eq!(
            cfg.exclude.paths_extend,
            vec!["user-extra/**", "repo-extra/**"]
        );

        // An explicit empty list cancels the lower tier's list — that is
        // the point of replacement semantics.
        let cfg = resolve(
            Some("[slop.exclude]\npaths = [\"a\"]\n"),
            Some("[slop.exclude]\npaths = []\n"),
        );
        assert_eq!(cfg.exclude.paths, [] as [std::string::String; 0]);
    }

    /// A broken repo tier degrades to the user tier; a broken user tier
    /// degrades to the built-ins. Both surface a reason; neither fails.
    #[test]
    fn malformed_toml_drops_that_tier_and_records_the_reason() {
        let cfg = resolve(
            Some("[slop]\nmode = \"block\"\n"),
            Some("[slop\nmode = \"block\"\n"), // unclosed table
        );
        assert_eq!(cfg.mode, Mode::Block, "user tier must stand");
        let repo_notes: Vec<&ConfigNote> =
            cfg.notes.iter().filter(|n| n.tier == Tier::Repo).collect();
        assert_eq!(
            repo_notes.len(),
            1,
            "exactly one repo note: {:?}",
            cfg.notes
        );
        assert!(
            repo_notes[0].reason.contains("malformed"),
            "{}",
            repo_notes[0].reason
        );

        let cfg = resolve(Some("this is } not toml"), None);
        assert_eq!(cfg.mode, Mode::Observe, "built-ins must stand");
        assert_eq!(cfg.timeout_ms, 4000);
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.tier == Tier::User && n.reason.contains("malformed")),
            "{:?}",
            cfg.notes
        );
    }

    /// A value of the wrong type is the same degradation as broken syntax:
    /// the tier goes, with the reason; the binary never sees an error.
    #[test]
    fn a_type_error_in_one_key_drops_the_tier_not_the_binary() {
        let cfg = resolve(
            None,
            Some("[slop]\ntimeout_ms = \"fast\"\nmode = \"block\"\n"),
        );
        assert_eq!(cfg.timeout_ms, 4000, "built-in survives the dropped tier");
        assert_eq!(
            cfg.mode,
            Mode::Observe,
            "the whole tier is dropped, not the key"
        );
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.tier == Tier::Repo && n.reason.contains("malformed")),
            "{:?}",
            cfg.notes
        );
    }

    #[test]
    fn unknown_rule_name_is_reported_not_fatal() {
        let cfg = resolve(
            None,
            Some(
                "[slop]\nmode = \"block\"\n\n[slop.rules]\ntypo-family = \"error\"\ncomment-history = \"warning\"\n",
            ),
        );
        // The rest of the tier still applies.
        assert_eq!(cfg.mode, Mode::Block);
        assert_eq!(cfg.rules.get("comment-history"), Some(&RuleLevel::Warning));
        // The name is kept (a newer binary may know it) and reported.
        assert_eq!(cfg.rules.get("typo-family"), Some(&RuleLevel::Error));
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.tier == Tier::Repo && n.reason.contains("typo-family")),
            "{:?}",
            cfg.notes
        );
    }

    /// Q1 + Q2: plugins are user-tier only, so the repo tier cannot lift a
    /// plugin's rule past the cap the user tier set, through the overrides
    /// table either.
    #[test]
    fn repo_overrides_cannot_raise_a_plugin_rule() {
        const USER: &str = "[slop.plugins.acme]\ncommand = [\"acme-slop\"]\nseverity = \"hint\"\n\n\
                            [slop.rules.overrides]\n\"acme/user-promoted\" = \"error\"\n";
        let repo = "[slop.rules.overrides]\n\"acme/x\" = \"error\"\n\"acme/y\" = \"warning\"\n\
                    \"acme/z\" = \"off\"\n\"acme/user-promoted\" = \"error\"\n";
        let cfg = resolve(Some(USER), Some(repo));
        assert_eq!(cfg.overrides.get("acme/x"), None, "{:?}", cfg.overrides);
        assert_eq!(cfg.overrides.get("acme/y"), None, "above the hint cap");
        assert_eq!(
            cfg.overrides.get("acme/z"),
            Some(&RuleLevel::Off),
            "the repo tier may lower"
        );
        assert_eq!(
            cfg.overrides.get("acme/user-promoted"),
            Some(&RuleLevel::Error),
            "an entry the user tier already set is not the repo tier's to refuse: \
             the user's own promotion stands"
        );
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.tier == Tier::Repo && n.reason.contains("acme/x ignored")),
            "{:?}",
            cfg.notes
        );
        // The user tier itself may promote a plugin rule.
        let user_only = resolve(Some(USER), None);
        assert_eq!(
            user_only.overrides.get("acme/user-promoted"),
            Some(&RuleLevel::Error)
        );
    }

    #[test]
    fn severity_resolution_routes_overrides_families_and_off() {
        let cfg = resolve(
            None,
            Some(
                r#"
[slop.rules]
comment-history = "warning"

[slop.rules.overrides]
"slop/todo-no-condition" = "error"
"ruff/ERA001" = "error"
"#,
            ),
        );
        // Overrides win over the family table and can promote imports.
        assert_eq!(
            cfg.severity_for("slop/todo-no-condition"),
            Some(Severity::Error)
        );
        assert_eq!(cfg.severity_for("ruff/ERA001"), Some(Severity::Error));
        // Family table (here: the tier's demotion).
        assert_eq!(
            cfg.severity_for("slop/comment-history"),
            Some(Severity::Warning)
        );
        // Built-in family untouched by the tier.
        assert_eq!(
            cfg.severity_for("slop/comment-meta"),
            Some(Severity::Warning)
        );
        // `off` and unoverridden foreign ids resolve to no severity.
        assert_eq!(cfg.severity_for("slop/comment-language"), None);
        assert_eq!(cfg.severity_for("ruff/F401"), None);
    }

    /// `language` is per-key like everything else, and `""` is a written
    /// value meaning "off" — distinct from absence.
    #[test]
    fn language_merges_per_key_and_empty_means_off() {
        let cfg = resolve(None, Some("[slop]\nlanguage = \"en\"\n"));
        assert_eq!(cfg.comment_language(), Some("en"));

        let cfg = resolve(
            Some("[slop]\nlanguage = \"en\"\n"),
            Some("[slop]\nlanguage = \"\"\n"),
        );
        assert_eq!(
            cfg.comment_language(),
            None,
            "repo cancels the user value per key"
        );

        let cfg = resolve(Some("[slop]\nlanguage = \"\"\n"), None);
        assert_eq!(
            cfg.language.as_deref(),
            Some(""),
            "empty stays distinct from unset"
        );
        assert_eq!(cfg.comment_language(), None);
    }

    /// The file-reading tier: a readable file applies, an absent file is
    /// silence, an unreadable path degrades with a reason. A directory
    /// stands in for an unreadable file so the failure does not depend on
    /// running as root.
    #[test]
    fn file_tiers_are_read_or_degraded_never_failed() {
        let dir = std::env::temp_dir();
        let user_path = dir.join(format!(
            "agent-slop-config-test-{}.toml",
            std::process::id()
        ));
        std::fs::write(&user_path, "[slop]\nmode = \"block\"\n").unwrap();

        let cfg = load_from_paths(Some(&user_path), Some(&dir));
        assert_eq!(cfg.mode, Mode::Block, "the readable user file applies");
        assert_eq!(
            cfg.notes.len(),
            1,
            "one note for the unreadable repo path: {:?}",
            cfg.notes
        );
        assert_eq!(cfg.notes[0].tier, Tier::Repo);
        assert!(
            cfg.notes[0].reason.contains("unreadable"),
            "{}",
            cfg.notes[0].reason
        );

        let absent = dir.join(format!(
            "agent-slop-config-absent-{}.toml",
            std::process::id()
        ));
        let cfg = load_from_paths(Some(&absent), None);
        assert!(
            cfg.notes.is_empty(),
            "absent files are the common case, not a note"
        );
        assert_eq!(cfg.mode, Mode::Observe);

        let _ = std::fs::remove_file(&user_path);
    }
}

// --- Exclusion predicates -------------------------------------------------
// These live beside the config they interpret rather than in `scan`, because
// BOTH surfaces must apply them: `scan`/`check` for reporting and the Stop
// gate for blocking. Housed in `scan`, the gate silently skipped them and a
// path a project had excluded still blocked the turn.

#[must_use]
pub fn excluded(cfg: &ResolvedConfig, rel: &str) -> bool {
    cfg.exclude
        .paths
        .iter()
        .chain(cfg.exclude.paths_extend.iter())
        .any(|p| glob_path(p, rel))
}

/// `vendor/**`, `**/*.generated.*`: the two shapes the config documents.
/// Anything else is compared literally.
pub(crate) fn glob_path(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(rest) = pattern.strip_prefix("**/") {
        return path
            .rsplit('/')
            .next()
            .is_some_and(|base| glob_path(rest, base));
    }
    star_match(pattern, path)
}

/// `*` matches any run of characters, including none; every other
/// character is literal. Patterns like `*.generated.*` need more than one
/// star, so the match walks the literal segments in order.
fn star_match(pattern: &str, path: &str) -> bool {
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return pattern == path;
    };
    if !path.starts_with(first) {
        return false;
    }
    let mut rest = &path[first.len()..];
    let segments: Vec<&str> = segments.collect();
    let Some((last, middle)) = segments.split_last() else {
        return rest.is_empty();
    };
    for seg in middle {
        match rest.find(seg) {
            Some(at) => rest = &rest[at + seg.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// A scoped exclude silences named rules under named paths.
#[must_use]
pub fn scoped_out(cfg: &ResolvedConfig, rel: &str, rule: &str) -> bool {
    cfg.exclude
        .scoped
        .iter()
        .any(|s| s.paths.iter().any(|p| glob_path(p, rel)) && s.rules.iter().any(|r| r == rule))
}

#[cfg(test)]
mod exclude_tests {
    use super::*;

    #[test]
    fn path_globs_cover_the_two_documented_shapes() {
        assert!(glob_path("vendor/**", "vendor/a.py"));
        assert!(glob_path("vendor/**", "vendor"));
        assert!(!glob_path("vendor/**", "vendored/a.py"));
        assert!(glob_path("**/*.generated.*", "src/api.generated.ts"));
        assert!(!glob_path("**/*.generated.*", "src/api.ts"));
    }
}
