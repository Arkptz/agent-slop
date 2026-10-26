#![allow(clippy::expect_used, clippy::panic)]

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
        self.run_with_env(args, &[])
    }

    /// [`Scratch::run`] with extra variables in the host's environment.
    fn run_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .args(args)
            .envs(extra.iter().copied())
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
        self.scan_args(&[])
    }

    /// `scan --format json` plus extra arguments, with the envelope parsed.
    fn scan_args(&self, extra: &[&str]) -> (i32, serde_json::Value, String) {
        let mut args = vec!["scan", "--format", "json"];
        args.extend_from_slice(extra);
        let (code, out, err) = self.run(&args);
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

    // The repo tier sets the level of a plugin the user installed.
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
    s3.repo_config(
        "[slop.plugins.acme]\nseverity = \"error\"\n\n\
         [slop.rules.overrides]\n\"acme/vague-why\" = \"error\"\n",
    );
    s3.write("a.py", RULE_1);
    let (_, v, err) = s3.scan();
    assert!(
        !err.contains("ignored"),
        "raising is the repo's call: {err}"
    );
    assert!(
        v["findings"].as_array().is_some_and(|fs| fs
            .iter()
            .any(|f| f["rule"] == "acme/vague-why" && f["severity"] == "error")),
        "the repo tier raised it: {v}"
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
    // The plugin sleeps 5 s against a 300 ms budget; a ceiling at the
    // sleep itself still proves the scan did not wait it out, and holds
    // on a loaded machine where process start-up alone takes seconds.
    assert!(
        started.elapsed() < std::time::Duration::from_millis(4500),
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

/// A starved baseline pass must not turn inherited debt into "added". The
/// plugin answers its first `check` (the head pass) at once and hangs on
/// the second (the baseline pass over the base blobs), so the outcome does
/// not depend on how fast the machine is.
#[test]
fn a_cut_baseline_pass_never_charges_inherited_plugin_findings() {
    let s = Scratch::new("since-starved");
    let calls = s.root.join("calls.txt");
    let hanging_second_call = PLUGIN.replace(
        "    input=$(cat)\n",
        &format!(
            "    input=$(cat)\n    echo x >> {calls}\n    [ \"$(wc -l < {calls})\" -ge 2 ] && sleep 30\n",
            calls = calls.display()
        ),
    );
    assert_ne!(
        hanging_second_call, PLUGIN,
        "the probe must patch the plugin"
    );
    s.write_plugin("acme-slop", &hanging_second_call);
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\ntimeout_ms = 20000\n");
    s.write("a.py", RULE_1);
    s.commit("base");
    s.write("a.py", &format!("{RULE_1}y = 2\n"));

    let (_, v, err) = s.scan_args(&["--since", "HEAD", "--deadline-ms", "3000"]);
    let ran = std::fs::read_to_string(&calls).map_or(0, |c| c.lines().count());
    assert_eq!(
        ran, 2,
        "head and baseline passes both called check: {v} {err}"
    );
    assert!(
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "acme/vague-why")),
        "the head finding is still reported: {v}"
    );
    assert!(
        v["added"].as_array().is_some_and(Vec::is_empty),
        "an inherited finding whose baseline pass was cut is not added: {v}"
    );
    assert!(
        v["skipped"]
            .as_array()
            .is_some_and(|sk| sk.iter().any(|x| x["tool"] == "acme"
                && x["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("baseline-")))),
        "the cut baseline pass is surfaced in skipped: {v}"
    );
}

/// Only a deterministic comments plugin pays for the base-side pass: a
/// plugin that runs once under `--since` keeps the whole deadline.
#[test]
fn a_plugin_without_a_baseline_pass_keeps_the_whole_deadline_under_since() {
    let s = Scratch::new("since-nondet");
    let slow_nondet = PLUGIN
        .replace("\"deterministic\":true", "\"deterministic\":false")
        .replace("    input=$(cat)\n", "    input=$(cat)\n    sleep 1\n");
    assert_ne!(slow_nondet, PLUGIN, "the probe must patch the plugin");
    s.write_plugin("acme-slop", &slow_nondet);
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\ntimeout_ms = 20000\n");
    s.write("a.py", RULE_1);
    s.commit("base");
    s.write("a.py", &format!("{RULE_1}y = 2\n"));

    let (_, v, err) = s.scan_args(&["--since", "HEAD", "--deadline-ms", "1800"]);
    assert!(
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "acme/vague-why")),
        "a 1 s plugin fits a 1.8 s deadline when it has no base-side pass: {v} {err}"
    );
}

/// A host started with `SIGINT` ignored (`nohup`, `cmd &` in a script)
/// keeps ignoring it while a plugin call is in flight.
#[test]
fn an_inherited_sigint_ignore_survives_a_plugin_call() {
    let s = Scratch::new("sigign");
    let slow = PLUGIN.replace("    input=$(cat)\n", "    input=$(cat)\n    sleep 2\n");
    s.write_plugin("acme-slop", &slow);
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\ntimeout_ms = 20000\n");
    s.write("a.py", RULE_1);
    let script = format!(
        "trap '' INT; {bin} scan --format json --plugins acme --deadline-ms 20000 a.py >/dev/null 2>&1 & \
         pid=$!; sleep 0.7; kill -INT $pid; wait $pid; echo $?",
        bin = env!("CARGO_BIN_EXE_agent-slop"),
    );
    let out = Command::new("sh")
        .arg("-c")
        .arg(&script)
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
        .output()
        .expect("spawn sh");
    let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_ne!(
        code, "130",
        "SIGINT was ignored at start and must stay ignored"
    );
    assert!(code == "0" || code == "1", "host exit {code}");
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
    run_hook_with_env(s, payload, &[])
}

fn run_hook_with_env(s: &Scratch, payload: &str, extra: &[(&str, &str)]) -> (i32, String, String) {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
        .arg("hook")
        .envs(extra.iter().copied())
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

const HOOK_PAYLOAD: &str = r#"{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"a.py"},"session_id":"s1"}"#;

/// A comment plugin whose single rule is announced at `$LEVEL`.
fn leveled_plugin(name: &str, level: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  describe) echo '{{"protocol":1,"name":"{name}","version":"1","input":"comments","cost":"A","rules":[{{"id":"r","default_level":"{level}"}}]}}' ;;
  check)
    cat >/dev/null
    echo '{{"protocol":1,"findings":[{{"rule":"r","path":"a.py","comment":0,"message":"m"}}]}}' ;;
esac
"#
    )
}

