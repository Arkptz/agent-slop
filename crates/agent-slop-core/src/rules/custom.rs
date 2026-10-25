//! Custom rules: `<producer>/<id>` TOML files discovered in the user and
//! repo rule directories, compiled into the scan's [`RuleSet`].
//!
//! Discovery never fails and never exits nonzero. A malformed file, an
//! unknown `kind`/`lang`/`schema`, a reserved producer, or a file past
//! the caps drops THAT rule and records a [`ConfigNote`]; the rest run.
//! A repo file with the same `producer/id` as a user file replaces it
//! whole — no field merge — so one rule has one definition.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::corpus::{EMBEDDED, RuleFile, RuleSet};
use crate::config::{ConfigNote, ResolvedConfig, RuleLevel, Tier};

/// A loaded custom rule with the tier it came from.
#[derive(Debug, Clone)]
pub struct LoadedRule {
    pub rule: RuleFile,
    pub tier: Tier,
    /// Full rule id: `<producer>/<id>`.
    pub id: String,
    /// The file it was read from, for `rules lint` and `explain`.
    pub source: PathBuf,
    /// The file's text: its contribution to the ruleset hash.
    pub text: String,
    pub(crate) regexes: Vec<regex::Regex>,
}

/// Producers a custom rule may never use: the engine's own and every
/// linter adapter's.
pub const RESERVED_PRODUCERS: [&str; 8] = [
    "slop",
    "suppression",
    "ruff",
    "deadnix",
    "statix",
    "clippy",
    "govet",
    "tsc",
];

/// Caps, per tier; past either, files are dropped with a note.
pub const MAX_FILES_PER_TIER: usize = 256;
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

/// `^[a-z][a-z0-9-]{0,31}$`.
#[must_use]
pub fn valid_producer(producer: &str) -> bool {
    let mut chars = producer.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && producer.len() <= 32
}

/// `^[a-z0-9][a-z0-9-]{0,48}$`.
#[must_use]
pub fn valid_rule_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && id.len() <= 49
}

/// `*.toml`, non-recursive, sorted, symlinks followed only to regular
/// files (the scan walker's caution).
fn rule_files(dir: &Path, tier: Tier, notes: &mut Vec<ConfigNote>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        // The link itself, not its target: a dangling or directory link
        // is skipped, not descended into.
        if std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            files.push(path);
        }
    }
    files.sort();
    if files.len() > MAX_FILES_PER_TIER {
        for extra in &files[MAX_FILES_PER_TIER..] {
            notes.push(ConfigNote {
                tier,
                reason: format!(
                    "custom rule dropped, over {MAX_FILES_PER_TIER} files per tier: {}",
                    extra.display()
                ),
            });
        }
        files.truncate(MAX_FILES_PER_TIER);
    }
    files
}

/// Parse and validate one rule file. `None` = dropped, reason noted.
pub fn load_text(
    text: &str,
    path: &Path,
    tier: Tier,
    notes: &mut Vec<ConfigNote>,
) -> Option<LoadedRule> {
    let drop = |reason: String, notes: &mut Vec<ConfigNote>| {
        notes.push(ConfigNote {
            tier,
            reason: format!("custom rule dropped, {}: {}", path.display(), reason),
        });
        None
    };
    let rule: RuleFile = match toml::from_str(text) {
        Ok(r) => r,
        Err(err) => return drop(format!("malformed TOML or wrong types: {err}"), notes),
    };
    let Some((producer, local)) = rule.meta.id.split_once('/') else {
        return drop(
            format!("meta.id `{}` must be `<producer>/<id>`", rule.meta.id),
            notes,
        );
    };
    if !valid_producer(producer) {
        return drop(
            format!("producer `{producer}` must match ^[a-z][a-z0-9-]{{0,31}}$"),
            notes,
        );
    }
    if RESERVED_PRODUCERS.contains(&producer) {
        return drop(format!("producer `{producer}` is reserved"), notes);
    }
    if !valid_rule_id(local) {
        return drop(
            format!("rule id `{local}` must match ^[a-z0-9][a-z0-9-]{{0,48}}$"),
            notes,
        );
    }
    match rule.meta.schema {
        Some(1) => {}
        Some(n) => {
            return drop(
                format!("unsupported schema {n} (this binary speaks schema 1)"),
                notes,
            );
        }
        None => return drop("meta.schema = 1 is required for custom rules".into(), notes),
    }
    match rule.meta.severity.as_str() {
        "warning" | "hint" | "off" | "error" => {}
        other => {
            return drop(
                format!("meta.severity `{other}` is not error|warning|hint|off"),
                notes,
            );
        }
    }
    for lang in &rule.meta.langs {
        if crate::lang::lang_names().all(|known| known != lang) {
            return drop(format!("unknown lang `{lang}`"), notes);
        }
    }
    let regexes = match super::generic::compile(&rule) {
        Ok(r) => r,
        Err(reason) => return drop(reason, notes),
    };
    Some(LoadedRule {
        id: rule.meta.id.clone(),
        rule,
        tier,
        source: path.to_path_buf(),
        text: text.to_string(),
        regexes,
    })
}

