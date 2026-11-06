//! The console: the agents reaper is watching, what it killed, and the rules.

mod app;
mod editor;
mod mori;
mod start;
mod ui;
mod watcher;
mod writer;

use std::io::{self, stdout};
use std::path::PathBuf;
use std::process::{self, ExitCode, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::{DefaultTerminal, Terminal};
use reaper_core::paths;

use app::{App, Command, Tab};

const TICK: Duration = Duration::from_millis(250);

pub fn main(args: &[String]) -> Result<ExitCode, String> {
    let (mut rules, mut events) = (paths::rules_file(), paths::events_file());
    let (mut tab, mut size, mut at, mut session, mut color) = (None, (100, 30), None, None, false);
    // A console on other files than reaper's own (the demo, a look at someone's log) sets nothing up.
    let mut elsewhere = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--rules" => (rules, elsewhere) = (PathBuf::from(value()?), true),
            "--events" => (events, elsewhere) = (PathBuf::from(value()?), true),
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
    if tab.is_none() && !elsewhere {
        match crate::hosts::guard() {
            (_, Some(problem)) => app.notify(problem, true),
            (guarded, None) if !guarded.is_empty() => {
                let names: Vec<&str> = guarded.iter().map(|(_, name)| *name).collect();
                let agent = if let [(id, _)] = guarded[..] { id } else { "AGENT" };
                app.notify(format!("Now guarding {} (reaper uninstall {agent} undoes it)", names.join(" and ")), false);
            }
            _ => {}
        }
    }
    app.guarded = crate::hosts::guarded();
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

    app.writer = writer::Writer::find().map(std::sync::Arc::new);
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
                    Command::Start(start) => {
                        let result = start::run(terminal, &start)?;
                        app.after_start(result);
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

/// Leaves the terminal as the console found it, for the editor or an agent to use.
fn step_aside() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)
}

/// Takes the terminal back from them.
fn come_back(terminal: &mut DefaultTerminal) -> io::Result<()> {
    execute!(stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.clear()
}

/// Runs `command` once the console has stepped aside. A ctrl-c it doesn't take as a key (a
/// shell it runs, a download) reaches the console too, in the same process group, so until
/// `command` exits the console lets it pass. `command` gets it as ever: a caught signal is
/// back to its default in a program that starts.
fn run_aside(command: &mut process::Command) -> io::Result<ExitStatus> {
    let ends_us = ctrl_c_ends_us();
    ends_us.iter().for_each(|flag| flag.store(false, Ordering::SeqCst));
    let status = command.status();
    ends_us.iter().for_each(|flag| flag.store(true, Ordering::SeqCst));
    status
}

/// Cleared, a ctrl-c doesn't end the console; set, it does, as by default.
#[cfg(unix)]
fn ctrl_c_ends_us() -> Option<&'static AtomicBool> {
    static ENDS_US: OnceLock<Option<Arc<AtomicBool>>> = OnceLock::new();
    let ends_us = ENDS_US.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(true));
        signal_hook::flag::register_conditional_default(signal_hook::consts::SIGINT, flag.clone()).ok().map(|_| flag)
    });
    ends_us.as_deref()
}

#[cfg(not(unix))]
fn ctrl_c_ends_us() -> Option<&'static AtomicBool> {
    None
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

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    #[test]
    fn a_ctrl_c_meant_for_what_has_the_terminal_leaves_the_console_be() {
        // Not a shell, which would be background work to a Claude Code watcher's test.
        let aside = |script: &str| super::run_aside(Command::new("perl").args(["-e", script])).unwrap();
        // This test process plays the console: were the ctrl-c to reach it, it would end here.
        assert!(aside("kill 'INT', getppid; select undef, undef, undef, 0.2").success());
        let own = aside("kill 'INT', $$; sleep 1");
        assert!(!own.success(), "what has the terminal still gets its own ctrl-c: {own}");
    }
}