/// Survivors of the r1b review: a plugin the host killed must not leave a
/// process tree behind (CWE-404). The plugin forks a long `sleep` whose pid
/// it records, then hangs; after the timeout that pid must be gone.
#[test]
fn a_timed_out_plugin_leaves_no_grandchildren() {
    let s = Scratch::new("orphans");
    let pidfile = s.root.join("grandchild.pid");
    s.write_plugin(
        "fork-slop",
        &format!(
            r#"#!/bin/sh
case "$1" in
  describe) echo '{{"protocol":1,"name":"fork","version":"1","input":"comments","cost":"A","rules":[{{"id":"x","default_level":"warning"}}]}}' ;;
  check) sleep 60 & echo $! > {pid}; sleep 60 ;;
esac
"#,
            pid = pidfile.display()
        ),
    );
    s.user_config("[slop.plugins.fork]\ncommand = [\"fork-slop\"]\ntimeout_ms = 300\n");
    s.write("a.py", RULE_1);
    let (_, v, _) = s.scan();
    assert!(
        v["skipped"]
            .as_array()
            .is_some_and(|sk| sk.iter().any(|x| x["reason"] == "timeout")),
        "{v}"
    );
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("the plugin recorded its grandchild")
        .trim()
        .parse()
        .expect("pid");
    // SIGKILL delivery and reaping by init are asynchronous.
    let mut alive = true;
    for _ in 0..40 {
        alive = Path::new(&format!("/proc/{pid}")).exists()
            && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|st| !st.contains(") Z "));
        if !alive {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if alive {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    }
    assert!(!alive, "grandchild {pid} survived the host's kill");
}

