//! Opening `reaper` sets it up: the agents on the machine are guarded without anyone
//! installing anything, and an agent the human took reaper out of stays out. HOME is a
//! sandbox with Claude Code's and pi's folders in it, and neither on PATH.

mod common;

use std::fs;
use std::process::Command;

use common::{SCRIPT, Terminal, has_terminal};

#[test]
fn opening_reaper_guards_the_agents_it_finds() {
    if !has_terminal() {
        return;
    }
    let root = common::sandbox("setup");
    let home = root.join("home");
    for dir in [".claude", ".pi/agent"] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    let command = |program: &str| {
        let mut command = Command::new(program);
        command.env_clear().env("HOME", &home).env("PATH", common::minimal_path(&root, &["sh", "stty"]));
        command
    };
    let open = |until: &str| {
        // The terminal `script` makes has no size until it's given one.
        let line = format!("stty cols 100 rows 30; exec '{}'", env!("CARGO_BIN_EXE_reaper"));
        let mut terminal = Terminal::open(command(SCRIPT), &line);
        common::wait_until(until, || plain(&terminal.screen.text()).contains(until));
        terminal.press(b"q");
        plain(&terminal.close())
    };

    let screen = open("Now guarding Claude Code and pi");
    assert!(fs::read_to_string(home.join(".claude/settings.json")).unwrap().contains(" hook claude-code"));
    assert!(home.join(".pi/agent/extensions/reaper/index.ts").is_file(), "{screen}");

    let out = command(env!("CARGO_BIN_EXE_reaper")).args(["uninstall", "pi", "--yes"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let screen = open("Start Claude Code the way");
    assert!(!screen.contains("Now guarding"), "nothing new to guard:\n{screen}");
    assert!(!home.join(".pi/agent/extensions/reaper").exists(), "an agent taken out stays out");

    // An older reaper's hooks, elsewhere and without the newest event, are brought up to date
    // without a word, and keep pointing where they did.
    let settings = home.join(".claude/settings.json");
    let read = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&settings).unwrap()).unwrap();
    let mut old = read();
    old["hooks"].as_object_mut().unwrap().remove("SessionEnd");
    fs::write(&settings, old.to_string().replace(env!("CARGO_BIN_EXE_reaper"), "/old/reaper")).unwrap();
    let screen = open("Start Claude Code the way");
    assert!(!screen.contains("Now guarding"), "{screen}");
    assert_eq!(read()["hooks"]["SessionEnd"][0]["hooks"][0]["command"], "/old/reaper hook claude-code");
}

/// The words on screen, in order: a terminal UI moves the cursor over blank cells instead of
/// writing spaces.
fn plain(raw: &str) -> String {
    let mut text = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            text.push(c);
            continue;
        }
        text.push(' ');
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() || c == '~' {
                    break;
                }
            }
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
