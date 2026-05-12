# Anvil v2.2.14 — Parallel tabs become solid, CC parity through v2.1.139

Released: 2026-05-12

v2.2.14 is the release that makes the v2.2.13 per-tab parallel inference
architecture actually production-ready, lands a wave of CC parity catch-up
spanning v2.1.132 through v2.1.139, finally wires two W-series modules
that had shipped as dormant code (W11 file-fingerprint cache, W15b
auto-promote engine), tightens two security boundaries discovered during
parity review, and fixes a long-standing Apple Terminal bug where the
Enter key would emit a literal newline instead of submitting the prompt.
Single-binary upgrade — no config migration, no behavior change for
existing sessions. Update via `anvil upgrade` or `brew reinstall anvil`.

This release ships on the same **seven platforms** as v2.2.13: macOS
ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD
x86_64, NetBSD x86_64. FreeBSD ARM64 and OpenBSD x86_64 remain
source-only. All binaries are SHA256-verified and signed by the release
pipeline.

---

## Headline: per-tab parallel inference, debugged to the layer

v2.2.13 introduced per-tab `ConversationRuntime` instances and concurrent
turn spawn so a user could fire a prompt on tab 2 while tab 1 was still
streaming. The architecture was sound. The implementation had three
bugs that only showed up under real concurrent use:

**1. Apple Terminal's Enter key was being eaten.** Apple Terminal sends
Enter as raw byte `0x0A`, which crossterm reports as `KeyCode::Char('j')
+ KeyModifiers::CONTROL`. A legacy handler in `input_handler.rs`
interpreted that as "insert literal newline" instead of falling through
to the real Enter handler — so on Apple Terminal, pressing Enter on tab 2
silently added a newline to the draft buffer and never submitted. The
Ctrl+J branch is gone. Enter now submits on every terminal Anvil has
been tested on.

**2. The submitted prompt vanished from tab 2's scrollback.** The idle
input path (`submit_input`) pushed a `LogEntry::User(draft)` to the
tab's log before queuing the turn. The in-flight wait-loop variant
(`handle_in_flight_key_extended`) did not — it consumed the draft via
`mem::take` and returned `SubmitChatPrompt` without recording the user
message anywhere. The model's reply would arrive on tab 2 with no
visible prompt above it, looking like a hallucinated response. Fixed:
the in-flight Enter branch now mirrors the idle path — history push,
log entry push, scroll-to-bottom, then submit.

**3. The "Thinking..." indicator never appeared on tab 2.** The
legacy active-tab dispatch path at `main.rs:4406` and `:4509` emits
`TuiEvent::ThinkLabel("Thinking...")` before calling `run_turn`. The
new per-tab `spawn_turn_for_tab` and `spawn_file_drop_turn_for_tab`
worker threads jumped straight to `rt.run_turn(...)` without that
emit. Concurrent tab-2 turns ran silently until the first `TextDelta`
arrived from the model — which can be 5–30 seconds on a cold local
model. Both per-tab spawn paths now surface `ThinkLabel` on the
target tab's sender as their first action.

These three bugs compounded into "tab 2 doesn't work" from the user's
perspective. Each fix is structural, not a workaround. The wait-loop
latency stayed at 20ms (cut from 80ms in v2.2.13 work) so the UI
doesn't lag while a background tab is streaming.

---

## Headline: CC parity catch-up through v2.1.139

The CC parity audit ran from v2.1.132 through v2.1.139, filing
twenty-one items for triage. v2.2.14 ships everything that wasn't
either already-shipped or deliberately-deferred:

**Hook ergonomics (v2.1.139)**
- Hook `args` accepts `string[]` exec form so commands with spaces in
  paths don't get word-split by the shell. The shell form still works;
  the array form is purely additive.
- `PostToolUse` hooks gain a `continueOnBlock` field. A hook that
  returns `block` no longer halts the rest of the chain by default;
  set `continueOnBlock: false` to opt back into halt-on-block.
- Hook env now carries `effort.level` alongside the existing
  `tool_name`, `tool_input`, `cwd`, and session env. Hook scripts can
  branch on the user's current /effort setting without an extra
  command.

**Per-session env propagation (v2.1.132, v2.1.133)**
- `ANVIL_SESSION_ID` is now exposed to every Bash subprocess and hook
  exec. Hooks can correlate their output to the session that fired
  them. MCP stdio children also get `ANVIL_PROJECT_DIR` so plugins
  know which workspace they're operating against.
