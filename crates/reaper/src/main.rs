//! reaper: rules for coding agents that the agents never see.

mod console;
mod engine;
mod hosts;

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use reaper_core::lint::{Level, lint};
use reaper_core::paths::{self, tilde};
use reaper_core::rules::{ParseError, parse};

fn usage() -> String {
    format!(
        "reaper: rules for coding agents that the agents never see.

usage:
  reaper [console options]       the console: agents, kills, rules
  reaper run [--pi PATH] [ARGS]  a guarded pi session in this directory (ARGS go to pi)
  reaper claude [ARGS]           a guarded Claude Code session, reaped when idle (ARGS go to claude)
  reaper install claude-code     guard Claude Code sessions (adds hooks to its settings)
  reaper uninstall claude-code   stop guarding them
  reaper lint [FILE]             check a rules file
  reaper rules [--json] [FILE]   show the rules (--json: for scripts)

used by agents, not by hand:
  reaper engine                  one JSON request on stdin (the pi extension, and reaper itself)
  reaper hook claude-code        one Claude Code hook event on stdin

console options:
  --rules FILE    rules file (default {rules})
  --events FILE   event log (default {events})
  --dump TAB      print one frame of `live`, `rules` or `mori` as plain text, then exit
  --size WxH      frame size for --dump (default 100x30)
  --color         keep the colours in --dump, as ANSI escapes
  --at MS         memento mori at this time: ms since the epoch, or +MS/-MS from the newest event
  --session N     memento mori follows session N (in order of appearance)

memento mori (tab 3): ←→ switch session, ↑↓ pick a character, enter shows it in the live tab,
esc lets go (twice: back to following the action), d plays a death for fun.
NO_COLOR is respected.

`reaper run` fetches pi {pi} once (checksum-verified) unless --pi or $REAPER_PI names one.
A session left idle (`reap.idle` in the rules, default {idle}) is shut down to free its memory,
sooner when `reap.live` or `reap.free` say room is short; `reaper run` then offers to resume
it in place, conversation and settings intact.
",
        rules = tilde(&paths::rules_file()),
        events = tilde(&paths::events_file()),
        pi = hosts::pi::release::VERSION,
        idle = reaper_core::rules::DEFAULT_IDLE,
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => hosts::pi::run::launch(&args[1..]),
        Some("claude") => hosts::claude_code::run::launch(&args[1..]),
        Some("engine") => engine(),
        Some("hook") => hook(&args[1..]),
        Some("install") => hosts::claude_code::install::run(&args[1..], false).map(|()| ExitCode::SUCCESS),
        Some("uninstall") => hosts::claude_code::install::run(&args[1..], true).map(|()| ExitCode::SUCCESS),
        Some("lint") => lint_file(&args[1..]),
        Some("rules") => show_rules(&args[1..]),
        Some("console") => console::main(&args[1..]),
        Some("-h" | "--help" | "help") => {
            print!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        Some("-V" | "--version") => {
            println!("reaper {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        _ => console::main(&args),
    };
    result.unwrap_or_else(|e| {
        eprintln!("reaper: {e}");
        ExitCode::from(2)
    })
}

fn stdin_json() -> Result<serde_json::Value, String> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).map_err(|e| format!("can't read stdin: {e}"))?;
    serde_json::from_str(&input).map_err(|e| format!("stdin isn't JSON: {e}"))
}

fn engine() -> Result<ExitCode, String> {
    let request = stdin_json()?;
    println!("{}", hosts::pi::reply(&engine::Engine::load(), &request));
    Ok(ExitCode::SUCCESS)
}

/// Problems are reported on stderr with a zero exit, so a broken reaper never blocks the
/// session (hooks fail open).
fn hook(args: &[String]) -> Result<ExitCode, String> {
    if args.first().map(String::as_str) != Some("claude-code") {
        return Err("usage: reaper hook claude-code".into());
    }
    match stdin_json() {
        Ok(event) => {
            if let Some(warm) = hosts::claude_code::warm_request(&event) {
                tell_engine(&warm);
            }
            if let Some(output) = hosts::claude_code::reply(&engine::Engine::load(), &event) {
                println!("{output}");
            }
        }
        Err(e) => eprintln!("reaper: {e}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Nobody waits for this engine: it carries on after the hook exits.
fn tell_engine(request: &serde_json::Value) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(me) = std::env::current_exe() else { return };
    let child =
        Command::new(me).arg("engine").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    if let Ok(mut child) = child
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(request.to_string().as_bytes());
    }
}

fn where_(e: &ParseError) -> String {
    match (e.line, e.column) {
        (Some(l), Some(c)) => format!(":{l}:{c}"),
        (Some(l), None) => format!(":{l}"),
        _ => String::new(),
    }
}

fn read_rules(args: &[String]) -> Result<(PathBuf, String), String> {
    let path = args.iter().find(|a| !a.starts_with("--")).map_or_else(paths::rules_file, PathBuf::from);
    match fs::read_to_string(&path) {
        Ok(text) => Ok((path, text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((path, String::new())),
        Err(e) => Err(format!("can't read {}: {e}", path.display())),
    }
}

fn show_rules(args: &[String]) -> Result<ExitCode, String> {
    let (path, text) = read_rules(args)?;
    let config = parse(&text).map_err(|e| format!("{}{}: {}", path.display(), where_(&e), e.message))?;
    if args.iter().any(|a| a == "--json") {
        println!("{}", config.to_json());
        return Ok(ExitCode::SUCCESS);
    }
    for (place, rule) in config.all_rules() {
        let scope = rule.scope().map_or("all tools".to_string(), |tools| tools.join(","));
        println!("{:<14} {:<22} {:<16} {scope}", place.unwrap_or("global"), rule.id, rule.checked_by());
    }
    Ok(ExitCode::SUCCESS)
}

fn lint_file(args: &[String]) -> Result<ExitCode, String> {
    let (path, text) = read_rules(args)?;
    let config = match parse(&text) {
        Ok(config) => config,
        Err(e) => {
            println!("error {}{}: {}", tilde(&path), where_(&e), e.message);
            return Ok(ExitCode::FAILURE);
        }
    };
    let issues = lint(&config);
    for issue in &issues {
        let level = match issue.level {
            Level::Error => "error",
            Level::Warn => "warn ",
        };
        let at = match (&issue.place, &issue.rule) {
            (Some(place), Some(rule)) => format!("{place}/{rule}: "),
            _ => String::new(),
        };
        println!("{level} {at}{}", issue.message);
    }
    if issues.is_empty() {
        println!("ok: {} rules in {}", config.all_rules().count(), tilde(&path));
    }
    Ok(if issues.iter().any(|i| i.level == Level::Error) { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}
