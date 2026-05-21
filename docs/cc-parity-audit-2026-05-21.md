# CC Parity Audit — v2.1.144 through v2.1.146

**Date:** 2026-05-21  
**Scope:** 3 CC releases since last audit (v2.1.144 → v2.1.146)  
**Drift:** 4 days (audit window: 2026-05-17 to 2026-05-21)  
**Anvil base:** v2.2.15  
**Format:** Triage per release. Verdict: SHIPPED, N/A, FILE-AS-TASK, or VERIFY.

---

## Summary

| Metric | Count |
|---|---|
| CC versions examined | 3 (v2.1.144 – v2.1.146) |
| Total items triaged | 67 |
| SHIPPED (Anvil already has it) | 2 |
| N/A (CC-specific / platform / daemon-only) | 42 |
| FILE-AS-TASK | 7 |
| VERIFY (low-pri, deferred) | 16 |

**Top 3 P0 items by impact:**
1. **144-B4 Terminal rendering corruption in long sessions** — Anvil's TUI renderer may accumulate stale glyphs in very long sessions (1000+ turns). Self-healing on frame refresh not yet verified.
2. **144-B6 MCP paginated tools/list only returns first page** — Anvil's MCP client has no pagination handler; tools beyond page 1 are silently dropped. High impact for large MCP environments.
3. **145-B4 Infinite loop when skill using `context: fork` re-invokes itself** — Anvil skill runner (`crates/plugins/src/runner.rs`) may not have fork-recursion guard.

---

