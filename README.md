<p align="center">
  <img src="assets/reaper.svg" width="128" alt="A pixel-art grim reaper with a scythe, in front of a full moon">
</p>

<h1 align="center">reaper</h1>

<p align="center">Rules for coding agents that the agents never see.</p>

---

**Kill your agents, mercilessly and with gusto.**
It's not murder if you can resume them.

## What it does

You write down what your coding agents must never do: force-push, touch the production
database, edit generated code, whatever matters in your project. The agents never see that
list.

reaper watches everything an agent does, every command it runs and every file it reads or
edits, and, for rules that ask, what it says. When something breaks a rule, reaper stops it
and tells the agent what to do instead: not "don't touch production" but "use the local
database". The agent carries on with the better instruction and never learns the rule. In
pi, the attempt is wiped from its memory too.

An agent that breaks the same rule three times, or keeps repeating the same step, has its
session ended. Agents you start through reaper are shut down after five idle minutes to free
the memory, and pick up exactly where they were when you come back.

Why hide the rules? Because telling a model "don't" works less well than you'd hope. In one
study, models told not to bring up a topic still did: two open models as often as when told
nothing, one of them more often, and even GPT-4 in one reply out of eight. Naming the thing
can put it on the model's mind. [docs/background.md](docs/background.md) has the evidence,
what reaper has measured so far, and the terms researchers use for all this (in short, a
runtime enforcement monitor).

## Install

```sh
git clone https://github.com/LeeJMorel/reaper
cargo install --path reaper/crates/reaper
```

That's the whole install: a single `reaper` binary (Rust 1.89+) with the pi extension built in.

## Quick start

Put your rules in `~/.config/reaper/rules.yaml`:

```yaml
rules:
  - id: force-push
    tools: [bash]
    match: { command: 'git\s+push\b.*(--force\b|-f\b)' }
    redirect: Push to a new branch and open a pull request for the change.

projects:
  my-app:
    paths: [~/code/my-app]
    rules:
      - id: prod-db
        tools: [bash]
        judge: Connecting to, querying or migrating a production database.
        redirect: Use the local development database for every query and migration.
```

Check them, start a guarded agent, and watch:

```sh
reaper lint                   # check the rules file
reaper run                    # a guarded pi session in this directory
reaper claude                 # …or a guarded Claude Code session
reaper install claude-code    # …or guard every `claude` you start
reaper                        # the console
```

## Rules

Each rule describes a violation privately and says what to do instead.

| Key | |
|---|---|
| `match` | Regexes over the call's arguments: `command`, `path` (any file read or written), or `"*"` for all of them. Deterministic and instant. |
| `judge` | A plain-English description, decided by the watcher model. |
| `redirect` | The only thing the agent ever hears. Write it as an instruction, not a prohibition; `reaper lint` flags redirects that say "don't". |
| `tools` | Only check these tools. `message` means what the agent says: only rules that list it look at replies, judged by their `text`. |
| `action: stop` | Terminate on the first violation instead of redirecting. |

Global rules apply everywhere; project rules apply under the paths you list (`*` matches one
level, `**` any depth). Agents are always kept out of reaper's own files. A rule that fires
`strikes` times (3) in a session ends it.

**The watcher** is a small model that judges `judge` rules: a trusted monitor watching a
larger agent. It can be any OpenAI-compatible endpoint (Ollama, llama.cpp, vLLM, OpenRouter…)
or Anthropic. Its prompt runs from most to least stable (rules → task → action), so checks
reuse the model's prefix cache; on OpenAI-compatible endpoints its answer is constrained to a
JSON verdict over your rule ids, with reasoning off; and when you submit a prompt it is sent
your rules and the new task ahead of the first check, so a model unloaded while you were away
is ready in time.

**Going in circles.** The same call that does something (a command, an edit, a fetch) made
`repeats` times in a row (5), with only reads and searches in between, counts as a violation:
it's suppressed, the agent is told to try a different approach, and it counts toward strikes.
`repeats: 0` turns this off.

See [examples/rules.yaml](examples/rules.yaml) for the full format.

## Agents

|  | suppress | remove from context | redirect | terminate |
|---|:-:|:-:|:-:|:-:|
| [pi](https://github.com/earendil-works/pi), via `reaper run` | ✓ | ✓ | ✓ | ✓ |
| Claude Code, via `reaper claude` or `reaper install claude-code` | ✓ |  | ✓ | ✓ |

Claude Code's hooks can't edit the conversation, so a blocked attempt stays in its history;
the agent still only hears the redirect, never the rule. Replies are checked differently too:
pi checks what the agent wrote at the end of every turn and cuts a bad reply, while Claude Code
checks its final reply when it stops and, if it breaks a rule, keeps it working with the
redirect. The first `reaper run` downloads a pinned pi release and checks it against a
built-in checksum.

## Idle sessions

Most agents spend most of their lives waiting on you, holding memory the whole time. reaper
evicts an interactive session once it has waited 5 minutes: pi started with `reaper run`, or
Claude Code started with `reaper claude`. It doesn't wait for memory to run short. Being
wrong is cheap: the conversation is on disk, reaper offers to resume it where you are, and
the resumed session sends the same prompt prefix, so the provider's prompt cache still
applies.

A session is idle only when its turn is over, nothing has been typed, no dialog is open and
nothing is queued. A half-written prompt resets the countdown. Claude Code fires no hook
while idle, so `reaper claude` starts it and stays its parent, and won't evict it while a
shell is still running under it (a dev server, a build). The console shows evicted
sessions as `◌ reaped`.

```yaml
reap:
  idle: 5m    # or 90s, 1h, off, auto
  live: 4     # at most 4 sessions running; a fifth evicts the longest idle
  free: 2G    # while free memory is below 2G, evict the longest idle (Linux)
```

Under pressure (`live`, `free`, both off unless set), the least recently active idle session
goes first and a working session never does; the one chosen confirms it is still idle before
it goes. `idle: auto` picks the timeout with the least expected cost over your own waits in
the event log: each idle minute held, against each return that would find its session gone,
with a resume counted as costing 10 idle minutes.

## Try it

You don't need an agent or a model:

```sh
examples/demo.sh
```

Three pretend agents work on a pretend project through the real Claude Code hook. One gets
redirected, and one keeps reaching for secrets until it dies. The demo uses its own rules and
log, so yours aren't touched.

## The console

Run `reaper` with no arguments:

1. **live**: agents, violations and strikes as they happen
2. **rules**: what's in force and where; `e` opens the file in `$EDITOR`
3. **memento mori**: every agent as a small coloured rectangle with a name, working, getting redirected, dying

## Development

```sh
cargo fmt
cargo lint           # clippy, with warnings as errors
cargo test

cd extensions/pi && npm install
npm run fix          # Biome: formats, and lints with safe fixes
npm run typecheck
npm test             # the pi extension, run against the real binary
```

CI runs the same checks and fails on any formatting difference or warning.

`crates/reaper/tests/eval.rs` measures reaper against baselines with a real agent and a local
model (see [docs/background.md](docs/background.md)). It runs for an hour or more, so run it
from a terminal, not alongside big builds:

```sh
cargo test -p reaper --test eval -- --ignored --nocapture --test-threads=1
```

## License

[MIT](LICENSE)
