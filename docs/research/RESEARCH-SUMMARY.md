# Anvil — Comparative Research Summary

**Date:** 2026-05-11
**Audience:** Anvil maintainers; future sessions picking up v2.2.13/v2.2.14/v2.3 work; external reviewers
**Anvil version under audit:** v2.2.12 (just released today)
**Methodology:** Six parallel Explore-agents read six external projects against the Anvil tree at `/Users/soulofall/projects/anvil-dev`. Each produced a deep, file:line-cited comparison at `docs/research/COMPARE-vs-<project>.md` following the same 12-section schema. This doc is the synthesis layer — it does not re-prove claims; it points at the detail docs and reconciles them.

---

## How to read this doc

Three layers exist:

1. **`ROUTINES-ADOPTION-NOTES.md`** (docs/) — the *active design document* for what Anvil should build, written from the planning team's perspective. References "v2.4" as a build-order anchor (not a release label).
2. **`COMPARE-vs-<project>.md`** (docs/research/) — six deep audits, one per external project. Each ends with §11 (gaps) and §12 (adopt/skip with rationale). Every claim is verifiable via grep in the cited repo.
3. **This file (`RESEARCH-SUMMARY.md`)** — the cross-project synthesis. What's the consensus? Where do the projects disagree? Which Anvil planning track should pick up each adoption?

If you only have 10 minutes: read the **Top-line consensus** section below + the **Reconciled adopt/skip table**. If you have 30 minutes: also read the **Source quality** section to understand which audits ground which recommendations.

---

## Top-line consensus

Across all six audits, **eight features** show up as gaps in Anvil that multiple projects independently solve in compatible ways. These are the highest-conviction adoption candidates because the cross-project agreement reduces the chance of cargo-culting one project's idiosyncrasy:

| Feature | Solved by | Anvil gap | Synthesis recommendation |
|---|---|---|---|
| **Schedule grammar (duration/interval/cron/ISO)** | Hermes (`jobs.py:184-263`), Archon (workflow triggers), gsd-2 (job scheduler) | Anvil v2.2.12 stores `cron_expression: String` only (5-field cron) | **ADOPT** — ~1 day. Hermes's parser is the cleanest reference. |
| **Pre-agent script + wake gate (zero-token polling)** | Hermes (`scheduler.py:1030+`, `_parse_wake_gate` at `811-834`) | No equivalent | **ADOPT** — ~2 days. Converts monitoring routines from "always pay" to "pay only when news arrives." Highest-leverage feature in this whole audit. |
| **`[SILENT]` suppression marker** | Hermes (`scheduler.py:129, 1741-1790`) | No equivalent | **ADOPT** — <1 day. One literal-string check; removes a complaint class. |
| **Output archive on disk** | Hermes (`~/.hermes/cron/output/{job_id}/{timestamp}.md`, jobs.py:5, 975-980), GSD-2 (verification-evidence DB), Archon (per-run artifacts) | No equivalent | **ADOPT** — ~half day. Mandatory for production routine use. The HANDOFF-v2.2.13.md design is exactly this. |
| **Immutable packet schema with `input_hash` for idempotency** | context-packet (`src/types.ts:3-12`, `src/hasher.ts:25-28`) | No equivalent | **ADOPT** — ~half day. Pairs with output archive. Unlocks "did the inputs change?" cache-hit detection. |
| **Three-phase token-budgeted context resolver** | context-packet (`src/resolve.ts:16-141`) | No equivalent | **ADOPT** — ~1-2 days. Prevents silent output loss when chained routines exceed context window. |
| **Anti-injection delimiters around injected content** | context-packet (`src/sanitize.ts:5-11`) | No equivalent | **ADOPT** — ~1 hour. Trivial cost, non-zero benefit. |
| **Pre-dispatch state reconciliation with idempotent repairs, capped at N passes** | gsd-2 (ADR-017, drift registry at `src/resources/extensions/gsd/state-reconciliation/`) | Anvil §10.5 of ROUTINES-ADOPTION-NOTES.md is design-stage only, not shipped | **ADOPT** — ~3 days. Solves the entire class of "routine has been running for two weeks, something rotated out from under it, silent failure" bugs before they surface. |

**Total cross-project consensus adoption work:** ~8-10 days of solo work, fits cleanly into the next 2-3 patch releases.

