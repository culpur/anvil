# Superpowers vs Anvil — research comparison

**Audited:** 2026-05-11
**Superpowers path:** /Users/soulofall/projects/superpowers
**Anvil path:** /Users/soulofall/projects/anvil-dev
**Anvil current version:** v2.2.12 (released 2026-05-11)

---

## 1. What Superpowers is (one paragraph)

Superpowers is a complete software development methodology packaged as a Claude Code / Codex / Cursor skill plugin (v5.1.0, 16K+ lines of markdown documentation, 176 files). It solves the problem of agent behavior inconsistency — ensuring coding agents systematically brainstorm before building, write tests before code (TDD), follow code review discipline, and coordinate across parallel tasks. Rather than a runtime or daemon, Superpowers is a skill-discipline package: reusable documentation, anti-pattern guards, and workflow choreography that shapes agent behavior at the LLM level via frontmatter + prompt injection. Licensed MIT. Last commit: 2026-04-30. Target audience: users of Claude Code, Codex (OpenAI), Cursor, Gemini, GitHub Copilot CLI, and Factory Droid who want their agents to follow a proven development methodology.

---

## 2. Architecture summary

Superpowers is a multi-harness skill plugin with three parallel plugin directories:

- `.claude-plugin/` (Claude Code plugin): `plugin.json` + `marketplace.json` (source-of-truth for CLI install) | `/Users/soulofall/projects/superpowers/.claude-plugin/plugin.json:1-20`
- `.codex-plugin/` (OpenAI Codex): `plugin.json` with distinct interface config | `/Users/soulofall/projects/superpowers/.codex-plugin/plugin.json` (not read, assumed for tooling)
- `.cursor-plugin/` (Cursor agent): third variant maintained in parallel

**Skills directory structure:** 16 skills in `skills/` directory (flat namespace, no nesting). Each skill is a directory containing:
- `SKILL.md` (mandatory): 70–655 lines per skill, frontmatter + markdown prose | `/Users/soulofall/projects/superpowers/skills/brainstorming/SKILL.md:1-4`
- Supporting files (optional): reference docs, pressure-test transcripts, prompt templates (e.g., `testing-anti-patterns.md`, `spec-document-reviewer-prompt.md`)

**Version management:** Synchronized across all six plugin targets via `.version-bump.json` | `/Users/soulofall/projects/superpowers/.version-bump.json` (maintains parity: `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`, `.cursor-plugin/plugin.json`, `package.json`, `gemini-extension.json`).

**Discovery model:** Skills are loaded on-demand via `/skill` CLI in Claude Code (or equivalent in other harnesses). No auto-load at session start; the bootstrap (`using-superpowers` skill) is the entry point that educates agents about the skill system.

---

## 3. Schedule grammar / dispatch

Superpowers is a documentation plugin, not a daemon. It has **no scheduling engine, no cron, no background dispatch**. All workflow execution is synchronous, within a single agent session. The closest construct is the `subagent-driven-development` skill, which dispatches fresh LLM subagents for each task within the *same interactive session* and waits for their completion before proceeding. | `/Users/soulofall/projects/superpowers/skills/subagent-driven-development/SKILL.md:1-12`

No pre-scheduled or event-triggered routines. The user must explicitly request work; the skill system guides how that work unfolds within the session.

---

## 4. Output / archive / delivery

Superpowers has **no persistent output archive or delivery mechanism**. Skills produce work artifacts (plans, specs, code changes) that live in the user's working tree; skill text directs the agent to save designs to `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md` and implementation plans to `docs/superpowers/plans/YYYY-MM-DD-<feature-name>.md` | `/Users/soulofall/projects/superpowers/skills/brainstorming/SKILL.md:111` and `/Users/soulofall/projects/superpowers/skills/writing-plans/SKILL.md:18`, but there is no built-in output archive, no delivery to email/webhook/Slack, no routine-specific output capture.

The *user's* git repo is the output store. The skill system is pure choreography.

---

## 5. Skill / agent definition format

**This is the central section for Superpowers.**

### Frontmatter (YAML)

