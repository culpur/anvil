# Anvil v2.2.7 — Cross-Platform Hardening, Windows Fixes, Honest Ollama

This is a fix-forward release after v2.2.6 shipped cross-compiled binaries with mismatched embedded versions. v2.2.7 corrects that, plus the Windows experience, the Ollama tool-use path, and adds cross-OS installers with signature verification. Everything in this release survived a regression test suite that would have blocked the original v2.2.6 bug.

## Highlights

**Fixed on Windows**
- Binary now correctly reports v2.2.7 (v2.2.6's Windows `.exe` was embedded with v2.2.1 due to a stale cross-compile artifact uploaded by the release pipeline — see "Under the hood" below)
- `HOME` and `PATH` references replaced with cross-platform alternatives throughout 24 files; Windows now reads `USERPROFILE`, splits PATH on `;`, and honors `PATHEXT` for binary discovery
- QMD discovery works on Windows via `%LOCALAPPDATA%\Programs\qmd\qmd.exe` and `%PROGRAMFILES%\qmd\qmd.exe` fallbacks
- Configuration directory follows platform conventions: `%APPDATA%\anvil` on Windows, preserves existing `~/.anvil` on Unix

**Fixed on every platform**
- TUI now has a 10,000-line in-buffer scrollback — PageUp / PageDown through history, End returns to live view
- Hold Shift (macOS: Option) while drag-selecting to let the terminal handle native text selection — Anvil no longer claims the event
- `/remote-control` returning 503 Service Unavailable is fixed — the WebSocket relay server is now under pm2 supervision on the backend, not a detached manual process

**Ollama tool use, corrected**
- When an Ollama model describes writing a file but doesn't actually emit a structured tool call, Anvil no longer silently pretends it worked. You now see a loud warning and the file is not saved.
- Added a four-format tool-call parser covering the common patterns Ollama models emit: `<tool_use>` XML tags, JSON inside ` ```json ` fences, natural-language phrasing with backtick paths, and the OpenAI-style function-call shape.
- Compatibility matrix for Llama 3.x, Qwen 2.5 / 3, Mistral Nemo, Codestral, Code Llama, Gemma 3, Phi 4 documented in the repo.

## New in this release

**Cross-OS installers**
- `curl -fsSL https://anvilhub.culpur.net/install.sh | sh` for macOS and Linux
- `irm https://anvilhub.culpur.net/install.ps1 | iex` for Windows
- Both verify the downloaded binary's SHA256 against the published value before running it — install aborts on mismatch
- Installs Anvil, Node.js + npm, QMD, and Git if missing
- Offers to install Ollama with a choice of Western-origin open-weight models plus Qwen (each model requires explicit confirmation — no silent pulls):
  - General-purpose: Llama 3.1 8B (default), Llama 3.3 70B, Qwen3 8B/14B, Mistral Nemo 12B, Gemma 3 4B/27B, Phi 4 14B
  - Coding-specialized: Qwen 2.5-Coder 7B (default), Code Llama 13B, Codestral 22B, plus smaller and larger variants
- First-run wizard: creates the Anvil config directory, prompts for preferred provider, guides API-key entry into the encrypted vault
- Shell completions for bash, zsh, fish, and PowerShell

**New commands**
- `anvil --check` — diagnostic runs through Anvil on PATH, Node/npm versions, QMD discoverability, Git, vault status, API key presence, relay reachability. Prints a green/red checklist.
- `anvil --uninstall` — removes the binary, prompts about keeping or removing `~/.anvil/` (vault, config, session history).
- `anvil upgrade` — fetches latest release from GitHub, verifies SHA256, replaces the running binary in place, and relaunches via the v2.2.6 self-respawn path so the session survives.

**Packaging**
- Debian `.deb` package for apt-based distros (Debian, Ubuntu)
- RHEL `.rpm` package for dnf-based distros (Fedora, RHEL, Rocky)

## Under the hood

The original v2.2.6 Windows binary shipped with `v2.2.1` embedded in its executable because the `anvil-release` MCP rebuilt only the native ARM macOS target, then uploaded all five cross-compiled artifacts from `target/release-artifacts/` — four of which were stale from April 14. No validation caught this.

v2.2.7 adds `auditBinaryVersions()` to the release pipeline. Every binary is scanned for its embedded `Anvil v<semver>` string and compared against the requested release version. Any mismatch blocks the upload entirely with a specific per-file error report. The fix is covered by six regression tests (T16a through T16f) — T16f specifically simulates the v2.2.6 scenario and asserts the pipeline halts.

The release notes file you are reading is required by the same hardened pipeline. Release notes can no longer be auto-generated from the commit subject (which produced "release: Anvil v2.2.6" in the v2.2.6 body — embarrassing, not informative).

## Install

```bash
# Homebrew (macOS and Linux)
brew install culpur/anvil/anvil

# Shell installer (macOS and Linux)
curl -fsSL https://anvilhub.culpur.net/install.sh | sh

# PowerShell installer (Windows)
irm https://anvilhub.culpur.net/install.ps1 | iex

# Direct download
https://github.com/culpur/anvil/releases/tag/v2.2.7
```

No account required. No sign-in. No telemetry.

## Verify

After install:
```
anvil --version   # 2.2.7
anvil --check     # full diagnostic
```

## Stats

- 584 tests passing, zero warnings
- 7 bug-fix buckets plus the installer package
- Covers macOS ARM, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64

## Upgrading

Existing users on v2.2.5 or v2.2.6:
```
brew upgrade culpur/anvil/anvil
# or
anvil upgrade
```

Windows users still on the broken v2.2.6 download: re-download from the release page above, or run the new PowerShell installer.
