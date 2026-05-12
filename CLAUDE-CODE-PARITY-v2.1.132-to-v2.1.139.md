# CC Parity Audit — v2.1.132 through v2.1.139

**Date:** 2026-05-12
**Scope:** 8 CC releases since last parity audit baseline (v2.1.131)
**Format:** Triage by release. Each item: HAVE-IT / MISSING / N/A with file:line evidence or rationale.

Note: "CC" used throughout per `feedback-cc-only-naming.md`. Items prefixed with their release. Tag suffix `-F` (feature) or `-B` (bug fix).

---

## v2.1.132 (2026-05-06)

| Item | Status | Evidence / Action |
|---|---|---|
| 132-F1: `CLAUDE_CODE_SESSION_ID` env in Bash subprocess | **MISSING** | No `ANVIL_SESSION_ID` set in `crates/runtime/src/bash.rs`. CC also passes it to hooks — verify hook side too. **File for v2.2.14.** |
| 132-F2: `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` opt-out | **MISSING** | `crates/anvil-cli/src/tui/mod.rs` enters alt-screen unconditionally. Add `ANVIL_DISABLE_ALTERNATE_SCREEN=1` env check. **File for v2.2.14.** |
| 132-F3: "Pasting…" footer hint during paste | **MISSING** | Paste handler in `tui/input_handler.rs` has no transient footer state. Lower priority. **Defer.** |
| 132-B1: External SIGINT triggers graceful shutdown | **MISSING** | No `tokio::signal::ctrl_c` or `SignalKind` registration found in main.rs or runtime. Anvil only handles Ctrl+C inside TUI loop. **File for v2.2.14 — relevant when daemon (Tier 2 #20-24) ships.** |
| 132-B2: Uncaught exception on terminal close mid-session | **VERIFY** | Native-build issue; needs Anvil-side audit of panic propagation when stdin closes. **File for verification.** |
| 132-B3: `--resume` failing with low-surrogate error on emoji truncation | **VERIFY** | Anvil session.rs parses transcripts; check `crates/runtime/src/session.rs` for utf-16 surrogate handling on load. **File for verification.** |
| 132-B4: `--permission-mode` ignored when resuming plan-mode session | **VERIFY** | Anvil has plan-mode (v2.2.12 task #338). Check that `--permission-mode plan` works with `--continue/--resume`. **File for verification.** |
| 132-B5: Fullscreen blank-screen after sleep/wake until keystroke | **LIKELY-HAVE** | Anvil's `RedrawScheduler` (v2.2.12 T2-G #403) was built for exactly this class. Verify behavior on macOS sleep/wake. |
| 132-B6: Cursor mid-grapheme on Ctrl+E/A/K/U with Indic/ZWJ emoji | **VERIFY** | Anvil input uses `unicode-segmentation`? Check `crates/anvil-cli/src/input/mod.rs`. |
| 132-B7: Vim operators corrupting NFD accented chars | **N/A** | Anvil has no vim-mode operator surface. |
| 132-B8: Pasting `/` swallowing the line | **LIKELY-HAVE** | Anvil paste path (CC paste parity #357) treats placeholders, but verify leading `/` isn't dispatching as slash command. **Verify.** |
| 132-B9: Pasted text + focus/mouse events interleaving stray escapes | **VERIFY** | `tui/input_handler.rs` bracketed-paste state machine — verify escape sequences during paste don't bleed into prompt. |

**v2.1.132 count:** 12 items → 4 missing (file v2.2.14), 6 verify, 1 likely-have, 1 N/A.

---

## v2.1.133 (2026-05-07)

| Item | Status | Evidence / Action |
|---|---|---|
| 133-F1: `worktree.baseRef` setting (`fresh` vs `head`) | **MISSING** | `crates/tools/src/worktree_ops.rs` exists per ROADMAP, but no `base_ref` setting. **File for v2.2.14.** |
| 133-F2: `sandbox.bwrapPath` + `sandbox.socatPath` (Linux/WSL) | **N/A** | Anvil sandbox is per-mode permission gating, not bwrap-based. CC-specific. |
| 133-F3: `parentSettingsBehavior` admin-tier key | **VERIFY** | Anvil has settings tiers per v2.2.11 W10 admin policy floor (#385). Confirm parent-merge behavior matches CC semantics. **Verify against requirements.toml.** |
| 133-F4: `effort.level` in hook JSON + `$CLAUDE_EFFORT` env | **MISSING** | `crates/runtime/src/hooks.rs:243` only has a doc comment mentioning "best-effort" — no `EffortLevel` field. Anvil has effort from v2.2.11 W2 (#377) but it doesn't propagate to hooks. **File for v2.2.14.** |
| 133-F5: Memory release of warm-spare workers under pressure | **N/A** | CC-internal worker pool. Anvil has no equivalent. |
| 133-F6: Improved focus mode behavior | **VERIFY** | Anvil has focus mode (v2.2.x #336/340). Compare against CC behavior. |
| 133-B1: Parallel sessions 401 after refresh-token race | **VERIFY** | Anvil shares credentials across per-tab runtimes — audit token-refresh under concurrent turns. **File for verification.** |
| 133-B2: `Edit`/`Write` allow rules on `C:\` or `/` matching incorrectly | **VERIFY** | Anvil permission rules in `crates/runtime/src/permission_memory.rs` — test root-path matchers. |
| 133-B3: `ECOMPROMISED` unhandled on history file lock | **VERIFY** | `crates/runtime/src/session.rs` lock handling — test with slow disk / clock skew. |
| 133-B4: Esc during compaction showing spurious "Error compacting" | **LIKELY-HAVE** | Anvil compaction has cancel handling but worth re-test. |
| 133-B5: `HTTP(S)_PROXY` / `NO_PROXY` / mTLS not respected in MCP OAuth | **VERIFY** | Anvil MCP client OAuth flow — verify proxy + mTLS env honored across discovery/registration/token-exchange/refresh. |
| 133-B6: Read/Write/Edit denied on mapped network drives via `--add-dir` | **N/A** | Windows-specific behavior; verify when 132-F2 (Windows ssh-agent) work lands. |
| 133-B7: Remote Control stop/interrupt not fully canceling CLI | **LIKELY-HAVE** | Anvil v2.2.12 RC interrupt fixes (#289). Verify queued messages advance correctly. |
| 133-B8: `/effort` cross-session leakage | **VERIFY** | Per-tab effort isolation — test that effort change in tab 1 doesn't propagate to tab 2. |
| 133-B9: Subagents not discovering project/user/plugin skills via Skill tool | **VERIFY** | Anvil TeamDelegate subagents (#290) — confirm they see the same skill registry as parent. **File for verification.** |
| 133-B10: `claude --help` listing `--remote-control` flags | **HAVE-IT** | Anvil `--help` lists RC flags from prior parity work. |

**v2.1.133 count:** 16 items → 2 missing (file v2.2.14), 9 verify, 1 likely-have, 1 have-it, 3 N/A.

---

## v2.1.136 (2026-05-08)

| Item | Status | Evidence / Action |
|---|---|---|
| 136-F1: `CLAUDE_CODE_ENABLE_FEEDBACK_SURVEY_FOR_OTEL` | **N/A** | CC-specific session quality survey. |
| 136-F2: `settings.autoMode.hard_deny` for classifier rules | **SHIPPED v2.2.14** | `crates/runtime/src/auto_mode.rs` + permission_gate short-circuit before hooks when WorkspaceWrite mode. ReadOnly + DangerFullAccess skip the list. |
| 136-B1: MCP servers from `.mcp.json` disappearing after `/clear` in VS Code/JetBrains/SDK | **N/A** | IDE extension behavior; Anvil terminal-native. |
| 136-B2: Login loop on concurrent credential write race | **VERIFY** | Anvil token storage race conditions — audit. |
| 136-B3: MCP OAuth refresh tokens lost on concurrent server refresh | **VERIFY** | Anvil MCP OAuth refresh path — atomicity check. |
| 136-B4: API 400 when extended thinking emits redacted block after tool call | **VERIFY** | Anvil thinking-block handling — verify redacted-block round-trip. |
| 136-B5: `--resume`/`--continue` failing on paths with underscores | **HIGH-PRI VERIFY** | Anvil session path resolution — test underscores. Real risk for users with `my_project/` style dirs. **File for verification.** |
| 136-B6: Plan mode not blocking writes when matching `Edit(...)` allow rule | **HIGH-PRI VERIFY** | Plan-mode is a security-critical gate. Test against Anvil's `crates/runtime/src/conversation/permission_gate.rs`. **File for v2.2.14.** |
| 136-B7: WSL2 image paste fallback via PowerShell | **N/A** | WSL-specific. |
| 136-B8: Plugin `Stop`/`UserPromptSubmit` hooks failing on cache cleanup | **VERIFY** | Anvil hook lifecycle vs plugin cache eviction. |
| 136-B9: Visual consistency across slash command dialogs | **LOW-PRI VERIFY** | Polish-tier; audit dialog footers + spacing. |
| 136-B10: Colors at wrong positions in bash output / markdown code blocks | **VERIFY** | Anvil renderer ANSI handling — bash output position drift. |
| 136-B11: ReasonML diffs rendering corrupted "undefined" at word-diff boundaries | **VERIFY** | Anvil word-diff renderer — test ReasonML samples. Edge case. |
| 136-B12: Worktree exit dialog wrong-dir warning after removal | **N/A** | Anvil worktree flow (#411) different. |
| 136-B13: `@` file picker not matching mid-session new files in small non-git dirs | **VERIFY** | Anvil `@`-mention path resolution. |
| 136-B14: `@`-mention picker not finding >100 files in a dir | **VERIFY** | Picker pagination/listing limits. |
| 136-B15: Failed tool calls not click-to-expand in fullscreen when truncated | **VERIFY** | Anvil tool-card Ctrl+O expand behavior in fullscreen mode. |
| 136-B16: Backspace/Ctrl+Backspace swapped after Ctrl+G editor in persistent-extended-key terminals | **N/A** | CC has Ctrl+G external-editor; Anvil has `/edit` differently. |
| 136-B17: `/usage` weekly reset showing time-of-day instead of calendar date | **VERIFY** | Anvil `/usage` display (#271). |
| 136-B18: Welcome banner ellipsis causing column overflow on CJK | **VERIFY** | Banner width math under CJK locales. |
| 136-B19: `/insights` crash on malformed tool input fields | **VERIFY** | Anvil insights/stats hardening. |
| 136-B20: Renderer crash on collapsibility classification change mid-session | **VERIFY** | Tool-card collapsibility audit. |
| 136-B21: `skills` entry in plugin.json hiding default `skills/` dir | **VERIFY** | Anvil plugin manifest loader (#94). |
| 136-B22: IDE shell-integration lock files not respecting `CLAUDE_CONFIG_DIR` | **N/A** | IDE-specific. |
| 136-B23: Trailing whitespace in copied terminal output during streaming | **VERIFY** | Clipboard copy path. |
| 136-B24: Plugin uninstall/enable/disable case-insensitive slug match | **VERIFY** | Slug-matching audit. |
| 136-B25: Tool error truncation marker negative count on surrogate pairs | **VERIFY** | UTF-16 surrogate handling in truncation math. |
| 136-B26: `CLAUDE_ENV_FILE` SessionStart hooks going stale after `/resume`/`/clear` | **VERIFY** | Anvil's `SessionStart` hook lifecycle. |
| 136-B27: `/branch` saving multi-line session title from pasted multi-line | **VERIFY** | Anvil `/branch` (or equivalent) title sanitization. |
| 136-B28: Stray leading space on second line of wrapped text at column boundary | **VERIFY** | Word-wrap edge case. |
| 136-B29: Esc not dismissing dialogs in `/install-github-app`, `/desktop`, `/resume`, `/web-setup` | **N/A** | CC-specific commands. |
| 136-B30: `/doctor` MCP schema errors not naming missing field + source path | **VERIFY** | Anvil `/doctor` (#415) error verbosity. |
| 136-B31: Bash permission prompt showing internal parser diagnostic | **VERIFY** | User-readable error formatting. |
| 136-B32: Plugin slash commands with spaces not resolving to namespaced form | **VERIFY** | Plugin slash routing. |
| 136-B33: `AskUserQuestion` discarding multi-select array answers | **VERIFY** | Anvil AskUserQuestion handler — array→multi-select unwrap. **File for v2.2.14.** |
| 136-B34: `/clear <name>` not labeling cleared session for `/resume` | **VERIFY** | Session naming on clear. |
| 136-B35: `CronList` missing qualifiers + scheduled prompt | **N/A** | CC-specific CronList tool surface. |
| 136-B36: "Jump to bottom" overlay color artifacts on CJK in fullscreen | **VERIFY** | Anvil scroll-to-bottom overlay. |
| 136-B37: Wide markdown tables stale render in scrollback while streaming | **VERIFY** | Streaming markdown renderer. |
| 136-B38: Pasted text dropped on auto-truncation with placeholder | **VERIFY** | Anvil paste placeholder (#357) + truncation interaction. |
| 136-B39: `/release-notes` stuck on old version after failed refresh | **N/A** | CC-specific. |
| 136-B40: `/mcp` server list not scrolling when overflowing terminal | **VERIFY** | Anvil `/mcp` list (#274 scrollable dialogs). |
| 136-B41: Mid-input slash command autocomplete after initial slash | **VERIFY** | Anvil slash autocomplete. |
| 136-B42: Scroll-to-bottom re-engaging auto-follow with `autoScrollEnabled: false` | **VERIFY** | Setting respected. |
| 136-B43: Prompt suggestions auto-submitted by Enter on empty input | **VERIFY** | Anvil suggestion Enter-handling. |
| 136-B44: Keyboard shortcut hints not reflecting `keybindings.json` rebinds | **VERIFY** | Hint rendering from rebinds. |
| 136-B45: `/settings` language change reverted on Escape | **N/A** | i18n not in Anvil. |
| 136-B46: `/terminal-setup` autocomplete partial-prefix | **N/A** | CC-specific. |
| 136-B47: "Chat about this" on AskUserQuestion erasing question | **VERIFY** | Anvil follow-up-chat flow. |
| 136-B48: MCP tool results invisible when server returns content blocks | **PASS (v2.2.14 audit)** | Anvil already iterates `result.content.iter()` at `crates/anvil-cli/src/providers.rs:1418-1432`; text blocks joined, non-text serialised as JSON. No fix required. |
| 136-B49: Plugin marketplace removal key collision (`r` → retry) | **N/A** | CC-specific keybind. |

**v2.1.136 count:** 49 items → 1 missing, 5 high-pri verify, 28 verify, 1 low-pri, 14 N/A.

---

## v2.1.137 (2026-05-09)

| Item | Status | Evidence / Action |
|---|---|---|
| 137-B1: [VSCode] extension activation on Windows | **N/A** | IDE-specific. |

**v2.1.137 count:** 1 item, 1 N/A.

---

## v2.1.138 (2026-05-09)

| Item | Status | Evidence / Action |
|---|---|---|
| 138-Internal | **N/A** | No user-facing changes. |

**v2.1.138 count:** 0 actionable.

---

## v2.1.139 (2026-05-11)

| Item | Status | Evidence / Action |
|---|---|---|
| 139-F1: Agent view (`claude agents` Research Preview) | **PARTIAL** | Anvil has `anvil agents` CLI action (main.rs:760, 8434) but as plugin browser, NOT cross-session live view. CC's view is "list every running session." Anvil doesn't have a cross-session live monitor surface. **File for v2.2.14 — design new surface or expand existing.** |
| 139-F2: `/goal` (completion condition + persist across turns) | **NAME-COLLISION** | Anvil has `/goal` (v2.2.11 W3 #378) but it's *goal persistence* (track named objectives across sessions). CC v2.1.139 `/goal` is *completion condition* (Claude keeps working autonomously until met, with live overlay panel). Different feature, same name. **File for v2.2.14 — decide: rename one, or unify under shared semantic.** |
| 139-F3: `/scroll-speed` with live preview | **MISSING** | No scroll-speed tuner found in crates. **File for v2.2.14 — small UX win.** |
| 139-F4: `anvil plugin details <name>` showing component inventory + token cost | **MISSING** | No `plugin details` subcommand. Useful for token-economy story. **File for v2.2.14.** |
| 139-F5: Transcript view nav (`?` shortcuts, `{`/`}` jump, `v` panel) | **MISSING** | Anvil transcript view lacks these nav keys. **File for v2.2.14.** |
| 139-F6: Hook `args: string[]` exec form (no shell) | **MISSING** | `crates/runtime/src/hooks.rs` only has shell-string command form. Add exec-array form to avoid quoting hazards. **File for v2.2.14.** |
| 139-F7: Hook `continueOnBlock` for `PostToolUse` | **MISSING** | Hook block semantics — Anvil hooks don't have a "feed rejection back to model" continuation. **File for v2.2.14.** |
| 139-F8: MCP stdio servers receive `CLAUDE_PROJECT_DIR` env | **MISSING** | No `ANVIL_PROJECT_DIR` in MCP child env. Hooks side has `cwd` but MCP needs explicit env. **File for v2.2.14.** |
| 139-F9: Compaction prompt preserves sensitive user instructions | **VERIFY** | Anvil compaction prompt — audit for instruction-preservation language. |
| 139-F10: `/mcp` reconnect picks up `.mcp.json` edits without restart | **VERIFY** | Anvil `/mcp` reconnect — hot-reload behavior. |
| 139-F11: `/context all` per-skill token estimates account for tokenizer | **VERIFY** | Anvil `/context` token counting fidelity (#319 already collapsed grid). |
| 139-F12: `anvil plugin install <name>@<marketplace>` auto-refresh marketplace before fail | **VERIFY** | Plugin install retry-with-refresh path. |
| 139-F13: `/plugin` installed details showing hook event names + MCP server names | **VERIFY** | Plugin details rendering. |
| 139-F14: `/context` showing providing plugin's name for plugin-sourced skills | **VERIFY** | Plugin attribution in `/context`. |
| 139-F15: Remote MCP server reconnect retry for all users | **HAVE-IT** | Anvil MCP retry already (#332). |
| 139-F16: Subagent OTel headers `x-claude-code-agent-id` / `parent-agent-id` | **MISSING** | Anvil TeamDelegate (#290) doesn't emit these OTel attributes. **File for v2.2.14.** |
| 139-F17: Disable RC/`/schedule`/connectors when `ANTHROPIC_API_KEY` set + login exists | **VERIFY** | Anvil auth-mode coexistence — test API key + OAuth login simultaneously. |
| 139-B1: Deadlock on expired credentials + `forceRemoteSettingsRefresh` blocking `claude auth` | **N/A** | CC-specific auth flow. |
| 139-B2: `autoAllowBashIfSandboxed` not auto-approving `$VAR` / `$(cmd)` expansions | **VERIFY** | Anvil auto-allow Bash + shell expansion (#317). |
| 139-B3: Hook writing to terminal corrupting interactive prompt | **VERIFY** | Hook execution stdout/stderr capture. |
| 139-B4: HTTP/SSE MCP unbounded memory growth | **VERIFY** | MCP transport buffer caps — 16 MB per SSE frame in CC. **File for verification.** |
| 139-B5: `Skill(name *)` wildcard not working as prefix match | **HIGH-PRI VERIFY** | Anvil permission rules — test `Skill(*)` prefix vs exact match against `Bash(ls *)` parity. **File for v2.2.14.** |
| 139-B6: Settings hot-reload not detecting symlinked `~/.claude/settings.json` edits | **NO_HOT_RELOAD (v2.2.14 audit)** | Anvil has no settings.json watcher — `notify` crate is not a dependency. ANVIL.md/MEMORY.md are mtime-polled per turn. Settings hot-reload is a v2.3 feature gap, not a v2.2.14 bug. When built, the watcher should canonicalize the path at registration to land symlink-correct on day one. |
| 139-B7: Plugin details failing when marketplace key ≠ manifest name | **VERIFY** | Plugin attribution mismatch handling. |
| 139-B8: `/model` picker "Default" row not reflecting env overrides | **VERIFY** | Anvil `/model` env override display (#341, #355). |
| 139-B9: Spurious "stream idle timeout" 5 min after response complete | **PASS (v2.2.14 audit)** | Anvil uses per-chunk single-shot `tokio::time::timeout` at `crates/api/src/providers/anvil_provider.rs:746` and `crates/api/src/providers/openai_compat.rs:360-364`. No long-lived watchdog task; the timeout future is cancelled by drop when the stream ends. Structurally immune to after-complete false-fire. |
| 139-B10: Silent `exit 1` with 10+ MCP servers + unwritable cache dir | **VERIFY** | Anvil MCP cache directory error path. |
| 139-B11: Typing cursor blinking on tab names, list pointers, dialog rows | **VERIFY** | TUI cursor positioning in non-input contexts. |
| 139-B12: Transcript letter shortcuts not working after mouse click | **VERIFY** | Click-to-focus + keyboard interaction. |
| 139-B13: Bash-mode up-arrow history clobbering in-progress draft | **VERIFY** | Anvil bash-mode history nav. |
| 139-B14: Pasting/dropping multiple images inserting only the last | **NOT_APPLICABLE (v2.2.14 audit)** | Anvil's clipboard path shells out to `osascript`/`xclip` (`crates/anvil-cli/src/tui/widgets.rs:109`); both OS APIs return a single image per pasteboard. Drag-drop iterates correctly in `crates/anvil-cli/src/file_drop.rs:59`. CC-139's fix targets a Node-style multi-entry clipboard API that doesn't apply to Anvil's primitives. |
| 139-B15: Hyperlinks unreadable on dark themes | **VERIFY** | Theme-aware link colors. |
| 139-B16: `/model` picker redundant "Current model" row for opus-alias 3P | **VERIFY** | 3P provider model display. |
| 139-B17: Legacy Opus picker entry on PAYG 3P resolving same model | **N/A** | CC-specific 3P billing. |
| 139-B18: Mouse wheel speed in Cursor/VS Code 1.92–1.104 | **N/A** | IDE-specific. |
| 139-B19: Scroll in Windows Terminal / VS Code when attached to background sessions | **N/A** | IDE/Windows-specific. |
| 139-B20: MCP resources from disconnected servers lingering in `@server:` autocomplete | **VERIFY** | Anvil `@server:` autocomplete dedup. |
| 139-B21: Two-file diff over-reporting truncated lines by one | **VERIFY** | Anvil diff truncation math. |
| 139-B22: Grep results not relativizing Windows drive paths + count mode wrong totals | **VERIFY** | Anvil Grep tool path handling. |
| 139-B23: Border-embedded text overflow on CJK/emoji visual cell width | **VERIFY** | CJK/emoji width math. |
| 139-B24: Fuzzy-match highlighting splitting emoji mid-pair | **VERIFY** | Fuzzy-match grapheme boundary. |
| 139-B25: Skill argument names with regex metacharacters breaking substitution | **VERIFY** | Skill arg-substitution regex escape. |
| 139-B26: ProgressBar full block on almost-full fractional cell | **VERIFY** | ProgressBar rounding. |
| 139-B27: Task polling + `fs.watch` resurrected when last subscriber leaves mid-fetch | **VERIFY** | Subscriber lifecycle. |
| 139-B28: Plugin dep resolution stale count when manifest ≠ source identifier | **VERIFY** | Plugin dep resolver. |
| 139-B29: Insights Time-of-Day chart skewing on unparseable timestamp | **VERIFY** | Anvil `/insights` or `/usage` chart robustness. |
| 139-B30: Keybindings using only cmd/super/win modifier flagged unparseable | **VERIFY** | Keybind parser. |
| 139-B31: `claude_code.active_time.total` OTel metric not emitted in `--print` mode | **VERIFY** | Anvil `--print` mode OTel emission. |
| 139-B32: `claude plugin update` not preserving cross-plugin symlinks | **VERIFY** | Plugin update path. |
| 139-B33: [VSCode] Cmd/Ctrl+Shift+T reopen closed session tab | **N/A** | IDE-specific. |

**v2.1.139 count:** 50+ items → 8 missing (file v2.2.14), 5 high-pri verify, 30+ verify, 1 have-it, 6 N/A.

---

## Triage summary

**File as v2.2.14 missing-feature work (15 items):**
- 132-F1 `ANVIL_SESSION_ID` in Bash subprocess
- 132-F2 `ANVIL_DISABLE_ALTERNATE_SCREEN`
- 132-B1 SIGINT graceful shutdown (pairs with daemon work)
- 133-F1 `worktree.baseRef` setting
- 133-F4 `ANVIL_EFFORT` + hook `effort.level` field
- 136-F2 `autoMode.hard_deny`
- 139-F2 `/goal` name collision — decide rename or unify
- 139-F3 `/scroll-speed`
- 139-F4 `anvil plugin details`
- 139-F5 Transcript nav (`?` `{` `}` `v`)
- 139-F6 Hook `args: string[]` exec form
- 139-F7 Hook `continueOnBlock`
- 139-F8 `ANVIL_PROJECT_DIR` in MCP child env
- 139-F16 Subagent OTel agent-id headers
- 139-F1 Cross-session agent view (design + ship)

**HIGH-PRI verify (7 items):**
- 136-B5 `--resume`/`--continue` on underscore paths
- 136-B6 Plan mode + Edit allow rule write-blocking
- 136-B48 MCP content-block results invisibility
- 139-B5 `Skill(*)` wildcard prefix match
- 139-B6 Settings hot-reload symlink
- 139-B9 Stream-idle 5-min-after-complete false fire
- 139-B14 Multi-image paste inserting only last

**Lower-pri verify queue:** ~50 items deferred to per-feature audit windows. Not blockers for v2.2.14, but each should be a quick test-and-fix when touched.

**N/A (CC-specific or IDE-specific):** ~25 items. No action.

**Total CC-side items audited:** ~128 across 8 releases.

---

## Recommended v2.2.14 CC parity scope

If picking by leverage, the **top 10 v2.2.14 candidates**:

1. **139-F8 `ANVIL_PROJECT_DIR` in MCP env** — small, completes hook/MCP env parity
2. **139-F6 Hook `args: string[]` exec form** — security win (no quoting hazards)
3. **139-F7 Hook `continueOnBlock`** — useful semantic for review hooks
4. **132-F1 `ANVIL_SESSION_ID` in Bash subprocess** — small, completes env story
5. **132-F2 `ANVIL_DISABLE_ALTERNATE_SCREEN`** — user-visible escape hatch
6. **139-F2 `/goal` name collision** — needs decision, then small work
7. **139-B9 Stream-idle watchdog** — verify + likely small fix; high-impact for long inference
8. **136-B5 Underscore path `--resume`** — verify; small fix; common user paths
9. **136-B6 Plan-mode write blocking** — security-critical verify
10. **139-F16 Subagent OTel headers** — small OTel attribute additions

These 10 are all small, mostly bounded, none touch `main.rs` heavily (per modularity rule from 2026-05-12). Most land in `crates/runtime/src/{hooks,mcp,session}.rs` or `crates/anvil-cli/src/tui/`.

**v2.2.14 scope (combined):**
- W11 wire file-fingerprint cache (carry-over)
- W15b wire auto-promote engine (carry-over)
- CC parity top-10 above
- routines daemon Tier 2 items 1-12 (per ROADMAP)

That's a healthy v2.2.14 with no `main.rs` growth.
