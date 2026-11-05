//! Asks an agent the human already has (Claude Code, else pi) to write a rule from their own
//! words. It gets reaper's rule-writing skill as its only instructions and no tools, so all it
//! can do is answer. Its snippet is checked like any other (`draft::check`), and goes back to
//! it once with whatever failed.

use std::env;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use reaper_core::draft::{self, Draft, Report};
use reaper_core::rules::parse;
use serde_json::Value;

use crate::SKILL;
use crate::hosts::{on_path, pi::release};

const TIMEOUT: Duration = Duration::from_secs(180);
const TRIES: usize = 2;

pub type Ask = dyn Fn(&str) -> Result<String, String> + Send + Sync;

pub struct Writer {
    /// The agent, as the human knows it.
    pub name: String,
    pub ask: Box<Ask>,
}

/// A rule as written, with what reaper found trying it.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub asked: String,
    pub draft: Draft,
    pub report: Report,
}

impl Writer {
    /// The agent to ask: Claude Code if it's here, else pi (reaper's own, if fetched already).
    pub fn find() -> Option<Self> {
        if let Some(claude) = on_path("claude") {
            return Some(Self {
                name: "Claude Code".into(),
                ask: Box::new(move |prompt| claude_code(&claude, prompt)),
            });
        }
        let pi = on_path("pi").or_else(release::cached)?;
        Some(Self { name: "pi".into(), ask: Box::new(move |prompt| pi_print(&pi, prompt)) })
    }

    /// A rule for what the human `asked`, checked against the rules file's `rules`. Errs only
    /// when there's nothing to show: no snippet came back at all.
    pub fn write(&self, asked: &str, rules: &str) -> Result<Proposal, String> {
        let mut prompt = request(asked, rules);
        let (mut shown, mut error) = (None, String::new());
        for _ in 0..TRIES {
            let reply = (self.ask)(&prompt)?;
            let failures = match draft::read(&reply) {
                Ok(draft) => {
                    let report = draft::check(&draft, rules);
                    let failures = report.failures();
                    let proposal = Proposal { asked: asked.to_string(), draft, report };
                    if failures.is_empty() {
                        return Ok(proposal);
                    }
                    shown = Some(proposal);
                    failures
                }
                Err(e) => {
                    error.clone_from(&e);
                    vec![e]
                }
            };
            prompt = again(&request(asked, rules), &reply, &failures);
        }
        shown.ok_or(error)
    }
}

fn request(asked: &str, rules: &str) -> String {
    let mut prompt =
        format!("Write one reaper rule for this request, in the person's own words:\n\n{}\n", asked.trim());
    if let Ok(config) = parse(rules) {
        let ids: Vec<&str> = config.all_rules().map(|(_, r)| r.id.as_str()).collect();
        if !ids.is_empty() {
            prompt.push_str(&format!("\nRule ids already taken: {}\n", ids.join(", ")));
        }
        for (name, project) in &config.projects {
            prompt.push_str(&format!("Project {name}: {}\n", project.paths.join(", ")));
        }
    }
    prompt.push_str("\nReply with the YAML snippet only.");
    prompt
}

fn again(request: &str, reply: &str, failures: &[String]) -> String {
    let found: Vec<String> = failures.iter().map(|f| format!("- {f}")).collect();
    format!(
        "{request}\n\nYour last reply:\n\n{}\n\nreaper tried it and found:\n{}\n\nReply with the corrected snippet only.",
        reply.trim(),
        found.join("\n")
    )
}

