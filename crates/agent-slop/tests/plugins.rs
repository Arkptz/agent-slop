//! WP-6 cover: the plugin host through the BUILT binary. The plugin is a
//! `/bin/sh` script written into the scratch `PATH`, speaking the real
//! `agent-slop-plugin/1` protocol. Tier trust, timeouts, output caps and
//! the `--since` baseline are all asserted against process exit codes and
//! the JSON envelope.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// A comment plugin in pure `sh`/`sed` (the Nix build sandbox has no
/// python): fires `acme/vague-why` on the last comment of each file whose
/// text contains "murky", declares itself deterministic, input comments,
/// cost A. The trigger word is not in the rule id, so an `allow` line
/// naming the rule never trips it.
const PLUGIN: &str = r#"#!/bin/sh
case "$1" in
  describe)
    echo '{"protocol":1,"name":"acme","version":"0.1.0","input":"comments","cost":"A","deterministic":true,"rules":[{"id":"vague-why","default_level":"warning","note":"say the constraint","exception":"it names the constraint"}]}'
    ;;
  check)
    input=$(cat)
    printf '{"protocol":1,"findings":['
    printf '%s' "$input" | sed 's/{"path":/\n&/g' | grep murky | {
      sep=""
      while IFS= read -r line; do
        path=$(printf '%s' "$line" | sed 's/^{"path":"\([^"]*\)".*/\1/')
        idx=$(printf '%s' "$line" | sed 's/.*"index":\([0-9]*\),"text":"[^"]*murky.*/\1/')
        printf '%s{"rule":"vague-why","path":"%s","comment":%s,"message":"the comment asserts a need without naming it"}' "$sep" "$path" "$idx"
        sep=","
      done
    }
    printf ']}'
    ;;
esac
"#;

/// A plugin whose manifest announces protocol 2.
const PLUGIN_FUTURE: &str = r#"#!/bin/sh
case "$1" in
  describe) echo '{"protocol":2,"name":"fut","version":"9","input":"comments","cost":"A","rules":[]}' ;;
  check) echo '{"protocol":2,"findings":[]}' ;;
esac
"#;

const TIMEOUT_PLUGIN: &str = r#"#!/bin/sh
case "$1" in
  describe) echo '{"protocol":1,"name":"slow","version":"1","input":"comments","cost":"A","rules":[{"id":"x","default_level":"warning"}]}' ;;
  check) sleep 5 ;;
esac
"#;

const LOUD_PLUGIN: &str = r#"#!/bin/sh
case "$1" in
  describe) echo '{"protocol":1,"name":"loud","version":"1","input":"comments","cost":"A","rules":[{"id":"x","default_level":"warning"}]}' ;;
  check) head -c 9000000 /dev/zero | tr '\\0' 'a' ;;
esac
"#;

struct Scratch {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
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
            "agent-slop-plugins-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let s = Scratch {
            repo: root.join("repo"),
            home: root.join("home"),
            bin: root.join("bin"),
            root,
        };
        std::fs::create_dir_all(&s.bin).expect("bin dir");
        std::fs::create_dir_all(&s.repo).expect("repo dir");
        std::fs::create_dir_all(s.home.join(".config").join("agent-slop")).expect("home");
        git(&s.repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&s.repo, &["config", "user.name", "agent-slop-plugins"]);
        git(
            &s.repo,
            &["config", "user.email", "plugins@example.invalid"],
        );
        s
    }

    fn write_plugin(&self, name: &str, body: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, body).expect("write plugin");
        std::fs::set_permissions(&path, Permissions::from_mode(0o755)).expect("chmod");
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.repo.join(rel), content).expect("write");
    }

    fn commit(&self, message: &str) {
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", message]);
    }

    fn user_config(&self, content: &str) {
        std::fs::write(
            self.home
                .join(".config")
                .join("agent-slop")
                .join("config.toml"),
            content,
        )
        .expect("write config");
    }

    fn repo_config(&self, content: &str) {
        std::fs::write(self.repo.join(".agent-hooks.toml"), content).expect("write repo config");
    }

    /// One invocation with the scratch `PATH` and `$HOME`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default(),
                ),
            )
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn agent-slop");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn scan(&self) -> (i32, serde_json::Value, String) {
        let (code, out, err) = self.run(&["scan", "--format", "json"]);
        let value = out
            .lines()
            .find(|l| l.starts_with('{'))
            .map_or(serde_json::Value::Null, |l| {
                serde_json::from_str(l).expect("json")
            });
        (code, value, err)
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

