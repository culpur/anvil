<div align="center">

<br>

# &#9874; Anvil

### The only AI coding assistant that doesn't lock you in.

[![Version](https://img.shields.io/badge/version-2.2.8-0FBCFF?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![Platform](https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows-lightgrey?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![5 Providers](https://img.shields.io/badge/5%20AI%20Providers-00D084?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![License](https://img.shields.io/badge/proprietary-1e293b?style=for-the-badge&labelColor=0a0f1e)](LICENSE)

**Your providers. Your credentials. Your data. Your cost.**<br>
**Five AI providers, one terminal. Switch freely. Own your workflow.**

[**Download**](https://github.com/culpur/anvil/releases/latest) &#8226; [**AnvilHub**](https://anvilhub.culpur.net) &#8226; [**Changelog**](#changelog) &#8226; [**Product Page**](https://culpur.net/anvil/)

<br>
</div>

---

## Why Anvil?

Every other AI coding assistant comes with a leash. Claude Code locks you to Anthropic. Copilot locks you to GitHub. Cursor locks you to their wrapper and their pricing. Your code, your data, your costs &mdash; all flow through one vendor's pipes.

**Anvil is the inverse.** Pick your provider. Use your own API keys, or run everything locally through Ollama. Switch between Claude, GPT, Gemini, Grok, and local models mid-conversation. When one hits a rate limit, fall over to the next. When one gets expensive, change it. When the provider does something you don't like, leave.

No account required. No telemetry. No lock-in. One 15 MB binary, zero dependencies, five providers.

---

## What you keep control of

| | |
|---|---|
| &#128273; **Your providers** | Anthropic, OpenAI, Google, xAI, or local Ollama. Configure priority chains. Automatic failover when one throttles. Never locked in. |
| &#128274; **Your credentials** | Typed credential vault &mdash; AES-256-GCM encrypted with Argon2id. API keys, SSH keys, TLS certs, TOTP codes, DB URLs. Nothing touches disk unencrypted. |
| &#128737; **Your data** | Single binary, zero telemetry, local Ollama support. Run air-gapped. Your prompts and code never leave your machine unless you send them. |
| &#128176; **Your cost** | Per-provider budgets. Per-session cost tracking. Hard caps. See what every token costs before you spend it. Run Ollama for zero-cost inference. |
| &#128225; **Your access** | Type `/remote-control` and hand any session to any browser. Pair with a 6-digit code. Full bidirectional control. Code from your phone. |
| &#127969; **Your deployment** | Run on your laptop. Run on a server. Share a session across devices. Nothing to install on the browser side. |

---

## Who this is for

- **Privacy-conscious developers** who don't want every prompt going to OpenAI or Anthropic &mdash; and can't afford a $50K local-inference stack
- **Consultants and contractors** juggling credentials across clients, needing isolation between projects
- **Open-source maintainers** tired of single-provider lock-in
- **Teams** who want deployment choice &mdash; cloud providers, local Ollama, or a mix

---

## Live Remote Control

**No other AI coding assistant does this.**

```
you@workstation:~$ anvil
> /remote-control
  Remote control active: https://passage.culpur.net/viewer#abc123
  Pairing code: 847291
  Open the URL on any device and enter the code.
```

Open that URL on your phone, your tablet, a colleague's laptop, or a monitor across the room. Enter the 6-digit code. You're connected.

- **Full bidirectional control** &mdash; type messages, run commands, manage tabs from any device
- **Real-time streaming** &mdash; see AI responses token-by-token in the browser
- **Same 98 slash commands** as the terminal, with deep autocomplete
- **Configure from the browser** &mdash; swap providers, change models, manage credentials
- **Encrypted** &mdash; secure WebSocket relay with automatic reconnection

*Perfect for pair programming, teaching, demos, monitoring long-running tasks, or coding from your phone while your workstation does the heavy lifting.*

---

## Install

```bash
# Homebrew (macOS & Linux)
brew install culpur/anvil/anvil

# Or download directly
curl -LO https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-apple-darwin
chmod +x anvil-* && sudo mv anvil-* /usr/local/bin/anvil
```

| Platform | Download |
|----------|----------|
| **macOS ARM** (M1/M2/M3/M4) | [`anvil-aarch64-apple-darwin`](https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-apple-darwin) |
| **macOS Intel** | [`anvil-x86_64-apple-darwin`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-apple-darwin) |
| **Linux x86_64** | [`anvil-x86_64-unknown-linux-gnu`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-linux-gnu) |
| **Linux ARM64** | [`anvil-aarch64-unknown-linux-gnu`](https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-unknown-linux-gnu) |
| **Windows x86_64** | [`anvil-x86_64-pc-windows-gnu.exe`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-pc-windows-gnu.exe) |

No account. No sign-in. Download, run, configure your providers.

---

## Five providers, one terminal

| Provider | Models | Auth |
|----------|--------|------|
| **Anthropic** | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | OAuth or API Key |
| **OpenAI** | GPT-5, o3, o4-mini | API Key |
| **Google** | Gemini 2.5 Pro, Gemini 2.5 Flash | API Key |
| **xAI** | Grok-3, Grok-3-mini | API Key |
| **Ollama** *(recommended)* | Llama, Qwen, Mistral, DeepSeek, Gemma | Local &mdash; no key needed |

Configure priority chains. Automatic failover when one hits a rate limit. Per-provider budgets. Cost tracking per session. Zero-cost local inference with Ollama.

---

## Quick Start

```bash
anvil                               # Start interactive session
/remote-control                     # Share via browser
/model claude-opus-4-6              # Switch model
/provider anthropic                 # Switch provider
/vault add                          # Store a credential
/productivity                       # Session stats
/mcp list                           # MCP server status
/fork experiment                    # Branch the conversation
/focus                              # Distraction-free mode
/export md                          # Export as Markdown
```

---

## Also in the box

**98 slash commands.** 45 built-in tools. MCP integration. Multi-tab sessions. Git integration. Code productivity dashboard. Session history search. 37-widget configurable status line with 16 presets. Vim keybindings. Focus view. File sandbox with permission modes. 7-language i18n. AnvilHub marketplace for skills, plugins, agents, and themes. Web UI with full configuration parity. All of it optional. None of it required.

Feature list is in [the changelog below](#changelog) and [anvilhub.culpur.net/about](https://anvilhub.culpur.net/about). The feature list isn't the story. The freedom is.

---

## Links

| | |
|---|---|
| &#127968; **Product Page** | [culpur.net/anvil](https://culpur.net/anvil/) |
| &#128230; **Marketplace** | [anvilhub.culpur.net](https://anvilhub.culpur.net) |
| &#128214; **Full Changelog** | [anvilhub.culpur.net/about](https://anvilhub.culpur.net/about) |
| &#128172; **Issues** | [github.com/culpur/anvil/issues](https://github.com/culpur/anvil/issues) |

---

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.

---

## Changelog



### v2.2.8 &mdash; April 22, 2026

**PAI-inspired composition, learning, and robustness.**

- &#10003; `/agent compose <traits...> "<task>"` &mdash; trait-based agent composition engine, 30-trait catalogue (expertise × personality × approach), dimension-conflicts hard-error by default. Adapted from Miessler's `Personal_AI_Infrastructure`.
- &#10003; Skill front-matter `triggers` with suggest-not-auto UX &mdash; three bundled reference skills (`security-audit`, `code-review`, `terse`). Never auto-inject (prompt-injection vector); user confirms via `/skill load <name>`.
- &#10003; Prompt-type hooks &mdash; plugin lifecycle hooks can now inject a string into the next model turn with `{tool_name}` / `{cwd}` / `{date}` / `{model}` interpolation. Backward-compatible with bare-string command hooks.
- &#10003; `anvil skill-eval` &mdash; three-arm evaluation harness (`__baseline__` / `__terse__` / `<skill>`) with honest caveats baked into every report. Adapted from `caveman`.
- &#10003; `/output-style precise|condensed` &mdash; user-selectable global response style. Precise (default) preserves full sentences; condensed activates the bundled `terse` skill. **Never auto-applies condensed** &mdash; Auto-Clarity rules still fire for security / irreversible / multi-step / consent contexts even when condensed.
- &#10003; Plugin loader is forward-compatible &mdash; a single bad manifest no longer crashes the entire binary. `PluginLoadDiagnostic` surfaces per-plugin warnings on stderr.
- &#10003; Bundled plugins are now embedded in the binary via `include_dir` &mdash; Homebrew users' bundled plugins are visible; developers' installed binaries no longer depend on their live source tree.
- &#10003; Claude-Code-parity bug fixes: 429 `Retry-After` minimum; 5-min stream dead-air timeout; configurable request timeout (`ANVIL_API_TIMEOUT_MS`); `/model` warns on mid-conversation switch; DangerFullAccess stability invariants.
- &#10003; 756 tests passing (+198 vs v2.2.7)

### v2.2.7 &mdash; April 21, 2026

**Cross-OS installers, `anvil upgrade`, shell completions, curated Ollama menu, Windows fixes, release-pipeline hardening.**

- &#10003; `install.sh` (macOS/Linux) and `install.ps1` (Windows) with SHA256 verification from anvilhub.culpur.net with GitHub fallback &mdash; aborts on dual failure, no unverified binary ever lands
- &#10003; `anvil upgrade`, `anvil --check`, `anvil --setup`, `anvil --uninstall` &mdash; full lifecycle from the binary itself
- &#10003; Shell completions for bash, zsh, fish, and PowerShell &mdash; all 101 slash commands, subcommands, flags, provider and model names
- &#10003; First-run wizard: curated Ollama model menu &mdash; Llama 3.x, Qwen 3 / 2.5-Coder, Mistral Nemo, Gemma 3, Phi 4, Code Llama, Codestral, per-model confirmation
- &#10003; TUI scrollback + text selection via Shift-drag pass-through to the terminal emulator
- &#10003; Windows: correct `HOME` / `PATH` / `PATHEXT` handling, `.exe` on respawn, cmd.exe-aware install detection
- &#10003; QMD cross-platform discovery &mdash; no more hard-coded Unix socket paths
- &#10003; Ollama tool-use: multi-format parser (Anthropic, OpenAI, XML, JSON-fence, natural language) with fail-loud on ambiguity
- &#10003; Remote-control 503 fixed &mdash; `anvil-relay-ws` now a declared pm2 app in the passage ecosystem
- &#10003; Release pipeline: per-binary embedded-version audit gate &mdash; makes the v2.2.6 Windows-exe-labeled-as-2.2.1 class of bug impossible
- &#10003; 618 tests passing, zero warnings

### v2.2.6 &mdash; April 20, 2026

**Command Parity, Deep Autocomplete, Web Config, AnvilHub Installer** &mdash; the biggest web UI release since v2.0.

- &#10003; 17 web config panels &mdash; vault, notifications, SSH, Docker/K8s, memory, and more
- &#10003; Full Status Line editor in browser &mdash; 36 widgets, 16 presets, drag-and-drop, live preview
- &#10003; AnvilHub installer &mdash; search, install, restart prompt &mdash; vault-gated, telemetry-tracked
- &#10003; Deep hierarchical autocomplete &mdash; `/vault store <Tab>` &rarr; 21 credential types
- &#10003; 8 previously-broken TUI handlers now working &mdash; `/mcp`, `/plugins`, `/session`, `/daily`, and more
- &#10003; New commands &mdash; `/tab`, `/fork`, `/share`, `/audit`, `/restart`
- &#10003; Self-respawn on macOS/Linux after plugin installs

### v2.2.5 &mdash; April 19, 2026

**Six Major Features** &mdash; interactive editor, productivity, MCP, history, plugins, agents.

- &#10003; Interactive Status Line Editor &mdash; full TUI editor with 6 sub-screens + WebUI drag-and-drop visual editor
- &#10003; 37 widgets, 16 presets (8 emoji-rich themes), per-widget category colors
- &#10003; Code Productivity Dashboard &mdash; live git diff tracking, `/productivity` command
- &#10003; MCP Server Manager &mdash; `/mcp` command, live McpStatus widget
- &#10003; Session History Browser &mdash; `/history-archive stats` with model breakdown
- &#10003; Plugin System UI &mdash; web viewer management panel with config toggles
- &#10003; Agent Panel Expansion &mdash; web viewer agent management buttons

### v2.2.4 &mdash; April 16, 2026

**Security Hardening + Optimization** &mdash; 17 audit findings fixed.

- &#10003; Constant-time HMAC verification, plugin command injection prevention
- &#10003; Path traversal protection, cryptographic session IDs
- &#10003; 110 functions made const fn, zero compiler warnings
- &#10003; RC widget: live client count with connect/disconnect signals

### v2.2.3 &mdash; April 15, 2026

**Six Major Features** &mdash; interactive editor, productivity, MCP, history, plugins, agents.

- &#10003; Interactive Status Line Editor &mdash; 37 widgets, 16 presets, visual editor
- &#10003; Code Productivity Dashboard &mdash; live git diff tracking
- &#10003; MCP Server Manager, Session History Browser, Plugin UI, Agent Panel

### v2.2.2 &mdash; April 14, 2026

**Customizable Widget-Based Status Line** &mdash; 8 presets for different workflows.

- &#10003; Widget-based status line system &mdash; 28 widget types, dynamic rendering
- &#10003; 8 presets: default, minimal, developer, token-heavy, git-heavy, compact, cost-focused, streamer
- &#10003; `/configure statusline` command with full tab completion
- &#10003; Web viewer config panel gains Status Line preset selector
- &#10003; Dynamic footer height &mdash; 2-line presets maximize content area

### v2.2.1 &mdash; April 14, 2026

**URL rendering fix, context-aware vault, CI/CD automation.**

- &#10003; URL rendering fix &mdash; terminal hyperlinks render correctly across all providers
- &#10003; Context-aware vault &mdash; vault auto-selects credentials based on active project context
- &#10003; CI/CD automation &mdash; `/cicd` command scaffolds pipelines for GitHub Actions and GitLab CI

### v2.2.0 &mdash; April 14, 2026

**Typed Credential Vault** &mdash; the vault is now the single source of truth for ALL sensitive data.

- &#10003; Typed credential entries &mdash; `name`, `type`, `value`, `tags`, `created_at`, `rotated_at`
- &#10003; Vault covers API keys, SSH keys, TLS certs, tokens, and environment secrets
- &#10003; `/vault add` &mdash; interactive typed credential entry with category selection
- &#10003; `/vault rotate` &mdash; rotate any credential in-place, preserving audit history
- &#10003; `/vault export` &mdash; encrypted vault export for backup and migration
- &#10003; `/vault inject` &mdash; load vault secrets into shell env for any subprocess
- &#10003; Audit trail v2 &mdash; every vault access logged with timestamp, operation, and credential type

### v2.1.4 &mdash; April 14, 2026

**Browser configuration panel, Gemini provider, slash command execution in web viewer.**

- &#10003; Browser-based visual configuration panel
- &#10003; Google Gemini as 5th provider
- &#10003; Slash commands execute from web viewer
- &#10003; 30+ commands with subcommand completions

### v2.1.3 &mdash; April 14, 2026

**Focus view, context warnings, stalled stream recovery.**

- &#10003; Focus view &mdash; `/focus` hides sidebars and agent panels for distraction-free mode
- &#10003; Context-low warning &mdash; proactive alert before auto-compaction fires
- &#10003; Stalled stream handling &mdash; detects and recovers from stuck token streams
- &#10003; `/loop` and `/proactive` &mdash; recurring prompt loops and proactive agent nudges

### v2.1.2 &mdash; April 14, 2026

**Credential scanner, egress control, conversation branching &mdash; 16 new features.**

- &#10003; Credential auto-detection &mdash; scans env vars, dotfiles, SSH keys, TLS certs
- &#10003; Network egress control &mdash; configurable domain allowlist
- &#10003; Signed session transcripts &mdash; HMAC-SHA256 audit trail
- &#10003; Conversation branching &mdash; `/fork` to snapshot and branch
- &#10003; Markdown session export &mdash; `/export md` with code blocks
- &#10003; Remote control browser auto-open
- &#10003; Expanded cost tracking &mdash; OpenAI, xAI, Ollama pricing
- &#10003; Smart context compaction &mdash; preserves recent messages and code blocks

### v2.1.1 &mdash; April 13, 2026

**Live streaming, remote control, and thinking mode.**

- &#10003; Live streaming responses &mdash; real-time token-by-token rendering
- &#10003; Remote control &mdash; `/remote-control` to share sessions via browser
- &#10003; Thinking mode &mdash; `/think` enables extended reasoning

### v2.1.0 &mdash; April 8, 2026

**Security-first release** &mdash; encrypted vault, file sandbox, permission modes.

### v2.0.0 &mdash; April 7, 2026

**Claude Code parity** &mdash; multi-agent system, TUI tabs, context management.

### v1.0.4 &mdash; April 7, 2026

Multi-agent system &mdash; 7 agent types with task orchestration.

### v1.0.3 &mdash; April 7, 2026

VS Code extension, 21 new features.

### v1.0.2 &mdash; April 7, 2026

Internationalization &mdash; 7 languages, 20 features.

### v1.0.1a &mdash; April 3, 2026

Cross-compilation CI pipeline &mdash; 5-platform builds.

### v1.0.0 &mdash; April 2, 2026

**Initial release.** Terminal-native AI coding assistant with credential vault and TUI.

---

<div align="center">

**Built by [Culpur Defense](https://culpur.net)** &#8226; **[AnvilHub](https://anvilhub.culpur.net)** &#8226; **[Product Page](https://culpur.net/anvil/)**

</div>