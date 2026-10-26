#![allow(clippy::expect_used, clippy::panic)]

//! POL-026 cover for `agent-slop self-test`: the command exists to say
//! whether the gate that will actually run is the frozen one, so its
//! subject is the RESOLVED config, not the built-in tier. Against the
//! built-ins alone a repo `.agent-hooks.toml` could demote a frozen rule,
//! promote a fourth, or flip `mode` back to `observe`, and the command
//! still printed "0 failed".
//!
//! Every scenario drives the built binary in a scratch repository with
//! `$HOME` inside it, so the machine's own user tier is never read.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
            "agent-slop-selftest-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let s = Scratch {
            repo: root.join("repo"),
            home: root.join("home"),
            root,
        };
        std::fs::create_dir_all(&s.repo).expect("repo dir");
        std::fs::create_dir_all(s.home.join(".config").join("agent-slop")).expect("home dir");
        git(&s.repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&s.repo, &["config", "user.name", "agent-slop-selftest"]);
        git(
            &s.repo,
            &["config", "user.email", "selftest@example.invalid"],
        );
        std::fs::write(s.repo.join("a.py"), "x = 1\n").expect("write");
        git(&s.repo, &["add", "-A"]);
        git(&s.repo, &["commit", "--quiet", "-m", "base"]);
        s
    }

    fn user_tier(&self, content: &str) {
        let path = self
            .home
            .join(".config")
            .join("agent-slop")
            .join("config.toml");
        std::fs::write(path, content).expect("write user tier");
    }

    fn repo_tier(&self, content: &str) {
        std::fs::write(self.repo.join(".agent-hooks.toml"), content).expect("write repo tier");
    }

    /// One real `agent-slop self-test`: exit code, then stdout and stderr
    /// joined, because the command reports verdicts on both.
    fn run(&self) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .arg("self-test")
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

/// The control: with no tier file anywhere the frozen set stands, so a
/// later failure is about the tier and not about the fixture.
#[test]
fn an_untouched_configuration_passes() {
    let s = Scratch::new("clean");
    let (code, text) = s.run();
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("0 failed"), "{text}");
}

/// A repo tier demoting the frozen blocking rule empties the gate.
#[test]
fn a_repo_tier_demoting_a_frozen_rule_fails_and_names_the_tier() {
    let s = Scratch::new("demote");
    s.repo_tier("[slop]\n[slop.rules]\ncomment-history = \"warning\"\n");
    let (code, text) = s.run();
    assert_eq!(code, 1, "a demoted frozen rule must fail: {text}");
    assert!(
        text.contains("slop/comment-history demoted by the repo tier"),
        "{text}"
    );
}

/// The other direction: a fourth blocking rule is also a departure from
/// the frozen set, and blocks turns the owner never approved.
#[test]
fn a_repo_tier_promoting_a_fourth_blocking_rule_fails() {
    let s = Scratch::new("promote");
    s.repo_tier("[slop]\n[slop.rules]\ntodo-no-condition = \"error\"\n");
    let (code, text) = s.run();
    assert_eq!(code, 1, "a fourth error rule must fail: {text}");
    assert!(
        text.contains("slop/todo-no-condition promoted by the repo tier"),
        "{text}"
    );
}

/// The promotion can also arrive as a full-id override rather than a
/// family level; both reach the same blocking set.
#[test]
fn an_override_promotion_is_caught_too() {
    let s = Scratch::new("override");
    s.repo_tier("[slop]\n[slop.rules.overrides]\n\"slop/perf-claim\" = \"error\"\n");
    let (code, text) = s.run();
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("slop/perf-claim promoted by the repo tier"),
        "{text}"
    );
}

/// `observe` runs the Stop leg and records telemetry but never exits 2,
/// so a repo flipping the Nix-set `block` back is an inert gate. The
/// weakening must be attributed to the repo tier: the built-in default
/// is `observe` too, and crediting it would name a tier that cannot
/// weaken anything.
#[test]
fn a_repo_tier_flipping_mode_back_to_observe_fails() {
    let s = Scratch::new("mode");
    s.user_tier("[slop]\nmode = \"block\"\n");
    s.repo_tier("[slop]\nmode = \"observe\"\n");
    let (code, text) = s.run();
    assert_eq!(code, 1, "an inert gate must fail: {text}");
    assert!(
        text.contains("mode weakened to observe by the repo tier"),
        "{text}"
    );
}

/// A user tier at `block` with no repo tier is the shipped state, not a
/// weakening.
#[test]
fn the_nix_user_tier_alone_passes() {
    let s = Scratch::new("usertier");
    s.user_tier("[slop]\nmode = \"block\"\n");
    let (code, text) = s.run();
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("mode Block from the user tier"), "{text}");
}

/// A tune that strips a frozen blocking family's phrases weakens the gate
/// exactly like a demotion, and must fail the same way.
#[test]
fn removing_a_frozen_families_phrases_by_tune_fails() {
    let s = Scratch::new("tune-weaken");
    s.user_tier("[slop.tune.comment-history]\nphrases_remove = [\"previously\"]\n");
    let (code, text) = s.run();
    assert_eq!(code, 1, "a weakened frozen rule must fail: {text}");
    assert!(
        text.contains("slop/comment-history weakened by tune"),
        "{text}"
    );
}

/// Extending a frozen family's list only adds matches: reported, not
/// failed.
#[test]
fn extending_a_frozen_families_phrases_is_info_not_failure() {
    let s = Scratch::new("tune-extend");
    s.user_tier("[slop.tune.comment-history]\nphrases_extend = [\"back then\"]\n");
    let (code, text) = s.run();
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("slop/comment-history tune extends its lists by 1 entries"),
        "{text}"
    );
}

/// A tune on a non-frozen family is nobody's business: no note, no
/// failure, but the behaviour changes.
#[test]
fn tuning_a_non_frozen_family_passes_and_changes_behaviour() {
    let s = Scratch::new("tune-other");
    s.user_tier("[slop.tune.perf-claim]\nphrases_extend = [\"blazing fast in prod\"]\n");
    let (code, text) = s.run();
    assert_eq!(code, 0, "{text}");

    std::fs::write(s.repo.join("a.py"), "# blazing fast in prod\nx = 1\n").expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
        .args(["check", "a.py"])
        .current_dir(&s.repo)
        .env("HOME", &s.home)
        .env("XDG_CACHE_HOME", s.root.join("cache"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn agent-slop");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("slop/perf-claim"),
        "the extended phrase must fire: {text}"
    );
}
