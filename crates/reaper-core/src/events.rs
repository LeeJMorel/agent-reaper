//! The agent event log (`events.jsonl`): one JSON object per line, appended by the
//! engine and by host adapters, read by the console. Nothing in it is shown to agents.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Map, Value};

/// Milliseconds since the Unix epoch, as events are stamped.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The line is written with a single call, so the many short-lived processes writing the
/// log (one per tool call) don't interleave.
pub fn append(path: &Path, host: &str, agent: &str, kind: &str, fields: Map<String, Value>) -> io::Result<()> {
    let mut event = Map::new();
    event.insert("at".into(), now().into());
    event.insert("host".into(), host.into());
    event.insert("agent".into(), agent.into());
    event.insert("type".into(), kind.into());
    event.extend(fields);
    let mut line = Value::Object(event).to_string();
    line.push('\n');
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Event {
    /// Milliseconds since the Unix epoch.
    pub at: i64,
    pub host: String,
    pub agent: String,
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum Kind {
    #[serde(rename = "agent.spawned")]
    Spawned {
        parent: Option<String>,
        project: Option<String>,
        dir: String,
        model: Option<String>,
        label: Option<String>,
    },
    /// The human asked the session for something. Cut short: some prompts are whole files.
    #[serde(rename = "agent.prompted")]
    Prompted { prompt: String },
    #[serde(rename = "agent.acted")]
    Acted { tool: String },
    #[serde(rename = "agent.checked")]
    Checked { tool: String, tier: Tier, ms: f64, verdict: Verdict, error: Option<String> },
    #[serde(rename = "agent.killed")]
    Killed {
        tool: String,
        rule: String,
        tier: Tier,
        strike: u32,
        #[serde(rename = "final")]
        is_final: bool,
        /// The blocked call's arguments, truncated. Absent in logs from before it was recorded.
        #[serde(default)]
        attempt: String,
        /// How much of its allowance the agent has used, this kill included: at 1 it's stopped.
        /// Absent in logs from before it was recorded.
        #[serde(default)]
        pressure: Option<f64>,
    },
    #[serde(rename = "agent.redirected")]
    Redirected {
        rule: String,
        #[serde(default)]
        redirect: String,
    },
    #[serde(rename = "agent.died")]
    Died { rule: String, reason: DeathReason },
    #[serde(rename = "agent.idle")]
    Idle,
    /// Shut down after sitting idle, to free what it held. Its session is saved, so nothing
    /// is lost: resuming it brings it back (as a new `agent.spawned` with the same id).
    #[serde(rename = "agent.reaped")]
    Reaped {
        /// How long it had been idle, in ms.
        idle: u64,
        #[serde(default)]
        reason: ReapReason,
    },
    /// The human closed the session (its subagents end with it). Resuming it brings it back,
    /// as for a reaped one.
    #[serde(rename = "agent.ended")]
    Ended,
    /// An event type this build doesn't know yet. Skipped, so newer engines don't break older consoles.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReapReason {
    /// It sat idle for `reap.idle`.
    #[default]
    Idle,
    /// More than `reap.live` sessions were running, and it had been idle longest.
    Crowded,
    /// Free memory was below `reap.free`, and it had been idle longest.
    Memory,
    #[serde(other)]
    Other,
}

impl ReapReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ReapReason::Idle => "idle",
            ReapReason::Crowded => "crowded",
            ReapReason::Memory => "memory",
            ReapReason::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Pattern,
    Cache,
    Watcher,
    Error,
    #[serde(other)]
    Other,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Pattern => "pattern",
            Tier::Cache => "cache",
            Tier::Watcher => "watcher",
            Tier::Error => "error",
            Tier::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Violation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeathReason {
    Strikes,
    Stop,
    /// Its session's agents broke rules too often between them: everyone in it was stopped.
    Together,
}

/// What the human wrote in a prompt, without the blocks agents wrap around it, like Claude
/// Code's `<ide_selection>…</ide_selection>` or `<system-reminder>…`. Those tags have a `-` or
/// `_` in their names, unlike HTML's. `None` when nothing the human wrote is left: a
/// background task reporting back.
pub fn written(text: &str) -> Option<&str> {
    let tag = |name: &str| {
        name.contains(['-', '_']) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let mut text = text.trim();
    loop {
        let before = text;
        if let Some(rest) = text.strip_prefix('<') {
            let name = rest.split(['>', ' ', '\n']).next().unwrap_or_default();
            let close = format!("</{name}>");
            if tag(name)
                && let Some(end) = text.find(&close)
            {
                text = text[end + close.len()..].trim_start();
            }
        }
        if let Some(open) = text.strip_suffix('>').and_then(|t| t.rfind("</")) {
            let name = &text[open + 2..text.len() - 1];
            if tag(name)
                && let Some(start) = text[..open].rfind(&format!("<{name}"))
            {
                text = text[..start].trim_end();
            }
        }
        if text == before {
            return (!text.is_empty()).then_some(text);
        }
    }
}

pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_is_what_the_human_wrote_without_what_their_editor_added() {
        let opened = "<ide_opened_file>The user opened a.rs.</ide_opened_file>still no prompt?";
        assert_eq!(written(opened), Some("still no prompt?"));
        let around =
            "<ide_selection lines=\"3\">fn a() {}</ide_selection>\n\nfix this\n<system-reminder>x</system-reminder>";
        assert_eq!(written(around), Some("fix this"));
        assert_eq!(written("<task-notification>done</task-notification>"), None, "a task reporting back");
        assert_eq!(written("<div>keep</div> as written"), Some("<div>keep</div> as written"), "HTML stays");
        assert_eq!(written("<ide_selection>cut off before the end"), Some("<ide_selection>cut off before the end"));
    }

    #[test]
    fn parses_every_kind_written_by_the_engine() {
        let log = include_str!("../tests/fixtures/events.jsonl");
        let events: Vec<Event> = log.lines().filter_map(parse_line).collect();
        assert_eq!(events.len(), log.lines().filter(|l| !l.trim().is_empty()).count());
        assert!(events.iter().all(|e| e.kind != Kind::Unknown));
        let killed = events.iter().find(|e| matches!(e.kind, Kind::Killed { .. })).unwrap();
        assert_eq!(
            killed.kind,
            Kind::Killed {
                tool: "bash".into(),
                rule: "no-violation-file".into(),
                tier: Tier::Watcher,
                strike: 1,
                is_final: false,
                attempt: r#"{"command":"touch violation.txt"}"#.into(),
                pressure: None,
            }
        );
    }

    #[test]
    fn unknown_types_and_junk_do_not_break_parsing() {
        let future = parse_line(r#"{"at":1,"host":"pi","agent":"a","type":"agent.teleported","to":"mars"}"#);
        assert_eq!(future.map(|e| e.kind), Some(Kind::Unknown));
        assert_eq!(parse_line("not json"), None);
        assert_eq!(parse_line("   "), None);
        let old = parse_line(r#"{"at":1,"host":"pi","agent":"a","type":"agent.redirected","rule":"r"}"#).unwrap();
        assert_eq!(old.kind, Kind::Redirected { rule: "r".into(), redirect: String::new() });
    }
}
