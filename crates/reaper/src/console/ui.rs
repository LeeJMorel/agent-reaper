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
use reaper_core::paths::tilde;
use reaper_core::rules::Rule;
use reaper_core::state::{Agent, Kill, Outcome, Status, WatcherHealth};

use super::app::{App, Pane, RuleRow, Tab, plural};
use super::mori;

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

fn header(frame: &mut Frame, area: Rect, app: &App) {
    let tab = |key: &str, name: &str, on: bool| {
        let style = if on { bold().add_modifier(Modifier::REVERSED) } else { dim() };
        Span::styled(format!(" {key} {name} "), style)
    };
    let left = Line::from(vec![
        Span::styled(" reaper ", Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        tab("1", "live", app.tab == Tab::Live),
        Span::raw(" "),
        tab("2", "rules", app.tab == Tab::Rules),
        Span::raw(" "),
        tab("3", "memento mori", app.tab == Tab::Mori),
    ]);
    let room = (area.width as usize).saturating_sub(left.width() + 1);
    let right = summary(app, room);
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right.width() as u16)]).areas(area);
    frame.render_widget(left, l);
    frame.render_widget(right, r);
}

/// Counts (of the agents listed) are dropped first, alerts last.
fn summary(app: &App, room: usize) -> Line<'static> {
    let agents = app.agents();
    let working = agents.iter().filter(|a| matches!(a.status, Status::Working | Status::Killed)).count();
    let dead = agents.iter().filter(|a| a.status == Status::Dead).count();
    let today = app.model.kills.iter().filter(|k| k.at >= app.today).count();
    let mut parts = vec![
        Span::raw(plural(agents.len(), "agent")),
        Span::styled(format!("{working} working"), if working > 0 { fg(Color::Green) } else { Style::new() }),
        Span::styled(format!("{dead} dead"), if dead > 0 { fg(ACCENT) } else { Style::new() }),
        Span::raw(format!("{} today", plural(today, "kill"))),
        match &app.model.watcher {
            WatcherHealth::Unknown => Span::styled("watcher –", dim()),
            WatcherHealth::Ok { .. } => Span::styled("watcher ok", fg(Color::Green)),
            WatcherHealth::Failing { at, .. } => Span::styled(
                format!("watcher failing ({} ago)", ago(app.now, *at)),
                fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        },
    ];
    if app.rules.error.is_some() {
        parts.push(Span::styled("rules: syntax error", fg(ACCENT).add_modifier(Modifier::BOLD)));
    }
    let width = |parts: &[Span]| parts.iter().map(Span::width).sum::<usize>() + 3 * parts.len().saturating_sub(1) + 1;
    while parts.len() > 1 && width(&parts) > room {
        parts.remove(0);
    }

    let mut spans = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(part);
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(notice) = &app.notice {
        Line::styled(format!(" {}", notice.text), if notice.bad { fg(ACCENT) } else { fg(Color::Green) })
    } else if let Some(error) = &app.events_error {
        Line::styled(format!(" Can't read {}: {error}", tilde(app.events_path())), fg(ACCENT))
    } else {
        let keys = match app.tab {
            Tab::Live => {
                vec![("1 2 3", "tabs"), ("tab", "switch pane"), ("↑↓", "select"), ("e", "edit rule"), ("q", "quit")]
            }
            Tab::Rules => vec![("1 2 3", "tabs"), ("↑↓", "select"), ("e", "edit in your editor"), ("q", "quit")],
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
    let mut keys = vec![("←→", "switch session"), ("↑↓", "pick agent")];
    if app.mori.picked().is_some() {
        keys.extend([("enter", "details"), ("esc", "unpick")]);
    } else if !app.mori.following_the_action() {
        keys.push(("esc", "follow activity"));
    }
    if app.mori.prompt(app.mori_now()).is_some() {
        keys.push(("p", if app.prompt_open { "hide prompt" } else { "show prompt" }));
    }
    keys.extend([("q", "quit"), ("d", "demo a death")]);
    keys
}

// ── live tab ────────────────────────────────────────────────────────────────

fn live(frame: &mut Frame, area: Rect, app: &App) {
    let [lists, detail] = Layout::vertical([Constraint::Min(6), Constraint::Length(10)]).areas(area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(lists);
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
        Status::Working => a.last_tool.clone().unwrap_or_default(),
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
    // A subagent works on its session's prompt. One line: some prompts are pages long.
    let prompt = a.prompt.as_deref().or_else(|| app.model.agent(a.parent.as_deref()?)?.prompt.as_deref());
    if let Some(prompt) = prompt {
        let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.insert(2, field("prompt", vec![Span::raw(fit(&flat, width.saturating_sub(LABEL)))]));
    }
    let room = rows.saturating_sub(lines.len());
    let recent: Vec<&Event> = a.recent.iter().rev().take(room).collect();
    for (i, event) in recent.into_iter().rev().enumerate() {
        let mut spans = vec![Span::styled(format!("{}  ", clock(event.at, &app.tz)), dim())];
        spans.push(describe(event));
        lines.push(field(if i == 0 { "recent" } else { "" }, spans));
    }
    Text::from(lines)
}

fn describe(event: &Event) -> Span<'static> {
    match &event.kind {
        Kind::Spawned { .. } => Span::raw("started"),
        Kind::Prompted { .. } => Span::raw("asked for something"),
        Kind::Acted { tool } => Span::raw(format!("ran {tool}")),
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
    if !app.rules.exists {
        let text = Text::from(vec![
            Line::styled(format!("No rules file at {}.", tilde(&app.rules.path)), bold()),
            Line::raw(""),
            Line::raw("Press e to create one from the example and open it in your editor."),
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

    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(main);
    rule_list(frame, left, app);
    let detail = rule_detail(app, right.width.saturating_sub(2) as usize);
    frame.render_widget(Paragraph::new(detail).block(pane("Rule".into(), false)).wrap(Wrap { trim: false }), right);
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
            Text::from(vec![Line::raw("No rules yet."), Line::styled("Press e to add some.", dim())])
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
    let checked = match rule.checked_by() {
        "pattern" => "pattern match, on every call",
        "watcher" => "the watcher model",
        "pattern+watcher" => "pattern first, then the watcher model",
        _ => "nothing (needs match or judge)",
    };
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
    fn live_tab_shows_agents_kills_and_what_the_agent_was_told() {
        let app = fixture();
        let screen = render(&app, 110, 28);
        for expected in [
            "2 agents",
            "◌ 000001 reaped",
            "0 working",
            "0 dead",
            "2 kills today",
            "watcher ok",
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
        assert!(rows[23].contains("pick"), "{screen}");
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
        // At 80 columns, the demo death makes room for what picking adds.
        assert_eq!(
            screen.lines().last(),
            Some(" ←→ switch session  ↑↓ pick agent  enter details  esc unpick  q quit"),
            "{screen}"
        );

        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.tab, Tab::Live);
        assert_eq!(app.selected_agent().map(|a| a.id.as_str()), Some("01a18e1b-7417-7499-8113-59ad5578e845"));
    }

    #[test]
    fn a_sessions_prompt_shows_on_one_line_and_opens_with_p() {
        let mut app = fixture();
        let piet = "01a18e1b-7417-7499-8113-59ad5578e845";
        let prompt = format!("Make the tests pass.\n\n\n{}", "Then tidy up. ".repeat(150));
        let line = serde_json::json!({ "at": 1_793_385_526_400_i64, "host": "pi", "agent": piet, "type": "agent.prompted", "prompt": prompt });
        let mut events: Vec<Event> =
            include_str!("../../../reaper-core/tests/fixtures/events.jsonl").lines().filter_map(parse_line).collect();
        events.insert(1, parse_line(&line.to_string()).unwrap());
        app.model = reaper_core::state::Model::default();
        app.model.apply_all(&events);
        app.mori.clear();
        app.mori.push(events);
        app.set_clock(app.now);
        app.tab = Tab::Mori;

        let screen = render(&app, 80, 24);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[2].starts_with(" Make the tests pass. Then tidy up.") && rows[2].ends_with('…'), "{screen}");
        assert_eq!(rows[2].chars().count(), 79, "{screen}");
        assert_eq!(
            rows[23], " ←→ switch session  ↑↓ pick agent  p show prompt  q quit  d demo a death",
            "only keys that do something: {screen}"
        );

        app.on_key(key(KeyCode::Char('p')));
        let screen = render(&app, 80, 24);
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rows[2], " Make the tests pass.", "its own lines: {screen}");
        assert_eq!(rows[3], "", "one blank line between paragraphs: {screen}");
        assert!(rows[4].starts_with(" Then tidy up."), "{screen}");
        assert!(rows[20].ends_with('…') && rows[21].is_empty(), "as much as fits: {screen}");
        assert!(rows[23].contains("p hide prompt"), "{screen}");

        app.on_key(key(KeyCode::Char('1')));
        app.pane = Pane::Agents;
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.selected_agent().map(|a| a.id.as_str()), Some(piet));
        let screen = render(&app, 110, 28);
        assert!(screen.contains("prompt    Make the tests pass. Then tidy up."), "{screen}");
    }

    #[test]
    fn opening_memento_mori_shows_the_title_card() {
        let mut app = fixture();
        // Shortly after Wren died, while its grave is still there.
        app.freeze(1_793_385_526_497 + 10_000);
        app.on_key(key(KeyCode::Char('3')));
        let piet = app.mori.name("01a18e1b-7417-7499-8113-59ad5578e845").unwrap();
        assert!(render(&app, 80, 24).contains(&format!("1 · {piet}")));
        app.on_key(key(KeyCode::Right));
        let wren = app.mori.name("01a18e1b-74d4-7499-8113-59ae26a82065").unwrap();
        let screen = render(&app, 80, 24);
        assert!(screen.contains(&format!("2 · {wren}")), "{screen}");
        // Wren hit the strike limit: a grave, with the one word for how it went.
        let stage = app.mori.scene(app.mori_now(), 80, 22).stage.unwrap();
        let Some(reaper_core::mori::Label::Epitaph(word)) = stage.actors[0].label else {
            panic!("dead, with an epitaph: {:?}", stage.actors[0]);
        };
        assert!(screen.contains(word), "{word}: {screen}");
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
        assert!(screen.contains('▼'), "a crack where it was stopped: {screen}");
        let Ok(mut terminal) = Terminal::new(TestBackend::new(80, 24));
        let Ok(_) = terminal.draw(|f| draw(f, &app));
        let buffer = terminal.backend().buffer();
        let drawn = buffer.content.iter().filter(|c| "█▀▄▓▒░▼".contains(c.symbol()));
        assert!(drawn.into_iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset), "no colour in the drawing");
    }
}
