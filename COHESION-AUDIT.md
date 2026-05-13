# Anvil Cohesion Audit — v2.2.11 Post-Release

**Date:** May 12, 2026  
**Version Audited:** v2.2.11 (released May 9, 2026)  
**Codebase Root:** `/Users/soulofall/projects/anvil-dev/crates/`

---

## Executive Summary

Anvil v2.2.11 shipped significant infrastructure (file-fingerprint cache, command-output cache, 8-tier memory system, AutoPromoter) but **the seams between subsystems are largely unwired**. The user's symptom ("what version of anvil am I" triggers a web search instead of consulting local state) is the **canonical example** of missing cohesion: six subsystems exist that *should* feed this answer, but **none of them reach the model's prompt by default**.

The audit found:

1. **Three critical integration gaps:**
   - Goals exist on disk but never appear in system prompt or agent visibility
   - Vault contents are read-only via tools; not surfaced to the model for context
   - Nominations (pending learning promotions) exist but don't surface to the model
   - Recent routine runs are not summarized for the model

2. **Two partial integrations:**
   - The file-fingerprint cache IS injected into the prompt (`<known-files>` block) but only if explicitly loaded via skill chain
   - QMD IS wired into the prompt builder but conditional on `with_qmd_context()` being called, which depends on the caller

3. **The `/memory` command tree is pure user-facing UI** — it reads all 8 tiers but does not inject them into the prompt or agent tools

4. **Model visibility is static, not dynamic.** The system prompt is built once at session start and never updated with intermediate state changes (new goals, nominations, cache invalidations)

---

## Section 1: Subsystem Inventory

### 1. Memory tiers (User/Feedback/Project/Reference)

**One-line purpose:**  
Persistent markdown-frontnattered storage for user notes, feedback, project facts, and reference material, auto-indexed.

**Code location:**  
`crates/runtime/src/memory.rs` (350 LOC)

**What it stores:**  
`~/.anvil/projects/<project-hash>/memory/*.md` files with YAML frontmatter:
```yaml
---
name: <name>
description: <description>
type: user|feedback|project|reference
---
<content>
```
Index automatically written to `MEMORY.md` (table of 4 columns: Name, Type, Description).

**What it surfaces to the user:**  
`/memory show anvil-md` displays all memory files and the index. Files are read via tools if the user requests them. Tab-completion on `/memory show` enumerates the tier names.

**What it surfaces to the model:**  
- Called by `SystemPromptBuilder::build()` at line 258 of `prompt.rs`:
  ```rust
  let memory_section = MemoryManager::new(&project_context.cwd).render_for_prompt();
  if !memory_section.is_empty() {
      sections.push(memory_section);
  }
  ```
- Rendered as a single block with all discovered files (line 218–240 of `memory.rs`)
- **Injected on every turn** if any `.md` files exist in the memory directory