Every SKILL.md file opens with a YAML frontmatter block. **Mandatory fields:**
- `name`: Kebab-case skill identifier (letters, numbers, hyphens only) | `/Users/soulofall/projects/superpowers/skills/brainstorming/SKILL.md:2`
- `description`: Third-person, ≤500 characters, describes *triggering conditions* not workflow. **Critical:** Must NOT summarize the skill's process — that causes Claude to follow the description instead of reading the full skill body. | `/Users/soulofall/projects/superpowers/skills/writing-skills/SKILL.md:140-172`

**Optional fields:**
- `triggers`: List of command aliases (e.g., `["review", "code review", "pr review"]`). Not present in all skills; example in Anvil equivalent | `/Users/soulofall/projects/anvil-dev/crates/commands/bundled/skills/code-review/SKILL.md:4`
- `chains_to`: Declarative chaining to other skills (Superpowers does NOT use this; Anvil does | §10 below)

### Markdown body structure (canonical pattern across 16 skills)

From `writing-skills/SKILL.md:105-137` (best-documented example):

```markdown
# Skill Name

## Overview
What is this? Core principle in 1-2 sentences.

## When to Use
[Small inline flowchart IF decision non-obvious]
Bullet list with SYMPTOMS and use cases
When NOT to use

## Core Pattern (for techniques/patterns)
Before/after code comparison

## Quick Reference
Table or bullets for scanning common operations

## Implementation
Inline code for simple patterns
Link to file for heavy reference or reusable tools

## Common Mistakes
What goes wrong + fixes
```

### Canonical examples from repo

1. **`brainstorming/SKILL.md`** (164 lines): Socratic design refinement, structured checklist (Explore → Question → Propose → Present → Document → Review → Gate), process flowchart (Graphviz dot). Design gate pattern. | `/Users/soulofall/projects/superpowers/skills/brainstorming/SKILL.md:1-150`

2. **`test-driven-development/SKILL.md`** (371 lines): RED-GREEN-REFACTOR cycle, the Iron Law (`NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST`), narrative anti-patterns (rationalization examples), hardcoded rule text. | `/Users/soulofall/projects/superpowers/skills/test-driven-development/SKILL.md:1-120`

3. **`subagent-driven-development/SKILL.md`** (279 lines): Dispatch per-task fresh subagents with two-stage review (spec compliance, then code quality). Subagent prompt templates referenced (./implementer-prompt.md, ./spec-reviewer-prompt.md). Red flags list (§9 below). Process flowchart. | `/Users/soulofall/projects/superpowers/skills/subagent-driven-development/SKILL.md:1-88`

4. **`writing-plans/SKILL.md`** (152 lines): Plan document header with **REQUIRED SUB-SKILL** prose (§9 below). Exact template for task structure (file list, checkbox steps, code blocks). No-placeholder checklist. Inline self-review pattern. | `/Users/soulofall/projects/superpowers/skills/writing-plans/SKILL.md:45-104`

### Loader / discovery

**No explicit loader code in Superpowers.** Skills are discovered by harness-specific mechanisms:
- Claude Code: `/skill` CLI with bash script wrappers (not shown in repo; lives in harness)
- Codex: Plugin system built into OpenAI's agent
- Cursor: Agent plugin marketplace integration

Superpowers source provides the SKILL.md files and plugin.json metadata; the harness plugin loader reads `name` + `description` from frontmatter and surfaces them in skill search.

---

## 6. Context injection mechanism

Superpowers skills are **loaded on-demand by agent choice** (or agent-system-prompt suggestion), not auto-injected.

**Trigger mechanism:**
1. Agent reads a user request → detects it maps to one or more skill `description` values (semantic match on triggering conditions)
2. Agent (via harness UI) presents matching skills: `[Load superpowers:brainstorming]` etc.
3. User or agent-prompt-rule (optional) invokes `/skill load brainstorming`
4. Harness reads `skills/brainstorming/SKILL.md` in full, injects into system prompt
5. Agent follows the skill's prose and flowcharts for the duration of the task

**Conditional loading:** No built-in conditional logic. The entire skill is injected if loaded; Anvil's `chains_to:` with `when:` conditions (§10) is absent. Superpowers does NOT use frontmatter-driven chaining. Instead, chaining happens via **imperative prose in the skill body** (see §9, "Imperative REQUIRED SUB-SKILL").

**Bootstrap:** The `using-superpowers` skill (117 lines, `/Users/soulofall/projects/superpowers/skills/using-superpowers/SKILL.md`) is a meta-skill that teaches agents the skill system exists. This is intended to be injected once per session, at startup. No automatic cycle.

