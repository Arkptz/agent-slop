//! End-to-end telemetry cover: the built binary's own hook legs against a
//! scratch git repository, and the row put through the `SessionEnd` rewrite
//! filter that the shipped recorder actually runs.
//!
//! The unit tests in `src/telemetry.rs` assert the row's shape. What only
//! an end-to-end run can show is that a row is written at all, by the real
//! process, and that the consumer does not delete it again: the recorder's
//! `SessionEnd` leg rewrites the shard keyed on `harness × session_id`, and
//! before the carve-out landed that rewrite dropped every row this
//! producer wrote (SPEC-006, "Telemetry row").

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Fires `slop/comment-history` at `error` — the fixture the engine's own
/// tests use.
const SLOP: &str = "# We previously always sent a Referer — that was a bug.\n";
const CLEAN: &str = "referer = None\n";

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
    /// A repository in `block` mode: the built-in default is `observe`,
    /// which never exits 2, so a test of the blocking path that forgot
    /// this would pass while proving nothing.
    fn new(name: &str) -> Scratch {
        assert!(
            Command::new("git")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success()),
            "git is absent: this telemetry cover cannot run, and a silent \
             pass would hide a producer that never writes (POL-026)"
        );
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let root = std::env::temp_dir().join(format!(
            "agent-slop-tel-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        // Untracked paths are scanned, so the shard must stay out of the
        // worktree or every scratch run turns into a full scan.
        let s = Scratch {
            repo: root.join("repo"),
            reviews: root.join("reviews"),
            cache: root.join("cache"),
            root,
        };
        std::fs::create_dir_all(&s.repo).expect("repo dir");
        std::fs::create_dir_all(&s.cache).expect("cache dir");
        s.git(&["init", "--quiet", "--initial-branch=main"]);
        s.git(&["config", "user.name", "agent-slop-tel"]);
        s.git(&["config", "user.email", "tel@example.invalid"]);
        s.write(".agent-hooks.toml", "[slop]\nmode = \"block\"\n");
        s.write("a.py", CLEAN);
        s.git(&["add", "-A"]);
        s.git(&["commit", "--quiet", "-m", "base"]);
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

    /// One real `agent-slop hook` run: stdin is the payload, the
    /// environment is hermetic, and the exit code is the hook's own.
    fn hook(&self, payload: &str) -> i32 {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-slop"))
            .arg("hook")
            .current_dir(&self.repo)
            .env("AGENT_SLOP_TELEMETRY_ROOT", &self.reviews)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GROK_SESSION_ID")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn agent-slop");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write payload");
        child
            .wait_with_output()
            .expect("wait")
            .status
            .code()
            .unwrap_or(-1)
    }

    /// Every row written under the scratch reviews root, in file order.
    fn rows(&self) -> Vec<serde_json::Value> {
        let mut files = Vec::new();
        collect(&self.reviews, &mut files);
        files.sort();
        files
            .iter()
            .flat_map(|p| {
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .map(|line| serde_json::from_str(&line).expect("each row is JSON"))
            .collect()
    }

    fn shard(&self) -> PathBuf {
        let mut files = Vec::new();
        collect(&self.reviews, &mut files);
        assert_eq!(files.len(), 1, "one day, one shard: {files:?}");
        files.remove(0)
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn items(row: &serde_json::Value) -> Vec<String> {
    row["defects"]
        .as_array()
        .map(|d| {
            d.iter()
                .map(|x| x["item"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_blocking_stop_writes_one_row_that_records_the_block() {
    let s = Scratch::new("blocked");
    let sid = "tel-blocked";
    assert_eq!(
        s.hook(&format!(
            r#"{{"hook_event_name":"SessionStart","session_id":"{sid}"}}"#
        )),
        0
    );
    s.write("a.py", &format!("{SLOP}{CLEAN}"));
    assert_eq!(
        s.hook(&format!(
            r#"{{"hook_event_name":"Stop","session_id":"{sid}"}}"#
        )),
        2,
        "a newly added error must block under mode = block"
    );

    let rows = s.rows();
    assert_eq!(
        rows.len(),
        1,
        "SessionStart writes nothing; Stop writes once"
    );
    let row = &rows[0];
    assert_eq!(row["record_kind"], "slop_scan");
    assert_eq!(row["schema"], 2);
    assert_eq!(row["event"], "Stop");
    assert_eq!(row["session_id"], sid);
    assert_eq!(row["project"], "repo");
    assert_eq!(row["telemetry"]["blocked"], true);
    assert_eq!(row["telemetry"]["would_block"], true);
    assert_eq!(row["telemetry"]["mode"], "block");
    assert!(row["telemetry"]["files_scanned"].as_u64().unwrap_or(0) >= 1);
    let items = items(row);
    assert!(
        items.contains(&"slop/comment-history".to_string()),
        "{items:?}"
    );
    assert!(
        items.contains(&"slop/gate-blocked".to_string()),
        "{items:?}"
    );
}

#[test]
fn a_clean_stop_still_writes_a_row() {
    let s = Scratch::new("clean");
    let sid = "tel-clean";
    assert_eq!(
        s.hook(&format!(
            r#"{{"hook_event_name":"SessionStart","session_id":"{sid}"}}"#
        )),
        0
    );
    assert_eq!(
        s.hook(&format!(
            r#"{{"hook_event_name":"Stop","session_id":"{sid}"}}"#
        )),
        0
    );

    let rows = s.rows();
    assert_eq!(
        rows.len(),
        1,
        "silence is not evidence: a clean turn records too"
    );
    assert_eq!(rows[0]["telemetry"]["blocked"], false);
    assert_eq!(rows[0]["telemetry"]["would_block"], false);
    assert_eq!(items(&rows[0]), Vec::<String>::new());
}

/// SPEC-006 scenario "A Stop re-entry does not re-block": no scan, and
/// therefore no row — a re-entry counted as a turn over-counts every rate
/// TASK-019 measures.
#[test]
fn a_stop_re_entry_writes_nothing() {
    let s = Scratch::new("reentry");
    s.write("a.py", &format!("{SLOP}{CLEAN}"));
    assert_eq!(
        s.hook(r#"{"hook_event_name":"Stop","session_id":"x","stop_hook_active":true}"#),
        0
    );
    assert!(s.rows().is_empty(), "precondition 1 must not record a turn");
}

#[test]
fn post_tool_use_records_the_file_it_advised_on() {
    let s = Scratch::new("ptu");
    s.write("a.py", &format!("{SLOP}{CLEAN}"));
    assert_eq!(
        s.hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"a.py"},"session_id":"p1"}"#),
        0,
        "the advisory leg never blocks"
    );
    let rows = s.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "PostToolUse");
    assert_eq!(rows[0]["telemetry"]["files_scanned"], 1);
    assert!(items(&rows[0]).contains(&"slop/comment-history".to_string()));
}

/// The exact jq program the recorder runs at `SessionEnd`, read out of the
/// shipped script rather than restated here — a restatement would pass
/// while the script that actually runs deletes the rows.
fn carve_out_filter() -> String {
    let script = feedback_record_path();
    let text =
        std::fs::read_to_string(&script).unwrap_or_else(|e| panic!("{}: {e}", script.display()));
    let line = text
        .lines()
        .find(|l| l.contains("jq -c") && l.contains("record_kind"))
        .unwrap_or_else(|| panic!("{}: no SessionEnd rewrite filter found", script.display()));
    let start = line.find('\'').expect("the jq program is single-quoted");
    let end = line.rfind('\'').expect("the jq program is single-quoted");
    line[start + 1..end].to_string()
}

/// The recorder belongs to the agent-feedback consumer, not to this
/// repository, so the compatibility check runs only when its path is
/// supplied. Once requested, absence is a failure, never a skip.
fn feedback_record_path() -> PathBuf {
    std::env::var_os("AGENT_SLOP_FEEDBACK_RECORD")
        .map(PathBuf::from)
        .expect("set AGENT_SLOP_FEEDBACK_RECORD to the consumer's feedback-record.sh")
}

fn jq(program: &str, harness: &str, session: &str, file: &Path) -> Vec<serde_json::Value> {
    let out = Command::new("jq")
        .args([
            "-c", "--arg", "h", harness, "--arg", "s", session, program, "--",
        ])
        .arg(file)
        .output()
        .unwrap_or_else(|e| panic!("jq is required for this test: {e}"));
    assert!(
        out.status.success(),
        "jq: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("jq emits JSON"))
        .collect()
}

/// The row must survive the rewrite that deletes the session's other
/// records, and the same test must show the filter can delete it: with
/// the `slop_scan` clause removed the row is gone, which is exactly the
/// state this producer shipped into before the carve-out edit landed.
#[test]
#[ignore = "agent-feedback consumer check: needs AGENT_SLOP_FEEDBACK_RECORD; run with --ignored"]
fn the_session_end_rewrite_keeps_the_row_and_would_drop_it_without_the_carve_out() {
    let s = Scratch::new("carveout");
    let sid = "tel-carve";
    assert_eq!(
        s.hook(&format!(
            r#"{{"hook_event_name":"SessionStart","session_id":"{sid}"}}"#
        )),
        0
    );
    s.write("a.py", &format!("{SLOP}{CLEAN}"));
    assert_eq!(
        s.hook(&format!(
            r#"{{"hook_event_name":"Stop","session_id":"{sid}"}}"#
        )),
        2
    );

    let shard = s.shard();
    // The recorder's own record for the same session: what the rewrite
    // exists to replace.
    let recorder = serde_json::json!({
        "schema": 2, "harness": "claude", "session_id": sid,
        "telemetry": {"source": "hook-payload"},
    });
    let mut body = std::fs::read_to_string(&shard).expect("shard");
    body.push_str(&serde_json::to_string(&recorder).expect("json"));
    body.push('\n');
    std::fs::write(&shard, &body).expect("rewrite shard");

    let filter = carve_out_filter();
    let kept = jq(&filter, "claude", sid, &shard);
    assert_eq!(
        kept.len(),
        1,
        "the slop row survives, the recorder record does not: {kept:?}"
    );
    assert_eq!(kept[0]["record_kind"], "slop_scan");

    let without = filter.replace(r#" or .record_kind == "slop_scan""#, "");
    assert_ne!(
        without, filter,
        "the carve-out clause must be present verbatim in the script"
    );
    let kept = jq(&without, "claude", sid, &shard);
    assert!(
        kept.is_empty(),
        "without the clause the row must be deleted: {kept:?}"
    );
}
