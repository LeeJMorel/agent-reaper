//! The human's rules file. This is the only parser for it: the engine, the console
//! and `reaper lint` all read rules through here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Map, Value, json};

use crate::shell;

/// What the agent says (its replies), checked as if it were a call to a tool of this name with
/// one argument, `text`. Only rules that list it in `tools` ever see a message.
pub const MESSAGE: &str = "message";

/// A shell command's argument. Its patterns are tried on each command it runs (see `shell`).
pub const COMMAND: &str = "command";

/// The whole shell command, for a pattern across commands (a pipe, a loop), less what isn't
/// commands: comments, and heredocs fed to anything but a shell (see `shell::script`).
pub const SCRIPT: &str = "script";

/// Tools a judge-only rule covers when it doesn't list any: reads and searches skip the watcher.
pub const MUTATING_TOOLS: &[&str] =
    &["bash", "edit", "write", "patch", "multiedit", "apply_patch", "notebookedit", "task", "webfetch"];

/// Keys the engine doesn't read. Kept so lint can point out typos instead of silently ignoring them.
pub type Unknown = BTreeMap<String, IgnoredAny>;

pub const DEFAULT_IDLE: &str = "5m";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub watcher: Option<Watcher>,
    pub reap: Option<Reap>,
    #[serde(default = "default_strikes")]
    pub strikes: u32,
    /// The same tool call this many times in a row is a stuck agent; 0 turns that off.
    #[serde(default = "default_repeats")]
    pub repeats: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

fn default_strikes() -> u32 {
    3
}

fn default_repeats() -> u32 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Self {
            watcher: None,
            reap: None,
            strikes: default_strikes(),
            repeats: default_repeats(),
            rules: Vec::new(),
            projects: BTreeMap::new(),
            unknown: Unknown::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Watcher {
    pub kind: Option<String>,
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "apiKeyEnv")]
    pub api_key_env: Option<String>,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
    #[serde(rename = "cacheKey")]
    pub cache_key: Option<bool>,
    #[serde(rename = "onError")]
    pub on_error: Option<String>,
    /// Watchers to ask in turn when this one doesn't answer. They share its `timeoutMs` and
    /// `onError`.
    #[serde(default)]
    pub fallback: Vec<Watcher>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Watcher {
    /// A watcher of `kind` (`None`: OpenAI-compatible) answering with `model`, at `base_url`
    /// unless it's Claude Code.
    pub fn new(kind: Option<&str>, base_url: Option<&str>, model: &str) -> Self {
        Self {
            kind: kind.map(str::to_string),
            base_url: base_url.map(str::to_string),
            model: Some(model.to_string()),
            api_key_env: None,
            timeout_ms: None,
            cache_key: None,
            on_error: None,
            fallback: Vec::new(),
            unknown: Unknown::new(),
        }
    }

    /// As in `reaper rules --json`: the file's own key names, unknown keys left out.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        put(&mut map, "kind", self.kind.clone());
        put(&mut map, "baseURL", self.base_url.clone());
        put(&mut map, "model", self.model.clone());
        put(&mut map, "apiKeyEnv", self.api_key_env.clone());
        put(&mut map, "timeoutMs", self.timeout_ms);
        put(&mut map, "cacheKey", self.cache_key);
        put(&mut map, "onError", self.on_error.clone());
        if !self.fallback.is_empty() {
            map.insert("fallback".into(), self.fallback.iter().map(Watcher::to_json).collect());
        }
        map.into()
    }
}

