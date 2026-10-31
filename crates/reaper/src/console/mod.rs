//! The console: the agents reaper is watching, what it killed, and the rules.

mod app;
mod editor;
mod mori;
mod ui;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::{DefaultTerminal, Terminal};
use reaper_core::paths;

use app::{App, Command, Tab};

const TICK: Duration = Duration::from_millis(250);

pub fn main(args: &[String]) -> Result<ExitCode, String> {
    let (mut rules, mut events) = (paths::rules_file(), paths::events_file());
    let (mut tab, mut size, mut at, mut session, mut color) = (None, (100, 30), None, None, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--rules" => rules = PathBuf::from(value()?),
            "--events" => events = PathBuf::from(value()?),
            "--dump" => {
                tab = Some(match value()?.as_str() {
                    "live" => Tab::Live,
                    "rules" => Tab::Rules,
                    "mori" | "memento-mori" => Tab::Mori,
                    other => return Err(format!("--dump takes live, rules or mori, not {other}")),
                })
            }
            "--size" => {
                let v = value()?;
                size = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .ok_or(format!("--size takes WIDTHxHEIGHT, not {v}"))?;
            }
            "--at" => at = Some(value()?),
            "--color" => color = true,
            "--session" => {
                let v = value()?;
                session = Some(
                    v.parse::<usize>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or(format!("--session takes a number from 1, not {v}"))?,
                );
            }
            other => return Err(format!("unknown argument {other} (see reaper --help)")),
        }
    }

    let mut app = App::new(rules, events);
    if let Some(at) = at {
        let newest = app.model.agents.iter().map(|a| a.last_at).max().unwrap_or(0);
        let at = match at.strip_prefix('+') {
            Some(after) => after.parse::<i64>().map(|ms| newest + ms),
            None if at.starts_with('-') => at.parse::<i64>().map(|ms| newest + ms),
            None => at.parse::<i64>(),
        }
        .map_err(|_| format!("--at takes milliseconds since the epoch, or +MS/-MS from the newest event, not {at}"))?;
        app.freeze(at);
    }
    if let Some(n) = session {
        app.mori.choose(n - 1);
    }
    if let Some(tab) = tab {
        app.tab = tab;
        print!("{}", dump(&app, size.0, size.1, color));
        return Ok(ExitCode::SUCCESS);
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS).map_err(|e| e.to_string())
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        if app.take_dirty() {
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
        // While memento mori moves it asks for frames sooner; otherwise the files are polled each tick.
        let wait = app.mori_wait().map_or(TICK, |wait| wait.min(TICK));
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.on_key(key) {
                    Command::Quit => return Ok(()),
                    Command::Edit { line } => {
                        let result = editor::open(terminal, &app.rules.path, line)?;
                        app.after_edit(result);
                    }
                    Command::Nothing => {}
                },
                Event::Resize(..) => app.mark_dirty(),
                _ => {}
            }
        }
        app.tick();
    }
}

/// For bug reports, docs, and checking the layout without a terminal. Memento mori needs
/// `color` to be read at all, since it paints its own sky.
fn dump(app: &App, width: u16, height: u16, color: bool) -> String {
    let Ok(mut terminal) = Terminal::new(TestBackend::new(width, height));
    let Ok(_) = terminal.draw(|frame| ui::draw(frame, app));
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..height {
        if !color {
            let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
            out.push_str(line.trim_end());
            out.push('\n');
            continue;
        }
        for x in 0..width {
            let cell = &buffer[(x, y)];
            let mut codes = vec!["0".to_string()];
            codes.extend(sgr(cell.fg, 38));
            codes.extend(sgr(cell.bg, 48));
            if cell.modifier.contains(ratatui::style::Modifier::BOLD) {
                codes.push("1".into());
            }
            if cell.modifier.contains(ratatui::style::Modifier::DIM) {
                codes.push("2".into());
            }
            out.push_str(&format!("\x1b[{}m{}", codes.join(";"), cell.symbol()));
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

fn sgr(color: ratatui::style::Color, base: u8) -> Option<String> {
    use ratatui::style::Color;
    match color {
        Color::Rgb(r, g, b) => Some(format!("{base};2;{r};{g};{b}")),
        Color::Indexed(i) => Some(format!("{base};5;{i}")),
        Color::DarkGray => Some(format!("{base};5;8")),
        Color::Black => Some(format!("{base};5;0")),
        _ => None,
    }
}
