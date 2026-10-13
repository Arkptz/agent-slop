//! The Laya second verdict end to end, through the built binary's
//! PostToolUse leg: a stub endpoint answers, the envelope carries the
//! verdict under its own finding, and the finding line is byte-identical to
//! the run with the judge off.
//!
//! The three properties that make the feature worth having are each
//! asserted here rather than in prose:
//!
//! * the annotation arrives (a stub that answers and a leg that drops it
//!   fails this file);
//! * it is additive (the finding line is compared byte for byte against the
//!   judge-off run, so a verdict can never have moved a severity or a
//!   message);
//! * the confidence floor is load-bearing (a stub that answers below it
//!   must produce no line at all, so a future "always print" regression
//!   fails here instead of shipping a classifier's noise into every edit).
//!
//! The model itself is not covered: its accuracy is measured offline on
//! held-out rows, and a test that asserted which way it votes would be a
//! test of a checkpoint, not of the gate.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Fires `slop/comment-history` at `error` (the built-in level) and is
/// the same fixture string the engine's own tests use: a Python comment
/// (the extractor reads `#` in a `.py` file, not `//`) whose wording the
/// rule matches. A fixture that does not fire the rule would make every
/// assertion below pass for the wrong reason.
const SLOP: &str = "# We previously always sent a Referer \u{2014} that was a bug.\n";

