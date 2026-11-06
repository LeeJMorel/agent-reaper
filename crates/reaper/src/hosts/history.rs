//! The tool calls agents made in the person's recent sessions, read from the transcripts Claude
//! Code and pi keep on disk, newest session first. A new rule is tried on them before it goes
//! in, so the person sees what it would have stopped in their own work.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use reaper_core::paths;
use serde_json::Value;

/// How far back sessions are read, and how much of them at most: a busy month is a few GB, so
/// the budget keeps a check to a few seconds by reading the newest first.
const DAYS: u64 = 30;
const BUDGET: u64 = 768 << 20;

/// Where each agent keeps its sessions.
pub struct Roots {
    pub claude_code: PathBuf,
    pub pi: PathBuf,
}

impl Roots {
    /// This machine's, where the agents look for them themselves. Unit tests have none: they
    /// never read the person's own.
    pub fn here() -> Self {
        if cfg!(test) {
            return Self { claude_code: PathBuf::new(), pi: PathBuf::new() };
        }
        let dir = |var: &str, default: &str| {
            std::env::var_os(var).filter(|v| !v.is_empty()).map_or_else(|| paths::home().join(default), PathBuf::from)
        };
        Self {
            claude_code: dir("CLAUDE_CONFIG_DIR", ".claude").join("projects"),
            pi: dir("PI_CODING_AGENT_DIR", ".pi/agent").join("sessions"),
        }
    }
}

/// One tool call, and the directory its session worked in.
pub struct Call<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub dir: &'a Path,
}

/// Hands every call to `f`, newest session first, and says how many days back the sessions it
/// read go. `None` when there were none.
pub fn each(roots: &Roots, mut f: impl FnMut(Call)) -> Option<u64> {
    let now = SystemTime::now();
    let mut files = Vec::new();
    for root in [&roots.claude_code, &roots.pi] {
        transcripts(root, 3, &mut files);
    }
    let recent = |at: &SystemTime| now.duration_since(*at).unwrap_or_default() < Duration::from_secs(DAYS * 86_400);
    files.retain(|(at, _, _)| recent(at));
    files.sort_by_key(|(at, _, _)| std::cmp::Reverse(*at));
    let (mut read, mut oldest) = (0, None);
    for (at, len, path) in files {
        if read > 0 && read + len > BUDGET {
            break;
        }
        read += len;
        oldest = Some(at);
        calls(&path, &mut f);
    }
    oldest.map(|at| now.duration_since(at).unwrap_or_default().as_secs().div_ceil(86_400).max(1))
}

/// The `.jsonl` files under `dir`, `depth` levels down at most (Claude Code keeps a session's
/// subagents a level below it), with when each last changed and how big it is.
fn transcripts(dir: &Path, depth: usize, out: &mut Vec<(SystemTime, u64, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        if meta.is_dir() && depth > 0 {
            transcripts(&path, depth - 1, out);
        } else if meta.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            out.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), path));
        }
    }
}

/// The calls in one transcript. Claude Code writes each one with the session's directory:
/// `{"type":"assistant","cwd":…,"message":{"content":[{"type":"tool_use","name":…,"input":…}]}}`.
/// pi names its directory once, in the session's first line, then writes
/// `{"type":"message","message":{"content":[{"type":"toolCall","name":…,"arguments":…}]}}`.
fn calls(path: &Path, f: &mut impl FnMut(Call)) {
    let Ok(file) = File::open(path) else { return };
    let mut session = PathBuf::new();
    for line in BufReader::new(file).split(b'\n').map_while(Result::ok) {
        let Ok(line) = std::str::from_utf8(&line) else { continue };
        let header = line.starts_with(r#"{"type":"session""#);
        if !header && !line.contains(r#""tool_use""#) && !line.contains(r#""toolCall""#) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        if header {
            session = PathBuf::from(entry["cwd"].as_str().unwrap_or_default());
            continue;
        }
        let own = entry["cwd"].as_str().map(PathBuf::from);
        let dir = own.as_deref().unwrap_or(&session);
        for block in entry["message"]["content"].as_array().into_iter().flatten() {
            let args = match block["type"].as_str() {
                Some("tool_use") => &block["input"],
                Some("toolCall") => &block["arguments"],
                _ => continue,
            };
            if let Some(tool) = block["name"].as_str() {
                f(Call { tool, args, dir });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn calls_come_from_both_agents_transcripts_with_their_directories() {
        let root = std::env::temp_dir().join(format!("reaper-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let roots = Roots { claude_code: root.join("claude/projects"), pi: root.join("pi/sessions") };
        let claude = roots.claude_code.join("-w-app/s1/subagents");
        fs::create_dir_all(&claude).unwrap();
        let assistant = json!({ "type": "assistant", "cwd": "/w/app", "message": { "content": [
            { "type": "text", "text": "Running the tests." },
            { "type": "tool_use", "name": "Bash", "input": { "command": "pytest" } },
        ] } });
        let result = json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "x", "content": "ok" }] } });
        fs::write(claude.join("agent-1.jsonl"), format!("{assistant}\n{result}\nnot json\n")).unwrap();
        let pi = roots.pi.join("--w-lib--");
        fs::create_dir_all(&pi).unwrap();
        let header = r#"{"type":"session","version":3,"id":"p1","cwd":"/w/lib"}"#;
        let call = json!({ "type": "message", "message": { "role": "assistant", "content": [
            { "type": "toolCall", "id": "c1", "name": "edit", "arguments": { "path": "src/a.ts" } },
        ] } });
        fs::write(pi.join("1_p1.jsonl"), format!("{header}\n{call}\n")).unwrap();

        let mut seen = Vec::new();
        let days = each(&roots, |c| seen.push((c.tool.to_string(), c.args.clone(), c.dir.to_path_buf())));
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            seen,
            [
                ("Bash".to_string(), json!({ "command": "pytest" }), PathBuf::from("/w/app")),
                ("edit".to_string(), json!({ "path": "src/a.ts" }), PathBuf::from("/w/lib")),
            ]
        );
        assert_eq!(days, Some(1));
        assert_eq!(each(&Roots { claude_code: root.join("none"), pi: root.join("none") }, |_| {}), None);
        let _ = fs::remove_dir_all(&root);
    }
}
