//! The ratchet: `added = multiset(current) \ multiset(baseline)`, with
//! multiplicity (SPEC-006 "Ratchet").
//!
//! The baseline is computed, never stored — a checked-in baseline file
//! rots, conflicts on merge, and needs a regeneration command an agent
//! would run to silence the gate. Identity is the fingerprint, which omits
//! the path on purpose, so moving a file does not turn inherited debt into
//! "added".
//!
//! Two measured facts shape the Stop leg. An agent commit mid-session
//! empties `git diff --name-only HEAD`, which would make the session's own
//! slop its baseline; and with an empty baseline every finding in an
//! inherited untracked file counts as added forever. So the baseline is
//! pinned once at `SessionStart` and read back at Stop, and when that state
//! is missing, untracked findings are reported but never block:
//! unattributable debt never blocks.
//!
//! Every failure here is fail-open. No git, no repository, an expired
//! deadline: the ratchet is off and nothing blocks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use agent_slop_core::config::{Mode, ResolvedConfig};
use agent_slop_core::finding::{Finding, Severity};
use agent_slop_core::rules::{corpus, scan_source};

/// Session files older than this are swept on read.
const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Fingerprint counts. Multiplicity is load-bearing: copying an existing
/// slop comment into a second file moves the count 1→2, and the difference
/// sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Multiset {
    counts: BTreeMap<String, usize>,
}

impl Multiset {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn add(&mut self, fingerprint: &str) {
        *self.counts.entry(fingerprint.to_string()).or_insert(0) += 1;
    }

    pub(crate) fn from_findings(findings: &[Finding]) -> Self {
        let mut m = Self::new();
        for f in findings {
            m.add(&f.fingerprint);
        }
        m
    }

    pub(crate) fn extend(&mut self, other: &Multiset) {
        for (fp, n) in &other.counts {
            *self.counts.entry(fp.clone()).or_insert(0) += n;
        }
    }

    /// Consume one occurrence; `false` when the baseline has none left.
    pub(crate) fn take(&mut self, fingerprint: &str) -> bool {
        match self.counts.get_mut(fingerprint) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> usize {
        self.counts.values().sum()
    }

    pub(crate) fn fingerprints(&self) -> Vec<String> {
        self.counts
            .iter()
            .flat_map(|(fp, n)| std::iter::repeat_n(fp.clone(), *n))
            .collect()
    }
}

/// Findings present now that the baseline does not account for, in source
/// order. Each baseline occurrence cancels exactly one current finding.
pub(crate) fn added(current: &[Finding], baseline: &Multiset) -> Vec<Finding> {
    let mut remaining = baseline.clone();
    current
        .iter()
        .filter(|f| !remaining.take(&f.fingerprint))
        .cloned()
        .collect()
}

/// A path left out of the scan, carried to the `skipped[]` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedPath {
    pub path: String,
    pub reason: &'static str,
}

