# Anvil v2.2.12 — Parallel and Transparent

Released: 2026-05-11

v2.2.11 taught the agent to know itself. v2.2.12 teaches it to get
out of your way. This is the structural rewrite of how Anvil runs:
per-tab parallel inference, a live TUI during streaming, tool-call
transparency, SSH tabs, session continuity that survives sleep and
restart, scrollback that actually shows what it should, and a `/quit`
that doesn't freeze. Single-binary upgrade — no config migration,
no new dependencies. Update via `anvil upgrade` or
`brew reinstall anvil`.

This release ships on **four platforms** instead of the usual five.
Windows x86_64 is held to v2.2.13 because the new SSH agent code
hits a Unix-domain-socket assumption that doesn't compile on Windows;
fix is in flight per ROADMAP Tier 8.5 Stream A. macOS ARM64, macOS
Intel, Linux x86_64, and Linux ARM64 binaries are signed,
SHA256-verified, and uploaded to the release. Windows users on v2.2.11
are unaffected — stay there until v2.2.13.

---

## Headline: per-tab parallel inference

For every version up through v2.2.11, the entire application was
serialized on a single conversation runtime behind one mutex.
Multiple tabs were a UI illusion: fire a prompt in tab 1 and tab 2
froze until tab 1 finished streaming. Open the SSH form mid-turn?
The TUI couldn't respond. Submit from a different tab? Held as a
draft until the lock released. The interface looked like a multi-tab
terminal, but underneath it was waiting in line.

v2.2.12 makes the multi-tab illusion real. Each tab now owns its own
`Arc<Mutex<ConversationRuntime>>` and spawns turns on a dedicated
worker thread via `thread::spawn` (not `thread::scope`). The global
lock is gone. Fire a prompt in tab 1, switch to tab 2, fire another —
both stream concurrently against different providers, different
models, different keys, different cumulative-usage counters.

`TuiEvent` carries a `tab_id` field now. Stream deltas, tool calls,
permission prompts, and errors route to the correct tab without
contention. The tab bar shows `*` for unread output and `⚠` for
pending permissions, both updating live during inference on a
different tab.

