# Anvil

**AI Coding Assistant by Culpur Defense**

![Version](https://img.shields.io/badge/version-2.2.14-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows%20%7C%20BSD-lightgrey)
![Tests](https://img.shields.io/badge/tests-1657%20passed-brightgreen)
![Security](https://img.shields.io/badge/security-AES--256--GCM%20vault-orange)

Anvil is a local AI coding-agent CLI implemented in safe Rust. It provides interactive sessions, one-shot prompts, workspace-aware tools, and 101 slash commands from a single binary — with no telemetry, full air-gap support, and encrypted credential storage.

---

## What's New in v2.2.14 — Parallel tabs become solid

**Per-tab parallel inference, debugged to the layer.** v2.2.13 introduced
the architecture for concurrent turn dispatch across multiple TUI tabs.
v2.2.14 makes it actually work end-to-end.

Three bugs surfaced during real concurrent use:

1. **Apple Terminal's Enter key was being eaten** — Apple Terminal sends
   Enter as raw byte `0x0A`, which crossterm reports as `Char('j') +
   CONTROL`. A legacy handler interpreted that as "insert literal
   newline" instead of submitting. The Ctrl+J branch is gone; Enter
   submits on every terminal Anvil supports.
2. **Submitted prompts vanished from tab 2's scrollback** — the
   in-flight wait-loop's Enter branch consumed the draft via `mem::take`
   without pushing a `LogEntry::User` to the tab's log. Fixed: the
   in-flight path now mirrors the idle path.
3. **"Thinking..." indicator never appeared on tab 2** — the per-tab
   spawn worker threads jumped straight to `run_turn` without emitting
   `ThinkLabel`. Both spawn paths now surface the indicator on the
   target tab's sender as their first action.

**CC parity through v2.1.139.** Twenty-one items from the v2.1.132 →
v2.1.139 audit ship: hook `args[]` exec form, hook `continueOnBlock`,
`/scroll-speed`, `/plugin details`, `/goal` auto-link, transcript-view
nav keys, `anvil agents` cross-session monitor, `worktree.baseRef`
setting, per-session env propagation (`ANVIL_SESSION_ID`,
`ANVIL_PROJECT_DIR`, `ANVIL_EFFORT`, `ANVIL_DISABLE_ALTERNATE_SCREEN`),
OTEL TRACEPARENT propagation, subagent OTel headers, autoMode hard-deny
short-circuit, `--resume` underscore-path support.

**W11 and W15b finally wired.** The file-fingerprint cache shipped as
dormant code in v2.2.11. v2.2.14 wires it into `read_file`,
`write_file`, `edit_file`, and the system prompt. The auto-promote
engine for `/memory` notes is now active too.

**Two security boundaries tightened.** ReadOnly mode now hard-blocks
even when an `Edit` allow rule in `~/.anvil/settings.json` would
otherwise grant write capability — mode gate runs before allow-rule
match. Tool allow-rule wildcards (`Skill(name *)`) gain a delimiter
check so prefix matches can't slip through on unexpected characters.

**TUI architecture cleanup.** Ctrl+C now cancels mid-flight streaming.
In-flight wait-loop latency cut from 80ms to 20ms. Mid-stream typed
messages are queued instead of dropped.

See [RELEASE-NOTES-v2.2.14.md](RELEASE-NOTES-v2.2.14.md) for the full
narrative.

---

## What's New in v2.2.13 — Windows is back, BSD joins, routines on disk

**Windows x86_64 is back in the release matrix.** v2.2.12 deferred
Windows because the new SSH agent code called
`russh::AgentClient::connect_uds()` — a Unix-only function. v2.2.13
gates the agent path with `#[cfg(unix)]` and provides a clear-error
stub on Windows. Key-file, password, and keyboard-interactive auth
work on Windows exactly as on Unix.

**FreeBSD x86_64 and NetBSD x86_64 join the build matrix.** Seven
binaries now ship per release: macOS ARM64, macOS Intel, Linux x86_64,
Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Cross-compile
runs through Culpur-owned builder images at
`registry.culpur.net/culpur/anvil-builder-<target>`. FreeBSD ARM64 and
OpenBSD x86_64 remain source-only — Rust ships no precompiled stdlib
for those targets yet.

**Release pipeline hardening.** v2.2.12 shipped one stale binary
because `cargo build … 2>&1 | tail -1` was masking the cross-compile
exit code. v2.2.13 strips every `| tail -1` mask from `release.sh`.
`set -euo pipefail` now actually catches build failures.

**Routines foundation on disk (no behavior change).** A new
`crates/runtime/src/routines/` module lands with `schedule`, `archive`,
and `packet` components — duration / interval / cron / ISO-8601
schedule grammar, atomic markdown output archive at
`~/.anvil/routines/output/`, anti-injection packet schema with
`<<<ROUTINE-PACKET-START>>>` delimiters, and a `[SILENT]` early-stop
marker. No code consumes the module in v2.2.13; the runtime daemon
and `/routine` slash command are queued for v2.3.

See [RELEASE-NOTES-v2.2.13.md](RELEASE-NOTES-v2.2.13.md) for the full
narrative.

---

## What's New in v2.2.12 — Live-typing during turns + /ssh tabs

**Live typing during in-flight turns.** Previously, Anvil's TUI froze
input while a turn was streaming — your keystrokes went nowhere until
the response landed. v2.2.12 turns the input box into a live draft
buffer that captures everything you type during a streaming turn. On
`TurnDone`, the draft becomes the next prompt automatically. No
keystrokes lost.

**`/ssh` tabs.** A new `TabKind::Ssh` opens a russh-backed vt100
terminal tab against any host. Modal connection form (host / port /
user / auth method / secret / alias) with Ctrl+F key-picker against
`~/.ssh/`. Vault-stored `HostCredential` schema for aliased
connections. Same Ctrl+B prefix keys as the rest of Anvil.

**Session UX polish.** On exit, the last line shows the session ID +
`anvil --continue` / `anvil --resume <id>` resume commands. `/fork`
became O(1) (pointer-based, not O(N) clone). `/rename` lands for
sessions. `/diff` auto-shows after file-modifying turns. Esc cancel
preserves the partial assistant message instead of nuking it. `/clear`
clears workspace context, not just the active tab. `MEMORY.md` and
`ANVIL.md` hot-reload on file change.

**Release-pipeline T1 batch.** `release.sh` gains a tag-vs-HEAD
pre-flight, builds from the tagged commit instead of the working tree,
a php-lint guard runs after every WP/AnvilHub page write, and AnvilHub
`/about` sources its changelog from `changelog.json` with render-time
injection.

See [RELEASE-NOTES-v2.2.12.md](RELEASE-NOTES-v2.2.12.md) for the full
narrative.

---

## What's New in v2.2.11 — The release that pays for itself

**v2.2.11 is the biggest Anvil release since v2.0.** Fifteen workstreams,
+67 tests (1077 total), +9,700 lines of net code, and the first release
where Anvil actively works to spend fewer tokens than the agent
naturally would.

If you're upgrading from v2.2.10, here's what changes for you, in order
of how often you'll feel it:

---

### 1. Token economy: Anvil now stops paying for redundant work

A typical 30-turn session reads the same handful of files four times,
runs `git status` six times, and re-explains the project layout to the
model on every turn. v2.2.11 stops paying for that.

**File-fingerprint cache** (W11). Every file Anvil reads is fingerprinted
with sha256 + mtime + size + first-200-line summary + detected language
+ extracted key symbols, then stored at
`~/.anvil/projects/<project-hash>/file-cache/`. A `<known-files>` block
is built from the cache and injected into the system prompt, so the
model knows what files exist and what's in them **before** it ever
calls `read_file`. Cache invalidates automatically when mtime or sha256
change, so stale entries can't poison a session. Concurrent writers are
safe (atomic rename + per-call unique tmp filenames after a race fix
that took two test runs to surface). 18 tests cover the lifecycle.

**Command-output cache** (W12). `bash` calls go through a TTL-bounded
cache keyed on `(command, cwd)`. A static allowlist gates which commands
are cacheable at all — read-only `git status`, `ls`, `cat`, `pwd`,
`grep`, `find` etc. cache freely; anything that mutates state, hits the
network, or runs a tool with side effects (`rm`, `apt-get`, `curl`,
`make install`) is **never** cached even if you ask. Cached entries
record their `touched_files`; when any touched file changes, the entry
is invalidated on the next lookup. Hot-path detection counts session
invocations so frequently re-run commands surface in `/memory budget`.
27 tests.

**Net effect for you:** in long sessions on a familiar codebase, you'll
see fewer redundant `read_file` and `bash` tool calls, and the model
will reach for facts already in the system prompt instead of asking for
them again. The bigger your session, the bigger the savings.

---

### 2. Skill chaining: skills can now compose

Anvil skills already had explicit triggers. v2.2.11 adds **declarative
chains** — a skill can declare follow-up skills in its frontmatter:

```yaml
---
name: code-review
triggers: [review, "code review", "pr review"]
chains_to:
  - skill: security-audit
    when: { mentions: ["auth", "crypto", "permission"] }
  - skill: terse
    when: { user_pattern: "^/quick" }
---
```

The chain evaluator walks the graph at turn-start, **suggesting** (never
auto-running) up to depth 3 and 25 KB of additional skill text. Cycle
detection via `HashSet`, hard byte-budget enforcement, suggest-not-auto
pattern preserved end-to-end. The audit pass also caught and fixed a
real bug where parsed `chains_to:` was being discarded at filesystem-
load sites — skill chaining was effectively dead for non-bundled skills
until v2.2.11. It works now.

---

### 3. Seven default token-economy skills, bundled at compile time

Bundled skill count grows from 3 (`code-review`, `security-audit`,
`terse`) to 10. The seven new ones are loaded with zero disk I/O:

| Skill | What it teaches |
|-------|-----------------|
| `token-economy` | Meta-skill that chains to the three cache skills |
| `file-fingerprint` | Fingerprint files into the W11 cache after reading |
| `command-cache-aware` | Check the W12 cache before `ls` / `git` / `cat` / `grep` |
| `pattern-promote` | Nominate repeated lookups to durable memory |
| `cache-budget` | Audit and prune file/command caches periodically |
| `anvil-md-curator` | Rules for what belongs in ANVIL.md vs ephemeral state |
| `silent-cat` | Answer "what's in X" from the cache before reading the file |

`/skill load token-economy` chains in the three cache skills
automatically. From that turn forward, the session burns fewer tokens
on repeat lookups. Each skill is ≤200 lines, has valid frontmatter, and
the chain references resolve to real bundled skills (a regression test
enforces this).

---

### 4. `/memory` — the inspector you've been missing

A new top-level slash command surfaces every memory tier Anvil can read
or write. Eight subcommands, eight tiers, all addressable:

```
/memory                    one-line summary across every tier
/memory show <tier>        dump one tier's contents
/memory inspect <key>      search every tier for a key
/memory promote <nom-id>   accept a pending nomination → ANVIL.md
/memory forget <key>       remove an entry / reject a nomination
/memory why                explain what's in the system prompt right now
/memory budget             byte + estimated-token usage per tier
/memory prune              evict stale entries
```

Tiers covered: `anvil-md`, `vault`, `private`, `nominations`, `daily`,
`file-cache`, `cmd-cache`, `goals`. Tab-completion is grammar-driven —
`/memory show <Tab>` enumerates the 8 valid tier names. The
implementation is pure formatting and dispatch on top of existing
runtime APIs, no new storage. 14 handler tests.

`/memory why` is the killer one. It finally answers the question every
agent user has wanted answered: **what exactly is the model seeing
right now?**

---

### 5. AutoPromoter: durable knowledge without auto-writes

`crates/runtime/src/auto_promote.rs` adds a session-scoped engine that
counts file reads, command runs, and stated facts. When a threshold is
crossed (5 reads of the same file, 3 runs of the same command, 2
re-statements of the same fact), it emits a nomination JSON file under
`~/.anvil/nominations/` for the user to review via `/memory promote`.

**Pure suggest-not-auto.** Nothing is automatically written to ANVIL.md,
the vault, or any persistent store. The user is the only source of
promotion authority. Path canonicalization (`/var` → `/private/var` on
macOS) and whitespace normalization (`git    status` ≡ `git status`)
mean equivalent operations consolidate into one nomination instead of
four. 9 tests.

This is the foundation for v2.3's session continuity — durable memory
that's curated, not accumulated.

---

### 6. Project instruction file is now `ANVIL.md`, not `CLAUDE.md`

Anvil already loaded `ANVIL.md` (and `.anvil/ANVIL.md`,
`ANVIL.local.md`, `.anvil/instructions.md`) — but every user-facing
mention referenced "CLAUDE.md", a borrowed name from a different tool.
v2.2.11 cleans that up:

- The bundled curator skill is renamed `anvil-md-curator`
- `/init`, `/pin`, `/unpin` help text says ANVIL.md
- The `/memory` tier id is `anvil-md`
- Nominations promote into ANVIL.md, not CLAUDE.md
- Detailed `/help memory` describes ANVIL.md

Existing ANVIL.md files are read as-is. If you've been keeping project
instructions in a CLAUDE.md (legacy from the Claude Code era), rename
it; Anvil will pick it up next session.

---

### 7. Ten parity-and-correctness workstreams (W1–W10)

These don't have catchy names, but they close real gaps and are now in
your daily path:

| | What changed | Why you'll feel it |
|---|---|---|
| **W1** | Hook events catch-up: SessionStart/End, FileChanged, CwdChanged, PermissionRequest/Denied, PostToolBatch, Notification all reach hook handlers | You can wire workflow automation around any of these events without patching Anvil |
| **W2** | Effort / reasoning slider (`/effort low|medium|high|max`) maps per-provider — Anthropic thinking budget, OpenAI `reasoning.effort`, Gemini `thinkingBudget` | One control across providers; no more "is it thinking on this model?" |
| **W3** | Goal persistence — file-backed JSON at `~/.anvil/goals/<project-hash>/<id>.json` with atomic writes | Multi-session goals survive restarts. The first attempt accidentally shipped a stub; this is the redone real implementation |
| **W4** | Named profiles — `[profiles.<name>]` config sections with active-profile selection | Switch between work/personal/sandbox configs without editing settings.json |
| **W5** | Published JSON Schema (`anvil --emit-schema`, draft 2020-12, all keys covered) | Editor autocomplete and validation for `~/.anvil/config.json` |
| **W6** | OpenTelemetry events — opt-in, redact-by-default, feature-flagged. `permission_decision` event newly wired (was orphaned in v2.2.10) | Real observability for self-hosted and team deployments |
| **W7** | Custom output styles — drop a markdown file into `~/.anvil/output-styles/<name>.md`, select via `/output-style <name>` | Per-project tone/format presets without forking the binary |
| **W8** | Reviewer-agent approval gate — deterministic regex scanner (not an LLM call) for high-risk operations | Cheap, fast, no API spend on safety checks |
| **W9** | `anvil mcp-server` mode — Codex parity, MCP stdio transport, exposes every Anvil tool over MCP | Use Anvil's tools from inside Claude, Codex, or any MCP-aware client |
| **W10** | `requirements.toml` admin policy floor — parsed at startup; `bypassPermissions` string now correctly parses to `DangerFullAccess` | Enforce minimum permission policies across a team or fleet |

Plus three round-2 audit fixes: W3 stub-replace, W1 dead-wired hooks
caught + tested, W6 permission_decision wiring + W5 schema tightening.

---

### 8. Quality bar

A deep audit pass after the W1–W10 merge uncovered (and fixed):

- The **`chains_to` propagation bug** (real correctness — skill chaining
  silently dead for non-bundled skills before v2.2.11)
- A **W11 atomic-write race** exposed by the new concurrent-store test
  (`subsec_nanos` collisions between threads on a fast machine — fixed
  with a process-local `AtomicU64` counter)
- Cosmetic dupes: a redundant `CommandCacheManager::is_cacheable`
  delegate, a dangling W13 doc-comment fragment

**Workspace tests:** 1077 pass on `cargo test --workspace --release --
--test-threads=1`. The serial-test verification caught the W11 race
that parallel runs masked. (+67 from v2.2.10's 1010.)

---

### Why v2.2.11 matters

Up to v2.2.10, Anvil was a faithful agent CLI. v2.2.11 is the first
release where Anvil is **actively trying to be cheaper, more
introspectable, and more memory-aware** than the model alone would be:

- The cache layers turn redundant work into free turns
- Skill chaining lets behaviour compose without orchestrator scripts
- `/memory` makes the previously invisible model state inspectable
- AutoPromoter turns lived experience into reviewable durable memory
- W1–W10 close the door on the "this works on Claude Code but not
  Anvil" parity drift

Full release notes: **[RELEASE-NOTES-v2.2.11.md](./RELEASE-NOTES-v2.2.11.md)**.

---

## What's New in v2.2.10 — TUI usability patch

Three fixes for issues visible in real v2.2.9 sessions on macOS Terminal.app.

### Long lines wrap instead of truncating with `…`
The chat content paragraph used to right-truncate every line at the
terminal edge — long prompts and assistant responses lost their tails.
v2.2.10 uses ratatui's built-in word-wrap so messages flow to the next
line cleanly.

### Native drag-to-select works in every terminal
Mouse capture stole text selection on macOS Terminal.app and many other
emulators. The Shift+Drag pass-through workaround only worked on iTerm2,
Windows Terminal, and Linux VTEs. v2.2.10 disables mouse capture by
default — drag-to-select with no modifier works everywhere now.

If you want mouse-wheel scrolling in chat / configure overlay, opt back
in with `ANVIL_TUI_MOUSE=1`.

### Tool-result lines tell you what actually happened
`bash [ok]: {`, `TeamCreate [ok]: {`, `TeamAddMember [ok]: {` told the
user nothing. v2.2.10 parses each tool's JSON output per-tool:

- `bash` shows the first non-empty stdout line + multi-line indicator
- `read_file` shows line count
- `write_file` / `edit_file` show the path
- `glob_search` / `grep_search` show match count
- generic tools fall back to `name=…`, `id=…`, or list top-level keys

You'll see `bash [ok]: ls -la (+12 more lines)` instead of `bash [ok]: {`.

## What's New in v2.2.9

### Anthropic prompt caching — every request now caches at 1h TTL
Anvil never sent `cache_control` to Anthropic, which meant every turn
re-billed the full system prompt + tool definitions at full input rate.
v2.2.9 injects two cache breakpoints (`{type:"ephemeral", ttl:"1h"}`) on
every Anthropic request — one on the system prompt, one on the last tool
definition. For long sessions, the cost reduction is substantial.

### `anvil project purge`
Wipe per-workspace state without touching the vault, settings, or OAuth
credentials. `anvil project purge [path] [--dry-run|-n] [--yes|-y]
[--all]`. Removes `~/.anvil/sessions/<hash>/`, `daily/<hash>/`,
`private/<hash>.enc`, `file-history/<hash>/`. Useful when archiving a
project or before publishing a previously-private repo.

### Hooks can invoke MCP tools directly
`{"type":"mcp_tool","server":"culpur-infra","tool":"redact","input":{...}}`.
Powerful for vault-scrubber-as-hook, post-tool redaction, etc. All MCP
failure modes (no invoker, server unavailable, transport error) become
warnings — never deny — so an unhealthy MCP server can't crash a turn.

### `--plugin-dir <zip>` and `--plugin-url <https-url>`
Try plugins without installing. HTTPS-only, magic-byte-validated zip
extraction, path-traversal-safe, 50 MiB cap, optional
`--plugin-sha256 HEX` for integrity verification.

### `alwaysLoad: true` on MCP servers
Tools from frequently-used servers (e.g. `culpur-infra`, `qmd`) skip
ToolSearch deferral and are immediately available — no extra discovery
round-trip per session.

### OAuth login works from SSH/WSL/containers
`anvil login` now races the localhost callback against a stdin paste
prompt — whichever completes first wins. If `TcpListener::bind` fails,
auto-fall-back to paste-only with manual-redirect URL. Accepts bare codes,
`code#state=…` suffixes, and full callback URLs.

### `settings.json` is field-tolerant
A stray comma, malformed hook entry, or wrong-shape oauth block no longer
nukes the entire settings file. Bad sections warn-and-skip; good sections
still apply.

### Bash tool survives a vanished CWD
`rm -rf $PWD` mid-session no longer breaks every subsequent Bash call.
Anvil falls back to `$HOME` → `/tmp`, calls `set_current_dir()` so the
process recovers, and emits a one-shot stderr warning.

### Anthropic streaming has a dead-air timer
The OpenAI-compat path got a 5-min dead-air timeout in v2.2.8; the
Anthropic path was overlooked. v2.2.9 fixes that — uses `Instant::now()`
(monotonic, wake-from-sleep safe), resets on every chunk including
`thinking_delta`. Override via `ANVIL_STREAM_DEAD_AIR_MS`.

### TUI dialogs scroll on overflow
The `/configure` overlay (MainMenu 17, WidgetPicker 36, PresetPicker 16,
Notifications 10, Search) and the slash-command completion popup (21+
entries) now route PgUp/PgDn/Home/End/mouse-wheel through a new
`ListViewport` primitive — no more invisibly-truncated screens.

### `/usage` and `/stats` aliases for `/cost`
CC v2.1.118 parity — three names, same handler.

### Previous Releases

- **v2.2.9**: Anthropic prompt caching (1h TTL on system + last tool), `anvil project purge`, MCP-tool hooks, `--plugin-dir <zip>` / `--plugin-url`, OAuth paste-code fallback, `alwaysLoad: true` on MCP servers, settings.json partial-tolerance, bash CWD vanish recovery, Anthropic stream dead-air timer, `/usage` and `/stats` aliases, scrollable TUI overlays — Claude Code v2.1.118 → v2.1.131 parity catch-up
- **v2.2.8**: Trait-based agent composition (`/agent compose`), skill front-matter triggers (suggest-not-auto), prompt-type hooks, three-arm skill-eval harness, `precise`/`condensed` output styles, plugin loader forward-compat, embedded bundled plugins
- **v2.2.7**: Cross-OS installers with SHA256 verification, `anvil upgrade`, shell completions, curated Ollama menu, Windows env fixes, release-pipeline hardening
- **v2.2.6**: 17 web config panels, full Status Line editor in browser, AnvilHub installer, deep hierarchical autocomplete, 8 previously-broken TUI handlers restored
- **v2.2.5**: Intelligent memory system — 6-tier architecture, self-improving knowledge base
- **v2.2.4**: Security hardening — 17 audit findings resolved, constant-time HMAC, zero warnings
- **v2.2.3**: Six major features — interactive widget editor, agent types, MCP config panel
- **v2.2.2**: Customizable widget-based status line with 8 presets
- **v2.2.1**: URL rendering fix, context-aware vault form
- **v2.2.0**: Typed credential vault — 21 credential types, category tabs, visual manager
- **v2.1.2**: Credential auto-detection, egress control, signed transcripts
- **v2.1.1**: Live streaming responses, remote control, thinking mode
- **v2.1.0**: AES-256-GCM encrypted vault, file sandbox, modular architecture
- **v2.0.0**: Full Claude Code parity

---

<!-- v2.2.8 details retained below for context -->

## What's New in v2.2.8

### Trait-based agent composition — `/agent compose`
A 30-trait catalogue (expertise × personality × approach) composes thousands of agent variants from one YAML file. `/agent compose security,skeptical,first-principles "audit crates/runtime/src/oauth.rs"` assembles a system prompt in locked order (intro → expertise → personality → approach → task), spawns a subagent turn, renders the response. Dimension conflicts hard-error by default to force intentional composition. Browse the catalogue with `/agent traits`. Adapted from Miessler's `Personal_AI_Infrastructure`.

### Skill front-matter triggers with suggest-not-auto
Skills now declare YAML front-matter `triggers: [keyword, "phrase"]`. Anvil scans your prompt with case-insensitive whole-word matching and **suggests** relevant skills instead of silently injecting them (auto-injection would be a prompt-injection vector when Anvil is shipped to others). The user confirms via `/skill load <name>`. Three bundled skills ship as reference: `security-audit`, `code-review`, `terse`. Adapted from Miessler's `USE WHEN` descriptions + SuperClaude's `commands/*.md` triggers.

### Prompt-type hooks
Plugin lifecycle hooks can now inject a string into the next model turn instead of only running shell commands. `{"type":"prompt","body":"Verify the {tool_name} on {cwd}"}` with variable interpolation. Backward-compatible with bare-string command hooks. The model cannot write its own prompt-hooks (self-modifying-prompt vector).

### Three-arm skill evaluation harness — `anvil skill-eval`
Honest measurement of whether a skill helps: `__baseline__` (no system prompt) vs `__terse__` ("Answer concisely.") vs `<skill>` ("Answer concisely.\n\n" + SKILL.md). Every report bakes in three honest caveats: directional tokenizer, measures count-only (not fidelity/latency/cost/quality), near-zero delta = skill doing nothing useful. JSON snapshots committed to git for diff-against. Adapted from `caveman`'s `evals/`.

### Output style — `precise` (default) vs `condensed`
User-selectable global response style: `/output-style precise` keeps full sentences; `/output-style condensed` activates the bundled terse skill. **Anvil never auto-applies condensed** — always an explicit choice. Auto-Clarity rules inside `terse` still fire for security warnings, irreversible actions, multi-step procedures, and consent confirmations even when condensed is active.

### Plugin loader is forward-compatible
A single bad plugin manifest used to crash the entire binary at startup. v2.2.8 isolates per-plugin failures — `PluginLoadDiagnostic` surfaces structured warnings on stderr and the other plugins continue loading. Exactly the class of bug that bricked a v2.2.7 binary reading v2.2.8 tagged-hook manifests.

### Bundled plugins are embedded in the binary
No more `env!("CARGO_MANIFEST_DIR")` — the bundled plugin tree is now embedded via `include_dir` and materialized to `~/.anvil/plugins/bundled/` on first run with SHA-based fingerprint for idempotent updates. Homebrew users' bundled plugins are finally visible; developers' installed binaries are no longer wired to their live source tree.

### Claude-Code-parity bug fixes
- 429 `Retry-After` is now a minimum, not authoritative (Claude Code v2.1.98 parity)
- 5-minute stream dead-air timeout — override via `ANVIL_STREAM_DEAD_AIR_MS` (v2.1.110 parity)
- Request timeout configurable — override via `ANVIL_API_TIMEOUT_MS` (default 10 min) (v2.1.101 parity)
- `/model` warns on mid-conversation switch (uncached re-read — v2.1.108 parity)
- DangerFullAccess stability invariants + subagent mode inheritance (v2.1.97/v2.1.98 parity)

### Previous Releases
- **v2.2.7**: Cross-OS installers with SHA256 verification, `anvil upgrade`, shell completions, curated Ollama menu, Windows env fixes, release-pipeline hardening
- **v2.2.6**: 17 web config panels, full Status Line editor in browser, AnvilHub installer, deep hierarchical autocomplete, 8 previously-broken TUI handlers restored
- **v2.2.5**: Intelligent memory system — 6-tier architecture, self-improving knowledge base
- **v2.2.4**: Security hardening — 17 audit findings resolved, constant-time HMAC, zero warnings
- **v2.2.3**: Six major features — interactive widget editor, agent types, MCP config panel
- **v2.2.2**: Customizable widget-based status line with 8 presets
- **v2.2.1**: URL rendering fix, context-aware vault form
- **v2.2.0**: Typed credential vault — 21 credential types, category tabs, visual manager
- **v2.1.2**: Credential auto-detection, egress control, signed transcripts
- **v2.1.1**: Live streaming responses, remote control, thinking mode
- **v2.1.0**: AES-256-GCM encrypted vault, file sandbox, modular architecture
- **v2.0.0**: Full Claude Code parity

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
anvil upgrade          # preferred — SHA256 verified, atomic swap
anvil --update         # legacy alias, same behavior
```

### Install health check

```bash
anvil --check          # prints a per-dependency readiness checklist
anvil --setup          # re-runs the first-run wizard
anvil --uninstall      # removes the binary + completions
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
| Windows x86_64 | `anvil-x86_64-pc-windows-gnu.exe` |

Every binary is published with a sibling `.sha256` checksum file on the GitHub release, and an out-of-band copy at `https://anvilhub.culpur.net/sha256/<binary>.sha256`. The installers and `anvil upgrade` check both sources and abort on failure — no unverified binary ever lands on disk.

### Shell completions

Completion scripts ship in `install/completions/` and are wired automatically by `install.sh` / `install.ps1`:

| Shell | Path |
|---|---|
| bash | `install/completions/anvil.bash` |
| zsh | `install/completions/anvil.zsh` (add to `$fpath`) |
| fish | `install/completions/anvil.fish` (drop in `~/.config/fish/completions/`) |
| PowerShell | `install/completions/anvil.ps1` (dotted into `$PROFILE`) |

All four cover the full surface: 101 slash commands, every subcommand, global flags, provider names, and model names.

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

### Live Streaming & Remote Control
Responses stream token-by-token with real-time TUI rendering. Share sessions via `/remote-control` — viewers connect through any browser with a pairing code. Full multi-client WebSocket relay.

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

- [v2.1.1](docs/releases/2.1.1.md) — Live streaming, remote control, thinking mode
- [v2.1.0](docs/releases/2.1.0.md) — Encrypted vault, file sandbox, modular architecture
- [v2.0.0](docs/releases/2.0.0.md) — Claude Code parity
- [v0.1.0](docs/releases/0.1.0.md) — Initial release

---

## License

See the repository root for licensing details.