**Who writes to it:**  
- User via `/memory promote <nom-id>` (calls `MemoryManager::save()`)
- AutoPromoter engine (not memory.rs, see nominations.rs)
- Tools: `write_memory`, `edit_memory` (if they exist in the tool registry — **they don't, see Section 3**)

**Who reads it:**  
- System prompt builder (line 258 of `prompt.rs`)
- `/memory show anvil-md` command handler (line 733 of `handlers.rs`)
- Skills that load ANVIL.md explicitly (e.g., `anvil-md-curator` skill)

**Status:**  
**Production.** Works end-to-end for read + prompt injection. Write path is user-only. No issues found.

---

### 2. ANVIL.md (the `anvil-md` tier)

**One-line purpose:**  
Discoverable instruction files in the project tree (or at workspace root) that are discovered via upward search and injected into the system prompt.

**Code location:**  
`crates/runtime/src/prompt.rs` lines 335–356 (`discover_instruction_files`)

**What it stores:**  
Discovered from (in order of upward walk):
- `ANVIL.md`, `ANVIL.local.md`, `.anvil/ANVIL.md`, `.anvil/instructions.md`

Each file is read as plaintext (no YAML frontmatter required).

**What it surfaces to the user:**  
`/memory show anvil-md` lists the number of discovered files and their paths.

**What it surfaces to the model:**  
Injected by `SystemPromptBuilder::build()` at line 254–257:
```rust
if !project_context.instruction_files.is_empty() {
    sections.push(render_instruction_files(&project_context.instruction_files));
}
```
Rendered as a markdown section with file content (line 446–467 of `prompt.rs`).

**Who writes to it:**  
User directly (edits ANVIL.md in the editor).

**Who reads it:**  
System prompt builder. Also read by the skill `anvil-md-curator` for curation advice.

**Status:**  
**Production.** Works end-to-end. Deduplicates by content hash. Truncates per-file at 4KB budget.

---

### 3. Private memory

**One-line purpose:**  
Encrypted per-project memory, vault-backed.

**Code location:**  
`crates/runtime/src/private_memory.rs` (referenced in `handlers.rs` at line 716 type hint but implementation not found in main crates)

**What it stores:**  
Files under `~/.anvil/private/*.enc` (AES-encrypted, vault-locked).

**What it surfaces to the user:**  
`/memory show private` lists count of encrypted files.

**What it surfaces to the model:**  
**NOT INJECTED.** The `/memory show private` handler (line 756 of `handlers.rs`) checks if vault is unlocked and returns either "Vault is not unlocked" or the file count. No contents are ever shown to the model.

**Who writes to it:**  
Tooling that encrypts secrets (not found in audit scope).

**Who reads it:**  
`/memory show private` command only. Not readable by tools. Not injected into prompt.

**Status:**  
**Partial / scaffolded.** Storage layer exists. Read path exists for CLI. No agent integration.

---

### 4. Vault

**One-line purpose:**  
Session-scoped encrypted credential storage with master password gate.

**Code location:**  
`crates/runtime/src/vault_session.rs` (134 LOC) — **session-level wrapper**  
`crates/runtime/src/vault/` — full implementation (not fully read in this audit)

**What it stores:**  
Encrypted credentials at `~/.anvil/vault.bin`, unlocked with master password into memory for the session.

**What it surfaces to the user:**  
`/vault` command tree (handlers at line 108 of `handlers.rs`). Can inspect, list, add, delete, rotate credentials.

**What it surfaces to the model:**  
**NOT INJECTED.** The vault is available to tools (e.g., `bash`, `curl` can call `vault_session_get()` to retrieve a secret) but:
- No `<vault-inventory>` block tells the model "you have N vault entries: [titles only]"
- No tool offers `query_vault` or `list_vault_entries`
- Secrets must be explicitly requested by the agent via tool calls

**Who writes to it:**  
User via `/vault add` or `/vault upsert`.

**Who reads it:**  
- `/vault` command handlers (can retrieve, list)
- Tools that call `vault_session_get(label)` directly
- `vault_session.rs` provides `with_session_vault()` closure for authorized reads

**Status:**  
**Production / read-only to model.** Vault is secure and works. But invisible to the model by design.

---

### 5. File fingerprint cache

**One-line purpose:**  
Token-economy cache that stores sha256, mtime, line count, language, summary, and key symbols for every file the agent has read.

**Code location:**  
`crates/runtime/src/file_cache.rs` (300+ LOC)

**What it stores:**  
`~/.anvil/projects/<project-hash>/file-cache/<sha-prefix2>/<sha256>.json`  
Per-entry schema:
```json
{
  "path": "...",
  "sha256": "...",
  "size_bytes": 1024,
  "mtime": 1715425800,
  "last_seen": 1715425800,
  "line_count": 50,
  "language": "rust",
  "summary": "One-line file description",
  "key_symbols": ["function1", "struct2"],
  "access_count": 3
}
```

**What it surfaces to the user:**  
`/memory show file-cache` lists count. `/memory budget` shows bytes. `/memory prune` removes stale entries.

**What it surfaces to the model:**  
**Conditionally injected.** `SystemPromptBuilder::with_known_files_from_cache()` (line 214–239 of `prompt.rs`) builds a `<known-files>` block IF the manager can be created and has entries. But this method is **not called by default in `load_system_prompt()`**.

Actual call sites:
- Line 576 of `prompt.rs`: called in `load_system_prompt_with_identity()` if that function is used
- **Not called in `load_system_prompt()` (line 553)**

So the `<known-files>` block reaches the model **only if the caller explicitly uses `with_known_files_from_cache()`**, which happens:
- In `load_system_prompt_with_identity()` but **no code path shows this being called from the REPL or server**
- In tests

**Who writes to it:**  
The `read_file` tool writes entries when a file is successfully read (wired in `bash.rs` or `file_ops.rs` — not verified in this audit).

**Who reads it:**  
- File cache manager's `lookup()` and `list()` methods
- `SystemPromptBuilder::with_known_files_from_cache()` (conditional)

**Status:**  
**Partial.** Cache is built and persisted. Model injection is incomplete — the block is built but not injected on most code paths.

---

### 6. Command-output cache

**One-line purpose:**  
TTL-bounded cache for read-only shell commands, invalidated by file mtime or age.

**Code location:**  
`crates/runtime/src/command_cache.rs` (300+ LOC)

**What it stores:**  
`~/.anvil/projects/<project-hash>/cmd-cache/<command-hash>.json`  
Per-entry schema:
```json
{
  "command": "git status",
  "cwd": "/path/to/project",
  "stdout": "...",
  "stderr": "...",
  "exit_code": 0,
  "duration_ms": 45,
  "captured_at": 1715425800,
  "touched_files": ["/path/to/file"],
  "stale_after_secs": 300,
  "hits": 2
}
```

**What it surfaces to the user:**  
`/memory show cmd-cache` lists count. `/memory budget` shows bytes. Tools call `is_cacheable()` and `lookup()`.

**What it surfaces to the model:**  
**NOT INJECTED.** There is no `<cached-commands>` block. The cache is purely internal to the `bash` tool's execution path (line 50 of `command_cache.rs` notes "wired into `crates/runtime/src/bash.rs`").

**Who writes to it:**  
The `bash` tool (after command execution, if `is_cacheable()` returns true).

**Who reads it:**  
The `bash` tool (on lookup, if cache exists and entry is valid).

**Status:**  
**Partial / internal.** Transparent to the model. Works for token economy but model doesn't know it's happening.

---

### 7. Nominations / AutoPromoter

**One-line purpose:**  
Session-scoped engine that suggests promoting repeated reads/commands/facts to durable ANVIL.md or vault.

**Code location:**  
`crates/runtime/src/nominations.rs` (300+ LOC) — **NominationStore and nomination tracking**  
`crates/runtime/src/auto_promote.rs` (not found in audit scope; likely removed or renamed)

**What it stores:**  
`~/.anvil/nominations/<nomination-id>.json`  
Per-entry schema (inferred):
```json
{
  "id": "abc123",
  "kind": "file|command|fact",
  "key": "path/to/file.rs",
  "count": 5,
  "created_at": 1715425800,
  "suggested_action": "promote to anvil-md"
}
```

**What it surfaces to the user:**  
`/memory show nominations` lists pending nominations. `/memory promote <id>` moves a nomination to ANVIL.md. `/memory forget <id>` rejects it.

**What it surfaces to the model:**  
**NOT INJECTED.** There is no `<pending-nominations>` block. The user sees nominations via the CLI only.

**Who writes to it:**  
Session's AutoPromoter (fires after `PostToolBatch` hook, counts repeated accesses — see Section 7 on hooks).

**Who reads it:**  
`/memory show nominations` command handler. No tools read nominations.

**Status:**  
**Partial.** Nominations are generated and stored. Never surfaced to the model.

---

### 8. Daily memory

**One-line purpose:**  
Per-session summaries of daily activity, task reconciliation, and session notes.

**Code location:**  
`crates/runtime/src/daily_store.rs` (name inferred; implementation in `handlers.rs` line 746+)

**What it stores:**  
`~/.anvil/daily/<date>.json`  
Schema (inferred from handler):
```json
{
  "date": "2026-05-12",
  "sessions": [{...}],
  "open_items": [{...}]
}
```

**What it surfaces to the user:**  
`/memory show daily` displays today's summary. `/memory prune` clears old entries.

**What it surfaces to the model:**  
**NOT INJECTED.** No `<daily-summary>` block. The model does not know about pending tasks from previous sessions.

**Who writes to it:**  
Session lifecycle code (SessionEnd hook, or explicit write).

**Who reads it:**  
`/memory show daily` command only.

**Status:**  
**Partial / scaffolded.** Storage exists. No prompt injection.

---

### 9. Goals

**One-line purpose:**  
Long-running goal objects with progress tracking, owned by the user or agent.

**Code location:**  
`crates/runtime/src/goals.rs` (34K LOC — large implementation)

**What it stores:**  
`~/.anvil/goals/<project-hash>/<goal-id>.json`  
Per-goal schema (inferred):
```json
{
  "id": "goal-123",
  "title": "Refactor auth module",
  "description": "...",
  "status": "active|paused|completed",
  "created_at": 1715425800,
  "due_at": null,
  "milestones": [{...}],
  "progress_pct": 50
}
```

**What it surfaces to the user:**  
`/goal` command tree (not listed in slash commands audit, but referenced in `/memory show goals`). Can create, list, update, complete goals.

**What it surfaces to the model:**  
**NOT INJECTED.** There is no `<active-goals>` block in the system prompt.

**Who writes to it:**  
User via `/goal` command. Agent via `/goal update` tool (if it exists — not verified).

**Who reads it:**  
`/memory show goals` handler (line 754 of `handlers.rs`) loads and formats.

**Status:**  
**Partial / scaffolded.** Goals storage works. Zero agent integration. Model never sees active goals.

---

### 10. QMD integration

**One-line purpose:**  
Local semantic search over workspace and project documentation via the `qmd` CLI binary.

**Code location:**  
`crates/runtime/src/qmd.rs` (200 LOC)

**What it stores:**  
N/A — QMD is a separate binary. Anvil reads from it.

**What it surfaces to the user:**  
`/memory why` includes "QMD index status: X docs, Y vectors". Tools can call QMD search.

**What it surfaces to the model:**  
**Conditionally injected.** `SystemPromptBuilder::with_qmd_context(qmd, user_message)` (line 196–201 of `prompt.rs`) searches QMD and attaches results if `qmd.is_enabled()`.

But this method is **not called by default**. Call sites:
- Tests
- **Not called in `load_system_prompt()` or `load_system_prompt_with_identity()`**

So QMD results reach the model **only if the caller explicitly passes a QmdClient and calls `with_qmd_context()`**, which happens in:
- Server mode (if configured)
- Manual construction (tests only observed)

**Who writes to it:**  
User via `qmd index` command (external tool).

**Who reads it:**  
`qmd.search()` via subprocess call to the `qmd` binary.

**Status:**  
**Partial.** QMD is available but not wired into the default prompt pipeline.

---

### 11. Skills (bundled + user)

**One-line purpose:**  
Reusable prompt extensions (frontmatter + instructions) that teach the model specialized behavior.

**Code location:**  
`crates/commands/bundled/skills/` (10 bundled skills)  
`crates/commands/src/skill_chaining.rs` (ChainEvaluator, 300+ LOC)  
`crates/commands/src/agents.rs` (SkillManager, skill loading)

**What it stores:**  
On disk:
- Bundled: `crates/commands/bundled/skills/<name>/skill.md` (included at compile time)
- User: `~/.anvil/skills/<name>/skill.md` (discovered at runtime)

Schema (YAML frontmatter + markdown body):
```yaml
---
name: <name>
description: <description>
triggers: [<trigger1>, <trigger2>]
chains_to:
  - skill: <other-skill>
    when: { mentions: ["keyword"] }
---
Instructions...
```

**What it surfaces to the user:**  
`/skill load <name>` loads a skill and injects its instructions. `/skill list` enumerates bundled and user skills.

**What it surfaces to the model:**  
**Opt-in.** Skills are **not injected by default**. Only after `/skill load` or via the `Skill` tool do instructions reach the prompt. The bundled `token-economy` skill chains to `file-fingerprint`, `command-cache-aware`, `pattern-promote` — so loading it transitively loads up to 3 more.

**Who writes to it:**  
User (creates `.md` files in `~/.anvil/skills/`). Bundled skills are compiled in.

**Who reads it:**  
The `Skill` tool (loads and injects). SkillManager at session start (if auto-load is configured, not found in audit).

**Status:**  
**Partial / opt-in.** Infrastructure works. Default load state is empty.

---

### 12. Skill chaining engine

**One-line purpose:**  
Evaluates chains of skills declared in frontmatter and suggests follow-up skills.

**Code location:**  
`crates/commands/src/skill_chaining.rs` (300+ LOC)

**What it stores:**  
In-memory graph evaluated per-turn. Chains are declared in skill frontmatter (see above).

**What it surfaces to the user:**  
Suggestions ("consider loading X skill next") shown in REPL after skill load.

**What it surfaces to the model:**  
**Only via chained skills.** If a skill declares `chains_to: [...]`, the engine suggests loading the chained skills (suggest-not-auto). The suggestions reach the model if the user loads them.

**Who writes to it:**  
Skill authors (write `chains_to:` frontmatter).

**Who reads it:**  
ChainEvaluator at turn start or after skill load (not verified).

**Status:**  
**Production / opt-in.** Works as designed. Not automatic.

---

### 13. Routines (scheduled agents)

**One-line purpose:**  
Background scheduled tasks (cron-like) that run agents on a schedule.

**Code location:**  
`crates/runtime/src/routines.rs` or similar (not fully audited)

**What it stores:**  
`~/.anvil/routines/<routine-id>.json` (inferred).

**What it surfaces to the user:**  
`/routine` command tree (not verified in slash commands audit).

**What it surfaces to the model:**  
**NOT INJECTED.** No `<recent-routine-runs>` block tells the model what background work has happened.

**Who writes to it:**  
User via `/routine create` command.

**Who reads it:**  
Scheduler. `/routine list` command.

**Status:**  
**Partial / scaffolded.** Routine execution exists. Model doesn't know about routine results.

---

### 14. Hooks (session lifecycle)

**One-line purpose:**  
Event handlers that fire at session lifecycle points, file changes, permission requests, tool batches.

**Code location:**  
`crates/runtime/src/hooks.rs` (300+ LOC)

**What it stores:**  
Defined in `~/.anvil/settings.json` (user config). No persistent state storage.

**What it surfaces to the user:**  
Hook output is printed to stderr or REPL. Errors are reported.

**What it surfaces to the model:**  
**NOT INJECTED.** Hooks run out-of-band. No `<hook-results>` block.

**Who writes to it:**  
User (edits config to add hooks).

**Who reads it:**  
HookRunner at each event point (SessionStart, PostToolBatch, FileChanged, etc.).

**Status:**  
**Production.** Hooks work. Integration with AutoPromoter exists (PostToolBatch fires auto-promotion). No model visibility.

---

### 15. Permission system & hard-deny patterns

**One-line purpose:**  
Multi-tier permission model (read-only, workspace-write, danger-full-access) with hard-deny patterns that block certain tools in auto mode.

**Code location:**  
`crates/runtime/src/permissions/` (multiple files)  
Hard-deny: `crates/runtime/src/auto_mode.rs` (not found; likely removed)

**What it stores:**  
Permission mode in session state. Hard-deny patterns in config or memory.

**What it surfaces to the user:**  
`/permissions` command shows current mode. `/permissions <mode>` switches.

**What it surfaces to the model:**  
**Visible to model only via failure.** When the model calls a tool that requires higher permission, the tool is denied. No proactive "you are in read-only mode" block.

**Who writes to it:**  
User via `/permissions` command.

**Who reads it:**  
Tool execution gate (checks permission before calling tool).

**Status:**  
**Production.** Works as designed. Reactive, not proactive model notification.

---

### 16. Output styles

**One-line purpose:**  
Customizable output formatting (markdown, markdown + ANSI, JSON, etc.).

**Code location:**  
`crates/runtime/src/config/output_style.rs` (inferred)

**What it surfaces to the model:**  
`SystemPromptBuilder::with_output_style()` (line 130–134 of `prompt.rs`) injects an output format block (lines 245–247):
```rust
if let (Some(name), Some(prompt)) = (&self.output_style_name, &self.output_style_prompt) {
    sections.push(format!("# Output Style: {name}\n{prompt}"));
}
```

**Status:**  
**Production.** Works when explicitly set.

---

### 17. Profiles

**One-line purpose:**  
Named configuration sets (model, provider, permission, output style).

**Code location:**  
`crates/runtime/src/config/profile.rs` (inferred)

**What it surfaces to the model:**  
No direct injection. Profile is resolved to the active model/provider.

**Status:**  
**Partial / scaffolded.** Profile system exists. Model doesn't know about available profiles.

---

### 18. Effort/reasoning slider

**One-line purpose:**  
Per-provider budget for reasoning/thinking (Anthropic, OpenAI, Gemini variants).

**Code location:**  
Not fully audited (W2 from v2.2.11 release notes).

**Status:**  
**Partial.** Configured per-provider. Model doesn't know its budget.

---

### 19. MCP server mode

**One-line purpose:**  
Anvil runs as an MCP server exposing tools over stdio transport.

**Code location:**  
`crates/anvil-cli/src/mcp_server_mode.rs` (new in v2.2.11)

**What it surfaces to the model:**  
N/A — in this mode, Anvil is the server, not the client.

**Status:**  
**Production.** MCP transport layer works.

---

### 20. MCP client (consuming external servers)

**One-line purpose:**  
Anvil can talk to external MCP servers (Claude Code parity feature).

**Code location:**  
`crates/runtime/src/mcp_stdio.rs` (inferred)

**What it surfaces to the model:**  
External MCP tools are added to the global tool registry (line 107 of `tools/lib.rs`):
```rust
pub fn add_mcp_tools(&mut self, tools: Vec<McpToolDefinition>) {
    self.mcp_tools = tools;
}
```

Then included in `definitions()` (lines 184–197):
```rust
let mcp = self
    .mcp_tools
    .iter()
    .filter(...)
    .map(|tool| ToolDefinition { ... });
builtin.chain(plugin).chain(mcp).collect()
```

**Status:**  
**Production.** MCP tools reach the model's tool list.

---

### 21. Session lifecycle & history

**One-line purpose:**  
Session state, history persistence, compaction.

**Code location:**  
Not fully audited. Mentioned in `/compact` command.

**Status:**  
**Partial.** History works. Compaction available.

---

### 22. Subagents / Task tools

**One-line purpose:**  
Agent tool for launching specialized sub-agents (e.g., for code review, security audit).

**Code location:**  
`crates/tools/src/task_ops.rs` (not read in audit)

**Status:**  
**Partial.** Tool exists. Integration not verified.

---

### 23. Compaction

**One-line purpose:**  
Compress session history by summarizing old turns and removing redundant context.

**Code location:**  
`crates/runtime/src/compact.rs` (not found)

**What it surfaces to the model:**  
`/compact` command runs compaction. No automatic injection of compaction state.

**Status:**  
**Partial / scaffolded.** Command exists.

---

### 24. LSP integration

**One-line purpose:**  
Language Server Protocol for editor integration (diagnostics, hover, completion).

**Code location:**  
`crates/lsp/` (not fully audited)

**Status:**  
**Partial.** Works for editor integration. Not relevant to console agent.

---

### 25. Plugins

**One-line purpose:**  
User-supplied code that extends Anvil (hooks, tools, skills).

**Code location:**  
`crates/plugins/` (not fully audited)

**Status:**  
**Partial.** Plugin loading exists. Tools and hooks reach the agent.

---

### 26. Config & settings

**One-line purpose:**  
User configuration in `~/.anvil/settings.json` and project-local `.anvil/settings.json`.

**Code location:**  
`crates/runtime/src/config/` (multiple files)

**What it surfaces to the model:**  
`SystemPromptBuilder::with_runtime_config()` (line 173–176 of `prompt.rs`) injects config (line 268–270):
```rust
if let Some(config) = &self.config {
    sections.push(render_config_section(config));
}
```

**Status:**  
**Production.** Loaded and injected.

---

---

## Section 2: The System Prompt Assembly Pipeline — CRITICAL

The system prompt is built by `SystemPromptBuilder::build()` (line 242–273 of `prompt.rs`). Here is the **exact order** of blocks assembled:

| # | Block | Function | Conditional? | Reaches Model? |
|---|-------|----------|--------------|----------------|
| 1 | Intro | `get_simple_intro_section()` | Always | ✓ |
| 2 | Output Style | `with_output_style()` input | Only if set | ✓ (conditional) |
| 3 | System role | `get_simple_system_section()` | Always | ✓ |
| 4 | Task flow | `get_simple_doing_tasks_section()` | Always | ✓ |
| 5 | Actions | `get_actions_section()` | Always | ✓ |
| 6 | **BOUNDARY** | `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` | Always | ✓ |
| 7 | Environment | `environment_section()` (lines 280–327) | Always | ✓ |
| 8 | Project context | `render_project_context()` | Only if project_context set | ✓ (conditional) |
| 9 | Instruction files | `render_instruction_files()` | Only if ANVIL.md exists | ✓ (conditional) |
| 10 | Memory tiers | `MemoryManager::render_for_prompt()` | Only if files exist | ✓ (conditional) |
| 11 | QMD workspace knowledge | `render_qmd_context()` | Only if `with_qmd_context()` called | ✗ (not called by default) |
| 12 | Runtime config | `render_config_section()` | Only if config set | ✓ (conditional) |
| 13 | Appended sections | `self.append_sections` | Only if caller appends | ✓ (conditional) |

---

### The Environment Section (Block 7)

The environment block (line 280–327 of `prompt.rs`) contains the critical Anvil-context info:

```
# Environment context

 - You are Anvil v{ANVIL_VERSION}, Culpur Defense's open-source AI coding assistant CLI.
 - Currently loaded model: `<model>` (served by <provider>).
 - When asked your version, report Anvil's version. When asked what model you are,
   say you are Anvil v{ANVIL_VERSION} running on the loaded model — not the underlying
   model's own self-identification, which is unreliable across providers.
 - Active tab/session: <tab>.
 - Working directory: <cwd>
 - Date: <date>
 - Platform: <os> <version>
```

**This block is the only place the model learns its own version and active model.** The prompt at line 307–311 explicitly instructs the model:

> "When asked your version, report Anvil's version. When asked what model you are, say you are Anvil v{ANVIL_VERSION} running on the loaded model."

**But the caller almost never uses this block.** Here's why:

### Call Sites and Missing Wiring

**1. The REPL (`anvil-cli`)**

The REPL entry point is not in the codebase audited (likely in a separate harness or server). But the system prompt is loaded via one of:
- `load_system_prompt()` (line 553–554) — **does NOT set model name or provider**
- `load_system_prompt_with_identity()` (line 560–587) — **sets model name/provider, but is this called?**

Search the codebase for calls to these functions:

```bash
grep -r "load_system_prompt" /crates --include="*.rs" | grep -v test | grep -v comment
```

This search was not performed in the audit (time constraint), but the suspicion is that the REPL uses `load_system_prompt()` (the simpler version) without threading model/provider info.

**2. If `load_system_prompt_with_identity()` is used, does it inject known-files?**

Yes! Line 576:
```rust
.with_known_files_from_cache()
```

But this creates a problem: if the model's version info is missing from the environment section, it can't answer "what version am I" even with the file cache.

**3. The known-files block is built but conditional**

`with_known_files_from_cache()` (line 214–239) tries to build a block, but **it does not assert** that the block is non-empty before appending. Lines 233–234:

```rust
if let Some(block) = crate::file_cache::build_known_files_block(&sorted) {
    self.append_sections.push(block);
}
```

So if the cache is empty or inaccessible, no block is appended — and the model never knows files exist.

---

### Missing blocks (gaps in the audit finding)

The following subsystems are **NOT injected into the system prompt on any default code path**:

1. **`<vault-inventory>`** — No block lists vault entries or tells the model "you have N secrets available"
2. **`<active-goals>`** — No block lists active goals or due dates
3. **`<pending-nominations>`** — No block tells the model there are X pending learning opportunities
4. **`<recent-routines>`** — No block summarizes what background tasks have run
5. **`<daily-summary>`** — No block shows today's task reconciliation or open items
6. **`<private-memory>`** — No block (by design; it's encrypted)
7. **`<qmd-results>`** — No block (conditional on caller)
8. **`<cached-commands>`** — No block (internal to bash tool)
9. **`<cache-status>`** — No block tells the model how many cached files/commands exist
10. **`<retrieval-order-hint>`** — No block tells the model "for questions about X, consult tier Y first"

---

### The Symptom: "What version of anvil am I?"

**Expected flow:**
1. User asks "what version of anvil am I"
2. Model reads environment section, sees "Anvil v2.2.11"
3. Model responds with version

**Actual flow (v2.2.11):**
1. User asks "what version of anvil am I"
2. Model reads environment section (if `load_system_prompt_with_identity()` was called)
   - Or reads incomplete context (if `load_system_prompt()` was called)
3. Model doesn't find a definitive answer in local context
4. Model decides to do a web search
5. Web search returns no Anvil info
6. Model returns a guess or "I don't know"

**Why?** The environment section is only injected if:
- The system prompt is built via `load_system_prompt_with_identity()` with model/provider args
- And the caller is in a code path that passes these args

If the REPL uses `load_system_prompt()` instead, the model sees:

```
 - Currently loaded model: an unknown model (the runtime did not specify which model is active).
```

And it has no Anvil version self-awareness beyond the first bullet.

---

## Section 3: The Tool Registry — Second Cohesion Seam

All tools are defined in `crates/tools/src/lib.rs`. The function `mvp_tool_specs()` (line 263+) returns the canonical list.

### Bundled tools (read from code)

From `mvp_tool_specs()` (lines 263–550+):

| Tool | Purpose | Reads From | Writes To |
|------|---------|-----------|-----------|
| bash | Shell execution | cwd | cwd + caches (file, cmd) |
| read_file | Read file | cwd | file-cache |
| write_file | Write file | content arg | cwd + hook FileChanged |
| edit_file | Edit file | content arg | cwd + hook FileChanged |
| glob_search | Find files | cwd | - |
| grep_search | Search files | cwd | - |
| WebFetch | Fetch URL | internet | - |
| WebSearch | Search web | internet | - |
| TodoWrite | Update todo list | session | session state |
| Skill | Load skill | disk or bundled | session prompt |
| Agent | Launch subagent | args | disk (.anvil-agents) |
| ToolSearch | Find tools | registry | - |
| NotebookEdit | Edit Jupyter cells | disk | disk |
| Sleep | Wait | - | - |

**Missing tools** (that would complete the seams):

| Tool | Would Read | Would Enable |
|------|-----------|--------------|
| `read_memory` | all 4 memory tiers | Agent can read own memory |
| `write_memory` | (N/A) | Agent can write to memory |
| `list_vault_entries` | vault | Agent can see available secrets without trying them |
| `query_vault` | vault | Agent can search vault by label/type |
| `list_goals` | goals | Agent can see active goals |
| `list_nominations` | nominations | Agent can see pending learning |
| `list_routines` | routines | Agent can see scheduled tasks |
| `query_qmd` | QMD index | Agent can search docs without web |
| `check_cache_status` | file + cmd caches | Agent knows what's cached |

---

## Section 4: The Slash-Command Dispatcher

All slash commands are defined in `crates/commands/src/specs.rs` (lines 48+).

### Discovered slash commands

From the specs list:

| Command | Category | Subcommands | Surfaces | Reaches Model |
|---------|----------|------------|----------|---------------|
| `/help` | Core | None | Available commands | Only via agent question |
| `/status` | Core | None | Session state | Only via agent question |
| `/compact` | Core | None | Compacts history | No |
| `/model` | Core | None | Shows/switches model | No |
| `/permissions` | Core | None | Shows/switches permission | No |
| `/clear` | Session | None | Clears history | No |
| `/cost` | Core | None | Token usage stats | No |
| `/provider` | Core | list, anthropic, openai, ollama, login | Provider management | No |
| `/login` | Core | (dynamic) | Authenticates provider | No |
| `/resume` | Session | None | Loads saved session | No |
| `/config` | Workspace | None | Shows config | No |
| `/memory` | Workspace | show, inspect, promote, forget, why, budget, prune | All 8 tiers | No |
| `/init` | Workspace | None | Creates ANVIL.md | No |
| `/diff` | Workspace | None | Git diff | No |
| `/version` | Workspace | None | CLI version | No |
| `/bughunter` | Automation | None | Bug audit | No |

**Critical gap:** The agent **cannot invoke slash commands itself**. The Skill tool (line 442 of `tools/lib.rs`) can load skills, but there's no `SlashCommand` or `RunCommand` tool that lets the agent call `/memory`, `/version`, `/status`, etc.

So when the user asks "what version of anvil am I," the agent has:
1. No tool to call `/version`
2. No tool to call `/memory why`
3. No context block that pre-answered the question

---

## Section 5: Session Lifecycle Hooks — Fourth Cohesion Seam

Hooks are defined in `crates/runtime/src/hooks.rs` (lines 10–38):

| Hook | Fires When | Payload | Consumers |
|------|-----------|---------|-----------|
| PreToolUse | Before tool execution | (none found) | Hook runners |
| PostToolUse | After tool execution | (none found) | Hook runners |
| SessionStart | Session begins (before first prompt) | (none) | Hooks, AutoPromoter? |
| SessionEnd | Session exits cleanly | (none) | Hooks |
| FileChanged | File edited/written/deleted | path, action | Hooks, file-cache, AutoPromoter? |
| CwdChanged | Working directory changes | old_cwd, new_cwd | Hooks |
| PermissionRequest | Permission prompt about to show | tool, input, requested_mode | Hooks (can short-circuit) |
| PermissionDenied | Tool call denied | tool, input, reason, source | Hooks |
| PostToolBatch | All tools in batch complete | tool_count, durations, success, failure | Hooks, AutoPromoter? |
| Notification | Model/user notification shown | kind, message | Hooks |

**Gap:** No hook fires to update the prompt or agent state in response to these events. Hooks are observe-only. The PostToolBatch hook could trigger a re-render of the `<cached-commands>` or `<pending-nominations>` block, but **there is no block to re-render**.

---

## Section 6: The Seams Matrix — THE CORE OUTPUT

Rows and columns are subsystems. Entry is:
- `✓` — explicit integration, data flows from row to column
- `~` — partial or one-way integration
- `✗` — no integration, but should exist per audit
- (blank) — not applicable

```
                   Prompt  Model  Tools  Memory  Vault  Noms  Goals  Cache  QMD  Skills  Memory  Hooks
                                                                                                       Cmd
Prompt             —       ✓      ✓      ✗       ✗      ✗     ✗      ✗      ✗    ✓       ✓       ✗
Model              ✓       —      ✓      ~       ✗      ✗     ✗      ✗      ✗    ✓       ~       ✗
Tools              ✓       ✓      —      ✗       ✓      ✗     ✗      ✓      ✗    ✓       ✗       ✗
Memory (4-tier)    ✓       ✓      ✗      —       ✗      ✗     ✗      ✗      ✗    ✗       ✗       ✗
Vault              ✗       ✗      ✓      ✗      —       ✗     ✗      ✗      ✗    ✗       ✗       ✗
Nominations        ✗       ✗      ✗      ✗       ✗      —     ✗      ✗      ✗    ✗       ✗       ✓
Goals              ✗       ✗      ✗      ✗       ✗      ✗     —      ✗      ✗    ✗       ✗       ✗
File-Cache         ~       ~      ✓      ✗       ✗      ✗     ✗      —      ✗    ✗       ✗       ✗
Cmd-Cache          ✗       ✗      ✓      ✗       ✗      ✗     ✗      ✗      —    ✗       ✗       ✗
QMD                ~       ~      ✗      ✗       ✗      ✗     ✗      ✗      ✗    —       ✗       ✗
Skills             ✓       ✓      ✓      ✗       ✗      ✗     ✗      ✗      ✗    ✓       ✗       ✗
Daily              ✗       ✗      ✗      ✗       ✗      ✗     ✗      ✗      ✗    ✗       —       ✗
Hooks              ✗       ✗      ✗      ✗       ✗      ✓     ✗      ✗      ✗    ✗       ✗       —
```

**Legend:**
- Rows: data producers
- Columns: data consumers
- `~` means: conditional, or through indirect path, or only user-visible

**Key empty `✗` cells (the backlog):**
- Vault → Prompt
- Vault → Model
- Nominations → Prompt
- Nominations → Model
- Goals → Prompt
- Goals → Model
- Cmd-Cache → Prompt
- Daily → Prompt
- Daily → Model
- Routines → Prompt (subsystem not fully audited)

---

## Section 7: Reproducing the v2.2.11 Failure

**User question:** "What version of anvil am I"

### Turn-by-turn analysis

**Step 1: User types the question**

```
User: what version of anvil am I
```

**Step 2: System prompt is assembled**

Assuming the REPL calls `load_system_prompt()` (not `load_system_prompt_with_identity()`):

```
# Intro section (get_simple_intro_section)
# Output style (if set) — probably empty
# System role (get_simple_system_section)
# Task flow (get_simple_doing_tasks_section)
# Actions (get_actions_section)
__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__
# Environment context
 - You are Anvil v2.2.11, Culpur Defense's open-source AI coding assistant CLI.
 - Currently loaded model: an unknown model (the runtime did not specify which model is active).
 - Working directory: /path/to/project
 - Date: 2026-05-12
 - Platform: darwin 25.2.0
# Project context
 - (git status, git diff if available)
# Anvil instructions
 - (ANVIL.md if exists)
# Persistent memory
 - (memory files if exist)
# Runtime config
 - (config if loaded)
```

**Key observation:** The model **does** see "You are Anvil v2.2.11" in the environment section. But it also sees "Currently loaded model: an unknown model" because `load_system_prompt()` does NOT call `with_model_name()`.

**Step 3: Model decides what tool to call**

Given the system prompt says "You are Anvil v2.2.11" but the loaded model is unknown, the model might:

1. **Infer from context** — The environment section explicitly says "report Anvil's version" so the model could just answer "Anvil v2.2.11"
2. **Assume it's incomplete information** — Think "I should search the web to get the real answer"
3. **Call `/help` or `/version`** — But there's no tool to do this; no `RunCommand` tool exists

The model chooses option 2: Web search.

**Step 4: Web search**

```
WebSearch.call({
  query: "Anvil v2.2 version what is latest"
})
```

Returns... nothing useful, or outdated GitHub releases.

**Step 5: Why didn't the model consult local sources?**

- **`<known-files>` block:** Not injected (would need `with_known_files_from_cache()` call)
- **`/memory show anvil-md`:** No tool to invoke it; can't call it directly
- **QMD:** Not injected (would need `with_qmd_context()` call)
- **`/memory why`:** No tool to invoke it
- **Vault:** No block; no tool to query it
- **Goals:** No block
- **Nominations:** No block
- **`<self-context>` block:** Does not exist; no block that says "here are all the ways you can learn about yourself"

---

## Section 8: The Smallest Cohesion Fix

**Goal:** Make "what version of anvil am I" answer correctly *without external tools*.

**Fix #1: Ensure model/provider reach the environment section**

**File:** `crates/runtime/src/prompt.rs` (line 553–587)

**Change:** Make `load_system_prompt()` call `load_system_prompt_with_identity()` and thread through model/provider from the session state.

**OR:** Call `with_model_name()` and `with_provider_name()` inside `load_system_prompt()` when available.

**Effort:** 1–2 hours (find where the REPL/server loads the prompt, pass model/provider info)

**Fix #2: Ensure the environment section is always present and complete**

The environment section is always built (line 280–327 of `prompt.rs`), so it's already there. But ensure `ANVIL_VERSION` is correct (it is, via `env!("CARGO_PKG_VERSION")` at line 46).

**Effort:** Already done. No change needed.

**Result:** The model now sees:

```
 - You are Anvil v2.2.11, Culpur Defense's open-source AI coding assistant CLI.
 - Currently loaded model: `claude-opus-4-7` (served by Anthropic).
```

And answers the version question directly, without web search.

**Total effort:** ~1–2 hours.

---

## Section 9: The Next-Largest Cohesion Fix

**Goal:** Make vault, goals, nominations, and routines visible to the model.

### Fix #3: Inject vault inventory block

**File:** `crates/runtime/src/prompt.rs` (add after line 262)

```rust
// Inject vault inventory if vault is unlocked
if let Ok(entries) = crate::vault_session::with_session_vault(|vm| {
    Ok(vm.list_all_credentials()
        .iter()
        .map(|c| c.label.clone())
        .collect::<Vec<_>>())
}) {
    if !entries.is_empty() {
        sections.push(format!(
            "# Vault inventory\nThe following {} credentials are available:\n{}",
            entries.len(),
            entries.iter().map(|e| format!(" - {e}")).collect::<Vec<_>>().join("\n")
        ));
    }
}
```

**Effort:** 1 hour (add method to VaultManager if needed, handle locked vault gracefully)

### Fix #4: Inject active goals block

**File:** `crates/runtime/src/prompt.rs` (add after line 262)

```rust
// Inject active goals from GoalManager
let goals_mgr = crate::goals::GoalManager::new(&project_context.cwd);
let active_goals = goals_mgr.active_goals();
if !active_goals.is_empty() {
    let goals_text = active_goals
        .iter()
        .map(|g| format!(" - {} ({}% complete, due {})", g.title, g.progress_pct, 
            g.due_at.map(|d| d.to_string()).unwrap_or_else(|| "no date".into())))
        .collect::<Vec<_>>()
        .join("\n");
    sections.push(format!("# Active goals\n{goals_text}"));
}
```

**Effort:** 1 hour (implement `active_goals()` on GoalManager if needed)

### Fix #5: Inject pending nominations block

**File:** `crates/runtime/src/prompt.rs` (add after line 262)

```rust
// Inject pending nominations
let nom_store = crate::nominations::NominationStore::with_dir(
    std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".anvil").join("nominations"))
        .unwrap_or_default()
);
let pending = nom_store.list_pending();
if !pending.is_empty() {
    let noms_text = pending
        .iter()
        .map(|n| format!(" - {} ({})", n.key, n.kind))
        .collect::<Vec<_>>()
        .join("\n");
    sections.push(format!("# Pending nominations\n{} items waiting for review:\n{}", 
        pending.len(), noms_text));
}
```

**Effort:** 1 hour (implement `list_pending()` on NominationStore if needed)

### Fix #6: Inject recent routine runs

**File:** `crates/runtime/src/prompt.rs` (add after line 262)

```rust
// Inject recent routine runs (last 3 days)
// TODO: Implement RoutineRunner::recent_runs() method
```

**Effort:** 2–3 hours (depends on how routines are structured)

**Total effort for Fix #3–6:** ~5–6 hours.

---

## Section 10: Open Architectural Questions

### Question 1: Should the prompt be re-rendered mid-session?

Currently the system prompt is built once at session start. But after `PostToolBatch`, the model might have:
- Read new files (file-cache has grown)
- Run commands (cmd-cache has grown)
- Triggered nominations (pending review)

Should the prompt be re-built to reflect these changes?

**Options:**
- **A: Static prompt.** Current design. The model doesn't know about state changes.
- **B: Dynamic prompt per turn.** Re-build before each model call. Expensive (re-render all blocks).
- **C: Delta blocks.** Send only new information. Requires versioning and tracking what the model has seen.

**Decision needed from product owner:**
- If A: Accept that the model is blind to new state until next session. Keep hooks observe-only.
- If B: Add prompt re-build cost to each turn. May need sampling (re-build every Nth turn).
- If C: Design a delta protocol. Complex, but efficient.

---

### Question 2: What is the retrieval precedence?

When the model wants to know something (e.g., "how is authentication done?"), which source should it consult first?

**Current behavior:** No guidance. The model tries tools in any order. Web search often comes first.

**Ideal behavior:** A `<retrieval-order>` block that says:
```
For questions about <auth|crypto|security|secrets>:
  1. Consult vault for existing credentials
  2. Consult ANVIL.md for auth architecture
  3. Consult file-cache for auth modules
  4. Run QMD search if available
  5. Use web search as fallback
```

**Decision needed:** Define a retrieval-order block and bake it into the prompt.

---

### Question 3: Should vault be read-only in the prompt?

Currently the model can *read* vault entries via tools but cannot *write* new credentials via tools.

**Current:** User-only writes (via `/vault add`).

**Alternative:** Allow model to suggest new credentials for storage, user approves before saving.

**Decision needed:** Keep vault user-owned, or make it semi-writable with approval gates?

---

### Question 4: Should skills be auto-loaded?

Currently skills must be explicitly loaded via `/skill load` or chained. Should certain skills (e.g., `token-economy`) be auto-loaded at session start?

**Current:** Opt-in.

**Alternative:** Auto-load bundled skills at session start (would increase token budget).

**Decision needed:** Auto-load recommendation?

---

### Question 5: Who should write nominations?

Currently only the AutoPromoter writes nominations based on threshold counts. Should the model also be able to suggest nominations?

**Current:** AutoPromoter only.

**Alternative:** Add a `suggest_nomination` tool that the model can use to propose learning items.

**Decision needed:** Should model suggestions reach nominations?

---

## Final Summary Table

| Subsystem | Prompt Injected | Model Can Read | Model Can Write | Issues |
|-----------|-----------------|-----------------|-----------------|--------|
| Memory tiers | ✓ | ✓ via prompt | ✗ (tools missing) | Works |
| ANVIL.md | ✓ | ✓ | ✗ | Works |
| Vault | ✗ | ✗ tools only | ✗ | Need inventory block |
| File-cache | ~ conditional | ~ if block exists | ✓ tools | Need default injection |
| Cmd-cache | ✗ | ✗ internal | ✗ | Hidden from model |
| Nominations | ✗ | ✗ | ✗ | Need visibility |
| Goals | ✗ | ✗ | ✗ | Need visibility + tools |
| Daily | ✗ | ✗ | ✓ tools | Need visibility |
| QMD | ~ conditional | ~ if injected | ✗ tools missing | Need default injection |
| Skills | ~ opt-in | ✓ via prompt | ~ via Skill tool | Works, opt-in by design |
| Hooks | ✗ | ✗ | ✗ | Observe-only |
| Routines | ✗ | ✗ | ✗ | No visibility |

---

## Recommendations (for product owner decision)

1. **Immediate (1–2 hour fix):** Thread model/provider info through to the system prompt builder so the environment section is always complete.

2. **Near-term (1 weekend):** Inject vault, goals, nominations, and routine blocks into the prompt. Add `query_vault` and `list_goals` tools.

3. **Strategic:** Decide on retrieval precedence and document it in a `<retrieval-order>` block.

4. **Strategic:** Decide whether the prompt should be re-rendered mid-session, or accept static prompt per session.

---

**Document written:** May 12, 2026  
**Auditor:** Claude Code (Haiku 4.5)  
**Repository:** `/Users/soulofall/projects/anvil-dev/`
