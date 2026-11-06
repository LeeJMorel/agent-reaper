---
name: reaper-rule
description: Write a rule for reaper, the guard that stops coding agents from doing what their person doesn't want. Use when someone asks for a reaper rule, or wants agents stopped, blocked or kept from doing something.
---

# Writing a reaper rule

reaper checks every command an agent runs and every file it touches against rules its person
wrote. When an action breaks a rule, reaper stops it and tells the agent the rule's redirect
instead. The agent never sees the rule itself, only the redirect.

You turn what the person wants, said in their own words, into one rule, written as a YAML
snippet:

```yaml
rule:
  id: whole-test-suite
  tools: [bash]
  match: { command: '^(pytest|vitest|jest)\b(?!.*(::|\.(py|[jt]sx?)\b|\$))' }
  redirect: Run the test files for the code you changed.
stops:
  - pytest
  - cd backend && uv run pytest -n 4 --dist loadgroup
  - npx vitest run
allows:
  - pytest app/users_test.py
  - npx vitest run src/lib/date.test.ts
  - cat pytest.ini
  - pgrep -af pytest
note: Assumed this is for every project.
```

For a rule that belongs to one project, add `project: name`, using a project that already
exists when one fits. For a new project, also add `paths: [~/code/app]` (`*` matches one
folder level, `**` any depth).

## How it's checked

- `match`: regexes over the action's arguments. Use it whenever the thing shows in the command
  or the file path. Keys: `command` for shell commands (see below), `script` for the whole
  shell command at once (only for a pattern across commands, such as a pipe or a loop), `path`
  for the file an agent reads or writes, `content` for the text it writes (a new file, or an
  edit's new text, never the text it replaces), `"*"` for any argument as sent. `path` and
  `content` work whichever agent it is. In examples, an edit is
  `{ tool: edit, path: …, new_string: … }`. Patterns are JavaScript regexes, searched anywhere
  in the text, and every key listed must match.
- `judge`: a plain-English description of the bad action, decided by a small model (the
  watcher). Use it only for meaning a pattern can't see, such as "Connecting to, querying or
  migrating a production database." Describe the action itself, not a rule about it. It needs
  a watcher set up, so say so in `note`.
- `tools`: which tools the rule checks. `[bash]` for commands, `[edit, write]` for changing
  files (add `read` to keep agents from reading them too).

## Command patterns

- reaper tries a `command` pattern on each command the shell would run, one at a time: the
  parts between `;`, `&&`, `||`, `|` and line breaks, and what runs inside `$( )`, backticks
  and `bash -c '…'`. Quotes are taken off and words are joined by single spaces. Comments
  aren't commands, and a heredoc counts as part of the command it's fed to.
- Each command is tried again with what runs it taken off the front: `sudo`, `env`,
  `VAR=value`, `timeout 60`, `nice`, `xargs`, `npx`, `pnpm exec`, `uv run`, `poetry run`,
  `python -m`, a program's directory (`./venv/bin/pytest`) and git's own options
  (`git -C repo`).
- So start the pattern with `^` and the program, and put `\b` after its name: `^pytest\b`
  fires on `uv run pytest` and `cd api && pytest -x`, and never on `cat pytest.ini`,
  `grep pytest` or a commit message that mentions it.
- "Without naming a file" is a lookahead over the rest of the command: `(?!.*\.py\b)`. Treat
  `$f` and similar variables as file names: a loop over files is fine.
- Options can take a value: in `pytest -n 2 --dist loadgroup`, `loadgroup` is a value, not a
  test path.
- Write patterns in single quotes. A single quote inside one is written twice: `''`.

## The redirect

- It's all the agent hears. Write one or two sentences saying what to do instead, as an
  instruction: "Use the local development database for every query and migration."
- Leave out "don't", "never", "avoid" and "stop", and don't repeat the rule back. reaper
  flags both.
- Give the goal, not a number to obey. An agent told "-n 2 at most" runs the whole suite with
  `-n 2`.
- When the trouble is the machine running out of room, add `free: 4G` (memory) or `load: 16`
  (busy). The rule then only applies while free memory is below that or the load is above it
  (Linux), and the agent is told which.

## Examples

- `stops`: three to five actions the rule must stop, one of them behind a wrapper or on a later
  line of a longer command.
- `allows`: three to five near misses it must let through, above all reading, searching for
  or checking on the same thing (`cat`, `grep`, `ls`, `pgrep`), and writing about it (a commit
  message).
- A string is a shell command. For files, write `{ tool: write, path: src/generated/api.ts }`.
- reaper tries every example before the rule is added: on the pattern, and for a `judge` rule
  on the watcher too, when there is one.
- reaper also runs a pattern rule over the person's recent sessions and says how many of their
  agents' calls it would have stopped, with a few of them. If one is something the person
  would want to go through, narrow the pattern.

## Everything else

- `id`: short, lowercase, words joined by `-`, and not one already taken.
- `action: stop` ends the agent's run at the first try instead of redirecting it. Use it only
  for what must never be attempted twice.
- `ask: true` lets an agent ask the person to allow one blocked call, with its reason. Use it
  where a real exception can come up (an agent's own unmerged migration), never for a line
  that must hold every time.
- If the request is unclear, pick the reading that prevents the harm while stopping the
  least, and say what you assumed in `note`.

## In a working session

If you're an agent and your person asks for a rule while you work: show them the snippet,
check it with `reaper rules add --check <<'EOF'` … `EOF`, show them what it would have stopped
in their sessions, and once they agree, add it with `reaper rules add <<'EOF'` … `EOF`. reaper puts it in its rules file for you. To change a rule,
give the new version the same id and add it with `reaper rules add --replace`: it takes the old
one's place, and your person is asked to allow it. `reaper rules remove <id>` takes one out, also
with their say. reaper's own files are off limits to agents.
