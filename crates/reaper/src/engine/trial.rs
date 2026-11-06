//! A rule on its way into the rules file, tried on more than its pattern can show
//! (`draft::check`): a `judge` rule's examples put to the watcher, and a pattern rule run over
//! the calls agents made in the person's own recent sessions, so they see what it would have
//! stopped before it stops anything.

use reaper_core::draft::{self, Draft, Report, Seen, Tried};
use reaper_core::paths::tilde;
use reaper_core::rules::{Config, Room, parse, portable};
use serde_json::Value;

use super::backend::{self, Backend};
use super::store::Store;
use super::watcher::{self, Settings};
use super::{glob_regex, places};
use crate::hosts::history::{self, Roots};

/// How many of the calls it would have stopped are shown.
const SHOWN: usize = 5;
/// How much of each is shown, on one line.
const WIDTH: usize = 100;

/// Trying a rule is as if the machine were short of room: one with `free` or `load` is in force.
const IN_FORCE: Room = Room { free: Some(0), load: Some(f64::INFINITY) };

/// `replacing`: a new version of a rule the file has (`reaper rules add --replace`).
pub fn check(draft: &Draft, rules: &str, replacing: bool) -> Report {
    let mut report = if replacing { draft::check_replacing(draft, rules) } else { draft::check(draft, rules) };
    let config = parse(rules).unwrap_or_default();
    if draft.rule.judge.is_some() {
        let store = Store::default();
        let backend = config.watcher.as_ref().and_then(|w| backend::from_config(w, &store));
        judge(draft, &mut report, backend.as_deref(), &store);
    } else {
        report.seen = seen(draft, &config, &Roots::here());
    }
    report
}

/// Each example put to the watcher, as the call would be. With `judge`, the watcher has the
/// last word, so its answers take the place of the pattern's.
fn judge(draft: &Draft, report: &mut Report, backend: Option<&dyn Backend>, store: &Store) {
    if draft.stops.is_empty() && draft.allows.is_empty() {
        return;
    }
    // Lint already says a judge rule needs a watcher.
    let Some(backend) = backend else { return };
    let settings = Settings { backend: Some(backend), kill_on_error: false, timeout: backend::BUDGET, room: IN_FORCE };
    let rules = [&draft.rule];
    let mut tried = Vec::new();
    for (examples, should_stop) in [(&draft.stops, true), (&draft.allows, false)] {
        for example in examples {
            let (tool, args) = example.call();
            let outcome = watcher::check(&rules, &[], "", &tool, &args, &settings, store);
            if let Some(error) = outcome.error {
                report
                    .warnings
                    .push(format!("the watcher didn't answer ({error}), so the examples weren't tried on it"));
                return;
            }
            let stopped = outcome.rule.is_some();
            tried.push(Tried { example: example.text(), should_stop, stopped, judged: outcome.judged });
        }
    }
    report.tried = tried;
}

/// The rule run over the calls in the person's recent sessions: those it looks at (its tools,
/// and for a project rule, those working in the project), and which of them it stops.
fn seen(draft: &Draft, config: &Config, roots: &Roots) -> Option<Seen> {
    let rule = &draft.rule;
    rule.pattern.as_ref()?;
    let paths = draft.project.as_ref().map(|name| config.projects.get(name).map_or(&draft.paths, |p| &p.paths));
    let globs: Option<Vec<regress::Regex>> = paths.map(|paths| paths.iter().filter_map(|g| glob_regex(g)).collect());
    let mut seen = Seen::default();
    seen.days = history::each(roots, |call| {
        if !rule.applies_to(call.tool) {
            return;
        }
        let args = portable(call.args);
        if let Some(globs) = &globs {
            let places = places(call.dir, &args);
            let inside = |re: &regress::Regex| places.iter().any(|p| re.find(&p.to_string_lossy()).is_some());
            if !globs.iter().any(inside) {
                return;
            }
        }
        seen.calls += 1;
        if rule.matches(call.tool, &args) {
            seen.stopped += 1;
            let line = one_line(&rule.fires_on(&args).unwrap_or_else(|| whole(call.tool, &args)));
            if seen.examples.len() < SHOWN && !seen.examples.contains(&line) {
                seen.examples.push(line);
            }
        }
    })?;
    Some(seen)
}

