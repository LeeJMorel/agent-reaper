//! Reaping Claude Code sessions. Claude Code runs no hook while a session sits idle, so when
//! one starts in a terminal its hook leaves a watcher beside it (`reaper watch claude-code
//! PID`), a few MB. The hooks tell it whether the session is waiting on the human (see
//! `hosts::claude_code::supervisor_state`); once it has waited `reap.idle`, or room is short
//! (see `engine::pressure`), the watcher shuts Claude Code down. The conversation is saved,
//! and `claude --resume` picks it up. Sessions in an editor, or run by a script (`-p`), are
//! left alone: Claude Code tells its hooks which kind it is.

use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use reaper_core::events::now;
use reaper_core::paths;
use reaper_core::rules::RulesFile;
use serde_json::{Value, json};

use super::HOST;
use crate::engine::store::{Store, mark_for};
use crate::engine::{Engine, pressure};
use crate::hosts::{SHELLS, process};

const TICK: Duration = Duration::from_secs(1);
/// An entry in the live list 10 s old is taken as gone.
const BEAT: Duration = Duration::from_secs(2);
const GRACE: Duration = Duration::from_secs(5);

/// Where the hooks of the Claude Code process `pid` tell its watcher how the session is.
pub fn state_file(pid: u32) -> PathBuf {
    paths::state_dir().join("runs").join(format!("{pid}.claude.json"))
}

fn lock(pid: u32) -> Option<File> {
    let path = state_file(pid).with_extension("lock");
    File::options().create(true).truncate(false).write(true).open(path).ok()
}

/// Starts the watcher for the Claude Code process `pid`, unless one is already there. It
/// outlives the hook, in a process group of its own, so the ctrl-c meant for Claude Code
/// doesn't reach it.
pub fn start(pid: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let Some(lock) = lock(pid) else { return };
        if lock.try_lock().is_err() {
            return;
        }
        drop(lock);
        let Ok(me) = env::current_exe() else { return };
        let _ = Command::new(me)
            .args(["watch", HOST, &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
    }
}

/// `reaper watch claude-code PID`.
pub fn run(args: &[String]) -> Result<ExitCode, String> {
    let pid = match args {
        [host, pid] if host == HOST => pid.parse::<u32>().map_err(|_| format!("not a process id: {pid}"))?,
        _ => return Err("usage: reaper watch claude-code PID".into()),
    };
    // One watcher per Claude Code: one started alongside leaves.
    let Some(lock) = lock(pid) else { return Ok(ExitCode::SUCCESS) };
    if lock.try_lock().is_err() {
        return Ok(ExitCode::SUCCESS);
    }
    let state = state_file(pid);
    if let Some((_, name)) = process(pid)
        && let Some(reaped) = watch(pid, &name, &state)
    {
        let fields = json!({ "idle": reaped.idle, "reason": reaped.reason, "file": reaped.transcript });
        Engine::load().event(HOST, &reaped.session, "agent.reaped", fields);
    }
    let _ = fs::remove_file(&state);
    let _ = fs::remove_file(state.with_extension("lock"));
    Ok(ExitCode::SUCCESS)
}

struct Reaped {
    session: String,
    transcript: String,
    idle: u64,
    reason: String,
}

/// Until Claude Code exits, or is reaped. It's known by its name too, so a process that
/// takes its id after it's gone is never mistaken for it.
fn watch(pid: u32, name: &str, state_file: &Path) -> Option<Reaped> {
    let alive = || process(pid).is_some_and(|(_, now)| now == name);
    let terminal = terminal_of(pid);
    let store = Store::default();
    let mut rules = RulesFile::load(paths::rules_file());
    let mut live: Option<(String, PathBuf)> = None;
    let mut beat = Instant::now();
    let mut was_idle = false;
    let mut reap_after: Option<u64> = None;
    let leave = |live: &Option<(String, PathBuf)>| {
        if let Some((_, file)) = live {
            store.forget(file);
        }
    };
    loop {
        if !alive() {
            leave(&live);
            return None;
        }
        rules.refresh();
        let config = &rules.config;
        let state: Option<Value> = fs::read(state_file).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let session = state.as_ref().and_then(|s| s["session"].as_str()).map(str::to_string);
        // Typing counts, though no hook hears it: the terminal keeps the time of the last key.
        let typed = terminal.as_deref().and_then(typed_at).unwrap_or(0);
        let idle_since = state.as_ref().and_then(|s| s["idleSince"].as_i64()).map(|since| since.max(typed));
        let now = now();

        if let Some(session) = &session
            && live.as_ref().is_none_or(|(s, _)| s != session)
        {
            leave(&live);
            live = store.register(HOST, session, now).ok().map(|(file, _)| (session.clone(), file));
            store.keep_live(&live.as_ref().expect("just registered").1, idle_since);
            pressure::relieve(config, &store, true);
        }
        let Some((_, file)) = &live else {
            thread::sleep(TICK);
            continue;
        };
        let became_idle = idle_since.is_some() && !was_idle;
        was_idle = idle_since.is_some();
        if became_idle {
            // Read once per wait: `auto` learns from the event log.
            reap_after = reaper_core::idle::reap_after_ms(config, &paths::events_file());
        }
        if became_idle || beat.elapsed() >= BEAT {
            store.keep_live(file, idle_since);
            beat = Instant::now();
        }
        if became_idle {
            pressure::relieve(config, &store, true);
        }

        let mark = mark_for(file);
        let marked = fs::read_to_string(&mark).ok().map(|reason| {
            let _ = fs::remove_file(&mark);
            reason.trim().to_string()
        });
        let timed_out = || {
            let after = reap_after? as i64;
            (now - idle_since? >= after).then(|| "idle".to_string())
        };
        if let (Some(reason), Some(since), Some(state), Some(session)) =
            (marked.or_else(timed_out), idle_since, &state, &session)
        {
            let transcript = state["transcript"].as_str().unwrap_or_default();
            // Nothing to resume yet, or work still running in the background: it stays.
            if Path::new(transcript).is_file() && !background_work(pid) {
                let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
                let asked = Instant::now();
                while alive() && asked.elapsed() < GRACE {
                    thread::sleep(Duration::from_millis(100));
                }
                if alive() {
                    let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
                }
                leave(&live);
                let idle = (now - since).max(0) as u64;
                return Some(Reaped { session: session.clone(), transcript: transcript.to_string(), idle, reason });
            }
        }
        thread::sleep(TICK);
    }
}

/// The terminal Claude Code reads keys from: where its input comes from (Linux), or what
/// `ps` says elsewhere.
fn terminal_of(pid: u32) -> Option<PathBuf> {
    if let Ok(path) = fs::read_link(format!("/proc/{pid}/fd/0")) {
        return path.starts_with("/dev/").then_some(path);
    }
    let out = Command::new("ps").args(["-o", "tty=", "-p", &pid.to_string()]).output().ok()?;
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!tty.is_empty() && !tty.starts_with('?')).then(|| Path::new("/dev").join(tty))
}