struct HookRun {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Scratch {
    root: PathBuf,
    repo: PathBuf,
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
            "git is absent: these scenarios cannot run (POL-026)"
        );
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "agent-slop-judge-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let s = Scratch {
            repo: root.join("repo"),
            cache: root.join("cache"),
            root,
        };
        std::fs::create_dir_all(&s.repo).expect("repo dir");
        std::fs::create_dir_all(&s.cache).expect("cache dir");
        s.git(&["init", "--quiet", "--initial-branch=main"]);
        s.git(&["config", "user.name", "agent-slop-judge"]);
        s.git(&["config", "user.email", "judge@example.invalid"]);
        // The base is clean and the fixture lands after it, uncommitted: the
        // leg only speaks about what this session added, so a fixture that
        // is already in the baseline would prove nothing. Both files are
        // tracked at the base because `git diff` cannot see an untracked
        // path, and the needs axis reads that diff.
        s.write("a.py", "referer = None\n");
        s.write("b.py", "def f():\n    pass\n");
        // Tracked at the base, like the others: `git diff` cannot see an
        // untracked path, and the needs axis reads that diff.
        s.write("c.py", "def lease(path):\n    return open(path)\n");
        s.commit("base");
        s
    }

    /// Pin the session baseline. Without it the ratchet has no way to tell
    /// what this session added, and the leg stays silent — a green test that
    /// proved nothing.
    fn pin(&self) {
        let run = self.hook(
            r#"{"hook_event_name":"SessionStart","session_id":"judge-session"}"#,
            "",
        );
        assert_eq!(run.code, 0, "SessionStart: {}", run.stderr);
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

    fn hook(&self, payload: &str, extra_toml: &str) -> HookRun {
        if !extra_toml.is_empty() {
            self.write(".agent-hooks.toml", extra_toml);
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .arg("hook")
            .current_dir(&self.repo)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GROK_SESSION_ID")
            .env("AGENT_SLOP_TELEMETRY_ROOT", self.root.join("reviews"))
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

/// The `additionalContext` the leg handed the model, as lines. The envelope
/// is one JSON string, so the newlines are two-byte escapes until it is
/// unescaped; comparing raw stdout would hide a reordering.
fn advice(run: &HookRun) -> String {
    let envelope: serde_json::Value = serde_json::from_str(&run.stdout).expect("envelope json");
    envelope["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn payload(file: &str) -> String {
    format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{file}"}},"session_id":"s1","stop_hook_active":false}}"#
    )
}

/// A one-shot HTTP stub for `POST /v1/systemone`. It echoes the criteria the
/// request actually sent, which is the only faithful shape for a stub: the
/// client looks each option up by its `"key: description"` spelling, so a
/// stub that answered with fixed keys would be asserting against a contract
/// the real server does not have.
fn stub(dist: Vec<f64>) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    // The thread holds a clone and bumps it per request; the caller keeps the
    // original so it can read the count while the stub is still listening.
    let served = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&served);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 16384];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let want: usize = head
                .split("Content-Length:")
                .nth(1)
                .and_then(|h| h.split_whitespace().next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = head.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
            while body.len() < want {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => body.push_str(&String::from_utf8_lossy(&buf[..k])),
                }
            }
            let request: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
            let criteria = request["questions"]["q"]["criteria"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut probabilities = serde_json::Map::new();
            for (i, c) in criteria.iter().enumerate() {
                let p = dist.get(i).copied().unwrap_or(0.0);
                probabilities.insert(c.as_str().unwrap_or_default().to_string(), serde_json::json!(p));
            }
            let answer = serde_json::json!({
                "model": "laya-stub",
                "answers": {"q": {
                    "type": "choice",
                    "choice": criteria.first().cloned().unwrap_or_default(),
                    "probabilities": serde_json::Value::Object(probabilities),
                }},
                "usage": {},
            });
            let body = answer.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (port, served)
}

/// delete .93 / rewrite .04 / keep .03
const VERDICT_DIST: [f64; 3] = [0.93, 0.04, 0.03];
/// delete .55 — below the floor in every scenario that uses it.
const HESITANT_DIST: [f64; 3] = [0.55, 0.2, 0.25];
/// yes .91 / no .09
const NEEDS_DIST: [f64; 2] = [0.91, 0.09];
/// no .96 / yes .04 — the model read the code and found nothing to say.
const NEEDS_NO_DIST: [f64; 2] = [0.04, 0.96];

fn config_for(port: u16, floor: f64) -> String {
    format!(
        "[slop]\nmode = \"block\"\n\n[slop.judge]\nenable = true\nurl = \"http://127.0.0.1:{port}/v1/systemone\"\nmin_confidence = {floor}\nneeds_min_p = {floor}\nmax_verdicts = 4\ntimeout_ms = 3000\n"
    )
}

/// The annotation reaches the model, under the finding it belongs to, and
/// leaves the finding untouched.
#[test]
fn a_verdict_annotates_the_finding_without_changing_it() {
    let s = Scratch::new("annotate");
    s.pin();
    s.write("a.py", SLOP);
    let (port, served) = stub(VERDICT_DIST.to_vec());

    let without = s.hook(&payload("a.py"), "[slop]\nmode = \"block\"\n");
    let with = s.hook(&payload("a.py"), &config_for(port, 0.9));

    assert_eq!(with.code, 0, "stderr: {}", with.stderr);
    assert!(with.stdout.contains("laya: delete 0.93"), "stdout: {}", with.stdout);
    assert!(
        with.stdout.contains("(delete 0.93 / rewrite 0.04 / keep 0.03)"),
        "the full distribution belongs in the line: {}",
        with.stdout
    );
    // Byte-equal finding lines are what "additive" means here. A verdict that
    // quietly rewrote a severity or a message would still produce a
    // plausible envelope, and only this comparison catches it.
    let finding_line = |run: &HookRun| {
        advice(run)
            .lines()
            .find(|l| l.contains("slop/comment-history"))
            .unwrap_or_else(|| panic!("no finding line in {}", run.stdout))
            .to_string()
    };
    assert_eq!(finding_line(&with), finding_line(&without));
    assert_ne!(with.stdout, without.stdout);
    // One request for the one comment the rule matched. A judge that quietly
    // skipped asking, or asked per token, would show up here.
    assert_eq!(served.load(Ordering::SeqCst), 1, "the judge asked once");
}

/// Below the floor the model has not said anything worth printing, and the
/// leg must look exactly as it does with the model off.
#[test]
fn a_verdict_below_the_floor_prints_nothing() {
    let s = Scratch::new("floor");
    s.pin();
    s.write("a.py", SLOP);
    let (port, _served) = stub(HESITANT_DIST.to_vec());
    let without = s.hook(&payload("a.py"), "[slop]\nmode = \"block\"\n");
    let with = s.hook(&payload("a.py"), &config_for(port, 0.9));
    assert!(!with.stdout.contains("laya:"), "stdout: {}", with.stdout);
    assert_eq!(with.stdout, without.stdout);
}

/// A model that is not running is a normal state: no line, no error, no
/// nonzero exit, and the same stdout the judge-off run produces.
#[test]
fn a_stopped_model_changes_nothing() {
    let s = Scratch::new("stopped");
    s.pin();
    s.write("a.py", SLOP);
    let without = s.hook(&payload("a.py"), "[slop]\nmode = \"block\"\n");
    // Port 1 is reserved and nothing listens there.
    let with = s.hook(
        &payload("a.py"),
        "[slop]\nmode = \"block\"\n\n[slop.judge]\nenable = true\nurl = \"http://127.0.0.1:1/v1/systemone\"\ntimeout_ms = 500\n",
    );
    assert_eq!(with.code, 0, "stderr: {}", with.stderr);
    assert_eq!(with.stdout, without.stdout);
    assert!(!with.stderr.contains("laya"), "stderr: {}", with.stderr);
}

/// A 503 is what the gateway answers when the model behind it is stopped,
/// which is the failure this leg is built around.
#[test]
fn a_503_is_absorbed() {
    let s = Scratch::new("unavailable");
    s.pin();
    s.write("a.py", SLOP);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = r#"{"detail":"model is not loaded"}"#;
            let response = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let without = s.hook(&payload("a.py"), "[slop]\nmode = \"block\"\n");
    let with = s.hook(&payload("a.py"), &config_for(port, 0.9));
    assert_eq!(with.code, 0, "stderr: {}", with.stderr);
    assert_eq!(with.stdout, without.stdout);
}

/// The second axis fires with no rule behind it: added code that reads as
/// if it were hiding a why gets an advisory line of its own.
#[test]
fn added_code_can_be_asked_about_a_missing_comment() {
    let s = Scratch::new("needs");
    s.pin();
    let (port, _served) = stub(NEEDS_DIST.to_vec());
    s.write(
        "b.py",
        "def f():\n    x = 1\n    y = 2\n    z = 3\n    return x + y + z\n",
    );
    let run = s.hook(&payload("b.py"), &config_for(port, 0.9));
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains("laya/needs-comment"),
        "an advisory needs line should appear for a comment-free run of added code: {}",
        run.stdout
    );
    assert!(run.stdout.contains("laya: yes 0.91"), "stdout: {}", run.stdout);
}

