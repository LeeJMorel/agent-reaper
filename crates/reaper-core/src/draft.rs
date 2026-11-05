//! A rule someone asked for, on its way into the rules file. An agent writes it as a snippet
//! (crates/reaper/SKILL.md) with examples of what it must stop and let through; reaper
//! tries the examples, lints the rule where it will live, and adds it to the file without
//! disturbing what's there: comments, order and layout stay as the human left them.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::lint::{Level, lint};
use crate::rules::{Config, Rule, Unknown, parse};

#[derive(Debug, Clone, Deserialize)]
pub struct Draft {
    /// The project it's for; `None` for every project.
    pub project: Option<String>,
    /// Where a new project lives. Ignored for one that exists.
    #[serde(default)]
    pub paths: Vec<String>,
    pub rule: Rule,
    #[serde(default)]
    pub stops: Vec<Example>,
    #[serde(default)]
    pub allows: Vec<Example>,
    /// What the writer assumed, for the human to read.
    pub note: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// An action to try the rule on: a shell command, or a tool's arguments with its name as `tool`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Example {
    Command(String),
    Call(BTreeMap<String, Value>),
}

impl Example {
    /// The tool and its arguments. A call that names no tool is a command if it has one, else a write.
    fn call(&self) -> (String, Value) {
        match self {
            Example::Command(command) => ("bash".into(), json!({ "command": command })),
            Example::Call(fields) => {
                let mut args: Map<String, Value> = fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let named = args.remove("tool").and_then(|t| t.as_str().map(str::to_string));
                let tool = named.unwrap_or_else(|| if args.contains_key("command") { "bash" } else { "write" }.into());
                (tool, Value::Object(args))
            }
        }
    }

