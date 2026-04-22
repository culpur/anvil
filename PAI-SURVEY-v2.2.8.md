# PAI survey — what to adopt into Anvil from Miessler / SuperClaude / caveman

Research deliverable for Anvil v2.2.8. Read-only survey of three reference projects against Anvil's current feature set. Every ADOPT-X entry cites specific source file paths and specific Anvil landing-zones.

---

## Source 1: Miessler Personal_AI_Infrastructure

- **Architecture**: TypeScript/Bun monorepo of "Packs" (skill bundles) layered on top of Claude Code's `~/.claude` directory. A Pack (e.g. `Packs/Agents`, `Packs/Thinking`, `Packs/Telos`) is a folder containing a top-level `SKILL.md` that routes to sub-skills, plus `Tools/` (executable `.ts` scripts), `Workflows/` (named procedures), `Data/` (YAML config), and `Templates/`. The whole system is conceived as seven components: Intelligence, Context, Personality, Tools, Security, Orchestration, Interface. v4.0.3 is the current release.
- **Agent model**: Hybrid. Named Agents (persistent identities with voice mappings and backstories defined in `Packs/Agents/src/AgentPersonalities.md`) + Custom/Dynamic Agents composed on-the-fly from a trait catalogue in `Packs/Agents/src/Data/Traits.yaml` (three dimensions: `expertise`, `personality`, `approach`, each with `prompt_fragment` snippets the composer concatenates). `ComposeAgent.ts` returns `{prompt, voice, voice_id, color}` and is dispatched via Claude Code's `general-purpose` subagent type. Per-agent "reading lists" live in `*Context.md` files that reference skills rather than duplicating them.
- **Memory model**: Three tiers — Session (native transcript), Work (`MEMORY/STATE/*` — project tracking with Ideal State Criteria), Learning (`MEMORY/LEARNING/SIGNALS/ratings.jsonl` — explicit 1-10 ratings + implicit sentiment). Statusline at `.claude/statusline-command.sh` reads those caches live.
- **Prompt pattern**: Every `SKILL.md` uses a YAML front-matter `description:` field loaded with trigger keywords ("USE WHEN first principles, decompose, ...") so Claude auto-routes via keyword match, then a table-based workflow-router maps request patterns to sub-skill files.
- **Distinctive patterns**:
  - *Ideal State Criteria (ISC)*: Before executing, the agent writes binary-testable success criteria, then the VERIFY phase tests each one. This converts "task done" from a vibe into a checklist.
  - *Trait-based agent composition* (`Traits.yaml` → `ComposeAgent.ts`): a small ~30-row table of prompt fragments yields 1000s of agent permutations without an agent-per-file explosion.
  - *SIGNALS capture* (ratings.jsonl) feeds a learning loop that evolves Steering Rules.
  - *Personal-vs-system skill split* (`_ALLCAPS` for personal, `TitleCase` for shareable) — a simple naming convention that lets users share their skill set without leaking personal data.
  - *SKILL.md description-field routing* — keyword list in front-matter is the auto-activation contract.
- **License**: MIT (Copyright 2025 Daniel Miessler). Safe to vendor/adapt.

## Source 2: SuperClaude_Framework

