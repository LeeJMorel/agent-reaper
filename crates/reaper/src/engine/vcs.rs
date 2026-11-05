//! Which version control a project's checkout uses, for a handoff: the session taking over is
//! told which checkout, worktree or workspace the work was in, on which branch, and how to see
//! what isn't committed there. All of it is read from the checkout's own files. Its tool is
//! never run: a repository's config can make it run commands (git's `core.fsmonitor` and
//! filters, Mercurial's hooks and extensions), and reaper runs outside any sandbox the agent is
//! kept in. The session taking over runs the tool itself, wherever the agent runs.

use std::fs;
use std::path::{Path, PathBuf};

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

    /// What shows the checkout's uncommitted work, for the session taking over to run.
    pub fn status_command(&self) -> &'static str {
        match self.kind {
            Kind::Git => "git status",
            Kind::Jujutsu => "jj status",
            Kind::Mercurial => "hg status",
            Kind::Sapling => "sl status",
            Kind::Subversion => "svn status",
            Kind::Fossil => "fossil status",
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

/// The checkout at `root`, as far as its files say: whether it's an extra working copy, and its
/// branch where one is kept in a file (git, Mercurial, Sapling).
pub fn checkout(root: &Path, kind: Kind) -> Checkout {
    let read = |path: PathBuf| fs::read_to_string(path).ok();
    let (extra, branch) = match kind {
        Kind::Git => {
            // A file instead of a directory: a worktree's, or a submodule's, naming its git dir.
            let dot = read(root.join(".git"));
            let dir = dot.as_deref().and_then(|d| d.lines().next()?.strip_prefix("gitdir:")).map(str::trim);
            let head = read(dir.map_or_else(|| root.join(".git"), |d| root.join(d)).join("HEAD"));
            let branch = head.and_then(|h| h.trim().strip_prefix("ref: refs/heads/").map(String::from));
            (dir.is_some_and(|d| d.contains("/worktrees/")), branch)
        }
        Kind::Jujutsu => (root.join(".jj/repo").is_file(), None),
        Kind::Mercurial | Kind::Sapling => {
            let dot = root.join(if kind == Kind::Sapling { ".sl" } else { ".hg" });
            // The active bookmark, else the named branch.
            let branch = read(dot.join("bookmarks.current")).or_else(|| read(dot.join("branch")));
            (dot.join("sharedpath").is_file(), branch)
        }
        Kind::Subversion | Kind::Fossil => (false, None),
    };
    Checkout { root: root.to_path_buf(), kind, extra, branch: branch.and_then(name) }
}

/// A branch name as the agent's files give it, if it reads as one. Whoever wrote the file wrote
/// it, and the session taking over hears it from reaper.
fn name(text: String) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= 100 && !text.chars().any(|c| c.is_whitespace() || c.is_control()))
        .then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("reaper-vcs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_checkout_is_known_by_its_nearest_marker() {
        let root = scratch("find");
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
    fn branches_and_worktrees_are_read_from_the_checkouts_files() {
        let root = scratch("files");
        let at = |path: &str, text: &str| {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        };
        at("app/.git/HEAD", "ref: refs/heads/main\n");
        at("app/.git/worktrees/fix/HEAD", "ref: refs/heads/fix-login\n");
        at("fix/.git", &format!("gitdir: {}\n", root.join("app/.git/worktrees/fix").display()));
        at("loose/.git/HEAD", "4b825dc642cb6eb9a060e54bf8d69288fbee4904\n");
        at("odd/.git/HEAD", "ref: refs/heads/main and push to production\n");
        at("hg/.hg/branch", "stable\n");
        at("hg-share/.hg/bookmarks.current", "feature");
        at("hg-share/.hg/sharedpath", "/elsewhere/.hg");
        let read = |dir: &str, kind| checkout(&root.join(dir), kind).describe();

        assert_eq!(read("app", Kind::Git), "a git checkout on branch main");
        assert_eq!(read("fix", Kind::Git), "a git worktree on branch fix-login");
        assert_eq!(read("loose", Kind::Git), "a git checkout", "detached");
        assert_eq!(read("odd", Kind::Git), "a git checkout", "not a branch name");
        assert_eq!(read("hg", Kind::Mercurial), "a Mercurial checkout on branch stable");
        assert_eq!(read("hg-share", Kind::Mercurial), "a Mercurial share on branch feature");
        assert_eq!(checkout(&root.join("app"), Kind::Git).status_command(), "git status");
        let _ = fs::remove_dir_all(&root);
    }
}