/// When a key was last pressed in `terminal`, in ms (to the second, or coarser: Linux rounds
/// a terminal's times to 8 s). The same time `w` shows as IDLE.
fn typed_at(terminal: &Path) -> Option<i64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(fs::metadata(terminal).ok()?.atime() * 1000)
    }
    #[cfg(not(unix))]
    None
}

/// Every process: its id, its parent's, and its name.
fn processes() -> Option<Vec<(u32, u32, String)>> {
    let out = Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output().ok()?;
    let procs = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            let comm = parts.collect::<Vec<_>>().join(" ");
            Some((pid, ppid, comm))
        })
        .collect();
    Some(procs)
}

/// A shell still running under Claude Code once its turn is over is background work, a dev
/// server or a build, that reaping would kill. Claude Code starts one per command and keeps
/// none of its own. When `ps` can't say, the answer is yes: nothing is reaped on a guess.
fn background_work(root: u32) -> bool {
    let Some(procs) = processes() else { return true };
    let (mut below, mut seen) = (vec![root], HashSet::new());
    while let Some(parent) = below.pop() {
        for (pid, _, comm) in procs.iter().filter(|(_, ppid, _)| *ppid == parent) {
            let name = comm.rsplit('/').next().unwrap_or(comm).trim_start_matches('-');
            if SHELLS.contains(&name) {
                return true;
            }
            if seen.insert(*pid) {
                below.push(*pid);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_running_below_counts_as_background_work() {
        // This test runs under cargo, with no shell below it.
        assert!(!background_work(std::process::id()));
        let mut shell = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(background_work(std::process::id()), "a shell below is background work");
        let _ = shell.kill();
        let _ = shell.wait();
    }
}
