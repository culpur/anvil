<div align="center">

<br>

# &#9874; Anvil

### The only AI coding assistant that doesn't lock you in.

[![Version](https://img.shields.io/badge/version-2.2.15-0FBCFF?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![Platform](https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows%20%7C%20BSD-lightgrey?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![31 AI Providers](https://img.shields.io/badge/31%20AI%20Providers-00D084?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![License](https://img.shields.io/badge/proprietary-1e293b?style=for-the-badge&labelColor=0a0f1e)](LICENSE)

**Your providers. Your credentials. Your data. Your cost.**<br>
**31 AI providers, one terminal. Switch freely. Own your workflow.**

[**Download**](https://github.com/culpur/anvil/releases/latest) &bull; [**AnvilHub**](https://anvilhub.culpur.net) &bull; [**Changelog**](#changelog) &bull; [**Product Page**](https://culpur.net/anvil/)

<br>
</div>

---

## Why Anvil?

Other AI coding assistants come with a leash. One vendor's pipe, one vendor's pricing, one vendor's rate limits — and when that vendor changes something you don't like, you're stuck. Your code, your data, your costs all flow through infrastructure you don't control.

**Anvil is the inverse.** Pick your provider. Use your own API keys, or run everything locally through Ollama. Switch between models mid-conversation. When one hits a rate limit, fall over to the next. When one gets expensive, change it. When the provider does something you don't like, leave.

No account required. No telemetry. No lock-in. A single ~24&ndash;42 MB binary, zero dependencies, **31 providers, seven platforms**.

---

## What you keep control of

| | |
|---|---|
| &#128273; **Your providers** | 31 providers including Anthropic (Max-plan OAuth supported), OpenAI, Google Gemini (Code Assist OAuth), AWS Bedrock (manual SigV4, no AWS SDK), Cursor Cloud Agents, GitHub Copilot, Azure OpenAI, Ollama (local + cloud), Groq, Fireworks, Mistral, Perplexity, DeepSeek, Together AI, DeepInfra, Cerebras, NVIDIA NIM, HuggingFace, Moonshot, Nebius, Scaleway, STACKIT, Baseten, Cortecs, 302.AI, ZAI, OpenRouter, LMStudio, Chutes, MiniMax. Configure priority chains. Automatic failover when one throttles. Never locked in. |
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

## What's new in v2.2.15 &mdash; The largest release in Anvil's history

**121 commits over v2.2.13. The provider catalog grows 6x. Cursor lands as a first-class agent surface. `/model` reaches across every configured provider in one TAB-completable list. Two foundational arcs that started in v2.2.5 finish here.**

### Provider catalog: 5 &rarr; 31

Six became thirty-one. Every implementation either uses a documented public API or identifies as Anvil honestly in headers &mdash; **no IDE spoofing, no scraped credentials, no silent fallbacks.**

- **AWS Bedrock** &mdash; hand-rolled SigV4 signing (no AWS SDK = ~50&nbsp;MB smaller binary), audited against AWS test vectors. `InvokeModel` + `InvokeModelWithResponseStream`.
- **Google Gemini OAuth + Antigravity** &mdash; Code Assist OAuth with PKCE flow, dynamic-port localhost callback, atomic token save, pre-emptive refresh. Headers identify Anvil honestly &mdash; not VS Code.
- **GitHub Copilot** &mdash; device flow.
- **Azure OpenAI** &mdash; deployment-name + `api-version` URL pattern.
- **Cursor Cloud Agents** &mdash; public API, repo-bound (see `/cursor` below).
- **24 OpenAI-compatible providers** added in one cycle: Groq, Fireworks, Mistral, Perplexity, DeepSeek, Together&nbsp;AI, DeepInfra, Cerebras, NVIDIA NIM, HuggingFace, Moonshot, Nebius, Scaleway, STACKIT, Baseten, Cortecs, 302.AI, ZAI, OpenRouter, LMStudio, Chutes, MiniMax, OpenCode, OpenCode-Go.

`/provider login <slug>` runs the right flow for each: API-key paste with live validation for direct-key providers, OAuth PKCE for Google, device flow for Copilot, SigV4 for Bedrock.

### `/cursor` first-class command tree

Cursor's public Cloud Agents API is repo-bound and uses a different UX than a typical chat provider, so it gets its own command tree. Six TAB-completable subcommands:

- `/cursor launch <prompt>` &mdash; spawn an agent on the current workspace's GitHub repo, opens an SSE-streamed agent tab
- `/cursor list` &mdash; show active agents with status, model, repo, branch
- `/cursor get <agent_id>` &mdash; full agent record + recent runs
- `/cursor cancel <agent_id> [<run_id>]` &mdash; terminate an in-flight run
- `/cursor artifacts <agent_id>` &mdash; list and download generated files
- `/cursor stream <agent_id> <run_id>` &mdash; re-attach to an SSE stream with `Last-Event-ID` resume

`git remote get-url origin` is mandatory &mdash; Cursor errors loudly if the current workspace isn't a GitHub repo.

### `/model` cross-provider unified picker

`/model<TAB>` now enumerates **every configured provider's models in one list**, provider-prefixed (`anthropic/claude-4.5-sonnet`, `groq/llama-3.3-70b`, `bedrock/anthropic.claude-3-5-sonnet-20241022-v2:0`, `cursor/claude-4-sonnet-thinking`, `ollama/qwen-coder`, ...). Picking a model performs an **atomic switch** &mdash; provider routing, system prompt identity, and TUI chrome all update together. Lists are live-fetched from each provider's `/models` endpoint (cached per session). Unconfigured providers are excluded from the picker.

### Memory Cohesion Arc &mdash; completing v2.2.5

The seven-layer memory system from v2.2.5 (Sensory / Working / Episodic / Semantic / Procedural / Reflective / Long-term) finally cohesion-tests end-to-end. Retrieval-order block in the system prompt, `WorkingMemorySnapshot` as `Vec<PromptSection>`, `PermissionMemory` wired into the permission gate, `/file-cache` real handler replacing the stub, egress allowlist wired into settings + `/policy view`, `/memory why` actually injecting daily summaries into the prompt.

### Capability Cohesion Arc &mdash; 8 axes, enforced at build time

Every Anvil capability now meets an 8-axis contract: **definition / registration / completion / handler / dispatch / rendering / permission gate / OTel + tests**. The cheap-drift gate enforces this at workspace build time &mdash; `every_slash_command_variant_has_a_spec` blocks bidirectional drift. Slash dispatch is unified at one site. Stub messages are banned.

### CC&rarr;Anvil migration

`anvil import claude-code` brings your prior assistant's investment forward verbatim with provenance frontmatter: memory entries get `imported_from` stamps, project instruction files get merge semantics that never clobber existing files, settings get conflict detection, skills get collision handling. Past sessions (up to ~1,800 JSONL files, ~1&nbsp;GB) can be optionally summarized via your configured provider into daily records. The day-2 command `anvil memory clean` rewrites imported entries through a configurable LLM and detects duplicate-meaning entries via Jaccard similarity. The first-run setup wizard offers migration as an opt-in step.

### Seven platforms

macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified at [anvilhub.culpur.net/sha256/](https://anvilhub.culpur.net/sha256/).

---

## Parallel work, transparent tools

**Per-tab inference, tool-call cards, and SSH terminals all live in the same window.**

### Per-tab parallel inference

Each tab owns its own runtime. Fire a prompt in tab 1, switch to tab 2, fire another &mdash; both stream concurrently and independently. The `*` (unread) and `&#9888;` (pending permission) markers in the tab bar update live. You navigate tabs with F2/F3, Ctrl+arrow, Alt+digit, or a click. None of it waits for a turn to finish.

### Tool-call cards

Every tool call &mdash; Glob, Grep, Read, Write, Edit, Bash, WebSearch, any MCP tool &mdash; renders as a bordered card showing the exact input the model sent (pattern, path, command) the moment it fires. Not a summary after the fact. Ctrl+O expands any card to the full input JSON and full result. You see exactly what the model is doing, as it's doing it.

### SSH tabs

`/ssh host` opens a modal connection form &mdash; host, port, user, auth method, key file, passphrase, and an alias to save the connection to your vault. The default key root is `~/.ssh`; Ctrl+F opens a bare-name key picker. Sessions run via russh with vt100 rendering and Ctrl+B prefix keys (tmux-style). An AI session and a live terminal to your server, side by side, in the same window.

### Mid-turn responsiveness

Ctrl+T (new tab), tab switching, `/ssh`, and submitting prompts in other tabs all respond immediately during streaming. The app is interactive throughout a turn &mdash; no waiting for the model to finish before the interface moves.

---

## Install

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

## 31 providers, one terminal

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

**120+ slash commands** (including the new `/cursor` command tree and `/memory clean` / `/cursor stream` / `anvil agents` cross-session monitor). **31 AI providers.** 45 built-in tools. MCP integration. Per-tab parallel inference. SSH tabs. Tool-call cards with Ctrl+O expand. Multi-tab sessions. Git integration. Code productivity dashboard. Session history search. 37-widget configurable status line with 16 presets. Vim keybindings. Focus view. File sandbox with permission modes. 7-language i18n. AnvilHub marketplace for skills, plugins, agents, and themes. Web UI with full configuration parity. First-run setup wizard. CC&rarr;Anvil migration (`anvil import claude-code`). anvil(1) manpage. All of it optional. None of it required.

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