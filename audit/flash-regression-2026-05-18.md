# Flash regression audit — 2026-05-18

Task #629. The #622 fix gated the *top-level* full-screen `Clear` widget in
every layout's `render()`, but flashing on Gnome Terminal AND Alacritty during
streaming token output persists. This is an AUDIT-ONLY report: code paths
ranked by likelihood, no edits applied.

Built from: `b652081` on `v2.2.14-phase1`. Binary verified post-#622, post-#626,
post-#627.

## What #622 actually shipped (re-verified)

1. `crates/anvil-cli/src/tui/mod.rs:1014-1042` — `commit_pending_redraw` soft
   path. `TextDeltaBatch`/`Spinner`/`KeyEvent` route to `draw()` + flush. No
   `terminal.clear()`. Correct.
2. `crates/anvil-cli/src/tui/layouts/{classic,vertical_split,three_pane,journal}.rs`
   — top-of-render `frame.render_widget(Clear, size)` is gated on
   `snap.dirty_regions.contains(DirtyRegions::ALL)`. Correct.
3. `crates/anvil-cli/src/tui/redraw.rs:200-219` — `last_committed_dirty` plumbing.
   Correct.
4. `crates/anvil-cli/src/tui/mod.rs:1515` — snapshot propagates the dirty set
   to layout renderers. Correct.

So the four explicit `frame.render_widget(Clear, size)` calls (one per layout)
no longer fire during streaming. The #622 fix at the top-level is wired
correctly and the tests pass. Something else must be firing.

## Suspect rank (by likelihood of being the streaming flash source)

### 1. [HIGHEST] Sub-region `frame.render_widget(Clear, ...)` inside layout renderers — never gated

The #622 fix gated ONLY the top-level full-screen `Clear`. The sub-region
Clears that fire UNCONDITIONALLY every frame were never touched and they cover
nearly the entire screen between them. Per ratatui semantics a sub-region
Clear followed by a Paragraph that re-paints the area should NOT cause a flash
(Paragraph::render at `ratatui-widgets-0.3.0/src/paragraph.rs:410` does
`buf.set_style(area, self.style)` over the whole area before content), BUT
this is the single largest delta between the #622 fix and the documented
anti-pattern — and it is the simplest hypothesis to falsify with a `script`
capture.

Specific call sites that fire on every streaming frame:

- `crates/anvil-cli/src/tui/layouts/classic.rs:299` — `render_widget(Clear, content_area)`.
  `content_area` is the entire scrollback region (most of the screen). Fires
  inside the non-SSH branch on every `draw()` including TextDeltaBatch frames.
- `crates/anvil-cli/src/tui/layouts/classic.rs:338` — `render_widget(Clear, panel_area)`
  inside `render_agent_panel`. Fires whenever the agent panel is visible.
- `crates/anvil-cli/src/tui/layouts/classic.rs:428` — `render_widget(Clear, area)`
  inside `render_memory_block`. Fires every frame on terminals ≥ 30 rows.
- `crates/anvil-cli/src/tui/layouts/vertical_split.rs:636` — `render_widget(Clear, content_area)`.
  The deck content area (most of the right pane).
- `crates/anvil-cli/src/tui/layouts/vertical_split.rs:609` — `render_widget(Clear, header_area)`.
- `crates/anvil-cli/src/tui/layouts/vertical_split.rs:193` — `render_widget(Clear, area)`
  inside `render_rail`. Full-height rail column.
- `crates/anvil-cli/src/tui/layouts/three_pane.rs:178,300,374` — `render_widget(Clear, area)`
  on FOCUS/LOG/CONTEXT panes. Together these cover the full screen.
- `crates/anvil-cli/src/tui/layouts/journal.rs:116,143,330` — header/input/body
  Clears. body Clear is the entire journal scrollback.

This is the most likely flash source on terminals where `Clear`-widget cells
emit a distinct background-erase per cell (Gnome VTE / Alacritty handle `\x1b[K`
+ `\x1b[39m\x1b[49m` differently than macOS Terminal.app).

### 2. ratatui Show-cursor + MoveTo emitted on EVERY frame