/// Claude Code with no tools and no hooks (reaper's own would check this very session), in
/// print mode. The rest of the human's settings stay, the model and any provider among them.
fn claude_code(exe: &Path, prompt: &str) -> Result<String, String> {
    let mut command = Command::new(exe);
    command.args(["-p", "--system-prompt", SKILL, "--tools", "", "--settings", r#"{"disableAllHooks":true}"#]);
    command.args(["--no-session-persistence", "--output-format", "json", prompt]);
    let out = run(&mut command)?;
    let reply: Value =
        serde_json::from_str(&out).map_err(|_| format!("Claude Code answered oddly: {}", first_line(&out)))?;
    let text = reply["result"].as_str().unwrap_or_default().to_string();
    if reply["is_error"].as_bool() == Some(true) { Err(first_line(&text)) } else { Ok(text) }
}

/// pi with no tools and no extensions (reaper's among them), in print mode, saving nothing.
fn pi_print(exe: &Path, prompt: &str) -> Result<String, String> {
    let mut command = Command::new(exe);
    command.args(["-p", "--no-tools", "--no-extensions", "--no-session", "--system-prompt", SKILL, prompt]);
    run(&mut command)
}

/// Runs it away from any project, so no project's instructions come along, and gives up
/// after `TIMEOUT`.
fn run(command: &mut Command) -> Result<String, String> {
    let mut child = command
        .current_dir(env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start it: {e}"))?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("stdout is piped")));
    let err = drain(Box::new(child.stderr.take().expect("stderr is piped")));
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within {} seconds", TIMEOUT.as_secs()));
            }
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() { Ok(out) } else { Err(first_line(if err.trim().is_empty() { &out } else { &err })) }
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("it failed without saying why").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const GOOD: &str = "```yaml\nrule:\n  id: no-sudo\n  tools: [bash]\n  match: { command: '(^|[;&|(]\\s*)sudo\\b' }\n  redirect: Run it without elevated rights, or ask the person to run it.\nstops: [sudo rm -rf build, ls && sudo make install]\nallows: [cat sudoers.md, grep sudo README.md]\n```";

    fn writer(replies: &[&str]) -> (Writer, Arc<Mutex<Vec<String>>>) {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let (seen, replies) = (prompts.clone(), Mutex::new(replies.iter().map(|r| r.to_string()).collect::<Vec<_>>()));
        let ask = move |prompt: &str| {
            seen.lock().unwrap().push(prompt.to_string());
            Ok(replies.lock().unwrap().remove(0))
        };
        (Writer { name: "test".into(), ask: Box::new(ask) }, prompts)
    }

    /// Asks the agent on this machine for real: `cargo test -p agent-reaper real_agent -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_real_agent_writes_a_rule() {
        let w = Writer::find().expect("Claude Code or pi");
        let asked = "agents keep running the whole test suite and it crashes my laptop. they should only run the tests for what they changed";
        let p = w.write(asked, reaper_core::EXAMPLE_RULES).unwrap();
        println!("{}: {:#?}\n{:#?}", w.name, p.draft, p.report.tried);
        println!("problems {:?}\nwarnings {:?}", p.report.problems, p.report.warnings);
        assert!(p.report.ok());
    }

    #[test]
    fn a_rule_that_passes_comes_back_at_once() {
        let (w, prompts) = writer(&[GOOD]);
        let proposal = w.write("never let them use sudo", reaper_core::EXAMPLE_RULES).unwrap();
        assert!(proposal.report.ok());
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("never let them use sudo") && prompts[0].contains("force-push, secrets"));
    }

    #[test]
    fn what_failed_goes_back_once() {
        let leaky = GOOD.replace("grep sudo README.md", "grep sudo README.md, echo hi; sudo -v");
        let (w, prompts) = writer(&["no yaml here", &leaky, GOOD]);
        let proposal = w.write("no sudo", "").unwrap();
        assert!(!proposal.report.ok(), "two tries, and the second still fails");
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].contains("not a rule snippet"), "{}", prompts[1]);

        let (w, prompts) = writer(&[&leaky, GOOD]);
        assert!(w.write("no sudo", "").unwrap().report.ok());
        assert!(prompts.lock().unwrap()[1].contains("`echo hi; sudo -v` should be let through"));
    }
}
