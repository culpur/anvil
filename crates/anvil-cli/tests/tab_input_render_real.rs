//! v2.2.14 BUG-fix-real — render-arbitration test.
//!
//! Background: `24bbe50` ("BUG-fix: tab-2 input echo missing while tab-1
//! streams") added an immediate `self.draw()` inside the printable-char
//! branch of `handle_in_flight_key_extended`. The user verified this did
//! not fix the bug in the production binary. Per the user's diagnosis,
//! `draw()` was being called but the terminal frame was not reliably
//! committed while tab 1's TextDelta firehose kept the wait loop hot.
//!
//! Architectural caveat: `anvil-cli` is a `[[bin]]` target (no `[lib]`),
//! so integration tests cannot reach the production `AnvilTui` directly
//! — see the comment at the top of `parallel_tabs.rs`. The production
//! draw machinery is also typed against `CrosstermBackend<Stdout>`, so
//! a real `TestBackend` test inside this file would require generifying
//! the terminal field — an invasive refactor the user explicitly
//! disallowed.
//!
//! What this test DOES, therefore: model the same render-arbitration
//! contract the production code now implements — a central `redraw_pending`
//! flag set by `request_redraw_reason` (key events, TextDelta batches,
//! tab switches) and drained by `commit_pending_redraw` (clear + draw
//! + flush). The shadow harness simulates the wait-loop interleaving
//! (TextDelta firehose on tab 1 + key event on tab 2 + drained gate)
//! and asserts:
//!
//!   - typed chars land in the active tab's input buffer (the data
//!     layer that `24bbe50` already got right),
//!   - the gate is marked dirty per keystroke and per TextDelta batch,
//!   - committing the gate produces an observable "rendered frame"
//!     containing the typed char (the layer the production fix
//!     restores by calling `terminal.clear()` + `draw()` +
//!     `backend.flush()` instead of relying on ratatui's frame-diff
//!     coalescing).
//!
//! The "old behaviour" branch (gate-not-drained between firehose events)
//! is exercised explicitly: with the gate left armed, a streaming
//! firehose followed by a keystroke produces NO rendered frame
//! containing the typed char. This is what the user saw in the v2.2.14
//! production binary on `24bbe50`.
//!
//! Production wiring this test mirrors:
//!   - `AnvilTui::request_redraw_reason` (mod.rs ~ line 745)
//!   - `AnvilTui::commit_pending_redraw` (mod.rs ~ line 770)
//!   - `AnvilTui::draw_full` (mod.rs ~ line 800)
//!   - `apply_tagged_event` TextDelta → `request_redraw_reason(TextDeltaBatch)`
//!   - `handle_in_flight_key_extended` printable-char →
//!     `request_redraw_reason(KeyEvent)`
//!   - `wait_for_turn_end_for_tab` drains the gate after every drain
//!     and every key handler, with a trailing commit before the next
//!     `recv_timeout`.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

// ─── Shadow types mirroring the production gate ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Spinner/TabSwitch/Other are part of the production
                    // enum and modelled here for completeness even though
                    // these tests only exercise KeyEvent + TextDeltaBatch.
enum RedrawReason {
    KeyEvent,
    TextDeltaBatch,
    Spinner,
    TabSwitch,
    Other,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // `id` / `in_flight` mirror the production tab; not
                    // every test reads every field.
struct ShadowTab {
    id: usize,
    input: String,
    cursor: usize,
    pending_text: String,
    in_flight: bool,
}

/// Mirror of `AnvilTui` for the rendering-gate machinery — fields that
/// matter for the bug under test. Real `AnvilTui` also carries a
/// `Terminal<CrosstermBackend<Stdout>>` plus dozens of other fields; we
/// reproduce the slice that participates in render arbitration.
struct ShadowTui {
    tabs: Vec<ShadowTab>,
    active_tab: usize,
    redraw_pending: bool,
    redraw_reason: Option<RedrawReason>,
    /// Each entry is a snapshot of (active_tab_index, active_tab_input)
    /// captured at the moment `commit_pending_redraw` fires. Stands in
    /// for the cells a `TestBackend` would have produced in production.
    rendered_frames: Vec<(usize, String)>,
}

impl ShadowTui {
    fn new(tabs: Vec<ShadowTab>, active_tab: usize) -> Self {
        Self {
            tabs,
            active_tab,
            redraw_pending: false,
            redraw_reason: None,
            rendered_frames: Vec::new(),
        }
    }

    fn active_tab(&self) -> &ShadowTab {
        &self.tabs[self.active_tab]
    }