---

## 7. Vault / secrets model

Superpowers has **no secrets system**. Skills are documentation; they do not store or reference API keys, tokens, or credentials. Skills that would require secrets (e.g., a "deploy" skill invoking an API) are out of scope. The assumption is that secrets live in the user's shell environment or the agent's native credential store (Anvil's vault, for example, if Anvil runs Superpowers code).

Skills may *reference* tools that consume secrets (e.g., "call the `git push` command," which may use an SSH key), but Superpowers itself does not manage those secrets.

---

## 8. Daemon / persistent execution

**Not applicable.** Superpowers is a skill library, not a runtime. There is no daemon process, no background jobs, no multi-session persistence. All execution is synchronous within a single interactive agent session.

---

## 9. Anti-pattern guardrails

**This is the second central section for Superpowers.** Two distinct patterns:

### A. Anti-Pattern Tables (Red Flags)

Superpowers' most distinctive anti-pattern guard is the **explicit anti-pattern narrative** embedded in skill prose. Not a structured "red_flags" block (Anvil's term, adopted from Superpowers but not yet implemented in Superpowers itself as frontmatter — see §11 below), but rather **titled anti-pattern sections with narrative + code examples**.

**Canonical example:** `test-driven-development/testing-anti-patterns.md` | `/Users/soulofall/projects/superpowers/skills/test-driven-development/testing-anti-patterns.md:1-300`

The file contains five numbered anti-patterns, each with:
- Violation code example (marked `❌ BAD`)
- Why it's wrong (explanation)
- Correction code example (marked `✅ GOOD`)
- Gate function (a decision rule to apply before violating)
- Red flags list at the end (summary of violation signatures)

Example from lines 13-62:
```
## The Iron Laws
1. NEVER test mock behavior
2. NEVER add test-only methods to production classes
3. NEVER mock without understanding dependencies

## Anti-Pattern 1: Testing Mock Behavior
**The violation:** [code example]
**Why this is wrong:** [explanation + impact]
**your human partner's correction:** [expected pushback phrase]
**The fix:** [code example]

### Gate Function
[Decision tree: BEFORE asserting on any mock element, ask...]
```

Lines 284–291 list the "Red Flags" summary table:
```
| Anti-Pattern | Fix |
| Assert on mock elements | Test real component or unmock it |
| Test-only methods in production | Move to test utilities |
| ...
```

**Appearance in skill text:** The entire `testing-anti-patterns.md` is referenced in the skill header:
> "When adding mocks or test utilities, read @testing-anti-patterns.md to avoid common pitfalls:" | `/Users/soulofall/projects/superpowers/skills/test-driven-development/SKILL.md:320-330`

The anti-patterns file is **not injected into the system prompt by default**; instead, the skill text directs the agent to read it for specific scenarios (when mocking).

### B. Imperative "REQUIRED SUB-SKILL" prose

**This is the pattern Anvil identified as worth adopting.** Superpowers uses imperative prose to enforce sequential workflows.

**Canonical example:** `writing-plans/SKILL.md`, lines 45–61:

```markdown
## Plan Document Header

**Every plan MUST start with this header:**

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development 
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
```

Another example, from `brainstorming/SKILL.md`, lines 12–14:

```markdown
<HARD-GATE>
Do NOT invoke any implementation skill, write any code, scaffold any project, or take 
any implementation action until you have presented a design and the user has approved it.
</HARD-GATE>
```

And from `subagent-driven-development/SKILL.md`, lines 269–273:

```markdown
**Required workflow skills:**
- **superpowers:using-git-worktrees** - Ensures isolated workspace
- **superpowers:writing-plans** - Creates the plan this skill executes
- **superpowers:requesting-code-review** - Code review template for reviewer subagents
- **superpowers:finishing-a-development-branch** - Complete development after all tasks
```

**How it works:** The skill prose contains **text like "REQUIRED SUB-SKILL" or explicit sequential rules**, trusting the agent to follow them. This is the *imperative* (procedural) complement to Anvil's *declarative* `chains_to:` graph. Superpowers does **not** track this in machine-readable frontmatter; it's purely narrative enforcement.

### C. "Your human partner" language

