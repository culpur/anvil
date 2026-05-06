# Anvil v2.2.10 — TUI usability patch

Released: 2026-05-06

Patch release fixing three usability bugs that all showed up in the same
real v2.2.9 session on macOS Terminal.app. No new features, no API changes,
no migration required. Update via `anvil upgrade` or
`brew reinstall anvil`.

---

## Fixes

### Long lines wrap instead of right-truncating with `…`

The chat content paragraph used to manually char-cap each line at the
terminal width and append `…`. Effect: every long prompt and assistant
response lost its tail at column N — users couldn't see what they
themselves had typed, let alone the model's response.

v2.2.10 uses ratatui's `Paragraph::wrap { trim: false }`. Lines now
soft-wrap at the right edge so the full message is visible, indentation
in code/tool-output is preserved.

### Native drag-to-select works in every terminal

Mouse capture was on by default with a `Shift+Drag` pass-through
workaround for selection. The pass-through worked on iTerm2, Windows
Terminal, and most Linux VTEs — but **not on macOS Terminal.app**, where
the user couldn't select chat text at all.

v2.2.10 disables mouse capture by default. Drag-to-select with no
modifier now works in every terminal. The startup hint reads:

> Drag to select text  •  Set ANVIL_TUI_MOUSE=1 to enable mouse-wheel scroll

If you want chat / configure-overlay wheel scroll back, opt in via
`ANVIL_TUI_MOUSE=1`.

### Tool-result lines actually tell you what happened

The post-call line was `{name} [{status}]: {first_line_of_summary}`
where `summary` was the raw JSON output. For tools returning
`{"stdout": "...", ...}` (bash) or `{"id": "...", "name": "..."}` (most
team / MCP tools), the JSON's first line is just the opening brace.
The line read `bash [ok]: {`, `TeamCreate [ok]: {`,
`TeamAddMember [ok]: {` — telling the user nothing.

v2.2.10 adds `tool_result_summary()` in `format_tool.rs` that parses
each tool's JSON output per-tool:

| Tool | New display |
|------|-------------|
| `bash` | first non-empty stdout line + `(+N more lines)` indicator |
| `read_file` | `N lines` |
| `write_file` | `wrote <path>` |
| `edit_file` | `edited <path>` |
| `glob_search` / `grep_search` | `N matches` |
| generic / MCP | string body, then known keys (`message`, `summary`, `name`, `id`), then `keys: a, b, c` listing as a last resort |

You'll see `bash [ok]: ls -la (+12 more lines)` instead of `bash [ok]: {`.

---

## Test coverage

5 new tests cover `tool_result_summary()`: bash multi-line extraction,
generic-key fallback, unknown-shape key listing, empty-bash-output
sentinel, error-passthrough. 217 anvil-cli tests pass; 21 workspace test
result lines green.

---

## Upgrade

```bash
anvil upgrade
# or
brew reinstall anvil
```

No config migration. No new dependencies. No breaking changes.
