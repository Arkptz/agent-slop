//! WP-4 cover: `rules lint`, `rules test`, `rules --format json`,
//! `explain` and `self-test` over custom rule files, driven through the
//! built binary in a scratch repository with `$HOME` inside it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const RULE: &str = r##"
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
shape = "a stub that a test swaps for a fake on purpose"

[note]
text = "Implement it, or file a TODO that names the condition for removing it."

[[fixture]]
expect = "hit"
lang = "python"
provenance = "acme/repo src/a.py:12"
source = "# placeholder implementation, later\nx = 1\n"

[[fixture]]
expect = "miss"
lang = "python"
provenance = "acme/repo src/b.py:40"
source = "# a real implementation\nx = 1\n"
"##;

struct Scratch {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let root = std::env::temp_dir().join(format!(
            "agent-slop-rules-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let s = Scratch {
            repo: root.join("repo"),
            home: root.join("home"),
            root,
        };
        std::fs::create_dir_all(&s.repo).expect("repo dir");
        std::fs::create_dir_all(s.home.join(".config").join("agent-slop").join("rules"))
            .expect("home dir");
        git(&s.repo, &["init", "--quiet", "--initial-branch=main"]);
        s
    }

    fn user_rule(&self, name: &str, text: &str) -> PathBuf {
        let path = self
            .home
            .join(".config")
            .join("agent-slop")
            .join("rules")
            .join(name);
        std::fs::write(&path, text).expect("write user rule");
        path
    }

    fn repo_rule(&self, name: &str, text: &str) -> PathBuf {
        let dir = self.repo.join(".agent-slop").join("rules");
        std::fs::create_dir_all(&dir).expect("repo rules dir");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write repo rule");
        path
    }

