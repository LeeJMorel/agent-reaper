//! Console state and key handling. Drawing lives in `ui.rs` and reads from here.

use std::cell::Cell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use reaper_core::draft;
use reaper_core::lint::{Issue, Level, lint};
use reaper_core::mori::Mori;
use reaper_core::paths::tilde;
use reaper_core::rules::{Rule, RulesFile};
use reaper_core::state::{Agent, Kill, Model, QUIET_FOR, Status};
use reaper_core::tail::{Tail, Update};

use super::mori::Colors;
use super::watcher::{self, Choice};
use super::writer::{Proposal, Writer};

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
/// A dead agent leaves the agent list this long after it died, as its session leaves memento
/// mori; a closed session's agents, this long after it closed.
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
    /// Memento mori shows the followed session's conversation over the level: everything it was
    /// asked, and what its agents said and did.
    pub chat_open: bool,
    /// The first of its rows shown, scrolled back; `None` at the latest, following it.
    pub chat_top: Option<usize>,
    /// The first row shown at the latest, as last drawn: 0 when it all fits.
    pub chat_back: Cell<usize>,
    /// The agents reaper is installed in, by name.
    pub guarded: Vec<&'static str>,
    /// The agent that writes rules from the human's own words, if there's one here.
    pub writer: Option<Arc<Writer>>,
    /// The new rule the human is describing, as they type.
    pub asking: Option<String>,
    /// A rule being written: what was asked, and where the answer arrives.
    writing: Option<(String, Receiver<Result<Proposal, String>>)>,
    /// A written rule, tried, waiting for the human to add it or drop it.
    pub proposal: Option<Proposal>,
    /// Choosing the watcher: what this machine has, and the one selected.
    pub watcher_pick: Option<(Vec<Choice>, usize)>,
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
            chat_open: false,
            chat_top: None,
            chat_back: Cell::new(0),
            guarded: Vec::new(),
            writer: None,
            asking: None,
            writing: None,
            proposal: None,
            watcher_pick: None,
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

        if let Some((_, answer)) = &self.writing {
            let written = match answer.try_recv() {
                Ok(written) => Some(written),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("it stopped without an answer".into())),
            };
            match written {
                None => {}
                Some(Ok(proposal)) => {
                    self.writing = None;
                    self.proposal = Some(proposal);
                    self.dirty = true;
                }
                Some(Err(e)) => {
                    self.writing = None;
                    let name = self.writer.as_ref().map_or("The agent", |w| w.name.as_str());
                    self.notify(format!("{name} couldn't write the rule: {e}"), true);
                }
            }
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
        if self.asking.is_some() {
            self.type_key(key.code);
            self.dirty = true;
            return Command::Nothing;
        }
        if self.watcher_pick.is_some() {
            self.pick_key(key.code);
            self.dirty = true;
            return Command::Nothing;
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
            KeyCode::Char('n') if self.tab == Tab::Rules => self.new_rule(),
            KeyCode::Char('w') if self.tab == Tab::Rules => self.open_watcher(watcher::choices),
            KeyCode::Enter if self.tab == Tab::Rules && self.proposal.is_some() => self.add_proposal(),
            KeyCode::Esc if self.tab == Tab::Rules && (self.proposal.is_some() || self.writing.is_some()) => {
                self.proposal = None;
                self.writing = None;
            }
            _ => return Command::Nothing,
        }
        self.dirty = true;
        Command::Nothing
    }

    /// Keys with their own meaning in memento mori. False for the rest, which work as anywhere.
    fn mori_key(&mut self, code: KeyCode) -> bool {
        let now = self.mori_now();
        let scrolls = self.chat_open && self.chat_back.get() > 0;
        match code {
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.chat_top = None;
                self.mori.switch(1, now);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.chat_top = None;
                self.mori.switch(-1, now);
            }
            // The chat covers the level, so there's nobody to pick while it's open.
            KeyCode::Up | KeyCode::Char('k') if scrolls => self.scroll_chat(-1),
            KeyCode::Down | KeyCode::Char('j') if scrolls => self.scroll_chat(1),
            KeyCode::PageUp if scrolls => self.scroll_chat(-PAGE),
            KeyCode::PageDown if scrolls => self.scroll_chat(PAGE),
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
            KeyCode::Char('p') => {
                self.chat_open = !self.chat_open;
                self.chat_top = None;
            }
            // A death for fun, played by stand-ins on a level of their own.
            KeyCode::Char('d') => {
                self.mori.rehearse(now);
            }
            _ => return false,
        }
        true
    }

    /// Up is back to older rows; down to the latest follows it again.
    fn scroll_chat(&mut self, delta: isize) {
        let latest = self.chat_back.get();
        let top = self.chat_top.unwrap_or(latest).min(latest).saturating_add_signed(delta);
        self.chat_top = (top < latest).then_some(top);
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

    /// Describing it again starts from what was asked last.
    fn new_rule(&mut self) {
        if self.writer.is_none() {
            return self.notify("Writing a rule for you takes Claude Code or pi; press e to write one yourself.", true);
        }
        self.asking = Some(self.proposal.as_ref().map(|p| p.asked.clone()).unwrap_or_default());
    }

    fn type_key(&mut self, code: KeyCode) {
        let Some(text) = self.asking.as_mut() else { return };
        match code {
            KeyCode::Char(c) => text.push(c),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Esc => self.asking = None,
            KeyCode::Enter if !text.trim().is_empty() => {
                let asked = std::mem::take(text);
                self.asking = None;
                self.start_writing(asked);
            }
            _ => {}
        }
    }

    fn start_writing(&mut self, asked: String) {
        let Some(writer) = self.writer.clone() else { return };
        let (answer, arrives) = mpsc::channel();
        let (question, rules) = (asked.clone(), self.rules.text.clone());
        thread::spawn(move || {
            let _ = answer.send(writer.write(&question, &rules));
        });
        self.proposal = None;
        self.writing = Some((asked, arrives));
    }

    pub fn writing(&self) -> Option<&str> {
        self.writing.as_ref().map(|(asked, _)| asked.as_str())
    }

    fn add_proposal(&mut self) {
        let Some(proposal) = &self.proposal else { return };
        // Checked again on the file as it is now: it may have changed while the agent wrote.
        // What the watcher and the sessions said stands.
        let mut report = draft::check(&proposal.draft, &self.rules.text);
        report.tried.clone_from(&proposal.report.tried);
        report.seen.clone_from(&proposal.report.seen);
        let id = proposal.draft.rule.id.clone();
        let text = report.text.clone().filter(|_| report.ok());
        if let Some(proposal) = self.proposal.as_mut() {
            proposal.report = report;
        }
        let Some(text) = text else {
            return self.notify("This rule can't go in as it is: n describes it again, e opens the file.", true);
        };
        if let Err(e) = draft::save(&self.rules.path, &text) {
            return self.notify(format!("Couldn't write {}: {e}", tilde(&self.rules.path)), true);
        }
        self.proposal = None;
        self.rules.refresh();
        self.issues = lint(&self.rules.config);
        self.rule_sel = Some(id.clone());
        self.notify(format!("Added {id}. Agents pick it up on their next action."), false);
    }

    /// The watchers this machine has, the current one selected.
    fn open_watcher(&mut self, choices: fn(Option<&reaper_core::rules::Watcher>) -> Vec<Choice>) {
        let current = self.rules.config.watcher.as_ref();
        let choices = choices(current);
        let at = choices.iter().position(|c| c.is(current)).unwrap_or(0);
        self.watcher_pick = Some((choices, at));
    }

    fn pick_key(&mut self, code: KeyCode) {
        let Some((choices, at)) = self.watcher_pick.as_mut() else { return };
        match code {
            KeyCode::Up | KeyCode::Char('k') => *at = at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *at = (*at + 1).min(choices.len() - 1),
            KeyCode::Esc => self.watcher_pick = None,
            KeyCode::Enter => self.set_watcher(),
            _ => {}
        }
    }

    /// Writes the chosen watcher in. What happens when it can't answer (`onError`) stays as
    /// the human set it; how long it may take goes back to the default for its kind.
    fn set_watcher(&mut self) {
        let Some((choices, at)) = self.watcher_pick.take() else { return };
        let choice = &choices[at];
        let mut picked = choice.watcher.clone();
        if let (Some(w), Some(old)) = (picked.as_mut(), self.rules.config.watcher.as_ref()) {
            w.on_error.clone_from(&old.on_error);
        }
        let text = match draft::set_watcher(&self.rules.text, picked.as_ref()) {
            Ok(text) => text,
            Err(e) => return self.notify(format!("Couldn't set the watcher: {e}."), true),
        };
        if let Err(e) = draft::save(&self.rules.path, &text) {
            return self.notify(format!("Couldn't write {}: {e}", tilde(&self.rules.path)), true);
        }
        self.rules.refresh();
        self.issues = lint(&self.rules.config);
        let backup =
            picked.as_ref().and_then(|w| w.fallback.first()).and_then(|f| choices.iter().find(|c| c.is(Some(f))));
        let said = match (&picked, backup) {
            (None, _) => "Watcher off: judged rules don't run.".to_string(),
            (Some(_), Some(backup)) => format!("Watcher: {}, and {} when it can't answer.", choice.name, backup.name),
            (Some(_), None) => format!("Watcher: {}.", choice.name),
        };
        self.notify(said, false);
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

    /// The dead leave the list a minute after they died, as they leave memento mori, and the
    /// closed a minute after they closed; their kills stay in the kill log. The reaped stay:
    /// they can be resumed. Anyone quiet for half a day has most likely ended unheard, and goes
    /// too, until they do something.
    pub fn agents(&self) -> Vec<&Agent> {
        // The dead leave the list a while after, unless their work is waiting for the human.
        let gone = |a: &&Agent| {
            let quiet = self.now - a.last_at;
            let over = matches!(a.status, Status::Dead | Status::Ended) && a.waits_for.is_none();
            quiet >= QUIET_FOR || over && quiet >= DEAD_FOR
        };
        let mut agents: Vec<&Agent> = self.model.agents.iter().filter(|a| !gone(a)).collect();
        let sinks = |a: &Agent| matches!(a.status, Status::Dead | Status::Reaped | Status::Ended);
        agents.sort_by_key(|a| (sinks(a), std::cmp::Reverse(a.last_at)));
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
    fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(draft::starter().as_bytes())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEventState;
    use reaper_core::EXAMPLE_RULES;

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
    fn esc_lets_go_of_the_agent_then_of_the_chosen_session() {
        let mut app = fixture();
        app.on_key(key(KeyCode::Char('3')));
        let auto = app.mori.followed();
        app.on_key(key(KeyCode::Right));
        assert_ne!(app.mori.followed(), auto, "chosen");
        app.on_key(key(KeyCode::Up));
        assert!(app.mori.picked().is_some());
        app.on_key(key(KeyCode::Esc));
        assert!(app.mori.picked().is_none() && !app.mori.following_the_action(), "the agent first");
        app.on_key(key(KeyCode::Esc));
        assert!(app.mori.following_the_action());
        assert_eq!(app.mori.followed(), auto, "back to the busy one");
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
    fn a_closed_session_leaves_the_agent_list_a_minute_after_it_closed() {
        let mut app = fixture();
        let at = app.now;
        for (dt, agent, kind, fields) in [
            (0, "s", "agent.spawned", r#","dir":"/w""#),
            (1, "sub", "agent.spawned", r#","dir":"/w","parent":"s""#),
            (2, "s", "agent.ended", ""),
        ] {
            let line =
                format!(r#"{{"at":{},"host":"claude-code","agent":"{agent}","type":"{kind}"{fields}}}"#, at + dt);
            app.model.apply(&reaper_core::events::parse_line(&line).unwrap());
        }
        let ended = |app: &App| app.agents().iter().filter(|a| a.status == Status::Ended).count();
        assert_eq!(ended(&app), 2, "listed as ended for a while");
        app.now = at + 2 + DEAD_FOR;
        assert_eq!(ended(&app), 0, "then gone, subagent and all");
    }

    #[test]
    fn anyone_quiet_for_half_a_day_leaves_the_agent_list() {
        let mut app = fixture();
        let newest = app.model.agents.iter().map(|a| a.last_at).max().unwrap();
        app.now = newest + QUIET_FOR - 1;
        assert!(app.agents().iter().any(|a| a.status == Status::Reaped));
        app.now = newest + QUIET_FOR;
        assert!(app.agents().is_empty(), "everyone, the reaped too");
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
    fn a_rule_described_in_plain_words_is_written_tried_and_added() {
        const SNIPPET: &str = "rule:\n  id: no-sudo\n  tools: [bash]\n  match: { command: '^sudo\\b' }\n  redirect: Run it without elevated rights, or ask the person to run it.\nstops: [sudo make install]\nallows: [cat sudoers.md]\n";
        let dir = std::env::temp_dir().join(format!("reaper-console-new-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rules = dir.join("rules.yaml");
        fs::write(&rules, EXAMPLE_RULES).unwrap();
        let mut app = App::new(rules.clone(), dir.join("events.jsonl"));
        app.tab = Tab::Rules;
        app.on_key(key(KeyCode::Char('n')));
        assert!(app.asking.is_none() && app.notice.as_ref().is_some_and(|n| n.bad), "no agent to ask");

        let asked = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = asked.clone();
        let ask = move |prompt: &str| {
            *seen.lock().unwrap() = prompt.to_string();
            Ok(SNIPPET.to_string())
        };
        app.writer = Some(Arc::new(Writer { name: "test".into(), ask: Box::new(ask) }));
        app.on_key(key(KeyCode::Char('n')));
        for c in "no sudoq".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.asking.as_deref(), Some("no sudoq"), "q is typed, not quit");
        app.on_key(key(KeyCode::Backspace));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.writing(), Some("no sudo"));
        let started = Instant::now();
        while app.proposal.is_none() && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(5));
            app.tick();
        }
        assert!(asked.lock().unwrap().contains("no sudo"));
        assert!(app.proposal.as_ref().is_some_and(|p| p.report.ok()));

        app.on_key(key(KeyCode::Enter));
        assert!(fs::read_to_string(&rules).unwrap().contains("  - id: no-sudo\n"));
        assert!(app.proposal.is_none());
        assert_eq!(app.selected_rule().map(|(_, r)| r.id.as_str()), Some("no-sudo"));
    }

    #[test]
    fn the_watcher_is_chosen_from_what_this_machine_has() {
        fn here(_: Option<&reaper_core::rules::Watcher>) -> Vec<Choice> {
            let mut claude = reaper_core::rules::Watcher::new(Some("claude-code"), None, "claude-haiku-5-5");
            let ollama = reaper_core::rules::Watcher::new(None, Some("http://localhost:11434/v1"), "qwen3:4b");
            claude.fallback.push(ollama.clone());
            let choice = |name: &str, watcher| Choice { name: name.into(), about: String::new(), watcher };
            vec![choice("Claude Code", Some(claude)), choice("Ollama", Some(ollama)), choice("None", None)]
        }
        let dir = std::env::temp_dir().join(format!("reaper-console-watcher-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rules = dir.join("rules.yaml");
        fs::write(&rules, EXAMPLE_RULES).unwrap();
        let mut app = App::new(rules.clone(), dir.join("events.jsonl"));
        app.tab = Tab::Rules;
        app.open_watcher(here);
        assert_eq!(app.watcher_pick.as_ref().map(|(_, at)| *at), Some(1), "the example's Ollama watcher");
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.watcher_pick.is_some(), "q doesn't quit from here");
        app.on_key(key(KeyCode::Enter));

        let watcher = app.rules.config.watcher.as_ref().unwrap();
        assert_eq!((watcher.kind.as_deref(), watcher.fallback.len()), (Some("claude-code"), 1));
        assert_eq!(watcher.on_error.as_deref(), Some("allow"), "kept from before");
        assert_eq!(app.rules.config.rules.len(), reaper_core::rules::parse(EXAMPLE_RULES).unwrap().rules.len());
        assert!(
            app.notice.as_ref().is_some_and(|n| n.text == "Watcher: Claude Code, and Ollama when it can't answer.")
        );

        app.open_watcher(here);
        app.on_key(key(KeyCode::End));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(app.rules.config.watcher.is_none(), "turned off");
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
        assert_eq!(fs::read_to_string(&rules).unwrap(), draft::starter(), "created from the example");
        assert!(app.rules.config.projects.is_empty(), "its sample project is only an illustration");

        fs::write(&rules, "rules:\n  - id: a\n    match: { command: [oops\n").unwrap();
        app.tick();
        let Some(line) = app.rules.error.as_ref().and_then(|e| e.line) else { panic!("expected a syntax error") };
        assert_eq!(app.on_key(key(KeyCode::Char('e'))), Command::Edit { line: Some(line as usize) });
    }
}
