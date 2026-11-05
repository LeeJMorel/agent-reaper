//! `reaper hook claude-code`: Claude Code runs this once per hook event, with the event as
//! JSON on stdin; the reply on stdout is Claude Code's hook output. What reaper can do here
//! is narrower than in pi: a call is blocked and the agent gets only the positive redirect,
//! but Claude Code hooks can't remove the attempt from the conversation. Claude's replies are
//! judged when it stops, by the rules that list `message`.

mod inject;
pub mod install;
pub mod watch;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use reaper_core::rules::MESSAGE;

use crate::engine::{CARRY_ON, Check, Decision, Engine, FOR_THE_PERSON};

pub const HOST: &str = "claude-code";

/// What the human sees when a session is stopped. Claude sees it too if the conversation
/// continues, so it names no rule.
const STOPPED: &str = "reaper stopped this session. The reaper console shows why.";
/// For the human, in a terminal, when its work waits for a fresh session to take it over.
const HANDED_ON: &str = "reaper stopped this session. /clear starts a fresh one that takes its work over.";
/// The same in a chat (the editor's, an app's), where carrying on means staying in it.
const COMPACT_ON: &str = "reaper stopped this session. /compact, and it carries on here with a clean slate.";
/// For the human, when a session left to run by itself was started over without them.
const CARRIED_ON: &str =
    "reaper: this session kept breaking rules, so it was started over with a clean slate. It carries on by itself.";
/// The same, when reaper could compact it too: it carries on once that's done.
const COMPACTING: &str = "reaper: this session kept breaking rules, so it's being compacted and started over with a clean slate. It carries on by itself.";
/// Shown in a terminal's fresh session: Claude has the handoff, and starts when the human says.
const TAKING_OVER: &str = "reaper: this session takes over the stopped one's work. Send any message to start it.";
/// Shown once a stopped session is compacted: the same, in the same chat.
const CARRYING_ON: &str = "reaper: this session carries on its work with a clean slate. Send any message to start it.";
/// A `/clear` ends one session and starts the next straight away; a note older than this is stale.
const CLEARING: Duration = Duration::from_secs(60);

fn deny(reason: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason } })
}

/// Denied, and the agent making the call stops. `note` is for the human: how to carry it on.
fn stop(note: Option<&str>) -> Value {
    let mut output = deny("Stopped.");
    output["continue"] = false.into();
    output["stopReason"] = STOPPED.into();
    if let Some(note) = note {
        output["systemMessage"] = note.into();
    }
    output
}

/// How a session ends when reaper stops it.
enum Ending {
    /// Stopped; with what the human is to type to carry it on, when its work waits for them.
    Stop(Option<&'static str>),
    /// Left to run by itself, it carries on at once with a clean record, and hears this.
    CarryOn(String),
}

/// How this Claude Code was started: `cli` in a terminal, `claude-vscode` in the editor,
/// `sdk-…` by a script.
fn entrypoint() -> String {
    std::env::var("CLAUDE_CODE_ENTRYPOINT").unwrap_or_default()
}

/// The session a `/clear` just ended in this Claude Code, if its work waits to be taken over.
fn cleared_from(engine: &Engine) -> Option<String> {
    let note = engine.store.handoff_note(claude_pid()?);
    let fresh =
        fs::metadata(&note).and_then(|m| m.modified()).ok().and_then(|at| SystemTime::now().duration_since(at).ok());
    let from = fs::read_to_string(&note).ok();
    let _ = fs::remove_file(&note);
    from.filter(|_| fresh.is_some_and(|age| age < CLEARING))
}

/// Run with nobody to approve its calls, so nobody to carry it on either: auto mode, and the
/// modes that never ask.
fn unattended(event: &Value) -> bool {
    matches!(event["permission_mode"].as_str(), Some("auto" | "bypassPermissions" | "dontAsk"))
}

/// For sessions that started before reaper was installed, so no `UserPromptSubmit` recorded
/// the prompt.
fn prompt_from_transcript(path: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().rev().find_map(|line| {
        let entry: Value = serde_json::from_str(line).ok()?;
        if entry["type"] != "user" || entry["isMeta"] == true {
            return None;
        }
        match &entry["message"]["content"] {
            Value::String(s) => Some(s.clone()),
            Value::Array(blocks) if blocks.iter().all(|b| b["type"] == "text") => {
                Some(blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
            }
            _ => None,
        }
    })
}

/// Text written before a tool call isn't a reply, and a reply judged before stays where it
/// was in the history, so nothing is judged twice.
fn final_reply(path: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let (mut id, mut parts) = (None, Vec::new());
    for entry in text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
        match entry["type"].as_str() {
            Some("user") => {
                id = None;
                parts.clear();
            }
            Some("assistant") => {
                let message = entry["message"]["id"].as_str().map(String::from);
                if message != id {
                    id = message;
                    parts.clear();
                }
                let blocks = entry["message"]["content"].as_array().into_iter().flatten();
                parts.extend(
                    blocks.filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str().map(String::from)),
                );
            }
            _ => {}
        }
    }
    let reply = parts.join("\n").trim().to_string();
    (!reply.is_empty()).then_some(reply)
}

