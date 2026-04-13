# Anvil — AI Coding Assistant

Built for defense. Geared for offense.

Multi-provider AI coding assistant with full-screen TUI, live streaming, remote control, encrypted credential vault, and 90+ commands.

## What's New in v2.1.1

- **Live Streaming** — Responses render token-by-token in real-time
- **Remote Control** — Share sessions via `/remote-control` with browser WebSocket relay
- **Thinking Mode** — Visible reasoning indicator with `/think` toggle
- **Cross-Platform Builds** — macOS ARM64/Intel, Linux x86_64/ARM64, Windows

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
- **Encrypted Vault** — AES-256-GCM credential storage with Argon2id KDF
- **File Sandbox** — safe file operations with permission gating
- **45+ Tools** — Bash, file ops, search, LSP, MCP servers, image generation
- **90+ Commands** — comprehensive slash command system
- **Full TUI** — tabs, themes, vim mode, thinking indicator
- **7 Agent Types** — specialized agents with worktree isolation
- **AnvilHub** — marketplace for skills, plugins, agents, and themes

## Quick Start

```bash
# Interactive session (wizard runs on first launch)
anvil

# One-shot prompt
anvil prompt "explain the architecture of this repo"

# Share your session via browser
/remote-control

# Toggle thinking mode
/think
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
- **[anvilhub.culpur.net/install](https://anvilhub.culpur.net/install)** — Installation guide
- **[anvilhub.culpur.net/about](https://anvilhub.culpur.net/about)** — Full changelog

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.