/// A manifest's `default_level` applies when no tier set a severity.
#[test]
fn the_manifest_default_level_applies_without_a_configured_severity() {
    for (declared, expect) in [
        ("hint", Some("hint")),
        ("warning", Some("warning")),
        ("off", None),
    ] {
        let s = Scratch::new(&format!("lvl-{declared}"));
        s.write_plugin("lvl-slop", &leveled_plugin("lvl", declared));
        s.user_config("[slop.plugins.lvl]\ncommand = [\"lvl-slop\"]\n");
        s.write("a.py", "# a comment\nx = 1\n");
        let (_, v, err) = s.scan();
        let got = v["findings"]
            .as_array()
            .and_then(|fs| fs.iter().find(|f| f["rule"] == "lvl/r"))
            .map(|f| f["severity"].as_str().unwrap_or("?").to_string());
        assert_eq!(got.as_deref(), expect, "declared {declared}: {v} {err}");
    }
    // A configured severity still wins over the manifest.
    let s = Scratch::new("lvl-cfg");
    s.write_plugin("lvl-slop", &leveled_plugin("lvl", "hint"));
    s.user_config("[slop.plugins.lvl]\ncommand = [\"lvl-slop\"]\nseverity = \"warning\"\n");
    s.write("a.py", "# a comment\nx = 1\n");
    let (_, v, _) = s.scan();
    assert!(
        v["findings"].as_array().is_some_and(|fs| fs
            .iter()
            .any(|f| f["rule"] == "lvl/r" && f["severity"] == "warning")),
        "{v}"
    );
}

/// The describe cache keys on every argv element that is a file: an
/// interpreter-launched script that gains a rule is not served its old
/// manifest.
#[test]
fn editing_an_interpreter_launched_script_invalidates_the_manifest_cache() {
    let s = Scratch::new("argv-cache");
    let script = s.bin.join("plugin.sh");
    let body = |rules: &str| {
        format!(
            r#"case "$1" in
  describe) echo '{{"protocol":1,"name":"scr","version":"1","input":"comments","cost":"A","rules":[{rules}]}}' ;;
  check)
    cat >/dev/null
    echo '{{"protocol":1,"findings":[{{"rule":"new","path":"a.py","comment":0,"message":"m"}}]}}' ;;
esac
"#
        )
    };
    std::fs::write(&script, body(r#"{"id":"old","default_level":"warning"}"#)).expect("v1");
    s.user_config(&format!(
        "[slop.plugins.scr]\ncommand = [\"sh\", \"{}\"]\n",
        script.display()
    ));
    s.write("a.py", "# a comment\nx = 1\n");
    let (_, v, _) = s.scan();
    assert!(
        !v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "scr/new")),
        "v1 does not declare `new`: {v}"
    );

    // Same length, newer mtime, a new rule: only the script path in argv
    // carries the change.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&script, body(r#"{"id":"new","default_level":"warning"}"#)).expect("v2");
    let (_, v, _) = s.scan();
    assert!(
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "scr/new")),
        "v2 declares `new`; a stale manifest dropped it: {v}"
    );
}