- `ANVIL_EFFORT` env carries the active effort level into the
  subprocess environment, alongside the hook field above.
- `ANVIL_DISABLE_ALTERNATE_SCREEN=1` is the opt-out for users who
  want Anvil's TUI to stay on the primary screen — for tmux-CC users,
  screen-capture workflows, and unusual terminals where alt-screen
  causes redraw artifacts.

**Slash commands**
- `/scroll-speed <N>` adjusts mouse-wheel scroll velocity with a live
  preview. Default 3 lines per wheel notch; range 1–10. Persists in
  settings.
- `/plugin details <name>` reports the plugin's tool inventory plus a
  token-cost estimate for its bundled prompt content. Useful before
  enabling a heavyweight plugin.
- `/goal` now auto-links the active session on `/goal new` so the new
  goal inherits session context instead of starting blank. Resolves
  the collision with v2.2.11's goal-persistence work.

**Transcript view**
- `?` shows keybindings, `{`/`}` jump to previous/next user turn, and
  `v` toggles between rendered and raw view. These match CC v2.1.139's
  transcript nav exactly.

**Cross-session agent monitor**
- `anvil agents` is a new subcommand that lives outside any session.
  It opens a live monitor of all running Anvil agents on the host —
  PID, model, current state (idle / running / waiting-on-permission),
  session ID, time-on-turn. Useful for catching stuck or runaway
  agents across multiple terminal tabs or worktrees.

**Worktree baseref**
- `worktree.baseRef` is a new setting. `fresh` (default) creates each
  worktree from `origin/HEAD` of the upstream remote so the worktree
  starts clean. `head` creates from local `HEAD` so it carries
  in-progress changes. Per-workspace override supported.

---

## Headline: W11 and W15b wired into the live runtime

v2.2.11 shipped W11 (file-fingerprint cache) and W15a (`/memory`
commands) as modules with **no callsites** — the code was there but
nothing in the production path invoked it. v2.2.14 closes that gap.

**W11: file-fingerprint cache is now wired.** The cache lifecycle is
hooked into `read_file`, `write_file`, and `edit_file` tools, and a
`<known-files>` block is injected into the system prompt at session
start (and refreshed on file changes). The cache lives at
`~/.anvil/projects/<project-hash>/file-cache/` with per-call atomic
writes so concurrent reads/writes don't corrupt it. Cache invalidates
automatically when mtime or sha256 change. 18 lifecycle tests carried
over from v2.2.11 plus 3 new wire-up tests.

**W15b: auto-promote engine is now wired.** When `/memory` records a
notes entry, the auto-promote engine evaluates whether that entry
qualifies for promotion to the long-lived project knowledge base
(`.anvil/memory/promoted.md`). Trigger conditions: same fact reaffirmed
≥3 times across sessions, OR an explicit `/memory promote` invocation.
Demoted entries leave a "deprecated at <timestamp>" marker rather than
deleting silently. Wired into both `read` and `bash` tool handlers and
`main.rs` install path.

Both modules were dead code on disk in v2.2.13. They're alive in v2.2.14.

---

## Two security boundaries tightened

**ReadOnly mode cannot be bypassed by env or Edit allow rule
(CC-136-B6).** Plan mode and ReadOnly permission mode are supposed to
guarantee zero filesystem writes. A reviewer demonstrated that a
user-side `Edit` allow rule in `~/.anvil/settings.json` could grant
write capability that ReadOnly was meant to deny — the allow-rule
matcher was evaluating before the mode gate. v2.2.14 reverses the
order: mode gate first, allow rules second. No Edit allow rule, env
override, or settings precedence can escalate a ReadOnly session into
writes.

**Tool allow-rule wildcards are now safely matched (CC-139-B5).** The
matcher for rules like `Skill(name *)` was using prefix-match without
delimiter validation. A user could write `Skill(format_*)` intending
"any skill starting with format_", but a skill named
`format__rm_workspace_force` (note the underscore-r prefix) could
slip through. The new matcher inserts a delimiter check so wildcards
match against `_`, `-`, `.`, or end-of-string only — not arbitrary
character continuation.

---

## TUI architecture cleanup

Three internal cleanups land in v2.2.14 that aren't user-visible but
are worth recording for the architecture:

**TUI-1: Ctrl+C cancels mid-flight streaming.** Previously, Ctrl+C
only worked in idle state. During an in-flight turn, Ctrl+C went to
the modal-cancel buffer instead of interrupting the stream. The
in-flight wait loop now treats Ctrl+C as a hard cancel and emits a
`StreamCanceled` event to the runtime.

**TUI-2: in-flight wait loop latency cut from 80ms to 20ms.** The
poll cadence in `wait_for_turn_end_for_tab` was set conservatively at
80ms to keep CPU low during long-running turns. With per-tab parallel
inference, that 80ms is the latency floor for keystroke echo on a
non-streaming tab. Cut to 20ms; CPU cost is bounded by the existing
needs_redraw gate so total CPU is unchanged.

**TUI-3: user messages typed during stream are queued, not dropped.**
A message typed while a turn is streaming used to be discarded on
Enter unless the user pressed Esc-Enter explicitly. The wait loop now
queues mid-stream submissions so they fire as soon as `TurnDone`
arrives — same behavior as Claude Code.

---

## Other notable fixes

- **OTEL TRACEPARENT propagation (CC-DRIFT-B5)**. Anvil OTEL spans now
  carry a W3C traceparent header into subagent spawns, MCP child
  processes, and Bash subprocesses. Distributed traces no longer
  fragment at every process boundary.
- **Subagent OTel headers (CC-139-F16)**. Subagents emit OTel spans
  tagged with both `agent.id` and `parent_agent.id` so multi-agent
  workflows render correctly in distributed tracing tools.
- **autoMode.hard_deny classifier short-circuit (CC-136-F2)**. The
  auto-mode classifier was running through the full LLM-based decision
  even when a `hard_deny` rule matched. The hard-deny check now
  short-circuits before the classifier call, eliminating ~300ms per
  denied tool call.
- **`/ollama` slash commands rewired (regression).** The v2.2.11 W4
  named-profile merge accidentally dropped the `/ollama` dispatch
  table. v2.2.14 re-registers the full set: `list`, `show`, `ps`,
  `tune`, `option`, `policy`, `pull`, `rm`, `cp`, `create`, `bench`,
  `requantize`.
- **`--resume` / `--continue` on underscore session paths
  (CC-136-B5)**. Session IDs containing underscores are now matched
  correctly by `--resume <id>` and `--continue` lookup. Previously the
  glob pattern stopped at the first underscore.
- **CC-DRIFT bundle**: agent panic on missing `tools[]` (B4), `/compact`
  usage rendering (B6), MCP test parity (B8), and the F1 cross-session
  agent view (above).

---

## Under the hood

- **Workspace version bump.** `Cargo.toml` workspace version moves from
  `2.2.13` to `2.2.14`. Pure metadata change; no source impact.
- **`spawn_turn_for_tab` + `spawn_file_drop_turn_for_tab`** in
  `anvil-cli/src/main.rs` now emit `TuiEvent::ThinkLabel` on the
  target tab's sender before `run_turn` / `run_turn_preloaded`. Both
  worker thread entry points are now parity-aligned with the legacy
  active-tab paths.
- **`handle_in_flight_key_extended`** in `anvil-cli/src/tui/mod.rs`
  Enter branch pushes `LogEntry::User(draft)` and updates history
  before returning `InFlightInterruption::SubmitChatPrompt`. The
  in-flight and idle Enter paths now produce identical log output.
- **`input_handler.rs`** loses the `KeyCode::Char('j' | 'J')` +
  `CONTROL` branch. Apple Terminal's `0x0A` Enter byte now falls
  through to `KeyCode::Enter` like every other terminal.
- **Build matrix unchanged from v2.2.13** — same seven binaries, same
  builder images, same Tier-3 soft-fail on NetBSD.

---

## Quality

- **All workspace tests pass.** Unit + integration + doctest gate clean
  across 7 crates. The three TUI dispatch fixes in this release are
  covered by the per-tab parallel-dispatch integration suite carried
  over from v2.2.13 plus the new wire-up tests for W11/W15b.
- **Zero failures, zero warnings on release-profile build.**
- **Seven binaries.** macOS ARM64, macOS Intel, Linux x86_64, Linux
  ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. FreeBSD ARM64
  and OpenBSD x86_64 source-only.
- **~22 MB single binary** — no runtime dependencies.

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