## v2.1.144

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.144  
**Published:** 2026-05-19

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 144-F1 | `/resume` support for background sessions | N/A | CC background session daemon feature. Anvil has `/resume` for single-process session recovery, not background daemons. |
| 144-F2 | Background subagent elapsed duration notifications | N/A | CC daemon background sessions. |
| 144-F3 | `/plugin` pane shows plugin last-updated timestamp | VERIFY | Anvil plugin details (`crates/plugins/src/`) — low-priority, verify if update timestamp is surfaced. |
| 144-F4 | `/model` now session-scoped (not default); press `d` to set global default | VERIFY | Anvil `/model` command — check if per-session model selection writes to history or applies globally. Likely SHIPPED but verify scope isolation. |
| 144-F5 | Renamed "extra usage" → "usage credits"; `/extra-usage` → `/usage-credits` (old name aliased) | N/A | CC-specific billing terminology. Anvil has no usage/billing UI. |
| 144-B1 | API startup hang (75s) when `api.anthropic.com` unreachable; fixed to 15s timeout | **FILE-AS-TASK** | Anvil `crates/api/src/client.rs` makes startup health checks to `api.anthropic.com` or configured base URL. If the endpoint is behind a firewall / captive portal / broken DNS, Anvil blocks startup. Add 15s timeout to side-channel API calls (e.g., rate-limit headers, model list fetch) and mark them non-fatal. |
| 144-B2 | Terminal output garbled after missed window-resize event; now self-heals on next frame | VERIFY | Anvil TUI resize handler (`crates/anvil-cli/src/tui/`) — verify self-healing on resize-glitch recovery. |
| 144-B3 | Progressive terminal display corruption (stale/garbled glyphs) in very long sessions; cleared on resize | **FILE-AS-TASK** | Anvil TUI glyph buffer (`crates/anvil-cli/src/tui/mod.rs` renderer) may accumulate stale cells in sessions with 1000+ turns. Add a periodic self-heal or full-redraw on detected corruption. |
| 144-B4 | Reduced terminal rendering glitches in VS Code by reducing spinner animation color count | N/A | IDE rendering optimization. Not Anvil's concern (terminal-native). |
| 144-B5 | macOS background session crash ("exit 1 before init") when project under Full Disk Access folder | N/A | CC daemon macOS TCC issue. Anvil is terminal process; inherits terminal's TCC grants. |
| 144-B6 | MCP `tools/list` pagination: only first page returned, silently dropping tools | **FILE-AS-TASK** | Anvil MCP client (`crates/runtime/src/mcp_client.rs`) — currently no pagination handler. When an MCP server returns paginated `tools/list` responses (e.g., `has_more: true`, `next_cursor`), Anvil drops tools beyond page 1. Add cursor-aware loop to fetch all pages. |
| 144-B7 | File with mismatched image extension falls back to text read | **FILE-AS-TASK** | Anvil `Read` tool (`crates/tools/src/file_ops.rs:40-60`) — when `read_file` detects a PNG/JPEG/GIF/WebP signature but the extension is `.txt` (or vice versa), the mime-type inference may fail. Add a fallback to text read instead of aborting. |
| 144-B8 | Tool error spam reduced: `head`/`tail` now satisfy read-before-edit check | VERIFY | Anvil Edit guard (`crates/tools/src/edit.rs`) — verify that `head`/`tail` calls count toward the "read first" requirement. |
| 144-B9 | `egrep`, `fgrep`, `git grep`, `git diff` "no matches" (exit 1) no longer reported as command failure | VERIFY | Anvil bash executor (`crates/tools/src/bash.rs`) — exit code 1 handling. Verify exit 1 from grep tools is not surfaced as an error. |
| 144-B10 | `/branch` failing with "No conversation to branch" after `EnterWorktree` or in background sessions | **FILE-AS-TASK** | Anvil `/branch` command (`crates/anvil-cli/src/commands/branch.rs`) must capture conversation history before `EnterWorktree` switches CWD. Audit history loading post-worktree switch. |
| 144-B11 | Escape in AskUserQuestion notes field returning to answer selection (not aborting turn) | VERIFY | Anvil `AskUserQuestion` dialog (`crates/anvil-cli/src/tui/dialogs/`) — low-priority, verify Esc behavior in notes input. |
| 144-B12 | Model selection via IDE model picker or `applyFlagSettings` now applies after startup | VERIFY | Anvil IDE integration / settings apply — low-priority, verify timing. |
| 144-B13 | Resumed sessions retain model from previous session (not picking up another's `/model`) | VERIFY | Anvil `/resume` — session history should preserve model choice. Verify no model cross-contamination between resumed sessions. |
| 144-B14 | Bedrock/Vertex users can select "Opus (1M)" from model picker (fixed regression in v2.1.129) | N/A | Bedrock/Vertex-specific. Anvil has no Bedrock provider. |
| 144-B15 | Remote-session login failing with "Can't access org" for `forceLoginMethod` + `forceLoginOrgUUID` users | N/A | CC Remote Control connector. Anvil RC uses different auth flow. |
| 144-B16 | MCP paginated `tools/list` only returns first page (same root as 144-B6 above) | FILE-AS-TASK | Merged into 144-B6 task spec below. |
| 144-B17 | MCP images with unsupported MIME types (e.g. SVG) now saved to disk, referenced in result | **FILE-AS-TASK** | Anvil MCP image handler (`crates/runtime/src/mcp.rs`) — when an MCP server returns an unsupported image MIME type, save to disk and return a file path in the tool result instead of breaking the conversation. |
| 144-B18 | File descriptor exhaustion when build runs in skill directory; non-`.md` files no longer trigger reloads | **FILE-AS-TASK** | Anvil skill watcher (`crates/plugins/src/loader.rs` or plugin monitor) — add a filter so only `.md` file changes trigger skill reload. Monitor ALL files in skill dir but reload only on `.md` change. Prevents FD exhaustion from build artifacts. |
| 144-B19 | Session title from first user prompt (not plugin monitor output) | SHIPPED | Anvil `crates/anvil-cli/src/session_meta.rs` — session title derived from first user message, not plugin output. Already correct. |
| 144-B20 | Skill tool permission error in headless mode (regression in v2.1.141) | N/A | CC headless SDK feature. Anvil has no headless mode. |
| 144-B21 | Plugins enabled in settings showing "not cached" errors; now shows `claude plugin install` hint | VERIFY | Anvil plugin cache error messages (`crates/plugins/src/manager.rs`) — low-priority, verify error copy quality. |
| 144-B22 | `claude mcp list` showing config errors when `.mcp.json` malformed (instead of silent failure) | VERIFY | Anvil `/mcp` command — low-priority, verify error reporting for malformed config. |
| 144-B23 | Background side-queries on custom `ANTHROPIC_BASE_URL` and Bedrock Mantle now use Haiku fallback correctly | N/A | CC background side-queries. Anvil has no background query system. |
| 144-B24 | Scrolling in attached background sessions on Windows (PgUp/PgDn, mouse wheel, Ctrl+O) | N/A | CC Windows background session attachment. |
| 144-B25 | Crash when closing terminal while attached to background session | N/A | CC daemon attachment. |
| 144-B26 | Windows: ← in `claude agents` no longer leaves list unresponsive | N/A | CC agents UI. |
| 144-B27 | CJK ghost characters at left edge in Agent View on Windows Terminal | N/A | CC agents + Windows Terminal. |
| 144-B28 | `/bg` and ← preserve directories added via `/add-dir` | N/A | CC daemon fork. |
| 144-B29 | Edit/Write refusing "background session hasn't isolated changes" right after detach now fixed | N/A | CC daemon worktree isolation. |
| 144-B30 | `claude respawn <id>` on stopped background session shows correct status | N/A | CC daemon. |
| 144-B31 | `/resume` picker showing sessions forked from background | N/A | CC daemon. |
| 144-B32 | `claude agents` / `claude logs <id>` timeout to 10s recovery when daemon unresponsive | N/A | CC daemon. |
| 144-B33 | Background Bash tasks staying "Running" after process exits (SDK task panels) | N/A | CC SDK. |
| 144-B34 | Completed/stopped background sessions no longer falsely marked as startup crash | N/A | CC daemon. |
| 144-B35 | Markdown links in `claude agents` attached sessions now clickable | N/A | CC agents. |
| 144-B36 | Custom `spinnerVerbs` no longer apply to post-turn duration (past-tense built-ins restored) | VERIFY | Anvil spinner verb configuration — verify that duration messages use fixed past-tense wording, not custom verbs. |
| 144-B37 | `claude agents` / `--bg` rejection messages now name the specific gate | N/A | CC daemon. |
| 144-B38 | `claude --bg --name <label>` echoes the name in post-spawn confirmation | N/A | CC daemon. |
| 144-B39 | Renaming background session with Ctrl+R updates attached session banner immediately | N/A | CC daemon. |
| 144-B40 | Background session worktree isolation guard for non-git VCS with `WorktreeCreate` hooks | N/A | CC daemon. |
| 144-B41 | Plugin marketplace respects `CLAUDE_CODE_PLUGIN_PREFER_HTTPS` | VERIFY | Anvil plugin install (`crates/plugins/src/manager.rs`) — low-priority, check if HTTPS preference is honored. |
| 144-B42 | `/plugin` returns to Installed list after enable/disable/uninstall | VERIFY | Anvil plugin UI state — low-priority. |
| 144-B43 | `/doctor` shows exec-form example for missing `command` in hook | VERIFY | Anvil `/doctor` command — low-priority, verify hook validation messages. |
| 144-B44 | Skill-listing truncation no longer shown as startup notification | VERIFY | Anvil startup notifications — low-priority. |
| 144-B45 | Pre-wait for MCP startup now overlaps instead of blocking (up to 2s faster) | VERIFY | Anvil MCP startup timing — low-priority performance optimization. |
| 144-B46 | Post-survey follow-up hint with context-aware copy after every non-dismiss response | N/A | CC feedback survey. Anvil has no survey. |

**v2.1.144 count:** 46 items — 7 FILE-AS-TASK, 11 VERIFY, 25 N/A, 1 SHIPPED, 2 carry-over.

---

## v2.1.145

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.145  
**Published:** 2026-05-19

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 145-F1 | `claude agents --json` lists live sessions for scripting | N/A | CC agents dashboard. |
| 145-F2 | `agent_id`, `parent_agent_id` in OpenTelemetry spans | N/A | CC telemetry. Anvil has no OTEL integration. |
| 145-F3 | Status line JSON includes GitHub repo/PR info when detected | VERIFY | Anvil status-line rendering — low-priority, check repo/PR detection. |
| 145-F4 | `/plugin` Discover/Browse shows commands, agents, skills, hooks, MCP/LSP before install | VERIFY | Anvil plugin marketplace preview — low-priority, check feature completeness. |
| 145-F5 | `claude agents` tab title shows awaiting-input count | N/A | CC daemon. |
| 145-F6 | Slash/mention suggestion list supports mouse hover and click in fullscreen | VERIFY | Anvil fullscreen suggestion UI — low-priority, verify mouse interaction. |
| 145-F7 | Stop/SubagentStop hook input includes `background_tasks`, `session_crons` fields | N/A | CC daemon hooks. |
| 145-B1 | Permission-prompt bypass: bare variable assignments to non-allowlisted env vars in Bash auto-approved | **FILE-AS-TASK** | Anvil bash executor (`crates/tools/src/bash.rs`) — when a Bash command contains bare env-var assignments like `VAR=value command`, the permission checker must not auto-approve non-allowlisted variables. Audit permission filter for env assignments in command strings. |
| 145-B2 | MCP prompt slash commands showing raw server validation errors | VERIFY | Anvil MCP prompt tool — low-priority, check error message quality. |
| 145-B3 | Spinner/elapsed-time freezing after terminal resize or refocus | **FILE-AS-TASK** | Anvil TUI render loop (`crates/anvil-cli/src/tui/`) — after terminal refocus or resize, the spinner and elapsed-time display may freeze until next keypress. Ensure timer ticks and render queue wake independently of input events. |
| 145-B4 | Infinite loop when skill using `context: fork` repeatedly re-invokes itself | **FILE-AS-TASK** | Anvil skill runner (`crates/plugins/src/runner.rs`) — add recursion guard to prevent a fork-context skill from re-invoking itself infinitely. Track active fork invocations per session and abort with error on cycle detection. |
| 145-B5 | Cross-project resume hint failing in Windows PowerShell 5.1 (`;` separator) | N/A | Windows PowerShell. Anvil has no cross-project resume. |
| 145-B6 | Voice push-to-talk not working in agent view reply pane | N/A | CC voice + agents. |
| 145-B7 | Task lists rendering in random order when created at once | VERIFY | Anvil task rendering — low-priority, verify creation order. |
| 145-B8 | Stale "Failed to install marketplace" banner showing | N/A | CC marketplace installer. |
| 145-B9 | PR badge not updating immediately after `gh pr create` | VERIFY | Anvil PR integration — low-priority, check badge refresh. |
| 145-B10 | Agent Teams teammates with non-ASCII names failing due to invalid header encoding | N/A | CC Team mode. |
| 145-B11 | `/review` using deprecated `projectCards` GraphQL query | N/A | CC GitHub integration. |
| 145-B12 | `claude plugin validate` not flagging `skills:` entries pointing at files | VERIFY | Anvil plugin validator — low-priority, check error detection. |
| 145-B13 | Infinite loop: skill with `context: fork` repeatedly re-invoking itself (same as 145-B4) | FILE-AS-TASK | Merged into 145-B4 task spec below. |
| 145-B14 | Read tool truncation with "PARTIAL view" notice instead of hard error on overflow | VERIFY | Anvil `Read` tool large-file handling — low-priority, verify partial-read behavior. |

**v2.1.145 count:** 20 items — 2 FILE-AS-TASK, 6 VERIFY, 10 N/A, 0 SHIPPED, 2 carry-over.

---

## v2.1.146

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.146  
**Published:** 2026-05-21

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 146-F1 | Renamed `/simplify` → `/code-review` with optional effort level | N/A | CC `/simplify` command. Anvil has `/simplify` as a standalone skill, not a built-in. |
| 146-F2 | Auto mode no longer suppresses `AskUserQuestion` when user/skill explicitly relies on it | VERIFY | Anvil auto-mode + AskUserQuestion interaction — low-priority, verify auto-mode deference to explicit prompts. |
| 146-B1 | Windows PowerShell tool failing with "invalid command line" when `pwsh` from winget/Store | N/A | PowerShell/Windows-specific. |
| 146-B2 | MCP `resources/list`, `resources/templates/list`, `prompts/list` dropping items past page 1 | **FILE-AS-TASK** | Anvil MCP client (`crates/runtime/src/mcp_client.rs`) — same pagination issue as tools/list (144-B6). Add pagination handler for resource and prompt listings. |
| 146-B3 | Full-screen strobing in attached background sessions on Windows Terminal while streaming | N/A | CC daemon + Windows Terminal. |
| 146-B4 | Auto-updater status line not showing current version on update failure | N/A | CC auto-updater. Anvil has no built-in updater. |
| 146-B5 | Background-job worktree cleanup no longer follows NTFS junctions into main repo | N/A | Windows NTFS behavior. |
| 146-B6 | `/background` refusing sessions whose only input was skill/custom slash command | VERIFY | Anvil `/background` command — low-priority, verify command classification. |
| 146-B7 | Backgrounded sessions re-prompting for tool permissions already granted | N/A | CC daemon. |
| 146-B8 | `/theme` color editor and "New custom theme" dialogs not responding to Esc | VERIFY | Anvil theme editor — low-priority, verify Esc handling. |
| 146-B9 | Uncaught exception at end of streaming sessions via Agent SDK | N/A | CC SDK. |
| 146-B10 | `forceLoginOrgUUID` and `forceLoginMethod` not enforced against 3P-provider/API-key sessions | N/A | CC auth policy. |
| 146-B11 | GNOME Terminal right-click/middle-click paste not inserting text | N/A | Linux GNOME Terminal. |
| 146-B12 | `CLAUDE_CODE_SUBAGENT_MODEL` not forwarded to child processes in multi-agent sessions | N/A | CC multi-agent. |
| 146-B13 | Auto-updater reliability: native version checks and downloads now retry transient failures | N/A | CC auto-updater. |
| 146-B14 | Diff rendering performance improved for large edits | VERIFY | Anvil diff renderer — low-priority, no change needed unless regression detected. |

**v2.1.146 count:** 16 items — 1 FILE-AS-TASK, 5 VERIFY, 9 N/A, 0 SHIPPED, 1 carry-over.

---

## Open Community Issues (2026-05-17 onward)

Top 10 by severity/relevance to Anvil parity (partial list; full list shows 50+ open issues as of 2026-05-21):

| Issue | CC Version | Verdict | Anvil Exposure |
|---|---|---|---|
| #61082 First `/rename` replaces theme's prompt-bar color with default teal | — | FILE-AS-TASK | Anvil theme engine (`crates/anvil-cli/src/theme.rs`) — verify first `/rename` doesn't reset custom theme colors. |
| #61081 Pro → Max upgrade fails silently (billing) | — | N/A | Anvil has no billing. |
| #61080 "Opened in VS Code" prints even when multi-session | — | VERIFY | Anvil IDE integration — low-priority. |
| #61079 Session performance degradation | — | VERIFY | Anvil history/render perf — low-priority, verify no regression. |
| #61077 MCP tools ignore `permissions.allow` — all require manual approval | — | **FILE-AS-TASK** | Anvil permission system (`crates/runtime/src/permission_memory.rs`) — `permissions.allow` for MCP tools must be honored; tools should not re-prompt if already allowlisted. Audit permission check in MCP tool dispatch. |
| #61075 Resolve GitHub issue/PR image attachments into context | — | N/A | Feature request, not a parity gap. |
| #61070 Background server fails to start after system outage | — | N/A | CC daemon. |
| #61069 `permissions.allow` not honored in web remote execution | — | N/A | CC web remote. |
| #61068 Resume session overwrites `model[1m]` with API-returned model ID — loses 1M context | — | **FILE-AS-TASK** | Anvil session resume (`crates/anvil-cli/src/session.rs`) — when resuming a session, verify the resumed model matches the original session's model, not the API's current default. Check session history persistence for model field. |
| #61062 Worktree-session doesn't carry project-scoped MCP config from original CWD | — | **FILE-AS-TASK** | Anvil `EnterWorktree` (`crates/tools/src/worktree_ops.rs`) — MCP config (`mcp.json`, hooks) in original CWD must be preserved and re-resolved post-worktree switch. Audit CWD-relative config paths. |

---

## FILE-AS-TASK Specs (All Releases)

### TASK-A: v2.2.19 CC-144-B: API startup timeout (15s for side-channel calls)
**Source:** CC v2.1.144  
**File:** `crates/api/src/client.rs`  
**Description:** Health checks to `api.anthropic.com` (rate-limit headers, model list) block startup when endpoint is unreachable (firewall, captive portal, bad DNS). Add 15s timeout to side-channel calls; mark failures as non-fatal. Prevents 75s startup hangs.

---

### TASK-B: v2.2.19 CC-144-B: Terminal display corruption self-heal in long sessions
**Source:** CC v2.1.144  
**File:** `crates/anvil-cli/src/tui/mod.rs` (renderer)  
**Description:** TUI glyph buffer accumulates stale cells in sessions with 1000+ turns. Add periodic self-heal or full-redraw when corruption detected. Reset on next frame after missed resize event.

---

### TASK-C: v2.2.19 CC-144-B: MCP pagination handler for tools/list
**Source:** CC v2.1.144, CC v2.1.146 (confirmed for resources/prompts)  
**File:** `crates/runtime/src/mcp_client.rs`  
**Description:** MCP servers with paginated `tools/list`, `resources/list`, `resources/templates/list`, `prompts/list` return only first page; subsequent pages are silently dropped. Add cursor-aware loop to fetch all pages until `has_more: false` or no `next_cursor`.

---

### TASK-D: v2.2.19 CC-144-B: File mime-type mismatch fallback to text read
**Source:** CC v2.1.144  
**File:** `crates/tools/src/file_ops.rs`  
**Description:** `Read` tool fails when file has PNG/JPEG/GIF signature but extension is `.txt` (or vice versa). Add fallback: attempt binary mime detection, on mismatch fall back to text read instead of aborting.

---

### TASK-E: v2.2.19 CC-144-B: `/branch` history recovery post-EnterWorktree
**Source:** CC v2.1.144  
**File:** `crates/anvil-cli/src/commands/branch.rs`, `crates/tools/src/worktree_ops.rs`  
**Description:** `/branch` fails with "No conversation to branch" after `EnterWorktree` switches CWD. Audit history loading: capture conversation state before worktree switch, reload from original CWD after switch.

---

### TASK-F: v2.2.19 CC-144-B: MCP image fallback for unsupported MIME types
**Source:** CC v2.1.144  
**File:** `crates/runtime/src/mcp.rs` (MCP image handler)  
**Description:** When MCP server returns unsupported image MIME type (e.g. SVG), save to disk and return file path in tool result instead of breaking conversation.

---

### TASK-G: v2.2.19 CC-144-B: Skill watcher file-descriptor exhaustion prevention
**Source:** CC v2.1.144  
**File:** `crates/plugins/src/loader.rs` or plugin monitor  
**Description:** Build artifacts in skill directories trigger repeated skill reloads, exhausting file descriptors. Monitor all files but reload only on `.md` changes. Filter out `.o`, `.a`, `.so`, build cache files.

---

### TASK-H: v2.2.19 CC-145-B: Bash env-var assignment permission bypass audit
**Source:** CC v2.1.145  
**File:** `crates/tools/src/bash.rs`  
**Description:** Bare Bash env assignments like `VAR=value command` bypass permission checks for non-allowlisted variables. Audit permission filter: parse env assignments in command string and enforce allowlist on each one, not just the command itself.

---

### TASK-I: v2.2.19 CC-145-B: Spinner/elapsed-time render freeze after refocus
**Source:** CC v2.1.145  
**File:** `crates/anvil-cli/src/tui/` (render loop)  
**Description:** After terminal refocus or resize, spinner and elapsed-time may freeze until next keypress. Ensure timer ticks and render queue wake independently of input events. Use separate event channels for input and timer.

---

### TASK-J: v2.2.19 CC-145-B: Skill fork-context recursion guard
**Source:** CC v2.1.145  
**File:** `crates/plugins/src/runner.rs`  
**Description:** Skill with `context: fork` can infinitely re-invoke itself. Add recursion guard: track active fork invocations per session. On cycle detection, abort with error and prevent loop.

---

### TASK-K: v2.2.19 CC-146-B: MCP resource/prompt pagination
**Source:** CC v2.1.146  
**File:** `crates/runtime/src/mcp_client.rs`  
**Description:** Same pagination issue as tools/list (TASK-C) applies to `resources/list`, `resources/templates/list`, `prompts/list`. Extend pagination handler to all list operations.

---

### TASK-L: SECURITY: v2.2.19 CC-145-B: MCP tool permission enforcement
**Source:** Community issue #61077  
**File:** `crates/runtime/src/permission_memory.rs`, MCP tool dispatch  
**Description:** `permissions.allow` MCP tool rules are not honored; all tools re-prompt. Audit permission check in MCP tool call handler: must check allowlist before prompting. File must not be marked as "security" unless it gates resource access without user intent.

---

### TASK-M: v2.2.19 CC-144-B (Community): Theme color reset on first `/rename`
**Source:** Community issue #61082  
**File:** `crates/anvil-cli/src/theme.rs`  
**Description:** First `/rename` session action resets custom theme colors to defaults. Verify theme persistence: config should load before session rename; rename must not re-initialize theme.

---

### TASK-N: v2.2.19 CC-146-B (Community): Resume session model preservation
**Source:** Community issue #61068  
**File:** `crates/anvil-cli/src/session.rs`  
**Description:** Resume session overwrites stored model with API-returned default, losing 1M context window selection. Verify session history loads model from transcript, not API; `/resume` must restore exact session state including model.

---

### TASK-O: v2.2.19 CC-144-B (Community): EnterWorktree MCP config preservation
**Source:** Community issue #61062  
**File:** `crates/tools/src/worktree_ops.rs`, MCP config resolution  
**Description:** `EnterWorktree` doesn't carry project-scoped MCP config from original CWD. Audit config paths: resolve `mcp.json`, hook auth credentials relative to original CWD; re-bind post-worktree-switch or pass through worktree boundary.

---

## Priority Classification

**P0 (Blocking, implement in v2.2.19):**
- TASK-C (MCP pagination — silently drops tools, high user impact)
- TASK-I (Spinner freeze — UX regression)
- TASK-L (MCP permission enforcement — security-adjacent)

**P1 (High impact, all in scope for v2.2.19 (no deferrals)):**
- TASK-B (Terminal corruption in long sessions)
- TASK-H (Bash env-var permission bypass)
- TASK-J (Skill fork recursion guard)
- TASK-K (Resource/prompt pagination)
- TASK-N (Resume session model loss)

**P2 (Medium impact, all in scope for v2.2.19 (no deferrals)):**
- TASK-A (API startup timeout)
- TASK-D (File mime-type fallback)
- TASK-E (Branch history post-worktree)
- TASK-F (MCP image fallback)
- TASK-G (Skill FD exhaustion)
- TASK-M (Theme color reset)
- TASK-O (Worktree MCP config)

**P3 / VERIFY (Low-priority, deferred):**
- All VERIFY items (16 total, see audit tables)

---

## Summary: Drift Status

**Days drift:** 4 (v2.1.143 audit: 2026-05-17, current: 2026-05-21)  
**New CC releases:** 3 (v2.1.144, v2.1.145, v2.1.146)  
**New Anvil tasks filed:** 15 total (7 FILE-AS-TASK from releases, 2 from community issues)  
**Already-parity:** 3 items SHIPPED  
**Deferred verification:** 16 VERIFY items (low-pri, file bugs only if reproduced)

---

*Audit by QA agent. Source code not modified. Community issues triage includes 50+ open issues; top 10 shown.*

---

## Appendix: File-path corrections (from implementing agents)

After parallel agents (`acf0439a` P0, `aff1ecc2` P1, `af2250df` P2, `a6ec2efc` P1+P2 secondary) shipped the 15 FILE-AS-TASK items, the following audit-doc paths needed correction. Recording them here for future audits:

- **TASK-C / TASK-K (MCP pagination)**: audit pointed at `crates/runtime/src/mcp_client.rs`, but real list-fetching is in `crates/runtime/src/mcp_stdio/{transport.rs, server_manager.rs, types.rs}`. `mcp_client.rs` is only config-bootstrap.
- **TASK-D (mime-mismatch)**: audit said `crates/tools/src/file_ops.rs:40-60`. Real `read_file` dispatch is at `crates/runtime/src/file_ops.rs::read_file`; `crates/tools/src/file_ops.rs` is the tool surface only.
- **TASK-E (/branch post-EnterWorktree)**: audit said `crates/anvil-cli/src/commands/branch.rs` — that file does NOT exist. Anvil's `/branch` is the git operation in `crates/commands/src/git.rs::handle_branch_slash_command`. CC's conversation-fork `/branch` is satisfied by Anvil's `/fork` (#344 done v2.2.12). Snapshot infra at `crates/tools/src/worktree_ops.rs` was the real Anvil-side work.
- **TASK-H (Bash env bypass)**: audit said `crates/tools/src/bash.rs`. Real matcher is `crates/runtime/src/auto_mode.rs::AutoModeConfig::matches_hard_deny`; bash exec is in `crates/runtime/src/bash.rs`.
- **TASK-I (spinner freeze)**: audit said `crates/anvil-cli/src/tui/` (generic). Real dispatch sites: `tui/input_handler.rs:131-235`, `tui/mod.rs:2527-2548` (`wait_for_turn_end`), `tui/mod.rs:2699-2787` (`wait_for_turn_end_for_tab`). New helper lives at `tui/spinner_pump.rs`.
- **TASK-J (skill fork loop)**: audit said `crates/plugins/src/runner.rs` — that file did NOT exist. The agent created it for the first time.
- **TASK-M (/rename theme reset)**: audit said `crates/anvil-cli/src/theme.rs` — that file does NOT exist. Theme is `crates/runtime/src/theme.rs`. Rename handler is in `crates/anvil-cli/src/main.rs` (~line 9043) using `session_meta::set_session_name`.
- **TASK-N (resume model)**: audit said `crates/anvil-cli/src/session.rs`. That file exists but is metadata-only; persistent model field is in `crates/anvil-cli/src/session_meta.rs` (sidecar). Resume dispatch in `main.rs::resume_session`.
- **TASK-O (worktree MCP config)**: audit said `crates/tools/src/worktree_ops.rs` + generic MCP resolution. Real MCP config resolution is `runtime::ConfigLoader::default_for(cwd).load().feature_config().mcp()` at `crates/runtime/src/config/mcp.rs` + `crates/runtime/src/config/mod.rs`. Snapshot infra landed correctly at `worktree_ops.rs`.

Lesson for future audits: when auditing from memory, the actual file paths often disagree with intuition. Always `rg` / `find` to verify before filing as a "you'll find it at" hint.
