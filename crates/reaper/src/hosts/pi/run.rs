//! `reaper run`: pi in this terminal, with the extension loaded. reaper stays pi's parent, a
//! few MB beside it; pi suspends its whole process group, so job control (ctrl-z, `fg`) stops
//! and resumes both.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reaper_core::paths;
use serde_json::{Map, Value, json};

use super::{extension, release};
use crate::hosts::{SHELLS, arguments, exit_code, process, program, resume_here, shell_word};

/// Set for the pi that `reaper run` starts: the file where the engine leaves word that the
/// extension reaped the session, and which session file to resume.
const RUN_NOTE: &str = "REAPER_RUN";

/// Called by the engine on `agent.reaped`, to tell the `reaper run` waiting on this pi, if any.
/// False when there's none: the human has to be told how to resume.
pub fn reaped(event: &Map<String, Value>) -> bool {
    let Some(note) = env::var_os(RUN_NOTE) else { return false };
    let note_text = json!({ "file": event.get("file"), "idle": event.get("idle"), "reason": event.get("reason") });
    let _ = fs::write(note, note_text.to_string());
    true
}

/// The command that resumes `session` (its id, as pi's own goodbye names it) in a pi
/// `reaper run` didn't start: what started it, as far as the system shows (Linux), with
/// `resume_args`. The engine asking is pi's child.
pub fn resume_command(session: &str) -> String {
    #[cfg(unix)]
    let started = {
        let pi = std::os::unix::process::parent_id();
        let launcher = process(pi).and_then(|(parent, _)| arguments(parent));
        let own = arguments(pi);
        launcher
            .and_then(|argv| script(&argv))
            .or_else(|| own.map(|argv| script(&argv).unwrap_or_else(|| split(&argv))))
    };
    #[cfg(not(unix))]
    let started: Option<(String, Vec<String>)> = None;
    let (name, args) = started.unwrap_or_else(|| ("pi".to_string(), Vec::new()));
    std::iter::once(name).chain(resume_args(&args, session)).map(|w| shell_word(&w)).collect::<Vec<_>>().join(" ")
}

fn split(argv: &[String]) -> (String, Vec<String>) {
    (program(&argv[0]).to_string(), argv[1..].to_vec())
}

/// A script run by its interpreter, as a process shows it (`python3 ~/.local/bin/x args…`):
/// the script is what was typed. A shell with no file to run is just where it was typed.
fn script(argv: &[String]) -> Option<(String, Vec<String>)> {
    let interpreter = program(argv.first()?);
    let known = SHELLS.contains(&interpreter)
        || interpreter.starts_with("python")
        || ["node", "bun", "deno", "ruby", "perl"].contains(&interpreter);
    argv.get(1).filter(|file| known && !file.starts_with('-')).map(|_| split(&argv[1..]))
}

/// `reaper run [--pi PATH] [pi arguments…]`. Use `--` before pi arguments that clash with ours.
pub fn launch(args: &[String]) -> Result<ExitCode, String> {
    let (mut pi_path, mut args_for_pi) = (None, Vec::new());
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--pi" => pi_path = Some(PathBuf::from(it.next().ok_or("--pi needs a path")?)),
            "--" => {
                args_for_pi.extend(it.by_ref().cloned());
                break;
            }
            _ => args_for_pi.push(arg.clone()),
        }
    }
    let pi = release::binary(pi_path)?;
    let extension = extension::install().map_err(|e| format!("couldn't unpack the pi extension: {e}"))?;
    let me = env::current_exe().map_err(|e| format!("can't locate the reaper binary: {e}"))?;
    let note = paths::state_dir().join("runs").join(std::process::id().to_string());
    fs::create_dir_all(note.parent().expect("runs dir"))
        .map_err(|e| format!("can't create {}: {e}", note.display()))?;
    let mut args = args_for_pi;
    loop {
        let _ = fs::remove_file(&note);
        let status = Command::new(&pi)
            .arg("-e")
            .arg(&extension)
            .args(&args)
            .env("REAPER_BIN", &me)
            .env(RUN_NOTE, &note)
            .status()
            .map_err(|e| format!("couldn't start {}: {e}", pi.display()))?;
        let reaped: Option<Value> = fs::read(&note).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let _ = fs::remove_file(&note);
        let Some(file) = reaped.as_ref().and_then(|r| r["file"].as_str()) else {
            return Ok(exit_code(status));
        };
        let idle = reaped.as_ref().and_then(|r| r["idle"].as_u64());
        let reason = reaped.as_ref().and_then(|r| r["reason"].as_str()).unwrap_or("idle");
        if !resume_here(idle, reason, &format!("reaper run --session {}", shell_word(file))) {
            return Ok(ExitCode::SUCCESS);
        }
        args = resume_args(&args, file);
    }
}

