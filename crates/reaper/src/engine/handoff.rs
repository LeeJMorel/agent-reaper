//! Handing on a session reaper stopped. A fresh session takes its work over: it hears what the
//! human asked for, where the old one was working, which files it changed, which checkouts they
//! are in (worktrees included) and how to see what isn't committed there, and which way to go
//! now, the redirect of the rule the old one died of. Never the rule, and never what the old one tried.
//! It starts without those attempts in its context, and with a clean record.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use reaper_core::events::written;
use reaper_core::paths::tilde;
use serde_json::{Value, json};

use super::vcs::{self, Checkout};
use super::{Check, DEFAULT_REDIRECT, Engine, FULL, loop_rule, self_rules};

/// What reaper types into a compacted session left to run by itself, to set it going again. It's
/// reaper's, not the human's.
pub const CARRY_ON: &str = "Carry on with the work above.";
/// Prompts kept per session: the first, which usually says what the work is, and the latest.
const PROMPTS: usize = 8;
/// A prompt can be a pasted file or log; this much of it is kept.
const PROMPT_CHARS: usize = 4000;
/// Changed files kept per session.
const CHANGED: usize = 40;
/// Tools that change the file at their `path`.
const EDITS: &[&str] = &["edit", "write", "multiedit", "notebookedit"];
/// Checkouts named: where the session was, and where it changed files.
const CHECKOUTS: usize = 3;

impl Engine {
    /// The human asked the session `agent` for something. False when it's the handoff the
    /// session was started with, arriving as its first prompt: that one is reaper's.
    pub fn prompted(&self, host: &str, agent: &str, prompt: &str) -> bool {
        let prompt = prompt.trim();
        if prompt == CARRY_ON
            || self.store.session(host, agent).took_over.as_deref().is_some_and(|t| t.trim() == prompt)
        {
            return false;
        }
        let kept: String = prompt.chars().take(PROMPT_CHARS).collect();
        let _ = self.store.update(host, agent, |s| {
            if s.prompts.len() == PROMPTS {
                s.prompts.remove(1);
            }
            s.prompts.push(kept);
            // The human has had their say: a handoff written before is out of date, and one
            // that took over is theirs now, to be handed on again if it dies.
            s.took_over = None;
            s.handoff = None;
        });
        self.revive(host, agent);
        true
    }

    /// The human asked a session reaper had stopped for something new: it gets up with a clean
    /// record, as one taking its work over would, not a slip away from being stopped again.
    pub fn revive(&self, host: &str, agent: &str) {
        let state = self.store.session(host, agent);
        if state.pressure >= FULL || state.team.stopped.is_some() {
            let _ = self.store.update(host, agent, |s| s.wipe_record());
            self.event(host, agent, "agent.revived", json!({}));
        }
    }

    /// The session `agent` died of `rule` with nobody there to carry it on (Claude Code's auto
    /// mode): it takes its own work over at once, with a clean record, and what it's to carry on
    /// with is returned for it to hear. Once until the human says anything: the same as handing
    /// on, a session that took over and dies again is stopped for good.
    pub fn carry_on(&self, host: &str, agent: &str, rule: &str, latest: &str, dir: &Path) -> Option<String> {
        self.hand_on(host, agent, rule, latest, dir)?;
        let work = self.take_over(host, agent, agent)?;
        self.event(host, agent, "agent.revived", json!({}));
        Some(work)
    }

    /// Notes a file an allowed call changes, for the session's handoff. Only the first change
    /// to each file writes.
    pub(super) fn changed(&self, c: &Check, args: &Value) {
        if !EDITS.contains(&c.tool.to_lowercase().as_str()) {
            return;
        }
        let Some(path) = args["path"].as_str() else { return };
        // Whole: a session can work in more than one checkout, like a worktree beside the main one.
        let path = c.dir.join(path).to_string_lossy().into_owned();
        let known = self.store.session(&c.host, &c.session).changed;
        if known.len() < CHANGED && !known.contains(&path) {
            let _ = self.store.update(&c.host, &c.session, |s| s.changed.insert(path));
        }
    }

