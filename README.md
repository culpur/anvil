# Anvil

**AI Coding Assistant by Culpur Defense**

![Version](https://img.shields.io/badge/version-1.0.3.1-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

Anvil is a local AI coding-agent CLI implemented in safe Rust. It provides interactive sessions, one-shot prompts, workspace-aware tools, and 86 slash commands from a single binary — with no telemetry and full air-gap support.

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
https://github.com/culpur/anvil-source/releases/latest
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
# Start an interactive session
anvil

# One-shot prompt
anvil prompt "explain the architecture of this repo"

# Switch provider or model
anvil --provider openai --model gpt-4o "review the latest changes"

# Set up the credential vault
anvil
/vault setup
```

Set your provider credentials before first use:

```bash
# Anthropic
export ANTHROPIC_API_KEY="..."

# OpenAI
export OPENAI_API_KEY="..."

# xAI / Grok
export XAI_API_KEY="..."

# Ollama (local, no key required)
export OLLAMA_BASE_URL="http://localhost:11434"
```

Or use the interactive wizard:

```bash
/configure
```

---

## Feature Highlights

### Credential Vault
AES-256-GCM encrypted credential store with Argon2id key derivation, plus a built-in TOTP (one-time password) manager. Credentials never leave the machine unencrypted.

```bash
/vault setup
/vault store github-token
/vault get github-token
/vault totp add my-account
/vault totp my-account
```

### Multi-Provider AI
Switch between Anthropic, OpenAI, Ollama, and xAI/Grok without leaving your session. Automatic failover handles rate limits across providers.

```bash
/provider list
/provider anthropic
/failover add claude-opus-4-5
```

### 7 Display Languages
The full TUI is localized in English, German, Spanish, French, Japanese, Simplified Chinese, and Russian.

```bash
/language de
/language zh-CN
```

### VS Code Extension
The Anvil VS Code extension provides a chat panel, hub browser, and inline AI actions directly inside the editor. Install from the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=culpur.anvil-vscode) or from AnvilHub.

### Infrastructure Commands
Purpose-built commands for SSH session management, Kubernetes, Terraform/IaC, Docker, log analysis, database tools, CI/CD pipeline generation, and multi-platform notifications including Matrix rooms.

### AnvilHub Marketplace
Browse and install skills, plugins, agents, and themes from the AnvilHub package registry without leaving the CLI.

```bash
/hub search react
/hub install react-expert
```

---

## Command Categories

Anvil ships 86 slash commands across six categories. Full reference at [anvilhub.culpur.net/usage](https://anvilhub.culpur.net/usage).

### Core Flow
`/help` `/status` `/model` `/provider` `/permissions` `/login` `/chat` `/vim` `/doctor` `/tokens` `/cost` `/failover` `/configure` `/theme` `/voice` `/language`

### Workspace & Memory
`/config` `/memory` `/init` `/diff` `/version` `/teleport` `/context` `/pin` `/unpin` `/qmd` `/semantic-search` `/scaffold` `/env` `/deps` `/mono` `/markdown` `/snippets` `/lsp` `/screenshot` `/vault`

### Sessions & Output
`/clear` `/resume` `/export` `/session` `/history` `/history-archive` `/collab`

### Git & GitHub
`/commit` `/commit-push-pr` `/pr` `/issue` `/branch` `/worktree` `/undo` `/git` `/changelog`

### Automation & Discovery
`/plugin` `/plugin-sdk` `/agents` `/skills` `/hub` `/bughunter` `/ultraplan` `/debug-tool-call` `/web` `/search` `/generate-image` `/docker` `/test` `/refactor` `/db` `/security` `/api` `/docs` `/perf` `/debug` `/notebook` `/k8s` `/iac` `/pipeline` `/review` `/browser` `/notify` `/migrate` `/regex` `/ssh` `/logs` `/finetune` `/webhook`

---

## Configuration

Anvil loads configuration from `~/.config/anvil/config.toml` and, when inside a project, from `ANVIL.md` in the workspace root.

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
| Anthropic | Claude 3.5 / Claude 4 series | `ANTHROPIC_API_KEY` |
| OpenAI | GPT-4o, o1, o3 series | `OPENAI_API_KEY` |
| Ollama | Any locally served model | Local endpoint (no key) |
| xAI / Grok | Grok-2, Grok-3 series | `XAI_API_KEY` |

---

## Dependencies

### QMD (Recommended)

QMD is the knowledge-base engine that powers Anvil's context and memory features. It indexes your codebase and session history for automatic context injection.

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
- **Homebrew tap**: [github.com/culpur/homebrew-tap](https://github.com/culpur/homebrew-tap)
- **Release binaries**: [github.com/culpur/anvil-source/releases](https://github.com/culpur/anvil-source/releases)

---

## Release Notes

- [v1.0.3.1](docs/releases/1.0.3.1.md)
- [v0.1.0](docs/releases/0.1.0.md)

---

## License

See the repository root for licensing details.
