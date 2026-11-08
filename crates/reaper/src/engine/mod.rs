//! The decision engine: which rule, if any, an agent's next action violates, and what
//! happens then. Every host (pi, Claude Code) calls it once per tool call, each time in a
//! fresh process, so it starts fast and keeps what must outlive a call in `store`.

pub mod backend;
mod handoff;
pub mod prefix;
pub mod pressure;
pub mod store;
pub mod trial;
mod vcs;
pub mod watcher;

pub use handoff::CARRY_ON;

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use reaper_core::paths::{self, expand_home};
use reaper_core::rules::{Config, MUTATING_TOOLS, Room, Rule, parse, portable, size_text};
use reaper_core::{draft, events, shell};
use serde_json::{Map, Value, json};

use prefix::{canonical, hash};
use store::Store;
use watcher::{Settings, Tier};

const MAX_ATTEMPT: usize = 300;
/// What an allowed call is logged with: each of its strings this long at most, so a file
/// written whole doesn't crowd out its path, and all of them this long.
const MAX_ARG: usize = 300;
const MAX_ARGS: usize = 1000;
/// What an agent said is logged this long at most.
const MAX_SAID: usize = 2000;
const DEFAULT_REDIRECT: &str = "Continue with the task.";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
/// Claude Code starts for each check and took about 4 s with reaper's prompt.
const CLAUDE_CODE_TIMEOUT: Duration = Duration::from_secs(15);
/// A warmup is on nobody's path, so a model may take this long to load for it.
const WARM_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Check {
    pub host: String,
    /// The top-level session, which everyone in it works for.
    pub session: String,
    /// The session or subagent making the call: strikes are its own.
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
    Kill {
        rule: String,
        tier: Tier,
        strike: u32,
        is_final: bool,
        redirect: String,
    },
    /// Everyone in the session is stopped: its agents broke rules too often between them,
    /// the last time by breaking `rule`.
    Stopped {
        rule: String,
    },
    /// The agent asked, with `reaper ask`, and made the blocked call again: the person decides
    /// whether to allow this one call. No strike either way.
    Ask {
        rule: String,
        redirect: String,
        why: String,
    },
    /// Nothing to check, only something to tell the agent: the answer to `reaper ask`.
    Noted {
        message: String,
    },
    /// The call breaks no rule but changes reaper itself (see `changes_reaper`): only the person
    /// can allow it. `change` says what, for them: "take the rule prod-db out of your rules".
    Confirm {
        change: String,
    },
}

/// What the redirect of a rule that can be asked about adds, so the agent knows how.
pub(crate) const ASKING: &str =
    "If this case needs it anyway, run reaper ask \"<why>\" and then make the same call again; the person decides.";

/// What an agent hears when it would change reaper and nobody can be asked.
pub(crate) const FOR_THE_PERSON: &str =
    "Changing what reaper guards is the person's to do. Ask them to run it themselves.";

/// An agent's pressure at which it's stopped: its whole allowance used.
const FULL: f64 = 1.0 - 1e-9;

/// The pressure at which everyone in a session of several agents is stopped: two agents'
/// whole allowances, however big the team. No agent carries more than its own, so one helper
/// going too far only ever stops itself. It doesn't grow with the team: limits that do let a
/// fleet break more between its agents the bigger it gets, while a fixed shared one holds
/// (the Irreversibility Budget, arXiv 2609.00275), and helpers making the same bad choice
/// usually share a cause.
const TOGETHER: f64 = 2.0 - 1e-9;

pub struct Engine {
    pub config: Config,
    /// Why the rules on disk aren't the ones in force, if they aren't.
    pub problem: Option<String>,
    pub store: Store,
    events: PathBuf,
    /// Free memory now, read only when a rule waits for it to run short.
    pub memory: fn() -> Option<u64>,
    /// The load average now, read only when a rule waits for the machine to be busy.
    pub load: fn() -> Option<f64>,
}

/// `*` is one path segment, `**` any depth; a pattern also covers everything below it.
fn glob_matches(glob: &str, dir: &Path) -> bool {
    glob_regex(glob).is_some_and(|re| re.find(&dir.to_string_lossy()).is_some())
}

/// A project path as the regex `glob_matches` tries, for trying it on many directories.
pub(crate) fn glob_regex(glob: &str) -> Option<regress::Regex> {
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
    regress::Regex::new(&body).ok()
}

/// The reason in a `reaper ask "<why>"` command, run by itself.
fn asking(c: &Check) -> Option<String> {
    if !c.tool.eq_ignore_ascii_case("bash") {
        return None;
    }
    let command = c.args["command"].as_str()?.trim();
    let re = regress::Regex::new(r"^(\S*/)?reaper\s+ask\b\s*([\s\S]*)$").expect("a valid regex");
    let m = re.find(command)?;
    let why = m.group(2).map_or("", |g| &command[g]).trim();
    let why = why.strip_prefix(['"', '\'']).and_then(|w| w.strip_suffix(['"', '\''])).unwrap_or(why);
    Some(if why.is_empty() { "(no reason given)".to_string() } else { why.to_string() })
}

