//! Embeds the pi extension (the TypeScript under extensions/pi/src) in the binary, so
//! `reaper run` needs nothing else installed: no Node, no npm.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let src =
        manifest.join("../../extensions/pi/src").canonicalize().expect("extension sources at <repo>/extensions/pi/src");
    println!("cargo:rerun-if-changed={}", src.display());

    let mut files = Vec::new();
    collect(&src, &src, &mut files);
    files.sort();

    let mut out = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in &files {
        writeln!(out, "    ({relative:?}, include_bytes!({absolute:?})),").expect("writing to a String");
    }
    out.push_str("];\n");
    let dest = PathBuf::from(env::var("OUT_DIR").expect("set by cargo")).join("extension.rs");
    fs::write(dest, out).expect("writing the embedded file table");
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("readable source directory") {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if path.extension().is_some_and(|e| e == "ts") {
            let relative = path.strip_prefix(root).expect("under root").to_string_lossy().replace('\\', "/");
            files.push((relative, path.to_string_lossy().into_owned()));
        }
    }
}
