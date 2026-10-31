//! The pi extension, embedded at build time (see build.rs) and unpacked on demand.

use std::fs;
use std::io;
use std::path::PathBuf;

use reaper_core::paths;
use sha2::{Digest, Sha256};

use crate::engine::prefix::hex;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/extension.rs"));
}

const ENTRY: &str = "pi.ts";

/// So each version of the extension unpacks to its own directory.
fn fingerprint() -> String {
    let mut hash = Sha256::new();
    for (path, bytes) in embedded::FILES {
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(bytes);
    }
    hex(&hash.finalize()[..8])
}

pub fn install() -> io::Result<PathBuf> {
    let dir = paths::cache_dir().join("pi-extension").join(fingerprint());
    let entry = dir.join(ENTRY);
    if entry.is_file() {
        return Ok(entry);
    }

    // Unpack beside the final directory, then move it into place, so a crash or a
    // second reaper starting at the same time never sees a half-written copy.
    let partial = dir.with_extension(format!("partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    for (path, bytes) in embedded::FILES {
        let file = partial.join(path);
        fs::create_dir_all(file.parent().expect("files live in a directory"))?;
        fs::write(file, bytes)?;
    }
    if fs::rename(&partial, &dir).is_err() {
        let _ = fs::remove_dir_all(&partial);
        if !entry.is_file() {
            return Err(io::Error::other(format!("couldn't move the extension into {}", dir.display())));
        }
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_extension_is_embedded() {
        let names: Vec<&str> = embedded::FILES.iter().map(|(path, _)| *path).collect();
        assert!(names.contains(&ENTRY), "{ENTRY} missing from {names:?}");
    }

    #[test]
    fn the_extension_needs_no_npm_packages() {
        // pi's standalone binary resolves only its own modules, node: built-ins, and relative files.
        let allowed =
            |spec: &str| spec.starts_with('.') || spec.starts_with("node:") || spec.starts_with("@earendil-works/");
        for (path, bytes) in embedded::FILES {
            let text = std::str::from_utf8(bytes).unwrap();
            // `from "x"` closes every static import and re-export, however many lines it spans.
            for (_, rest) in text.match_indices("from \"").map(|(i, m)| (i, &text[i + m.len()..])) {
                let spec = &rest[..rest.find('"').unwrap()];
                assert!(allowed(spec), "{path} imports {spec}: pi can't load that without npm");
            }
        }
    }
}