/// What a shell command would change about reaper itself, said for the person: `reaper rules
/// remove`, `reaper rules add --replace` and `reaper uninstall`, however they're run (`sudo`, a
/// full path, `bash -c`). An agent never hears its rules, and these would let it take one out or
/// loosen it, or take reaper away.
fn changes_reaper(c: &Check) -> Option<String> {
    if !c.tool.eq_ignore_ascii_case("bash") {
        return None;
    }
    let command = c.args["command"].as_str()?;
    let changes: Vec<String> = shell::commands(command)
        .iter()
        .map(|words| shell::bare(words))
        .filter(|words| words[0] == "reaper")
        .filter_map(|words| {
            let words: Vec<&str> = words[1..].iter().map(String::as_str).collect();
            match words.as_slice() {
                ["uninstall", agent @ ..] => {
                    let agent = if agent.contains(&"pi") { "pi" } else { "Claude Code" };
                    Some(format!("stop reaper guarding {agent}"))
                }
                // `--check` only says whether it could.
                ["rules", "remove", rest @ ..] if !rest.contains(&"--check") => {
                    Some(match rest.iter().find(|w| !w.starts_with("--")) {
                        Some(id) => format!("take the rule {id} out of your rules"),
                        None => "take a rule out of your rules".to_string(),
                    })
                }
                // A snippet given in a heredoc is the command's last word.
                ["rules", "add", rest @ ..] if rest.contains(&"--replace") && !rest.contains(&"--check") => {
                    Some(match rest.last().and_then(|snippet| draft::read(snippet).ok()) {
                        Some(new) => format!("change the rule {} in your rules", new.rule.id),
                        None => "change a rule in your rules".to_string(),
                    })
                }
                _ => None,
            }
        })
        .collect();
    // The same change several times over is said once, with how many.
    let mut said: Vec<(String, usize)> = Vec::new();
    for change in changes {
        match said.iter_mut().find(|(c, _)| *c == change) {
            Some((_, n)) => *n += 1,
            None => said.push((change, 1)),
        }
    }
    let said: Vec<String> = said.into_iter().map(|(c, n)| if n > 1 { format!("{c} ({n} times)") } else { c }).collect();
    (!said.is_empty()).then(|| said.join(" and "))
}

/// Where a call works: the session's directory, the file it names, and wherever its commands
/// `cd` to. A project's rules apply when any of them is inside the project.
pub(crate) fn places(dir: &Path, args: &Value) -> Vec<PathBuf> {
    let mut places = vec![dir.to_path_buf()];
    if let Some(path) = args["path"].as_str() {
        places.push(resolve(dir, path));
    }
    let mut here = dir.to_path_buf();
    for words in shell::commands(args["command"].as_str().unwrap_or_default()) {
        if matches!(words[0].as_str(), "cd" | "pushd") {
            match words[1..].iter().find(|w| *w == "-" || !w.starts_with('-')) {
                Some(back) if back == "-" => continue,
                Some(to) => here = resolve(&here, to),
                None => here = paths::home(),
            }
            places.push(here.clone());
        }
    }
    places
}

/// `to`, from `dir`: `~` and `$HOME` expanded, and `.` and `..` worked out without looking at
/// the disk.
fn resolve(dir: &Path, to: &str) -> PathBuf {
    let to = ["$HOME", "${HOME}"]
        .iter()
        .find_map(|home| to.strip_prefix(home))
        .map_or_else(|| expand_home(to), |rest| paths::home().join(rest.trim_start_matches('/')));
    let mut out = PathBuf::new();
    for part in dir.join(to).components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A call's arguments as the log keeps them: canonical JSON, cut short (see `MAX_ARG`).
fn logged(args: &Value) -> String {
    fn cut(value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(s.chars().take(MAX_ARG).collect()),
            Value::Array(items) => Value::Array(items.iter().map(cut).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), cut(v))).collect()),
            other => other.clone(),
        }
    }
    canonical(&cut(args)).chars().take(MAX_ARGS).collect()
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
        strikes: None,
        free: None,
        load: None,
        ask: false,
        unknown: Default::default(),
    }
}

