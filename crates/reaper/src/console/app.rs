//! Console state and key handling. Drawing lives in `ui.rs` and reads from here.

use std::cell::Cell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use reaper_core::EXAMPLE_RULES;
use reaper_core::lint::{Issue, Level, lint};
use reaper_core::mori::Mori;
use reaper_core::paths::tilde;
use reaper_core::rules::{Rule, RulesFile};
use reaper_core::state::{Agent, Kill, Model, Status};
use reaper_core::tail::{Tail, Update};

use super::mori::Colors;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Live,
    Rules,
    Mori,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Agents,
    Kills,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Nothing,
    Quit,
    Edit { line: Option<usize> },
}

pub struct Notice {
    pub text: String,
    pub bad: bool,
    until: Instant,
}

pub enum RuleRow<'a> {
    Heading { title: String, detail: String },
    Rule { place: Option<&'a str>, rule: &'a Rule },
}

const NOTICE_FOR: Duration = Duration::from_secs(5);
const PAGE: isize = 10;
/// Memento mori runs this far behind the clock, so an event read on the next poll still plays
/// from its start.
const MORI_LAG: i64 = 350;
/// A dead agent leaves the agent list this long after it died, as its session leaves memento mori.
const DEAD_FOR: i64 = 60_000;

pub struct App {
    pub tab: Tab,
    pub pane: Pane,
    pub model: Model,
    tail: Tail,
    pub events_error: Option<String>,
    pub rules: RulesFile,
    pub issues: Vec<Issue>,
    /// Selected agent id; `None` selects the first in display order.
    agent_sel: Option<String>,
    /// Selected kill (index into `model.kills`); `None` follows the newest.
    kill_sel: Option<usize>,
    rule_sel: Option<String>,
    pub notice: Option<Notice>,
    pub tz: TimeZone,
    /// Milliseconds since the epoch, refreshed once a second.
    pub now: i64,
    /// Start of the local day, for "kills today".
    pub today: i64,
    /// Fed the same events as `model`, played on its own clock.
    pub mori: Mori,
    /// When memento mori next wants a frame, as of the last one drawn.
    pub mori_next: Cell<Option<i64>>,
    pub colors: Colors,
    /// A fixed memento mori clock, for frames printed by `--dump` and for tests.
    clock: Option<i64>,
    dirty: bool,
}

impl App {
    pub fn new(rules: PathBuf, events: PathBuf) -> Self {
        let rules = RulesFile::load(rules);
        let issues = lint(&rules.config);
        let mut app = Self {
            tab: Tab::Live,
            pane: Pane::Kills,
            model: Model::default(),
            tail: Tail::new(events),
            events_error: None,
            rules,
            issues,
            agent_sel: None,
            kill_sel: None,
            rule_sel: None,
            notice: None,
            tz: TimeZone::system(),
            now: 0,
            today: 0,
            mori: Mori::new(),
            mori_next: Cell::new(None),
            colors: Colors::detect(),
            clock: None,
            dirty: true,
        };
        app.tick();
        app
    }

    pub fn mori_now(&self) -> i64 {
        self.clock.unwrap_or_else(|| Timestamp::now().as_millisecond() - MORI_LAG)
    }

    pub fn freeze(&mut self, at: i64) {
        self.mori.clear();
        if let Ok(Update::Appended(events) | Update::Reset(events)) = Tail::new(self.events_path()).poll() {
            self.mori.push(events);
        }
        self.set_clock(at);
    }

    pub fn set_clock(&mut self, at: i64) {
        self.clock = Some(at);
        self.mori.advance(at);
        self.dirty = true;
    }

    pub fn mori_wait(&self) -> Option<Duration> {
        if self.tab != Tab::Mori {
            return None;
        }
        let next = [self.mori_next.get(), self.mori.waiting()].into_iter().flatten().min()?;
        Some(Duration::from_millis((next - self.mori_now()).max(0) as u64))
    }