/// Freeing what idle sessions hold. A session waiting on the human is the one least likely
/// to be needed soon, so it goes first, and without waiting for memory to run short: its
/// conversation is on disk, and resuming it sends the same prefix, so a provider's prompt
/// cache still applies. Under pressure, the longest idle go first; working sessions never.
#[derive(Debug, Clone, Deserialize)]
pub struct Reap {
    /// How long a session may sit idle before it's shut down: `90s`, `5m`, `1h`, `off`, or
    /// `auto`, learned from how long you take to come back (see `idle`).
    pub idle: Option<String>,
    /// At most this many sessions keep running; past it, the longest idle are reaped at once.
    pub live: Option<u32>,
    /// While free memory is below this (`512M`, `2G`), idle sessions are reaped, longest idle
    /// first. Read from /proc/meminfo, so Linux only.
    pub free: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// `512M`, `2G`, `1024K` in bytes (powers of 1024). `None` for anything else, zero included.
pub fn size_bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let (n, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let scale: u64 = match unit.trim().to_ascii_uppercase().trim_end_matches(['B', 'I']) {
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        _ => return None,
    };
    n.parse::<u64>().ok().filter(|&n| n > 0)?.checked_mul(scale)
}

/// Bytes as `512M` or `1.8G`, for telling an agent how much memory is free.
pub fn size_text(bytes: u64) -> String {
    if bytes >= 1 << 30 { format!("{:.1}G", bytes as f64 / f64::from(1u32 << 30)) } else { format!("{}M", bytes >> 20) }
}

/// `500ms`, `90s`, `5m`, `1h` in milliseconds. `None` for anything else, zero included.
pub fn duration_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    let (n, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let scale = match unit.trim() {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    n.parse::<u64>().ok().filter(|&n| n > 0)?.checked_mul(scale)
}

/// What the machine has to spare right now, for rules with `free` or `load`. `None`: not read,
/// or nothing to read it from.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Room {
    /// Free memory, in bytes.
    pub free: Option<u64>,
    /// The load average over the last minute.
    pub load: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    pub tools: Option<Vec<String>>,
    /// Tool argument name → regex (JavaScript syntax). `*` tests every argument.
    #[serde(rename = "match")]
    pub pattern: Option<BTreeMap<String, String>>,
    /// Plain-language description of the violation, for the watcher model.
    pub judge: Option<String>,
    /// What the agent is told after a kill.
    pub redirect: Option<String>,
    /// `redirect` (default) or `stop`.
    pub action: Option<String>,
    /// How many times an agent may break this rule before it's stopped, when not the global
    /// `strikes`.
    pub strikes: Option<u32>,
    /// Only while free memory is below this (`4G`): what matters is the room the machine has
    /// left, not how much an agent asks for. Read from /proc/meminfo, so Linux only.
    pub free: Option<String>,
    /// Only while the load average over the last minute is above this (`16`): the machine is
    /// busy. With `free` too, the rule applies while either is short: the call needs both.
    /// Read from /proc/loadavg, so Linux only.
    pub load: Option<f64>,
    /// The agent may ask the person to allow one call this rule blocked, with `reaper ask`.
    #[serde(default)]
    pub ask: bool,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Rule {
    /// Whether the rule applies with the machine's `room` now. A rule without `free` or `load`
    /// always does; one with them, while free memory is below `free` or the load is above
    /// `load`. What can't be read, or a setting that isn't a size, never counts (lint reports it).
    pub fn in_force(&self, room: &Room) -> bool {
        if self.free.is_none() && self.load.is_none() {
            return true;
        }
        self.short_of_memory(room) || self.busy(room)
    }

    pub fn short_of_memory(&self, room: &Room) -> bool {
        self.free.as_deref().and_then(size_bytes).zip(room.free).is_some_and(|(floor, free)| free < floor)
    }

    pub fn busy(&self, room: &Room) -> bool {
        self.load.zip(room.load).is_some_and(|(most, load)| load > most)
    }

    /// The rule as JSON, in the file's own key names, unknown keys left out.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".into(), self.id.clone().into());
        put(&mut map, "tools", self.tools.clone());
        put(&mut map, "match", self.pattern.as_ref().map(|m| json!(m)));
        put(&mut map, "judge", self.judge.clone());
        put(&mut map, "redirect", self.redirect.clone());
        put(&mut map, "action", self.action.clone());
        put(&mut map, "strikes", self.strikes);
        put(&mut map, "free", self.free.clone());
        put(&mut map, "load", self.load);
        put(&mut map, "ask", self.ask.then_some(true));
        map.into()
    }

    pub fn checked_by(&self) -> &'static str {
        match (self.pattern.is_some(), self.judge.is_some()) {
            (true, false) => "pattern",
            (false, true) => "watcher",
            (true, true) => "pattern+watcher",
            (false, false) => "nothing",
        }
    }