/// A comment that also trips the plugin (and `slop/comment-meta`, for the
/// `--fail-on` assertion), for the ratchet tests.
const RULE_1: &str = "# for now, for murky reasons\nx = 1\n";

#[test]
fn a_plugin_finding_is_reported_and_suppressible() {
    let s = Scratch::new("basic");
    s.write_plugin("acme-slop", PLUGIN);
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\ntimeout_ms = 4000\n");
    s.write("a.py", RULE_1);
    let (_, v, err) = s.scan();
    assert!(
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "acme/vague-why"
                && f["file"] == "a.py"
                && f["severity"] == "warning")),
        "{v} {err}"
    );
    assert!(
        v["skipped"]
            .as_array()
            .is_none_or(|sk| !sk.iter().any(|x| x["tool"] == "acme")),
        "the plugin must not be listed as skipped: {v}"
    );

    // `agent-slop: allow` applies to plugin findings.
    s.write(
        "a.py",
        "# agent-slop: allow acme/vague-why -- constraint is documented in ADR-031\n# murky reasons, kept\nx = 1\n",
    );
    let (_, v, _) = s.scan();
    let f = v["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["rule"] == "acme/vague-why")
        .expect("carried");
    assert_eq!(f["suppression"]["valid"], true, "{v}");
}

/// The trust rule: a cloned repo cannot choose what executes.
#[test]
fn repo_tier_cannot_declare_a_plugin_command() {
    let s = Scratch::new("trust");
    s.write_plugin("acme-slop", PLUGIN);
    // No user-tier declaration: the plugin is on PATH, but never declared.
    s.repo_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\nenable = true\n");
    s.write("a.py", RULE_1);
    let (_, v, err) = s.scan();
    assert!(
        v["findings"]
            .as_array()
            .is_none_or(|fs| !fs.iter().any(|f| f["rule"] == "acme/vague-why")),
        "a repo-declared plugin must not run: {v}"
    );
    assert!(err.contains("only the user tier may set it"), "{err}");
    assert!(err.contains("enable = true ignored"), "{err}");

    // A severity may only be lowered by the repo tier.
    let s2 = Scratch::new("trust-lower");
    s2.write_plugin("acme-slop", PLUGIN);
    s2.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\n");
    s2.repo_config("[slop.plugins.acme]\nseverity = \"hint\"\n");
    s2.write("a.py", RULE_1);
    let (_, v, _) = s2.scan();
    assert!(
        v["findings"].as_array().is_some_and(|fs| fs
            .iter()
            .any(|f| f["rule"] == "acme/vague-why" && f["severity"] == "hint")),
        "the repo tier lowered it: {v}"
    );

    let s3 = Scratch::new("trust-raise");
    s3.write_plugin("acme-slop", PLUGIN);
    s3.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\n");
    s3.repo_config("[slop.plugins.acme]\nseverity = \"error\"\n");
    s3.write("a.py", RULE_1);
    let (_, v, err) = s3.scan();
    assert!(
        err.contains("the repo tier may only lower it"),
        "raising must be refused: {err}"
    );
    assert!(
        v["findings"].as_array().is_some_and(|fs| fs
            .iter()
            .any(|f| f["rule"] == "acme/vague-why" && f["severity"] == "warning")),
        "capped at warning anyway: {v}"
    );
}

#[test]
fn plugin_timeout_kills_and_skips() {
    let s = Scratch::new("timeout");
    s.write_plugin("slow-slop", TIMEOUT_PLUGIN);
    s.user_config("[slop.plugins.slow]\ncommand = [\"slow-slop\"]\ntimeout_ms = 300\n");
    s.write("a.py", RULE_1);
    let started = std::time::Instant::now();
    let (_, v, _) = s.scan();
    assert!(
        started.elapsed() < std::time::Duration::from_millis(1000),
        "the scan must not wait out the plugin's sleep: {:?}",
        started.elapsed()
    );
    assert!(
        v["skipped"].as_array().is_some_and(|sk| sk
            .iter()
            .any(|x| x["tool"] == "slow" && x["reason"] == "timeout")),
        "{v}"
    );
}

