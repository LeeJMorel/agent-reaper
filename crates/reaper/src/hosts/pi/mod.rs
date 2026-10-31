//! `reaper engine`: the pi extension (extensions/pi) sends one JSON request on stdin and
//! reads one JSON reply on stdout, for every tool call and lifecycle event.
//!
//!   {"op":"check","host":"pi","session":…,"agent":…,"dir":…,"tool":…,"args":{…},"goal":…}
//!     → {"kind":"allow"} | {"kind":"kill","rule":…,"redirect":…,"strike":n,"final":bool,"reason"?:…}
//!       (plus "problem" when the rules on disk can't be used)
//!   {"op":"warm","host":…,"agent":…,"dir":…,"goal":…}  → {"ok":true}, once the watcher has
//!     the session's prefix: the human asked for something, so the prompt is logged too
//!   {"op":"event","host":"pi","agent":…,"type":"agent.idle",…fields}  → {"ok":true}
//!     (agent.spawned and agent.idle add "reapAfterMs" while idle sessions are reaped: a
//!     session starts out idle, and goes idle when its turn ends; for agent.reaped,
//!     "file" names the saved session, which `reaper run` offers to resume, and without
//!     `reaper run` the reply's "say" tells the human how)

pub mod extension;
pub mod install;
pub mod release;
pub mod run;

use std::path::PathBuf;

use reaper_core::events::{now, written};
use serde_json::{Map, Value, json};

use crate::engine::{Check, Decision, Engine};

/// Enough of a prompt to tell sessions apart by, and to read in the console.
const MAX_PROMPT: usize = 2000;

fn text(request: &Value, key: &str) -> String {
    request[key].as_str().unwrap_or_default().to_string()
}

