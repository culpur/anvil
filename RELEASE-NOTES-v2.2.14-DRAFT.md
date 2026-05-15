# Anvil v2.2.14 — The Cohesion Release

Released: 2026-05-DD <!-- TODO fill on release day -->

**v2.2.5 named the arc "Intelligent Memory System." It shipped foundation — six
storage tiers and a sensitivity classifier. What it didn't ship was the
cohesion: subsystems that compose, a lifecycle that promotes facts as they
mature, a retrieval policy that tells the agent to trust its own context
before reaching for the web. v2.2.14 completes that arc.**

Seven layers replace the eight ad-hoc tiers, organized by what each layer is
*for* rather than where it lives on disk. Working, Episodic, Semantic,
Procedural, Identity, Policy, Cache. Memory becomes inspectable —
`/memory show <layer>` walks the live state. Sensitive infrastructure facts
stop leaking to plaintext nominations. Permission decisions actually persist.
`/memory promote` actually promotes. The retrieval-order policy block lands
in the system prompt, and the agent finally trusts its own context before
reaching for the web.

Plus: a Claude-Code-parity catch-up wave (v2.1.132 → v2.1.139), the tab-2
parallel-inference fixes that make v2.2.13's per-tab `ConversationRuntime`
production-ready, two security boundaries tightened, a complete migration
wizard for users moving in from Claude Code, a unified slash-dispatch path
that prevents the "stub message" class of regression for good, the OAuth
Max-plan 429 fix, a /model picker that reads live provider APIs, embedded
release notes accessible from inside the binary, and the provider catalog
expanding from 5 to 31 across the open-source ecosystem.

This is the largest Anvil release to date by code volume (111 commits since
v2.2.13), architectural depth (the seven-layer memory model is now the
runtime's organizing principle), and surface area (every public-facing slash
command that touches memory, the permission system, the prompt builder, and
the provider router has been revisited).

Single-binary upgrade — no config migration, no breaking changes for
existing sessions. Update via `anvil upgrade` or `brew reinstall anvil`.

This release ships on the same **seven platforms** as v2.2.13: macOS ARM64,
macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64,
NetBSD x86_64. FreeBSD ARM64 and OpenBSD x86_64 remain source-only. All
binaries are SHA256-verified and signed by the release pipeline.

---

## The seven-layer memory model

Up through v2.2.13, Anvil had eight ad-hoc memory tiers — six storage tiers
introduced in v2.2.5 plus QMD plus the W11 file cache and the W15 nominations
— each with its own commands, its own format, and its own retrieval path.
That collection worked, but it had no organizing principle: facts could
live in two tiers, nominations leaked into the system prompt, permission
decisions were stored but never re-read, and `/memory promote` was a stub.

v2.2.14 reorganizes Anvil's memory around **seven layers**, named by purpose:

| Layer | Purpose | Lifetime |
|---|---|---|
| **L1 Working** | Everything the model sees this turn | Per-turn |
| **L2 Episodic** | Conversation history + daily summaries | 90 days (configurable) |
| **L3 Semantic** | ANVIL.md + promoted facts + QMD collections | Long-lived |
| **L4 Procedural** | Goals + skills + cron + routines | Long-lived |
| **L5 Identity** | Labels and encrypted vault keys | Permanent + locked-at-rest |
| **L6 Policy** | Permission grants + auto-deny + egress | Long-lived |
| **L7 Cache** | File fingerprints + command output + QMD index | LRU, size-capped |

Every layer is **introspectable** via `/memory show <layer>`. Every
slash-command verb that names a tier (`/memory show file-cache`,
`/memory show nominations`, `/memory show daily`) now resolves through the
unified layer model — the old verbs still work as aliases that emit a
deprecation note pointing at the new name. Layer composition is explicit:
L3 *Semantic* pulls from MemoryManager, the ANVIL.md store, and the
`anvil-semantic` QMD collection in one resolved view. L4 *Procedural*
composes goals + skills + cron + routines so the user sees their full
operational context in one frame.