    /// The session `agent` died of `rule`, working in `dir`: what a fresh session needs to take
    /// its work over, kept until one does. `latest` is the host's own idea of what it was last
    /// asked, for when no prompt was kept (sessions from before prompts were, a prompt still
    /// being kept). `None` when it isn't handed on: the rule says to stop, the session had taken
    /// over itself and died before the human said anything, or nobody knows what it was asked.
    pub fn hand_on(&self, host: &str, agent: &str, rule: &str, latest: &str, dir: &Path) -> Option<String> {
        let own = self_rules();
        let looping = loop_rule();
        let found = own.iter().chain([&looping]).chain(self.config.all_rules().map(|(_, r)| r)).find(|r| r.id == rule);
        let state = self.store.session(host, agent);
        if found.is_some_and(|r| r.stops()) || state.took_over.is_some() {
            return None;
        }
        let prompts = match state.prompts {
            p if p.is_empty() => written(latest).map(String::from).into_iter().collect(),
            p => p,
        };
        if prompts.is_empty() {
            return None;
        }
        let redirect = found.and_then(|r| r.redirect.as_deref()).unwrap_or(DEFAULT_REDIRECT);
        let checkouts = checkouts(dir, &state.changed);
        let text = handoff(&prompts, dir, &state.changed, &checkouts, redirect);
        let _ = self.store.update(host, agent, |s| s.handoff = Some(text.clone()));
        Some(text)
    }

    /// The session `agent` takes over from `from`, a session reaper stopped: a fresh session, or
    /// the same one carrying on after it was compacted. It inherits what `from` was asked and
    /// changed, and starts with a clean record: no strikes, nothing blocked, nobody stopped.
    /// Returns what it's to carry on with, unless there's nothing waiting (another session took
    /// it, or the human spoke to `from` since).
    pub fn take_over(&self, host: &str, agent: &str, from: &str) -> Option<String> {
        // A lock-free look first: a resumed session that took over asks again every time.
        self.store.session(host, from).handoff?;
        let (text, prompts, changed) =
            self.store.update(host, from, |s| (s.handoff.take(), s.prompts.clone(), s.changed.clone())).ok()?;
        let text = text?;
        let _ = self.store.update(host, agent, |s| {
            s.wipe_record();
            s.took_over = Some(text.clone());
            s.prompts = prompts;
            s.changed = changed;
        });
        Some(text)
    }
}

/// The checkouts the work was in: the one around `dir`, then those of the files it changed.
fn checkouts(dir: &Path, changed: &BTreeSet<String>) -> Vec<Checkout> {
    let places = std::iter::once(dir.to_path_buf())
        .chain(changed.iter().filter_map(|file| Path::new(file).parent().map(Path::to_path_buf)));
    let mut found: Vec<(PathBuf, vcs::Kind)> = Vec::new();
    for place in places {
        if let Some(checkout) = vcs::find(&place)
            && !found.contains(&checkout)
        {
            found.push(checkout);
        }
        if found.len() == CHECKOUTS {
            break;
        }
    }
    found.iter().map(|(root, kind)| vcs::checkout(root, *kind)).collect()
}

