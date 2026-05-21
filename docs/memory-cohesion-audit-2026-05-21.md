# Anvil 7-Layer Memory Architecture Audit
**Date:** 2026-05-21  
**Scope:** v2.2.19 (HEAD) / Cargo workspace version 2.2.13  
**Methodology:** Code inspection + prior-session memory synthesis

---

## Layer 1 — Sensory

**Spec intent:** Captures immediate input from environment or user interaction. Named but unnamed in code.

**Current implementation:**  
`crates/runtime/src/conversation/mod.rs` (line 1) houses the entry point; hook dispatch at `crates/runtime/src/hooks.rs` (line 1+); `crates/commands/src/handlers.rs` exposes `/event` / observable hooks. Environment snapshot at `crates/runtime/src/prompt.rs:280-327`.

**What's wired:**  
- Hooks fire on session start (`run_session_start_hooks`), message arrival, state changes
- Observer pattern works (hooks emit events, TUI displays)
- Environment block injected with version, model, cwd, date
- Hardcoded retrieval-order block injected at `crates/runtime/src/prompt.rs:495-552`

**What's stubbed:**  
- No named type `SensoryMemory` or `SensoryLayer` in code
- `/event show` or `/event list` do not exist; hooks are fire-and-forget
- No sensor registry; no machine-facing event schema

**What's missing:**  
- Formal event schema (types, provenance fields)
- Persistence (sensory events disappear on session end)
- Deduplication of repeated sensor inputs
- Cost tracking per input

**Verdict:** YELLOW — Pattern works but untyped and anonymous.

**Smallest improvement:** Introduce `struct SensoryEvent { kind, timestamp, body, cost_estimate }` and a per-session `Vec<SensoryEvent>` that survives into episodic snapshot.

---

## Layer 2 — Working Memory

**Spec intent:** Holds current context, active tasks, recent actions. System-prompt blocks + message buffer assembled for *this* turn.

**Current implementation:**  
`crates/runtime/src/prompt.rs:101-273` (SystemPromptBuilder); `crates/runtime/src/conversation/mod.rs:300-340` (snapshot); `crates/runtime/src/compact.rs:10-32` (CompactionConfig); `crates/runtime/src/prompt_section.rs` (PromptSection + WorkingMemorySnapshot).

**What's wired:**  
- `WorkingMemorySnapshot { sections: Vec<PromptSection>, generated_at: u64 }` typed at `crates/runtime/src/prompt_section.rs:200+`
- PromptSection tagged with 21 kinds (Intro, OutputStyle, RetrievalOrder, Qmd, etc.) at `crates/runtime/src/prompt_section.rs:36-93`
- Retrieval-order block hardcoded at `crates/runtime/src/prompt.rs:495-552` (Bucket 0: static, 1–7: Sensory→Working→Episodic→Semantic→Procedural→Long-term→Web)
- Compaction preserves 6 recent messages by default (`compact.rs:27`)
- Boundary marker at `crates/runtime/src/prompt.rs:45` (`SYSTEM_PROMPT_DYNAMIC_BOUNDARY`)

**What's stubbed:**  
- `/memory why` at `crates/commands/src/handlers.rs:899-914` is hand-written static text, not live introspection
- Four `insert(0, ...)` sites (`anvil-cli/src/main.rs:5466, 5672, 5790, 6754`) stomp order; no enforcement
- Promotion/demotion between working ↔ episodic has no code