#[test]
fn oversized_output_is_skipped() {
    let s = Scratch::new("loud");
    s.write_plugin("loud-slop", LOUD_PLUGIN);
    s.user_config("[slop.plugins.loud]\ncommand = [\"loud-slop\"]\n");
    s.write("a.py", RULE_1);
    let (_, v, _) = s.scan();
    assert!(
        v["skipped"].as_array().is_some_and(|sk| sk
            .iter()
            .any(|x| x["tool"] == "loud" && x["reason"] == "output-too-large")),
        "{v}"
    );
}

#[test]
fn protocol_mismatch_is_skipped() {
    let s = Scratch::new("future");
    s.write_plugin("fut-slop", PLUGIN_FUTURE);
    s.user_config("[slop.plugins.fut]\ncommand = [\"fut-slop\"]\n");
    s.write("a.py", RULE_1);
    let (_, v, _) = s.scan();
    assert!(
        v["skipped"]
            .as_array()
            .is_some_and(|sk| sk.iter().any(|x| x["tool"] == "fut"
                && x["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("protocol-mismatch")))),
        "{v}"
    );
}

#[test]
fn an_absent_plugin_binary_is_skipped_not_an_error() {
    let s = Scratch::new("absent");
    s.user_config("[slop.plugins.ghost]\ncommand = [\"no-such-plugin-binary\"]\n");
    s.write("a.py", RULE_1);
    let (code, v, _) = s.scan();
    assert_eq!(code, 0, "fail-open");
    assert!(
        v["skipped"].as_array().is_some_and(|sk| sk
            .iter()
            .any(|x| x["tool"] == "ghost" && x["reason"] == "absent")),
        "{v}"
    );
}

/// `--plugins none` and `--plugins <name>` select like `--linters`.
#[test]
fn plugins_flag_selects() {
    let s = Scratch::new("flag");
    s.write_plugin("acme-slop", PLUGIN);
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\n");
    s.write("a.py", RULE_1);

    let (_, out, _) = s.run(&["scan", "--format", "json", "--plugins", "none"]);
    assert!(
        !out.contains("acme/vague-why"),
        "--plugins none must silence: {out}"
    );
    let (_, out, _) = s.run(&["scan", "--format", "json", "--plugins", "acme"]);
    assert!(out.contains("acme/vague-why"), "{out}");
    let (_, out, _) = s.run(&["scan", "--format", "json", "--plugins", "other"]);
    assert!(!out.contains("acme/vague-why"), "{out}");
}

/// A plugin failure never changes the exit code (fail-open).
#[test]
fn scan_exit_code_is_unchanged_by_plugin_failure() {
    let s = Scratch::new("failopen");
    s.write_plugin("slow-slop", TIMEOUT_PLUGIN);
    s.user_config("[slop.plugins.slow]\ncommand = [\"slow-slop\"]\ntimeout_ms = 200\n");
    s.write("a.py", RULE_1);
    let (code, _, _) = s.run(&["scan", "--format", "json"]);
    assert_eq!(code, 0);
    let (code, _, _) = s.run(&["scan", "--format", "json", "--fail-on", "warning"]);
    assert_eq!(
        code, 1,
        "the slop RULE still trips --fail-on, the plugin's skip does not"
    );
}

/// The `--since` ratchet: a finding the file already carried at the base
/// is not "added"; a new one is.
#[test]
fn comment_plugin_findings_ratchet_under_since() {
    let s = Scratch::new("since");
    s.write_plugin("acme-slop", PLUGIN);
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\n");
    s.write("a.py", RULE_1);
    s.commit("base");
    let fork = git_out(&s.repo, &["rev-parse", "HEAD"]);

    // Edit the file without adding a second vague comment: nothing added.
    s.write("a.py", &format!("{RULE_1}y = 2\n"));
    let (_, out, _) = s.run(&["scan", "--format", "json", "--since", "HEAD"]);
    let v: serde_json::Value =
        serde_json::from_str(out.lines().find(|l| l.starts_with('{')).expect("json"))
            .expect("parse");
    assert!(
        v["added"].as_array().is_some_and(Vec::is_empty),
        "inherited plugin finding charged: {v} base {fork}"
    );

    // A second vague comment in a NEW file is added.
    s.write("b.py", RULE_1);
    let (_, out, _) = s.run(&["scan", "--format", "json", "--since", "HEAD"]);
    let v: serde_json::Value =
        serde_json::from_str(out.lines().find(|l| l.starts_with('{')).expect("json"))
            .expect("parse");
    assert!(
        v["added"].as_array().is_some_and(|a| !a.is_empty()),
        "a genuinely new plugin finding must be added: {v}"
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(
        &Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git")
            .stdout,
    )
    .trim()
    .to_string()
}

/// A nondeterministic (or file-input) plugin's findings are reported but
/// never counted as added — unattributable debt never blocks.
#[test]
fn file_input_plugin_findings_are_reported_but_never_added() {
    const FILE_PLUGIN: &str = r#"#!/bin/sh
case "$1" in
  describe) echo '{"protocol":1,"name":"fil","version":"1","input":"files","cost":"A","exts":["py"],"rules":[{"id":"len","default_level":"warning"}]}' ;;
  check) echo '{"protocol":1,"findings":[{"rule":"len","path":"a.py","line":2,"col":1,"message":"file is two lines"}]}' ;;
esac
"#;
    let s = Scratch::new("filein");
    s.write_plugin("fil-slop", FILE_PLUGIN);
    s.user_config("[slop.plugins.fil]\ncommand = [\"fil-slop\"]\n");
    s.write("a.py", "x = 1\ny = 2\n");
    s.commit("base");
    s.write("a.py", "x = 1\ny = 2\nz = 3\n");
    let (_, out, _) = s.run(&["scan", "--format", "json", "--since", "HEAD"]);
    let v: serde_json::Value =
        serde_json::from_str(out.lines().find(|l| l.starts_with('{')).expect("json"))
            .expect("parse");
    assert!(
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "fil/len")),
        "reported: {v}"
    );
    assert!(
        v["added"].as_array().is_some_and(Vec::is_empty),
        "never added: {v}"
    );
}