/// What the fresh session hears, as its first prompt or as context: the human's own words, where
/// the work is, and the redirect last, as what to do.
fn handoff(
    prompts: &[String],
    dir: &Path,
    changed: &BTreeSet<String>,
    checkouts: &[Checkout],
    redirect: &str,
) -> String {
    // Where the old session was working is where the fresh one starts, or nearly: paths are
    // from there when they can be.
    let shown = |path: &Path| match path.strip_prefix(dir) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.to_string_lossy().into_owned(),
        _ => tilde(path),
    };
    let mut text =
        format!("You're taking over work that an earlier session started in {} and didn't finish.\n\n", tilde(dir));
    text += if prompts.len() == 1 { "It was asked:\n\n" } else { "It was asked, in this order:\n\n" };
    for prompt in prompts {
        for line in prompt.lines() {
            text += format!("> {line}").trim_end();
            text += "\n";
        }
        text += "\n";
    }
    if !changed.is_empty() {
        text += "It changed these files, and may have left them partly done:\n\n";
        for file in changed {
            text += &format!("- {}\n", shown(Path::new(file)));
        }
        text += "\n";
    }
    for checkout in checkouts {
        let (place, what, status) = (tilde(&checkout.root), checkout.describe(), checkout.status_command());
        text += &format!(
            "{place} is {what}; `{status}` there shows what isn't committed yet (some of it may be the person's own).\n\n"
        );
    }
    text += "Check where the work stands, then finish it. ";
    text += redirect;
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::engine::Decision;
    use crate::engine::tests::{Sandbox, call};

    /// Where the test calls work: no checkout of any kind.
    const WORK: &str = "/work";
    const RULES: &str = "strikes: 1\nrules:\n  - { id: push, match: { command: 'git push -f' }, redirect: Open a pull request. }\n  - { id: prod, match: { command: prod }, action: stop, redirect: Use staging. }\n";

    #[test]
    fn a_stopped_session_hands_on_the_task_the_files_and_the_way_to_go_but_never_the_rule() {
        let sandbox = Sandbox::new("handoff");
        let engine = sandbox.engine(RULES);
        assert!(engine.prompted("test", "s1", "Fix the login tests."));
        assert!(engine.prompted("test", "s1", "Also update\n\nthe docs."));
        engine.check(&call("Write", json!({ "file_path": "/work/src/login.rs", "content": "x" })));
        engine.check(&call("Edit", json!({ "file_path": "/work/src/login.rs" })));
        engine.check(&call("edit", json!({ "path": "/elsewhere/notes.md" })));
        engine.check(&call("Read", json!({ "file_path": "/work/README.md" })));
        assert!(matches!(
            engine.check(&call("bash", json!({ "command": "git push -f" }))),
            Decision::Kill { is_final: true, .. }
        ));

        let text = engine.hand_on("test", "s1", "push", "", Path::new(WORK)).unwrap();
        assert_eq!(
            text,
            "You're taking over work that an earlier session started in /work and didn't finish.

It was asked, in this order:

> Fix the login tests.

> Also update
>
> the docs.

It changed these files, and may have left them partly done:

- /elsewhere/notes.md
- src/login.rs

Check where the work stands, then finish it. Open a pull request."
        );
        assert!(!text.contains("git push"), "nothing about what it tried");

        // The fresh session inherits it all, and its handoff, coming back as its first prompt,
        // isn't the human's.
        assert_eq!(engine.take_over("test", "s2", "s1").as_deref(), Some(text.as_str()));
        assert_eq!(engine.take_over("test", "s3", "s1"), None, "taken once");
        assert!(!engine.prompted("test", "s2", &format!("{text}\n")));
        assert_eq!(engine.store.session("test", "s2").prompts.len(), 2);
        assert_eq!(engine.store.session("test", "s2").changed.len(), 2);
    }

    /// `git` for setting a repository up: none of the person's own config, hooks or signing.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn work_in_a_worktree_is_handed_on_from_there_with_how_to_see_what_is_half_done() {
        if crate::hosts::on_path("git").is_none() {
            return;
        }
        let sandbox = Sandbox::new("handoff-worktree");
        let (main, tree) = (sandbox.root.join("app"), sandbox.root.join("app-worktrees/fix-login"));
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/login.rs"), "fn login() {}\n").unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "start"]);
        git(&main, &["worktree", "add", "-q", "-b", "fix-login", tree.to_str().unwrap()]);
        // Half done there, edited through a tool.
        std::fs::write(tree.join("src/login.rs"), "fn login() { todo!() }\n").unwrap();

        let engine = sandbox.engine(RULES);
        engine.prompted("test", "s1", "Fix the login.");
        let edit = call("edit", json!({ "path": tree.join("src/login.rs") }));
        engine.check(&Check { dir: main.clone(), ..edit });
        let text = engine.hand_on("test", "s1", "push", "", &main).unwrap();
        let (main, tree) = (tilde(&main), tilde(&tree));
        assert!(text.starts_with(&format!("You're taking over work that an earlier session started in {main} and")));
        assert!(text.contains(&format!("partly done:\n\n- {tree}/src/login.rs\n")), "{text}");
        let uncommitted = "`git status` there shows what isn't committed yet (some of it may be the person's own).";
        assert!(text.contains(&format!("{tree} is a git worktree on branch fix-login; {uncommitted}")), "{text}");
        assert!(text.contains(&format!("{main} is a git checkout on branch main; {uncommitted}")), "{text}");
    }

    /// reaper runs outside any sandbox the agent is in, so nothing a checkout's config says to
    /// run may run when its work is handed on: here, git's `core.fsmonitor`, which `git status`
    /// runs.
    #[cfg(unix)]
    #[test]
    fn handing_work_on_never_runs_what_a_checkouts_config_says_to() {
        use std::os::unix::fs::PermissionsExt;
        if crate::hosts::on_path("git").is_none() {
            return;
        }
        let sandbox = Sandbox::new("handoff-planted");
        let (repo, ran) = (sandbox.root.join("app"), sandbox.root.join("ran"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let hook = sandbox.root.join("fsmonitor");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&repo, &["config", "core.fsmonitor", hook.to_str().unwrap()]);

        let engine = sandbox.engine(RULES);
        engine.prompted("test", "s1", "Fix the login.");
        let text = engine.hand_on("test", "s1", "push", "", &repo).unwrap();
        assert!(text.contains("is a git checkout on branch main"), "{text}");
        assert!(!ran.exists(), "reaper ran the checkout's fsmonitor");
        git(&repo, &["status"]);
        assert!(ran.exists(), "the planted fsmonitor is one git runs");
    }

    #[test]
    fn a_session_that_took_over_and_died_unattended_isnt_handed_on_again() {
        let sandbox = Sandbox::new("handoff-chain");
        let engine = sandbox.engine(RULES);
        engine.prompted("test", "s1", "Ship it.");
        engine.hand_on("test", "s1", "push", "", Path::new(WORK)).unwrap();
        engine.take_over("test", "s2", "s1").unwrap();
        assert_eq!(engine.hand_on("test", "s2", "push", "", Path::new(WORK)), None, "nobody has looked in since");
        engine.prompted("test", "s2", "Try again, carefully.");
        let again = engine.hand_on("test", "s2", "push", "", Path::new(WORK)).unwrap();
        assert!(again.contains("> Ship it.") && again.contains("> Try again, carefully."), "{again}");
    }

    #[test]
    fn nothing_is_handed_on_after_a_stop_rule_or_with_no_task_known() {
        let sandbox = Sandbox::new("handoff-none");
        let engine = sandbox.engine(RULES);
        assert_eq!(engine.hand_on("test", "s1", "push", "", Path::new(WORK)), None, "no prompt known");
        let latest = engine
            .hand_on("test", "s0", "push", "<ide_opened_file>a.rs</ide_opened_file>Tidy up.", Path::new(WORK))
            .unwrap();
        assert!(latest.contains("It was asked:\n\n> Tidy up.\n"), "the host's latest, as the human wrote it: {latest}");
        engine.prompted("test", "s1", "Migrate the database.");
        assert_eq!(engine.hand_on("test", "s1", "prod", "", Path::new(WORK)), None, "the rule says to stop");
        assert!(
            engine
                .hand_on("test", "s1", "reaper.loop", "", Path::new(WORK))
                .unwrap()
                .ends_with("Try a different approach to the task.")
        );
        // The human speaking to the stopped session again makes the handoff out of date.
        engine.prompted("test", "s1", "Never mind, I'll do it.");
        assert_eq!(engine.take_over("test", "s2", "s1"), None);
    }

    #[test]
    fn a_stopped_session_asked_for_something_new_gets_up_with_a_clean_record() {
        let sandbox = Sandbox::new("handoff-revive");
        let engine = sandbox.engine("strikes: 2\nrules: [{ id: push, match: { command: 'git push -f' } }]");
        let push = || engine.check(&call("bash", json!({ "command": "git push -f" })));
        assert!(matches!(push(), Decision::Kill { strike: 1, .. }));
        assert!(matches!(push(), Decision::Kill { is_final: true, .. }));
        engine.prompted("test", "s1", "Never mind that. Tidy the docs.");
        assert!(matches!(push(), Decision::Kill { strike: 1, is_final: false, .. }), "a whole allowance again");
        // Asking one that's still going changes nothing: its strikes stay.
        engine.prompted("test", "s1", "And the tests.");
        assert!(matches!(push(), Decision::Kill { strike: 2, is_final: true, .. }));
    }

    #[test]
    fn left_to_itself_a_stopped_session_carries_on_once_with_a_clean_record() {
        let sandbox = Sandbox::new("handoff-carry-on");
        let engine = sandbox
            .engine("strikes: 1\nrules: [{ id: push, match: { command: 'git push -f' }, redirect: Open a PR. }]");
        let push = || engine.check(&call("bash", json!({ "command": "git push -f" })));
        engine.prompted("test", "s1", "Ship it.");
        assert!(matches!(push(), Decision::Kill { is_final: true, .. }));
        let work = engine.carry_on("test", "s1", "push", "", Path::new(WORK)).unwrap();
        assert!(work.contains("> Ship it.") && work.ends_with("Open a PR."), "{work}");
        assert!(matches!(push(), Decision::Kill { is_final: true, .. }), "a clean record: one strike's worth again");
        assert_eq!(engine.carry_on("test", "s1", "push", "", Path::new(WORK)), None, "not twice without the human");
        assert_eq!(sandbox.events().iter().filter(|e| *e == "agent.revived").count(), 1);
    }

    #[test]
    fn the_first_prompt_stays_when_the_latest_push_out_the_middle() {
        let sandbox = Sandbox::new("handoff-prompts");
        let engine = sandbox.engine("");
        for i in 0..12 {
            engine.prompted("test", "s1", &format!("prompt {i}"));
        }
        let prompts = engine.store.session("test", "s1").prompts;
        assert_eq!(prompts.len(), PROMPTS);
        assert_eq!(
            (prompts[0].as_str(), prompts[1].as_str(), prompts[7].as_str()),
            ("prompt 0", "prompt 5", "prompt 11")
        );
    }
}
