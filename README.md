# Anvil — AI Coding Assistant

**Built for defense. Geared for offense.**

![Version](https://img.shields.io/badge/version-2.1.3-0FBCFF?style=flat-square) ![License](https://img.shields.io/badge/license-proprietary-red?style=flat-square) ![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-555?style=flat-square)

You're shipping production code. You don't have time for a browser tab that forgets your context, drops the thread mid-response, or locks you into a single AI provider with no fallback. Anvil is a full-screen terminal AI assistant 15 MB, zero dependencies, four AI providers with automatic failover, an encrypted credential vault, 90+ commands, and a multi-agent system that does real work while you stay in the loop. It runs on your hardware, keeps your secrets, and doesn't phone home.

---

## Why Anvil?

- **Never interrupted by rate limits.** Four providers (Claude, OpenAI, Ollama, xAI) with smart failover means when one provider throttles you, Anvil switches automatically. Your session continues without breaking stride.

- **Your credentials stay encrypted.** AES-256-GCM with Argon2id KDF. The vault auto-detects API keys, SSH keys, and TLS certs in your filesystem and offers to protect them. Nothing leaves your machine in plaintext.

- **Air-gap ready, zero telemetry.** Point Anvil at a local Ollama instance and it works with no internet at all. No analytics, no usage reporting, no callbacks to anyone.

- **Serious terminal UX.** Full-screen ratatui TUI with tabs, vim keybindings, thinking mode indicator, inline image rendering, Focus View (Ctrl+O), and real-time streaming — not a CLI wrapper bolted onto a chat API.

- **A whole toolchain in 15 MB.** 45 built-in tools, 7 agent types with worktree isolation, MCP server support, HMAC-SHA256 signed audit trails, and an AnvilHub marketplace for community skills and plugins.

---

## Quick Install

```bash
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

Or grab a platform binary directly:

```bash
# macOS Apple Silicon
curl -LO https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-apple-darwin
chmod +x anvil-aarch64-apple-darwin && sudo mv anvil-aarch64-apple-darwin /usr/local/bin/anvil

# macOS Intel
curl -LO https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-apple-darwin
chmod +x anvil-x86_64-apple-darwin && sudo mv anvil-x86_64-apple-darwin /usr/local/bin/anvil

# Linux x86_64
curl -LO https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-linux-gnu
chmod +x anvil-x86_64-unknown-linux-gnu && sudo mv anvil-x86_64-unknown-linux-gnu /usr/local/bin/anvil

# Linux ARM64
curl -LO https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-unknown-linux-gnu
chmod +x anvil-aarch64-unknown-linux-gnu && sudo mv anvil-aarch64-unknown-linux-gnu /usr/local/bin/anvil
```

Windows: Download `anvil-x86_64-pc-windows-gnu.exe` from [releases](https://github.com/culpur/anvil/releases/latest).

---

## What's New in v2.1.3

- **Performance Upgrade** — Modernized internals with latest dependencies. Leaner and faster across the board.
- **Focus View (Ctrl+O)** — See only prompts, tool summaries, and responses. Everything else disappears.
- **Context-Low Warning** — Footer shows ⚠ at 80% context usage, CRITICAL at 95%. No more surprise compactions.
- **Stalled Stream Recovery** — Streams idle for 5+ minutes are aborted and retried non-streaming automatically.
- **WebFetch Cleanup** — CSS, JS, nav, and cookie banners stripped from fetched pages before the model sees them.
- **`/loop` and `/proactive`** — Recurring prompt loops and agent-initiated suggestions, now built in.

---

## Feature Grid

| | | |
|---|---|---|
| 🔒 Encrypted Vault | 🤖 7 Agent Types | 🌐 Remote Control |
| ⚡ Live Streaming | 🔍 45+ Tools | 🛡 Network Egress Control |
| 🧠 Thinking Mode | 📋 Signed Audit Trail | 🔥 AnvilHub Marketplace |
| 🗂 Tabbed Sessions | 🖥 Vim Keybindings | 📦 MCP Server Support |
| 🔄 Smart Failover | 🏠 Air-Gap / Ollama | 👁 Focus View (Ctrl+O) |
| ⚠ Context Warning | ♻ Stream Recovery | 🌿 Worktree Isolation |

---

## Supported Providers

| Provider | Models | Auth |
|----------|--------|------|
| Anthropic | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | OAuth or API Key |
| OpenAI | GPT-5, o3, o4-mini, Codex | API Key |
| Ollama | Llama, Qwen, Mistral, DeepSeek (local) | No key needed |
| xAI | Grok-3, Grok-3-mini | API Key |

---

## Quick Start

```bash
anvil                    # Interactive session
anvil prompt "explain"   # One-shot prompt
/remote-control          # Share live session via browser
/fork experiment         # Branch the conversation
/vault scan              # Detect and vault credentials
/security egress         # Manage network allowlist
/loop 10m /standup       # Recurring prompt every 10 minutes
/think                   # Toggle extended thinking mode
/export md               # Export session as Markdown
```

---

## Links

- **[anvilhub.culpur.net](https://anvilhub.culpur.net)** — Package marketplace, skills, plugins, themes
- **[anvilhub.culpur.net/docs](https://anvilhub.culpur.net/docs)** — Full documentation
- **[anvilhub.culpur.net/about](https://anvilhub.culpur.net/about)** — Changelog and roadmap
- **[Releases](https://github.com/culpur/anvil/releases)** — All platform binaries

---

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.