pub fn reply(engine: &Engine, request: &Value) -> Value {
    let host = request["host"].as_str().unwrap_or("pi");
    match request["op"].as_str() {
        Some("check") => {
            let check = Check {
                host: host.into(),
                session: text(request, "session"),
                agent: text(request, "agent"),
                dir: PathBuf::from(text(request, "dir")),
                tool: text(request, "tool"),
                args: request["args"].clone(),
                goal: text(request, "goal"),
            };
            let mut reply = match engine.check(&check) {
                Decision::Allow => json!({ "kind": "allow" }),
                Decision::Kill { rule, strike, is_final, redirect, .. } => {
                    let mut kill = json!({ "kind": "kill", "rule": rule, "redirect": redirect, "strike": strike, "final": is_final });
                    if is_final {
                        let stops = engine.config.all_rules().any(|(_, r)| r.id == rule && r.stops());
                        kill["reason"] = (if stops { "stop" } else { "strikes" }).into();
                    }
                    kill
                }
                // A pi session is one agent, which its own limit stops first; kept for any host
                // with helpers that talks to the engine this way.
                Decision::Stopped { rule } => {
                    json!({ "kind": "kill", "rule": rule, "redirect": "", "strike": 0, "final": true, "reason": "together" })
                }
            };
            if let Some(problem) = &engine.problem {
                reply["problem"] = problem.clone().into();
            }
            reply
        }
        Some("event") => {
            let kind = text(request, "type");
            let mut fields: Map<String, Value> = request.as_object().cloned().unwrap_or_default();
            for key in ["op", "host", "agent", "type"] {
                fields.remove(key);
            }
            if kind == "agent.spawned"
                && let Some(dir) = fields.get("dir").and_then(Value::as_str)
            {
                let (project, _) = engine.rules_for(&PathBuf::from(dir));
                if let Some(project) = project {
                    fields.insert("project".into(), project.into());
                }
            }
            if kind == "agent.spawned" {
                install::refresh();
            }
            let agent = text(request, "agent");
            let say = (kind == "agent.reaped" && !run::reaped(&fields)).then(|| {
                let reason = fields.get("reason").and_then(Value::as_str).unwrap_or("idle");
                let idle = fields.get("idle").and_then(Value::as_u64);
                let [why, how] = super::reaped(idle, reason, &run::resume_command(&agent));
                format!("\n{why}\n{how}\n")
            });
            let interactive = fields.remove("interactive") == Some(Value::Bool(true));
            engine.event(host, &agent, &kind, Value::Object(fields));
            let mut reply = json!({ "ok": true });
            if let Some(say) = say {
                reply["say"] = say.into();
            }
            if matches!(kind.as_str(), "agent.spawned" | "agent.idle")
                && let Some(ms) = engine.reap_after_ms()
            {
                reply["reapAfterMs"] = ms.into();
            }
            // Only interactive sessions can be reaped: they're the ones listed as live.
            if kind == "agent.spawned"
                && interactive
                && let Ok((file, mark)) = engine.store.register(host, &agent, now())
            {
                reply["live"] = json!({ "file": file, "mark": mark });
            }
            if matches!(kind.as_str(), "agent.spawned" | "agent.idle") {
                engine.relieve(true);
            }
            reply
        }
        Some("warm") => {
            let goal = text(request, "goal");
            // Cut once the editor's context is off: a long selection would leave no prompt.
            if let (Some(agent), Some(prompt)) = (request["agent"].as_str(), written(&goal)) {
                let prompt: String = prompt.chars().take(MAX_PROMPT).collect();
                engine.event(host, agent, "agent.prompted", json!({ "prompt": prompt }));
            }
            engine.warm(&PathBuf::from(text(request, "dir")), &goal);
            json!({ "ok": true })
        }
        _ => json!({ "error": "unknown op" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::Sandbox;

    #[test]
    fn a_check_reports_the_kill_and_why_the_session_ends() {
        let sandbox = Sandbox::new("pi-check");
        let engine =
            sandbox.engine("strikes: 1\nrules: [{ id: push, match: { command: push }, redirect: Open a PR. }]");
        let request = json!({ "op": "check", "session": "s", "agent": "s", "dir": "/w", "tool": "bash", "args": { "command": "git push" }, "goal": "" });
        assert_eq!(
            reply(&engine, &request),
            json!({ "kind": "kill", "rule": "push", "redirect": "Open a PR.", "strike": 1, "final": true, "reason": "strikes" })
        );
    }

    #[test]
    fn an_idle_session_learns_when_it_will_be_reaped() {
        let idle = json!({ "op": "event", "agent": "a", "type": "agent.idle" });
        let on = Sandbox::new("pi-idle-on");
        assert_eq!(reply(&on.engine("reap: { idle: 90s }"), &idle), json!({ "ok": true, "reapAfterMs": 90_000 }));
        let off = Sandbox::new("pi-idle-off");
        assert_eq!(reply(&off.engine("reap: { idle: off }"), &idle), json!({ "ok": true }));
    }

    #[test]
    fn a_new_interactive_session_past_the_cap_makes_the_longest_idle_one_reap_itself() {
        let sandbox = Sandbox::new("pi-crowded");
        let engine = sandbox.engine("reap: { idle: 1h, live: 1 }");
        let start = |agent: &str| {
            reply(
                &engine,
                &json!({ "op": "event", "agent": agent, "type": "agent.spawned", "dir": "/w", "interactive": true }),
            )
        };
        let first = start("a");
        let mark = std::path::PathBuf::from(first["live"]["mark"].as_str().unwrap());
        assert!(!mark.exists(), "alone, nobody is in the way");
        // The second session is the newest idle one, so the first goes. Idle times are in ms,
        // and two sessions started in the same one tie, leaving the pick to directory order.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = start("b");
        assert_eq!(std::fs::read_to_string(&mark).unwrap(), "crowded");
        assert!(!std::path::Path::new(second["live"]["mark"].as_str().unwrap()).exists());
        // Print runs and the like are never listed, so they can't be asked.
        assert_eq!(
            reply(&engine, &json!({ "op": "event", "agent": "c", "type": "agent.spawned", "dir": "/w" }))["live"],
            Value::Null
        );
        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        assert!(!log.contains("interactive"), "bookkeeping stays out of the log");
    }

    #[test]
    fn auto_learns_the_timeout_from_how_long_the_human_takes_to_come_back() {
        let sandbox = Sandbox::new("pi-auto");
        let engine = sandbox.engine("reap: { idle: auto }");
        let idle = json!({ "op": "event", "agent": "a", "type": "agent.idle" });
        assert_eq!(reply(&engine, &idle)["reapAfterMs"], 300_000, "the default, with nothing to learn from yet");
        // 24 earlier waits: back after 2 minutes, three times in four.
        let mut log = String::new();
        for i in 0..24i64 {
            let (at, back) = (i * 10_000_000, if i % 4 == 3 { 3_000_000 } else { 120_000 });
            log += &format!("{{\"at\":{at},\"host\":\"pi\",\"agent\":\"s{i}\",\"type\":\"agent.idle\"}}\n");
            log += &format!(
                "{{\"at\":{},\"host\":\"pi\",\"agent\":\"s{i}\",\"type\":\"agent.acted\",\"tool\":\"bash\"}}\n",
                at + back
            );
        }
        std::fs::write(sandbox.root.join("events.jsonl"), log).unwrap();
        assert_eq!(reply(&engine, &idle)["reapAfterMs"], 120_000);
    }

    #[test]
    fn spawned_events_get_their_project() {
        let sandbox = Sandbox::new("pi-event");
        let engine = sandbox.engine("projects:\n  app: { paths: [/w/app], rules: [] }\n");
        assert_eq!(
            reply(&engine, &json!({ "op": "event", "agent": "a", "type": "agent.spawned", "dir": "/w/app/src" })),
            json!({ "ok": true, "reapAfterMs": 300_000 })
        );
        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let event: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(
            (event["host"].as_str(), event["project"].as_str(), event["dir"].as_str()),
            (Some("pi"), Some("app"), Some("/w/app/src"))
        );
    }

    #[test]
    fn a_new_prompt_is_logged_cut_short() {
        let sandbox = Sandbox::new("pi-prompt");
        let engine = sandbox.engine("");
        let long = "fix the tests ".repeat(500);
        assert_eq!(
            reply(&engine, &json!({ "op": "warm", "agent": "a", "dir": "/w", "goal": long })),
            json!({ "ok": true })
        );
        reply(&engine, &json!({ "op": "warm", "dir": "/w", "goal": "from an older extension" }));
        let log = std::fs::read_to_string(sandbox.root.join("events.jsonl")).unwrap();
        let events: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(events.len(), 1, "only with the agent it's for");
        assert_eq!((events[0]["type"].as_str(), events[0]["agent"].as_str()), (Some("agent.prompted"), Some("a")));
        assert_eq!(events[0]["prompt"].as_str().unwrap().chars().count(), MAX_PROMPT);
    }
}