Superpowers uses consistent anthropomorphic language — "your human partner" — throughout skill text. This is **deliberate** (per CLAUDE.md:238–239, "Superpowers has its own tested philosophy about skill design, agent behavior shaping, and terminology").

Example phrases:
- From testing-anti-patterns.md:259: `"your human partner's question: 'Do we need to be using a mock here?'"`
- From brainstorming/SKILL.md:18: `"Do NOT invoke any implementation skill... This applies to EVERY project regardless of perceived simplicity."`

This language is *part of the behavior-shaping technique*; rewriting it is explicitly forbidden in CLAUDE.md (§ "What We Will Not Accept" > "Compliance changes to skills").

### D. Gate functions and condition blocks

Superpowers uses structured **gate functions** — explicit decision trees — to prevent violations. From testing-anti-patterns.md, lines 51–61:

```
### Gate Function

BEFORE asserting on any mock element:
  Ask: "Am I testing real component behavior or just mock existence?"

  IF testing mock existence:
    STOP - Delete the assertion or unmock the component

  Test real behavior instead
```

These are narrative (not machine-executable), but they form a recognizable decision pattern that agents can follow.

---

## 10. Where Anvil already has parity

| Surface | Superpowers | Anvil v2.2.12 |
|---------|-------------|---------------|
| **Skill format** | YAML frontmatter + markdown prose | Identical: YAML frontmatter + markdown prose |
| **Mandatory fields** | `name`, `description` | Same: `name`, `description` |
| **Skill discovery** | Flat namespace in `skills/` directory | Flat namespace in `crates/commands/bundled/skills/` |
| **Skill chaining** | Imperative prose ("REQUIRED SUB-SKILL") | Declarative `chains_to:` + imperative prose (both supported) |
| **Anti-pattern narration** | Explicit anti-pattern files + Red Flags tables | `code-review` skill body has "prioritised review" language, but no dedicated anti-pattern file yet |
| **Markdown prose focus** | Core methodology — skills ARE prose | Core methodology — skills ARE prose |
| **Trigger matching** | `description` field semantically matched by agent | `description` field used for discovery |
| **Bootstrap** | `using-superpowers` meta-skill | Equivalent: skill search UI + `/help` system |

**Skill chaining parity detail:**
- Superpowers: `chains_to:` field (if present) is **not widely used** in the 16 published skills; instead, chaining is stated as prose ("REQUIRED SUB-SKILL"). Anvil's implementation (§2 of this doc) supports both.
- Anvil `/Users/soulofall/projects/anvil-dev/crates/commands/src/skill_chaining.rs:1-97`: supports `chains_to: [{skill: name, when: condition}]` with depth-3 traversal, cycle detection, and byte-count limits. **Anvil has *more* structure here than Superpowers as deployed.**

---

## 11. Where Anvil has gaps relative to Superpowers

Two primary gaps identified by prior research; verification:

### Gap 1: `red_flags` block in frontmatter

**Superpowers implementation:**
- Anti-pattern tables exist as *narrative sections* in skill markdown (example: `testing-anti-patterns.md` lines 284–291 have a Red Flags table).
- These are NOT in skill frontmatter; they're in supporting files.
- **Superpowers does NOT use a machine-readable `red_flags:` frontmatter block.**

**Anvil implementation:**
- `/Users/soulofall/projects/anvil-dev/docs/ROUTINES-ADOPTION-NOTES.md:260–274` documents the *proposed* `red_flags:` block for v2.4:
  ```yaml
  red_flags:
    - "I'll skip the tests, the change is obvious"  → "No. Run the tests anyway."
    - "I can summarize without reading the diff"     → "No. Read the diff first."
  ```
- This has **NOT been implemented in Anvil v2.2.12 yet**. It remains a design proposal.

**Gap confirmed:** Anvil **lacks** the explicit `red_flags:` frontmatter block and system-prompt injection that the ROUTINES-ADOPTION-NOTES identified as worth adopting. Superpowers doesn't have it either (yet), but the pattern is present as prose in supporting files.

**Cost to implement in Anvil:** § 13 of ROUTINES-ADOPTION-NOTES estimates 0.5 days (schema extension + system-prompt injection when skill loads). | `/Users/soulofall/projects/anvil-dev/docs/ROUTINES-ADOPTION-NOTES.md:299`

