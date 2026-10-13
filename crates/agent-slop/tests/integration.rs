//! SPEC-006's ratchet + suppression matrix through the BUILT binary's
//! `hook` leg: the Stop scenarios exactly as a harness drives them,
//! asserted on the process exit code. `tests/telemetry.rs` covers the
//! rows these legs write; this file covers only the verdicts.
//!
//! Every scenario gets a fresh repository in `block` mode — the built-in
//! mode is `observe`, which never exits 2, so a test of the blocking
//! path that forgot that would pass while proving nothing.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fires `slop/comment-history` at error — the fixture the engine's own
/// tests use.
const SLOP: &str = "# We previously always sent a Referer — that was a bug.\n";
const CLEAN: &str = "referer = None\n";
/// The accepted suppression form (src/rules/suppression.rs): rule id,
/// `--`, then a reason of at least 12 non-whitespace characters that
/// carries a referent.
const ALLOW_VALID: &str =
    "# agent-slop: allow slop/comment-history -- rollback steps required by ADR-031\n";
const ALLOW_NO_REASON: &str = "# agent-slop: allow slop/comment-history\n";
const ALLOW_SHORT_REASON: &str = "# agent-slop: allow slop/comment-history -- too short\n";

struct HookRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl HookRun {
    fn assert_rc(&self, want: i32, label: &str) {
        assert_eq!(
            self.code, want,
            "{label}: expected exit {want}, got {} --- stdout ---\n{}--- stderr ---\n{}",
            self.code, self.stdout, self.stderr
        );
    }
}