/// Q3 (owner decision 2026-10-25): plugins run in `PostToolUse`, advisory
/// only — advice on stdout at exit 0, failures as a stderr note.
/// The hook entry with a real payload: advice reaches stdout's
/// additionalContext, the exit stays 0, and a broken plugin only notes on
/// stderr.
#[test]
fn posttooluse_payload_carries_plugin_advice_and_survives_failures() {
    let s = Scratch::new("hook-payload");
    s.write_plugin("acme-slop", PLUGIN);
    s.write_plugin("slow-slop", TIMEOUT_PLUGIN);
    s.user_config(
        "[slop.plugins.acme]\ncommand = [\"acme-slop\"]\n\n[slop.plugins.slow]\ncommand = [\"slow-slop\"]\ntimeout_ms = 200\n",
    );
    s.write("a.py", "# murky reasons here\nx = 1\n");
    let payload = r#"{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"a.py"},"session_id":"s1"}"#;
    let (code, out, err) = run_hook(&s, payload);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("additionalContext"), "{out}");
    assert!(
        out.contains("acme/vague-why"),
        "plugin advice on stdout: {out}"
    );
    assert!(
        err.contains("plugin slow skipped: timeout"),
        "the slow plugin's failure is a stderr note: {err}"
    );

    // A clean file: no advice even though the plugin layer is on.
    s.write("clean.py", "x = 1\n");
    let payload = r#"{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"clean.py"},"session_id":"s1"}"#;
    let (code, out, err) = run_hook(&s, payload);
    assert_eq!(code, 0);
    assert!(out.is_empty(), "{out}");
    assert!(
        err.contains("plugin slow skipped: timeout"),
        "the skip note still surfaces: {err}"
    );
}

fn run_hook(s: &Scratch, payload: &str) -> (i32, String, String) {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
        .arg("hook")
        .current_dir(&s.repo)
        .env("HOME", &s.home)
        .env("XDG_CACHE_HOME", s.root.join("cache"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agent-slop hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("hook run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}
