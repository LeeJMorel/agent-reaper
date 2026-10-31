//! Agent tools reaper supervises. Each translates its tool's events into engine checks
//! and carries out the decisions as far as the tool allows.

pub mod claude_code;
pub mod pi;

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, ExitStatus};

use reaper_core::paths;

/// Shells, as process names: one running under an agent is a command it started.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh"];

/// Guards the agents on this machine that aren't yet. Opening `reaper` does this, so nobody
/// has to install anything. An agent `reaper uninstall` took reaper out of stays out, and
/// nothing is touched while reaper's files are elsewhere (`REAPER_HOME`, `REAPER_STATE`: the
/// demo, tests), since installed hooks would never read them. Returns the agents it guarded
/// (id and name), and what went wrong, if anything.
pub fn guard() -> (Vec<(&'static str, &'static str)>, Option<String>) {
    let (mut guarded, mut problem) = (Vec::new(), None);
    if ["REAPER_HOME", "REAPER_STATE"].iter().any(|var| env::var_os(var).is_some()) {
        return (guarded, problem);
    }
    let Ok(me) = env::current_exe() else { return (guarded, Some("can't locate the reaper binary".into())) };
    let unless_chosen = |id: &str, present: bool, installed: bool, add: &dyn Fn() -> Result<(), String>| {
        (present && !installed && !left_alone(id).exists()).then(add)
    };
    let tried = [
        (
            "claude-code",
            "Claude Code",
            unless_chosen("claude-code", claude_code::install::present(), claude_code::install::installed(), &|| {
                claude_code::install::add(&me)
            }),
        ),
        ("pi", "pi", unless_chosen("pi", pi::install::present(), pi::install::installed(), &|| pi::install::add(&me))),
    ];
    for (id, name, result) in tried {
        match result {
            Some(Ok(())) => guarded.push((id, name)),
            Some(Err(e)) => problem = Some(format!("couldn't guard {name}: {e}")),
            None => {}
        }
    }
    (guarded, problem)
}

/// The agents reaper is installed in.
pub fn guarded() -> Vec<&'static str> {
    let agents = [("Claude Code", claude_code::install::installed()), ("pi", pi::install::installed())];
    agents.into_iter().filter(|(_, installed)| *installed).map(|(name, _)| name).collect()
}

/// Where `reaper uninstall` leaves word that an agent is to stay unguarded.
fn left_alone(agent: &str) -> PathBuf {
    paths::state_dir().join("uninstalled").join(agent)
}

/// Remembers the human's own `reaper install` or `reaper uninstall`, which `guard` keeps to.
fn chose(agent: &str, guarded: bool) {
    let mark = left_alone(agent);
    if guarded {
        let _ = fs::remove_file(mark);
    } else if fs::create_dir_all(mark.parent().expect("uninstalled/")).is_ok() {
        let _ = fs::write(mark, "");
    }
}

fn on_path(program: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|dirs| env::split_paths(&dirs).map(|dir| dir.join(program)).find(|p| p.is_file()))
}

fn exit_code(status: ExitStatus) -> ExitCode {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitCode::from((128 + signal).clamp(0, 255) as u8);
        }
    }
    ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8)
}

/// A program's name as a process list shows it: no directory, no login shell's leading `-`.
fn program(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path).trim_start_matches('-')
}

/// A process's parent, and its own name: from /proc where there is one, else from `ps`.
fn process(pid: u32) -> Option<(u32, String)> {
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        // `pid (name) state ppid …`, where the name may hold anything, brackets included.
        let (head, tail) = stat.rsplit_once(')')?;
        let ppid = tail.split_whitespace().nth(1)?.parse().ok()?;
        return Some((ppid, head.split_once('(')?.1.to_string()));
    }
    let out = Command::new("ps").args(["-o", "ppid=,comm=", "-p", &pid.to_string()]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (ppid, comm) = text.trim().split_once(char::is_whitespace)?;
    Some((ppid.parse().ok()?, program(comm.trim()).to_string()))
}

/// A process's arguments, the program first, where the system shows them (Linux).
fn arguments(pid: u32) -> Option<Vec<String>> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    (!bytes.is_empty()).then(|| bytes.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
}

fn shell_word(text: &str) -> String {
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Why a reaped session is gone, and how to pick it up again with `later`.
fn reaped(idle: Option<u64>, reason: &str, later: &str) -> [String; 2] {
    let sat = idle.map(|ms| format!(" for {}", minutes(ms))).unwrap_or_default();
    let why = match reason {
        "crowded" => format!("too many sessions were running and this one had sat idle longest{sat}, so it made room"),
        "memory" => format!("memory ran short and this session had sat idle longest{sat}, so it made room"),
        _ => format!("this session sat idle{sat}, so it was shut down to free what it held"),
    };
    [format!("reaper: {why}. It is saved."), format!("reaper: to pick it up again: {later}")]
}

/// Says why a reaped session is gone and asks whether to resume it here; true to resume.
/// Without a terminal to ask on, it only prints `later`, the command to resume it with.
fn resume_here(idle: Option<u64>, reason: &str, later: &str) -> bool {
    let [why, how] = reaped(idle, reason, later);
    eprintln!("\n{why}");
    if !io::stdin().is_terminal() {
        eprintln!("{how}");
        return false;
    }
    eprint!("reaper: press enter to resume it here, or ctrl-d to leave (later: {later}) ");
    let mut line = String::new();
    let resume = matches!(io::stdin().read_line(&mut line), Ok(n) if n > 0);
    if !resume {
        eprintln!();
    }
    resume
}

fn confirm(question: &str) -> bool {
    if !io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

fn minutes(ms: u64) -> String {
    match ms / 1000 {
        s @ 0..60 => format!("{s}s"),
        s @ 60..3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}