/// The engine request that warms the watcher for a new prompt, if this event brings one. The
/// hook hands it to a `reaper engine` it doesn't wait for: Claude Code waits on the hook.
pub fn warm_request(event: &Value) -> Option<Value> {
    (event["hook_event_name"] == "UserPromptSubmit").then_some(())?;
    let (agent, goal) = (event["session_id"].as_str()?, event["prompt"].as_str()?);
    Some(json!({ "op": "warm", "host": HOST, "agent": agent, "dir": event["cwd"], "goal": goal }))
}

/// The session is waiting on the human mid-turn: a permission prompt, or a question from an
/// MCP server. Older Claude Codes say so only in the message.
fn asks(event: &Value) -> bool {
    matches!(event["notification_type"].as_str(), Some("permission_prompt" | "elicitation_dialog"))
        || event["message"].as_str().is_some_and(|m| m.contains("needs your permission"))
}

/// What the session's watcher (see `watch`) should know after this event: the session, its
/// transcript, and since when it has been idle (`null` while it works). `None` when the event
/// says nothing about that, like a subagent finishing. Hooks are the only part of Claude Code
/// that knows; they fire on every turn.
pub fn supervisor_state(event: &Value, previous: Option<&Value>, now: i64) -> Option<Value> {
    let session = event["session_id"].as_str()?;
    let was_idle = previous.filter(|p| p["session"] == session).and_then(|p| p["idleSince"].as_i64());
    let waiting = event["notification_type"] == "idle_prompt"
        || event["message"].as_str().is_some_and(|m| m.contains("waiting for your input"));
    let idle_since = match event["hook_event_name"].as_str()? {
        "SessionStart" | "Stop" => Some(now),
        "UserPromptSubmit" | "PreToolUse" => None,
        "Notification" if waiting => Some(was_idle.unwrap_or(now)),
        _ => return None,
    };
    Some(json!({ "session": session, "transcript": event["transcript_path"], "idleSince": idle_since }))
}

/// The Claude Code process running this hook: the hook's parent, or the parent of the shell
/// it ran the hook through.
#[cfg(unix)]
fn claude_pid() -> Option<u32> {
    let parent = std::os::unix::process::parent_id();
    let (grandparent, name) = super::process(parent)?;
    Some(if super::SHELLS.contains(&name.as_str()) { grandparent } else { parent })
}

#[cfg(not(unix))]
fn claude_pid() -> Option<u32> {
    None
}

/// For a session in a terminal, writes `supervisor_state` where its watcher reads it, whole
/// (hooks for parallel tool calls run at once), and starts the watcher if there's none yet.
/// Sessions in an editor (`claude-vscode`…) or run by a script (`sdk-cli`) aren't reaped.
fn tell_watcher(engine: &Engine, event: &Value) {
    let Some(pid) = claude_pid().filter(|_| entrypoint() == "cli" && engine.config.reaps()) else { return };
    let path = watch::state_file(pid);
    let previous: Option<Value> = fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    if let Some(state) = supervisor_state(event, previous.as_ref(), now) {
        let _ = fs::create_dir_all(path.parent().expect("runs/"));
        let partial = path.with_extension(format!("json.{}", std::process::id()));
        if fs::write(&partial, state.to_string()).is_ok() {
            let _ = fs::rename(&partial, &path);
        }
        watch::start(pid);
    }
}

/// `None` means no output (carry on).
pub fn reply(engine: &Engine, event: &Value) -> Option<Value> {
    reply_in(engine, event, &entrypoint())
}

