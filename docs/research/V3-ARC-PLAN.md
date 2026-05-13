# Anvil v2.2.14 — Memory Cohesion Arc Plan

**Status:** DRAFT for redline
**Date:** 2026-05-13
**Author:** Claude (Opus 4.7 1M)
**Reviewer:** Jared
**Direction (2026-05-13):** "the seven-layer memory work is the heart of v2.2.14 work as we did not complete this in v2.2.5"

> **This document is a thin coordination layer.** The canonical architecture lives in `docs/research/SEVEN-LAYER-MEMORY.md` + the seven per-layer docs (`MEMORY-LAYER-{1..7}-*.md`). **Read those first; this is the wrapper.**

---

## 0. TL;DR

**v2.2.14 is the cohesion release.** It completes the memory architecture v2.2.5 introduced as storage but did not finish as a lifecycle. It also folds in the CC-parity catch-up + tab-2 production fixes already on the `v2.2.14-cc-parity` branch.

Single substantial release. Single narrative: "completing what v2.2.5 started." Four to six weeks calendar.

The seven-layer architecture is **already designed**. `docs/research/SEVEN-LAYER-MEMORY.md` plus its seven per-layer detail docs is grounded, code-cited, and complete. It proposes four migration buckets (~12–16 engineer-days). The synthesis-author labeled these v2.2.15–v2.2.18 but that was their assumption — Jared's direction folds all four buckets into **v2.2.14 as phases**.

What this plan adds, beyond the synthesis:

1. **Release shape.** v2.2.14 carries cc-parity (already done) + Bucket 0 (new) + Buckets 1-4 (synthesis sequence as Phases 1-4 of v2.2.14).
2. **Bucket 0 — the retrieval-order block.** The "what version of anvil am I" failure is a policy gap not addressed in the synthesis. Two-day fix; ships in Phase 1.
3. **Three-pillar reconciliation.** Mapping each layer's design choices to Anvil's security / persistent-memory / extensibility pillars.
4. **Live in-context learning constraint.** Explicit: no fine-tuning, no training, no model retraining. Memory enriches prompt; base model unchanged.
5. **Quantization / local sidecar role.** Where small local models fit (post-v2.2.14, v2.3.x territory).
6. **v2.2.5 → v2.2.14 public framing.** "Completing what v2.2.5 started" — the narrative for the release.

---

## 1. The canonical architecture (pointer, do not duplicate)

Read these first. This plan does NOT re-explain them:

- **`docs/research/SEVEN-LAYER-MEMORY.md`** — the synthesis. Layer model, current → proposed mapping (mermaid), per-layer one-page summary, 11 discovered defects, four-bucket migration sequence, cross-layer dependency notes, SKIP list, reconciliation with ROADMAP + ROUTINES-ADOPTION-NOTES.
- **`docs/research/MEMORY-LAYER-1-Working.md`** — L1 detail (16KB, code-cited)
- **`docs/research/MEMORY-LAYER-2-Episodic.md`** — L2 detail (20KB)
- **`docs/research/MEMORY-LAYER-3-Semantic.md`** — L3 detail (18KB)
- **`docs/research/Memory-Layer-4-Procedural.md`** — L4 detail (25KB)
- **`docs/research/MEMORY-LAYER-5-identity.md`** — L5 detail (20KB)
- **`docs/research/MEMORY-LAYER-6-Policy.md`** — L6 detail (20KB)
- **`docs/research/MEMORY-LAYER-7-Cache.md`** — L7 detail (18KB)

**Anvil's seven layers (canonical):**

| # | Layer | Function |
|---|---|---|
| L1 | Working | Per-turn system prompt + message buffer |
| L2 | Episodic | Per-session events, replayable history |
| L3 | Semantic | Durable plaintext project knowledge |
| L4 | Procedural | Goals, skills, cron, routines — programs that act |
| L5 | Identity / Secrets | Vault + private memory (encrypted, KEK-gated) |
| L6 | Policy | Persistent decisions about agent permissions |
| L7 | Cache (derived) | file-cache + cmd-cache + QMD index |

