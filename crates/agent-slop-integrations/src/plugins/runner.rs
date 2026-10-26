//! Process runner for plugins: a bounded, environment-scrubbed subprocess
//! call with a hard wall-clock budget.
//!
//! No new dependencies. Stdin is written and stdout/stderr are drained on
//! their own threads so a plugin that floods a pipe cannot deadlock the
//! host; the main thread polls `try_wait` every 5 ms and kills the child
//! when the budget runs out. The child gets its own process group, so that
//! kill takes its forked helpers with it (a plugin that outlives its
//! budget must not leave a tree behind). Results come back over a channel
//! with a deadline, never a join, so a grandchild that inherited the pipe
//! cannot hold the host past its budget.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
pub fn run_process(
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
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
fn kill_group(pgid: u32) {
    if let Some(pgid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
    }
}

/// Kill the plugin's whole group, then the child itself (a no-op once the
/// group signal took it), and reap it.
fn kill(child: &mut std::process::Child) {
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}