/// The plugin sees `stage = "check"` from `agent-slop check`, `scan` from
/// scan, and `hook` from the `PostToolUse` leg.
#[test]
fn the_stage_in_the_request_names_the_command() {
    let s = Scratch::new("stage");
    let seen = s.root.join("stage.txt");
    s.write_plugin(
        "stg-slop",
        &format!(
            r#"#!/bin/sh
case "$1" in
  describe) echo '{{"protocol":1,"name":"stg","version":"1","input":"comments","cost":"A","rules":[{{"id":"r","default_level":"warning"}}]}}' ;;
  check) input=$(cat); printf '%s\n' "$(printf '%s' "$input" | sed 's/.*"stage":"\([a-z]*\)".*/\1/')" >> {seen}; echo '{{"protocol":1,"findings":[]}}' ;;
esac
"#,
            seen = seen.display()
        ),
    );
    s.user_config("[slop.plugins.stg]\ncommand = [\"stg-slop\"]\n");
    s.write("a.py", "# a comment\nx = 1\n");
    let _ = s.run(&["scan", "--format", "json"]);
    let _ = s.run(&["check", "a.py"]);
    let _ = run_hook(&s, HOOK_PAYLOAD);
    let stages = std::fs::read_to_string(&seen).expect("the plugin ran");
    assert_eq!(
        stages.lines().collect::<Vec<_>>(),
        ["scan", "check", "hook"],
        "{stages}"
    );
}

/// `--deadline-ms` bounds the sum of plugin calls, not each one: three
/// slow plugins under a 1 s deadline may not cost three budgets.
#[test]
fn plugin_budgets_share_the_scans_deadline() {
    let s = Scratch::new("deadline");
    s.user_config(&three_slow_plugins(&s, 900));
    s.write("a.py", RULE_1);
    let started = std::time::Instant::now();
    let (_, v, _) = s.scan_args(&["--deadline-ms", "1200"]);
    let took = started.elapsed();
    // Three unshared budgets would cost 3 x 900 ms of sleeping plus start-up;
    // the shared deadline stops the second and third plugin from starting.
    // The ceiling sits below that sum, and the skip reasons carry the logic.
    assert!(
        took < std::time::Duration::from_millis(2700),
        "three 900 ms budgets under a 1200 ms deadline took {took:?}: {v}"
    );
    let skipped_for = |reason: &str| {
        v["skipped"]
            .as_array()
            .map_or(0, |sk| sk.iter().filter(|x| x["reason"] == reason).count())
    };
    assert!(
        skipped_for("deadline") >= 1 && skipped_for("timeout") <= 2,
        "at most two plugins may burn a budget before the deadline stops the rest: {v}"
    );
}

/// Installs `aa`, `bb` and `cc` plugins that hang in `check` and returns
/// the user config declaring them with `timeout_ms`.
fn three_slow_plugins(s: &Scratch, timeout_ms: u64) -> String {
    use std::fmt::Write;
    let mut config = String::new();
    for n in ["aa", "bb", "cc"] {
        s.write_plugin(&format!("{n}-slop"), &slow_plugin(n));
        let _ = write!(
            config,
            "[slop.plugins.{n}]\ncommand = [\"{n}-slop\"]\ntimeout_ms = {timeout_ms}\n\n"
        );
    }
    config
}

fn slow_plugin(name: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  describe) echo '{{"protocol":1,"name":"{name}","version":"1","input":"comments","cost":"A","rules":[{{"id":"x","default_level":"warning"}}]}}' ;;
  check) sleep 30 ;;
esac
"#
    )
}

/// The hook's 300 ms ceiling is for the whole leg: three slow cost-A
/// plugins may not add up to three ceilings on the model's path.
#[test]
fn the_hook_ceiling_covers_every_plugin_together() {
    let s = Scratch::new("hook-sum");
    s.user_config(&three_slow_plugins(&s, 2000));
    s.write("a.py", RULE_1);
    let started = std::time::Instant::now();
    let (code, _, err) = run_hook(&s, HOOK_PAYLOAD);
    let took = started.elapsed();
    assert_eq!(code, 0, "{err}");
    // Each plugin hangs for 30 s against a 2 s `timeout_ms`; three
    // independent budgets would be 6 s. The shared 300 ms ceiling means at
    // most one call can time out and the others never start.
    assert!(
        took < std::time::Duration::from_millis(5000),
        "three plugins under one 300 ms ceiling took {took:?}: {err}"
    );
    let timeouts = err.matches("skipped: timeout").count();
    let deadlines = err.matches("skipped: deadline").count();
    assert!(
        timeouts <= 1 && timeouts + deadlines == 3,
        "one shared ceiling: at most one timeout, the rest never start: {err}"
    );
}

