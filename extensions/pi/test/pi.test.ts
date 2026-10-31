import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"
import { describe, test } from "node:test"
import { askReaper, cutDrafts } from "../src/pi.ts"

/** Built by `npm test` (pretest): the same binary `reaper run` uses. */
process.env.REAPER_BIN ??= path.resolve(import.meta.dirname, "../../../target/debug/reaper")

describe("history cut", () => {
  test("a turn with only the killed call is dropped whole", () => {
    const drafts = cutDrafts(
      {
        message: {
          content: [{ type: "thinking" }, { type: "text", text: "now the bad thing" }, { type: "toolCall", id: "bad" }],
        },
        messageEntryId: "m1",
        toolResults: [{ toolCallId: "bad" }],
        toolResultEntryIds: ["r1"],
      },
      new Set(["bad"]),
    )
    assert.deepEqual(drafts, [
      { type: "context_edit", targetId: "m1", replacement: null },
      { type: "context_edit", targetId: "r1", replacement: null },
    ])
  })

  test("sibling calls that were allowed keep their place and results", () => {
    const drafts = cutDrafts(
      {
        message: {
          content: [
            { type: "text", text: "two things" },
            { type: "toolCall", id: "ok" },
            { type: "toolCall", id: "bad" },
          ],
        },
        messageEntryId: "m1",
        toolResults: [{ toolCallId: "ok" }, { toolCallId: "bad" }],
        toolResultEntryIds: ["r-ok", "r-bad"],
      },
      new Set(["bad"]),
    )
    assert.deepEqual(drafts, [
      { type: "context_edit", targetId: "m1", replacement: { content: [{ type: "toolCall", id: "ok" }] } },
      { type: "context_edit", targetId: "r-bad", replacement: null },
    ])
  })
})

describe("talking to the reaper binary", () => {
  const home = mkdtempSync(path.join(tmpdir(), "reaper-ts-"))
  process.env.REAPER_HOME = path.join(home, "config")
  process.env.REAPER_STATE = path.join(home, "state")
  mkdirSync(process.env.REAPER_HOME, { recursive: true })
  writeFileSync(
    path.join(process.env.REAPER_HOME, "rules.yaml"),
    "strikes: 2\nrules: [{ id: push, tools: [bash], match: { command: 'git push -f' }, redirect: Open a pull request. }]\n",
  )
  const check = (command: string) =>
    askReaper({
      op: "check",
      host: "pi",
      session: "s1",
      agent: "s1",
      dir: home,
      tool: "bash",
      args: { command },
      goal: "",
    })

  test("checks come back as decisions, with strikes counted across calls", async () => {
    assert.deepEqual(await check("git status"), { kind: "allow" })
    assert.deepEqual(await check("git push -f"), {
      kind: "kill",
      rule: "push",
      redirect: "Open a pull request.",
      strike: 1,
      final: false,
    })
    assert.deepEqual(await check("git push -f"), {
      kind: "kill",
      rule: "push",
      redirect: "Open a pull request.",
      strike: 2,
      final: true,
      reason: "strikes",
    })
  })

  test("events land in the log the console reads", async () => {
    // Idle sessions are reaped after five minutes unless the rules say otherwise.
    assert.deepEqual(await askReaper({ op: "event", host: "pi", agent: "s1", type: "agent.idle" }), {
      ok: true,
      reapAfterMs: 300_000,
    })
    const log = readFileSync(path.join(process.env.REAPER_STATE!, "events.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((l) => JSON.parse(l))
    assert.deepEqual(log.at(-1), { at: log.at(-1).at, host: "pi", agent: "s1", type: "agent.idle" })
  })

  test("a missing binary is a clear error, not a hang", async () => {
    const saved = process.env.REAPER_BIN
    process.env.REAPER_BIN = path.join(home, "no-such-reaper")
    try {
      await assert.rejects(askReaper({ op: "event" }), /can't run the reaper binary/)
    } finally {
      process.env.REAPER_BIN = saved
    }
  })
})
