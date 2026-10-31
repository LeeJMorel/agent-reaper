//! End to end with pi, exactly as users run it: the real `reaper run`, the pinned pi binary
//! it fetches (checksum-verified, kept in target/ after the first run), the embedded
//! extension, and every decision made by the reaper binary. Only the model is fake: a
//! local OpenAI-compatible server playing both the coding model and the watcher. Node and
//! npm are not on PATH.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{SCRIPT, Terminal, event, events, has_terminal, sse, text_of, wait_until, watcher_reply};
use serde_json::{Value, json};

const TASK: &str = "MAKE_FILES: set up the marker files for this project.";
const RULE_ID: &str = "no-violation-file";
const RULE_TEXT: &str = "Creating a file named violation.txt.";
const REDIRECT: &str = "Create the marker file allowed.txt in the project root.";
const SCHEMA_REDIRECT: &str = "Follow docs/schema.md for every entity and relation.";

/// One run at a time: the first one fetches pi, the rest reuse it.
static SERIAL: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct Seen {
    /// Every request the coding model got, as JSON.
    agent: Vec<String>,
    /// The system prompt of every watcher request.
    watcher: Vec<String>,
    /// What every watcher request asked: the task, then the action.
    asks: Vec<String>,
}

fn completion(text: Option<&str>, command: Option<&str>) -> String {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let (delta, finish) = match command {
        Some(command) => {
            let id = format!("call_{}", CALLS.fetch_add(1, Ordering::SeqCst));
            let arguments = json!({ "command": command }).to_string();
            let call = json!({ "index": 0, "id": id, "type": "function", "function": { "name": "bash", "arguments": arguments } });
            (json!({ "role": "assistant", "tool_calls": [call] }), "tool_calls")
        }
        None => (json!({ "role": "assistant", "content": text.unwrap_or("Done.") }), "stop"),
    };
    let chunk = |choice: Value| json!({ "id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m", "choices": [choice] });
    let first = chunk(json!({ "index": 0, "delta": delta, "finish_reason": null }));
    let mut last = chunk(json!({ "index": 0, "delta": {}, "finish_reason": finish }));
    last["usage"] = json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 });
    format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n")
}

/// The script: try the forbidden file, follow a redirect to the allowed one, then stop.
fn model(seen: &Mutex<Seen>, request: &Value) -> common::Response {
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let system = messages.first().map(|m| text_of(&m["content"])).unwrap_or_default();
    if system.starts_with("You monitor a coding agent") {
        let ask = messages.last().map(|m| text_of(&m["content"])).unwrap_or_default();
        let mut seen = seen.lock().unwrap();
        seen.watcher.push(system);
        seen.asks.push(ask);
        drop(seen);
        return common::json(watcher_reply(request, RULE_ID, "violation.txt"));
    }
    seen.lock().unwrap().agent.push(Value::Array(messages.clone()).to_string());
    let last = messages.last().cloned().unwrap_or(Value::Null);
    let said = text_of(&last["content"]);
    let task = messages.iter().find(|m| m["role"] == "user").map(|m| text_of(&m["content"])).unwrap_or_default();
    sse(match () {
        // A stuck agent: the same command, whatever happens.
        _ if task.contains("LOOP") => completion(None, Some("echo ran >> runs.txt; false")),
        // A reply that breaks the schema rule, until it's told what to do instead.
        _ if task.contains("SCHEMA") && said.contains(SCHEMA_REDIRECT) => {
            completion(Some("Members go in the bridge tables."), None)
        }
        _ if task.contains("SCHEMA") => completion(Some("Let's move members to a NoSQL store."), None),
        _ if last["role"] == "tool" => completion(Some("Done."), None),
        _ if said.contains("allowed.txt") => completion(None, Some("touch allowed.txt")),
        _ if said.contains("MAKE_FILES") => completion(None, Some("touch violation.txt")),
        _ => completion(Some("Done."), None),
    })
}

struct Outcome {
    project: PathBuf,
    events: Vec<Value>,
    /// Every pi session file, concatenated.
    sessions: String,
    agent_requests: Vec<String>,
    watcher_systems: Vec<String>,
    watcher_asks: Vec<String>,
}

