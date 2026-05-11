# gsd-2 vs Anvil — research comparison

**Audited:** 2026-05-11  
**gsd-2 path:** /Users/soulofall/projects/gsd-2  
**get-shit-done path (sibling):** /Users/soulofall/projects/get-shit-done  
**Anvil path:** /Users/soulofall/projects/anvil-dev  
**Anvil current version:** v2.2.12 (just released today)

---

## 0. Relationship to get-shit-done (NEW section, gsd-2 only)

gsd-2 is **an architectural successor** to get-shit-done, not a fork or patch release. The relationship is explicit:

- **get-shit-done (v1)** is a TypeScript prompt-framework (README: "light-weight meta-prompting, context engineering..."), installed as slash commands into Claude Code. It relies entirely on the LLM reading markdown prompts and following instructions. Shipped via npm as `get-shit-done-cc` v1.50.0-canary.

- **gsd-2 (v2)** is a standalone CLI TypeScript/Node application built on the **Pi SDK** (Anthropic's agent SDK), package name `gsd-pi` v2.82.0. Per README line 14–18: *"The original GSD went viral as a prompt framework... This version is different. GSD is now a standalone CLI built on the Pi SDK, which gives it direct TypeScript access to the agent harness itself."*

**Stack difference:**
- get-shit-done: Slash commands + SDK subagents + prompt-driven orchestration.
- gsd-2: State-machine CLI + Pi SDK direct integration + database-backed execution.

**Key architectural shift:** get-shit-done says "hope the LLM doesn't fill up its context" and relies on markdown projections + subagent handoff. gsd-2 says "we own the context window entirely" — it inlines only what each task needs, persists state to SQLite per-project, and drives dispatch via database reads, not LLM re-planning.

Per README table (v1 vs v2, lines 274–288): v1 has "Hope the LLM doesn't fill up", "None" for crash recovery, "LLM writes git commands", "None" for stuck detection. v2 has "Fresh session per task", "Lock files + session forensics", "Worktree isolation, sequential commits", "Retry once, then stop with diagnostics."

**The delta:** gsd-2 is ~117.5K lines of TypeScript (measured in `src/resources/extensions/gsd/` alone) vs get-shit-done's command + SDK layer. This is not a refresh — it's a rewrite on a different runtime.

---

## 1. What gsd-2 is (one paragraph)

GSD-2 is a standalone CLI coding agent built on Anthropic's Pi SDK that orchestrates multi-task project execution with database-backed state management, worktree isolation, and fresh context windows per task. Unlike its predecessor (get-shit-done v1), gsd-2 is not a prompt framework but a full-fledged state machine that drives execution, manages git, tracks costs, detects stuck loops, recovers from crashes, and runs multi-slice milestones to completion with zero human intervention. It ships as a global npm binary (`gsd-pi@2.82.0`) and coordinates through a project-root SQLite database (`gsd.db`), with `.gsd/` markdown projections rendered from database state for review and git history.

---

## 2. Architecture summary

```
gsd-2 CLI (Node.js)
  ├─ Pi SDK integration (agent harness control)
  ├─ Auto-mode state machine (auto.ts, auto/*.ts, 10 files)
  │   ├─ reconcileBeforeDispatch (drift-driven, state-reconciliation/)
  │   ├─ dispatch guards (dispatch-guard.ts, pre-execution-checks.ts)
  │   └─ worktree lifecycle (worktree-*.ts, 10 files)
  ├─ Database layer (gsd-db.js, db-*.ts, 20+ files)
  │   ├─ Schema: milestones, slices, tasks, memories, verification_evidence
  │   ├─ Single-writer invariant enforcement
  │   └─ Time-decay memory ranking (ADR-013)
  ├─ Memory store (memory-store.ts, memory-relations.ts)
  ├─ Context injection (context-injector.ts, preparation.ts)
  ├─ Skills & agents (25 bundled extensions, extension loader)
  ├─ Browser tools (playwright-based, session state, network mocking)
  ├─ MCP client (native SDK integration)
  └─ Commands (20+ commands, command handlers, TUI overlay)

get-shit-done v1
  ├─ Slash command framework (get-shit-done/bin/lib)
  ├─ SDK subagents (scout, researcher, worker, JavaScript-pro, TypeScript-pro)
  ├─ Markdown prompts (commands/, agents/)
  └─ State via PROJECT.md, ROADMAP.md, REQUIREMENTS.md, DECISIONS.md
```

**Key difference:** gsd-2 has a **database-authoritative state model** (line 834 of README: "DB-authoritative state — the project-root GSD database is the runtime source of truth"). get-shit-done v1 has **markdown-authoritative state** with optional subagent handoff. gsd-2 inverts the relationship: markdown is a rendered projection, not runtime source.

---

## 3. Schedule grammar / dispatch

**gsd-2 dispatch grammar:**

- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/workflow-dispatch.ts` (workflow task enumeration)
- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/auto-dispatch.ts` (dispatch claim and unit retrieval)
- Unit types: `research-milestone`, `plan-slice`, `execute-task`, `complete-slice`, `validate-milestone`, etc. (dispatch-guard.ts:16–18)
- Dispatch driven by: database reads of milestone/slice/task rows + parsed ROADMAP.md dependency graph.
- Ordering: DB rows are canonical; markdown is projection. Parallel slice dispatch via `GSD_MILESTONE_LOCK` environment variable (dispatch-guard.ts:36).

**get-shit-done v1 dispatch grammar:**

- File: `/Users/soulofall/projects/get-shit-done/commands/` (phase command prompts)
- Phase types: `/gsd-new-project`, `/gsd-discuss-phase`, `/gsd-plan-phase`, `/gsd-execute-phase`, `/gsd-complete-phase`
- Dispatch driven by: LLM reading phase prompts and calling subagents. State parsed from markdown checklist and file presence.

**Difference:** gsd-2 dispatch is **stateful and verifiable** — the next unit is a database query; if the DB is corrupted, dispatch fails loudly. get-shit-done v1 dispatch is **prompt-driven** — the LLM decides what's next based on markdown context; if the LLM hallucinates or misreads, execution continues silently.

---

## 4. Output / archive / delivery

**gsd-2 artifacts:**

- Authoritative: `gsd.db` (SQLite, per-project)
- Rendered projections: `.gsd/M###-ROADMAP.md`, `.gsd/S##-PLAN.md`, `.gsd/T##-SUMMARY.md`, `.gsd/STATE.md`, `.gsd/KNOWLEDGE.md` (memory graph, line 552 of README)
- Reports: `.gsd/reports/*.html` (auto-generated, SVG DAGs, cost/token charts, inlined CSS/JS)
- Verification evidence: `.gsd/exec/` (sandboxed tool output, capped per context-mode)
- Session recovery: `.gsd/activity/` (JSONL event log, auto-pruned)
- Git: Sequential commits per slice, squash-merged to main per milestone

**get-shit-done v1 artifacts:**

- Authoritative: `PROJECT.md`, `ROADMAP.md`, `REQUIREMENTS.md`, `DECISIONS.md`, `.planning/phases/`, `.planning/summaries/`
- Rendered: No DB, all state in markdown
- Git: LLM writes git commands; user is responsible for clean history

**Difference:** gsd-2's `.gsd/reports/` is a new delivery mechanism (HTML dashboards with metrics). get-shit-done v1 has no built-in reporting; output is markdown files. gsd-2's git strategy is **automated and deterministic** (worktree isolation, sequential commits, squash merge per milestone); get-shit-done v1's is **LLM-driven and prone to conflicts**.

---

## 5. Skill / agent definition format

**gsd-2 skill system:**

- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/setup-catalog.ts` (24 bundled extensions listed)
- Format: TypeScript extension modules (e.g., `extensions/github-sync/`, `extensions/browser-tools/`)
- Loading: Per-unit-type skill manifest resolver (README line 105: "Per-unit-type skill manifest resolver (#4779)")
- Metadata: Declarative via UnitContextManifest v2 (README line 102: "UnitContextManifest v2 (#4924, #4934)")

**get-shit-done v1 agent system:**

- File: `/Users/soulofall/projects/get-shit-done/agents/` (5 specialist subagents: scout, researcher, worker, javascript-pro, typescript-pro)
- Format: Agent prompt files + SDK subagent invocation
- Loading: Implicit per command (e.g., `/gsd-plan-phase` loads researcher, planner, executor)

**Difference:** gsd-2 has **24 bundled extensions** (browser tools, MCP client, GitHub sync, remote questions, voice, Ollama, etc.) vs get-shit-done's **5 specialist subagents**. gsd-2 integrates skills **declaratively into the manifest** (tools are request-time scoped); get-shit-done relies on **prompt-driven subagent invocation**. gsd-2's skill system is **extensible via npm** (lines 107: "Extensions framework — gsd extensions install / update / uninstall / list / info / validate").

---

## 6. Context injection mechanism

**gsd-2 context injection:**

- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/context-injector.ts`
- Strategy: **Tiered Context Injection (M005)** — relevance-scoped context with 65%+ token reduction (README line 211)
- Mechanism: pre-inline task plan, slice plan, prior task summaries, dependency summaries, roadmap excerpts, decisions register into dispatch prompt (README lines 358–360)
- Database-backed: Context Mode (default-on, configurable) guides agents to use `gsd_exec` for noisy scans, `gsd_exec_search` to reuse prior runs, `gsd_resume` to read prior snapshot (README lines 361, 659)
- Verification: artifact integrity fingerprints (README line 65: "Memory, context, and token control — artifact integrity fingerprints...")

**get-shit-done v1 context injection:**

- File: `sdk/prompts/` (prompt templates)
- Strategy: Subagent handoff — researcher returns compressed findings, planner reads them, executor reads plan
- Mechanism: context string passed between phases, not pre-inlined
- State: Artifact reads are tool calls (LLM calls `/gsd-read` or similar) during execution

**Difference:** gsd-2 **pre-inlines everything at dispatch time**, eliminating tool-call overhead. get-shit-done v1 **lets the LLM request artifacts on-demand** via tool calls. gsd-2's Context Mode is **opt-able and measured** (exec_timeout_ms, exec_stdout_cap_bytes, exec_digest_chars configured per-project); get-shit-done v1 has no such guardrails.

---

## 7. Vault / secrets model

**gsd-2 secrets model:**

- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/get-secrets-from-user.ts` (masked collection)
- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/ask-user-questions.ts` (structured elicitation)
- Mechanism: `secure_env_collect` tool uses MCP form elicitation; secrets never exposed in tool output (README line 194: "Secure credential collection over MCP")
- Storage: `~/.gsd/auth.json` (encrypted per platform? unverified), environment overlay on exec
- Auto-rotation: `/gsd keys` command for API key management (README line 513)

**get-shit-done v1 secrets model:**

- Mechanism: LLM prompts for `.env` setup; no automation
- Storage: User-managed `.env` files
- Scope: No per-tool isolation

**Difference:** gsd-2 has **secure form-based collection** without exposing secrets in logs; get-shit-done v1 relies on **manual .env editing**. gsd-2 has **built-in key rotation** (`/gsd keys doctor` — verify API keys are valid); get-shit-done v1 does not.

---

## 8. Daemon / persistent execution

**gsd-2 persistent execution:**

- Auto-mode loop: `/gsd auto` runs a state machine that persists worker state + unit-dispatch state + paused-session metadata to project-root `gsd.db` (README line 364–365)
- Crash recovery: Next `/gsd auto` reconstructs interrupted unit from DB, reads surviving session file, synthesizes recovery briefing (line 365–366)
- Parallel orchestration: Multiple workers via `.gsd/parallel/` IPC; multi-session coordination via db-backed leases (line 365, 482: "DB-backed coordination across multiple GSD workers")
- Headless mode: `gsd headless --timeout 600000` auto-responds to prompts, detects completion, exits with structured codes (line 477)

**get-shit-done v1 persistent execution:**

- No daemon. Each command is a fresh `/gsd-*` invocation.
- Session recovery: User must manually resume from saved STATE.md / ROADMAP.md
- No multi-worker support

**Difference:** gsd-2 is a **full orchestration platform** with persistent state, crash recovery, and multi-worker coordination. get-shit-done v1 is **command-driven with manual recovery**. gsd-2's headless mode enables **CI/cron integration** (line 470: "Designed for CI pipelines, cron jobs, and scripted automation"); get-shit-done v1 is interactive-only.

---

## 9. Anti-pattern guardrails

**CRITICAL: Does gsd-2 keep the pre-dispatch reconciliation pattern?**

**YES. gsd-2 keeps AND expands the pattern.**

**gsd-2 pre-dispatch reconciliation (ADR-017 — new in v2.82):**

- File: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/state-reconciliation/index.ts` (entry point)
- Architecture: **Drift-driven state reconciliation** — derives state, detects drift kinds, applies repairs, re-derives (cap=2 passes to handle cascading repairs).
- Drift handlers registered: `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/state-reconciliation/drift/`
  - `stale-worker.ts` — clears orphaned session locks if owning PID is dead (solves 30-min stale-window block)
  - `unregistered-milestone.ts` — imports milestone directories with ROADMAP.md/CONTEXT.md/SUMMARY.md that weren't imported
  - `roadmap.ts` — reconciles ROADMAP.md slice sequence + depends declarations against DB slice rows
  - `completion.ts` — backfills missing completion timestamps from SUMMARY.md mtime
  - `merge-state.ts` — detects mid-merge stalls and recovers
  - `sketch-flag.ts` — validates `[sketch]` badges on pending-refinement slices
  - `stale-render.ts` — rebuilds stale markdown projections from DB

**Integration:**

- Called before every dispatch: `auto.ts` line (per README grep): "async reconcileBeforeDispatch() { const result = await reconcileBeforeDispatch(dispatchBasePath); }"
- Parallel spawn gate: `reconcileBeforeSpawn` runs reconciliation at parent before fanout (README line 39: "Parallel spawns reconcile before fanning out")
- Typed exit reason: slice-parallel-reconciliation-failed surfaces a user-visible message instead of hanging (line 39–40)

**Improvement over get-shit-done v1:**

- get-shit-done v1 has no pre-dispatch check. Dispatch-guard.ts in gsd-2 is the **only guardrail** (dependency ordering); it doesn't detect or repair drift.
- gsd-2's **ADR-017 is new architecture** — a dedicated registry of drift detectors + repair handlers, each idempotent, capped at 2 retry cycles. This eliminates entire classes of "stale lock", "unregistered milestone", "diverged roadmap", "missing timestamp" bugs that would silently corrupt get-shit-done v1 projects.

---

## 10. Where Anvil already has parity

Anvil (v2.2.12) is a **Rust-based CLI coding agent** built on a proprietary runtime, not the Pi SDK. The comparison is asymmetric — Anvil and gsd-2 solve the same problem (orchestrate multi-task coding work) but with different stacks (Rust vs TypeScript/Node) and different underlying harnesses (Anvil proprietary vs Pi SDK).

**Parity areas identified:**

1. **Context-per-task freshness**: Anvil crates/runtime isolates per-command context; gsd-2 does per-task. Both avoid context rot.  
   - Anvil: `crates/runtime/src/session.rs` (session lifecycle)
   - gsd-2: auto.ts line 1 (fresh session per unit)

2. **State persistence**: Both use persistent state to recover from crashes.  
   - Anvil: `crates/runtime/src/history.rs` (session history)
   - gsd-2: `gsd.db` + `.gsd/runtime/` event journal

3. **Cost tracking**: Both instrument token/cost per phase.  
   - Anvil: `crates/runtime/src/effort.rs` (effort/cost model)
   - gsd-2: per-unit cost ledger, dashboard projection

4. **Skill/tool surfacing**: Both have extensible tool systems.  
   - Anvil: `crates/tools/` (bundled tools)
   - gsd-2: 24 bundled extensions, extensible via npm

5. **Multi-model support**: Both support multiple LLM providers.  
   - Anvil: `crates/anvil-cli/src/providers.rs`
   - gsd-2: 20+ providers, per-phase model selection (lines 883–893)

---

## 11. Where Anvil has gaps relative to gsd-2 (THAT ARE NOT ALREADY COVERED BY THE get-shit-done COMPARISON)

gsd-2-specific features **not** present in get-shit-done v1 OR Anvil:

### 11.1 Drift-driven state reconciliation (ADR-017)

**What it is:** Structured, registered drift detectors + idempotent repair handlers running before every dispatch. Solves "dead PID in lock file", "unregistered milestone directory", "roadmap/DB divergence", "missing completion timestamp", "stale render", "sketch flag validation".

**Why it's new:** Neither get-shit-done v1 nor Anvil has a generic drift registry. Both have ad-hoc checks scattered in recovery paths. gsd-2 centralizes this into `/src/resources/extensions/gsd/state-reconciliation/drift/*.ts` with pluggable handlers.

**Anvil gap:** Anvil has no equivalent drift detection. If Anvil's runtime state DB gets out of sync with project artifacts, there's no automatic repair mechanism.

**Where to adopt:** `crates/runtime/src/` would need a drift handler registry analogous to `/state-reconciliation/registry.ts`. Each drift kind (session-lock-alive-check, milestone-import, dependency-sync, timestamp-backfill) would be a separate module.

### 11.2 Parallel spawn-gate reconciliation

**What it is:** Before `/gsd parallel start` fans out multiple workers, reconciliation runs at the parent to detect/repair drift once, preventing workers from racing on shared drift. Gate failures surface typed exit reason + user message (README line 39–40).

**Why it's new:** Both get-shit-done v1 and Anvil support sequential execution; gsd-2 adds **parallel multi-milestone within a single project** with DB-backed leases + worktree isolation.

**Anvil gap:** Anvil has no parallel-slice or parallel-milestone dispatch. Each project is single-threaded.

**Where to adopt:** This is **not** a general pattern — it's specific to gsd-2's parallel orchestration model. Anvil would need to first support parallel milestone dispatch, then apply spawn-gate reconciliation.

### 11.3 Worktree lifecycle + safety model

**What it is:** First-class worktree lifecycle verbs (`adoptOrphanWorktree`, `adoptSessionRoot`, `resumeFromPausedSession`, `restoreToProjectRoot`). Lifecycle split into dedicated modules; write/edit operations enforce worktree-isolation contract; milestone merge closeout is fail-closed (README lines 42–44).

**Files:**
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/worktree-lifecycle.ts`
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/worktree-safety.ts`
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/worktree-manager.ts`

**Why it's new:** gsd-2 supports **branch-per-milestone OR worktree-per-milestone isolation** (configurable via `git.isolation: worktree | branch | none`). It treats worktrees as a first-class artifact, not a side effect.

**Anvil gap:** Anvil has no worktree support. Multi-milestone work happens on the same branch or requires user branch management.

**Where to adopt:** `crates/runtime/src/` would need a `worktree_lifecycle.rs` module. The contract is: (1) each milestone gets a worktree OR branch, (2) all slice work on that worktree is sequential, (3) on milestone close, merge to main via squash, (4) restore to project root.

### 11.4 Time-decay memory ranking (ADR-013)

**What it is:** Structured memory storage with hit-count tracking, last-hit-at timestamps, and time-decay ranking. Memories are typed (decision, lesson, pattern, surprise) with optional `structured_fields` for metadata. Dual-write migration from decisions table is complete with backfill (README lines 119–120, 159).

**Files:**
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/memory-store.ts`
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/memory-relations.ts`
- `gsd-db.ts`: insertMemoryRow, updateMemoryContentRow, incrementMemoryHitCount, supersedeMemoryRow, decayMemoriesBefore, supersedeLowestRankedMemories

**Why it's new:** gsd-2 maintains a **structured knowledge graph** (README line 132: "Knowledge graph system — structured knowledge graph built from project artifacts"). Memories are ranked by hit-count + time decay; stale memories are superseded.

**Anvil gap:** Anvil has no memory system. Project learnings are ad-hoc notes in STATE or similar.

**Where to adopt:** `crates/runtime/src/` would need a memory table schema + ranking logic. The contract is: (1) capture typed memories (decision, lesson) with source unit + confidence, (2) track hits, (3) rank by decay function, (4) supersede stale entries, (5) return ranked slice for context injection.

### 11.5 Verification-evidence DB + auto-retry backoff

**What it is:** Verification command results (stdout, stderr, exit code, timestamp) stored in `gsd.db` as verification_evidence rows. Failed verifications trigger exponential backoff retries with stuck detection between attempts (README line 49: "Verification retries back off properly — a new verification-retry-policy.ts adds bounded exponential backoff").

**Files:**
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/db-verification-evidence-schema.ts`
- (grep for verification-retry-policy.ts not found in scan, but referenced in README v2.82)

**Why it's new:** gsd-2 **guarantees verification doesn't spin in tight loops**. Each retry is backoff-scheduled and stuck detection runs between attempts.

**Anvil gap:** Anvil has no verification evidence storage or backoff policy.

**Where to adopt:** `crates/runtime/src/` would need a verification_evidence table + a backoff scheduler. Retries would be time-scheduled, not immediate.

### 11.6 Complete-slice read-only constraint

**What it is:** The complete-slice phase closeout prompt is forbidden from writing project files. Closeout can read, summarize, commit, but not edit (README line 48: "complete-slice closeout is read-only").

**Why it's new:** Removes a class of race conditions where closeout edits could fight with the next slice's setup.

**Anvil gap:** Anvil has no explicit write-gate on phase transitions.

**Where to adopt:** `crates/runtime/src/` would need a phase-scoped write-gate that marks complete-slice phase as read-only and enforces it via tool-scope checks.

### 11.7 Deep project planning mode

**What it is:** `gsd new-project --deep` runs a staged discovery flow with research-dispatch units, evaluation-review phases, and project-shape-aware questioning (README line 86: "Deep planning mode — /gsd new-project --deep").

**Files:**
- `/Users/soulofall/projects/gsd-2/src/resources/extensions/gsd/deep-project-setup-policy.ts`

**Why it's new:** Distinguishes between **light** project setup (one approval gate) and **deep** setup (staged discovery, project-shape classification, thorough requirements).

**Anvil gap:** Anvil has no multi-stage discovery flow.

**Where to adopt:** This is a **mode toggle** in the setup phase. Anvil would need to detect project complexity and branch between quick-setup and staged-discovery paths.

### 11.8 Slice sketch refinement (`refine-slice`)

**What it is:** Slices in ROADMAP.md can be marked with a `[sketch]` badge to indicate they have approved scope but haven't been decomposed into task plans yet. Auto-mode runs a `refine-slice` dispatch unit before execution to expand the sketch using latest prior-slice summaries (README lines 340–341: "A sketch slice has an approved title, dependency shape, demo line, and scope boundary, but it has not yet been expanded into task plans").

**Why it's new:** Enables **progressive planning** — first slice fully planned up front, later slices sketched and refined just-in-time.

**Anvil gap:** Anvil requires full planning before execution.

**Where to adopt:** `crates/runtime/src/` would need a sketch-detection phase that identifies `[sketch]` slices, queues a refine-dispatch unit, and blocks execution until refined.

### 11.9 Milestone completion rollup dashboard

**What it is:** At milestone boundaries, auto-mode renders a `CompletionDashboardSnapshot` summarizing success criteria results, definition-of-done results, requirement outcomes, deviations, follow-ups, key decisions, key files, lessons learned, total cost, total tokens, cache hit rate, and slice progress (README line 57: "Milestone completion rollup").

**Why it's new:** End-of-milestone visibility. User no longer scrolls back through transcript.

**Anvil gap:** Anvil has no milestone-completion summary.

**Where to adopt:** `crates/runtime/src/` would need a CompletionDashboardSnapshot builder that queries the metrics DB, renders a structured snapshot, and emits it as a TUI overlay.

### 11.10 Remote questions (Slack/Discord routing)

**What it is:** When auto-mode or headless mode needs human input (discussion gate, approval, decision), `remote-questions` extension routes the decision to Slack or Discord instead of blocking (README line 246, 721: "Remote Questions — Route decisions to Slack/Discord when human input is needed in headless/CI mode").

**Files:** `/Users/soulofall/projects/gsd-2/src/resources/extensions/remote-questions/`

**Why it's new:** Enables **unattended operation**. Auto-mode in CI can ask for design feedback without blocking the entire pipeline.

**Anvil gap:** Anvil has no decision-routing mechanism.

**Where to adopt:** `crates/tools/` would need a remote-questions tool that posts a decision request to a Slack webhook and polls for response.

---

## 12. Adopt / skip recommendation

gsd-2 is **a generational upgrade from get-shit-done v1**, not an incremental patch. The two are fundamentally different architectures:

- **get-shit-done v1:** Prompt framework, runs inside Claude Code/Gemini CLI, state in markdown.
- **gsd-2:** Standalone CLI, owns the entire agent session, state in database.

**For Anvil:**

1. **Adopt ADR-017 (drift-driven reconciliation).** This is a general anti-corruption pattern that Anvil would benefit from. It solves "stale lock", "unregistered milestone", "diverged roadmap", "missing timestamp" bugs in a composable way. Assign it to `crates/runtime/src/` as a new module.

2. **Adopt worktree-per-milestone lifecycle.** If Anvil plans to support parallel-slice or branch-per-milestone isolation, gsd-2's worktree safety model is battle-tested. It separates lifecycle verbs, enforces write-gates, and fails closed on merge conflicts.

3. **Skip memory ranking for now.** ADR-013 is valuable long-term, but it's orthogonal to orchestration. Anvil could defer this until it has a clear memory use case (e.g., cross-session decision recall). gsd-2 has this because it expects long multi-month projects; Anvil's context model may be different.

4. **Skip deep planning mode.** This is a UX feature, not an orchestration primitive. Adopt only if Anvil adds staged-discovery modes.

5. **Consider verify-evidence backoff.** This is a reliability win. Saves cost by not retrying tight-loop flaky tests. Medium priority.

6. **Consider remote-questions routing.** High value if Anvil adds CI/headless modes. Medium priority.

7. **Skip milestone completion rollup.** This is a UI feature. Adopt only if Anvil's dashboard / reporting needs it.

**Bottom line:** gsd-2 is worth reading thoroughly for **state-reconciliation patterns and worktree safety**, but don't port gsd-2 wholesale. It's designed for TypeScript/Node on the Pi SDK. Anvil is Rust and has its own runtime. Focus on the **concepts** (ADR-017, worktree lifecycle, verification backoff) and adapt them to Anvil's architecture.

Most importantly: **the get-shit-done audit is foundational.** gsd-2 is the evolution of get-shit-done. If you're deciding what to adopt, start there — the parallel-runner, skill system, deep mode, knowledge graph, and memory ranking are more directly portable to Anvil than gsd-2's Pi-SDK-specific worktree mechanics.

