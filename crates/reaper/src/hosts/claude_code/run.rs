//! `reaper claude`: Claude Code, guarded, and reaped when idle. No hook fires while a Claude
//! Code session sits idle, so reaper starts Claude Code itself and stays its parent, as
//! `reaper run` does for pi. The hooks say when the session is waiting on the human (see
//! `hosts::claude_code::supervisor_state`); once it has waited `reap.idle`, or room is short
//! (see `engine::pressure`), reaper shuts it down and offers to resume it here.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use reaper_core::events::now;
use reaper_core::paths;
use reaper_core::rules::RulesFile;
use serde_json::{Value, json};

use super::{HOST, STATE_ENV, install};
use crate::engine::store::{Store, mark_for};
use crate::engine::{Engine, pressure};
use crate::hosts::{exit_code, resume_here, shell_word};

const TICK: Duration = Duration::from_millis(500);
/// An entry in the live list 10 s old is taken as gone.
const BEAT: Duration = Duration::from_secs(2);
const GRACE: Duration = Duration::from_secs(5);
/// A shell still running under Claude Code once its turn is over is background work, a dev
/// server or a build, that reaping would kill. Claude Code starts one per command and keeps
/// none of its own.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh"];

fn binary() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("REAPER_CLAUDE").filter(|p| !p.is_empty()) {
        return Ok(path.into());
    }
    env::var_os("PATH")
        .and_then(|paths| env::split_paths(&paths).map(|dir| dir.join("claude")).find(|p| p.is_file()))
        .ok_or_else(|| "Claude Code isn't on PATH (or set $REAPER_CLAUDE)".to_string())
}

pub fn launch(args: &[String]) -> Result<ExitCode, String> {
    let claude = binary()?;
    let me = env::current_exe().map_err(|e| format!("can't locate the reaper binary: {e}"))?;
    let state = paths::state_dir().join("runs").join(format!("{}.claude.json", std::process::id()));
    fs::create_dir_all(state.parent().expect("runs dir"))
        .map_err(|e| format!("can't create {}: {e}", state.display()))?;
    let hooks = (!install::installed() && !args.iter().any(|a| a == "--settings"))
        .then(|| install::hooks_only(&install::command(&me)).to_string());
    let mut args = args.to_vec();
    loop {
        let _ = fs::remove_file(&state);
        let mut command = Command::new(&claude);
        if let Some(hooks) = &hooks {
            command.arg("--settings").arg(hooks);
        }
        let mut child = command
            .args(&args)
            .env(STATE_ENV, &state)
            .spawn()
            .map_err(|e| format!("couldn't start {}: {e}", claude.display()))?;
        let end = supervise(&mut child, &state);
        let _ = fs::remove_file(&state);
        let reaped = match end {
            End::Exited(status) => return Ok(exit_code(status)),
            End::Reaped(reaped) => reaped,
        };
        restore_terminal();
        let fields = json!({ "idle": reaped.idle, "reason": reaped.reason, "file": reaped.transcript });
        Engine::load().event(HOST, &reaped.session, "agent.reaped", fields);
        let later = format!("reaper claude --resume {}", shell_word(&reaped.session));
        if !resume_here(Some(reaped.idle), &reaped.reason, &later) {
            return Ok(ExitCode::SUCCESS);
        }
        args = resume_args(&args, &reaped.session);
    }
}

struct Reaped {
    session: String,
    transcript: String,
    idle: u64,
    reason: String,
}

enum End {
    Exited(ExitStatus),
    Reaped(Reaped),
}

fn supervise(child: &mut Child, state_file: &Path) -> End {
    let store = Store::default();
    let mut rules = RulesFile::load(paths::rules_file());
    let mut live: Option<(String, PathBuf)> = None;
    let mut beat = Instant::now();
    let mut was_idle = false;
    let mut reap_after: Option<u64> = None;
    let mut asked: Option<(Instant, Reaped)> = None;
    loop {
        match child.try_wait() {
            Ok(None) => {}
            done => {
                if let Some((_, file)) = &live {
                    store.forget(file);
                }
                let status = match done {
                    Ok(Some(status)) => status,
                    _ => child.wait().unwrap_or_default(),
                };
                return match asked {
                    Some((_, reaped)) => End::Reaped(reaped),
                    None => End::Exited(status),
                };
            }
        }
        if let Some((at, _)) = &asked {
            if at.elapsed() > GRACE {
                let _ = child.kill();
            }
            thread::sleep(TICK);
            continue;
        }

        rules.refresh();
        let config = &rules.config;
        let state: Option<Value> = fs::read(state_file).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let session = state.as_ref().and_then(|s| s["session"].as_str()).map(str::to_string);
        // Typing counts, though no hook hears it: the terminal keeps the time of the last key.
        let idle_since =
            state.as_ref().and_then(|s| s["idleSince"].as_i64()).map(|since| since.max(typed_at().unwrap_or(0)));
        let now = now();

        if let Some(session) = &session
            && live.as_ref().is_none_or(|(s, _)| s != session)
        {
            if let Some((_, file)) = &live {
                store.forget(file);
            }
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
            if Path::new(transcript).is_file() && !background_work(child.id()) {
                let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
                let reaped = Reaped {
                    session: session.clone(),
                    transcript: transcript.to_string(),
                    idle: (now - since).max(0) as u64,
                    reason,
                };
                asked = Some((Instant::now(), reaped));
            }
        }
        thread::sleep(TICK);
    }
}

