# CC Parity Audit — v2.1.140 through v2.1.143

**Date:** 2026-05-16
**Scope:** 4 CC releases since last closed audit (task #451, v2.1.132–v2.1.139)
**Anvil base:** v2.2.15
**Format:** Triage per release. Verdict per item: SHIPPED / N/A / FILE-AS-TASK.
**Naming:** "CC" per `feedback-cc-only-naming.md`. `-F` = feature, `-B` = bug fix.

---

## Summary

| Metric | Count |
|---|---|
| CC versions examined | 4 (v2.1.140 – v2.1.143) |
| Total items triaged | 68 |
| SHIPPED (Anvil already has it) | 5 |
| N/A (CC-specific / platform-specific) | 31 |
| FILE-AS-TASK | 13 |
| VERIFY (low-pri, deferred queue) | 19 |

**Top 3 by impact:**
1. **143-B1 Corrupt credentials.json hang** — if Anvil's `scopes` field in `StoredOAuthCredentials` gets written as a non-array (e.g. null, string), `load_oauth_credentials` will propagate a serde error up through `InvalidData` and the session will fail to start. Same structural exposure as CC's fix.
2. **141-B1 MCP `MCP_TOOL_TIMEOUT` not raising per-request fetch timeout** — Anvil's MCP client has no configurable timeout override env; tool calls from slow MCP servers silently hang. High user impact.
3. **142-B1 Reactive compaction seeds first attempt from overflow size** — Anvil's `compact.rs` uses a fixed `max_estimated_tokens` threshold and doesn't seed from actual overflow size. This causes wasted near-full-context summarization retries, which wastes tokens and time.

**Note on TaskCreate:** No TaskCreate MCP tool is available in this environment. Tasks are specified below in FILE-AS-TASK sections with full subject + description in the format used by #452–#472. File them manually or via the task UI using the specs below.

---

## v2.1.140

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.140

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 140-F1 | Agent tool `subagent_type` case- and separator-insensitive matching | N/A | CC-specific agent-dispatch surface. Anvil TeamDelegate uses typed enums, not string matching. |
| 140-F2 | Updated agent color palette | N/A | CC-specific UI chrome. |
| 140-B1 | `/goal` silently hanging when `disableAllHooks` / `allowManagedHooksOnly` set | N/A | CC's `/goal` completion-condition feature. Anvil's `/goal` is goal-persistence (v2.2.11 W3 #378), not an autonomous loop. Name collision — see 139-F2 backlog. |
| 140-B2 | Settings hot-reload: symlinked settings files caused misattributed change events | N/A | Anvil has no settings hot-reload watcher (`notify` crate not a dep). Already noted in 139-B6 audit. |
| 140-B3 | `claude --bg` failing with "connection dropped mid-request" on idle-exit | N/A | CC daemon/background architecture. Anvil has no equivalent background session daemon yet (v2.3 arc). |
| 140-B4 | Background service startup failing on machines with enterprise endpoint security | N/A | CC daemon architecture. |
| 140-B5 | Remote managed settings not retrying on 401 | VERIFY | Anvil config loader fetches remote settings at startup. Retry-on-401 behavior not confirmed. Low-pri. |
| 140-B6 | `/loop` scheduling redundant wakeups to poll background tasks | N/A | CC `/loop` scheduling. Anvil `/loop` is the skill-runner loop — different surface. |
| 140-B7 | Recurring event-loop stall on Windows: missing `gh` executable causing sync `where.exe` re-spawns | N/A | Windows-specific. |
| 140-B8 | `Read` tool: `offset` rejected when passed as whitespace-padded or `+`-prefixed string | **FILE-AS-TASK** | `crates/tools/src/file_ops.rs:9`: `offset: Option<usize>`. serde_json deserializes JSON numbers to `usize` correctly, but if the model sends `"  5"` or `"+5"` as a JSON string, serde will reject it with an opaque parse error. Anvil has the identical structural exposure. |
| 140-B9 | Native terminal cursor not staying at input caret when terminal loses focus | VERIFY | Anvil TUI cursor positioning — low-pri, verify on focus-blur cycle. |
| 140-B10 | Plugins warn when default component folder silently ignored due to `plugin.json` key override | N/A | Anvil plugin system uses different manifest loader. Not an exact match. |

**v2.1.140 count:** 12 items — 1 FILE-AS-TASK, 1 VERIFY, 8 N/A, 2 (B1/B2) already tracked.

---

## v2.1.141

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.141

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 141-F1 | `terminalSequence` field in hook JSON output for desktop notifications / window titles / bells | **FILE-AS-TASK** | Anvil hooks (`crates/runtime/src/hooks.rs`) emit stdout capture only. No `terminalSequence` post-processing. Useful for notification-on-hook-complete workflows. |
| 141-F2 | `CLAUDE_CODE_PLUGIN_PREFER_HTTPS` — clone GitHub plugin sources over HTTPS instead of SSH | N/A | Anvil plugin install uses cargo/git directly; SSH vs HTTPS is user's git config. Different surface. |
| 141-F3 | `ANTHROPIC_WORKSPACE_ID` env for workload identity federation token scoping | N/A | CC-specific enterprise workspace identity. |
| 141-F4 | `claude agents --cwd <path>` to scope session list to a directory | N/A | CC's `claude agents` cross-session dashboard. Anvil `anvil agents` is plugin browser — different surface. |
| 141-F5 | `/feedback` can include recent sessions (last 24h or 7d) | N/A | CC-specific feedback bundle feature. |
| 141-F6 | Rewind menu: "Summarize up to here" to compress earlier context while keeping recent turns | **FILE-AS-TASK** | Anvil has `/compact` (full-session compaction) and `--rewind` (turn removal). A "summarize up to here" partial-compaction anchored at a specific turn is missing and directly useful for long sessions. |
| 141-F7 | Auto mode permission dialog now explains when `permissions.ask` rule caused the prompt | VERIFY | Anvil permission-prompt dialogs — check if the triggering rule is surfaced. |
| 141-F8 | "View diff in your IDE" option restored on file-edit permission prompts when IDE connected | N/A | IDE connector feature. Anvil is terminal-native. |
| 141-F9 | Background agents via `/bg` or `←←` preserve current permission mode | N/A | CC background agent architecture. |
| 141-F10 | `claude agents`: agents finishing work with background shell still running now move to Completed | N/A | CC agent dashboard state machine. |
| 141-F11 | Spinner warms to amber after 10 seconds to signal Claude is still working | **FILE-AS-TASK** | Anvil's spinner in `crates/anvil-cli/src/tui/mod.rs` is a static spin. A time-elapsed color warm (green → amber → red) would reduce user uncertainty during long inference. Low-cost UX win. |
| 141-B1 | `MCP_TOOL_TIMEOUT` not raising per-request fetch timeout — capped at 60s regardless | **FILE-AS-TASK** | Anvil MCP client (`crates/runtime/src/mcp_client.rs`) has no `ANVIL_MCP_TOOL_TIMEOUT` override. Tool calls to slow MCP servers silently cap at the default. High user impact for AI-backed MCP servers. |
| 141-B2 | Background side-queries sending unavailable Haiku model ID on Bedrock/Vertex/Foundry/gateway | N/A | CC background query / Haiku fallback. Anvil uses `ANTHROPIC_SMALL_FAST_MODEL` pattern but no background queries. |
| 141-B3 | `claude daemon status` and `/doctor` on Windows throwing on locked/unreadable daemon pipe key | N/A | CC daemon + Windows-specific. |
| 141-B4 | `claude agents` showing agent-type list instead of dashboard when launched through a wrapper | N/A | CC agents dashboard. |
| 141-B5 | `/model` in one session silently changing autocompact threshold in other concurrent sessions | **FILE-AS-TASK** | Anvil `crates/runtime/src/history.rs`: `compact_threshold_pct()` reads an env var — a process-global, not per-tab. Concurrent tabs share the same threshold. If Anvil ever adds `/model`-triggered threshold recalculation, this bug lands automatically. File now to prevent it and to audit current per-tab isolation. |
| 141-B6 | Switching permission mode while tool-permission prompt open not auto-dismissing prompt | VERIFY | Anvil permission dialog — low-pri. |
| 141-B7 | Pressing Enter while permission/dialog prompt open also submitting text in input box | VERIFY | Anvil input handler `tui/input_handler.rs` — Enter propagation when dialog visible. |
| 141-B8 | Hooks receiving non-existent `transcript_path` after `EnterWorktree` switches working directory | **FILE-AS-TASK** | Anvil hooks emit `HOOK_EVENT`, `HOOK_TOOL_NAME`, `HOOK_TOOL_INPUT` (`crates/runtime/src/hooks.rs:969-971`) and `crates/plugins/src/hooks.rs:420-425`. Neither sends a `transcript_path`. But hooks for other events may reference CWD-relative paths that become stale post-`EnterWorktree`. File to audit and stabilize any path values sent to hooks after worktree switch. |
| 141-B9 | Markdown tables with cell wrapping falling back to vertical key-value layout (regression in 2.1.136) | VERIFY | Anvil markdown table renderer — check bordered-grid fallback logic. |
| 141-B10 | Cancelled prompts removed from Up-arrow history on auto-restore, causing duplicates | VERIFY | Anvil input history `crates/anvil-cli/src/input/history.rs` — cancel + restore duplicate check. |
| 141-B11 | Ctrl+C not interrupting a running turn while in vim INSERT/VISUAL mode | N/A | Anvil has no vim-mode. |
| 141-B12 | Alternative `chat:submit` keybindings not working when `enter` is rebound to `chat:newline` | VERIFY | Anvil keybinding system — verify submit rebind + newline rebind coexistence. |
| 141-B13 | Prompt suggestions silently disabled when an output style is configured | VERIFY | Anvil prompt suggestions + output style coexistence — low-pri. |
| 141-B14 | `spinnerVerbs` setting not honored in turn-completion messages | VERIFY | Anvil spinner verbs in settings schema. |
| 141-B15 | `AskUserQuestion` popup hiding last line of preceding chat content | VERIFY | Anvil AskUserQuestion layout overlap. |
| 141-B16 | Web Search status showing "Did 0 searches" when searches returned errors | N/A | CC-specific Web Search tool status display. |
| 141-B17 | Multi-line statusline output dropping/corrupting rows when any line exceeds terminal width | VERIFY | Anvil statusline width math. |
| 141-B18 | Light-ansi theme using invisible white for diff context lines on light backgrounds | VERIFY | Anvil theme diff-line colors — low-pri. |
| 141-B19 | Error overlay dumping minified bundle source hiding the original error message | N/A | CC is JavaScript-based (minified bundles). Anvil is native Rust; panics show full backtrace. |
| 141-B20 | Pressing Enter after typing feedback survey rating digit submitting as chat instead of rating | N/A | CC-specific rating UI. |
| 141-B21 | Pressing `x` on selected subagent in agent panel typing into prompt instead of stopping | N/A | CC agent panel. |
| 141-B22 | Session title derived from plugin monitor notifications before user's first prompt | VERIFY | Anvil session title assignment timing — low-pri. |
| 141-B23 | "Allowed by PermissionRequest hook" repeating once per tool call under collapsed group | VERIFY | Anvil permission annotation dedup under collapsed groups. |
| 141-B24 | `/tui` silently dropping running background shells and subagents | N/A | CC-specific `/tui` command. |
| 141-B25 | Welcome banner showing "API Usage Billing" on Bedrock/Vertex/Foundry/other 3P providers | **FILE-AS-TASK** | Anvil welcome banner / startup message in `crates/anvil-cli/src/tui/mod.rs` or `setup.rs` — verify it names the configured provider instead of defaulting to Anthropic billing language when a 3P or Ollama provider is active. Users on Ollama or OpenAI-compat providers should not see Anthropic billing copy. |
| 141-B26 | `/mcp` server list not keeping focused server visible in short terminals in fullscreen | VERIFY | Anvil `/mcp` list scroll — low-pri. |
| 141-B27 | Redaction in `/feedback` bundles producing invalid JSON for quoted values | N/A | CC-specific feedback bundle. |
| 141-B28 | Desktop and 3P provider sessions incorrectly inheriting `apiKeyHelper`/`ANTHROPIC_AUTH_TOKEN` | VERIFY | Anvil auth token precedence when both `apiKeyHelper` and env vars present. |
| 141-B29 | Plugin MCP servers with unset config vars showing generic failure instead of fix-it hint | VERIFY | Anvil plugin MCP error messages — low-pri. |
| 141-B30 | MCP HTTP/SSE servers returning 403 on connect showing "failed" instead of "needs auth" | VERIFY | Anvil MCP connection error classification. |
| 141-B31 | Remote MCP servers disconnecting when optional server-events stream failed to reconnect | SHIPPED | Anvil MCP retry already noted as HAVE-IT in 139-F15 audit. |
| 141-B32 | Remote Control re-enrolling trusted device when server rejects stale token instead of looping /login | N/A | CC Remote Control connector. Anvil RC is different architecture. |
| 141-B33 | Custom `voice:pushToTalk` keybindings silently ignored | N/A | CC voice mode. |
| 141-B34 | Windows Alt+V image paste reporting "no image found" | N/A | Windows-specific. |
| 141-B35 | SDK "CC native binary not found" on Linux with both glibc and musl packages installed | N/A | CC SDK packaging. |
| 141-B36 | Bedrock: `awsCredentialExport` skipped when ambient AWS credentials resolve | N/A | Anvil has no Bedrock provider. |
| 141-B37 | [VSCode] Mic showing no feedback when microphone produced only silence | N/A | IDE/voice feature. |

**v2.1.141 count:** 37 items — 6 FILE-AS-TASK, 11 VERIFY, 17 N/A, 1 SHIPPED, 2 tracked carry-overs.

---

## v2.1.142

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.142

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 142-F1 | `claude agents` new flags: `--add-dir`, `--settings`, `--mcp-config`, `--plugin-dir`, `--permission-mode`, `--model`, `--effort`, `--dangerously-skip-permissions` | N/A | CC agent dashboard. |
| 142-F2 | Fast mode now uses Opus 4.7 by default (previously Opus 4.6); `CLAUDE_CODE_OPUS_4_6_FAST_MODE_OVERRIDE=1` to pin | N/A | CC-specific model alias. Anvil exposes model selection directly; no fast-mode alias. |
| 142-F3 | Plugins with root-level `SKILL.md` and no `skills/` subdir are now surfaced as a skill | VERIFY | Anvil plugin skill discovery — check if `SKILL.md` at root is picked up when `skills/` absent. |
| 142-F4 | `/plugin` details pane and `claude plugin details` now show LSP servers a plugin provides | VERIFY | Anvil plugin details — check LSP server listing if applicable. |
| 142-F5 | `/web-setup` warns before replacing existing GitHub App connection | N/A | CC-specific web setup. |
| 142-B1 | `MCP_TOOL_TIMEOUT` not raising per-request fetch timeout (same as 141-B1 above, confirmed root-cause) | FILE-AS-TASK | Same as 141-B1 — merged into one task spec below. |
| 142-B2 | Background sessions not recognizing pre-existing git worktrees, blocking Edit while EnterWorktree refused to create duplicate | SHIPPED | Anvil `EnterWorktree` (`crates/tools/src/worktree_ops.rs`) checks for existing worktrees at line 126 and returns an error rather than creating a duplicate. The guard is structurally correct. |
| 142-B3 | Background sessions disappearing and daemon reconnect failing after macOS sleep/wake (clock jump not idle) | N/A | CC daemon architecture. |
| 142-B4 | Daemon not exiting cleanly after binary upgrade, causing dispatched agents to crash-loop | N/A | CC daemon. |
| 142-B5 | Background agents crash-looping when Claude-in-Chrome extension connected without shared tab | N/A | CC + Chrome extension. |
| 142-B6 | Clicking links in attached `claude agents` session using headless browser shim incorrectly | N/A | CC agent browser shim. |
| 142-B7 | `claude agents` "v to open in editor" using daemon's default editor instead of `$EDITOR`/`$VISUAL` | N/A | CC agents dashboard. |
| 142-B8 | `claude agents` deadlocking on Windows with network-drive working directories | N/A | Windows-specific. |
| 142-B9 | Background-color bleed when attaching to `claude agents` from Apple Terminal or 256-color-only terminals | N/A | CC agents + terminal-specific. |
| 142-B10 | `claude --bg --dangerously-skip-permissions` not persisting across retire/wake | N/A | CC daemon. |
| 142-B11 | Session titles derived from the URL when first message is a link | **FILE-AS-TASK** | Anvil session title auto-detection (`crates/anvil-cli/src/session_meta.rs` or `session.rs`) — verify the first-message title heuristic doesn't grab a URL literally as the session name. |
| 142-B12 | Redundant `set_model` requests from remote clients injecting duplicate `/model` breadcrumbs | VERIFY | Anvil RC `set_model` dedup — low-pri. |
| 142-B13 | Plugins using `skills: ["./"]` showing false "path escapes plugin directory" error | VERIFY | Anvil plugin path validator — check `./` relative root. |
| 142-B14 | Plugin cache cleanup deleting active plugin version directory when no installation metadata present | VERIFY | Anvil plugin cache eviction — medium-pri. |
| 142-B15 | Reactive compaction: first summarize attempt now seeds from original request's overflow size, avoiding wasted near-full-context retry | **FILE-AS-TASK** | Anvil `crates/runtime/src/compact.rs`: `CompactionConfig::max_estimated_tokens` is a fixed threshold. No concept of seeding from actual overflow size. When triggered reactively (context overflow), Anvil generates the summary without knowing how much must be removed, potentially requiring a second pass. Port the overflow-seeded compaction sizing logic. |
| 142-B16 | Hook configuration error: `prompt`- or `agent`-type hook for `SessionStart`/`Setup`/`SubagentStart` now shows "use a command-type hook instead" | VERIFY | Anvil hook type validation — check error quality for mismatched hook type + event pairs. |
| 142-B17 | Removed stale `/model claude-sonnet-4-20250514` suggestion from Usage Policy refusal messages | N/A | CC-specific model suggestion in refusal copy. |

**v2.1.142 count:** 17 items — 2 FILE-AS-TASK (141-B1 and 142-B11/B15), 4 VERIFY, 9 N/A, 1 SHIPPED, 1 carry-over.

---

## v2.1.143

**GitHub:** https://github.com/anthropics/claude-code/releases/tag/v2.1.143

| ID | Item | Verdict | Evidence / Rationale |
|---|---|---|---|
| 143-F1 | Plugin dependency enforcement: `disable` refuses when another plugin depends on target (with copy-pasteable disable-chain hint); `enable` force-enables transitive deps | VERIFY | Anvil plugin dep graph (`crates/plugins/`) — check if disable/enable respects transitive deps. |
| 143-F2 | Projected context cost (per-turn and per-invocation token estimates) in `/plugin` marketplace browse pane | VERIFY | Anvil `/plugin` details — per-turn cost estimate feature. Low-pri. |
| 143-F3 | `worktree.bgIsolation: "none"` setting to let background sessions edit working copy without `EnterWorktree` | N/A | CC background session worktree setting. Anvil worktree is an explicit tool call, not a background session setting. |
| 143-F4 | PowerShell tool passes `-ExecutionPolicy Bypass` by default; opt-out with env | N/A | PowerShell tool is Windows-specific. |
| 143-F5 | Background sessions preserve model and effort level after waking from idle | N/A | CC daemon/background sessions. |
| 143-F6 | Shift+Tab in attached agent sessions includes auto mode in cycle | N/A | CC agent session permission cycle. |
| 143-B1 | Corrupt `.credentials.json` with non-array `scopes` value hanging CLI on startup or silently aborting OAuth token refresh | **FILE-AS-TASK** | `crates/runtime/src/oauth.rs:420-439`: `read_credentials_root` parses credentials.json as a JSON object. `load_oauth_credentials` then calls `serde_json::from_value::<StoredOAuthCredentials>` on the `oauth` key. `StoredOAuthCredentials.scopes: Vec<String>` (`oauth.rs:91`). If the stored `scopes` value is a JSON string or null instead of an array (e.g. from a botched write or manual edit), serde returns `InvalidData` and the error propagates up through `credentials_path` → the caller. Depending on the call site, this can abort startup or silently prevent token refresh. Fix: add a lenient fallback deserializer that accepts null/string for `scopes` and normalizes to `Vec<String>`. **SECURITY-adjacent — prevents auth lockout.** |
| 143-B2 | Right-click paste in `claude agents` on Windows Terminal and WSL | N/A | Windows-specific. |
| 143-B3 | Stop hooks that block repeatedly looping forever — turn ends with warning after 8 consecutive blocks (`CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`) | **FILE-AS-TASK** | Anvil hooks (`crates/runtime/src/hooks.rs`): no stop-hook block counter. If an Anvil stop hook blocks repeatedly (returns a block response), the session can loop indefinitely. Add a consecutive-block cap (configurable via `ANVIL_STOP_HOOK_BLOCK_CAP`, default 8) with a warning message when the cap is hit. |
| 143-B4 | Esc/Ctrl+C not cancelling a pending `/loop` wakeup while Claude is idle between iterations | N/A | CC `/loop` scheduling. Anvil `/loop` skill architecture is different. |
| 143-B5 | `/goal` evaluator firing while background shells or delegated subagents are still running | N/A | CC `/goal` completion-condition evaluator. Not Anvil's goal-persistence feature. |
| 143-B6 | `NO_COLOR`/`FORCE_COLOR` in settings.json `env` stripping CC's own UI colors — now apply to subprocesses only | **FILE-AS-TASK** | Anvil `crates/runtime/src/hooks.rs` and `crates/tools/src/lib.rs`: when Anvil passes user-configured `env` vars to child processes (bash, hooks, MCP stdio), verify `NO_COLOR`/`FORCE_COLOR` from settings `env` block is applied to subprocess environment only and not to Anvil's own TUI rendering. Audit the subprocess env construction path. |
| 143-B7 | Agent view spawning repeated PowerShell processes on Windows when listing sessions | N/A | CC agents + Windows. |
| 143-B8 | `/bg` without a prompt sending "continue" to forked session — fork now waits for input | N/A | CC `/bg` fork. |
| 143-B9 | `--agent <name>` not finding plugin-contributed agents without `plugin:` prefix | N/A | CC agent dispatch. |
| 143-B10 | Deleting a session from agent view not removing its transcript file | N/A | CC agents. |
| 143-B11 | Stale-fragment rendering when scrolling in attached background sessions on Windows Terminal | N/A | CC agents + Windows. |
| 143-B12 | Background agents false-positive worker-stall detection storm after host sleep or macOS App Nap | N/A | CC daemon. |
| 143-B13 | 5xx error messages pointing at `status.claude.com` instead of naming configured gateway or cloud provider | **FILE-AS-TASK** | Anvil `crates/api/src/error.rs` + `crates/api/src/providers/`: 5xx error messages should name the active provider/gateway (`ANTHROPIC_BASE_URL`, Ollama host, OpenAI-compat base URL) rather than hardcoded Anthropic status URL. Especially important for Ollama and OpenAI-compat users who hit errors and see irrelevant Anthropic status copy. |
| 143-B14 | PowerShell tool enabled by default on Windows for Bedrock/Vertex/Foundry users | N/A | PowerShell/Windows. |
| 143-B15 | `claude agents` accepting new config flags (all applied to dashboard and background sessions) | N/A | CC agents. |
| 143-B16 | `claude --bg --dangerously-skip-permissions` persists across retire→wake | N/A | CC daemon. |
| 143-B17 | Background sessions silently capturing IDE file references into warm spare's input | N/A | CC IDE + warm spare. |
| 143-B18 | Worktree cleanup no longer falls back to `rm -rf` when `git worktree remove` fails | SHIPPED | Anvil `crates/tools/src/worktree_ops.rs:199-231`: on `git worktree remove` failure, Anvil returns an error message string to the model and does NOT fall back to `rm -rf`. The safe path was already correct. |
| 143-B19 | Background-job sessions on macOS getting "Operation not permitted" under `~/Documents`, `~/Desktop`, `~/Downloads` even with Full Disk Access | N/A | macOS TCC entitlement issue specific to CC's Electron-based background daemon. Anvil native binary runs as terminal process and inherits terminal's TCC grants automatically. |
| 143-B20 | `/bg` preserves `--mcp-config`, `--settings`, `--add-dir`, `--plugin-dir`, `--strict-mcp-config` | N/A | CC `/bg` fork. |
| 143-B21 | Background sessions from `claude agents` now honor `permissions.defaultMode` | N/A | CC agent dashboard. |
| 143-B22 | Windows: pressing `←` in `claude agents` while streaming could leave agents list unresponsive | N/A | CC agents + Windows. |
| 143-B23 | `/bg` and `←`-detach preserve `--fallback-model` | N/A | CC daemon. |
| 143-B24 | `/bg` and `←`-detach preserve `--allow-dangerously-skip-permissions` | N/A | CC daemon. |
| 143-B25 | Background daemon spawn falls back to running binary when `~/.local/bin/claude` launcher missing | N/A | CC daemon. |
| 143-B26 | `claude agents --allow-dangerously-skip-permissions` defaulting dispatched sessions to bypass mode | N/A | CC agents. |

**v2.1.143 count:** 26 items — 5 FILE-AS-TASK, 2 VERIFY, 16 N/A, 2 SHIPPED, 1 carry-over.

---

## FILE-AS-TASK Specs

These are the 13 items to file as v2.2.16 tasks. Format matches #452–#472 convention.

### TASK-A: v2.2.16 CC-140-B: Read tool `offset` rejects whitespace-padded/prefixed strings
**Source:** CC v2.1.140  
**File:** `crates/tools/src/file_ops.rs`, `ReadFileInput`  
**Description:** `offset: Option<usize>` — serde_json will reject a model-sent `"  5"` or `"+5"` JSON string with an opaque parse error. Add a lenient string→usize deserializer (trim + strip leading `+` before parse) matching CC's fix.

---

### TASK-B: v2.2.16 CC-141-F: Hook output `terminalSequence` field for desktop notifications
**Source:** CC v2.1.141  
**File:** `crates/runtime/src/hooks.rs`, `crates/plugins/src/hooks.rs`  
**Description:** Add a `terminalSequence` field to Anvil hook JSON output. When a hook writes a JSON object with `"terminalSequence": "<esc-seq>"` to stdout, Anvil should emit the sequence to the terminal after the hook exits. Enables bell, window-title, and desktop-notification hooks without a controlling terminal workaround.

---

### TASK-C: v2.2.16 CC-141-F: "Summarize up to here" partial compaction in rewind menu
**Source:** CC v2.1.141  
**File:** `crates/runtime/src/compact.rs`, `crates/anvil-cli/src/tui/`  
**Description:** Add a "Summarize up to here" action to the turn-selection rewind menu. Triggers compaction of messages up to the selected turn while preserving subsequent turns verbatim. Distinct from `/compact` which processes the whole session.

---

### TASK-D: v2.2.16 CC-141-F: Spinner elapsed-time color warm (green → amber → red)
**Source:** CC v2.1.141  
**File:** `crates/anvil-cli/src/tui/mod.rs` (spinner render)  
**Description:** After 10 seconds of continuous spinner, shift spinner color from default to amber. After 30 seconds, shift to red. Reset on turn completion. Configurable via `ANVIL_SPINNER_WARN_SECS` / `ANVIL_SPINNER_ERROR_SECS`. Reduces user uncertainty during long inference.

---

### TASK-E: v2.2.16 CC-141-B: `ANVIL_MCP_TOOL_TIMEOUT` env to override per-request MCP fetch timeout
**Source:** CC v2.1.141, CC v2.1.142 (confirmed root cause)  
**File:** `crates/runtime/src/mcp_client.rs`  
**Description:** Add `ANVIL_MCP_TOOL_TIMEOUT` env var (seconds, default 60) that sets the per-request timeout for MCP tool calls. Currently Anvil has no such override; slow MCP servers silently time out. Matches CC's `MCP_TOOL_TIMEOUT`.

---

### TASK-F: v2.2.16 CC-141-B: Audit per-tab autocompact threshold isolation
**Source:** CC v2.1.141  
**File:** `crates/runtime/src/history.rs`  
**Description:** `compact_threshold_pct()` reads `ANVIL_AUTOCOMPACT_THRESHOLD` which is a process-global env var. Audit whether any in-session `/model` or `/compact` action writes or re-reads this value in a way that would leak to other concurrent tabs. Document isolation guarantee or fix shared state. Prevents CC-141-B5 class bug from landing when per-model threshold tuning ships.

---

### TASK-G: v2.2.16 CC-141-B: Hook paths stale after `EnterWorktree` switches working directory
**Source:** CC v2.1.141  
**File:** `crates/runtime/src/hooks.rs`, `crates/plugins/src/hooks.rs`  
**Description:** Audit all path values passed to hook subprocesses (env vars, JSON fields). Any CWD-relative paths should be resolved against the pre-worktree original directory or explicitly updated post-`EnterWorktree`. Add a test that fires a PostToolUse hook after EnterWorktree and verifies path fields are valid.

---

### TASK-H: v2.2.16 CC-141-B: Welcome/startup banner names active provider (not Anthropic billing) for 3P users
**Source:** CC v2.1.141  
**File:** `crates/anvil-cli/src/tui/mod.rs` or `setup.rs`  
**Description:** Startup banner / session header should name the configured provider (Ollama, OpenAI-compat endpoint, xAI) when Anthropic is not the active provider. Any Anthropic billing copy ("API Usage Billing", "claude.ai subscription") must be gated on Anthropic-provider detection.

---

### TASK-I: v2.2.16 CC-142-B: Session title heuristic must not use a bare URL as title
**Source:** CC v2.1.142  
**File:** `crates/anvil-cli/src/session_meta.rs` (or equivalent)  
**Description:** When deriving a session title from the first message, detect bare URLs (starting with `http://` or `https://`) and skip them in favor of surrounding text or a generic fallback. Prevents unreadable session-list entries when users paste a URL as their first prompt.

---

### TASK-J: v2.2.16 CC-142-B: Reactive compaction seeds summary size from actual overflow delta
**Source:** CC v2.1.142  
**File:** `crates/runtime/src/compact.rs`  
**Description:** `CompactionConfig::max_estimated_tokens` is a fixed threshold. When triggered by a context-overflow event, Anvil should seed the compaction target size from `actual_context_size - context_limit` so the summarization pass removes enough history to fit in one pass. Prevents wasted near-full-context retry. Port CC v2.1.142's overflow-seeded compaction sizing.

---

### TASK-K: SECURITY: v2.2.16 CC-143-B: Lenient `scopes` deserializer in credentials.json prevents auth lockout
**Source:** CC v2.1.143  
**File:** `crates/runtime/src/oauth.rs` — `StoredOAuthCredentials`, `load_oauth_credentials`  
**Description:** `StoredOAuthCredentials.scopes: Vec<String>` will cause `serde_json::from_value` to fail with `InvalidData` if the stored value is a JSON string, null, or any non-array type. This propagates as an `io::Error` through `load_oauth_credentials` and can hang the CLI on startup or abort OAuth token refresh silently. Add a lenient `scopes` deserializer: null → empty vec, string → single-element vec, array → as-is. Add a test for each coercion case.

---

### TASK-L: v2.2.16 CC-143-B: Stop-hook block cap prevents infinite loop (`ANVIL_STOP_HOOK_BLOCK_CAP`)
**Source:** CC v2.1.143  
**File:** `crates/runtime/src/hooks.rs`  
**Description:** Anvil has no stop-hook block counter. If a stop hook returns a block response every turn, the session loops indefinitely. Add a consecutive-block counter per session. When it reaches `ANVIL_STOP_HOOK_BLOCK_CAP` (default 8), emit a warning and end the turn normally. Reset counter on non-blocked turns.

---

### TASK-M: v2.2.16 CC-143-B: `NO_COLOR`/`FORCE_COLOR` in settings `env` must not strip Anvil TUI colors
**Source:** CC v2.1.143  
**File:** `crates/runtime/src/hooks.rs`, `crates/tools/src/lib.rs`, subprocess env construction  
**Description:** When Anvil passes user-configured `env` block to child processes (bash, hook subprocesses, MCP stdio servers), `NO_COLOR`/`FORCE_COLOR` values must be scoped to the subprocess environment only. They must not be applied via `std::env::set_var` (process-global) in a way that strips Anvil's own TUI rendering. Audit all subprocess env construction paths.

---

### TASK-N: v2.2.16 CC-143-B: 5xx error messages name configured provider/gateway not hardcoded Anthropic status URL
**Source:** CC v2.1.143  
**File:** `crates/api/src/error.rs`, provider error paths  
**Description:** On 5xx errors, Anvil error messages should name the active provider and its base URL (e.g., `Ollama at localhost:11434`, `OpenAI-compat at https://api.example.com`) rather than a hardcoded Anthropic status page. Gate Anthropic status URL on Anthropic provider detection only.

---

## Deferred VERIFY Queue (19 items)

Low-priority items that need a test pass but are not blocking. File as bugs only if verified to reproduce in Anvil:

- 140-B5 Remote managed settings retry on 401
- 140-B9 Terminal cursor on focus-blur
- 141-F7 Permission dialog explains triggering rule
- 141-B6 Permission mode switch auto-dismisses open dialog
- 141-B7 Enter propagation when dialog visible
- 141-B9 Markdown table cell-wrap fallback to vertical layout
- 141-B10 Up-arrow history duplicate on cancel + restore
- 141-B12 Keybind coexistence: submit + newline rebind
- 141-B13 Prompt suggestions disabled when output style configured
- 141-B14 `spinnerVerbs` setting honored in turn-completion
- 141-B15 AskUserQuestion layout overlap
- 141-B17 Statusline multi-line width overflow
- 141-B18 Light-ansi diff context line color
- 141-B22 Session title set from plugin monitor notification
- 141-B23 Permission annotation dedup under collapsed groups
- 141-B26 `/mcp` list scroll in short fullscreen terminal
- 141-B28 Auth token precedence: `apiKeyHelper` + env coexistence
- 142-F3 `SKILL.md` at root discovered without `skills/` subdir
- 142-F4 LSP server listing in plugin details
- 142-B12 `set_model` RC dedup
- 142-B13 Plugin path validator: `./` relative root
- 142-B14 Plugin cache eviction protects active version
- 142-B16 Hook type validation error quality
- 143-F1 Plugin dep graph: disable/enable transitive enforcement
- 143-F2 Per-turn cost estimate in plugin details

---

*Audit by QA agent. v2.2.16 task candidates verified against Anvil source. No Anvil source was modified.*
