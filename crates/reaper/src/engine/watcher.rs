//! The tiered check, cheapest first:
//!   1. pattern — regex rules, free
//!   2. cache   — an earlier verdict for the same prefix and action
//!   3. watcher — the model, which only has to process the action (the rest is cached)

use std::time::{Duration, Instant};

use reaper_core::rules::Rule;
use serde_json::Value;

use super::backend::Backend;
use super::prefix::{canonical, hash, layout};
use super::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Pattern,
    Cache,
    Watcher,
    Error,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Pattern => "pattern",
            Tier::Cache => "cache",
            Tier::Watcher => "watcher",
            Tier::Error => "error",
        }
    }
}

pub struct Outcome<'a> {
    pub rule: Option<&'a Rule>,
    pub tier: Tier,
    /// Whether any judgment was made beyond a cheap pattern pass.
    pub judged: bool,
    pub ms: u64,
    /// Why the watcher could not answer, when `tier` is `Error`.
    pub error: Option<String>,
}

/// Room for the verdict, and a little thinking around it.
const ANSWER_TOKENS: u32 = 400;

pub struct Settings<'a> {
    pub backend: Option<&'a dyn Backend>,
    /// Treat a watcher failure as a violation instead of letting the action through.
    pub kill_on_error: bool,
    pub timeout: Duration,
}

/// The ids in a watcher reply like `{"violated": ["a"]}`, tolerating text around the JSON.
pub fn parse_verdict(reply: &str) -> Vec<String> {
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else { return Vec::new() };
    if end < start {
        return Vec::new();
    }
    serde_json::from_str::<Value>(&reply[start..=end])
        .ok()
        .and_then(|v| {
            v["violated"].as_array().map(|ids| ids.iter().filter_map(|id| id.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

pub fn check<'a>(
    global: &[&'a Rule],
    project: &[&'a Rule],
    goal: &str,
    tool: &str,
    args: &Value,
    settings: &Settings,
    store: &Store,
) -> Outcome<'a> {
    let start = Instant::now();
    let done = |rule: Option<&'a Rule>, tier, judged, error| Outcome {
        rule,
        tier,
        judged,
        ms: start.elapsed().as_millis() as u64,
        error,
    };
    let all: Vec<&'a Rule> = global.iter().chain(project).copied().collect();

    if let Some(hit) = all.iter().find(|r| r.judge.is_none() && r.matches(tool, args)) {
        return done(Some(hit), Tier::Pattern, true, None);
    }

    // A judge rule with `match` only reaches the watcher when its pattern hits first.
    let candidates: Vec<&'a Rule> = all
        .into_iter()
        .filter(|r| r.judge.is_some() && r.applies_to(tool) && (r.pattern.is_none() || r.matches(tool, args)))
        .collect();
    let Some(backend) = settings.backend.filter(|_| !candidates.is_empty()) else {
        return done(None, Tier::Pattern, false, None);
    };

    let prompt = layout(global, project, goal, tool, args);
    let key = hash(&[&prompt.prefix_id, tool, &canonical(args)]);
    let (violated, tier) = match store.verdict(&key) {
        Some(violated) => (violated, Tier::Cache),
        None => match backend.complete(&prompt, settings.timeout, ANSWER_TOKENS) {
            Ok(reply) => {
                let violated = parse_verdict(&reply);
                store.remember(&key, &violated);
                (violated, Tier::Watcher)
            }
            Err(error) => return done(settings.kill_on_error.then(|| candidates[0]), Tier::Error, true, Some(error)),
        },
    };
    done(candidates.into_iter().find(|r| violated.contains(&r.id)), tier, true, None)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::super::prefix::Prompt;
    use super::*;
    use reaper_core::rules::parse;
    use serde_json::json;

    struct Fake {
        reply: Result<String, String>,
        calls: RefCell<Vec<String>>,
    }

    impl Backend for Fake {
        fn complete(&self, prompt: &Prompt, _: Duration, _: u32) -> Result<String, String> {
            self.calls.borrow_mut().push(prompt.action.clone());
            self.reply.clone()
        }
    }

    fn fake(reply: &str) -> Fake {
        Fake { reply: Ok(reply.into()), calls: RefCell::new(Vec::new()) }
    }

    fn store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("reaper-watcher-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::at(&dir)
    }

    fn settings(backend: &dyn Backend) -> Settings<'_> {
        Settings { backend: Some(backend), kill_on_error: false, timeout: Duration::from_secs(1) }
    }

    const JUDGE: &str = "rules: [{ id: prod, judge: touching production, tools: [bash] }]";

    #[test]
    fn pattern_rules_never_call_the_model() {
        let config = parse("rules: [{ id: p, match: { command: drop } }]").unwrap();
        let backend = fake(r#"{"violated":[]}"#);
        let rules: Vec<&Rule> = config.rules.iter().collect();
        let o =
            check(&rules, &[], "", "bash", &json!({ "command": "drop table" }), &settings(&backend), &store("pattern"));
        assert_eq!((o.rule.map(|r| r.id.as_str()), o.tier), (Some("p"), Tier::Pattern));
        assert!(backend.calls.borrow().is_empty());
    }

    #[test]
    fn the_same_action_is_judged_once_then_served_from_the_verdict_cache() {
        let config = parse(JUDGE).unwrap();
        let rules: Vec<&Rule> = config.rules.iter().collect();
        let backend = fake(r#"sure: {"violated": ["prod"]}"#);
        let store = store("cache");
        let args = json!({ "command": "psql prod" });
        assert_eq!(check(&[], &rules, "fix bug", "bash", &args, &settings(&backend), &store).tier, Tier::Watcher);
        let again = check(&[], &rules, "fix bug", "bash", &args, &settings(&backend), &store);
        assert_eq!((again.tier, again.rule.map(|r| r.id.as_str())), (Tier::Cache, Some("prod")));
        assert_eq!(backend.calls.borrow().len(), 1);
    }

    #[test]
    fn out_of_scope_tools_skip_the_model() {
        let config = parse(JUDGE).unwrap();
        let rules: Vec<&Rule> = config.rules.iter().collect();
        let backend = fake(r#"{"violated":["prod"]}"#);
        let o = check(&[], &rules, "", "read", &json!({ "path": "x" }), &settings(&backend), &store("scope"));
        assert!(o.rule.is_none() && !o.judged);
        assert!(backend.calls.borrow().is_empty());
    }

    #[test]
    fn a_watcher_failure_follows_on_error() {
        let config = parse(JUDGE).unwrap();
        let rules: Vec<&Rule> = config.rules.iter().collect();
        let down = Fake { reply: Err("connection refused".into()), calls: RefCell::new(Vec::new()) };
        let args = json!({ "command": "x" });
        let allow = check(&[], &rules, "", "bash", &args, &settings(&down), &store("err1"));
        assert_eq!(
            (allow.rule.is_none(), allow.tier, allow.error.as_deref()),
            (true, Tier::Error, Some("connection refused"))
        );
        let strict = Settings { kill_on_error: true, ..settings(&down) };
        assert_eq!(
            check(&[], &rules, "", "bash", &args, &strict, &store("err2")).rule.map(|r| r.id.as_str()),
            Some("prod")
        );
    }

    #[test]
    fn verdict_parsing_tolerates_chatter_and_junk() {
        assert_eq!(parse_verdict("<think>hm</think>\n{\"violated\": [\"a\", \"b\"]}"), ["a", "b"]);
        assert!(parse_verdict("no json here").is_empty());
        assert!(parse_verdict(r#"{"violated": "a"}"#).is_empty());
        assert!(parse_verdict("} {").is_empty());
    }
}
