//! End to end with Claude Code, exactly as users run it: the real `claude` binary, with
//! hooks put in place by the real `reaper install claude-code`, and every decision made by
//! the reaper binary those hooks run. Only the model is fake: a local server speaking the
//! Anthropic Messages API (via ANTHROPIC_BASE_URL) and playing the watcher too.
//!
//! HOME and CLAUDE_CONFIG_DIR point into a sandbox, so your own Claude Code settings are
//! never read or changed. Needs Claude Code installed (`claude` on PATH, or REAPER_TEST_CLAUDE).

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{SCRIPT, Terminal, event, events, has_terminal, kinds, sse, text_of, watcher_reply};
use serde_json::{Value, json};

const TASK: &str = "MAKE_FILES: set up the marker files for this project.";
/// The same task, for an agent that ignores every redirect.
const STUBBORN: &str = "MAKE_FILES STUBBORN: set up the marker files for this project.";
const RULE_ID: &str = "no-violation-file";
const RULE_TEXT: &str = "Creating a file named violation.txt.";
const REDIRECT: &str = "Create the marker file allowed.txt in the project root.";
const SCHEMA_REDIRECT: &str = "Follow docs/schema.md for every entity and relation.";

fn claude() -> PathBuf {
    if let Some(path) = env::var_os("REAPER_TEST_CLAUDE") {
        return path.into();
    }
    env::var_os("PATH")
        .and_then(|paths| env::split_paths(&paths).map(|dir| dir.join("claude")).find(|p| p.is_file()))
        .expect("Claude Code isn't installed: put `claude` on PATH or set REAPER_TEST_CLAUDE")
}

fn stream(block: Value, delta: Value, stop_reason: &str) -> String {
    let events = [
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "type": "message", "role": "assistant", "model": "claude-test", "content": [], "stop_reason": null, "stop_sequence": null, "usage": { "input_tokens": 10, "output_tokens": 1 } } }),
        ),
        ("content_block_start", json!({ "type": "content_block_start", "index": 0, "content_block": block })),
        ("content_block_delta", json!({ "type": "content_block_delta", "index": 0, "delta": delta })),
        ("content_block_stop", json!({ "type": "content_block_stop", "index": 0 })),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason, "stop_sequence": null }, "usage": { "output_tokens": 5 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ];
    events.iter().map(|(name, data)| format!("event: {name}\ndata: {data}\n\n")).collect()
}

fn say(text: &str) -> String {
    stream(json!({ "type": "text", "text": "" }), json!({ "type": "text_delta", "text": text }), "end_turn")
}

fn bash(command: &str) -> String {
    let input = json!({ "command": command, "description": "Create a marker file" }).to_string();
    // Every call its own id: a stubborn agent makes the same one again.
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let id = format!("toolu_{}", CALLS.fetch_add(1, Ordering::SeqCst));
    let block = json!({ "type": "tool_use", "id": id, "name": "Bash", "input": {} });
    stream(block, json!({ "type": "input_json_delta", "partial_json": input }), "tool_use")
}

/// The script: try the forbidden file, follow a redirect to the allowed one, then stop.
/// Requests without tools (titles, summaries) get a plain answer.
fn model(seen: &Mutex<Vec<String>>, request: &Value) -> common::Response {
    if request["tools"].as_array().is_none_or(|tools| tools.is_empty()) {
        return sse(say("Marker files"));
    }
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    seen.lock().unwrap().push(Value::Array(messages.clone()).to_string());
    // Claude Code can put a system message after the user's turn; answer the user's turn.
    let last = messages.iter().rev().find(|m| m["role"] != "system").cloned().unwrap_or(Value::Null);
    let result =
        last["content"].as_array().and_then(|blocks| blocks.iter().rev().find(|b| b["type"] == "tool_result").cloned());
    // A reply that breaks the schema rule, until Claude is told what to do instead.
    let asked = Value::Array(messages.clone()).to_string();
    if asked.contains("SCHEMA:") {
        return sse(say(if asked.contains(SCHEMA_REDIRECT) {
            "Members go in the bridge tables."
        } else {
            "Let's move members to a NoSQL store."
        }));
    }
    // Starts the work with a command, then, told the way to go, goes the other way until it's
    // stopped; a fresh session taking over does as it's told.
    let stubborn = asked.contains("STUBBORN") && !asked.contains("taking over work");
    sse(match result {
        _ if stubborn && !asked.contains("half.txt") => bash("echo half > half.txt"),
        Some(_) if stubborn => bash("touch violation.txt"),
        Some(result) if text_of(&result["content"]).contains("allowed.txt") => bash("touch allowed.txt"),
        Some(_) => say("Done."),
        // A fresh session taking over goes the way it's told.
        None if asked.contains("taking over work") => bash("touch allowed.txt"),
        None if text_of(&last["content"]).contains("MAKE_FILES") => bash("touch violation.txt"),
        None => say("Done."),
    })
}

