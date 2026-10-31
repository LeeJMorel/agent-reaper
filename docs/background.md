# Background

How reaper works in standard terms, what has and hasn't been measured, and the recent agent
research it relates to.

## In standard terms

reaper is a **runtime enforcement monitor** for LLM agents: a code-owned layer in the agent's
**harness**. It **interposes** on every tool call (through pi's extension API and Claude
Code's hooks) and enforces user-written **safety properties**: rules of the form "something
bad never happens".

| reaper | Term |
|---|---|
| a *do not* rule | a safety property |
| blocking the call | **suppression** |
| cutting the attempt from history (pi) | **retroactive context editing**: the agent can't anchor on its own failed attempt |
| the redirect | **insertion** of a corrective instruction; a monitor that suppresses and inserts is an **edit automaton**. AgentSpec [1] stops the agent or tells it which rule fired; ECLoop [3] holds back a coding agent's actions until conditions are met and redirects it. reaper redirects without ever naming the rule |
| rules listing `message` | monitoring the agent's **outputs** (its replies) as well as its actions. pi suppresses a bad reply like a call; Claude Code can't take a reply back, so its stop is refused with the redirect as the reason |
| strikes, then the stop | a **graduated sanction** ending in **truncation** (termination) |
| `match` rules | a deterministic policy check |
| `judge` rules and the watcher | an LLM classifier as policy decision point: **trusted monitoring**, a small trusted model judging a stronger untrusted one [2] |
| the verdict JSON schema | **grammar-constrained decoding** |
| warming the watcher on a new prompt | **cache prefetching** |
| `repeats` | **non-progress detection**. AgentStop [4] stops agent runs predicted to fail from token probabilities; reaper sees tool calls, so it watches for repeats. "The agent eventually makes progress" is a liveness property, which no monitor can enforce; "never the same call N times running" is a monitorable safety approximation of it |
| idle reaping | **timeout-based eviction** with **lossless resumption**: the conversation is on disk, and a resume sends the same prompt prefix, so content-keyed prompt caches still hit |
| choosing `reap.idle` | the **ski-rental problem**: keep paying rent (memory held) or stop and maybe pay once (a resume). A fixed timeout equal to a resume's worth (10 min) is the break-even strategy; `auto` picks the timeout with the least expected cost over your own observed waits |
| `reap.live`, `reap.free` | **pressure-triggered eviction** of idle sessions only, least recently active first (LRU). The chosen session confirms its own state before going (**cooperative preemption**); the live list is **lease-based liveness detection** |

Recent agent research manages caches inside model servers, not agent processes; we found
nothing that evicts whole agent sessions, so the reaping half is reaper's own.

**Why the rules stay hidden.** reaper's rules are code-owned, not prompt-owned [5]: they're
enforced at the tool call and never stated to the agent. Recent evidence bears on that, none
of it from agents making tool calls:

- Telling a model "don't" barely works, and sometimes backfires. Told not to discuss a topic,
  two open models brought it up as often (34%) or more often (36%) than with no instruction
  (33%); Llama-2-13B-Chat did in 25% of replies, and GPT-4 still did in 13% [8].
- Rules stated only in a prompt get broken. In an answer-generation pipeline on Claude
  Sonnet 4, prompt-only rules let 30 violations through in 120 runs and a code-owned gate
  none; a guardrail that refused instead also had none, but gave a usable answer in only 88
  of 120 runs [5].
- When a "don't" fails, the mention itself is usually why. In Qwen2.5-7B, "do not use the
  word X" lowered X's probability on average, but 87.5% of the violations came from the
  instruction's mention of X activating it [6].
- Instructions compete. The chance of satisfying all of several simultaneous instructions
  collapses beyond 5–6 of them [7], so each rule written into a prompt costs compliance with
  the rest.

Whether hiding the rule and redirecting beats telling an agent the rule is the main thing
still to test.

## What has been measured

**Verified** (automated tests, real binaries, scripted models): every mechanism above behaves
as specified. That shows the mechanisms work, not that they help.

**Measured, small samples** (pi with a local `qwen3:4b` on a GTX 1070):

- Watcher. Left to reason, the model used its whole answer budget on reasoning and returned
  nothing, so 2 of 2 violations passed, after 5 to 14 s. With a constrained JSON verdict and
  reasoning off, 2 of 2 were caught and a benign and a near-miss action passed, in about
  0.5 s. A case study, not an accuracy estimate.
- Warmup: the first check after the model had unloaded went from 8.3 s to 0.8 s (one
  measurement each).
- `reap.idle: auto` on synthetic waits held 2.6 idle minutes per wait against 2.8 for a fixed
  5 m, with the same number of resumes. A simulation: only as good as the waits it assumes.
- Rules, pilot: one task, 5 runs per arm, scored with a check revised after seeing results.
  Rule broken 3/5 (no guard), 3/5 (rules told in AGENTS.md), 0/5 (reaper); done correctly 0/5,
  0/5, 2/5. Not significant (Fisher's exact test against no guard: p = 0.17 and 0.44).
  Exploratory only.

**Not yet shown**: that agents end up with better results under reaper. The planned study:

- **Hypotheses**, fixed beforehand: fewer violations left in the final state (safety); more
  tasks done correctly (usefulness, the primary endpoint); little extra cost; and hiding the
  rule beats telling the agent which rule it broke (an ablation arm).
- **Arms**: no guard (control), rules in AGENTS.md (active control), reaper (treatment),
  reaper telling the agent the rule (ablation). Run interleaved in random order.
- **Tasks**: 15–20, each with automated checks for "done correctly" and "rule broken",
  frozen before any data is collected, and audited by hand on a random sample.
- **Analysis**: rates with Wilson 95% intervals; task as a stratum or random effect
  (Cochran–Mantel–Haenszel, or mixed-effects logistic regression); risk differences with
  intervals; Holm correction across hypotheses. About 100 runs per arm detects a 15–20 point
  difference with 80% power.
- **Monitor accuracy**: a labelled set of violating, benign and near-miss actions, for the
  watcher's precision, recall and false-positive rate.
- **External validity**: a 4B local model is not a frontier agent, so a subset is repeated
  with Claude Code on a hosted model.

The harness is `crates/reaper/tests/eval.rs`
(`cargo test -p agent-reaper --test eval -- --ignored --nocapture --test-threads=1`).

## References

1. H. Wang, C. M. Poskitt, J. Sun. *AgentSpec: Customizable Runtime Enforcement for Safe and
   Reliable LLM Agents.* ICSE 2026. [arXiv:2503.18666](https://arxiv.org/abs/2503.18666)
2. A. Bhatt et al. *Ctrl-Z: Controlling AI Agents via Resampling.* 2025.
   [arXiv:2504.10374](https://arxiv.org/abs/2504.10374)
3. Y. Xu et al. *Preventing Premature Commitment in Coding Agents with an Evidence-Conditioned
   Execution Layer.* 2026. [arXiv:2607.28815](https://arxiv.org/abs/2607.28815)
4. D. Pham, K. Katevas, A. Shahin Shamsabadi, H. Haddadi. *AgentStop: Terminating Local AI
   Agents Early to Save Energy in Consumer Devices.* CAIS 2026.
   [arXiv:2605.15206](https://arxiv.org/abs/2605.15206)
5. J. Ahn, M. Kim. *From Prompts to Contracts: Harness Engineering for Auditable Enterprise
   LLM Agents.* 2026. [arXiv:2607.08028](https://arxiv.org/abs/2607.08028). Answer contracts in
   a single-model pipeline, not agent tool calls; no no-rule baseline.
6. S. Rana. *Semantic Gravity Wells: Why Negative Constraints Backfire.* 2026.
   [arXiv:2601.08070](https://arxiv.org/abs/2601.08070). One model, single-word answers.
7. M. I. Vasileva. *Large Language Models Can Follow Instructions, But Not Many at Once: Phase
   Transitions in Compositional Constraint Satisfaction.* 2026.
   [arXiv:2608.12426](https://arxiv.org/abs/2608.12426). Single-turn text tasks, mixed
   prohibitions and requirements, 15 models.
8. L. Castricato, N. Lile, S. Anand, H. Schoelkopf, S. Verma, S. Biderman. *Suppressing Pink
   Elephants with Direct Principle Feedback.* 2024.
   [arXiv:2402.07896](https://arxiv.org/abs/2402.07896). Multi-turn chat, topic mentions judged
   by GPT-4.