### Gap 2: Imperative "REQUIRED SUB-SKILL" prose support

**Superpowers implementation:**
- Prose directives like "REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development" are embedded in skill markdown. | `/Users/soulofall/projects/superpowers/skills/writing-plans/SKILL.md:52` and `/Users/soulofall/projects/superpowers/skills/subagent-driven-development/SKILL.md:269–280`
- These are **narrative instructions trusted to be followed by the agent**, not machine-enforced.
- No special syntax or parsing; just prose.

**Anvil implementation:**
- Anvil has `chains_to:` (declarative, machine-parsed). | `/Users/soulofall/projects/anvil-dev/crates/commands/src/skill_chaining.rs:12–37`
- Anvil does **NOT parse or special-case prose directives** like "REQUIRED SUB-SKILL."
- A skill *can* include such prose, but it's treated as regular markdown, not enforced.

**Verification:** Search of Anvil bundled skills (`crates/commands/bundled/skills/`) finds **no instances of "REQUIRED SUB-SKILL"** prose. | Bash confirmation: `grep -r "REQUIRED" /Users/soulofall/projects/anvil-dev/crates/commands/bundled/skills --include="*.md"` (no matches)

**Gap confirmed:** Anvil supports the *declarative* `chains_to:` but has not adopted the *imperative* prose pattern. Both could coexist (prose for workflow intent, `chains_to:` for conditional dispatch).

**Cost to implement:** No code changes required — it's a prose-only pattern. Any Anvil skill can immediately adopt "REQUIRED SUB-SKILL" language. However, documenting the pattern in Anvil's skill authoring guide (equivalent to `superpowers/skills/writing-skills/SKILL.md`) would take ~1–2 hours. Making it system-prompt-aware (so routine runs detect and enforce it) would add ~1 day.

### Gap 3: Multi-harness plugin architecture

**Superpowers:**
- Ships as a plugin for six harnesses: Claude Code, Codex, Cursor, Gemini, Factory Droid, GitHub Copilot CLI. | `/Users/soulofall/projects/superpowers/README.md:7–152` (install instructions per harness)
- Three parallel plugin.json files (`.claude-plugin/`, `.codex-plugin/`, `.cursor-plugin/`) maintained in sync. | `/Users/soulofall/projects/superpowers/.version-bump.json`
- Custom `sync-to-codex-plugin` script mirrors into OpenAI Codex marketplace. | `/Users/soulofall/projects/superpowers/RELEASE-NOTES.md:30–38`

**Anvil:**
- Runs as a CLI tool + daemon, not a plugin.
- No multi-harness distribution model.
- Standalone release binary (macOS, Linux).

**Gap confirmed:** Anvil does not ship as a skill plugin for other harnesses. This is **architectural** (Anvil is a tool, not a plugin library) and unlikely to be adopted.

### Gap 4: Subagent dispatch via prose directives

**Superpowers:**
- `subagent-driven-development` skill (lines 1–87) documents a specific **procedural workflow**: extract tasks, dispatch implementer subagent, review (spec compliance + code quality), mark complete, loop. | `/Users/soulofall/projects/superpowers/skills/subagent-driven-development/SKILL.md:42–87`
- Subagent prompts are **templates referenced by file path**, not inlined: `./implementer-prompt.md`, `./spec-reviewer-prompt.md`, `./code-quality-reviewer-prompt.md`. | Line 124

**Anvil:**
- Has `subagent` support via the LLM tool surface (agents can call `/agent` to spawn subagents).
- Does not have a bundled skill defining the Superpowers-style **two-stage review** workflow (spec compliance first, then code quality).
- No equivalent of the `subagent-driven-development` skill methodology.

**Gap confirmed:** Anvil can spawn subagents but has no built-in skill teaching the **Superpowers review discipline** (spec then quality, separate subagents). This is a methodology gap, not a capability gap.

**Cost to implement:** Write a skill documenting the two-stage review workflow, with template references. ~2–4 hours. Would require Anvil's subagent system to support passing templates or prompt bodies; likely already supported.

### Gap 5: Plan-driven execution with checkpoint gates