struct Outcome {
    project: PathBuf,
    events: Vec<Value>,
    /// Every main-loop request Claude Code sent, as JSON.
    requests: Vec<String>,
    /// Claude's output, the event log and the requests, for failure messages.
    report: String,
}

const API_KEY: &str = "sk-ant-test-not-a-real-key";

/// A home directory with Claude Code's settings and reaper's rules, against the fake model.
struct Home {
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    claude_dir: PathBuf,
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Home {
    fn new(name: &str, settings: &str) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let shared = seen.clone();
        let url = common::server(move |path, request| {
            if path.ends_with("/chat/completions") {
                Some(common::json(watcher_reply(request, RULE_ID, "violation.txt")))
            } else if path.contains("count_tokens") {
                Some(common::json(json!({ "input_tokens": 100 })))
            } else if path.contains("/v1/messages") {
                Some(model(&shared, request))
            } else {
                None
            }
        });

        let root = common::sandbox(&format!("claude-{name}"));
        let home = root.join("home");
        let project = home.join("code/project");
        let claude_dir = home.join(".claude");
        let config = home.join(".config/reaper");
        for dir in [&project, &claude_dir, &config] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            config.join("rules.yaml"),
            format!(
                "watcher: {{ kind: openai, baseURL: \"{url}/v1\", model: m }}
{settings}
projects:
  e2e:
    paths: [\"~/code/project\"]
    rules:
      - id: {RULE_ID}
        tools: [bash]
        judge: {RULE_TEXT}
        redirect: {REDIRECT}
"
            ),
        )
        .unwrap();
        Self { root, home, project, claude_dir, url, seen }
    }

    /// `program` in the project, with nothing of yours: no settings, no PATH beyond a few tools.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let path = common::minimal_path(&self.root, &["bash", "sh", "touch", "git", "ls", "cat", "ps", "kill", "stty"]);
        let mut command = Command::new(program);
        command
            .current_dir(&self.project)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &path)
            .env("CLAUDE_CONFIG_DIR", &self.claude_dir)
            .env("ANTHROPIC_BASE_URL", &self.url)
            .env("ANTHROPIC_API_KEY", API_KEY)
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
        command
    }
}

