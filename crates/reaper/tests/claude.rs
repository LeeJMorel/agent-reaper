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
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use common::{SCRIPT, Terminal, event, events, has_terminal, sse, text_of, watcher_reply};
use serde_json::{Value, json};

const TASK: &str = "MAKE_FILES: set up the marker files for this project.";
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
    let block = json!({ "type": "tool_use", "id": format!("toolu_{}", command.len()), "name": "Bash", "input": {} });
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
    sse(match result {
        Some(result) if text_of(&result["content"]).contains("allowed.txt") => bash("touch allowed.txt"),
        Some(_) => say("Done."),
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
    assert_eq!(event(&o.events, "agent.redirected").expect("a redirect was logged")["redirect"], REDIRECT);

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

/// Claude Code in a terminal, started by `reaper claude` with no `reaper install`: the hooks
/// come with the session. The human asks once and walks away; reaper shuts the idle session
/// down, and enter brings it back (`claude --resume`) with its conversation, until it sits
/// idle again and ctrl-d leaves.
#[test]
fn an_idle_claude_session_is_reaped_and_resumes_where_it_left_off() {
    if !has_terminal() {
        return;
    }
    let h = Home::new("reap", "reap: { idle: 1s }");
    // A returning user, as far as Claude Code's first-run questions go.
    let project = h.project.to_string_lossy().into_owned();
    let config = json!({
        "hasCompletedOnboarding": true,
        "theme": "dark",
        "customApiKeyResponses": { "approved": [&API_KEY[API_KEY.len() - 20..]], "rejected": [] },
        "projects": { project: { "hasTrustDialogAccepted": true, "hasCompletedProjectOnboarding": true } },
    });
    fs::write(h.claude_dir.join(".claude.json"), config.to_string()).unwrap();

    let task = "SAY_DONE: tell me when you are ready.";
    let line = format!("exec '{}' claude '{task}'", env!("CARGO_BIN_EXE_reaper"));
    let mut script = h.command(SCRIPT);
    script.env("SHELL", "/bin/sh").env("TERM", "xterm-256color").env("REAPER_CLAUDE", claude());
    let mut terminal = Terminal::open(script, &line);
    terminal.screen.wait_for("press enter to resume", 1);
    terminal.press(b"\n");
    terminal.screen.wait_for("press enter to resume", 2);
    terminal.press(b"\x04");
    let screen = terminal.close();

    let events = events(&h.home);
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        ["agent.spawned", "agent.idle", "agent.reaped", "agent.spawned", "agent.reaped"],
        "{events:#?}\n{screen}"
    );
    let agent = &events[0]["agent"];
    assert!(events.iter().all(|e| &e["agent"] == agent), "resumed as the same session");
    for reaped in events.iter().filter(|e| e["type"] == "agent.reaped") {
        assert_eq!(reaped["reason"], "idle");
        assert!(reaped["idle"].as_u64().unwrap() >= 1000, "{reaped}");
        assert!(
            fs::read_to_string(reaped["file"].as_str().unwrap()).unwrap().contains(task),
            "the transcript is saved"
        );
    }
    assert_eq!(h.seen.lock().unwrap().len(), 1, "resuming never sends the first prompt again");
    assert!(!h.claude_dir.join("settings.json").exists(), "the user's settings were never touched");
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