Note this is **not** the cognitive-science taxonomy (Sensory/Working/Episodic/Semantic/Procedural/Reflective/Long-term). The synthesis chose a functional cleavage that maps cleanly to Anvil's existing code surface. L5 is Identity/Secrets, L6 is Policy, L7 is Cache. QMD lives in L7 as the indexed cache of the L3 semantic corpus.

---

## 2. What the synthesis covers (so this plan doesn't redo it)

Already in `SEVEN-LAYER-MEMORY.md`:

- ✅ The 7-layer model with one-line function + current home per layer
- ✅ Current → proposed mapping diagram (mermaid)
- ✅ Three category errors the redesign fixes (`nominations` as peer tier, `file-cache`/`cmd-cache` as peer tiers, invisible state)
- ✅ Per-layer one-page summaries pointing to detail docs
- ✅ 11 discovered defects with file:line citations and severity ratings
- ✅ Four-bucket migration sequence (originally labeled v2.2.15-v2.2.18; **reframed as Phases 1-4 of v2.2.14 per Jared's direction**)
- ✅ Effort estimates per bucket (~12–16 engineer-days total)
- ✅ Per-bucket dependency graph
- ✅ Cross-layer dependency notes (L1↔L7, L1↔L4, L2→L3, L3↔L7, L5↔L3, L5↔L7, L6↔L7, L6↔L5)
- ✅ SKIP list (vault format, QMD CLI, daily JSON schema, permission scope variants)
- ✅ Reconciliation with ROADMAP.md and docs/ROUTINES-ADOPTION-NOTES.md
- ✅ Verification grep commands to spot-check the audit claims

Already in the per-layer docs (each has the same 7-section schema):

- §1 Layer definition
- §2 Current Anvil state (with code citations)
- §3 What's missing or miscategorized
- §4 Inspector surface
- §5 Migration moves (numbered with effort estimates)
- §6 Risks
- §7 Cross-layer touch points

**The architecture is done.** What follows is everything the synthesis doesn't address.

---

## 3. v2.2.14 release shape — what ships

v2.2.14 is **one substantial release** carrying three streams of work merged into a single narrative:

### Stream A — cc-parity + tab-2 fixes (already done, ~0 additional days)

The 30 commits currently on `v2.2.14-cc-parity`. Production fixes; tests pass; just needs to land in the final release:

- CC parity v2.1.132 → v2.1.139 (hook args[], continueOnBlock, /scroll-speed, /plugin details, /goal unify, transcript nav, `anvil agents` cross-session monitor, worktree.baseRef, per-session env propagation, OTEL TRACEPARENT, subagent OTel headers, autoMode hard-deny short-circuit, `--resume` underscore paths, plan-mode + Edit allow-rule write blocking, Skill(name *) wildcard delimiter check)
- W11 file-fingerprint cache wiring (was dormant in v2.2.11)
- W15b auto-promote engine wiring
- TUI architecture fixes: Ctrl+C cancels mid-flight streaming; in-flight wait latency 80ms → 20ms; mid-stream user messages queued not dropped
- Tab-2 four-bug bundle (Apple Terminal Enter, missing User log, missing Thinking, render arbitration)
- CC-DRIFT bundle: agent panic on missing tools[], /compact usage rendering, MCP content-block test parity

### Stream B — Bucket 0 (new, ~2 days)

**The retrieval-order block** — see §4. Fixes the verified "what version of anvil am I → web search" failure. Belongs in Phase 1.

### Stream C — Buckets 1-4 (synthesis migration sequence, ~14 days)

Reframed from synthesis-author's v2.2.15-v2.2.18 labels to v2.2.14 phases per Jared's direction:

- **Phase 1 (Bucket 1 + Bucket 0)** — Bug-fix + retrieval-order block. ~5 days. Fixes synthesis defects #1, #6, #7, #9, #10, #11 + ships the retrieval-order policy.
- **Phase 2 (Bucket 2)** — Inspector unification. ~5 days. Lands the user-facing 7-layer model via `/memory show <layer>`.
- **Phase 3 (Bucket 3)** — Safety + semantics. ~4 days. Fixes **critical** defects #5 and #8 (silent-drop on /memory promote; plaintext leak of infra facts). **Pillar 1 deliverables.**
- **Phase 4 (Bucket 4)** — Polish. ~2.5 days. Retention, caps, alias deprecations, dynamic-boundary consumer.

### Stream A + B + C = v2.2.14

**Total calendar: 4-6 weeks** to ship.

**Tag movement caveat (carried from earlier conversation):** the `v2.2.14` tag was partially-shipped during a rollback incident; tag is on `culpur/anvil-source` + `culpur/anvil`, Homebrew formula bumped, AnvilHub config bumped, GitHub Release was deleted. Per the user's choice the tag wasn't reverted. **The existing tag at `e90a10d` IS the right starting commit** (workspace already bumped to 2.2.14, cc-parity branch ahead). When v2.2.14 actually ships, move the tag forward to point at the final release commit; document the move in release notes. This is normal release-pipeline behavior, not a rewrite of history.

---

## 4. Bucket 0 — the retrieval-order block (ships in Phase 1)

**Not in the synthesis.** This is the only architecturally-new item this plan adds.

**The problem:** verified earlier in conversation. Ask Anvil "what version of anvil am I" → it web-searches → returns nothing. The version IS in the prompt at `prompt.rs:280-327` correctly threaded from `main.rs:3921`. The model ignores the authoritative info because there is no policy instructing it to consult local context first.

**Not a wiring bug** (the synthesis already documented the wiring is correct at L1 §2). A **policy gap**.

### 4.1 The block

Concrete draft. ~200 tokens. Goes into `prompt.rs::environment_section()` or as a new section between the boundary and the environment block:

```
# Retrieval order

When answering questions, consult Anvil's local context in this order
before reaching for external tools:

1. **This prompt's environment block** — your identity, version, model,
   working directory, date, active goal. Authoritative.

2. **Loaded instruction files** — ANVIL.md and project-specific
   instructions. Consult before assuming project conventions.

3. **Known files cache** — the `<known-files>` block lists every file
   Anvil has fingerprinted with summaries and key symbols. Check there
   before calling `read_file` on anything Anvil's already seen.

4. **Project memory** — the `# Persistent memory` block carries
   user-stated facts, preferences, and project conventions.

5. **Project tree** — `read_file`, `glob_search`, `grep_search` on the
   working directory for source/config/release info.

6. **Knowledge base (QMD)** — `mcp__qmd__query` for documented project
   history, infrastructure, or prior decisions.

7. **Web search** — only when the question is genuinely about external
   information that local sources cannot answer.

Questions about Anvil itself answer from sources 1–4. Do not web search.
Questions about *this project* answer from sources 2–6 before web search.
```

### 4.2 Acceptance test (Bucket 0)

```rust
#[test]
fn retrieval_order_block_appears_in_prompt() {
    let prompt = build_test_prompt();
    assert!(prompt.contains("# Retrieval order"));
    assert!(prompt.contains("Do not web search."));
}

#[test]
fn self_identity_question_answered_locally() {
    // Manual: fresh anvil session, ask "what version of anvil are you?"
    // Expected: model answers from environment block.
    // No `WebSearch` tool call. No `read_file` on Cargo.toml.
}
```

The manual test is the **diagnostic gate for the entire arc**. If the model still web-searches after this block lands, the diagnosis is wrong and the architecture argument needs reconsideration. If it works (it will), Bucket 0 validates the whole approach for ~$0 in two days, before any of the bigger buckets land.

**This is why Bucket 0 ships in Phase 1, not later.** It's the cheapest gate on the most expensive arc.

---

## 5. Three-pillar reconciliation

Each pillar maps onto the seven layers:

### Pillar 1 — Security

**"Removing sensitive information from plaintext, and giving a safe mechanism for model to access them, and automatically filtering them from plaintext usage."**

- **L5 (Identity/Secrets)** is the home of Pillar 1. AES-256-GCM, KEK derived via Argon2id, per-credential DEK envelopes.
- **The Sensitivity Classifier** (`vault/scan.rs::classify_learning`) is the L1→L5 boundary. **Currently dead-wired (synthesis defect #8). Fixing it in Phase 3 (Bucket 3 §3) closes the plaintext leak.**
- **The L7 invariant** ("no cache byte contains unencrypted L5 content") is the second Pillar 1 guarantee. Fixed in Phase 3 (Bucket 3 §5).
- **Zero-injection test** (Phase 3 / Bucket 3 §6): integration test asserts no L5 label/key/value appears in any rendered system prompt. **This is Pillar 1 verified in CI.**

Pillar 1 lands cleanly in v2.2.14. Three deliverables (Sensitivity Classifier wired, L7 invariant enforced, zero-injection test) all in Phase 3.

### Pillar 2 — Persistent, structured memory

**"Tiered, markdown-frontmattered, survives sessions."**

- The seven layers ARE the implementation. v2.2.5 shipped six storage tiers (Session, Project, Knowledge, Nominations, Private, Daily). The synthesis's redesign reorganizes those plus four others (history, permission_memory, QMD-index, goals/skills/routines) into the seven functional layers.
- **The synthesis defects #4, #5, #9 are all Pillar 2 violations.** History grows forever (no retention). Nominations never actually promote to ANVIL.md. Permission decisions never persist across sessions. Each of these is a documented feature that doesn't work.
- Phases 1-4 fix all of them.

Pillar 2 IS the v2.2.14 memory arc.

### Pillar 3 — Open to providers, skills, agents, abilities

**"Expandable to skills, agents, abilities."**

- **L4 (Procedural)** is the home of Pillar 3. Skills, goals, cron, routines. The marketplace (AnvilHub) ships content into L4.
- The synthesis's Phase 2 / Bucket 2 §4 surfaces L4 as a unified procedural tier (today fragmented across `goals`, bundled skills, on-disk skills, cron entries).
- Plugins are L4 producers. They register skills, register routines, register cron jobs — all L4.

Pillar 3 lands in Phase 2.

**Net:** all three pillars have explicit homes in the seven-layer model. None require new architecture. All three's outstanding gaps are addressed by Phases 1-4 of v2.2.14.

---

## 6. Live in-context learning — explicit constraint

**Rule:** No fine-tuning. No training datasets. No custom model weights. The base model never gets retrained.

**What this means:**

- Memory + context + retrieved patterns enrich each session's prompt.
- The *same* base model behaves smarter over time because it sees more relevant context.
- Quantization (§7) is for layer-internal housekeeping models, not for retraining anything.
- "Learning" in Anvil = better retrieval, better organization, better consolidation. Never weight updates.

**Why this matters for the arc:**

- No GPU dependencies in any phase.
- No training infrastructure. No labeling. No eval harness for trained models.
- Phases 1-4 are all rewiring + new code paths in existing crates. No ML-ops surface.

**Implication for Phase 3 / Bucket 3 §3** (the `classify_learning` wire-up): the classifier itself stays heuristic — regex + structural patterns — not a trained classifier. If we ever want a smarter classifier, it's a separate research project, not part of v2.2.14.

---

## 7. Quantization and the local sidecar (POST-v2.2.14)

**Where local quantized models fit in the layer model, without contradicting §6 (no training):**

Several layer-internal tasks are *summarization, classification, or dedup*. They benefit from running on a small fast model that's local and cheap. They do NOT require model training:

- **L1 → L5 redaction** — Sensitivity classification at journal-write time. Heuristic today; could be small classifier model for fuzzy cases.
- **L2 → L3+L4 promotion** — When compaction promotes facts, deciding what's worth keeping. Summarization task. Small model.
- **L7 cache hash** — Embedding queries for episodic search. Embedding model, local.
- **L6 Policy pattern recognition** — "the user has approved Bash(git push *) 47 times — auto-allow." Heuristic today; could grow.

**Concrete shape:**

```toml
[memory.sidecar]
enabled = false        # opt-in until Anvil ships pre-bundled local models
model = "phi-3-mini:3b-q4_k_m"
runs = ["redaction", "compaction-summarize", "policy-pattern"]
fallback = "heuristic"  # never block on missing model
```

Uses the existing Ollama integration. `HardwareProfile` (from the v2.2.x Ollama deep-integration work) gates whether sidecar tasks run locally or fall back to heuristic-only.

**Not in scope for v2.2.14.** Filed as **post-arc work, v2.3.x candidate.** v2.2.14's four phases deliver the cohesion fix without sidecar models. Sidecar is optimization, not correctness.

---

## 8. v2.2.5 → v2.2.14 — the public narrative

For the v2.2.14 release notes, this is the framing:

**v2.2.5** (April 19, 2026) — "Intelligent Memory System." Shipped six storage tiers (nominations, private memory, daily summaries on top of session/project/QMD) plus the Sensitivity Classifier. The foundation. **Storage without lifecycle.**

**v2.2.11** (May 2026) — Token economy. W11 file-fingerprint cache, W15 /memory commands, W15 AutoPromoter, W13 skill chaining. Deepened L4 (Procedural) and added L7 (Cache) without naming them. More storage, still no lifecycle.

**v2.2.13** (May 11, 2026) — Routines foundation. Schedule grammar, output archive, packet schema. Substrate for the L4 routines sub-view.

**v2.2.14** — **The Cohesion Release. Completing what v2.2.5 started.**

The v2.2.14 release notes lead with a single story:

> *"v2.2.5 introduced six storage tiers and named the arc 'Intelligent Memory System.' What it shipped was foundation — six places to keep things, plus a sensitivity classifier. What it didn't ship was the cohesion: subsystems that compose, a lifecycle that promotes facts as they mature, a retrieval policy that tells the agent to trust its own context before reaching for the web. v2.2.14 completes that arc.*
>
> *Seven layers replace the eight ad-hoc tiers, organized by what each layer is FOR rather than where it lives on disk. Working, Episodic, Semantic, Procedural, Identity, Policy, Cache. Memory becomes inspectable — `/memory show <layer>` walks the live state. Sensitive infra facts stop leaking to plaintext nominations. Permission decisions actually persist. `/memory promote` actually promotes. The retrieval-order policy block ships in the system prompt, and the agent finally trusts its own context. Plus 30 commits of Claude Code parity catch-up and the tab-2 parallel inference fixes the user has been waiting on."*

The framing rule (per `feedback-no-influence-attribution.md`): no external project names appear in the public release notes. The seven-layer model is described as "Anvil's memory redesign" — not derived from anyone else.

---

## 9. v2.2.14 phase-by-phase summary

For ease of redline, the full phase plan:

### Phase 1 — Bucket 0 + Bucket 1 (~5 days)

**Headline: "the agent finally trusts its own context."**

- Bucket 0: retrieval-order block in system prompt (§4)
- Bucket 1: synthesis defects #1, #6, #7, #9, #10, #11
  - `WorkingMemorySnapshot` + replace `Vec<String>` with `Vec<PromptSection>` (~1.5 days)
  - Fix `memory_summary` path mismatch (L3 §3, ~1 hour)
  - Fix `vault.bin` init-marker (L5 §5, ~15 min)
  - **Wire `PermissionMemory` into the gate (L6 §1) — gate behind `permissions.use_permission_memory`** (~1 day)
  - Replace `/file-cache` stub with real handler (L7 §1, ~2-3 hours)
  - Fix `memory_summary`/`memory_budget` path mismatches (L7 §2, ~1 hour)
- Bucket 0: retrieval-order block + acceptance tests (~2 days)
- Phase 1 acceptance: "ask 'what version of anvil are you' → no web search"

### Phase 2 — Bucket 2 (~5 days)

**Headline: "the seven-layer model becomes user-visible."**

- `/memory show working` introspects live WorkingMemorySnapshot (~6 hours)
- `episodic` tier in `memory_summary`, `memory_show`, `memory_budget` (~4 hours)
- `nominations` recast as `semantic --pending` (~4 hours)
- `procedural` tier composing goals + skills + cron entries (~12 hours)
- `identity` tier showing labels/keys (~9 hours)
- `policy` tier showing permission grants (~6 hours)
- Unified `cache` tier with file/cmd/qmd sub-views (~10 hours)
- Spec + subcommand updates (~2 hours)

### Phase 3 — Bucket 3 (~4 days) — Pillar 1 deliverables

**Headline: "memory becomes safe."**

- `/memory promote` actually writes to ANVIL.md / MemoryManager (~1 day)
- Register `anvil-semantic` QMD collection; `nominated_from` frontmatter (~6 hours)
- **Wire `vault::scan::classify_learning` into the nomination-emit path** (~6 hours) — closes the plaintext leak
- `PermissionEffect { Allow, Deny, Prompt }` enum (~2 hours)
- **L5 invariant enforcement: cache MUST NOT contain decrypted vault content** (~1 day)
- **Zero-injection integration test: no L5 byte in any rendered prompt** (~2 hours)
- Fix daily-reconciliation lie in /memory why (~3 hours)

### Phase 4 — Bucket 4 (~2.5 days)

**Headline: "polish + retention."**

- 90-day history retention with trash-bin + dry-run (~3 hours)
- File-cache + cmd-cache size caps + LRU eviction (~1 day)
- `/goal web_available` audit (~2-8 hours)
- Alias deprecations (~3 hours)
- `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` consumer for prompt-caching split (~1 day)

### Total v2.2.14

- Stream A (cc-parity, done): 0 additional days
- Stream B + C (memory work): ~16-17 engineer-days across 4 phases
- Calendar: 4-6 weeks to ship

---

## 10. Open questions for redline

The synthesis already documents most decisions. These are the conversation-derived ones:

### 10.1 Auto-load `silent-cat` skill in Phase 1?

The retrieval-order block alone may be enough. Adding silent-cat auto-load is belt-and-suspenders.

**Recommendation:** ship retrieval-order block only in Bucket 0. Defer silent-cat auto-load to Phase 2 if the block doesn't fully fix the symptom.

### 10.2 v2.2.14 tag movement

Per §3 — move the partially-shipped tag forward to point at the actual release commit, document in release notes. Normal release-pipeline behavior.

**Recommendation: yes, move forward.** Tag was a rollback artifact.

### 10.3 Public framing — describe the seven-layer model in release notes?

The synthesis is internal. The release notes ship publicly.

**Recommendation:** describe at the headline level (per §8 narrative), without going into per-layer detail. The internal architecture doc stays internal. The public release notes say "seven cognitive layers replace eight ad-hoc tiers" but don't walk through L1-L7 individually.

### 10.4 Sidecar / quantization scope

§7 says "post-v2.2.14, v2.3.x candidate." Confirm we're OK leaving it there?

**Recommendation:** leave at v2.3.x. v2.2.14 is already substantial; sidecar is optimization not correctness.

### 10.5 Phase ordering rigidity

The synthesis's bucket order has explicit dependencies (e.g., "Bucket 1 §4 unblocks Bucket 2 §6"). Can phases overlap or do they need to run strictly sequential?

**Recommendation:** Phase 1 must complete before Phase 2 (the snapshot type + handler-side accessors are prerequisites). Phases 2 and 3 can overlap on parallel tracks if multiple engineers work it. Phase 4 is final polish, runs last.

### 10.6 What about the routines / Reflective-equivalent work?

The v2.2.13 routines foundation is real but unused. Phase 2's Bucket 2 §4 stubs the L4 routines sub-view as "Tier 2 in ROADMAP" — implying routines runtime stays out of v2.2.14.

**Recommendation:** **yes, routines runtime stays out of v2.2.14.** The L4 procedural tier surfaces routines metadata when entries exist, but the actual routines runtime (TOML loader, daemon, schedule executor) is v2.3.x. v2.2.14 stays focused on the seven-layer cohesion.

---

## 11. What this plan does NOT do

- **Does not redesign anything.** The synthesis + per-layer docs are canonical. This plan adds release-shape, framing, and the retrieval-order policy block.
- **Does not propose a server pivot.** Process-per-session stays.
- **Does not propose a daemon for Reflective-equivalent work.** The synthesis treats routines + AutoPromoter as in-process; routines runtime is post-v2.2.14.
- **Does not propose fine-tuning or training.** Live in-context learning only.
- **Does not introduce a protocol crate.** Separate cohesion fix, deferred.
- **Does not mention external projects publicly.** Per `feedback-no-influence-attribution.md`.

---

## 12. Action list — what to do, in order

### Immediate (today / this week)

1. **Redline this document.** Open questions in §10 — answer them.
2. **Decide on tag movement** per §3 / §10.2.

### Phase 1 sprint (~1 week)

3. Implement Bucket 0 (retrieval-order block) per §4.
4. Implement Bucket 1 (synthesis §Bucket 1).
5. Phase 1 acceptance test: "ask 'what version of anvil are you' → no web search."

### Phase 2 sprint (~1 week)

6. Implement Bucket 2 (synthesis §Bucket 2). Inspector unification.

### Phase 3 sprint (~1 week)

7. Implement Bucket 3 (synthesis §Bucket 3). **Pillar 1 deliverables land here.**

### Phase 4 sprint (~3-4 days)

8. Implement Bucket 4 (synthesis §Bucket 4). Polish + retention.

### Release (final week)

9. Workspace tests pass (already do; verify after each phase).
10. Update RELEASE-NOTES-v2.2.14.md to the §8 narrative.
11. Surface propagation: public README, AnvilHub /about + /install, culpur.net /anvil WordPress page.
12. Grep all public surfaces for forbidden names per `feedback-no-influence-attribution.md`.
13. Move `v2.2.14` tag forward to release commit per §10.2.
14. Run release pipeline **only after explicit "ship it" approval** per `feedback-no-unauthorized-release.md`.

### Throughout

- CC parity catch-up continues on the side (parallel to memory phases, doesn't conflict). Any new CC parity items file as task #X86X+ and fold into v2.2.14 if they land before ship; otherwise queue for v2.2.15.
- QMD ingest this plan + the synthesis + per-layer docs once redlined.

---

## 13. Appendix — artifact references

- `docs/research/SEVEN-LAYER-MEMORY.md` — canonical synthesis (32KB)
- `docs/research/MEMORY-LAYER-{1..7}-*.md` — per-layer detail docs (~140KB total)
- `docs/research/SEVEN-LAYER-MEMORY-CONTEXT.md` — earlier briefing (superseded by synthesis; kept for history)
- `docs/research/RESEARCH-SUMMARY.md` — May 11 cross-project research (routines-focused, complementary)
- `JCODE-ANALYSIS.md` — comparison work that prompted this conversation
- `COHESION-AUDIT.md` — Haiku audit (smoking-gun finding disproved; retrieval-order diagnosis confirmed)
- v2.2.5 commit `da0eed9` — "Intelligent Memory System" — the arc this completes
- v2.2.14-cc-parity branch — 30 commits folded into the v2.2.14 release

**End of plan. Redline freely.**
