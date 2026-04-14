# Anvil — AI Coding Assistant

Built for defense. Geared for offense.

Multi-provider AI coding assistant with full-screen TUI, credential auto-detection, network egress control, and 90+ commands.

## What's New in v2.1.3

- **Edition 2024 Mode** — Optimized session defaults and provider routing for 2024-era models.
- **Focus View** — Distraction-free single-pane mode. Toggle with `/focus` to hide sidebars and agent panels.
- **Context-Low Warning** — Proactive alert when context window nears capacity, before auto-compaction fires.
- **Stalled Stream Handling** — Detects and recovers from stuck token streams without losing the response.
- **WebFetch Cleanup** — Improved HTML-to-text extraction; strips nav, footer, and cookie banners automatically.
- **`/loop` and `/proactive`** — New slash commands for recurring prompt loops and proactive agent nudges.

## Installation

```bash
# One-line install (macOS / Linux)
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

```bash
# macOS ARM (M1/M2/M3/M4)
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

## Features

- **Multi-Provider** — Claude, OpenAI, Ollama, xAI with smart failover
- **1M Token Context** — automatic archival and QMD-powered retrieval
- **Live Streaming** — real-time token-by-token response rendering
- **Remote Control** — share sessions from any browser via WebSocket
- **Credential Scanner** — auto-detects and vaults API keys, SSH keys, TLS certs
- **Encrypted Vault** — AES-256-GCM storage with Argon2id KDF
- **Network Egress Control** — domain allowlist for tool network access
- **File Sandbox** — safe file operations with permission gating
- **45+ Tools** — Bash, file ops, search, LSP, MCP servers, image generation
- **90+ Commands** — comprehensive slash command system
- **Full TUI** — tabs, themes, vim mode, thinking indicator, inline images
- **7 Agent Types** — specialized agents with worktree isolation
- **Signed Audit Trail** — HMAC-SHA256 session transcripts
- **AnvilHub** — marketplace for skills, plugins, agents, and themes

## Quick Start

```bash
anvil                    # Interactive session
anvil prompt "explain"   # One-shot prompt
/remote-control          # Share via browser
/fork experiment         # Branch the conversation
/vault scan              # Detect and vault credentials
/security egress         # Manage network allowlist
/export md               # Export as Markdown
/think                   # Toggle thinking mode
```

## Supported Providers

| Provider | Models | Auth |
|----------|--------|------|
| Anthropic | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | OAuth or API Key |
| OpenAI | GPT-5, o3, o4-mini, Codex | API Key |
| Ollama | Llama, Qwen, Mistral, DeepSeek (local) | No key needed |
| xAI | Grok-3, Grok-3-mini | API Key |

## Documentation

- **[anvilhub.culpur.net](https://anvilhub.culpur.net)** — Package marketplace & documentation
- **[anvilhub.culpur.net/about](https://anvilhub.culpur.net/about)** — Full changelog

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.

