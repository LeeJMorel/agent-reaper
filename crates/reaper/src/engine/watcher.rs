//! The tiered check, cheapest first:
//!   1. pattern — regex rules, free
//!   2. cache   — an earlier verdict for the same prefix and action
//!   3. watcher — the model, which only has to process the action (the rest is cached)

use std::time::{Duration, Instant};

use reaper_core::rules::{Room, Rule};
use serde::{Deserialize, Serialize};
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
    /// The agent's own words that the watcher found break the rule (see `own_words`).
    pub quote: Option<String>,
}

/// Room for the verdict, and a little thinking around it.
const ANSWER_TOKENS: u32 = 400;

/// The longest quote an agent hears back, in characters.
const MAX_QUOTE: usize = 240;

/// What the watcher answered: the ids of the rules the action breaks, and the words in it that
/// break them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub violated: Vec<String>,
    #[serde(default)]
    pub quote: String,
}

pub struct Settings<'a> {
    pub backend: Option<&'a dyn Backend>,
    /// Treat a watcher failure as a violation instead of letting the action through.
    pub kill_on_error: bool,
    pub timeout: Duration,
    /// What the machine has to spare now, for rules that apply only when it's short (`free`,
    /// `load`).
    pub room: Room,
}

/// A watcher reply like `{"violated": ["a"], "quote": "…"}`, tolerating text around the JSON
/// and a reply without a quote.
pub fn parse_verdict(reply: &str) -> Verdict {
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else { return Verdict::default() };
    if end < start {
        return Verdict::default();
    }
    let Ok(v) = serde_json::from_str::<Value>(&reply[start..=end]) else { return Verdict::default() };
    Verdict {
        violated: v["violated"]
            .as_array()
            .map(|ids| ids.iter().filter_map(|id| id.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        quote: v["quote"].as_str().unwrap_or_default().to_string(),
    }
}

/// The watcher's quote, when it is the agent's own words from this call, so the agent hears back
/// only what it wrote, never something the watcher made up. Text an edit replaces doesn't count:
/// the agent is taking it out.
fn own_words(quote: &str, args: &Value) -> Option<String> {
    fn written<'v>(value: &'v Value, out: &mut Vec<&'v str>) {
        match value {
            Value::String(s) => out.push(s),
            Value::Array(items) => items.iter().for_each(|v| written(v, out)),
            Value::Object(map) => map.iter().filter(|(k, _)| !k.starts_with("old")).for_each(|(_, v)| written(v, out)),
            _ => {}
        }
    }
    let quote = plain(quote);
    let quote = quote.trim_matches(|c: char| c.is_whitespace() || "\"'…".contains(c));
    let quote = quote.trim_start_matches("...").trim_end_matches("...").trim();
    if quote.chars().count() < 8 {
        return None;
    }
    let mut texts = Vec::new();
    written(args, &mut texts);
    if !texts.into_iter().any(|t| plain(t).contains(quote)) {
        return None;
    }
    Some(match quote.char_indices().nth(MAX_QUOTE) {
        Some((cut, _)) => format!("{}…", quote[..cut].trim_end()),
        None => quote.to_string(),
    })
}

/// Text as the watcher may copy it from the JSON it was shown: escapes read, every run of
/// whitespace one space, and without the comment marks a quote leaves out where a comment runs
/// on to the next line (`*`, `//`, `#`).
fn plain(text: &str) -> String {
    let text = text.replace("\\n", " ").replace("\\t", " ").replace("\\\"", "\"");
    let words = text.split_whitespace().filter(|w| !w.chars().all(|c| "/*#".contains(c)));
    words.collect::<Vec<_>>().join(" ")
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
        quote: None,
    };
    // Only rules in force can be broken, but the watcher's prompt lists them all, so its
    // prefix stays the same as memory comes and goes.
    let all: Vec<&'a Rule> = global.iter().chain(project).copied().filter(|r| r.in_force(&settings.room)).collect();

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
    let (verdict, tier) = match store.verdict(&key) {
        Some(verdict) => (verdict, Tier::Cache),
        None => match backend.complete(&prompt, settings.timeout, ANSWER_TOKENS) {
            Ok(reply) => {
                let verdict = parse_verdict(&reply);
                store.remember(&key, &verdict);
                (verdict, Tier::Watcher)
            }
            Err(error) => return done(settings.kill_on_error.then(|| candidates[0]), Tier::Error, true, Some(error)),
        },
    };
    let rule = candidates.into_iter().find(|r| verdict.violated.contains(&r.id));
    Outcome { quote: rule.and_then(|_| own_words(&verdict.quote, args)), ..done(rule, tier, true, None) }
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
            self.calls.borrow_mut().push(prompt.prefix_id.clone());
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
        Settings {
            backend: Some(backend),
            kill_on_error: false,
            timeout: Duration::from_secs(1),
            room: Room::default(),
        }
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
    fn a_rule_waiting_for_memory_to_run_short_leaves_the_prompt_alone() {
        let config = parse(
            "rules:\n  - { id: prod, judge: touching production, tools: [bash] }\n  - { id: suite, judge: running the whole test suite, tools: [bash], free: 4G }\n",
        )
        .unwrap();
        let rules: Vec<&Rule> = config.rules.iter().collect();
        let backend = fake(r#"{"violated":["suite"]}"#);
        let args = json!({ "command": "pytest" });
        let plenty = Settings { room: Room { free: Some(12 << 30), load: None }, ..settings(&backend) };
        assert!(check(&rules, &[], "", "bash", &args, &plenty, &store("free1")).rule.is_none(), "not in force");
        let short = Settings { room: Room { free: Some(2 << 30), load: None }, ..settings(&backend) };
        let o = check(&rules, &[], "", "bash", &args, &short, &store("free2"));
        assert_eq!(o.rule.map(|r| r.id.as_str()), Some("suite"));
        let prefixes = backend.calls.borrow();
        assert_eq!((prefixes.len(), &prefixes[0]), (2, &prefixes[1]), "the same prefix, so the cache still applies");
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
        assert_eq!(parse_verdict("<think>hm</think>\n{\"violated\": [\"a\", \"b\"]}").violated, ["a", "b"]);
        assert_eq!(
            parse_verdict(r#"{"violated": ["a"], "quote": "an attacker could"}"#),
            Verdict { violated: vec!["a".into()], quote: "an attacker could".into() }
        );
        assert_eq!(parse_verdict("no json here"), Verdict::default());
        assert!(parse_verdict(r#"{"violated": "a"}"#).violated.is_empty());
        assert_eq!(parse_verdict("} {"), Verdict::default());
    }

    #[test]
    fn the_agent_hears_back_only_its_own_words() {
        let config =
            parse("rules: [{ id: story, judge: telling how a check could be got around, tools: [write, edit] }]")
                .unwrap();
        let rules: Vec<&Rule> = config.rules.iter().collect();
        let file = "/** Reads the scheme.\n *\n * Browsers skip control characters, so an attacker can\n * hide javascript: behind a tab.\n */\nexport const x = 1;\n";
        let quoted = |quote: &str, args: &Value| {
            let reply = json!({ "violated": ["story"], "quote": quote }).to_string();
            let backend = fake(&reply);
            check(&[], &rules, "", "write", args, &settings(&backend), &store(&format!("quote-{}", quote.len()))).quote
        };
        let write = json!({ "path": "a.ts", "content": file });
        assert_eq!(
            quoted("so an attacker can\\n * hide javascript: behind a tab.", &write).as_deref(),
            Some("so an attacker can hide javascript: behind a tab."),
            "copied from the JSON it was shown, line breaks and all"
        );
        assert_eq!(
            quoted("Browsers skip control characters, so an attacker can hide javascript: behind a tab.", &write)
                .as_deref(),
            Some("Browsers skip control characters, so an attacker can hide javascript: behind a tab."),
            "across a line break, without the comment's *"
        );
        assert_eq!(quoted("an attacker could read every guild's rows", &write), None, "words it made up");
        assert_eq!(quoted("tab", &write), None, "too short to point at anything");
        let edit = json!({ "path": "a.ts", "old_string": file, "new_string": "/** Reads the scheme. */\n", "content": "/** Reads the scheme. */\n" });
        assert_eq!(quoted("so an attacker can", &edit), None, "what an edit takes out");
        let long = format!("{} the end.", "word ".repeat(100));
        let said = quoted(&long, &json!({ "path": "a.ts", "content": format!("{long}\n{long}") })).unwrap();
        assert!(said.chars().count() <= MAX_QUOTE + 1 && said.ends_with('…'), "{said}");
        let reply = json!({ "violated": [], "quote": "so an attacker can hide javascript" }).to_string();
        let backend = fake(&reply);
        let o = check(&[], &rules, "", "write", &write, &settings(&backend), &store("quote-pass"));
        assert_eq!((o.rule.is_some(), o.quote), (false, None), "no rule broken, nothing to quote");
    }
}
