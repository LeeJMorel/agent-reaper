//! `reaper hook claude-code`: Claude Code runs this once per hook event, with the event as
//! JSON on stdin; the reply on stdout is Claude Code's hook output. What reaper can do here
//! is narrower than in pi: a call is blocked and the agent gets only the positive redirect,
//! but Claude Code hooks can't remove the attempt from the conversation. Claude's replies are
//! judged when it stops, by the rules that list `message`.

pub mod install;
pub mod watch;

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use reaper_core::rules::MESSAGE;

use crate::engine::{Check, Decision, Engine};

pub const HOST: &str = "claude-code";

/// What the human sees when a session is stopped. Claude sees it too if the conversation
/// continues, so it names no rule.
const STOPPED: &str = "reaper stopped this session. The reaper console shows why.";

fn deny(reason: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason } })
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
    Some(json!({ "op": "warm", "dir": event["cwd"], "goal": event["prompt"].as_str()? }))
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
    let terminal = std::env::var("CLAUDE_CODE_ENTRYPOINT").is_ok_and(|e| e == "cli");
    let Some(pid) = claude_pid().filter(|_| terminal && engine.config.reaps()) else { return };
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
    tell_watcher(engine, event);
    let session = event["session_id"].as_str().unwrap_or("unknown");
    let agent = event["agent_id"].as_str().unwrap_or(session);
    let dir = PathBuf::from(event["cwd"].as_str().unwrap_or_default());

    let announce = |model: Option<&str>| {
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
    let died = |rule: &str| {
        let stops = engine.config.all_rules().any(|(_, r)| r.id == rule && r.stops());
        engine.event(
            HOST,
            agent,
            "agent.died",
            json!({ "rule": rule, "reason": if stops { "stop" } else { "strikes" } }),
        );
    };

    match event["hook_event_name"].as_str()? {
        "SessionStart" => {
            // A resumed session is announced again: it may have been reaped meanwhile.
            if event["source"] == "resume" {
                let _ = engine.store.update(HOST, agent, |s| s.announced = false);
            }
            announce(event["model"].as_str());
            // A new session takes memory: idle pi sessions make room if it's short.
            engine.relieve(false);
            None
        }
        "UserPromptSubmit" => {
            if let Some(prompt) = event["prompt"].as_str() {
                let _ = engine.store.update(HOST, session, |s| s.goal = Some(prompt.to_string()));
            }
            None
        }
        "PreToolUse" => {
            announce(None);
            match judge(event["tool_name"].as_str().unwrap_or_default(), event["tool_input"].clone()) {
                Decision::Allow => None,
                Decision::Kill { rule, is_final: false, redirect, .. } => {
                    engine.event(HOST, agent, "agent.redirected", json!({ "rule": rule, "redirect": redirect }));
                    Some(deny(&redirect))
                }
                Decision::Kill { rule, is_final: true, .. } => {
                    died(&rule);
                    let mut output = deny("Stopped.");
                    output["continue"] = false.into();
                    output["stopReason"] = STOPPED.into();
                    Some(output)
                }
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
                Some(Decision::Kill { rule, is_final: true, .. }) => {
                    died(&rule);
                    engine.event(HOST, agent, "agent.idle", json!({}));
                    Some(json!({ "continue": false, "stopReason": STOPPED }))
                }
                _ => {
                    engine.event(HOST, agent, "agent.idle", json!({}));
                    None
                }
            }
        }
        "SubagentStop" => {
            engine.event(HOST, agent, "agent.idle", json!({}));
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
        let out = reply(&engine, &pre_tool_use("psql prod")).unwrap();
        assert_eq!(out, deny("Use the staging database."));
        assert!(!out.to_string().contains("no-prod"), "the rule id never reaches Claude");
    }

    #[test]
    fn the_strike_limit_stops_the_session_without_naming_the_rule() {
        let sandbox = Sandbox::new("cc-stop");
        let engine = sandbox.engine(RULES);
        reply(&engine, &pre_tool_use("psql prod"));
        let out = reply(&engine, &pre_tool_use("psql prod")).unwrap();
        assert_eq!(out["continue"], false);
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(!out.to_string().contains("no-prod") && !out.to_string().contains("staging"));
        assert!(sandbox.events().ends_with(&["agent.killed".to_string(), "agent.died".to_string()]));
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
    fn a_new_prompt_warms_the_watcher_with_exactly_the_goal_checks_will_use() {
        let submit = json!({ "hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": "/w", "prompt": "fix the tests" });
        assert_eq!(warm_request(&submit), Some(json!({ "op": "warm", "dir": "/w", "goal": "fix the tests" })));
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