/// A home directory with pi set up against the fake model, and reaper's rules.
struct Home {
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    pi_dir: PathBuf,
    seen: Arc<Mutex<Seen>>,
}

impl Home {
    fn new(name: &str, settings: &str) -> Self {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = seen.clone();
        let url =
            common::server(move |path, request| path.ends_with("/chat/completions").then(|| model(&shared, request)));

        let root = common::sandbox(&format!("pi-{name}"));
        let home = root.join("home");
        let project = home.join("code/project");
        let pi_dir = home.join(".pi/agent");
        let config = home.join(".config/reaper");
        for dir in [&project, &pi_dir, &config] {
            fs::create_dir_all(dir).unwrap();
        }
        let models = json!({ "providers": { "mock": { "baseUrl": format!("{url}/v1"), "api": "openai-completions", "apiKey": "x", "models": [{ "id": "m", "name": "m" }] } } });
        fs::write(pi_dir.join("models.json"), models.to_string()).unwrap();
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
        Self { root, home, project, pi_dir, seen }
    }

    /// `program` as the human would start it in the project, with nothing else on PATH.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.project)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", common::minimal_path(&self.root, &["bash", "sh", "touch", "git", "ls", "cat"]))
            .env("PI_CODING_AGENT_DIR", &self.pi_dir)
            // The pinned pi, fetched for real by the first run and kept between test runs.
            .env("REAPER_DATA", PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reaper-data"));
        command
    }

    fn sessions(&self) -> String {
        let mut sessions = String::new();
        let mut stack = vec![self.pi_dir.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "jsonl") {
                    sessions.push_str(&fs::read_to_string(&path).unwrap());
                }
            }
        }
        sessions
    }
}

fn run(name: &str, strikes: u32) -> Outcome {
    run_task(name, &format!("strikes: {strikes}"), TASK)
}

fn run_task(name: &str, settings: &str, task: &str) -> Outcome {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let h = Home::new(name, settings);
    let output = h
        .command(env!("CARGO_BIN_EXE_reaper"))
        .args(["run", "--", "--model", "mock/m", "--offline", "-p", task])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "reaper run failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The warmup is handed off without waiting, so it may land just after pi exits.
    if task == TASK {
        wait_until("the watcher warmup", || h.seen.lock().unwrap().asks.iter().any(|a| a.ends_with("(none yet)")));
    }
    let seen = h.seen.lock().unwrap();
    Outcome {
        project: h.project.clone(),
        events: events(&h.home),
        sessions: h.sessions(),
        agent_requests: seen.agent.clone(),
        watcher_systems: seen.watcher.clone(),
        watcher_asks: seen.asks.clone(),
    }
}

