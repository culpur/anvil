# Context Package — Seven-Layer Memory Design Doc

**For:** the planning session for `docs/research/SEVEN-LAYER-MEMORY.md`
**Date:** 2026-05-12
**Status:** Live working notes — update as we go

This file is the **briefing material** for whoever drafts `SEVEN-LAYER-MEMORY.md`. It captures everything decided in conversation so the draft doesn't reinvent it. Read this first.

---

## The seven layers (user-stated, canonical)

These are Jared's words verbatim, lightly formatted. The doc must use exactly these layer names and definitions:

1. **Sensory Memory** — Captures immediate input (text, audio, visual) from the environment or user interaction.
2. **Working Memory (Short-Term)** — Holds current context, active tasks, and recent actions (e.g., in Redis) for immediate reasoning.
3. **Episodic Memory** — Stores specific, timestamped experiences and past interactions, allowing the agent to recall "what happened".
4. **Semantic Memory (Knowledge Base)** — Stores general knowledge, facts, and structured information (often via vector embeddings).
5. **Procedural Memory (Habits/Skills)** — Learns and stores "how-to" actions, successful workflows, and tool usage patterns.
6. **Reflective Memory (Dreaming/Consolidation)** — A background process that reviews, consolidates, and reorganizes memory during inactivity to improve future performance (e.g., Anthropic's "dreaming" feature).
7. **Long-Term Storage** — Permanent storage for all consolidated data, user preferences, and learned patterns (e.g., SQL + Vector DB).

**Anvil-specific binding for layer 7:** Long-Term Storage IS QMD. Stated by Jared: "QMD is long term storage all things eventually go to QMD."

---

## Three pillars (Anvil thesis — must be reconciled with the layer model)

1. **Security** — vault for secrets, automatic filtering of sensitive plaintext, safe model access paths
2. **Persistent structured memory** — tiered, markdown-frontmattered, survives sessions. *The seven-layer model IS the implementation of this pillar.*
3. **Open to providers, skills, agents, abilities** — extensible via plugins, marketplace (AnvilHub)

**The mapping (already decided in conversation):**

- The seven layers ARE the implementation of Pillar 2.
- Pillar 1 (security) sits parallel to Semantic memory; vault is a typed semantic store with special promotion rules. Secrets never flow Working → Semantic without explicit user approval. Filtering happens at the **Sensory → Working** boundary (strip before entering working memory) and the **Working → display** boundary (redact before showing the user).
- Pillar 3 (extensibility) works because skills, plugins, agents are all **producers and consumers of Procedural memory.** The marketplace composes only because the layer is well-defined.

---

## Learning model (decided)

**Live in-context learning. No fine-tuning. No training.**

The base model never gets retrained. Memory + context + retrieved patterns enrich each session's prompt, so the *same* base model behaves smarter over time because it sees more relevant context.

This means the design doc must NOT propose:
- Fine-tuning loops
- Training datasets
- Custom model weights

It MAY propose:
- Local quantized models for retrieval/summarization/memory-sidecar tasks (running alongside the main model)
- Embedding models (local or hosted) for semantic indexing
- Model-to-hardware optimization for the sidecar layer

The quantization angle: a small quantized model can run on the user's hardware for layer-internal tasks (consolidation, summarization, embedding) so the main coding model isn't paying tokens for memory housekeeping.

---

## The design constraints (must be testable properties)

These came out of the conversation as hard requirements:

1. **Memory must survive compaction.** Stated as a reaction to Claude Code's failure mode: "even in the same session after compacting [it forgets everything]." After `/compact` runs, the model must still answer "what did we just decide" correctly because the relevant facts were promoted out of the transcript into a tier compaction doesn't touch.

2. **The prompt must be dynamic, not static.** Stated as: "it should be able to learn on the fly, adapt on the fly, change course on the fly." The system prompt is a snapshot today. The design must specify a **delta protocol** — subsystems emit state-change events, the prompt builder injects deltas since last turn. Not full re-render (too expensive); not static (too blind). Delta blocks.

3. **Subsystems must compose.** The cohesion audit's finding: 7 of 11 subsystems are stored-but-invisible to the model. Every subsystem must declare which layer it produces to and which layer it consumes from. No more isolated storage.

4. **Retrieval order must be explicit.** Today the model picks any tool order; web search often wins. The design must specify the **layer-priority retrieval order** — Sensory → Working → Episodic → Semantic → Procedural → Long-term → web. Web search is below all seven layers.

5. **Vault security gate stays intact.** Agent can suggest vault additions (with user approval). Agent never writes vault directly. Symmetric to nominations.

---

## What Anvil has today, per layer (audit findings)

From `COHESION-AUDIT.md` (2026-05-12) and verified call-site analysis:

| Layer | Current state | Existing code |
|---|---|---|
| **1. Sensory** | Implicit, unnamed. Hooks fire (W1 catch-up in v2.2.11) but are observe-only. | `crates/runtime/src/hooks.rs` |
| **2. Working** | Conversation history. Destroyed by compaction (truncated to 160-char summary per message). | `crates/runtime/src/history.rs`, `compact.rs` |
| **3. Episodic** | **Does not exist.** Session files on disk but no episode index, no recall-by-time. | (none) |
| **4. Semantic** | Split across three unconnected storages: memory tiers (markdown, manual), QMD (vector-indexed), ANVIL.md (per-project). Not unified. | `crates/runtime/src/memory.rs`, `qmd.rs`, prompt builder |
| **5. Procedural** | Skills exist, AutoPromoter exists, **they don't share data.** 7 bundled skills hand-written, not learned. | `crates/commands/bundled/skills/`, `crates/runtime/src/auto_promote.rs` |
| **6. Reflective** | **Does not exist as a runtime concept.** No background consolidation. Routines daemon is the natural fit (v2.2.13 foundation laid). | (none yet; `crates/runtime/src/routines/` is the seed) |
| **7. Long-term** | QMD. Working as designed. | `crates/runtime/src/qmd.rs` |

**Score: 1.5 of 7 layers in production.** Working + partial Long-term. Episodic and Reflective completely missing. Sensory unnamed. Semantic and Procedural fragmented.

---

## Compaction is the most concrete failure mode

From the audit + verification: `crates/runtime/src/compact.rs` (808 LOC) is **pure transcript truncation**. The output of `summarize_block()` is a 160-character string. No calls to `MemoryManager`. No `nominations`. No `promote`. No `save`. The conversation gets chopped and the facts in the removed messages are **gone**.

Today: facts in working memory → 160-char string → loss.
Required: facts in working memory → promoted to Episodic + Semantic → safe truncation.

The Claude Code complaint is reproducible in Anvil. **The design doc must specify how compaction becomes layer-aware.**

---

## What the SEVEN-LAYER-MEMORY.md doc must contain (table of contents)

This is the structure the planning session should propose and Jared should approve:

1. **Introduction: the three pillars and where memory sits.** Restate Anvil's thesis. Position the seven-layer model as Pillar 2's implementation.

2. **The seven layers in detail.** One section per layer. For each:
   - Definition (from Jared's spec, verbatim)
   - Storage shape (data structure, persistence, lifecycle)
   - Read interface (how subsystems and the prompt builder query it)
   - Write interface (who writes, what gates apply)
   - Anvil-specific binding (which existing code, if any, maps to this layer)
   - Capacity / retention rules
   - Promotion-up rules (when does data flow to the next layer)
   - Retrieval-down rules (when does the layer get pulled into working memory)

3. **The lifecycle: how facts flow.** Diagram + prose. The promotion graph. The decay rules. The reflective consolidation pass.

4. **The delta protocol.** How the prompt becomes dynamic. Subsystem event format. Journal shape. Per-turn delta assembly. Compaction as promotion.

5. **The retrieval order.** Explicit priority for the model. Layer-priority order. Tool gating. Web search as fallback only.

6. **Reconciliation with the three pillars.**
   - Security: vault sits where, gates apply where, redaction happens where.
   - Persistent memory: this IS the implementation.
   - Extensibility: how plugins/skills/agents declare layer producer/consumer roles.

7. **Reconciliation with existing subsystems.** A table: every subsystem (vault, goals, nominations, routines, file-cache, cmd-cache, skills, hooks, ANVIL.md, QMD, AutoPromoter, MCP) mapped to the layer(s) it produces to and consumes from.

8. **Implementation arc.** Not a feature list — an arc that gets us from 1.5 layers to 7. Phased. Each phase delivers a testable property (e.g., Phase 1 delivers "memory survives compaction"). 2–3 months total per earlier estimate.

9. **Quantization and hardware sidecar.** Where small local models fit. Which layer-internal tasks they handle. How the main model stays cloud/big while housekeeping runs local.

10. **Open questions.** What we DON'T know yet. Where the design leaves room for evolution.

11. **What this design does NOT change.** Three pillars unchanged. Process model unchanged (still process-per-session for v3.0; daemon is a v3.x question). No protocol crate yet. No server pivot.

12. **Testable acceptance criteria.** Concrete tests that prove the layers work:
    - "After /compact, asking the model about a decision from before /compact returns the correct decision."
    - "After a routine runs in the background, the next turn's prompt includes a summary of what happened."
    - "When the user states a fact ('we use PostgreSQL'), the next session sees that fact without the user restating it."
    - etc.

---

## What this design must NOT do

Hard rules for the planning session:

- **Do not propose a server architecture.** That's a separate decision Jared explicitly didn't endorse. Process-per-session stays.
- **Do not propose fine-tuning.** Live in-context learning only.
- **Do not propose a new wire protocol.** The protocol crate is a separate Layer 2 deliverable, not in this doc.
- **Do not propose UI/UX changes.** This is an internal architecture doc.
- **Do not reference external projects by name.** Per `feedback-no-influence-attribution.md` (jcode, hermes, gsd-2, archon, superpowers, context-packet, get-shit-done are all forbidden in user-facing surfaces, but this is an internal doc — still, don't name-drop; describe patterns by what they do, not who does them).
- **Do not propose a complete rewrite.** Existing code (memory.rs, hooks.rs, compact.rs, qmd.rs, auto_promote.rs, routines/) must be mapped INTO the layer model. The doc shows how today's code becomes the layers, not how to throw it out.

---

## Prior research already done

`docs/research/` contains:

- `RESEARCH-SUMMARY.md` — cross-project synthesis from May 11, identifying 8 consensus adoption candidates (mostly routines-shaped, not memory-shaped). Useful for understanding what's *already* in the v2.2.14–16 build plan; the seven-layer doc should reference but not duplicate.
- `COMPARE-vs-archon.md`, `COMPARE-vs-context-packet.md`, `COMPARE-vs-get-shit-done.md`, `COMPARE-vs-gsd-2.md`, `COMPARE-vs-hermes-agent.md`, `COMPARE-vs-superpowers.md` — six deep audits. The planning session can mine these for memory-shaped patterns if needed, but again: describe by behavior, never by source project.

`ROADMAP.md` and `ROUTINES-ADOPTION-NOTES.md` are the active design docs for routines work. The seven-layer doc must reconcile with both — specifically, Reflective Memory (layer 6) is what routines becomes once it's idle-time consolidation, not just cron scheduling.

---

## The "what version of anvil am I" failure — recast

The audit (`COHESION-AUDIT.md`) claimed the symptom was a wiring bug. Verification disproved this:

- The REPL at `crates/anvil-cli/src/main.rs:3921` already calls `build_system_prompt_with_identity(Some(model), provider, None)`.
- The environment block at `crates/runtime/src/prompt.rs:280-327` already emits "You are Anvil v2.2.11" with model + provider.
- **The model has the answer in its prompt and ignores it.**

So the failure is NOT a missing wire. It's a **retrieval-order / policy** failure. The model has no explicit instruction that local context outranks web search. This is exactly what the seven-layer model's **retrieval order** section will fix:

> For questions about self-identity, project state, recent activity, or known files — consult layers in order (Sensory → Working → Episodic → Semantic → Procedural → Long-term) BEFORE any external tool. Web search is layer 8: last resort.

The design doc must make this explicit. The "smallest fix" then becomes: add a `<retrieval-order>` block to the system prompt that enumerates the layer priority. That's the cohesion fix. Not a code change in the prompt assembler — a *content* change.

---

## Handoff checklist for the planning session

When the ultraplan run produces a draft plan for SEVEN-LAYER-MEMORY.md, verify:

- [ ] All seven layers named exactly as Jared specified
- [ ] Three pillars referenced and reconciled
- [ ] Live in-context learning explicit; no training language
- [ ] Compaction-as-promotion is a load-bearing design property
- [ ] Delta protocol for dynamic prompt is specified, not hand-waved
- [ ] Retrieval order is explicit and enumerates layers + web as last
- [ ] Existing code mapped to layers (no greenfield language)
- [ ] No external project names
- [ ] No server pivot
- [ ] No new wire protocol
- [ ] Quantization / local sidecar section present
- [ ] Acceptance criteria are testable
- [ ] Implementation arc is phased, each phase has a testable property
- [ ] "What this does NOT change" section present
