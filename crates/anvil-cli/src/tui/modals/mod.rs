//! In-TUI modal overlays for interactive prompts (task #627).
//!
//! These modals replace `print!` + `read_line` confirmation prompts that
//! were left as BUG-DEFER follow-ups in the println audit (#626).  All
//! three sites — `/restart`, `/iac apply`, `/vault unlock` — used to
//! write a prompt into stdout while ratatui owned the alt-screen and
//! stdin, breaking the back-buffer and freezing the input loop.
//!
//! ## Modules
//!
//! - [`confirm`] — yes/no confirmation modal with arrow-key highlight.
//! - [`password`] — masked single-line input modal with retry-on-error.
//!
//! ## Lifecycle (follows the OAuth modal pattern from #578)
//!
//! 1. A slash command intercept in `handle_repl_command_tui` opens a
//!    modal by storing it on `AnvilTui` (e.g. `tui.confirm_modal =
//!    Some(...)`) along with a `pending_*_action` enum describing what
//!    to do on a positive resolution.
//! 2. The next `read_input` tick draws the modal as a centered overlay
//!    on top of whatever the active layout rendered.
//! 3. The input handler routes every keystroke to the modal's
//!    `handle_key` while the modal is open.
//! 4. On resolution (Yes/No/Submit/Cancel) the input handler executes
//!    the pending action and clears the modal slot.
//!
//! ## 8-axis capability contract (per `feedback-anvil-capability-contract.md`)
//!
//! 1. Definition         — `ConfirmModal` / `PasswordModal` structs +
//!                         their state machines below.
//! 2. Registration       — `pub mod confirm` / `pub mod password` here
//!                         and `pub(super) mod modals` in `tui/mod.rs`.
//! 3. Completion         — N/A (modals are not autocompleted).
//! 4. Handler            — `handle_key` on each modal returns an
//!                         action enum the host interprets.
//! 5. Dispatch           — `handle_repl_command_tui` (main.rs)
//!                         intercepts the slash commands.
//! 6. Rendering          — `render` methods + snapshot types pulled
//!                         into `LayoutSnapshot` for the draw closure.
//! 7. Gate               — `AnvilTui::has_active_modal()` accounts for
//!                         both modals; ESC always cancels.
//! 8. OTel + tests       — unit tests per modal (this directory) plus
//!                         wired-site coverage in main.rs tests module.

pub mod confirm;
pub mod password;
pub mod queue;
pub mod text_input;

use ratatui::style::Color;

/// Secondary-text color used for modal hint rows, prompt labels, and
/// option descriptions across every modal (v2.2.17 #644 Item 5).
///
/// `Color::DarkGray` historically rendered around ~25% luminance on the
/// dark-background terminals Anvil ships against, which user testing
/// flagged as unreadable.  The replacement is an explicit RGB value
/// targeting ~65% luminance — bright enough to read comfortably while
/// still visibly subordinate to the primary white text.
///
/// We also drop `Modifier::DIM` from the call-sites that use this color:
/// stacking DIM on top of an already-secondary RGB collapses the value
/// back to the unreadable range on terminals that interpret DIM as a
/// 50% luminance reduction (Gnome Terminal, kitty).
///
/// The regression test
/// `modal_secondary_text_color_meets_luminance_threshold` asserts the
/// returned value has a relative luminance >= 0.55 against the
/// rec.709 coefficient sum (0.2126·R + 0.7152·G + 0.0722·B).
pub(crate) fn modal_secondary_color() -> Color {
    // RGB(170,170,170) → relative luminance 170/255 ≈ 0.667 against the
    // rec.709 sum (each channel equal, so luminance == component / 255).
    Color::Rgb(170, 170, 170)
}

/// Relative luminance of a `Color::Rgb(r,g,b)` against the rec.709
/// coefficient sum. Returns `None` for any non-RGB variant (palette /
/// named colors depend on the terminal theme, so we don't try to
/// luminance-check them — call sites that need a luminance guarantee
/// MUST use `Color::Rgb`).
///
/// Public to the crate so the regression test can exercise it directly.
#[allow(dead_code)]
pub(crate) fn rgb_relative_luminance(color: Color) -> Option<f32> {
    match color {
        Color::Rgb(r, g, b) => {
            let r = r as f32 / 255.0;
            let g = g as f32 / 255.0;
            let b = b as f32 / 255.0;
            Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
        }
        _ => None,
    }
}

pub(crate) use confirm::{ConfirmAction, ConfirmModal};
pub(crate) use password::{PasswordAction, PasswordModal};
pub use confirm::ConfirmChoice;
// Task #579: modal queue foundation. Re-export the queue types so
// the wizard adapter (and any future multi-step caller) can target
// `tui::modals::queue::ModalQueue` without an internal-path import.
#[allow(unused_imports)]
pub(crate) use queue::{ModalQueue, ModalAnswer, QueuedModal, WizardChoiceModal};
// Task #642: TextInputModal for free-text wizard steps (Ollama URL,
// profile name, etc.). Plugged into ModalQueue via QueuedModal::TextInput
// and ModalAnswer::TextInput variants in queue.rs.
#[allow(unused_imports)]
pub(crate) use text_input::{TextInputAction, TextInputModal};

// ─── Pending action enums (held alongside the modal on AnvilTui) ─────────────

/// What to do when a `ConfirmModal` resolves to `Yes`.
///
/// `None` is set on the AnvilTui slot whenever the modal is open; the
/// pending action is consumed atomically with the modal slot on
/// resolution so a leaked action cannot fire twice.
#[derive(Debug, Clone)]
pub enum PendingConfirmAction {
    /// `/restart` (hard): respawn the current Anvil binary in-place.
    Restart,
    /// `/iac apply`: invoke `tofu apply` / `terraform apply` after the
    /// user has reviewed the diff summary.
    IacApply,
}

/// What to do when a `PasswordModal` resolves with a submitted secret.
///
/// The host runs the action with the captured password.  On error
/// (`Err(_)`) the modal stays open with an error banner and the buffer
/// cleared; the host increments `attempts` and locks the modal out after
/// the third miss.
#[derive(Debug, Clone)]
pub enum PendingPasswordAction {
    /// `/vault unlock`: attempt `VaultManager::unlock(password)`.
    VaultUnlock,
}
