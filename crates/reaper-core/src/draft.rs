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

use crate::EXAMPLE_RULES;
use crate::lint::{Level, lint};
use crate::rules::{Config, Rule, Unknown, Watcher, parse, portable};

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
    pub fn call(&self) -> (String, Value) {
        match self {
            Example::Command(command) => ("bash".into(), portable(&json!({ "command": command }))),
            Example::Call(fields) => {
                let mut args: Map<String, Value> = fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let named = args.remove("tool").and_then(|t| t.as_str().map(str::to_string));
                let tool = named.unwrap_or_else(|| if args.contains_key("command") { "bash" } else { "write" }.into());
                (tool, portable(&Value::Object(args)))
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

/// One example tried against the rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Tried {
    pub example: String,
    pub should_stop: bool,
    pub stopped: bool,
    /// Decided by the watcher, not the pattern.
    pub judged: bool,
}

impl Tried {
    pub fn passed(&self) -> bool {
        self.should_stop == self.stopped
    }
}

/// What the rule would have stopped in the person's own recent sessions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    /// The calls it looks at (its tools, in its project), from sessions this many days back.
    pub calls: usize,
    pub days: u64,
    /// How many of them it stops, and a few of those, each once.
    pub stopped: usize,
    pub examples: Vec<String>,
}

impl Seen {
    /// What it found, in a line: "stops 41 of the 9312 calls it looks at in your sessions from
    /// the last 30 days".
    pub fn summary(&self) -> String {
        let days = if self.days == 1 { "day".to_string() } else { format!("{} days", self.days) };
        match (self.calls, self.stopped) {
            (0, _) => format!("your sessions from the last {days} have no calls it looks at"),
            (calls, 0) => format!("stops none of the {calls} calls it looks at in your sessions from the last {days}"),
            (calls, n) => format!("stops {n} of the {calls} calls it looks at in your sessions from the last {days}"),
        }
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
    /// The rule tried on recent sessions, when there were any to try it on.
    pub seen: Option<Seen>,
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
            let by = if t.judged { "the watcher" } else { "the pattern" };
            out.push(format!("`{}` should be {should}, but {by} {did} it", t.example));
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
    check_as(draft, rules, false)
}

/// The same for a new version of a rule the file has: it takes the old one's place.
pub fn check_replacing(draft: &Draft, rules: &str) -> Report {
    check_as(draft, rules, true)
}

fn check_as(draft: &Draft, rules: &str, replacing: bool) -> Report {
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

    // With `judge` too, the pattern only picks what the watcher is asked about: it must hit
    // everything the rule stops, and whatever it lets through the watcher decides (the engine's
    // `trial` asks it).
    if rule.pattern.is_some() {
        let allows = if rule.judge.is_some() { &[][..] } else { &draft.allows[..] };
        for (examples, should_stop) in [(&draft.stops[..], true), (allows, false)] {
            for example in examples {
                let (tool, args) = example.call();
                let stopped = rule.matches(&tool, &args);
                report.tried.push(Tried { example: example.text(), should_stop, stopped, judged: false });
            }
        }
        if draft.stops.is_empty() {
            report.warnings.push("no examples of what it stops, so the pattern is untried".into());
        }
    }

    let (text, want) = if replacing {
        (replace(rules, draft), expected_replaced(&old, draft))
    } else {
        (insert(rules, draft), expected(&old, draft))
    };
    let text = match text {
        Ok(text) => text,
        Err(e) => {
            report.problems.push(e);
            return report;
        }
    };
    let by_hand = "couldn't add it to the rules file without changing anything else; add it by hand";
    match parse(&text) {
        Ok(new) if new.to_json() == want => {
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

/// The rules file's text without the rule `id`, the rest as it was. Checked the same way as an
/// insert: read back, the file must be the old one less that rule and nothing else.
pub fn remove(text: &str, id: &str) -> Result<String, String> {
    let old = parse(text).map_err(|_| "the rules file has a syntax error; it has to be fixed first".to_string())?;
    let place = old.all_rules().find(|(_, r)| r.id == id).map(|(p, _)| p.map(str::to_string));
    let Some(place) = place else { return Err(format!("there is no rule {id}")) };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let Some((at, through, dash)) = item_lines(&lines, id) else {
        return Err(format!("{id} isn't written as a list item of its own; remove it by hand"));
    };
    lines.drain(at..=through);
    // One blank line between neighbours, not two.
    if at > 0 && lines[at - 1].trim().is_empty() && lines.get(at).is_none_or(|l| l.trim().is_empty()) {
        lines.remove(at - 1);
    }
    // A list left with nothing in it is written empty, not as a bare key. A project left with
    // no rules goes whole: it was only ever there to scope them.
    let mut emptied = None;
    if let Some(key) = (0..at.min(lines.len())).rev().find(|&j| content(&lines[j]) && indent(&lines[j]) < dash.max(1)) {
        let after = (key + 1..lines.len()).find(|&j| content(&lines[j]));
        let empty = after.is_none_or(|j| indent(&lines[j]) <= indent(&lines[key]) && !item(&lines[j]));
        let project = place
            .as_ref()
            .and_then(|_| (0..key).rev().find(|&j| content(&lines[j]) && indent(&lines[j]) < indent(&lines[key])));
        match project {
            Some(p) if empty && lines[key].trim_end().ends_with("rules:") => {
                let at = indent(&lines[p]);
                let through = last(&lines, p, end(&lines, p, lines.len()));
                lines.drain(p..=through);
                // No gap left right under its parent, and one at most between neighbours.
                let blank = |lines: &[String], j: usize| lines.get(j).is_some_and(|l| l.trim().is_empty());
                if blank(&lines, p) && (p == 0 || blank(&lines, p - 1) || lines[p - 1].trim_end().ends_with(':')) {
                    lines.remove(p);
                } else if p > 0 && blank(&lines, p - 1) && (p == lines.len() || blank(&lines, p)) {
                    lines.remove(p - 1);
                }
                let parent = (0..p.min(lines.len())).rev().find(|&j| content(&lines[j]) && indent(&lines[j]) < at);
                if let Some(q) = parent.filter(|&q| lines[q].trim_end().ends_with("projects:")) {
                    let next = (q + 1..lines.len()).find(|&j| content(&lines[j]));
                    if next.is_none_or(|j| indent(&lines[j]) <= indent(&lines[q])) {
                        lines[q] = format!("{} {{}}", lines[q].trim_end());
                    }
                }
                emptied = place.clone();
            }
            _ if empty && lines[key].trim_end().ends_with("rules:") => {
                lines[key] = format!("{} []", lines[key].trim_end());
            }
            _ => {}
        }
    }
    let new_text = lines.join("\n") + "\n";
    let mut want = old.to_json();
    let list = match &place {
        None => &mut want["rules"],
        Some(name) => &mut want["projects"][name.as_str()]["rules"],
    };
    if let Some(list) = list.as_array_mut() {
        list.retain(|r| r["id"] != id);
    }
    if let (Some(name), Some(projects)) = (&emptied, want["projects"].as_object_mut()) {
        projects.remove(name);
    }
    match parse(&new_text) {
        Ok(new) if new.to_json() == want => Ok(new_text),
        _ => Err(format!("couldn't take {id} out of the rules file without changing anything else; remove it by hand")),
    }
}

/// Where the rule `id` is written: its first and last lines, and its dash's column. Its list
/// item is `- id: x` in block style or `- { id: x, … }` on one line, never a comment, and runs to
/// the next line with content at its dash or left of it. A comment at the dash's level belongs
/// to what follows.
fn item_lines(lines: &[String], id: &str) -> Option<(usize, usize, usize)> {
    let quoted = |s: &str| [s.to_string(), format!("\"{s}\""), format!("'{s}'")];
    let named = |line: &str| {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("- ") else { return false };
        let rest = rest.trim_start().trim_start_matches("{ ").trim_start();
        let Some(value) = rest.strip_prefix("id:") else { return false };
        let value = value.trim_start();
        quoted(id).iter().any(|q| {
            value
                .strip_prefix(q.as_str())
                .is_some_and(|after| after.is_empty() || after.starts_with([' ', ',', '}', '#']))
        })
    };
    let at = lines.iter().position(|l| content(l) && named(l))?;
    let dash = indent(&lines[at]);
    let stop = (at + 1..lines.len()).find(|&j| content(&lines[j]) && indent(&lines[j]) <= dash).unwrap_or(lines.len());
    let through =
        (at + 1..stop).rev().find(|&j| content(&lines[j]) || indent(&lines[j]) > dash && !lines[j].trim().is_empty());
    Some((at, through.unwrap_or(at), dash))
}

/// The rules file's text with the rule of the draft's id written over where it is: the same
/// place in the same list, the comments around it as they were. Its project has to stay the
/// same, since the id is what says which rule it is.
pub fn replace(text: &str, draft: &Draft) -> Result<String, String> {
    let id = draft.rule.id.as_str();
    let old = parse(text).map_err(|_| "the rules file has a syntax error; it has to be fixed first".to_string())?;
    let Some(place) = old.all_rules().find(|(_, r)| r.id == id).map(|(p, _)| p.map(str::to_string)) else {
        return Err(format!("there is no rule {id} to replace; add it without --replace"));
    };
    if place != draft.project {
        let whose = place.as_deref().map_or("every project's".to_string(), |name| format!("{name}'s"));
        return Err(format!("{id} is {whose} rule, and its new version has to be too"));
    }
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let Some((at, through, dash)) = item_lines(&lines, id) else {
        return Err(format!("{id} isn't written as a list item of its own; change it by hand"));
    };
    lines.splice(at..=through, render(&draft.rule, dash));
    Ok(lines.join("\n") + "\n")
}

/// The rules as JSON once the draft's rule is in place of the one with its id.
fn expected_replaced(old: &Config, draft: &Draft) -> Value {
    let mut json = old.to_json();
    let list = match &draft.project {
        None => &mut json["rules"],
        Some(name) => &mut json["projects"][name.as_str()]["rules"],
    };
    if let Some(rule) = list.as_array_mut().and_then(|l| l.iter_mut().find(|r| r["id"] == draft.rule.id.as_str())) {
        *rule = draft.rule.to_json();
    }
    json
}

/// A new rules file: the example, its sample project commented out, so it shows how a project
/// is written without applying to anything of the person's.
pub fn starter() -> String {
    let mut lines: Vec<String> = EXAMPLE_RULES.lines().map(str::to_string).collect();
    if let Some((i, _)) = key(&lines, 0, lines.len(), 0, "projects") {
        let through = last(&lines, i, end(&lines, i, lines.len()));
        for line in lines[i..=through].iter_mut().filter(|l| content(l)) {
            line.insert_str(indent(line), "# ");
        }
        lines.insert(i, "# A sample project: uncomment it and put in your own paths.".into());
    }
    lines.join("\n") + "\n"
}

/// The rules file's text with its watcher set to `watcher`, or taken out, the rest as it was.
/// Checked like an insert: read back, only the watcher may have changed. A new one goes above
/// the file's first key, below its opening comments.
pub fn set_watcher(text: &str, watcher: Option<&Watcher>) -> Result<String, String> {
    let old = parse(text).map_err(|_| "the rules file has a syntax error; it has to be fixed first".to_string())?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let block = watcher.map(render_watcher).unwrap_or_default();
    match key(&lines, 0, lines.len(), 0, "watcher") {
        Some((i, _)) => {
            let through = last(&lines, i, end(&lines, i, lines.len()));
            let gone = block.is_empty();
            lines.splice(i..=through, block);
            if gone && lines.get(i).is_some_and(|l| l.trim().is_empty()) && (i == 0 || lines[i - 1].trim().is_empty()) {
                lines.remove(i);
            }
        }
        None if block.is_empty() => return Ok(text.to_string()),
        None => {
            let first = (0..lines.len()).find(|&j| content(&lines[j]) && indent(&lines[j]) == 0);
            let at = first.unwrap_or(lines.len());
            let mut block = block;
            if first.is_some() {
                block.push(String::new());
            }
            lines.splice(at..at, block);
        }
    }
    let new_text = lines.join("\n") + "\n";
    let mut want = old.to_json();
    if let Some(map) = want.as_object_mut() {
        match watcher {
            Some(w) => map.insert("watcher".into(), w.to_json()),
            None => map.remove("watcher"),
        };
    }
    match parse(&new_text) {
        Ok(new) if new.to_json() == want => Ok(new_text),
        _ => Err("couldn't set the watcher without changing anything else; set it by hand".into()),
    }
}

/// A top-level `watcher:` block, its fallbacks one to a line.
fn render_watcher(w: &Watcher) -> Vec<String> {
    fn fields(w: &Watcher, quote: fn(&str) -> String) -> Vec<String> {
        let text = [
            ("kind", w.kind.as_deref()),
            ("baseURL", w.base_url.as_deref()),
            ("model", w.model.as_deref()),
            ("apiKeyEnv", w.api_key_env.as_deref()),
            ("onError", w.on_error.as_deref()),
        ];
        let mut out: Vec<String> = text.iter().filter_map(|(k, v)| v.map(|v| format!("{k}: {}", quote(v)))).collect();
        out.extend(w.timeout_ms.map(|ms| format!("timeoutMs: {ms}")));
        out.extend(w.cache_key.map(|on| format!("cacheKey: {on}")));
        out
    }
    let mut lines = vec!["watcher:".to_string()];
    lines.extend(fields(w, scalar).into_iter().map(|f| format!("  {f}")));
    if !w.fallback.is_empty() {
        lines.push("  fallback:".into());
        lines.extend(w.fallback.iter().map(|f| format!("    - {{ {} }}", fields(f, flow).join(", "))));
    }
    lines
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
    put("load", rule.load.map(|l| l.to_string()));
    put("action", rule.action.as_deref().map(scalar));
    put("strikes", rule.strikes.map(|n| n.to_string()));
    put("ask", rule.ask.then(|| "true".to_string()));
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

    const SUITE: &str = r#"Here's the rule:

```yaml
rule:
  id: whole-test-suite
  tools: [bash]
  match: { command: '^(pytest|vitest)\b(?!.*(::|\.(py|[jt]sx?)\b|\$))' }
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
    match: { command: '^(pytest|vitest)\b(?!.*(::|\.(py|[jt]sx?)\b|\$))' }
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
    fn a_rule_comes_out_and_nothing_else_moves() {
        let text = check(&draft(SUITE), EXAMPLE_RULES).text.unwrap();
        assert_eq!(remove(&text, "whole-test-suite").unwrap(), EXAMPLE_RULES, "added, then taken out again");

        let without = remove(EXAMPLE_RULES, "secrets").unwrap();
        let config = parse(&without).unwrap();
        assert_eq!(config.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["force-push"]);
        assert!(without.contains("  # - id: whole-test-suite"), "the commented-out example stays");
        assert!(!without.contains("\n\n\n"), "no double gaps left behind");

        let generated = remove(EXAMPLE_RULES, "generated-code").unwrap();
        assert!(!generated.contains("generated-code") && generated.contains("# `path` is the file"), "{generated}");
        assert_eq!(parse(&generated).unwrap().projects["my-app"].rules.len(), 2);

        let last = remove("rules:\n  - id: a\n    match: { command: x }\nstrikes: 2\n", "a").unwrap();
        assert_eq!(last, "rules: []\nstrikes: 2\n");
        let projects = "projects:\n  app:\n    paths: [~/app]\n    rules:\n      - id: a\n        match: { command: x }\n\n  web:\n    paths: [~/web]\n    rules:\n      - id: b\n        match: { command: y }\nstrikes: 2\n";
        let one = remove(projects, "a").unwrap();
        assert_eq!(
            one,
            "projects:\n  web:\n    paths: [~/web]\n    rules:\n      - id: b\n        match: { command: y }\nstrikes: 2\n",
            "a project left with no rules goes"
        );
        assert_eq!(remove(&one, "b").unwrap(), "projects: {}\nstrikes: 2\n");
        let flow = remove("rules:\n  - { id: a, match: { command: x } }\n  - { id: b, match: { command: y } }\n", "a");
        assert_eq!(flow.unwrap(), "rules:\n  - { id: b, match: { command: y } }\n");
        assert_eq!(remove(EXAMPLE_RULES, "nope").unwrap_err(), "there is no rule nope");
        let inline = remove("rules: [{ id: a, match: { command: x } }]\n", "a").unwrap_err();
        assert!(inline.contains("by hand"), "{inline}");
    }

    #[test]
    fn a_new_version_takes_the_old_ones_place_and_nothing_else_moves() {
        let (old, new) = (r"'^git push\b.*\s(-f\b|--force\b|\+\S)'", r"'^git push\b.*\s(-f\b|--force\b)'");
        let snippet = format!(
            "rule:\n  id: force-push\n  tools: [bash]\n  match: {{ command: {new} }}\n  redirect: Push to a new branch and open a pull request for the change.\n"
        );
        let report = check_replacing(&draft(&snippet), EXAMPLE_RULES);
        assert!(report.ok(), "{:?}", report.failures());
        assert_eq!(report.text.unwrap(), EXAMPLE_RULES.replacen(old, new, 1));
        let added = check(&draft(&snippet), EXAMPLE_RULES);
        assert!(added.problems.iter().any(|p| p.contains("duplicate")), "{:?}", added.problems);
        let missing = check_replacing(&draft(&snippet.replace("force-push", "push-force")), EXAMPLE_RULES);
        assert_eq!(missing.problems, ["there is no rule push-force to replace; add it without --replace"]);
        let moved = check_replacing(&draft(&format!("project: my-app\n{snippet}")), EXAMPLE_RULES);
        assert_eq!(moved.problems, ["force-push is every project's rule, and its new version has to be too"]);
    }

    #[test]
    fn a_new_rules_file_keeps_the_sample_project_as_comments() {
        let text = starter();
        let (new, example) = (parse(&text).unwrap(), parse(EXAMPLE_RULES).unwrap());
        assert!(new.projects.is_empty(), "{text}");
        assert_eq!(new.rules.len(), example.rules.len());
        assert_eq!(new.watcher.as_ref().map(Watcher::to_json), example.watcher.as_ref().map(Watcher::to_json));
        assert!(text.contains("\n# projects:\n  # my-app:\n") && text.contains("      # - id: prod-db\n"), "{text}");
        assert!(lint(&new).iter().all(|i| i.level != Level::Error));
    }

    #[test]
    fn the_watcher_is_set_in_place_and_nothing_else_moves() {
        let mut claude = Watcher::new(Some("claude-code"), None, "claude-haiku-5-5");
        claude.on_error = Some("allow".into());
        claude.fallback.push(Watcher::new(None, Some("http://localhost:11434/v1"), "qwen3:4b"));
        let set = set_watcher(EXAMPLE_RULES, Some(&claude)).unwrap();
        let config = parse(&set).unwrap();
        assert_eq!(config.watcher.as_ref().unwrap().to_json(), claude.to_json());
        assert!(set.contains("    - { baseURL: http://localhost:11434/v1, model: qwen3:4b }"), "{set}");
        assert_eq!(config.rules.len(), parse(EXAMPLE_RULES).unwrap().rules.len());
        assert!(set.starts_with(EXAMPLE_RULES.split("watcher:").next().unwrap()), "the opening comments stay");

        let none = set_watcher(&set, None).unwrap();
        assert!(parse(&none).unwrap().watcher.is_none() && !none.contains("\n\n\n"), "{none}");
        let back = set_watcher(&none, Some(&claude)).unwrap();
        assert_eq!(parse(&back).unwrap().to_json(), config.to_json(), "a new one goes back in");
        let fresh = set_watcher("", Some(&claude)).unwrap();
        assert_eq!(parse(&fresh).unwrap().watcher.unwrap().to_json(), claude.to_json());
        let flow = set_watcher("watcher: { model: m, baseURL: \"http://x\" }\nstrikes: 2\n", Some(&claude)).unwrap();
        assert!(flow.ends_with("\nstrikes: 2\n") && !flow.contains("http://x"), "{flow}");
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
