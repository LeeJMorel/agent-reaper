//! `reaper install pi` / `reaper uninstall pi`: puts the extension where pi finds extensions
//! on its own, so every pi is guarded however it's started: `pi`, a script, another launcher.
//! The copy knows the binary that installed it, and that binary keeps it up to date. Under
//! `reaper run`, which loads its own copy, an installed one stands aside.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use reaper_core::paths::{self, tilde};
use serde_json::{Value, json};

use super::{extension, release};
use crate::hosts::{chose, confirm, on_path};

/// The line the installed copy has filled in.
const BLANK: &str = "const INSTALLED = undefined as";
const FILLED: &str = "const INSTALLED = ";
/// pi loads a folder's index.ts.
const INDEX: &str = "index.ts";

/// The extension's files as installed for `bin`.
fn files(bin: &Path) -> Vec<(String, Vec<u8>)> {
    let filled = format!("{FILLED}{} as", json!({ "bin": bin.to_string_lossy() }));
    extension::embedded::FILES
        .iter()
        .map(|(path, bytes)| match *path {
            extension::ENTRY => (INDEX.to_string(), String::from_utf8_lossy(bytes).replacen(BLANK, &filled, 1).into()),
            _ => (path.to_string(), bytes.to_vec()),
        })
        .collect()
}

/// The binary an installed copy in `dir` runs; `None` when there's no copy, or it isn't ours.
fn installed_bin(dir: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dir.join(INDEX)).ok()?;
    let (_, rest) = text.split_once(FILLED)?;
    let value = serde_json::Deserializer::from_str(rest).into_iter::<Value>().next()?.ok()?;
    value["bin"].as_str().map(PathBuf::from)
}