    /// Tools the rule applies to; `None` means every tool.
    pub fn scope(&self) -> Option<Vec<String>> {
        match (&self.tools, &self.pattern) {
            (Some(tools), _) => Some(tools.iter().map(|t| t.to_lowercase()).collect()),
            (None, Some(_)) => None,
            (None, None) => Some(MUTATING_TOOLS.iter().map(|t| t.to_string()).collect()),
        }
    }

    pub fn stops(&self) -> bool {
        self.action.as_deref() == Some("stop")
    }

    /// How many times an agent may break it: once for a `stop` rule, else its own `strikes`
    /// or the global ones (`all`).
    pub fn limit(&self, all: u32) -> u32 {
        if self.stops() { 1 } else { self.strikes.unwrap_or(all).max(1) }
    }

    pub fn applies_to(&self, tool: &str) -> bool {
        let tool = tool.to_lowercase();
        if tool == MESSAGE {
            // Opt-in only: a rule over every argument (`"*"`) mustn't start judging what the agent says.
            return self.tools.as_ref().is_some_and(|tools| tools.iter().any(|t| t.eq_ignore_ascii_case(MESSAGE)));
        }
        self.scope().is_none_or(|scope| scope.contains(&tool))
    }

    /// Whether every `match` entry hits the tool's arguments (a JSON object). Patterns are
    /// JavaScript regexes, searched anywhere in the value; `*` tests every argument as sent.
    /// `command` is tried on each command the shell would run, in each of its forms (see
    /// `shell`), and `script` on the whole shell command, for patterns across commands.
    /// A rule without `match` never matches here (it is the watcher's to judge).
    pub fn matches(&self, tool: &str, args: &Value) -> bool {
        let Some(patterns) = &self.pattern else { return false };
        if !self.applies_to(tool) {
            return false;
        }
        let runs = std::cell::OnceCell::new();
        let runs = || {
            runs.get_or_init(|| {
                let text = args.get(COMMAND).and_then(Value::as_str).unwrap_or_default();
                shell::commands(text).iter().flat_map(|words| shell::forms(words)).collect::<Vec<_>>()
            })
        };
        patterns.iter().all(|(key, pattern)| {
            let hit = |value: &Value| search(pattern, &arg_text(value));
            match key.as_str() {
                "*" => args.as_object().is_some_and(|args| args.values().any(hit)),
                COMMAND => runs().iter().any(|form| search(pattern, form)),
                // A tool with an argument of its own by that name keeps it.
                SCRIPT => match (args.get(SCRIPT), args.get(COMMAND).and_then(Value::as_str)) {
                    (Some(own), _) => hit(own),
                    (None, Some(command)) => search(pattern, &shell::script(command)),
                    (None, None) => false,
                },
                _ => args.get(key).is_some_and(hit),
            }
        })
    }
}

impl Rule {
    /// The command in a shell command that the `command` pattern fires on, as it was written,
    /// for showing a person why.
    pub fn fires_on(&self, args: &Value) -> Option<String> {
        let pattern = self.pattern.as_ref()?.get(COMMAND)?;
        let text = args.get(COMMAND)?.as_str()?;
        let words = shell::commands(text).into_iter().find(|w| shell::forms(w).iter().any(|f| search(pattern, f)))?;
        Some(words.join(" "))
    }
}