fn run(name: &str, strikes: u32) -> Outcome {
    let h = Home::new(name, &format!("strikes: {strikes}"));
    let (project, home, seen) = (h.project.clone(), h.home.clone(), h.seen.clone());

    // Exactly what a user runs, pointed at the sandboxed settings by CLAUDE_CONFIG_DIR.
    let installed = h.command(env!("CARGO_BIN_EXE_reaper")).args(["install", "claude-code", "--yes"]).output().unwrap();
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    // --yes takes the skill too; with it in place, Claude still never hears the rule.
    assert!(h.claude_dir.join("skills/reaper-rule/SKILL.md").is_file());

    let output = h.command(claude()).args(["-p", TASK, "--permission-mode", "bypassPermissions"]).output().unwrap();
    assert!(
        output.status.success() || strikes == 1,
        "claude failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = seen.lock().unwrap().clone();
    let events = events(&home);
    let report = format!(
        "claude exited {}\nstdout: {}\nstderr: {}\nevents: {:#?}\nrequests: {}\nfirst request's messages: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        events,
        requests.len(),
        requests.first().map(|r| r.chars().take(3000).collect::<String>()).unwrap_or_default()
    );
    // A script's run (`-p`) is never reaped, so no watcher is left beside it.
    let runs = home.join(".local/state/reaper/runs");
    assert!(fs::read_dir(&runs).map_or(true, |mut d| d.next().is_none()), "a watcher for a -p run");
    Outcome { project, events, requests, report }
}

#[test]
fn a_violation_is_denied_and_the_agent_only_hears_the_redirect() {
    let o = run("redirect", 3);

    assert!(!o.project.join("violation.txt").exists(), "the violating command must never run");
    assert!(o.project.join("allowed.txt").exists(), "Claude follows the redirect\n{}", o.report);

    assert!(event(&o.events, "agent.spawned").is_some(), "the session was announced: {:#?}", o.events);
    let killed = event(&o.events, "agent.killed").expect("a kill was logged");
    assert_eq!(
        (killed["rule"].as_str(), killed["tier"].as_str(), killed["final"].as_bool()),
        (Some(RULE_ID), Some("watcher"), Some(false))
    );
    assert!(killed["attempt"].as_str().unwrap().contains("touch violation.txt"));
    let redirected = event(&o.events, "agent.redirected").expect("a redirect was logged")["redirect"].clone();
    assert!(redirected.as_str().is_some_and(|r| r.starts_with(REDIRECT)), "{redirected}");

    // The denial Claude saw carries the redirect, and nothing anywhere names the rule.
    assert!(o.requests.iter().any(|r| r.contains(REDIRECT)), "{:#?}", o.requests);
    for request in &o.requests {
        assert!(!request.contains(RULE_TEXT) && !request.contains(RULE_ID), "Claude saw the rule: {request}");
    }
}

#[test]
fn the_strike_limit_stops_the_session() {
    let o = run("strikes", 1);

    assert!(!o.project.join("violation.txt").exists());
    assert!(!o.project.join("allowed.txt").exists(), "nothing runs after the stop");
    assert_eq!(
        event(&o.events, "agent.killed").unwrap_or_else(|| panic!("no kill logged\n{}", o.report))["final"],
        true
    );
    assert!(event(&o.events, "agent.died").is_some());
    assert_eq!(o.requests.len(), 1, "no model call after the stop: {:#?}", o.requests);
}

/// Claude Code in a terminal, started as usual, with the hooks `reaper install` put in place.
/// The human asks once and walks away; the watcher the hook left reaps the idle session, and
/// `claude --resume` brings it back with its conversation, until it sits idle again.
#[test]
fn an_idle_claude_session_is_reaped_and_resumes_where_it_left_off() {
    reaped_and_resumed("reap", false);
}

/// The same, with a launcher between the shell and Claude Code: a script that runs it and,
/// like many, would die of the signal without passing it on. Claude Code is what gets reaped,
/// so the launcher lives to finish.
#[test]
fn a_claude_started_by_a_launcher_is_reaped_itself() {
    reaped_and_resumed("launcher", true);
}

/// Claude Code in a terminal with reaper installed, for someone who has used both before: no
/// first-run questions.
fn returning(h: &Home) {
    let installed = h.command(env!("CARGO_BIN_EXE_reaper")).args(["install", "claude-code", "--yes"]).output().unwrap();
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let project = h.project.to_string_lossy().into_owned();
    let config = json!({
        "hasCompletedOnboarding": true,
        "theme": "dark",
        "customApiKeyResponses": { "approved": [&API_KEY[API_KEY.len() - 20..]], "rejected": [] },
        "projects": { project: { "hasTrustDialogAccepted": true, "hasCompletedProjectOnboarding": true } },
    });
    fs::write(h.claude_dir.join(".claude.json"), config.to_string()).unwrap();
}

fn reaped_and_resumed(name: &str, launcher: bool) {
    use std::os::unix::fs::PermissionsExt;
    if !has_terminal() {
        return;
    }
    let h = Home::new(name, "reap: { idle: 1s }");
    returning(&h);
    let start = if launcher {
        let script = h.root.join("launch-claude");
        fs::write(&script, format!("#!/bin/sh\n'{}' \"$@\"\necho launcher finished\n", claude().display())).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    } else {
        claude()
    };
    let open = |args: &str| {
        let terminal = Terminal::open(h.command(SCRIPT), &format!("'{}' {args}; echo claude exited", start.display()));
        terminal.screen.wait_for("claude exited", 1);
        terminal.close()
    };

    let task = "SAY_DONE: tell me when you are ready.";
    let first = open(&format!("'{task}'"));
    let session = events(&h.home)[0]["agent"].as_str().unwrap().to_string();
    let again = open(&format!("--resume {session}"));
    let runs = h.home.join(".local/state/reaper/runs");
    common::wait_until("the watchers to leave", || fs::read_dir(&runs).is_ok_and(|mut d| d.next().is_none()));

    let events = events(&h.home);
    let prompts: Vec<&str> = events.iter().filter_map(|e| e["prompt"].as_str()).collect();
    assert_eq!(prompts, [task], "{events:#?}");
    let kinds = kinds(&events);
    // Claude Code ends its session as it's reaped; the reap, recorded once it has gone, wins.
    assert_eq!(
        kinds,
        ["agent.spawned", "agent.idle", "agent.ended", "agent.reaped", "agent.spawned", "agent.ended", "agent.reaped"],
        "{events:#?}\n{first}\n{again}"
    );
    assert!(events.iter().all(|e| e["agent"] == session), "resumed as the same session");
    for reaped in events.iter().filter(|e| e["type"] == "agent.reaped") {
        assert_eq!(reaped["reason"], "idle");
        assert!(reaped["idle"].as_u64().unwrap() >= 1000, "{reaped}");
        assert!(
            fs::read_to_string(reaped["file"].as_str().unwrap()).unwrap().contains(task),
            "the transcript is saved"
        );
    }
    assert_eq!(h.seen.lock().unwrap().len(), 1, "resuming never sends the first prompt again");
    if launcher {
        assert!(first.contains("launcher finished") && again.contains("launcher finished"), "{first}\n{again}");
    }
}

/// A session in a terminal that keeps breaking a rule is killed at its third strike, and
/// `/clear` resets it: the fresh session takes the work over (the human's task and the way to
/// go, never the rule or what was tried) and, once the human says so, does it right.
#[test]
fn a_stopped_session_cleared_hands_its_work_to_the_fresh_one() {
    if !has_terminal() {
        return;
    }
    let h = Home::new("take-over", "strikes: 3");
    returning(&h);
    let mut terminal =
        Terminal::open(h.command(SCRIPT), &format!("'{}' '{STUBBORN}'; echo claude exited", claude().display()));
    terminal.screen.wait_for("/clear starts a fresh one", 1);
    terminal.press(b"/clear\r");
    terminal.screen.wait_for("this session takes over", 1);
    terminal.press(b"go on\r");
    common::wait_until("the fresh session to do it", || h.project.join("allowed.txt").exists());
    common::wait_until("its turn to end", || {
        events(&h.home).iter().filter(|e| e["type"] == "agent.idle").any(|e| e["agent"] != events(&h.home)[0]["agent"])
    });
    terminal.press(b"/exit\r");
    terminal.screen.wait_for("claude exited", 1);
    let screen = terminal.close();

    assert!(!h.project.join("violation.txt").exists(), "{screen}");
    let events = events(&h.home);
    let spawned: Vec<&Value> = events.iter().filter(|e| e["type"] == "agent.spawned").collect();
    assert_eq!(spawned.len(), 2, "{events:#?}");
    assert_eq!(spawned[1]["after"], spawned[0]["agent"], "it says whom it took over from: {events:#?}");
    assert!(event(&events, "agent.died").is_some_and(|d| d["agent"] == spawned[0]["agent"]));
    let of = |kind: &str| events.iter().filter(|e| e["type"] == kind && e["agent"] == spawned[0]["agent"]).count();
    assert_eq!(
        (of("agent.killed"), of("agent.redirected")),
        (3, 2),
        "redirected twice, killed at the third: {events:#?}"
    );
    let last = events.iter().rfind(|e| e["type"] == "agent.killed").unwrap();
    assert_eq!((last["strike"].as_u64(), last["final"].as_bool()), (Some(3), Some(true)));

    let requests = h.seen.lock().unwrap();
    let first = requests.iter().find(|r| r.contains("taking over work")).expect("the fresh session's request");
    assert!(first.contains(STUBBORN) && first.contains(REDIRECT), "{first}");
    for secret in [RULE_ID, RULE_TEXT, "violation.txt"] {
        assert!(!first.contains(secret), "the fresh session heard {secret:?}: {first}");
    }
    let runs = h.home.join(".local/state/reaper/runs");
    common::wait_until("the watcher to leave", || fs::read_dir(&runs).is_ok_and(|mut d| d.next().is_none()));
}

/// Claude Code in a git worktree of its own (`--worktree`): a session that starts the work and
/// then keeps breaking a rule is stopped, and after `/clear` the fresh one is told where the
/// half-done work is: the worktree, its branch, and the file a command made there.
#[test]
fn a_stopped_session_in_a_worktree_hands_on_what_is_half_done_there() {
    if !has_terminal() {
        return;
    }
    let h = Home::new("take-over-worktree", "strikes: 3");
    returning(&h);
    fs::write(h.project.join("README.md"), "project\n").unwrap();
    common::git(&h.project, &["init", "-q", "-b", "main"]);
    common::git(&h.project, &["add", "."]);
    common::git(&h.project, &["commit", "-q", "-m", "start"]);
    let line = format!("'{}' --worktree fix '{STUBBORN}'; echo claude exited", claude().display());
    let mut terminal = Terminal::open(h.command(SCRIPT), &line);
    terminal.screen.wait_for("/clear starts a fresh one", 1);
    terminal.press(b"/clear\r");
    terminal.screen.wait_for("this session takes over", 1);
    terminal.press(b"go on\r");
    common::wait_until("the fresh session's turn to end", || {
        let events = events(&h.home);
        events.iter().filter(|e| e["type"] == "agent.idle").any(|e| e["agent"] != events[0]["agent"])
    });
    terminal.press(b"/exit\r");
    terminal.screen.wait_for("claude exited", 1);
    let screen = terminal.close();

    let events = events(&h.home);
    let spawned: Vec<&Value> = events.iter().filter(|e| e["type"] == "agent.spawned").collect();
    assert_eq!(spawned.len(), 2, "{events:#?}\n{screen}");
    let tree = spawned[0]["dir"].as_str().unwrap();
    assert!(tree != h.project.to_str().unwrap() && Path::new(tree).join("half.txt").exists(), "{events:#?}");
    eprintln!("worktree: {tree}\nafter /clear: {}", spawned[1]["dir"]);

    let requests = h.seen.lock().unwrap();
    let first = requests.iter().find(|r| r.contains("taking over work")).expect("the fresh session's request");
    assert!(first.contains(&format!("Not committed yet in {tree}, a git worktree on branch")), "{first}");
    assert!(first.contains("- new half.txt") && first.contains(STUBBORN) && first.contains(REDIRECT), "{first}");
    for secret in [RULE_ID, RULE_TEXT, "violation.txt"] {
        assert!(!first.contains(secret), "the fresh session heard {secret:?}: {first}");
    }
}

/// What Claude says is judged too, by rules that list `message`: a reply that breaks one keeps
/// Claude working, with the redirect, instead of ending the turn.
#[test]
fn a_reply_that_breaks_a_message_rule_keeps_claude_working_with_the_redirect() {
    let rules = format!(
        "rules:\n  - id: bridge-schema\n    tools: [message]\n    match: {{ text: NoSQL }}\n    redirect: {SCHEMA_REDIRECT}\n"
    );
    let h = Home::new("message", &rules);
    let installed = h.command(env!("CARGO_BIN_EXE_reaper")).args(["install", "claude-code", "--yes"]).output().unwrap();
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let output = h
        .command(claude())
        .args(["-p", "SCHEMA: propose where members are stored.", "--permission-mode", "bypassPermissions"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
    let events = events(&h.home);
    let killed = event(&events, "agent.killed").unwrap_or_else(|| panic!("the reply was judged: {events:#?}\n{said}"));
    assert_eq!((killed["tool"].as_str(), killed["rule"].as_str()), (Some("message"), Some("bridge-schema")));
    assert_eq!(event(&events, "agent.redirected").expect("and redirected")["redirect"], SCHEMA_REDIRECT);
    let requests = h.seen.lock().unwrap().clone();
    assert!(requests.last().is_some_and(|r| r.contains(SCHEMA_REDIRECT)), "Claude heard the redirect: {requests:#?}");
    assert!(said.contains("bridge tables"), "and its final answer follows it: {said}");
    assert!(requests.iter().all(|r| !r.contains("bridge-schema")), "the rule is never named");
}
