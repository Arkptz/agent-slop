//! Process runner for plugins: a bounded, environment-scrubbed subprocess
//! call with a hard wall-clock budget.
//!
//! Stdin is written and stdout/stderr are drained on their own threads so
//! a plugin that floods a pipe cannot deadlock the host; the main thread
//! polls `try_wait` every 5 ms and kills the child when the budget runs
//! out. On unix the child leads its own process group, so that kill takes
//! its forked helpers with it (a plugin that outlives its budget must not
//! leave a tree behind). Results come back over a channel with a
//! deadline, never a join, so a grandchild that inherited the pipe cannot
//! hold the host past its budget.
//!
//! A child in its own group does not receive a signal aimed at the host's
//! group (terminal Ctrl-C, a harness killing the hook's group), so while a
//! call is in flight the host catches `SIGINT`/`SIGTERM`, kills the
//! plugin's group from its poll loop and then dies of the same signal.
//! Outside a call the signals keep their default action.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use super::ResolvedPluginRef;

/// Largest stdout the host will read from one call.
pub const MAX_STDOUT: usize = 8 * 1024 * 1024;

/// Environment a plugin always receives; anything else needs the user
/// tier's `env_passthrough`.
const ENV_ALLOWLIST: [&str; 6] = ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "XDG_CACHE_HOME"];

const POLL: Duration = Duration::from_millis(5);

/// Run `argv + [subcommand]` from `cwd`, feed `stdin`, and return its
/// stdout.
///
/// # Errors
///
/// A skip reason, never a failure of the host: `absent`,
/// `spawn failed: …`, `timeout`, `output-too-large`, `failed:<exit>`.
/// A `SIGINT`/`SIGTERM` for the host kills the plugin tree and then the
/// host itself, so no reason is returned for it.
pub fn run_process(
    plugin: &ResolvedPluginRef<'_>,
    subcommand: &str,
    stdin: &[u8],
    cwd: &Path,
    budget: Duration,
) -> Result<Vec<u8>, String> {
    let call = host_signals::enter();
    let result = run_inner(plugin, subcommand, stdin, cwd, budget);
    call.leave();
    result
}

fn run_inner(
    plugin: &ResolvedPluginRef<'_>,
    subcommand: &str,
    stdin: &[u8],
    cwd: &Path,
    budget: Duration,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let Some((program, args)) = plugin.command.split_first() else {
        return Err("absent".to_string());
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .arg(subcommand)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    for key in ENV_ALLOWLIST
        .iter()
        .copied()
        .chain(plugin.env_passthrough.iter().map(String::as_str))
    {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("absent".to_string()),
        Err(e) => return Err(format!("spawn failed: {e}")),
    };

    if let Some(mut pipe) = child.stdin.take() {
        let bytes = stdin.to_vec();
        std::thread::spawn(move || {
            // A plugin that exits without reading its input closes the
            // pipe; that is its prerogative.
            let _ = pipe.write_all(&bytes);
        });
    }

    let oversize = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    if let Some(mut out) = child.stdout.take() {
        let flag = Arc::clone(&oversize);
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut out).take(MAX_STDOUT as u64 + 1).read_to_end(&mut buf);
            if buf.len() > MAX_STDOUT {
                flag.store(true, Ordering::SeqCst);
            }
            let _ = tx.send(buf);
        });
    }
    if let Some(mut err) = child.stderr.take() {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut err, &mut std::io::sink());
        });
    }

    let status = loop {
        if host_signals::pending() {
            kill(&mut child);
            return Err("interrupted".to_string());
        }
        if oversize.load(Ordering::SeqCst) {
            kill(&mut child);
            return Err("output-too-large".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill(&mut child);
                return Err(format!("spawn failed: {e}"));
            }
        }
        if started.elapsed() >= budget {
            kill(&mut child);
            return Err("timeout".to_string());
        }
        std::thread::sleep(POLL);
    };

    let left = budget.saturating_sub(started.elapsed());
    let Ok(stdout) = rx.recv_timeout(left.max(POLL)) else {
        // The child exited but a helper of its group still holds stdout.
        kill_group(child.id());
        return Err("timeout".to_string());
    };
    if oversize.load(Ordering::SeqCst) || stdout.len() > MAX_STDOUT {
        return Err("output-too-large".to_string());
    }
    if !status.success() {
        return Err(status.code().map_or_else(
            || "failed:signal".to_string(),
            |code| format!("failed:{code}"),
        ));
    }
    Ok(stdout)
}

