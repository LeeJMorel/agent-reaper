//! Drawing: pure functions of `App`.

use std::path::Path;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap};
use reaper_core::events::{DeathReason, Event, Kind, ReapReason, Verdict};
use reaper_core::lint::Level;
use reaper_core::mori::about;
use reaper_core::paths::tilde;
use reaper_core::rules::Rule;
use reaper_core::state::{Agent, Kill, Outcome, Status, WatcherHealth};

use super::app::{App, Pane, RuleRow, Tab, plural};
use super::mori;
use super::watcher::Choice;
use super::writer::Proposal;

const ACCENT: Color = Color::Red;
const LABEL: usize = 10;

fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

fn dim() -> Style {
    fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return format!("{text:<width$}");
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Session ids share their leading (time-based) characters.
fn short(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    chars[chars.len().saturating_sub(6)..].iter().collect()
}

fn clock(at: i64, tz: &TimeZone) -> String {
    Timestamp::from_millisecond(at)
        .map(|t| t.to_zoned(tz.clone()).strftime("%H:%M:%S").to_string())
        .unwrap_or_else(|_| "--:--:--".into())
}

fn ago(now: i64, at: i64) -> String {
    let s = (now - at).max(0) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn field(name: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(fit(name, LABEL), dim())];
    spans.extend(value);
    Line::from(spans)
}

/// Text as rows at most `room` wide. Words longer than a row (JSON, paths) are split.
pub(super) fn rows(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut rows = vec![String::new()];
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        loop {
            let row = rows.last_mut().expect("rows starts non-empty");
            let used = row.chars().count();
            let gap = usize::from(used > 0);
            if used + gap + word.len() <= room {
                row.push_str(&" ".repeat(gap));
                row.extend(word);
                break;
            }
            if used > 0 {
                rows.push(String::new());
                continue;
            }
            let rest = word.split_off(room);
            row.extend(word);
            rows.push(String::new());
            word = rest;
        }
    }
    rows.retain(|r| !r.is_empty());
    rows
}

fn wrapped(name: &str, text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let mut rows = rows(text, width.saturating_sub(LABEL).max(8));
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| field(if i == 0 { name } else { "" }, vec![Span::styled(row, style)]))
        .collect()
}

fn pane(title: String, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused { fg(Color::Cyan) } else { dim() })
        .title(Span::styled(format!(" {title} "), if focused { bold() } else { Style::new() }))
}

/// The marker column is always reserved, so rows don't shift as focus moves.
fn list<'a>(items: Vec<ListItem<'a>>, block: Block<'a>) -> List<'a> {
    List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
}

fn status_look(status: Status) -> (&'static str, &'static str, Style) {
    match status {
        Status::Working => ("●", "working", fg(Color::Green)),
        Status::Asking => ("?", "asking", fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Status::Idle => ("○", "idle", dim()),
        Status::Killed => ("◆", "killed", fg(Color::Yellow)),
        Status::Dead => ("✕", "dead", fg(ACCENT)),
        Status::Reaped => ("◌", "reaped", dim()),
        Status::Ended => ("·", "ended", dim()),
    }
}

fn outcome_look(outcome: &Outcome) -> Span<'static> {
    match outcome {
        Outcome::Pending => Span::styled(fit("…", 12), dim()),
        Outcome::Redirected(_) => Span::styled(fit("↻ redirected", 12), fg(Color::Yellow)),
        Outcome::Died(_) => Span::styled(fit("✕ died", 12), fg(ACCENT)),
    }
}

fn level_mark(level: Option<Level>) -> Span<'static> {
    match level {
        Some(Level::Error) => Span::styled("✖ ", fg(ACCENT)),
        Some(Level::Warn) => Span::styled("⚠ ", fg(Color::Yellow)),
        None => Span::raw("  "),
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let [top, body, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    header(frame, top, app);
    match app.tab {
        Tab::Live => live(frame, body, app),
        Tab::Rules => rules(frame, body, app),
        Tab::Mori => mori::draw(frame, body, app),
    }
    footer(frame, bottom, app);
}

// ── header / footer ─────────────────────────────────────────────────────────

/// On a narrow terminal the summary's counts go first, then, to make room for anything that needs
/// the human, the other tabs come down to their numbers and the name goes.
fn header(frame: &mut Frame, area: Rect, app: &App) {
    let tabs = |named: bool, brand: bool| {
        let tab = |key: &str, name: &str, on: bool| {
            let style = if on { bold().add_modifier(Modifier::REVERSED) } else { dim() };
            Span::styled(if named || on { format!(" {key} {name} ") } else { format!(" {key} ") }, style)
        };
        let mut spans = Vec::new();
        if brand {
            spans.push(Span::styled(" reaper ", Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)));
            spans.push(Span::raw(" "));
        }
        spans.extend([
            tab("1", "live", app.tab == Tab::Live),
            Span::raw(" "),
            tab("2", "rules", app.tab == Tab::Rules),
            Span::raw(" "),
            tab("3", "memento mori", app.tab == Tab::Mori),
        ]);
        Line::from(spans)
    };
    let width = usize::from(area.width);
    let room = |left: &Line| width.saturating_sub(left.width() + 1);
    let with_summary = |left: Line<'static>| {
        let right = summary(app, room(&left));
        (left, right)
    };
    let (left, right) = [tabs(true, true), tabs(false, true)]
        .into_iter()
        .map(with_summary)
        .find(|(left, right)| right.width() <= room(left))
        .unwrap_or_else(|| with_summary(tabs(false, false)));
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right.width() as u16)]).areas(area);
    frame.render_widget(left, l);
    frame.render_widget(right, r);
}