**Why this matters**: before v2.2.14, "where does this fact live?" was a
question only the runtime could answer, and even then it required reading
the source. After v2.2.14, the user can ask the runtime, and the answer is
deterministic.

---

## The retrieval-order block — the agent trusts its own context

The single most-felt change in this release: a new block at the top of every
system prompt that tells the agent the **order** in which it should look for
information:

```
1. The current turn (Working)
2. Recent conversation (Episodic)
3. Project knowledge (Semantic / ANVIL.md)
4. Loaded skills + tools (Procedural)
5. The vault / identity (Identity)
6. Permission policy (Policy)
7. Caches (Cache)
8. Web — only after 1–7 have been exhausted
```

Before v2.2.14, asking "what version of Anvil are you?" frequently triggered
a web search to `crates.io` or GitHub. The retrieval-order block flips that
default: the agent first checks its own embedded `--version` answer,
the in-context system prompt, and the ANVIL.md store before reaching for the
network. v2.2.14 also embeds release notes into the binary itself
(`/changelog` no longer hits the web), so the agent's own release history
is available offline.

---

## Memory becomes safe

Three layers had **safety gaps** in v2.2.13 that v2.2.14 closes.

**L5 Identity — zero-byte invariant enforcement.** The vault subsystem was
correct, but nothing prevented decrypted vault material from being captured
into L7 *Cache* outputs (e.g., a Bash command that printed a secret could
land in `/tmp/cache.json` indefinitely). v2.2.14 adds the `is_l5_path`
sentinel and a path-gate at every cache store/lookup site, plus a
zero-injection integration test that fails the build if any L5 byte appears
in any rendered prompt section. The classifier (`vault::scan::classify_learning`)
is now wired into the nomination-emit path, behind `ANVIL_L5_AUTOROUTE`,
so secrets that *would* have leaked to plaintext nominations are routed
to the vault instead.

**L6 Policy — `PermissionMemory` is now load-bearing.** Permission grants
were being stored in `~/.anvil/permissions.json` since v2.2.5, but the
permission gate never read them — every grant decision was re-prompted on
every session. v2.2.14 wires `PermissionMemory` into the gate (behind
`permissions.use_permission_memory`, default on), introduces the
`PermissionEffect { Allow, Deny, Prompt }` enum so "Prompt" is a first-class
decision, and surfaces the active permission set via `/memory show policy`.
Permission decisions persist across sessions; the user is no longer asked
the same question twice.

**L3 Semantic — `/memory promote` writes through.** The promotion verb
existed in v2.2.5 and did nothing. v2.2.14 makes it real: `/memory promote
<entry>` writes through to `ANVIL.md` with atomic-rename semantics
(no torn writes), updates the MemoryManager cache, and registers the
promoted entry with the `anvil-semantic` QMD collection so it's
searchable from `/memory search`. Promoted entries carry a
`nominated_from` frontmatter pointer so the provenance survives.

---

## The tab-2 fixes (carrying forward from the original v2.2.14 work)

v2.2.13 introduced per-tab `ConversationRuntime` instances and concurrent
turn spawn so a user could fire a prompt on tab 2 while tab 1 was still
streaming. The architecture was sound; the implementation had three bugs
that only surfaced under real concurrent use.

**1. Apple Terminal's Enter key was eaten.** Apple Terminal sends Enter as
raw byte `0x0A`, which crossterm reports as `KeyCode::Char('j') +
KeyModifiers::CONTROL`. A legacy handler interpreted that as "insert literal
newline" instead of falling through to the real Enter handler — so on Apple
Terminal, pressing Enter on tab 2 silently added a newline to the draft
buffer and never submitted. The Ctrl+J branch is gone. Enter now submits on
every terminal Anvil has been tested on.

**2. The submitted prompt vanished from tab 2's scrollback.** The idle
input path (`submit_input`) pushed a `LogEntry::User(draft)` to the tab's
log before queuing the turn. The in-flight wait-loop variant
(`handle_in_flight_key_extended`) did not — it consumed the draft via
`mem::take` and returned `SubmitChatPrompt` without recording the user
message anywhere. The model's reply would arrive on tab 2 with no visible
prompt above it, looking like a hallucinated response. Fixed: the
in-flight Enter branch now mirrors the idle path — history push, log entry
push, scroll-to-bottom, then submit.

