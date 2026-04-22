# Claude Code parity catch-up — v2.1.94 → v2.1.117

Released: 2026-04-07 through 2026-04-22 (16 days).
Baseline: last parity check was at Claude Code v2.1.94.

Classification per user directive (2026-04-22): for each shipped item, pick
one of **implement** (mirror in Anvil), **defer** (note blocker), or
**decline** (document the deviation). Bug fixes get priority because the
same class likely exists in Anvil.

---

## Bugs Anvil very likely shares — highest priority

### BUG-1 · 429 retries burn all attempts in ~13s on small Retry-After (v2.1.94, v2.1.98)
Claude Code fixed exponential backoff so small `Retry-After` values don't burn the whole retry budget in 13s. Anvil's provider retry loop is in `crates/api/src/providers/openai_compat.rs` — check whether we honor `Retry-After` floor as minimum, not authoritative.

### BUG-2 · Stalled streaming hangs indefinitely (v2.1.98, v2.1.105, v2.1.110)
Three separate fixes for streams that stall mid-response. Anvil has its own stall — we saw it in the Ollama empty-stream case (already fixed in v2.2.7+1 as `bd6599d`), but we do NOT have a stream-stall timeout. Add a 5-minute dead-air timeout with non-streaming fallback.

### BUG-3 · `--dangerously-skip-permissions` silently downgraded to accept-edits after approving a write to protected path (v2.1.97, v2.1.98)
If we ever regress our sandbox-mode atomic such that approving a write changes the mode, we'd have this bug too. **Add a regression test** that takes a DangerFullAccess mode through an approval cycle and asserts the mode never changes.

### BUG-4 · Subagents not inheriting leader's permission mode under `--dangerously-skip-permissions` (v2.1.98)
Anvil has a subagent system (agents crate). Check whether spawned agents pick up the caller's `PermissionMode`. If not, that's a silent security regression.

### BUG-5 · `permissions.additionalDirectories` not applying mid-session (v2.1.97, v2.1.98)
Anvil doesn't have `additionalDirectories` per se, but we should confirm our on-the-fly `/permissions` mode switch actually takes effect on the next tool call (current v2.2.8 WIP fix — #77 test coverage already addresses this).

### BUG-6 · Bash compound commands bypass forced permission prompts in auto/bypass modes (v2.1.98)
Relevant if Anvil gains an auto-allow list. Parkinglot this until we have one.

### BUG-7 · Hardcoded 5-min request timeout aborted slow backends (local LLMs, extended thinking) regardless of API_TIMEOUT_MS (v2.1.101)
Anvil very likely has the same bug. Ollama requests can take 2+ minutes on large prompts. Check `crates/api/src/providers/openai_compat.rs` for request timeout and make it env-configurable.

### BUG-8 · SSE/HTTP MCP connections accumulate ~50 MB/hr (v2.1.97)
Our MCP transports are in `crates/runtime/src/mcp_stdio/`, `mcp_client`, etc. Check for unbounded buffers on reconnect.

### BUG-9 · Pasted images dropped when attached to queued messages (v2.1.105)
Anvil doesn't yet handle images in the same way; check `crates/api/src/image_paste.rs` and `/paste` command for similar edge cases.

### BUG-10 · `DISABLE_COMPACT` + `CLAUDE_CODE_MAX_CONTEXT_TOKENS` interaction (v2.1.98)
Anvil has `/compact` and `auto_compact`. Check that our equivalent env kills actually respect each other.

### BUG-11 · macOS `/private/{etc,var,tmp,home}` under `rm:*` treated as dangerous (v2.1.113)
Anvil's sandbox right now allows `/tmp` unconditionally. That means `rm -rf /private/tmp/...` is auto-allowed. Not exploitable directly but worth a guard rail.

### BUG-12 · Bash deny rules not matching `env`/`sudo`/`watch`/`ionice`/`setsid`-wrapped commands (v2.1.113)
Anvil bash permission matching is in `crates/tools/` — check whether we strip exec wrappers before matching against deny rules.

### BUG-13 · `find -exec` / `find -delete` auto-approved under `Bash(find:*)` (v2.1.113)
Anti-regression: any blanket `find` allow must not cover these two flags.

### BUG-14 · Non-streaming fallback retries cause multi-minute hangs (v2.1.110) — then **reverted** in v2.1.111
Watch for the same cap: aggressive retry caps trade hangs for outright failures. Don't mirror the v2.1.110 cap without the v2.1.111 revert logic.

---

## Features worth mirroring

### FEAT-1 · `/tui fullscreen` — switch to flicker-free rendering mid-session (v2.1.110)
Anvil already has a fullscreen vs. inline TUI toggle, but not a runtime switch. Add `/tui <mode>` equivalent.

### FEAT-2 · `refreshInterval` on status line (v2.1.97)
Anvil has a status line but it's static-render. Add a `refreshInterval` knob.

### FEAT-3 · `workspace.git_worktree` exposed to status line JSON (v2.1.97, v2.1.98)
Our status line widgets should know when we're in a linked worktree. Our `worktree` subcommand exists; surface it.

### FEAT-4 · `/recap` (session recap when returning) (v2.1.108)
Anvil has `/resume` and daily summaries. Adding a `/recap` that gives "what happened last session" on start would match. Opt-out env var like `CLAUDE_CODE_ENABLE_AWAY_SUMMARY`.

### FEAT-5 · Focus view (`Ctrl+O`) — shows prompt, one-line tool summaries with edit diffstats, final response only (v2.1.97, v2.1.110)
Massive UX win for long sessions. Anvil already has scrollback and transcript — layering a "focus" filter would be cheap.