`ratatui-core-0.1.0/src/terminal/terminal.rs:469-475` (the `try_draw` epilogue):

```rust
match cursor_position {
    None => self.hide_cursor()?,
    Some(position) => {
        self.show_cursor()?;            // \x1b[?25h
        self.set_cursor_position(position)?;  // \x1b[<y>;<x>H
    }
}
```

Anvil layouts always call `frame.set_cursor_position`, so `Some(...)` always
fires. Both `show_cursor` and `set_cursor_position` use crossterm's `execute!`
which flushes immediately (see `ratatui-crossterm-0.1.0/src/lib.rs:288-303`).

On Gnome Terminal + Alacritty during a streaming loop firing at ~20-80ms
intervals, the `\x1b[?25h` re-show interacting with the terminal's native
cursor blink can produce visible cursor flicker that the user may describe as
"flash". Not full-screen, but rhythmic, and synchronous with token streaming.

This is intrinsic to ratatui and cannot be fixed at the Anvil layer without a
different approach (`hide_cursor` for the duration of streaming, only re-show
at idle).

### 3. Per-cell SGR storm from large pending-text deltas

Streaming text grows the `pending` string. Every TextDelta widens the visible
content_area diff. ratatui-crossterm `draw()` emits per-cell
`MoveTo` + `SetColors` + `Print` for each changed cell. Many cells changing
per 20-80ms frame on a wide terminal could produce visible cell-by-cell
repaint that the user perceives as flickering — especially in Gnome VTE where
the rendering pipeline batches differently than macOS Terminal.app.

This is NOT a code bug per se but is the structural reason streaming is
visually busy. A frame-rate cap (already partially in place via
`RedrawScheduler::frame_budget = 16ms`) could be tightened to 50ms or even
100ms during streaming. The current `commit_pending_redraw` path bypasses the
scheduler's frame-budget gate entirely (it uses the `redraw_pending` flag
directly), so the 60fps cap is effectively not enforced for TextDelta batches.

### 4. `redraw_pending` triple-commit per wait-loop iteration

`crates/anvil-cli/src/tui/mod.rs:2428, 2455, 2470, 2488, 2508, 2526, 2538, 2551, 2557`
— inside `wait_for_turn_end_for_tab` the `commit_pending_redraw()` is called
up to NINE times per iteration of the loop (after drain, after key event,
after mouse tab click, after paste, after burst substitution, twice as
trailing draws). Each call drains the gate. None of them rate-limit. A burst
of TextDelta events in one iteration → exactly one draw fires, but the
trailing `recv_timeout`-branch commit at line 2557 can fire ANOTHER draw if a
new TextDelta arrived during the recv.

So worst case is 2 draws per ~20ms = 100 frames/sec during streaming. Even
with the soft path (no `\x1b[2J`) that's a lot of per-cell SGR writes.

### 5. `RedrawScheduler::commit_pending` is dead code

`crates/anvil-cli/src/tui/mod.rs:940-947` defines `commit_redraw()` which is
the only caller of `RedrawScheduler::commit_pending`. `commit_redraw()` is
NEVER called from anywhere (verified via `grep -rn '\.commit_redraw'` →
zero hits outside the definition).

Consequence: every `self.redraw.request(region)` call in `input_handler.rs`,
`mod.rs`, etc. is a no-op as far as triggering a paint. Only
`request_redraw_reason` (which sets the simple `redraw_pending` flag)
actually drives paints. This means the `frame_budget = 16ms` gate inside
`RedrawScheduler::commit_pending` is bypassed entirely during streaming.

This is structural rot but probably not the flash source on its own. It does
mean Suspect #4's fix (rate-limit streaming) cannot rely on the scheduler.

### 6. ratatui `autoresize` on every `draw()`

`ratatui-core-0.1.0/src/terminal/terminal.rs:312-319` — `autoresize()` queries
terminal size on every `draw()` and calls `resize()` (which emits
`terminal.clear()` → `\x1b[2J`) if the size changed. If the user's terminal
size is racing with another window manager event (workspace switch, dock
hover, etc.) during streaming, an unrelated transient resize would trigger
`\x1b[2J`. Low likelihood but easy to verify in the `script` capture.

