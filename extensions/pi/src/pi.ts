import { spawn } from "node:child_process"
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import type { ExtensionAPI, ExtensionContext, SessionBoundaryDraft } from "@earendil-works/pi-coding-agent"

/**
 * reaper's pi extension. Decisions come from the reaper binary (`reaper engine`, which
 * owns the rules, watcher, strikes and event log); this file carries them out with pi's
 * documented extension API:
 *
 *   1. block    — `tool_call` returns `{ block: true }`, so the tool never runs
 *   2. cut      — at `turn_end`, `context_edit` entries drop the attempt from model context.
 *                 They are append-only: the session file keeps the original, labelled. Only
 *                 the tail changes, so the provider's cached prefix stays valid.
 *   3. redirect — a `custom_message` with the rule's positive instruction, and `continue: true`
 *   4. stop     — at the strike limit the run is terminated instead of redirected
 *   5. reap     — a session left idle (finished, nothing typed, no dialog open) for the time
 *                 the engine names is shut down, freeing what it holds. Its conversation is
 *                 saved, and `reaper run` offers to resume it in the same terminal (a pi
 *                 started some other way is told the command that resumes it). Under
 *                 pressure (too many sessions, memory short) the engine asks the longest idle
 *                 to go sooner, through the file it keeps in reaper's list of live sessions.
 *                 Only interactive sessions: print, JSON and RPC runs end or are driven by others.
 *
 * What the agent says is checked too, at the end of each turn, by rules that list `message`
 * among their tools: a reply that breaks one is cut and redirected the same way.
 */

const HOST = "pi"

type Kill = {
  kind: "kill"
  rule: string
  redirect: string
  strike: number
  final: boolean
  reason?: "strikes" | "stop" | "together"
}
type Decision = ({ kind: "allow" } | Kill) & { problem?: string }
type EventReply = { ok: true; reapAfterMs?: number; live?: { file: string; mark: string }; say?: string }
export type Ask = <Reply>(request: Record<string, unknown>) => Promise<Reply>

/** Filled in by `reaper install pi` in the copy it puts where pi finds extensions. */
const INSTALLED = undefined as { bin: string } | undefined

/** `reaper run` points REAPER_BIN at itself. */
const bin = () => process.env.REAPER_BIN || INSTALLED?.bin || "reaper"

export const askReaper: Ask = (request) =>
  new Promise((resolve, reject) => {
    const child = spawn(bin(), ["engine"], { stdio: ["pipe", "pipe", "pipe"] })
    let out = ""
    let err = ""
    child.stdout.on("data", (d) => (out += d))
    child.stderr.on("data", (d) => (err += d))
    child.on("error", (e) =>
      reject(
        new Error(`can't run the reaper binary (${e.message}); start pi with \`reaper run\` or \`reaper install pi\``),
      ),
    )
    child.on("close", (code) => {
      if (code !== 0) return reject(new Error(err.trim() || `reaper engine exited with ${code}`))
      try {
        resolve(JSON.parse(out))
      } catch {
        reject(new Error(`reaper engine sent: ${out.slice(0, 200)}`))
      }
    })
    child.stdin.end(JSON.stringify(request))
  })

/** A request nobody waits for: the engine carries on with it after pi has moved on, or exited. */
export function tell(request: Record<string, unknown>) {
  try {
    const child = spawn(bin(), ["engine"], {
      stdio: ["pipe", "ignore", "ignore"],
      detached: true,
    })
    child.on("error", () => {})
    child.stdin.on("error", () => {})
    child.stdin.end(JSON.stringify(request))
    child.unref()
  } catch {}
}

type Content = { type: string; id?: string; text?: string }

/**
 * Context edits for one turn: the killed calls' results are dropped, and the assistant
 * message keeps only its other tool calls (its text and thinking announced the attempt).
 * An assistant message left with nothing is dropped whole.
 */
export function cutDrafts(
  turn: {
    message: object
    messageEntryId: string
    toolResults: { toolCallId: string }[]
    toolResultEntryIds: string[]
  },
  killed: Set<string>,
): SessionBoundaryDraft[] {
  const raw = "content" in turn.message ? turn.message.content : undefined
  const content = (Array.isArray(raw) ? raw : []) as Content[]
  const kept = content.filter((c) => c.type === "toolCall" && !killed.has(c.id!))
  const drafts: SessionBoundaryDraft[] = [
    {
      type: "context_edit",
      targetId: turn.messageEntryId,
      replacement: kept.length ? { content: kept as never } : null,
    },
  ]
  turn.toolResults.forEach((r, i) => {
    if (killed.has(r.toolCallId) && turn.toolResultEntryIds[i])
      drafts.push({ type: "context_edit", targetId: turn.toolResultEntryIds[i]!, replacement: null })
  })
  return drafts
}