/// What reaper is doing about it: anyone waiting on the human first, then today's kills,
/// deaths over all time (they only go up), and the watcher. Short of room, the counts go first;
/// what needs the human never does.
fn summary(app: &App, room: usize) -> Line<'static> {
    const ALERT: u8 = 9;
    let today = app.model.kills.iter().filter(|k| k.at >= app.today).count();
    // Asking for a yes or an answer, or stopped with its work waiting for them.
    let asking = app.agents().iter().filter(|a| a.status == Status::Asking || a.waits_for.is_some()).count();
    // Each with how readily it goes when there isn't room, lowest first.
    let mut parts: Vec<(u8, Span<'static>)> = Vec::new();
    if asking > 0 {
        let need = if asking == 1 { "needs" } else { "need" };
        let help = format!("{} {need} help", plural(asking, "agent"));
        parts.push((ALERT, Span::styled(help, fg(Color::Cyan).add_modifier(Modifier::BOLD))));
    }
    parts.push((0, Span::raw(format!("{} today", plural(today, "kill")))));
    parts.push((1, Span::raw(format!("{} dead", app.model.deaths))));
    parts.push(match &app.model.watcher {
        WatcherHealth::Unknown => (2, Span::styled("watcher –", dim())),
        WatcherHealth::Ok { .. } => (2, Span::styled("watcher ok", fg(Color::Green))),
        WatcherHealth::Failing { at, .. } => (
            ALERT,
            Span::styled(
                format!("watcher failing ({} ago)", ago(app.now, *at)),
                fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        ),
    });
    if app.rules.error.is_some() {
        parts.push((ALERT, Span::styled("rules: syntax error", fg(ACCENT).add_modifier(Modifier::BOLD))));
    }
    let width = |parts: &[(u8, Span)]| {
        parts.iter().map(|(_, s)| s.width()).sum::<usize>() + 3 * parts.len().saturating_sub(1) + 1
    };
    while width(&parts) > room
        && let Some(least) = (0..parts.len()).filter(|&i| parts[i].0 < ALERT).min_by_key(|&i| parts[i].0)
    {
        parts.remove(least);
    }

    let mut spans = Vec::new();
    for (i, (_, part)) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(part);
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(text) = &app.asking {
        let (label, hint) = (" Describe the rule: ", "   enter ask · esc cancel");
        // The end of a long description, where the typing is.
        let room = usize::from(area.width).saturating_sub(label.len() + hint.len() + 1).max(8);
        let shown: String = text.chars().skip(text.chars().count().saturating_sub(room)).collect();
        Line::from(vec![
            Span::styled(label, bold()),
            Span::raw(shown),
            Span::styled("█", fg(ACCENT)),
            Span::styled(hint, dim()),
        ])
    } else if let Some(notice) = &app.notice {
        Line::styled(format!(" {}", notice.text), if notice.bad { fg(ACCENT) } else { fg(Color::Green) })
    } else if let Some(error) = &app.events_error {
        Line::styled(format!(" Can't read {}: {error}", tilde(app.events_path())), fg(ACCENT))
    } else {
        let keys = match app.tab {
            Tab::Live => {
                vec![("1 2 3", "tabs"), ("tab", "switch pane"), ("↑↓", "select"), ("e", "edit rule"), ("q", "quit")]
            }
            Tab::Rules if app.watcher_pick.is_some() => vec![("↑↓", "choose"), ("enter", "use it"), ("esc", "cancel")],
            Tab::Rules if app.proposal.is_some() => {
                vec![("enter", "add this rule"), ("n", "describe again"), ("esc", "drop it"), ("q", "quit")]
            }
            Tab::Rules if app.writing().is_some() => vec![("esc", "stop waiting"), ("1 2 3", "tabs"), ("q", "quit")],
            Tab::Rules => {
                vec![
                    ("1 2 3", "tabs"),
                    ("↑↓", "select"),
                    ("n", "new rule"),
                    ("w", "watcher"),
                    ("e", "edit in your editor"),
                    ("q", "quit"),
                ]
            }
            Tab::Mori => mori_keys(app),
        };
        // A hint that doesn't fit is left out whole, not cut off.
        let (mut spans, mut used) = (vec![Span::raw(" ")], 1);
        for (key, what) in keys {
            let len = key.chars().count() + 1 + what.chars().count();
            if used + len > usize::from(area.width) {
                continue;
            }
            used += len + 2;
            spans.push(Span::styled(key, bold()));
            spans.push(Span::styled(format!(" {what}  "), dim()));
        }
        Line::from(spans)
    };
    frame.render_widget(line, area);
}

/// Only the keys that do something just now, most useful first, so a narrow terminal loses
/// the least useful.
fn mori_keys(app: &App) -> Vec<(&'static str, &'static str)> {
    let scrolls = app.chat_open && app.chat_back.get() > 0;
    let mut keys = vec![("←→", "switch session"), ("↑↓", if scrolls { "scroll chat" } else { "check on agent" })];
    if app.mori.picked().is_some() {
        keys.extend([("enter", "details"), ("esc", "deselect")]);
    } else if !app.mori.following_the_action() {
        // Back to showing whichever session is busy.
        keys.push(("esc", "auto-switch"));
    }
    if !app.mori.chat(app.mori_now()).is_empty() {
        keys.push(("p", if app.chat_open { "hide chat" } else { "show chat" }));
    }
    keys.extend([("q", "quit"), ("d", "demo a death")]);
    keys
}

/// Two panes side by side, the first `first` percent of the room, or one over the other on a
/// narrow terminal that's taller than it is wide.
fn split(area: Rect, first: u16) -> [Rect; 2] {
    let shares = [Constraint::Percentage(first), Constraint::Percentage(100 - first)];
    if area.width < 80 && area.width < 2 * area.height {
        Layout::vertical(shares).areas(area)
    } else {
        Layout::horizontal(shares).areas(area)
    }
}

// ── live tab ────────────────────────────────────────────────────────────────

fn live(frame: &mut Frame, area: Rect, app: &App) {
    let [lists, detail] = Layout::vertical([Constraint::Min(6), Constraint::Length(10)]).areas(area);
    let [left, right] = split(lists, 40);
    agents(frame, left, app);
    kills(frame, right, app);

    let (rows, width) = (detail.height.saturating_sub(2) as usize, detail.width.saturating_sub(2) as usize);
    let text = match app.pane {
        Pane::Agents => agent_detail(app, rows, width),
        Pane::Kills => kill_detail(app, width),
    };
    frame.render_widget(Paragraph::new(text).block(pane("Detail".into(), false)).wrap(Wrap { trim: false }), detail);
}

fn agents(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.pane == Pane::Agents;
    let list = app.agents();
    let block = pane(format!("Agents {}", list.len()), focused);
    if list.is_empty() {
        let mut lines = vec![Line::styled("No agents yet.", bold()), Line::raw("")];
        if app.guarded.is_empty() {
            lines.push(Line::raw("Start a guarded pi session in a project:"));
            lines.push(Line::styled("  reaper run", fg(Color::Cyan)));
        } else {
            lines.push(Line::raw(format!(
                "Start {} the way you usually do. It shows up here.",
                app.guarded.join(" or ")
            )));
        }
        lines.extend([Line::raw(""), Line::styled(format!("Watching {}", tilde(app.events_path())), dim())]);
        let text = Text::from(lines);
        frame.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), area);
        return;
    }
    let items: Vec<ListItem> = list.iter().map(|a| ListItem::new(agent_line(a, app.now))).collect();
    let mut state = ListState::default().with_selected(if focused { app.selected_agent_row() } else { None });
    frame.render_stateful_widget(self::list(items, block), area, &mut state);
}