    pub fn events_path(&self) -> &Path {
        self.tail.path()
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn tick(&mut self) {
        match self.tail.poll() {
            Ok(update) => {
                if self.events_error.take().is_some() {
                    self.dirty = true;
                }
                match update {
                    Update::Unchanged => {}
                    Update::Appended(events) => {
                        self.model.apply_all(&events);
                        self.mori.push(events);
                        self.dirty = true;
                    }
                    Update::Reset(events) => {
                        self.model = Model::default();
                        self.model.apply_all(&events);
                        self.mori.clear();
                        self.mori.push(events);
                        self.agent_sel = None;
                        self.kill_sel = None;
                        self.dirty = true;
                    }
                }
            }
            Err(e) => {
                let error = Some(e.to_string());
                if self.events_error != error {
                    self.events_error = error;
                    self.dirty = true;
                }
            }
        }

        if self.rules.refresh() {
            self.issues = lint(&self.rules.config);
            self.dirty = true;
        }

        let now = Timestamp::now();
        if now.as_second() != self.now / 1000 {
            self.now = now.as_millisecond();
            self.today = now.to_zoned(self.tz.clone()).start_of_day().map_or(0, |d| d.timestamp().as_millisecond());
            self.dirty = true;
        }

        if self.notice.as_ref().is_some_and(|n| Instant::now() >= n.until) {
            self.notice = None;
            self.dirty = true;
        }

        let now = self.mori_now();
        let played = self.mori.advance(now);
        if self.tab == Tab::Mori && (played || self.mori_next.get().is_some_and(|t| t <= now)) {
            self.dirty = true;
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, bad: bool) {
        self.notice = Some(Notice { text: text.into(), bad, until: Instant::now() + NOTICE_FOR });
        self.dirty = true;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Command {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Command::Quit;
        }
        if self.tab == Tab::Mori && self.mori_key(key.code) {
            self.dirty = true;
            return Command::Nothing;
        }
        match key.code {
            KeyCode::Char('q') => return Command::Quit,
            KeyCode::Char('1') => self.tab = Tab::Live,
            KeyCode::Char('2') => self.tab = Tab::Rules,
            KeyCode::Char('3') if self.tab != Tab::Mori => {
                self.tab = Tab::Mori;
                self.mori.enter(self.mori_now());
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l')
                if self.tab == Tab::Live =>
            {
                self.pane = match self.pane {
                    Pane::Agents => Pane::Kills,
                    Pane::Kills => Pane::Agents,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::PageUp => self.step(-PAGE),
            KeyCode::PageDown => self.step(PAGE),
            KeyCode::Home | KeyCode::Char('g') => self.step(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.step(isize::MAX),
            KeyCode::Char('e') => return self.edit(),
            _ => return Command::Nothing,
        }
        self.dirty = true;
        Command::Nothing
    }

    /// Keys with their own meaning in memento mori. False for the rest, which work as anywhere.
    fn mori_key(&mut self, code: KeyCode) -> bool {
        let now = self.mori_now();
        match code {
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.mori.switch(1, now),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.mori.switch(-1, now),
            KeyCode::Up | KeyCode::Char('k') => self.mori.pick(1, now),
            KeyCode::Down | KeyCode::Char('j') => self.mori.pick(-1, now),
            KeyCode::Esc => {
                if !self.mori.unpick() {
                    self.mori.follow_the_action(now);
                }
            }
            KeyCode::Enter => {
                let Some(id) = self.mori.picked().map(str::to_string) else { return true };
                self.tab = Tab::Live;
                self.pane = Pane::Agents;
                self.agent_sel = Some(id);
            }
            // A death for fun, played by stand-ins on a level of their own.
            KeyCode::Char('d') => {
                self.mori.rehearse(now);
            }
            _ => return false,
        }
        true
    }

    fn step(&mut self, delta: isize) {
        fn moved(current: usize, delta: isize, len: usize) -> usize {
            current.saturating_add_signed(delta).min(len.saturating_sub(1))
        }
        match (self.tab, self.pane) {
            (Tab::Live, Pane::Agents) => {
                let ids: Vec<String> = self.agents().iter().map(|a| a.id.clone()).collect();
                if let Some(i) = self.selected_agent_row() {
                    self.agent_sel = Some(ids[moved(i, delta, ids.len())].clone());
                }
            }
            (Tab::Live, Pane::Kills) => {
                let len = self.model.kills.len();
                if let Some(row) = self.selected_kill_row() {
                    let row = moved(row, delta, len);
                    // The top row follows new kills as they arrive.
                    self.kill_sel = (row > 0).then(|| len - 1 - row);
                }
            }
            (Tab::Mori, _) => {}
            (Tab::Rules, _) => {
                let ids: Vec<String> = self
                    .rule_rows()
                    .iter()
                    .filter_map(|r| match r {
                        RuleRow::Rule { rule, .. } => Some(rule.id.clone()),
                        RuleRow::Heading { .. } => None,
                    })
                    .collect();
                if !ids.is_empty() {
                    let i = self.rule_sel.as_ref().and_then(|s| ids.iter().position(|id| id == s)).unwrap_or(0);
                    self.rule_sel = Some(ids[moved(i, delta, ids.len())].clone());
                }
            }
        }
    }

    fn edit(&mut self) -> Command {
        if !self.rules.exists {
            match create_example(&self.rules.path) {
                Ok(()) => {
                    self.rules.refresh();
                    self.issues = lint(&self.rules.config);
                    self.notify(
                        format!("Created {} from the example; its global rules apply now.", tilde(&self.rules.path)),
                        false,
                    );
                }
                Err(e) => {
                    self.notify(format!("Couldn't create {}: {e}", tilde(&self.rules.path)), true);
                    return Command::Nothing;
                }
            }
        }
        // A syntax error is the most urgent place to be; otherwise, the rule behind the selection.
        if let Some(line) = self.rules.error.as_ref().and_then(|e| e.line) {
            return Command::Edit { line: usize::try_from(line).ok() };
        }
        let rule = match (self.tab, self.pane) {
            (Tab::Rules, _) => self.selected_rule().map(|(_, r)| r.id.clone()),
            (Tab::Live, Pane::Kills) => self.selected_kill().map(|k| k.rule.clone()),
            (Tab::Live, Pane::Agents) => None,
            (Tab::Mori, _) => self.mori.picked_rule().map(str::to_string),
        };
        Command::Edit { line: rule.and_then(|id| self.rules.line_of(&id)) }
    }

    pub fn after_edit(&mut self, result: Result<(), String>) {
        if let Err(e) = result {
            return self.notify(e, true);
        }
        self.rules.refresh();
        self.issues = lint(&self.rules.config);
        if let Some(e) = &self.rules.error {
            let at = e.line.map(|l| format!(" at line {l}")).unwrap_or_default();
            return self.notify(format!("Syntax error{at}: running agents keep the previous rules."), true);
        }
        let errors = self.issues.iter().filter(|i| i.level == Level::Error).count();
        let warnings = self.issues.len() - errors;
        match (errors, warnings) {
            (0, 0) => self.notify("Rules saved. No issues; agents pick them up on their next action.", false),
            _ => self.notify(
                format!("Rules saved with {}, {}.", plural(errors, "error"), plural(warnings, "warning")),
                errors > 0,
            ),
        }
    }

    // ── what the UI shows ──────────────────────────────────────────────────

    /// The dead leave the list a minute after they died, as they leave memento mori; their
    /// kills stay in the kill log. The reaped stay: they can be resumed.
    pub fn agents(&self) -> Vec<&Agent> {
        let gone = |a: &&Agent| a.status == Status::Dead && self.now - a.last_at >= DEAD_FOR;
        let mut agents: Vec<&Agent> = self.model.agents.iter().filter(|a| !gone(a)).collect();
        agents.sort_by_key(|a| (matches!(a.status, Status::Dead | Status::Reaped), std::cmp::Reverse(a.last_at)));
        agents
    }

    pub fn selected_agent_row(&self) -> Option<usize> {
        let agents = self.agents();
        if agents.is_empty() {
            return None;
        }
        Some(self.agent_sel.as_ref().and_then(|id| agents.iter().position(|a| &a.id == id)).unwrap_or(0))
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.selected_agent_row().map(|i| self.agents()[i])
    }

    pub fn kills(&self) -> impl Iterator<Item = (usize, &Kill)> {
        self.model.kills.iter().enumerate().rev()
    }

    pub fn selected_kill_row(&self) -> Option<usize> {
        let len = self.model.kills.len();
        if len == 0 {
            return None;
        }
        Some(self.kill_sel.filter(|&i| i < len).map_or(0, |i| len - 1 - i))
    }

    pub fn selected_kill(&self) -> Option<&Kill> {
        self.selected_kill_row().map(|row| &self.model.kills[self.model.kills.len() - 1 - row])
    }

    pub fn rule_rows(&self) -> Vec<RuleRow<'_>> {
        let config = &self.rules.config;
        let mut rows = Vec::new();
        if !config.rules.is_empty() {
            rows.push(RuleRow::Heading { title: "global".into(), detail: "every project".into() });
            rows.extend(config.rules.iter().map(|rule| RuleRow::Rule { place: None, rule }));
        }
        for (name, project) in &config.projects {
            rows.push(RuleRow::Heading { title: name.clone(), detail: project.paths.join(", ") });
            rows.extend(project.rules.iter().map(|rule| RuleRow::Rule { place: Some(name.as_str()), rule }));
        }
        rows
    }

    pub fn selected_rule_row(&self) -> Option<usize> {
        let rows = self.rule_rows();
        let is = |row: &RuleRow, id: Option<&String>| match row {
            RuleRow::Rule { rule, .. } => id.is_none_or(|id| &rule.id == id),
            RuleRow::Heading { .. } => false,
        };
        rows.iter().position(|r| is(r, self.rule_sel.as_ref())).or_else(|| rows.iter().position(|r| is(r, None)))
    }

    pub fn selected_rule(&self) -> Option<(Option<&str>, &Rule)> {
        let row = self.selected_rule_row()?;
        match self.rule_rows().into_iter().nth(row)? {
            RuleRow::Rule { place, rule } => Some((place, rule)),
            RuleRow::Heading { .. } => None,
        }
    }

    /// The rule as it is in the file now, by id.
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.config.all_rules().find(|(_, r)| r.id == id).map(|(_, r)| r)
    }

    pub fn issues_for(&self, id: &str) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(move |i| i.rule.as_deref() == Some(id))
    }
}

pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn create_example(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(EXAMPLE_RULES.as_bytes())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEventState;

    pub fn fixture() -> App {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut app =
            App::new(root.join("examples/rules.yaml"), root.join("crates/reaper-core/tests/fixtures/events.jsonl"));
        app.tz = TimeZone::UTC;
        app.now = 1_793_385_526_497 + 3 * 60 * 1000;
        app.today = 1_793_318_400_000;
        app.colors = Colors::True;
        app.freeze(app.now);
        app
    }

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: ratatui::crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn kill_log_follows_the_newest_until_you_move() {
        let mut app = fixture();
        assert_eq!(app.selected_kill().map(|k| k.is_final), Some(true), "newest kill: the agent that died");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected_kill().map(|k| k.is_final), Some(false));
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.kill_sel, None, "back on top: following again");
    }

    #[test]
    fn the_dead_and_the_reaped_sink_to_the_bottom_of_the_agent_list() {
        let mut app = fixture();
        let died = app.model.agents.iter().find(|a| a.status == Status::Dead).unwrap().last_at;
        app.now = died + 10_000;
        let statuses: Vec<Status> = app.agents().iter().map(|a| a.status).collect();
        assert_eq!(statuses, [Status::Idle, Status::Reaped, Status::Dead]);
        app.now = died + DEAD_FOR;
        let statuses: Vec<Status> = app.agents().iter().map(|a| a.status).collect();
        assert_eq!(statuses, [Status::Idle, Status::Reaped], "a minute later, the dead have gone; the reaped stay");
    }

    #[test]
    fn edit_opens_at_the_rule_behind_the_selection() {
        let mut app = fixture();
        app.on_key(key(KeyCode::Char('2')));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        let (_, rule) = app.selected_rule().unwrap();
        assert_eq!(rule.id, "prod-db");
        let line = app.rules.line_of("prod-db");
        assert!(line.is_some());
        assert_eq!(app.on_key(key(KeyCode::Char('e'))), Command::Edit { line });
    }

    #[test]
    fn rule_selection_skips_headings_and_stops_at_the_ends() {
        let mut app = fixture();
        app.tab = Tab::Rules;
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.selected_rule().unwrap().1.id, "bridge-schema");
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.selected_rule().unwrap().1.id, "bridge-schema");
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.selected_rule().unwrap().1.id, "force-push");
    }

    #[test]
    fn edit_goes_to_a_syntax_error_first_and_a_missing_file_is_created() {
        let dir = std::env::temp_dir().join(format!("reaper-console-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let rules = dir.join("config/rules.yaml");
        let mut app = App::new(rules.clone(), dir.join("events.jsonl"));
        assert!(!app.rules.exists);

        app.tab = Tab::Rules;
        assert!(matches!(app.on_key(key(KeyCode::Char('e'))), Command::Edit { .. }));
        assert_eq!(fs::read_to_string(&rules).unwrap(), EXAMPLE_RULES, "created from the example");

        fs::write(&rules, "rules:\n  - id: a\n    match: { command: [oops\n").unwrap();
        app.tick();
        let Some(line) = app.rules.error.as_ref().and_then(|e| e.line) else { panic!("expected a syntax error") };
        assert_eq!(app.on_key(key(KeyCode::Char('e'))), Command::Edit { line: Some(line as usize) });
    }
}
