//! The human's rules file. This is the only parser for it: the engine, the console
//! and `reaper lint` all read rules through here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Map, Value, json};

/// What the agent says (its replies), checked as if it were a call to a tool of this name with
/// one argument, `text`. Only rules that list it in `tools` ever see a message.
pub const MESSAGE: &str = "message";

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
    #[serde(flatten)]
    pub unknown: Unknown,
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
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Rule {
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

    pub fn applies_to(&self, tool: &str) -> bool {
        let tool = tool.to_lowercase();
        if tool == MESSAGE {
            // Opt-in only: a rule over every argument (`"*"`) mustn't start judging what the agent says.
            return self.tools.as_ref().is_some_and(|tools| tools.iter().any(|t| t.eq_ignore_ascii_case(MESSAGE)));
        }
        self.scope().is_none_or(|scope| scope.contains(&tool))
    }

    /// Whether every `match` entry hits the tool's arguments (a JSON object). Patterns are
    /// JavaScript regexes, searched anywhere in the value; `*` tests every argument.
    /// A rule without `match` never matches here (it is the watcher's to judge).
    pub fn matches(&self, tool: &str, args: &Value) -> bool {
        let Some(patterns) = &self.pattern else { return false };
        if !self.applies_to(tool) {
            return false;
        }
        patterns.iter().all(|(key, pattern)| {
            let hit = |value: &Value| search(pattern, &arg_text(value));
            if key == "*" {
                args.as_object().is_some_and(|args| args.values().any(hit))
            } else {
                args.get(key).is_some_and(hit)
            }
        })
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

    /// Every rule with where it lives: `None` for global, else the project name.
    pub fn all_rules(&self) -> impl Iterator<Item = (Option<&str>, &Rule)> {
        let global = self.rules.iter().map(|r| (None, r));
        let projects = self.projects.iter().flat_map(|(name, p)| p.rules.iter().map(move |r| (Some(name.as_str()), r)));
        global.chain(projects)
    }

    /// The rules as JSON, for scripts (`reaper rules --json`): the file's own key names,
    /// defaults filled in, unknown keys left out.
    pub fn to_json(&self) -> Value {
        fn put<T: Into<Value>>(map: &mut Map<String, Value>, key: &str, value: Option<T>) {
            if let Some(value) = value {
                map.insert(key.into(), value.into());
            }
        }
        fn rule(r: &Rule) -> Value {
            let mut map = Map::new();
            map.insert("id".into(), r.id.clone().into());
            put(&mut map, "tools", r.tools.clone());
            put(&mut map, "match", r.pattern.as_ref().map(|m| json!(m)));
            put(&mut map, "judge", r.judge.clone());
            put(&mut map, "redirect", r.redirect.clone());
            put(&mut map, "action", r.action.clone());
            map.into()
        }
        let mut out = Map::new();
        if let Some(w) = &self.watcher {
            let mut map = Map::new();
            put(&mut map, "kind", w.kind.clone());
            put(&mut map, "baseURL", w.base_url.clone());
            put(&mut map, "model", w.model.clone());
            put(&mut map, "apiKeyEnv", w.api_key_env.clone());
            put(&mut map, "timeoutMs", w.timeout_ms);
            put(&mut map, "cacheKey", w.cache_key);
            put(&mut map, "onError", w.on_error.clone());
            out.insert("watcher".into(), map.into());
        }
        let mut reap = json!({ "idle": self.idle() });
        if let Some(r) = &self.reap {
            put(reap.as_object_mut().expect("an object"), "live", r.live);
            put(reap.as_object_mut().expect("an object"), "free", r.free.clone());
        }
        out.insert("reap".into(), reap);
        out.insert("strikes".into(), self.strikes.into());
        out.insert("repeats".into(), self.repeats.into());
        out.insert("rules".into(), self.rules.iter().map(rule).collect());
        let projects: Map<String, Value> = self
            .projects
            .iter()
            .map(|(name, p)| {
                (name.clone(), json!({ "paths": p.paths, "rules": p.rules.iter().map(rule).collect::<Vec<_>>() }))
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
watcher: { baseURL: "http://localhost:11434/v1", model: m, timeoutMs: 500, typo: 1 }
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
                "watcher": { "baseURL": "http://localhost:11434/v1", "model": "m", "timeoutMs": 500 },
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