/// pi's arguments for resuming `session`: the original ones, minus everything that chose or
/// started a session (the first prompt, `@files`, `--continue`, `--session`, `--print`…),
/// plus `--session`. The rest (model, tools, extensions…) stays, so the resumed session sends
/// the same prefix and a provider's prompt cache still applies. Mirrors pi's own parser for
/// the pinned version: flags it reads a value after, and unknown `--flags` (from extensions)
/// that take the next word unless it looks like a flag or a file.
pub fn resume_args(args: &[String], session: &str) -> Vec<String> {
    const KEEP_WITH_VALUE: &[&str] = &[
        "--provider",
        "--model",
        "--api-key",
        "--system-prompt",
        "--append-system-prompt",
        "--session-dir",
        "--models",
        "--tools",
        "-t",
        "--exclude-tools",
        "-xt",
        "--thinking",
        "--extension",
        "-e",
        "--skill",
        "--prompt-template",
        "--theme",
        "--use-theme",
        "--tui-mode",
    ];
    const KEEP_ALONE: &[&str] = &[
        "--no-tools",
        "--no-builtin-tools",
        "--no-extensions",
        "--no-mcp",
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--no-context-files",
        "--verbose",
        "--approve",
        "--no-approve",
        "--offline",
    ];
    const DROP_WITH_VALUE: &[&str] = &["--session", "--session-id", "--fork", "--name", "-n", "--mode", "--export"];
    const DROP: &[&str] = &["--continue", "-c", "--resume", "-r", "--print", "-p", "--no-session"];
    let mut kept = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let a = arg.as_str();
        if a == "--" {
            break; // everything after is the first prompt
        } else if KEEP_WITH_VALUE.contains(&a) {
            kept.push(arg.clone());
            kept.extend(it.next().cloned());
        } else if DROP_WITH_VALUE.contains(&a) {
            it.next();
        } else if DROP.contains(&a) || !a.starts_with('-') {
            // a prompt or an @file, or a flag that picks or ends the session
        } else if a.starts_with("--") && !a.contains('=') && !KEEP_ALONE.contains(&a) {
            kept.push(arg.clone());
            if it.peek().is_some_and(|next| !next.starts_with('-') && !next.starts_with('@')) {
                kept.extend(it.next().cloned());
            }
        } else {
            kept.push(arg.clone());
        }
    }
    kept.extend(["--session".to_string(), session.to_string()]);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_script_is_what_was_typed_and_a_shell_is_only_where() {
        let launcher = args(&["/home/u/.venv/bin/python3", "/home/u/.local/bin/fcc-pi", "--model", "x"]);
        assert_eq!(script(&launcher), Some(("fcc-pi".to_string(), args(&["--model", "x"]))));
        assert_eq!(script(&args(&["node", "/usr/local/bin/pi"])), Some(("pi".to_string(), vec![])));
        assert_eq!(script(&args(&["bash", "/home/u/bin/mypi", "-c"])), Some(("mypi".to_string(), args(&["-c"]))));
        assert_eq!(script(&args(&["-zsh"])), None);
        assert_eq!(script(&args(&["sh", "-c", "pi --model x"])), None);
        assert_eq!(script(&args(&["tmux", "new"])), None);
        assert_eq!(split(&args(&["/opt/pi/pi", "--model", "x"])), ("pi".to_string(), args(&["--model", "x"])));
    }

    #[test]
    fn a_resumed_session_keeps_its_setup_but_not_its_first_prompt() {
        let original = args(&[
            "--model",
            "mock/m",
            "-t",
            "read,bash",
            "--offline",
            "-c",
            "--name",
            "fix",
            "@notes.md",
            "--plan-mode",
            "strict",
            "--verbose",
            "fix the tests",
            "--",
            "-more prompt",
        ]);
        assert_eq!(
            resume_args(&original, "/s/a.jsonl"),
            args(&[
                "--model",
                "mock/m",
                "-t",
                "read,bash",
                "--offline",
                "--plan-mode",
                "strict",
                "--verbose",
                "--session",
                "/s/a.jsonl"
            ])
        );
        let again = resume_args(&resume_args(&original, "/s/a.jsonl"), "/s/a.jsonl");
        assert_eq!(again, resume_args(&original, "/s/a.jsonl"), "resuming again changes nothing");
        assert_eq!(resume_args(&args(&["-p", "go", "--session", "old"]), "new"), args(&["--session", "new"]));
    }
}