- **Architecture**: Python packaging of a Claude-Code "plugin" that injects behavioral context. Installer copies markdown to `~/.claude/`. Plugin content lives at `plugins/superclaude/` with subdirs `agents/` (20 persona `.md` files), `commands/` (29 `.md` files implementing `/sc:*` slash commands), `modes/` (7 behavioral modes), `core/` (FLAGS.md / PRINCIPLES.md / RULES.md / BUSINESS_SYMBOLS.md), and `hooks/hooks.json`. Also bundles an MCP roster via `.mcp.json` (Context7, Sequential-Thinking, Magic, Morphllm, Serena, Playwright, etc.).
- **Command model**: Each command is a markdown file with YAML front-matter (`name`, `description`, `category`, `complexity`, `mcp-servers`, `personas`) plus Triggers / Usage / Behavioral Flow / Tool Coordination / Key Patterns / Examples sections. Not executable — it's *context injection*. User types `/sc:brainstorm` and Claude loads the behavioral spec.
- **Persona model**: 20 specialist `.md` files (`security-engineer.md`, `backend-architect.md`, `refactoring-expert.md`, `root-cause-analyst.md`, `technical-writer.md`, `socratic-mentor.md`, etc.). Each file: Triggers, Behavioral Mindset, Focus Areas, Key Actions, Outputs, `Will/Will Not` Boundaries.
- **Flag system** (`core/FLAGS.md`): `--brainstorm --introspect --task-manage --orchestrate --token-efficient --think/--think-hard/--ultrathink --delegate --concurrency --loop --iterations --validate --safe-mode --uc` — a unified vocabulary across commands that maps to behavioral modes, MCP routing, and resource thresholds (Green/Yellow/Red zones at 75%/85% context).
- **Mode model**: 7 `MODE_*.md` files (Brainstorming, Introspection, Orchestration, Task_Management, Token_Efficiency, DeepResearch, Business_Panel). Introspection uses transparency markers 🤔🎯⚡📊💡. Task_Management defines a memory schema: `plan_*, phase_*, task_*, todo_*, checkpoint_*, blockers, decisions`.
- **Hooks**: `hooks/hooks.json` — SessionStart runs a shell init, Stop emits a "check for uncommitted changes" prompt, PostToolUse on `Write|Edit` injects "verify the edit" prompt.
- **Distinctive patterns**:
  - *Token Efficiency mode* (`modes/MODE_Token_Efficiency.md`): a symbol lexicon (→ ⇒ ← ⇄ » ∴ ∵ ✅❌⚠️🔄🚨⚡🔍🔧🛡️📦🎨🏗️) + abbreviation dictionary. Claimed 30-50% reduction, ≥95% info preservation.
  - *Flag→mode→MCP routing matrix* (`modes/MODE_Orchestration.md`): `--c7` → Context7, `--seq` → Sequential, `--magic` → Magic. Infrastructure keywords (nginx/traefik/docker) auto-trigger `WebFetch(official docs)` before recommendations — "Evidence > assumptions" enforced as a rule.
  - *PM Agent meta-layer*: a second agent activates *after* task completion for root-cause analysis and documentation evolution. Separation of doer from learner.
  - *Prompt-hooks* (non-shell hooks that inject a prompt into the model turn) — `hooks.json` supports `"type": "prompt"`, not just `"type": "command"`.
  - *Resource zones* tied to context-window percent — behavioral degradation is explicit, not ad-hoc.
- **License**: MIT (Copyright 2024 SuperClaude Framework Contributors). Safe to adapt.

## Source 3: caveman

- **Architecture**: Single-purpose Claude Code skill + plugin. Installs via `.claude-plugin/plugin.json` (SessionStart hook → `caveman-activate.js`; UserPromptSubmit hook → `caveman-mode-tracker.js`). Ships 4 slash commands (`/caveman`, `/caveman-commit`, `/caveman-review`, `/caveman-help`), 2 skills (`caveman`, `compress`), per-platform install/uninstall scripts, and AGENTS.md/GEMINI.md siblings to CLAUDE.md so the same behavior works across Codex, Gemini, Cursor, Windsurf, Cline.
- **Skill content** (`skills/caveman/SKILL.md`): terse rewriting rules with 6 intensity levels (`lite/full/ultra/wenyan-lite/wenyan-full/wenyan-ultra`). Has explicit **Auto-Clarity** section: "Drop caveman for: security warnings, irreversible action confirmations, multi-step sequences where fragment order risks misread" — the skill knows when to turn *itself* off.
- **Mode-tracker hook** (`hooks/caveman-config.js`): resolution order env → `$XDG_CONFIG_HOME/caveman/config.json` → `~/.config/caveman/config.json` → `%APPDATA%\caveman\config.json` → default. Symlink-safe flag writing (`O_NOFOLLOW`, refuses symlinked parent dir, atomic temp+rename, `0600`).
- **Evals harness** (`evals/`): three-arm control — `__baseline__` (no system prompt) vs `__terse__` ("Answer concisely.") vs `<skill>` ("Answer concisely.\n\nSKILL.md"). Runs the real `claude -p` CLI per prompt × arm, snapshots to `snapshots/results.json` committed to git, measures with tiktoken in CI. Explicit about what it *doesn't* measure: fidelity, latency, cost, cross-model. Acknowledges tiktoken ≠ Claude tokenizer.
- **Distinctive patterns**:
  - *Self-disabling skill* (Auto-Clarity) — skills that know when they're inappropriate.
  - *Honest control-arm evals* — the baseline-vs-terse-vs-skill three-arm design explicitly called out earlier versions that conflated "be terse" with "skill contribution." Rare intellectual honesty in benchmark design.
  - *Symlink-safe flag writes* — production-grade filesystem hygiene (O_NOFOLLOW, atomic, parent symlink check, best-effort silent fail).
  - *Cross-harness portability* — one skill, shipped simultaneously as `.claude-plugin/`, `.codex/`, `.cursor/`, `.windsurf/`, `.clinerules/`, `.agents/`, plus `CLAUDE.md` / `AGENTS.md` / `GEMINI.md`. Same behavior, six harnesses.
  - *XDG Base Directory + Windows APPDATA + macOS fallbacks* done correctly in ~40 lines.
