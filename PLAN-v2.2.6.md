# Anvil v2.2.6 — Command Parity, Deep Autocomplete, Web Config Panels, AnvilHub Installer

**Status:** APPROVED — executing
**Target:** v2.2.6 (fresh tag, no force-push over v2.2.5)
**Date:** 2026-04-20

---

## Guiding Principles

1. **Single source of truth for commands.** Parser, TUI completer, TUI handler, CLI handler, and web completer must all read from one registry. Drift between them is the root cause of this entire patch.
2. **No new "Command not available in TUI mode" fallthroughs.** The catch-all at `main.rs:3245` gets deleted. Every `SlashCommand` variant must have an explicit arm.
3. **Web UI is a first-class surface.** Every feature configurable via TUI wizard is configurable via web panel. Every command invocable in CLI is invocable in web.
4. **Vault gate is non-negotiable.** AnvilHub installer refuses to act while vault is locked. Browse is allowed; install is not.
5. **Respawn prefers safety over convenience.** Self-respawn only when we can verify the parent is a normal terminal launch. Otherwise prompt.

---

## Scope (what's in this release)

| # | Item | Surface | Owner |
|---|------|---------|-------|
| 1 | CommandSpec v2 registry with subcommand tree | commands crate | core |
| 2 | Wire 8 missing TUI handlers | anvil-cli | core |
| 3 | Remove/implement 4 ghost autocompletes (/tab /fork /share /audit) | tui | core |
| 4 | Deep hierarchical autocomplete (TUI + web) | tui + server | core |
| 5 | Web UI: 15 configuration panels | server + viewer.html | web |
| 6 | Web UI: AnvilHub installer (search + install + restart prompt) | server + viewer.html | web |
| 7 | Self-respawn mechanism | anvil-cli | core |
| 8 | Release pipeline via anvil-release MCP | — | release |