/// When a key was last pressed in this terminal, in ms (to the second, or coarser: Linux
/// rounds a terminal's times to 8 s). The same time `w` shows as IDLE.
fn typed_at() -> Option<i64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !std::io::stdin().is_terminal() {
            return None;
        }
        Some(fs::metadata("/dev/stdin").ok()?.atime() * 1000)
    }
    #[cfg(not(unix))]
    None
}

/// When `ps` can't say, the answer is yes: nothing is reaped on a guess.
fn background_work(root: u32) -> bool {
    let Ok(out) = Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output() else { return true };
    let procs: Vec<(u32, u32, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            let comm = parts.collect::<Vec<_>>().join(" ");
            Some((pid, ppid, comm))
        })
        .collect();
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

/// In case Claude Code had no chance to put the terminal back.
fn restore_terminal() {
    if !std::io::stdin().is_terminal() {
        return;
    }
    let _ = Command::new("stty").arg("sane").stdin(Stdio::inherit()).status();
    // Show the cursor, and stop bracketed paste and mouse reports.
    let _ = std::io::stderr().write_all(b"\x1b[?25h\x1b[?2004l\x1b[?1000l\x1b[?1006l");
}

/// Claude Code's arguments for resuming `session`: the ones that set up the session (model,
/// permissions, tools, settings, MCP servers…), plus `--resume`. Unlike pi, Claude Code isn't
/// pinned, so only flags known to be safe to repeat are kept; everything else, the first
/// prompt included, is left out. Flags taking lists take every word up to the next flag,
/// as Claude Code's own parser does.
pub fn resume_args(args: &[String], session: &str) -> Vec<String> {
    const WITH_VALUE: &[&str] = &[
        "--model",
        "--permission-mode",
        "--settings",
        "--setting-sources",
        "--append-system-prompt",
        "--system-prompt",
        "--system-prompt-snapshot",
        "--effort",
        "--agent",
        "--agents",
        "--fallback-model",
        "--plugin-dir",
        "--autocompact",
    ];
    const LISTS: &[&str] = &[
        "--add-dir",
        "--allowedTools",
        "--allowed-tools",
        "--disallowedTools",
        "--disallowed-tools",
        "--mcp-config",
        "--tools",
        "--betas",
    ];
    const ALONE: &[&str] = &[
        "--dangerously-skip-permissions",
        "--allow-dangerously-skip-permissions",
        "--strict-mcp-config",
        "--ide",
        "--chrome",
        "--no-chrome",
        "--verbose",
        "--brief",
        "--exclude-dynamic-system-prompt-sections",
        "--ax-screen-reader",
    ];
    let mut kept = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        if arg.contains('=') && (WITH_VALUE.contains(&name) || LISTS.contains(&name)) || ALONE.contains(&name) {
            kept.push(arg.clone());
        } else if WITH_VALUE.contains(&name) {
            kept.push(arg.clone());
            kept.extend(it.next().cloned());
        } else if LISTS.contains(&name) {
            kept.push(arg.clone());
            while let Some(value) = it.next_if(|next| !next.starts_with('-')) {
                kept.push(value.clone());
            }
        }
    }
    kept.extend(["--resume".to_string(), session.to_string()]);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_resumed_session_keeps_its_setup_but_not_its_first_prompt() {
        let original = args(&[
            "--model",
            "opus",
            "--add-dir",
            "../lib",
            "../docs",
            "--permission-mode=plan",
            "--verbose",
            "-c",
            "--name",
            "fix",
            "--frobnicate",
            "loudly",
            "fix the tests",
        ]);
        assert_eq!(
            resume_args(&original, "abc"),
            args(&[
                "--model",
                "opus",
                "--add-dir",
                "../lib",
                "../docs",
                "--permission-mode=plan",
                "--verbose",
                "--resume",
                "abc"
            ])
        );
        let again = resume_args(&resume_args(&original, "abc"), "abc");
        assert_eq!(again, resume_args(&original, "abc"), "resuming again changes nothing");
        assert_eq!(resume_args(&args(&["-r", "old", "-p", "go"]), "new"), args(&["--resume", "new"]));
    }

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
