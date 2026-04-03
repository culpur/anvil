# Anvil

Anvil is a local coding-agent CLI implemented in safe Rust. It is **Claude Code inspired** and developed as a **clean-room implementation**: it aims for a strong local agent experience, but it is **not** a direct port or copy of Claude Code.

The Rust workspace is the current main product surface. The `anvil` binary provides interactive sessions, one-shot prompts, workspace-aware tools, local agent workflows, and plugin-capable operation from a single workspace.

## Current status

- **Version:** `1.0.1`
- **Release stage:** general availability, binary distribution via GitHub Releases
- **Primary implementation:** Rust workspace in this repository
- **Platform focus:** macOS, Linux, and Windows developer workstations

## Install

### Quick install (macOS / Linux)

```bash
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

### Quick install (Windows)

```powershell
irm https://anvilhub.culpur.net/install.ps1 | iex
```

### Manual download

Pre-built binaries are published to GitHub Releases for each version:

```
https://github.com/culpur/anvil-source/releases/latest
```

Available targets:

| Platform | Binary |
|---|---|
| macOS Apple Silicon | `anvil-aarch64-apple-darwin` |
| macOS Intel | `anvil-x86_64-apple-darwin` |
| Linux x86_64 | `anvil-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `anvil-aarch64-unknown-linux-gnu` |
| Windows x86_64 | `anvil-x86_64-pc-windows-msvc.exe` |

Each binary has a corresponding `.sha256` checksum file at the same release URL.

Example (macOS Apple Silicon):

```bash
curl -fsSL https://github.com/culpur/anvil-source/releases/download/v1.0.1/anvil-aarch64-apple-darwin -o anvil
curl -fsSL https://github.com/culpur/anvil-source/releases/download/v1.0.1/anvil-aarch64-apple-darwin.sha256 -o anvil.sha256
shasum -a 256 -c anvil.sha256
chmod +x anvil && sudo mv anvil /usr/local/bin/anvil
```

## Dependencies

### QMD (Recommended)
QMD is the knowledge base engine that powers Anvil's intelligent context system.
It indexes your codebase and previous sessions, enabling automatic context injection.

```bash
npm install -g @tobilu/qmd
```

- Repository: https://github.com/tobi/qmd
- npm: [@tobilu/qmd](https://www.npmjs.com/package/@tobilu/qmd)
- Anvil works without QMD but memory, history search, and auto-context features will be disabled.

## Build from source

### Prerequisites

- Rust stable toolchain
- Cargo
- Provider credentials for the model you want to use

### Authentication

Anthropic-compatible models:

```bash
export ANTHROPIC_API_KEY="..."
# Optional when using a compatible endpoint
export ANTHROPIC_BASE_URL="https://api.anthropic.com"
```

Grok models:

```bash
export XAI_API_KEY="..."
# Optional when using a compatible endpoint
export XAI_BASE_URL="https://api.x.ai"
```

OAuth login is also available:

```bash
cargo run --bin anvil -- login
```

### Install locally

```bash
cargo install --path crates/anvil-cli --locked
```

### Build

```bash
cargo build --release -p anvil-cli
```

### Run

From the workspace:

```bash
cargo run --bin anvil -- --help
cargo run --bin anvil --
cargo run --bin anvil -- prompt "summarize this workspace"
cargo run --bin anvil -- --model sonnet "review the latest changes"
```

From the release build:

```bash
./target/release/anvil
./target/release/anvil prompt "explain crates/runtime"
```

## Supported capabilities

- Interactive REPL and one-shot prompt execution
- Saved-session inspection and resume flows
- Built-in workspace tools for shell, file read/write/edit, search, web fetch/search, todos, and notebook updates
- Slash commands for status, compaction, config inspection, diff, export, session management, and version reporting
- Local agent and skill discovery with `anvil agents` and `anvil skills`
- Plugin discovery and management through the CLI and slash-command surfaces
- OAuth login/logout plus model/provider selection from the command line
- Workspace-aware instruction/config loading (`ANVIL.md`, config files, permissions, plugin settings)

## Current limitations

- GitHub CI verifies `cargo check`, `cargo test`, and release builds
- Current CI targets Ubuntu, macOS, and Windows
- Some live-provider integration coverage is opt-in because it requires external credentials and network access
- The command surface may continue to evolve during the `1.x` series

## Implementation

The Rust workspace is the active product implementation. It currently includes these crates:

- `anvil-cli` — user-facing binary
- `api` — provider clients and streaming
- `runtime` — sessions, config, permissions, prompts, and runtime loop
- `tools` — built-in tool implementations
- `commands` — slash-command registry and handlers
- `plugins` — plugin discovery, registry, and lifecycle support
- `lsp` — language-server protocol support types and process helpers
- `server` and `compat-harness` — supporting services and compatibility tooling

## Roadmap

- Add more task-focused examples and operator documentation
- Continue tightening feature coverage and UX polish across the Rust implementation

## Release notes

- Draft 1.0.1 release notes: [`docs/releases/1.0.1.md`](docs/releases/1.0.1.md)

## License

See the repository root for licensing details.