/// Agents never get to open, list or run anything in reaper's own directories, however
/// they spell them (absolute, `~/…`, `$HOME/…`, `${HOME}/…`, `$XDG_CONFIG_HOME/reaper`…).
/// Only `path` and `command` are checked: a file whose contents merely mention a directory
/// is fine to write.
fn self_rules() -> [Rule; 2] {
    let home = paths::home().to_string_lossy().into_owned();
    let mut spellings = Vec::new();
    for dir in [paths::config_dir(), paths::state_dir(), paths::cache_dir(), paths::pi_extension_dir()] {
        let dir = dir.to_string_lossy().into_owned();
        if let Some(rest) = dir.strip_prefix(&home).filter(|r| r.starts_with('/') && !home.is_empty()) {
            spellings.extend([format!("~{rest}"), format!("$HOME{rest}"), format!("${{HOME}}{rest}")]);
        }
        spellings.push(dir);
    }
    for var in ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        spellings.extend([format!("${var}/reaper"), format!("${{{var}}}/reaper")]);
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
        strikes: None,
        free: None,
        load: None,
        ask: false,
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
        Self { config, problem, store, events, memory: pressure::available, load: pressure::load }
    }

    pub fn rules_for(&self, dir: &Path) -> (Option<String>, Vec<&Rule>) {
        self.rules_at(&[dir.to_path_buf()])
    }

    /// The projects any of `places` is in, and their rules.
    pub fn rules_at(&self, places: &[PathBuf]) -> (Option<String>, Vec<&Rule>) {
        let inside = |glob: &String| places.iter().any(|place| glob_matches(glob, place));
        let projects: Vec<(&String, &reaper_core::rules::Project)> =
            self.config.projects.iter().filter(|(_, p)| p.paths.iter().any(inside)).collect();
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
        // Claude Code starts afresh for every check: warming it would only spend the
        // subscription.
        if !global.iter().chain(&project).any(|r| r.judge.is_some())
            || watcher.kind.as_deref() == Some(backend::CLAUDE_CODE)
        {
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
        // A lock-free read first: only an agent's first call writes.
        let team = self.store.session(&c.host, &c.session).team;
        if let Some(rule) = team.stopped {
            return Decision::Stopped { rule };
        }
        if !team.members.contains(&c.agent) {
            let _ = self.store.update(&c.host, &c.session, |s| s.team.members.insert(c.agent.clone()));
        }
        if let Some(why) = asking(c) {
            return self.ask(c, &team, why);
        }
        let args = portable(&c.args);
        let own = self_rules();
        let global: Vec<&Rule> = own.iter().chain(&self.config.rules).collect();
        let (_, project) = self.rules_at(&places(&c.dir, &args));
        let rules = || global.iter().chain(&project);
        let room = Room {
            free: rules().any(|r| r.free.is_some()).then(self.memory).flatten(),
            load: rules().any(|r| r.load.is_some()).then(self.load).flatten(),
        };
        let watcher = self.config.watcher.as_ref();
        let backend = watcher.and_then(|w| backend::from_config(w, &self.store));
        let settings = Settings {
            backend: backend.as_deref(),
            kill_on_error: watcher.and_then(|w| w.on_error.as_deref()) == Some("kill"),
            timeout: watcher
                .and_then(|w| w.timeout_ms)
                .map_or_else(
                    || match watcher.and_then(|w| w.kind.as_deref()) {
                        Some(backend::CLAUDE_CODE) => CLAUDE_CODE_TIMEOUT,
                        _ => DEFAULT_TIMEOUT,
                    },
                    Duration::from_millis,
                )
                .min(backend::BUDGET),
            room,
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
                // After the rules: whatever else the command runs is judged first.
                if let Some(change) = changes_reaper(c) {
                    self.event(&c.host, &c.agent, "agent.asked", json!({ "tool": c.tool, "change": change }));
                    return Decision::Confirm { change };
                }
                // A message that passes isn't an action: nothing was done.
                if c.tool.eq_ignore_ascii_case(reaper_core::rules::MESSAGE) {
                    let text: String = c.args["text"].as_str().unwrap_or_default().chars().take(MAX_SAID).collect();
                    self.event(&c.host, &c.agent, "agent.said", json!({ "text": text }));
                } else {
                    self.event(&c.host, &c.agent, "agent.acted", json!({ "tool": c.tool, "args": logged(&c.args) }));
                    self.changed(c, &args);
                }
                return Decision::Allow;
            }
        };

        // An exception is the person's to give, one call at a time, for a rule that allows asking,
        // once per rule until their next prompt. A reply has nothing to allow.
        let askable = rule.ask
            && !rule.stops()
            && !c.tool.eq_ignore_ascii_case(reaper_core::rules::MESSAGE)
            && !team.asked.contains(&rule.id);
        let call = hash(&[&c.tool.to_lowercase(), &canonical(&args)]);
        let asked = self.store.session(&c.host, &c.agent).asking.filter(|(r, h, _)| *r == rule.id && *h == call);
        if let (true, Some((_, _, why))) = (askable, asked) {
            let _ = self.store.update(&c.host, &c.agent, |s| {
                s.asking = None;
                s.blocked = None;
            });
            let _ = self.store.update(&c.host, &c.session, |s| s.team.asked.insert(rule.id.clone()));
            let attempt: String = canonical(&c.args).chars().take(MAX_ATTEMPT).collect();
            self.event(
                &c.host,
                &c.agent,
                "agent.asked",
                json!({ "tool": c.tool, "rule": rule.id, "attempt": attempt, "why": why }),
            );
            let redirect = rule.redirect.clone().unwrap_or_else(|| DEFAULT_REDIRECT.to_string());
            return Decision::Ask { rule: rule.id.clone(), redirect, why };
        }

        // A strike costs one over the rule's limit of the agent's allowance. A rule that waits for
        // the machine to be short (`free`, `load`) says "not now", which the agent couldn't have
        // seen coming: it costs nothing, unless the agent keeps making the same call.
        let waits = rule.free.is_some() || rule.load.is_some();
        let share =
            if waits && !self.repeated(c, &args) { 0.0 } else { 1.0 / f64::from(rule.limit(self.config.strikes)) };
        let (strike, mine) = self
            .store
            .update(&c.host, &c.agent, |s| {
                let n = s.strikes.entry(rule.id.clone()).or_default();
                *n += 1;
                s.pressure += share;
                s.blocked = askable.then(|| (rule.id.clone(), call.clone()));
                s.asking = None;
                // Going in circles is about one call, gone once the context is compacted.
                if let Some(redirect) =
                    rule.redirect.as_ref().filter(|r| rule.id != looping.id && !s.redirects.contains(r))
                {
                    s.redirects.push(redirect.clone());
                }
                (*n, s.pressure)
            })
            .unwrap_or((1, share));
        // The session's pressure is its agents', each counting for no more than all of theirs.
        let mut members = team.members;
        members.insert(c.agent.clone());
        let ours: f64 = members
            .iter()
            .map(|m| if *m == c.agent { mine } else { self.store.session(&c.host, m).pressure })
            .map(|p| p.min(1.0))
            .sum();
        // A lone agent is its own session: its own limit is the session's.
        let together = members.len() > 1 && ours >= TOGETHER;
        if together {
            let _ = self.store.update(&c.host, &c.session, |s| s.team.stopped = Some(rule.id.clone()));
        }
        let is_final = together || mine >= FULL;
        let attempt: String = canonical(&c.args).chars().take(MAX_ATTEMPT).collect();
        self.event(
            &c.host,
            &c.agent,
            "agent.killed",
            json!({ "tool": c.tool, "rule": rule.id, "tier": tier.as_str(), "strike": strike, "final": is_final, "attempt": attempt, "pressure": mine }),
        );
        if together {
            return Decision::Stopped { rule: rule.id.clone() };
        }
        let mut redirect = rule.redirect.clone().unwrap_or_else(|| DEFAULT_REDIRECT.to_string());
        // Told how much room there is, an agent can size its next step to it. A fixed limit
        // in the redirect ("-n 2 at most") gets followed to the letter instead, however big
        // the step it's applied to.
        let (mut short, mut figures) = (Vec::new(), Vec::new());
        if let (true, Some(free)) = (rule.short_of_memory(&room), room.free) {
            short.push("low on memory");
            figures.push(format!("{} free", size_text(free)));
        }
        if let (true, Some(load)) = (rule.busy(&room), room.load) {
            short.push("busy");
            figures.push(format!("load {load:.0}"));
        }
        if !short.is_empty() {
            redirect =
                format!("This machine is {} right now ({}). {redirect}", short.join(" and "), figures.join(", "));
        }
        // What the watcher found, in the agent's own words: in a long file or PR body, the
        // redirect alone doesn't say which sentence to change.
        if let Some(quote) = &outcome.quote {
            redirect = format!("{redirect} The part to change: \"{quote}\"");
        }
        if askable && !is_final {
            redirect = format!("{redirect} {ASKING}");
        }
        Decision::Kill { rule: rule.id.clone(), tier, strike, is_final, redirect }
    }

    /// A new prompt from the human: a stopped session starts again, and every rule can be asked
    /// about once more.
    pub fn restart(&self, host: &str, session: &str) {
        let team = self.store.session(host, session).team;
        if team.stopped.is_some() || !team.asked.is_empty() {
            let _ = self.store.update(host, session, |s| {
                s.team.stopped = None;
                s.team.asked.clear();
            });
        }
    }

    /// The agent's context was compacted. The summary can drop what reaper told it (summaries
    /// of real sessions dropped what not to touch about six times as often as what to build,
    /// arXiv 2610.31778), and restating a dropped constraint after compaction cut violations
    /// (arXiv 2608.11242). Its redirects since the human's last prompt, to hear again; `None`
    /// if it had none.
    pub fn compacted(&self, host: &str, agent: &str) -> Option<String> {
        let redirects = self.store.session(host, agent).redirects;
        if redirects.is_empty() {
            return None;
        }
        let mut text = String::from("Carried over from earlier in this task:\n");
        for redirect in redirects {
            text += &format!("\n- {redirect}");
        }
        Some(text)
    }

    /// `reaper ask "<why>"`: the agent asks the person to allow, once, the call reaper just
    /// blocked. Nothing runs; the agent is told what happens next.
    fn ask(&self, c: &Check, team: &store::Team, why: String) -> Decision {
        let note = |message: &str| Decision::Noted { message: message.to_string() };
        let Some((rule, call)) = self.store.session(&c.host, &c.agent).blocked else {
            return note("Nothing reaper blocked can be asked about now. Continue the task another way.");
        };
        if team.asked.contains(&rule) {
            return note("The person has answered about this already, until their next message. Continue another way.");
        }
        let _ = self.store.update(&c.host, &c.agent, |s| s.asking = Some((rule.clone(), call.clone(), why.clone())));
        note("Asked. Make the same call again: the person sees your reason and decides.")
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
        // Each push a different one: the same call again is the agent asking the person.
        let push = |to: &str| call("bash", json!({ "command": format!("git push -f {to}") }));
        assert!(
            matches!(engine.check(&push("a")), Decision::Kill { strike: 1, is_final: false, ref redirect, .. } if redirect == "Open a pull request.")
        );
        assert!(matches!(engine.check(&push("b")), Decision::Kill { strike: 2, is_final: true, .. }));
        assert_eq!(engine.check(&call("bash", json!({ "command": "git status" }))), Decision::Allow);
        assert_eq!(sandbox.events(), ["agent.checked", "agent.killed", "agent.checked", "agent.killed", "agent.acted"]);

        // A subagent's strikes are its own: the session it works for starts clean.
        let sandbox = Sandbox::new("strikes-sub");
        let engine = sandbox.engine("strikes: 2\nrules: [{ id: push, match: { command: 'git push -f' } }]");
        engine.check(&Check { agent: "sub".into(), ..push("a") });
        assert!(matches!(
            engine.check(&Check { agent: "sub".into(), ..push("b") }),
            Decision::Kill { strike: 2, is_final: true, .. }
        ));
        assert!(matches!(engine.check(&push("a")), Decision::Kill { strike: 1, is_final: false, .. }));
    }

    #[test]
    fn a_rule_can_allow_more_or_fewer_strikes_and_they_add_up_across_rules() {
        let sandbox = Sandbox::new("limits");
        let engine = sandbox.engine(
            "strikes: 3
rules:
  - { id: lint, match: { command: lint }, strikes: 6 }
  - { id: prod, match: { command: prod }, strikes: 1 }
  - { id: push, match: { command: push } }",
        );
        // Each on its own, so none takes the others with it.
        let run = |agent: &str, command: &str| {
            let call = call("bash", json!({ "command": command }));
            engine.check(&Check { session: agent.into(), agent: agent.into(), ..call })
        };
        for i in 0..5 {
            assert!(matches!(run("a", &format!("lint {i}")), Decision::Kill { is_final: false, .. }));
        }
        assert!(matches!(run("a", "lint 5"), Decision::Kill { strike: 6, is_final: true, .. }));
        assert!(matches!(run("b", "prod"), Decision::Kill { strike: 1, is_final: true, .. }));
        // Half of push's allowance, then half of lint's: all of it.
        run("c", "lint 1");
        run("c", "lint 2");
        run("c", "lint 3");
        assert!(matches!(run("c", "push 1"), Decision::Kill { is_final: false, .. }));
        assert!(matches!(run("c", "push 2"), Decision::Kill { strike: 2, is_final: true, .. }));
    }

    #[test]
    fn strikes_stay_however_much_work_comes_between() {
        let sandbox = Sandbox::new("no-fade");
        let engine = sandbox.engine("strikes: 3\nrules: [{ id: push, match: { command: push } }]");
        let push = || engine.check(&call("bash", json!({ "command": "git push" })));
        for round in 0..2 {
            assert!(matches!(push(), Decision::Kill { is_final: false, .. }));
            for i in 0..20 {
                engine.check(&call("bash", json!({ "command": format!("make step{round}-{i}") })));
            }
        }
        assert!(matches!(push(), Decision::Kill { strike: 3, is_final: true, .. }));
    }

    #[test]
    fn a_session_whose_agents_keep_breaking_rules_is_stopped_together_until_the_next_prompt() {
        let sandbox = Sandbox::new("together");
        let engine = sandbox.engine("strikes: 2\nrules: [{ id: push, match: { command: push } }]");
        let as_ = |agent: &str, command: &str| {
            engine.check(&Check { agent: agent.into(), ..call("bash", json!({ "command": command })) })
        };
        assert_eq!(as_("s1", "ls"), Decision::Allow);
        as_("a", "push 1");
        as_("b", "push 1");
        assert!(matches!(as_("a", "push 2"), Decision::Kill { is_final: true, .. }), "a goes alone");
        assert_eq!(as_("b", "push 2"), Decision::Stopped { rule: "push".into() }, "then b takes everyone with it");
        assert_eq!(as_("s1", "ls"), Decision::Stopped { rule: "push".into() }, "the main agent too");
        engine.restart("test", "s1");
        assert_eq!(as_("s1", "ls"), Decision::Allow, "a new prompt starts again");

        // A big team gets no more room: two going too far stop everyone.
        let sandbox = Sandbox::new("together-big");
        let engine = sandbox.engine("strikes: 1\nrules: [{ id: push, match: { command: push } }]");
        let as_ = |agent: &str, command: &str| {
            engine.check(&Check { agent: agent.into(), ..call("bash", json!({ "command": command })) })
        };
        for helper in 0..10 {
            assert_eq!(as_(&format!("h{helper}"), "make"), Decision::Allow);
        }
        assert!(matches!(as_("h0", "push"), Decision::Kill { is_final: true, .. }));
        assert_eq!(as_("h1", "push"), Decision::Stopped { rule: "push".into() });

        // A lone agent is only ever stopped by its own limit, and a new prompt brings it back.
        let sandbox = Sandbox::new("alone");
        let engine = sandbox.engine("strikes: 1\nrules: [{ id: push, match: { command: push } }]");
        assert!(matches!(
            engine.check(&call("bash", json!({ "command": "push" }))),
            Decision::Kill { is_final: true, .. }
        ));
        assert_eq!(engine.check(&call("bash", json!({ "command": "ls" }))), Decision::Allow);
    }

    #[test]
    fn compacted_an_agent_hears_again_what_it_was_told_since_the_last_prompt() {
        let sandbox = Sandbox::new("compacted");
        let engine = sandbox.engine(
            "strikes: 9\nrepeats: 2\nrules:\n  - { id: push, match: { command: 'git push -f' }, redirect: Open a pull request. }\n  - { id: prod, match: { command: prod }, redirect: Use staging. }\n  - { id: quiet, match: { command: rm } }\n",
        );
        let run = |command: &str| engine.check(&call("bash", json!({ "command": command })));
        assert_eq!(engine.compacted("test", "s1"), None, "nothing told, nothing to tell again");
        for command in ["git push -f a", "psql prod", "git push -f b", "rm x", "make", "make"] {
            run(command);
        }
        // Each once, oldest first; going in circles and the default redirect are left out.
        assert_eq!(
            engine.compacted("test", "s1").as_deref(),
            Some("Carried over from earlier in this task:\n\n- Open a pull request.\n- Use staging.")
        );
        assert!(engine.prompted("test", "s1", "Now update the docs."));
        assert_eq!(engine.compacted("test", "s1"), None, "a new task starts the list over");
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
        assert_eq!(
            sandbox.events(),
            ["agent.checked", "agent.killed", "agent.said"],
            "a message that passes is no action"
        );
    }

    #[test]
    fn an_allowed_call_is_logged_with_its_arguments_cut_short() {
        let sandbox = Sandbox::new("acted");
        let engine = sandbox.engine("rules: []");
        let whole = "x".repeat(5000);
        engine.check(&call("write", json!({ "file_path": "/work/big.txt", "content": whole })));
        engine.check(&call("message", json!({ "text": whole })));
        let log = fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let events: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let args: Value = serde_json::from_str(events[0]["args"].as_str().unwrap()).unwrap();
        assert_eq!(
            args,
            json!({ "content": "x".repeat(MAX_ARG), "file_path": "/work/big.txt" }),
            "the path outlives the file"
        );
        assert_eq!(events[1]["text"].as_str().unwrap().chars().count(), MAX_SAID);
    }

    #[test]
    fn a_rule_with_free_waits_for_memory_to_run_short_and_says_how_short() {
        let sandbox = Sandbox::new("free");
        // The example in examples/rules.yaml.
        let mut engine = sandbox.engine(
            r#"strikes: 10
rules:
  - id: whole-test-suite
    tools: [bash]
    match: { command: '^(pytest|vitest|jest|(npm|pnpm|yarn) (run )?test)\b(?!.*(::|\.(py|[jt]sx?)\b|\$))' }
    free: 4G
    redirect: Run the test files for the code you changed.
"#,
        );
        let run = |engine: &Engine, command: &str| engine.check(&call("bash", json!({ "command": command })));
        engine.memory = || Some(12 << 30);
        assert_eq!(run(&engine, "uv run pytest -n 2 --dist loadgroup"), Decision::Allow, "room to spare");
        engine.memory = || Some(1_932_735_283);
        assert!(
            matches!(run(&engine, "cd backend && uv run pytest -n 2 --dist loadgroup"), Decision::Kill { ref redirect, .. }
                if redirect.starts_with("This machine is low on memory right now (1.8G free). Run the test files for the code you changed."))
        );
        for wide in ["npm test", "pytest tests/ -x", "pnpm vitest run 2>&1 | tail -5"] {
            assert!(matches!(run(&engine, wide), Decision::Kill { .. }), "{wide}");
        }
        for fine in [
            "pytest app/api/users_test.py -q",
            "pnpm vitest run src/lib/a.test.ts",
            "cat pytest.ini",
            "pgrep -af pytest",
            "for f in a_test.py b_test.py; do pytest -q $f; done",
        ] {
            assert_eq!(run(&engine, fine), Decision::Allow, "{fine}");
        }
        engine.memory = || None;
        assert_eq!(run(&engine, "npm test"), Decision::Allow, "memory that can't be read never kills");
    }

    #[test]
    fn an_agent_asks_on_purpose_for_one_call_once_per_rule_until_the_next_prompt() {
        let sandbox = Sandbox::new("ask");
        let engine = sandbox.engine(
            "strikes: 9\nrules:\n  - { id: push, match: { command: 'git push -f' }, redirect: Open a pull request., ask: true }\n  - { id: dev, match: { command: 'push dev' } }",
        );
        let run = |command: &str| engine.check(&call("bash", json!({ "command": command })));
        let noted =
            |d: Decision, start: &str| matches!(d, Decision::Noted { ref message } if message.starts_with(start));
        assert!(noted(run("reaper ask 'please'"), "Nothing"), "nothing blocked yet");
        assert!(
            matches!(run("git push -f"), Decision::Kill { strike: 1, ref redirect, .. } if redirect.ends_with(ASKING))
        );
        assert!(
            matches!(run("git push -f"), Decision::Kill { strike: 2, .. }),
            "a plain retry is blocked, not asked about"
        );
        assert!(noted(run("reaper ask \"my own branch, nobody else pushes to it\""), "Asked."));
        assert_eq!(
            run("git push -f"),
            Decision::Ask {
                rule: "push".into(),
                redirect: "Open a pull request.".into(),
                why: "my own branch, nobody else pushes to it".into()
            }
        );
        assert_eq!(sandbox.events().iter().filter(|e| *e == "agent.asked").count(), 1);
        // Answered: until the person's next prompt, blocked, with no offer to ask.
        assert!(
            matches!(run("git push -f"), Decision::Kill { strike: 3, ref redirect, .. } if !redirect.contains(ASKING))
        );
        assert!(noted(run("reaper ask 'again'"), "Nothing"));
        engine.restart("test", "s1");
        assert!(
            matches!(run("git push -f"), Decision::Kill { ref redirect, .. } if redirect.ends_with(ASKING)),
            "a new prompt"
        );
        // A rule that doesn't allow asking: just blocked.
        assert!(matches!(run("push dev"), Decision::Kill { ref redirect, .. } if !redirect.contains(ASKING)));
        assert!(noted(run("reaper ask 'please'"), "Nothing"));

        // A stop rule is never asked about, even with `ask` (lint says so).
        let sandbox = Sandbox::new("ask-stop");
        let engine = sandbox.engine("rules: [{ id: prod, match: { command: prod }, action: stop, ask: true }]");
        assert!(
            matches!(engine.check(&call("bash", json!({ "command": "psql prod" }))), Decision::Kill { ref redirect, .. } if !redirect.contains(ASKING))
        );
    }

    #[test]
    fn taking_a_rule_out_or_reaper_away_is_the_persons_to_allow() {
        let sandbox = Sandbox::new("confirm");
        let engine = sandbox.engine("rules: [{ id: push, match: { command: 'git push -f' } }]");
        let run = |command: &str| engine.check(&call("bash", json!({ "command": command })));
        let confirm = |change: &str| Decision::Confirm { change: change.into() };
        assert_eq!(run("reaper rules remove prod-db"), confirm("take the rule prod-db out of your rules"));
        assert_eq!(run("~/.cargo/bin/reaper uninstall pi"), confirm("stop reaper guarding pi"));
        assert_eq!(
            run("cd /w && reaper rules remove 'a' && reaper uninstall claude-code"),
            confirm("take the rule a out of your rules and stop reaper guarding Claude Code")
        );
        assert_eq!(run("sudo ~/.cargo/bin/reaper uninstall"), confirm("stop reaper guarding Claude Code"));
        assert_eq!(run("bash -c 'reaper rules remove push'"), confirm("take the rule push out of your rules"));
        assert_eq!(run("grep -n 'reaper uninstall' README.md"), Decision::Allow, "a mention");
        let loosened = "reaper rules add --replace <<'EOF'\nrule: { id: push, match: { command: '^never' } }\nEOF";
        assert_eq!(run(loosened), confirm("change the rule push in your rules"));
        assert_eq!(run("reaper rules add --replace < new.yaml"), confirm("change a rule in your rules"));
        assert_eq!(
            run("reaper rules add --replace < a.json && reaper rules add --replace < b.json"),
            confirm("change a rule in your rules (2 times)")
        );
        assert_eq!(run("reaper rules add --replace --check < new.yaml"), Decision::Allow);
        assert_eq!(run("reaper rules add < new.yaml"), Decision::Allow, "adding one is fine");
        for fine in
            ["reaper rules --json", "reaper rules remove x --check", "reaper rules add --check", "reaper install pi"]
        {
            assert_eq!(run(fine), Decision::Allow, "{fine}");
        }
        // The rules come first: the rest of the command isn't let through with it.
        assert!(
            matches!(run("reaper rules remove push && git push -f"), Decision::Kill { ref rule, .. } if rule == "push")
        );
        assert_eq!(engine.store.session("test", "s1").strikes.get("push"), Some(&1), "asking is no strike");
    }

    #[test]
    fn a_rule_with_load_waits_for_a_busy_machine_and_says_so() {
        let sandbox = Sandbox::new("load");
        let mut engine = sandbox.engine(
            "strikes: 9\nrules: [{ id: suite, match: { command: pytest }, free: 12G, load: 16, redirect: Run the test files for the code you changed. }]",
        );
        let run = |engine: &Engine| engine.check(&call("bash", json!({ "command": "pytest" })));
        (engine.memory, engine.load) = (|| Some(20 << 30), || Some(3.0));
        assert_eq!(run(&engine), Decision::Allow, "room and time to spare");
        engine.load = || Some(23.4);
        assert!(
            matches!(run(&engine), Decision::Kill { ref redirect, .. } if redirect.starts_with("This machine is busy right now (load 23). Run the test files"))
        );
        engine.memory = || Some(4 << 30);
        assert!(
            matches!(run(&engine), Decision::Kill { ref redirect, .. } if redirect.starts_with("This machine is low on memory and busy right now (4.0G free, load 23)."))
        );
        engine.load = || None;
        assert!(
            matches!(run(&engine), Decision::Kill { ref redirect, .. } if redirect.starts_with("This machine is low on memory right now (4.0G free).")),
            "what can't be read isn't mentioned"
        );
    }

    #[test]
    fn a_judged_kill_quotes_the_words_the_watcher_found() {
        const ANSWER: &str = r#"{"choices":[{"message":{"content":"{\"violated\":[\"story\"],\"quote\":\"so an attacker can hide javascript: behind a tab\"}"}}]}"#;
        let url = super::backend::tests::picky(Default::default(), "none", ANSWER);
        let sandbox = Sandbox::new("quote");
        let engine = sandbox.engine(&format!(
            "watcher: {{ baseURL: '{url}', model: m }}\nrules: [{{ id: story, judge: telling how a check could be got around, tools: [write], redirect: Say what the code does. }}]"
        ));
        let file = "/** Reads the scheme.\n * Browsers skip control characters, so an attacker can hide javascript: behind a tab.\n */\nexport const x = 1;\n";
        let out = engine.check(&call("write", json!({ "file_path": "a.ts", "content": file })));
        assert!(
            matches!(out, Decision::Kill { ref redirect, .. }
                if redirect == "Say what the code does. The part to change: \"so an attacker can hide javascript: behind a tab\""),
            "{out:?}"
        );
    }

    #[test]
    fn waiting_for_room_costs_nothing_unless_the_agent_keeps_making_the_same_call() {
        let sandbox = Sandbox::new("room-free");
        let mut engine = sandbox
            .engine("repeats: 3\nrules: [{ id: suite, match: { command: pytest }, free: 12G, redirect: Push and let CI run them. }]");
        engine.memory = || Some(4 << 30);
        let run = |command: &str| engine.check(&call("bash", json!({ "command": command })));
        for file in ["a_test.py", "b_test.py", "c_test.py", "d_test.py"] {
            assert!(matches!(run(&format!("pytest {file}")), Decision::Kill { is_final: false, .. }), "{file}");
        }
        assert_eq!(engine.store.session("test", "s1").pressure, 0.0, "a short machine is no strike");
        run("pytest a_test.py");
        run("pytest a_test.py");
        assert!(matches!(run("pytest a_test.py"), Decision::Kill { is_final: false, .. }));
        assert!(engine.store.session("test", "s1").pressure > 0.0, "the same call over and over is");
    }

    #[test]
    fn strikes_survive_across_processes() {
        let sandbox = Sandbox::new("persist");
        let rules = "strikes: 2\nrules: [{ id: push, match: { command: push } }]";
        let push = |to: &str| call("bash", json!({ "command": format!("push {to}") }));
        assert!(matches!(sandbox.engine(rules).check(&push("a")), Decision::Kill { strike: 1, .. }));
        assert!(matches!(sandbox.engine(rules).check(&push("b")), Decision::Kill { strike: 2, is_final: true, .. }));
    }

    /// A hook that runs past its host's timeout lets the call through, so a check never waits out
    /// a session's lock: whatever holds it (something stuck, or the agent on purpose), the call
    /// is refused, its strike unrecorded.
    #[test]
    fn a_violation_is_refused_even_when_its_strike_cant_be_recorded() {
        let sandbox = Sandbox::new("held-lock");
        let engine = sandbox.engine("rules: [{ id: push, match: { command: 'git push -f' } }]");
        let push = call("bash", json!({ "command": "git push -f" }));
        let start = std::time::Instant::now();
        let held = engine.store.update("test", "s1", |_| engine.check(&push)).unwrap();
        assert!(matches!(held, Decision::Kill { strike: 1, .. }), "{held:?}");
        assert!(start.elapsed() < Duration::from_secs(5), "waited {:?}", start.elapsed());
        assert!(engine.store.session("test", "s1").strikes.is_empty(), "nothing was recorded meanwhile");
    }

    #[test]
    fn reapers_own_files_are_off_limits_however_they_are_spelled() {
        let sandbox = Sandbox::new("self");
        let engine = sandbox.engine("");
        let config = paths::config_dir().to_string_lossy().into_owned();
        let home = paths::home().to_string_lossy().into_owned();
        let tilde = config.replacen(&home, "~", 1);
        let braced = config.replacen(&home, "${HOME}", 1);
        for command in [
            format!("cat {config}/rules.yaml"),
            format!("cat {tilde}/rules.yaml"),
            format!("cat {braced}/rules.yaml"),
            "cat ${XDG_CONFIG_HOME}/reaper/rules.yaml".to_string(),
        ] {
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
    fn a_call_gets_the_rules_of_every_project_it_touches() {
        let sandbox = Sandbox::new("places");
        let engine = sandbox.engine(
            "projects:\n  app: { paths: [/work/app], rules: [{ id: gen, match: { path: /generated/ } }, { id: suite, tools: [bash], match: { command: '^pytest$' } }] }\n",
        );
        let from_tools =
            |tool: &str, args: Value| engine.check(&Check { dir: "/work/tools".into(), ..call(tool, args) });
        let broke = |d: Decision, id: &str| matches!(d, Decision::Kill { ref rule, .. } if rule == id);
        assert!(broke(from_tools("Edit", json!({ "file_path": "/work/app/generated/x.ts" })), "gen"));
        assert!(
            broke(from_tools("edit", json!({ "path": "../app/generated/y.ts" })), "gen"),
            "relative to the session"
        );
        assert!(broke(from_tools("bash", json!({ "command": "cd ../app && pytest" })), "suite"));
        assert_eq!(from_tools("bash", json!({ "command": "pytest" })), Decision::Allow, "not in the project");
        assert_eq!(from_tools("Edit", json!({ "file_path": "/work/tools/generated/x.ts" })), Decision::Allow);
        assert_eq!(resolve(Path::new("/a/b"), "../c/./d"), PathBuf::from("/a/c/d"));
        assert_eq!(resolve(Path::new("/a"), "$HOME/x"), paths::home().join("x"));
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
