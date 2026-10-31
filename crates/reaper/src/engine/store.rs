//! What must outlive a single check. Hosts run the engine in a fresh process per tool
//! call, so strikes, the session's task, the verdict cache and the last good rules live
//! on disk. Session updates hold a file lock: parallel tool calls check concurrently.
//!
//! Running sessions that can be reaped are listed in `live/`, one file each, kept by the
//! session itself: when it went idle (`null` while it works), rewritten every couple of
//! seconds, so the file's age says whether the session is still there. To reap one, the
//! engine leaves a mark beside it; the session reaps itself if it is still idle.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use reaper_core::paths;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Session {
    /// Kills per rule id.
    #[serde(default)]
    pub strikes: BTreeMap<String, u32>,
    /// What it has done wrong, as a share of what it's allowed: each kill adds one over the
    /// rule's limit. Nothing takes it away: strikes are never forgotten.
    #[serde(default)]
    pub pressure: f64,
    /// As the session everyone in it works for (kept under the main agent's id).
    #[serde(default)]
    pub team: Team,
    #[serde(default)]
    pub goal: Option<String>,
    /// Whether `agent.spawned` was logged.
    #[serde(default)]
    pub announced: bool,
    /// The latest call that does something, and how many times in a row it has been made.
    #[serde(default)]
    pub last_call: Option<String>,
    #[serde(default)]
    pub repeats: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Team {
    /// Every agent that has worked in the session, the main one included, gone or not.
    #[serde(default)]
    pub members: BTreeSet<String>,
    /// The rule whose breaking stopped everyone, until the human's next prompt.
    #[serde(default)]
    pub stopped: Option<String>,
}

pub struct Store {
    state: PathBuf,
    cache: PathBuf,
}

/// A live session's file, as its extension keeps it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LiveFile {
    #[serde(rename = "idleSince")]
    idle_since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub file: PathBuf,
    /// When it went idle, in ms since the epoch; `None` while it works.
    pub idle_since: Option<i64>,
    /// Already asked to reap itself.
    pub marked: bool,
}

/// A live file this old belongs to a session that's gone (it rewrites the file every 2 s).
const STALE: Duration = Duration::from_secs(10);

pub fn mark_for(file: &Path) -> PathBuf {
    file.with_extension("reap")
}