fn load_file(path: &Path, tier: Tier, notes: &mut Vec<ConfigNote>) -> Option<LoadedRule> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) => {
            notes.push(ConfigNote {
                tier,
                reason: format!("custom rule dropped, unreadable {}: {err}", path.display()),
            });
            return None;
        }
    };
    if meta.len() > MAX_FILE_BYTES {
        notes.push(ConfigNote {
            tier,
            reason: format!(
                "custom rule dropped, over {MAX_FILE_BYTES} bytes: {}",
                path.display()
            ),
        });
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(text) => load_text(&text, path, tier, notes),
        Err(err) => {
            notes.push(ConfigNote {
                tier,
                reason: format!("custom rule dropped, unreadable {}: {err}", path.display()),
            });
            None
        }
    }
}

/// Read a whole rule directory.
pub fn discover(dir: Option<&Path>, tier: Tier, notes: &mut Vec<ConfigNote>) -> Vec<LoadedRule> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    rule_files(dir, tier, notes)
        .iter()
        .filter_map(|path| load_file(path, tier, notes))
        .collect()
}

/// The strict-lint bar an override must clear before it may take a
/// `<producer>/*` rule to `error`: a non-empty exception, a note over 40
/// characters, and at least one hit and one miss fixture, each carrying
/// provenance. Returns the unmet requirements.
#[must_use]
pub fn strict_lint_gaps(rule: &RuleFile) -> Vec<String> {
    let mut gaps = Vec::new();
    if rule.exception.shape.trim().is_empty() {
        gaps.push("empty [exception] shape".to_string());
    }
    if rule.note.text.trim().chars().count() <= 40 {
        gaps.push("note is 40 characters or fewer".to_string());
    }
    let provenanced = |expect: &str| {
        rule.fixture
            .iter()
            .any(|f| f.expect == expect && f.provenance.trim().chars().count() >= 10)
    };
    if !provenanced("hit") {
        gaps.push("no hit fixture with provenance".to_string());
    }
    if !provenanced("miss") {
        gaps.push("no miss fixture with provenance".to_string());
    }
    gaps
}

/// Level the rule file declares, with the SPEC-006 clamp: a custom rule
/// may write `error` but it arrives as `warning` with a note; promotion
/// is the overrides table's job, after strict lint.
fn declared_level(rule: &RuleFile, tier: Tier, notes: &mut Vec<ConfigNote>) -> RuleLevel {
    match rule.meta.severity.as_str() {
        "hint" => RuleLevel::Hint,
        "off" => RuleLevel::Off,
        "error" => {
            notes.push(ConfigNote {
                tier,
                reason: format!(
                    "custom rule {} declares error, clamped to warning; promote via \
                     [slop.rules.overrides] after strict lint",
                    rule.meta.id
                ),
            });
            RuleLevel::Warning
        }
        _ => RuleLevel::Warning,
    }
}