**3. The "Thinking..." indicator never appeared on tab 2.** The legacy
active-tab dispatch path emitted `TuiEvent::ThinkLabel("Thinking...")`
before calling `run_turn`. The new per-tab `spawn_turn_for_tab` and
`spawn_file_drop_turn_for_tab` worker threads jumped straight to
`rt.run_turn(...)` without that emit. Concurrent tab-2 turns ran silently
until the first `TextDelta` arrived from the model — 5–30 seconds on a
cold local model. Both per-tab spawn paths now surface `ThinkLabel` on
the target tab's sender as their first action.

The in-flight wait-loop latency was also cut from 80ms to 20ms so the UI
doesn't lag while a background tab is streaming. Mid-flight Ctrl+C now
cancels the active stream (previously it only worked in idle state). User
messages typed during a turn are queued and fire as soon as `TurnDone`
arrives instead of being silently dropped.

---

## Slash-dispatch unification — the stub-message class of regression cannot recur

The biggest cohesion win that's invisible to end users: all slash-command
dispatch now flows through a single canonical dispatcher
(`crates/commands/src/dispatch.rs::dispatch_slash_command`). Before
v2.2.14, slash commands had **four different dispatch sites** in
`anvil-cli/src/main.rs`, each with its own match block. New commands
sometimes only got wired into one of the four sites and silently produced
"that's not a real command" stub messages from the others.

v2.2.14 consolidates to **two dispatch sites** in main.rs (idle path +
in-flight path), both delegating to the canonical dispatcher. A
bidirectional drift test (`lib.rs::every_slash_command_variant_has_a_spec`)
fails the build if a `SlashCommand` enum variant doesn't have a corresponding
spec entry, or vice versa. Stub messages are banned outright. The
sync-LLM lint gate adds four new blocked patterns for the same purpose.

Net effect: future slash commands are correct-by-construction or the build
fails. The "I added a command but only one of the four match sites knew
about it" failure mode is dead.

---

## /model — a live picker that asks the providers

`/model` previously read from a static `MODEL_REGISTRY` in the binary,
which meant the list of available models lagged the providers by however
long it took someone to bump a constant. v2.2.14 rewires `/model` to
**lazily call live provider `/models` APIs** on first TAB, per-session
cache, with the static registry as a fallback for offline use.

`/model` also now **atomically switches** the running session's model:
provider routing, system prompt identity, and TUI chrome are updated in
a single transaction. Previously you could end up with the chrome
showing Claude while the routing pointed at qwen — a critical
correctness bug that's now structurally impossible.

Autocomplete is live-provider-aware: typing `/model` and pressing TAB
pulls the current list of accessible models from every configured
provider. OAuth-authenticated providers count as configured;
unconfigured providers are hidden.

---

## Provider catalog: 5 → 31

Anvil's provider directory expands from 5 to 31 entries spanning the
open-source ecosystem. Among the additions: more Ollama variants for
local hosting, additional commercial endpoints, and a growing set of
community-maintained gateways. The full catalog ships in the binary
and is queryable via `/model list`.

---

## Claude-Code parity catch-up — v2.1.132 → v2.1.139

A parity audit ran across CC v2.1.132 through v2.1.139, filing 21 items
for triage. v2.2.14 ships everything that wasn't already-shipped or
deliberately-deferred.

**Hook ergonomics (CC v2.1.139)**
- Hook `args` accepts `string[]` exec form so commands with spaces in
  paths don't get word-split. Shell form still works; array form is
  additive.
- `PostToolUse` hooks gain a `continueOnBlock` field. Returning `block`
  no longer halts the rest of the chain by default; opt back in with
  `continueOnBlock: false`.
- Hook env now carries `effort.level` alongside `tool_name`, `tool_input`,
  `cwd`, and session env. Hook scripts can branch on the user's `/effort`
  setting without an extra command.