/// Normalisation is bounded by the finding cap and the budget, not by
/// findings × file size: a plugin that floods thousands of findings over a
/// big file costs the host a bounded amount.
#[test]
fn a_flood_of_findings_is_truncated_before_normalising() {
    let s = Scratch::new("flood");
    s.write_plugin(
        "flood-slop",
        r#"#!/bin/sh
case "$1" in
  describe) echo '{"protocol":1,"name":"flood","version":"1","input":"comments","cost":"A","rules":[{"id":"r","default_level":"warning"}]}' ;;
  check)
    cat >/dev/null
    printf '{"protocol":1,"findings":['
    i=0
    while [ $i -lt 20000 ]; do
      [ $i -gt 0 ] && printf ','
      printf '{"rule":"r","path":"a.py","comment":%s,"message":"m"}' $((i % 300))
      i=$((i+1))
    done
    printf ']}'
    ;;
esac
"#,
    );
    s.user_config("[slop.plugins.flood]\ncommand = [\"flood-slop\"]\ntimeout_ms = 5000\n");
    let big = "# a comment\nx = 1\n".repeat(300);
    s.write("a.py", &big);
    let started = std::time::Instant::now();
    let (_, v, _) = s.scan_args(&["--deadline-ms", "8000"]);
    let took = started.elapsed();
    assert!(
        v["skipped"]
            .as_array()
            .is_some_and(|sk| sk.iter().any(|x| x["tool"] == "flood"
                && x["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("truncated at 5000")))),
        "{v}"
    );
    let n = v["findings"]
        .as_array()
        .map_or(0, |fs| fs.iter().filter(|f| f["rule"] == "flood/r").count());
    assert!(n <= 5000, "{n} findings kept");
    // The flood script alone is thousands of shell iterations; the bound
    // only has to separate "capped" from "normalised without end", so it
    // is generous and the logical cap assertions above carry the proof.
    assert!(
        took < std::time::Duration::from_secs(30),
        "normalising a capped flood took {took:?}"
    );
}

/// The scrubbed environment: a parent secret never reaches the plugin, a
/// user-tier `env_passthrough` does, and a repo-tier one is refused.
#[test]
fn the_plugin_environment_is_scrubbed_and_passthrough_is_user_only() {
    let s = Scratch::new("env");
    let dump = s.root.join("env.txt");
    s.write_plugin(
        "env-slop",
        &format!(
            r#"#!/bin/sh
case "$1" in
  describe) echo '{{"protocol":1,"name":"envp","version":"1","input":"comments","cost":"A","rules":[{{"id":"r","default_level":"warning"}}]}}' ;;
  check) cat >/dev/null; env > {dump}; echo '{{"protocol":1,"findings":[]}}' ;;
esac
"#,
            dump = dump.display()
        ),
    );
    s.user_config(
        "[slop.plugins.envp]\ncommand = [\"env-slop\"]\nenv_passthrough = [\"USER_PASSED\"]\n",
    );
    s.repo_config("[slop.plugins.envp]\nenv_passthrough = [\"REPO_WANTED\"]\n");
    s.write("a.py", "# a comment\nx = 1\n");
    let extra = [
        ("PARENT_SECRET", "hunter2"),
        ("USER_PASSED", "yes"),
        ("REPO_WANTED", "leaked"),
    ];
    let (_, out, err) = s.run_with_env(&["scan", "--format", "json"], &extra);
    let env = std::fs::read_to_string(&dump).unwrap_or_else(|e| panic!("{e}: {out} {err}"));
    assert!(
        !env.contains("PARENT_SECRET"),
        "the parent env leaked:\n{env}"
    );
    assert!(
        env.contains("USER_PASSED=yes"),
        "a user-tier passthrough is forwarded:\n{env}"
    );
    assert!(
        !env.contains("REPO_WANTED"),
        "a repo-tier passthrough must be refused:\n{env}"
    );
    assert!(
        err.contains("env_passthrough ignored: only the user tier may set it"),
        "{err}"
    );
    assert!(
        env.contains("PATH="),
        "the allowlist still holds PATH:\n{env}"
    );
}