fn agent_line(a: &Agent, now: i64) -> Line<'static> {
    let (glyph, word, style) = status_look(a.status);
    let place = a.project.clone().or_else(|| a.dir.as_deref().map(|d| tilde(Path::new(d)))).unwrap_or_default();
    let doing = match a.status {
        Status::Working | Status::Asking => a.last_tool.clone().unwrap_or_default(),
        Status::Killed | Status::Dead => format!("strike {}", a.strike),
        Status::Idle | Status::Reaped | Status::Ended => String::new(),
    };
    Line::from(vec![
        Span::styled(format!("{glyph} "), style),
        Span::styled(short(&a.id), bold()),
        Span::raw(" "),
        Span::styled(fit(word, 8), style),
        Span::styled(format!("{:>4}  ", ago(now, a.last_at)), dim()),
        Span::raw(format!("{place} ")),
        Span::styled(doing, dim()),
    ])
}

fn kills(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.pane == Pane::Kills;
    let block = pane(format!("Kill log {}", app.model.kills.len()), focused);
    if app.model.kills.is_empty() {
        let text = Text::from(vec![
            Line::styled("No kills yet.", bold()),
            Line::raw(""),
            Line::raw("Every action reaper blocks shows up here, with what"),
            Line::raw("the agent tried and what it was told to do instead."),
        ]);
        frame.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), area);
        return;
    }
    let items: Vec<ListItem> = app.kills().map(|(_, k)| ListItem::new(kill_line(k, &app.tz))).collect();
    let mut state = ListState::default().with_selected(if focused { app.selected_kill_row() } else { None });
    frame.render_stateful_widget(list(items, block), area, &mut state);
}

fn kill_line(k: &Kill, tz: &TimeZone) -> Line<'static> {
    Line::from(vec![
        Span::styled(clock(k.at, tz), dim()),
        Span::raw("  "),
        Span::styled(short(&k.agent), bold()),
        Span::raw("  "),
        Span::styled(fit(&k.rule, 18), fg(ACCENT)),
        Span::raw(" "),
        outcome_look(&k.outcome),
        Span::styled(format!(" {} #{}", k.tier.as_str(), k.strike), dim()),
    ])
}

fn kill_detail(app: &App, width: usize) -> Text<'static> {
    let Some(k) = app.selected_kill() else {
        return Text::styled("Kills show up here: what the agent tried, and what it was told instead.", dim());
    };
    let agent = app.model.agent(&k.agent);
    let project = agent.and_then(|a| a.project.clone()).unwrap_or_default();
    let dir = agent.and_then(|a| a.dir.as_deref()).map(|d| tilde(Path::new(d))).unwrap_or_default();

    let (told, told_style) = match &k.outcome {
        Outcome::Redirected(text) if text.is_empty() => ("(redirected; not recorded)".to_string(), dim()),
        Outcome::Redirected(text) => (format!("“{text}”"), fg(Color::Yellow)),
        Outcome::Died(DeathReason::Strikes) => ("nothing: stopped at the strike limit".to_string(), fg(ACCENT)),
        Outcome::Died(DeathReason::Stop) => ("nothing: this rule stops the session".to_string(), fg(ACCENT)),
        Outcome::Died(DeathReason::Together) => {
            ("nothing: its session broke rules too often, and was stopped".to_string(), fg(ACCENT))
        }
        Outcome::Pending => ("not yet: the redirect goes out when this turn ends".to_string(), dim()),
    };
    let tried = if k.attempt.is_empty() { k.tool.clone() } else { format!("{} {}", k.tool, k.attempt) };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(k.rule.clone(), fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(
                    "  {} · strike {} · {} ({} ago)",
                    k.tier.as_str(),
                    k.strike,
                    clock(k.at, &app.tz),
                    ago(app.now, k.at)
                ),
                dim(),
            ),
        ]),
        field(
            "agent",
            vec![Span::styled(short(&k.agent), bold()), Span::raw(format!("  {project}  ")), Span::styled(dir, dim())],
        ),
    ];
    lines.extend(wrapped("tried", &tried, Style::new(), width));
    lines.extend(wrapped("told", &told, told_style, width));
    match app.rule(&k.rule) {
        Some(rule) => lines.extend(rule_terms(rule, width)),
        None => lines.push(field("rule", vec![Span::styled("no longer in rules.yaml", dim())])),
    }
    Text::from(lines)
}

fn rule_terms(rule: &Rule, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = rule
        .pattern
        .iter()
        .flatten()
        .flat_map(|(key, re)| wrapped("match", &format!("{key} /{re}/"), fg(Color::Magenta), width))
        .collect();
    if let Some(judge) = &rule.judge {
        lines.extend(wrapped("judge", judge, Style::new(), width));
    }
    let mut short = Vec::new();
    if let Some(free) = &rule.free {
        short.push(format!("free memory is below {free}"));
    }
    if let Some(load) = rule.load {
        short.push(format!("the load is above {load}"));
    }
    if !short.is_empty() {
        lines.extend(wrapped("while", &short.join(" or "), Style::new(), width));
    }
    if rule.ask {
        lines.extend(wrapped(
            "asking",
            "the agent may ask you to allow one call, once until your next prompt",
            Style::new(),
            width,
        ));
    }
    lines
}

