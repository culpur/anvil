# Anvil

**AI Coding Assistant by Culpur Defense**

![Version](https://img.shields.io/badge/version-2.2.9-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)
![Tests](https://img.shields.io/badge/tests-837%20passed-brightgreen)
![Security](https://img.shields.io/badge/security-AES--256--GCM%20vault-orange)

Anvil is a local AI coding-agent CLI implemented in safe Rust. It provides interactive sessions, one-shot prompts, workspace-aware tools, and 101 slash commands from a single binary — with no telemetry, full air-gap support, and encrypted credential storage.

---

## What's New in v2.2.9

### Anthropic prompt caching — every request now caches at 1h TTL
Anvil never sent `cache_control` to Anthropic, which meant every turn
re-billed the full system prompt + tool definitions at full input rate.
v2.2.9 injects two cache breakpoints (`{type:"ephemeral", ttl:"1h"}`) on
every Anthropic request — one on the system prompt, one on the last tool
definition. For long sessions, the cost reduction is substantial.

### `anvil project purge`
Wipe per-workspace state without touching the vault, settings, or OAuth
credentials. `anvil project purge [path] [--dry-run|-n] [--yes|-y]
[--all]`. Removes `~/.anvil/sessions/<hash>/`, `daily/<hash>/`,
`private/<hash>.enc`, `file-history/<hash>/`. Useful when archiving a
project or before publishing a previously-private repo.

### Hooks can invoke MCP tools directly
`{"type":"mcp_tool","server":"culpur-infra","tool":"redact","input":{...}}`.
Powerful for vault-scrubber-as-hook, post-tool redaction, etc. All MCP
failure modes (no invoker, server unavailable, transport error) become
warnings — never deny — so an unhealthy MCP server can't crash a turn.

### `--plugin-dir <zip>` and `--plugin-url <https-url>`
Try plugins without installing. HTTPS-only, magic-byte-validated zip
extraction, path-traversal-safe, 50 MiB cap, optional
`--plugin-sha256 HEX` for integrity verification.

### `alwaysLoad: true` on MCP servers
Tools from frequently-used servers (e.g. `culpur-infra`, `qmd`) skip
ToolSearch deferral and are immediately available — no extra discovery
round-trip per session.

### OAuth login works from SSH/WSL/containers
`anvil login` now races the localhost callback against a stdin paste
prompt — whichever completes first wins. If `TcpListener::bind` fails,
auto-fall-back to paste-only with manual-redirect URL. Accepts bare codes,
`code#state=…` suffixes, and full callback URLs.

### `settings.json` is field-tolerant
A stray comma, malformed hook entry, or wrong-shape oauth block no longer
nukes the entire settings file. Bad sections warn-and-skip; good sections
still apply.

### Bash tool survives a vanished CWD
`rm -rf $PWD` mid-session no longer breaks every subsequent Bash call.
Anvil falls back to `$HOME` → `/tmp`, calls `set_current_dir()` so the
process recovers, and emits a one-shot stderr warning.

### Anthropic streaming has a dead-air timer
The OpenAI-compat path got a 5-min dead-air timeout in v2.2.8; the
Anthropic path was overlooked. v2.2.9 fixes that — uses `Instant::now()`
(monotonic, wake-from-sleep safe), resets on every chunk including
`thinking_delta`. Override via `ANVIL_STREAM_DEAD_AIR_MS`.

### TUI dialogs scroll on overflow
The `/configure` overlay (MainMenu 17, WidgetPicker 36, PresetPicker 16,
Notifications 10, Search) and the slash-command completion popup (21+
entries) now route PgUp/PgDn/Home/End/mouse-wheel through a new
`ListViewport` primitive — no more invisibly-truncated screens.

### `/usage` and `/stats` aliases for `/cost`
CC v2.1.118 parity — three names, same handler.

### Previous Releases

- **v2.2.8**: Trait-based agent composition (`/agent compose`), skill front-matter triggers (suggest-not-auto), prompt-type hooks, three-arm skill-eval harness, `precise`/`condensed` output styles, plugin loader forward-compat, embedded bundled plugins
- **v2.2.7**: Cross-OS installers with SHA256 verification, `anvil upgrade`, shell completions, curated Ollama menu, Windows env fixes, release-pipeline hardening
- **v2.2.6**: 17 web config panels, full Status Line editor in browser, AnvilHub installer, deep hierarchical autocomplete, 8 previously-broken TUI handlers restored
- **v2.2.5**: Intelligent memory system — 6-tier architecture, self-improving knowledge base
- **v2.2.4**: Security hardening — 17 audit findings resolved, constant-time HMAC, zero warnings
- **v2.2.3**: Six major features — interactive widget editor, agent types, MCP config panel
- **v2.2.2**: Customizable widget-based status line with 8 presets
- **v2.2.1**: URL rendering fix, context-aware vault form
- **v2.2.0**: Typed credential vault — 21 credential types, category tabs, visual manager
- **v2.1.2**: Credential auto-detection, egress control, signed transcripts
- **v2.1.1**: Live streaming responses, remote control, thinking mode
- **v2.1.0**: AES-256-GCM encrypted vault, file sandbox, modular architecture
- **v2.0.0**: Full Claude Code parity

---

<!-- v2.2.8 details retained below for context -->

## What's New in v2.2.8

### Trait-based agent composition — `/agent compose`
A 30-trait catalogue (expertise × personality × approach) composes thousands of agent variants from one YAML file. `/agent compose security,skeptical,first-principles "audit crates/runtime/src/oauth.rs"` assembles a system prompt in locked order (intro → expertise → personality → approach → task), spawns a subagent turn, renders the response. Dimension conflicts hard-error by default to force intentional composition. Browse the catalogue with `/agent traits`. Adapted from Miessler's `Personal_AI_Infrastructure`.

### Skill front-matter triggers with suggest-not-auto
Skills now declare YAML front-matter `triggers: [keyword, "phrase"]`. Anvil scans your prompt with case-insensitive whole-word matching and **suggests** relevant skills instead of silently injecting them (auto-injection would be a prompt-injection vector when Anvil is shipped to others). The user confirms via `/skill load <name>`. Three bundled skills ship as reference: `security-audit`, `code-review`, `terse`. Adapted from Miessler's `USE WHEN` descriptions + SuperClaude's `commands/*.md` triggers.

### Prompt-type hooks
Plugin lifecycle hooks can now inject a string into the next model turn instead of only running shell commands. `{"type":"prompt","body":"Verify the {tool_name} on {cwd}"}` with variable interpolation. Backward-compatible with bare-string command hooks. The model cannot write its own prompt-hooks (self-modifying-prompt vector).

### Three-arm skill evaluation harness — `anvil skill-eval`
Honest measurement of whether a skill helps: `__baseline__` (no system prompt) vs `__terse__` ("Answer concisely.") vs `<skill>` ("Answer concisely.\n\n" + SKILL.md). Every report bakes in three honest caveats: directional tokenizer, measures count-only (not fidelity/latency/cost/quality), near-zero delta = skill doing nothing useful. JSON snapshots committed to git for diff-against. Adapted from `caveman`'s `evals/`.

### Output style — `precise` (default) vs `condensed`
User-selectable global response style: `/output-style precise` keeps full sentences; `/output-style condensed` activates the bundled terse skill. **Anvil never auto-applies condensed** — always an explicit choice. Auto-Clarity rules inside `terse` still fire for security warnings, irreversible actions, multi-step procedures, and consent confirmations even when condensed is active.

### Plugin loader is forward-compatible
A single bad plugin manifest used to crash the entire binary at startup. v2.2.8 isolates per-plugin failures — `PluginLoadDiagnostic` surfaces structured warnings on stderr and the other plugins continue loading. Exactly the class of bug that bricked a v2.2.7 binary reading v2.2.8 tagged-hook manifests.

### Bundled plugins are embedded in the binary
No more `env!("CARGO_MANIFEST_DIR")` — the bundled plugin tree is now embedded via `include_dir` and materialized to `~/.anvil/plugins/bundled/` on first run with SHA-based fingerprint for idempotent updates. Homebrew users' bundled plugins are finally visible; developers' installed binaries are no longer wired to their live source tree.

### Claude-Code-parity bug fixes
- 429 `Retry-After` is now a minimum, not authoritative (Claude Code v2.1.98 parity)
- 5-minute stream dead-air timeout — override via `ANVIL_STREAM_DEAD_AIR_MS` (v2.1.110 parity)
- Request timeout configurable — override via `ANVIL_API_TIMEOUT_MS` (default 10 min) (v2.1.101 parity)
- `/model` warns on mid-conversation switch (uncached re-read — v2.1.108 parity)
- DangerFullAccess stability invariants + subagent mode inheritance (v2.1.97/v2.1.98 parity)

### Previous Releases
- **v2.2.7**: Cross-OS installers with SHA256 verification, `anvil upgrade`, shell completions, curated Ollama menu, Windows env fixes, release-pipeline hardening
- **v2.2.6**: 17 web config panels, full Status Line editor in browser, AnvilHub installer, deep hierarchical autocomplete, 8 previously-broken TUI handlers restored
- **v2.2.5**: Intelligent memory system — 6-tier architecture, self-improving knowledge base
- **v2.2.4**: Security hardening — 17 audit findings resolved, constant-time HMAC, zero warnings
- **v2.2.3**: Six major features — interactive widget editor, agent types, MCP config panel
- **v2.2.2**: Customizable widget-based status line with 8 presets
- **v2.2.1**: URL rendering fix, context-aware vault form
- **v2.2.0**: Typed credential vault — 21 credential types, category tabs, visual manager
- **v2.1.2**: Credential auto-detection, egress control, signed transcripts
- **v2.1.1**: Live streaming responses, remote control, thinking mode
- **v2.1.0**: AES-256-GCM encrypted vault, file sandbox, modular architecture
- **v2.0.0**: Full Claude Code parity

---

## Install

### Homebrew (macOS / Linux)

```bash
brew install culpur/tap/anvil
```

### curl installer (macOS / Linux)

```bash
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

### PowerShell installer (Windows)

```powershell
irm https://anvilhub.culpur.net/install.ps1 | iex
```

### Self-update

```bash
anvil upgrade          # preferred — SHA256 verified, atomic swap
anvil --update         # legacy alias, same behavior
```

### Install health check

```bash
anvil --check          # prints a per-dependency readiness checklist
anvil --setup          # re-runs the first-run wizard
anvil --uninstall      # removes the binary + completions
```

### Manual download

Pre-built binaries for each release are published to GitHub Releases:

```
https://github.com/culpur/anvil/releases/latest
```

| Platform | Binary |
|---|---|
| macOS Apple Silicon | `anvil-aarch64-apple-darwin` |
| macOS Intel | `anvil-x86_64-apple-darwin` |
| Linux x86_64 | `anvil-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `anvil-aarch64-unknown-linux-gnu` |
| Windows x86_64 | `anvil-x86_64-pc-windows-gnu.exe` |

Every binary is published with a sibling `.sha256` checksum file on the GitHub release, and an out-of-band copy at `https://anvilhub.culpur.net/sha256/<binary>.sha256`. The installers and `anvil upgrade` check both sources and abort on failure — no unverified binary ever lands on disk.

### Shell completions

Completion scripts ship in `install/completions/` and are wired automatically by `install.sh` / `install.ps1`:

| Shell | Path |
|---|---|
| bash | `install/completions/anvil.bash` |
| zsh | `install/completions/anvil.zsh` (add to `$fpath`) |
| fish | `install/completions/anvil.fish` (drop in `~/.config/fish/completions/`) |
| PowerShell | `install/completions/anvil.ps1` (dotted into `$PROFILE`) |

All four cover the full surface: 101 slash commands, every subcommand, global flags, provider names, and model names.

---

## Quick Start

```bash
# Start an interactive session (wizard runs on first launch)
anvil

# One-shot prompt
anvil prompt "explain the architecture of this repo"

# Switch provider or model
anvil --provider openai --model gpt-4o "review the latest changes"
```

The first-run wizard guides you through:
1. **Vault password** — encrypts all credentials with AES-256-GCM
2. **Provider setup** — Anthropic, OpenAI, Ollama, or xAI
3. **Model selection** — choose your default model
4. **Thinking mode** — enable/disable reasoning for supported models

Or set credentials via environment variables:

```bash
export ANTHROPIC_API_KEY="..."    # Anthropic
export OPENAI_API_KEY="..."       # OpenAI
export XAI_API_KEY="..."          # xAI / Grok
export OLLAMA_HOST="..."          # Ollama (local, no key required)
```

---

## Security Architecture

### Encrypted Vault
- **AES-256-GCM** envelope encryption with random DEK per credential
- **Argon2id** key derivation (65MB memory, 3 iterations, 4 parallelism)
- Master password prompted once per session, KEK held in memory only
- Built-in **TOTP generator** (RFC 6238) for 2FA codes
- Zero plaintext credentials on disk

```bash
/vault setup           # Initialize the vault
/vault store my-token  # Store a credential
/vault get my-token    # Retrieve (auto-copies to clipboard)
/vault totp add github # Add TOTP entry
/vault totp github     # Generate current code
```

### File Write Sandbox
- Writes blocked outside project root (detected via `.git`, `Cargo.toml`, `package.json`)
- Always-allowed: `/tmp`, `$TMPDIR`, `~/.anvil/`
- Bypass for power users: `ANVIL_ALLOW_GLOBAL_WRITES=1`
- Read operations allowed everywhere with out-of-boundary warnings

### Content Filter
- Prompt injection detection in tool outputs
- Credential leak scanning (AWS, GitHub, OpenAI, Anthropic, Stripe patterns)
- Modern OpenAI key formats detected (`sk-proj-*`, `sk-svcacct-*`)

### Permission System
- Three modes: `read-only`, `workspace-write`, `danger-full-access`
- Persistent permission memory per project
- Tool-level approval with "allow always" option

---

## Feature Highlights

### Live Streaming & Remote Control
Responses stream token-by-token with real-time TUI rendering. Share sessions via `/remote-control` — viewers connect through any browser with a pairing code. Full multi-client WebSocket relay.

### Multi-Provider AI
Switch between Anthropic, OpenAI, Ollama, and xAI/Grok without leaving your session. Native API support for each provider. Automatic failover handles rate limits.

```bash
/provider list
/provider anthropic
/model opus
/failover add claude-sonnet-4-6
```

### Native Ollama Support
Direct `/api/chat` integration — not OpenAI-compat. Proper thinking mode control, streaming, and token tracking for local models.

```bash
/model qwen3:8b          # Thinking-capable local model
/model llama3.2           # Fast local inference
/model deepseek-r1:14b    # Reasoning model with think: true
```

### 7 Display Languages
Full TUI localization: English, German, Spanish, French, Japanese, Simplified Chinese, Russian.

```bash
/language de
/language zh-CN
```

### VS Code Extension
Chat panel, hub browser, and inline AI actions inside the editor.

### Infrastructure Commands
SSH session management, Kubernetes, Terraform/IaC, Docker, log analysis, database tools, CI/CD pipeline generation, and multi-platform notifications.

### AnvilHub Marketplace
Browse and install skills, plugins, agents, and themes from the AnvilHub package registry.

```bash
/hub search react
/hub install react-expert
```

### Multi-Agent System
7 specialized agent types running in parallel with worktree isolation.

---

## Command Categories

Anvil ships 90 slash commands across six categories. Full reference at [anvilhub.culpur.net/usage](https://anvilhub.culpur.net/usage).

### Core Flow
`/help` `/status` `/model` `/provider` `/permissions` `/login` `/chat` `/vim` `/doctor` `/tokens` `/cost` `/failover` `/configure` `/theme` `/voice` `/language` `/fast`

### Workspace & Memory
`/config` `/memory` `/init` `/diff` `/version` `/teleport` `/context` `/pin` `/unpin` `/qmd` `/semantic-search` `/scaffold` `/env` `/deps` `/mono` `/markdown` `/snippets` `/lsp` `/screenshot` `/vault`

### Sessions & Output
`/clear` `/resume` `/export` `/session` `/history` `/history-archive` `/collab`

### Git & GitHub
`/commit` `/commit-push-pr` `/pr` `/issue` `/branch` `/worktree` `/undo` `/git` `/changelog` `/review-pr`

### Automation & Discovery
`/plugin` `/plugin-sdk` `/agents` `/skills` `/hub` `/bughunter` `/ultraplan` `/debug-tool-call` `/web` `/search` `/generate-image` `/docker` `/test` `/refactor` `/db` `/security` `/api` `/docs` `/perf` `/debug` `/notebook` `/k8s` `/iac` `/pipeline` `/review` `/browser` `/notify` `/migrate` `/regex` `/ssh` `/logs` `/finetune` `/webhook`

---

## Architecture

| Metric | Value |
|--------|-------|
| Language | Rust (100% safe, zero `unsafe`) |
| Crates | 12 workspace members |
| Module Files | 134 |
| Total Lines | 63,576 |
| Largest File | 4,770 lines |
| Slash Commands | 90 |
| Tools | 45 |
| Agent Types | 7 |
| Providers | 4 (Anthropic, OpenAI, Ollama, xAI) |
| Tests | 394 passing |
| Clippy Warnings | 0 (strict mode) |

---

## Configuration

Anvil loads configuration from `~/.anvil/config.json` and, when inside a project, from `ANVIL.md` in the workspace root.

Key settings:

| Setting | Description |
|---------|-------------|
| `model` | Default AI model |
| `provider` | Default provider (`anthropic`, `openai`, `ollama`, `xai`) |
| `permission_mode` | `read-only`, `workspace-write`, or `danger-full-access` |
| `language` | Display language code (e.g. `en`, `de`, `ja`) |
| `theme` | TUI color theme name |

Use `/configure` for an interactive setup wizard, or `/config` to inspect the merged config.

---

## Supported Providers

| Provider | Models | Auth |
|----------|--------|------|
| Anthropic | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | `ANTHROPIC_API_KEY` or OAuth |
| OpenAI | GPT-5.4, GPT-5, o3, o4-mini, GPT-5-codex | `OPENAI_API_KEY` |
| Ollama | Any local model (llama3, qwen3, mistral, etc.) | Local endpoint (no key) |
| xAI / Grok | Grok-3, Grok-3-mini, Grok-2 | `XAI_API_KEY` |

---

## Dependencies

### QMD (Recommended)

QMD is the knowledge-base engine that powers Anvil's context and memory features.

```bash
npm install -g @tobilu/qmd
```

Anvil works without QMD, but memory, history search, and auto-context features require it.

---

## Build from Source

### Prerequisites

- Rust stable toolchain + Cargo
- Provider credentials for your chosen model

```bash
cargo build --release -p anvil-cli
cargo install --path crates/anvil-cli --locked
```

---

## Links

- **Documentation & usage reference**: [anvilhub.culpur.net/usage](https://anvilhub.culpur.net/usage)
- **Package marketplace**: [anvilhub.culpur.net](https://anvilhub.culpur.net)
- **VS Code extension**: [marketplace.visualstudio.com](https://marketplace.visualstudio.com/items?itemName=culpur.anvil-vscode)
- **Homebrew tap**: [github.com/culpur/homebrew-anvil](https://github.com/culpur/homebrew-anvil)
- **Release binaries**: [github.com/culpur/anvil/releases](https://github.com/culpur/anvil/releases)

---

## Release Notes

- [v2.1.1](docs/releases/2.1.1.md) — Live streaming, remote control, thinking mode
- [v2.1.0](docs/releases/2.1.0.md) — Encrypted vault, file sandbox, modular architecture
- [v2.0.0](docs/releases/2.0.0.md) — Claude Code parity
- [v0.1.0](docs/releases/0.1.0.md) — Initial release

---

## License

See the repository root for licensing details.
