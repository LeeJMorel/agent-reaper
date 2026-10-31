//! The decision engine: which rule, if any, an agent's next action violates, and what
//! happens then. Every host (pi, Claude Code) calls it once per tool call, each time in a
//! fresh process, so it starts fast and keeps what must outlive a call in `store`.

pub mod backend;
pub mod prefix;
pub mod pressure;
pub mod store;
pub mod watcher;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reaper_core::events;
use reaper_core::paths::{self, expand_home};
use reaper_core::rules::{Config, MUTATING_TOOLS, Rule, parse};
use serde_json::{Map, Value, json};

use prefix::{canonical, hash};
use store::Store;
use watcher::{Settings, Tier};

const MAX_ATTEMPT: usize = 300;
const DEFAULT_REDIRECT: &str = "Continue with the task.";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
/// A warmup is on nobody's path, so a model may take this long to load for it.
const WARM_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Check {
    pub host: String,
    /// The top-level session; strikes are counted here.
    pub session: String,
    /// The session or subagent making the call.
    pub agent: String,
    pub dir: PathBuf,
    pub tool: String,
    pub args: Value,
    /// What the human asked for, for the watcher. Empty if unknown.
    pub goal: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Kill { rule: String, tier: Tier, strike: u32, is_final: bool, redirect: String },
}

pub struct Engine {
    pub config: Config,
    /// Why the rules on disk aren't the ones in force, if they aren't.
    pub problem: Option<String>,
    pub store: Store,
    events: PathBuf,
}

/// `*` is one path segment, `**` any depth; a pattern also covers everything below it.
fn glob_matches(glob: &str, dir: &Path) -> bool {
    let glob = expand_home(glob).to_string_lossy().trim_end_matches('/').to_string();
    let mut body = String::from("^");
    let mut rest = glob.as_str();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            body.push_str(".*");
            rest = after;
        } else if let Some(after) = rest.strip_prefix('*') {
            body.push_str("[^/]*");
            rest = after;
        } else {
            let c = rest.chars().next().expect("non-empty");
            if ".+?^${}()|[]\\/".contains(c) {
                body.push('\\');
            }
            body.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    body.push_str("(/.*)?$");
    regress::Regex::new(&body).is_ok_and(|re| re.find(&dir.to_string_lossy()).is_some())
}

/// Agents name a file argument differently (pi: `path`, Claude Code: `file_path`), so file
/// tools also get `path`: one rule works whichever agent runs.
fn portable(args: &Value) -> Value {
    let mut args = args.clone();
    if let Some(map) = args.as_object_mut()
        && !map.contains_key("path")
        && let Some(file) = ["file_path", "filePath", "notebook_path"].iter().find_map(|k| map.get(*k).cloned())
    {
        map.insert("path".into(), file);
    }
    args
}

/// The rule for a stuck agent: the same call made `repeats` times in a row. It is decided
/// by counting (see `Engine::repeated`), never by a pattern or the watcher.
fn loop_rule() -> Rule {
    Rule {
        id: "reaper.loop".into(),
        tools: None,
        pattern: None,
        judge: None,
        redirect: Some("Try a different approach to the task.".into()),
        action: None,
        unknown: Default::default(),
    }
}

/// Agents never get to open, list or run anything in reaper's own directories, however
/// they spell them (absolute, `~/…`, `$HOME/…`). Only `path` and `command` are checked:
/// a file whose contents merely mention a directory is fine to write.
fn self_rules() -> [Rule; 2] {
    let home = paths::home().to_string_lossy().into_owned();
    let mut spellings = Vec::new();
    for dir in [paths::config_dir(), paths::state_dir(), paths::cache_dir()] {
        let dir = dir.to_string_lossy().into_owned();
        if let Some(rest) = dir.strip_prefix(&home).filter(|r| r.starts_with('/') && !home.is_empty()) {
            spellings.extend([format!("~{rest}"), format!("$HOME{rest}")]);
        }
        spellings.push(dir);
    }
    let escaped: Vec<String> = spellings
        .iter()
        .map(|s| {
            s.chars().map(|c| if ".*+?^${}()|[]\\/".contains(c) { format!("\\{c}") } else { c.to_string() }).collect()
        })
        .collect();
    let on = |arg: &str| Rule {
        id: "reaper.self".into(),
        tools: None,
        pattern: Some([(arg.to_string(), escaped.join("|"))].into()),
        judge: None,
        redirect: Some("Continue the task using the files inside the project workspace.".into()),
        action: None,
        unknown: Default::default(),
    };
    [on("path"), on("command")]
}

