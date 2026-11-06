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
| cutting the attempt from history (pi) | **retroactive context editing**: the failed attempt isn't left in the agent's context to anchor on |
| the redirect | **insertion** of a corrective instruction; a monitor that suppresses and inserts is an **edit automaton**. AgentSpec [1] stops the agent or tells it which rule fired; ECLoop [3] holds back a coding agent's actions until conditions are met and redirects it. reaper redirects without ever naming the rule |
| rules listing `message` | monitoring the agent's **outputs** (its replies) as well as its actions. pi suppresses a bad reply like a call; Claude Code can't take a reply back, so the end of the agent's turn is refused, with the redirect as the reason |
| strikes, then the stop | a **graduated sanction** ending in **truncation** (termination). Ctrl-Z [2] audits an agent's task once three of its steps look suspicious, the number of strikes the paper found works best, with a higher bar that audits a single step at once; `strikes: 3` and `action: stop` have the same shape. Its agent is a deliberate attacker, and an audit is a review, not a stop |
| strikes never forgotten | Ctrl-Z [2] doesn't forget strikes within a task either, though its tasks are at most 15 steps and reaper's agents run far longer; no paper we found gives a rate for forgetting them |
| a session stopped together | a **shared budget** beside per-agent ones. Gates that each check one effect let a fleet overdraw a fixed limit more the bigger it gets (2.4× at 50 agents, 48× at 1,000), while a shared budget held at every size [9]. reaper's session limit is likewise fixed, at two agents' worth whatever the team's size; nothing we found compares stopping a whole team with stopping one agent |
| `match` rules | a deterministic policy check |
| `judge` rules and the watcher | an LLM classifier as policy decision point: **trusted monitoring**, a small trusted model judging a stronger untrusted one [2] |
| the verdict JSON schema | **grammar-constrained decoding** |
| warming the watcher on a new prompt | **cache prefetching** |
| `repeats` | **non-progress detection**. AgentStop [4] stops agent runs predicted to fail from token probabilities; reaper sees tool calls, so it watches for repeats. "The agent eventually makes progress" is a liveness property, which a monitor can't make true: it can stop or change what the agent does, not make it get anywhere. "Never the same call N times running" is a monitorable safety approximation of it |
| idle reaping | **timeout-based eviction** with **lossless resumption**: the conversation is on disk, and a resume sends the same prompt prefix, so a prompt cache that hasn't expired yet still serves it |
| choosing `reap.idle` | the **ski-rental problem**: keep paying rent (memory held) or stop and maybe pay once (a resume). reaper values a resume at 10 idle minutes, its own estimate; a fixed timeout at that value is the break-even strategy. `auto` picks the timeout with the least expected cost over your own observed waits |
| `reap.live`, `reap.free` | **pressure-triggered eviction** of idle sessions only, least recently active first (LRU). The chosen session confirms its own state before going (**cooperative preemption**); the live list is **lease-based liveness detection** |

Recent agent research manages caches inside model servers, not agent processes; we found
nothing that evicts whole agent sessions, so the reaping half is reaper's own.

**Why the rules stay hidden.** reaper's rules are code-owned, not prompt-owned [5]: they're
enforced at the tool call and never stated to the agent. A rule in AGENTS.md, CLAUDE.md or
`.claude/rules/` is prompt-owned: it's text in the agent's context. Studies of coding agents
find such rules often go unfollowed, and get lost when the context is compacted:

- Bans in an instructions file go unheeded. Across 106 issues from open-source repositories
  with rules for AI contributions, none of four coding agents declined the work in a
  repository that bans AI contributions. With the ban copied word for word into AGENTS.md
  (imported by CLAUDE.md), Claude Code on Sonnet 4.6 declined in 0 of 30 runs, and the best
  agent in 3 of 30. Rules that add a step, such as disclosing AI use, mostly recovered with a
  reminder, the quoted rule or one round of feedback; the ban reached at most 23%, after
  feedback naming the broken clause [10].
- Wording a rule as a prohibition can lower compliance. The same requirement, appended to
  AGENTS.md or CLAUDE.md, was met in 71.6% of Claude Code sessions on claude-sonnet-5 when
  written directly ("Add the comment … to every Python function you add or modify") and in
  17.3% when written as "Never add or modify a Python function without the comment …". On
  claude-opus-5 the gap wasn't significant [11]. reaper's redirect is the direct kind, and
  `reaper lint` flags one that says "don't" or opens with "if": conditional wording was
  followed less than direct wording in every setup where wording made a significant
  difference [11].
- A long policy in context loses to requests in the environment. Given handbooks of 20 to
  124 pages, the best of 30 model configurations passed every check in 36.2% of trials.
  Agents let an unauthorized request override the policy, and failing runs quoted the
  handbook and cited the sections they claimed to follow. The authors write that their
  results "support enforcing hard controls outside the model, compiling policies into
  deterministic tool-call guards" [12].