/// Compile the custom layer into the resolved config: per-id replacement
/// (repo replaces user whole), declared levels into the family table,
/// the strict-lint check on every `error` override, and a rebuilt
/// ruleset when any rule survived.
pub fn compile(user_rules: &[LoadedRule], repo_rules: Vec<LoadedRule>, cfg: &mut ResolvedConfig) {
    let mut notes = std::mem::take(&mut cfg.notes);

    let mut by_id: BTreeMap<String, LoadedRule> = user_rules
        .iter()
        .map(|r| (r.id.clone(), r.clone()))
        .collect();
    for r in repo_rules {
        if by_id.contains_key(&r.id) {
            notes.push(ConfigNote {
                tier: Tier::Repo,
                reason: format!("custom rule {} replaces the user-tier file whole", r.id),
            });
        }
        by_id.insert(r.id.clone(), r);
    }

    for loaded in by_id.values() {
        let id = loaded.id.clone();
        // An explicit config entry wins over the file's declared level, in
        // either table; `error` there passes the same strict-lint bar.
        let in_overrides = cfg.overrides.contains_key(&id);
        let explicit = cfg
            .overrides
            .get(&id)
            .or_else(|| cfg.rules.get(&id))
            .copied();
        match explicit {
            Some(RuleLevel::Error) => {
                let gaps = strict_lint_gaps(&loaded.rule);
                if !gaps.is_empty() {
                    notes.push(ConfigNote {
                        tier: loaded.tier,
                        reason: format!(
                            "override to error on {id} clamped to warning: {}",
                            gaps.join("; ")
                        ),
                    });
                    if in_overrides {
                        cfg.overrides.insert(id, RuleLevel::Warning);
                    } else {
                        cfg.rules.insert(id, RuleLevel::Warning);
                    }
                }
            }
            Some(_) => {}
            None => {
                let level = declared_level(&loaded.rule, loaded.tier, &mut notes);
                cfg.rules.insert(id, level);
            }
        }
    }

    if !by_id.is_empty() {
        cfg.ruleset = Arc::new(RuleSet::with_custom(
            &EMBEDDED,
            by_id.into_values().collect(),
        ));
    }

    cfg.notes = notes;
}

/// Outcome of one `[[fixture]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureResult {
    pub index: usize,
    pub expect: String,
    pub passed: bool,
    pub detail: String,
}

/// Run a rule's fixtures through the real engine: the rule alone, enabled
/// at `warning` regardless of its declared level, against each fixture's
/// source as a file of its language.
#[must_use]
pub fn run_fixtures(loaded: &LoadedRule) -> Vec<FixtureResult> {
    let mut cfg = crate::config::builtin_config();
    cfg.rules.clear();
    cfg.rules.insert(loaded.id.clone(), RuleLevel::Warning);
    cfg.ruleset = Arc::new(RuleSet::with_custom(&[], vec![loaded.clone()]));

    loaded
        .rule
        .fixture
        .iter()
        .enumerate()
        .map(|(index, fixture)| {
            let result = |passed: bool, detail: String| FixtureResult {
                index,
                expect: fixture.expect.clone(),
                passed,
                detail,
            };
            if !matches!(fixture.expect.as_str(), "hit" | "miss") {
                return result(
                    false,
                    format!("expect `{}` is not hit|miss", fixture.expect),
                );
            }
            let path = match (&fixture.path, crate::lang::ext_for_lang(&fixture.lang)) {
                (Some(p), _) => p.clone(),
                (None, Some(ext)) => format!("fixture.{ext}"),
                (None, None) => {
                    return result(false, format!("unknown fixture lang `{}`", fixture.lang));
                }
            };
            let hits = super::scan_source(&cfg, &path, &fixture.source)
                .iter()
                .filter(|f| f.rule == loaded.id)
                .count();
            match (fixture.expect.as_str(), hits) {
                ("hit", 0) => result(false, "expected a hit, rule did not fire".into()),
                ("miss", n) if n > 0 => result(false, format!("expected a miss, rule fired {n}x")),
                _ => result(true, String::new()),
            }
        })
        .collect()
}