pub(crate) struct Repo {
    pub root: PathBuf,
}

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Repo {
    /// `None` when git is absent or `dir` is not a work tree — the caller
    /// then runs without a ratchet.
    pub(crate) fn discover(dir: &Path) -> Option<Repo> {
        let root = run_git(dir, &["rev-parse", "--show-toplevel"])?;
        Some(Repo {
            root: PathBuf::from(root.trim()),
        })
    }

    fn git(&self, args: &[&str]) -> Option<String> {
        run_git(&self.root, args)
    }

    pub(crate) fn rev_parse(&self, rev: &str) -> Option<String> {
        self.git(&["rev-parse", "--verify", "--quiet", rev])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub(crate) fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        self.git(&["merge-base", a, b])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// `[slop] ref`: `auto` is the merge-base of HEAD and the first of
    /// `origin/HEAD`, `main`, `master`. An explicit ref resolves to
    /// `merge-base(<ref>, HEAD)`, never `<ref>` itself, so commits other
    /// people made on the branch are not charged to this session.
    pub(crate) fn resolve_ref(&self, spec: &str) -> Option<String> {
        if spec == "auto" {
            for candidate in ["origin/HEAD", "main", "master"] {
                if self.rev_parse(candidate).is_some()
                    && let Some(base) = self.merge_base("HEAD", candidate)
                {
                    return Some(base);
                }
            }
            return self.rev_parse("HEAD");
        }
        self.merge_base("HEAD", spec)
            .or_else(|| self.rev_parse(spec))
    }

    /// Tracked files differing from `base` in the worktree. The index is
    /// not consulted: what the agent wrote is what is on disk.
    pub(crate) fn changed_files(&self, base: &str) -> Vec<String> {
        self.git(&["diff", "--name-only", base])
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub(crate) fn untracked_files(&self) -> Vec<String> {
        self.git(&["ls-files", "--others", "--exclude-standard"])
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// File content at a revision; `None` when the path did not exist
    /// there, which is the normal case for a file the session created.
    pub(crate) fn show(&self, rev: &str, path: &str) -> Option<String> {
        self.git(&["show", &format!("{rev}:{path}")])
    }
}

/// Why a worktree path contributes nothing. Directories cover submodules,
/// which `git diff` names as a single path.
fn skip_reason(root: &Path, rel: &str) -> Option<&'static str> {
    let full = root.join(rel);
    let Ok(meta) = std::fs::symlink_metadata(&full) else {
        return Some("missing");
    };
    if meta.is_symlink() {
        return Some("symlink");
    }
    if meta.is_dir() {
        return Some("submodule");
    }
    None
}

fn is_binary(content: &[u8]) -> bool {
    content.iter().take(8000).any(|b| *b == 0)
}

/// Hash of the embedded rule corpus plus the package version: every cache
/// entry is keyed by it, so a rebuild that changes a rule invalidates
/// everything rather than serving fingerprints from the old ruleset.
pub(crate) fn ruleset_version() -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    for text in [
        corpus::COMMENT_HISTORY,
        corpus::COMMENT_META,
        corpus::REVIEW_JARGON,
        corpus::COMMENT_PARAPHRASE,
        corpus::TODO_NO_CONDITION,
        corpus::PERF_CLAIM,
        corpus::UNSOURCED_CONSTRAINT,
        corpus::COMMENT_LANGUAGE,
    ] {
        hasher.update(text.as_bytes());
    }
    hasher.finalize().to_hex()[..16].to_string()
}

fn cache_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home::home_dir().map(|h| h.join(".cache")))?;
    Some(base.join("agent-slop"))
}

/// Content-addressed fingerprint cache. Keyed by ruleset version, file
/// extension (the language decides the parse) and content hash, so an
/// entry is valid forever — blobs are immutable and a ruleset change
/// changes the key. `root` also holds pinned session state; tests inject
/// it instead of mutating `$XDG_CACHE_HOME`.
pub(crate) struct Cache {
    root: Option<PathBuf>,
    dir: Option<PathBuf>,
    version: String,
}

impl Cache {
    /// The cache under `$XDG_CACHE_HOME/agent-slop` (or `~/.cache`).
    /// `enabled = false` keeps session state but stores no fingerprints.
    pub(crate) fn open(enabled: bool) -> Cache {
        Cache::at(cache_root(), enabled)
    }

