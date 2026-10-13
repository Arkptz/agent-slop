//! End-to-end ratchet cover through the shipped library surface: the
//! SPEC-006 situation-table rows the unit tests inside `src/ratchet.rs`
//! do not reach — detached HEAD, gitignored paths, binary/symlink/
//! submodule skips, the content-addressed cache behind a real
//! `$XDG_CACHE_HOME`, the session pin through the real filesystem (the
//! unit tests deliberately bypass it), the TTL sweep, and `--since`
//! merge-base charging. Engine modules are pulled in via `#[path]`
//! exactly as `tests/rule_fixtures_main.rs` does; no engine code is
//! copied.

#[path = "../src/config.rs"]
pub mod config;
#[path = "../src/finding.rs"]
pub mod finding;
#[path = "../src/lang.rs"]
pub mod lang;
#[path = "../src/ratchet.rs"]
pub mod ratchet;
#[path = "../src/rules/mod.rs"]
pub mod rules;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use config::{builtin_config, Mode, ResolvedConfig};

/// Fires `slop/comment-history` at `error` — the fixture string the
/// engine's own unit tests use.
const SLOP: &str = "# We previously always sent a Referer — that was a bug.\n";
const CLEAN: &str = "referer = None\n";
/// Same family, different normalised payload: a second fingerprint.
const SLOP2: &str = "# We previously always sent a User-Agent — that was a bug.\n";

/// `evaluate` reads `$XDG_CACHE_HOME` on every call, so exactly one
/// scratch repository may exist at a time: the env var is held under a
/// lock for the Scratch's lifetime and restored on drop. Nothing here
/// ever touches the user's real cache.
static CACHE_LOCK: Mutex<()> = Mutex::new(());

struct Scratch {
    root: PathBuf,
    dir: PathBuf,
    cache: PathBuf,
    prev_xdg: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        match &self.prev_xdg {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
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
            "git is absent: these ratchet tests cannot run, and a silent \
             pass would hide a gate that never fires (POL-026)"
        );
        let _guard = CACHE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "agent-slop-it-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        // The cache dir must sit OUTSIDE the worktree: everything under
        // the repo root that is untracked enters the changed set.
        let dir = root.join("repo");
        let cache = root.join("cache");
        std::fs::create_dir_all(&dir).expect("scratch repo dir");
        std::fs::create_dir_all(&cache).expect("scratch cache dir");
        let prev_xdg = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &cache);
        let s = Scratch {
            root,
            dir,
            cache,
            prev_xdg,
            _guard,
        };
        s.git(&["init", "--quiet", "--initial-branch=main"]);
        s.git(&["config", "user.name", "agent-slop-it"]);
        s.git(&["config", "user.email", "it@example.invalid"]);
        s
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_at(&self.dir, args)
    }

    fn git_at(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(path, content).expect("write");
    }

    fn write_bytes(&self, rel: &str, bytes: &[u8]) {
        std::fs::write(self.dir.join(rel), bytes).expect("write bytes");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }

    fn repo(&self) -> ratchet::Repo {
        ratchet::Repo::discover(&self.dir).expect("scratch is a repo")
    }

    fn pin(&self, session_id: &str) -> ratchet::SessionBaseline {
        ratchet::pin_session(&builtin_config(), &self.repo(), session_id, None)
            .expect("pinning HEAD")
    }

    /// The Stop leg's library path: pinned state read back from the real
    /// cache directory, ratchet evaluated with the cache enabled.
    fn stop(&self, session_id: &str) -> ratchet::GateOutcome {
        self.stop_with(&builtin_config(), session_id)
    }

    fn stop_with(&self, cfg: &ResolvedConfig, session_id: &str) -> ratchet::GateOutcome {
        ratchet::evaluate(
            cfg,
            &self.repo(),
            Some(session_id),
            None,
            &ratchet::Cache::open(true),
            None,
        )
        .expect("git present and scratch is a repository")
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.cache
            .join("agent-slop")
            .join("session")
            .join(format!("{session_id}.json"))
    }

    fn cache_entries(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.cache.join("agent-slop").join("v1"))
            .expect("cache dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect()
    }
}

