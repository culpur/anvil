# Claude Code parity audit: v2.1.118 → v2.1.131

Audited: 2026-05-07. Source: https://github.com/anthropics/claude-code/releases

**Scope note:** Versions v2.1.124, v2.1.125, v2.1.127, and v2.1.130 were never published — neither a tagged GitHub release nor a CHANGELOG entry exists for those numbers. They were skipped in upstream's release sequence. The 14-release range therefore contains 9 actual releases: v2.1.118, v2.1.119, v2.1.120, v2.1.121, v2.1.122, v2.1.123, v2.1.126, v2.1.128, v2.1.129, v2.1.131.

Items already adopted in Anvil (BUG-1..42 / FEAT-28..42 from prior parity sweeps and this session's TeamDelegate / TUI / format_tool work) are NOT re-listed below — the disposition table flags them explicitly only when the upstream release happens to include them.

---

## v2.1.118 (2026-04-23)

**Bug fixes:**
- BUG-118-1: `/mcp` menu hid OAuth Authenticate/Re-authenticate actions for servers configured with `headersHelper`.
- BUG-118-2: HTTP/SSE MCP servers using custom headers got stuck in "needs authentication" after a transient 401 instead of retrying.
- BUG-118-3: MCP OAuth tokens missing `expires_in` forced hourly re-authentication.
- BUG-118-4: MCP step-up authorization silently refreshed instead of prompting the user for re-consent.
- BUG-118-5: Unhandled promise rejection when MCP OAuth flow timed out / was cancelled.
- BUG-118-6: MCP OAuth refresh raced without a cross-process lock — concurrent refreshes overwrote fresh tokens.
- BUG-118-7: macOS keychain race let concurrent MCP token refresh clobber a newer token.
- BUG-118-8: OAuth token refresh failed silently when server revoked the token before local expiry.
- BUG-118-9: Credential save crashed on Linux/Windows, corrupting `~/.claude/.credentials.json`.
- BUG-118-10: `/login` had no effect in sessions launched with `CLAUDE_CODE_OAUTH_TOKEN`.
- BUG-118-11: Unreadable text in "new messages" scroll pill and `/plugin` badges.
- BUG-118-12: Plan-acceptance dialog offered the wrong default option under `--dangerously-skip-permissions`.
- BUG-118-13: Agent-type hooks failed with "Messages are required" for non-Stop / non-SubagentStop events.
- BUG-118-14: `prompt`-type hooks re-fired on tool calls made by agent-hook verifier subagents (already covered conceptually by Anvil BUG-21 fix; this is the hook-side variant).
- BUG-118-15: `/fork` wrote the full parent conversation to disk; now writes a pointer.
- BUG-118-16: Alt+K / Alt+X / Alt+^ / Alt+_ froze keyboard input.
- BUG-118-17: Connecting to a remote session overwrote local `model` setting.
- BUG-118-18: Typeahead error when pasting file paths starting with `/`.
- BUG-118-19: `plugin install` failed to re-resolve dependencies at the wrong versions.
- BUG-118-20: Unhandled errors from file watcher on invalid paths or fd exhaustion.
- BUG-118-21: Remote Control sessions got archived on transient CCR initialization issues.
- BUG-118-22: Subagents resumed via `SendMessage` did not restore explicit `cwd`.

**Features:**
- FEAT-118-1: Vim visual mode (`v`) and visual-line mode (`V`) — selection, operators, visual feedback.
- FEAT-118-2 (already-have): `/usage` consolidating `/cost` + `/stats` — shipped in Anvil as FEAT-28.
- FEAT-118-3: Custom themes — named user themes via `/theme`, hand-edit JSON in `~/.claude/themes/`, plus a plugin `themes/` directory for shipped themes.
- FEAT-118-4 (already-have): Hooks invoke MCP tools via `type: "mcp_tool"` — shipped as FEAT-30.
- FEAT-118-5: `DISABLE_UPDATES` env var blocks all update paths including `claude update`.
- FEAT-118-6: WSL on Windows can inherit Windows-side managed settings via `wslInheritsWindowsSettings` policy key.
- FEAT-118-7: Auto-mode customization — include `"$defaults"` inside `autoMode.allow`/`soft_deny`/`environment` to layer custom rules on top of built-ins.
- FEAT-118-8: "Don't ask again" option on the auto-mode opt-in prompt.
- FEAT-118-9: `claude plugin tag` creates release git tags for plugins with version validation.
- FEAT-118-10: `--continue` / `--resume` find sessions that added the current cwd via `/add-dir`.
- FEAT-118-11: `/color` syncs session accent color to claude.ai/code over Remote Control.
- FEAT-118-12: `/model` picker honors `ANTHROPIC_DEFAULT_*_MODEL_NAME` / `_DESCRIPTION` overrides for custom `ANTHROPIC_BASE_URL` gateways.

**Anvil status:** needs-review — heavy MCP-OAuth concentration; most of these only matter once Anvil ships first-party MCP OAuth. Vim visual mode and custom themes are real user-visible gaps. Plan-mode dialog under DangerFullAccess (BUG-118-12) is worth checking.

---

## v2.1.119 (2026-04-23)

**Bug fixes:**
- BUG-119-1: CRLF content (Windows clipboards, Xcode console) inserted extra blank lines on paste.
- BUG-119-2: Multi-line paste lost newlines under kitty keyboard protocol sequences.
- BUG-119-3: Glob and Grep tools disappeared from advertised toolset when Bash tool was denied.
- BUG-119-4: Scrolling in fullscreen mode snapped back to bottom after every tool completion.
- BUG-119-5: MCP HTTP connections failed with "Invalid OAuth error response".
- BUG-119-6: Rewind overlay showed "(no prompt)" for messages with image attachments.
- BUG-119-7: Auto-mode overrode plan-mode with conflicting instructions.
- BUG-119-8: Async `PostToolUse` hooks wrote empty entries to session transcript.
- BUG-119-9: Tool-search unsupported-beta-header error on Vertex AI.
- BUG-119-10: `@`-file Tab completion replaced the entire prompt inside slash commands.
- BUG-119-11: Stray `p` character at prompt on macOS Terminal.app.
- BUG-119-12: `${ENV_VAR}` placeholders in MCP server headers were not substituted.
- BUG-119-13: MCP OAuth client secret was not sent during token exchange.
- BUG-119-14: `/skills` Enter key closed the dialog instead of pre-filling the chosen skill.
- BUG-119-15: `/agents` mislabeled unavailable tools as "Unrecognized".
- BUG-119-16: MCP servers from plugins did not spawn on Windows.
- BUG-119-17: `/export` showed current default model instead of actual conversation model.
- BUG-119-18: Verbose-output setting did not persist after restart.
- BUG-119-19: `/usage` progress bars overlapped their labels.
- BUG-119-20: Plugin MCP servers failed when `${user_config.*}` fields were optional.
- BUG-119-21: List items with sentence-final numbers wrapped incorrectly.
- BUG-119-22: `/plan` and `/plan open` did nothing on existing plans.
- BUG-119-23 (already-have): Skills invoked before auto-compaction were re-executed — Anvil BUG-21.
- BUG-119-24: `/reload-plugins` and `/doctor` reported errors for disabled plugins.
- BUG-119-25: Agent tool with `isolation: "worktree"` reused stale worktrees.
- BUG-119-26: Disabled MCP servers appeared as "failed".
- BUG-119-27: `TaskList` returned tasks in arbitrary order instead of sorted by ID.
- BUG-119-28: Spurious "GitHub API rate limit exceeded" hints.
- BUG-119-29: SDK/bridge `read_file` did not enforce the size cap correctly.
- BUG-119-30: PR did not link back to the originating session inside git worktrees.
- BUG-119-31: `/doctor` warned about overridden MCP server entries.
- BUG-119-32: False-positive Windows MCP config warnings.
- BUG-119-33: Voice-dictation first recording produced nothing on macOS.

**Features:**
- FEAT-119-1: `/config` settings (theme, editor mode, verbose, etc.) persist to `~/.claude/settings.json` with project / local / policy precedence.
- FEAT-119-2: `prUrlTemplate` setting — point the footer PR badge at custom code-review URLs instead of github.com.
- FEAT-119-3: `CLAUDE_CODE_HIDE_CWD` env var hides the cwd in startup logo.
- FEAT-119-4: `--from-pr` accepts GitLab MR, Bitbucket PR, and GitHub Enterprise PR URLs.
- FEAT-119-5: `--print` mode honors agent's `tools:` / `disallowedTools:` frontmatter.
- FEAT-119-6: `--agent <name>` honors the agent's `permissionMode` for built-in agents.
- FEAT-119-7: PowerShell tool commands can be auto-approved in permission mode.
- FEAT-119-8: `PostToolUse` and `PostToolUseFailure` hooks include `duration_ms`.
- FEAT-119-9: Subagent / SDK MCP-server reconfigure connects servers in parallel.
- FEAT-119-10: Plugins pinned by another plugin's version constraint auto-update to the highest satisfying git tag.
- FEAT-119-11: Vim — Esc in INSERT no longer pulls queued messages back into input.
- FEAT-119-12: Slash-command suggestion highlighting and multi-line description wrapping improved.
- FEAT-119-13: `owner/repo#N` shorthand uses the git remote's host instead of always github.com.
- FEAT-119-14: OTel — `tool_result` / `tool_decision` events include `tool_use_id`; `tool_result` adds `tool_input_size_bytes`.
- FEAT-119-15: Status line JSON includes `effort.level` and `thinking.enabled`.

**Security:**
- SEC-119-1: `blockedMarketplaces` correctly enforces `hostPattern` and `pathPattern` entries.

**Anvil status:** needs-mirror — multiple correctness / UX bugs (CRLF paste, Glob/Grep disappearing when Bash denied, scroll snap, Tab-completion path replace) are likely reproducible in Anvil. Settings persistence (FEAT-119-1) and `--from-pr` multi-host (FEAT-119-4) are workflow features Anvil should reach for v2.3.

---

## v2.1.120 (2026-04-25)

**Bug fixes:**
- BUG-120-1: Esc during stdio MCP tool calls closed the entire server connection (regression).
- BUG-120-2: `/rewind` and interactive overlays unresponsive after `--resume` startup.
- BUG-120-3: Terminal scrollback duplication in non-fullscreen mode.
- BUG-120-4: "Dangerous rm operation" false-positive permission prompts in auto mode for multi-line bash with pipes/redirects.
- BUG-120-5: Telemetry settings did not suppress usage metrics correctly.
- BUG-120-6: Selection-menu clipping in fullscreen mode.
- BUG-120-7: Plugin marketplace loading / installation issues.

**Features:**
- FEAT-120-1: `claude ultrareview [target]` — runs `/ultrareview` non-interactively from CI/scripts, prints findings to stdout.
- FEAT-120-2: Skills can reference current effort level via `${CLAUDE_EFFORT}` variable.
- FEAT-120-3: Subprocess `AI_AGENT` env var for `gh`-traffic attribution.
- FEAT-120-4: Auto-compact in auto mode shows `auto` label instead of token counts.
- FEAT-120-5: Hidden spinner tips for desktop app / skills / agents when already installed.
- FEAT-120-6: "Use PgUp/PgDn to scroll" hint shown when terminal sends arrow keys instead of scroll wheel.
- FEAT-120-7: Faster session startup with many claude.ai connectors.
- FEAT-120-8: Auto-mode denial messages link to configuration documentation.
- FEAT-120-9: `claude plugin validate` enhancements.

**Anvil status:** needs-review — `anvil ultrareview` (FEAT-120-1) is a high-value CI integration that maps cleanly to Anvil's `/security-review` skill. Dangerous-rm false-positive (BUG-120-4) is worth a regression test in Anvil's permission classifier.

---

## v2.1.121 (2026-04-28)

**Bug fixes:**
- BUG-121-1: Multi-GB RSS growth when processing many images.
- BUG-121-2 (already-have): `/usage` ~2GB memory leak — Anvil BUG-31.
- BUG-121-3: Memory leak when long-running tools failed to emit progress events.
- BUG-121-4 (already-have): Bash tool unusable when startup directory deleted mid-session — Anvil BUG-17.
- BUG-121-5: `--resume` crashed on startup in external builds.
- BUG-121-6: `--resume` corruption recovery — now skips corrupt lines instead of crashing on large sessions.
- BUG-121-7: Bedrock `thinking.type.enabled is not supported` error with Bedrock ARNs.
- BUG-121-8: Microsoft 365 OAuth duplicate / unsupported `prompt` parameter.
- BUG-121-9: Scrollback duplication on tmux, GNOME Terminal, Windows Terminal, Konsole with Ctrl+L.
- BUG-121-10: claude.ai MCP connectors disappeared on transient auth errors.
- BUG-121-11: Built-in tool "Always allow" rules survive worker restarts in remote sessions.
- BUG-121-12: `NO_PROXY` honored for all HTTP clients in native build.
- BUG-121-13: Managed-settings approval prompt no longer exits the session.
- BUG-121-14: `/usage` rate-limiting fixed by auto-refreshing stale OAuth token.
- BUG-121-15 (already-have): Invalid legacy enums no longer invalidate entire `settings.json` — Anvil BUG-34/35.
- BUG-121-16: `/usage` dialog clipping when no-flicker mode is off.
- BUG-121-17: `/focus` "Unknown command" when fullscreen renderer is off.
- BUG-121-18: Embedded grep/find/rg fall back to installed tools when bundled binary deleted.
- BUG-121-19: Reduced peak fd usage on large directory trees during find.

**Features:**
- FEAT-121-1 (already-have): `alwaysLoad` MCP option — Anvil FEAT-41.
- FEAT-121-2: `claude plugin prune` removes orphaned auto-installed plugin deps; `plugin uninstall --prune` cascades.
- FEAT-121-3 (already-have): Type-to-filter on `/skills` — Anvil FEAT-34.
- FEAT-121-4: PostToolUse hooks can replace tool output for ALL tools via `hookSpecificOutput.updatedToolOutput` (was MCP-only).
- FEAT-121-5: Fullscreen typing no longer jumps scroll to bottom after scrolling up.
- FEAT-121-6 (already-have): Scrollable dialogs — Anvil FEAT-36.
- FEAT-121-7: Clicking wrapped URLs in fullscreen mode opens the full URL.
- FEAT-121-8: `CLAUDE_CODE_FORK_SUBAGENT` works in non-interactive SDK and `claude -p`.
- FEAT-121-9: `--dangerously-skip-permissions` no longer prompts for `.claude/skills/`, `.claude/agents/`, `.claude/commands/` writes.
- FEAT-121-10: iTerm2 clipboard support via `/terminal-setup` (incl. tmux).
- FEAT-121-11: MCP servers auto-retry up to 3 times on transient startup errors.
- FEAT-121-12: Terminal session title generated in configured language.
- FEAT-121-13: Claude.ai connectors with same upstream URL deduplicated.
- FEAT-121-14: Vertex AI mTLS — X.509 cert-based Workload Identity Federation.
- FEAT-121-15: Faster startup — Recent Activity panel removed from release-notes splash.
- FEAT-121-16: LSP diagnostic summaries expand on click / Ctrl+O.
- FEAT-121-17: SDK MCP `mcp_authenticate` supports `redirectUri` for custom-scheme completion.
- FEAT-121-18: OTel — `stop_reason`, `gen_ai.response.finish_reasons`, `user_system_prompt`.
- FEAT-121-19: VSCode voice dictation respects `accessibility.voice.speechLanguage`.
- FEAT-121-20: VSCode `/context` opens native token-usage dialog.

**Anvil status:** needs-mirror — image-RSS leak (BUG-121-1), tool-progress leak (BUG-121-3), and Ctrl+L scrollback dup (BUG-121-9) are real user-hit bugs. PostToolUse `updatedToolOutput` for all tools (FEAT-121-4) and `claude plugin prune` (FEAT-121-2) are clean wins.

---

## v2.1.122 (2026-04-28)

**Bug fixes:**
- BUG-122-1: `/branch` produced forks that failed with "tool_use ids without tool_result blocks" when source session contained rewound entries.
- BUG-122-2: `/model` did not show Effort option for Bedrock application-inference-profile ARNs; ARNs did not receive `output_config.effort`.
- BUG-122-3: Vertex / Bedrock returned `output_config: Extra inputs are not permitted` on session-title generation and structured output.
- BUG-122-4: Vertex AI `count_tokens` returned 400 behind proxy gateways.
- BUG-122-5: `spinnerTipsOverride.excludeDefault` did not suppress time-based tips.
- BUG-122-6: `!exit` / `!quit` in bash mode terminated the CLI instead of running as shell command.
- BUG-122-7: Images sent to newer models resized to 2576px instead of correct 2000px max.
- BUG-122-8: Remote Control session idle status redrew twice/sec, flooding tmux -CC control pipes.
- BUG-122-9: Assistant messages appeared blank in some sessions due to a stale view preference.
- BUG-122-10 (already-have): Malformed hooks entry in `settings.json` no longer invalidates the file — Anvil BUG-34/35.
- BUG-122-11: Voice-mode keybindings bound to Caps Lock now show an error (terminals don't deliver Caps Lock as key event).
- BUG-122-12: `/mcp` clarified message when MCP server still unauthorized after browser sign-in.

**Features:**
- FEAT-122-1: `ANTHROPIC_BEDROCK_SERVICE_TIER` env var (`default`/`flex`/`priority`) sent as `X-Amzn-Bedrock-Service-Tier` header.
- FEAT-122-2: Pasting a PR URL into `/resume` finds the session that created that PR (GitHub / GitHub Enterprise / GitLab / Bitbucket).
- FEAT-122-3: `/mcp` shows claude.ai connectors hidden by manually-added servers with same URL, hints to remove duplicate.
- FEAT-122-4: OTel numeric attributes on `api_request`/`api_error` emitted as numbers; new `claude_code.at_mention` event for `@`-mention resolution.
- FEAT-122-5: ToolSearch picks up MCP tools that connected after session start in nonblocking mode.

**Anvil status:** needs-review — most items are vendor-gateway (Bedrock/Vertex) or `/branch` corner cases. PR-URL `/resume` lookup (FEAT-122-2) maps to Anvil session search and is genuinely useful. ToolSearch late-MCP discovery (FEAT-122-5) needs an Anvil-specific check.

---

## v2.1.123 (2026-04-29)

**Bug fixes:**
- BUG-123-1: OAuth authentication 401 retry-loop when `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1`.

**Features:** none.

**Anvil status:** not-applicable — Anvil doesn't gate features on a `DISABLE_EXPERIMENTAL_BETAS` flag in the same way; no analog.

---

## v2.1.126 (2026-05-01)

**Bug fixes:**
- BUG-126-1: `allowManagedDomainsOnly` / `allowManagedReadPathsOnly` ignored when higher-priority managed-settings sources lacked a `sandbox` block.
- BUG-126-2: Images >2000px broke sessions; now auto-downscaled on paste.
- BUG-126-3: Login screen appeared for "OAuth not allowed for organization" errors.
- BUG-126-4: OAuth login timeouts on slow / proxied connections and IPv6-only devcontainers.
- BUG-126-5: Race that cleared valid OAuth refresh tokens.
- BUG-126-6: API retry countdown stuck at "0s".
- BUG-126-7 (already-have): Stream idle timeout after Mac sleep / long thinking — Anvil BUG-15.
- BUG-126-8: Background / remote sessions falsely aborted with "Stream idle timeout".
- BUG-126-9: Assistant finished thinking with no output after empty turns.
- BUG-126-10: Overly fast trackpad scrolling in Cursor and VS Code 1.92–1.104.
- BUG-126-11: claude.ai MCP connectors suppressed by manual servers stuck in needs-auth.
- BUG-126-12: Japanese / Korean / Chinese text rendered as garbled on Windows.
- BUG-126-13: Ctrl+L cleared prompt input.
- BUG-126-14: Deferred tools unavailable to skills with `context: fork`.
- BUG-126-15: Plan-mode tools unavailable in interactive sessions with `--channels`.
- BUG-126-16: `/plugin` Uninstall reported incorrect status.
- BUG-126-17: File-modified reminders unbounded when linters touched many files.
- BUG-126-18: `/remote-control` retries appeared stuck.
- BUG-126-19: Windows clipboard exposed content in EDR/SIEM telemetry.
- BUG-126-20: PowerShell bare `--` misidentified as `--%` stop-parsing token.
- BUG-126-21: Agent SDK hung on malformed parallel tool calls.

**Features:**
- FEAT-126-1: `/model` picker lists models from gateway's `/v1/models` endpoint when `ANTHROPIC_BASE_URL` points at an Anthropic-compatible gateway.
- FEAT-126-2 (already-have): `claude project purge [path]` — Anvil FEAT-39 (`anvil project purge`).
- FEAT-126-3: `--dangerously-skip-permissions` bypasses prompts for `.claude/`, `.git/`, `.vscode/`, shell config, other protected paths (catastrophic-removal commands still prompt).
- FEAT-126-4 (already-have): OAuth login accepts pasted code when localhost callback unreachable — Anvil FEAT-40.
- FEAT-126-5: OTel `claude_code.skill_activated` fires for user-typed slash commands; new `invocation_trigger` attribute (`user-slash` / `claude-proactive` / `nested-skill`).
- FEAT-126-6: Auto-mode spinner turns red when permission check stalls (vs looking like the tool is running).
- FEAT-126-7: Improved PowerShell 7 detection (Microsoft Store, MSI without PATH, .NET global tool); PowerShell treated as primary shell when enabled.
- FEAT-126-8: Host-managed deployments no longer auto-disable analytics on Bedrock/Vertex/Foundry.

**Anvil status:** needs-mirror — image >2000px session break (BUG-126-2), Ctrl+L clearing prompt (BUG-126-13), CJK garble on Windows (BUG-126-12), and empty-turn finish (BUG-126-9) are concrete user-hit issues. Auto-downscale-on-paste (BUG-126-2) is a clean defensive fix.

---

## v2.1.128 (2026-05-04)

**Bug fixes:**
- BUG-128-1: Focus mode briefly dimmed previous response when submitting a new prompt.
- BUG-128-2: Stray desktop notifications on `/exit` in Kitty / OSC-9 terminals.
- BUG-128-3: Remote Control showed empty "Opening your options…" on rate limit.
- BUG-128-4: Drag-and-drop image upload hung on "Pasting text…" when image read failed.
- BUG-128-5: Crash loop when piping >10 MB to `claude -p` via stdin.
- BUG-128-6: Long URLs not individually clickable on wrapped rows in fullscreen mode.
- BUG-128-7: `/plugin` Components panel error for plugins loaded via `--plugin-dir`.
- BUG-128-8: MCP tool results dropped images with structured content.
- BUG-128-9: Fenced code blocks in lists retained leading whitespace on copy-paste.
- BUG-128-10: Tab navigation in `/config` stranded focus.
- BUG-128-11: Markdown link labels lost on terminals without OSC 8 support.
- BUG-128-12: Sessions on 1M-context models falsely blocked with "Prompt is too long".
- BUG-128-13 (already-have): Parallel-shell failing read-only command no longer cancels siblings — Anvil BUG-28.
- BUG-128-14: Banner showed effort labels on unsupported models.
- BUG-128-15: `/fast` fuzzy-matched on 3P providers.
- BUG-128-16: Bedrock default model resolved incorrectly.
- BUG-128-17: Vim NORMAL-mode `Space` key behavior fixed.
- BUG-128-18: Terminal progress indicator flickered between tool calls.
- BUG-128-19: `/rename` failed on resumed sessions with compact boundaries.
- BUG-128-20: Stale status lines from prior sessions after `--resume`/`--continue`.
- BUG-128-21: Stale plugin cache directory entries polluting PATH.
- BUG-128-22: MCP stdio servers received corrupted arguments with spaces.
- BUG-128-23: Sub-agent progress summaries missing prompt-cache data.
- BUG-128-24: `/plugin update` failed to detect npm-sourced plugin versions.
- BUG-128-25: Sub-agent summaries fired repeatedly on idle sub-agents.

**Features:**
- FEAT-128-1: Bare `/color` command picks a random session color.
- FEAT-128-2: `/mcp` shows tool count for connected servers; flags servers that connected with 0 tools.
- FEAT-128-3 (already-have): `--plugin-dir` accepts `.zip` archives — Anvil FEAT-42.
- FEAT-128-4: `--channels` works with console (API-key) auth; orgs with managed settings must set `channelsEnabled: true`.
- FEAT-128-5: `/model` picker collapsed duplicate Opus 4.7 entries; current Opus shown as "Opus".
- FEAT-128-6: Subprocesses (Bash, hooks, MCP, LSP) no longer inherit `OTEL_*` env vars.
- FEAT-128-7: `workspace` is now a reserved MCP server name.
- FEAT-128-8: MCP reconnect re-announced tools summarized by server prefix instead of full list flood.
- FEAT-128-9: SDK hosts receive a persistent `localSettings` suggestion for Bash permission prompts.
- FEAT-128-10: `EnterWorktree` creates branches from local HEAD as documented (was `origin/<default>`).
- FEAT-128-11: Auto-mode classifier errors include hints (retry / `/compact` / `--debug`).
- FEAT-128-12: `--output-format stream-json` includes `--plugin-dir` load failures in `init.plugin_errors`.

**Anvil status:** needs-mirror — stdin >10MB crash (BUG-128-5), MCP stdio arg corruption with spaces (BUG-128-22), `/rename` on compacted sessions (BUG-128-19), and stale status lines after `--resume` (BUG-128-20) are correctness bugs. `/mcp` tool-count + zero-tools warning (FEAT-128-2) is a small but useful diagnostic. OTel-env subprocess isolation (FEAT-128-6) is a quiet but real telemetry-leak fix.

---

## v2.1.129 (2026-05-06)

**Bug fixes:**
- BUG-129-1: API errors with unrecognized 400 status codes showed raw JSON instead of underlying message.
- BUG-129-2: `/clear` did not reset terminal tab title.
- BUG-129-3: Session-title chip from `/rename` disappeared while a permission/other dialog was active.
- BUG-129-4: Agent panel below prompt was hidden when subagents ran (regression in 2.1.122).
- BUG-129-5: External-editor handoff (Ctrl+G) blanked conversation history above prompt.
- BUG-129-6: `/context` dumped its rendered ASCII grid into conversation (~1.6k token waste/call).
- BUG-129-7: `/agents` Library list arrow-key navigation kept highlight visible when list exceeds viewport.
- BUG-129-8: `/branch` success message did not include new branch's session id for `/resume`.
- BUG-129-9: Bold headers with keycap/ZWJ/skin-tone emoji lost trailing chars in fullscreen.
- BUG-129-10: Server-managed settings policy did not apply for enterprise/team users whose stored OAuth credentials lacked `user:inference` scope.
- BUG-129-11: OAuth refresh race after wake-from-sleep could log out all running sessions.
- BUG-129-12: 1-hour prompt-cache TTL silently downgraded to 5 minutes.
- BUG-129-13: Cache-miss warning appeared spuriously after `/clear` or compaction when changing `/effort` or `/model`.
- BUG-129-14: `Bash(mkdir *)`, `Bash(touch *)` allow rules not honored for in-project paths.
- BUG-129-15: `deniedMcpServers` patterns with `*://` scheme wildcard did not match mixed-case hostnames.
- BUG-129-16: Harmless WebSocket warning logged as error in `--debug` during voice mode.
- BUG-129-17: `/clear` did not clear conversation context and displayed transcript in VSCode.

**Features:**
- FEAT-129-1 (already-have): `--plugin-url <url>` fetches plugin .zip from URL — Anvil FEAT-42.
- FEAT-129-2: `CLAUDE_CODE_FORCE_SYNC_OUTPUT=1` force-enables synchronized output (e.g., Emacs `eat`).
- FEAT-129-3: `CLAUDE_CODE_PACKAGE_MANAGER_AUTO_UPDATE` for Homebrew/WinGet — runs upgrade in background, prompts to restart.
- FEAT-129-4: Plugin manifests should declare `themes` and `monitors` under `experimental:` (top-level still works but warns from `claude plugin validate`).
- FEAT-129-5: Gateway `/v1/models` discovery now opt-in via `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` (was automatic in 126–128).
- FEAT-129-6: Ctrl+R history picker searches all prompts across all projects (pre-2.1.124 behavior); Ctrl+S narrows to current project/session.
- FEAT-129-7: `skillOverrides` accepts `off` / `user-invocable-only` / `name-only`.
- FEAT-129-8: OTel `claude_code.pull_request.count` now counts MCP-tool PRs not just shell.
- FEAT-129-9: Policy-refusal error messages include API Request ID for support debugging.

**Anvil status:** needs-mirror — 1-hour cache TTL silent downgrade (BUG-129-12) is a CACHING REGRESSION worth checking against Anvil's BUG-26 cache_control implementation immediately. `/clear` not clearing transcript (BUG-129-17) and Bash allow-rule regression (BUG-129-14) are real. `CLAUDE_CODE_PACKAGE_MANAGER_AUTO_UPDATE` (FEAT-129-3) maps to Anvil's `anvil upgrade` self-update path.

---

## v2.1.131 (2026-05-06)

**Bug fixes:**
- BUG-131-1: VS Code extension failed to activate on Windows (hardcoded build path in bundled SDK; `createRequire` polyfill bug).
- BUG-131-2: Mantle endpoint authentication missing `x-api-key` header.

**Features:** none.

**Anvil status:** not-applicable — VS Code extension is CC-only; no Anvil VS Code extension yet. Mantle endpoint is an Anthropic-internal gateway that Anvil doesn't target.

---

## Dispositions

Triage table — each row is one item Anvil should consider. "already-have" items are listed once for completeness with a reason. Versions v2.1.124, v2.1.125, v2.1.127, v2.1.130 don't exist upstream so have no rows.

| ID | Version | Type | Title | Anvil status | Priority |
|----|---------|------|-------|--------------|----------|
| BUG-118-12 | 2.1.118 | bug | Plan-acceptance dialog wrong default under DangerFullAccess | needs-review | med |
| BUG-118-15 | 2.1.118 | bug | `/fork` writes full parent conversation (should be pointer) | needs-review | low |
| BUG-118-16 | 2.1.118 | bug | Alt+K/X/^/_ freeze keyboard input | needs-mirror | med |
| BUG-118-17 | 2.1.118 | bug | Connecting to remote session overwrites local model setting | needs-review | med |
| BUG-118-22 | 2.1.118 | bug | Subagents resumed via SendMessage lose explicit cwd | needs-mirror | med |
| FEAT-118-1 | 2.1.118 | feat | Vim visual mode + visual-line mode | needs-mirror | med |
| FEAT-118-3 | 2.1.118 | feat | Custom user themes + plugin themes/ dir | needs-mirror | low |
| FEAT-118-5 | 2.1.118 | feat | `DISABLE_UPDATES` env var blocks all update paths | needs-mirror | low |
| FEAT-118-7 | 2.1.118 | feat | Auto-mode `"$defaults"` layering in allow/soft_deny | needs-review | low |
| FEAT-118-9 | 2.1.118 | feat | `claude plugin tag` for release tags | needs-review | low |
| FEAT-118-10 | 2.1.118 | feat | `--continue`/`--resume` find sessions that `/add-dir`'d cwd | needs-mirror | med |
| FEAT-118-12 | 2.1.118 | feat | `/model` picker honors ANTHROPIC_DEFAULT_*_MODEL_NAME overrides | needs-review | low |
| BUG-119-1 | 2.1.119 | bug | CRLF paste inserts extra blank lines (Windows clip / Xcode) | needs-mirror | high |
| BUG-119-2 | 2.1.119 | bug | Multi-line paste loses newlines under kitty kbd protocol | needs-mirror | high |
| BUG-119-3 | 2.1.119 | bug | Glob/Grep tools disappear when Bash is denied | needs-mirror | high |
| BUG-119-4 | 2.1.119 | bug | Fullscreen scroll snaps back to bottom after every tool | needs-mirror | med |
| BUG-119-7 | 2.1.119 | bug | Auto-mode overrides plan-mode | needs-review | med |
| BUG-119-10 | 2.1.119 | bug | `@`-file Tab completion replaces entire prompt in slash cmd | needs-mirror | high |
| BUG-119-17 | 2.1.119 | bug | `/export` shows default model not actual conversation model | needs-mirror | med |
| BUG-119-18 | 2.1.119 | bug | Verbose output setting doesn't persist | needs-mirror | med |
| BUG-119-21 | 2.1.119 | bug | List items with sentence-final numbers wrap incorrectly | needs-mirror | low |
| BUG-119-25 | 2.1.119 | bug | Agent worktree isolation reuses stale worktrees | needs-review | med |
| BUG-119-27 | 2.1.119 | bug | TaskList returns tasks in arbitrary order (not sorted by ID) | needs-mirror | med |
| BUG-119-29 | 2.1.119 | bug | SDK/bridge `read_file` doesn't enforce size cap | needs-mirror | high |
| FEAT-119-1 | 2.1.119 | feat | `/config` settings persist with project/local/policy precedence | needs-mirror | high |
| FEAT-119-2 | 2.1.119 | feat | `prUrlTemplate` for custom code-review hosts | needs-mirror | med |
| FEAT-119-3 | 2.1.119 | feat | `CLAUDE_CODE_HIDE_CWD` env var | needs-mirror | low |
| FEAT-119-4 | 2.1.119 | feat | `--from-pr` accepts GitLab/Bitbucket/GHE URLs | needs-mirror | med |
| FEAT-119-5 | 2.1.119 | feat | `--print` honors agent tools/disallowedTools frontmatter | needs-mirror | med |
| FEAT-119-6 | 2.1.119 | feat | `--agent` honors agent permissionMode | needs-mirror | med |
| FEAT-119-8 | 2.1.119 | feat | PostToolUse / PostToolUseFailure hooks include duration_ms | needs-mirror | med |
| FEAT-119-9 | 2.1.119 | feat | Subagent / SDK MCP-server reconfigure connects in parallel | needs-mirror | med |
| FEAT-119-13 | 2.1.119 | feat | `owner/repo#N` shorthand uses git remote host | needs-mirror | low |
| FEAT-119-14 | 2.1.119 | feat | OTel tool_result includes tool_input_size_bytes | needs-review | low |
| BUG-120-1 | 2.1.120 | bug | Esc during stdio MCP closes server connection | needs-mirror | high |
| BUG-120-2 | 2.1.120 | bug | `/rewind` overlays unresponsive after `--resume` | needs-mirror | med |
| BUG-120-3 | 2.1.120 | bug | Terminal scrollback duplication in non-fullscreen mode | needs-mirror | med |
| BUG-120-4 | 2.1.120 | bug | Dangerous-rm false-positive on multi-line bash with pipes | needs-mirror | high |
| FEAT-120-1 | 2.1.120 | feat | `claude ultrareview` non-interactive CI invocation | needs-mirror | high |
| FEAT-120-2 | 2.1.120 | feat | Skills reference effort via `${CLAUDE_EFFORT}` | needs-mirror | low |
| FEAT-120-3 | 2.1.120 | feat | `AI_AGENT` subprocess env var for `gh` attribution | needs-mirror | low |
| FEAT-120-6 | 2.1.120 | feat | "Use PgUp/PgDn" hint when terminal sends arrows | needs-mirror | low |
| BUG-121-1 | 2.1.121 | bug | Multi-GB RSS leak processing many images | needs-mirror | high |
| BUG-121-3 | 2.1.121 | bug | Memory leak when long-running tools fail to emit progress | needs-mirror | high |
| BUG-121-5 | 2.1.121 | bug | `--resume` startup crash in external builds | needs-review | med |
| BUG-121-6 | 2.1.121 | bug | `--resume` skips corrupt lines instead of crashing | needs-mirror | med |
| BUG-121-9 | 2.1.121 | bug | Ctrl+L scrollback duplication on tmux/GNOME/WT/Konsole | needs-mirror | high |
| BUG-121-12 | 2.1.121 | bug | NO_PROXY honored for all HTTP clients | needs-mirror | med |
| BUG-121-13 | 2.1.121 | bug | Managed-settings approval prompt no longer exits session | needs-review | low |
| BUG-121-18 | 2.1.121 | bug | Embedded grep/find/rg fall back when bundled binary deleted | needs-review | low |
| FEAT-121-2 | 2.1.121 | feat | `claude plugin prune` for orphaned plugin deps | needs-mirror | med |
| FEAT-121-4 | 2.1.121 | feat | PostToolUse hooks can replace output for ALL tools | needs-mirror | med |
| FEAT-121-5 | 2.1.121 | feat | Fullscreen typing doesn't snap scroll to bottom | needs-mirror | med |
| FEAT-121-7 | 2.1.121 | feat | Wrapped URL clicks open the full URL in fullscreen | needs-mirror | low |
| FEAT-121-8 | 2.1.121 | feat | `CLAUDE_CODE_FORK_SUBAGENT` works in `claude -p` | needs-review | low |
| FEAT-121-9 | 2.1.121 | feat | `--dangerously-skip-permissions` doesn't prompt for .claude/ writes | needs-mirror | med |
| FEAT-121-10 | 2.1.121 | feat | iTerm2 clipboard via `/terminal-setup` (incl. tmux) | needs-mirror | low |
| FEAT-121-11 | 2.1.121 | feat | MCP servers auto-retry up to 3 times on startup error | needs-mirror | med |
| FEAT-121-16 | 2.1.121 | feat | LSP diagnostic summaries expand on click / Ctrl+O | needs-review | low |
| BUG-122-1 | 2.1.122 | bug | `/branch` produces forks with orphan tool_use ids from rewind | needs-review | med |
| BUG-122-7 | 2.1.122 | bug | Images resized to 2576px instead of 2000px max | needs-mirror | med |
| BUG-122-8 | 2.1.122 | bug | Remote Control idle redraws 2x/s, floods tmux -CC | needs-mirror | high |
| BUG-122-9 | 2.1.122 | bug | Assistant messages appear blank from stale view preference | needs-review | med |
| FEAT-122-2 | 2.1.122 | feat | Pasting PR URL into `/resume` finds the source session | needs-mirror | med |
| FEAT-122-5 | 2.1.122 | feat | ToolSearch picks up MCP tools that connect after session start | needs-mirror | med |
| BUG-123-1 | 2.1.123 | bug | OAuth 401 retry-loop with DISABLE_EXPERIMENTAL_BETAS=1 | not-applicable | n/a |
| BUG-126-1 | 2.1.126 | bug | allowManagedDomainsOnly ignored when no sandbox block | needs-review | med |
| BUG-126-2 | 2.1.126 | bug | Images >2000px break sessions; auto-downscale on paste | needs-mirror | high |
| BUG-126-4 | 2.1.126 | bug | OAuth login timeouts on slow / IPv6-only / proxied | needs-mirror | med |
| BUG-126-5 | 2.1.126 | bug | Race clears valid OAuth refresh tokens | needs-mirror | high |
| BUG-126-6 | 2.1.126 | bug | API retry countdown stuck at "0s" | needs-mirror | low |
| BUG-126-8 | 2.1.126 | bug | Background/remote sessions falsely abort with stream-idle | needs-review | high |
| BUG-126-9 | 2.1.126 | bug | Assistant finishes thinking with no output after empty turns | needs-mirror | high |
| BUG-126-12 | 2.1.126 | bug | CJK text renders garbled on Windows | needs-mirror | high |
| BUG-126-13 | 2.1.126 | bug | Ctrl+L clears prompt input | needs-mirror | high |
| BUG-126-17 | 2.1.126 | bug | File-modified reminders unbounded when linters touch many files | needs-mirror | med |
| BUG-126-19 | 2.1.126 | bug | Windows clipboard exposes content in EDR/SIEM telemetry | needs-review | med |
| BUG-126-21 | 2.1.126 | bug | Agent SDK hangs on malformed parallel tool calls | needs-mirror | med |
| FEAT-126-1 | 2.1.126 | feat | `/model` lists from gateway `/v1/models` (opt-in by 129) | needs-mirror | med |
| FEAT-126-3 | 2.1.126 | feat | `--dangerously-skip-permissions` bypasses .git/.vscode/shell-config | needs-mirror | med |
| FEAT-126-5 | 2.1.126 | feat | OTel skill_activated fires for user-typed slashes + invocation_trigger | needs-review | low |
| FEAT-126-6 | 2.1.126 | feat | Auto-mode spinner turns red when permission stalls | needs-mirror | low |
| FEAT-126-7 | 2.1.126 | feat | PowerShell 7 detection (Store/MSI/.NET tool) | needs-mirror | med |
| BUG-128-1 | 2.1.128 | bug | Focus mode dims previous response on submit | needs-review | low |
| BUG-128-5 | 2.1.128 | bug | Crash loop piping >10 MB to `claude -p` via stdin | needs-mirror | high |
| BUG-128-6 | 2.1.128 | bug | Long URLs not individually clickable on wrapped rows | needs-mirror | low |
| BUG-128-9 | 2.1.128 | bug | Fenced code in lists retains leading whitespace on copy | needs-mirror | med |
| BUG-128-12 | 2.1.128 | bug | 1M-context sessions falsely blocked with "Prompt too long" | needs-mirror | high |
| BUG-128-19 | 2.1.128 | bug | `/rename` fails on resumed sessions with compact boundaries | needs-mirror | med |
| BUG-128-20 | 2.1.128 | bug | Stale status lines from prior sessions after `--resume` | needs-mirror | med |
| BUG-128-21 | 2.1.128 | bug | Stale plugin cache directory entries pollute PATH | needs-mirror | med |
| BUG-128-22 | 2.1.128 | bug | MCP stdio servers receive corrupted args with spaces | needs-mirror | high |
| BUG-128-23 | 2.1.128 | bug | Sub-agent progress summaries miss prompt-cache data | needs-mirror | med |
| BUG-128-25 | 2.1.128 | bug | Sub-agent summaries fire repeatedly on idle sub-agents | needs-mirror | med |
| FEAT-128-1 | 2.1.128 | feat | Bare `/color` picks random session color | needs-mirror | low |
| FEAT-128-2 | 2.1.128 | feat | `/mcp` shows tool count; flags zero-tool servers | needs-mirror | med |
| FEAT-128-6 | 2.1.128 | feat | Subprocesses don't inherit OTEL_* env vars | needs-mirror | med |
| FEAT-128-7 | 2.1.128 | feat | `workspace` is a reserved MCP server name | needs-review | low |
| FEAT-128-8 | 2.1.128 | feat | MCP reconnect summarizes re-announced tools by server prefix | needs-mirror | med |
| FEAT-128-10 | 2.1.128 | feat | `EnterWorktree` creates branches from local HEAD | needs-mirror | med |
| FEAT-128-11 | 2.1.128 | feat | Auto-mode classifier errors include retry/compact/debug hints | needs-mirror | low |
| FEAT-128-12 | 2.1.128 | feat | `--output-format stream-json` includes plugin_errors | needs-mirror | low |
| BUG-129-1 | 2.1.129 | bug | API errors with unrecognized 400 codes show raw JSON | needs-mirror | med |
| BUG-129-2 | 2.1.129 | bug | `/clear` doesn't reset terminal tab title | needs-mirror | low |
| BUG-129-4 | 2.1.129 | bug | Agent panel hidden when subagents run (regression) | needs-review | high |
| BUG-129-5 | 2.1.129 | bug | External-editor handoff blanks history above prompt | needs-mirror | med |
| BUG-129-6 | 2.1.129 | bug | `/context` dumps ASCII grid into conversation (1.6k tokens/call) | needs-mirror | high |
| BUG-129-10 | 2.1.129 | bug | Server-managed settings policy missing user:inference scope | needs-review | med |
| BUG-129-11 | 2.1.129 | bug | OAuth refresh race after wake logs out all sessions | needs-mirror | high |
| BUG-129-12 | 2.1.129 | bug | 1-hour prompt-cache TTL silently downgraded to 5min | needs-mirror | high |
| BUG-129-13 | 2.1.129 | bug | Spurious cache-miss warning after /clear or /effort/model change | needs-mirror | med |
| BUG-129-14 | 2.1.129 | bug | `Bash(mkdir *)`, `Bash(touch *)` allow rules ignored for in-project paths | needs-mirror | high |
| BUG-129-15 | 2.1.129 | bug | `deniedMcpServers` with `*://` doesn't match mixed-case hosts | needs-mirror | med |
| BUG-129-17 | 2.1.129 | bug | `/clear` doesn't clear conversation+transcript in VSCode | needs-review | med |
| FEAT-129-2 | 2.1.129 | feat | `CLAUDE_CODE_FORCE_SYNC_OUTPUT=1` for terminals like Emacs eat | needs-mirror | low |
| FEAT-129-3 | 2.1.129 | feat | Background pkg-mgr auto-update + restart prompt | needs-mirror | med |
| FEAT-129-4 | 2.1.129 | feat | Plugin manifest themes/monitors under `experimental:` | needs-mirror | low |
| FEAT-129-6 | 2.1.129 | feat | Ctrl+R history searches all projects; Ctrl+S narrows scope | needs-mirror | med |
| FEAT-129-7 | 2.1.129 | feat | `skillOverrides` accepts off / user-invocable-only / name-only | needs-mirror | med |
| FEAT-129-9 | 2.1.129 | feat | Policy-refusal errors include API Request ID | needs-mirror | low |
| BUG-131-1 | 2.1.131 | bug | VS Code extension fails to activate on Windows | not-applicable | n/a |
| BUG-131-2 | 2.1.131 | bug | Mantle endpoint missing x-api-key header | not-applicable | n/a |

### Already-have justifications

| Anvil ID | Maps to upstream | Why marked already-have |
|----------|------------------|-------------------------|
| BUG-1 | (pre-118) | Retry-After honored as min — fixed earlier than this audit window |
| BUG-2 | (pre-118) | Stream-stall timeout w/ non-streaming fallback — earlier window |
| BUG-3/4 | (pre-118) | DangerFullAccess + subagent inheritance — earlier window |
| BUG-7 | (pre-118) | Configurable request timeout — earlier window |
| BUG-15 | BUG-126-7 | Stream idle timeout post-sleep / long thinking — same root cause |
| BUG-17 | BUG-121-4 | Bash unusable when CWD deleted — same fix |
| BUG-21 | BUG-119-23 | Skills re-exec after auto-compact — same fix |
| BUG-26 | (pre-118) | cache_control on every turn — earlier window |
| BUG-28 | BUG-128-13 | Failing parallel Bash cancels siblings — same fix |
| BUG-31 | BUG-121-2 | /usage 2GB leak — same fix |
| BUG-34/35 | BUG-121-15, BUG-122-10 | settings.json partial-tolerance — same fix |
| FEAT-28 | FEAT-118-2 | /usage tabs (cost+stats) |
| FEAT-30 | FEAT-118-4 | Hooks invoke MCP via type=mcp_tool |
| FEAT-34 | FEAT-121-3 | Type-to-filter on /skill picker |
| FEAT-36 | FEAT-121-6 | Scrollable dialogs (PgUp/PgDn/wheel) |
| FEAT-39 | FEAT-126-2 | `anvil project purge` |
| FEAT-40 | FEAT-126-4 | OAuth pasted code when localhost unreachable |
| FEAT-41 | FEAT-121-1 | alwaysLoad on MCP server config |
| FEAT-42 | FEAT-128-3, FEAT-129-1 | --plugin-dir zip + --plugin-url |

### not-applicable justifications

| ID | Why not applicable |
|----|--------------------|
| BUG-123-1 | Anvil has no `DISABLE_EXPERIMENTAL_BETAS` flag; OAuth path is OpenRouter-style not Anthropic-OAuth |
| BUG-131-1 | Anvil has no VS Code extension yet |
| BUG-131-2 | Mantle is an Anthropic-internal gateway endpoint Anvil doesn't target |

### Top-priority items (high) for v2.3 backlog triage

These are the items the user should look at first — high-impact correctness or stability bugs that any Anvil user could realistically hit, plus a few high-value workflow features:

- **Caching regression** — BUG-129-12 (1h TTL silently downgraded to 5min) collides directly with Anvil's BUG-26 cache_control work; verify Anvil's beta header negotiation.
- **Memory leaks** — BUG-121-1 (image RSS), BUG-121-3 (tool-progress leak).
- **Paste / input correctness** — BUG-119-1 (CRLF), BUG-119-2 (kitty multiline), BUG-119-10 (Tab-completion path replace), BUG-126-13 (Ctrl+L clears prompt), BUG-126-2 (>2000px image breaks session).
- **Stream / runtime stability** — BUG-126-9 (empty-output finish), BUG-128-5 (>10MB stdin crash), BUG-128-12 (1M-context falsely "prompt too long").
- **Tool surface bugs** — BUG-119-3 (Glob/Grep disappear when Bash denied), BUG-128-22 (MCP stdio space-arg corruption), BUG-129-14 (Bash allow-rule regression for in-project paths), BUG-119-29 (read_file size cap not enforced).
- **OAuth stability** — BUG-126-5 (race clears refresh tokens), BUG-129-11 (wake-from-sleep logs out all sessions).
- **CJK + i18n** — BUG-126-12 (Windows CJK garble).
- **Workflow features** — FEAT-119-1 (settings persistence with precedence), FEAT-120-1 (`anvil ultrareview` for CI), FEAT-122-2 (PR-URL → session lookup).
- **Subagent regressions** — BUG-129-4 (agent panel hidden during subagent run — already handled in this session for the panel visibility fix, double-check).
- **Context waste** — BUG-129-6 (`/context` dumps ASCII grid into conversation, 1.6k tokens/call).