### 7. `set_cursor_position` outside the `draw()` closure in layouts

`three_pane.rs:102-105`, `journal.rs:150-154` and the classic-layout footer
all call `frame.set_cursor_position`. These get consumed by ratatui's
`try_draw` epilogue (Suspect #2). Not a separate source — included here only
because the rate of cursor position emission scales with frame rate.

## Verified clean (NOT the flash source)

- `commit_pending_redraw` soft path (mod.rs:1014-1042) — correct.
- Top-level `frame.render_widget(Clear, size)` in each layout — gated on
  `DirtyRegions::ALL` and the gate is reachable.
- `draw_full()` (mod.rs:1061-1069) — only called from `commit_pending_redraw`
  hard path which routes only `TabSwitch`/`Other`. Not reachable from
  TextDelta during streaming.
- Alt-screen toggles (`leave_alt_screen_for_inline_op`, `restore_alt_screen`,
  `EnterAlternateScreen` at startup, `LeaveAlternateScreen` at drop) — fire
  once at TUI lifecycle boundaries, NOT per-event. Confirmed via grep.
- render.rs ClearType emissions (lines 75, 94, 112, 750, 769, 1248) — all
  `ClearType::CurrentLine`, never `All` or `FromCursorDown`. Safe.
- Modals (mod.rs:1698, modals/confirm.rs:137, modals/password.rs:171,
  provider_login.rs:344,1097, ssh_form.rs:599,768) — `Clear` is on `modal_area`
  only, and only fires when a modal is active. Not the streaming path.
- Completion popup (common.rs:245) — gated on `snap.completion_visible`. Not
  active during pure streaming.
- `RedrawReason::Other` callers — line 1079 (`force_full_repaint_after_inline_op`),
  1820 (`set_active_tab_layout`), 2394/2415/2537/2550 (TurnDone / channel
  close). None fire during pure TextDelta streaming.

## Suggested next step (after `script` capture)

Run the user's repro through `script -fq /tmp/anvil-trace.log` while a turn is
streaming, then inspect the trace:

- **If N ≥ 1 of `\x1b[2J` (ESC `[2J`) per second during streaming** →
  something is still calling `terminal.clear()` despite #622. Audit Suspect #6
  (autoresize) and look for any new `draw_full` caller introduced since
  `1bf0b7f`. `grep -c $'\e\\[2J' /tmp/anvil-trace.log` will count occurrences.
- **If `\x1b[2J` count is 0 but cursor `\x1b[?25h`/`\x1b[?25l` toggle thousands
  of times** → Suspect #2 (cursor show/move per frame). Fix at the Anvil layer
  by hiding the cursor during streaming and re-showing it on idle.
- **If neither `\x1b[2J` nor cursor toggles** → it's Suspect #1 or #3
  (per-cell SGR storm from sub-region Clear + Paragraph diff). Apply the same
  `DirtyRegions::ALL` gate to every sub-region `frame.render_widget(Clear, ...)`
  in classic.rs / vertical_split.rs / three_pane.rs / journal.rs, OR drop
  those sub-region Clears entirely (Paragraph::render already
  `buf.set_style(area, self.style)` on the whole area, so the Clears are
  belt-and-suspenders).
- **If trace shows TextDelta-driven frames firing faster than ~30fps** →
  Suspect #4. Add an explicit rate-limit to `commit_pending_redraw` for
  `TextDeltaBatch` reason (e.g. coalesce up to ~33ms).

## Reproduction tip for the user

```
script -fq /tmp/anvil-trace.log
./anvil   # start a turn, watch streaming flash, exit
exit
# inspect:
LC_ALL=C grep -c $'\e\\[2J' /tmp/anvil-trace.log
LC_ALL=C grep -c $'\e\\[?25h' /tmp/anvil-trace.log
LC_ALL=C grep -c $'\e\\[?25l' /tmp/anvil-trace.log
wc -c /tmp/anvil-trace.log
```

The byte count + erase-screen count + cursor toggle count will pick which
suspect to fix in one shot.