/// Agents name file arguments differently, so file tools also get `path` (pi: `path`, Claude
/// Code: `file_path`) and `content`: what the call writes, a write's content or an edit's new
/// text, never the text it replaces. One rule works whichever agent runs.
pub fn portable(args: &Value) -> Value {
    let mut args = args.clone();
    let Some(map) = args.as_object_mut() else { return args };
    if !map.contains_key("path")
        && let Some(file) = ["file_path", "filePath", "notebook_path"].iter().find_map(|k| map.get(*k).cloned())
    {
        map.insert("path".into(), file);
    }
    if !map.contains_key("content") {
        let new = |call: &Map<String, Value>| {
            ["new_string", "newText", "new_source"].iter().find_map(|k| call.get(*k)?.as_str().map(str::to_string))
        };
        let edits = map.get("edits").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object);
        let written: Vec<String> = new(map).into_iter().chain(edits.filter_map(new)).collect();
        if !written.is_empty() {
            map.insert("content".into(), written.join("\n").into());
        }
    }
    args
}

fn put<T: Into<Value>>(map: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        map.insert(key.into(), value.into());
    }
}

fn arg_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

thread_local! {
    /// Compiled once per pattern. An invalid pattern never matches (lint reports it).
    static COMPILED: std::cell::RefCell<std::collections::HashMap<String, Option<regress::Regex>>> = Default::default();
}

fn search(pattern: &str, text: &str) -> bool {
    COMPILED.with(|compiled| {
        compiled
            .borrow_mut()
            .entry(pattern.to_string())
            .or_insert_with(|| regress::Regex::new(pattern).ok())
            .as_ref()
            .is_some_and(|re| re.find(text).is_some())
    })
}

impl Config {
    pub fn idle(&self) -> &str {
        self.reap.as_ref().and_then(|r| r.idle.as_deref()).unwrap_or(DEFAULT_IDLE)
    }

    /// How long an idle session lives, in ms, when set as a duration. `None` when reaping
    /// is off, for `auto` (see `idle::reap_after_ms`), or when the setting isn't a duration
    /// (lint reports it): an unreadable setting never kills.
    pub fn reap_after_ms(&self) -> Option<u64> {
        duration_ms(self.idle())
    }

    pub fn reap_live(&self) -> Option<usize> {
        self.reap.as_ref().and_then(|r| r.live).filter(|&n| n > 0).map(|n| n as usize)
    }

    /// In bytes.
    pub fn reap_free(&self) -> Option<u64> {
        self.reap.as_ref().and_then(|r| r.free.as_deref()).and_then(size_bytes)
    }

    /// Whether idle sessions are reaped at all: after a while, or to make room.
    pub fn reaps(&self) -> bool {
        self.idle() == "auto"
            || self.reap_after_ms().is_some()
            || self.reap_live().is_some()
            || self.reap_free().is_some()
    }

    /// Every rule with where it lives: `None` for global, else the project name.
    pub fn all_rules(&self) -> impl Iterator<Item = (Option<&str>, &Rule)> {
        let global = self.rules.iter().map(|r| (None, r));
        let projects = self.projects.iter().flat_map(|(name, p)| p.rules.iter().map(move |r| (Some(name.as_str()), r)));
        global.chain(projects)
    }

    /// The rules as JSON, for scripts (`reaper rules --json`): the file's own key names,
    /// defaults filled in, unknown keys left out.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        if let Some(w) = &self.watcher {
            out.insert("watcher".into(), w.to_json());
        }
        let mut reap = json!({ "idle": self.idle() });
        if let Some(r) = &self.reap {
            put(reap.as_object_mut().expect("an object"), "live", r.live);
            put(reap.as_object_mut().expect("an object"), "free", r.free.clone());
        }
        out.insert("reap".into(), reap);
        out.insert("strikes".into(), self.strikes.into());
        out.insert("repeats".into(), self.repeats.into());
        out.insert("rules".into(), self.rules.iter().map(Rule::to_json).collect());
        let projects: Map<String, Value> = self
            .projects
            .iter()
            .map(|(name, p)| {
                (
                    name.clone(),
                    json!({ "paths": p.paths, "rules": p.rules.iter().map(Rule::to_json).collect::<Vec<_>>() }),
                )
            })
            .collect();
        out.insert("projects".into(), projects.into());
        out.into()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

/// An empty file, or one with only comments, means "no rules" (as on the engine side).
fn is_blank(text: &str) -> bool {
    text.lines().map(str::trim).all(|l| l.is_empty() || l.starts_with('#') || l == "---" || l == "...")
}

