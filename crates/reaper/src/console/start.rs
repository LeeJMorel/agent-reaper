//! Starting an agent from the live tab (`s`, left out of the footer). reaper guards agents
//! however they're started, so this is only for when the console is where you are. The agent
//! gets this terminal, as the editor does, and the console comes back when it exits.

use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use ratatui::DefaultTerminal;
use reaper_core::paths::tilde;
use reaper_core::state::{Agent, Status};

use super::ui::short;
use crate::hosts::{claude_code, on_path, pi::release};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Start {
    pub name: String,
    pub about: String,
    /// The agent, as the human knows it.
    pub agent: &'static str,
    /// As the event log names it.
    pub host: &'static str,
    /// The session it picks up again, if it's not a new one.
    pub resumes: Option<String>,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub dir: PathBuf,
}

/// Picking `selected` up where it left off, once it was reaped or closed, then a new session
/// where `selected` works and in `here`, in each agent there is: Claude Code while reaper
/// guards it, and pi through `reaper run`, which fetches one if it has to.
pub fn choices(selected: Option<&Agent>, here: &Path) -> Vec<Start> {
    let claude = on_path("claude").filter(|_| claude_code::install::installed());
    offer(selected, here, claude, env::current_exe().ok(), release::cached().is_some())
}

fn offer(
    selected: Option<&Agent>,
    here: &Path,
    claude: Option<PathBuf>,
    reaper: Option<PathBuf>,
    pi_fetched: bool,
) -> Vec<Start> {
    let claude_code = |resumes: Option<&str>, dir: &Path, name: &str, about: String| {
        let args = resumes.map(|id| vec!["--resume".into(), id.into()]).unwrap_or_default();
        let (agent, host, resumes) = ("Claude Code", claude_code::HOST, resumes.map(String::from));
        Some(Start { name: name.into(), about, agent, host, resumes, program: claude.clone()?, args, dir: dir.into() })
    };
    let pi = |resumes: Option<&str>, dir: &Path, name: &str, about: String| {
        let mut args = vec!["run".to_string()];
        args.extend(resumes.iter().flat_map(|id| ["--session".into(), id.to_string()]));
        let (agent, host, resumes) = ("pi", "pi", resumes.map(String::from));
        Some(Start { name: name.into(), about, agent, host, resumes, program: reaper.clone()?, args, dir: dir.into() })
    };
    let mut out = Vec::new();
    let worked_in = selected.and_then(|a| Some((a, Path::new(a.dir.as_deref()?))));
    if let Some((a, dir)) = worked_in.filter(|(a, _)| matches!(a.status, Status::Reaped | Status::Ended))
        && a.parent.is_none()
    {
        let name = format!("Resume {}", short(&a.id));
        let about = |agent: &str| format!("Its {agent} session in {}, where it left off.", tilde(dir));
        out.extend(match a.host.as_str() {
            claude_code::HOST => claude_code(Some(&a.id), dir, &name, about("Claude Code")),
            "pi" => pi(Some(&a.id), dir, &name, about("pi")),
            _ => None,
        });
    }
    let fetch = format!(" reaper fetches pi {} first, once.", release::VERSION);
    let elsewhere = worked_in.map(|(_, dir)| dir).filter(|dir| *dir != here);
    for dir in elsewhere.into_iter().chain([here]) {
        let new = format!("A new session in {}.", tilde(dir));
        out.extend(claude_code(None, dir, "Claude Code", new.clone()));
        out.extend(pi(None, dir, "pi", if pi_fetched { new } else { new + &fetch }));
    }
    out
}

/// Runs `start` in this terminal. When it fails, what it said stays on screen until enter; a
/// signal (reaped, killed) isn't failing.
pub fn run(terminal: &mut DefaultTerminal, start: &Start) -> io::Result<Result<(), String>> {
    super::step_aside()?;
    let status = super::run_aside(Command::new(&start.program).args(&start.args).current_dir(&start.dir));
    if let Ok(status) = &status
        && status.code().is_some_and(|code| code != 0)
    {
        eprint!("\nreaper: {} stopped ({status}). Press enter for the console. ", start.agent);
        let _ = io::stdin().read_line(&mut String::new());
    }
    super::come_back(terminal)?;
    Ok(status.map(|_| ()).map_err(|e| format!("Couldn't start {} in {}: {e}", start.agent, tilde(&start.dir))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reaper_core::events::parse_line;
    use reaper_core::state::Model;

    fn model(lines: &[&str]) -> Model {
        let mut model = Model::default();
        for (at, line) in lines.iter().enumerate() {
            let (host, agent, kind, fields) = {
                let parts: Vec<&str> = line.splitn(4, ' ').collect();
                (parts[0], parts[1], parts[2], parts.get(3).copied().unwrap_or(""))
            };
            let json = format!(r#"{{"at":{at},"host":"{host}","agent":"{agent}","type":"{kind}"{fields}}}"#);
            model.apply(&parse_line(&json).unwrap());
        }
        model
    }

    fn shown(starts: &[Start]) -> Vec<String> {
        starts
            .iter()
            .map(|s| format!("{} @ {}: {} {}", s.name, s.dir.display(), s.program.display(), s.args.join(" ")))
            .collect()
    }

    #[test]
    fn a_reaped_or_closed_session_is_resumed_where_it_was_and_new_ones_start_there_and_here() {
        let model = model(&[
            r#"pi p-0001 agent.spawned ,"dir":"/w/a""#,
            r#"pi p-0001 agent.reaped ,"idle":300000"#,
            r#"claude-code c-0002 agent.spawned ,"dir":"/w/b""#,
            "claude-code c-0002 agent.ended",
            r#"claude-code c-0003 agent.spawned ,"dir":"/here""#,
            r#"claude-code sub-04 agent.spawned ,"dir":"/here","parent":"c-0003""#,
            "claude-code c-0003 agent.ended",
        ]);
        let (here, claude, reaper) = (Path::new("/here"), Some("/bin/claude".into()), Some("/bin/reaper".into()));
        let offer = |id: &str, claude: Option<PathBuf>| offer(model.agent(id), here, claude, reaper.clone(), true);

        assert_eq!(
            shown(&offer("p-0001", claude.clone())),
            [
                "Resume p-0001 @ /w/a: /bin/reaper run --session p-0001",
                "Claude Code @ /w/a: /bin/claude ",
                "pi @ /w/a: /bin/reaper run",
                "Claude Code @ /here: /bin/claude ",
                "pi @ /here: /bin/reaper run"
            ]
        );
        let resume = &offer("c-0002", claude.clone())[0];
        assert_eq!(shown(std::slice::from_ref(resume)), ["Resume c-0002 @ /w/b: /bin/claude --resume c-0002"]);
        assert_eq!((resume.host, resume.resumes.as_deref()), ("claude-code", Some("c-0002")));
        assert_eq!(offer("sub-04", claude.clone()).len(), 2, "a subagent goes with its session, which works here");
        assert_eq!(
            shown(&offer("c-0002", None)),
            ["pi @ /w/b: /bin/reaper run", "pi @ /here: /bin/reaper run"],
            "no Claude Code, nothing to resume it"
        );

        let working = model.agent("p-0001").map(|a| Agent { status: Status::Idle, ..a.clone() });
        assert_eq!(super::offer(working.as_ref(), here, None, reaper.clone(), true).len(), 2, "it's still there");
        let fetching = super::offer(None, here, None, reaper.clone(), false);
        assert!(fetching[0].about.ends_with("first, once."), "{:?}", fetching[0].about);
    }
}