**Per-session env propagation (CC v2.1.132, v2.1.133)**
- `ANVIL_SESSION_ID` exposed to every Bash subprocess and hook exec.
- MCP stdio children get `ANVIL_PROJECT_DIR`.
- `ANVIL_EFFORT` carries the active effort level into subprocesses.
- `ANVIL_DISABLE_ALTERNATE_SCREEN=1` opts out of the alternate screen for
  tmux-CC users, screen-capture workflows, and unusual terminals.

**Slash commands**
- `/scroll-speed <N>` adjusts mouse-wheel scroll velocity with live
  preview. Default 3 lines per wheel notch; range 1–10. Persists in
  settings.
- `/plugin details <name>` reports the plugin's tool inventory plus a
  token-cost estimate for its bundled prompt content.
- `/goal` auto-links the active session on `/goal new` so the new goal
  inherits session context instead of starting blank.

**Transcript view (CC v2.1.139)** — `?` shows keybindings, `{` / `}` jump
to previous/next user turn, `v` toggles between rendered and raw view.

**Cross-session agent monitor** — `anvil agents` is a new subcommand that
opens a live monitor of all running Anvil agents on the host: PID, model,
current state, session ID, time-on-turn.

**Worktree baseref** — `worktree.baseRef` setting. `fresh` (default)
creates each worktree from `origin/HEAD`; `head` creates from local
`HEAD` so the worktree carries in-progress changes.

---

## CC migration wizard — the entire Phase 6 stream

A complete pipeline for users migrating in from Claude Code. The wizard
walks settings, skills, agents, plugins, and memory through a staged
import with conflict detection and a final reconciliation report:

- `settings.json` schema translation with conflict staging
- skills import with disabled-by-default collision handling
- plugin manifest translation including v2.2.x skills/agents fields
- memory entry discovery + translation (ANVIL.md merge semantics)
- session summaries via the `--include-sessions` flag
- A final report generator that surfaces every staged conflict and every
  successful translation

`anvil import` invokes the headless run; the wizard guides interactive
use. Five composed pipelines orchestrate the full process.

---

## Two security boundaries tightened

**ReadOnly mode cannot be bypassed by env or Edit allow rule (CC-136-B6).**
Plan mode and ReadOnly permission mode are supposed to guarantee zero
filesystem writes. A reviewer demonstrated that a user-side `Edit` allow
rule in `~/.anvil/settings.json` could grant write capability that ReadOnly
was meant to deny — the allow-rule matcher was evaluating before the mode
gate. v2.2.14 reverses the order: mode gate first, allow rules second. No
Edit allow rule, env override, or settings precedence can escalate a
ReadOnly session into writes.

**Tool allow-rule wildcards are safely matched (CC-139-B5).** The matcher
for rules like `Skill(name *)` was using prefix-match without delimiter
validation. A user could write `Skill(format_*)` intending "any skill
starting with format_", but a skill named `format__rm_workspace_force`
could slip through. The new matcher inserts a delimiter check so wildcards
match against `_`, `-`, `.`, or end-of-string only — not arbitrary
character continuation.

---

## W11 and W15b — the dead modules are alive

v2.2.11 shipped W11 (file-fingerprint cache) and W15b (auto-promote engine)
as modules with **no callsites** — the code was there but nothing in the
production path invoked it. v2.2.14 closes that gap.

**W11: file-fingerprint cache is wired.** The cache lifecycle is hooked
into `read_file`, `write_file`, and `edit_file`, and a `<known-files>`
block is injected into the system prompt at session start (and refreshed
on file changes). The cache lives at `~/.anvil/projects/<project-hash>/file-cache/`
with per-call atomic writes. Cache invalidates automatically when mtime
or sha256 change. Size-capped with LRU eviction.

**W15b: auto-promote engine is wired.** When `/memory` records a notes
entry, the auto-promote engine evaluates whether that entry qualifies for
promotion to ANVIL.md. Trigger conditions: same fact reaffirmed ≥3 times
across sessions, OR an explicit `/memory promote`. Demoted entries leave
a "deprecated at <timestamp>" marker rather than deleting silently.

---

