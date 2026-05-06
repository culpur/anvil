# Anvil v2.2.9 — Claude Code parity catch-up

Released: 2026-05-06

v2.2.9 closes the parity gap with Claude Code v2.1.118 → v2.1.131 (14 days,
9 published releases). The release ships **four bug fixes** Anvil shared
with Claude Code (one of which — missing `cache_control` — was a bigger
issue for Anvil than for CC) plus **seven feature parity items**, all
landed in parallel by a five-stream agent build. No capability removed,
additive only.

Full audit doc: `CLAUDE-CODE-PARITY-v2.1.118-to-v2.1.131.md`.

---

## Bug fixes (parity)

### BUG-15 — Anthropic streaming had no dead-air timer
The OpenAI-compatible provider got a 5-minute dead-air timer in v2.2.8
(CC-BUG-2). The Anthropic provider was overlooked; a stalled upstream could
hang the session indefinitely. v2.2.9 mirrors the timer onto
`crates/api/src/providers/anvil_provider.rs::MessageStream`. Uses
`Instant::now()` (monotonic, wake-from-sleep safe) and resets on every
chunk including `thinking_delta`, so long native-thinking pauses don't
false-trip.

### BUG-17 — Bash tool was unusable when its CWD got deleted
A user running `rm -rf $PWD` (or having a shell deletion happen mid-session)
broke every subsequent Bash call until the Anvil session restarted. The fix
adds a `resolve_cwd_with_fallback()` chain: `env::current_dir()` →
`$HOME` → `/tmp`. On fallback, also calls `set_current_dir()` so other
Anvil internals recover, and emits a one-shot stderr warning.

### BUG-26 — Anvil sent zero `cache_control`, re-billing every turn at full rate
Bigger than the Claude Code bug it was inspired by. The Anthropic API
supports prompt caching with `cache_control: {type:"ephemeral", ttl:"1h"}`
markers on the system prompt and tool definitions. Anvil **never sent
this**, meaning every single turn re-billed the full system + tools at
full input rate. v2.2.9 injects two breakpoints on every Anthropic
request — one on the system prompt, one on the last tool definition. For
long sessions on Anthropic, the cost reduction is substantial.

(OpenAI-compat and Ollama paths intentionally untouched — they'd error on
`cache_control` keys.)

### BUG-34/35 — `settings.json` was all-or-nothing
A stray comma anywhere in `~/.anvil/settings.json`, or one malformed hook
entry, or a wrong-shape `oauth` block, would invalidate the entire file
and effectively reset every setting. v2.2.9 makes the parser tolerant per
section: bad sections warn-and-skip while good sections still apply.
Specifically:
- `read_optional_json_object` now returns `Ok(None)` on JSON syntax error
  (previously: `Err`).
- `parse_hook_spec_array` keeps valid entries when one is malformed.
- The top-level `load()` runs each section parser through a
  `tolerate_section()` helper that warns to stderr and falls back to
  `Default` on `Err`.
- `merge_mcp_servers` is now infallible — a bad entry doesn't drop sibling
  valid entries.

---

## Feature parity

### `/usage` and `/stats` aliases for `/cost`
CC v2.1.118 merged `/cost` and `/stats` into `/usage`. Anvil mirrors the
canonical name — all three now resolve to the same handler, like
`/plugin / /plugins / /marketplace`.

### Hooks can invoke MCP tools directly
CC v2.1.118 added `type: "mcp_tool"` as a hook action. Anvil's
`RuntimeHookSpec::McpTool { server, tool, input }` does the same — useful
for vault-scrubber-as-hook, post-tool redaction, etc. All MCP failure
modes (no invoker, server unavailable, transport error) are warnings,
never denies; an unhealthy MCP server can't crash a turn.

### `filter_skills()` for skill picker
CC v2.1.121 added a type-to-filter search box to its `/skills` picker.
Anvil's `commands::filter_skills(query, skills)` ships the case-insensitive
substring filter against skill name OR trigger keyword. The interactive
picker UI (where the filter input would live) doesn't exist yet — the
filter logic is ready for when it lands.