**What's missing:**  
- Live section inventory (user can't query "what's in my prompt right now?")
- Layer-aware promotion logic (Working facts do not auto-lift to Episodic)
- Cost/size tracking per section kind
- Conflict detection (duplicate sections)

**Verdict:** YELLOW — Typed structure exists; injection chain works; but promotion and introspection are stubs.

**Smallest improvement:** Wire `working_memory_snapshot()` + `PromptSectionsExt` into a live `/memory layer 1` handler that renders the actual snapshot, replacing hardcoded text at `handlers.rs:899-914`.

---

## Layer 3 — Episodic Memory

**Spec intent:** Per-session events, timestamped experiences. Replayable.

**Current implementation:**  
Three independent stores:  
1. Daily summaries: `crates/runtime/src/daily.rs:18-51` (DailyStore at `~/.anvil/daily/YYYY-MM-DD.json`)
2. History archive: `crates/runtime/src/history.rs:33-82` + usage at `crates/runtime/src/compact.rs:31-54` (at `~/.anvil/history/<session>-<ts>.md`, append-only)
3. Workspace sessions: no exposed code; lives at `<cwd>/.anvil/sessions/<id>.json` in `Session` serialization

**What's wired:**  
- `DailyStore::new()` creates/loads daily record at session exit
- `DailySummary` typed at `crates/runtime/src/daily.rs:22-34` (sessions, open_items, token counts)
- History written on compaction at `crates/runtime/src/compact.rs:31-54` (format_and_write_history)
- Daily injection into prompt at `crates/runtime/src/prompt.rs` (PromptSectionKind::DailySummary, line 81)

**What's stubbed:**  
- `/memory show episodic` does not exist (no handler for it)
- History directory invisible to `/memory budget` + `/memory prune`
- No lifecycle policy (90-day retention mentioned in spec but not coded)
- Daily summaries not queried for promotion to Semantic

**What's missing:**  
- Unified episodic tier in `/memory` (three stores are de facto invisible)
- Retention/pruning with dry-run mode
- Cross-session deduplication ("we tried this yesterday")
- Provenance linking (episodic entry → which tool/agent created it)

**Verdict:** RED — Structure exists; wiring to prompt works; but tier is invisible and no lifecycle.

**Smallest improvement:** Introduce `/memory show episodic` handler that enumerates daily + history + sessions; add `--dry-run` to `/memory prune episodic` with 90-day TTL.

---

## Layer 4 — Semantic Memory

**Spec intent:** Durable plaintext knowledge about the project. Hand-curated, AI-suggested.

**Current implementation:**  
`crates/runtime/src/memory.rs:54-150` (MemoryManager at `~/.anvil/projects/<hash>/memory/*.md`); nominations pipeline at `crates/runtime/src/nominations.rs` (staging area); ANVIL.md discovery at `crates/runtime/src/prompt.rs:335-356`.

**What's wired:**  
- `/memory show anvil-md` handler at `crates/commands/src/handlers.rs:717-751` renders ANVIL.md files
- MemoryManager loads user-created `.md` files from per-project storage
- `/memory inspect <id>` fetches nomination by ID (`handlers.rs:776-812`)
- Nominations appear in prompt when `nominations_generated > 0` (tracked in DailySummary)

**What's stubbed:**  
- `/memory promote <id>` at `handlers.rs:833-845` **flips status field only**; never calls `MemoryManager::save`
- `MemoryManager::save / append_to_file` at `memory.rs:131-150` has **zero production callers** (tests only)
- No chain from nomination.accept → ANVIL.md append
- No frontmatter (nominated_from, provenance, timestamp)

**What's missing:**  
- Actual promotion from nominations → Semantic layer (nominations are stuck in limbo)
- QMD indexing integration (semantic facts should feed QMD corpus)
- Auto-dedup (suggested facts already in ANVIL.md)
- User review/approval flow before auto-promotion

**Verdict:** RED — Pipeline designed but terminal step broken; nominations never reach persistent storage.

**Smallest improvement:** Fix `/memory promote` to chain `get(id)` → `mark_accepted()` → `MemoryManager::append()` with minimal frontmatter (nominated_at, source).

---

## Layer 5 — Procedural Memory

**Spec intent:** How-to memory; programs that act. Goals, skills, routines.

**Current implementation:**  
`crates/runtime/src/goals.rs` (GoalManager); `crates/runtime/src/agents.rs:724-766` (bundled skills via include_str!); on-disk skills at `.codex/skills` / `.anvil/skills`; routines/cron at `crates/runtime/src/cron.rs` + `crates/runtime/src/routines/`.

**What's wired:**  
- Goals injected into prompt at `crates/runtime/src/prompt.rs` (PromptSectionKind::Goal)
- `/goal set <text>` handler at `crates/commands/src/handlers.rs:954-966` persists goal to session + file
- Skills loaded + injected at prompt build time (`prompt.rs:500+`)
- `/skill load <name>` command exists (dispatched in `commands/dispatch.rs`)

**What's stubbed:**  
- `/memory show procedural` does not exist (goals are in `/memory`, skills invisible)
- Bundled skill bodies hardcoded; no dynamic discovery (`agents.rs:724-766`)
- Cron storage (`~/.anvil/cron.json`) readable at runtime but no `/cron` user command
- Routines TOML loader is a stub (`routines/parse.rs`; `load_routines()` not called from runtime)
- No deduplication between bundled + on-disk skills

**What's missing:**  
- Unified procedural tier in `/memory` (Goals visible, Skills+Cron+Routines invisible)
- Skill execution tracking (which skills were invoked, success rate)
- Routine trigger evaluation (should-run-now? logic)
- Cost attribution (which skill consumed tokens)

**Verdict:** YELLOW — Goals work; skills load; but tier fragmentation + visibility gap.

**Smallest improvement:** Introduce `/memory show procedural` that enumerates goals + load_skills_from_roots() + BUNDLED_SKILL_BODIES + CronManager state; stub routines as "coming in v3.x".

---

## Layer 6 — Reflective Memory

**Spec intent:** Background consolidation, reviewing, dreaming during inactivity.

**Current implementation:**  
`crates/runtime/src/reflection.rs` (TurnState, StuckDetectorConfig, stuck-detector rolling window); reflection state threaded into `ConversationRuntime` at `conversation/mod.rs:214`.

**What's wired:**  
- Stuck detector maintains a 20-event rolling window (task #636, `reflection.rs`)
- Scratchpad per turn (reflection state carries pattern, count, window)
- `/reflect` command exists (dispatched)
- TurnState exposed as read-only via `reflection()` + mutable via `reflection_mut()`

**What's stubbed:**  
- `/reflect` handler does not run consolidation (does not inspect working → episodic promotion)
- No background task/daemon (no idle-time reflection)
- No analysis output (no "here's what we've learned" summary)
- Stuck-detector window is in-memory only; does not persist across sessions

**What's missing:**  
- Consolidation logic (working facts → episodic recap)
- Learned-pattern capture (procedural anti-patterns identified)
- Daemon/routine integration (reflection should run on session boundary)
- User-facing insights ("we tried X 3 times; Y worked best")

**Verdict:** RED — Rolling window exists; but no consolidation logic, no persistence, no actionable output.

**Smallest improvement:** Implement consolidation in `/reflect` handler: iterate stuck-detector window + snapshot → generate episodic recap + learned pattern → append to daily summary with "reflection" tag.

---

## Layer 7 — Long-Term / Cache

**Spec intent:** Recomputable performance state (safe to nuke) + durable long-term storage (QMD).

**Current implementation:**  
`crates/runtime/src/file_cache.rs:121+` (sha-prefix sharded at `~/.anvil/projects/<hash>/file-cache/<prefix>/<sha>.json`); `crates/runtime/src/command_cache.rs:562+` (cmd-hash keyed at `~/.anvil/projects/<hash>/cmd-cache/<cmd-hash>.json`); `crates/runtime/src/qmd.rs:65-98` (QmdClient wrapping external `qmd` CLI binary).

**What's wired:**  
- FileCacheManager instantiated at query time; sha-prefix keying ensures < 100 files per dir
- CommandCacheManager has TTL per class (60–1800s, `command_cache.rs:580+`)
- QMD client auto-detects `qmd` binary; search results cached per-session
- QMD injection into prompt at `crates/runtime/src/prompt.rs:515+` (PromptSectionKind::Qmd)
- `/cmd-cache list|stats|prune|forget` handler at `handlers.rs:1026+`

**What's stubbed:**  
- `/file-cache` command is deprecation banner only (`handlers.rs:555-559`); handler is no-op
- Path mismatch: `/memory budget` checks `~/.anvil/file-cache/` but real path is `~/.anvil/projects/<hash>/file-cache/` → counts always zero
- `/memory prune` ignores L7 entirely (`handlers.rs:685-714` skips cache)
- QMD query integration is manual (prompt.rs:515+ calls QmdClient, but no auto-query on every turn)
- No size cap / LRU eviction on FileCacheManager

**What's missing:**  
- Unified cache tier in `/memory` (file-cache + cmd-cache + QMD as sub-views)
- Correct path discovery for budget calculation
- Pruning with retention policy + dry-run
- L5 security invariant enforcement (cache must not contain decrypted vault)
- Auto-query trigger (should QMD search fire on *every* user message, or on-demand only?)

**Verdict:** YELLOW — Cmd-cache works; QMD wired; file-cache has path bug; tier hidden; no auto-query policy.

**Smallest improvement:** Fix path bug at `memory_budget` calculation; wire `/memory show cache` to enumerate file|cmd|qmd stats; add `/memory prune cache --dry-run` chaining prune calls.

---

## Cross-Layer Cohesion Analysis

### Compaction Policy

**File:** `crates/runtime/src/compact.rs:162-198`  
**Finding:** Compaction **truncates, not promotes**. Algorithm:
1. Keep last N messages verbatim (`preserve_recent_messages`, default 6)
2. Summarize older messages with `summarize_messages()` (LLM call, ~160 chars)
3. Discard original messages

**Gap:** No layer-aware promotion. Working facts (system prompt sections) are not extracted and elevated to Episodic before truncation. The summary is plaintext, not structured nominations.

**Evidence:**  
- `compact_session_force` at line 145: `do_compact_session` → removed messages → LLM summarize → discard
- `summarize_messages` is opaque (calls provider, returns string)
- No call to `nominations.suggest()` or `NominationStore::emit()` on summary

**Verdict:** Compaction violates the "promote before truncate" design constraint.

### Retrieval-Order Prompt Block

**File:** `crates/runtime/src/prompt.rs:495-552`  
**Finding:** Block is **hardcoded static text**, not data-driven.

Order (lines 496-513):
1. Environment block (version, model, cwd, date)
2. Loaded instruction files (ANVIL.md)
3. Known-files cache (W11 fingerprints)
4. Project memory (MEMORY.md)
5. Project tree (read_file, glob_search)
6. QMD knowledge base
7. Web search (last resort)

**Evidence:**  
- 23-line hardcoded string, no loop over layer registry
- No tagged section kind per layer (bucket 0 is RetrievalOrder; buckets 1–7 are inferred by position)
- Not injectable; order cannot be customized per-project

**Verdict:** Order is correct per spec; but not machine-readable and not layer-aware.

### Promotion Engine

**Finding:** No promotion engine exists in code.

**Wiring attempt:**
- `nominations.rs`: `NominationStore::get / put / list` work
- `memory.rs`: `MemoryManager::save / append_to_file` exist but unused
- `reflection.rs`: `TurnState` exists but does not emit promotions
- `auto_promote.rs`: **Does not exist** (grep returns nothing)

**Result:** Facts flow into nominations (Sensory → Semantic staging); but nominations never reach Episodic (L2) or durable Semantic (L3).

### QMD as Layer 7

**File:** `crates/runtime/src/qmd.rs:70-160`; injection at `crates/runtime/src/prompt.rs:515-530`

**Finding:** QMD is **partially wired**. Client works; injection works; but trigger policy is undefined.

**Evidence:**  
- `QmdClient::new()` auto-detects binary (lines 85–91)
- `QmdClient::search()` queries + caches (lines 100–120)
- `SystemPromptBuilder::with_qmd_context()` injects results (lines 515–530)
- **No auto-trigger:** User must explicitly call or prompt-builder caller must decide when to query

**Current behavior:** QMD results appear in prompt only when:
1. Caller explicitly invokes `with_qmd_context(qmd, message)` (turn executor does this)
2. AND the user's message is non-empty (used as query)

**Verdict:** QMD is live but manual; not on the critical path; and not in `/memory` visibility.

---

## Sub-task Design Proposal

### I.2 — Working Memory Introspection (Layer 1)
**Files to touch:** `handlers.rs`, `prompt_section.rs`  
**Difficulty:** S  
**Implementation:** Wire `working_memory_snapshot()` + `PromptSectionsExt::iter_by_kind()` into new handler `/memory layer 1` that renders live section inventory (kinds, bodies, sizes).  
**Test:** Assert `/memory layer 1` output includes all injected sections in order; assert size ≈ sum of bodies.  
**Dependency:** None.

### I.3 — Episodic Tier Exposure (Layer 2)
**Files to touch:** `handlers.rs`, `daily.rs`, `history.rs`  
**Difficulty:** M  
**Implementation:** New handler `/memory show episodic` enumerates daily + history + sessions. Add `/memory prune episodic --dry-run` with configurable TTL (default 90 days); add trash-bin for safety.  
**Test:** Create 3 daily summaries, 1 old and 2 new; `prune --dry-run` lists old; `prune --confirm` removes it; verify trash has one entry.  
**Dependency:** I.2 (for consistency with other `/memory layer N` naming).

### I.4 — Semantic Promotion Fix (Layer 3)
**Files to touch:** `nominations.rs`, `memory.rs`, `handlers.rs`  
**Difficulty:** M  
**Implementation:** Replace `/memory promote` handler stub with full chain: `NominationStore::get(id)` → extract fact + parse query keyword → `MemoryManager::append()` to ANVIL.md → `NominationStore::mark_accepted(id)` → emit provisional frontmatter (nominated_at, source).  
**Test:** Create nomination, promote, verify ANVIL.md updated with frontmatter, nomination status changes to accepted.  
**Dependency:** I.2 (for feature consistency).

### I.5 — Procedural Tier Consolidation (Layer 4)
**Files to touch:** `handlers.rs`, `agents.rs`, `cron.rs`  
**Difficulty:** M  
**Implementation:** New handler `/memory show procedural` that composes GoalManager::list() + load_skills_from_roots(discover_skill_roots(cwd)) + BUNDLED_SKILL_BODIES iterator + CronManager state. Stub routines sub-view as "Coming v3.x".  
**Test:** Create goal, load on-disk skill, bundle skill loaded, cron entry exists; `/memory show procedural` lists all four.  
**Dependency:** I.3 (episodic baseline).

### I.6 — Reflective Consolidation (Layer 6)
**Files to touch:** `reflection.rs`, `handlers.rs`, `daily.rs`  
**Difficulty:** L  
**Implementation:** Implement `/reflect` handler to consume stuck-detector window + derive episodic recap + identify procedural patterns (repeated failures, successes) + append to daily with "reflection" tag. Stub daemon trigger as future work.  
**Test:** Run 5 turns with same failure pattern; `/reflect` generates recap; daily summary tagged "reflection" contains summary.  
**Dependency:** I.4 + I.5 (for pattern analysis context).

---

## Critical Path Summary

**Must-do before v2.2.19 ships:**
1. ✅ Retrieval-order block (already wired at `prompt.rs:495-552`)
2. ❌ I.4: Semantic promotion (nominations stuck)
3. ❌ Cache tier path fix (counts wrong)

**Highest-leverage quick wins (1–2 hour each):**
- I.2: Live Working layer introspection
- Cache tier path fix + `/memory show cache`
- I.3: Episodic tier visibility

**Blockers for v2.2.20+:**
- Compaction must promote, not truncate
- Reflective consolidation (requires daemon model decision)

---

**Generated:** 2026-05-21  
**Audit methodology:** Direct code inspection + prior-session synthesis (memory at `~/.claude/projects/.../project-anvil-seven-layer-memory.md`, age 8 days).