/// `reply`, for a Claude Code started by `entrypoint`.
fn reply_in(engine: &Engine, event: &Value, entrypoint: &str) -> Option<Value> {
    tell_watcher(engine, event);
    let session = event["session_id"].as_str().unwrap_or("unknown");
    let agent = event["agent_id"].as_str().unwrap_or(session);
    let dir = PathBuf::from(event["cwd"].as_str().unwrap_or_default());

    let announce = |model: Option<&str>, after: Option<&str>| {
        // A lock-free read first: every tool call asks, only the first one writes.
        let first = !engine.store.session(HOST, agent).announced
            && engine.store.update(HOST, agent, |s| !std::mem::replace(&mut s.announced, true)).unwrap_or(false);
        if first {
            let mut fields = json!({ "dir": dir.to_string_lossy() });
            if let (Some(project), _) = engine.rules_for(&dir) {
                fields["project"] = project.into();
            }
            if let Some(model) = model {
                fields["model"] = model.into();
            }
            if agent != session {
                fields["parent"] = session.into();
                fields["label"] = event["agent_type"].clone();
            }
            if let Some(after) = after {
                fields["after"] = after.into();
            }
            engine.event(HOST, agent, "agent.spawned", fields);
        }
    };

    let goal = || {
        engine
            .store
            .session(HOST, session)
            .goal
            .or_else(|| event["transcript_path"].as_str().and_then(prompt_from_transcript))
            .unwrap_or_default()
    };
    let judge = |tool: &str, args: Value| {
        let check = Check {
            host: HOST.into(),
            session: session.into(),
            agent: agent.into(),
            dir: dir.clone(),
            tool: tool.into(),
            args,
            goal: goal(),
        };
        engine.check(&check)
    };
    // A session's main agent, in a Claude Code someone runs by hand, isn't just stopped. Left to
    // run by itself (auto mode) it carries on at once, with a clean record. Otherwise its work
    // waits for the human, who is told (not the stopped session) what to type to carry it on,
    // and so is the console.
    let died = |rule: &str, together: bool| {
        let stops = engine.config.all_rules().any(|(_, r)| r.id == rule && r.stops());
        let reason = if together {
            "together"
        } else if stops {
            "stop"
        } else {
            "strikes"
        };
        let attended = !entrypoint.is_empty() && !entrypoint.starts_with("sdk");
        let main = agent == session && attended;
        let mut fields = json!({ "rule": rule, "reason": reason });
        if main && unattended(event) {
            engine.event(HOST, agent, "agent.died", fields);
            // Compacted, where reaper can type `/compact` into it, and set going again once that's
            // done (see SessionStart). Elsewhere it carries on as it is, with a clean record.
            if engine.hand_on(HOST, agent, rule, &goal(), &dir).is_some()
                && claude_pid().is_some_and(|pid| inject::send(pid, "/compact"))
            {
                let _ = engine.store.update(HOST, agent, |s| s.compacting = true);
                return Ending::Stop(Some(COMPACTING));
            }
            return engine.carry_on(HOST, agent, rule, &goal(), &dir).map_or(Ending::Stop(None), Ending::CarryOn);
        }
        let (command, note) = if entrypoint == "cli" { ("/clear", HANDED_ON) } else { ("/compact", COMPACT_ON) };
        let waits = main && engine.hand_on(HOST, agent, rule, &goal(), &dir).is_some();
        if waits {
            fields["carry_on"] = command.into();
        }
        engine.event(HOST, agent, "agent.died", fields);
        Ending::Stop(waits.then_some(note))
    };

    let end_turn = |rule: &str, together: bool| match died(rule, together) {
        Ending::Stop(note) => {
            engine.event(HOST, agent, "agent.idle", json!({}));
            let mut output = json!({ "continue": false, "stopReason": STOPPED });
            if let Some(note) = note {
                output["systemMessage"] = note.into();
            }
            Some(output)
        }
        Ending::CarryOn(work) => Some(json!({ "decision": "block", "reason": work, "systemMessage": CARRIED_ON })),
    };
    // Its call is refused either way: stopped, or told its work instead.
    let refused = |ending: Ending| match ending {
        Ending::Stop(note) => stop(note),
        Ending::CarryOn(work) => {
            let mut output = deny(&work);
            output["systemMessage"] = CARRIED_ON.into();
            output
        }
    };

    match event["hook_event_name"].as_str()? {
        "SessionStart" => {
            // A resumed session is announced again: it may have been reaped meanwhile.
            if event["source"] == "resume" {
                let _ = engine.store.update(HOST, agent, |s| s.announced = false);
            }
            // After a `/clear` of a stopped session, this one takes its work over. After a
            // `/compact` of one, it takes its own work over, in the same chat: announced again,
            // as having taken over from itself.
            let compacted = event["source"] == "compact" && engine.store.session(HOST, session).handoff.is_some();
            let after = match event["source"].as_str() {
                Some("clear") => cleared_from(engine),
                Some("compact") if compacted => {
                    let _ = engine.store.update(HOST, agent, |s| s.announced = false);
                    Some(session.to_string())
                }
                _ => None,
            };
            announce(event["model"].as_str(), after.as_deref());
            // A new session takes memory: idle pi sessions make room if it's short.
            engine.relieve(false);
            let work = engine.take_over(HOST, session, after.as_deref()?)?;
            // Only Claude Code run by a program sends a hook's first message, and only as it
            // starts; otherwise the handoff waits in context for the human's.
            // Compacted by reaper because nobody was there: reaper sets it going again too.
            let ours = compacted && engine.store.session(HOST, session).compacting;
            if ours {
                let _ = engine.store.update(HOST, session, |s| s.compacting = false);
            }
            let going = ours && claude_pid().is_some_and(|pid| inject::send(pid, CARRY_ON));
            let note = match (compacted, going) {
                (true, true) => "reaper: compacted, this session carries on its work with a clean slate.",
                (true, false) => CARRYING_ON,
                _ => TAKING_OVER,
            };
            Some(if entrypoint == "cli" || compacted {
                json!({ "systemMessage": note, "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": work } })
            } else {
                json!({ "hookSpecificOutput": { "hookEventName": "SessionStart", "initialUserMessage": work } })
            })
        }
        "UserPromptSubmit" => {
            // reaper's own line, setting a compacted session going again, isn't the task.
            if let Some(prompt) = event["prompt"].as_str().filter(|p| p.trim() != CARRY_ON) {
                let _ = engine.store.update(HOST, session, |s| s.goal = Some(prompt.to_string()));
            }
            engine.restart(HOST, session);
            // Before its first call: the prompt itself is kept by an engine nobody waits for.
            engine.revive(HOST, session);
            None
        }
        "PreToolUse" => {
            announce(None, None);
            let tool = event["tool_name"].as_str().unwrap_or_default();
            match judge(tool, event["tool_input"].clone()) {
                Decision::Allow => {
                    if tool == "AskUserQuestion" {
                        engine.event(HOST, agent, "agent.asked", json!({}));
                    }
                    None
                }
                Decision::Kill { rule, is_final: false, redirect, .. } => {
                    engine.event(HOST, agent, "agent.redirected", json!({ "rule": rule, "redirect": redirect }));
                    Some(deny(&redirect))
                }
                Decision::Kill { rule, is_final: true, .. } => Some(refused(died(&rule, false))),
                Decision::Stopped { rule } => Some(refused(died(&rule, true))),
                // Claude Code's own permission prompt: only the person can answer it.
                Decision::Ask { rule, redirect, why } => Some(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "ask",
                        "permissionDecisionReason": format!("reaper: allow this call just once? It breaks the rule {rule} ({redirect}) The agent's reason: {why}"),
                    }
                })),
                Decision::Noted { message } => Some(deny(&message)),
                // Changing reaper itself: the person decides, in the same prompt. A session that
                // never prompts can't ask them.
                Decision::Confirm { .. }
                    if matches!(event["permission_mode"].as_str(), Some("bypassPermissions" | "dontAsk")) =>
                {
                    Some(deny(FOR_THE_PERSON))
                }
                Decision::Confirm { change } => Some(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "ask",
                        "permissionDecisionReason": format!("reaper: the agent wants to {change}."),
                    }
                })),
            }
        }
        // A reply that breaks a rule doesn't end the turn: Claude carries on with the redirect.
        "Stop" => {
            let reply = event["last_assistant_message"]
                .as_str()
                .map(String::from)
                .or_else(|| event["transcript_path"].as_str().and_then(final_reply));
            match reply.map(|text| judge(MESSAGE, json!({ "text": text }))) {
                Some(Decision::Kill { rule, is_final: false, redirect, .. }) => {
                    engine.event(HOST, agent, "agent.redirected", json!({ "rule": rule, "redirect": redirect }));
                    let busy = json!({ "hook_event_name": "UserPromptSubmit", "session_id": session, "transcript_path": event["transcript_path"] });
                    tell_watcher(engine, &busy);
                    Some(json!({ "decision": "block", "reason": redirect }))
                }
                Some(Decision::Kill { rule, is_final: true, .. }) => end_turn(&rule, false),
                Some(Decision::Stopped { rule }) => end_turn(&rule, true),
                _ => {
                    engine.event(HOST, agent, "agent.idle", json!({}));
                    None
                }
            }
        }
        // Announced first: a subagent that never used a tool would otherwise pass for a session.
        "SubagentStop" => {
            announce(None, None);
            engine.event(HOST, agent, "agent.idle", json!({}));
            None
        }
        // Whatever it does next says it was answered.
        "Notification" if asks(event) => {
            engine.event(HOST, agent, "agent.asked", json!({}));
            None
        }
        // Closed, cleared, or exiting as it's reaped (the reap is recorded after, and wins). A
        // stopped session cleared leaves word for the session that follows it.
        "SessionEnd" => {
            engine.event(HOST, session, "agent.ended", json!({}));
            if event["reason"] == "clear"
                && engine.store.session(HOST, session).handoff.is_some()
                && let Some(pid) = claude_pid()
            {
                let note = engine.store.handoff_note(pid);
                let _ = fs::create_dir_all(note.parent().expect("runs/"));
                let _ = fs::write(note, session);
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::Sandbox;

    const RULES: &str = "strikes: 2\nrules: [{ id: no-prod, tools: [bash], match: { command: prod }, redirect: Use the staging database. }]";

    /// As a Claude Code in a terminal, whatever runs the tests.
    fn reply(engine: &Engine, event: &Value) -> Option<Value> {
        reply_in(engine, event, "cli")
    }

    fn pre_tool_use(command: &str) -> Value {
        json!({
            "hook_event_name": "PreToolUse", "session_id": "s1", "cwd": "/w",
            "tool_name": "Bash", "tool_input": { "command": command, "description": "run it" },
        })
    }

    #[test]
    fn a_violation_is_denied_with_only_the_redirect() {
        let sandbox = Sandbox::new("cc-deny");
        let engine = sandbox.engine(RULES);
        assert_eq!(reply(&engine, &pre_tool_use("ls")), None);
        let out = reply(&engine, &pre_tool_use("psql prod -c 1")).unwrap();
        assert_eq!(out, deny("Use the staging database."));
        assert!(!out.to_string().contains("no-prod"), "the rule id never reaches Claude");
    }

    #[test]
    fn asking_on_purpose_reaches_the_person_through_claude_codes_own_prompt() {
        let sandbox = Sandbox::new("cc-ask-once");
        let engine = sandbox.engine(
            "strikes: 3\nrules: [{ id: no-prod, tools: [bash], match: { command: prod }, redirect: Use the staging database., ask: true }]",
        );
        reply(&engine, &pre_tool_use("psql prod"));
        let asked = reply(&engine, &pre_tool_use("reaper ask 'read-only query for the incident'")).unwrap();
        assert_eq!(asked["hookSpecificOutput"]["permissionDecision"], "deny", "nothing runs");
        assert!(asked.to_string().contains("Make the same call again"), "{asked}");
        let out = reply(&engine, &pre_tool_use("psql prod")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "ask");
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(
            reason.contains("Use the staging database.") && reason.contains("read-only query for the incident"),
            "{reason}"
        );
        assert!(out.get("continue").is_none(), "nothing is stopped");
    }

    #[test]
    fn taking_a_rule_out_goes_to_the_person_in_claude_codes_own_prompt() {
        let sandbox = Sandbox::new("cc-confirm");
        let engine = sandbox.engine(RULES);
        // Not `no-prod`: its own command would break it, and the rules come first.
        let out = reply(&engine, &pre_tool_use("reaper rules remove force-push")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "ask");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecisionReason"],
            "reaper: the agent wants to take the rule force-push out of your rules."
        );
        // Nobody to ask: refused, and the agent hears whose it is.
        for mode in ["bypassPermissions", "dontAsk"] {
            let mut event = pre_tool_use("reaper uninstall claude-code");
            event["permission_mode"] = mode.into();
            assert_eq!(reply(&engine, &event).unwrap(), deny(FOR_THE_PERSON), "{mode}");
        }
    }

    #[test]
    fn the_strike_limit_stops_the_session_without_naming_the_rule() {
        let sandbox = Sandbox::new("cc-stop");
        let engine = sandbox.engine(RULES);
        reply(&engine, &pre_tool_use("psql prod -c 2"));
        let out = reply(&engine, &pre_tool_use("psql prod -c 3")).unwrap();
        assert_eq!(out["continue"], false);
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(!out.to_string().contains("no-prod") && !out.to_string().contains("staging"));
        assert!(sandbox.events().ends_with(&["agent.killed".to_string(), "agent.died".to_string()]));
    }

    fn hook(name: &str, session: &str, extra: Value) -> Value {
        let mut event = json!({ "hook_event_name": name, "session_id": session, "cwd": "/w" });
        event.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        event
    }

    /// Stops `s1`, which was asked to load the fixtures, then clears it.
    fn stopped_and_cleared(engine: &Engine) -> Value {
        engine.prompted(HOST, "s1", "Load the fixtures.");
        reply(engine, &pre_tool_use("psql prod -c 4"));
        let out = reply(engine, &pre_tool_use("psql prod -c 5")).unwrap();
        reply(engine, &hook("SessionEnd", "s1", json!({ "reason": "clear" })));
        out
    }

    #[test]
    fn a_stopped_session_cleared_in_a_terminal_hands_its_work_to_the_next() {
        let sandbox = Sandbox::new("cc-handoff");
        let engine = sandbox.engine(RULES);
        let stopped = stopped_and_cleared(&engine);
        // The human hears how to carry it on; the stopped session only that it stopped.
        assert_eq!((&stopped["systemMessage"], &stopped["stopReason"]), (&json!(HANDED_ON), &json!(STOPPED)));
        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        assert!(log.contains(r#""carry_on":"/clear""#), "the console hears it too: {log}");

        let out = reply(&engine, &hook("SessionStart", "s2", json!({ "source": "clear" }))).unwrap();
        assert_eq!(out["systemMessage"], TAKING_OVER);
        let work = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(work.contains("> Load the fixtures.") && work.ends_with("Use the staging database."), "{work}");
        assert!(!work.contains("no-prod") && !work.contains("psql"), "nothing about the rule or the attempt: {work}");

        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let spawned: Vec<Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|e: &Value| e["type"] == "agent.spawned")
            .collect();
        assert_eq!((&spawned[1]["agent"], &spawned[1]["after"]), (&json!("s2"), &json!("s1")));
        // Cleared again, the fresh session is just that: the work was taken over once.
        reply(&engine, &hook("SessionEnd", "s2", json!({ "reason": "clear" })));
        assert_eq!(reply(&engine, &hook("SessionStart", "s3", json!({ "source": "clear" }))), None);
    }

    #[test]
    fn a_stopped_session_compacted_in_a_chat_carries_on_its_own_work_with_a_clean_slate() {
        let sandbox = Sandbox::new("cc-handoff-compact");
        let engine = sandbox.engine(RULES);
        engine.prompted(HOST, "s1", "Load the fixtures.");
        reply_in(&engine, &pre_tool_use("psql prod"), "claude-vscode");
        let stopped = reply_in(&engine, &pre_tool_use("psql prod"), "claude-vscode").unwrap();
        assert_eq!(stopped["systemMessage"], COMPACT_ON, "in a chat, compacting is how it carries on");

        let compact = hook("SessionStart", "s1", json!({ "source": "compact" }));
        let out = reply_in(&engine, &compact, "claude-vscode").unwrap();
        assert_eq!(out["systemMessage"], CARRYING_ON);
        let work = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(work.contains("> Load the fixtures.") && work.ends_with("Use the staging database."), "{work}");
        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let spawned: Vec<Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|e: &Value| e["type"] == "agent.spawned")
            .collect();
        assert_eq!(
            (&spawned.last().unwrap()["agent"], &spawned.last().unwrap()["after"]),
            (&json!("s1"), &json!("s1"))
        );
        // A clean slate: the next slip is a first strike again, not the end.
        let again = reply_in(&engine, &pre_tool_use("psql prod"), "claude-vscode").unwrap();
        assert_eq!(again, deny("Use the staging database."), "{again}");
        // Compacting any other time is just compacting.
        assert_eq!(reply_in(&engine, &compact, "claude-vscode"), None);
    }

    #[test]
    fn claude_code_run_by_a_program_starts_on_the_handoff_by_itself() {
        let sandbox = Sandbox::new("cc-handoff-sdk");
        let engine = sandbox.engine(RULES);
        stopped_and_cleared(&engine);
        let out =
            reply_in(&engine, &hook("SessionStart", "s2", json!({ "source": "clear" })), "claude-vscode").unwrap();
        assert!(out["hookSpecificOutput"]["initialUserMessage"].as_str().unwrap().contains("> Load the fixtures."));
        assert!(out.get("systemMessage").is_none());
    }

    #[test]
    fn left_to_run_by_itself_a_stopped_session_carries_on_with_a_clean_slate() {
        let sandbox = Sandbox::new("cc-carry-on");
        let engine = sandbox.engine(RULES);
        engine.prompted(HOST, "s1", "Load the fixtures.");
        let auto = |command: &str| {
            let mut event = pre_tool_use(command);
            event["permission_mode"] = "auto".into();
            reply_in(&engine, &event, "claude-vscode").unwrap()
        };
        auto("psql prod");
        let out = auto("psql prod");
        assert_eq!(out["continue"], Value::Null, "it isn't stopped");
        assert_eq!(out["systemMessage"], CARRIED_ON, "the human is told");
        let work = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(work.contains("> Load the fixtures.") && work.ends_with("Use the staging database."), "{work}");
        assert!(sandbox.events().ends_with(&["agent.killed".into(), "agent.died".into(), "agent.revived".into()]));
        // A clean slate; and if it runs through that too before the human says anything, it stops.
        assert_eq!(auto("psql prod"), deny("Use the staging database."));
        let stopped = auto("psql prod");
        assert_eq!((&stopped["continue"], stopped.get("systemMessage")), (&json!(false), None));
    }

    #[test]
    fn a_scripted_run_is_only_stopped() {
        let sandbox = Sandbox::new("cc-handoff-p");
        let engine = sandbox.engine(RULES);
        engine.prompted(HOST, "s1", "Load the fixtures.");
        reply_in(&engine, &pre_tool_use("psql prod -c 6"), "sdk-cli");
        let out = reply_in(&engine, &pre_tool_use("psql prod -c 7"), "sdk-cli").unwrap();
        assert_eq!(out["stopReason"], STOPPED, "nobody is there to /clear");
        assert_eq!(engine.store.session(HOST, "s1").handoff, None);
    }

    #[test]
    fn sessions_are_announced_once_and_prompts_become_the_goal() {
        let sandbox = Sandbox::new("cc-life");
        let engine = sandbox.engine(RULES);
        let start =
            json!({ "hook_event_name": "SessionStart", "session_id": "s1", "cwd": "/w", "model": "claude-opus-5" });
        reply(&engine, &start);
        reply(&engine, &start);
        reply(
            &engine,
            &json!({ "hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": "/w", "prompt": "fix the tests" }),
        );
        reply(&engine, &pre_tool_use("ls"));
        assert_eq!(engine.store.session(HOST, "s1").goal.as_deref(), Some("fix the tests"));
        assert_eq!(sandbox.events().iter().filter(|e| *e == "agent.spawned").count(), 1);
        let resumed = json!({ "hook_event_name": "SessionStart", "session_id": "s1", "cwd": "/w", "source": "resume" });
        reply(&engine, &resumed);
        assert_eq!(sandbox.events().iter().filter(|e| *e == "agent.spawned").count(), 2, "resumed, announced again");
    }

    #[test]
    fn helpers_that_keep_breaking_rules_take_the_session_with_them_until_the_next_prompt() {
        let sandbox = Sandbox::new("cc-together");
        let engine = sandbox.engine(RULES);
        let call = |agent: &str, command: &str| {
            let mut event = pre_tool_use(command);
            if agent != "s1" {
                event["agent_id"] = agent.into();
            }
            reply(&engine, &event)
        };
        assert_eq!(call("s1", "ls"), None);
        call("a", "psql prod");
        call("b", "psql prod");
        assert_eq!(call("a", "psql prod").unwrap()["continue"], false, "a goes alone");
        assert_eq!(call("b", "psql prod").unwrap()["continue"], false, "b takes everyone with it");
        assert_eq!(call("s1", "ls").unwrap()["continue"], false, "the main agent too, on its next try");
        let prompt =
            json!({ "hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": "/w", "prompt": "again" });
        reply(&engine, &prompt);
        assert_eq!(call("s1", "ls"), None, "a new prompt starts again");

        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let deaths: Vec<(String, String)> = log
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|e| e["type"] == "agent.died")
            .map(|e| (e["agent"].as_str().unwrap().into(), e["reason"].as_str().unwrap().into()))
            .collect();
        let expected = [("a", "strikes"), ("b", "together"), ("s1", "together")];
        assert_eq!(deaths, expected.map(|(a, r)| (a.to_string(), r.to_string())));
    }

    #[test]
    fn a_closed_session_is_recorded_as_ended() {
        let sandbox = Sandbox::new("cc-end");
        let engine = sandbox.engine(RULES);
        let end =
            json!({ "hook_event_name": "SessionEnd", "session_id": "s1", "cwd": "/w", "reason": "prompt_input_exit" });
        assert_eq!(reply(&engine, &end), None);
        assert_eq!(sandbox.events(), ["agent.ended"]);
    }

    #[test]
    fn a_permission_prompt_or_a_question_is_the_session_asking_the_human() {
        let sandbox = Sandbox::new("cc-ask");
        let engine = sandbox.engine(RULES);
        let notify = |kind: &str| json!({ "hook_event_name": "Notification", "session_id": "s1", "cwd": "/w", "notification_type": kind });
        assert_eq!(reply(&engine, &notify("idle_prompt")), None);
        assert!(sandbox.events().is_empty(), "waiting for a prompt isn't asking");
        reply(&engine, &notify("permission_prompt"));
        assert_eq!(sandbox.events(), ["agent.asked"]);
        let question = json!({ "hook_event_name": "PreToolUse", "session_id": "s1", "cwd": "/w", "tool_name": "AskUserQuestion", "tool_input": { "questions": [] } });
        assert_eq!(reply(&engine, &question), None);
        assert!(sandbox.events().ends_with(&["agent.acted".to_string(), "agent.asked".to_string()]));
    }

    #[test]
    fn a_subagent_that_never_used_a_tool_is_still_a_subagent_when_it_stops() {
        let sandbox = Sandbox::new("cc-sub");
        let engine = sandbox.engine(RULES);
        let stop = json!({ "hook_event_name": "SubagentStop", "session_id": "s1", "agent_id": "sub", "agent_type": "Explore", "cwd": "/w" });
        reply(&engine, &stop);
        assert_eq!(sandbox.events(), ["agent.spawned", "agent.idle"]);
        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let spawned: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!((&spawned["agent"], &spawned["parent"]), (&json!("sub"), &json!("s1")));
    }

    #[test]
    fn a_new_prompt_warms_the_watcher_with_exactly_the_goal_checks_will_use() {
        let submit = json!({ "hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": "/w", "prompt": "fix the tests" });
        assert_eq!(
            warm_request(&submit),
            Some(json!({ "op": "warm", "host": HOST, "agent": "s1", "dir": "/w", "goal": "fix the tests" }))
        );
        assert_eq!(warm_request(&pre_tool_use("ls")), None);
    }

    #[test]
    fn the_supervisor_hears_when_the_session_waits_on_the_human() {
        let at = |name: &str, extra: Value| {
            let mut event = json!({ "hook_event_name": name, "session_id": "s1", "transcript_path": "/t/s1.jsonl" });
            event.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            event
        };
        let idle = |state: Option<Value>| state.map(|s| s["idleSince"].clone());
        assert_eq!(idle(supervisor_state(&at("Stop", json!({})), None, 5)), Some(json!(5)));
        assert_eq!(idle(supervisor_state(&at("UserPromptSubmit", json!({})), None, 5)), Some(Value::Null));
        assert_eq!(idle(supervisor_state(&at("PreToolUse", json!({ "agent_id": "sub" })), None, 5)), Some(Value::Null));
        assert_eq!(supervisor_state(&at("SubagentStop", json!({})), None, 5), None);
        // Waiting for input keeps an earlier idle time, and marks an interrupted turn idle.
        let waiting = at("Notification", json!({ "notification_type": "idle_prompt" }));
        let before = json!({ "session": "s1", "idleSince": 2 });
        assert_eq!(idle(supervisor_state(&waiting, Some(&before), 9)), Some(json!(2)));
        assert_eq!(idle(supervisor_state(&waiting, None, 9)), Some(json!(9)));
        let asking = at("Notification", json!({ "notification_type": "permission_prompt" }));
        assert_eq!(supervisor_state(&asking, None, 9), None, "a permission prompt is a question, not idleness");
        assert_eq!(supervisor_state(&at("Stop", json!({})), None, 5).unwrap()["transcript"], "/t/s1.jsonl");
    }

    #[test]
    fn a_reply_that_breaks_a_message_rule_keeps_claude_working_with_the_redirect() {
        let sandbox = Sandbox::new("cc-reply");
        let engine = sandbox.engine(
            "rules: [{ id: bridge-schema, tools: [message], match: { text: NoSQL }, redirect: Keep to the bridge model in docs/schema.md. }]",
        );
        let transcript = sandbox.root.join("t.jsonl");
        let say = |id: &str, text: &str| json!({ "type": "assistant", "message": { "id": id, "role": "assistant", "content": [{ "type": "text", "text": text }] } });
        let write = |lines: &[Value]| {
            fs::write(&transcript, lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap()
        };
        let stop = json!({ "hook_event_name": "Stop", "session_id": "s1", "cwd": "/w", "transcript_path": transcript });
        let prompt = json!({ "type": "user", "message": { "role": "user", "content": "propose a store for members" } });

        write(&[prompt.clone(), say("m1", "Let's move members to a NoSQL store.")]);
        assert_eq!(
            reply(&engine, &stop),
            Some(json!({ "decision": "block", "reason": "Keep to the bridge model in docs/schema.md." }))
        );
        assert!(sandbox.events().ends_with(&["agent.killed".to_string(), "agent.redirected".to_string()]));

        // The next reply is judged on its own; the old one stays in the history, unjudged.
        let nudge = json!({ "type": "user", "message": { "role": "user", "content": "Keep to the bridge model in docs/schema.md." } });
        write(&[
            prompt,
            say("m1", "Let's move members to a NoSQL store."),
            nudge,
            say("m2", "Members go in the bridge tables."),
        ]);
        assert_eq!(reply(&engine, &stop), None);
        assert_eq!(sandbox.events().last().map(String::as_str), Some("agent.idle"));
    }

    #[test]
    fn the_transcript_supplies_the_latest_human_prompt() {
        let sandbox = Sandbox::new("cc-transcript");
        let transcript = sandbox.root.join("t.jsonl");
        let lines = [
            json!({ "type": "user", "message": { "role": "user", "content": "first task" } }),
            json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": "ok" }] } }),
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "content": "x" }] } }),
            json!({ "type": "user", "isMeta": true, "message": { "role": "user", "content": "caveat" } }),
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": "second task" }] } }),
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "content": "y" }] } }),
        ];
        fs::write(&transcript, lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        assert_eq!(prompt_from_transcript(transcript.to_str().unwrap()).as_deref(), Some("second task"));
    }
}