The bug filings for this work — Bug 3 Commits 1 through 7 (tasks
#432–#438) — span tab_id tagging, per-tab runtime, concurrent turn
spawn, permission prompt routing through the TUI, docs + integration
tests + viewer.html audit, early return on user action, and the
modal-aware wait loop with the `/quit` dispatch fix.

Tests: 5 new per-tab inference integration tests covering concurrent
streams against the same and different providers, cross-tab
permission routing, and clean shutdown under in-flight turns.

---

## Headline: tool-call transparency

Every other AI coding tool shows you a one-line tool summary *after*
it completes. Anvil v2.2.12 shows you the actual input the model
sent the moment the call fires, in a bordered card you can expand
to see the full JSON request and the full result.

The TUI gains a `LogEntry::ToolCall { name, detail, done, is_error,
expanded, full_input, full_result }` variant. Every Glob, Grep,
Read, Write, Edit, Bash, WebSearch, and MCP tool renders as a card
with the input visible immediately — pattern, path, command, search
query — not a summary after completion. Cards animate in-place from
active to done. Errors render with red borders. Long-running Bash
commands show elapsed time.

**Ctrl+O expands any card** to the full input JSON and the full
result. Inspect exactly what the model asked for, and exactly what
came back.

A new `ToolCallActive { name, detail, full_input }` push-if-missing
handler in `crates/anvil-cli/src/tui/mod.rs` (around line 1754)
ensures the card appears at the moment of invocation rather than at
completion. Result text is captured into `full_result` so expansion
shows complete data even after the card has rolled out of the
active region.

Tests: 4 new tool-card integration tests covering render order,
expansion state, error styling, and re-collapse.

---

## Headline: SSH tabs

`/ssh host` opens a modal connection form with fields for host, port,
user, auth method, key path, passphrase, and a save-as-alias name.
The default key root is `~/.ssh`; Ctrl+F opens a bare-name resolver
that autocompletes existing key files. Saved connections go into
the encrypted vault as `HostCredential` aliases — AES-256-GCM with
the existing Argon2id KDF, per-project scopes supported.

Each SSH tab runs a real russh session with full vt100 terminal
emulation via `crates/runtime/src/ssh/`. Ctrl+B prefix keys
(tmux-style: digit to jump tabs, q to close). Resize is honored.
Colors and box-drawing render correctly. The auth chain tries Agent
→ KeyFile → Password → KeyboardInteractive in that order, with
clean fallback messages on each method failure.

This is what makes the agent + ops loop tight. An AI session in
one tab, a live SSH terminal to the box you're working on in the
next tab. Tell the agent to draft a config change, watch it land
in real time, paste the test command into the SSH tab. No context
switch, no separate terminal app, no copy-paste between windows.

The work bundle was T5-Ssh-A through T5-Ssh-F (tasks #418–#423):
SlashCommand::Ssh parser, russh driver + 4-method auth chain,
vault HostCredential schema, TabKind::Ssh vt100-backed terminal
tab, modal form overlay, and the wire-up + integration test.

Tests: 5 new SSH integration tests covering connect, auth fallback,
input forward, resize, and clean disconnect.

---

## Headline: mid-turn TUI responsiveness

Bug 1 (slash command typed mid-turn was held-as-draft instead of
executed) and the related deferred Bug 3 work were the gating items
for "the TUI is live during inference." Together they enabled:

* Ctrl+T (new tab), F2/F3 (tab switch), `/ssh` (open SSH form), and
  prompts from other tabs all respond immediately while another tab
  is mid-turn.
* `wait_for_turn_end_for_tab` returns early on any user action, not
  just on turn completion.
* Slash commands typed during a streaming response execute against
  the active tab as intended, instead of accumulating in a draft
  buffer until the response finishes.

The interface never waits for the model. You always wait for the
model.

Tests: covered by the per-tab inference integration tests above
plus targeted unit tests for the draft-vs-command dispatch path.

---

## Headline: scrollback correctness

PageUp/Up in HISTORICAL VIEW used to show only the first 1–4
characters of each assistant line — you'd see `#`, `##`, `**`, `-`
instead of full text. The pending-message line cache wasn't
invalidating as new tokens arrived, so only the initial fragment
was ever displayed.

`crates/anvil-cli/src/tui/scrollback.rs` gains a `pop_back_n(n)`
operation; `Tab` gains a `scrollback_pending_lines: usize` field;
the populate path now does a stable-log/mutable-region split where
the last log entry plus pending_text is treated as the mutable
region and rebuilt on every delta. Cache entries for in-flight
messages are invalidated on every delta.

Your full conversation is now scrollable, every line intact.

Tests: 3 new scrollback tests covering `pop_back_n_removes_trailing_lines`,
`pop_back_n_saturates_at_buffer_size`, and
`streaming_deltas_replace_not_append`.

---

## Headline: session continuity

`anvil --continue` now honors the model saved in the session's
`.meta.json` sidecar (Bug 4, task #429), so an Ollama session
reconnects to Ollama instead of failing with a missing-credentials
error on the default Anthropic provider. When Anvil exits, the
last line shows the session ID and friendly name with the exact
`anvil --continue` and `anvil --resume <name>` commands to paste
(T3-Exit-UX, task #410). `/clear` now clears workspace context
across all tabs, not just the active one (T4-N, task #416). `/fork`
uses `Arc`-shared snapshots when the log is unchanged (T3-I, task
#411). `/rename` for sessions lands as a top-level slash command
parallel to `/tab rename` (T3-J, task #412). Session friendly
names + resume-by-name + `/fork` snapshots together cover most of
the "v2.3 session persistence" scope from the prior roadmap, which
is why that label retired into the patch-shipping framing.

---

## Headline: `/quit` no longer deadlocks

`/quit` was self-deadlocking via re-entrant lock acquisition in
`record_daily` — that function acquired `active_runtime()`, kept the
guard alive, then re-acquired the same mutex on a later line and
hung because `std::sync::Mutex` is non-reentrant. The fix
restructures the lock scope: acquire once in a scoped block, clone
the data, drop the guard, then proceed to the runtime re-acquisition.

`crates/anvil-cli/src/main.rs` around line 7325 now does:

```rust
let (messages, tokens_used, tool_count) = {
    let guard = self.active_runtime();
    let session_data = guard.session();
    let messages = session_data.messages.clone();
    let tokens_used = guard.usage().cumulative_usage().total_tokens();
    let tool_count = messages.iter().flat_map(|m| &m.blocks).filter(|b| {
        matches!(b, runtime::ContentBlock::ToolUse { .. })
    }).count() as u64;
    (messages, tokens_used, tool_count)
};  // guard dropped here
```

Same pattern is now the linted norm everywhere `record_daily`-style
re-entrancy was possible. A unit test in the runtime mutex-discipline
suite guards against regression.

---

## v2.2.12 correctness work (T1–T5)

Twenty named tasks of correctness and infrastructure work landed
alongside the headline features. None are flashy individually;
together they harden the release pipeline against the v2.2.11
incidents and finish off the v2.2.11 follow-up backlog.

| Bundle | What |
|---|---|
| T1-A | `release.sh` tag-vs-HEAD pre-flight (task #404) |
| T1-B | `release.sh` builds from the tagged commit, not the working tree (task #405) |
| T1-C | `release.sh` post-write WordPress/AnvilHub php-lint guard (task #406) |
| T1-D | AnvilHub `/about` source uses `changelog.json` + render-time injection (task #407) |
| T1-#400 | TUI accepts live typing while a turn is in flight (task #408) |
| T2-E | Shared safe-edit helper crate — quote-balance + lint guard + dry-run (task #401) |
| T2-F | Find-within-context replacement for page edits (task #402) |
| T2-G | Unified `RedrawScheduler` for the TUI — kills a class of race bugs caused by scattered per-component redraw logic (task #403) |
| T3-H | Session persistence + reconnect (carry-over of #54/#55, task #409) |
| T3-Exit-UX | `anvil` exit prints session ID/name + resume commands (task #410) |
| T3-I | `/fork` O(N) → pointer (carry-over of #344, task #411) |
| T3-J | `/rename` for sessions (carry-over of #321, task #412) |
| T4-K | `/diff` auto-show after file-modifying turns (task #413) |
| T4-L | Esc cancel preserves partial assistant message (task #414) |
| T4-M | `anvil doctor --release` pre-flight self-check (task #415) |
| T4-N | `/clear` clears workspace context, not just active tab (task #416) |
| T4-O | MEMORY.md and ANVIL.md hot-reload on file change (task #417) |
| T5-Ssh-A..F | SSH tab work bundle (tasks #418–#423) |
| Tab UX | `~/.ssh` as default key root + Ctrl+F picker overlay (#424), terminal-friendly keys + clickable tabs (#425) |
| anvil(1) | Full manpage shipping with Homebrew installs (task #426) |
| First-run wizard | Mouse + theme + permission mode opt-ins before first session (task #427) |

The release-pipeline work (T1 bundle) is the durable win. v2.2.11
caught its own surface-propagation incidents *after* the binaries
shipped; v2.2.12's T1 changes make the same class of bug a hard
fail at build time. Tag-vs-HEAD pre-flight stops "tag says commit
X but HEAD is at commit Y" drift; build-from-tag stops "I built
the working tree by accident"; php-lint guard catches WordPress
syntax breakage before it 500s the live site; changelog.json
render-time injection on AnvilHub /about kills the "agent rewrote
historical entries" failure mode at its root.

---

## Audit & cleanup work

A deep audit pass after the T1–T5 merge uncovered (and either fixed
or filed):

* **Windows cross-compile broken in the new SSH agent code.** Caught
  by the release pipeline at binary-version audit time, not at build
  time, because `release.sh` cargo invocations end with `| tail -1`
  which swallows compiler errors and replaces the cargo exit code
  with `tail`'s (always 0). The Windows build *failed compilation*
  but the docker wrapper reported exit 0 and the cp step silently
  copied a stale v2.2.11 artifact from a prior build. Deferred to
  v2.2.13 along with the `| tail -1` fix — see ROADMAP Tier 8.5
  Stream A, task #441.

* **AnvilHub /about source `<TAG>` JSX parse failure.** The
  `update_pages` MCP injected literal `RELEASE-NOTES-<TAG>.md`
  text into the AnvilHub /about source, which Next.js parsed as
  an opening JSX tag with no closing element and refused to compile.
  Fixed inline with HTML-entity escaping; the MCP itself still has
  the bug (filed under task #441 v2.2.13 bundle as item 4).

* **anvil-release MCP looks for pm2 process `anvilhub-web` but the
  process is actually named `anvilhub`.** A v2.2.11-era doc note
  said `anvilhub-web (NOT anvilhub)`; the process was renamed since
  and the MCP didn't get the memo. Reference memory corrected;
  MCP fix bundled into task #441.

* **`verify` tool regex doesn't match new page structure** — false
  negatives on the WP shortcode + AnvilHub source pages even after
  successful updates. Bundled into task #441.

* **Three under-documented historical entries surfaced during
  drafting:** v2.2.11 `/about` had only 4 bullets covering the W1
  commit subject, missing the W1–W10 narrative entirely. v2.2.11
  README at `culpur/anvil` had the *opposite* problem — it listed
  W11–W15 features (which are v2.3 in-progress work) under
  v2.2.11. Both corrected in place this release; new
  `feedback-changelog-preserve-historical.md` rule codifies the
  one-time-correction-of-under-documentation exception to the
  immutability rule.

---

## Test coverage

| Crate | Tests | Δ from v2.2.11 |
|-------|-------|----------------|
| anvil-cli | 240 | +15 (5 per-tab inference + 4 tool-card + 3 scrollback + 3 others) |
| commands | 116 | +5 (SSH dispatch, /rename, /clear --all) |
| runtime | 521 | +15 (5 SSH integration + mutex discipline + RedrawScheduler + safe-edit helper) |
| api | 108 | unchanged |
| (others) | 132 | +5 (T2-E shared safe-edit suite) |
| **Total workspace** | **1117** | **+40** |

All green on `cargo test --workspace --release -- --test-threads=1`.
318 of those tests cover the new v2.2.12 surfaces directly — per-tab
inference, tool-call cards, SSH integration, scrollback correctness,
mutex discipline.

Zero warnings. Zero failures.

---

## Surfaces touched

* `runtime/src/ssh/` — new module: `driver.rs`, `keys.rs`, vt100
  state, the russh integration, auth chain, vault HostCredential
* `runtime/src/lib.rs` — `ConversationRuntime` no longer assumed
  process-singleton; tab_id flows through
* `anvil-cli/src/tui/{mod,state,scrollback}.rs` — major: per-tab
  state, tool-call cards, scrollback fix, RedrawScheduler
* `anvil-cli/src/tui/ssh_tab.rs` — new: TabKind::Ssh + vt100 render
* `anvil-cli/src/main.rs` — Bug 3 commits 1–7 spread across the
  main event loop + record_daily fix
* `anvil-cli/src/share.rs`, `anvil-cli/src/main.rs` — synthetic
  Tab init now includes `scrollback_pending_lines: 0`
* `commands/src/lib.rs` — `SlashCommand::Ssh`, `/rename`,
  `/clear --all` additions
* `scripts/release.sh` — T1-A/B/C/D pre-flight + build-from-tag +
  php-lint + changelog.json wiring
* `anvilhub/packages/web/src/app/about/page.tsx` — render-time
  changelog.json injection replaces compile-time inline
* `crates/safe-edit/` — new crate: quote-balance + lint guard +
  dry-run (T2-E)
* `docs/anvil.1` — new: full manpage

---

## Upgrade

```bash
anvil upgrade
# or
brew reinstall anvil
```

No config migration required. Per-tab runtime is transparent to
existing sessions; old sessions reload with `--continue` honoring
the saved provider/model. The SSH tab feature is opt-in via `/ssh`
and requires no config. The first-run wizard runs only on machines
where it hasn't run before; existing installs are not re-prompted.

If you want to see the new tool-call cards in action right away,
fire any tool-heavy prompt (`find me all the Bash invocations in
crates/runtime/`, for example) and watch the cards stream in. Ctrl+O
on any card to expand.

If you're on Windows, **stay on v2.2.11** until v2.2.13 ships the
cross-compile fix. The v2.2.12 binaries do not include Windows.

---

## What's next

v2.2.13 is the Windows cross-compile fix plus four release-pipeline
bugs caught during this cycle (`scripts/release.sh | tail -1`
masking exit codes, anvil-release MCP unescaped JSX injection, pm2
process name drift, verify regex false-negatives). Per ROADMAP Tier
8.5 Stream A: v2.2.13 ships when items 1, 2, and 4 of that stream
are done.

Beyond that, the backlog framing rules: no v2.3 label. v2.3-era
work (token-economy iteration, hosted Ollama via Passage, the
durable session journal, BSD cross-compile per Tier 8.5 Stream B,
the routines daemon per Tier 2) ships into whichever `v2.2.x` is
ready next. The version number is cheap; user trust is not. A v3.0
only when there's a real reason — on-disk break, CLI break, or
architectural seam users have to learn around.

For now: v2.2.12 is the release where the app gets out of your
way. Open a tab, fire a prompt, switch tabs, open SSH to your
server, watch the agent work in transparent cards while you type
the next thing. That's the contract.