export function spoken(message: object): string {
  const raw = "content" in message ? message.content : undefined
  if (typeof raw === "string") return raw.trim()
  const blocks = (Array.isArray(raw) ? raw : []) as Content[]
  return blocks
    .filter((b) => b.type === "text")
    .map((b) => b.text ?? "")
    .join("\n")
    .trim()
}

const textOf = (content: unknown) =>
  typeof content === "string"
    ? content
    : Array.isArray(content)
      ? content.map((c: Content) => (c.type === "text" ? c.text : "")).join("\n")
      : ""

export function reaper(ask: Ask = askReaper) {
  return (pi: ExtensionAPI) => {
    const seen = new Set<string>()
    /** toolCallId → decision, for kills in the current turn. */
    const kills = new Map<string, Kill>()
    let warned = false

    /** How long an idle session lives, from the engine's latest answer; unset when it isn't reaped. */
    let reapAfter: number | undefined
    let reapTimer: ReturnType<typeof setTimeout> | undefined
    /** Since the agent settled, or the human last touched a key; unset while it works. */
    let idleSince: number | undefined
    /** Open dialogs: an answer is pending, so the session isn't idle. */
    let dialogs = 0
    /** Set once this session is being reaped, for session_shutdown to record. */
    let reaping: { idle: number; file: string; reason: string } | undefined
    /** This session's file in reaper's list of live sessions, and where a request to reap it appears. */
    let live: { file: string; mark: string } | undefined
    let heartbeat: ReturnType<typeof setInterval> | undefined
    let stopListening: (() => void) | undefined

    /** Problems are the human's to fix; tell them once, and let the agent carry on. */
    function warn(ctx: ExtensionContext, message: string) {
      if (!warned && ctx.hasUI) ctx.ui.notify(`reaper: ${message}`, "warning")
      warned = true
    }

    function emit(ctx: ExtensionContext, agent: string, type: string, fields: Record<string, unknown> = {}) {
      return ask<EventReply>({ op: "event", host: HOST, agent, type, ...fields }).catch((e: Error) =>
        warn(ctx, e.message),
      )
    }

    async function agent(ctx: ExtensionContext) {
      const id = ctx.sessionManager.getSessionId()
      if (!seen.has(id)) {
        seen.add(id)
        const reply = await emit(ctx, id, "agent.spawned", {
          dir: ctx.cwd,
          model: ctx.model?.id,
          label: ctx.sessionManager.getSessionName(),
          interactive: ctx.mode === "tui",
        })
        reapAfter = reply?.reapAfterMs
        live = reply?.live
      }
      return id
    }

    function goal(ctx: ExtensionContext) {
      let latest = ""
      for (const entry of ctx.sessionManager.getBranch())
        if (entry.type === "message" && entry.message.role === "user") latest = textOf(entry.message.content)
      return latest
    }

    /** Rewritten every couple of seconds too: a file left alone means the session is gone. */
    function keepLive() {
      if (!live) return
      try {
        writeFileSync(live.file, JSON.stringify({ idleSince: idleSince ?? null }))
      } catch {}
    }

    function arm(ctx: ExtensionContext) {
      clearTimeout(reapTimer)
      reapTimer = undefined
      if (!reapAfter || ctx.mode !== "tui" || idleSince === undefined) return
      reapTimer = setTimeout(() => reapIdle(ctx, "idle"), Math.max(0, idleSince + reapAfter - Date.now()))
      reapTimer.unref?.() // the countdown never keeps pi running by itself
    }

    function idle(ctx: ExtensionContext) {
      idleSince = Date.now()
      keepLive()
      arm(ctx)
    }

    function busy() {
      idleSince = undefined
      keepLive()
      clearTimeout(reapTimer)
      reapTimer = undefined
    }

    /** Checks and shuts down in one go, so nothing typed can slip in between. A session
     *  with nothing saved yet has nothing to resume, so it stays. */
    function reapIdle(ctx: ExtensionContext, reason: string) {
      reapTimer = undefined
      const file = ctx.sessionManager.getSessionFile()
      if (idleSince === undefined || !file || !existsSync(file)) return
      if (!ctx.isIdle() || ctx.hasPendingMessages() || dialogs > 0) return
      reaping = { idle: Date.now() - idleSince, file, reason }
      ctx.shutdown()
    }

    pi.on("session_start", async (_event, ctx) => {
      await agent(ctx)
      if (ctx.mode !== "tui") return
      // Typing counts as activity, before anything is sent: a half-written prompt is never lost.
      stopListening = ctx.ui.onTerminalInput(() => {
        if (idleSince !== undefined) {
          idleSince = Date.now()
          arm(ctx)
        }
        return undefined
      })
      if (live) {
        // The engine asks a session to make room by leaving a mark; it goes only if still idle.
        heartbeat = setInterval(() => {
          keepLive()
          if (!live || !existsSync(live.mark)) return
          let reason = "crowded"
          try {
            reason = readFileSync(live.mark, "utf8").trim() || reason
            rmSync(live.mark, { force: true })
          } catch {}
          reapIdle(ctx, reason)
        }, 2000)
        heartbeat.unref?.()
      }
      // A session starts out waiting on the human, a resumed one included.
      if (ctx.isIdle()) idle(ctx)
    })

    pi.on("input", async () => busy())
    // The human asked for something: checks are seconds away, so the watcher gets the rules
    // and this task now, while the agent's own model call runs.
    pi.on("before_agent_start", async (event, ctx) => {
      if (event.messageType === "user") tell({ op: "warm", host: HOST, dir: ctx.cwd, goal: event.prompt })
    })
    pi.on("agent_start", async () => busy())
    pi.on("ui_prompt_start", async () => {
      dialogs++
      busy()
    })
    pi.on("ui_prompt_end", async (_event, ctx) => {
      dialogs = Math.max(0, dialogs - 1)
      if (dialogs === 0 && ctx.isIdle()) idle(ctx)
    })

    pi.on("session_shutdown", async (event, ctx) => {
      clearTimeout(reapTimer)
      clearInterval(heartbeat)
      stopListening?.()
      if (live) for (const path of [live.file, live.mark]) rmSync(path, { force: true })
      // Quit, or left for another session. A reload carries on with this one.
      if (!reaping) {
        if (event.reason !== "reload") await emit(ctx, await agent(ctx), "agent.ended")
        return
      }
      // Recorded only once the shutdown is under way, so a reap the human beat to it never shows.
      const reply = await emit(ctx, await agent(ctx), "agent.reaped", reaping)
      // Without `reaper run` to offer the resume, the human is told how, once pi has let go of the terminal.
      const say = reply?.say
      if (say) process.once("exit", () => process.stderr.write(say))
    })

    pi.on("tool_call", async (event, ctx) => {
      const id = await agent(ctx)
      let decision: Decision
      try {
        decision = await ask<Decision>({
          op: "check",
          host: HOST,
          session: id,
          agent: id,
          dir: ctx.cwd,
          tool: event.toolName,
          args: event.input,
          goal: goal(ctx),
        })
      } catch (e) {
        warn(ctx, (e as Error).message)
        return
      }
      if (decision.problem) warn(ctx, decision.problem)
      if (decision.kind === "allow") return
      kills.set(event.toolCallId, decision)
      // The reason is never seen by the model: the result is cut at turn_end.
      return { block: true, reason: "Interrupted.", terminate: decision.final }
    })

    pi.on("turn_end", async (event, ctx) => {
      const ids = new Set(event.toolResults.map((r) => r.toolCallId).filter((c) => kills.has(c)))
      const decisions = [...ids].map((c) => kills.get(c)!)
      for (const c of ids) kills.delete(c)

      const said = spoken(event.message)
      if (said) {
        const id = await agent(ctx)
        try {
          const verdict = await ask<Decision>({
            op: "check",
            host: HOST,
            session: id,
            agent: id,
            dir: ctx.cwd,
            tool: "message",
            args: { text: said },
            goal: goal(ctx),
          })
          if (verdict.problem) warn(ctx, verdict.problem)
          if (verdict.kind === "kill") decisions.push(verdict)
        } catch (e) {
          warn(ctx, (e as Error).message)
        }
      }
      if (decisions.length === 0) return

      const id = await agent(ctx)
      const entries = [...event.entries, ...cutDrafts(event, ids)]
      const final = decisions.find((d) => d.final)
      const rule = (final ?? decisions[0]!).rule
      // Marks the dead attempt in the session tree; labels never reach the model.
      pi.setLabel(event.messageEntryId, `reaper: ${rule}`)
      if (final) {
        await emit(ctx, id, "agent.died", { rule, reason: final.reason ?? "strikes" })
        if (ctx.hasUI) ctx.ui.notify(`reaper stopped this session (rule ${rule}).`, "error")
        return { entries }
      }
      const redirect = [...new Set(decisions.map((d) => d.redirect))].join("\n")
      await emit(ctx, id, "agent.redirected", { rule, redirect })
      return {
        entries: [
          ...entries,
          { type: "custom_message", customType: "reaper.redirect", content: redirect, display: true },
        ],
        continue: true,
      }
    })

    pi.on("agent_settled", async (_event, ctx) => {
      if (ctx.isIdle() && !ctx.hasPendingMessages()) {
        idleSince = Date.now()
        keepLive() // before the engine hears of it, so it can count this session as a candidate
      }
      const reply = await emit(ctx, await agent(ctx), "agent.idle")
      reapAfter = reply?.reapAfterMs
      if (idleSince !== undefined) arm(ctx)
    })
  }
}

/** pi loads extensions from a default-exported factory. Under `reaper run`, which loads its own
 *  copy, an installed one stands aside. */
export default INSTALLED && process.env.REAPER_RUN ? () => {} : reaper()
