# Anvil

**AI Coding Assistant by Culpur Defense**

![Version](https://img.shields.io/badge/version-2.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)
![Tests](https://img.shields.io/badge/tests-394%20passed-brightgreen)
![Security](https://img.shields.io/badge/security-AES--256--GCM%20vault-orange)

Anvil is a local AI coding-agent CLI implemented in safe Rust. It provides interactive sessions, one-shot prompts, workspace-aware tools, and 90 slash commands from a single binary — with no telemetry, full air-gap support, and encrypted credential storage.

---

## What's New in v2.1.0

### Encrypted Credential Vault (Security)
All API keys and credentials are now stored in an **AES-256-GCM encrypted vault** with Argon2id key derivation. The setup wizard prompts for a vault password before provider configuration. Credentials never touch disk unencrypted.

### File Write Sandbox (Security)
Write operations are now **sandboxed to the project root** by default. Agents cannot write outside your project boundary, preventing path traversal attacks. Always-allowed targets include `/tmp` and `~/.anvil/`.

### Native Ollama API
Switched from OpenAI-compatible `/v1/chat/completions` to Ollama's native `/api/chat` endpoint. Proper `think: true/false` control for reasoning models (qwen3, deepseek-r1). Better streaming and token tracking.

### Multi-Line Input
The input area dynamically expands from 1 to 5 lines based on content length, with proper word-wrap and cursor tracking.

### Modular Architecture
Codebase restructured from 4 monolithic files into **134 focused modules**. The largest file dropped from 15,756 to 4,770 lines. Zero-warning compilation with strict clippy.

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
anvil --update
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
| Windows x86_64 | `anvil-x86_64-pc-windows-msvc.exe` |

Each binary ships with a `.sha256` checksum file at the same URL.

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

- [v2.1.0](docs/releases/2.1.0.md)
- [v2.0.0](docs/releases/2.0.0.md)
- [v0.1.0](docs/releases/0.1.0.md)

---

## License

See the repository root for licensing details.