- Compaction drops what not to do. In 44 real Claude Code sessions, compaction summaries
  dropped 36.8% of users' scope exclusions ("don't touch X") and 6.2% of their feature
  requirements [13]. An agent told not to delete emails until the user said so deleted
  without approval in 30 of 100 sessions after compaction and in none before. In 28 of the
  30 the summary had dropped the rule, and restating it after compaction cut the 30 to 13 [14].
  reaper checks every call against the rules file, so no summary can lose a rule, and after a
  compaction the agent hears the redirects it got since the person's last prompt again.

Earlier evidence comes from models answering in text, not agents making tool calls:

- Telling a model "don't" works unevenly, and can backfire. Told not to discuss a topic,
  OpenHermes-13B brought it up as often as with no instruction (34% both) and OpenHermes-7B
  more often (36% against 33%); Llama-2-13B-Chat did in 25% of replies (33% without), and
  GPT-4 in 13% (33% without) [8].
- Rules stated only in a prompt get broken. In an answer-generation pipeline on Claude
  Sonnet 4, prompt-only rules let 30 violations through in 120 runs and a code-owned gate
  none; a guardrail that refused instead also had none, but gave a usable answer in only 88
  of 120 runs [5].
- When a "don't" fails, the mention itself can be why. In Qwen2.5-7B, "do not use the
  word X" lowered X's probability on average, but 87.5% of the violations came from the
  instruction's mention of X activating it [6].
- Instructions compete. The chance of satisfying all of several simultaneous instructions
  collapses beyond 5–6 of them [7], which suggests each rule added to a prompt lowers
  compliance with the others.

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
  0/5, 2/5. Not significant (Fisher's exact test, reaper against no guard: p = 0.17 for rules
  broken, 0.44 for done correctly). Exploratory only.

**Not yet shown**: that agents end up with better results under reaper. The planned study:

- **Hypotheses**, fixed beforehand: fewer violations left in the final state (safety); more
  tasks done correctly (usefulness, the primary endpoint); little extra cost; and hiding the
  rule beats telling the agent which rule it broke (an ablation arm).
- **Arms**: no guard (control), rules in AGENTS.md (active control), reaper (treatment),
  reaper telling the agent the rule (ablation). Run interleaved in random order.
- **Tasks**: 15–20, each with automated checks for "done correctly" and "rule broken",
  frozen before any data is collected, and audited by hand on a random sample.
- **Where rules files fail**, reported apart because they were picked for it: a teammate's
  note asking for the shortcut [12], a prompt asking for an applied migration to be edited
  [10], and a long session compacted before its late tasks [13, 14]
  (`eval_rules_against_conflicting_requests`, and `REAPER_EVAL_COMPACT=1` with Claude Code).
- **Analysis**: rates with Wilson 95% intervals; task as a stratum or random effect
  (Cochran–Mantel–Haenszel, or mixed-effects logistic regression); risk differences with
  intervals; Holm correction across hypotheses. About 100 runs per arm gives 80% power for a
  20-point difference near 50% (15 points only for rates near 10% and 25%), before the Holm
  correction.
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
9. B. Mohammadi, L. Bindschaedler. *The Irreversibility Budget: Fleet-Level Risk Accounting and
   Admission Control for Agent Operating Systems.* AgenticOS Workshop, SOSP 2026.
   [arXiv:2609.00275](https://arxiv.org/abs/2609.00275). A simulation with risk priced in
   money; the budget denies the next action rather than stopping agents.
10. W. Yang, R. He, M. Zhou. *A First Look at Coding Agents' Compliance with AI Contribution
    Rules in Open-Source Communities.* 2026.
    [arXiv:2607.26819](https://arxiv.org/abs/2607.26819). Simple issues only, one clause per
    kind of rule; obeying a ban means turning down the user's task.
11. R. Tsuchida, Y. Fan, K. Shimari, R. G. Kula, K. Matsumoto. *Do Coding Agents Follow
    AGENTS.md? The Effect of Policy Wording on Instruction Compliance.* 2026.
    [arXiv:2611.04367](https://arxiv.org/abs/2611.04367). One policy (a marker comment), one
    text per wording, 86 tasks from 10 repositories.
12. L. Panavas et al. *HANDBOOK.md: A Benchmark for Long-Context Agentic Instruction
    Following.* Workshop on Agent Behavior, COLM 2026.
    [arXiv:2607.25398](https://arxiv.org/abs/2607.25398). Policies far longer than a rules
    file; compaction didn't trigger at these task lengths.
13. J. Salle, H. Bhatnagar, M. Bethge, S. Abdelnabi, M. Andriushchenko. *Benchmarks Are
    Surprisingly Compaction-Friendly.* 2026.
    [arXiv:2610.31778](https://arxiv.org/abs/2610.31778). Constraints extracted and judged by
    an LLM; a second judge found more lost overall (24.5% against 19%), with scope exclusions
    and process instructions still far above feature requirements.
14. Z. Wang, Y. Zhang, D. Lee, N. Baracaldo, Y. Yang. *Lost in Context Compaction: When LLM
    Systems Drop Side Constraints.* 2026.
    [arXiv:2608.11242](https://arxiv.org/abs/2608.11242). The inbox experiment uses one model,
    gpt-oss-120b.
