//! Shared by the end-to-end tests: a tiny HTTP server for fake model endpoints, and
//! sandbox helpers. Everything else in those tests is the real thing.

#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub struct Response {
    pub content_type: &'static str,
    pub body: String,
}

pub fn json(value: Value) -> Response {
    Response { content_type: "application/json", body: value.to_string() }
}

pub fn sse(body: String) -> Response {
    Response { content_type: "text/event-stream", body }
}

pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p["text"].as_str().map(String::from).unwrap_or_else(|| text_of(&p["content"])))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn read_chunked(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        reader.read_line(&mut size)?;
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            return Ok(body);
        }
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk[..size]);
    }
}

type Handler = dyn Fn(&str, &Value) -> Option<Response> + Send + Sync;

fn serve(mut stream: TcpStream, handler: &Handler) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let (mut request_line, mut length, mut chunked) = (String::new(), 0, false);
    reader.read_line(&mut request_line)?;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let header = line.trim_end().to_ascii_lowercase();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        chunked |= header.starts_with("transfer-encoding:") && header.contains("chunked");
    }
    let body = if chunked {
        read_chunked(&mut reader)?
    } else {
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        body
    };
    let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match handler(&path, &request) {
        Some(response) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.content_type,
                response.body.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(response.body.as_bytes())?;
        }
        None => stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")?,
    }
    stream.flush()
}

pub fn server(handler: impl Fn(&str, &Value) -> Option<Response> + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handler: Arc<Handler> = Arc::new(handler);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            thread::spawn(move || {
                let _ = serve(stream, &*handler);
            });
        }
    });
    url
}

pub fn sandbox(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// No node, no npm.
pub fn minimal_path(dir: &Path, tools: &[&str]) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for tool in tools {
        for root in ["/usr/bin", "/bin"] {
            let path = Path::new(root).join(tool);
            if path.exists() && !bin.join(tool).exists() {
                std::os::unix::fs::symlink(&path, bin.join(tool)).unwrap();
            }
        }
    }
    bin
}

pub fn events(home: &Path) -> Vec<Value> {
    let log = fs::read_to_string(home.join(".local/state/reaper/events.jsonl")).unwrap_or_default();
    log.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

pub fn event<'a>(events: &'a [Value], kind: &str) -> Option<&'a Value> {
    events.iter().find(|e| e["type"] == kind)
}

/// The watcher's side of the fake model: flags any action that mentions `needle`.
pub fn watcher_reply(request: &Value, rule: &str, needle: &str) -> Value {
    let action =
        request["messages"].as_array().and_then(|m| m.last()).map(|m| text_of(&m["content"])).unwrap_or_default();
    let violated: Vec<&str> = if action.contains(needle) { vec![rule] } else { vec![] };
    let content = serde_json::json!({ "violated": violated }).to_string();
    serde_json::json!({ "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }] })
}

/// What a terminal shows, as it arrives: a pty's output, read on a thread.
pub struct Screen(Arc<Mutex<String>>);

impl Screen {
    pub fn follow(mut from: impl Read + Send + 'static) -> Self {
        let text = Arc::new(Mutex::new(String::new()));
        let shared = text.clone();
        thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(n @ 1..) = from.read(&mut chunk) {
                shared.lock().unwrap().push_str(&String::from_utf8_lossy(&chunk[..n]));
            }
        });
        Self(text)
    }

    pub fn wait_for(&self, needle: &str, times: usize) {
        let start = Instant::now();
        while self.0.lock().unwrap().matches(needle).count() < times {
            assert!(start.elapsed() < Duration::from_secs(90), "never saw {needle:?} x{times}:\n{}", self.text());
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

pub const SCRIPT: &str = "/usr/bin/script";

/// A command in a real terminal: a pty from util-linux `script`.
pub struct Terminal {
    child: std::process::Child,
    pub screen: Screen,
    keys: std::process::ChildStdin,
}

impl Terminal {
    /// Runs the shell command `line` in a pty, with `script`'s environment and directory
    /// (`script` is a `Command::new(SCRIPT)` set up by the caller).
    pub fn open(mut script: Command, line: &str) -> Self {
        let mut child = script
            .args(["-q", "-e", "-f", "-c", line, "/dev/null"])
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let screen = Screen::follow(child.stdout.take().unwrap());
        let keys = child.stdin.take().unwrap();
        Self { child, screen, keys }
    }

    pub fn press(&mut self, keys: &[u8]) {
        self.keys.write_all(keys).unwrap();
    }

    pub fn close(mut self) -> String {
        let status = self.child.wait().unwrap();
        let text = self.screen.text();
        assert!(status.success(), "reaper run failed ({status}):\n{text}");
        text
    }
}

pub fn has_terminal() -> bool {
    let found = Path::new(SCRIPT).exists();
    if !found {
        eprintln!("skipped: needs util-linux script for a terminal");
    }
    found
}

pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(90), "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}
