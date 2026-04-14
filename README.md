# Anvil — AI Coding Assistant

**Built for defense. Geared for offense.**

![Version](https://img.shields.io/badge/version-2.2.1-blue)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

15 MB. Zero dependencies. Five AI providers. Typed Credential Vault. 90+ commands. And the only AI coding assistant with **live remote control** — hand your terminal session to any browser, in real-time, with full bidirectional control.

## Why Anvil?

- **Never interrupted** — Five providers (Claude, OpenAI, Ollama, xAI, Gemini) with automatic failover. When one hits a rate limit, Anvil switches to the next without dropping your context.
- **Live Remote Control** — Type `/remote-control` and open the URL on any device. Not a transcript. Not a screenshot. A live, bidirectional terminal-to-browser bridge with real-time streaming, 98 commands, and secure 6-digit pairing.
- **Typed Credential Vault** — The vault is now the single source of truth for ALL sensitive data. API keys, SSH keys, TLS certs, tokens, and environment secrets — typed, named, and AES-256-GCM encrypted with Argon2id key derivation. Credentials never touch disk unencrypted.
- **Air-gap ready** — Single binary, zero telemetry, local Ollama support. Runs where other tools can't.
- **Full-screen TUI** — Tabs, streaming output, tool call visualization, focus view, thinking mode, vim keybindings. Not a chat window — a development environment.
- **AnvilHub Marketplace** — Skills, plugins, agents, and themes from the community. Install with one command.

## Live Remote Control

No other AI coding assistant does this. Type `/remote-control` in your terminal:

1. Anvil generates a URL and a 6-digit pairing code
2. Open the URL in **any browser** — phone, tablet, laptop, another continent
3. Enter the code — you're now connected to the live session
4. **Both sides have full control** — type messages, run slash commands, create and manage tabs
5. See the AI's response streaming token-by-token in real-time
6. Status bar, context usage, model info, 98-command autocomplete — all in the browser
7. Encrypted connection, secure pairing, automatic reconnection

Perfect for pair programming, teaching, demos, monitoring long-running tasks, or working from your phone while your workstation runs the heavy lifting.

## Install

```bash
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

Or download directly from [releases](https://github.com/culpur/anvil/releases/latest):

| Platform | Binary |
|----------|--------|
| macOS ARM (M1/M2/M3/M4) | `anvil-aarch64-apple-darwin` |
| macOS Intel | `anvil-x86_64-apple-darwin` |
| Linux x86_64 | `anvil-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `anvil-aarch64-unknown-linux-gnu` |
| Windows x86_64 | `anvil-x86_64-pc-windows-gnu.exe` |

## What's New in v2.2.1

| Feature | Description |
|---------|-------------|
| **Typed Credential Vault** | The vault is now the single source of truth for ALL sensitive data — API keys, SSH keys, TLS certs, tokens, and env secrets with named types |
| **Vault Schema v2** | Structured credential entries: `name`, `type`, `value`, `tags`, `created_at`, `rotated_at` |
| **`/vault add`** | Interactive typed credential entry with category selection |
| **`/vault rotate`** | Rotate any credential in-place, preserving audit history |
| **`/vault export`** | Encrypted vault export for backup and migration |
| **Env secret injection** | `/vault inject` — load vault secrets into shell env for any subprocess |
| **Audit trail v2** | Every vault access logged with timestamp, operation, and credential type |

## Features

| | | |
|---|---|---|
| ⚡ Multi-Provider Failover | 🖥 Full-Screen TUI | 🔐 Typed Credential Vault |
| 🌐 Live Remote Control | 📋 90+ Commands | 🧠 1M Token Context |
| 🔧 45 Built-in Tools | 🤖 7 Agent Types | 🛒 AnvilHub Marketplace |
| 👁 Focus View | 📚 Smart Compaction | 🛡 File Sandbox |
| 🌍 7 Languages | 🎨 Custom Themes | 📱 Browser Access |

## Providers

| Provider | Models | Auth |
|----------|--------|------|
| Anthropic | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | OAuth or API Key |
| OpenAI | GPT-5, o3, o4-mini | API Key |
| Ollama | Llama, Qwen, Mistral, DeepSeek, Gemma | Local — no key |
| xAI | Grok-3, Grok-3-mini | API Key |
| Google | Gemini 2.5 Pro, Gemini 2.5 Flash, Gemini 1.5 Pro | API Key |

## Quick Start

```bash
anvil                               # Start interactive session
/remote-control                     # Share via browser
/model claude-opus-4-6              # Switch model
/fork experiment                    # Branch the conversation
/focus                              # Toggle focus view
/vault add                          # Add a typed credential
/vault scan                         # Detect and vault credentials
/export md                          # Export as Markdown
```

## Links

- **[anvilhub.culpur.net](https://anvilhub.culpur.net)** — Marketplace & documentation
- **[anvilhub.culpur.net/about](https://anvilhub.culpur.net/about)** — Full changelog

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.