fn agent_detail(app: &App, rows: usize, width: usize) -> Text<'static> {
    let Some(a) = app.selected_agent() else {
        return Text::styled("Agents show up here once pi runs with the reaper extension.", dim());
    };
    let (glyph, word, style) = status_look(a.status);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} {word}  "), style),
            Span::styled(a.id.clone(), bold()),
            Span::styled(format!("  {} · model {}", a.host, a.model.as_deref().unwrap_or("?")), dim()),
        ]),
        field(
            "project",
            vec![
                Span::raw(a.project.clone().unwrap_or_else(|| "(no project rules)".into())),
                Span::styled(format!("  {}", a.dir.as_deref().map(|d| tilde(Path::new(d))).unwrap_or_default()), dim()),
            ],
        ),
        field(
            "activity",
            vec![Span::raw(format!(
                "{} · {} · started {} · last active {} ago",
                plural(a.actions as usize, "action"),
                plural(a.kills as usize, "kill"),
                clock(a.first_at, &app.tz),
                ago(app.now, a.last_at)
            ))],
        ),
    ];
    // What it was asked last. A subagent works on its session's prompt, and a session carrying on
    // a dead one's work on that one's until it's asked something itself. One line: some prompts
    // are pages long.
    fn latest(a: &Agent) -> Option<&str> {
        a.prompts.last().map(|(_, prompt)| prompt.as_str())
    }
    let prompt = latest(a).or_else(|| latest(app.model.agent(a.parent.as_deref().or(a.after.as_deref())?)?));
    if let Some(prompt) = prompt {
        let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.insert(2, field("prompt", vec![Span::raw(fit(&flat, width.saturating_sub(LABEL)))]));
    }
    if let Some(command) = &a.waits_for {
        let next = format!("stopped, its work waiting for you: {command} in its session carries it on");
        lines.insert(2, field("next", vec![Span::styled(fit(&next, width.saturating_sub(LABEL)), fg(Color::Cyan))]));
    }
    let room = rows.saturating_sub(lines.len());
    let recent: Vec<&Event> = a.recent.iter().rev().take(room).collect();
    for (i, event) in recent.into_iter().rev().enumerate() {
        let mut spans = vec![Span::styled(format!("{}  ", clock(event.at, &app.tz)), dim())];
        let room = width.saturating_sub(LABEL + spans[0].width());
        spans.push(describe(event, room));
        lines.push(field(if i == 0 { "recent" } else { "" }, spans));
    }
    Text::from(lines)
}

/// One line, `room` wide at most: a command or a reply can run to pages.
fn describe(event: &Event, room: usize) -> Span<'static> {
    let flat = |text: &str| fit(&text.split_whitespace().collect::<Vec<_>>().join(" "), room);
    match &event.kind {
        Kind::Spawned { .. } => Span::raw("started"),
        Kind::Prompted { .. } => Span::raw("asked for something"),
        Kind::Acted { tool, args } => Span::raw(flat(&format!("ran {tool} {}", about(args).unwrap_or_default()))),
        Kind::Said { text } => Span::styled(flat(&format!("said: {text}")), dim()),
        Kind::Checked { tool, error: Some(error), .. } => {
            Span::styled(format!("check of {tool} failed: {error}"), fg(ACCENT))
        }
        Kind::Checked { tool, tier, ms, verdict, .. } => {
            let verdict = match verdict {
                Verdict::Pass => "pass",
                Verdict::Violation => "violation",
            };
            Span::styled(format!("checked {tool}: {verdict} ({}, {ms:.0} ms)", tier.as_str()), dim())
        }
        Kind::Killed { tool, rule, strike, .. } => {
            Span::styled(format!("killed on {tool}: {rule} (strike {strike})"), fg(ACCENT))
        }
        Kind::Redirected { .. } => Span::styled("redirected", fg(Color::Yellow)),
        Kind::Died { rule, .. } => Span::styled(format!("died: {rule}"), fg(ACCENT).add_modifier(Modifier::BOLD)),
        Kind::Revived => Span::styled("started over with a clean record", fg(Color::Green)),
        Kind::Asked => Span::styled("asked you", fg(Color::Cyan)),
        Kind::Idle => Span::styled("finished its turn", dim()),
        Kind::Ended => Span::styled("session closed", dim()),
        Kind::Reaped { idle, reason } => {
            let idle = ago(*idle as i64, 0);
            let why = match reason {
                ReapReason::Crowded => format!("reaped to make room ({idle} idle)"),
                ReapReason::Memory => format!("reaped, memory was short ({idle} idle)"),
                ReapReason::Idle | ReapReason::Other => format!("reaped after {idle} idle"),
            };
            Span::styled(format!("{why}; resumable"), dim())
        }
        Kind::Unknown => Span::raw("?"),
    }
}

// ── rules tab ───────────────────────────────────────────────────────────────

fn rules(frame: &mut Frame, area: Rect, app: &App) {
    if !app.rules.exists && app.proposal.is_none() && app.writing().is_none() && app.watcher_pick.is_none() {
        let text = Text::from(vec![
            Line::styled(format!("No rules file at {}.", tilde(&app.rules.path)), bold()),
            Line::raw(""),
            Line::raw("Press n to describe a rule in your own words, and an agent you have writes it."),
            Line::raw("Or press e to create the file from the example and open it in your editor."),
            Line::raw("Agents pick up changes on their next action; nothing needs restarting."),
        ]);
        frame.render_widget(Paragraph::new(text).block(pane("Rules".into(), true)).wrap(Wrap { trim: false }), area);
        return;
    }

    let problems = problems(app);
    let main = if problems.is_empty() {
        area
    } else {
        let [banner, main] =
            Layout::vertical([Constraint::Length(problems.len() as u16 + 2), Constraint::Min(5)]).areas(area);
        let block = pane("Problems".into(), false).border_style(fg(ACCENT));
        frame.render_widget(Paragraph::new(problems).block(block).wrap(Wrap { trim: false }), banner);
        main
    };

    let [left, right] = split(main, 45);
    rule_list(frame, left, app);
    let width = right.width.saturating_sub(2) as usize;
    let (title, detail) = match (app.writing(), &app.proposal) {
        _ if app.watcher_pick.is_some() => ("Watcher", watcher_detail(app)),
        (Some(asked), _) => ("New rule", writing_detail(app, asked, width)),
        (None, Some(proposal)) => ("New rule", proposal_detail(app, proposal, width)),
        (None, None) => ("Rule", rule_detail(app, width)),
    };
    frame.render_widget(Paragraph::new(detail).block(pane(title.into(), false)).wrap(Wrap { trim: false }), right);
}

