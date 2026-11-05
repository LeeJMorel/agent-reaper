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
  match: { command: '(^|[;&|(]\s*)(npx |uv run |python3? -m )?(pytest|vitest|jest)\b(?![^;&|]*(::|\.(py|[jt]sx?)\b|\$))' }
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
  or the file path. Keys: `command` for shell commands, `path` for the file an agent reads or
  writes (whichever agent it is), `"*"` for any argument. Patterns are JavaScript regexes,
  searched anywhere in the text, and every key listed must match.
- `judge`: a plain-English description of the bad action, decided by a small model (the
  watcher). Use it only for meaning a pattern can't see, such as "Connecting to, querying or
  migrating a production database." Describe the action itself, not a rule about it. It needs
  a watcher set up, so say so in `note`.
- `tools`: which tools the rule checks. `[bash]` for commands, `[edit, write]` for changing
  files (add `read` to keep agents from reading them too).

## Command patterns

- Fire when the program runs, never when it's only mentioned. Anchor it to the start of a
  command: the start of the text, or just after `;`, `&`, `|` or `(`, which is
  `(^|[;&|(]\s*)`. Allow the launchers that matter before it (`npx `, `uv run `,
  `python3? -m `). Without the anchor, `cat pytest.ini`, `grep pytest` and `pgrep -f pytest`
  break the rule.
- Put `\b` after a program's name and `\s+` between words.
- "Without naming a file" is a lookahead over the rest of that command: `(?![^;&|]*\.py\b)`.
  Treat `$f` and similar variables as file names: a loop over files is fine.
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

- `stops`: three to five actions the rule must stop.
- `allows`: three to five near misses it must let through, above all reading, searching for
  or checking on the same thing (`cat`, `grep`, `ls`, `pgrep`).
- A string is a shell command. For files, write `{ tool: write, path: src/generated/api.ts }`.
- reaper tries every example against the pattern before the rule is added. For `judge` rules
  the examples are shown to the person but not tried.

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
check it with `reaper rules add --check <<'EOF'` … `EOF`, and once they agree, add it with
`reaper rules add <<'EOF'` … `EOF`. reaper puts it in its rules file for you. To change a rule,
take it out with `reaper rules remove <id>` and add the new version. reaper's own files are off
limits to agents.