/// A call as the person would recognise it.
fn whole(tool: &str, args: &Value) -> String {
    match (args["command"].as_str(), args["path"].as_str()) {
        (Some(command), _) => command.to_string(),
        (None, Some(path)) => format!("{} {}", tool.to_lowercase(), tilde(std::path::Path::new(path))),
        _ => format!("{} {args}", tool.to_lowercase()),
    }
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > WIDTH { format!("{}…", flat.chars().take(WIDTH - 1).collect::<String>()) } else { flat }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::time::Duration;

    use super::super::prefix::Prompt;
    use super::*;
    use serde_json::json;

    /// A watcher that stops whatever mentions `prod`.
    struct Prod(RefCell<usize>);

    impl Backend for Prod {
        fn complete(&self, prompt: &Prompt, _: Duration, _: u32) -> Result<String, String> {
            *self.0.borrow_mut() += 1;
            Ok(if prompt.action.contains("prod") { r#"{"violated":["prod-db"]}"# } else { r#"{"violated":[]}"# }.into())
        }
    }

    struct Down;

    impl Backend for Down {
        fn complete(&self, _: &Prompt, _: Duration, _: u32) -> Result<String, String> {
            Err("connection refused".into())
        }
    }

    fn sandbox(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("reaper-trial-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    const JUDGED: &str = "rule:\n  id: prod-db\n  tools: [bash]\n  judge: Querying the production database.\n  redirect: Use the local database.\nstops: [psql prod-db.internal]\nallows: [psql localhost/app, cat prod-notes.md]\n";

    #[test]
    fn a_judge_rules_examples_are_put_to_the_watcher() {
        let root = sandbox("judge");
        let store = Store::at(&root);
        let draft = draft::read(JUDGED).unwrap();
        let mut report = draft::check(&draft, "");
        let watcher = Prod(RefCell::new(0));
        judge(&draft, &mut report, Some(&watcher), &store);
        let verdicts: Vec<_> = report.tried.iter().map(|t| (t.example.as_str(), t.stopped, t.judged)).collect();
        assert_eq!(
            verdicts,
            [
                ("psql prod-db.internal", true, true),
                ("psql localhost/app", false, true),
                ("cat prod-notes.md", true, true)
            ]
        );
        assert_eq!(report.failures(), ["`cat prod-notes.md` should be let through, but the watcher stopped it"]);
        assert_eq!(*watcher.0.borrow(), 3);

        // A fresh store: the one above would answer from its cache.
        let mut report = draft::check(&draft, "");
        judge(&draft, &mut report, Some(&Down), &Store::at(&sandbox("judge-down")));
        assert!(report.tried.is_empty() && report.ok());
        assert!(report.warnings.iter().any(|w| w.contains("connection refused")), "{:?}", report.warnings);
    }

    #[test]
    fn with_a_pattern_too_the_watcher_has_the_last_word() {
        let root = sandbox("both");
        let draft = draft::read(&JUDGED.replace("  judge:", "  match: { command: prod }\n  judge:")).unwrap();
        let report = draft::check(&draft, "");
        assert_eq!(report.tried.len(), 1, "the pattern alone can't say what's let through");
        let mut report = draft::check(&draft, "");
        judge(&draft, &mut report, Some(&Prod(RefCell::new(0))), &Store::at(&root));
        let missed = report.tried.iter().find(|t| t.example == "psql localhost/app").unwrap();
        assert!(!missed.stopped && !missed.judged, "the pattern let it through before the watcher saw it");
    }

    #[test]
    fn a_pattern_rule_is_run_over_recent_sessions_in_its_project() {
        let root = sandbox("seen");
        let roots = Roots { claude_code: root.join("projects"), pi: root.join("sessions") };
        fs::create_dir_all(roots.claude_code.join("p")).unwrap();
        let call = |dir: &str, command: &str| json!({ "type": "assistant", "cwd": dir, "message": { "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": command } }] } });
        let lines = [
            call("/w/app", "uv run pytest -x"),
            call("/w/app", "uv run pytest -x"),
            call("/w/app", "pytest tests/test_a.py"),
            call("/w/app", "cat pytest.ini"),
            call("/w/other", "pytest"),
            call("/w/other", "cd ../app &&\n  pytest -q"),
            json!({ "type": "assistant", "cwd": "/w/app", "message": { "content": [{ "type": "tool_use", "name": "Read", "input": { "file_path": "/w/app/x" } }] } }),
        ];
        let text: Vec<String> = lines.iter().map(Value::to_string).collect();
        fs::write(roots.claude_code.join("p/s.jsonl"), text.join("\n")).unwrap();
        let snippet = "project: app\npaths: [/w/app]\nrule:\n  id: suite\n  tools: [bash]\n  match: { command: '^pytest\\b(?!.*\\.py\\b)' }\n  redirect: Run the test files you changed.\n";
        let seen = seen(&draft::read(snippet).unwrap(), &Config::default(), &roots).unwrap();
        assert_eq!((seen.calls, seen.stopped), (5, 3), "{seen:?}");
        assert_eq!(seen.examples, ["uv run pytest -x", "pytest -q"], "the command it fires on, each once");
        assert_eq!(seen.summary(), "stops 3 of the 5 calls it looks at in your sessions from the last day");

        let global = draft::read(&snippet.replace("project: app\npaths: [/w/app]\n", "")).unwrap();
        assert_eq!(seen_in(&global, &roots), (6, 4), "every project's");
        let judged = draft::read(JUDGED).unwrap();
        assert!(super::seen(&judged, &Config::default(), &roots).is_none(), "the watcher would have to judge each");
    }

    fn seen_in(draft: &Draft, roots: &Roots) -> (usize, usize) {
        let seen = seen(draft, &Config::default(), roots).unwrap();
        (seen.calls, seen.stopped)
    }
}