**Superpowers:**
- `writing-plans` skill (152 lines) defines an exact plan document structure with checkpoint gates and `REQUIRED SUB-SKILL` directives. | `/Users/soulofall/projects/superpowers/skills/writing-plans/SKILL.md:45–104`
- Every plan includes a mandatory header section explicitly telling agentic workers to invoke `subagent-driven-development` or `executing-plans`. | Lines 50–52

**Anvil:**
- Has worktree management and task execution (`crates/tools/src/worktree_ops.rs`). | ROUTINES-ADOPTION-NOTES.md:24
- No equivalent skill defining a Superpowers-style **plan document format with embedded skill invocation directives**.

**Gap confirmed:** Anvil lacks a skill (or Routine document format) that explicitly choreographs the plan→review→execute flow with skill invocations as prose directives.

**Cost to implement:** Adapt Superpowers' `writing-plans` skill language to Anvil context + wire into routine execution. ~1–2 days.

---

## 12. Adopt / skip recommendation

For each gap from §11:

### Gap 1: `red_flags` block in frontmatter — **ADOPT**

**Rationale:** Low-cost, high-leverage behavior steering. The pattern exists in Superpowers as narrative (anti-pattern files); formalizing it as frontmatter and injecting into the system prompt costs 0.5 days and delivers measurable agent compliance improvement. Anvil's ROUTINES-ADOPTION-NOTES already endorses this (§18). Recommended for Tier 2 of the roadmap (routine work), shipping in a v2.2.x patch. The formal syntax:
```yaml
red_flags:
  - "I'll skip TDD because this is trivial"  → "No. TDD always applies."
  - "I'll summarize without reading the code" → "No. Read completely first."
```
Inject verbatim into the system prompt when the skill loads. Cost: implement, integrate, test. ~4 hours total.

### Gap 2: Imperative "REQUIRED SUB-SKILL" prose — **ADOPT**

**Rationale:** Zero code cost, pure documentation discipline. Superpowers proves the pattern works. Anvil should document it in the skill authoring guide (`crates/commands/src/` equivalent of `superpowers/skills/writing-skills/SKILL.md`) and adopt it in Anvil's own skills. Recommended for v2.2.x. Cost: ~1–2 hours (document the pattern, add 2–3 examples). If routine execution needs to *enforce* these directives (e.g., parse for "REQUIRED SUB-SKILL" and auto-invoke), add 1 day. Start with documentation only.

### Gap 3: Multi-harness plugin architecture — **SKIP**

**Rationale:** Architectural mismatch. Anvil is a CLI tool + daemon; Superpowers is a skill library for existing harnesses. The cost to distribute Anvil as a Claude Code / Codex plugin would require decoupling the runtime from the CLI (effectively creating a parallel plugin codebase) and maintaining parity across six harnesses. Not worth 5+ days of work. If Anvil's command library becomes reusable as a plugin (v3.0+ agenda), revisit. For now, users can invoke Anvil commands from within Claude Code / Codex via shell.

### Gap 4: Subagent discipline skill (two-stage review) — **ADOPT**

**Rationale:** Anvil already has subagent dispatch. Documenting the Superpowers-style review workflow (spec compliance, then code quality, separate subagents per task) would benefit routine automation. This is a **skill-writing effort**, not a runtime feature. Recommended for Tier 2. Cost: ~3–4 hours (study Superpowers subagent skill, adapt to Anvil context, create skill with embedded reviewer prompts). Deliverable: `crates/commands/bundled/skills/subagent-review/SKILL.md` with two-stage workflow docs and prompt templates.

### Gap 5: Plan document format with skill invocation — **DEFER**

**Rationale:** Superpowers' `writing-plans` skill is deeply intertwined with the brainstorming → planning → executing workflow (a methodology choice). Anvil's equivalent is the Routine TOML format (§8 of ROUTINES-ADOPTION-NOTES). Before adopting Superpowers' plan structure, clarify whether Anvil's Routine format should *embed* skill invocation directives or whether the routine agent should infer them from context. This requires a design discussion and eval work (does prescriptive prose help or constrain?). Defer to v2.3 or later when routine tooling stabilizes. Cost estimate if adopted: 1–2 days.

---

**Summary:** Recommend ADOPT for gaps 1–2 and 4 (red_flags, REQUIRED SUB-SKILL prose, subagent discipline skill). Low cost, proven value from Superpowers. SKIP gap 3 (multi-harness). DEFER gap 5 (plan format) pending routine design clarity.