- **License**: MIT (Copyright 2026 Julius Brussee). Safe to adapt.

---

## Miessler's blog essay — the PAI philosophy

- **Core idea**: "AI should magnify everyone, not just the top 1%." PAI is the infrastructure layer between a user's goals and a bare chatbot/agent. The three evolution levels: Chatbots (ask→answer→forget) → Agentic Platforms (ask→use tools→result) → PAI (observe→think→plan→execute→verify→**learn**→improve). The critical delta is the **learning loop** — feedback captured as signals, feeding back into the system's steering rules over time.

- **Three concrete mental models worth adopting**:
  1. **Current State → Desired State, with Ideal State Criteria.** Every non-trivial task starts by writing binary-testable ISC. "Verify" is not "looks good" — it's "did each ISC pass." Converts task completion from vibe to checklist.
  2. **Scaffolding > Model.** "The model stays the same. The scaffolding gets better every day." Anvil is already a scaffolding product — this is the first-principles justification for investing in context management, hooks, prompt templates, memory over chasing model capability.
  3. **Goal → Code → CLI → Prompts → Agents decision hierarchy.** "If you can solve it with a bash script, don't use AI." Always try deterministic solutions before probabilistic ones. Agents are the *last* resort, not the first. This directly informs hook/skill/agent ordering in Anvil.

---

## Adoption candidates (prioritized)

### TIER 1 — adopt for v2.2.8

**ADOPT-1: Trait-based agent composition** (from Miessler `Packs/Agents/src/Data/Traits.yaml` + `Packs/Agents/src/Tools/ComposeAgent.ts`)
- Why: Anvil's `AgentSummary` in `crates/commands/src/agents.rs:32` is a one-agent-per-file model. A 30-row traits catalogue (expertise × personality × approach) yields thousands of agent variants without a file per variant. Fills a gap elegantly.
- How: New file `crates/commands/src/traits.rs` holds a `Traits` struct deserialized from a YAML bundled under `crates/commands/assets/traits.yaml`. Add `compose_agent(traits: &[&str], task: &str)` returning `AgentSummary`-compatible prompt/model metadata. Extend `AgentSummary` with optional `composed_from: Vec<String>` field (backwards compatible). New slash command `/agent compose --traits security,skeptical,thorough --task "audit auth.rs"` wired through `crates/commands/src/handlers.rs`.
- License: MIT → MIT. Compatible. Adapt, do not vendor verbatim.
- Effort: M

**ADOPT-2: Skill YAML front-matter with trigger-keyword description** (from Miessler `Packs/*/src/SKILL.md` + SuperClaude `plugins/superclaude/commands/*.md`)
- Why: Anvil's `SkillSummary` in `crates/commands/src/agents.rs:42` carries only name + description + source. Adding a `triggers: Vec<String>` field from YAML front-matter lets the runtime auto-activate skills by keyword match, matching the PAI auto-routing UX. Today users must invoke skills explicitly.
- How: Extend the existing skill-loading code in `crates/commands/src/agents.rs` (SkillOrigin handling) to parse YAML front-matter (add `serde_yaml` or reuse existing `serde_yaml` if already a dep). Add trigger-matching pass in `crates/runtime/src/prompt.rs` before prompt assembly. Opt-in per skill — skills without front-matter behave as today.
- License: Pattern-level only, not code. No license issue.
- Effort: S