struct Scratch {
    root: PathBuf,
    repo: PathBuf,
    reviews: PathBuf,
    cache: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        assert!(
            Command::new("git")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success()),
            "git is absent: these scenarios cannot run, and a silent pass \
             would hide a gate that never fires (POL-026)"
        );
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let root = std::env::temp_dir().join(format!(
            "agent-slop-scenario-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        // Untracked paths are scanned, so the session pin must stay out of
        // the worktree or every scratch run turns into a full scan.
        let s = Scratch {
            repo: root.join("repo"),
            reviews: root.join("reviews"),
            cache: root.join("cache"),
            root,
        };
        std::fs::create_dir_all(&s.repo).expect("repo dir");
        std::fs::create_dir_all(&s.cache).expect("cache dir");
        s.git(&["init", "--quiet", "--initial-branch=main"]);
        s.git(&["config", "user.name", "agent-slop-scenario"]);
        s.git(&["config", "user.email", "scenario@example.invalid"]);
        s.write(".agent-hooks.toml", "[slop]\nmode = \"block\"\n");
        s.write("a.py", CLEAN);
        s.commit("base");
        s
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.repo.join(rel), content).expect("write");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }

    /// One real `agent-slop hook` run: stdin is the payload, the
    /// environment is hermetic, and the exit code is the hook's own.
    fn hook(&self, payload: &str) -> HookRun {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .arg("hook")
            .current_dir(&self.repo)
            .env("AGENT_SLOP_TELEMETRY_ROOT", &self.reviews)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GROK_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agent-slop");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write payload");
        let out = child.wait_with_output().expect("wait");
        HookRun {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

fn session_start(session_id: &str) -> String {
    format!(r#"{{"hook_event_name":"SessionStart","session_id":"{session_id}"}}"#)
}

fn stop(session_id: &str) -> String {
    format!(r#"{{"hook_event_name":"Stop","session_id":"{session_id}"}}"#)
}

/// SPEC-006 "Inherited debt never blocks": the slop landed in a commit
/// made before the session pinned its baseline, so the finding cancels
/// against it. The clean edit exists to put a.py into the changed set —
/// without it the file is never scanned and the cancel goes untested.
#[test]
fn committed_slop_does_not_block() {
    let s = Scratch::new("committed");
    s.write("a.py", &format!("{SLOP}{CLEAN}"));
    s.commit("debt landed before the session");
    let sid = "scenario-committed";
    s.hook(&session_start(sid)).assert_rc(0, "SessionStart");
    s.write("a.py", &format!("{SLOP}{CLEAN}x = 1\n"));
    s.hook(&stop(sid)).assert_rc(0, "Stop over inherited debt");
}

/// SPEC-006 "A newly added error finding blocks": the same fixture,
/// written after the pin.
#[test]
fn slop_added_by_the_session_blocks() {
    let s = Scratch::new("added");
    let sid = "scenario-added";
    s.hook(&session_start(sid)).assert_rc(0, "SessionStart");
    s.write("a.py", &format!("{SLOP}{CLEAN}"));
    s.hook(&stop(sid))
        .assert_rc(2, "Stop on slop the session added");
}

/// SPEC-006 "A suppression with a valid reason does not block": the
/// finding is added by the session, carried with its suppression, and
/// never reaches the blocking set.
#[test]
fn a_valid_suppression_on_an_added_finding_does_not_block() {
    let s = Scratch::new("valid-allow");
    let sid = "scenario-valid";
    s.hook(&session_start(sid)).assert_rc(0, "SessionStart");
    s.write("a.py", &format!("{ALLOW_VALID}{SLOP}{CLEAN}"));
    s.hook(&stop(sid))
        .assert_rc(0, "Stop with a valid suppression");
}

/// SPEC-006 "A suppression without a reason blocks": no `--` section —
/// the finding stands and `slop/suppression-empty` is raised at error.
#[test]
fn a_suppression_without_a_reason_still_blocks() {
    let s = Scratch::new("no-reason");
    let sid = "scenario-noreason";
    s.hook(&session_start(sid)).assert_rc(0, "SessionStart");
    s.write("a.py", &format!("{ALLOW_NO_REASON}{SLOP}{CLEAN}"));
    s.hook(&stop(sid))
        .assert_rc(2, "Stop with a reasonless suppression");
}

/// The other arm of the same scenario: a reason under 12 non-whitespace
/// characters counts as empty by the same rule.
#[test]
fn a_suppression_with_a_short_reason_still_blocks() {
    let s = Scratch::new("short-reason");
    let sid = "scenario-short";
    s.hook(&session_start(sid)).assert_rc(0, "SessionStart");
    s.write("a.py", &format!("{ALLOW_SHORT_REASON}{SLOP}{CLEAN}"));
    s.hook(&stop(sid))
        .assert_rc(2, "Stop with a too-short reason");
}

/// The slop-sweep skill tells the agent to read `explain` before fixing
/// under an unfamiliar rule, so it must print each family's note and
/// exception straight from the embedded corpus, not a placeholder.
#[test]
fn explain_prints_the_rule_note_and_exception_from_the_corpus() {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
        .args(["explain", "slop/comment-meta"])
        .env("HOME", std::env::temp_dir())
        .output()
        .expect("run agent-slop explain");
    assert!(out.status.success(), "explain exit: {:?}", out.status);
    let text = String::from_utf8_lossy(&out.stdout);

    let corpus: toml::Value = toml::from_str(include_str!(
        "../../agent-slop-core/rules/comment-meta.toml"
    ))
    .expect("corpus parses");
    let note = corpus["note"]["text"].as_str().expect("note.text").trim();
    let shape = corpus["exception"]["shape"]
        .as_str()
        .expect("exception.shape")
        .trim();

    assert!(text.contains(note), "note missing from explain:\n{text}");
    assert!(
        text.contains(shape),
        "exception missing from explain:\n{text}"
    );
    assert!(
        !text.contains("corpus lands"),
        "placeholder text is back:\n{text}"
    );
}

/// `scan --since <ref>` reports only files that differ from the ref. In a
/// monorepo the unnarrowed scan listed every project's findings, and the
/// PR's own were cut off by truncation.
#[test]
fn scan_since_is_limited_to_the_diff() {
    let s = Scratch::new("scan-since");
    s.write(
        "old.py",
        "# We previously sent a Referer.\nreferer = None\n",
    );
    s.commit("base");
    let base = s.git(&["rev-parse", "HEAD"]).trim().to_string();
    s.write("new.py", "# We previously cached this.\ncache = None\n");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
        .args([
            "scan",
            "--since",
            &base,
            "--format",
            "json",
            "--linters",
            "none",
        ])
        .current_dir(&s.repo)
        .env("XDG_CACHE_HOME", &s.cache)
        .env("AGENT_SLOP_TELEMETRY_ROOT", &s.reviews)
        .env("HOME", &s.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn agent-slop");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json envelope");
    let files: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|f| f["file"].as_str())
        .collect();
    assert_eq!(files, ["new.py"], "unchanged files must not be scanned");
}