impl Engine {
    pub fn load() -> Self {
        Self::with(&paths::rules_file(), Store::default(), paths::events_file())
    }

    pub fn with(rules: &Path, store: Store, events: PathBuf) -> Self {
        let last_good = || store.good_rules().and_then(|text| parse(&text).ok()).unwrap_or_default();
        let (config, problem) = match fs::read_to_string(rules) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (last_good(), Some(format!("can't read {}: {e}; the last good rules apply", rules.display()))),
            Ok(text) => match parse(&text) {
                Ok(config) => {
                    store.save_good_rules(&text);
                    (config, None)
                }
                Err(e) => {
                    let at = e.line.map(|l| format!(" at line {l}")).unwrap_or_default();
                    (
                        last_good(),
                        Some(format!("rules.yaml has a syntax error{at}: {}; the last good rules apply", e.message)),
                    )
                }
            },
        };
        Self { config, problem, store, events }
    }

    pub fn rules_for(&self, dir: &Path) -> (Option<String>, Vec<&Rule>) {
        let projects: Vec<(&String, &reaper_core::rules::Project)> =
            self.config.projects.iter().filter(|(_, p)| p.paths.iter().any(|g| glob_matches(g, dir))).collect();
        let name =
            (!projects.is_empty()).then(|| projects.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("+"));
        (name, projects.iter().flat_map(|(_, p)| p.rules.iter()).collect())
    }

    /// The log is for the human, so a failure to write never stops the agent.
    pub fn event(&self, host: &str, agent: &str, kind: &str, fields: Value) {
        let fields = match fields {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        if let Err(e) = events::append(&self.events, host, agent, kind, fields) {
            eprintln!("reaper: can't write {}: {e}", self.events.display());
        }
    }

    /// Gets the watcher ready for a session's checks: the human just asked for something, so
    /// checks are seconds away, and the agent's own first model call leaves time to prefill
    /// the rules and the task. A local model unloaded while everyone was away loads now, not
    /// during a check against its timeout. Only when a judge rule could ask it anything.
    pub fn warm(&self, dir: &Path, goal: &str) {
        let own = self_rules();
        let global: Vec<&Rule> = own.iter().chain(&self.config.rules).collect();
        let (_, project) = self.rules_for(dir);
        let Some(watcher) = self.config.watcher.as_ref() else { return };
        if !global.iter().chain(&project).any(|r| r.judge.is_some()) {
            return;
        }
        if let Some(backend) = backend::from_config(watcher, &self.store) {
            let _ = backend.complete(&prefix::warmup(&global, &project, goal), WARM_TIMEOUT, 1);
        }
    }

    /// Whether this call repeats the agent's last call that did something for the
    /// `repeats`-th time in a row. Looking around in between (reads, searches) doesn't
    /// count: running the same command again with nothing changed is how an agent goes in
    /// circles, re-running a failing test or retrying an edit that can't apply. AgentStop
    /// (arXiv 2605.15206) stops agent runs it predicts will fail, from token
    /// probabilities; reaper sees tool calls, not probabilities, so it looks for repeats.
    fn repeated(&self, c: &Check, args: &Value) -> bool {
        let tool = c.tool.to_lowercase();
        if self.config.repeats < 2 || !MUTATING_TOOLS.contains(&tool.as_str()) {
            return false;
        }
        let call = hash(&[&tool, &canonical(args)]);
        let run = self.store.update(&c.host, &c.agent, |s| {
            if s.last_call.as_deref() == Some(call.as_str()) {
                s.repeats += 1;
            } else {
                s.last_call = Some(call.clone());
                s.repeats = 1;
            }
            s.repeats
        });
        run.is_ok_and(|n| n >= self.config.repeats)
    }

    pub fn reap_after_ms(&self) -> Option<u64> {
        reaper_core::idle::reap_after_ms(&self.config, &self.events)
    }

    pub fn relieve(&self, crowding: bool) {
        pressure::relieve(&self.config, &self.store, crowding);
    }

    pub fn check(&self, c: &Check) -> Decision {
        // Work under way is when memory runs short: a build, a test suite.
        self.relieve(false);
        let args = portable(&c.args);
        let own = self_rules();
        let global: Vec<&Rule> = own.iter().chain(&self.config.rules).collect();
        let (_, project) = self.rules_for(&c.dir);
        let watcher = self.config.watcher.as_ref();
        let backend = watcher.and_then(|w| backend::from_config(w, &self.store));
        let settings = Settings {
            backend: backend.as_deref(),
            kill_on_error: watcher.and_then(|w| w.on_error.as_deref()) == Some("kill"),
            timeout: watcher.and_then(|w| w.timeout_ms).map_or(DEFAULT_TIMEOUT, Duration::from_millis),
        };
        let outcome = watcher::check(&global, &project, &c.goal, &c.tool, &args, &settings, &self.store);

        if outcome.judged {
            let mut fields = json!({
                "tool": c.tool,
                "tier": outcome.tier.as_str(),
                "ms": outcome.ms,
                "verdict": if outcome.rule.is_some() { "violation" } else { "pass" },
            });
            if let Some(error) = &outcome.error {
                fields["error"] = error.clone().into();
            }
            self.event(&c.host, &c.agent, "agent.checked", fields);
        }
        let looping = loop_rule();
        let (rule, tier) = match outcome.rule {
            Some(rule) => (rule, outcome.tier),
            None if self.repeated(c, &args) => (&looping, Tier::Pattern),
            None => {
                // A message that passes isn't an action: nothing was done.
                if !c.tool.eq_ignore_ascii_case(reaper_core::rules::MESSAGE) {
                    self.event(&c.host, &c.agent, "agent.acted", json!({ "tool": c.tool }));
                }
                return Decision::Allow;
            }
        };

        let strike = self
            .store
            .update(&c.host, &c.session, |s| {
                let n = s.strikes.entry(rule.id.clone()).or_default();
                *n += 1;
                *n
            })
            .unwrap_or(1);
        let is_final = rule.stops() || strike >= self.config.strikes;
        let attempt: String = canonical(&c.args).chars().take(MAX_ATTEMPT).collect();
        self.event(
            &c.host,
            &c.agent,
            "agent.killed",
            json!({ "tool": c.tool, "rule": rule.id, "tier": tier.as_str(), "strike": strike, "final": is_final, "attempt": attempt }),
        );
        Decision::Kill {
            rule: rule.id.clone(),
            tier,
            strike,
            is_final,
            redirect: rule.redirect.clone().unwrap_or_else(|| DEFAULT_REDIRECT.to_string()),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub struct Sandbox {
        pub root: PathBuf,
    }

    impl Sandbox {
        pub fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("reaper-engine-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        pub fn engine(&self, rules: &str) -> Engine {
            let file = self.root.join("rules.yaml");
            fs::write(&file, rules).unwrap();
            Engine::with(&file, Store::at(&self.root), self.root.join("events.jsonl"))
        }

        pub fn events(&self) -> Vec<String> {
            let log = fs::read_to_string(self.root.join("events.jsonl")).unwrap_or_default();
            log.lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap()["type"].as_str().unwrap().to_string())
                .collect()
        }
    }

    pub fn call(tool: &str, args: Value) -> Check {
        Check {
            host: "test".into(),
            session: "s1".into(),
            agent: "s1".into(),
            dir: "/work".into(),
            tool: tool.into(),
            args,
            goal: String::new(),
        }
    }

    #[test]
    fn strikes_escalate_to_a_final_kill_and_every_step_is_logged() {
        let sandbox = Sandbox::new("strikes");
        let engine = sandbox.engine(
            "strikes: 2\nrules: [{ id: push, match: { command: 'git push -f' }, redirect: Open a pull request. }]",
        );
        let push = || call("bash", json!({ "command": "git push -f" }));
        assert!(
            matches!(engine.check(&push()), Decision::Kill { strike: 1, is_final: false, ref redirect, .. } if redirect == "Open a pull request.")
        );
        assert!(matches!(engine.check(&push()), Decision::Kill { strike: 2, is_final: true, .. }));
        assert_eq!(engine.check(&call("bash", json!({ "command": "git status" }))), Decision::Allow);
        assert_eq!(sandbox.events(), ["agent.checked", "agent.killed", "agent.checked", "agent.killed", "agent.acted"]);
    }

    #[test]
    fn an_agent_making_the_same_call_over_and_over_is_stuck() {
        let sandbox = Sandbox::new("loop");
        let engine = sandbox.engine("strikes: 2\nrepeats: 3");
        let test = || call("bash", json!({ "command": "make test" }));
        let look = || call("read", json!({ "path": "src/main.c" }));
        assert_eq!(engine.check(&test()), Decision::Allow);
        assert_eq!(engine.check(&look()), Decision::Allow, "looking around doesn't count");
        assert_eq!(engine.check(&test()), Decision::Allow);
        assert!(
            matches!(engine.check(&test()), Decision::Kill { ref rule, strike: 1, is_final: false, ref redirect, .. }
                if rule == "reaper.loop" && redirect == "Try a different approach to the task.")
        );
        assert!(matches!(engine.check(&test()), Decision::Kill { strike: 2, is_final: true, .. }));

        // Doing something else in between is progress.
        let fresh = Sandbox::new("loop-progress").engine("repeats: 3");
        let edit = call("edit", json!({ "path": "src/main.c", "old_string": "a", "new_string": "b" }));
        for _ in 0..4 {
            assert_eq!(fresh.check(&test()), Decision::Allow);
            assert_eq!(fresh.check(&edit), Decision::Allow);
        }
        let off = Sandbox::new("loop-off").engine("repeats: 0");
        assert!((0..6).all(|_| off.check(&test()) == Decision::Allow));
    }

    #[test]
    fn what_the_agent_says_is_judged_by_the_rules_that_ask() {
        let sandbox = Sandbox::new("message");
        let engine = sandbox.engine(
            "rules:\n  - { id: bridge-schema, tools: [message, write], match: { '*': 'NoSQL' }, redirect: Keep to the bridge model in docs/schema.md. }\n  - { id: secrets, match: { '*': '\\.env' } }\n",
        );
        let says = |text: &str| call("message", json!({ "text": text }));
        assert!(
            matches!(engine.check(&says("Let's move members to a NoSQL store.")), Decision::Kill { ref rule, ref redirect, .. }
                if rule == "bridge-schema" && redirect == "Keep to the bridge model in docs/schema.md.")
        );
        assert_eq!(engine.check(&says("The keys live in .env; I left them alone.")), Decision::Allow);
        assert_eq!(sandbox.events(), ["agent.checked", "agent.killed"], "a message that passes logs nothing");
    }

    #[test]
    fn strikes_survive_across_processes() {
        let sandbox = Sandbox::new("persist");
        let rules = "strikes: 2\nrules: [{ id: push, match: { command: push } }]";
        let push = call("bash", json!({ "command": "push" }));
        assert!(matches!(sandbox.engine(rules).check(&push), Decision::Kill { strike: 1, .. }));
        assert!(matches!(sandbox.engine(rules).check(&push), Decision::Kill { strike: 2, is_final: true, .. }));
    }

    #[test]
    fn reapers_own_files_are_off_limits_however_they_are_spelled() {
        let sandbox = Sandbox::new("self");
        let engine = sandbox.engine("");
        let config = paths::config_dir().to_string_lossy().into_owned();
        let home = paths::home().to_string_lossy().into_owned();
        let tilde = config.replacen(&home, "~", 1);
        for command in [format!("cat {config}/rules.yaml"), format!("cat {tilde}/rules.yaml")] {
            assert!(
                matches!(engine.check(&call("bash", json!({ "command": command }))), Decision::Kill { ref rule, .. } if rule == "reaper.self")
            );
        }
        assert_eq!(engine.check(&call("bash", json!({ "command": "cat ~/.config/other/x" }))), Decision::Allow);

        // File tools are covered through `path`; mentioning the directory in a file's contents is not.
        let read = call("Read", json!({ "file_path": format!("{config}/rules.yaml") }));
        assert!(matches!(engine.check(&read), Decision::Kill { ref rule, .. } if rule == "reaper.self"));
        let docs = call("Write", json!({ "file_path": "/w/README.md", "content": format!("Rules live in {tilde}.") }));
        assert_eq!(engine.check(&docs), Decision::Allow);
    }

    #[test]
    fn one_rule_matches_file_arguments_from_every_agent() {
        let sandbox = Sandbox::new("portable");
        let engine = sandbox.engine("rules: [{ id: gen, tools: [edit, write], match: { path: /generated/ } }]");
        for args in
            [json!({ "path": "/a/generated/x.ts" }), json!({ "file_path": "/a/generated/x.ts", "old_string": "a" })]
        {
            assert!(matches!(engine.check(&call("Edit", args)), Decision::Kill { .. }));
        }
        assert_eq!(engine.check(&call("Edit", json!({ "file_path": "/a/src/x.ts" }))), Decision::Allow);
    }

    #[test]
    fn a_broken_edit_keeps_the_last_good_rules_in_force() {
        let sandbox = Sandbox::new("broken");
        let good = sandbox.engine("rules: [{ id: a, match: { command: x } }]");
        assert!(good.problem.is_none());
        let broken = sandbox.engine("rules: [ { broken\n");
        assert!(broken.problem.as_deref().is_some_and(|p| p.contains("syntax error")), "{:?}", broken.problem);
        assert_eq!(broken.config.rules.len(), 1, "the last good rules still apply");
    }

    #[test]
    fn projects_resolve_by_path_and_global_rules_always_apply() {
        let sandbox = Sandbox::new("projects");
        let engine = sandbox.engine(
            "rules: [{ id: g, match: { command: x } }]\nprojects:\n  a: { paths: [/work/a], rules: [{ id: a1, match: { command: y } }] }\n  b: { paths: [/work/b], rules: [{ id: b1, match: { command: z } }] }\n",
        );
        let (name, rules) = engine.rules_for(Path::new("/work/a/src"));
        assert_eq!(name.as_deref(), Some("a"));
        assert_eq!(rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["a1"]);
        assert!(engine.rules_for(Path::new("/elsewhere")).1.is_empty());
    }

    #[test]
    fn globs_cover_the_directory_and_everything_below_it() {
        let home = paths::home();
        assert!(glob_matches("~/code/app-worktrees/*", &home.join("code/app-worktrees/feature")));
        assert!(glob_matches("~/code/app-worktrees/*", &home.join("code/app-worktrees/feature/backend/src")));
        assert!(!glob_matches("~/code/app-worktrees/*", &home.join("code/app-worktrees")));
        assert!(glob_matches("/srv/**/api", Path::new("/srv/a/b/api/x")));
        assert!(!glob_matches("/srv/app", Path::new("/srv/app2")));
    }
}
