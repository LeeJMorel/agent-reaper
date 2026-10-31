//! Follows the event log as it grows. Only new bytes are read on each poll; a
//! half-written last line waits until its newline arrives; if the file is
//! truncated or replaced, reading starts over.

use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::events::{Event, parse_line};

#[derive(Debug, PartialEq)]
pub enum Update {
    Unchanged,
    Appended(Vec<Event>),
    /// The log was replaced or truncated: these events are the whole log now.
    Reset(Vec<Event>),
}

pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    identity: Option<u64>,
    /// Lines that were not valid events since the last reset (e.g. cut short by a crash).
    pub skipped: usize,
}

impl Tail {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), offset: 0, partial: Vec::new(), identity: None, skipped: 0 }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn poll(&mut self) -> io::Result<Update> {
        let meta = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if self.offset == 0 && self.identity.is_none() {
                    return Ok(Update::Unchanged);
                }
                self.restart(None);
                return Ok(Update::Reset(Vec::new()));
            }
            Err(e) => return Err(e),
        };

        let identity = identity(&meta);
        let replaced = (self.identity.is_some() && identity != self.identity) || meta.len() < self.offset;
        if replaced {
            self.restart(identity);
            return Ok(Update::Reset(self.read_new()?));
        }
        self.identity = identity;
        if meta.len() == self.offset {
            return Ok(Update::Unchanged);
        }
        Ok(Update::Appended(self.read_new()?))
    }

    fn restart(&mut self, identity: Option<u64>) {
        self.offset = 0;
        self.partial.clear();
        self.identity = identity;
        self.skipped = 0;
    }

    fn read_new(&mut self) -> io::Result<Vec<Event>> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = std::mem::take(&mut self.partial);
        self.offset += file.read_to_end(&mut buf)? as u64;

        let complete = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        self.partial = buf.split_off(complete);

        let mut events = Vec::new();
        for line in buf.split(|&b| b == b'\n').filter(|l| !l.trim_ascii().is_empty()) {
            match std::str::from_utf8(line).ok().and_then(parse_line) {
                Some(event) => events.push(event),
                None => self.skipped += 1,
            }
        }
        Ok(events)
    }
}

/// Distinguishes a replaced file from a grown one.
#[cfg(unix)]
fn identity(meta: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn identity(_: &Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(agent: &str) -> String {
        format!("{{\"at\":1,\"host\":\"pi\",\"agent\":\"{agent}\",\"type\":\"agent.idle\"}}\n")
    }

    fn agents(update: Update) -> Vec<String> {
        match update {
            Update::Appended(events) | Update::Reset(events) => events.into_iter().map(|e| e.agent).collect(),
            Update::Unchanged => vec![],
        }
    }

    fn append(path: &Path, text: &str) {
        fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reaper-tail-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("events.jsonl")
    }

    #[test]
    fn reads_only_what_was_appended() {
        let path = scratch("append");
        let mut tail = Tail::new(&path);
        assert_eq!(tail.poll().unwrap(), Update::Unchanged, "missing file is just empty");

        append(&path, &(line("a") + &line("b")));
        assert_eq!(agents(tail.poll().unwrap()), ["a", "b"]);
        assert_eq!(tail.poll().unwrap(), Update::Unchanged);

        append(&path, &line("c"));
        assert_eq!(agents(tail.poll().unwrap()), ["c"]);
    }

    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let path = scratch("partial");
        let mut tail = Tail::new(&path);
        let whole = line("a");
        let (first, rest) = whole.split_at(20);

        append(&path, first);
        assert_eq!(agents(tail.poll().unwrap()), Vec::<String>::new());
        append(&path, rest);
        assert_eq!(agents(tail.poll().unwrap()), ["a"]);
        assert_eq!(tail.skipped, 0);
    }

    #[test]
    fn truncation_and_replacement_start_over() {
        let path = scratch("reset");
        let mut tail = Tail::new(&path);
        append(&path, &(line("a") + &line("b")));
        tail.poll().unwrap();

        fs::write(&path, line("c")).unwrap();
        assert!(matches!(tail.poll().unwrap(), Update::Reset(e) if e.len() == 1 && e[0].agent == "c"));

        let fresh = path.with_extension("new");
        fs::write(&fresh, line("d") + &line("e")).unwrap();
        fs::rename(&fresh, &path).unwrap();
        assert!(matches!(tail.poll().unwrap(), Update::Reset(e) if e.len() == 2));

        fs::remove_file(&path).unwrap();
        assert_eq!(tail.poll().unwrap(), Update::Reset(vec![]));
        assert_eq!(tail.poll().unwrap(), Update::Unchanged);
    }

    #[test]
    fn junk_lines_are_counted_and_skipped() {
        let path = scratch("junk");
        let mut tail = Tail::new(&path);
        append(&path, &format!("{}garbage\n\n{}", line("a"), line("b")));
        assert_eq!(agents(tail.poll().unwrap()), ["a", "b"]);
        assert_eq!(tail.skipped, 1);
    }
}
