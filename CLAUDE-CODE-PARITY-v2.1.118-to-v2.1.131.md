# Claude Code parity catch-up — v2.1.118 → v2.1.131

Released: 2026-04-23 through 2026-05-06 (14 days, 9 published releases).
Baseline: prior parity report covers v2.1.94 → v2.1.117. Anvil current: v2.2.8.

Classification: **implement** (mirror in Anvil), **defer** (note blocker), or
**decline** (document the deviation). Bug fixes get priority because the
same class likely exists in Anvil. No reduction of capability — additive only.

---

## Bugs Anvil very likely shares — file as tasks now

### BUG-15 · Stream idle timeout false-fires after wake-from-sleep + during long thinking pauses (v2.1.126)
Two related fixes. Anvil added a stream-stall timeout in v2.2.8 (CC-BUG-2 / task #82). Verify our timer:
- Resets after wake-from-sleep (or detects monotonic-clock jumps and re-arms).
- Doesn't trip during long native thinking pauses on Sonnet 4.6 / Opus 4.7.
File: `crates/api/src/providers/openai_compat.rs`, the stall timer logic.

### BUG-16 · API retry countdown sticks at "0s" instead of counting down (v2.1.126)
Anvil shows a retry-in-Ns banner in TUI. Check our render path doesn't lock at 0 between attempts. Likely cosmetic but confusing.

### BUG-17 · Bash tool permanently unusable when starting CWD is deleted/moved mid-session (v2.1.121)
Anvil's bash tool reads CWD at every tool call. Add a fallback to `$HOME` or the workspace root if CWD vanishes. Currently we'd just error every Bash call.
File: `crates/tools/src/bash.rs` (or equivalent).

### BUG-18 · Sub-agent transcript triggers repeated summary calls while idle, no cache (v2.1.128)
Anvil's `/agent compose` and Agent tool subagents — verify we don't re-summarize a static transcript on every tick, and that prompt cache is honored on the summarize call.

### BUG-19 · `/branch` (rewind branching) produces forks with orphaned tool_use IDs (v2.1.122)
Anvil has session forking via `/fork` (planned) and rewind via `/rewind`. If we ever produce a fork with `tool_use` blocks that lack matching `tool_result`, the next API call 400s. Add validator on fork.

### BUG-20 · Bash tool `mkdir`/`touch` allow-rules not honored on in-project paths (v2.1.129)
Anvil's bash permission matcher needs an audit — ensure `Bash(mkdir *)`-style rules match relative paths inside the workspace, not just absolute.

### BUG-21 · Skills invoked before auto-compaction re-execute against next user message (v2.1.119)
Anvil's `/skill` system — if a skill ran in turn N and auto-compact fires before turn N+1, the skill should not re-trigger on the next user message. Audit the skill state machine.

### BUG-22 · Async `PostToolUse` hooks emitting no payload write empty entries to transcript (v2.1.119)
Anvil's hooks (`crates/runtime/src/hooks.rs`) — verify a hook that returns no JSON doesn't pollute the transcript with `{}` entries.

### BUG-23 · `--print`/headless mode doesn't honor agent `tools:`/`disallowedTools:` frontmatter (v2.1.119)
Anvil has `--print` (single-shot) mode. Check that agent compositions still apply tool filters in non-interactive runs.

### BUG-24 · Memory leak when long-running tools fail to emit clear progress event (v2.1.121)
Tool progress events accumulate in some unbounded buffer. Audit the tool-progress channel for drop-on-overflow.

### BUG-25 · `--continue`/`--resume` finds sessions only via original cwd (v2.1.118)
Anvil session resume — if we add files via `/add-dir`-equivalent, the resume index should track all directories that participated, not just the original. (We don't yet have `/add-dir`; check whether resume is brittle to cwd changes.)

### BUG-26 · 1-hour prompt cache TTL silently downgraded to 5 minutes (v2.1.129)
Anvil sends `cache_control: {type: "ephemeral", ttl: "1h"}` somewhere — verify the request envelope and that no shim is rewriting it. Check `crates/api/src/providers/anthropic.rs`.

### BUG-27 · Session-title generation 400s with `output_config: Extra inputs` on Vertex/Bedrock (v2.1.122)
Anvil has session naming. We don't currently target Vertex/Bedrock, but if a user points `ANTHROPIC_BASE_URL` at one, our title gen might 400. Add a fallback that drops the structured-output schema if the provider rejects it.

### BUG-28 · Failing read-only Bash sibling cancels parallel tool calls (v2.1.128)
Anvil parallel tool execution — verify a single failing tool doesn't cancel siblings.

### BUG-29 · Embedded grep/find/rg fail when binary deleted mid-session (v2.1.121)
Anvil ships its own search via the binary, falls back to system tools. Verify we have the same fallback. (Anvil is one binary; if it's deleted, the running process keeps fd-pinned access — but verify.)

### BUG-30 · MCP servers receive corrupted args when shell-prefix wraps spaces (v2.1.128)
Anvil's MCP stdio launcher — if a user has a wrapper command (proxychains, sudo, tsx-node), spaces in args could be mangled. Audit `crates/runtime/src/mcp_stdio.rs`.

### BUG-31 · `/usage` (`/cost`) leaks ~2GB on machines with large transcript histories (v2.1.121)
Anvil's `/cost` and history walker — make sure we stream rather than slurp `~/.anvil/sessions/`.

### BUG-32 · Crash loop when piping >10MB stdin to `claude -p` (v2.1.128)
Anvil headless-mode stdin handler — bound the input read or stream it to disk before processing.

### BUG-33 · Plan-mode tools unavailable in `--channels`-launched sessions (v2.1.126)
We don't have channels yet. Note for later.

### BUG-34 · Settings file `legacy enum value` invalidates whole file (v2.1.121)
Anvil's `~/.anvil/settings.json` parsing — if a single field is invalid, we should warn-and-skip, not nuke the file. Audit `crates/runtime/src/settings.rs` (or wherever).

### BUG-35 · Malformed hooks entry invalidates whole settings.json (v2.1.122)
Same as BUG-34 — partial-tolerance on hook config.

---

## Features worth mirroring

### FEAT-27 · Vim visual mode (`v` / `V`) in TUI (v2.1.118)
Anvil has vim mode. If we don't have visual selection, add it.

### FEAT-28 · `/usage` consolidating `/cost`+`/stats` (v2.1.118)
Anvil has `/cost`. Adding `/usage` as a parent with cost + usage tabs is a UX win — both names alias. Easy.

### FEAT-29 · Named custom themes via `~/.claude/themes/*.json` (v2.1.118)
Anvil already has themes. Add JSON file-based custom themes in `~/.anvil/themes/`. Plugins can ship themes — extend our plugin manifest.

### FEAT-30 · Hooks invoke MCP tools directly (`type: "mcp_tool"`) (v2.1.118)
Anvil hooks fire shell commands today. Add an `mcp_tool` action type that calls a tool by name with JSON args. Powerful for hook authors.

### FEAT-31 · `DISABLE_UPDATES` env var (v2.1.118)
Anvil already has `anvil upgrade`; add `ANVIL_DISABLE_UPDATES=1` to block all update paths (manual + auto).

### FEAT-32 · `claude plugin tag` — git tag + version validation for plugin authors (v2.1.118)
Anvil plugin authors push to AnvilHub. Adding `anvil plugin tag` to validate `plugin.toml` version + create a git tag would smooth the publish flow.

### FEAT-33 · `claude plugin prune` — remove orphaned dep plugins (v2.1.121)
Once Anvil supports plugin dependencies (FEAT-11 from prior report), add prune.

### FEAT-34 · Type-to-filter on `/skills` picker (v2.1.121)
Anvil's `/skill suggest` — add a substring filter input.

### FEAT-35 · `PostToolUse` hooks can rewrite tool output via `hookSpecificOutput.updatedToolOutput` (v2.1.121)
Anvil's hook contract — extend post-tool to allow output mutation, not just side effects. Useful for redaction (vault scrubber as a hook).

### FEAT-36 · Scrollable dialogs (arrow keys, PgUp/PgDn) (v2.1.121)
TUI polish. Anvil dialogs (vault list, agent picker, etc.) should be scrollable when they overflow.

### FEAT-37 · `--dangerously-skip-permissions` no longer prompts for writes to `.claude/skills,agents,commands/` (v2.1.121)
Anvil's DangerFullAccess mode — verify it doesn't gate writes to `.anvil/skills/`, `.anvil/agents/`, `.anvil/commands/`.

### FEAT-38 · MCP servers auto-retry up to 3× on transient startup errors (v2.1.121)
Anvil MCP launcher — add retry-with-backoff on initial connect failures.

### FEAT-39 · `claude project purge [path]` (v2.1.126)
Anvil should have `anvil project purge` to wipe `~/.anvil/sessions/<workspace>`, `daily/`, file history. Useful when archiving a project. Support `--dry-run`, `--all`.

### FEAT-40 · OAuth `/auth login` accepts pasted code when localhost callback unreachable (v2.1.126)
Anvil OAuth (Anthropic) — for SSH/WSL/container users where the browser can't reach `localhost:PORT`, add the paste-code flow.

### FEAT-41 · `alwaysLoad: true` on MCP servers — skip tool-search deferral (v2.1.121)
Anvil ships tool-search. Add a `always_load` knob on MCP server config to mark a server's tools as always available. Fixes the "MCP tool I always use takes 2 calls to discover" UX issue.

### FEAT-42 · `--plugin-dir` accepts `.zip` archives (v2.1.128) and `--plugin-url <url>` (v2.1.129)
Anvil plugin loader — support zip archives and remote URLs as session-scoped plugins. Useful for "test this plugin without installing" workflow.

### FEAT-43 · Parallel MCP server reconnect (v2.1.119)
Anvil reconfigures MCP servers serially today. Parallelize the reconnect for faster `/reload`.

### FEAT-44 · `/recap` was already filed. Pair with `claude_code.skill_activated` OTel event with `invocation_trigger` attribute (v2.1.126)
Decline OTel for now; revisit if/when we ship telemetry.

### FEAT-45 · `prUrlTemplate` setting for footer PR badge (v2.1.119)
Anvil shows PR info in the status line. If we let users set a template, GitLab/Gitea/self-hosted users can route the badge correctly.

### FEAT-46 · Persistent `/config` settings — theme, editor mode, verbose (v2.1.119)
Anvil settings already persist. Confirm parity: theme + editor mode + verbose + `/effort` selection all survive restart.

### FEAT-47 · `claude plugin validate` (implied by v2.1.129 manifest schema warning)
Already partially have via task #94 — extend to validate `experimental:` block placement.

### FEAT-48 · Ctrl+R history picker — search all prompts across all projects (v2.1.129)
Anvil has history. Ctrl+R behavior — verify we search across workspaces by default; Ctrl+S narrows to current.

### FEAT-49 · `skillOverrides` setting: `off` / `user-invocable-only` / `name-only` (v2.1.129)
Anvil's skill suggester — add a per-skill or per-skill-pattern override knob. Useful when a noisy skill keeps suggesting itself.

---

## Anvil-specific deviations to keep

### DECLINE-5 · Bedrock service tier env var (v2.1.122)
Decline. We don't target Bedrock. If we did, mirror the env var.

### DECLINE-6 · Voice mode and dictation features (v2.1.121, v2.1.122)
Decline — voice mode is VS Code-extension specific; Anvil is terminal-first.

### DECLINE-7 · VS Code extension features (v2.1.121, v2.1.128, v2.1.131)
Decline — Anvil is the CLI; we don't ship a VS Code extension. (User can use anything.)

### DECLINE-8 · WSL-specific Windows settings inheritance (`wslInheritsWindowsSettings`) (v2.1.118)
Decline — niche, can revisit if a Windows-WSL user requests it.

### DECLINE-9 · `--from-pr` URL parsing for GitLab/Bitbucket (v2.1.119)
Decline for now — requires us to ship a `from-pr` flag first. File for v2.4 if there's demand.

### DECLINE-10 · Mantle endpoint `x-api-key` fix (v2.1.131)
Decline — we don't target Mantle.

### DECLINE-11 · Vertex tool-search opt-in flip (v2.1.122)
Decline — we don't target Vertex.

### DECLINE-12 · iTerm2 `/terminal-setup` clipboard wiring (v2.1.121)
Decline — Anvil leaves terminal config to the user.

---

## Summary

| Bucket | Count |
|--------|-------|
| BUGS to investigate (BUG-15..35) | 21 |
| FEATS to mirror (FEAT-27..49) | 23 |
| DECLINEs (out-of-scope) | 8 |

**Highest-priority bugs** (likely shared with Anvil, ship in v2.2.9 or v2.3):
BUG-15 (idle timeout post-sleep), BUG-17 (bash CWD vanish), BUG-21 (skill re-fire after compact), BUG-26 (1h cache TTL downgrade), BUG-28 (parallel sibling cancel), BUG-31 (`/cost` 2GB leak), BUG-34/35 (settings file partial-tolerance).

**Highest-value features** (cheap UX wins):
FEAT-28 (`/usage` consolidator), FEAT-30 (MCP-tool hooks), FEAT-34 (skill filter), FEAT-36 (scrollable dialogs), FEAT-39 (`anvil project purge`), FEAT-40 (paste-code OAuth), FEAT-41 (`always_load` MCP).

These get filed individually as tasks below.