/// Situation row "detached HEAD": the pinned `<commit>:<path>` resolves
/// and the gate still blocks.
#[test]
fn detached_head_resolves_the_pinned_commit_and_blocks() {
    let s = Scratch::new("detached");
    s.write("a.py", CLEAN);
    s.commit("base");
    s.pin("sess");

    s.git(&["checkout", "--quiet", "--detach", "HEAD"]);
    assert_eq!(s.git(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "HEAD");
    s.write("a.py", SLOP);

    let out = s.stop("sess");
    assert_eq!(out.baseline_source, ratchet::BaselineSource::Session);
    assert_eq!(out.blocking.len(), 1, "added: {:?}", out.added);
    assert_eq!(out.blocking[0].rule, "slop/comment-history");
}

/// Situation row "gitignored: skipped entirely" — neither scanned nor
/// listed in `skipped[]`, invisible to the changed set.
#[test]
fn a_gitignored_file_is_skipped_entirely() {
    let s = Scratch::new("gitignore");
    s.write(".gitignore", "gen.py\n");
    s.write("a.py", CLEAN);
    s.commit("base");
    s.pin("sess");

    s.write("gen.py", SLOP);
    // The fixture must really be ignored, not merely unlisted.
    assert_eq!(s.git(&["check-ignore", "gen.py"]).trim(), "gen.py");
    assert!(s.repo().untracked_files().is_empty());

    let out = s.stop("sess");
    assert!(out.findings.is_empty(), "{:?}", out.findings);
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    assert_eq!(out.files_scanned, 0);
    assert!(!out.would_block);
}

/// Situation row "binary / symlink: skipped, listed in `skipped[]`".
#[test]
fn binary_and_symlink_paths_are_skipped_and_listed() {
    let s = Scratch::new("skips");
    s.write("a.py", CLEAN);
    s.commit("base");
    s.pin("sess");

    let mut blob = SLOP.as_bytes().to_vec();
    blob.extend_from_slice(b"\x00\x01\x00\x02 trailer\n");
    s.write_bytes("blob.py", &blob);
    std::os::unix::fs::symlink("a.py", s.dir.join("link.py")).expect("symlink");

    let out = s.stop("sess");
    let reasons: Vec<(&str, &str)> = out
        .skipped
        .iter()
        .map(|p| (p.path.as_str(), p.reason))
        .collect();
    assert!(reasons.contains(&("blob.py", "binary")), "{reasons:?}");
    assert!(reasons.contains(&("link.py", "symlink")), "{reasons:?}");
    assert!(
        out.findings.is_empty(),
        "slop inside a binary blob must not fire"
    );
    assert!(!out.would_block);
}

/// The submodule half of that row: a bumped gitlink enters the changed
/// set as one directory path and is skipped, never descended into.
#[test]
fn a_submodule_path_is_skipped_and_listed() {
    let s = Scratch::new("submodule");
    s.write("a.py", CLEAN);
    s.commit("base");

    let inner = s.root.join("inner");
    std::fs::create_dir_all(&inner).expect("inner dir");
    s.git_at(&inner, &["init", "--quiet", "--initial-branch=main"]);
    s.git_at(&inner, &["config", "user.name", "agent-slop-it"]);
    s.git_at(&inner, &["config", "user.email", "it@example.invalid"]);
    std::fs::write(inner.join("lib.py"), CLEAN).expect("inner write");
    s.git_at(&inner, &["add", "-A"]);
    s.git_at(&inner, &["commit", "--quiet", "-m", "inner base"]);

    // git ≥2.38 refuses the file transport for submodules by default, and
    // a repo-local setting does not reach the clone child — only the
    // propagated -c form does.
    let out = Command::new("git")
        .current_dir(&s.dir)
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
        ])
        .arg(inner.to_str().unwrap())
        .arg("sub")
        .output()
        .expect("git submodule add");
    assert!(
        out.status.success(),
        "git submodule add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    s.commit("submodule added");
    let pinned = s.pin("sess");

    // Advance the submodule: the outer worktree gitlink moves, so "sub"
    // reaches the changed set without any outer commit.
    std::fs::write(inner.join("lib.py"), SLOP).expect("inner write");
    s.git_at(&inner, &["add", "-A"]);
    s.git_at(&inner, &["commit", "--quiet", "-m", "inner moves on"]);
    let new_tip = s.git_at(&inner, &["rev-parse", "HEAD"]).trim().to_string();
    let sub = s.dir.join("sub");
    s.git_at(&sub, &["config", "protocol.file.allow", "always"]);
    s.git_at(&sub, &["fetch", "--quiet", "origin"]);
    s.git_at(&sub, &["checkout", "--quiet", &new_tip]);

    let changed = s.repo().changed_files(&pinned.commit);
    assert!(
        changed.iter().any(|p| p == "sub"),
        "fixture must reach the changed set, got {changed:?}"
    );
    let out = s.stop("sess");
    assert!(
        out.skipped
            .iter()
            .any(|p| p.path == "sub" && p.reason == "submodule"),
        "{:?}",
        out.skipped
    );
    assert!(
        out.findings.is_empty(),
        "the submodule's slop must not be charged"
    );
    assert!(!out.would_block);
}