**ADOPT-3: Token-efficiency "caveman" mode as a bundled skill** (from caveman `skills/caveman/SKILL.md` + SuperClaude `modes/MODE_Token_Efficiency.md`)
- Why: Anvil supports 5 providers incl. Ollama where token budgets matter. A terse output mode is valuable and well-studied (caveman's evals show real savings over a "be terse" baseline). Zero cost to ship; off by default.
- How: Add `crates/commands/bundled/skills/terse.md` (MIT-attributed to caveman + SuperClaude) with YAML front-matter triggers `terse, brief, compress, less tokens`. Hook the symbol table from SuperClaude `modes/MODE_Token_Efficiency.md` as an optional appendix. Ship as a bundled skill, not a mode — keeps it opt-in and portable.
- License: Both MIT. Include attribution headers in `terse.md`. Do not copy caveman's wenyan variants (CJK-specific; not Anvil's audience).
- Effort: S

**ADOPT-4: Prompt-type hooks** (from SuperClaude `plugins/superclaude/hooks/hooks.json`)
- Why: Anvil's hook system in `crates/runtime/src/hooks.rs` + `crates/plugins/src/hooks.rs` currently runs *commands* on lifecycle events. SuperClaude's hooks.json adds `"type": "prompt"` — the hook injects a prompt into the next model turn instead of running a shell command. Cheap to add, large UX win (e.g. PostToolUse(Write|Edit) → "verify the edit"; Stop → "any uncommitted work?").
- How: Extend `PluginHooks` in `crates/plugins/src/manifest.rs:17` to accept a tagged enum `Hook::Command{..}` | `Hook::Prompt{prompt: String}`. Route prompt-hooks through the turn executor at `crates/runtime/src/conversation/turn_executor.rs` rather than `bash.rs`. Back-compat: bare strings remain command hooks.
- License: Pattern + ~50 lines of schema. MIT → MIT. No issue.
- Effort: M

