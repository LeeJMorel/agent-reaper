//! Hands the terminal to the human's editor, and takes it back when they're done.

use std::env;
use std::io::{self, stdout};
use std::path::Path;
use std::process::Command;

use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

fn configured() -> String {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(value) = env::var(var)
            && !value.trim().is_empty()
        {
            return value;
        }
    }
    let installed = |program: &str| {
        env::var_os("PATH").is_some_and(|paths| env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
    };
    ["nano", "vi"].into_iter().find(|p| installed(p)).unwrap_or("vi").to_string()
}

fn ensure(args: &mut Vec<String>, flag: &str) {
    if !args.iter().any(|a| a == flag || a == "-w") {
        args.push(flag.into());
    }
}

pub fn command(editor: &str, path: &Path, line: Option<usize>) -> (String, Vec<String>) {
    let mut words = editor.split_whitespace().map(String::from);
    let program = words.next().unwrap_or_else(|| "vi".into());
    let mut args: Vec<String> = words.collect();
    let name = Path::new(&program).file_name().and_then(|n| n.to_str()).unwrap_or(&program).to_string();
    let file = path.display().to_string();
    let at = |n: usize| format!("{file}:{n}");

    match name.as_str() {
        // GUI editors return immediately unless told to wait for the file to close.
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => {
            ensure(&mut args, "--wait");
            match line {
                Some(n) => args.extend(["--goto".into(), at(n)]),
                None => args.push(file),
            }
        }
        "subl" | "zed" => {
            ensure(&mut args, "--wait");
            args.push(line.map_or(file.clone(), at));
        }
        "hx" | "helix" => args.push(line.map_or(file.clone(), at)),
        "vi" | "vim" | "nvim" | "nano" | "pico" | "emacs" | "emacsclient" | "micro" | "kak" | "mg" | "joe" | "ne" => {
            args.extend(line.map(|n| format!("+{n}")));
            args.push(file);
        }
        _ => args.push(file),
    }
    (program, args)
}

/// The outer error is the terminal failing to come back; the inner one is the editor
/// failing, which the console reports and survives.
pub fn open(terminal: &mut DefaultTerminal, path: &Path, line: Option<usize>) -> io::Result<Result<(), String>> {
    let (program, args) = command(&configured(), path, line);

    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)?;
    let status = Command::new(&program).args(&args).status();
    execute!(stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.clear()?;

    Ok(match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{program} exited with {s}")),
        Err(e) => Err(format!("Couldn't start your editor ({program}): {e}. Set $EDITOR.")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(editor: &str, line: Option<usize>) -> String {
        let (program, args) = command(editor, Path::new("/r/rules.yaml"), line);
        std::iter::once(program).chain(args).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn opens_at_the_line_in_each_editor_family() {
        assert_eq!(run("vim", Some(12)), "vim +12 /r/rules.yaml");
        assert_eq!(run("/usr/bin/nano", Some(3)), "/usr/bin/nano +3 /r/rules.yaml");
        assert_eq!(run("hx", Some(12)), "hx /r/rules.yaml:12");
        assert_eq!(run("code", Some(12)), "code --wait --goto /r/rules.yaml:12");
        assert_eq!(run("code -w", Some(12)), "code -w --goto /r/rules.yaml:12");
        assert_eq!(run("subl", None), "subl --wait /r/rules.yaml");
        assert_eq!(run("someeditor --flag", Some(12)), "someeditor --flag /r/rules.yaml");
    }
}
