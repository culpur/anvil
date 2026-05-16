# Task #579 — Migrate wizard.rs prompts to in-TUI modal

**Status**: Filed, not started  
**Parent**: #578 (ProviderLoginModal)  
**Target**: v2.2.16

## Problem

`crates/anvil-cli/src/wizard.rs` uses `std::io::stdin().read_line()` and
`println!()` for first-run setup prompts (model selection, API key entry,
permission mode).  When `wizard.rs` runs inside TUI mode (e.g. first launch
while the alternate screen is active), it drops to the CLI in the same
disorienting way that #578 fixed for `/provider login`.

## Deliverables

1. New `WizardModal` (or extend `ProviderLoginModal` with a `WizardStep`
   variant) that presents the first-run questions as a TUI overlay.
2. `AnvilTui::open_wizard_modal()` — analogous to `open_provider_login_modal`.
3. Key handler arm in `input_handler.rs` for wizard step navigation.
4. `wizard.rs` guard: detect TUI context via `alternate_screen_enabled()`
   and delegate to `open_wizard_modal()` instead of stdin.
5. Unit tests for all wizard step transitions.

## Out of scope

- Do not change the non-TUI (first-run, no alternate screen) path — stdin
  prompts are appropriate there.
- Do not change `provider_login.rs` logic landed in #578.
