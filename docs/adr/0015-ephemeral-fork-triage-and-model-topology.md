# Ephemeral fork triage for non-owner wakes; the resident/fork/advisor model topology

Agreed 2026-08-20. Amends the wake conventions built on [ADR-0005]; keeps its
principle. Realizes the cost/attention structure `docs/product-direction.md`
asks for ("routes exactly the decisions that need him, well-framed, and doing
the rest autonomously") at the model-topology level.

## Context

Every wake today — Sub-agent terminal event, monitor fire, timer — enters the
one Agent session as a full turn. That forces a bad choice: run the main model
expensive and pay top-tier tokens for "worker still running, nothing to do"
turns while plumbing silts up the durable transcript, or run it cheap and let
the model that frames judgments for the Owner be a flash-class one. Wake volume
dominates turn *count*; Owner conversation dominates turn *value*. One session,
one model cannot price both correctly.

Separately: the Owner runs a Claude subscription and a ChatGPT subscription.
Anthropic's current terms prohibit routing subscription OAuth through a
third-party harness (enforced against opencode, 2026-01), so the lash main
agent cannot legitimately ride the Claude plan; headless `claude` as a
self-contained work unit can. The Codex provider already rides the ChatGPT
subscription natively through lash.

## Decision

**Two lash-resident agents, ever: the main Agent and the fork.**

- **Main Agent** — persistent, the one interlocutor, globally aware, durable
  transcript. Turns only on Owner messages and fork injections. Runs on the
  Codex provider (GPT-5.6 Sol) under the ChatGPT subscription.
- **Fork** — *ephemeral*: exactly one fork is spawned per incoming non-owner
  message, triages it, and ends. No standing second session, no fork-to-fork
  state. Runs cheap (GPT-5.6 Luna). A fork receives a curated context pack —
  open-Task inventory snapshot, recent conversation tail, standing rules, the
  event — never the raw transcript. It has exactly three exits:
  1. write or update an `info`/`summary` event or Task status directly (the
     main session never turns);
  2. inject one distilled brief into the main session's queue (judgment
     needed);
  3. drop.
  A fork never speaks to the Owner, never spawns Sub-agents, and escalates
  (exit 2) when unsure. Owner messages never pass through a fork.

**Prompts are managed artifacts; policy lives in prompts; capability lives in
tools.** hirsel owns exactly two resident prompts (`prompts/agent.md`,
`prompts/fork.md`), config-overridable and Settings-editable. *How* the main
Agent delegates — downward to workers, upward to an advisor — is prose in its
prompt, never host code. *What* it can delegate to is the Sub-agent catalog
that generates `subagents_spawn`'s input schema: an off-catalog model is an
invalid tool call, not a disobeyed instruction.

**The advisor is a Sub-agent, not a mechanism.** Fable-class consultation
(commitment points: architecture, API shape, migration, taste-critical
surfaces) is a catalog row under the Claude driver — headless `claude` under
the Claude subscription, the self-contained shape those terms permit — plus an
advisor section in `agent.md`: when consulting is mandatory, the packaged
brief (decision, context, constraints, options), the bounded verdict ("Do X,
not Y, because Z" plus the one deciding risk), and act-or-surface — the main
Agent never absorbs a disagreement silently.

## Relation to ADR-0005

The principle stands: the host installs no wake *policy*. The host gains one
mechanical dispatch — non-owner inputs spawn a fork instead of turning the
main session — and everything discretionary (what to drop, what to write, what
to inject, how to treat agent-requested timer wakes) is the fork prompt,
editable in Settings. Wake behavior remains changeable without recompiling,
which is what ADR-0005 was protecting.

## Alternatives rejected

- **Cheap main as interlocutor / router, smart model delegated upward.** The
  fast conversational back-and-forth is where model quality shows most; a
  relay either paraphrases (laundering the smart model's judgment at the last
  mile) or pipes verbatim (plumbing that shouldn't cost a model turn). The
  distilled-slice sub-agent is also not globally aware, violating invariant 4.
  Sol-as-resident is accepted precisely because it is workhorse-grade at the
  hub's one critical skill: knowing when to consult upward.
- **Fable-as-main on metered API.** Architecturally clean (kept as the
  fallback provider mode) but pays top-tier pricing for the resident; a
  persistent session is the best cache topology for an expensive model, yet
  under flat-rate subscriptions the point is moot and the topology above costs
  nothing marginal.
- **Headless `claude` as the main-agent engine.** The session would live
  inside Claude Code's harness, not lash: RLM mode, agent-wired wakes,
  queued-turn control, the durable transcript, fork spawning, and context
  assembly would all be forfeit. Acceptable for self-contained Sub-agent work
  units; a different runtime for a resident.

## Consequences

- Fork model and both resident prompts become validated config with a Settings
  surface; hot edits propagate per the config store's semantics.
- Bridge/monitor/timer wake routing moves from "turn the main session" to
  "spawn a fork"; the main Agent's prompt drops its subscribe-to-terminal-
  events conventions in favor of the injection contract.
- The transcript the main Agent accumulates is Owner conversation plus
  distilled briefs — which is also what keeps a long-lived session good.
- The recorded-rules layer (`docs/product-direction.md` §11) gets its natural
  consumer: rules injected into the fork's context pack let ruled decisions
  auto-resolve at the triage layer, with the applied rule cited in the emitted
  event.

[ADR-0005]: 0005-agent-manages-its-own-wakes.md
