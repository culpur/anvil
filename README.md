<div align="center">

<br>

# &#9874; Anvil

### The only AI coding assistant that doesn't lock you in.

[![Version](https://img.shields.io/badge/version-2.2.19-0FBCFF?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![Platform](https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows%20%7C%20BSD-lightgrey?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![35 AI Providers](https://img.shields.io/badge/35%20AI%20Providers-00D084?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![License](https://img.shields.io/badge/proprietary-1e293b?style=for-the-badge&labelColor=0a0f1e)](LICENSE)

**Your providers. Your credentials. Your data. Your cost.**<br>
**35 AI providers, one terminal. Switch freely. Own your workflow.**

[**Download**](https://github.com/culpur/anvil/releases/latest) &bull; [**AnvilHub**](https://anvilhub.culpur.net) &bull; [**Changelog**](#changelog) &bull; [**Product Page**](https://culpur.net/anvil/)

<br>
</div>

---

## Why Anvil?

Other AI coding assistants come with a leash. One vendor's pipe, one vendor's pricing, one vendor's rate limits — and when that vendor changes something you don't like, you're stuck. Your code, your data, your costs all flow through infrastructure you don't control.

**Anvil is the inverse.** Pick your provider. Use your own API keys, or run everything locally through Ollama. Switch between models mid-conversation. When one hits a rate limit, fall over to the next. When one gets expensive, change it. When the provider does something you don't like, leave.

No account required. No telemetry. No lock-in. A single ~24&ndash;42 MB binary, zero dependencies, **35 providers, seven platforms**.

---

## What you keep control of

| | |
|---|---|
| &#128273; **Your providers** | 35 providers including Anthropic (Max-plan OAuth supported), OpenAI, Google Gemini (Code Assist OAuth), AWS Bedrock (manual SigV4, no AWS SDK), Cursor Cloud Agents, GitHub Copilot, Azure OpenAI, Ollama (local + cloud), Groq, Fireworks, Mistral, Perplexity, DeepSeek, Together AI, DeepInfra, Cerebras, NVIDIA NIM, HuggingFace, Moonshot, Nebius, Scaleway, STACKIT, Baseten, Cortecs, 302.AI, ZAI, OpenRouter, LMStudio, Chutes, MiniMax. Configure priority chains. Automatic failover when one throttles. Never locked in. |
| &#128274; **Your credentials** | Typed credential vault &mdash; AES-256-GCM encrypted with Argon2id. API keys, SSH keys, TLS certs, TOTP codes, DB URLs. Nothing touches disk unencrypted. |
| &#128737; **Your data** | Single binary, zero telemetry, local Ollama support. Run air-gapped. Your prompts and code never leave your machine unless you send them. |
| &#128176; **Your cost** | Per-provider budgets. Per-session cost tracking. Hard caps. See what every token costs before you spend it. Run Ollama for zero-cost inference. |
| &#128225; **Your access** | Type `/remote-control` and hand any session to any browser. Pair with a 6-digit code. Full bidirectional control. Code from your phone. |
| &#127969; **Your deployment** | Run on your laptop. Run on a server. Share a session across devices. Nothing to install on the browser side. |

---

## Who this is for

- **Privacy-conscious developers** who don't want every prompt going to a cloud API &mdash; and can't afford a $50K local-inference stack
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
- **Same slash commands** as the terminal, with deep autocomplete
- **Configure from the browser** &mdash; swap providers, change models, manage credentials
- **Encrypted** &mdash; secure WebSocket relay with automatic reconnection

*Perfect for pair programming, teaching, demos, monitoring long-running tasks, or coding from your phone while your workstation does the heavy lifting.*

---

## What's new in v2.2.19 &mdash; 18 Languages, Memory Cohesion Complete, Web MCP Builder

**v2.2.19 closes two long-running arcs.** The internationalization commitment that started in v2.2.18 ships in 18 languages with a wizard picker, live runtime switching, OS locale auto-detect on first launch, and a soft drift gate so any locale that diverges from `en` is visible immediately. The seven-layer memory architecture promised since v2.2.14 finally has every layer wired end-to-end &mdash; including the three RED layers (Semantic, Reflective, Cache) that were still stubs. Plus a new web-based MCP Builder lands on AnvilHub at [/build](https://anvilhub.culpur.net/build), a full Claude Code parity sweep (CC v2.1.144 → v2.1.146) lands 15 concrete fixes including 3 P0 security/correctness items, and the release pipeline grows per-phase START/OK/FAIL gates that catch silent exits like the v2.2.18 Phase&nbsp;6 incident.

### Internationalization &mdash; 18 locales

The TUI, wizard, slash-command output, and remote-control viewer all flow through `rust-i18n` v4 in Rust and the new `viewer.locales.js` runtime in the browser. **Tier-1** ships English, Spanish, Simplified Chinese, French, Brazilian Portuguese, Russian, Japanese, German &mdash; 264 keys each. **Tier-2** adds Korean, Italian, Turkish, Vietnamese, Polish, Indonesian, Dutch, Swedish, Norwegian Bokmål, Ukrainian. Locale selection persists to `~/.anvil/config.json`, falls back to `$LANG` on first launch, and applies immediately to every wizard step.

The in-TUI `/configure` menu has a Language Picker submenu rendering all 18 locales in their native script (한국어, Русский, 中文, العربية support is excluded by scope &mdash; RTL ships in a future release). The viewer ships 176 fully-wired keys covering chrome plus `vault.*` (entry forms, master-key modal) and `config.*` (SSH, Database, Plugins+MCP, Layout, Status-line editor) panels &mdash; all routed through `data-i18n-key` attributes that the live re-render walker translates on locale switch with no page reload.

### Seven-layer memory &mdash; all GREEN

The 2026-05-21 cohesion audit found that Layer 3 (Semantic), Layer 6 (Reflective), and Layer 7 (Cache) were still partial. v2.2.19 closes all three.

- **Layer 3 &mdash; Semantic.** `/memory promote <nomination-id>` now actually persists nominated facts to disk. The v2.2.14 stub flipped a status flag without ever calling `MemoryManager::save()` &mdash; nominated facts never reached `ANVIL.md`. The full chain now writes the fact and appends provenance comments (`# nominated_at`, `# source: nomination/<id>`) before marking the nomination accepted. New `--target <file>` flag routes a nomination to a specific file with relative, absolute, and `~`-expanded path support.
- **Layer 7 &mdash; Cache.** The file-cache path-discovery bug is fixed: previously `memory_budget` checked a directory that no longer exists in the project-scoped layout, so cache counts always reported zero. Now uses `FileCacheManager::new(cwd)` to discover the actual per-project path. `/memory show cache` enumerates file-cache, command-cache, and QMD-cache stats; `/memory prune cache --dry-run` walks both `FileCacheManager` and `CommandCacheManager` for stale candidates without mutating anything.
- **Layer 1, 2, 4 &mdash; live introspection.** `/memory layer 1` renders a live snapshot of the working-memory inventory via `PromptSectionsExt::iter_by_kind()`. `/memory show episodic` unifies daily summaries, history archives, and workspace sessions; `/memory prune episodic` adds TTL-based retention with a trash-bin safety net so candidates move to `~/.anvil/trash/<unix-ts>/...` rather than being deleted. `/memory show procedural` consolidates GoalManager state, on-disk skills, bundled skills, and CronManager schedule into one view.

### AnvilHub `/build` page + `anvil-mcp-builder` micro-service

Anvil's `/mcp builder` TUI wizard from v2.2.18 now has a web counterpart. The `anvil-mcp-builder` micro-service runs at `127.0.0.1:4090` on the AnvilHub host and exposes three endpoints: `POST /api/builder/spec` (LLM-generated spec from free-text prompt, SSE-streamed response), `POST /api/builder/generate` (turns spec into a base64 tarball &mdash; Node.js, TypeScript, or Python templates), and `POST /api/builder/sandbox` (extracts the tarball and runs `anvil-sandbox-runner` network-cut).

**Security.** The operator OAuth token is loaded from the Anvil vault at startup, never from `.env`. The service exits 1 if vault is locked or the entry is missing; token is cached in process memory only and redacted from all log output. The sandbox endpoint &mdash; which runs `npm install` / `pip install` per request &mdash; is gated on publisher standing: user must be in the `anvilhub-publishers` Authentik group OR have at least one HubPackage already published. The check hits AnvilHub's `/api/users/<email>/publisher-status` with a 5-minute Map cache and falls closed on backend error.

### CC parity sweep &mdash; v2.1.144 → v2.1.146

A complete CC parity audit covered the 4-day window since v2.1.143 across 3 CC releases. 15 fixes filed and shipped this release &mdash; 3 P0, 5 P1, 7 P2. Highlights:

- **P0 &mdash; MCP pagination (CC v2.1.144-B6 / v2.1.146-B2).** The MCP client now consumes the full `nextCursor` / `has_more` pagination chain for `tools/list`, `resources/list`, `resources/templates/list`, and `prompts/list`. Previously MCP servers with paginated responses had everything beyond page 1 silently dropped.
- **P0 &mdash; Spinner/elapsed-time freeze (CC v2.1.145-B3).** The TUI render queue wakes from a wall-clock timer in addition to input events. After a terminal refocus or resize, the spinner and elapsed-time display no longer freeze until next keypress.
- **P0 SECURITY &mdash; MCP `permissions.allow` not honored (CC community #61077).** `permissions.allow` rules with patterns like `mcp__server__tool` or `mcp__server__*` are now consulted at MCP tool dispatch time. Previously the allowlist was loaded but the MCP dispatch path bypassed it and always prompted.
- **P1 SECURITY &mdash; Bash env-var assignment permission bypass (CC v2.1.145-B1).** Bash patterns of the form `KEY=VALUE command` are now decomposed and the command portion checked against the allowlist.
- **P1 &mdash; Skill fork-context recursion guard (CC v2.1.145-B4).** A skill cannot transitively invoke itself; the recursion check uses the full skill ancestry chain, not just the immediate parent.
- **P1 &mdash; Resume session model preservation (CC community #61068).** `anvil --resume` restores the model that was active when the session was saved, not the global default.

Plus 7 P2 fixes covering API startup timeout, mime-type fallback in `Read`, `/branch` history recovery post-EnterWorktree, MCP image fallback for unsupported MIME, skill watcher FD exhaustion prevention, theme color reset on first `/session rename`, and EnterWorktree MCP config preservation.

### `anvild` &mdash; separate process name on 7 platforms

The background OAuth-refresh + routines daemon now runs as `anvild`, not `anvil daemon foreground`. A new `anvild_path_from(anvil_binary)` helper rewrites the binary path used by every supervisor unit &mdash; macOS LaunchAgent, Linux user-systemd, FreeBSD/NetBSD rc.d, Windows Task Scheduler &mdash; plus the in-TUI `daemon::spawn_detached` fallback. `ps -ef | grep daemon` now shows `anvild` rather than masquerading as the foreground TUI binary. `install.sh` and `install.ps1` create the `anvild` symlink (Unix) or hardlink (Windows NTFS) alongside the main binary.

### Wizard P0 bundle

Three wizard bugs surfaced during preview-binary testing and landed before tag:

- **Ollama choice modal clipped (#767).** `ConfirmModal` had a hardcoded 9-row height. On the Ollama wizard's "already_running" modal where the body embeds a multi-model list, body wrap consumed all 7 inner rows and the Yes/No buttons + key hint were clipped invisible. Now derives `modal_h` from `wrap_body(body, width).len() + 6` so buttons stay visible regardless of body length. New `Ctrl+B` Back keybind on Choice + Confirm modals.
- **Daemon-install prompt broke alt-screen (#768).** The "Install anvild as background service?" prompt was running as `println!` + `io::stdin().read_line()` from `anvild_bootstrap::ensure_anvild_for_session` *after* the wizard exited its alt-screen, breaking the rule that the wizard never drops to CLI. New `wizard_daemon::run_daemon_step` runs inside the same alt-screen as Step&nbsp;8 of 9 with three choices (Install / Ask later / Never) and persists `anvild.autostart` to `config.json`.
- **Vertical-split column selection hint (#769).** A normal click-drag in the deck column pulls in left-rail text because terminal emulators select at pixel coordinates with no awareness of Anvil's columns. v2.2.19 renders a 1-line hint above the BUILD section in the rail bottom group: `⌥+drag deck only` on macOS, `Alt+drag deck only` on Linux/Windows/BSD. i18n keys added across all 18 supported locales.

### Release-pipeline hardening (#714, #730)

`scripts/release.sh` now wraps every phase in `step "PN: <description>"` + `ok "PN"` / `fail "PN"` markers. The new `scripts/release-helpers/step-gates.sh` provides primitives + JSON status persistence + an EXIT-trap silent-exit detector that marks any RUNNING phase as FAIL on premature script exit. This closes the v2.2.18 Phase&nbsp;6 silent-exit class of bugs &mdash; the `set +e` / `SSH_RC=$?` / `set -e` pattern now wraps every SSH call so heredoc-style remote work surfaces its exit code instead of cascading into a `set -e` silent kill. `scripts/test-release-gates.sh` is the regression harness; runs release.sh in `--dry-run` mode and asserts every expected phase fires START + terminal marker exactly once.

### Quality bar

Net +50 tests across the workspace: i18n drift gate + picker invariant + 8 locale-load tests, +9 episodic / +6 promote / +6 cache / +5 working / +3 procedural for memory cohesion, +35 across all 15 CC-parity fixes, +7 publisher-standing tests in the new micro-service. One pre-existing flake fixed: `routines::proposal::drop_pending_only_targets_named_routine` now uses `tempfile::TempDir` for guaranteed per-thread isolation.

### Compatibility

v2.2.19 is a drop-in upgrade from v2.2.18. Config, vault, and session formats are forward-compatible &mdash; no migration steps required. New locale key in `config.json` is optional (defaults to `$LANG` then `en`). The `anvild` rename is supervisor-unit-level only; existing daemons keep running until next restart, at which point the new unit definitions take effect.

---

### Install

Seven platforms, SHA256-verified, single binary, no runtime required.

```bash
# Homebrew (macOS & Linux)
brew install culpur/anvil/anvil

# Or download directly
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

| Platform | Download |
|----------|----------|
| **macOS ARM** (M1/M2/M3/M4) | [`anvil-aarch64-apple-darwin`](https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-apple-darwin) |
| **macOS Intel** | [`anvil-x86_64-apple-darwin`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-apple-darwin) |
| **Linux x86_64** | [`anvil-x86_64-unknown-linux-gnu`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-linux-gnu) |
| **Linux ARM64** | [`anvil-aarch64-unknown-linux-gnu`](https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-unknown-linux-gnu) |
| **Windows x86_64** | [`anvil-x86_64-pc-windows-gnu.exe`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-pc-windows-gnu.exe) |
| **FreeBSD x86_64** | [`anvil-x86_64-unknown-freebsd`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-freebsd) |
| **NetBSD x86_64** | [`anvil-x86_64-unknown-netbsd`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-netbsd) |

No account. No sign-in. Download, run, configure your providers.

---

## 35 providers, one terminal

| Provider | Models | Auth |
|----------|--------|------|
| **Anthropic** | claude-opus-4-7, claude-sonnet-4-6, claude-haiku-4-5 | OAuth (Max plan supported) or API Key |
| **OpenAI** | GPT-5, o3, o4-mini | API Key |
| **OpenAI Codex** | codex-mini | API Key |
| **Google Gemini OAuth + Antigravity** | Gemini 2.5 Pro, Gemini 2.5 Flash, Gemini 2.0 Flash Thinking | Code Assist OAuth (PKCE) |
| **AWS Bedrock** | Anthropic Claude family, Llama, Mistral, Titan | manual SigV4 (no AWS SDK) |
| **Cursor Cloud Agents** | claude-4-sonnet-thinking, sonnet-4, sonnet-3-7-thinking | API Key + GitHub repo binding |
| **GitHub Copilot** | gpt-5, gpt-5-mini, gpt-4.1, gpt-4o, sonnet-4, opus-4.5 | Device flow |
| **Azure OpenAI** | (deployment-name based) | API Key + `api-version` |
| **xAI** | Grok-4, Grok-4-mini, Grok-3 | API Key |
| **Ollama** *(recommended)* | Llama, Qwen, Mistral, DeepSeek, Gemma, GPT-OSS | Local &mdash; no key needed |
| **Ollama Cloud** | kimi-k2.6:cloud, gpt-oss:120b-cloud | ed25519 device key (via local daemon) |
| **Groq** | Llama 3.3 70B, Mixtral, DeepSeek R1 | API Key |
| **Fireworks AI** | Llama 3.1/3.2 family, Mixtral, DeepSeek | API Key |
| **Mistral** | Mistral Large, Codestral, Mixtral | API Key |
| **Perplexity** | sonar, sonar-pro, sonar-reasoning | API Key |
| **DeepSeek** | deepseek-chat, deepseek-coder, deepseek-r1 | API Key |
| **Together AI** | Llama, Qwen, Mistral, Mixtral, DeepSeek | API Key |
| **DeepInfra** | Llama, Qwen, DeepSeek, Mistral | API Key |
| **Cerebras** | Llama 3.1/3.3, Qwen | API Key |
| **NVIDIA NIM** | Llama 3.x, Nemotron family | API Key |
| **HuggingFace** | Inference-API hosted models | API Token |
| **Moonshot AI** | Kimi K2, moonshot-v1 | API Key |
| **Nebius** | Llama, Qwen, DeepSeek | API Key |
| **Scaleway** | Llama, Mistral | API Key |
| **STACKIT** | Llama 3.1 | API Key |
| **Baseten** | Llama, Qwen, DeepSeek | API Key |
| **Cortecs** | Llama, Qwen, Mistral | API Key |
| **302.AI** | OpenAI-compatible aggregator | API Key |
| **ZAI** | OpenAI-compatible aggregator | API Key |
| **OpenRouter** | 200+ models from every major provider | API Key |
| **LMStudio** | local OpenAI-compatible server | Local &mdash; no key needed |
| **Chutes** | OpenAI-compatible aggregator | API Key |
| **MiniMax** | minimax-text, abab models | API Key |

Configure priority chains. Automatic failover when one hits a rate limit. Per-provider budgets. Cost tracking per session. Zero-cost local inference with Ollama or LMStudio. **No IDE spoofing, no scraped credentials.** Every provider implementation either uses a documented public API or identifies as Anvil honestly in headers.

---

## Quick Start

```bash
anvil                               # Start interactive session
/remote-control                     # Share via browser
/model claude-opus-4-7              # Switch model
/provider anthropic                 # Switch provider
/vault add                          # Store a credential
/ssh myserver                       # Open SSH tab
/productivity                       # Session stats
/mcp list                           # MCP server status
/fork experiment                    # Branch the conversation
/focus                              # Distraction-free mode
/export md                          # Export as Markdown
```

---

## Also in the box

**120+ slash commands** (including the new `/cursor` command tree and `/memory clean` / `/cursor stream` / `anvil agents` cross-session monitor). **35 AI providers.** 45 built-in tools. MCP integration. Per-tab parallel inference. SSH tabs. Tool-call cards with Ctrl+O expand. Multi-tab sessions. Git integration. Code productivity dashboard. Session history search. 37-widget configurable status line with 16 presets. Vim keybindings. Focus view. File sandbox with permission modes. 7-language i18n. AnvilHub marketplace for skills, plugins, agents, and themes. Web UI with full configuration parity. First-run setup wizard. CC&rarr;Anvil migration (`anvil import claude-code`). anvil(1) manpage. All of it optional. None of it required.

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





### v2.2.19 &mdash; May 22, 2026

**18 Languages. Memory Cohesion Complete. The Web-Based MCP Builder.**

- &#10003; **Internationalization &mdash; 18 locales** &mdash; the TUI, wizard, slash-command output, and remote-control viewer all flow through `rust-i18n` v4 in Rust and the new `viewer.locales.js` runtime in the browser. Tier-1 ships English, Spanish, Simplified Chinese, French, Brazilian Portuguese, Russian, Japanese, German &mdash; 264 keys each. Tier-2 adds Korean, Italian, Turkish, Vietnamese, Polish, Indonesian, Dutch, Swedish, Norwegian Bokm&aring;l, Ukrainian. Locale selection persists to `~/.anvil/config.json`, falls back to `$LANG` on first launch, applies immediately to every wizard step. `/configure` menu has a Language Picker submenu rendering native scripts (&#54620;&#44397;&#50612;, &#1056;&#1091;&#1089;&#1089;&#1082;&#1080;&#1081;, &#20013;&#25991;). Viewer ships 176 fully-wired keys covering chrome plus `vault.*` and `config.*` panels; live re-render walker on locale switch with no page reload.
- &#10003; **Seven-layer memory &mdash; all GREEN** &mdash; the 2026-05-21 cohesion audit found Layer 3 (Semantic), Layer 6 (Reflective), and Layer 7 (Cache) still partial. v2.2.19 closes all three. **Layer 3:** `/memory promote <nomination-id>` now actually persists nominated facts to disk &mdash; the v2.2.14 stub flipped a status flag without ever calling `MemoryManager::save()`. Full chain writes the fact and appends provenance comments before marking the nomination accepted. New `--target <file>` flag. **Layer 7:** file-cache path-discovery bug fixed; `memory_budget` no longer checks a path that doesn&rsquo;t exist in the project-scoped layout. `/memory show cache` enumerates file-cache, command-cache, and QMD-cache stats. `/memory prune cache --dry-run` walks both `FileCacheManager` and `CommandCacheManager`. **Layers 1, 2, 4:** `/memory layer 1` renders a live snapshot via `PromptSectionsExt::iter_by_kind()`. `/memory show episodic` unifies daily summaries, history archives, workspace sessions. `/memory prune episodic` adds TTL retention with a trash-bin safety net. `/memory show procedural` consolidates GoalManager + skills + CronManager.
- &#10003; **AnvilHub `/build` page + `anvil-mcp-builder` micro-service** &mdash; the v2.2.18 `/mcp builder` TUI wizard now has a web counterpart. Three endpoints: `POST /api/builder/spec` (LLM-generated spec from free-text, SSE-streamed), `POST /api/builder/generate` (turns spec into a base64 tarball &mdash; Node.js, TypeScript, or Python templates), `POST /api/builder/sandbox` (extracts the tarball and runs `anvil-sandbox-runner` network-cut). Operator OAuth token loaded from the Anvil vault at startup, never from `.env`. Sandbox endpoint gated on publisher standing &mdash; user must be in the `anvilhub-publishers` Authentik group OR have at least one HubPackage published. 5-minute Map cache; falls closed on backend error.
- &#10003; **MCP pagination (CC v2.1.144-B6 / v2.1.146-B2)** &mdash; MCP client now consumes the full `nextCursor` / `has_more` pagination chain for `tools/list`, `resources/list`, `resources/templates/list`, `prompts/list`. Previously MCP servers with paginated responses had everything beyond page 1 silently dropped.
- &#10003; **Spinner/elapsed-time freeze fix (CC v2.1.145-B3)** &mdash; TUI render queue wakes from a wall-clock timer in addition to input events. After terminal refocus or resize, the spinner and elapsed-time display no longer freeze until next keypress. New `RedrawReason::TerminalStructural` routes Resize/FocusGained/FocusLost through the soft draw path (no ANSI clear flash, per the photosensitivity rule from v2.2.17).
- &#10003; **MCP `permissions.allow` honored (CC community #61077 SECURITY)** &mdash; `permissions.allow` rules with patterns like `mcp__server__tool` or `mcp__server__*` are now consulted at MCP tool dispatch time. Previously the allowlist was loaded but the MCP dispatch path bypassed it and always prompted.
- &#10003; **Bash env-var permission bypass (CC v2.1.145-B1 SECURITY)** &mdash; Bash patterns of the form `KEY=VALUE command` are decomposed and the command portion checked against the allowlist.
- &#10003; **Skill fork-context recursion guard (CC v2.1.145-B4)** &mdash; a skill cannot transitively invoke itself; recursion check uses the full skill ancestry chain.
- &#10003; **Resume session model preservation (CC community #61068)** &mdash; `anvil --resume` restores the model that was active when the session was saved, not the global default.
- &#10003; **CC parity P2 sweep** &mdash; API startup 15s timeout for side-channel calls (#722), mime-type magic-number fallback in `Read` (#723), `/branch` history recovery post-EnterWorktree via `worktree_ops::original_sessions_dir()` (#724), MCP image fallback for unsupported MIME &mdash; saves to `~/.anvil/mcp-images/<sha256>.<ext>` (#725), skill watcher FD exhaustion prevention &mdash; excludes `target/`, `node_modules/`, `.git/`, `dist/`, `build/`, `.cache/`, `.next/`, `__pycache__/` (#726), theme color reset on first `/session rename` fixed (#727), EnterWorktree MCP config preservation via snapshot before `set_current_dir` (#728).
- &#10003; **`anvild` separate process name across 7 platforms (#766)** &mdash; the background OAuth-refresh + routines daemon now runs as `anvild`, not `anvil daemon foreground`. New `anvild_path_from(anvil_binary)` helper rewrites the binary path used by every supervisor unit &mdash; macOS LaunchAgent, Linux user-systemd, FreeBSD/NetBSD rc.d, Windows Task Scheduler &mdash; plus the in-TUI `daemon::spawn_detached` fallback. `ps -ef | grep daemon` now shows `anvild`. `install.sh` and `install.ps1` create the `anvild` symlink (Unix) or hardlink (Windows NTFS) alongside the main binary.
- &#10003; **Wizard P0 bundle &mdash; Ollama modal clipping, daemon prompt alt-screen, vertical-split hint (#767, #768, #769)** &mdash; `ConfirmModal` height now derived from body wrap so Yes/No buttons stay visible regardless of body length; new `Ctrl+B` Back keybind. The &ldquo;Install anvild as background service?&rdquo; prompt moved into the alt-screen as Step 8 of 9 (no more drop-to-CLI mid-wizard). New 1-line hint above BUILD section in the rail bottom group: `&#8997;+drag deck only` on macOS, `Alt+drag deck only` on Linux/Windows/BSD. i18n keys added across all 18 locales.
- &#10003; **Release-pipeline step-gates (#714, #730)** &mdash; `scripts/release.sh` now wraps every phase in `step "PN: <description>"` + `ok "PN"` / `fail "PN"` markers. New `scripts/release-helpers/step-gates.sh` provides primitives + JSON status persistence + an EXIT-trap silent-exit detector. Closes the v2.2.18 Phase 6 silent-exit class. `scripts/test-release-gates.sh` runs release.sh in `--dry-run` and asserts every expected phase fires START + terminal marker exactly once.
- &#10003; **Net +50 tests across the workspace** &mdash; i18n drift gate + picker invariant + 8 locale-load; +9 episodic / +6 promote / +6 cache / +5 working / +3 procedural for memory cohesion; +35 across all 15 CC-parity fixes; +7 publisher-standing tests in the new micro-service.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.18 &mdash; May 20, 2026

**The Web Viewer Lands. Autocompact Gets Honest. Mouse Capture Off by Default.**

- &#10003; **Web viewer &mdash; full TUI parity (#680, #681, #683, #692, #695, #696)** &mdash; `passage` relay and AnvilHub viewer get a tab-routing rewrite matching the TUI&rsquo;s per-tab architecture. `/tab new`, `/tab rename`, `/tab switch`, `Ctrl+T`. Per-tab `user_message` and `slash_result` routing (no more cross-tab leakage). Default layout `vertical_split + tabs`. Cost-type chip (OAuth / local / cloud) instead of fabricated dollar figures. Cached + broadcast `MemorySnapshot` so the memory rail populates on reconnect. `SessionMeta` carries `context_max` and `build_sha`. Default-allow forwarding for unhandled viewer messages. Collapsible tool cards. Always-visible slash bar with `Cmd+K` palette.
- &#10003; **Autocompact threshold fix (#697 CRITICAL)** &mdash; `maybe_auto_compact` was measuring against `max_output_tokens` (8K&ndash;16K) instead of `context_window` (64K&ndash;200K+). Sessions on long-context models were compacting at roughly 6K input tokens. Now reads `session.context_window` and ignores `max_output_tokens` entirely. New OTel span `anvil.autocompact.threshold` emits `context_window`, `used_tokens`, `threshold_pct`, `triggered`. `/compact why` prints the full threshold calculation.
- &#10003; **Mouse capture default OFF (#696 P4)** &mdash; mouse capture disabled by default on all platforms, restoring terminal copy-paste (Cmd+C / Ctrl+Shift+C / Ctrl+C) for users who hadn&rsquo;t opted in. One-time first-run toast shows `/config mouse_capture true` and `--mouse`. `mouse_capture_default_off_regression` test asserts the default at the type level.
- &#10003; **Bracketed paste in textarea modals (#685)** &mdash; multi-line paste now works inside `/mcp builder` and other wizard textareas. Wires `tui::paste::handle_paste` into the textarea event loop with `\r\n` &rarr; `\n` normalization.
- &#10003; **Per-tab relay routing (#696)** &mdash; `relay::user_message` and `relay::slash_result` route to active `Tab.id` rather than broadcasting. Concurrent inference in two tabs no longer leaks between them.
- &#10003; **OAuth / local / cloud cost label (#696 P1)** &mdash; TUI status footer shows a semantic cost-type chip instead of a fabricated dollar amount for providers where per-token cost is not knowable.
- &#10003; **`MemorySnapshot` rail parity (#695)** &mdash; vertical-split rail uses `layouts::common` helpers instead of a hand-rolled draw path. Same fidelity as the classic inline view.
- &#10003; **Alt-screen raw mode restore (#688)** &mdash; `restore_alt_screen` re-enables raw mode on return from inline operations. Was the root cause of &ldquo;keyboard stops working after `/mcp builder` cancel&rdquo; in v2.2.17.
- &#10003; **`FORCE_FULL_REDRAW` consumption (#688)** &mdash; consumed inside `handle_repl_command_tui` so the blank-screen-after-cancel regression cannot recur.
- &#10003; **Mouse capture + alt-screen pairing (#688)** &mdash; mouse capture state is paired with alt-screen state. Enabling mouse capture outside the alt-screen no longer leaves the terminal inconsistent after exit.
- &#10003; **Force full redraw after inline-op restore (#687)** &mdash; any inline operation that restores the alt-screen forces a full redraw. Eliminates partial-frame artifacts.
- &#10003; **Textarea keybinds corrected (#686)** &mdash; `Enter` submits, `Ctrl+N` inserts a newline. Previously inverted.
- &#10003; **`/mcp builder` long-description textarea (#684)** &mdash; long-description field is now a multi-line textarea modal instead of a single-line input.
- &#10003; **PermissionPrompt round-trip regression test (#677)** &mdash; end-to-end test fires a tool call that requires a permission prompt, verifies the prompt renders, sends the approval, asserts the turn completes. Guards permission-prompt state from desyncing with the turn loop.
- &#10003; **Release-pipeline Phase 6 silent-exit guard (#654)** &mdash; `scripts/release.sh` Phase 6 wraps every SSH hop in an explicit `|| { echo "Phase 6 SSH failed"; exit 1; }` guard. Previously a failed remote call could terminate the script with exit 0, leaving subsequent surface updates unrun.
- &#10003; **anvil-release MCP host targeting fix (#698 CRITICAL)** &mdash; `anvilhub_pm2_host` reverted to `dev0001` after `#655` incorrectly routed pm2 ops to CT 113 (which is dead). Apache vhost has always proxied `anvilhub.culpur.net` to `dev0001:3100`.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.17 &mdash; May 18, 2026

**The Setup Wizard, Reflection, Sandboxing, and the Source Viewer.**

- &#10003; **New first-run wizard** &mdash; welcome card &rarr; nine modal steps &rarr; vault unlock &rarr; main TUI, all in one alt-screen. Per-step descriptions explain why each question is asked. OAuth waits poll on 100&nbsp;ms and stream the elapsed counter live. Step 9 is CC migration in-modal. Brighter grey font for modal text.
- &#10003; **Autonomous reflection loop** &mdash; stuck-detector switches strategy and writes a multi-attempt scratchpad when a turn loops without progress.
- &#10003; **`anvil-sandbox-runner` companion binary** &mdash; hub-install detonation runs in an isolated binary, shipped alongside `anvil` on all seven platforms.
- &#10003; **AnvilHub source viewer** &mdash; every one of the 558 HubPackages has a viewable source archive; 547 packages got synthesized `Documentation` tabs from DB columns.
- &#10003; **Vertical-split rail-stays-painted fix (#648)** &mdash; ratatui `swap_buffers` contract violation in the `#574` region-gated repaint surfaced as blank/garbage rails after wizard exit. All three layouts now always paint every region every frame.
- &#10003; **TUI flash eliminated on Gnome Terminal/alacritty (#622, #629)** &mdash; full-screen `Clear()` is now gated on `DirtyRegions::ALL` and `commit_pending_redraw` no longer routes `TextDelta` through `terminal.clear()`. Photosensitivity hazard during streaming output resolved.
- &#10003; **Wizard mouse-capture default OFF (#623)** &mdash; native text selection now works cross-platform. Banner is no longer Mac-only.
- &#10003; **`/agent compose` + `/agent traits` rewired (#624)** &mdash; no more `println!` corrupting the alt-screen. 23 BUG sites fixed in the broader println audit (#626), with `#![deny(clippy::print_stdout, clippy::print_stderr)]` on every TUI-touching file and a regression test to block future drift.
- &#10003; **In-TUI ConfirmModal + PasswordModal** &mdash; vault unlock for returning users is now a modal in the existing alt-screen, not a CLI prompt; ConfirmModal supports two-button destructive-action confirmation.
- &#10003; **Vertical-split Shift+drag deck-only selection (#625)** &mdash; rail no longer comes along when you select conversation text.
- &#10003; **Vertical-split rail keybinds wired (#634)** &mdash; g / d / s / a / Ctrl+R now work in the rail, with a drift gate to keep them wired.
- &#10003; **Reactive compaction sizes from actual overflow (#564)** &mdash; summary-size budget is now seeded from the real overflow delta, not a fixed guess.
- &#10003; **`ANVIL_STOP_HOOK_BLOCK_CAP` (#566)** &mdash; caps Stop-hook blocking to prevent infinite-loop runaway if a hook mis-fires.
- &#10003; **Session auto-titling wired (#580)** &mdash; `derive_title_from_first_message()` now actually drives the trigger.
- &#10003; **Hook PWD refresh after worktree switch (#561)** &mdash; PWD-relative hooks no longer go stale when `EnterWorktree` runs.
- &#10003; **Welcome banner names active provider for 3P users (#562)** &mdash; no more hardcoded Anthropic when you're on Groq/Bedrock/etc.
- &#10003; **`release-surfaces.yaml` enforcement gate (#614)** &mdash; one manifest is the single source of truth for every public surface; `scripts/verify-release-surfaces.sh` is the gate.
- &#10003; **AnvilHub `/sha256/2.2.17.txt` published** &mdash; out-of-band checksum manifest for primary-source SHA256 verification.
- &#10003; **`install.sh` + `install.ps1` rebuilt** &mdash; live versions on `anvilhub.culpur.net` updated to the proper `/api/version`-aware variants. Fixes the `tag_name` regex breakage (#619) and the hardcoded `windows-msvc` Windows target (#612).
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.16 &mdash; May 17, 2026

**The TUI Layout System. Eight live-switchable layout variants on a per-tab `TuiLayoutConfig`. New default: Vertical Split + Tabs.**

- &#10003; **TUI Layout System** &mdash; four layout architectures (Vertical Split / Classic / Three-Pane / Journal) &times; tabs/no-tabs = eight variants, every one a real renderer. Per-tab `TuiLayoutConfig`. `/layout list`, `/layout <alias>`, `/layout <alias> --global` (writes to `~/.anvil/config.json`), `/layout reset`. State-machine contract enforced by integration tests; shared session state (log, input, model) survives the switch; terminal clear on switch so previous cells don't bleed.
- &#10003; **New default: Vertical Split + Tabs** &mdash; persistent left rail (sessions, agents, tools, MEMORY, gate state) next to a swappable right deck. Rail owns all chrome (banner, status, model, cost); deck has input only. Mouse-draggable split anchor. Migration-safe: users with an explicit `tui_layout` setting keep their value; only upgraders without the key see the change, plus a one-time intro toast.
- &#10003; **Wizard step 7 highlight** &mdash; first-run setup wizard now defaults to Vertical Split (option `[1]`); Classic moves to `[2]`. New installs that just press Enter land on the rail+deck view.
- &#10003; **Slash-completion popup wired into all renderers** &mdash; completion works regardless of which layout is active. Every render path also redraws on every keystroke so input is live.
- &#10003; **Three-pane Insert discoverability** &mdash; framed hint + ghost input makes the always-on input legible; CONTEXT band uses `Constraint::Fill` so it actually fills available height. Vim modal removed &mdash; typing always edits the input.
- &#10003; **Paste handling rebuild** &mdash; consolidated paste handler routes terminal bracketed paste, OSC52, and drag-and-drop file paths. Mouse capture is off by default so native terminal copy works. `ContentBlock::Document` is wired end-to-end for PDF and Office docs &mdash; dropping a `.pdf` attaches it as a document block to the next request. Long-paste placeholder collapses multi-KB pastes to a `[Pasted N chars]` token in history. Keystroke-burst detection converts drag-and-drop on terminals without OSC52 into a single paste.
- &#10003; **Ctrl+C mid-stream cancel across all 7 providers** &mdash; `DefaultRuntimeClient` honors the cancel token in every provider implementation. `tokio::select!` wraps the blocking HTTP read so the connection actually closes when the token fires. New wiremock integration test covers the cancel path end-to-end.
- &#10003; **Vertical-split rail polish** &mdash; uppercase section headers, cross-tab status aggregates, tool-call boxes close cleanly, markdown styled, cost rendered to 2 decimals, QMD folded into MEMORY, agent tab-binding, split-anchor draggable.
- &#10003; **AnvilHub verification gate** &mdash; `/hub status` ships all 8 axes, `HubPackage` carries verified-badge structs, `require_verified` config gate, `/plugin update REVOKED` guard, update probe prefers anvilhub `/api/version` and falls back to GitHub Releases.
- &#10003; **TUI correctness long tail** &mdash; `/vault unlock` retries up to 3 times and pre-fills the prompt on failure; welcome banner names the active provider, not hardcoded Anthropic; session-title heuristic skips a bare URL as first message; 5xx errors name the configured provider/gateway; spinner color warms warm-green&rarr;amber&rarr;red on elapsed time; `Read` offset accepts string forms with whitespace/`+` prefix; `ANVIL_MCP_TOOL_TIMEOUT` env override per request; async `fetch_all_configured_models` with timeout + Ctrl+C cancel; strict RFC 6749 token-exchange parser + startup validator; lenient scopes deserializer prevents OAuth lockout; `ProviderLoginModal` for in-TUI OAuth/API-key flows; layout-switch terminal clear prevents stale-cell ghosting.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.15 &mdash; May 15, 2026

**The largest release in Anvil's history. 121 commits over v2.2.13. Supersedes the corrupted v2.2.14 tag (never published).**

- &#10003; **Provider catalog 5 &rarr; 31** &mdash; AWS Bedrock (manual SigV4, no AWS SDK), Google Gemini OAuth + Antigravity (Code Assist OAuth with honest Anvil identification &mdash; not VS Code spoofing), GitHub Copilot (device flow), Azure OpenAI (deployment-name + api-version), Cursor Cloud Agents (repo-bound), plus 24 OpenAI-compatible providers: Groq, Fireworks, Mistral, Perplexity, DeepSeek, Together AI, DeepInfra, Cerebras, NVIDIA NIM, HuggingFace, Moonshot, Nebius, Scaleway, STACKIT, Baseten, Cortecs, 302.AI, ZAI, OpenRouter, LMStudio, Chutes, MiniMax, OpenCode, OpenCode-Go.
- &#10003; **`/cursor` first-class command tree** &mdash; six TAB-completable subcommands (`launch`, `list`, `get`, `cancel`, `artifacts`, `stream`) for Cursor's public Cloud Agents API. Repo-bound by design &mdash; `git remote get-url origin` is mandatory.
- &#10003; **`/model` cross-provider unified picker** &mdash; one TAB-completable list across every configured provider, provider-prefixed (`anthropic/claude-4.5-sonnet`, `groq/llama-3.3-70b`, `cursor/claude-4-sonnet-thinking`). Atomic switch updates routing + system prompt identity + TUI chrome together. Live-fetched from each provider's `/models` endpoint; static registry is fallback only.
- &#10003; **Memory Cohesion Arc** &mdash; the seven-layer memory system from v2.2.5 (Sensory / Working / Episodic / Semantic / Procedural / Reflective / Long-term) cohesion-tested end-to-end. Retrieval-order block in system prompt, `WorkingMemorySnapshot` as `Vec<PromptSection>`, `PermissionMemory` wired into permission gate, `/file-cache` real handler, egress allowlist in settings + `/policy view`, `/memory why` injecting daily summaries.
- &#10003; **Capability Cohesion Arc** &mdash; every Anvil capability now meets an 8-axis contract (definition / registration / completion / handler / dispatch / rendering / permission gate / OTel + tests). Build-time drift gate enforces it. Slash dispatch unified at one site. Stub messages banned.
- &#10003; **CC&rarr;Anvil migration** &mdash; `anvil import claude-code` brings prior assistant work forward verbatim with provenance frontmatter. Memory entries get `imported_from` stamps. Project instruction files merged without clobbering. Settings get conflict detection. Skills get collision handling. Past sessions (up to ~1,800 JSONL, ~1&nbsp;GB) optionally summarized into daily records. The day-2 command `anvil memory clean` rewrites entries through a configurable LLM and detects duplicate meanings via Jaccard similarity. First-run wizard offers migration as opt-in.
- &#10003; **CC parity catch-up v2.1.132 &rarr; v2.1.139** &mdash; 17 features + 7 security/verify items: `ANVIL_PROJECT_DIR` / `ANVIL_SESSION_ID` / `ANVIL_DISABLE_ALTERNATE_SCREEN` / `ANVIL_EFFORT` env propagation, transcript view nav, cross-session `anvil agents` live monitor, `worktree.baseRef`, `autoMode.hard_deny` short-circuit, hook `continueOnBlock`, hook args `string[]` exec form, `/scroll-speed` with live preview, `anvil plugin details`, subagent OTel headers with parent-agent-id, plus security gates on `--resume`/`--continue` underscore paths, plan-mode + Edit allow rule write blocking, MCP content-block result visibility, `Skill(name *)` wildcard delimiter check, settings hot-reload on symlinks, stream-idle false-fire elimination, multi-image paste correctness.
- &#10003; **Token economy + reliability** &mdash; file-fingerprint cache wired into `read_file`/`write_file`/`edit_file`/system prompt (shipped dormant in v2.2.11). Command-output cache wired into `glob_search` and `grep_search`. WebFetch + WebSearch get 5-min and 1-hour TTL caches. Skill-chaining engine depth-3 wired (suggestion engine; executor lands in v2.2.16). Auto-promote engine for `/memory` notes active.
- &#10003; **Honesty contract codified** &mdash; test-suite-enforced contracts: no silent deferral, 8-axis capability contract, CC-only naming, changelog preservation (historical entries byte-immutable), migration instinct (bring everything verbatim-with-flag), live-model-list (not registry), atomic provider/model switch.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.14 &mdash; (internal-only, never publicly released)

v2.2.14 was tagged internally but never published as binaries due to a release-pipeline incident. All v2.2.14 work is included in v2.2.15 above (Memory Cohesion Arc, Capability Cohesion Arc, CC parity v2.1.132-139, per-tab parallel inference fixes, file-fingerprint cache wiring, auto-promote engine).

### v2.2.13 &mdash; May 11, 2026

**Windows is back, BSD joins, routines on disk** &mdash; seven platforms now.

- &#10003; **FreeBSD x86_64 + NetBSD x86_64 binaries** &mdash; first-ever BSD support. Every binary SHA256-verified and signed by the release pipeline, with paired `.sha256` manifests at [anvilhub.culpur.net/sha256/](https://anvilhub.culpur.net/sha256/).
- &#10003; **Windows x86_64 is back** &mdash; the v2.2.12 hold is fixed. ssh-agent auth is now `#[cfg(unix)]`-gated with a clean Windows stub. The rest of the SSH driver (key-file, password, kbd-interactive) works on Windows exactly as on Unix.
- &#10003; **Seven platforms total** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64.
- &#10003; **Release pipeline hardening** &mdash; build errors now hard-fail instead of silently producing partial releases (the v2.2.12 incident where the Windows build failed silently and a stale artifact was published).
- &#10003; **Routines foundation on disk** &mdash; schedule grammar (duration, interval, cron, ISO timestamp), output archive with `[SILENT]` early-stop, and SHA-256 input-hash packet schema. 63 new tests. The v2.2.14 daemon ships on top.
- &#10003; FreeBSD ARM64 and OpenBSD x86_64 are not in this release &mdash; the Rust toolchain does not publish a precompiled standard library for either target. Both are queued for v2.2.14.
- &#10003; 1,146 workspace tests passing across every crate. Zero failures. Zero warnings.

### v2.2.12 &mdash; May 11, 2026

**Parallel and Transparent** &mdash; the app gets out of your way.

- &#10003; Per-tab parallel inference &mdash; each tab owns its own `Arc<Mutex<ConversationRuntime>>` and runs turns on dedicated worker threads; fire prompts in multiple tabs simultaneously
- &#10003; Mid-turn TUI responsiveness &mdash; Ctrl+T, F2/F3, `/ssh`, and cross-tab submit all respond immediately during streaming; the app is interactive throughout a turn
- &#10003; Tool-call cards &mdash; every Glob/Grep/Read/Write/Edit/Bash/WebSearch/MCP call renders a bordered card with actual input (pattern, path, command) the moment it fires; Ctrl+O expands to full JSON + result
- &#10003; SSH tabs &mdash; `/ssh host` opens a modal connection form with russh backend, vt100 rendering, Ctrl+B prefix keys; connections saved as vault `HostCredential` aliases
- &#10003; Tab bar markers &mdash; `*` (unread), `&#9888;` (pending permission), `&#215;` (clickable close); terminal-friendly navigation (F2/F3/Ctrl+arrow/Alt+digit/click)
- &#10003; Session continuity &mdash; `anvil --continue` honors saved model from `.meta.json` sidecar; Ollama sessions reconnect without credential errors; exit prints resume commands
- &#10003; Scrollback fix &mdash; HISTORICAL VIEW was showing only 1&ndash;4 chars per assistant line; pending text growth now invalidates cached scrollback line vectors
- &#10003; `/quit` no longer deadlocks &mdash; self-recursive mutex in `record_daily` fixed
- &#10003; First-run setup wizard &mdash; mouse capture, theme, permission mode opt-ins on first launch; `anvil setup` / `anvil --first-run` to reconfigure
- &#10003; anvil(1) manpage ships with Homebrew installs
- &#10003; `/clear` clears workspace context across all tabs, not just the active one
- &#10003; Release pipeline hardening &mdash; tag-vs-HEAD pre-flight, build-from-tag, php-lint guard, changelog.json render-time injection on AnvilHub
- &#10003; 318 tests passing, ~22 MB binary

### v2.2.11 &mdash; May 9, 2026

**Outweigh-them-all release** &mdash; self-awareness plus ten core surfaces in one cut.

- &#10003; System prompt now leads with "You are Anvil v2.2.11" and references the currently loaded model + provider in every turn &mdash; no more hallucinating a different identity
- &#10003; **W1 hook events:** PreToolUse, PostToolUse, UserPromptSubmit, SessionStart, SessionEnd, PreCompact, Notification &mdash; full CC parity
- &#10003; **W2 effort slider:** `/effort low|medium|high` &mdash; tune reasoning depth per turn, persisted per session
- &#10003; **W3 goal persistence:** per-session goals survive `/clear` and reconnect, surfaced in status line
- &#10003; **W4 named profiles:** save and switch (provider, model, effort, output style) tuples by name
- &#10003; **W5 published JSON schema:** `settings.json` fully typed, IDE-completable, served at `anvilhub.culpur.net/schema/settings.json`
- &#10003; **W6 OpenTelemetry events:** `OTEL_EXPORTER_OTLP_ENDPOINT` support, permission_decision + tool_call + token_usage spans
- &#10003; **W7 custom output styles:** define your own `/output-style` names in settings, ship them as plugins
- &#10003; **W8 reviewer-agent approval gate:** optional second-agent review before file writes, configurable threshold
- &#10003; **W9 anvil mcp-server mode:** run Anvil itself as an MCP server &mdash; expose agents and tools to any MCP client
- &#10003; **W10 requirements.toml admin policy floor:** enforce minimum versions, required plugins, denied domains org-wide
- &#10003; Rename `CLAUDE.md` &rarr; `ANVIL.md` across user-facing strings + the anvil-md-curator skill
- &#10003; Build-time fix: `cargo:rerun-if-changed` now watches the actual ref file, not just `.git/HEAD` &mdash; GIT_SHA stays current across rebuilds

### v2.2.10 &mdash; May 6, 2026

**TUI usability patch.**

- &#10003; TUI: long lines wrap instead of right-truncating
- &#10003; TUI: native terminal selection restored (Shift-drag works again)
- &#10003; TUI: tool-result summaries now actually summarize
- &#10003; Release pipeline: regenerate sha256 manifests every build + verify-before-release
- &#10003; Release pipeline: fix repo target on `gh release` calls

### v2.2.9 &mdash; May 6, 2026

**Claude Code parity catch-up.**

- &#10003; Claude Code parity: `--print`/`--agent` honor frontmatter, plugin prune, scroll snap
- &#10003; Subagent summaries, `/mcp` tool count, API 400 error surfacing
- &#10003; OTEL env vars, MCP reconnect summary, worktree HEAD detection
- &#10003; Spinner red on errors, theme refresh, env vars (`DISABLE_UPDATES`, `HIDE_CWD`, `EFFORT`, `AI_AGENT`)
- &#10003; Long URL clickability, `/clear` tab title cleanup, editor handoff hardening

### v2.2.8 &mdash; April 22, 2026

**PAI-inspired composition, learning, and robustness.**

- &#10003; `/agent compose <traits...> "<task>"` &mdash; trait-based agent composition engine, 30-trait catalogue (expertise &times; personality &times; approach), dimension-conflicts hard-error by default. Adapted from Miessler's `Personal_AI_Infrastructure`.
- &#10003; Skill front-matter `triggers` with suggest-not-auto UX &mdash; three bundled reference skills (`security-audit`, `code-review`, `terse`). Never auto-inject (prompt-injection vector); user confirms via `/skill load <name>`.
- &#10003; Prompt-type hooks &mdash; plugin lifecycle hooks can now inject a string into the next model turn with `{tool_name}` / `{cwd}` / `{date}` / `{model}` interpolation. Backward-compatible with bare-string command hooks.
- &#10003; `anvil skill-eval` &mdash; three-arm evaluation harness (`__baseline__` / `__terse__` / `<skill>`) with honest caveats baked into every report. Adapted from `caveman`.
- &#10003; `/output-style precise|condensed` &mdash; user-selectable global response style. Precise (default) preserves full sentences; condensed activates the bundled `terse` skill. **Never auto-applies condensed** &mdash; Auto-Clarity rules still fire for security / irreversible / multi-step / consent contexts even when condensed.
- &#10003; Plugin loader is forward-compatible &mdash; a single bad manifest no longer crashes the entire binary. `PluginLoadDiagnostic` surfaces per-plugin warnings on stderr.
- &#10003; Bundled plugins are now embedded in the binary via `include_dir` &mdash; Homebrew users' bundled plugins are visible; developers' installed binaries no longer depend on their live source tree.
- &#10003; Claude-Code-parity bug fixes: 429 `Retry-After` minimum; 5-min stream dead-air timeout; configurable request timeout (`ANVIL_API_TIMEOUT_MS`); `/model` warns on mid-conversation switch; DangerFullAccess stability invariants.
- &#10003; 756 tests passing.

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
- &#10003; Remote-control 503 fixed &mdash; relay WebSocket process declaration corrected
- &#10003; Release pipeline: per-binary embedded-version audit gate &mdash; makes the v2.2.6 Windows-exe-labeled-as-2.2.1 class of bug impossible
- &#10003; 618 tests passing, zero warnings

### v2.2.6 &mdash; April 20, 2026

**Command Parity, Deep Autocomplete, Web Config, AnvilHub Installer.**

- &#10003; 17 web config panels &mdash; vault, notifications, SSH, Docker/K8s, memory, and more
- &#10003; Full Status Line editor in browser &mdash; 36 widgets, 16 presets, drag-and-drop, live preview
- &#10003; AnvilHub installer &mdash; search, install, restart prompt &mdash; vault-gated, telemetry-tracked
- &#10003; Deep hierarchical autocomplete &mdash; `/vault store <Tab>` &rarr; 21 credential types
- &#10003; 8 previously-broken TUI handlers now working &mdash; `/mcp`, `/plugins`, `/session`, `/daily`, and more
- &#10003; New commands &mdash; `/tab`, `/fork`, `/share`, `/audit`, `/restart`
- &#10003; Self-respawn on macOS/Linux after plugin installs

### v2.2.5 &mdash; April 19, 2026

**Intelligent Memory System &mdash; six major features.**

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

**Edition 2024, dependency modernization, Claude Code parity.**

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

**Live streaming responses, thinking status indicator, remote control.**

- &#10003; Live streaming responses &mdash; real-time token-by-token rendering
- &#10003; Remote control &mdash; `/remote-control` to share sessions via browser
- &#10003; Thinking mode &mdash; `/think` enables extended reasoning

### v2.1.0 &mdash; April 8, 2026

**Encrypted vault, file sandbox, modular architecture** &mdash; security-first release.

### v2.0.0 &mdash; April 8, 2026

**Full Claude Code Parity** &mdash; multi-agent system, TUI tabs, context management.

### v1.0.4 &mdash; April 7, 2026

Multi-agent system &mdash; 7 agent types with task orchestration.

### v1.0.3 &mdash; April 7, 2026

VS Code extension, 21 new features, credential vault, 86 commands.

### v1.0.2 &mdash; April 7, 2026

Internationalization &mdash; 7 languages, 20 features.

### v1.0.1 &mdash; April 3, 2026

Cross-compilation CI pipeline &mdash; 5-platform builds, theme system, QMD documentation.

### v1.0.0 &mdash; April 2, 2026

**Initial release.** Terminal-native AI coding assistant with credential vault and TUI.

---

<div align="center">

**Built by [Culpur Defense](https://culpur.net)** &#8226; **[AnvilHub](https://anvilhub.culpur.net)** &#8226; **[Product Page](https://culpur.net/anvil/)**

</div>
