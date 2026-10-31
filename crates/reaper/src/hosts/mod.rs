//! Agent tools reaper supervises. Each translates its tool's events into engine checks
//! and carries out the decisions as far as the tool allows.

pub mod claude_code;
pub mod pi;

use std::io::{self, IsTerminal};
use std::process::{ExitCode, ExitStatus};

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

fn shell_word(text: &str) -> String {
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Says why a reaped session is gone and asks whether to resume it here; true to resume.
/// Without a terminal to ask on, it only prints `later`, the command to resume it with.
fn resume_here(idle: Option<u64>, reason: &str, later: &str) -> bool {
    let sat = idle.map(|ms| format!(" for {}", minutes(ms))).unwrap_or_default();
    let why = match reason {
        "crowded" => format!("too many sessions were running and this one had sat idle longest{sat}, so it made room"),
        "memory" => format!("memory ran short and this session had sat idle longest{sat}, so it made room"),
        _ => format!("this session sat idle{sat}, so it was shut down to free what it held"),
    };
    eprintln!("\nreaper: {why}. It is saved.");
    if !io::stdin().is_terminal() {
        eprintln!("reaper: to pick it up again: {later}");
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

fn minutes(ms: u64) -> String {
    match ms / 1000 {
        s @ 0..60 => format!("{s}s"),
        s @ 60..3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}