### FEAT-6 · `/setup-bedrock`, `/setup-vertex` interactive wizards (v2.1.98, v2.1.101)
Anvil has `anvil --setup` covering Anthropic, OpenAI, Ollama, etc. Wizards for Bedrock/Vertex would add enterprise gravitas. Defer until there's user demand.

### FEAT-7 · `--exclude-dynamic-system-prompt-sections` for cross-user prompt cache hits in print mode (v2.1.98)
Only relevant if we add shared-cache infra. Defer.

### FEAT-8 · Monitor tool for streaming events from background scripts (v2.1.98)
Already partially in Anvil (we have `/loop`). Cross-check whether our background-task model exposes an event stream API.

### FEAT-9 · `PreCompact` hook (v2.1.105)
Anvil's hooks are in `crates/runtime/src/hooks.rs`. Add a PreCompact event that can block compaction via exit code 2.

### FEAT-10 · Skill frontmatter `disable-model-invocation` (v2.1.110)
Anvil's skill system in the AnvilHub model — add a frontmatter knob for "don't let the model auto-invoke this."

### FEAT-11 · Plugin dependency installation (v2.1.110, v2.1.116, v2.1.117)
Anvil plugin installs don't resolve transitive deps. Several bugs in Claude Code around this — mirror the `plugin install` behavior that auto-resolves dependencies from already-configured marketplaces.

### FEAT-12 · `sandbox.network.deniedDomains` — deny list that overrides broader allow wildcards (v2.1.113)
Anvil has `/security egress` with allowlist. Add a deny list that takes precedence. Security win.

### FEAT-13 · Default effort `high` on Pro/Max for Opus 4.6 and Sonnet 4.6 (v2.1.117)
Anvil uses provider-default effort. Consider defaulting Opus/Sonnet to `high` for parity if the user has an active Anthropic Pro/Max subscription. Needs UX call.

### FEAT-14 · `/model` warns before mid-conversation switch (re-reads full history uncached) (v2.1.108)
Anvil has `/model` — add the warning. Easy.

### FEAT-15 · Auto theme (matches terminal dark/light) (v2.1.111)
Anvil has a full theme system. Add auto-detect.

### FEAT-16 · `/less-permission-prompts` — scans transcripts, proposes allowlist (v2.1.111)
Anvil has a permission-memory system (`crates/runtime/src/permission_memory.rs`). This would be a natural extension — scan memory, propose rules for `settings.json`.

### FEAT-17 · `Ctrl+U` clears input buffer; `Ctrl+Y` restores (v2.1.111)
Pure UX polish. Easy.

### FEAT-18 · `Ctrl+A`/`Ctrl+E` readline line-start/end in multiline input (v2.1.113)
Anvil's input handler may already do this; verify.

### FEAT-19 · `OTEL_LOG_RAW_API_BODIES` for OpenTelemetry debugging (v2.1.111)
We don't ship OTEL yet — decline unless OTEL ships.

### FEAT-20 · Push-notification tool in Remote Control (v2.1.110)
We have a RemoteTrigger MCP already. Add a push-notify tool pattern.

### FEAT-21 · `cleanupPeriodDays` retention for `~/.claude/tasks/`, `shell-snapshots/`, `backups/` (v2.1.117)
Anvil has `~/.anvil/` with sessions, daily, history. Add a cleanup policy.

### FEAT-22 · `/doctor` with auto-fix (v2.1.105, v2.1.116)
Anvil has `anvil --check` — extend with a `--fix` flag.

### FEAT-23 · Native binary via per-platform optional dependency (v2.1.113)
Anvil already is native binary via Rust. Already past parity here.

### FEAT-24 · Slack MCP compact `Slacked #channel` header (v2.1.94)
Anvil doesn't have Slack MCP. Decline.

### FEAT-25 · Skill listing cap raised 250→1536 chars, warn on truncation (v2.1.105)
Anvil's skill manifest handling — check our cap, match it.

### FEAT-26 · `WebFetch` strips `<style>` and `<script>` (v2.1.105, v2.1.117)
Anvil doesn't have WebFetch; if/when we add it, strip style/script.

---

## Anvil-specific deviations to keep

### DECLINE-1 · Mantle / Bedrock / Vertex setup wizards (v2.1.94, v2.1.98, v2.1.101)
Not in Anvil's freedom-first positioning. Culpur infra doesn't need cloud-proxied Anthropic.

### DECLINE-2 · Amazon Bedrock support (v2.1.94)
Decline — runs against the local/self-hosted positioning. Users can add via OpenAI-compatible proxy if needed.

### DECLINE-3 · `/ultraplan`, `/ultrareview`, "Refine with Ultraplan" (v2.1.101, v2.1.111, v2.1.113)
Cloud-backed features. Decline — not part of Anvil's model.

### DECLINE-4 · Forked subagents toggle (v2.1.117)
Anvil has its own subagent system. Decline — keep our model.

---

## Next actions

Bugs 1, 2, 3, 4, 7, 11, 12, 13 get filed as individual tasks (critical, ship in v2.2.8 if they're real).

Features 1, 9, 14, 15, 17 get filed as v2.2.8 scope.
Features 11, 16, 21, 22 get filed as v2.2.9 scope.
All other features get filed with owner=null and status=pending for v2.3+ planning.

Each item gets a cross-check: `grep` Anvil for the equivalent code path, confirm presence/absence, file the task with specific line numbers.