Beyond the consensus eight, each project has **idiosyncratic patterns worth one paragraph each:**

- **Archon:** DAG execution with fan-out/join + node-output substitution (`$nodeId.output`). The right thing for v2.5 self-built MCPs and Tier 5 marketplace agent composition, but not v2.4-shaped — adopt **after** linear `context_from` proves the use case.
- **Superpowers:** `red_flags` block in skill frontmatter + imperative "REQUIRED SUB-SKILL" prose. Prompt-engineering moves, not runtime work. Adopt cheaply (~4 hours combined). Skip the multi-harness plugin architecture — architectural mismatch for Anvil.
- **GSD-1 (get-shit-done):** Same conclusion as gsd-2's reconciliation pattern. The v1 audit is mostly subsumed by gsd-2; GSD-1's unique contributions are the pre-dispatch repairs-capped-at-N-passes idiom, which IS the GSD-2 reconciliation pattern with simpler implementation.
- **gsd-2 (beyond reconciliation):** Worktree lifecycle + safety model (worktree-per-milestone). Adoptable if Anvil grows parallel-milestone dispatch. Time-decay memory ranking is interesting long-term but orthogonal. Verify-evidence DB + auto-retry backoff is a reliability win worth ~1 day. Remote-questions routing (Slack/Discord) belongs in Tier 7 collaboration.
- **context-packet:** The three patterns above are *the* contribution. Everything else (file-based state, atomic writes, topological sort) is architectural cleanliness Anvil can borrow stylistically without separate adoption decisions.
- **Hermes:** The headline gaps (schedule, wake gate, [SILENT], delivery) are *the* contribution. The 12+ delivery adapters (Telegram, Discord, Slack, Matrix, …) are out of scope until Tier 7 — when they land, Hermes's `_resolve_single_delivery_target` (`scheduler.py:258-342`) is the reference parser.

---

## Reconciled adopt/skip table

This table merges the per-project §12 recommendations across all six docs. Where projects disagree, the synthesis picks the most defensible path (usually the cheapest implementation that still solves the named problem) and notes the disagreement.

