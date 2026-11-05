//! Setting the watcher from the rules tab. The choices are what this machine has: the Claude
//! Code the human is signed in to, and each model their Ollama has. Whichever of the two isn't
//! picked answers when the picked one can't.

use std::time::Duration;

use reaper_core::rules::Watcher;
use serde_json::Value;

use crate::engine::backend::CLAUDE_CODE;
use crate::hosts::on_path;

/// Small and quick enough to sit on every judged check.
pub const HAIKU: &str = "claude-haiku-5-5";
const OLLAMA: &str = "http://localhost:11434";

pub struct Choice {
    pub name: String,
    pub about: String,
    /// `None` turns the watcher off.
    pub watcher: Option<Watcher>,
}

impl Choice {
    /// Whether `current` is this choice, fallbacks aside.
    pub fn is(&self, current: Option<&Watcher>) -> bool {
        let key = |w: &Watcher| (w.kind.clone().filter(|k| k != "openai"), w.base_url.clone(), w.model.clone());
        self.watcher.as_ref().map(key) == current.map(key)
    }
}

/// What can watch here, best first, and turning it off.
pub fn choices(current: Option<&Watcher>) -> Vec<Choice> {
    offer(on_path("claude").is_some(), &ollama_models(), current)
}

fn offer(claude: bool, ollama: &[String], current: Option<&Watcher>) -> Vec<Choice> {
    let claude_code = || Watcher::new(Some(CLAUDE_CODE), None, HAIKU);
    let local = |model: &str| Watcher::new(None, Some(&format!("{OLLAMA}/v1")), model);
    // The Ollama model already in use, if it's still there; else the smallest.
    let in_use = current
        .into_iter()
        .flat_map(|w| std::iter::once(w).chain(&w.fallback))
        .filter(|w| w.base_url.as_deref().is_some_and(|u| u.starts_with(OLLAMA)))
        .find_map(|w| w.model.clone().filter(|m| ollama.contains(m)));
    let backup = in_use.or_else(|| ollama.first().cloned());
    let mut out = Vec::new();
    if claude {
        let mut watcher = claude_code();
        watcher.fallback.extend(backup.as_deref().map(local));
        let about = "Your Claude subscription, no API key. A few seconds a check.";
        out.push(Choice { name: format!("Claude Code · {HAIKU}"), about: about.into(), watcher: Some(watcher) });
    }
    for model in ollama {
        let mut watcher = local(model);
        if claude {
            watcher.fallback.push(claude_code());
        }
        let about = "On this machine: quick once loaded, and it takes memory while it is.";
        out.push(Choice { name: format!("Ollama · {model}"), about: about.into(), watcher: Some(watcher) });
    }
    let off = "Judged rules don't run; pattern rules still do.";
    out.push(Choice { name: "None".into(), about: off.into(), watcher: None });
    out
}

/// The models Ollama has here, smallest first, leaving out the ones made for embeddings.
fn ollama_models() -> Vec<String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_millis(500))).build().into();
    let Ok(mut response) = agent.get(&format!("{OLLAMA}/api/tags")).call() else { return Vec::new() };
    let Ok(body) = response.body_mut().read_to_string() else { return Vec::new() };
    let tags: Value = serde_json::from_str(&body).unwrap_or_default();
    let mut models: Vec<(u64, String)> = tags["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["size"].as_u64().unwrap_or(0), m["name"].as_str()?.to_string())))
        .filter(|(_, name)| !name.contains("embed"))
        .collect();
    models.sort();
    models.into_iter().map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_choice_falls_back_on_the_other_kind_here() {
        let ollama = ["qwen3:4b".to_string(), "qwen2.5:7b".to_string()];
        let mut current = Watcher::new(None, Some("http://localhost:11434/v1"), "qwen2.5:7b");
        current.on_error = Some("kill".into());
        let both = offer(true, &ollama, Some(&current));
        let names: Vec<&str> = both.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Claude Code · claude-haiku-5-5", "Ollama · qwen3:4b", "Ollama · qwen2.5:7b", "None"]);
        let claude = both[0].watcher.as_ref().unwrap();
        assert_eq!(claude.fallback[0].model.as_deref(), Some("qwen2.5:7b"), "the model already in use");
        assert_eq!(both[1].watcher.as_ref().unwrap().fallback[0].kind.as_deref(), Some(CLAUDE_CODE));
        assert!(both[2].is(Some(&current)) && !both[1].is(Some(&current)));

        let alone = offer(true, &[], None);
        assert!(alone[0].watcher.as_ref().unwrap().fallback.is_empty());
        assert_eq!(offer(false, &[], None).len(), 1, "nothing here but turning it off");
    }
}
