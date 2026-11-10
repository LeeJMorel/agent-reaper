//! What a shell command runs. `command` patterns are tried on each command in it, one at a
//! time: the parts between `;`, `&&`, `||`, `|` and newlines, and what runs inside `( )`,
//! `$( )`, backticks, `bash -c '…'`, `eval` and `find -exec`. A pattern that starts with `^`
//! then fires when the program runs, and never when it's only mentioned: in a commit message,
//! a comment, or a search for it. A heredoc belongs to the command it's fed to, so a commit
//! message given on stdin is still the commit's.
//!
//! Each command is tried again with what runs it taken off the front (see `forms`), so one
//! pattern covers `sudo`, `timeout 60`, `uv run`, `python -m` and the rest.
//!
//! It reads the shell's syntax, not its meaning: nothing is expanded, so `$f` stays `$f`.

/// How deep `bash -c "…"` inside `$( … )` inside … is read. Past it, a substitution is skipped
/// over without reading what runs in it.
const DEPTH: usize = 8;

/// Words that only shape what runs: `if git diff --quiet; then git push; fi` runs `git diff
/// --quiet` and `git push`.
const KEYWORDS: &[&str] = &["!", "{", "}", "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "esac"];

/// Shells, which run the text they're given as commands.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

/// Each command `text` runs, as its words with the quotes taken off, in the order they appear.
pub fn commands(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    read(text, 0, &mut out);
    out
}

/// One command as patterns see it: its words joined by single spaces, then the same again each
/// time something that only runs the rest comes off the front. `sudo -u ci timeout 60 uv run
/// pytest -x` is tried as itself, `timeout 60 uv run pytest -x`, `uv run pytest -x` and
/// `pytest -x`.
pub fn forms(words: &[String]) -> Vec<String> {
    peeled(words).iter().map(|w| w.join(" ")).collect()
}

/// The command with everything in front that only runs it taken off: `sudo -E uv run pytest
/// -x` is `pytest -x`.
pub fn bare(words: &[String]) -> Vec<String> {
    peeled(words).pop().expect("never empty")
}

fn peeled(words: &[String]) -> Vec<Vec<String>> {
    let mut all = vec![words.to_vec()];
    while all.len() < 16 {
        match peel(all.last().expect("never empty")) {
            Some(next) => all.push(next),
            None => break,
        }
    }
    all
}

/// The whole shell command less what isn't commands: comments, and heredocs fed to anything but
/// a shell (a file's contents, a commit message). For patterns across commands: a pipe, a loop.
pub fn script(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let (_, mut cuts) = Lexer::new(&chars, 0, 0, false).run(&mut Vec::new());
    cuts.sort_unstable();
    let (mut kept, mut at) = (String::new(), 0);
    for (from, to) in cuts {
        if from > at {
            kept.extend(&chars[at..from]);
        }
        at = at.max(to);
    }
    kept.extend(&chars[at.min(chars.len())..]);
    kept
}

fn read(text: &str, depth: usize, out: &mut Vec<Vec<String>>) {
    if depth <= DEPTH {
        let chars: Vec<char> = text.chars().collect();
        Lexer::new(&chars, 0, depth, false).run(out);
    }
}

/// A heredoc waiting for its body, which starts on the line after the one it's named on.
struct Heredoc {
    delimiter: String,
    /// `<<-`: tabs at the start of its lines are taken off.
    tabs: bool,
    /// The command it's fed to, once that one is read: `Some(None)` when it isn't one.
    owner: Option<Option<usize>>,
}

struct Lexer<'a> {
    s: &'a [char],
    i: usize,
    depth: usize,
    /// Inside `$( )`: stops at the `)` that closes it.
    nested: bool,
    /// `(` opened and not yet closed, so a `)` closes them before it closes `nested`.
    parens: usize,
    /// The command being read.
    words: Vec<String>,
    word: String,
    /// Whether `word` has begun: `''` is a word, if an empty one.
    started: bool,
    heredocs: Vec<Heredoc>,
    /// Where `s` isn't commands: comments, and heredocs fed to anything but a shell.
    cuts: Vec<(usize, usize)>,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a [char], i: usize, depth: usize, nested: bool) -> Self {
        Self {
            s,
            i,
            depth,
            nested,
            parens: 0,
            words: Vec::new(),
            word: String::new(),
            started: false,
            heredocs: Vec::new(),
            cuts: Vec::new(),
        }
    }

    fn peek(&self, n: usize) -> Option<char> {
        self.s.get(self.i + n).copied()
    }

    fn push(&mut self, c: char) {
        self.word.push(c);
        self.started = true;
    }

    /// Reads to the end, or to the `)` closing `nested`, and says where it stopped and where the
    /// text wasn't commands.
    fn run(mut self, out: &mut Vec<Vec<String>>) -> (usize, Vec<(usize, usize)>) {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => {
                    self.end_word();
                    self.i += 1;
                }
                '\n' => {
                    self.i += 1;
                    self.end_command(out);
                    self.bodies(out);
                }
                '#' if !self.started => {
                    let start = self.i;
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                    self.cuts.push((start, self.i));
                }
                ')' if self.nested && self.parens == 0 => {
                    self.end_command(out);
                    return (self.i, self.cuts);
                }
                ';' | '&' | '|' | '(' | ')' => self.operator(out),
                '<' | '>' => self.redirect(out),
                '\\' => {
                    match self.peek(1) {
                        Some('\n') => {}
                        Some(next) => self.push(next),
                        None => {}
                    }
                    self.i += 2;
                }
                '\'' => {
                    self.started = true;
                    self.i += 1;
                    while let Some(c) = self.peek(0) {
                        self.i += 1;
                        if c == '\'' {
                            break;
                        }
                        self.word.push(c);
                    }
                }
                '"' => self.double(out),
                '`' => self.backtick(out),
                '$' => self.dollar(out, false),
                _ => {
                    self.push(c);
                    self.i += 1;
                }
            }
        }
        self.end_command(out);
        self.bodies(out);
        (self.s.len(), self.cuts)
    }

    fn end_word(&mut self) {
        if self.started {
            self.words.push(std::mem::take(&mut self.word));
            self.started = false;
        }
    }

    fn end_command(&mut self, out: &mut Vec<Vec<String>>) {
        self.end_word();
        let at = command(std::mem::take(&mut self.words), self.depth, out);
        for doc in self.heredocs.iter_mut().filter(|d| d.owner.is_none()) {
            doc.owner = Some(at);
        }
    }

    /// `;`, `&&`, `||`, `|`, `&` and parentheses end a command. `a=(x y)` is an array.
    fn operator(&mut self, out: &mut Vec<Vec<String>>) {
        let c = self.s[self.i];
        if c == '&' && self.peek(1) == Some('>') {
            return self.redirect(out);
        }
        if c == '(' && self.started && self.word.ends_with('=') {
            let end = close(self.s, self.i + 1);
            let stop = (end + 1).min(self.s.len());
            self.word.extend(&self.s[self.i..stop]);
            self.i = stop;
            return;
        }
        match c {
            '(' => self.parens += 1,
            ')' => self.parens = self.parens.saturating_sub(1),
            _ => {}
        }
        self.end_command(out);
        self.i += 1;
    }

    /// `>`, `2>&1`, `<<EOF` and the rest. The target stays among the command's words; a
    /// heredoc's body is read once its line ends. `<(…)` runs a command.
    fn redirect(&mut self, out: &mut Vec<Vec<String>>) {
        if self.peek(1) == Some('(') && matches!(self.peek(0), Some('<' | '>')) {
            self.started = true;
            let start = self.i;
            self.i += 2;
            return self.substitute(start, out);
        }
        let mut op = String::new();
        if self.started && !self.word.is_empty() && self.word.chars().all(|c| c.is_ascii_digit()) {
            op = std::mem::take(&mut self.word);
            self.started = false;
        } else {
            self.end_word();
        }
        if self.peek(0) == Some('&') {
            op.push('&');
            self.i += 1;
        }
        let Some(first) = self.peek(0) else { return };
        op.push(first);
        self.i += 1;
        match (first, self.peek(0)) {
            ('<', Some('<')) => {
                self.i += 1;
                if self.peek(0) == Some('<') {
                    self.i += 1;
                    self.words.push("<<<".into());
                    return;
                }
                let tabs = self.peek(0) == Some('-');
                if tabs {
                    self.i += 1;
                }
                while matches!(self.peek(0), Some(' ' | '\t')) {
                    self.i += 1;
                }
                let delimiter = self.delimiter();
                self.heredocs.push(Heredoc { delimiter, tabs, owner: None });
                return;
            }
            ('>', Some(next @ ('>' | '|' | '&'))) | ('<', Some(next @ ('>' | '&'))) => {
                op.push(next);
                self.i += 1;
            }
            _ => {}
        }
        if op.ends_with('&') {
            while let Some(c) = self.peek(0).filter(|c| c.is_ascii_digit() || *c == '-') {
                op.push(c);
                self.i += 1;
            }
        }
        self.words.push(op);
    }

    /// A heredoc's delimiter, quotes off: `EOF`, `'EOF'`, `"EOF"`.
    fn delimiter(&mut self) -> String {
        let mut d = String::new();
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => break,
                '\'' | '"' => {
                    self.i += 1;
                    while let Some(q) = self.peek(0) {
                        self.i += 1;
                        if q == c {
                            break;
                        }
                        d.push(q);
                    }
                }
                '\\' => {
                    if let Some(next) = self.peek(1) {
                        d.push(next);
                    }
                    self.i += 2;
                }
                _ => {
                    d.push(c);
                    self.i += 1;
                }
            }
        }
        d
    }

    /// The bodies of the heredocs named on the line that just ended. A shell given one reads
    /// it as commands; anything else gets it as one more word, at the end.
    fn bodies(&mut self, out: &mut Vec<Vec<String>>) {
        for doc in std::mem::take(&mut self.heredocs) {
            let start = self.i;
            let mut lines = Vec::new();
            while self.i < self.s.len() {
                let end = self.s[self.i..].iter().position(|&c| c == '\n').map_or(self.s.len(), |n| self.i + n);
                let line: String = self.s[self.i..end].iter().collect();
                self.i = (end + 1).min(self.s.len());
                let line = if doc.tabs { line.trim_start_matches('\t') } else { line.as_str() };
                if line.trim_end_matches('\r') == doc.delimiter {
                    break;
                }
                lines.push(line.to_string());
            }
            let body = lines.join("\n");
            match doc.owner.flatten() {
                Some(at) if runs_stdin(&out[at]) => read(&body, self.depth + 1, out),
                owner => {
                    self.cuts.push((start, self.i));
                    if let Some(at) = owner.filter(|_| !lines.is_empty()) {
                        out[at].push(body);
                    }
                }
            }
        }
    }

    fn double(&mut self, out: &mut Vec<Vec<String>>) {
        self.started = true;
        self.i += 1;
        while let Some(c) = self.peek(0) {
            match c {
                '"' => {
                    self.i += 1;
                    return;
                }
                '\\' => match self.peek(1) {
                    Some(next @ ('$' | '`' | '"' | '\\')) => {
                        self.word.push(next);
                        self.i += 2;
                    }
                    Some('\n') => self.i += 2,
                    _ => {
                        self.word.push('\\');
                        self.i += 1;
                    }
                },
                '$' => self.dollar(out, true),
                '`' => self.backtick(out),
                _ => {
                    self.word.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// `$(…)` runs a command; `$((…))`, `${…}` and `$x` are only text. Inside double quotes,
    /// `$'` and `$"` are a dollar sign.
    fn dollar(&mut self, out: &mut Vec<Vec<String>>, quoted: bool) {
        self.started = true;
        let start = self.i;
        match self.peek(1) {
            Some('(') if self.peek(2) == Some('(') => self.skip(start, close(self.s, start + 2)),
            Some('(') => {
                self.i += 2;
                self.substitute(start, out);
            }
            Some('{') => {
                let end = (start + 2..self.s.len()).find(|&j| self.s[j] == '}').unwrap_or(self.s.len());
                self.skip(start, end);
            }
            Some('\'') if !quoted => {
                self.i += 2;
                while let Some(c) = self.peek(0) {
                    self.i += 1;
                    match c {
                        '\'' => break,
                        '\\' => {
                            if let Some(next) = self.peek(0) {
                                self.word.push(next);
                                self.i += 1;
                            }
                        }
                        _ => self.word.push(c),
                    }
                }
            }
            Some('"') if !quoted => {
                self.i += 1;
                self.double(out);
            }
            _ => {
                self.word.push('$');
                self.i += 1;
            }
        }
    }

    /// The text from `start` through `end` goes into the word as it is.
    fn skip(&mut self, start: usize, end: usize) {
        let stop = (end + 1).min(self.s.len());
        self.word.extend(&self.s[start..stop]);
        self.i = stop;
    }

    /// `$(` or `<(` from `start`, with `self.i` just inside: what runs in it is read as commands
    /// of its own, and its text goes into the word as it is.
    fn substitute(&mut self, start: usize, out: &mut Vec<Vec<String>>) {
        let end = if self.depth < DEPTH {
            let (end, cuts) = Lexer::new(self.s, self.i, self.depth + 1, true).run(out);
            self.cuts.extend(cuts);
            end
        } else {
            close(self.s, self.i)
        };
        self.skip(start, end);
    }

    fn backtick(&mut self, out: &mut Vec<Vec<String>>) {
        self.started = true;
        let start = self.i;
        self.i += 1;
        let mut inner = String::new();
        while let Some(c) = self.peek(0) {
            self.i += 1;
            match c {
                '`' => break,
                '\\' if matches!(self.peek(0), Some('`' | '\\' | '$')) => {
                    inner.push(self.s[self.i]);
                    self.i += 1;
                }
                _ => inner.push(c),
            }
        }
        read(&inner, self.depth + 1, out);
        self.word.extend(&self.s[start..self.i]);
    }
}

/// Where the `(` just before `from` closes, by counting, for what only needs skipping over.
fn close(s: &[char], from: usize) -> usize {
    let (mut depth, mut i) = (1, from);
    while let Some(&c) = s.get(i) {
        match c {
            '\\' => i += 1,
            '\'' => {
                i += 1;
                while s.get(i).is_some_and(|&c| c != '\'') {
                    i += 1;
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// Adds one command to `out`, then whatever runs inside it: a shell's `-c` text, `eval`'s, and
/// `find -exec`'s. Returns where it went, unless it isn't one: a loop's header, or only
/// keywords.
fn command(mut words: Vec<String>, depth: usize, out: &mut Vec<Vec<String>>) -> Option<usize> {
    loop {
        let keywords = words.iter().take_while(|w| KEYWORDS.contains(&w.as_str())).count();
        words.drain(..keywords);
        if words.first().is_some_and(|w| w == "function") {
            words.drain(..2.min(words.len()));
        } else {
            break;
        }
    }
    if words.is_empty() || matches!(words[0].as_str(), "for" | "select" | "case") {
        return None;
    }
    let at = out.len();
    out.push(words.clone());
    let inner = bare(&words);
    let args = &inner[1..];
    match basename(&inner[0]) {
        sh if SHELLS.contains(&sh) => {
            if let Some(script) = shell_text(args) {
                read(script, depth + 1, out);
            } else if let Some(text) = args.iter().position(|w| w == "<<<").and_then(|p| args.get(p + 1))
                && runs_stdin(&inner)
            {
                read(text, depth + 1, out);
            }
        }
        "eval" => read(&args.join(" "), depth + 1, out),
        "find" => {
            let mut rest = args;
            while let Some(p) = rest.iter().position(|w| matches!(w.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
            {
                let after = &rest[p + 1..];
                let end = after.iter().position(|w| w == ";" || w == "+").unwrap_or(after.len());
                command(after[..end].to_vec(), depth, out);
                rest = &after[end..];
            }
        }
        _ => {}
    }
    Some(at)
}

/// A shell's options, up to its first other argument: whether `-c` is among them, and where
/// that argument is.
fn shell_options(args: &[String]) -> (bool, usize) {
    let (mut c, mut i) = (false, 0);
    while let Some(w) = args.get(i) {
        if w == "--" {
            return (c, i + 1);
        }
        if !(w.starts_with('-') || w.starts_with('+')) || w.len() == 1 {
            break;
        }
        if matches!(w.as_str(), "-o" | "+o" | "--rcfile" | "--init-file") {
            i += 1;
        } else if !w.starts_with("--") && w[1..].contains('c') {
            c = true;
        }
        i += 1;
    }
    (c, i)
}

/// The text `bash -c '…'` runs.
fn shell_text(args: &[String]) -> Option<&str> {
    let (c, i) = shell_options(args);
    c.then(|| args.get(i).map(String::as_str)).flatten()
}

/// Whether this command is a shell reading its commands from stdin: given no script to run.
fn runs_stdin(words: &[String]) -> bool {
    let inner = bare(words);
    if !SHELLS.contains(&basename(&inner[0])) {
        return false;
    }
    let (c, i) = shell_options(&inner[1..]);
    !c && inner[1..].get(i).is_none_or(|w| redirection(w))
}

/// `>`, `2>`, `&>`, `<<<`: a redirection's operator, kept among a command's words.
fn redirection(word: &str) -> bool {
    word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&').starts_with(['<', '>'])
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().filter(|b| !b.is_empty()).unwrap_or(word)
}

/// `NAME=value` in front of a command.
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let name = name.strip_suffix('+').unwrap_or(name);
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// What follows a wrapper's own options. `values` are its options that take the next word.
fn options<'w>(words: &'w [String], values: &[&str]) -> &'w [String] {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if w == "--" {
            return &words[i + 1..];
        }
        if !w.starts_with('-') || w == "-" {
            break;
        }
        i += if values.contains(&w.as_str()) { 2 } else { 1 };
    }
    words.get(i..).unwrap_or_default()
}

const UV_RUN: &[&str] = &[
    "--with",
    "--with-requirements",
    "--with-editable",
    "--python",
    "-p",
    "--project",
    "--directory",
    "--package",
    "--extra",
    "--group",
    "--env-file",
    "--index",
    "--index-url",
    "--extra-index-url",
    "--from",
];

/// The command with one thing that only runs the rest taken off its front, if it has one:
/// variable assignments, a program's directory, `sudo`, `env`, `timeout`, `nice`, `xargs`…,
/// the package runners (`npx`, `uv run`, `poetry run`, `pnpm exec`…), an interpreter given a
/// module or a script (`python -m pytest`, `node build.js`, `bash deploy.sh`), and git's own
/// options (`git -C repo push` is `git push`).
fn peel(words: &[String]) -> Option<Vec<String>> {
    let first = words.first()?;
    if assignment(first) {
        let rest: Vec<String> = words.iter().skip_while(|w| assignment(w)).cloned().collect();
        return (!rest.is_empty()).then_some(rest);
    }
    let name = basename(first);
    if name != first {
        let mut named = words.to_vec();
        named[0] = name.to_string();
        return Some(named);
    }
    let rest = &words[1..];
    let sub = rest.first().map_or("", String::as_str);
    let after: &[String] = match name {
        "sudo" => options(rest, &["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-T", "-U", "-R"]),
        "doas" => options(rest, &["-u", "-C"]),
        "env" => options(rest, &["-u", "-C", "--unset", "--chdir"]),
        "timeout" => options(rest, &["-s", "-k", "--signal", "--kill-after"]).get(1..)?,
        "nice" => options(rest, &["-n", "--adjustment"]),
        "ionice" => options(rest, &["-c", "-n", "--class", "--classdata"]),
        "nohup" => rest,
        "time" => options(rest, &["-f", "-o", "--format", "--output"]),
        "exec" => options(rest, &["-a"]),
        // `command -v git` only looks git up.
        "command" if rest.iter().take_while(|w| w.starts_with('-')).any(|w| w.contains(['v', 'V'])) => return None,
        "command" => options(rest, &[]),
        "xargs" => options(
            rest,
            &[
                "-a",
                "-d",
                "-E",
                "-I",
                "-L",
                "-n",
                "-P",
                "-s",
                "--arg-file",
                "--delimiter",
                "--max-args",
                "--max-lines",
                "--max-procs",
                "--max-chars",
            ],
        ),
        "stdbuf" => options(rest, &["-i", "-o", "-e"]),
        "npx" | "pnpx" | "bunx" => options(rest, &["-p", "--package"]),
        "uvx" => options(rest, UV_RUN),
        "uv" | "poetry" | "pipenv" | "pdm" | "hatch" | "rye" | "pipx" if sub == "run" => options(&rest[1..], UV_RUN),
        "conda" | "mamba" | "micromamba" if sub == "run" => {
            options(&rest[1..], &["-n", "-p", "--name", "--prefix", "--cwd"])
        }
        "bundle" if sub == "exec" => &rest[1..],
        "npm" | "bun" if matches!(sub, "exec" | "x") => options(&rest[1..], &["-p", "--package"]),
        // `pnpm vitest` runs vitest, as `pnpm exec vitest` does.
        "pnpm" | "yarn" => {
            let after = options(rest, &["--filter", "-F", "-C", "--dir", "--cwd"]);
            match after.first().map(String::as_str) {
                Some("exec" | "dlx") => options(&after[1..], &["-p", "--package"]),
                _ => after,
            }
        }
        "git" => {
            let after = options(rest, &["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"]);
            if after.len() == rest.len() {
                return None;
            }
            return Some(std::iter::once(first.clone()).chain(after.iter().cloned()).collect());
        }
        python if python.strip_prefix("python").is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.')) => {
            let mut i = 0;
            loop {
                let w = rest.get(i)?;
                if w == "-m" {
                    break rest.get(i + 1..)?;
                }
                // `-` reads the program from stdin, often a heredoc, which is code, not a path.
                if w == "-c" || w == "-" {
                    return None;
                }
                if !w.starts_with('-') {
                    break &rest[i..];
                }
                i += if matches!(w.as_str(), "-X" | "-W") { 2 } else { 1 };
            }
        }
        "node"
            if rest
                .iter()
                .take_while(|w| w.starts_with('-'))
                .any(|w| ["-e", "--eval", "-p", "--print", "-"].contains(&w.as_str())) =>
        {
            return None;
        }
        "node" => options(rest, &["-r", "--require", "--import", "--loader"]),
        sh if SHELLS.contains(&sh) => {
            let (c, i) = shell_options(rest);
            match rest.get(i) {
                Some(script) if !c && !redirection(script) => &rest[i..],
                _ => return None,
            }
        }
        _ => return None,
    };
    (!after.is_empty()).then(|| after.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(text: &str) -> Vec<String> {
        commands(text).iter().map(|w| w.join(" ")).collect()
    }

    fn every_form(text: &str) -> Vec<String> {
        commands(text).iter().flat_map(|w| forms(w)).collect()
    }

    #[test]
    fn commands_are_the_parts_between_operators_and_lines() {
        assert_eq!(runs("cd repo && git push -f"), ["cd repo", "git push -f"]);
        assert_eq!(runs("cd repo\ngit push --force origin main"), ["cd repo", "git push --force origin main"]);
        assert_eq!(runs("make || true; ls | wc -l & wait"), ["make", "true", "ls", "wc -l", "wait"]);
        assert_eq!(runs("(cd a && make) 2>&1 | tail -5"), ["cd a", "make", "2>&1", "tail -5"]);
        assert_eq!(runs("pytest -q \\\n  tests/a.py"), ["pytest -q tests/a.py"]);
        assert_eq!(runs("pytest 2>&1 >/dev/null &>log"), ["pytest 2>&1 > /dev/null &> log"]);
    }

    #[test]
    fn quotes_comments_and_heredocs_are_not_commands() {
        assert_eq!(runs("git commit -m 'why; git push -f is out'"), ["git commit -m why; git push -f is out"]);
        assert_eq!(runs("git status # then git push -f"), ["git status"]);
        assert_eq!(runs("echo \"a\"'b'c\\ d"), ["echo abc d"]);
        assert_eq!(runs("echo $'it\\'s' \"cost: 5$\""), ["echo it's cost: 5$"]);
        let notes = "cat <<'EOF' > notes.md\nRun: cd x && git push -f\nEOF\nls";
        assert_eq!(runs(notes), ["cat > notes.md Run: cd x && git push -f", "ls"], "the body is cat's");
        assert_eq!(runs("cat <<-EOF\n\tone\n\tEOF\necho done"), ["cat one", "echo done"]);
    }

    #[test]
    fn what_runs_inside_a_command_is_read_too() {
        assert_eq!(runs("echo `git push -f`"), ["git push -f", "echo `git push -f`"]);
        assert_eq!(
            runs("x=$(git rev-parse HEAD) && echo $x"),
            ["git rev-parse HEAD", "x=$(git rev-parse HEAD)", "echo $x"]
        );
        assert_eq!(runs("bash -c 'git push -f'"), ["bash -c git push -f", "git push -f"]);
        assert_eq!(runs("sudo sh -ec \"cd /srv && make\""), ["sudo sh -ec cd /srv && make", "cd /srv", "make"]);
        assert_eq!(runs("eval git push -f"), ["eval git push -f", "git push -f"]);
        assert_eq!(runs("find . -name '*.pyc' -exec rm {} \\;"), ["find . -name *.pyc -exec rm {} ;", "rm {}"]);
        assert_eq!(runs("bash <<'EOF'\ngit push -f\nEOF"), ["bash", "git push -f"], "a shell reads its heredoc");
        assert_eq!(runs("bash 2>/dev/null <<EOF\nmake\nEOF"), ["bash 2> /dev/null", "make"]);
        assert_eq!(runs("python3 - <<EOF\nprint(1)\nEOF"), ["python3 - print(1)"], "anything else gets it as input");
        assert_eq!(runs("bash <<< 'git push -f'"), ["bash <<< git push -f", "git push -f"]);
        assert_eq!(runs("diff <(git show a) b"), ["git show a", "diff <(git show a) b"]);
        assert_eq!(runs("arr=(a b) && make"), ["arr=(a b)", "make"], "an array isn't a subshell");
    }

    #[test]
    fn a_commit_message_from_a_heredoc_stays_the_commits() {
        let commit = "git commit -m \"$(cat <<'EOF'\nDon't push: cd x && git push -f\n\nCo-Authored-By: someone\nEOF\n)\" && git push";
        let found = runs(commit);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("cat Don't push"), "{found:?}");
        assert!(
            found[1].starts_with("git commit -m $(cat <<'EOF'") && found[1].contains("Co-Authored-By"),
            "{found:?}"
        );
        assert_eq!(found[2], "git push", "the apostrophe doesn't swallow what follows");
        assert_eq!(runs("git commit -F - <<EOF\nsubject\nEOF"), ["git commit -F - subject"]);
    }

    #[test]
    fn a_script_is_the_text_less_comments_and_heredocs_that_are_not_commands() {
        let written =
            "cat > poll.sh <<'EOF'\nwhile true; do gh run view; sleep 30; done\nEOF\nchmod +x poll.sh # then run it";
        assert_eq!(script(written), "cat > poll.sh <<'EOF'\nchmod +x poll.sh ");
        let run = "bash <<'EOF'\nwhile true; do gh run view; sleep 30; done\nEOF";
        assert_eq!(script(run), run, "a shell's heredoc is commands");
        let nested = "x=$(cat <<EOF\nwhile\nEOF\n) && make | tail";
        assert_eq!(script(nested), "x=$(cat <<EOF\n) && make | tail");
        assert_eq!(script("make 2>&1 | tail -5"), "make 2>&1 | tail -5");
    }

    #[test]
    fn keywords_shape_what_runs_and_loop_headers_run_nothing() {
        assert_eq!(runs("if ! git diff --quiet; then git push -f; fi"), ["git diff --quiet", "git push -f"]);
        assert_eq!(runs("for f in a b; do pytest $f; done"), ["pytest $f"]);
        assert_eq!(runs("while true; do\n  sleep 5\ndone"), ["true", "sleep 5"]);
        assert_eq!(runs("{ make; make test; }"), ["make", "make test"]);
        assert_eq!(runs("deploy() { git push -f; }"), ["deploy", "git push -f"]);
        assert_eq!(runs("case $x in a) git push -f;; esac"), ["git push -f"]);
    }

    #[test]
    fn each_command_is_tried_again_without_what_runs_it() {
        let forms = |text: &str| forms(&commands(text)[0]);
        assert_eq!(
            forms("sudo -u ci timeout 60 uv run pytest -x"),
            ["sudo -u ci timeout 60 uv run pytest -x", "timeout 60 uv run pytest -x", "uv run pytest -x", "pytest -x"]
        );
        assert_eq!(
            forms("GIT_TRACE=1 A=b git -C repo -c x=y push -f"),
            ["GIT_TRACE=1 A=b git -C repo -c x=y push -f", "git -C repo -c x=y push -f", "git push -f"]
        );
        assert_eq!(
            forms("env -i PATH=/bin nice -n 5 make"),
            ["env -i PATH=/bin nice -n 5 make", "PATH=/bin nice -n 5 make", "nice -n 5 make", "make"]
        );
        assert_eq!(
            forms("./.venv/bin/python3 -u -m pytest tests"),
            ["./.venv/bin/python3 -u -m pytest tests", "python3 -u -m pytest tests", "pytest tests"]
        );
        assert_eq!(
            forms("uv run python scripts/drop.py --all"),
            [
                "uv run python scripts/drop.py --all",
                "python scripts/drop.py --all",
                "scripts/drop.py --all",
                "drop.py --all"
            ]
        );
        assert_eq!(forms("pnpm --filter web exec vitest run").last().unwrap(), "vitest run");
        assert_eq!(forms("pnpm vitest run").last().unwrap(), "vitest run");
        assert_eq!(forms("npx -y prettier --write .").last().unwrap(), "prettier --write .");
        assert_eq!(forms("xargs -n 1 git push -f").last().unwrap(), "git push -f");
        assert_eq!(forms("bash -e deploy.sh prod").last().unwrap(), "deploy.sh prod");
        assert_eq!(forms("node --import tsx scripts/seed.ts").last().unwrap(), "seed.ts");
        for alone in ["command -v git", "python3 -c 'print(1)'", "node -e 'x()'", "timeout 60", "sudo -v", "git status"]
        {
            assert_eq!(forms(alone).len(), 1, "{alone}");
        }
        for stdin in ["python3 - <<'EOF'\nprint(\"backend/vitest\")\nEOF", "node - <<'EOF'\nrun('bin/pytest')\nEOF"] {
            assert_eq!(forms(stdin).len(), 1, "a program on stdin is code, not a command: {stdin}");
        }
    }

    #[test]
    fn nothing_breaks_it() {
        for text in [
            "", "'", "\"", "$(", "`", "<<", "a <<EOF", "$((1+", "${x", "\\", "((((", "))))", "a=(", "|&;", "<(", "x >&",
        ] {
            let _ = every_form(text);
        }
        let deep = "$(".repeat(5000);
        let _ = every_form(&deep);
    }
}