fn watcher_detail(app: &App) -> Text<'static> {
    let Some((choices, at)) = &app.watcher_pick else { return Text::default() };
    let now = app.rules.config.watcher.as_ref();
    let mut lines = vec![
        Line::styled("Choose the watcher", fg(ACCENT).add_modifier(Modifier::BOLD)),
        Line::styled("It judges the rules that have a judge, in every project. Pattern rules don't need it.", dim()),
        Line::raw(""),
    ];
    let backup = |choice: &Choice| {
        let first = choice.watcher.as_ref().and_then(|w| w.fallback.first());
        first.and_then(|f| choices.iter().find(|c| c.is(Some(f)))).map(|c| c.name.clone())
    };
    for (i, choice) in choices.iter().enumerate() {
        let here = i == *at;
        let mut name = vec![
            Span::styled(if here { "› " } else { "  " }, fg(ACCENT)),
            Span::styled(choice.name.clone(), if here { bold() } else { Style::new() }),
        ];
        if choice.is(now) {
            name.push(Span::styled("  (now)", dim()));
        }
        lines.push(Line::from(name));
        lines.push(Line::styled(format!("    {}", choice.about), dim()));
        if let Some(backup) = backup(choice) {
            lines.push(Line::styled(format!("    When it can't answer: {backup}."), dim()));
        }
    }
    if choices.len() == 1 {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Neither Claude Code nor Ollama is here. To use another watcher, press e and set it in the file.",
            dim(),
        ));
    }
    Text::from(lines)
}

fn writing_detail(app: &App, asked: &str, width: usize) -> Text<'static> {
    let name = app.writer.as_ref().map_or("the agent", |w| w.name.as_str());
    let mut lines = vec![Line::styled(format!("Asking {name} for a rule"), fg(ACCENT).add_modifier(Modifier::BOLD))];
    lines.extend(wrapped("asked", &format!("“{}”", asked.trim()), dim(), width));
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "It writes the rule with examples of what it should stop and let through, and reaper tries them. This can take a minute or two.",
        dim(),
    ));
    Text::from(lines)
}

fn proposal_detail(app: &App, proposal: &Proposal, width: usize) -> Text<'static> {
    let (draft, report) = (&proposal.draft, &proposal.report);
    let rule = &draft.rule;
    let mut lines = vec![Line::styled(rule.id.clone(), fg(ACCENT).add_modifier(Modifier::BOLD))];
    lines.extend(wrapped("asked", &format!("“{}”", proposal.asked.trim()), dim(), width));
    let applies = match &draft.project {
        None => "every project".to_string(),
        Some(name) => {
            let known = app.rules.config.projects.get(name).map(|p| &p.paths);
            format!("{name} ({})", known.unwrap_or(&draft.paths).join(", "))
        }
    };
    lines.extend(wrapped("applies", &applies, Style::new(), width));
    let tools = rule.scope().map_or("all tools".to_string(), |tools| tools.join(", "));
    lines.extend(wrapped("checked", &format!("{} · {tools}", checked(rule)), Style::new(), width));
    lines.extend(rule_terms(rule, width));
    lines.extend(match &rule.redirect {
        Some(text) => wrapped("redirect", &format!("“{}”", text.trim()), fg(Color::Yellow), width),
        None => wrapped("redirect", "none: the agent is only told to continue", dim(), width),
    });
    for (kind, stops) in [("stops", true), ("allows", false)] {
        for (n, tried) in report.tried.iter().filter(|t| t.should_stop == stops).enumerate() {
            let (mark, style, miss) = match (tried.passed(), stops) {
                (true, _) => ("✓ ", fg(Color::Green), ""),
                (false, true) => ("✗ ", fg(ACCENT), "  (let through)"),
                (false, false) => ("✗ ", fg(ACCENT), "  (stopped)"),
            };
            let example = vec![Span::styled(mark, style), Span::raw(format!("{}{miss}", tried.example))];
            lines.push(field(if n == 0 { kind } else { "" }, example));
        }
    }
    if let Some(seen) = &report.seen {
        lines.extend(wrapped("seen", &seen.summary(), Style::new(), width));
        for example in &seen.examples {
            lines.push(field("", vec![Span::styled(example.clone(), dim())]));
        }
    }
    if let Some(note) = &draft.note {
        lines.extend(wrapped("note", note, dim(), width));
    }
    let issues =
        report.problems.iter().map(|p| (Level::Error, p)).chain(report.warnings.iter().map(|w| (Level::Warn, w)));
    for (level, text) in issues {
        lines.push(Line::from(vec![level_mark(Some(level)), Span::raw(text.clone())]));
    }
    lines.push(Line::raw(""));
    lines.push(if report.ok() {
        Line::styled(format!("Enter adds it to {}. Agents only ever hear the redirect.", tilde(&app.rules.path)), dim())
    } else {
        Line::styled("It can't go in as it is: n to describe it again, or e to write it yourself.", fg(ACCENT))
    });
    Text::from(lines)
}

fn problems(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(e) = &app.rules.error {
        let at = match (e.line, e.column) {
            (Some(l), Some(c)) => format!(" at line {l}, column {c}"),
            (Some(l), None) => format!(" at line {l}"),
            _ => String::new(),
        };
        lines.push(Line::from(vec![
            level_mark(Some(Level::Error)),
            Span::raw(format!("Syntax error{at}: {}", e.message)),
        ]));
        lines.push(Line::styled(
            "  Running agents keep the previous rules; agents started now get none until this is fixed.",
            dim(),
        ));
    }
    for issue in app.issues.iter().filter(|i| i.rule.is_none()) {
        lines.push(Line::from(vec![level_mark(Some(issue.level)), Span::raw(issue.message.clone())]));
    }
    lines
}

fn rule_list(frame: &mut Frame, area: Rect, app: &App) {
    let config = &app.rules.config;
    let watcher = config.watcher.as_ref().map_or("none".to_string(), |w| {
        format!("{} {}", w.kind.as_deref().unwrap_or("openai"), w.model.as_deref().unwrap_or("?"))
    });
    let block = pane(format!("Rules · {}", tilde(&app.rules.path)), true)
        .title_bottom(Line::styled(format!(" strikes {} · watcher {watcher} ", config.strikes), dim()));

    let rows = app.rule_rows();
    if rows.is_empty() {
        let text = if app.rules.parsed {
            Text::from(vec![
                Line::raw("No rules yet."),
                Line::styled("Press n to describe one, or e to write it yourself.", dim()),
            ])
        } else {
            Text::from(vec![
                Line::raw("Nothing to show until the syntax error is fixed."),
                Line::styled("Press e to open the file at the error.", dim()),
            ])
        };
        frame.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), area);
        return;
    }
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            RuleRow::Heading { title, detail } => ListItem::new(Line::from(vec![
                Span::styled(title.clone(), bold()),
                Span::styled(format!("  {detail}"), dim()),
            ])),
            RuleRow::Rule { rule, .. } => {
                let worst = app.issues_for(&rule.id).map(|i| i.level).max();
                let scope = rule.scope().map_or("all tools".to_string(), |tools| tools.join(","));
                ListItem::new(Line::from(vec![
                    Span::raw(" "),
                    level_mark(worst),
                    Span::raw(fit(&rule.id, 16)),
                    Span::raw(" "),
                    Span::styled(fit(rule.checked_by(), 8), dim()),
                    Span::styled(scope, dim()),
                ]))
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(app.selected_rule_row());
    frame.render_stateful_widget(list(items, block), area, &mut state);
}