/// A confident NO is the model saying the code reads plainly. Printing it
/// under a header that claims the code may be missing a comment inverts the
/// answer while looking more confident than ever, which is the worst way for
/// an annotator to be wrong.
#[test]
fn a_confident_no_is_not_a_needs_hit() {
    let s = Scratch::new("needs-no");
    s.pin();
    let (port, _served) = stub(NEEDS_NO_DIST.to_vec());
    s.write(
        "b.py",
        "def f():\n    x = 1\n    y = 2\n    z = 3\n    return x + y + z\n",
    );
    let run = s.hook(&payload("b.py"), &config_for(port, 0.9));
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        !run.stdout.contains("laya/needs-comment"),
        "a no must not be announced as a missing comment: {}",
        run.stdout
    );
}

/// `--max-verdicts` exists because the per-hook-leg cap of four would
/// truncate a whole-repo pass at four comments and still look finished. Zero
/// means every eligible comment; read as a literal cap it means none, and
/// the run prints its findings with no verdict at all — indistinguishable
/// from a model that had nothing to say.
#[test]
fn the_scan_cap_decides_how_many_comments_are_asked() {
    let s = Scratch::new("cap");
    s.pin();
    let (port, served) = stub(VERDICT_DIST.to_vec());
    // Three comment-history findings, one comment each.
    s.write(
        "a.py",
        "# We previously always sent a Referer — that was a bug.\n# This used to be a hard-coded host.\n# It was renamed from fallback_url.\n",
    );
    let cfg = config_for(port, 0.0);
    s.write(".agent-hooks.toml", &cfg);

    let run = |cap: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .args(["scan", "--linters", "none", "--format", "human"])
            .args(["--max-verdicts", cap])
            .args(["--deadline-ms", "30000"])
            .arg("a.py")
            .current_dir(&s.repo)
            .env("XDG_CACHE_HOME", &s.cache)
            .env("HOME", &s.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("AGENT_SLOP_TELEMETRY_ROOT", s.root.join("reviews"))
            .output()
            .expect("run scan");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let all = run("0");
    let all_n = served.swap(0, Ordering::SeqCst);
    assert!(all_n >= 3, "0 means every comment, asked {all_n}\nscan output:\n{all}");
    assert_eq!(all.matches("laya: ").count(), all_n, "one line per answer");

    let one = run("1");
    assert_eq!(served.swap(0, Ordering::SeqCst), 1, "1 means one comment");
    assert_eq!(one.matches("laya: ").count(), 1);
}

/// The needs half has to be reachable from the audit command, not only from
/// the PostToolUse leg. On a branch whose commits are mostly non-comment
/// code the comment axis has nothing to say — the comment findings are zero,
/// so a verdict run returns an empty audit that looks like a broken feature.
/// The needs axis needs no rule to fire, which is exactly why it is the one
/// that can still answer on such a diff.
#[test]
fn the_audit_command_reaches_the_needs_axis() {
    let s = Scratch::new("audit-needs");
    s.pin();
    let (port, served) = stub(NEEDS_DIST.to_vec());
    s.write(".agent-hooks.toml", &config_for(port, 0.9));
    // A comment-free run of added lines: the shape the needs axis is trained
    // on, and one no rule will ever produce a finding for.
    s.write(
        "c.py",
        "def lease(path, retries):\n    handle = open(path)\n    deadline = 4321\n    return handle, deadline\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
        .args(["scan", "--linters", "none", "--format", "human", "--since", "HEAD"])
        .args(["--max-verdicts", "0", "--deadline-ms", "30000"])
        .current_dir(&s.repo)
        .env("XDG_CACHE_HOME", &s.cache)
        .env("HOME", &s.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("AGENT_SLOP_TELEMETRY_ROOT", s.root.join("reviews"))
        .output()
        .expect("run scan");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(served.load(Ordering::SeqCst), 1, "the audit asked once: {text}");
    assert!(
        text.contains("laya/needs-comment") && text.contains("laya: yes 0.91"),
        "an audit over code with no findings still has to produce the needs axis: {text}"
    );
}