/// The verdict crosses the config mode exactly as the Stop leg reads it
/// from `[slop] mode = "block"` — and the pin itself goes through the
/// real session file, the path the unit tests bypass.
#[test]
fn block_mode_blocks_and_observe_mode_does_not() {
    let s = Scratch::new("mode");
    s.write("a.py", CLEAN);
    s.commit("base");
    let pinned = s.pin("sess");
    assert!(
        s.session_path("sess").exists(),
        "the pin landed in the scratch cache dir"
    );
    assert_eq!(
        ratchet::read_session("sess").map(|p| p.commit).as_deref(),
        Some(pinned.commit.as_str())
    );

    s.write("a.py", SLOP);

    let mut block = builtin_config();
    block.mode = Mode::Block;
    let out = s.stop_with(&block, "sess");
    assert_eq!(out.blocking.len(), 1);
    assert!(out.would_block);
    assert!(
        out.blocked,
        "block mode turns an added error into the exit-2 shape"
    );

    let out = s.stop_with(&builtin_config(), "sess");
    assert!(out.would_block);
    assert!(!out.blocked, "observe mode never blocks");
}

/// The cache in the dangerous direction: a corrupted entry is SERVED
/// verbatim — same tree, flipped verdict — which only a hit, not a
/// recompute, can produce; and content keys the entries, so a second
/// file gets its own. The ruleset-version half of the key is
/// compile-time material (`ruleset_version()` reads the package version)
/// and cannot be mutated inside one process; its stability is pinned by
/// the unit test.
#[test]
fn corrupted_cache_entries_are_served_and_content_keys_the_entries() {
    let s = Scratch::new("cache");
    s.write("a.py", SLOP);
    s.commit("base");
    s.pin("s1");

    // Session edits a.py without touching its slop: a.py reaches the
    // changed set, its baseline content is cached, verdict clean.
    s.write("a.py", &format!("{SLOP}x = 1\n"));
    let out = s.stop("s1");
    assert!(!out.would_block, "{:?}", out.added);
    assert_eq!(s.cache_entries().len(), 1);

    for entry in s.cache_entries() {
        std::fs::write(&entry, "[]").expect("tamper");
    }
    let out = s.stop("s1");
    assert_eq!(
        out.added.len(),
        1,
        "the tampered entry was served, not healed"
    );
    assert!(out.would_block);

    // Different content, different key: b.py is cached fresh and the
    // verdict is clean again despite a.py's still-corrupt entry.
    s.commit("session work");
    s.write("b.py", SLOP2);
    s.commit("second base");
    s.pin("s2");
    s.write("b.py", &format!("{SLOP2}y = 2\n"));
    let out = s.stop("s2");
    assert!(!out.would_block, "{:?}", out.added);
    assert_eq!(
        s.cache_entries().len(),
        2,
        "b.py produced a second, distinct entry"
    );
}

