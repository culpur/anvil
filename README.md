# Anvil — AI Coding Assistant

Built for defense. Geared for offense.

Multi-provider AI coding assistant with full-screen TUI, 1M token context, encrypted credential vault, and 44+ built-in tools.

## Installation

```bash
# macOS ARM (M1/M2/M3)
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
- **Encrypted Vault** — AES-256-GCM credential storage
- **File Sandbox** — safe file operations with permission gating
- **44+ Tools** — Bash, file ops, search, LSP, MCP servers
- **Full TUI** — tabs, themes, vim mode, live streaming
- **Remote Control** — access sessions from any browser

## Documentation

[anvilhub.culpur.net](https://anvilhub.culpur.net)

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.