**ADOPT-5: Three-arm eval harness for bundled skills** (from caveman `evals/`)
- Why: Anvil ships bundled skills and a marketplace (AnvilHub). Today there's no principled way to measure whether a skill helps vs. a plain instruction. Caveman's `__baseline__` / `__terse__` / `<skill>` design is the honest way to do this. Reviewers see through the "it's better than nothing" trick.
- How: New `crates/compat-harness/src/skill_evals.rs` (there's already a compat-harness crate) running skills through the existing Anvil runtime, snapshotting outputs to `target/skill-evals/snapshots.json`, measuring with an offline tokenizer. Opt-in `anvil skill-eval <skill>`. Not on by default.
- License: MIT. Adapt python harness to Rust; no code copied.
- Effort: M

### TIER 2 — queue for v2.2.9 / v2.3

**ADOPT-6: Ideal State Criteria pattern in task execution** (from Miessler — `Packs/Thinking/src/Science/METHODOLOGY.md` + PAI philosophy). Add an `anvil task` flow that writes binary-testable criteria before execution and verifies each one post-hoc. Lands in `crates/runtime/src/task.rs`. Effort: M.

**ADOPT-7: SIGNALS rating capture** (from Miessler `MEMORY/LEARNING/SIGNALS/ratings.jsonl`). Per-turn thumbs up/down stored to `~/.anvil/signals.jsonl`, consumed by `daily.rs` for the daily summary. A feedback loop Anvil doesn't have today. Lands in `crates/runtime/src/nominations.rs` (already similar). Effort: M.

**ADOPT-8: Resource-zone behavior tied to context window %** (from SuperClaude `modes/MODE_Orchestration.md`). Green (<75%), Yellow (75-85%), Red (>85%). Anvil already has `crates/runtime/src/usage.rs` and `usage_tracking.rs` — plumb thresholds into prompt assembly so the TUI status line shows zone and the model gets a hint to be terser as the window fills. Effort: S.

**ADOPT-9: Cross-harness portability** (from caveman's multi-harness shipping). Emit AGENTS.md / GEMINI.md / `.cursor/` / `.windsurf/` alongside Anvil's existing artifacts so Anvil skills are portable to other harnesses. Lands in `scripts/export-skills.sh` + `crates/commands/src/specs.rs`. Effort: S.

**ADOPT-10: PM-agent meta-layer** (from SuperClaude `plugins/superclaude/agents/pm-agent.md` + `pm_agent/` Python module). A second agent that runs post-turn to capture learnings and evolve skill docs. Conceptually similar to our daily summaries but per-turn. Lands in `crates/agents` (when that crate is built up) + `crates/runtime/src/daily.rs`. Effort: L.

**ADOPT-11: Skill Auto-Clarity pattern** (from caveman `skills/caveman/SKILL.md` Auto-Clarity section). Skills declare contexts where they should self-disable (security warnings, irreversible ops, multi-step sequences). Extend skill front-matter with `disable_when: [...]` field. Effort: S.

**ADOPT-12: Flag vocabulary standardization** (from SuperClaude `core/FLAGS.md`). Anvil has 101 slash commands but no unified modifier vocabulary. A shared flag set (`--think`, `--validate`, `--safe-mode`, `--uc`) across commands would reduce the flag-surface users have to memorize. Audit `crates/commands/src/handlers.rs` + `crates/commands/src/subcommands.rs` first. Effort: L.

### TIER 3 — interesting but intentional deviation (decline)

- **Hybrid named-agent + voice mapping** (Miessler `AgentPersonalities.md`, ElevenLabs voice IDs). Anvil is a CLI coding assistant; voice is out of scope. Decline.
- **TELOS life-OS skill** (Miessler `Packs/Telos/`). Life-coaching context is explicit non-goal for Anvil. Decline.
- **Business Panel** (SuperClaude `plugins/superclaude/agents/business-panel-experts.md`). Product/market persona simulation — out of scope for a coding CLI. Decline.
- **Wenyan (classical Chinese) compression modes** (caveman `skills/caveman/SKILL.md`). Fun, not Anvil's audience. Decline.
- **Bundled MCP opinionations** (SuperClaude `.mcp.json` hardcodes Context7/Magic/Morphllm/Serena). Anvil's MCP story is BYO — bundling opinionated servers locks users in. Decline.
- **Claude-Code-specific `~/.claude` directory layout** (Miessler + SuperClaude both assume it). Anvil lives at `~/.anvil` and treats Claude Code harness-compat as a feature (see `crates/compat-harness`), not a base dep. Decline.
- **Python installer + package layout** (SuperClaude `setup.py`, `pyproject.toml`). Anvil is a single 16 MB Rust binary — that's a feature. Decline.

---

## What Anvil already does better

- **Single 16 MB Rust binary vs Python/TypeScript installs** — no interpreter, no `bun install`, no `pip`. Miessler/SuperClaude both require a runtime; caveman requires Node.
- **5-provider failover (Anthropic/OpenAI/Google/xAI/Ollama)** — all three reference projects are Claude-Code-only or Claude-first. Anvil is harness-independent.
- **21-type encrypted vault (AES-256-GCM + Argon2id)** — PAI puts secrets in `.env`; SuperClaude has none; caveman has none. Anvil's vault is the mature answer.
- **Sandbox modes (`read-only` / `workspace-write` / `danger-full-access`)** — SuperClaude has `--safe-mode` as a behavioral flag only (model is asked nicely). Anvil enforces in the runtime. Real security vs vibes-security.
- **101 first-class slash commands + subcommands in Rust** — SuperClaude's 29 `/sc:*` commands are markdown context-injection. Anvil's are real code. Faster, more reliable, no prompt-injection risk from the command itself.
- **Remote control (6-digit paired WebSocket)** — nothing in any reference project.
- **QMD knowledge-base integration** — Miessler has SIGNALS; SuperClaude has Serena MCP; caveman has nothing. QMD is a proper BM25+vec+hyde local engine over markdown, which beats all three.
- **MCP stdio + HTTP + SSE transport support** — SuperClaude's `.mcp.json` only wires stdio. Anvil's `crates/runtime/src/mcp.rs` has all three.
- **LSP integration + session persistence + TUI with 16 themes** — no reference project has real LSP. SuperClaude "has Serena" for symbols, which is semantic-search, not true LSP.
- **Hooks system with permission-gating** — caveman's hooks are best-in-class on filesystem hygiene (symlink safety) but Anvil already has `permission_gate.rs` + `egress.rs` + `sandbox.rs` covering a broader threat surface.
- **Plugin marketplace (AnvilHub)** — Miessler has Packs in a monorepo; SuperClaude has a single-plugin design; caveman is one skill. AnvilHub as a discovery + install layer is ahead.
- **Bundled-vs-project-vs-user skill precedence** (`DefinitionSource` in `crates/commands/src/agents.rs:9`) — Anvil already has the layered-override model PAI hacks around with `SKILLCUSTOMIZATIONS/` paths.

Net: Anvil's engine, security, and transport story is ahead. The gaps are in *prompt-engineering patterns* (traits, triggers, modes, ISC) and *learning loops* (signals, evals) — which is exactly what the Tier 1/2 adoptions address.