**Out of scope** (deliberately deferred):
- Session persistence/reconnect design (forward list)
- Draw loop cloning perf (task #23)
- Anvil Routines
- Mobile web UI responsiveness (v2.3)

---

## Phase 0: CommandSpec v2 Registry (foundation)

**File:** `/Users/soulofall/projects/anvil-dev/crates/commands/src/specs.rs` (extend, not rewrite)

### Current shape
```rust
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub argument_hint: Option<&'static str>,  // flat string, no structure
    pub resume_supported: bool,
    pub category: SlashCommandCategory,
    pub detailed_help: &'static str,
}
```

### New shape (additive — existing fields preserved for compat)
```rust
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub argument_hint: Option<&'static str>,
    pub resume_supported: bool,
    pub category: SlashCommandCategory,
    pub detailed_help: &'static str,
    // NEW:
    pub subcommands: &'static [SubcommandSpec],  // [] for leaf commands
    pub tui_available: bool,   // gate for "implemented in TUI" — prevents silent fallthrough
    pub web_available: bool,   // gate for web UI
    pub requires_vault: bool,  // locks the command if vault is locked
    pub requires_restart: RestartRequirement,
}

pub struct SubcommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
    pub subcommands: &'static [SubcommandSpec], // recursive — tree depth N
}

pub enum ArgSpec {
    Literal(&'static str),                              // "add", "remove"
    OneOf(&'static [&'static str]),                     // ["anthropic", "openai"]
    DynamicEnum(DynamicEnumSource),                     // resolved at runtime
    FreeText { hint: &'static str },                    // <prompt>, <query>
    OptionalFlag { name: &'static str, value: Option<ArgSpecValue> },
}

pub enum DynamicEnumSource {
    VaultCredentialTypes,    // 21 types from vault/scan.rs
    InstalledPlugins,         // from plugins directory
    InstalledThemes,          // from themes registry
    InstalledAgents,          // from agents registry
    InstalledSkills,          // from skills registry
    McpServers,               // from ~/.anvil/mcp.json
    Sessions,                 // recent sessions
    Models,                   // provider's advertised models
    Providers,                // anthropic/openai/ollama/xai
    Languages,                // i18n codes
}

pub enum RestartRequirement {
    None,
    Soft,      // reload config
    Full,      // requires process restart (plugins, MCP servers)
}
```

### Decisions
- Keep it `const` where possible; dynamic enums resolve through a `CompletionContext` trait that callers implement.
- Spec entries use macros to stay readable — `spec!("/mcp", summary = "...", subcommands = [sub!("list", ...), sub!("status", ...), sub!("tools", args = [dyn(McpServers)])])`.
- Backward-compat shim: `argument_hint` gets auto-derived from `subcommands` when left as `None`.

### Deliverables
- [ ] `SubcommandSpec`, `ArgSpec`, `DynamicEnumSource`, `RestartRequirement` enums
- [ ] `CompletionContext` trait + `NoopCompletionContext` default
- [ ] Populate subcommand trees for all 98 commands (audited list below)
- [ ] `suggest_completions(input: &str, ctx: &dyn CompletionContext) -> Vec<Completion>` — the new deep completer
- [ ] Unit tests: every `SlashCommand` variant round-trips through spec → parse → spec
- [ ] Compile-time check: `SLASH_COMMAND_SPECS.len() == SlashCommand::variants()` (static assert via test)

---

## Phase 1: Wire 8 Missing TUI Handlers

**File:** `/Users/soulofall/projects/anvil-dev/crates/anvil-cli/src/main.rs`

Delete the `_ => "Command not available in TUI mode"` at line 3245. Add explicit arms for:

| Command | CLI arm (exists) | TUI implementation plan |
|---------|------------------|-------------------------|
| `/mcp [list\|status\|tools <server>]` | 4111 | Reuse CLI handler; render output in TUI scrollback |
| `/plugins [list\|enable\|disable\|search]` | 4064 | Reuse CLI handler |
| `/session [list\|load\|save\|export]` | 4061 | Reuse CLI handler; `load` triggers session switch in TUI state |
| `/resume <path>` | 4036 | Same as `/session load`; load into current tab |
| `/sleep` | 4388 | ScheduleWakeup-equivalent for TUI; show countdown widget |
| `/productivity` | 4115 | Reuse CLI handler; render dashboard widget |
| `/knowledge [review\|accept\|reject\|list]` | 4119 | `review` opens modal; others reuse CLI |
| `/daily [date]` | 4123 | Reuse CLI handler; render summary pane |

### Ghost commands in widgets.rs (from earlier audit)
| Command | Decision |
|---------|----------|
| `/tab` | **Implement** — list/new/close/switch (Anvil supports multiple tabs already, just exposed via Ctrl+T/]/[) |
| `/fork` | **Implement** — duplicate current tab with same context |
| `/share` | **Implement** — alias to `/remote-control start` |
| `/audit` | **Implement** — alias to `/security scan` + `/deps audit` + `/vault verify` combined |

Add all 4 to the `SlashCommand` enum and spec registry.

### Deliverables
- [ ] 8 new TUI match arms with real handlers (not stubs)
- [ ] 4 ghost commands promoted to real enum variants
- [ ] Delete the `_ =>` fallthrough at line 3245
- [ ] Compile-time exhaustiveness check: match `SlashCommand` without `_`

---

## Phase 2: Deep Hierarchical Autocomplete

**File:** `/Users/soulofall/projects/anvil-dev/crates/anvil-cli/src/tui/widgets.rs`

### Behavior
| User input | Completions shown |
|-----------|-------------------|
| `/` | All 98 root commands, categorized |
| `/v` | `/vault`, `/version`, `/vim`, `/voice` |
| `/vault ` | `setup`, `unlock`, `lock`, `store`, `get`, `list`, `delete`, `totp` |
| `/vault store ` | 21 credential types (API_KEY, SSH_KEY, TLS_CERT, TOTP, DATABASE_URL, ...) |
| `/vault store SSH_KEY ` | `<label>` free-text hint |
| `/mcp ` | `list`, `status`, `tools` |
| `/mcp tools ` | Dynamic: list of connected MCP servers |
| `/model ` | Dynamic: list of available models for current provider |
| `/theme set ` | Dynamic: list of installed themes |

### Implementation
- Replace `all_slash_commands` static list in widgets.rs with a call to `suggest_completions(input, &TuiCompletionContext)`.
- `TuiCompletionContext` provides the dynamic enum resolvers (MCP servers from runtime, models from provider, themes from registry, etc.).
- Keybindings: Tab cycles through completions, Enter accepts, Escape dismisses, arrow keys navigate.
- Render: popup under the input box with category grouping (Core / Workspace / Git / ...). Show the summary inline.
- For free-text args (`<prompt>`, `<query>`), render the hint greyed out as placeholder.

### Deliverables
- [ ] `TuiCompletionContext` implementation of `CompletionContext`
- [ ] Rewrite `render_slash_completions` in widgets.rs to use `suggest_completions`
- [ ] Update `input_handler.rs` for Tab cycling and multi-level completion state
- [ ] Golden tests for 20 representative input strings

---

## Phase 3: Web UI — 15 Configuration Panels

**Files:**
- `/Users/soulofall/projects/anvil-dev/crates/server/src/viewer.html` (extend)
- `/Users/soulofall/projects/anvil-dev/crates/server/src/assets/config.js` (NEW)
- `/Users/soulofall/projects/anvil-dev/crates/server/src/assets/config.css` (NEW)
- `/Users/soulofall/projects/anvil-dev/crates/anvil-cli/src/remote_control.rs` (extend protocol)

### Panel list (authoritative from configure.rs audit)
1. **Providers** — Anthropic / OpenAI / Ollama / xAI credentials
2. **Models** — default model, image model, failover chain
3. **Context** — context size, auto-compact threshold, QMD toggle, history archival toggle
4. **Search** — 7 search providers (Tavily/Brave/SearXNG/Exa/Perplexity/Google/Bing)
5. **Permissions** — read-only / workspace-write / danger-full-access
6. **Display** — Vim mode, Chat mode, tab keybindings
7. **Integrations** — AnvilHub URL, WordPress, GitHub token
8. **Language & Theme** — i18n + active theme
9. **Vault** — session TTL, auto-lock
10. **Notifications** — Discord / Slack / Telegram / Matrix / Signal webhooks
11. **Failover** — cooldown, budget, auto-recovery
12. **SSH** — key path, bastion, config path
13. **Docker & K8s** — compose path, registry, k8s context/namespace
14. **Database** — DB URL, schema tool
15. **Memory & Archive** — auto-save, archive freq, retention, memory dir
16. **Plugins & Cron** — search paths, auto-enable, cron
17. **Status Line Editor** — interactive widget arranger (already TUI-only)

*(Actually 17 panels; 15 was the audit's rough count. Promoting the list to exact.)*

### Web protocol additions
New WS envelope types between viewer ↔ relay ↔ host:
- `config.snapshot` (host → web) — full config JSON on connect
- `config.update` (web → host) — `{ panel: "vault", field: "auto_lock", value: true }`
- `config.saved` (host → web) — ack with updated snapshot
- `config.error` (host → web) — validation failure

### Per-panel UI components
- Toggle: checkbox with live feedback
- Text: input with debounced save (500ms)
- Masked text: password field with show/hide
- Dropdown: `<select>` populated from `DynamicEnumSource`
- Numeric: `<input type="number">` with min/max validation
- List: add/remove rows (e.g., failover chain)
- Webhook URL: validate scheme + save
- Status Line Editor: deferred — link to TUI command for now with "open in TUI" button

### Security
- All panels render but **vault-sensitive fields hide values while vault is locked** — show `••••` and disable edit
- Integration tokens (GitHub, WordPress, webhooks) are vault-backed
- Password-style fields never round-trip the plaintext; only write-through

### Deliverables
- [ ] Config route in viewer.html with nav sidebar + main pane
- [ ] 17 panel components in config.js
- [ ] WS protocol extensions in remote_control.rs
- [ ] Serde-compatible config update messages
- [ ] CSS for panels matching existing viewer aesthetics
- [ ] Vault lock state integration (disable edit, show lock icon)

---

## Phase 4: Web UI — AnvilHub Installer

**Files:**
- `/Users/soulofall/projects/anvil-dev/crates/server/src/assets/anvilhub.js` (NEW)
- `/Users/soulofall/projects/anvil-dev/crates/runtime/src/hub.rs` (extend)
- `/Users/soulofall/projects/anvil-dev/crates/anvil-cli/src/remote_control.rs` (extend protocol)

### UI
- Tab in the web viewer: "AnvilHub"
- Filters: type (All/Skills/Plugins/Agents/Themes), category, sort (downloads/rating/recent)
- Search box with debounced GET to `/v1/hub/packages/search?q=`
- Grid of package cards: name, author, rating, downloads, install button
- Click card → detail drawer: readme, versions, reviews, compatibility
- Install button:
  - If vault locked → button shows "Unlock vault to install" (disabled)
  - If vault unlocked → "Install v1.2.3"
  - Click → POST via relay to host → host calls `hub::install(package)`
  - Progress indicator during download + scan
- Post-install:
  - Check `RestartRequirement` from package manifest
  - If `None` → toast "Installed"
  - If `Soft` → toast "Installed (config reloaded)"
  - If `Full` → modal "Restart required — restart now?" with [Restart Now] [Later]

### Backend
- Reuse existing `GET /v1/hub/packages*` (public, no auth)
- Install endpoint: POST to host via relay (not to AnvilHub — AnvilHub is read-only from client)
- Host performs download + signature verification + vault-protected storage
- Package types:
  - Skill: write to `~/.anvil/skills/<slug>/`
  - Plugin: write to `~/.anvil/plugins/<slug>/`
  - Agent: write to `~/.anvil/agents/<slug>/`
  - Theme: write to `~/.anvil/themes/<slug>/`

### Deliverables
- [ ] AnvilHub browser panel in viewer
- [ ] Search + filter UI wired to existing API
- [ ] Install handler on host side (calls runtime/hub.rs)
- [ ] Vault lock gate on install action
- [ ] Restart prompt modal with respawn integration

---

## Phase 5: Self-Respawn Mechanism

**File:** `/Users/soulofall/projects/anvil-dev/crates/anvil-cli/src/respawn.rs` (NEW)

### Strategy
1. Record argv[0] + args at startup (`RespawnContext`)
2. Before exec: save session state to `~/.anvil/sessions/<id>/resume.json`
3. Detect launch context:
   - **Safe to respawn** (default): stdin is a TTY, argv[0] is a normal path
   - **Unsafe** (require manual restart):
     - stdin is a pipe (e.g., `echo "foo" | anvil`)
     - launched via `cargo run`
     - launched via debugger (`DEBUGGER` env var present)
     - launched with `--no-respawn` flag
     - Windows (respawn semantics are fragile)
4. On respawn:
   - macOS/Linux: `execvp(argv[0], argv)` — replaces current process in-place, keeps TTY
   - Prompt path: print message + exit with code 42 so user knows to relaunch

### Triggers
- `/restart` command (explicit)
- Post-install full-restart confirmation
- Post-config-change when `RestartRequirement::Full` detected

### Deliverables
- [ ] `respawn.rs` with detection + exec logic
- [ ] `/restart` SlashCommand variant + TUI/CLI handler
- [ ] Integration with AnvilHub installer's restart modal
- [ ] Session state preservation + auto-reload on respawn

---

## Phase 6: Release via anvil-release MCP

**Version bump:** v2.2.5 → v2.2.5.1

### Steps (each via MCP tool)
1. `anvil_build` — cargo build --release, verify zero warnings
2. Run full test suite (should be green before proceeding)
3. Bump version in Cargo.toml + workspace manifests
4. `anvil_tag v2.2.5.1` — create and push tag
5. `anvil_github_release v2.2.5.1` — create release with binaries (5 platforms)
6. `anvil_homebrew v2.2.5.1` — update formula with new SHA256s
7. `anvil_update_pages v2.2.5.1` — update culpur.net + anvilhub.culpur.net + README + 5 other pages
8. `anvil_verify v2.2.5.1` — confirm all 8 pages reflect new version
9. `anvil_full_release` — one-shot that runs the above in sequence

### Local binary install
- `sudo cp /Users/soulofall/projects/anvil-dev/target/release/anvil /opt/homebrew/bin/anvil`

### Rollback plan
- If v2.2.5.1 breaks, revert Homebrew formula (keep v2.2.5 installable via `brew install culpur/anvil/anvil@2.2.5`)
- Yank GitHub release (don't delete tag — tag becomes historical record)

---

## Test Matrix

| Scenario | TUI | CLI | Web UI |
|----------|-----|-----|--------|
| `/mcp list` | ✓ | ✓ | ✓ |
| `/mcp tools <server>` with autocomplete | ✓ | n/a | ✓ |
| `/daily 2026-04-19` | ✓ | ✓ | ✓ |
| `/vault store SSH_KEY prod-key` | ✓ | ✓ | panel |
| `/configure vault session_ttl 3600` | ✓ | ✓ | panel |
| Install skill from AnvilHub (vault unlocked) | n/a | `/hub install X` | UI flow |
| Install skill from AnvilHub (vault locked) | n/a | reject | reject |
| Install MCP server → prompt restart → respawn | n/a | n/a | ✓ |
| Deep completion `/configure ssh key_path ~/...` | Tab works | n/a | live |
| Previously-failing `/knowledge review` | works | works | modal |
| Ghost commands `/fork`, `/tab`, `/share`, `/audit` | works | works | works |

### Regression checks
- [ ] Every existing test still passes
- [ ] `cargo build --release` produces zero warnings
- [ ] TUI starts, accepts `/help`, shows all 98 + 4 = 102 commands
- [ ] Web viewer pairs successfully, all 17 panels render
- [ ] Respawn works on macOS (primary dev platform)
- [ ] Respawn gracefully prompts on Windows (noted as limitation)

---

## Execution Order

Strict ordering because each phase depends on the previous:

1. **Phase 0** (registry) — 2-3 hours. Nothing ships without this.
2. **Phase 1** (TUI handlers) — 1-2 hours. Wire enum variants first, then handlers. Delete the fallthrough last (so compile errors surface missing arms).
3. **Phase 2** (autocomplete) — 1 hour. Data-driven, small once registry is done.
4. **Phase 3** (config panels) — 4-5 hours. Biggest JS/HTML lift. Can run in parallel with Phase 4 conceptually, but protocol needs to be settled first.
5. **Phase 4** (AnvilHub installer) — 2-3 hours. Builds on Phase 3 protocol.
6. **Phase 5** (respawn) — 1 hour. Small but needs careful testing.
7. **Phase 6** (release) — 30 min via MCP. After all tests green.

**Estimated total:** 12-16 hours of focused work. Expect 1 full day + spillover.

---

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Registry refactor breaks existing `suggest_slash_commands` callers | Keep old API as a shim; new API is additive |
| Web UI panels overwhelm existing viewer.html (~250 lines) | Extract to separate `config.html` or use a tab-routed SPA pattern; keep additions in separate JS files |
| AnvilHub install could be abused (malicious packages) | Rely on existing passage-culpur scan; don't bypass APPROVED/FLAGGED filter |
| Respawn race condition (double-launch) | Use PID file lock in `~/.anvil/.running.pid` |
| Cargo workspace version bump churn | Single top-level version in workspace Cargo.toml |
| Session state loss on respawn | Save before exec, restore on startup via existing resume code |

---

## Memory Updates After Release

Save these lessons to memory:
- **feedback**: "Every SlashCommand variant needs explicit arms; never rely on `_ =>` fallthrough — it silently becomes 'Command not available'"
- **feedback**: "Configure.rs sections are the authoritative config inventory; when adding a panel anywhere (TUI/web), add it there first"
- **reference**: Pointer to `PLAN-v2.2.5.1.md` as architectural reference for future feature-surface work

---

## Resolved Decisions (2026-04-20)

1. **Status Line Editor web panel**: FULL implementation. User must be able to configure all elements and all lines from web. Parity with TUI status line editor (37 widgets, 16 presets, per-widget category colors, drag-and-drop).
2. **AnvilHub install auth**: Vault-only for v2.2.6. Public packages remain public. Install action still POSTs to AnvilHub to increment download/install counters (anonymous, no Authentik required). Future v2.3+ may add Authentik gate for private/paid packages; counters must track regardless.
3. **Homebrew versioned formula**: YES — ship both `anvil` (always latest) AND `anvil@2.2.6` (pinned) formulas.
4. **Ghost `/share`**: DISTINCT command. Not a /remote-control alias. `/share` shares the current tab (specific conversation context) as a read-only link; `/remote-control` exposes full control of the whole Anvil instance. Different scope, different UX.

## Scope Additions from Decisions

### New: Install telemetry to AnvilHub
When a user installs a package from web UI or CLI, host emits:
```
POST /v1/hub/packages/:slug/install
  { version: "1.2.3", client: "anvil/2.2.6", platform: "darwin-arm64" }
```
- No auth required (anonymous)
- AnvilHub increments `downloads` counter
- Optional: records install events (aggregated daily) for "trending" rankings
- Gracefully degrades if AnvilHub is offline — install still succeeds

### New: `/share` distinct command
- `/share` — shares the CURRENT TAB's conversation as a read-only URL
  - Subcommands: `/share`, `/share stop`, `/share list`
  - Generates ephemeral URL via passage-culpur relay (separate namespace from /remote-control)
  - Read-only: viewers see messages but can't send input
  - Expires after 24h by default
  - Requires vault unlock (anti-abuse; prevents silent share)

### New: Status Line web editor (full implementation)
- Left panel: widget palette (37 widgets, searchable)
- Center: visual canvas showing multi-line status bar with drag-and-drop
- Right panel: widget properties (category color, format, padding)
- Top: 16 preset dropdown + "Save as Preset"
- Live preview in the main TUI (via WS updates)
- Multi-line support: configure line 1, line 2, etc.