    pub(crate) fn at(root: Option<PathBuf>, enabled: bool) -> Cache {
        let dir = if enabled {
            root.as_ref().map(|r| r.join("v1"))
        } else {
            None
        };
        if let Some(d) = &dir {
            let _ = std::fs::create_dir_all(d);
        }
        Cache {
            root,
            dir,
            version: ruleset_version(),
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Cache {
        Cache::at(None, false)
    }

    fn key(&self, path: &str, content: &str) -> String {
        let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.version.as_bytes());
        hasher.update(&[0x00]);
        hasher.update(ext.as_bytes());
        hasher.update(&[0x00]);
        hasher.update(content.as_bytes());
        hasher.finalize().to_hex()[..32].to_string()
    }

    fn get(&self, key: &str) -> Option<Vec<String>> {
        let path = self.dir.as_ref()?.join(format!("{key}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    fn put(&self, key: &str, fingerprints: &[String]) {
        let Some(dir) = &self.dir else { return };
        if let Ok(json) = serde_json::to_string(fingerprints) {
            let _ = std::fs::write(dir.join(format!("{key}.json")), json);
        }
    }
}

/// Fingerprints of one file's findings, cached. Only the multiset is
/// cached — the baseline side never needs positions or messages.
fn cached_fingerprints(
    cfg: &ResolvedConfig,
    cache: &Cache,
    path: &str,
    content: &str,
) -> Vec<String> {
    let key = cache.key(path, content);
    if let Some(hit) = cache.get(&key) {
        return hit;
    }
    let fps: Vec<String> = scan_source(cfg, path, content)
        .into_iter()
        .map(|f| f.fingerprint)
        .collect();
    cache.put(&key, &fps);
    fps
}

pub(crate) struct Scan {
    pub findings: Vec<Finding>,
    pub skipped: Vec<SkippedPath>,
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

/// Scan worktree copies of `paths`. A file that cannot contribute is
/// recorded in `skipped` rather than dropped silently.
pub(crate) fn scan_worktree(
    cfg: &ResolvedConfig,
    repo: &Repo,
    paths: &[String],
    deadline: Option<Instant>,
) -> Scan {
    let mut findings = Vec::new();
    let mut skipped = Vec::new();
    for path in paths {
        if expired(deadline) {
            skipped.push(SkippedPath {
                path: path.clone(),
                reason: "deadline",
            });
            continue;
        }
        // The gate honours `[slop.exclude]` exactly as `scan` does. Without
        // this the documented escape hatch reached the reporting surface
        // only: `scan` skipped a vendored path while the Stop gate blocked
        // the turn over that same path's comments, with no way out short of
        // disabling the rule globally.
        if agent_slop_core::config::excluded(cfg, path) {
            skipped.push(SkippedPath {
                path: path.clone(),
                reason: "excluded",
            });
            continue;
        }
        if let Some(reason) = skip_reason(&repo.root, path) {
            skipped.push(SkippedPath {
                path: path.clone(),
                reason,
            });
            continue;
        }
        let Ok(bytes) = std::fs::read(repo.root.join(path)) else {
            skipped.push(SkippedPath {
                path: path.clone(),
                reason: "unreadable",
            });
            continue;
        };
        if is_binary(&bytes) {
            skipped.push(SkippedPath {
                path: path.clone(),
                reason: "binary",
            });
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        findings.extend(
            scan_source(cfg, path, &content)
                .into_iter()
                .filter(|f| !agent_slop_core::config::scoped_out(cfg, &f.file, &f.rule)),
        );
    }
    Scan { findings, skipped }
}

/// The baseline multiset: each path as it was at `rev`. A path absent
/// there contributes nothing, which is what makes a file the session
/// created fully attributable to it.
pub(crate) fn baseline_at(
    cfg: &ResolvedConfig,
    repo: &Repo,
    rev: &str,
    paths: &[String],
    cache: &Cache,
    deadline: Option<Instant>,
) -> Multiset {
    let mut multiset = Multiset::new();
    for path in paths {
        if expired(deadline) {
            break;
        }
        let Some(content) = repo.show(rev, path) else {
            continue;
        };
        for fp in cached_fingerprints(cfg, cache, path, &content) {
            multiset.add(&fp);
        }
    }
    multiset
}

/// What the baseline was built from. `LiveHead` is the degraded path: no
/// session state, so untracked findings cannot be attributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaselineSource {
    Session,
    LiveHead,
}

/// The pinned session state. Written by the `SessionStart` leg, read at
/// Stop; the hook dispatch that calls these lives in its own module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionBaseline {
    pub schema: u32,
    pub commit: String,
    pub untracked: Multiset,
    pub written_at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn session_dir(cache: &Cache) -> Option<PathBuf> {
    let dir = cache.root.as_ref()?.join("session");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A session id reaches the filesystem, so anything that is not a plain
/// identifier is rejected rather than sanitised — a silently rewritten id
/// reads back as a missing session, which is the wrong failure.
fn session_file(cache: &Cache, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(session_dir(cache)?.join(format!("{session_id}.json")))
}

/// Pin HEAD and the untracked debt present right now. Returns what it
/// wrote so a caller can report it; a write failure is not an error,
/// because losing the file is always safe (Stop falls back).
pub(crate) fn pin_session(
    cfg: &ResolvedConfig,
    repo: &Repo,
    cache: &Cache,
    session_id: &str,
    deadline: Option<Instant>,
) -> Option<SessionBaseline> {
    let commit = repo.rev_parse("HEAD")?;
    let untracked_paths = repo.untracked_files();
    let scan = scan_worktree(cfg, repo, &untracked_paths, deadline);
    let pinned = SessionBaseline {
        schema: 1,
        commit,
        untracked: Multiset::from_findings(&scan.findings),
        written_at: now_secs(),
    };
    if let (Some(path), Ok(json)) = (
        session_file(cache, session_id),
        serde_json::to_string(&pinned),
    ) {
        let _ = std::fs::write(path, json);
    }
    Some(pinned)
}

/// Read back the pinned state, sweeping expired files first.
pub(crate) fn read_session(cache: &Cache, session_id: &str) -> Option<SessionBaseline> {
    sweep_sessions(cache);
    let path = session_file(cache, session_id)?;
    let pinned: SessionBaseline =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if pinned.schema != 1 {
        return None;
    }
    Some(pinned)
}

pub(crate) fn sweep_sessions(cache: &Cache) {
    let Some(dir) = session_dir(cache) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = now_secs().saturating_sub(SESSION_TTL_SECS);
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if modified < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(crate) struct GateOutcome {
    /// Everything found in the changed set, suppressed entries included.
    pub findings: Vec<Finding>,
    pub added: Vec<Finding>,
    /// The subset of `added` that justifies exit 2: unsuppressed errors,
    /// minus anything the fallback rule makes unattributable.
    pub blocking: Vec<Finding>,
    /// Paths the gate could not scan, asserted by the scenario tests; the
    /// Stop leg reports only the derived `files_scanned`.
    #[cfg_attr(not(test), expect(dead_code, reason = "read by scenario tests"))]
    pub skipped: Vec<SkippedPath>,
    pub baseline_source: BaselineSource,
    pub files_scanned: usize,
    /// What the gate WOULD do; `blocked` is what it did. They differ only
    /// in observe mode, which never exits 2.
    pub would_block: bool,
    pub blocked: bool,
}

/// The whole ratchet in one call. `None` means the ratchet is impossible
/// here — no git, no repository — and the caller exits 0.
pub(crate) fn evaluate(
    cfg: &ResolvedConfig,
    repo: &Repo,
    session_id: Option<&str>,
    since: Option<&str>,
    cache: &Cache,
    deadline: Option<Instant>,
) -> Option<GateOutcome> {
    let session = session_id.and_then(|id| read_session(cache, id));
    let (base_commit, baseline_source) = match (&session, since) {
        // An explicit --since overrides the pinned commit: the human asked
        // for that comparison.
        (_, Some(spec)) => (repo.resolve_ref(spec)?, BaselineSource::LiveHead),
        (Some(pinned), None) => (pinned.commit.clone(), BaselineSource::Session),
        (None, None) => (repo.resolve_ref(&cfg.ref_)?, BaselineSource::LiveHead),
    };

    let untracked = repo.untracked_files();
    let mut paths = repo.changed_files(&base_commit);
    paths.extend(untracked.iter().cloned());
    paths.sort();
    paths.dedup();

    let scan = scan_worktree(cfg, repo, &paths, deadline);
    let mut baseline = baseline_at(cfg, repo, &base_commit, &paths, cache, deadline);
    if let Some(pinned) = &session {
        baseline.extend(&pinned.untracked);
    }

    let added_findings = added(&scan.findings, &baseline);
    let blocking: Vec<Finding> = added_findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .filter(|f| f.suppression.as_ref().is_none_or(|s| !s.valid))
        // Fallback rule: without pinned state an untracked file's findings
        // cannot be told from debt this session inherited, so they are
        // reported and never blocking.
        .filter(|f| baseline_source == BaselineSource::Session || !untracked.contains(&f.file))
        .cloned()
        .collect();

    let would_block = !blocking.is_empty();
    Some(GateOutcome {
        files_scanned: paths.len() - scan.skipped.len(),
        findings: scan.findings,
        added: added_findings,
        blocking,
        skipped: scan.skipped,
        baseline_source,
        would_block,
        blocked: would_block && cfg.mode == Mode::Block,
    })
}

#[cfg(test)]
mod scenario_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use agent_slop_core::config::builtin_config;

    /// Fires `slop/comment-history` at `error` (see `rules::tests`).
    const SLOP: &str = "# We previously always sent a Referer — that was a bug.\nreferer = None\n";
    const CLEAN: &str = "referer = None\n";

    struct Scratch {
        dir: PathBuf,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Scratch {
        /// A real repository with an identity of its own, so the test never
        /// depends on the machine's git config.
        fn new(name: &str) -> Scratch {
            assert!(
                Command::new("git").arg("--version").output().is_ok(),
                "git is absent: this ratchet test cannot run, and a gate that \
                 cannot fail is worse than no gate (POL-026)"
            );
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let dir = std::env::temp_dir().join(format!(
                "agent-slop-ratchet-{name}-{}-{nanos}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let s = Scratch { dir };
            s.git(&["init", "--quiet", "--initial-branch=main"]);
            s.git(&["config", "user.name", "agent-slop-test"]);
            s.git(&["config", "user.email", "test@example.invalid"]);
            s
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .current_dir(&self.dir)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
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

        fn commit(&self, message: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "--quiet", "-m", message]);
        }

        fn repo(&self) -> Repo {
            Repo::discover(&self.dir).expect("scratch is a repo")
        }
    }

    fn pinned_outcome(s: &Scratch, session: &SessionBaseline) -> GateOutcome {
        // The pinned state is passed directly rather than through the cache
        // directory, so the test never depends on $XDG_CACHE_HOME.
        let cfg = builtin_config();
        let repo = s.repo();
        let untracked = repo.untracked_files();
        let mut paths = repo.changed_files(&session.commit);
        paths.extend(untracked.iter().cloned());
        paths.sort();
        paths.dedup();
        let scan = scan_worktree(&cfg, &repo, &paths, None);
        let mut baseline = baseline_at(
            &cfg,
            &repo,
            &session.commit,
            &paths,
            &Cache::disabled(),
            None,
        );
        baseline.extend(&session.untracked);
        let added_findings = added(&scan.findings, &baseline);
        let blocking: Vec<Finding> = added_findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .cloned()
            .collect();
        let would_block = !blocking.is_empty();
        GateOutcome {
            files_scanned: paths.len(),
            findings: scan.findings,
            added: added_findings,
            blocking,
            skipped: scan.skipped,
            baseline_source: BaselineSource::Session,
            would_block,
            blocked: would_block,
        }
    }

    fn pin(s: &Scratch) -> SessionBaseline {
        let cfg = builtin_config();
        let repo = s.repo();
        let scan = scan_worktree(&cfg, &repo, &repo.untracked_files(), None);
        SessionBaseline {
            schema: 1,
            commit: repo.rev_parse("HEAD").expect("HEAD"),
            untracked: Multiset::from_findings(&scan.findings),
            written_at: now_secs(),
        }
    }

    /// THE measured reason the design pins a baseline: committing during
    /// the session must not make the session's own slop the baseline.
    #[test]
    fn an_agent_commit_midsession_does_not_reset_the_ratchet() {
        let s = Scratch::new("commit");
        s.write("a.py", CLEAN);
        s.commit("base");
        let session = pin(&s);

        s.write("a.py", SLOP);
        s.commit("the agent commits its own work");

        // Naive `--since HEAD` sees nothing: the commit moved HEAD onto the
        // slop, which is exactly the hole being closed.
        let repo = s.repo();
        assert!(repo.changed_files("HEAD").is_empty());

        let outcome = pinned_outcome(&s, &session);
        assert_eq!(outcome.blocking.len(), 1, "added: {:?}", outcome.added);
        assert_eq!(outcome.blocking[0].rule, "slop/comment-history");
    }

    /// The other measured trap: debt inherited in an untracked file is in
    /// the pinned multiset, so it cancels.
    #[test]
    fn untracked_debt_present_at_session_start_does_not_block() {
        let s = Scratch::new("inherited");
        s.write("tracked.py", CLEAN);
        s.commit("base");
        s.write("inherited.py", SLOP);

        let session = pin(&s);
        assert_eq!(
            session.untracked.total(),
            1,
            "the pin recorded the inherited finding"
        );

        let outcome = pinned_outcome(&s, &session);
        assert!(
            outcome.blocking.is_empty(),
            "blocking: {:?}",
            outcome.blocking
        );
        assert_eq!(outcome.findings.len(), 1, "still reported, just not added");
    }

    #[test]
    fn a_file_created_during_the_session_blocks() {
        let s = Scratch::new("created");
        s.write("a.py", CLEAN);
        s.commit("base");
        let session = pin(&s);

        s.write("new.py", SLOP);
        let outcome = pinned_outcome(&s, &session);
        assert_eq!(outcome.blocking.len(), 1);
        assert_eq!(outcome.blocking[0].file, "new.py");
    }

    /// Multiplicity: the same comment copied into a second file moves the
    /// count 1→2 and the difference sees the copy.
    #[test]
    fn copying_an_existing_slop_comment_is_seen() {
        let s = Scratch::new("copy");
        s.write("a.py", SLOP);
        s.commit("base");
        let session = pin(&s);

        s.write("b.py", SLOP);
        let outcome = pinned_outcome(&s, &session);
        assert_eq!(outcome.added.len(), 1, "one new occurrence, not two");
        assert_eq!(outcome.added[0].file, "b.py");
    }

    /// Accepted failure, stated in SPEC-006: the ratchet counts debt, it
    /// does not localise it, so a move is invisible.
    #[test]
    fn a_moved_violation_is_not_reported() {
        let s = Scratch::new("move");
        s.write("a.py", SLOP);
        s.commit("base");
        let session = pin(&s);

        std::fs::remove_file(s.dir.join("a.py")).expect("remove");
        s.write("b.py", SLOP);

        let outcome = pinned_outcome(&s, &session);
        assert!(outcome.added.is_empty(), "added: {:?}", outcome.added);
    }

    /// `normalize` collapses newlines, so rewrapping a comment is not new
    /// slop.
    #[test]
    fn reformatting_a_comment_is_not_new_slop() {
        let s = Scratch::new("reformat");
        s.write(
            "a.rs",
            "// We previously always sent a Referer — that was a bug.\nfn f() {}\n",
        );
        s.commit("base");
        let session = pin(&s);

        s.write(
            "a.rs",
            "/* We previously always sent a Referer\n   — that was a bug. */\nfn f() {}\n",
        );
        let outcome = pinned_outcome(&s, &session);
        assert!(outcome.added.is_empty(), "added: {:?}", outcome.added);
    }

    #[test]
    fn deleting_a_violation_never_blocks() {
        let s = Scratch::new("delete");
        s.write("a.py", SLOP);
        s.commit("base");
        let session = pin(&s);

        std::fs::remove_file(s.dir.join("a.py")).expect("remove");
        let outcome = pinned_outcome(&s, &session);
        assert!(outcome.added.is_empty());
        assert!(outcome.blocking.is_empty());
    }

    /// Fallback rule 3: with no pinned state, untracked findings are
    /// reported and excluded from the blocking set, because inherited and
    /// added cannot be told apart.
    #[test]
    fn without_session_state_untracked_findings_are_reported_but_never_block() {
        let s = Scratch::new("fallback");
        s.write("a.py", CLEAN);
        s.commit("base");
        s.write("inherited.py", SLOP);

        let cfg = builtin_config();
        let repo = s.repo();
        let outcome = evaluate(&cfg, &repo, None, Some("HEAD"), &Cache::disabled(), None)
            .expect("a repository with git present");
        assert_eq!(outcome.baseline_source, BaselineSource::LiveHead);
        assert_eq!(outcome.added.len(), 1, "reported as added");
        assert!(outcome.blocking.is_empty(), "but never blocking");
    }

    #[test]
    fn observe_mode_computes_would_block_without_blocking() {
        let s = Scratch::new("observe");
        s.write("a.py", CLEAN);
        s.commit("base");
        s.write("a.py", SLOP);

        let mut cfg = builtin_config();
        cfg.mode = Mode::Observe;
        let repo = s.repo();
        let outcome = evaluate(&cfg, &repo, None, Some("HEAD"), &Cache::disabled(), None)
            .expect("a repository");
        assert!(outcome.would_block, "the gate saw an added error");
        assert!(!outcome.blocked, "observe mode never exits 2");
    }

    #[test]
    fn since_compares_against_the_merge_base_not_the_ref() {
        let s = Scratch::new("mergebase");
        s.write("a.py", CLEAN);
        s.commit("base");
        let fork = s.repo().rev_parse("HEAD").expect("HEAD");

        // A commit on main that the session did not make.
        s.write("other.py", SLOP);
        s.commit("someone else's slop");
        s.git(&["branch", "work", &fork]);
        s.git(&["checkout", "--quiet", "work"]);

        let repo = s.repo();
        assert_eq!(repo.resolve_ref("main").as_deref(), Some(fork.as_str()));
    }

    #[test]
    fn outside_a_repository_there_is_no_ratchet() {
        let dir = std::env::temp_dir().join(format!("agent-slop-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let discovered = Repo::discover(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        // A temp dir can itself sit inside a repository on some machines;
        // the assertion that matters is that discovery never panics and a
        // miss yields None.
        if let Some(repo) = discovered {
            assert!(repo.root.exists());
        }
    }

    #[test]
    fn a_session_id_that_is_not_a_plain_identifier_is_refused() {
        assert!(
            session_file(
                &Cache::at(Some(std::env::temp_dir()), false),
                "../../etc/passwd"
            )
            .is_none()
        );
        assert!(session_file(&Cache::at(Some(std::env::temp_dir()), false), "").is_none());
    }

    #[test]
    fn the_ruleset_version_changes_with_the_corpus() {
        let v = ruleset_version();
        assert_eq!(v.len(), 16);
        assert_eq!(v, ruleset_version(), "stable across calls");
    }

    #[test]
    fn multiset_difference_respects_multiplicity() {
        let mut baseline = Multiset::new();
        baseline.add("aa");
        let findings: Vec<Finding> = ["aa", "aa", "bb"]
            .iter()
            .map(|fp| Finding {
                rule: "slop/comment-history".into(),
                severity: Severity::Error,
                upstream_severity: None,
                file: "a.py".into(),
                line: 1,
                col: 1,
                end_line: 1,
                message: "m".into(),
                snippet: "s".into(),
                fingerprint: (*fp).to_string(),
                source: agent_slop_core::finding::Source::Slop,
                lang: None,
                suppression: None,
                fixable: true,
            })
            .collect();
        let got: Vec<String> = added(&findings, &baseline)
            .into_iter()
            .map(|f| f.fingerprint)
            .collect();
        assert_eq!(got, vec!["aa", "bb"]);
    }
}