pub fn parse(text: &str) -> Result<Config, ParseError> {
    if is_blank(text) {
        return Ok(Config::default());
    }
    serde_saphyr::from_str(text).map_err(|e| {
        let location = e.location();
        // The parser's message repeats the location ("error: line 4 column 5: …"); it's kept separately.
        let full = e.to_string();
        let first = full.lines().next().unwrap_or("invalid YAML");
        let first = first.strip_prefix("error: ").unwrap_or(first);
        let message = match first.split_once(": ") {
            Some((at, rest)) if at.starts_with("line ") => rest,
            _ => first,
        };
        ParseError {
            message: message.to_string(),
            line: location.map(|l| l.line()).filter(|&l| l > 0),
            column: location.map(|l| l.column()).filter(|&c| c > 0),
        }
    })
}

pub struct RulesFile {
    pub path: PathBuf,
    pub exists: bool,
    pub text: String,
    /// The last version that parsed. A syntax error doesn't throw it away (the engine
    /// behaves the same way), so the UI can keep showing what is in force.
    pub config: Config,
    /// Whether `config` came from this file. False when it has never parsed (or doesn't exist).
    pub parsed: bool,
    pub error: Option<ParseError>,
    stamp: Option<(SystemTime, u64)>,
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl RulesFile {
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let mut file = Self {
            path: path.into(),
            exists: false,
            text: String::new(),
            config: Config::default(),
            parsed: false,
            error: None,
            stamp: None,
        };
        file.reload();
        file
    }

    /// Returns whether the file changed and was re-read.
    pub fn refresh(&mut self) -> bool {
        if stamp(&self.path) == self.stamp {
            return false;
        }
        self.reload();
        true
    }

