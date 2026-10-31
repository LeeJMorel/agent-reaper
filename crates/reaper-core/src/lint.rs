//! Checks a rules file before agents run into it: missing redirects, redirects phrased
//! as prohibitions, invalid patterns, and keys or values the engine would silently ignore.

use std::collections::HashSet;

use regress::Regex;

use crate::rules::{Config, Rule, Unknown, duration_ms, size_bytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub level: Level,
    /// `global` or the project name, when the issue belongs to a rule.
    pub place: Option<String>,
    pub rule: Option<String>,
    pub message: String,
}

/// Phrasings that hand the agent a "don't" to fixate on.
const NEGATIVE: &str = r"\b(don'?t|do not|never|avoid|stop|must not|mustn'?t|cannot|can'?t|shouldn'?t|should not|not allowed|forbidden|prohibited|instead of|refrain)\b";

const ACTIONS: &[&str] = &["redirect", "stop"];
const KINDS: &[&str] = &["openai", "anthropic"];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/')))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn echoes(redirect: &str, secret: &str) -> bool {
    let haystack = format!(" {} ", words(redirect).join(" "));
    words(secret).windows(4).any(|w| haystack.contains(&format!(" {} ", w.join(" "))))
}

struct Linter {
    negative: Regex,
    issues: Vec<Issue>,
}

impl Linter {
    fn push(&mut self, level: Level, place: Option<&str>, rule: Option<&str>, message: String) {
        self.issues.push(Issue { level, place: place.map(Into::into), rule: rule.map(Into::into), message });
    }

    fn unknown(&mut self, keys: &Unknown, place: Option<&str>, rule: Option<&str>, what: &str) {
        for key in keys.keys() {
            self.push(Level::Warn, place, rule, format!("unknown {what} key `{key}` is ignored"));
        }
    }

    fn rule(&mut self, rule: &Rule, place: &str) {
        let (p, id) = (Some(place), Some(rule.id.as_str()));
        if rule.id.is_empty() {
            self.push(Level::Error, p, None, "rule has no id".into());
        }
        if rule.pattern.is_none() && rule.judge.is_none() {
            self.push(Level::Error, p, id, "rule needs `match`, `judge`, or both".into());
        }
        for (key, pattern) in rule.pattern.iter().flatten() {
            if let Err(e) = Regex::new(pattern) {
                self.push(Level::Error, p, id, format!("match.{key} is not a valid regex: {e}"));
            }
        }
        if let Some(action) = &rule.action
            && !ACTIONS.contains(&action.as_str())
        {
            self.push(Level::Error, p, id, format!("action `{action}` is not one of: {}", ACTIONS.join(", ")));
        }
        if !rule.stops() {
            match &rule.redirect {
                None => self.push(
                    Level::Warn,
                    p,
                    id,
                    "no `redirect`: the agent will only be told to continue, and may repeat itself".into(),
                ),
                Some(redirect) => {
                    if let Some(m) = self.negative.find(redirect) {
                        let word = &redirect[m.range()];
                        self.push(Level::Warn, p, id, format!("redirect says \"{word}\": phrase it as what to do"));
                    }
                    if rule.judge.as_deref().is_some_and(|judge| echoes(redirect, judge)) {
                        self.push(
                            Level::Warn,
                            p,
                            id,
                            "redirect repeats the rule's description back to the agent".into(),
                        );
                    }
                }
            }
        }
        self.unknown(&rule.unknown, p, id, "rule");
    }
}