/// Host-supplied ids become file names: keep them to a safe alphabet.
fn file_name(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| {
            p.chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("--")
}

impl Default for Store {
    fn default() -> Self {
        Self { state: paths::state_dir(), cache: paths::cache_dir() }
    }
}

impl Store {
    #[cfg(test)]
    pub fn at(root: &std::path::Path) -> Self {
        Self { state: root.join("state"), cache: root.join("cache") }
    }

    fn session_path(&self, host: &str, session: &str) -> PathBuf {
        self.state.join("sessions").join(format!("{}.json", file_name(&[host, session])))
    }

    pub fn update<T>(&self, host: &str, session: &str, change: impl FnOnce(&mut Session) -> T) -> io::Result<T> {
        let path = self.session_path(host, session);
        fs::create_dir_all(path.parent().expect("sessions dir"))?;
        let lock = File::options().create(true).truncate(false).write(true).open(path.with_extension("lock"))?;
        lock.lock()?;
        let mut state: Session = fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let result = change(&mut state);
        let partial = path.with_extension(format!("json.{}", std::process::id()));
        fs::write(&partial, serde_json::to_vec(&state)?)?;
        fs::rename(&partial, &path)?;
        Ok(result)
    }

    pub fn session(&self, host: &str, session: &str) -> Session {
        fs::read(self.session_path(host, session))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Lists a session that can be reaped, as idle since `now`. Returns its file and where its
    /// mark would go, for the session to keep.
    pub fn register(&self, host: &str, session: &str, now: i64) -> io::Result<(PathBuf, PathBuf)> {
        let file = self.state.join("live").join(format!("{}.json", file_name(&[host, session])));
        fs::create_dir_all(file.parent().expect("live dir"))?;
        let _ = fs::remove_file(mark_for(&file));
        let partial = file.with_extension(format!("json.{}", std::process::id()));
        fs::write(&partial, serde_json::to_vec(&LiveFile { idle_since: Some(now) })?)?;
        fs::rename(&partial, &file)?;
        let mark = mark_for(&file);
        Ok((file, mark))
    }

    /// The sessions still running, oldest-idle order not implied. Files left by sessions that
    /// went away without saying (a crash, a closed terminal) are removed.
    pub fn live(&self) -> Vec<Live> {
        let Ok(entries) = fs::read_dir(self.state.join("live")) else { return Vec::new() };
        let now = SystemTime::now();
        let mut live = Vec::new();
        for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")) {
            let fresh = fs::metadata(&path)
                .and_then(|m| m.modified())
                .is_ok_and(|at| now.duration_since(at).unwrap_or_default() < STALE);
            if !fresh {
                let _ = fs::remove_file(mark_for(&path));
                let _ = fs::remove_file(&path);
                continue;
            }
            let Some(state) = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<LiveFile>(&b).ok()) else {
                continue; // being rewritten right now
            };
            let marked = mark_for(&path).exists();
            live.push(Live { file: path, idle_since: state.idle_since, marked });
        }
        live
    }

    /// Rewrites a live session's file: whether it's idle, and that it's still there.
    pub fn keep_live(&self, file: &Path, idle_since: Option<i64>) {
        let _ = fs::write(file, serde_json::to_vec(&LiveFile { idle_since }).unwrap_or_default());
    }

    pub fn forget(&self, file: &Path) {
        let _ = fs::remove_file(mark_for(file));
        let _ = fs::remove_file(file);
    }

    pub fn mark(&self, live: &Live, reason: &str) {
        let _ = fs::write(mark_for(&live.file), reason);
    }

    fn refused_path(&self) -> PathBuf {
        self.cache.join("refused.json")
    }

    /// What `endpoint` (a base URL and model) has turned down, by request field.
    pub fn refused(&self, endpoint: &str) -> Vec<String> {
        let all: BTreeMap<String, Vec<String>> =
            fs::read(self.refused_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        all.get(endpoint).cloned().unwrap_or_default()
    }

    pub fn refuse(&self, endpoint: &str, field: &str) {
        let path = self.refused_path();
        let mut all: BTreeMap<String, Vec<String>> =
            fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let fields = all.entry(endpoint.to_string()).or_default();
        if !fields.iter().any(|f| f == field) {
            fields.push(field.to_string());
        }
        let _ = fs::create_dir_all(&self.cache);
        let _ = fs::write(path, serde_json::to_vec(&all).unwrap_or_default());
    }

    fn down_path(&self) -> PathBuf {
        self.cache.join("down.json")
    }

    /// Watcher endpoints that failed lately, and until when (ms) they wait at the back of the line.
    pub fn down(&self) -> BTreeMap<String, i64> {
        fs::read(self.down_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// `until` is `None` once the endpoint answers again.
    pub fn set_down(&self, endpoint: &str, until: Option<i64>) {
        let mut all = self.down();
        let changed = match until {
            Some(until) => all.insert(endpoint.to_string(), until) != Some(until),
            None => all.remove(endpoint).is_some(),
        };
        if changed {
            let _ = fs::create_dir_all(&self.cache);
            let _ = fs::write(self.down_path(), serde_json::to_vec(&all).unwrap_or_default());
        }
    }

    fn verdict_path(&self, key: &str) -> PathBuf {
        self.cache.join("verdicts").join(&key[..2.min(key.len())]).join(key)
    }

    /// An earlier watcher verdict for the same prefix and action.
    pub fn verdict(&self, key: &str) -> Option<Vec<String>> {
        serde_json::from_slice(&fs::read(self.verdict_path(key)).ok()?).ok()
    }

    pub fn remember(&self, key: &str, violated: &[String]) {
        let path = self.verdict_path(key);
        let _ = fs::create_dir_all(path.parent().expect("verdict dir"));
        let _ = fs::write(path, serde_json::to_vec(violated).unwrap_or_default());
    }

    fn good_rules_path(&self) -> PathBuf {
        self.state.join("rules.last-good.json")
    }

    /// Keeps the rules that last parsed, for when the file is mid-edit or broken.
    pub fn save_good_rules(&self, text: &str) {
        if self.good_rules().as_deref() == Some(text) {
            return; // every check runs this; only write when the rules changed
        }
        let _ = fs::create_dir_all(&self.state);
        let _ = fs::write(self.good_rules_path(), text);
    }

    pub fn good_rules(&self) -> Option<String> {
        fs::read_to_string(self.good_rules_path()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reaper-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn concurrent_strikes_are_not_lost() {
        let root = scratch("strikes");
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    Store::at(&root)
                        .update("pi", "s1", |s| {
                            *s.strikes.entry("r".into()).or_default() += 1;
                        })
                        .unwrap()
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(Store::at(&root).session("pi", "s1").strikes["r"], 16);
    }

    #[test]
    fn live_sessions_are_listed_until_they_stop_keeping_their_file() {
        let store = Store::at(&scratch("live"));
        let (file, mark) = store.register("pi", "s1", 1000).unwrap();
        assert_eq!(mark, file.with_extension("reap"));
        assert_eq!(store.live(), vec![Live { file: file.clone(), idle_since: Some(1000), marked: false }]);
        store.mark(&store.live()[0], "crowded");
        assert!(store.live()[0].marked);
        assert_eq!(fs::read_to_string(&mark).unwrap(), "crowded");

        // A session that stopped rewriting its file is gone, mark and all.
        let old = SystemTime::now() - STALE - Duration::from_secs(1);
        File::options().write(true).open(&file).unwrap().set_modified(old).unwrap();
        assert!(store.live().is_empty());
        assert!(!file.exists() && !mark.exists());
    }

    #[test]
    fn host_ids_cannot_escape_the_state_dir() {
        let store = Store::at(&scratch("escape"));
        let path = store.session_path("claude-code", "../../etc/passwd");
        assert!(path.starts_with(store.state.join("sessions")), "{}", path.display());
    }
}