/// Situation row "`--since <ref>`: merge-base, never the ref" — slop
/// another person landed on main after the fork is not charged to the
/// session; only the session's own edit blocks.
#[test]
fn since_charges_the_merge_base_not_the_ref() {
    let s = Scratch::new("since");
    s.write("a.py", CLEAN);
    s.commit("base");
    let fork = s.repo().rev_parse("HEAD").expect("HEAD");

    s.git(&["checkout", "--quiet", "-b", "work"]);
    s.git(&["checkout", "--quiet", "main"]);
    s.write("other.py", SLOP);
    s.commit("someone else's slop on main");
    let main_tip = s.repo().rev_parse("HEAD").expect("main tip");
    s.git(&["checkout", "--quiet", "work"]);

    let repo = s.repo();
    // The comparison the spec forbids would drag other.py in.
    assert!(
        repo.changed_files(&main_tip)
            .iter()
            .any(|p| p == "other.py"),
        "fixture: the naive ref diff does charge main's file"
    );
    let base = repo.resolve_ref("main").expect("main resolves");
    assert_eq!(base, fork, "an explicit ref resolves to the merge-base");
    assert!(!repo.changed_files(&base).iter().any(|p| p == "other.py"));

    s.pin("sess");
    s.write("a.py", SLOP);
    let out = ratchet::evaluate(
        &builtin_config(),
        &repo,
        Some("sess"),
        Some("main"),
        &ratchet::Cache::open(true),
        None,
    )
    .expect("repository");
    assert_eq!(out.blocking.len(), 1, "added: {:?}", out.added);
    assert_eq!(
        out.blocking[0].file, "a.py",
        "only the session's own slop is added"
    );
}

/// Session files older than the 7-day TTL are swept on read; a fresh pin
/// survives the same sweep.
#[test]
fn expired_session_files_are_swept_on_read() {
    let s = Scratch::new("sweep");
    s.write("a.py", CLEAN);
    s.commit("base");
    s.pin("old");
    s.pin("fresh");
    let path = s.session_path("old");
    assert!(path.exists());

    let stale = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60 + 3600);
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("session file");
    file.set_times(
        std::fs::FileTimes::new()
            .set_accessed(stale)
            .set_modified(stale),
    )
    .expect("backdate mtime");
    drop(file);

    assert!(
        ratchet::read_session("old").is_none(),
        "the expired pin must be swept"
    );
    assert!(!path.exists(), "sweep removes the file, not just the read");
    assert!(
        ratchet::read_session("fresh").is_some(),
        "a fresh pin survives the same sweep"
    );
}

/// `[slop.exclude]` must reach the GATE, not just the reporting surface.
/// Before this, `scan` skipped a vendored path while the Stop gate blocked
/// the turn over that same path's comments — the documented escape hatch
/// could not be used to escape.
#[test]
fn an_excluded_path_does_not_block_the_gate() {
    let s = Scratch::new("exclude-gate");
    s.write("keep.py", CLEAN);
    s.commit("base");
    s.pin("ex1");

    s.write("vendor/dep.py", SLOP);
    let blocking_without = s.stop("ex1");
    assert!(
        !blocking_without.blocking.is_empty(),
        "control: an unexcluded vendored file must block, else this test proves nothing"
    );

    let mut cfg = builtin_config();
    cfg.exclude.paths = vec!["vendor/**".to_string()];
    let out = s.stop_with(&cfg, "ex1");
    assert!(
        out.blocking.is_empty(),
        "an excluded path must not block the turn, got {:?}",
        out.blocking
            .iter()
            .map(|f| f.file.clone())
            .collect::<Vec<_>>()
    );
}

/// A scoped exclude silences named rules under named paths, in the gate as
/// well as in `scan`.
#[test]
fn a_scoped_exclude_silences_that_rule_in_the_gate() {
    let s = Scratch::new("exclude-scoped");
    s.write("keep.py", CLEAN);
    s.commit("base");
    s.pin("sc1");

    s.write("migrations/0001.py", SLOP);
    assert!(
        !s.stop("sc1").blocking.is_empty(),
        "control: it blocks without the scoped exclude"
    );

    let mut cfg = builtin_config();
    cfg.exclude.scoped = vec![config::ScopedExclude {
        paths: vec!["migrations/**".to_string()],
        rules: vec!["slop/comment-history".to_string()],
    }];
    assert!(
        s.stop_with(&cfg, "sc1").blocking.is_empty(),
        "the scoped rule must not block under its scoped path"
    );
}