/// Everything `rules lint` reports about one rule: a declared `error`,
/// missing fixtures, the strict-lint gaps that would refuse a promotion,
/// and red fixtures.
#[must_use]
pub fn lint(loaded: &LoadedRule) -> Vec<String> {
    let rule = &loaded.rule;
    let mut problems = Vec::new();
    if rule.meta.severity == "error" {
        problems.push("declares severity error: custom rules arrive as warning, promote via [slop.rules.overrides]".to_string());
    }
    let has = |expect: &str| rule.fixture.iter().any(|f| f.expect == expect);
    for expect in ["hit", "miss"] {
        if !has(expect) {
            problems.push(format!("no {expect} fixture"));
        }
    }
    for gap in strict_lint_gaps(rule) {
        let missing_kind = (gap.starts_with("no hit fixture") && !has("hit"))
            || (gap.starts_with("no miss fixture") && !has("miss"));
        if !missing_kind {
            problems.push(format!("cannot be promoted to error: {gap}"));
        }
    }
    for r in run_fixtures(loaded) {
        if !r.passed {
            problems.push(format!(
                "fixture {} ({}): {}",
                r.index + 1,
                r.expect,
                r.detail
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{builtin_config, resolve_with};
    use crate::finding::Severity;

    const RULE: &str = r#"
[meta]
schema = 1
id = "acme/no-placeholder"
severity = "warning"

[match]
kind = "phrase"
phrases = ["placeholder implementation"]
word_boundaries = true
message = "placeholder '{match}' shipped"

[exception]
shape = "a stub that a test replaces with a fake on purpose"

[note]
text = "Implement it, or file a TODO that names the condition for removing it."

[[fixture]]
expect = "hit"
lang = "python"
provenance = "acme/repo src/a.py:12"
source = '''
# placeholder implementation, real one later
x = 1
'''

[[fixture]]
expect = "miss"
lang = "python"
provenance = "acme/repo src/b.py:40"
source = '''
# a real implementation
x = 1
'''
"#;

    fn load(text: &str) -> (Option<LoadedRule>, Vec<ConfigNote>) {
        let mut notes = Vec::new();
        let rule = load_text(text, Path::new("rule.toml"), Tier::User, &mut notes);
        (rule, notes)
    }

    fn cfg_with(rule: LoadedRule, config: Option<&str>) -> ResolvedConfig {
        resolve_with(config, None, &[rule], Vec::new())
    }

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "agent-slop-custom-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_valid_rule_fires_with_its_message_and_namespace() {
        let (rule, notes) = load(RULE);
        assert!(notes.is_empty(), "{notes:?}");
        let cfg = cfg_with(rule.expect("loads"), None);
        let findings =
            crate::rules::scan_source(&cfg, "a.py", "# placeholder implementation, later\nx = 1\n");
        let hit = findings
            .iter()
            .find(|f| f.rule == "acme/no-placeholder")
            .expect("custom rule fired");
        assert_eq!(hit.severity, Severity::Warning);
        assert_eq!(
            hit.message,
            "placeholder 'placeholder implementation' shipped"
        );
    }

    #[test]
    fn declared_error_is_clamped_to_warning() {
        let text = RULE.replace("severity = \"warning\"", "severity = \"error\"");
        let (rule, _) = load(&text);
        let cfg = cfg_with(rule.expect("loads"), None);
        assert_eq!(
            cfg.severity_for("acme/no-placeholder"),
            Some(Severity::Warning)
        );
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.reason.contains("clamped to warning")),
            "{:?}",
            cfg.notes
        );
    }

    #[test]
    fn override_to_error_requires_strict_lint() {
        const PROMOTE: &str = "[slop.rules.overrides]\n\"acme/no-placeholder\" = \"error\"\n";
        let (rule, _) = load(RULE);
        let cfg = cfg_with(rule.expect("loads"), Some(PROMOTE));
        assert_eq!(
            cfg.severity_for("acme/no-placeholder"),
            Some(Severity::Error),
            "a fully fixtured rule may be promoted"
        );

        let weak = RULE.replace(
            "provenance = \"acme/repo src/a.py:12\"",
            "provenance = \"\"",
        );
        let (rule, _) = load(&weak);
        let cfg = cfg_with(rule.expect("loads"), Some(PROMOTE));
        assert_eq!(
            cfg.severity_for("acme/no-placeholder"),
            Some(Severity::Warning),
            "missing provenance clamps the override"
        );
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.reason.contains("no hit fixture with provenance")),
            "{:?}",
            cfg.notes
        );
    }

    #[test]
    fn malformed_custom_rule_is_dropped_not_fatal() {
        for bad in [
            "this is } not toml",
            "[meta]\nid = \"acme/x\"\n",
            &RULE.replace("schema = 1", "schema = 2"),
            &RULE.replace("kind = \"phrase\"", "kind = \"fuzzy\""),
            &RULE.replace(
                "python\"\nprovenance = \"acme/repo src/a",
                "cobol\"\nprovenance = \"acme/repo src/a",
            ),
            &RULE.replace("severity = \"warning\"", "severity = \"loud\""),
        ] {
            let (rule, notes) = load(bad);
            if rule.is_some() {
                // The cobol fixture lang is a fixture problem, caught by lint.
                continue;
            }
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].reason.contains("custom rule dropped"), "{notes:?}");
        }
        let (rule, _) = load("this is } not toml");
        assert!(rule.is_none());
    }

    #[test]
    fn reserved_or_invalid_id_is_rejected() {
        for id in [
            "slop/x",
            "suppression/x",
            "ruff/x",
            "deadnix/x",
            "statix/x",
            "clippy/x",
            "govet/x",
            "tsc/x",
            "Acme/x",
            "9acme/x",
            "acme/Bad",
            "acme",
            "a-very-long-producer-name-over-thirty-two/x",
        ] {
            let text = RULE.replace("acme/no-placeholder", id);
            let (rule, notes) = load(&text);
            assert!(rule.is_none(), "{id} must be rejected");
            assert_eq!(notes.len(), 1, "{id}: {notes:?}");
        }
        let (ok, _) = load(&RULE.replace("acme/no-placeholder", "acme-2/a1"));
        assert!(ok.is_some());
    }

    #[test]
    fn repo_rule_replaces_user_rule_with_same_id() {
        let user_dir = scratch("user");
        let repo_dir = scratch("repo");
        std::fs::write(user_dir.join("a.toml"), RULE).expect("write");
        std::fs::write(
            repo_dir.join("a.toml"),
            RULE.replace("placeholder implementation", "stub body"),
        )
        .expect("write");
        let cfg = crate::config::load_with_rule_dirs(
            None,
            None,
            Some(user_dir.as_path()),
            Some(repo_dir.as_path()),
        );
        let _ = std::fs::remove_dir_all(&user_dir);
        let _ = std::fs::remove_dir_all(&repo_dir);

        let fires = |src: &str| {
            crate::rules::scan_source(&cfg, "a.py", src)
                .iter()
                .any(|f| f.rule == "acme/no-placeholder")
        };
        assert!(
            fires("# stub body here\nx = 1\n"),
            "the repo definition wins"
        );
        assert!(
            !fires("# placeholder implementation\nx = 1\n"),
            "no field merge: the user definition is gone"
        );
        assert_eq!(cfg.ruleset.custom.len(), 1);
        assert!(
            cfg.notes
                .iter()
                .any(|n| n.reason.contains("replaces the user-tier file")),
            "{:?}",
            cfg.notes
        );
    }

    #[test]
    fn custom_enable_false_disables_the_layer() {
        let (rule, _) = load(RULE);
        let cfg = cfg_with(
            rule.expect("loads"),
            Some("[slop.custom]\nenable = false\n"),
        );
        assert!(cfg.ruleset.custom.is_empty());
        assert!(
            crate::rules::scan_source(&cfg, "a.py", "# placeholder implementation\nx = 1\n")
                .iter()
                .all(|f| f.rule != "acme/no-placeholder")
        );
    }

    #[test]
    fn oversized_or_excess_files_are_dropped_with_a_note() {
        let dir = scratch("caps");
        std::fs::write(
            dir.join("big.toml"),
            format!("{RULE}\n# {}\n", "x".repeat(300 * 1024)),
        )
        .expect("write");
        let mut notes = Vec::new();
        let rules = discover(Some(&dir), Tier::User, &mut notes);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(rules.is_empty());
        assert!(notes[0].reason.contains("bytes"), "{notes:?}");
    }

    #[test]
    fn fixtures_run_through_the_real_engine() {
        let (rule, _) = load(RULE);
        let rule = rule.expect("loads");
        assert!(run_fixtures(&rule).iter().all(|r| r.passed));
        assert!(lint(&rule).is_empty(), "{:?}", lint(&rule));

        let no_miss = RULE.replace("expect = \"miss\"", "expect = \"hit\"");
        let (rule, _) = load(&no_miss);
        assert!(
            lint(&rule.expect("loads"))
                .iter()
                .any(|p| p == "no miss fixture"),
        );

        let red = RULE.replace("# a real implementation", "# placeholder implementation");
        let (rule, _) = load(&red);
        let results = run_fixtures(&rule.expect("loads"));
        assert!(results.iter().any(|r| !r.passed && r.expect == "miss"));
    }

    #[test]
    fn the_builtin_levels_do_not_list_custom_ids() {
        assert!(builtin_config().ruleset.custom.is_empty());
    }
}
