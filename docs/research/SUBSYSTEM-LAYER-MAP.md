# Subsystem → Memory Layer Map

**Status:** Verification table — built independently from the seven-layer doc to cross-check it when it lands.
**Date:** 2026-05-12
**Method:** Every shipped Anvil subsystem mapped to which of the 7 layers it produces to and consumes from. Every cell grounded in a real file path. When the seven-layer doc lands, any cell where this map and that doc disagree is a finding.

---

## The seven layers (Jared's spec, canonical)

1. **Sensory** — Immediate input (text, audio, visual) from environment or user interaction
2. **Working (Short-Term)** — Current context, active tasks, recent actions
3. **Episodic** — Specific, timestamped experiences and past interactions
4. **Semantic** — General knowledge, facts, structured information (often vector-embedded)
5. **Procedural** — How-to actions, successful workflows, tool usage patterns
6. **Reflective (Dreaming/Consolidation)** — Background process: review, consolidate, reorganize
7. **Long-Term Storage** — Permanent storage for consolidated data, preferences, learned patterns

(Long-Term = QMD, per Jared's binding.)

---

## Mapping table

`P` = subsystem **produces** data at this layer (writes here)
`C` = subsystem **consumes** data from this layer (reads here)
`PC` = both
`-` = no relationship
**bold** = currently working; *italic* = currently broken or partial; ~~strike~~ = exists but unwired/dead

| # | Subsystem | File path(s) | L1 Sensory | L2 Working | L3 Episodic | L4 Semantic | L5 Procedural | L6 Reflective | L7 Long-term |
|---|---|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| 1 | **Memory tiers (User/Feedback/Project/Reference)** | `runtime/src/memory.rs` | - | **C** | - | ***PC*** | - | - | - |
| 2 | **ANVIL.md / instruction files** | `runtime/src/prompt.rs::discover_instruction_files` | - | **C** | - | **PC** | - | - | - |
| 3 | **Private memory** | `runtime/src/private_memory.rs` | - | **C** | - | **PC** | - | - | - |
| 4 | **Vault (secrets)** | `runtime/src/vault/{mod,storage,crypto,scan}.rs` | *P* (scan filters) | - | - | ***P*** | - | - | - |
| 5 | **File fingerprint cache** | `runtime/src/file_cache.rs` | - | ***PC*** | - | - | - | - | - |
| 6 | **Command-output cache** | `runtime/src/command_cache.rs` | - | ***PC*** | - | - | - | - | - |
| 7 | **Nominations / AutoPromoter** | `runtime/src/nominations.rs`, `auto_promote.rs` | - | **C** | - | *P* (suggest-not-auto) | - | **P** (the consolidation impulse) | - |
| 8 | **Daily memory** | `runtime/src/daily.rs` | - | - | ***P*** | - | - | - | - |
| 9 | **Goals** | `runtime/src/goals.rs` | - | - | - | **PC** | - | - | - |
| 10 | **QMD integration** | `runtime/src/qmd.rs` | - | ~~C~~ (only when `with_qmd_context()` called) | - | - | - | - | **PC** |
| 11 | **Skills (bundled + user)** | `crates/commands/bundled/skills/`, `commands/src/agents.rs` | - | **C** | - | - | ***PC*** | - | - |
| 12 | **Skill chaining engine** | `commands/src/skill_chaining.rs` | - | - | - | - | **C** | - | - |
| 13 | **Routines (scheduled agents)** | `runtime/src/routines/{mod,schedule,archive,packet}.rs` | - | - | *P* (archive entries) | - | - | *P* (the future home of reflection) | - |
| 14 | **Hooks (session lifecycle)** | `runtime/src/hooks.rs` | ***P*** (events) | - | - | - | - | - | - |
| 15 | **Permission system + reviewer** | `runtime/src/permissions/{mod,reviewer}.rs` | **C** (gates on tool inputs) | - | - | - | - | - | - |
| 16 | **Permission memory (decisions)** | `runtime/src/permission_memory.rs` | ~~P~~ | ~~C~~ | ~~P~~ | - | - | - | - |
| 17 | **Auto-mode hard-deny** | `runtime/src/auto_mode.rs` | **C** | - | - | - | - | - | - |
| 18 | **Content filter (secret redaction)** | `runtime/src/content_filter.rs` | **P** (filters sensory) | **P** (filters working) | - | - | - | - | - |
| 19 | **Conversation history** | `runtime/src/history.rs`, `session.rs` | - | ***PC*** | - | - | - | - | - |
| 20 | **Compaction** | `runtime/src/compact.rs` | - | ***PC*** (destroys, doesn't promote) | ✗ should P | ✗ should P | - | - | - |
| 21 | **Audit log** | `runtime/src/audit.rs` | - | - | **P** | - | - | - | - |
| 22 | **Hub / marketplace** | `runtime/src/hub.rs` | - | - | - | - | **PC** (skills + plugins) | - | - |
| 23 | **MCP client (consuming servers)** | `runtime/src/mcp_client.rs`, `mcp_stdio/` | - | **PC** | - | - | **C** (external tools) | - | - |
| 24 | **MCP server mode (Anvil exports tools)** | `anvil-cli/src/mcp_server_mode.rs` | - | - | - | - | **P** | - | - |
| 25 | **LSP integration** | `crates/lsp/` | - | **P** (diagnostics into prompt) | - | - | - | - | - |
| 26 | **Effort / reasoning slider** | `runtime/src/effort.rs` | - | **PC** (per-turn config) | - | - | - | - | - |
| 27 | **Output styles** | `runtime/src/config/output_style.rs` | - | **C** | - | - | **PC** | - | - |
| 28 | **Profiles** | `runtime/src/config/profile.rs` | - | **C** | - | - | - | - | - |
| 29 | **OAuth / provider auth** | `runtime/src/oauth.rs` | - | **C** | - | **P** (token metadata) | - | - | - |
| 30 | **Usage / cost tracking** | `runtime/src/usage.rs` (or per `crates/runtime/src/conversation/usage_tracking.rs`) | - | **P** | **P** (token-spend over time) | - | - | - | - |

---

## Per-layer roster (rotated view)

### L1 Sensory — current producers/consumers

**Producers:** Hooks (events from tools, file changes, permission events); Content filter (strips secrets before working memory); Auto-mode hard-deny (blocks risky tool inputs at this boundary).
**Consumers:** Permission system (gates on tool inputs); Auto-mode hard-deny.
**Gap:** Hooks are observe-only today — they fire but the resulting events don't flow anywhere durable. **Sensory has no journal.** The seven-layer doc's delta-protocol section needs to specify this.

### L2 Working — current producers/consumers

**Producers:** File-cache, Command-cache (cache hits feed working state); Compaction (destructively rewrites this layer); History; LSP (diagnostics); Effort slider; OAuth (current account state); Output styles (per-turn config); Profiles; MCP client.
**Consumers:** Almost everything. This is the layer that reaches the prompt today.
**Gap:** This layer is healthy as a *cache* but unhealthy as a *promotion source* — facts decay here without being promoted to L3/L4 before compaction.

### L3 Episodic — current producers/consumers

**Producers:** Daily memory; Audit log; Usage tracking; Routines archive (when it runs).
**Consumers:** None today. Nothing reads Episodic into the prompt.
**Gap:** **This layer has no reader.** Daily writes; audit writes; routines write; nothing surfaces those writes to the model. This is the biggest single defect surfaced by the map.

### L4 Semantic — current producers/consumers

**Producers:** Memory tiers, ANVIL.md, Private memory, Vault, Nominations (suggest), Goals, OAuth (token metadata).
**Consumers:** Memory tiers (re-read), ANVIL.md (re-read), Private memory (re-read), Goals (read for active-goal prompt fragment).
**Gap:** Vault and Nominations don't reach the prompt today (per cohesion audit). Vault is C-only via tools; Nominations is P-only with no reader. **Fixing this is most of the "make subsystems compose" work.**

### L5 Procedural — current producers/consumers

**Producers:** Skills, Skill chaining, Hub/marketplace, MCP server mode (exports tools), Output styles.
**Consumers:** Skills (load), Skill chaining (resolve), Hub (browse), MCP client (consume external).
**Gap:** Skill chaining and AutoPromoter don't share data. Skills are hand-written, not learned from observed patterns. **No write-path from Working/Sensory observations into Procedural.**

### L6 Reflective — current producers/consumers

**Producers:** Nominations (suggest-not-auto is the impulse toward consolidation); Routines (when it becomes idle-time consolidation).
**Consumers:** None today.
**Gap:** **This layer does not exist as a runtime concept.** Nominations is the closest impulse; routines/ is the structural foundation. **Building this layer is the v3.0 arc.**

### L7 Long-Term (QMD) — current producers/consumers

**Producers:** None inside Anvil (QMD is fed by external `qmd` CLI / scanner).
**Consumers:** QMD integration (conditional on `with_qmd_context()` being called).
**Gap:** **No write path from Anvil to QMD.** Working memory facts can flow into L4 Semantic (memory tiers) but never reach L7. The Reflective layer (L6) is what should write to L7 — confirmed by the seven-layer spec, currently unbuilt.

---

## The 11 defects this map implies

Cross-checking the cells, here are the gaps to verify against the seven-layer doc:

1. **L1 has no journal.** Hooks fire and the events vanish.
2. **L3 has no reader.** Daily/audit/usage/routine-archive write but nothing surfaces to the prompt.
3. **L4 vault never reaches prompt.** Vault entries exist but the model can't see the inventory.
4. **L4 nominations never reach prompt.** Pending nominations exist but the model doesn't know.
5. **L4 goals reach prompt** (via `build_active_goal_prompt_fragment`) — this one works.
6. **L5 has no auto-write path.** AutoPromoter observes patterns; nothing promotes them to skills.
7. **L6 doesn't exist.** No background consolidation runs.
8. **L7 is read-only.** Anvil can query QMD but can't promote anything *into* QMD.
9. **Compaction destroys L2 instead of promoting L2→L3/L4.** Confirmed: `compact.rs` is 808 lines of pure truncation, no `MemoryManager` calls.
10. **`permission_memory.rs` is dead-wired.** Disk store exists, runtime gate doesn't consult it. (Listed in audit's `~~strike~~` row.)
11. **QMD context is conditional.** `with_qmd_context()` must be explicitly called by the caller; default path doesn't.

The seven-layer doc landing should address (1)–(9). (10) and (11) are wiring bugs surfaceable as part of the same arc.

---

## Cross-check protocol when the seven-layer doc lands

For each of the 8 files committed in `7d312a7`:

1. Open the layer doc (`MEMORY-LAYER-N-*.md`).
2. Find the "current state in Anvil" or equivalent section.
3. Compare against this map's per-layer roster.
4. **Disagreements are findings.** Either this map missed a subsystem, the doc's mapping differs, or we have ambiguous shared territory between layers.

Particular cells to scrutinize:

- **Vault placement.** This map puts vault in L4 Semantic (with security gates). The doc might place it elsewhere (its own layer, a sub-layer of Sensory, or a cross-cutting concern). Either is defensible; we need one decision.
- **Routines placement.** This map puts routines as the future L6 home. The doc might split routines: schedule grammar = L6 trigger, packet schema = L4 fact, archive = L3 episode. That's actually cleaner.
- **Compaction's correct behavior.** This map asserts compaction *should* promote to L3+L4. The doc should specify which facts go to which.
- **Content filter scope.** This map places content_filter as L1+L2 producer (filters at both boundaries). The doc should confirm.
- **Routines layering.** Per Jared's "routines daemon → server-side" trajectory, L6 Reflective is the natural home for the daemon. Confirm the doc agrees.

---

## What this map does NOT capture

Honesty about scope:

- **Cross-layer flows.** This map shows producer/consumer per layer but not the *flow rules* (promotion conditions, retrieval triggers, decay schedules). That's the seven-layer doc's job.
- **Capacity / retention.** No size limits, eviction policies, or time-to-live values here. Per-layer doc territory.
- **Test coverage.** No assertion of which mappings have tests vs. which are unverified runtime behavior.
- **The model's behavior.** Whether the model actually *uses* the layers correctly — see RETRIEVAL-ORDER-DRAFT.md for that side.
- **Subsystems we don't have yet.** Episodic readers, Procedural writers, Reflective runner — these are the missing pieces. They're listed as gaps, not subsystems.

---

## Bottom-line read

Of 30 subsystems mapped:

- **L1 Sensory:** 4 producers, 2 consumers — wired at the input boundary but no journal
- **L2 Working:** 12 producer/consumers — healthy as cache, unhealthy as promotion source
- **L3 Episodic:** 4 producers, 0 consumers — write-only graveyard
- **L4 Semantic:** 7 producers, 4 consumers — most cohesion work lives here
- **L5 Procedural:** 5 producers, 4 consumers — exists but no auto-learning path
- **L6 Reflective:** 1+1 future producer, 0 consumers — **does not exist**
- **L7 Long-term:** 0 producers, 1 consumer — read-only QMD, no write path

The "everything is connected" story Anvil claims is true *within layers* and false *between layers*. Subsystems compose with peers (memory tier + ANVIL.md + private memory all live in L4 and read each other) but **promotion flows L1→L2→L3→L4→L5→L7 via L6 are mostly absent.** L3 and L6 are the two missing layers; L7 is read-only because L6 is missing.

When the seven-layer doc lands, the test is whether its implementation arc rebuilds those three: L3 episodic reader, L6 reflective runner, L7 write path. Everything else is wiring on top.
