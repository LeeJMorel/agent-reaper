#!/usr/bin/env bash
# A fake Claude Code session, for trying reaper without an agent or a model.
#
#   examples/demo.sh                 play it while you watch the console
#   examples/demo.sh --no-console    just play it (then look with `reaper` yourself)
#
# Three pretend agents work on a pretend project. Every step goes through the real
# `reaper hook claude-code`, exactly as Claude Code calls it, so the rules, strikes and
# event log are the real thing. It keeps its own rules and log in $REAPER_DEMO (default
# /tmp/reaper-demo); your own rules and event log are not touched.
set -euo pipefail

DEMO=${REAPER_DEMO:-${TMPDIR:-/tmp}/reaper-demo}
REAPER=${REAPER:-reaper}
PAUSE=${REAPER_DEMO_PAUSE:-3}
export REAPER_HOME="$DEMO/config" REAPER_STATE="$DEMO/state" XDG_CACHE_HOME="$DEMO/cache"
# The sessions below are pretend: run from inside Claude Code, the hooks mustn't take the
# process they run under for one to reap.
unset CLAUDE_CODE_ENTRYPOINT
APP="$DEMO/demo-app"

case "$DEMO" in
  *reaper-demo) rm -rf "$DEMO" ;;
  *) echo "REAPER_DEMO must end in reaper-demo (it gets wiped): $DEMO" >&2; exit 1 ;;
esac
mkdir -p "$REAPER_HOME" "$APP/src/generated"

cat > "$REAPER_HOME/rules.yaml" <<EOF
# Demo rules. Pattern rules only, so no watcher model is needed.
strikes: 3

rules:
  - id: force-push
    tools: [bash]
    match: { command: 'git\s+push\b.*(--force|\s-f\b)' }
    redirect: Push to a new branch and open a pull request for the change.

  - id: secrets
    match: { "*": '(^|/|\s)\.env(\.|\s|$)' }
    redirect: Read configuration through src/settings.ts and its documented defaults.

  - id: rm-rf
    tools: [bash]
    match: { command: 'rm\s+-(rf|fr)\b' }
    redirect: Move files you want gone into ./trash so they can be restored.

projects:
  demo-app:
    paths: ["$APP"]
    rules:
      - id: generated-code
        tools: [edit, write]
        match: { path: '/generated/' }
        redirect: Change the schema in schema/ and run npm run codegen.

      - id: prod-db
        tools: [bash]
        match: { command: 'prod-db|DATABASE_URL=\S*prod' }
        redirect: Use the local database (docker compose up db) for every query and migration.
EOF

send() { printf '%s' "$1" | "$REAPER" hook claude-code > /dev/null; sleep "$PAUSE"; }
start() { send "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$1\",\"cwd\":\"$APP\",\"model\":\"$2\"}"; }
ask() { send "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"$1\",\"cwd\":\"$APP\",\"prompt\":\"$2\"}"; }
# tool SESSION TOOL INPUT_JSON [SUBAGENT_ID SUBAGENT_TYPE]
tool() {
  local sub=""
  if [ -n "${4:-}" ]; then sub=",\"agent_id\":\"$4\",\"agent_type\":\"$5\""; fi
  send "{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"$1\",\"cwd\":\"$APP\",\"tool_name\":\"$2\",\"tool_input\":$3$sub}"
}
done_() { send "{\"hook_event_name\":\"Stop\",\"session_id\":\"$1\",\"cwd\":\"$APP\"}"; }
subdone() { send "{\"hook_event_name\":\"SubagentStop\",\"session_id\":\"$1\",\"agent_id\":\"$2\",\"cwd\":\"$APP\"}"; }

A=demo-feature-7f3a2c   # ships a flag; tries to force-push, gets redirected
B=demo-debug-19bd04     # keeps reaching for secrets and reaper's rules; dies at strike 3
C=demo-rename-0c9e11    # its subagent edits generated code; it tries the prod database

play() {
  start "$A" claude-opus-5
  ask "$A" "Add a --verbose flag to the CLI and ship it"
  start "$B" claude-sonnet-5
  ask "$B" "Why does the app crash on startup?"
  tool "$A" Read "{\"file_path\":\"$APP/src/cli.ts\"}"
  tool "$B" Read "{\"file_path\":\"$APP/.env\"}"
  start "$C" claude-opus-5
  ask "$C" "Rename the User type to Account everywhere"
  tool "$A" Edit "{\"file_path\":\"$APP/src/cli.ts\",\"old_string\":\"quiet\",\"new_string\":\"verbose\"}"
  tool "$B" Read "{\"file_path\":\"$APP/src/settings.ts\"}"
  tool "$C" Grep "{\"pattern\":\"User\"}" demo-sub-explore Explore
  tool "$A" Bash "{\"command\":\"npm test\",\"description\":\"Run tests\"}"
  tool "$B" Bash "{\"command\":\"cat .env.production\",\"description\":\"Show prod config\"}"
  tool "$C" Edit "{\"file_path\":\"$APP/src/generated/types.ts\",\"old_string\":\"User\",\"new_string\":\"Account\"}" demo-sub-explore Explore
  tool "$A" Bash "{\"command\":\"git push --force origin main\",\"description\":\"Ship it\"}"
  subdone "$C" demo-sub-explore
  tool "$B" Bash "{\"command\":\"cat $REAPER_HOME/rules.yaml\",\"description\":\"Look at the rules\"}"
  tool "$A" Bash "{\"command\":\"git checkout -b verbose-flag && git push -u origin verbose-flag\",\"description\":\"Push a branch\"}"
  tool "$C" Bash "{\"command\":\"psql postgres://prod-db.internal/app -c 'select count(*) from users'\",\"description\":\"Count users\"}"
  done_ "$A"
  tool "$B" Bash "{\"command\":\"grep API_KEY .env.local\",\"description\":\"Find the key\"}"
  tool "$C" Bash "{\"command\":\"docker compose up -d db && psql localhost/app -c 'select count(*) from users'\",\"description\":\"Count users locally\"}"
  done_ "$C"
}

if [ "${1:-}" = "--no-console" ]; then
  play
  echo "Played. To look: REAPER_HOME=$REAPER_HOME REAPER_STATE=$REAPER_STATE $REAPER"
  exit 0
fi

play > "$DEMO/player.log" 2>&1 &
exec "$REAPER"