    fn reload(&mut self) {
        self.stamp = stamp(&self.path);
        match fs::read_to_string(&self.path) {
            Ok(text) => {
                self.exists = true;
                match parse(&text) {
                    Ok(config) => {
                        self.config = config;
                        self.parsed = true;
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
                self.text = text;
            }
            Err(_) => {
                self.exists = false;
                self.text.clear();
                self.config = Config::default();
                self.parsed = false;
                self.error = None;
            }
        }
    }

    /// 1-based line where the rule with `id` is defined.
    pub fn line_of(&self, id: &str) -> Option<usize> {
        line_of(&self.text, id)
    }
}

/// Finds `id: <id>` in block (`- id: x`) or flow (`{ id: x, … }`) style, quoted or not.
pub fn line_of(text: &str, id: &str) -> Option<usize> {
    let found = text.lines().position(|line| {
        let mut rest = line;
        while let Some(i) = rest.find("id:") {
            let before = rest[..i].chars().next_back();
            let value = rest[i + 3..].trim_start();
            let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
            if before.is_none_or(|c| c.is_whitespace() || c == '{' || c == ',' || c == '-')
                && let Some(after) = value.strip_prefix(id)
                && after.chars().next().is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '}' | '"' | '\'' | '#'))
            {
                return true;
            }
            rest = &rest[i + 3..];
        }
        false
    })?;
    Some(found + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shipped_example() {
        let config = parse(crate::EXAMPLE_RULES).unwrap();
        assert_eq!(config.strikes, 3);
        assert_eq!(config.watcher.as_ref().and_then(|w| w.model.as_deref()), Some("qwen3:4b"));
        assert_eq!(config.reap_after_ms(), Some(300_000));
        let ids: Vec<_> = config.all_rules().map(|(place, r)| (place.unwrap_or("global"), r.id.as_str())).collect();
        assert_eq!(
            ids,
            [
                ("global", "force-push"),
                ("global", "secrets"),
                ("my-app", "prod-db"),
                ("my-app", "generated-code"),
                ("my-app", "bridge-schema")
            ]
        );
        assert!(config.unknown.is_empty() && config.all_rules().all(|(_, r)| r.unknown.is_empty()));
    }

    #[test]
    fn json_keeps_the_files_names_and_drops_unknown_keys() {
        let config = parse(
            r#"
watcher: { baseURL: "http://localhost:11434/v1", model: m, timeoutMs: 500, typo: 1, fallback: [{ baseURL: "http://localhost:8080/v1", model: n }] }
rules:
  - { id: push, tools: [bash], match: { command: 'git push -f' }, redirect: Open a PR., redirects: oops }
projects:
  app:
    paths: [~/app]
    rules: [{ id: db, judge: Touching prod., action: stop }]
"#,
        )
        .unwrap();
        assert_eq!(
            config.to_json(),
            json!({
                "watcher": {
                    "baseURL": "http://localhost:11434/v1",
                    "model": "m",
                    "timeoutMs": 500,
                    "fallback": [{ "baseURL": "http://localhost:8080/v1", "model": "n" }]
                },
                "reap": { "idle": "5m" },
                "strikes": 3,
                "repeats": 5,
                "rules": [{ "id": "push", "tools": ["bash"], "match": { "command": "git push -f" }, "redirect": "Open a PR." }],
                "projects": { "app": { "paths": ["~/app"], "rules": [{ "id": "db", "judge": "Touching prod.", "action": "stop" }] } }
            })
        );
        assert_eq!(
            parse("").unwrap().to_json(),
            json!({ "reap": { "idle": "5m" }, "strikes": 3, "repeats": 5, "rules": [], "projects": {} })
        );
    }

    #[test]
    fn idle_sessions_are_reaped_unless_turned_off() {
        assert_eq!(parse("").unwrap().reap_after_ms(), Some(300_000), "on by default");
        assert_eq!(parse("reap: { idle: 90s }").unwrap().reap_after_ms(), Some(90_000));
        assert_eq!(parse("reap: { idle: 1h }").unwrap().reap_after_ms(), Some(3_600_000));
        for never in ["off", "'off'", "0s", "5 minutes", "300"] {
            assert_eq!(parse(&format!("reap: {{ idle: {never} }}")).unwrap().reap_after_ms(), None, "{never}");
        }
        assert_eq!(duration_ms("250ms"), Some(250));

        let crowded = parse("reap: { live: 4, free: 2G }").unwrap();
        assert_eq!((crowded.reap_live(), crowded.reap_free()), (Some(4), Some(2 << 30)));
        assert_eq!(crowded.to_json()["reap"], json!({ "idle": "5m", "live": 4, "free": "2G" }));
        assert_eq!(parse("").unwrap().reap_live(), None, "no cap unless asked");
        assert_eq!(size_bytes("512MiB"), Some(512 << 20));
        for bad in ["2", "2T", "0G", "lots"] {
            assert_eq!(size_bytes(bad), None, "{bad}");
        }
        assert_eq!(duration_ms("m"), None);
        assert_eq!(duration_ms(&format!("{}h", u64::MAX)), None, "overflow is not a duration");
    }

    #[test]
    fn a_rule_with_free_applies_only_while_memory_is_short() {
        let rules = parse(
            "rules:\n  - { id: suite, match: { command: pytest }, free: 4G }\n  - { id: push, match: { command: push } }\n  - { id: typo, match: { command: x }, free: lots }\n",
        )
        .unwrap()
        .rules;
        let gb = |n: u64| Room { free: Some(n << 30), load: None };
        assert!(rules[0].in_force(&gb(3)) && !rules[0].in_force(&gb(4)) && !rules[0].in_force(&gb(12)));
        assert!(!rules[0].in_force(&Room::default()), "memory that can't be read never kills");
        assert!(rules[1].in_force(&gb(12)) && rules[1].in_force(&Room::default()), "other rules always apply");
        assert!(!rules[2].in_force(&gb(1)), "a setting that isn't a size never applies");
        assert_eq!(parse("rules:\n  - { id: suite, free: 4G }\n").unwrap().to_json()["rules"][0]["free"], "4G");
        assert_eq!((size_text(1932735283), size_text(512 << 20)), ("1.8G".to_string(), "512M".to_string()));
    }

    #[test]
    fn a_rule_with_load_applies_while_the_machine_is_busy_or_short_of_memory() {
        let rules = parse("rules:\n  - { id: busy, match: { command: x }, load: 16 }\n  - { id: both, match: { command: x }, free: 12G, load: 16 }\n").unwrap().rules;
        let room = |free: u64, load: f64| Room { free: Some(free << 30), load: Some(load) };
        assert!(rules[0].in_force(&room(2, 20.0)) && !rules[0].in_force(&room(2, 3.0)), "memory doesn't come into it");
        assert!(!rules[0].in_force(&Room { free: None, load: None }), "a load that can't be read never kills");
        assert!(rules[1].in_force(&room(4, 3.0)), "short of memory");
        assert!(rules[1].in_force(&room(20, 20.0)), "busy");
        assert!(!rules[1].in_force(&room(20, 3.0)), "room for it: both");
        assert_eq!(parse("rules:\n  - { id: x, load: 16 }\n").unwrap().to_json()["rules"][0]["load"], 16.0);
    }

    #[test]
    fn empty_or_comment_only_files_mean_no_rules() {
        for text in ["", "\n\n", "# nothing yet\n", "---\n# still nothing\n"] {
            let config = parse(text).unwrap();
            assert!(config.rules.is_empty() && config.projects.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn syntax_errors_carry_a_location() {
        let err = parse("rules:\n  - id: a\n    match: { command: [unclosed\n").unwrap_err();
        assert!(err.line.is_some(), "{err:?}");
        assert!(!err.message.contains('\n'));
        assert!(
            !err.message.contains("line ") && !err.message.starts_with("error"),
            "location kept separately: {err:?}"
        );
    }

    #[test]
    fn only_rules_that_ask_judge_what_the_agent_says() {
        let rules = parse(
            "rules:\n  - { id: secrets, match: { '*': '\\.env' } }\n  - { id: schema, tools: [message, edit], match: { text: 'NoSQL' } }\n",
        )
        .unwrap()
        .rules;
        let said = json!({ "text": "I put the keys in .env and switched to NoSQL." });
        assert!(!rules[0].matches(MESSAGE, &said), "a rule over every argument stays off messages");
        assert!(rules[1].matches(MESSAGE, &said));
        assert!(rules[1].applies_to("Edit") && !rules[1].applies_to("bash"));
    }

    #[test]
    fn a_command_pattern_fires_when_the_program_runs_and_not_when_it_is_mentioned() {
        let rule =
            &parse(r"rules: [{ id: push, tools: [bash], match: { command: '^git push\b.*\s(-f\b|--force\b|\+\S)' } }]")
                .unwrap()
                .rules[0];
        let stops = |command: &str| rule.matches("bash", &json!({ "command": command }));
        for run in [
            "git push -f",
            "cd repo && git push --force",
            "cd repo\ngit push --force origin main",
            "timeout 60 git push -f",
            "sudo git push -f",
            "env GIT_TRACE=1 git push -f",
            "GIT_TRACE=1 git push -f",
            "git -c core.x=y push -f",
            "git -C ../app push -f",
            "bash -c 'git push -f'",
            "echo `git push -f`",
            "git push origin +main",
            "/usr/bin/git push -f",
            "git commit -m \"$(cat <<'EOF'\nIt's done\nEOF\n)\" && git push -f",
        ] {
            assert!(stops(run), "{run}");
        }
        for mention in [
            "git status",
            "git push origin feature",
            "cat <<'EOF' > notes.md\nRun: cd x && git push -f\nEOF",
            "git commit -m 'document why; git push -f is banned'",
            "grep -rn 'git push -f' docs",
            "ls # git push -f later",
        ] {
            assert!(!stops(mention), "{mention}");
        }
    }

    #[test]
    fn a_script_pattern_sees_the_whole_command_as_sent() {
        let rule = &parse(r"rules: [{ id: piped, match: { script: '\|\s*tail\b', run_in_background: 'true' } }]")
            .unwrap()
            .rules[0];
        assert!(rule.matches("Bash", &json!({ "command": "make test 2>&1 | tail -5", "run_in_background": true })));
        assert!(!rule.matches("Bash", &json!({ "command": "make test 2>&1 | tail -5" })));
        assert!(!rule.matches("Bash", &json!({ "command": "make test > log", "run_in_background": true })));
        let written = json!({ "command": "cat > ci.sh <<'EOF'\nmake | tail\nEOF", "run_in_background": true });
        assert!(!rule.matches("Bash", &written), "a file being written isn't the command");
        let own = &parse(r"rules: [{ id: s, match: { script: '^deploy' } }]").unwrap().rules[0];
        assert!(own.matches("mcp_run", &json!({ "script": "deploy all", "command": "x" })), "a tool's own script");
    }

    #[test]
    fn a_rule_on_content_sees_what_any_agent_writes_and_not_what_it_replaces() {
        let rule =
            &parse("rules: [{ id: dash, tools: [edit, multiedit, write], match: { content: '—' } }]").unwrap().rules[0];
        let hit = |tool: &str, args: Value| rule.matches(tool, &portable(&args));
        assert!(hit("Edit", json!({ "file_path": "a.md", "old_string": "a", "new_string": "a — b" })), "Claude Code");
        assert!(
            hit(
                "edit",
                json!({ "path": "a.md", "edits": [{ "oldText": "a", "newText": "b" }, { "oldText": "c", "newText": "c — d" }] })
            ),
            "pi"
        );
        assert!(hit(
            "MultiEdit",
            json!({ "file_path": "a.md", "edits": [{ "old_string": "a", "new_string": "a — b" }] })
        ));
        assert!(hit("Write", json!({ "file_path": "a.md", "content": "a — b" })));
        assert!(
            !hit("Edit", json!({ "file_path": "a.md", "old_string": "a — b", "new_string": "a, b" })),
            "taking one out"
        );
        assert_eq!(portable(&json!({ "file_path": "a.md" }))["path"], "a.md");
    }

    #[test]
    fn scope_mirrors_the_engine() {
        let rules = parse(
            "rules:\n  - { id: a, match: { command: x } }\n  - { id: b, judge: y }\n  - { id: c, tools: [Bash], judge: z }\n",
        )
        .unwrap()
        .rules;
        assert_eq!(rules[0].scope(), None);
        assert_eq!(rules[1].scope().unwrap().len(), MUTATING_TOOLS.len());
        assert_eq!(rules[2].scope(), Some(vec!["bash".to_string()]));
    }

    #[test]
    fn finds_rule_lines_in_any_style() {
        let text = "rules:\n  - id: force-push\n  - { id: \"secrets\", match: {} }\nprojects:\n  a:\n    rules:\n      - id: prod-db # db\n";
        assert_eq!(line_of(text, "force-push"), Some(2));
        assert_eq!(line_of(text, "secrets"), Some(3));
        assert_eq!(line_of(text, "prod-db"), Some(7));
        assert_eq!(line_of(text, "prod"), None, "prefixes don't count");
        assert_eq!(line_of("    valid: x\n", "x"), None, "only the id key");
    }

    #[test]
    fn a_broken_edit_keeps_the_last_good_rules() {
        let dir = std::env::temp_dir().join(format!("reaper-rules-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.yaml");
        fs::write(&path, "rules:\n  - { id: a, match: { command: x } }\n").unwrap();
        let mut file = RulesFile::load(&path);
        assert_eq!(file.config.rules.len(), 1);

        fs::write(&path, "rules: [ {{ broken\n").unwrap();
        assert!(file.refresh());
        assert!(file.error.is_some());
        assert_eq!(file.config.rules.len(), 1, "still the last good version");
        assert!(file.parsed);
        assert!(!RulesFile::load(&path).parsed, "a file that never parsed has no rules to show");

        fs::remove_file(&path).unwrap();
        assert!(file.refresh());
        assert!(!file.exists && file.error.is_none() && file.config.rules.is_empty());
    }
}