    /// One real invocation: exit code, then stdout and stderr joined.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn agent-slop");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_clean_rule_lints_and_tests_green() {
    let s = Scratch::new("clean");
    s.user_rule("no-placeholder.toml", RULE);
    let (code, text) = s.run(&["rules", "lint"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("0 problems"), "{text}");
    let (code, text) = s.run(&["rules", "test"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("2 fixtures, 0 red"), "{text}");
}

/// A rule with no miss fixture proves nothing about false positives.
#[test]
fn lint_exits_1_on_a_rule_with_no_miss_fixture() {
    let s = Scratch::new("nomiss");
    s.user_rule(
        "no-placeholder.toml",
        &RULE.replace("expect = \"miss\"", "expect = \"hit\""),
    );
    let (code, text) = s.run(&["rules", "lint"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("no miss fixture"), "{text}");
}

#[test]
fn lint_reports_a_dropped_rule_with_its_reason() {
    let s = Scratch::new("dropped");
    s.user_rule("bad.toml", &RULE.replace("acme/no-placeholder", "ruff/x"));
    let (code, text) = s.run(&["rules", "lint"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("producer `ruff` is reserved"), "{text}");
}

#[test]
fn test_exits_1_when_a_fixture_is_red() {
    let s = Scratch::new("red");
    s.user_rule(
        "no-placeholder.toml",
        &RULE.replace("# a real implementation", "# placeholder implementation"),
    );
    let (code, text) = s.run(&["rules", "test"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("FAIL acme/no-placeholder"), "{text}");
    assert!(text.contains("expected a miss"), "{text}");
}

/// A hit fixture the rule does not fire on is red for `rules lint` AND
/// `rules test`, both with the "expected a hit" detail — with a miss
/// fixture present, so lint cannot be failing for the missing-kind reason.
#[test]
fn a_red_hit_fixture_fails_lint_and_test() {
    let s = Scratch::new("red-hit");
    s.user_rule(
        "no-placeholder.toml",
        &RULE.replace(
            "# placeholder implementation, later",
            "# nothing matching here",
        ),
    );
    let (code, text) = s.run(&["rules", "lint"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("fixture 1 (hit)"), "{text}");
    assert!(text.contains("expected a hit"), "{text}");
    assert!(!text.contains("no miss fixture"), "{text}");
    assert!(!text.contains("no hit fixture"), "{text}");
    let (code, text) = s.run(&["rules", "test"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("expected a hit"), "{text}");
}

#[test]
fn named_files_are_linted_instead_of_the_tiers() {
    let s = Scratch::new("named");
    let path = s.repo_rule(
        "x.toml",
        &RULE.replace("expect = \"miss\"", "expect = \"hit\""),
    );
    let (code, text) = s.run(&["rules", "lint", path.to_str().expect("utf8")]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("(repo tier"), "{text}");
}

/// A red repo-rule fixture fails the self-test and names the tier.
#[test]
fn self_test_exits_1_and_names_the_tier_for_a_red_repo_fixture() {
    let s = Scratch::new("selftest");
    s.repo_rule(
        "no-placeholder.toml",
        &RULE.replace("# a real implementation", "# placeholder implementation"),
    );
    let (code, text) = s.run(&["self-test"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("fixture 2 (miss) is red in the repo tier"),
        "{text}"
    );

    let green = Scratch::new("selftest-green");
    green.repo_rule("no-placeholder.toml", RULE);
    let (code, text) = green.run(&["self-test"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("2 custom rule fixtures run"), "{text}");
}

#[test]
fn rules_json_carries_tier_and_producer() {
    let s = Scratch::new("json");
    s.user_rule("no-placeholder.toml", RULE);
    let (code, text) = s.run(&["rules", "--format", "json"]);
    assert_eq!(code, 0, "{text}");
    let line = text
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("a json line");
    let v: serde_json::Value = serde_json::from_str(line).expect("json");
    assert_eq!(v["schema"], 1);
    let rules = v["rules"].as_array().expect("rules array");
    let custom = rules
        .iter()
        .find(|r| r["id"] == "acme/no-placeholder")
        .expect("custom rule listed");
    assert_eq!(custom["producer"], "acme");
    assert_eq!(custom["tier"], "user");
    assert_eq!(custom["level"], "warning");
    let builtin = rules
        .iter()
        .find(|r| r["id"] == "slop/comment-history")
        .expect("builtin listed");
    assert_eq!(builtin["producer"], "slop");
    assert_eq!(builtin["tier"], "built-in");
}

#[test]
fn explain_prints_note_exception_suppression_and_source() {
    let s = Scratch::new("explain");
    let path = s.user_rule("no-placeholder.toml", RULE);
    let (code, text) = s.run(&["explain", "acme/no-placeholder"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("acme/no-placeholder: warning"), "{text}");
    assert!(text.contains("do instead: Implement it"), "{text}");
    assert!(
        text.contains("exception: a stub that a test swaps"),
        "{text}"
    );
    assert!(
        text.contains("suppress: // agent-slop: allow acme/no-placeholder --"),
        "{text}"
    );
    assert!(
        text.contains(&format!("source: {}", path.display())),
        "{text}"
    );
}

/// The custom rule is a first-class producer for scan, suppression
/// included.
#[test]
fn scan_reports_a_custom_rule_and_honours_its_suppression() {
    let s = Scratch::new("scan");
    s.user_rule("no-placeholder.toml", RULE);
    std::fs::write(
        s.repo.join("a.py"),
        "# placeholder implementation, later\nx = 1\n",
    )
    .expect("write");
    std::fs::write(
        s.repo.join("b.py"),
        "# agent-slop: allow acme/no-placeholder -- stub tracked in ADR-031\n\
         # placeholder implementation, later\nx = 1\n",
    )
    .expect("write");
    let (code, text) = s.run(&["scan", "--format", "json"]);
    assert_eq!(code, 0, "{text}");
    let v: serde_json::Value =
        serde_json::from_str(text.lines().find(|l| l.starts_with('{')).expect("json"))
            .expect("parse");
    let findings = v["findings"].as_array().expect("findings");
    let a = findings
        .iter()
        .find(|f| f["file"] == "a.py" && f["rule"] == "acme/no-placeholder")
        .expect("a.py flagged");
    assert_eq!(a["suppression"], serde_json::Value::Null);
    let b = findings
        .iter()
        .find(|f| f["file"] == "b.py" && f["rule"] == "acme/no-placeholder")
        .expect("b.py carried");
    assert_eq!(b["suppression"]["valid"], true);
}