### Scrollable TUI surfaces
CC v2.1.121 made dialogs scrollable. Anvil's overflowing surfaces — the
`/configure` overlay screens (MainMenu 17, WidgetPicker 36, PresetPicker
16, Notifications 10, Search) and the slash-command completion popup
(21+ entries capped at 12 visible rows) — now route PgUp/PgDn/Home/End
and mouse-wheel through a new `tui::ListViewport` primitive. The brief's
listed "dialogs" (`/vault list`, `/agents`, etc.) are actually chat-log
text emitting via `LogEntry::System`, already scrollable since v2.2.7.

### `anvil project purge`
CC v2.1.126 added `claude project purge`. Anvil's equivalent removes
per-workspace state keyed by sha256 of the canonical workspace path:
`~/.anvil/sessions/<hash>/`, `daily/<hash>/`,
`private/<hash>.enc`, `file-history/<hash>/`. The vault, settings, theme,
keybindings, and OAuth credentials are **never touched**. Flags:
`[path] [--dry-run|-n] [--yes|-y] [--interactive|-i] [--all]`.

### OAuth paste-code fallback for SSH/WSL/container users
CC v2.1.126 added it; we mirror it. The localhost listener now races a
stdin paste prompt — whichever completes first wins. If `TcpListener::bind`
fails (port in use, sandbox, etc.) we go paste-only with
`https://platform.claude.com/oauth/code/callback` as the manual redirect.
Accepts: bare codes, `code#state=…` / `code?state=…`, full callback URLs.
State validation prevents replay across sessions; mismatched state hard-errors.

### `alwaysLoad: true` on MCP servers
CC v2.1.121 added it. Anvil now propagates `alwaysLoad` through every MCP
server variant (stdio, SSE, HTTP, WebSocket, SDK, managed-proxy). When
true, that server's tools skip ToolSearch deferral and are immediately
available to the model. Frequently-used servers (e.g. `culpur-infra`,
`qmd`) no longer cost an extra discovery round-trip per session.

### `--plugin-dir <zip>` and `--plugin-url <https-url>`
CC v2.1.128/v2.1.129. Anvil's plugin loader now accepts:
- A directory path (existing behavior).
- A `.zip` archive — extracted to a session-scoped temp dir, cleaned on
  exit.
- An `https://` URL fetched via reqwest, validated as a real zip via
  magic-byte check (`PK\x03\x04`), extracted with path-traversal-safe
  rules.

Security guardrails: HTTPS-only (no `http://`), reqwest `https_only(true)`
so even a redirect-to-http is blocked; zip entries with `..`, absolute
paths, or non-UTF-8 names are rejected; 50 MiB cap on both download body
and cumulative extracted bytes (zip-bomb defense); optional
`--plugin-sha256 HEX` enables strict integrity verification (TOFU when
omitted).

---

## Audited and cleared (no fix needed)

Anvil is structurally immune to several CC regressions:

| CC bug | Why Anvil isn't affected |
|--------|--------------------------|
| BUG-19 (`/branch` orphans tool_use) | `/fork` is UI-only; no `/rewind` exists. |
| BUG-20 (`mkdir *` allow-rule) | Anvil has no `Bash(cmd *)` rule format. |
| BUG-21 (skill re-fire post-compact) | Skills are suggest-never-auto. |
| BUG-28 (parallel sibling cancel) | Tool dispatch is sequential. (Filed `#280` to add parallelism — must use `JoinSet::join_all`, NOT `try_join_all`.) |
| BUG-31 (`/cost` 2GB leak) | `/cost` reads in-memory `UsageTracker`, doesn't crawl `~/.anvil/sessions/`. |

---

## Test coverage

| Crate | Tests | New in v2.2.9 |
|-------|-------|---------------|
| api | 93 + 13 integration | 5 (cache_control wire + Anthropic stall) |
| runtime | 329 | 27 (5 mcp_tool hook + 6 settings tolerance + 9 alwaysLoad MCP + 5 OAuth paste-code + 2 manager) |
| anvil-cli | 212 + 5 integration | 32 (19 ListViewport + 9 project purge + 4 plugin-zip CLI) |
| plugins | 55 | 11 (zip extraction, traversal rejection, URL gating) |
| commands | 70 | 6 (filter_skills) |

Total v2.2.9 net-new tests: **81**. `cargo check --workspace` clean. All workspace test suites green.

---

## Co-development credit

Built by Maverick + five parallel agent streams (A: api, B: config,
C: tui, D: skills/commands, E: hooks/runtime). Stream coordination via
explicit file-boundary contracts in the dispatch prompts; merge resolved
without conflicts on a single branch.