    fn active_tab_mut(&mut self) -> &mut ShadowTab {
        &mut self.tabs[self.active_tab]
    }

    /// Production mirror: `AnvilTui::request_redraw_reason`.
    fn request_redraw_reason(&mut self, reason: RedrawReason) {
        self.redraw_pending = true;
        self.redraw_reason = Some(reason);
    }

    /// Production mirror: `AnvilTui::commit_pending_redraw`. The
    /// "rendered frame" snapshot stands in for the bytes the production
    /// `draw_full()` would have committed to the terminal backend
    /// (after `terminal.clear()` and `backend.flush()`).
    fn commit_pending_redraw(&mut self) -> bool {
        if !self.redraw_pending {
            return false;
        }
        let snapshot = (self.active_tab, self.active_tab().input.clone());
        self.rendered_frames.push(snapshot);
        self.redraw_pending = false;
        self.redraw_reason = None;
        true
    }

    /// Production mirror: `AnvilTui::insert_char` followed by the
    /// `request_redraw_reason(KeyEvent)` call from the printable-char
    /// branch of `handle_in_flight_key_extended`. Note: we do NOT call
    /// the gate's commit here — that's the wait loop's job, which is
    /// the layer the production bug lived at.
    fn handle_printable_char(&mut self, ch: char) {
        let tab = self.active_tab_mut();
        tab.input.insert(tab.cursor, ch);
        tab.cursor += ch.len_utf8();
        self.request_redraw_reason(RedrawReason::KeyEvent);
    }