#[test]
fn a_violation_is_blocked_cut_from_history_and_redirected() {
    let o = run("redirect", 3);

    assert!(!o.project.join("violation.txt").exists(), "the violating command must never run");
    assert!(o.project.join("allowed.txt").exists(), "the agent follows the redirect");

    let killed = event(&o.events, "agent.killed").expect("a kill was logged");
    assert_eq!(killed["rule"], RULE_ID);
    assert_eq!(killed["tier"], "watcher");
    assert_eq!(killed["final"], false);
    assert_eq!(killed["attempt"], r#"{"command":"touch violation.txt"}"#);
    assert_eq!(event(&o.events, "agent.redirected").expect("a redirect was logged")["redirect"], REDIRECT);
    assert_eq!(event(&o.events, "agent.spawned").expect("the agent was announced")["project"], "e2e");

    // What the coding model saw after the kill: the redirect, and no trace of the attempt.
    let after: Vec<&String> = o.agent_requests.iter().skip(1).collect();
    assert!(!after.is_empty() && after[0].contains(REDIRECT), "{:#?}", o.agent_requests);
    for request in &after {
        assert!(!request.contains("violation.txt"), "attempt leaked into a later request: {request}");
        assert!(!request.contains("Interrupted"), "block reason leaked: {request}");
    }
    for request in &o.agent_requests {
        assert!(!request.contains(RULE_TEXT) && !request.contains(RULE_ID), "the agent saw the rule: {request}");
    }

    // The session record keeps the dead attempt, labelled, hidden by context edits.
    assert!(o.sessions.contains("touch violation.txt"), "attempt kept in the session record");
    assert!(o.sessions.contains(&format!("reaper: {RULE_ID}")), "attempt labelled");
    assert!(o.sessions.contains("context_edit"), "attempt hidden from the model by context edits");

    // Every watcher call starts with the same bytes, so its prefix stays cached.
    assert!(o.watcher_systems.len() >= 2);
    assert!(o.watcher_systems.iter().all(|s| s == &o.watcher_systems[0]));
    // The warmup sent that prefix and the task ahead of the first check, byte for byte.
    let task = |ask: &str| ask.rsplit_once("\n\nNext action: ").map(|(task, _)| task.to_string());
    let warm = o.watcher_asks.iter().find(|a| a.ends_with("(none yet)")).unwrap();
    let check = o.watcher_asks.iter().find(|a| a.contains("touch violation.txt")).expect("a real check");
    assert!(task(warm).is_some() && task(warm) == task(check), "{warm:?} vs {check:?}");
}

#[test]
fn a_stuck_agent_is_redirected_then_stopped() {
    let o = run_task("loop", "strikes: 3\nrepeats: 3", "LOOP: get the build green.");
    let runs = fs::read_to_string(o.project.join("runs.txt")).unwrap_or_default();
    assert_eq!(runs.lines().count(), 2, "the third identical run never happens");
    let kills: Vec<&Value> = o.events.iter().filter(|e| e["type"] == "agent.killed").collect();
    assert_eq!(kills.len(), 3, "{:#?}", o.events);
    assert!(kills.iter().all(|k| k["rule"] == "reaper.loop" && k["tier"] == "pattern"));
    assert_eq!(kills.iter().map(|k| k["strike"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(event(&o.events, "agent.died").expect("stopped at the strike limit")["reason"], "strikes");
    let redirects = o.events.iter().filter(|e| e["type"] == "agent.redirected").count();
    assert_eq!(redirects, 2);
    // The agent heard only the positive instruction.
    let last = o.agent_requests.last().unwrap();
    assert!(last.contains("Try a different approach to the task.") && !last.contains("reaper.loop"), "{last}");
}

#[test]
fn a_reply_that_breaks_a_message_rule_is_cut_and_redirected() {
    let rules = format!(
        "rules:\n  - id: bridge-schema\n    tools: [message]\n    match: {{ text: NoSQL }}\n    redirect: {SCHEMA_REDIRECT}\n"
    );
    let o = run_task("message", &rules, "SCHEMA: propose where members are stored.");
    let killed = event(&o.events, "agent.killed").expect("the reply was judged");
    assert_eq!((killed["tool"].as_str(), killed["rule"].as_str()), (Some("message"), Some("bridge-schema")));
    assert_eq!(event(&o.events, "agent.redirected").expect("and redirected")["redirect"], SCHEMA_REDIRECT);
    assert_eq!(o.agent_requests.len(), 2, "one more try, with the redirect");
    let retry = &o.agent_requests[1];
    assert!(retry.contains(SCHEMA_REDIRECT) && !retry.contains("NoSQL"), "the bad reply is cut: {retry}");
    assert!(o.agent_requests.iter().all(|r| !r.contains("bridge-schema")), "the rule is never named");
}

#[test]
fn hitting_the_strike_limit_kills_the_agent_for_good() {
    let o = run("strikes", 1);

    assert!(!o.project.join("violation.txt").exists());
    assert!(!o.project.join("allowed.txt").exists());
    assert_eq!(event(&o.events, "agent.killed").expect("a kill was logged")["final"], true);
    assert!(event(&o.events, "agent.died").is_some());
    assert!(event(&o.events, "agent.redirected").is_none());
    assert_eq!(o.agent_requests.len(), 1, "no model call after death");
}

fn open(h: &Home, task: &str) -> Terminal {
    let line = format!("exec '{}' run -- --model mock/m --offline '{task}'", env!("CARGO_BIN_EXE_reaper"));
    Terminal::open(h.command(SCRIPT), &line)
}

/// An interactive session: the human asks once, then walks away. reaper shuts the idle
/// session down, and enter brings it back with its conversation, ready to go on, until it
/// sits idle again and ctrl-d leaves.
#[test]
fn an_idle_session_is_reaped_and_resumes_where_it_left_off() {
    if !has_terminal() {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let h = Home::new("reap", "reap: { idle: 1s }");
    let task = "SAY_DONE: tell me when you are ready.";
    let mut terminal = open(&h, task);
    terminal.screen.wait_for("press enter to resume", 1);
    terminal.press(b"\n");
    terminal.screen.wait_for("press enter to resume", 2);
    terminal.press(b"\x04");
    terminal.close();

    let events = events(&h.home);
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["agent.spawned", "agent.idle", "agent.reaped", "agent.spawned", "agent.reaped"], "{events:#?}");
    let agent = &events[0]["agent"];
    assert!(events.iter().all(|e| &e["agent"] == agent), "resumed as the same agent");
    for reaped in events.iter().filter(|e| e["type"] == "agent.reaped") {
        assert!(reaped["idle"].as_u64().unwrap() >= 1000, "{reaped}");
        let file = reaped["file"].as_str().unwrap();
        assert!(fs::read_to_string(file).unwrap().contains(task), "the conversation is saved in {file}");
    }
    let seen = h.seen.lock().unwrap();
    assert_eq!(seen.agent.len(), 1, "resuming never sends the first prompt again");
    assert!(h.sessions().contains(task));
    let runs = h.home.join(".local/state/reaper/runs");
    assert!(fs::read_dir(&runs).map_or(true, |mut d| d.next().is_none()), "no note left behind");
}

/// Two sessions where there's room for one: the second arriving makes the first, idle and
/// waiting on the human, reap itself to make room. The new one keeps running.
#[test]
fn a_new_session_past_the_cap_makes_the_longest_idle_one_make_room() {
    if !has_terminal() {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let h = Home::new("crowded", "reap: { idle: 1h, live: 1 }");
    let kinds = |agent: &Value| -> Vec<String> {
        events(&h.home)
            .iter()
            .filter(|e| &e["agent"] == agent)
            .map(|e| e["type"].as_str().unwrap().to_string())
            .collect()
    };

    let mut first = open(&h, "SAY_DONE: the first one.");
    wait_until("the first session to go idle", || event(&events(&h.home), "agent.idle").is_some());
    let a = event(&events(&h.home), "agent.spawned").unwrap()["agent"].clone();

    let mut second = open(&h, "SAY_DONE: the second one.");
    first.screen.wait_for("press enter to resume", 1);
    assert!(first.screen.text().contains("too many sessions were running"), "{}", first.screen.text());
    first.press(b"\x04");
    first.close();

    let reaped = event(&events(&h.home), "agent.reaped").expect("the first made room").clone();
    assert_eq!((&reaped["agent"], reaped["reason"].as_str()), (&a, Some("crowded")));
    assert_eq!(kinds(&a), ["agent.spawned", "agent.idle", "agent.reaped"]);

    let b = events(&h.home).iter().find(|e| e["type"] == "agent.spawned" && e["agent"] != a).unwrap()["agent"].clone();
    wait_until("the second session to go idle", || kinds(&b).contains(&"agent.idle".to_string()));
    second.press(b"\x04"); // pi's own quit
    let screen = second.close();
    assert!(!screen.contains("press enter to resume"), "the newcomer was never asked to go:\n{screen}");
    assert_eq!(kinds(&b), ["agent.spawned", "agent.idle", "agent.ended"], "quit, not reaped");
    let live = h.home.join(".local/state/reaper/live");
    assert!(fs::read_dir(&live).map_or(true, |mut d| d.next().is_none()), "both sessions left the live list");
}

/// The pinned pi, fetched by `reaper run` if no test has yet.
fn pinned_pi(h: &Home) -> PathBuf {
    let output = h.command(env!("CARGO_BIN_EXE_reaper")).args(["run", "--", "--version"]).output().unwrap();
    let text = String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
    let version = text.split_whitespace().find(|w| w.starts_with(|c: char| c.is_ascii_digit())).expect(&text);
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("reaper-data/pi/{version}/pi/pi"))
}

fn install_pi(h: &Home) {
    let installed = h.command(env!("CARGO_BIN_EXE_reaper")).args(["install", "pi", "--yes"]).output().unwrap();
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    assert!(h.pi_dir.join("extensions/reaper/index.ts").is_file());
}

/// pi started without `reaper run`: the extension `reaper install pi` put where pi looks guards
/// it all the same, with no reaper on PATH. Under `reaper run` the installed copy stands
/// aside, so nothing is checked twice.
#[test]
fn an_installed_extension_guards_pi_however_it_is_started() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let h = Home::new("installed", "strikes: 3");
    let pi = pinned_pi(&h);
    install_pi(&h);

    let output = h.command(&pi).args(["--model", "mock/m", "--offline", "-p", TASK]).output().unwrap();
    assert!(output.status.success(), "pi failed: {}", String::from_utf8_lossy(&output.stderr));
    assert!(!h.project.join("violation.txt").exists(), "the violating command must never run");
    assert!(h.project.join("allowed.txt").exists(), "the agent follows the redirect");
    let first = events(&h.home);
    assert_eq!(event(&first, "agent.killed").expect("a kill was logged")["rule"], RULE_ID);
    assert!(event(&first, "agent.redirected").is_some());

    fs::remove_file(h.project.join("allowed.txt")).unwrap();
    let output = h
        .command(env!("CARGO_BIN_EXE_reaper"))
        .args(["run", "--", "--model", "mock/m", "--offline", "-p", TASK])
        .output()
        .unwrap();
    assert!(output.status.success(), "reaper run failed: {}", String::from_utf8_lossy(&output.stderr));
    assert!(h.project.join("allowed.txt").exists());
    let events = events(&h.home);
    let count = |kind: &str| events.iter().filter(|e| e["type"] == kind).count();
    assert_eq!((count("agent.spawned"), count("agent.killed")), (2, 2), "each session checked once: {events:#?}");
}

/// An installed pi, started by a script of the human's and reaped while they're away. No
/// `reaper run` is there to offer the resume, so it says what to type, and that brings it back.
#[test]
fn a_reaped_pi_started_by_a_script_says_how_to_bring_it_back() {
    use std::os::unix::fs::PermissionsExt;
    if !has_terminal() {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let h = Home::new("installed-reap", "reap: { idle: 1s }");
    let pi = pinned_pi(&h);
    install_pi(&h);
    let script = common::minimal_path(&h.root, &[]).join("mypi");
    fs::write(&script, format!("#!/bin/sh\n'{}' --model mock/m --offline \"$@\"\n", pi.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let task = "SAY_DONE: tell me when you are ready.";
    let first = Terminal::open(h.command(SCRIPT), &format!("mypi '{task}'"));
    first.screen.wait_for("to pick it up again: ", 1);
    let screen = first.close();
    let later = screen.rsplit("to pick it up again: ").next().unwrap().lines().next().unwrap().trim().to_string();
    let session = events(&h.home)[0]["agent"].as_str().unwrap().to_string();
    assert_eq!(later, format!("mypi --session {session}"), "{screen}");

    let again = Terminal::open(h.command(SCRIPT), &later);
    again.screen.wait_for("to pick it up again: ", 1);
    again.close();
    let events = events(&h.home);
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["agent.spawned", "agent.idle", "agent.reaped", "agent.spawned", "agent.reaped"], "{events:#?}");
    assert!(events.iter().all(|e| e["agent"] == events[0]["agent"]), "resumed as the same agent");
    assert_eq!(h.seen.lock().unwrap().agent.len(), 1, "resuming never sends the first prompt again");
}