fn checked(rule: &Rule) -> &'static str {
    match rule.checked_by() {
        "pattern" => "pattern match, on every call",
        "watcher" => "the watcher model",
        "pattern+watcher" => "pattern first, then the watcher model",
        _ => "nothing (needs match or judge)",
    }
}

fn rule_detail(app: &App, width: usize) -> Text<'static> {
    let Some((place, rule)) = app.selected_rule() else {
        return Text::styled("Select a rule.", dim());
    };
    let config = &app.rules.config;
    let applies = match place {
        None => "every project".to_string(),
        Some(name) => {
            let paths = config.projects.get(name).map(|p| p.paths.join(", ")).unwrap_or_default();
            format!("{name} ({paths})")
        }
    };
    let checked = checked(rule);
    let tools = rule.scope().map_or("all tools".to_string(), |tools| tools.join(", "));
    let on_kill = if rule.stops() {
        "stop the agent".to_string()
    } else {
        format!("redirect; the agent stops at strike {}, sooner if it keeps breaking rules", rule.limit(config.strikes))
    };

    let mut lines = vec![Line::styled(rule.id.clone(), fg(ACCENT).add_modifier(Modifier::BOLD))];
    lines.extend(wrapped("applies", &applies, Style::new(), width));
    lines.extend(wrapped("checked", &format!("{checked} · {tools}"), Style::new(), width));
    lines.extend(rule_terms(rule, width));
    lines.extend(match &rule.redirect {
        Some(text) => wrapped("redirect", &format!("“{}”", text.trim()), fg(Color::Yellow), width),
        None => wrapped("redirect", "none: the agent is only told to continue", dim(), width),
    });
    lines.push(field("on kill", vec![Span::raw(on_kill)]));
    if let Some(line) = app.rules.line_of(&rule.id) {
        lines.push(field("line", vec![Span::raw(line.to_string()), Span::styled("  e to edit", dim())]));
    }
    for issue in app.issues_for(&rule.id) {
        lines.push(Line::from(vec![level_mark(Some(issue.level)), Span::raw(issue.message.clone())]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "The agent only ever sees the redirect. Match and judge stay with you and the watcher.",
        dim(),
    ));
    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::app::tests::{fixture, key};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use reaper_core::events::parse_line;

    pub fn render(app: &App, width: u16, height: u16) -> String {
        let Ok(mut terminal) = Terminal::new(TestBackend::new(width, height));
        let Ok(_) = terminal.draw(|f| draw(f, app));
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_new_rule_shows_how_its_examples_went_before_it_goes_in() {
        let mut app = fixture();
        app.tab = Tab::Rules;
        let snippet = "rule:\n  id: no-sudo\n  tools: [bash]\n  match: { command: '^sudo\\b' }\n  redirect: Run it without elevated rights.\nstops: [sudo make install]\nallows: [cat sudoers.md, echo x; sudo -v]\n";
        let draft = reaper_core::draft::read(snippet).unwrap();
        let report = reaper_core::draft::check(&draft, &app.rules.text);
        app.proposal = Some(Proposal { asked: "no sudo, ever".into(), draft, report });
        let screen = render(&app, 120, 30);
        assert!(screen.contains("stops     ✓ sudo make install"), "{screen}");
        assert!(screen.contains("✗ echo x; sudo -v  (stopped)"), "{screen}");
        assert!(screen.contains("It can't go in as it is"), "{screen}");
        assert_eq!(screen.lines().last(), Some(" enter add this rule  n describe again  esc drop it  q quit"));

        app.on_key(key(KeyCode::Char('n')));
        assert!(app.asking.is_none(), "the fixture has no agent to ask");
    }

    #[test]
    fn live_tab_shows_agents_kills_and_what_the_agent_was_told() {
        let app = fixture();
        let screen = render(&app, 110, 28);
        for expected in [
            "2 kills today · 1 dead · watcher ok",
            "◌ 000001 reaped",
            "78e845",
            "a82065",
            "↻ redirected",
            "✕ died",
            "nothing: stopped at the strike limit",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
        }
    }

    #[test]
    fn the_header_puts_anyone_waiting_on_the_human_first_and_never_drops_them() {
        let mut app = fixture();
        let header = |app: &App, width: u16| render(app, width, 28).lines().next().unwrap_or_default().to_string();
        assert!(!header(&app, 110).contains("help"), "nobody waiting, no mention: {}", header(&app, 110));
        let piet = "01a18e1b-7417-7499-8113-59ad5578e845";
        let asked = format!(r#"{{"at":1793385526500,"host":"pi","agent":"{piet}","type":"agent.asked"}}"#);
        app.model.apply(&parse_line(&asked).unwrap());
        assert!(
            header(&app, 110).ends_with("1 agent needs help · 2 kills today · 1 dead · watcher ok"),
            "{}",
            header(&app, 110)
        );
        // A session stopped with its work waiting for the human needs them too, and says how.
        let wren = "01a18e1b-74d4-7499-8113-59ae26a82065";
        let waits = format!(
            r#"{{"at":1793385526600,"host":"claude-code","agent":"{wren}","type":"agent.died","rule":"r","reason":"strikes","carry_on":"/compact"}}"#
        );
        app.model.apply(&parse_line(&waits).unwrap());
        assert!(header(&app, 110).contains("2 agents need help"), "{}", header(&app, 110));
        assert_eq!(app.model.agent(wren).unwrap().waits_for.as_deref(), Some("/compact"));
        // Short of room, the counts go and the call for help stays.
        let narrow = header(&app, 80);
        assert!(narrow.contains("2 agents need help") && !narrow.contains("kills today"), "{narrow}");
        // Narrower still, the tabs make room for it rather than run into it.
        let tight = header(&app, 44);
        assert!(tight.starts_with(" 1 live   2   3 ") && tight.ends_with("2 agents need help"), "{tight}");
        app.model = Default::default();
        let calm = header(&app, 50);
        assert_eq!(calm, " reaper   1 live   2 rules   3 memento mori", "the counts just go");
    }

    #[test]
    fn kill_detail_shows_the_attempt_and_the_redirect() {
        let mut app = fixture();
        app.on_key(key(KeyCode::Down));
        let screen = render(&app, 110, 28);
        assert!(screen.contains(r#"tried     bash {"command":"touch violation.txt"}"#), "{screen}");
        assert!(screen.contains("“Create the marker file allowed.txt in the project root.”"), "{screen}");
    }

    #[test]
    fn rules_tab_lists_rules_by_place_with_details() {
        let mut app = fixture();
        app.on_key(key(KeyCode::Char('2')));
        let screen = render(&app, 110, 28);
        for expected in
            ["global", "force-push", "my-app", "prod-db", "watcher openai qwen3:4b", "“Push to a new branch"]
        {
            assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
        }
    }

    #[test]
    fn the_watcher_choices_show_what_falls_back_and_which_is_in_use() {
        use reaper_core::rules::Watcher;
        let mut app = fixture();
        app.on_key(key(KeyCode::Char('2')));
        let mut claude = Watcher::new(Some("claude-code"), None, "claude-haiku-5-5");
        let ollama = Watcher::new(None, Some("http://localhost:11434/v1"), "qwen3:4b");
        claude.fallback.push(ollama.clone());
        let choice = |name: &str, watcher| Choice { name: name.into(), about: "About it.".into(), watcher };
        let choices =
            vec![choice("Claude Code · claude-haiku-5-5", Some(claude)), choice("Ollama · qwen3:4b", Some(ollama))];
        app.watcher_pick = Some((choices, 0));
        let screen = render(&app, 110, 28);
        for expected in [
            "› Claude Code · claude-haiku-5-5",
            "When it can't answer: Ollama · qwen3:4b.",
            "qwen3:4b  (now)",
            "enter use it",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
        }
    }

    #[test]
    fn empty_state_explains_how_to_start() {
        let mut app = fixture();
        app.model = Default::default();
        let screen = render(&app, 110, 28);
        assert!(screen.contains("No agents yet."), "{screen}");
        assert!(screen.contains("reaper run"), "{screen}");
        assert!(screen.contains("No kills yet."), "{screen}");
    }

    #[test]
    fn wrapped_values_continue_under_the_value_and_long_words_split() {
        let text = |lines: Vec<Line>| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(
            text(wrapped("told", "one two three four", Style::new(), LABEL + 9)),
            ["told      one two", "          three", "          four"]
        );
        assert_eq!(
            text(wrapped("tried", r#"{"command":"x"}"#, Style::new(), LABEL + 8)),
            ["tried     {\"comman", "          d\":\"x\"}"]
        );
        assert_eq!(text(wrapped("x", "", Style::new(), 40)), ["x         "]);
    }

    #[test]
    fn narrow_terminals_do_not_panic() {
        let mut app = fixture();
        for (w, h) in [(20, 5), (40, 12), (1, 1)] {
            render(&app, w, h);
            app.on_key(key(KeyCode::Char('2')));
            render(&app, w, h);
            app.on_key(key(KeyCode::Char('3')));
            render(&app, w, h);
            app.on_key(key(KeyCode::Char('1')));
        }
    }

    #[test]
    fn memento_mori_reads_at_80_by_24() {
        let mut app = fixture();
        app.tab = Tab::Mori;
        let screen = render(&app, 80, 24);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[0].contains("3 memento mori"), "{screen}");
        // The followed session is a highlighted number and the other a strip. The session where
        // everyone died, minutes ago, has been cleared away.
        assert!(rows[1].contains(" 1 ") && !rows[1].contains(" 2 ") && rows[1].contains(" 3 "), "{screen}");
        // Nothing explains the space: no labels, as in the game.
        assert!(!screen.contains("the stacks") && !screen.contains("the engine room"), "{screen}");
        assert!(rows[22].starts_with("████"), "land along the bottom: {screen}");
        assert!(rows[22].contains("▀▀▀"), "with a gap in it: {screen}");
        assert!(rows[23].contains("check on agent"), "{screen}");
        let Ok(mut terminal) = Terminal::new(TestBackend::new(80, 24));
        let Ok(_) = terminal.draw(|f| draw(f, &app));
        let buffer = terminal.backend().buffer();
        let stage = app.mori.scene(app.mori_now(), 80, 22).stage.unwrap();
        let reaper_core::mori::Rgb(r, g, b) = stage.actors[0].hue;
        assert!(buffer.content.iter().any(|c| c.fg == Color::Rgb(r, g, b)), "a character: {screen}");
        let sky = |y: u16| buffer[(40, y)].bg;
        assert!(sky(3) != Color::Reset && sky(3) != sky(18), "a dusk sky: {:?} {:?}", sky(3), sky(18));
    }

    #[test]
    fn picking_a_character_answers_and_enter_opens_it_in_the_live_tab() {
        let mut app = fixture();
        app.tab = Tab::Mori;
        let now = app.mori_now();
        app.on_key(key(KeyCode::Up));
        app.set_clock(now + 3000);
        let screen = render(&app, 80, 24);
        let piet = app.mori.name("01a18e1b-7417-7499-8113-59ad5578e845").unwrap();
        assert!(screen.contains(&format!("▾ {piet}")), "{screen}");
        // At home, its turn over: a line about being home, not about work.
        assert!(screen.contains(&format!("{piet} was home already.")), "{screen}");
        // At 80 columns, the demo death and quitting make room for what picking adds.
        assert_eq!(
            screen.lines().last(),
            Some(" ←→ switch session  ↑↓ check on agent  enter details  esc deselect  p show chat"),
            "{screen}"
        );

        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.tab, Tab::Live);
        assert_eq!(app.selected_agent().map(|a| a.id.as_str()), Some("01a18e1b-7417-7499-8113-59ad5578e845"));
    }

    #[test]
    fn a_sessions_latest_prompt_shows_on_one_line_and_p_opens_the_chat() {
        let mut app = fixture();
        let piet = "01a18e1b-7417-7499-8113-59ad5578e845";
        let prompted = |at: i64, prompt: &str| {
            let line = serde_json::json!({ "at": at, "host": "pi", "agent": piet, "type": "agent.prompted", "prompt": prompt });
            parse_line(&line.to_string()).unwrap()
        };
        let mut events: Vec<Event> =
            include_str!("../../../reaper-core/tests/fixtures/events.jsonl").lines().filter_map(parse_line).collect();
        let long = format!("Make the tests pass.\n\n\n{}", "Then tidy up. ".repeat(150));
        events.insert(1, prompted(1_793_385_526_350, "Fix the login."));
        events.insert(2, prompted(1_793_385_526_400, &long));
        // Nothing waits to play, so what's said next plays as soon as it's due.
        events.retain(|e| e.at <= app.now);
        app.model = reaper_core::state::Model::default();
        app.model.apply_all(&events);
        app.mori.clear();
        app.mori.push(events);
        app.set_clock(app.now);
        app.tab = Tab::Mori;
        let name = app.mori.name(piet).unwrap();

        let screen = render(&app, 80, 24);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[2].starts_with(" Make the tests pass. Then tidy up.") && rows[2].ends_with('…'), "{screen}");
        assert_eq!(rows[2].chars().count(), 79, "{screen}");
        assert_eq!(
            rows[23], " ←→ switch session  ↑↓ check on agent  p show chat  q quit  d demo a death",
            "only keys that do something: {screen}"
        );

        // Open, it starts at the latest: what the agent did, and said, after the human asked.
        app.on_key(key(KeyCode::Char('p')));
        let screen = render(&app, 80, 24);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[17].starts_with("       Then tidy up."), "{screen}");
        assert!(rows[18].starts_with(&format!(" {name:<6}✗ bash touch violation.txt")), "who's talking: {screen}");
        assert!(rows[19].starts_with("       bash touch allowed.txt"), "{screen}");
        assert!(
            rows[20].starts_with("       Created allowed.txt in the project root.") && rows[20].ends_with('┃'),
            "{screen}"
        );
        assert!(rows[21].is_empty(), "off the level: {screen}");
        assert!(rows[23].contains("↑↓ scroll chat") && rows[23].contains("p hide chat"), "{screen}");

        // Scrolled back to the start, new lines don't move it.
        app.on_key(key(KeyCode::PageUp));
        app.on_key(key(KeyCode::PageUp));
        let screen = render(&app, 80, 24);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[2].starts_with(" you   Fix the login.") && rows[2].ends_with('┃'), "{screen}");
        assert!(rows[3].starts_with(" ─────"), "a rule between turns: {screen}");
        assert!(
            rows[4].starts_with(" you   Make the tests pass.") && rows[4].ends_with('┃'),
            "its own lines: {screen}"
        );
        assert_eq!(rows[5].trim(), "┃", "one blank line between paragraphs: {screen}");
        assert!(rows[6].starts_with("       Then tidy up."), "{screen}");
        assert!(!rows[20].ends_with('┃'), "{screen}");
        let at = app.mori_now() + 1000;
        let said = serde_json::json!({ "at": at, "host": "pi", "agent": piet, "type": "agent.said", "text": "Done." });
        app.model.apply(&parse_line(&said.to_string()).unwrap());
        app.mori.push([parse_line(&said.to_string()).unwrap()]);
        app.set_clock(at);
        let screen = render(&app, 80, 24);
        assert!(screen.lines().nth(2).unwrap().starts_with(" you   Fix the login."), "{screen}");

        // Down to the latest, it follows it again; closed and opened again, it's back there too.
        app.on_key(key(KeyCode::End));
        for _ in 0..40 {
            app.on_key(key(KeyCode::Down));
        }
        assert_eq!(app.chat_top, None);
        app.on_key(key(KeyCode::PageUp));
        app.on_key(key(KeyCode::Char('p')));
        app.on_key(key(KeyCode::Char('p')));
        let screen = render(&app, 80, 24);
        assert!(screen.lines().nth(20).unwrap().starts_with("       Done."), "{screen}");

        app.on_key(key(KeyCode::Char('1')));
        app.pane = Pane::Agents;
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.selected_agent().map(|a| a.id.as_str()), Some(piet));
        let screen = render(&app, 110, 28);
        assert!(screen.contains("prompt    Make the tests pass. Then tidy up."), "{screen}");
        assert!(screen.contains("ran bash touch allowed.txt"), "{screen}");
    }

    #[test]
    fn opening_memento_mori_shows_the_title_card() {
        let mut app = fixture();
        // Shortly after Wren died, while its grave is still there.
        app.freeze(1_793_385_526_497 + 10_000);
        app.on_key(key(KeyCode::Char('3')));
        // Wren's death still has the stage, whichever session was being followed.
        let wren = app.mori.name("01a18e1b-74d4-7499-8113-59ae26a82065").unwrap();
        let screen = render(&app, 80, 24);
        assert!(screen.contains(&format!("2 · {wren}")), "{screen}");
        // Wren hit the strike limit: a grave, with the one word for how it went.
        let stage = app.mori.scene(app.mori_now(), 80, 22).stage.unwrap();
        let Some(reaper_core::mori::Label::Epitaph(word)) = stage.actors[0].label else {
            panic!("dead, with an epitaph: {:?}", stage.actors[0]);
        };
        assert!(screen.contains(word), "{word}: {screen}");
        // Switching takes the stage back at once.
        app.on_key(key(KeyCode::Left));
        let piet = app.mori.name("01a18e1b-7417-7499-8113-59ad5578e845").unwrap();
        assert!(render(&app, 80, 24).contains(&format!("1 · {piet}")));
    }

    #[test]
    fn without_colour_shapes_and_dots_carry_the_mori() {
        let mut app = fixture();
        app.colors = crate::console::mori::Colors::Off;
        app.tab = Tab::Mori;
        app.on_key(key(KeyCode::Up));
        let screen = render(&app, 80, 24);
        let piet = app.mori.name("01a18e1b-7417-7499-8113-59ad5578e845").unwrap();
        assert!(screen.contains(&format!("▾ {piet}")), "{screen}");
        assert!(screen.contains('█') && screen.contains('▓'), "{screen}");
        let bar = screen.lines().find(|l| l.starts_with(&format!(" {piet} "))).expect("its health bar");
        assert!(bar.contains("░ 1"), "stopped once, counted after the bar: {screen}");
        let Ok(mut terminal) = Terminal::new(TestBackend::new(80, 24));
        let Ok(_) = terminal.draw(|f| draw(f, &app));
        let buffer = terminal.backend().buffer();
        let drawn = buffer.content.iter().filter(|c| "█▀▄▓▒░▼".contains(c.symbol()));
        assert!(drawn.into_iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset), "no colour in the drawing");
    }
}