/// Written beside the final directory, outside the folder pi looks in, then moved into place.
fn write(dir: &Path, bin: &Path) -> io::Result<()> {
    let agent = dir.parent().and_then(Path::parent).expect("…/extensions/reaper");
    let partial = agent.join(format!(".reaper-extension-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    for (path, bytes) in files(bin) {
        let file = partial.join(path);
        fs::create_dir_all(file.parent().expect("files live in a directory"))?;
        fs::write(file, bytes)?;
    }
    fs::create_dir_all(dir.parent().expect("…/extensions"))?;
    let _ = fs::remove_dir_all(dir);
    fs::rename(&partial, dir).inspect_err(|_| {
        let _ = fs::remove_dir_all(&partial);
    })
}

/// Rewrites an installed copy that its binary has outgrown (an upgrade), or whose binary is
/// gone (moved): `me` takes it over. Another reaper binary's copy is left alone.
fn refresh_in(dir: &Path, me: &Path) -> io::Result<bool> {
    let Some(bin) = installed_bin(dir) else { return Ok(false) };
    if bin != me && bin.exists() {
        return Ok(false);
    }
    let stale = files(me).iter().any(|(path, bytes)| fs::read(dir.join(path)).ok().as_ref() != Some(bytes));
    if stale {
        write(dir, me)?;
    }
    Ok(stale)
}

/// On every pi session start: an upgraded binary updates its installed copy for the next one.
pub fn refresh() {
    if let Ok(me) = env::current_exe() {
        let _ = refresh_in(&paths::pi_extension_dir(), &me);
    }
}

/// pi is on this machine: on PATH, or its settings are.
pub fn present() -> bool {
    let dir = paths::pi_extension_dir();
    on_path("pi").is_some() || dir.parent().and_then(Path::parent).is_some_and(Path::is_dir)
}

pub fn installed() -> bool {
    installed_bin(&paths::pi_extension_dir()).is_some()
}

/// The extension, added without asking: what opening `reaper` does.
pub fn add(me: &Path) -> Result<(), String> {
    let dir = paths::pi_extension_dir();
    if dir.exists() && installed_bin(&dir).is_none() {
        return Err(format!("{} isn't reaper's; leaving it alone", tilde(&dir)));
    }
    write(&dir, me).map_err(|e| format!("can't write {}: {e}", tilde(&dir)))
}

fn version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim_start_matches('v').splitn(3, '.').map(|p| p.parse().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The version of the pi on PATH, if it's older than the one reaper is tested with.
fn older_pi() -> Option<String> {
    let out = Command::new("pi").arg("--version").output().ok()?;
    let text = [out.stdout, out.stderr].concat();
    let found = String::from_utf8_lossy(&text).split_whitespace().find(|w| version(w).is_some())?.to_string();
    (version(&found)? < version(release::VERSION)?).then_some(found)
}

pub fn run(args: &[String], remove: bool) -> Result<(), String> {
    let mut yes = false;
    for arg in args {
        match arg.as_str() {
            "pi" => {}
            "--yes" | "-y" => yes = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let dir = paths::pi_extension_dir();
    let ours = installed_bin(&dir).is_some();
    if dir.exists() && !ours {
        return Err(format!("{} isn't reaper's; leaving it alone", tilde(&dir)));
    }
    if remove && !ours {
        chose("pi", false);
        println!("reaper's extension isn't in {}.", tilde(&dir));
        return Ok(());
    }

    if remove {
        println!("Remove reaper's extension from {}.", tilde(&dir));
    } else {
        println!("Add reaper's extension to {}, where every pi session loads it.", tilde(&dir));
    }
    if !yes && !confirm("Go ahead?") {
        return Err("nothing changed (pass --yes to skip the question)".into());
    }
    if remove {
        fs::remove_dir_all(&dir).map_err(|e| format!("can't remove {}: {e}", tilde(&dir)))?;
        chose("pi", false);
        println!("Done.");
        return Ok(());
    }
    let me = env::current_exe().map_err(|e| format!("can't locate the reaper binary: {e}"))?;
    add(&me)?;
    chose("pi", true);
    println!("Done. New pi sessions are now watched; open `reaper` to see them.");
    if let Some(old) = older_pi() {
        println!("Your pi is {old}; reaper is tested with {}. Update pi if reaper misbehaves.", release::VERSION);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("reaper-pi-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root.join("agent/extensions/reaper")
    }

    #[test]
    fn the_installed_copy_knows_its_binary() {
        let index = files(Path::new("/opt/it's/reaper")).into_iter().find(|(path, _)| path == INDEX).unwrap().1;
        let text = String::from_utf8(index).unwrap();
        assert!(!text.contains(BLANK), "filled in");
        assert_eq!(text.matches(FILLED).count(), 1);

        let dir = sandbox("knows");
        write(&dir, Path::new("/opt/it's/reaper")).unwrap();
        assert_eq!(installed_bin(&dir), Some(PathBuf::from("/opt/it's/reaper")));
        let extensions = fs::read_dir(dir.parent().unwrap()).unwrap().count();
        assert_eq!(extensions, 1, "nothing half-written is left where pi looks");
        let _ = fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_copy_follows_its_own_binary_and_no_other() {
        let dir = sandbox("refresh");
        let me = env::current_exe().unwrap();
        let gone = Path::new("/nowhere/reaper");
        assert!(!refresh_in(&dir, &me).unwrap(), "nothing installed, nothing to do");

        write(&dir, gone).unwrap();
        assert!(refresh_in(&dir, &me).unwrap(), "its binary moved: this one takes over");
        assert_eq!(installed_bin(&dir), Some(me.clone()));
        assert!(!refresh_in(&dir, &me).unwrap(), "up to date");

        fs::write(dir.join(INDEX), fs::read_to_string(dir.join(INDEX)).unwrap() + "// older\n").unwrap();
        assert!(refresh_in(&dir, &me).unwrap(), "its binary was upgraded");

        let other = env::current_dir().unwrap(); // exists, and isn't this binary
        write(&dir, &other).unwrap();
        assert!(!refresh_in(&dir, &me).unwrap(), "another reaper's copy stays");
        assert_eq!(installed_bin(&dir), Some(other));
        let _ = fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version("0.80.4") < version("1.4.0"));
        assert!(version("v1.10.0") > version("1.4.0"));
        assert_eq!(version("pi"), None);
    }
}
