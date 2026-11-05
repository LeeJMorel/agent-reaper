//! Typing into a Claude Code from outside it, for what its hooks can't do: compacting a session
//! reaper stopped, and setting it going again, when nobody is there to (auto mode and the like).
//! A Claude Code that a program drives (the editor's extension, an app) reads JSON lines from a
//! pipe, and on Linux another process may add a line to it, the way that program would. Claude
//! Code doesn't offer this, so an update could stop it working; then nothing is sent, and the
//! session carries on without being compacted.

use std::fs::{self, OpenOptions};
use std::io::Write;

use serde_json::json;

/// Sends `text` to the Claude Code `pid` as a user message, as the program driving it would: it
/// is taken once the turn under way is over. False when that can't be done here: in a terminal,
/// with no /proc to reach its input through, or when `pid` isn't Claude Code at all.
pub fn send(pid: u32, text: &str) -> bool {
    claude(pid) && write_line(pid, text)
}

/// Only ever into Claude Code: never into a launcher, a shell, or whatever else ran the hook.
fn claude(pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|argv| String::from_utf8_lossy(&argv).contains("claude"))
}

fn write_line(pid: u32, text: &str) -> bool {
    let input = format!("/proc/{pid}/fd/0");
    let piped = fs::read_link(&input).is_ok_and(|p| p.to_string_lossy().starts_with("pipe:"));
    // One write, short of what a pipe takes in one piece, so it never lands inside another line.
    let line = format!("{}\n", json!({ "type": "user", "message": { "role": "user", "content": text } }));
    piped && OpenOptions::new().write(true).open(&input).and_then(|mut pipe| pipe.write_all(line.as_bytes())).is_ok()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn a_line_reaches_a_program_reading_its_input_from_a_pipe_and_a_terminal_is_left_alone() {
        if !std::path::Path::new("/proc/self/fd/0").exists() {
            return;
        }
        // `head -n 1` stands in for Claude Code: it reads its input and says what it got.
        let mut reader =
            Command::new("head").args(["-n", "1"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        assert!(!send(reader.id(), "/compact"), "not Claude Code: left alone");
        assert!(write_line(reader.id(), "/compact"));
        let mut got = String::new();
        BufReader::new(reader.stdout.take().unwrap()).read_line(&mut got).unwrap();
        let _ = reader.wait();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&got).unwrap()["message"]["content"], "/compact");
        // Its input a file: nothing to add a line to.
        let file = std::env::temp_dir().join(format!("reaper-inject-{}", std::process::id()));
        fs::write(&file, "").unwrap();
        let mut cat = Command::new("sleep").arg("5").stdin(fs::File::open(&file).unwrap()).spawn().unwrap();
        assert!(!write_line(cat.id(), "/compact"));
        let _ = cat.kill();
        let _ = cat.wait();
        let _ = fs::remove_file(file);
    }
}
