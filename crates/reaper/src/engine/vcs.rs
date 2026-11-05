//! What a project's version control says isn't committed yet, whichever one it uses, for a
//! handoff: the session taking over then sees work that commands did too, not only edits, and
//! which branch, worktree or workspace it was in. Each system is known by the marker it leaves at
//! the top of a checkout. One that isn't installed, fails, or takes too long leaves nothing out
//! but itself.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A big repository's status takes a while, and the human is waiting on the stopped session.
const WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Git,
    Jujutsu,
    Mercurial,
    Sapling,
    Subversion,
    Fossil,
}

/// Checked in this order in each directory: jj and Sapling repositories can carry a `.git` too.
const MARKERS: &[(&str, Kind)] = &[
    (".jj", Kind::Jujutsu),
    (".sl", Kind::Sapling),
    (".hg", Kind::Mercurial),
    (".git", Kind::Git),
    (".svn", Kind::Subversion),
    (".fslckout", Kind::Fossil),
    ("_FOSSIL_", Kind::Fossil),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Checkout {
    pub root: PathBuf,
    pub kind: Kind,
    /// One of several working copies of a repository: a git worktree, a jj workspace, a
    /// Mercurial share.
    pub extra: bool,
    pub branch: Option<String>,
    /// What isn't committed: how it changed ("modified", "new", "deleted", "renamed"), and the
    /// path from the root.
    pub changes: Vec<(&'static str, String)>,
}

impl Checkout {
    /// "a git worktree on branch fix-login", for the handoff.
    pub fn describe(&self) -> String {
        let what = match (self.kind, self.extra) {
            (Kind::Git, false) => "a git checkout",
            (Kind::Git, true) => "a git worktree",
            (Kind::Jujutsu, false) => "a jj repository",
            (Kind::Jujutsu, true) => "a jj workspace",
            (Kind::Mercurial, false) => "a Mercurial checkout",
            (Kind::Mercurial, true) => "a Mercurial share",
            (Kind::Sapling, _) => "a Sapling checkout",
            (Kind::Subversion, _) => "a Subversion working copy",
            (Kind::Fossil, _) => "a Fossil checkout",
        };
        let on = match self.kind {
            Kind::Jujutsu | Kind::Sapling => "bookmark",
            _ => "branch",
        };
        match &self.branch {
            Some(branch) => format!("{what} on {on} {branch}"),
            None => what.to_string(),
        }
    }
}

/// The checkout `dir` is in, and which system it uses: the nearest directory, going up, with a
/// marker.
pub fn find(dir: &Path) -> Option<(PathBuf, Kind)> {
    dir.ancestors().find_map(|root| {
        MARKERS.iter().find(|(marker, _)| root.join(marker).exists()).map(|(_, kind)| (root.to_path_buf(), *kind))
    })
}

/// What the checkout at `root` has that isn't committed, as its own tool says.
pub fn status(root: &Path, kind: Kind) -> Option<Checkout> {
    let run = |program: &str, args: &[&str]| run(root, program, args);
    let (extra, branch, changes) = match kind {
        Kind::Git => {
            // A file instead of a directory: a worktree's, or a submodule's.
            let dot = std::fs::read_to_string(root.join(".git")).unwrap_or_default();
            // New files are half the work, whatever the person's own settings hide.
            let args = ["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=normal"];
            let (branch, changes) = git(&run("git", &args)?);
            (dot.contains("/worktrees/"), branch, changes)
        }
        Kind::Jujutsu => {
            let changes = letter_first(&run("jj", &["--no-pager", "--color=never", "diff", "--summary"])?);
            let bookmarks = run("jj", &["--no-pager", "--color=never", "log", "--no-graph", "-r", "@", "-T", "bookmarks"]);
            (root.join(".jj/repo").is_file(), named(bookmarks), changes)
        }
        Kind::Mercurial | Kind::Sapling => {
            // Sapling has bookmarks and no named branches.
            let (program, at) = match kind {
                Kind::Sapling => ("sl", "{activebookmark}"),
                _ => ("hg", "{activebookmark}\\n{branch}"),
            };
            let changes = letter_first(&run(program, &["status"])?);
            let at = run(program, &["log", "-r", ".", "-T", at]).unwrap_or_default();
            let branch = at.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from);
            (root.join(".hg/sharedpath").is_file(), branch, changes)
        }
        Kind::Subversion => {
            let changes = svn(&run("svn", &["status"])?);
            let url = run("svn", &["info", "--show-item", "relative-url"]);
            (false, named(url.map(|u| u.trim().trim_start_matches("^/").to_string())), changes)
        }
        Kind::Fossil => {
            let mut changes = fossil(&run("fossil", &["changes"])?);
            let extras = run("fossil", &["extras"]).unwrap_or_default();
            changes.extend(extras.lines().filter(|l| !l.trim().is_empty()).map(|l| ("new", l.trim().to_string())));
            (false, named(run("fossil", &["branch", "current"])), changes)
        }
    };
    Some(Checkout { root: root.to_path_buf(), kind, extra, branch, changes })
}

fn named(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// `program`'s output in `root`, if it ran, said yes, and was done in time. Mercurial and
/// Sapling are told to keep their output plain, and git not to take locks it can do without.
fn run(root: &Path, program: &str, args: &[&str]) -> Option<String> {
    run_for(WAIT, root, program, args)
}

fn run_for(wait: Duration, root: &Path, program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("HGPLAIN", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read alongside, so a long status can't fill the pipe and stall the tool.
    let reader = thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).map(|_| out)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if start.elapsed() > wait {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let out = reader.join().ok()?.ok()?;
    status.success().then_some(out)
}

/// `git status --porcelain=v1 -z --branch`: the branch, and each change. A rename names its old
/// path in the field after.
fn git(out: &str) -> (Option<String>, Vec<(&'static str, String)>) {
    let (mut branch, mut changes) = (None, Vec::new());
    let mut fields = out.split('\0');
    while let Some(field) = fields.next() {
        if let Some(head) = field.strip_prefix("## ") {
            let head = head.strip_prefix("No commits yet on ").unwrap_or(head);
            branch = (!head.starts_with("HEAD (no branch)"))
                .then(|| head.split("...").next().unwrap_or(head).split(' ').next().unwrap_or(head).to_string());
            continue;
        }
        let (Some(xy), Some(path)) = (field.get(..2), field.get(3..)) else { continue };
        let how = match xy {
            "??" => "new",
            _ if xy.contains('R') => {
                fields.next();
                "renamed"
            }
            _ if xy.contains('A') => "new",
            _ if xy.contains('D') => "deleted",
            _ => "modified",
        };
        changes.push((how, path.to_string()));
    }
    (branch, changes)
}

/// One change a line, its letter first: `hg status`, `sl status` and `jj diff --summary`.
fn letter_first(out: &str) -> Vec<(&'static str, String)> {
    out.lines()
        .filter_map(|line| {
            let (code, path) = line.split_once(' ')?;
            let how = match code {
                "A" | "?" | "C" => "new",
                "R" if line.contains(" => ") => "renamed",
                "R" | "!" | "D" => "deleted",
                _ => "modified",
            };
            Some((how, path.trim().to_string()))
        })
        .collect()
}

/// `svn status`: seven columns of flags, a space, then the path.
fn svn(out: &str) -> Vec<(&'static str, String)> {
    out.lines()
        .filter_map(|line| {
            let path = line.get(8..)?.trim();
            let how = match line.chars().next()? {
                'A' | '?' => "new",
                'D' | '!' => "deleted",
                'M' | 'R' | 'C' | '~' => "modified",
                // Only its properties changed.
                ' ' if line[1..].starts_with('M') => "modified",
                _ => return None,
            };
            (!path.is_empty()).then(|| (how, path.to_string()))
        })
        .collect()
}

/// `fossil changes`: a word for the change, then the path.
fn fossil(out: &str) -> Vec<(&'static str, String)> {
    out.lines()
        .filter_map(|line| {
            let (word, path) = line.trim().split_once(char::is_whitespace)?;
            let how = match word {
                "ADDED" => "new",
                "DELETED" | "MISSING" => "deleted",
                "RENAMED" => "renamed",
                _ => "modified",
            };
            Some((how, path.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn each_systems_status_reads_as_the_same_kinds_of_change() {
        let (branch, changes) =
            git("## fix-login...origin/fix-login [ahead 1]\0 M src/a.rs\0?? notes.md\0R  new.rs\0old.rs\0D  gone.rs\0A  b.rs\0");
        assert_eq!(branch.as_deref(), Some("fix-login"));
        assert_eq!(
            changes,
            [
                ("modified", "src/a.rs".into()),
                ("new", "notes.md".into()),
                ("renamed", "new.rs".into()),
                ("deleted", "gone.rs".into()),
                ("new", "b.rs".into())
            ]
        );
        assert_eq!(git("## HEAD (no branch)\0").0, None);
        assert_eq!(git("## No commits yet on main\0").0.as_deref(), Some("main"));

        // Mercurial and Sapling, then jj.
        let hg = letter_first("M src/a.rs\nA b.rs\nR gone.rs\n! lost.rs\n? notes.md\n");
        let hows: Vec<&str> = hg.iter().map(|c| c.0).collect();
        assert_eq!(hows, ["modified", "new", "deleted", "deleted", "new"]);
        let jj = letter_first("M src/a.rs\nA src/b.rs\nD old.rs\nR {old => new}.rs\n");
        assert_eq!(jj.iter().map(|c| c.0).collect::<Vec<_>>(), ["modified", "new", "deleted", "renamed"]);

        let svn = svn("M       src/a.rs\n?       notes.md\n M      props.rs\nA  +    copied.rs\n!       lost.rs\nX       vendor\n");
        assert_eq!(
            svn,
            [
                ("modified", "src/a.rs".into()),
                ("new", "notes.md".into()),
                ("modified", "props.rs".into()),
                ("new", "copied.rs".into()),
                ("deleted", "lost.rs".into())
            ]
        );
        let fossil = fossil("EDITED     src/a.rs\nADDED      b.rs\nMISSING    lost.rs\n");
        assert_eq!(fossil.iter().map(|c| c.0).collect::<Vec<_>>(), ["modified", "new", "deleted"]);
    }

    #[test]
    fn a_checkout_is_known_by_its_nearest_marker() {
        let root = std::env::temp_dir().join(format!("reaper-vcs-find-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("jj/.jj")).unwrap();
        fs::create_dir_all(root.join("jj/.git")).unwrap();
        fs::create_dir_all(root.join("jj/sub/hg/.hg")).unwrap();
        fs::create_dir_all(root.join("jj/sub/hg/deep")).unwrap();
        fs::create_dir_all(root.join("fossil")).unwrap();
        fs::write(root.join("fossil/.fslckout"), "").unwrap();
        assert_eq!(find(&root.join("jj/sub")), Some((root.join("jj"), Kind::Jujutsu)), "jj over its git");
        assert_eq!(find(&root.join("jj/sub/hg/deep")), Some((root.join("jj/sub/hg"), Kind::Mercurial)), "the nearest");
        assert_eq!(find(&root.join("fossil")), Some((root.join("fossil"), Kind::Fossil)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_tool_or_a_slow_one_says_nothing() {
        let here = std::env::temp_dir();
        assert_eq!(run(&here, "reaper-no-such-vcs", &["status"]), None);
        let start = Instant::now();
        assert_eq!(run_for(Duration::from_millis(200), &here, "sleep", &["30"]), None);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