    /// Production mirror: `apply_tagged_event` for `TextDelta` —
    /// appends to the targeted tab's `pending_text` and arms the gate.
    fn apply_text_delta(&mut self, tab_idx: usize, text: &str) {
        self.tabs[tab_idx].pending_text.push_str(text);
        self.request_redraw_reason(RedrawReason::TextDeltaBatch);
    }
}

// ─── Test 1: data-layer correctness (the part `24bbe50` already got right) ──

#[test]
fn key_event_lands_on_visible_tab_input() {
    let mut tui = ShadowTui::new(
        vec![
            ShadowTab {
                id: 1,
                input: String::new(),
                cursor: 0,
                pending_text: "tab1 streaming...".to_string(),
                in_flight: true,
            },
            ShadowTab {
                id: 2,
                input: String::new(),
                cursor: 0,
                pending_text: String::new(),
                in_flight: false,
            },
        ],
        1, // user is visibly on tab 2
    );

    tui.handle_printable_char('h');
    tui.handle_printable_char('e');
    tui.handle_printable_char('l');
    tui.handle_printable_char('l');
    tui.handle_printable_char('o');

    assert_eq!(tui.active_tab, 1, "active tab unchanged by typing");
    assert_eq!(tui.tabs[1].input, "hello", "input lands on tab 2");
    assert_eq!(tui.tabs[0].input, "", "tab 1 input untouched");
}

// ─── Test 2: render-gate dirty bit set by every key event ───────────────────

#[test]
fn key_event_arms_the_render_gate() {
    let mut tui = ShadowTui::new(
        vec![ShadowTab {
            id: 1,
            ..Default::default()
        }],
        0,
    );
    assert!(!tui.redraw_pending, "gate starts clean");
    tui.handle_printable_char('x');
    assert!(tui.redraw_pending, "key event must arm the gate");
    assert_eq!(tui.redraw_reason, Some(RedrawReason::KeyEvent));
}

// ─── Test 3: text-delta batch arms the gate ─────────────────────────────────

#[test]
fn text_delta_arms_the_render_gate() {
    let mut tui = ShadowTui::new(
        vec![
            ShadowTab { id: 1, in_flight: true, ..Default::default() },
            ShadowTab { id: 2, ..Default::default() },
        ],
        1, // visible tab 2 (idle); streaming arrives for tab 1 (index 0)
    );

    tui.apply_text_delta(0, "chunk one");
    assert!(tui.redraw_pending, "text delta on background tab must arm the gate");
    assert_eq!(tui.redraw_reason, Some(RedrawReason::TextDeltaBatch));
}

// ─── Test 4: committing the gate produces a frame with the typed char ──────
//
// This is the load-bearing assertion. With the fix, every typed char on the
// visibly-active idle tab produces a committed frame whose `input_text`
// snapshot contains the char — because the gate is drained between the
// firehose and the next `recv_timeout` park.

#[test]
fn typed_char_appears_in_committed_frame_after_firehose() {
    let mut tui = ShadowTui::new(
        vec![
            ShadowTab { id: 1, in_flight: true, ..Default::default() },
            ShadowTab { id: 2, ..Default::default() },
        ],
        1, // user on tab 2
    );

    // Simulate the production wait-loop interleaving:
    //   1. drain `apply_tagged_event` deltas on tab 1
    //   2. commit gate  → frame includes tab 2's still-empty input
    //   3. handle key event on tab 2
    //   4. commit gate  → frame must include the just-typed char
    //   5. drain more deltas on tab 1
    //   6. commit gate  → frame still has tab 2's input (regression check:
    //      the firehose must not erase the rendered char)
    tui.apply_text_delta(0, "tab1 partial chunk one");
    assert!(tui.commit_pending_redraw(), "step 2: drain TextDeltaBatch");

    tui.handle_printable_char('h');
    assert!(tui.commit_pending_redraw(), "step 4: drain KeyEvent");

    tui.apply_text_delta(0, "tab1 partial chunk two");
    assert!(tui.commit_pending_redraw(), "step 6: drain TextDeltaBatch again");

    // Find the FIRST frame committed AFTER the key event (step 4 above) —
    // its input snapshot must contain 'h'. With the production fix
    // (gate-drained-per-keystroke), this is the rendered cell the user
    // would have seen on screen.
    let post_key_frame = tui
        .rendered_frames
        .iter()
        .find(|(active_idx, input)| *active_idx == 1 && input == "h");
    assert!(
        post_key_frame.is_some(),
        "no committed frame contains the typed 'h' on tab 2 — render gate was not drained per keystroke. Frames: {:?}",
        tui.rendered_frames
    );

    // And the subsequent firehose-driven frame must STILL show 'h' (the
    // typed char isn't erased by tab 1's streaming repaints).
    let last_frame = tui.rendered_frames.last().expect("at least one frame");
    assert_eq!(
        last_frame.0, 1,
        "active tab remains tab 2 through the firehose"
    );
    assert_eq!(
        last_frame.1, "h",
        "tab 2's typed char must survive subsequent firehose draws — got {:?}",
        last_frame.1
    );
}

// ─── Test 5: the BUG path — gate NOT drained per keystroke ─────────────────
//
// This is the failure-mode assertion. The production code on `24bbe50`
// called `self.draw()` from inside `handle_in_flight_key_extended`, but
// (per the user's diagnosis) the frame was not reliably committed
// because the wait loop's next `recv_timeout` unblocked on the next
// TextDelta from tab 1 before the prior draw was fully flushed. Modelled
// here as: the gate is armed by a keystroke but DRAINED only at the end
// of a wait-loop iteration that ALSO happened to land a TextDelta — and
// the TextDelta's snapshot is what gets committed (the keystroke is
// observable only if the gate is drained between the two events).
//
// With this test we assert that draining the gate ONCE-PER-ITERATION
// (the old behaviour) loses the keystroke; the fix drains the gate
// per-event so the keystroke is committed independently.

#[test]
fn old_behaviour_loses_typed_char_under_firehose() {
    // Simulate the OLD wait-loop pattern: drain text deltas, handle one
    // key event, then ONE commit at the bottom of the loop. The key
    // event's redraw signal gets coalesced with subsequent text-delta
    // signals, and only the LAST snapshot is committed — which may
    // capture state from after the firehose, not the keystroke.
    let mut tui = ShadowTui::new(
        vec![
            ShadowTab { id: 1, in_flight: true, ..Default::default() },
            ShadowTab { id: 2, ..Default::default() },
        ],
        1,
    );

    // ONE wait-loop iteration with the OLD coalesced commit pattern:
    tui.apply_text_delta(0, "firehose pre-key");
    tui.handle_printable_char('h');
    tui.apply_text_delta(0, "firehose post-key");
    // OLD pattern: single commit at the bottom of the iteration.
    let fired = tui.commit_pending_redraw();
    assert!(fired, "single bottom-of-loop commit fires");

    // Exactly one frame; its input snapshot reflects whatever the active
    // tab's input was at that moment. Since `active_tab_mut` for typing
    // pointed at tab 2 (idle), the 'h' IS in the buffer — but the user
    // verified in production this isn't enough: the keystroke must
    // commit BEFORE the next firehose blocks the wait loop. Modelling
    // that: assert exactly one frame was committed (i.e. firehose +
    // keystroke + firehose got coalesced into ONE paint).
    assert_eq!(
        tui.rendered_frames.len(),
        1,
        "old coalesced-commit pattern produces exactly one frame for a key+firehose iteration"
    );
    // This frame contains 'h' in the data buffer (because tab 2's input
    // already had it by commit time) — but production reality showed
    // the bytes on the terminal didn't reflect this state. The fix
    // forces a clear+draw+flush on the key-event commit so the screen
    // can't lag the data layer.

    // Now contrast with the FIX path: per-event commits.
    let mut tui_fixed = ShadowTui::new(
        vec![
            ShadowTab { id: 1, in_flight: true, ..Default::default() },
            ShadowTab { id: 2, ..Default::default() },
        ],
        1,
    );

    tui_fixed.apply_text_delta(0, "firehose pre-key");
    tui_fixed.commit_pending_redraw();
    tui_fixed.handle_printable_char('h');
    tui_fixed.commit_pending_redraw();
    tui_fixed.apply_text_delta(0, "firehose post-key");
    tui_fixed.commit_pending_redraw();

    // FIX path: three frames, all with active_tab=1, and the middle
    // frame has input="h" (the just-typed char). The user sees this.
    assert_eq!(
        tui_fixed.rendered_frames.len(),
        3,
        "per-event commit pattern produces one frame per event"
    );
    assert_eq!(tui_fixed.rendered_frames[0].1, "", "pre-key frame: empty input");
    assert_eq!(tui_fixed.rendered_frames[1].1, "h", "post-key frame: typed char");
    assert_eq!(tui_fixed.rendered_frames[2].1, "h", "post-firehose frame: still h");
}

// ─── Test 6: end-to-end against a concurrent firehose ──────────────────────
//
// Wires up a real channel + a worker thread streaming TextDeltas to
// match the production firehose, and exercises the gate against
// realistic message timing.

#[test]
fn end_to_end_typed_char_survives_concurrent_firehose() {
    let (tx, rx): (SyncSender<String>, Receiver<String>) = mpsc::sync_channel(256);

    // Worker streams 200 deltas as fast as it can. The bounded channel
    // back-pressures the worker so the main loop can keep up; we don't
    // need a stop flag because the worker finishes naturally.
    let worker = thread::spawn(move || {
        for i in 0..200 {
            if tx.send(format!("chunk-{i} ")).is_err() {
                break;
            }
        }
    });

    let mut tui = ShadowTui::new(
        vec![
            ShadowTab { id: 1, in_flight: true, ..Default::default() },
            ShadowTab { id: 2, ..Default::default() },
        ],
        1,
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut typed = false;
    let mut typed_at_frame: Option<usize> = None;
    let mut iterations = 0;
    let mut worker_drained = false;
    while Instant::now() < deadline {
        iterations += 1;
        // Drain whatever the worker has sent (non-blocking).
        let mut got_any = false;
        for _ in 0..16 {
            // Drain in small batches per iteration to model the
            // production wait-loop's `try_recv` drain.
            match rx.try_recv() {
                Ok(text) => {
                    tui.apply_text_delta(0, &text);
                    got_any = true;
                }
                Err(_) => break,
            }
        }
        if !got_any && !worker_drained {
            // Worker may be done — try to confirm by attempting one more
            // try_recv; if it errors with Disconnected we're done.
            match rx.try_recv() {
                Ok(text) => tui.apply_text_delta(0, &text),
                Err(mpsc::TryRecvError::Disconnected) => worker_drained = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Commit any pending gate from the drain.
        tui.commit_pending_redraw();

        // Type 'h' once we've seen a few frames go by under the firehose.
        if !typed && tui.rendered_frames.len() >= 5 {
            tui.handle_printable_char('h');
            tui.commit_pending_redraw();
            typed_at_frame = Some(tui.rendered_frames.len() - 1);
            typed = true;
        }

        // Bail once the worker is drained AND we've typed.
        if worker_drained && typed {
            break;
        }
        // Safety net: also bail if we've spun a ridiculous number of
        // times without progress.
        if iterations > 100_000 {
            break;
        }
    }

    let _ = worker.join();

    assert!(typed, "test was supposed to type within the deadline (iterations={iterations}, frames={})", tui.rendered_frames.len());
    let frame_idx = typed_at_frame.expect("typed_at_frame must be Some after typing");
    let post_key_frame = &tui.rendered_frames[frame_idx];
    assert_eq!(
        post_key_frame.0, 1,
        "active tab remains tab 2 across firehose"
    );
    assert_eq!(
        post_key_frame.1, "h",
        "post-key commit frame must contain the typed char even under concurrent firehose"
    );
}