## OAuth Max-plan 429 fix

Users on Anthropic Max-plan OAuth tokens were hitting empty-body 429
responses from `messages.anthropic.com` that looked like quota exhaustion
but were actually gate rejections. v2.2.14 sends both required headers in
combination — `anthropic-beta: claude-code-20250219,oauth-2025-04-20` and
the identity system block — and the gate now passes. Re-login isn't
necessary on existing tokens. Curl-probe with the two markers if you need
to confirm.

---

## Other notable fixes

- **OTEL TRACEPARENT propagation.** Anvil OTEL spans now carry a W3C
  traceparent header into subagent spawns, MCP child processes, plugin
  tool subprocesses, and Bash subprocesses. Distributed traces no
  longer fragment at every process boundary.
- **Subagent OTel headers.** Subagents emit spans tagged with both
  `agent.id` and `parent_agent.id` so multi-agent workflows render
  correctly in distributed tracing tools.
- **autoMode.hard_deny short-circuit.** The auto-mode classifier was
  running through the full LLM-based decision even when a `hard_deny`
  rule matched. Short-circuits now save ~300ms per denied tool call.
- **`/ollama` slash commands rewired.** The v2.2.11 W4 named-profile
  merge accidentally dropped the `/ollama` dispatch table. v2.2.14
  re-registers the full set: `list`, `show`, `ps`, `tune`, `option`,
  `policy`, `pull`, `rm`, `cp`, `create`, `bench`, `requantize`.
- **`--resume` / `--continue` on underscore session paths.** Session IDs
  containing underscores are now matched correctly. Previously the glob
  pattern stopped at the first underscore.
- **`/changelog` no longer freezes the TUI.** A blocking network call
  on the render thread is now async + cached.
- **EgressPolicy wired into web ops + automation ops.** The policy was
  declared but unenforced for the web tool family. v2.2.14 enforces it.
- **TeamDelegate delegations** propagate `parent_agent_id` correctly
  through the agents-live channel.
- **PromptSection migration.** The system prompt internal representation
  flips from `Vec<String>` to `Vec<PromptSection>` end-to-end, enabling
  the `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` to be a real prompt-caching
  split-point in a future release.

---

## Under the hood

- **Workspace version bump.** `Cargo.toml` workspace version moves from
  `2.2.13` to `2.2.14`.
- **Build matrix unchanged from v2.2.13** — same seven binaries, same
  builder images, same Tier-3 soft-fail on NetBSD.
- **Slash command spec drift is now caught by a test** — the
  bidirectional check fails the build if `SlashCommand` enum and
  `slash_command_specs` get out of sync.
- **Per-binary serial test tokens** were the cause of a long-standing
  parallel-test flake cluster. v2.2.14 introduces three coordinated
  fixes — `#[serial]` annotations, long-lived background thread teardown,
  and latent race surfacing — that drop the flake rate to zero in CI.
- **Release notes are embedded in the binary.** `RELEASE-NOTES-vX.Y.Z.md`
  ships inside the executable; `/changelog` reads from the embed.

---

## Quality

- **All workspace tests pass.** Unit + integration + doctest gate clean
  across 7 crates.
- **Zero failures, zero warnings on release-profile build.**
- **Seven binaries.** macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64,
  Windows x86_64, FreeBSD x86_64, NetBSD x86_64. FreeBSD ARM64 and OpenBSD
  x86_64 source-only.
- **~22 MB single binary** — no runtime dependencies.
- **111 commits since v2.2.13.** The largest delta in the Anvil 2.x line.

---

## Install

```bash
# Homebrew (macOS & Linux)
brew upgrade culpur/anvil/anvil
# or fresh install
brew install culpur/anvil/anvil

# curl installer (macOS / Linux / FreeBSD / OpenBSD / NetBSD)
curl -fsSL https://anvilhub.culpur.net/install.sh | bash

# PowerShell (Windows)
irm https://anvilhub.culpur.net/install.ps1 | iex

# Already installed
anvil upgrade
```

---

## Full changelog

https://github.com/culpur/anvil/compare/v2.2.13...v2.2.14
