<p align="center">
  <img src="assets/reaper.svg" width="128" alt="A pixel-art grim reaper with a scythe, in front of a full moon">
</p>

<h1 align="center">reaper</h1>

<p align="center">Rules for coding agents that the agents never see.</p>

<p align="center">
  <a href="https://github.com/LeeJMorel/agent-reaper/actions/workflows/ci.yml"><img src="https://github.com/LeeJMorel/agent-reaper/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license"></a>
  <img src="https://img.shields.io/badge/rust-1.89%2B-orange" alt="Rust 1.89+">
  <img src="https://img.shields.io/badge/agents-pi%20%C2%B7%20Claude%20Code-6e5494" alt="Works with pi and Claude Code">
</p>

---

**Kill your agents, mercilessly and with gusto.**
It's not murder if you can resume them.

AI coding agents like Claude Code and pi will sometimes do things you'd never let a person
do, like force-pushing over your work or running a migration against the production
database. reaper checks every command an agent runs and every file it touches against a list
of rules you wrote. When the agent tries something on the list, reaper stops it and gives it
a better instruction instead ("use the local database"), and the agent keeps working without
ever seeing the rule. An agent that breaks the same rule three times is killed. Agents you
start through reaper are put down after five idle minutes to free up memory, and pick up
exactly where they left off when you come back.

You can watch all of this happen. The console's **memento mori** tab draws each agent as a
small coloured rectangle with a name and a personality, and a narrator follows them as they
work and get stopped. When an agent is killed, it dies on screen. How it dies is left for
you to find out. If nothing is dying, press `d` and some stand-ins will do it for you.

> Otto arrived. Nobody had asked Otto whether they wanted to arrive. Nobody ever asked.
>
> Otto went the other way, grumbling. It was a perfectly good way. That wasn't the point.
>
> Nobody had needed Otto for ages. So Otto lay down. Let them come looking.

## What it does

A rule describes a violation privately, either as a pattern or in plain English for a small
watcher model to judge, and says what to do instead. That redirect is the only thing the
agent ever hears, so it's written as an instruction: not "don't touch production" but "use
the local database". In pi, the blocked attempt is also wiped from the agent's memory. An
agent that keeps repeating the same step is treated as breaking a rule too, so a stuck one
gets nudged and then killed.

Why hide the rules? Because telling a model "don't" works less well than you'd hope. In one
study, models told not to bring up a topic still did: two open models as often as when told
nothing, one of them more often, and even GPT-4 in one reply out of eight. Naming the thing
can put it on the model's mind. [docs/background.md](docs/background.md) has the evidence,
what reaper has measured so far, and the terms researchers use for all this (in short, a
runtime enforcement monitor).

## Install

```sh
curl -fsSL https://github.com/LeeJMorel/agent-reaper/releases/latest/download/install.sh | sh
```

That's the whole install, on Linux and macOS: a single `reaper` binary with the pi extension
built in. Run it again to update. To build it yourself (Rust 1.89+),
`cargo install --locked --git https://github.com/LeeJMorel/agent-reaper agent-reaper`.

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

Then open the console:

```sh
reaper
```

It guards the Claude Code and pi on this machine, however you start them, and shows them
as they work. Problems in the rules file show up at the top.

## Rules

Each rule describes a violation privately and says what to do instead.

| Key | |
|---|---|
| `match` | Regexes over the call's arguments: `command`, `path` (any file read or written), or `"*"` for all of them. Deterministic and instant. |
| `judge` | A plain-English description, decided by the watcher model. |
| `redirect` | The only thing the agent ever hears. Write it as an instruction, not a prohibition; `reaper lint` flags redirects that say "don't". |
| `tools` | Only check these tools. `message` means what the agent says: only rules that list it look at replies, judged by their `text`. |
| `action: stop` | Terminate on the first violation instead of redirecting. |
| `strikes` | How many times an agent may break this rule, if not the global `strikes`. |

Global rules apply everywhere; project rules apply under the paths you list (`*` matches one
level, `**` any depth). Agents are always kept out of reaper's own files. An agent that
breaks a rule `strikes` times (3) is stopped; a rule can set its own `strikes`, and breaking
several adds up. Strikes are never forgotten, and a session whose agents keep breaking rules
between them is stopped together, until your next prompt.

**The watcher** is a small model that judges `judge` rules: a trusted monitor watching a
larger agent. It can be any OpenAI-compatible endpoint (Ollama, llama.cpp, vLLM, OpenRouter…)
or Anthropic, with others under `fallback` to ask when it doesn't answer. Its prompt runs
from most to least stable (rules → task → action), so checks reuse the model's prefix
cache; on OpenAI-compatible endpoints its answer is constrained to a JSON verdict over your
rule ids, with reasoning off; and when you submit a prompt it is sent your rules and the new
task ahead of the first check, so a model unloaded while you were away is ready in time.

**Going in circles.** The same call that does something (a command, an edit, a fetch) made
`repeats` times in a row (5), with only reads and searches in between, counts as a violation:
it's suppressed, the agent is told to try a different approach, and it counts toward strikes.
`repeats: 0` turns this off.

See [examples/rules.yaml](examples/rules.yaml) for the full format.

## Agents

|  | suppress | remove from context | redirect | terminate |
|---|:-:|:-:|:-:|:-:|
| [pi](https://github.com/earendil-works/pi) | ✓ | ✓ | ✓ | ✓ |
| Claude Code | ✓ |  | ✓ | ✓ |

Claude Code's hooks can't edit the conversation, so a blocked attempt stays in its history;
the agent still only hears the redirect, never the rule. Replies are checked differently too:
pi checks what the agent wrote at the end of every turn and cuts a bad reply, while Claude Code
checks its final reply when it stops and, if it breaks a rule, keeps it working with the
redirect. Without pi installed, `reaper run` starts a guarded one, after downloading a pinned
release and checking it against a built-in checksum.

## Idle sessions

Most agents spend most of their lives waiting on you, holding memory the whole time. reaper
evicts a session in a terminal once it has waited 5 minutes, pi and Claude Code alike. It
doesn't wait for memory to run short. Being wrong is cheap: the conversation is on disk, the
agent's own resume picks it up (`reaper run` offers it in place), and the resumed session
sends the same prompt prefix, so the provider's prompt cache still applies.

A session is idle only when its turn is over, nothing has been typed, no dialog is open and
nothing is queued. A half-written prompt resets the countdown. Claude Code fires no hook
while idle, so its hook leaves a small watcher beside each terminal session, which won't
evict it while a shell is still running under it (a dev server, a build). Sessions in an
editor or run with `-p` are left alone. The console shows evicted sessions as `◌ reaped`.

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
3. **memento mori**: every agent as a small coloured rectangle with a name, working, getting redirected, dying; `d` plays a death on demand

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

CI runs the same checks on Linux (x86_64 and arm64) and macOS, fails on any formatting
difference or warning, builds with the oldest supported Rust, and plays the demo against a
release build.

`crates/reaper/tests/eval.rs` measures reaper against baselines with a real agent and a local
model (see [docs/background.md](docs/background.md)). It runs for an hour or more, so run it
from a terminal, not alongside big builds:

```sh
cargo test -p agent-reaper --test eval -- --ignored --nocapture --test-threads=1
```

## License

[MIT](LICENSE)
