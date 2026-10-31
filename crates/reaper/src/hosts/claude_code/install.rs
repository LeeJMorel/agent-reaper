//! `reaper install claude-code` / `reaper uninstall claude-code`: adds or removes reaper's
//! hooks in Claude Code's user settings, leaving everything else in the file as it was.

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use reaper_core::paths::{home, tilde};
use serde_json::{Map, Value, json};

const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some("*")),
    ("Stop", None),
    ("SubagentStop", None),
    // "Waiting for your input": idle even when the last turn was interrupted (no `Stop`).
    ("Notification", None),
];

/// Enough for a watcher call (8 s by default) plus starting the process.
const TIMEOUT_SECONDS: u64 = 30;

pub fn settings_path() -> PathBuf {
    env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home().join(".claude"), PathBuf::from)
        .join("settings.json")
}

pub fn command(exe: &Path) -> String {
    let path = exe.to_string_lossy();
    if path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        format!("{path} hook claude-code")
    } else {
        format!("'{}' hook claude-code", path.replace('\'', r"'\''"))
    }
}

fn is_ours(handler: &Value) -> bool {
    handler["command"].as_str().is_some_and(|c| c.ends_with(" hook claude-code"))
}

pub fn uninstall(settings: &mut Value) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return false };
    let mut changed = false;
    for groups in hooks.values_mut().filter_map(Value::as_array_mut) {
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = handlers.len();
                handlers.retain(|h| !is_ours(h));
                changed |= handlers.len() != before;
            }
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().expect("settings is an object").remove("hooks");
    }
    changed
}

/// Replaces any older handlers of reaper's, e.g. from a moved binary.
pub fn install(settings: &mut Value, command: &str) {
    uninstall(settings);
    let root = settings.as_object_mut().expect("settings is an object");
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().expect("hooks is an object");
    for (event, matcher) in EVENTS {
        let mut group = json!({ "hooks": [{ "type": "command", "command": command, "timeout": TIMEOUT_SECONDS }] });
        if let Some(matcher) = matcher {
            group["matcher"] = (*matcher).into();
        }
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        if let Some(groups) = groups.as_array_mut() {
            groups.push(group);
        }
    }
}

pub fn installed() -> bool {
    let Ok(settings) = read(&settings_path()) else { return false };
    let groups = settings["hooks"].as_object().into_iter().flat_map(|h| h.values()).filter_map(Value::as_array);
    groups.flatten().filter_map(|g| g["hooks"].as_array()).flatten().any(is_ours)
}

/// For `claude --settings`: one session guarded, with nothing written to the user's settings.
pub fn hooks_only(command: &str) -> Value {
    let mut settings = json!({});
    install(&mut settings, command);
    settings
}

fn read(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value @ Value::Object(_)) => Ok(value),
            Ok(_) => Err(format!("{} isn't a JSON object; leaving it alone", tilde(path))),
            Err(e) => Err(format!("{} isn't valid JSON ({e}); leaving it alone", tilde(path))),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("can't read {}: {e}", tilde(path))),
    }
}

fn write(path: &Path, settings: &Value) -> Result<(), String> {
    let fail = |e: io::Error| format!("can't write {}: {e}", tilde(path));
    if path.exists() {
        fs::copy(path, path.with_extension("json.reaper-backup")).map_err(fail)?;
    }
    fs::create_dir_all(path.parent().expect("settings file has a directory")).map_err(fail)?;
    let mut text = serde_json::to_string_pretty(settings).expect("JSON value");
    text.push('\n');
    fs::write(path, text).map_err(fail)
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

pub fn run(args: &[String], remove: bool) -> Result<(), String> {
    let mut path = settings_path();
    let mut yes = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "claude-code" => {}
            "--yes" | "-y" => yes = true,
            "--settings" => path = PathBuf::from(it.next().ok_or("--settings needs a path")?),
            other => return Err(format!("unknown argument {other}; the only agent to install into is claude-code")),
        }
    }
    let mut settings = read(&path)?;
    if remove {
        if !uninstall(&mut settings) {
            println!("No reaper hooks in {}.", tilde(&path));
            return Ok(());
        }
    } else {
        let exe = env::current_exe().map_err(|e| format!("can't locate the reaper binary: {e}"))?;
        install(&mut settings, &command(&exe));
    }

    let what = if remove { "Remove reaper's hooks from" } else { "Add reaper's hooks to" };
    let events = EVENTS.iter().map(|(e, _)| *e).collect::<Vec<_>>().join(", ");
    println!("{what} {} ({events}). Other settings stay as they are.", tilde(&path));
    if !yes && !confirm("Go ahead?") {
        return Err("nothing changed (pass --yes to skip the question)".into());
    }
    write(&path, &settings)?;
    println!("Done. A copy of the previous file is next to it (*.reaper-backup).");
    if !remove {
        println!("New Claude Code sessions are now watched; open `reaper` to see them.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_keeps_other_settings_and_hooks_and_is_idempotent() {
        let mut settings = json!({
            "model": "opus",
            "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "my-check.sh" }] }] },
        });
        install(&mut settings, "/bin/reaper hook claude-code");
        install(&mut settings, "/new/reaper hook claude-code");
        assert_eq!(settings["model"], "opus");
        let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "the user's own hook stays, reaper's appears once");
        assert_eq!(pre[0]["hooks"][0]["command"], "my-check.sh");
        assert_eq!(
            pre[1],
            json!({ "matcher": "*", "hooks": [{ "type": "command", "command": "/new/reaper hook claude-code", "timeout": 30 }] })
        );
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["command"], "/new/reaper hook claude-code");
    }

    #[test]
    fn uninstall_removes_only_reapers_hooks() {
        let mut settings = json!({ "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "my-check.sh" }] }] } });
        let original = settings.clone();
        install(&mut settings, "/bin/reaper hook claude-code");
        assert!(uninstall(&mut settings));
        assert_eq!(settings, original);
        assert!(!uninstall(&mut settings), "nothing left to remove");

        let mut only_ours = json!({});
        install(&mut only_ours, "/bin/reaper hook claude-code");
        uninstall(&mut only_ours);
        assert_eq!(only_ours, json!({}));
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        assert_eq!(command(Path::new("/usr/local/bin/reaper")), "/usr/local/bin/reaper hook claude-code");
        assert_eq!(command(Path::new("/Users/Jo Smith/bin/reaper")), "'/Users/Jo Smith/bin/reaper' hook claude-code");
    }
}