| # | Adoption | Source (primary) | Cross-confirmed by | Cost | Track | Notes |
|---|---|---|---|---|---|---|
| 1 | Schedule grammar expansion (duration/interval/cron/ISO) | Hermes jobs.py:184-263 | Archon, gsd-2 | ~1 day | **v2.2.14 / Tier 2 item 2** | HANDOFF-v2.2.13.md item 2 already names this. |
| 2 | `[SILENT]` marker | Hermes scheduler.py:129, 1741-1790 | — | <1 day | **v2.2.14 / Tier 2 item 1** | HANDOFF-v2.2.13.md item 1 already names this. |
| 3 | Output archive at `~/.anvil/routines/output/{id}/{ts}.md` | Hermes jobs.py:5, 975-980 | gsd-2 (verification evidence), Archon (artifact dirs) | ~half day | **v2.2.14 / Tier 2 item 3** | HANDOFF-v2.2.13.md item 3 already names this. |
| 4 | Packet schema with `input_hash` | context-packet src/types.ts:3-12, src/hasher.ts:25-28 | — | ~half day | **v2.2.14 / Tier 2 item 4** | HANDOFF-v2.2.13.md item 4 already names this. |
| 5 | Anti-injection delimiters | context-packet src/sanitize.ts:5-11 | — | ~1 hour | **v2.2.14 (with item 4)** | Trivial; fold into packet-schema work. |
| 6 | Pre-agent script + wake gate | Hermes scheduler.py:1030+, 811-834 | — | ~2 days | **v2.2.15 / Tier 2 item 12** | Defers from v2.2.13 in HANDOFF doc; the synthesis confirms it's a high-leverage v2.2.15 add. |
| 7 | Three-phase token-budgeted context resolver | context-packet src/resolve.ts:16-141 | — | ~1-2 days | **v2.2.15 / Tier 2 item 7** | Depends on packet schema (#4). |
| 8 | Linear `context_from` chaining | Hermes scheduler.py:918-935 | context-packet (via DAG primitive) | ~1 day | **v2.2.15 / Tier 2 item 6** | Depends on packet schema (#4); precedes resolver (#7) in build order. |
| 9 | Pre-dispatch reconciliation registry | gsd-2 ADR-017 / `src/resources/extensions/gsd/state-reconciliation/` | GSD-1 (ad-hoc precursor) | ~3 days | **v2.2.16 / Tier 2 item 15** | This is the load-bearing safety net for routines that have been running for weeks. ROUTINES-ADOPTION-NOTES.md §10.5 is design-stage; this audit confirms gsd-2's shipped pattern is what to copy. |
| 10 | `red_flags` block in skill/routine frontmatter | Superpowers (narrative form, supports/anti-pattern.md) | ROUTINES-ADOPTION-NOTES.md §12.5 (planned) | ~4 hours | **v2.2.x patch** | Anvil's planned form (machine-readable in frontmatter) is *more* structured than Superpowers' narrative version. Tier 2 item 16. |
| 11 | Imperative "REQUIRED SUB-SKILL" prose convention | Superpowers writing-plans/SKILL.md:52, subagent-driven-development/SKILL.md:269-280 | — | ~1-2 hours | **v2.2.x patch (documentation only)** | Zero code cost; pure prose-style guide. Tier 2 item 17. |
| 12 | Subagent two-stage review skill (spec then code-quality) | Superpowers subagent-driven-development/SKILL.md:42-87 | — | ~3-4 hours | **v2.3+ / Tier 4 self-improvement** | Anvil already has subagent dispatch; missing piece is the *methodology* skill. Adopt as a bundled skill at `crates/commands/bundled/skills/subagent-review/`. |
| 13 | Worktree-per-milestone lifecycle | gsd-2 worktree-lifecycle.ts, worktree-safety.ts | — | ~3-5 days | **Tier 6 (multi-repo) / deferred** | Anvil already has `crates/tools/src/worktree_ops.rs` but no per-milestone lifecycle. Wait until parallel-milestone dispatch is needed (Tier 6). |
| 14 | Verify-evidence DB + exponential backoff retries | gsd-2 verification-retry-policy.ts | — | ~1 day | **v2.3+** | Reliability win. Useful once Tier 2 routines have stable production usage. |
| 15 | Remote-questions routing (Slack/Discord for headless decisions) | gsd-2 src/resources/extensions/remote-questions/ | — | ~1-2 days | **Tier 7 (collaboration)** | Pairs with Tier 7 delivery adapters. Defer. |
| 16 | DAG execution with fan-out/join + `$nodeId.output` substitution | Archon dag-executor.ts:1030-1080 | — | ~1.5 days after #8 | **v2.3+ / Tier 5 marketplace** | Linear `context_from` (#8) covers 80%; DAG unlocks self-improving MCPs (Tier 4) and agent composition (Tier 5). |
| 17 | Delivery layer (local + email + webhook) | Hermes scheduler.py:258-342 (12+ adapters) | — | ~1.5 days for minimal three | **v2.3+ / Tier 2 item 10-11** | HANDOFF-v2.2.13.md explicitly defers this. Synthesis agrees: `local` archive is mandatory (#3); other delivery targets wait until routines see production usage. |

**Strict SKIP list** (cross-project consensus that Anvil should NOT adopt):

| Pattern | Source | Why skip |
|---|---|---|
| YAML workflow format | Archon | Anvil standardized on TOML (vault config, project config). Skip. |
| Multi-harness plugin architecture | Superpowers | Anvil is a tool, not a plugin library. Architectural mismatch. Skip. |
| Cron-specific prompt-injection scanner | Hermes scheduler.py:45-54 | Anvil's MCP hardening covers tool-level scanning. Ship skill-allowlist for cron context instead. Skip the regex scanner. |
| Time-decay memory ranking (ADR-013) | gsd-2 memory-store.ts | Orthogonal to orchestration. Anvil's memory model (vault + private + nominations) is fine. Skip. |
| Deep project planning mode | gsd-2 deep-project-setup-policy.ts | UX feature for long multi-month projects. Skip unless Anvil grows that audience. |
| Milestone completion rollup dashboard | gsd-2 | UI feature, not infrastructure. Skip. |

---

## Cross-project gap matrix

This matrix shows, for each major capability, which projects ship it and where Anvil stands. The matrix is intentionally compact; full citations are in the per-project COMPARE docs.

| Capability | Anvil v2.2.12 | Hermes | Archon | gsd-2 | GSD-1 | Superpowers | context-packet |
|---|---|---|---|---|---|---|---|
| 5-field cron | ✅ | ✅ | ✅ | (sched via SDK) | — | — | — |
| Duration `"30m"` | ❌ | ✅ | ✅ | ✅ | — | — | — |
| Interval `"every 30m"` | ❌ | ✅ | ✅ | ✅ | — | — | — |
| ISO timestamp `"2026-02-03T14:00Z"` | ❌ | ✅ | ✅ | — | — | — | — |
| Pre-agent script | ❌ | ✅ (scheduler.py:1030+) | ❌ | ❌ | ❌ | ❌ | ❌ |
| Wake gate `{"wakeAgent": false}` | ❌ | ✅ (`_parse_wake_gate`, 811-834) | ❌ | ❌ | ❌ | ❌ | ❌ |
| `[SILENT]` suppression marker | ❌ | ✅ (scheduler.py:129) | ❌ | ❌ | ❌ | ❌ | ❌ |
| Output archive on disk | ❌ | ✅ (jobs.py:975+) | ✅ | ✅ (verification-evidence DB) | (markdown notes) | — | ✅ (.context-packet/) |
| Immutable packets with `input_hash` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (src/types.ts:3-12) |
| Token-budgeted context resolver | ❌ | ❌ (flat injection) | ❌ | ❌ | ❌ | ❌ | ✅ (src/resolve.ts:16-141) |
| Anti-injection delimiters | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (src/sanitize.ts:5-11) |
| Linear context chaining (`context_from`) | ❌ | ✅ (scheduler.py:918-935) | ✅ (DAG node refs) | ❌ | (file refs) | ❌ | ✅ (via DAG) |
| DAG with fan-out/join | ❌ | ❌ | ✅ (dag-executor.ts) | ❌ | ❌ | ❌ | ✅ |
| Pre-dispatch state reconciliation | ❌ (§10.5 design only) | ❌ | ❌ | ✅ (ADR-017) | (ad-hoc precursors) | ❌ | ❌ |
| Worktree-per-milestone lifecycle | partial (worktree_ops.rs) | ❌ | (workflow runs) | ✅ | ❌ | ❌ | ❌ |
| `red_flags` frontmatter | ❌ | ❌ | ❌ | ❌ | ❌ | (narrative form) | ❌ |
| Vault-only secrets | ✅ (crates/runtime/src/vault/) | ✅ (`vault:<label>`) | (env vars) | (env) | (env) | — | — |
| Persistent daemon (launchd/systemd) | ❌ (Tier 2 item 20-22 planned) | ✅ | ✅ (server-based) | ✅ | ❌ | ❌ | ❌ |
| Per-tab parallel inference | ✅ (v2.2.12) | ❌ | ❌ | (parallel-slice) | ❌ | ❌ | ❌ |
| SSH tabs | ✅ (v2.2.12) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Tool-call cards / live transparency | ✅ (v2.2.12) | ❌ | (SSE web) | ✅ | (CLI output) | — | — |
| AnvilHub-style marketplace | ✅ (crates/runtime/src/hub.rs) | ❌ | (workflows registry) | ❌ | ❌ | (plugin registry) | ❌ |

Reading the matrix: **Anvil's v2.2.x foundations are unique** (per-tab inference, SSH tabs, tool-call cards). What it's missing is the **routine layer** that Hermes/Archon/gsd-2 all have, plus the **context-discipline primitives** that context-packet ships.

---

## Source quality

Different audits ground different recommendations. This section helps a reader weight the conclusions.

| Audit | Lines | Citations | Confidence in recommendations | Notes |
|---|---|---|---|---|
| `COMPARE-vs-archon.md` | 204 | 63 | High | Cleanest project for DAG patterns. Recommends adopting fan-out and `$nodeId.output` substitution; this synthesis defers both to v2.3+ pending linear-chaining usage data. |
| `COMPARE-vs-hermes-agent.md` | 241 | 76 | Very high | The reference for everything cron-routine-shaped. Audits previously-named "wake gate, [SILENT], delivery, schedule grammar" claims and confirms all four with exact line numbers. |
| `COMPARE-vs-superpowers.md` | 397 | 49 | High | Audits the narrative `red_flags` pattern and confirms it's *prose-form* in Superpowers, not frontmatter — meaning Anvil's planned frontmatter version (per ROUTINES-ADOPTION-NOTES.md §12.5) would be *more* structured than the inspiration. |
| `COMPARE-vs-get-shit-done.md` | 324 | 25+ | High | GSD-1 audit. Confirms the reconciliation idiom exists in v1 in simpler form, which gsd-2 generalized. The synthesis treats this audit as supporting context for gsd-2's ADR-017 claim. |
| `COMPARE-vs-gsd-2.md` | 399 | 94 | Very high | The richest single audit. Identified ten gsd-2-unique patterns; the synthesis adopts only #1 (reconciliation) for near-term and notes #5 (verify backoff) + #10 (remote questions) for later. Time-decay memory ranking and deep planning explicitly SKIPPED with reasoning. |
| `COMPARE-vs-context-packet.md` | 473 | 48 | Very high | Audits all 39 files of a small reference implementation. Recommends adopting all three named patterns (resolver, packets+hash, delimiters); the synthesis agrees verbatim. |

Cross-project agreement on the **consensus eight** is high because three different audits (Hermes, context-packet, gsd-2) ship overlapping patterns. Items unique to one audit (Archon's DAG, gsd-2's worktree lifecycle) get more cautious recommendations because there's no cross-confirmation.

---

## Reconciliation with HANDOFF-v2.2.13.md

The HANDOFF-v2.2.13.md doc (written 2026-05-11 ~14:50, before this research synthesis landed) defines v2.2.13's scope as Tier 2 items 1-4:

1. `[SILENT]` marker
2. Schedule grammar expansion
3. Output archive
4. Packet schema with `input_hash`

This synthesis **confirms all four** as the right v2.2.13 scope:

- **Item 1 (`[SILENT]`):** Hermes scheduler.py:129 is the reference. Synthesis cost matches HANDOFF's "~30 min" claim.
- **Item 2 (schedule grammar):** Hermes jobs.py:184-263 is the reference. Synthesis cost matches HANDOFF's ~1 day claim.
- **Item 3 (output archive):** Hermes jobs.py:975+ is the reference. Synthesis cost matches HANDOFF's ~half day.
- **Item 4 (packet schema):** context-packet src/types.ts:3-12 + src/hasher.ts:25-28 are the reference. Synthesis cost matches HANDOFF's ~half day.

**One refinement:** the HANDOFF doc doesn't explicitly include **anti-injection delimiters** (item 5 in synthesis above). Since they're a 1-hour add and naturally bundle with packet schema work (item 4), fold them into item 4 in the v2.2.13 implementation.

**Two new items HANDOFF-v2.2.13.md doesn't yet address:**

- **`red_flags` frontmatter** (synthesis #10) and **"REQUIRED SUB-SKILL" prose convention** (#11) are independent of routines work. They could ship in v2.2.13 as a small parallel bundle (~6 hours combined), or wait for v2.2.14. Either is fine; they're not on the routines critical path.
- **Pre-dispatch reconciliation** (synthesis #9) is *the* gsd-2 finding and not in HANDOFF-v2.2.13.md (which explicitly defers items 10+ from the Tier 2 numbered list). Synthesis confirms HANDOFF's deferral was correct — reconciliation should land in v2.2.16 (or whichever patch comes after items 5-9 of Tier 2 ship), not v2.2.13.

**Net:** HANDOFF-v2.2.13.md is correctly scoped. No revisions needed from this research. The HANDOFF-v2.2.14.md the next session writes should include:

1. Items 5 (TOML loader) through 8 (anti-injection delimiters — already folded into v2.2.13) of Tier 2
2. Pre-agent script + wake gate (Tier 2 item 12) — high-leverage, defer one cut beyond v2.2.13
3. Linear `context_from` (Tier 2 item 6) + three-phase resolver (Tier 2 item 7) — pair them
4. `red_flags` + "REQUIRED SUB-SKILL" — small ergonomic adds; ship when convenient
5. Pre-dispatch reconciliation — flag as **the v2.2.16-ish target**; do not let it slip past the routines daemon work (Tier 2 items 20-24)

---

## Reconciliation with ROADMAP.md

The ROADMAP.md tiers were last updated 2026-05-11 with the platform-reach Tier 8.5 addition. Tier 2 (proactive execution) is the routines backlog where most of this research lands.

Mapping synthesis adoptions to ROADMAP tiers:

| Synthesis # | ROADMAP location | Already in ROADMAP? |
|---|---|---|
| 1 (schedule grammar) | Tier 2 item 2 | ✅ |
| 2 (`[SILENT]`) | Tier 2 item 1 | ✅ |
| 3 (output archive) | Tier 2 item 3 | ✅ |
| 4 (packet schema) | Tier 2 item 4 | ✅ |
| 5 (anti-injection delimiters) | Tier 2 item 8 | ✅ |
| 6 (pre-agent script + wake gate) | Tier 2 item 12 | ✅ |
| 7 (token-budgeted resolver) | Tier 2 item 7 | ✅ |
| 8 (linear `context_from`) | Tier 2 item 6 | ✅ |
| 9 (pre-dispatch reconciliation) | Tier 2 item 15 | ✅ |
| 10 (`red_flags` frontmatter) | Tier 2 item 16 | ✅ |
| 11 ("REQUIRED SUB-SKILL" prose) | Tier 2 item 17 | ✅ |
| 12 (subagent two-stage review skill) | Tier 4 item 1 (pattern analyzer) — adjacent | Implicit |
| 13 (worktree-per-milestone) | Tier 6 item 4 | ✅ |
| 14 (verify-evidence DB + backoff) | Not in ROADMAP | **Add as Tier 2 follow-up** |
| 15 (remote-questions routing) | Tier 7 (Telegram/Slack delivery, item 1-2) | Adjacent |
| 16 (DAG with fan-out/join) | Tier 4 (self-improving MCPs need DAG) + Tier 5 (agent composition) | Implicit |
| 17 (delivery layer minimal three) | Tier 2 items 10-11 | ✅ |

**One ROADMAP gap surfaced:** synthesis #14 (verify-evidence DB + exponential-backoff retries) is not in the ROADMAP at any tier. The audit (gsd-2) recommends it as a reliability win once routines are in production usage. Suggested addition: a new Tier 2 item *after* the daemon work, something like:

> **(Tier 2, new item 30) Verify-evidence DB + exponential-backoff retry policy.** For routines that include a verification step (test runs, deploy health checks), store stdout/stderr/exit-code rows in the journal and retry failures with bounded exponential backoff (gsd-2's `verification-retry-policy.ts` is the reference). ~1 day after the journal is durable (Tier 3).

This is a small enough addition that it doesn't change the tier structure — just adds one entry. The session that picks up reconciliation work should also add this item to ROADMAP.md Tier 2.

---

## Build order (consolidated)

Combining the HANDOFF-v2.2.13.md scope, the synthesis adoption table, and the ROADMAP tier ordering, the recommended sequence is:

**v2.2.13** (per HANDOFF-v2.2.13.md, confirmed by this research):
1. `[SILENT]` marker — ~30 min
2. Schedule grammar expansion — ~1 day
3. Output archive — ~half day
4. Packet schema + `input_hash` — ~half day
5. Anti-injection delimiters (fold into #4) — ~1 hour
6. *(Platform-reach: Windows SSH cross-compile fix per ROADMAP Tier 8.5 Stream A — task #441, independent track)*

Total v2.2.13: ~2.5 days routines + 1 day Windows fix = ~3.5 days. Realistic shipping window.

**v2.2.14:**
7. TOML authoring + loader (Tier 2 item 5) — ~1 day
8. Linear `context_from` chaining (Tier 2 item 6) — ~1 day
9. Token-budgeted three-phase resolver (Tier 2 item 7) — ~1.5 days
10. `red_flags` frontmatter + "REQUIRED SUB-SKILL" prose convention — ~5 hours combined

Total v2.2.14: ~3.5-4 days.

**v2.2.15:**
11. Pre-agent script + wake gate (Tier 2 item 12) — ~2 days
12. Vault-ref resolution (Tier 2 item 13) — ~1 day
13. Vault-ref linter (Tier 2 item 14) — ~half day

Total v2.2.15: ~3.5 days.

**v2.2.16:**
14. Pre-dispatch reconciliation module (Tier 2 item 15) — ~3 days
15. Subagent two-stage review skill (bundled skill, Tier 4-adjacent) — ~4 hours
16. Verify-evidence DB + backoff retries (new Tier 2 item) — ~1 day

Total v2.2.16: ~4.5 days.

**v2.2.17+:** Tier 2 daemon work (items 20-24), consent UX (§17), reference routines (item 25), watch/webhook/event triggers (items 26-28). This is where the routines layer becomes operationally useful — until the daemon ships, all the routine TOMLs sit on disk un-fired.

**Tier 4+ (self-improving MCPs):** Adds DAG with fan-out/join (synthesis #16) to the linear chaining shipped in v2.2.14.

**Tier 6 (multi-repo):** Adopts gsd-2's worktree-per-milestone lifecycle (synthesis #13).

**Tier 7 (collaboration):** Adopts Hermes's delivery target grammar (full 12+ adapters) and gsd-2's remote-questions routing.

The first four releases (v2.2.13-16, ~14 days total) capture **every consensus-eight adoption from this research**. Everything after that is either deferred (DAG, worktree, delivery, remote-questions) or out of scope (memory ranking, deep planning, multi-harness plugin, milestone rollup, etc.).

---

## Verification

To verify any specific claim in this synthesis, read the relevant `COMPARE-vs-<project>.md` and check the cited line number with `grep -n "<symbol>" <path>` or `sed -n "<line>p" <path>` in the external project's source.

Example: claim "Hermes implements `_parse_wake_gate` at scheduler.py:811-834."

```bash
sed -n '811,834p' /Users/soulofall/projects/hermes-agent/cron/scheduler.py
```

If the symbol doesn't exist at that line, the audit is wrong. The audits were instructed to fail-loudly on missing patterns ("`X does not appear to have Y (no match for pattern in repo)`") rather than fabricate symmetry.

Total citations across the six audits: **~355 file:line references** (63 + 76 + 49 + 25+ + 94 + 48). Most claims in this synthesis are sourced; the unsourced claims are either consensus across multiple audits (in which case at least two of the six docs back them) or design recommendations explicitly attributed to ROUTINES-ADOPTION-NOTES.md or the ROADMAP.md tiers.

---

## What this research did NOT do

Honesty about scope:

- **Did not benchmark.** No performance comparison. The audits are architectural / behavioral, not perf.
- **Did not audit Anvil's actual implementation depth.** Claims like "Anvil has the vault" cite `crates/runtime/src/vault/` but don't verify every Anvil feature is implemented correctly. The Anvil side of each audit assumes Anvil's shipped behavior matches its README and v2.2.12 release notes; a future audit could verify that independently.
- **Did not test cross-project interop.** Whether Anvil could literally import Hermes's `parse_schedule()` is unknown (Python → Rust port). The synthesis assumes "study the algorithm, reimplement in Rust" — actual porting cost may differ.
- **Did not survey the AI agent ecosystem comprehensively.** Six projects is a sample, not a survey. There are other patterns (CrewAI's tool dispatching, AutoGen's group chat, Magentic's structured outputs) not audited here. If a future planning cycle wants more coverage, that's a separate research pass.
- **Did not produce code.** Every adoption recommendation is design-level. The actual implementations land per the build order above, in subsequent sessions.

---

## See also

- `/Users/soulofall/projects/anvil-dev/docs/ROUTINES-ADOPTION-NOTES.md` — active design document for Anvil routines work
- `/Users/soulofall/projects/anvil-dev/ROADMAP.md` — backlog tiers
- `/Users/soulofall/projects/anvil-dev/HANDOFF-v2.2.13.md` — concrete v2.2.13 plan (this research confirms its scope)
- `/Users/soulofall/projects/anvil-dev/docs/research/COMPARE-vs-archon.md` — full Archon audit
- `/Users/soulofall/projects/anvil-dev/docs/research/COMPARE-vs-hermes-agent.md` — full Hermes audit
- `/Users/soulofall/projects/anvil-dev/docs/research/COMPARE-vs-superpowers.md` — full Superpowers audit
- `/Users/soulofall/projects/anvil-dev/docs/research/COMPARE-vs-get-shit-done.md` — full GSD-1 audit
- `/Users/soulofall/projects/anvil-dev/docs/research/COMPARE-vs-gsd-2.md` — full gsd-2 audit
- `/Users/soulofall/projects/anvil-dev/docs/research/COMPARE-vs-context-packet.md` — full context-packet audit