/// SIGKILL the process group the plugin leads (`process_group(0)` made its
/// pid the group id). A group with no members left is a harmless ESRCH.
#[cfg(unix)]
fn kill_group(pgid: u32) {
    if let Some(pgid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
    }
}

/// No process groups here: `kill` falls back to killing the child alone.
#[cfg(not(unix))]
fn kill_group(_pgid: u32) {}

/// Kill the plugin's whole group, then the child itself (a no-op once the
/// group signal took it), and reap it.
fn kill(child: &mut std::process::Child) {
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// `SIGINT`/`SIGTERM` handling for the duration of a plugin call.
///
/// The handlers are installed once per process and stay installed: the
/// signal registry cannot restore the previous disposition. While no call
/// is in flight they emulate the default action, so the host dies exactly
/// as it would without them. While a call is in flight they only record
/// the signal; the poll loop kills the plugin group and [`Call::leave`]
/// re-raises the signal with the default action.
#[cfg(unix)]
mod host_signals {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use signal_hook::consts::{SIGINT, SIGTERM};

    struct State {
        /// True while no call is in flight (the default action may run).
        idle: Arc<AtomicBool>,
        /// Signals received, set by the handler.
        hits: Vec<(i32, Arc<AtomicBool>)>,
        /// Calls in flight, so a concurrent call keeps `idle` false.
        inflight: Mutex<usize>,
    }

    static STATE: OnceLock<Option<State>> = OnceLock::new();

    fn state() -> Option<&'static State> {
        STATE
            .get_or_init(|| {
                let idle = Arc::new(AtomicBool::new(true));
                let mut hits = Vec::new();
                for sig in [SIGINT, SIGTERM] {
                    let hit = Arc::new(AtomicBool::new(false));
                    // Actions run in registration order: record first,
                    // then the default action when nothing is in flight.
                    signal_hook::flag::register(sig, Arc::clone(&hit)).ok()?;
                    signal_hook::flag::register_conditional_default(sig, Arc::clone(&idle)).ok()?;
                    hits.push((sig, hit));
                }
                Some(State {
                    idle,
                    hits,
                    inflight: Mutex::new(0),
                })
            })
            .as_ref()
    }

    /// One plugin call in flight.
    #[derive(Debug)]
    pub(super) struct Call(bool);

    pub(super) fn enter() -> Call {
        let Some(state) = state() else {
            return Call(false);
        };
        if let Ok(mut n) = state.inflight.lock() {
            *n += 1;
            state.idle.store(false, Ordering::SeqCst);
        }
        Call(true)
    }

    /// Has the host been told to stop?
    pub(super) fn pending() -> bool {
        state().is_some_and(|s| s.hits.iter().any(|(_, h)| h.load(Ordering::SeqCst)))
    }

    impl Call {
        /// End the call; if a signal arrived meanwhile the host now dies of it.
        pub(super) fn leave(self) {
            let Some(state) = state().filter(|_| self.0) else {
                return;
            };
            let signal = state
                .hits
                .iter()
                .find(|(_, h)| h.load(Ordering::SeqCst))
                .map(|(s, _)| *s);
            if let Ok(mut n) = state.inflight.lock() {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    state.idle.store(true, Ordering::SeqCst);
                }
            }
            if let Some(sig) = signal {
                let _ = signal_hook::low_level::emulate_default_handler(sig);
            }
        }
    }
}

#[cfg(not(unix))]
mod host_signals {
    /// No signal forwarding without process groups.
    #[derive(Debug)]
    pub(super) struct Call;

    pub(super) fn enter() -> Call {
        Call
    }

    pub(super) fn pending() -> bool {
        false
    }

    impl Call {
        pub(super) fn leave(self) {}
    }
}