pub fn lint(config: &Config) -> Vec<Issue> {
    let mut l =
        Linter { negative: Regex::with_flags(NEGATIVE, "i").expect("NEGATIVE is a valid regex"), issues: Vec::new() };

    for (place, rule) in config.all_rules() {
        l.rule(rule, place.unwrap_or("global"));
    }

    let mut seen = HashSet::new();
    for (place, rule) in config.all_rules() {
        if !seen.insert(&rule.id) {
            l.push(Level::Error, Some(place.unwrap_or("global")), Some(&rule.id), "duplicate rule id".into());
        }
    }

    for (name, project) in &config.projects {
        if project.paths.is_empty() {
            l.push(Level::Error, None, None, format!("project {name} has no paths"));
        }
        l.unknown(&project.unknown, Some(name), None, "project");
    }

    match &config.watcher {
        None if config.all_rules().any(|(_, r)| r.judge.is_some()) => l.push(
            Level::Warn,
            None,
            None,
            "`judge` rules need a `watcher` backend; without one they never fire".into(),
        ),
        None => {}
        Some(w) => {
            if w.base_url.is_none() || w.model.is_none() {
                l.push(Level::Error, None, None, "watcher needs `baseURL` and `model`".into());
            }
            if let Some(kind) = &w.kind
                && !KINDS.contains(&kind.as_str())
            {
                l.push(Level::Error, None, None, format!("watcher kind `{kind}` is not one of: {}", KINDS.join(", ")));
            }
            l.unknown(&w.unknown, None, None, "watcher");
        }
    }
    if config.repeats == 1 {
        l.push(Level::Error, None, None, "repeats: 1 would stop every call; use 2 or more, or 0 for off".into());
    }
    if let Some(reap) = &config.reap {
        if let Some(idle) = reap.idle.as_deref().filter(|i| !matches!(*i, "off" | "auto") && duration_ms(i).is_none()) {
            l.push(
                Level::Error,
                None,
                None,
                format!(
                    "reap.idle `{idle}` is not a duration like 90s, 5m or 1h (or auto, or off); idle sessions are kept"
                ),
            );
        }
        if reap.live == Some(0) {
            l.push(
                Level::Error,
                None,
                None,
                "reap.live is 0: keep at least one session running, or leave it out".into(),
            );
        }
        if let Some(free) = reap.free.as_deref() {
            if size_bytes(free).is_none() {
                l.push(Level::Error, None, None, format!("reap.free `{free}` is not a size like 512M or 2G"));
            } else if !cfg!(target_os = "linux") {
                l.push(Level::Warn, None, None, "reap.free reads /proc/meminfo, so it only works on Linux".into());
            }
        }
        l.unknown(&reap.unknown, None, None, "reap");
    }
    l.unknown(&config.unknown, None, None, "top-level");
    l.issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse;

    fn messages(issues: &[Issue], id: &str) -> String {
        issues
            .iter()
            .filter(|i| i.rule.as_deref() == Some(id))
            .map(|i| i.message.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn same_findings_as_the_engine_lint() {
        let issues = lint(
            &parse(
                r#"
rules:
  - { id: a, match: { command: x }, redirect: "Don't push to main." }
  - { id: b, judge: "editing files in the generated client folder", redirect: "Leave the generated client folder alone; edit the schema." }
  - { id: c, match: { command: "(" }, redirect: Use the script. }
  - { id: c, match: { command: y }, redirect: Run the tests. }
"#,
            )
            .unwrap(),
        );
        assert!(messages(&issues, "a").contains("Don't"));
        assert!(messages(&issues, "b").contains("repeats"));
        assert!(messages(&issues, "c").contains("not a valid regex"));
        assert!(issues.iter().any(|i| i.message == "duplicate rule id"));
        assert!(issues.iter().any(|i| i.message.contains("watcher")));
    }

    #[test]
    fn javascript_regex_syntax_is_accepted() {
        // Lookbehind and named groups are valid in the engine (JavaScript) even where other flavours differ.
        let issues = lint(
            &parse(r#"rules: [{ id: a, match: { command: '(?<=git )push(?<force> -f)?' }, redirect: Open a PR. }]"#)
                .unwrap(),
        );
        assert_eq!(issues, vec![]);
    }

    #[test]
    fn typos_and_bad_values_are_caught() {
        let issues = lint(
            &parse(
                r#"
watcher: { kind: openia, model: m, timeout: 5 }
reap: { idle: 5 minutes, after: 1, live: 0, free: lots }
repeats: 1
rule: []
projects:
  app:
    path: [~/app]
    rules:
      - { id: x, match: { command: x }, redirects: Run it locally., action: kill }
"#,
            )
            .unwrap(),
        );
        let all: Vec<_> = issues.iter().map(|i| i.message.as_str()).collect();
        for expected in [
            "watcher needs `baseURL` and `model`",
            "watcher kind `openia` is not one of: openai, anthropic",
            "unknown watcher key `timeout` is ignored",
            "reap.idle `5 minutes` is not a duration like 90s, 5m or 1h (or auto, or off); idle sessions are kept",
            "unknown reap key `after` is ignored",
            "reap.live is 0: keep at least one session running, or leave it out",
            "reap.free `lots` is not a size like 512M or 2G",
            "repeats: 1 would stop every call; use 2 or more, or 0 for off",
            "unknown top-level key `rule` is ignored",
            "unknown project key `path` is ignored",
            "project app has no paths",
            "unknown rule key `redirects` is ignored",
            "action `kill` is not one of: redirect, stop",
        ] {
            assert!(all.contains(&expected), "missing {expected:?} in {all:#?}");
        }
    }

    #[test]
    fn the_shipped_example_is_clean() {
        assert_eq!(lint(&parse(crate::EXAMPLE_RULES).unwrap()), vec![]);
    }
}