    pub fn text(&self) -> String {
        match self {
            Example::Command(command) => command.clone(),
            Example::Call(fields) => fields
                .iter()
                .map(|(k, v)| format!("{k}: {}", v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// One example tried against the rule's pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Tried {
    pub example: String,
    pub should_stop: bool,
    pub stopped: bool,
}

impl Tried {
    pub fn passed(&self) -> bool {
        self.should_stop == self.stopped
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Why it can't be added as it is.
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub tried: Vec<Tried>,
    /// The rules file with the rule added, when nothing stands in the way.
    pub text: Option<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.text.is_some() && self.tried.iter().all(Tried::passed)
    }

    /// Everything that keeps it out, in words an agent can fix it from.
    pub fn failures(&self) -> Vec<String> {
        let mut out = self.problems.clone();
        for t in self.tried.iter().filter(|t| !t.passed()) {
            let (should, did) = if t.should_stop { ("stopped", "let through") } else { ("let through", "stopped") };
            out.push(format!("`{}` should be {should}, but the pattern {did} it", t.example));
        }
        out
    }
}

/// The snippet in an agent's reply: inside its ```yaml fence, if it has one.
pub fn read(reply: &str) -> Result<Draft, String> {
    let text = fenced(reply).unwrap_or(reply);
    serde_saphyr::from_str::<Draft>(text).map_err(|e| {
        let full = e.to_string();
        let first = full.lines().next().unwrap_or("invalid YAML");
        format!("not a rule snippet: {}", first.strip_prefix("error: ").unwrap_or(first))
    })
}

fn fenced(text: &str) -> Option<&str> {
    let body = &text[text.find("```")? + 3..];
    let body = &body[body.find('\n')? + 1..];
    Some(&body[..body.find("```")?])
}

/// Tries the draft against its examples and against the rules file's text as it is now.
pub fn check(draft: &Draft, rules: &str) -> Report {
    let mut report = Report::default();
    let old = match parse(rules) {
        Ok(config) => config,
        Err(e) => {
            let at = e.line.map(|l| format!(" at line {l}")).unwrap_or_default();
            report.problems.push(format!("the rules file has a syntax error{at}; it has to be fixed first"));
            return report;
        }
    };
    let rule = &draft.rule;
    if rule.id.trim().is_empty() {
        report.problems.push("the rule has no id".into());
    }
    report.warnings.extend(draft.unknown.keys().map(|k| format!("unknown key `{k}` is ignored")));
    report.warnings.extend(rule.unknown.keys().map(|k| format!("unknown rule key `{k}` is ignored")));

    if rule.pattern.is_some() {
        for (examples, should_stop) in [(&draft.stops, true), (&draft.allows, false)] {
            for example in examples {
                let (tool, args) = example.call();
                let stopped = rule.matches(&tool, &args);
                report.tried.push(Tried { example: example.text(), should_stop, stopped });
            }
        }
        if draft.stops.is_empty() {
            report.warnings.push("no examples of what it stops, so the pattern is untried".into());
        }
    } else if !draft.stops.is_empty() || !draft.allows.is_empty() {
        report.warnings.push("the watcher model judges this rule as it happens, so the examples aren't tried".into());
    }

    let text = match insert(rules, draft) {
        Ok(text) => text,
        Err(e) => {
            report.problems.push(e);
            return report;
        }
    };
    let by_hand = "couldn't add it to the rules file without changing anything else; add it by hand";
    match parse(&text) {
        Ok(new) if new.to_json() == expected(&old, draft) => {
            // Issues the file had already aren't this rule's to answer for.
            let before: Vec<String> = lint(&old).into_iter().filter(|i| i.rule.is_none()).map(|i| i.message).collect();
            for issue in lint(&new) {
                let ours = issue.rule.as_deref() == Some(rule.id.as_str());
                if ours || (issue.rule.is_none() && !before.contains(&issue.message)) {
                    match issue.level {
                        Level::Error => report.problems.push(issue.message),
                        Level::Warn => report.warnings.push(issue.message),
                    }
                }
            }
        }
        _ => report.problems.push(by_hand.into()),
    }
    if report.problems.is_empty() {
        report.text = Some(text);
    }
    report
}

/// The rules as JSON once the draft's rule is in: `old` with it at the end of its list.
fn expected(old: &Config, draft: &Draft) -> Value {
    let mut json = old.to_json();
    let list = match &draft.project {
        None => &mut json["rules"],
        Some(name) => {
            let projects = json["projects"].as_object_mut().expect("projects is an object");
            &mut projects.entry(name.clone()).or_insert_with(|| json!({ "paths": draft.paths, "rules": [] }))["rules"]
        }
    };
    if let Some(list) = list.as_array_mut() {
        list.push(draft.rule.to_json());
    }
    json
}

/// Writes the rules file whole, so a check that reads it meanwhile never sees half a rule. A
/// rules file that links elsewhere (dotfiles) is written where it links to.
pub fn save(path: &Path, text: &str) -> io::Result<()> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let new = path.with_extension("yaml.new");
    fs::write(&new, text)?;
    fs::rename(&new, &path)
}

// ── putting it in the file ──────────────────────────────────────────────────

/// How a key's value is written.
enum Form {
    /// Below it, indented.
    Block,
    /// `[]` or `{}`: nothing yet.
    Empty,
    /// On the key's own line.
    Inline,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn content(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#')
}

fn item(line: &str) -> bool {
    line.trim_start().starts_with("- ")
}

/// `name:` at exactly `at` columns, among `lines[from..to]`.
fn key(lines: &[String], from: usize, to: usize, at: usize, name: &str) -> Option<(usize, Form)> {
    (from..to).find_map(|i| {
        let line = &lines[i];
        if !content(line) || indent(line) != at {
            return None;
        }
        let rest = line.trim_start();
        let spellings = [format!("{name}:"), format!("\"{name}\":"), format!("'{name}':")];
        let rest = spellings.iter().find_map(|k| rest.strip_prefix(k.as_str()))?;
        let value = rest.split(" #").next().unwrap_or_default().trim();
        Some((
            i,
            match value {
                "" => Form::Block,
                "[]" | "{}" => Form::Empty,
                _ => Form::Inline,
            },
        ))
    })
}

/// Where the block under the key on line `i` ends: at the next line with content indented no
/// deeper than the key. A list item at the key's own indentation still belongs to it.
fn end(lines: &[String], i: usize, to: usize) -> usize {
    let at = indent(&lines[i]);
    (i + 1..to)
        .find(|&j| content(&lines[j]) && (indent(&lines[j]) < at || indent(&lines[j]) == at && !item(&lines[j])))
        .unwrap_or(to)
}

/// The block's last line. A comment indented under the key is part of it, so a commented-out
/// rule keeps its place; one at the key's level belongs to what follows.
fn last(lines: &[String], i: usize, end: usize) -> usize {
    let at = indent(&lines[i]);
    (i + 1..end)
        .rev()
        .find(|&j| content(&lines[j]) || indent(&lines[j]) > at && !lines[j].trim().is_empty())
        .unwrap_or(i)
}

/// The rule as a list item under the key on line `i`, after the block's last line, indented
/// and spaced like the items already there.
fn add_item(lines: &mut Vec<String>, i: usize, rule: &Rule) {
    let stop = end(lines, i, lines.len());
    let at = last(lines, i, stop);
    let items: Vec<usize> = (i + 1..stop).filter(|&j| item(&lines[j])).collect();
    let dash = items.first().map_or(indent(&lines[i]) + 2, |&j| indent(&lines[j]));
    let spaced = (i + 1..at).any(|j| lines[j].trim().is_empty());
    let mut block = if spaced && at > i { vec![String::new()] } else { Vec::new() };
    block.extend(render(rule, dash));
    lines.splice(at + 1..at + 1, block);
}

/// A new top-level section at the end of the file.
fn append(lines: &mut Vec<String>, block: Vec<String>) {
    if lines.last().is_some_and(|l| !l.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.extend(block);
}

/// The rules file's text with the draft's rule added at the end of its list: the global one,
/// or its project's, made if new. Lists written in block style (as in the example) are
/// edited; one written on a single line is left for the human.
pub fn insert(text: &str, draft: &Draft) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let by_hand = |what: &str| format!("{what} is written on one line in the rules file; add the rule by hand");
    let top = lines.len();
    match &draft.project {
        None => match key(&lines, 0, top, 0, "rules") {
            Some((i, Form::Block)) => add_item(&mut lines, i, &draft.rule),
            Some((i, Form::Empty)) => {
                lines[i] = "rules:".into();
                add_item(&mut lines, i, &draft.rule);
            }
            Some((_, Form::Inline)) => return Err(by_hand("`rules`")),
            None => {
                append(&mut lines, vec!["rules:".into()]);
                let i = lines.len() - 1;
                add_item(&mut lines, i, &draft.rule);
            }
        },
        Some(name) => {
            let project = |pad: usize| -> Result<Vec<String>, String> {
                if draft.paths.is_empty() {
                    return Err(format!("{name} is a new project, so it needs `paths`"));
                }
                let p = " ".repeat(pad);
                let paths: Vec<String> = draft.paths.iter().map(|path| flow(path)).collect();
                let mut block = vec![
                    format!("{p}{}:", scalar(name)),
                    format!("{p}  paths: [{}]", paths.join(", ")),
                    format!("{p}  rules:"),
                ];
                block.extend(render(&draft.rule, pad + 4));
                Ok(block)
            };
            match key(&lines, 0, top, 0, "projects") {
                None => {
                    let mut block = vec!["projects:".to_string()];
                    block.extend(project(2)?);
                    append(&mut lines, block);
                }
                Some((i, Form::Empty)) => {
                    lines[i] = "projects:".into();
                    lines.splice(i + 1..i + 1, project(2)?);
                }
                Some((_, Form::Inline)) => return Err(by_hand("`projects`")),
                Some((i, Form::Block)) => {
                    let stop = end(&lines, i, top);
                    let child = (i + 1..stop).find(|&j| content(&lines[j])).map_or(2, |j| indent(&lines[j]));
                    match key(&lines, i + 1, stop, child, name) {
                        None => {
                            let at = last(&lines, i, stop);
                            lines.splice(at + 1..at + 1, project(child)?);
                        }
                        Some((_, Form::Inline | Form::Empty)) => return Err(by_hand(&format!("project {name}"))),
                        Some((p, Form::Block)) => {
                            let own = end(&lines, p, stop);
                            let at =
                                (p + 1..own).find(|&j| content(&lines[j])).map_or(child + 2, |j| indent(&lines[j]));
                            match key(&lines, p + 1, own, at, "rules") {
                                Some((r, Form::Block)) => add_item(&mut lines, r, &draft.rule),
                                Some((r, Form::Empty)) => {
                                    lines[r] = format!("{}rules:", " ".repeat(at));
                                    add_item(&mut lines, r, &draft.rule);
                                }
                                Some((_, Form::Inline)) => return Err(by_hand(&format!("{name}'s `rules`"))),
                                None => {
                                    let after = last(&lines, p, own);
                                    let mut block = vec![format!("{}rules:", " ".repeat(at))];
                                    block.extend(render(&draft.rule, at + 2));
                                    lines.splice(after + 1..after + 1, block);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(lines.join("\n") + "\n")
}

/// The rule as a block-style list item with its dash at `dash` columns, keys in the order
/// the example uses.
fn render(rule: &Rule, dash: usize) -> Vec<String> {
    let mut fields = vec![format!("id: {}", scalar(&rule.id))];
    if let Some(tools) = &rule.tools {
        fields.push(format!("tools: [{}]", tools.iter().map(|t| flow(t)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(pattern) = &rule.pattern {
        let entries: Vec<String> = pattern.iter().map(|(k, v)| format!("{}: {}", flow(k), quoted(v))).collect();
        fields.push(format!("match: {{ {} }}", entries.join(", ")));
    }
    let mut put = |key: &str, value: Option<String>| fields.extend(value.map(|v| format!("{key}: {v}")));
    put("judge", rule.judge.as_deref().map(scalar));
    put("free", rule.free.as_deref().map(scalar));
    put("action", rule.action.as_deref().map(scalar));
    put("strikes", rule.strikes.map(|n| n.to_string()));
    put("redirect", rule.redirect.as_deref().map(scalar));
    let pad = " ".repeat(dash);
    fields
        .into_iter()
        .enumerate()
        .map(|(n, field)| if n == 0 { format!("{pad}- {field}") } else { format!("{pad}  {field}") })
        .collect()
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Plain when YAML is sure to read it back as the same string, else single-quoted.
fn scalar(text: &str) -> String {
    if plain(text, false) { text.to_string() } else { quoted(text) }
}

/// The same, inside `[…]` or `{…}`.
fn flow(text: &str) -> String {
    if plain(text, true) { text.to_string() } else { quoted(text) }
}

fn plain(text: &str, in_flow: bool) -> bool {
    text.chars().next().is_some_and(|c| c.is_alphanumeric() || "~/._".contains(c))
        && !text.ends_with(|c: char| c.is_whitespace() || c == ':')
        && !text.contains(": ")
        && !text.contains(" #")
        && !text.contains(['\n', '\t'])
        && !(in_flow && text.contains([',', '[', ']', '{', '}']))
        && !matches!(text.to_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~")
        && text.parse::<f64>().is_err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EXAMPLE_RULES;

    const SUITE: &str = r#"Here's the rule:

```yaml
rule:
  id: whole-test-suite
  tools: [bash]
  match: { command: '(^|[;&|(]\s*)(npx |uv run )?(pytest|vitest)\b(?![^;&|]*(::|\.(py|[jt]sx?)\b|\$))' }
  free: 4G
  redirect: Run the test files for the code you changed.
stops:
  - pytest
  - cd backend && uv run pytest -n 4 --dist loadgroup
allows:
  - pytest app/users_test.py
  - cat pytest.ini
note: Assumed every project.
```
"#;

    fn draft(snippet: &str) -> Draft {
        read(snippet).unwrap()
    }

    #[test]
    fn reads_the_snippet_out_of_a_reply_and_tries_its_examples() {
        let d = draft(SUITE);
        assert_eq!((d.rule.id.as_str(), d.note.as_deref()), ("whole-test-suite", Some("Assumed every project.")));
        let report = check(&d, EXAMPLE_RULES);
        assert!(report.ok(), "{:?}", report.failures());
        assert_eq!(report.tried.iter().filter(|t| t.should_stop && t.stopped).count(), 2);

        let leaky = draft(&SUITE.replace("  - cat pytest.ini", "  - cat pytest.ini\n  - uv run pytest -x"));
        let report = check(&leaky, EXAMPLE_RULES);
        assert!(!report.ok());
        assert_eq!(report.failures(), ["`uv run pytest -x` should be let through, but the pattern stopped it"]);

        assert!(read("Sorry, I can't help with that.").unwrap_err().starts_with("not a rule snippet"));
    }

    #[test]
    fn a_global_rule_goes_after_the_last_one_and_nothing_else_moves() {
        let text = check(&draft(SUITE), EXAMPLE_RULES).text.unwrap();
        let block = r"
  - id: whole-test-suite
    tools: [bash]
    match: { command: '(^|[;&|(]\s*)(npx |uv run )?(pytest|vitest)\b(?![^;&|]*(::|\.(py|[jt]sx?)\b|\$))' }
    free: 4G
    redirect: Run the test files for the code you changed.
";
        assert_eq!(text.replacen(block, "", 1), EXAMPLE_RULES, "only the rule is new");
        let at = text.find(block).unwrap();
        assert!(text.find("#   redirect: Run the test files").unwrap() < at, "after the commented-out example");
        assert!(at < text.find("# Project rules apply").unwrap(), "before the next section's comment");
    }

    #[test]
    fn a_project_rule_joins_its_project_or_makes_one() {
        let mine = SUITE.replace("rule:\n", "project: my-app\nrule:\n");
        let text = check(&draft(&mine), EXAMPLE_RULES).text.unwrap();
        let config = parse(&text).unwrap();
        assert_eq!(config.projects["my-app"].rules.last().unwrap().id, "whole-test-suite");
        assert!(text.ends_with("    redirect: Run the test files for the code you changed.\n"), "the last project");

        let new = SUITE.replace("rule:\n", "project: api\npaths: [~/code/api, ~/code/api-worktrees/*]\nrule:\n");
        let text = check(&draft(&new), EXAMPLE_RULES).text.unwrap();
        assert!(text.contains(
            "  api:\n    paths: [~/code/api, ~/code/api-worktrees/*]\n    rules:\n      - id: whole-test-suite\n"
        ));

        let pathless = SUITE.replace("rule:\n", "project: api\nrule:\n");
        assert_eq!(check(&draft(&pathless), EXAMPLE_RULES).problems, ["api is a new project, so it needs `paths`"]);
    }

    #[test]
    fn any_block_style_file_takes_a_rule() {
        let d = draft(SUITE);
        for (before, has) in [
            ("", "rules:\n  - id: whole-test-suite\n"),
            ("# mine\nstrikes: 2\n", "strikes: 2\n\nrules:\n  - id: whole-test-suite\n"),
            ("rules: []\nstrikes: 2\n", "rules:\n  - id: whole-test-suite\n"),
            (
                "rules:\n- id: a\n  match: { command: x }\n  redirect: Do y.\nstrikes: 2\n",
                "  redirect: Do y.\n- id: whole-test-suite\n",
            ),
            ("projects:\n  app:\n    paths: [~/app]\n", "  app:\n    paths: [~/app]\n\nrules:\n"),
        ] {
            let report = check(&d, before);
            let text = report.text.unwrap_or_else(|| panic!("{before:?}: {:?}", report.problems));
            assert!(text.contains(has), "{before:?} became {text:?}");
        }
        let inline = check(&d, "rules: [{ id: a, match: { command: x }, redirect: Do y. }]\n");
        assert_eq!(inline.problems, ["`rules` is written on one line in the rules file; add the rule by hand"]);
        let broken = check(&d, "rules: [ {{\n");
        assert!(broken.problems[0].starts_with("the rules file has a syntax error"));
    }

    #[test]
    fn awkward_text_survives_the_trip_and_lint_has_its_say() {
        let snippet = r#"
rule:
  id: secrets
  match: { "*": '(^|/)\.env(\.|$)', command: "it's" }
  judge: "Reading secrets: keys, tokens #and the like"
  redirect: "Don't read secrets: use the settings module."
stops: [cat .env]
"#;
        let report = check(&draft(snippet), EXAMPLE_RULES);
        assert!(report.problems.iter().any(|p| p == "duplicate rule id"), "{:?}", report.problems);

        let renamed = snippet.replace("id: secrets", "id: env-files");
        let report = check(&draft(&renamed), EXAMPLE_RULES);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert!(
            report.warnings.iter().any(|w| w.contains("Don't")),
            "lint's warnings are passed on: {:?}",
            report.warnings
        );
        let added = parse(&report.text.unwrap()).unwrap().rules.last().unwrap().to_json();
        assert_eq!(added, draft(&renamed).rule.to_json(), "the same rule, read back");
    }
}
