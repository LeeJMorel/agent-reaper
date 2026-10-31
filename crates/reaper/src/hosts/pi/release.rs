//! The pi that `reaper run` starts: a pinned release, fetched once and accepted only
//! if it matches the checksum compiled in here. Never "latest": reaper is tested
//! against this version's extension API.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use reaper_core::paths;
use sha2::{Digest, Sha256};

use crate::engine::prefix::hex;

pub const VERSION: &str = "1.4.0";

/// SHA-256 sums from the release's SHA256SUMS.
const ASSETS: &[(&str, &str, &str, &str)] = &[
    ("linux", "x86_64", "pi-linux-x64.tar.gz", "f2e4e4329603223d03260cdb8bd5033dbdc1f73ba58ab8939db2e56546e115d2"),
    ("linux", "aarch64", "pi-linux-arm64.tar.gz", "fa5829c7ef83ffeaebba015715c8306220f89b2aa8a00154efbba98dd5440156"),
    ("macos", "x86_64", "pi-darwin-x64.tar.gz", "1bdefc9e1074c1de458ea9f2e43e6013ce49cd05326009b3de144037036585e2"),
    ("macos", "aarch64", "pi-darwin-arm64.tar.gz", "79d9979d76a2a5adb7c28e27e9b44629928ccd0dc9fbb6ae7217860bd5e8be5d"),
];

/// The archives are 30–40 MB.
const MAX_BYTES: u64 = 200 * 1024 * 1024;

fn asset(os: &str, arch: &str) -> Option<(&'static str, &'static str)> {
    ASSETS.iter().find(|(o, a, ..)| *o == os && *a == arch).map(|(_, _, name, sha)| (*name, *sha))
}

pub fn binary(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit.or_else(|| env::var_os("REAPER_PI").map(PathBuf::from)) {
        return if path.is_file() { Ok(path) } else { Err(format!("no pi at {}", path.display())) };
    }
    let dir = paths::data_dir().join("pi").join(VERSION);
    let exe = dir.join("pi").join("pi");
    if !exe.is_file() {
        let (name, sha) = asset(env::consts::OS, env::consts::ARCH).ok_or_else(|| {
            format!(
                "no pinned pi build for {}/{}; install pi yourself and pass --pi PATH",
                env::consts::OS,
                env::consts::ARCH
            )
        })?;
        fetch(name, sha, &dir)?;
    }
    if exe.is_file() { Ok(exe) } else { Err(format!("pi {VERSION} unpacked without {}", exe.display())) }
}

fn fetch(name: &str, sha: &str, dir: &Path) -> Result<(), String> {
    let url = format!("https://github.com/earendil-works/pi/releases/download/v{VERSION}/{name}");
    eprintln!("reaper: fetching pi {VERSION} ({name}); this happens once.");
    let bytes = ureq::get(&url)
        .call()
        .map_err(|e| format!("couldn't download {url}: {e}"))?
        .into_body()
        .into_with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|e| format!("couldn't download {url}: {e}"))?;
    verify(&bytes, sha, name)?;
    unpack(&bytes, dir).map_err(|e| format!("couldn't unpack {name} into {}: {e}", dir.display()))
}

fn verify(bytes: &[u8], expected: &str, name: &str) -> Result<(), String> {
    let actual = hex(&Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{name} failed its checksum (expected {expected}, got {actual}); nothing was installed"))
    }
}

/// Unpacks beside `dir`, then moves into place: no half-installed pi after a crash.
fn unpack(archive: &[u8], dir: &Path) -> std::io::Result<()> {
    let parent = dir.parent().expect("versioned dir has a parent");
    fs::create_dir_all(parent)?;
    let partial = parent.join(format!(".{VERSION}-partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    tar::Archive::new(GzDecoder::new(archive)).unpack(&partial)?;
    if fs::rename(&partial, dir).is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_platform_has_a_pinned_checksum() {
        for (os, arch) in [("linux", "x86_64"), ("linux", "aarch64"), ("macos", "x86_64"), ("macos", "aarch64")] {
            let (name, sha) = asset(os, arch).unwrap();
            assert!(name.ends_with(".tar.gz"));
            assert_eq!(sha.len(), 64);
        }
        assert_eq!(asset("windows", "x86_64"), None, "Windows needs --pi until zip archives are handled");
    }

    #[test]
    fn a_tampered_download_is_refused() {
        let good = hex(&Sha256::digest(b"archive"));
        assert!(verify(b"archive", &good, "x").is_ok());
        assert!(verify(b"archive!", &good, "x").unwrap_err().contains("failed its checksum"));
    }
}