/// A manifest whose `name` differs from the configured key is skipped
/// before it can run.
#[test]
fn a_manifest_name_that_differs_from_the_key_is_skipped() {
    let s = Scratch::new("name");
    s.write_plugin("liar-slop", &leveled_plugin("someone-else", "warning"));
    s.user_config("[slop.plugins.liar]\ncommand = [\"liar-slop\"]\n");
    s.write("a.py", "# a comment\nx = 1\n");
    let (_, v, _) = s.scan();
    assert!(
        v["skipped"]
            .as_array()
            .is_some_and(|sk| sk.iter().any(|x| x["tool"] == "liar"
                && x["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("name-mismatch")))),
        "{v}"
    );
    assert!(
        v["findings"]
            .as_array()
            .is_none_or(|fs| fs.iter().all(|f| f["source"]["tool"] != "liar")),
        "a mismatched plugin must not run: {v}"
    );
}

fn cost_plugin(name: &str, cost: &str) -> String {
    leveled_plugin(name, "warning").replace(r#""cost":"A""#, &format!(r#""cost":"{cost}""#))
}

/// Cost classes: the hook runs class A only; `scan` runs A–C and D only
/// when the plugin is named.
#[test]
fn cost_classes_gate_the_stage() {
    let s = Scratch::new("cost");
    s.write_plugin("bee-slop", &cost_plugin("bee", "B"));
    s.write_plugin("dee-slop", &cost_plugin("dee", "D"));
    s.user_config(
        "[slop.plugins.bee]\ncommand = [\"bee-slop\"]\n\n[slop.plugins.dee]\ncommand = [\"dee-slop\"]\n",
    );
    s.write("a.py", "# a comment\nx = 1\n");

    let (_, out, err) = run_hook(&s, HOOK_PAYLOAD);
    assert!(
        !out.contains("bee/r"),
        "a class B plugin ran in a hook: {out}"
    );
    assert!(err.contains("plugin bee skipped: cost"), "{err}");

    let (_, v, _) = s.scan();
    let has = |v: &serde_json::Value, rule: &str| {
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == rule))
    };
    assert!(has(&v, "bee/r"), "class B runs in scan: {v}");
    assert!(!has(&v, "dee/r"), "class D needs naming: {v}");
    let (_, v, _) = s.scan_args(&["--plugins", "dee"]);
    assert!(has(&v, "dee/r"), "a named class D plugin runs: {v}");
}

/// Only a deterministic comment plugin can be attributed to a baseline: a
/// nondeterministic one's NEW finding is reported but never "added".
#[test]
fn a_nondeterministic_plugin_is_never_attributed() {
    let s = Scratch::new("nondet");
    s.write_plugin(
        "acme-slop",
        &PLUGIN.replace(r#""deterministic":true,"#, r#""deterministic":false,"#),
    );
    s.user_config("[slop.plugins.acme]\ncommand = [\"acme-slop\"]\n");
    s.write("a.py", "x = 1\n");
    s.commit("base");
    s.write("b.py", RULE_1);
    let (_, v, _) = s.scan_args(&["--since", "HEAD"]);
    assert!(
        v["findings"]
            .as_array()
            .is_some_and(|fs| fs.iter().any(|f| f["rule"] == "acme/vague-why")),
        "reported: {v}"
    );
    assert!(
        v["added"].as_array().is_some_and(|a| a.iter().all(|fp| {
            v["findings"].as_array().is_some_and(|fs| {
                fs.iter()
                    .filter(|f| f["fingerprint"] == *fp)
                    .all(|f| f["rule"] != "acme/vague-why")
            })
        })),
        "a nondeterministic plugin finding must not count as added: {v}"
    );
}
