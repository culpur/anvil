# Anvil v2.2.12 — Parallel and Transparent

v2.2.11 taught the agent to know itself. v2.2.12 teaches it to get out of your way.

This release is the product of a punishing architectural cycle: per-tab parallel inference, mid-turn TUI responsiveness, SSH tabs, tool-call transparency, scrollback correctness, and a /quit that doesn't freeze. None of these are features you add on top of a working product. They are structural changes to how the app works at its core — the kind that require rewriting the runtime threading model, the event dispatch loop, and the mutex discipline that holds a concurrent TUI together. The product that comes out the other side is faster, more honest about what it's doing, and no longer blocks you while it thinks.

---

## Highlights

- **Per-tab parallel inference.** Each tab now owns its own runtime and runs turns on a dedicated worker thread. Fire a prompt in tab 1, switch to tab 2, fire another — both stream concurrently and independently. Previously the entire application was serialized on a single runtime. That single-threaded bottleneck is gone.

- **The TUI responds during streaming.** Ctrl+T (new tab), F2/F3 (tab switch), `/ssh` (open SSH connection form), and submitting a prompt from a different tab all work while another tab is mid-turn. You no longer wait for the model to finish before the interface responds. The `*` and `⚠` markers in the tab bar update in real time to show which tabs have unread output or pending permission prompts.

- **Tool-call cards.** Every tool call — Glob, Grep, Read, Write, Edit, Bash, WebSearch, and any MCP tool — now renders as a bordered card showing the actual input the model sent (pattern, path, command) at the moment it fires. Not a one-line summary after completion. Ctrl+O expands any card to show the full input JSON and the full result. You see exactly what the model is doing, as it's doing it.

- **SSH tabs.** `/ssh host` opens a modal connection form — host, port, user, auth method, key path, passphrase, and a save-as-alias field. The default key root is `~/.ssh`; Ctrl+F opens a bare-name resolver that autocompletes your existing key files. Connections run via russh with vt100 rendering and Ctrl+B prefix keys (tmux-style: digit to jump tabs, q to close). Saved connections go into the encrypted vault as `HostCredential` aliases. An AI session and an SSH terminal to the machine you're working on, side by side, in the same window.

- **Session continuity fixed.** `anvil --continue` now honors the model saved in the session's `.meta.json` sidecar, so an Ollama session reconnects to Ollama instead of failing with a missing-credentials error on the default provider. When Anvil exits, the last line shows the session ID and name with the exact `anvil --continue` and `anvil --resume <name>` commands to paste. `/clear` now clears workspace context across all tabs, not just the active one.

- **First-run setup wizard.** New installs walk through mouse capture, theme selection, and permission mode opt-ins before the first session starts. The anvil(1) manpage ships with Homebrew installs. `anvil setup` and `anvil --first-run` are new entry points for reconfiguring these preferences at any time.

- **Scrollback correctness.** PageUp/Up in HISTORICAL VIEW was showing only the first 1–4 characters of each assistant line — you'd see `#`, `##`, `**`, `-` instead of full text. Pending text growth wasn't invalidating cached scrollback lines. Fixed.

---

## Under the hood

- **`Arc<Mutex<ConversationRuntime>>` per tab.** The old design held one shared runtime across all tabs. The new design moves a dedicated `Arc<Mutex<ConversationRuntime>>` into each `Tab` struct and spawns turns on per-tab worker threads via `thread::spawn` rather than `thread::scope`. This is what makes concurrent tab inference possible. TuiEvents now carry a `tab_id` field so event routing reaches the correct tab without any global lock contention.

- **Modal-aware wait loop and /quit fix.** The `wait_for_turn_end_for_tab` function was returning early only when a turn finished. It now also returns early on any user action (new tab, tab switch, SSH open, submit-in-other-tab). `/quit` was deadlocking via self-recursive mutex acquisition in `record_daily` — fixed by restructuring the lock scope so the daily log write completes before the runtime lock is re-acquired.

- **RedrawScheduler.** A unified `RedrawScheduler` replaces the scattered per-component redraw logic that was causing the idle-draw rate bug and the scrollback invalidation bug. Dirty-region tracking means the TUI only redraws the cells that actually changed.

- **Scrollback line cache invalidation.** The HISTORICAL VIEW renders assistant output by caching wrapped line vectors per message. Pending (in-progress) messages were not invalidating their cache entries as new tokens arrived, so only the initial fragment was ever displayed. Cache entries for in-flight messages are now invalidated on every delta.

- **Release pipeline hardening (T1 batch).** `release.sh` now runs a tag-vs-HEAD pre-flight before building, builds from the tagged commit rather than the working tree, and runs a php-lint guard after writing to WordPress and AnvilHub to catch quote-mangling before it causes a 500. AnvilHub /about now pulls from `changelog.json` at render time rather than having changelog content baked into the source file.

---

## Quality

- **318 tests pass** — 304 lib tests + 5 per-tab inference integration tests + 5 SSH integration tests + 4 tool-card integration tests. Zero failures. Zero warnings.
- **5 platforms** — macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64.
- **~22 MB single binary** — no runtime dependencies, no install prerequisites beyond the binary itself.

---

## Install

```bash
# Homebrew (macOS & Linux)
brew upgrade culpur/anvil/anvil
# or fresh install
brew install culpur/anvil/anvil

# curl installer (macOS/Linux)
curl -fsSL https://anvilhub.culpur.net/install.sh | bash

# PowerShell (Windows)
irm https://anvilhub.culpur.net/install.ps1 | iex

# Already installed
anvil upgrade
```

---

## Full changelog

https://github.com/culpur/anvil/compare/v2.2.11...v2.2.12
