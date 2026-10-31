//! Where reaper's files live. The engine, the console and every host adapter use these.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn var(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|v| !v.is_empty())
}

pub fn home() -> PathBuf {
    var("HOME").map(PathBuf::from).unwrap_or_default()
}

fn xdg(name: &str, fallback: &str) -> PathBuf {
    var(name).map_or_else(|| home().join(fallback), PathBuf::from)
}

/// Agents are never allowed to read from here.
pub fn config_dir() -> PathBuf {
    var("REAPER_HOME").map_or_else(|| xdg("XDG_CONFIG_HOME", ".config").join("reaper"), PathBuf::from)
}

/// Where reaper keeps its own bookkeeping (the event log).
pub fn state_dir() -> PathBuf {
    var("REAPER_STATE").map_or_else(|| xdg("XDG_STATE_HOME", ".local/state").join("reaper"), PathBuf::from)
}

/// Downloaded tools reaper runs (the pinned pi build).
pub fn data_dir() -> PathBuf {
    var("REAPER_DATA").map_or_else(|| xdg("XDG_DATA_HOME", ".local/share").join("reaper"), PathBuf::from)
}

/// Files reaper can recreate at any time (the unpacked pi extension).
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("reaper")
}

pub fn rules_file() -> PathBuf {
    config_dir().join("rules.yaml")
}

pub fn events_file() -> PathBuf {
    state_dir().join("events.jsonl")
}

/// `~/x` → `$HOME/x`, as rule paths are written.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}

/// The reverse, for display: `$HOME/x` → `~/x`.
pub fn tilde(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}
