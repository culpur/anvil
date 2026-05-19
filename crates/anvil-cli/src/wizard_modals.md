# Wizard Modal Primitives — v2.2.18 (task #666)

Public crate-internal API surface for Agent A1's three deliverables.
This doc is the contract Agents A3 / A4 / A5 should code against.

All types are at `crates/anvil-cli/src/tui/modals/` and re-exported
through `crate::tui::modals`. The runner is at
`crate::wizard_runner::WizardModalRunner`.

The wizard runs in alt-screen — no `println!` / `print!` / `eprintln!`
anywhere on these code paths. Subprocess output is captured into reader
threads and sent into the modal via `mpsc::channel` so it never bypasses
the TUI back-buffer (per `feedback-tui-stdout-anti-pattern.md`).

---

## Deliverable 1: `StreamingOutputModal`

Renders a long-running subprocess's stdout/stderr live, in the
alt-screen. Used by A2's Ollama installer, A3's `ollama pull` /
`bench`, A4's `npm install -g @tobilu/qmd` / `qmd update` / `qmd
embed`, and A5's healing actions.

### Public API

```rust
use crate::tui::modals::StreamingOutputModal;
use std::process::Command;

let mut cmd = Command::new("sh");
cmd.args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"]);

let modal = StreamingOutputModal::new(
    "Installing Ollama",
    "Running Ollama installer…",
)
.with_subprocess(cmd)
.with_progress_detector(|line: &str| {
    // Parse e.g. "downloading 42%" → Some(0.42); else None.
    line.strip_prefix("downloading ")
        .and_then(|rest| rest.trim_end_matches('%').parse::<f32>().ok())
        .map(|p| p / 100.0)
});

let result = runner.run_streaming_output("step-ollama-install", modal)?;
match result {
    ModalAnswer::StreamingResult { exit_code: 0, .. } => {
        // success
    }
    ModalAnswer::StreamingResult { exit_code: -1, .. } => {
        // user cancelled (SIGTERM → SIGKILL after 5s)
    }
    ModalAnswer::StreamingResult { exit_code, output_tail } => {
        // subprocess failed; `output_tail` has up to the last 8 lines
    }
    _ => unreachable!("run_streaming_output always returns StreamingResult"),
}
```

### Builder methods

| method | purpose |
|---|---|
| `StreamingOutputModal::new(title, status)` | create the modal |
| `.with_subprocess(cmd: Command)` | attach the pre-built `Command` |
| `.with_progress_detector(fn(&str) -> Option<f32>)` | per-line progress parser |
| `.set_progress(p: f32)` | manually set 0.0..=1.0 (clamped) |
| `.set_detail(s: impl Into<String>)` | one-line rich status under the spinner |
| `.push_line(line: String)` | inject a tail line (used internally; safe externally) |

### Cancellation

- Esc opens a nested `ConfirmModal` ("Cancel install?")
- On Yes the runner calls `cancel_with_grace(handle, CANCEL_GRACE)`:
  1. Sends SIGTERM via `/bin/kill -TERM <pid>` (Unix) or `Child::kill()` (Windows)
  2. Polls `try_wait()` for up to `CANCEL_GRACE` (5s by default)
  3. SIGKILL via `Child::kill()` if still alive
- Returns `ModalAnswer::StreamingResult { exit_code: -1, output_tail }`

### Rendering budget

- 1 redraw per `REDRAW_BUDGET_MS` (100ms) max
- Spinner ticks once per redraw via `tick_spinner()`
- Tail keeps the last `TAIL_LINES` (8) merged stdout/stderr lines
- Reader threads drain at up to 32 lines per loop iteration

### Cross-platform

- Linux, macOS, FreeBSD, NetBSD, OpenBSD: SIGTERM via `/bin/kill`, SIGKILL via `Child::kill()`
- Windows: `Child::kill()` only (TerminateProcess)
- No `/proc`, no GNU-only flags, no `libc` direct calls

---

## Deliverable 2: Rich `WizardChoiceModal` (the 4-state pattern)

Extends the existing `WizardChoiceModal` (task #579) with optional
badges + descriptions + custom footer hint. **Backward-compatible**:
every legacy `WizardChoiceModal::new(title, vec!["a".into(), ...])`
caller compiles untouched.

### New types

```rust
pub(crate) struct Choice {
    pub label: String,
    pub badge: Option<String>,
    pub description: Option<String>,
}

impl Choice {
    fn new(label: impl Into<String>) -> Self;
    fn with_badge(self, badge: impl Into<String>) -> Self;
    fn with_description(self, desc: impl Into<String>) -> Self;
}
```

### New builder methods on `WizardChoiceModal`

| method | purpose |
|---|---|
| `WizardChoiceModal::new(title, Vec<String>)` | legacy — preserved verbatim |
| `WizardChoiceModal::new_titled(title)` | rich-form entry point |
| `.with_choices(Vec<Choice>)` | rich rows (panics if empty) |
| `.with_footer_hint(impl Into<String>)` | override default footer; empty string suppresses |

### Sample call

```rust
use crate::tui::modals::{Choice, WizardChoiceModal};

let modal = WizardChoiceModal::new_titled("Install Ollama?")
    .with_choices(vec![
        Choice::new("Install Ollama now")
            .with_badge("recommended")
            .with_description("Full installer + benchmark, ~5min"),
        Choice::new("I have it elsewhere")
            .with_description("Point Anvil at an existing Ollama URL"),
        Choice::new("Skip — don't ask again")
            .with_description("Hide Ollama features entirely"),
        Choice::new("Maybe later")
            .with_description("Remind me in a few sessions"),
    ])
    .with_footer_hint("press 1-4, or use ↑/↓ + Enter");
```

Returns `ModalAnswer::Choice(index)` exactly as before — caller branch
logic is unchanged.

### Wizard.rs migration witness

Step 7 layout picker in `wizard.rs::run_first_run_wizard_v2` (the
v2.2.17 default path) is migrated to use `Choice::with_badge` +
`Choice::with_description` to prove the back-compat contract. The
older one-arg `run_steps_4_through_8` path is left untouched as the
back-compat witness.

---

## Deliverable 3: `HealthProbeModal`

Multi-issue repair checklist with spacebar-toggle. Drives A5's
HealingModal.

### Public API

```rust
use crate::tui::modals::{HealthIssue, HealthProbeModal, HealthStatus};

let modal = HealthProbeModal::new(
    "Anvil setup needs attention",
    "We checked your install and found:",
)
.with_issues(vec![
    HealthIssue::new(HealthStatus::Ok, "Vault OK"),
    HealthIssue::new(HealthStatus::Ok, "Anthropic auth OK"),
    HealthIssue::new_repair(HealthStatus::Fail, "Ollama daemon not running"),
    HealthIssue::new_repair(HealthStatus::Warn, "QMD index hasn't refreshed in 8 days"),
    HealthIssue::new(HealthStatus::Warn, "Bash completions missing"),
]);

let answer = runner.run_health_probe("setup-health", modal)?;
match answer {
    ModalAnswer::HealthCheck { quit: true, .. } => {
        // user quit — wizard should abort
    }
    ModalAnswer::HealthCheck { repair, quit: false } => {
        // `repair` holds the indices to repair (may be empty)
        for idx in repair {
            // re-run the relevant remediation
        }
    }
    _ => unreachable!(),
}
```

### Builder methods

| method | purpose |
|---|---|
| `HealthProbeModal::new(title, preamble)` | create the modal (no issues yet) |
| `.with_issues(Vec<HealthIssue>)` | attach issues + park cursor on first repairable row |
| `HealthIssue::new(status, label)` | row, NOT pre-flagged for repair |
| `HealthIssue::new_repair(status, label)` | row, pre-flagged for repair if `status != Ok` |

### Keys

| key | action |
|---|---|
| ↑/↓ | move highlight |
| Space | toggle highlighted row's repair flag (Ok rows ignored) |
| r / R | resolve with currently-flagged repair set |
| a / A | flag ALL repairable rows, then resolve |
| c / C | resolve with empty repair set (continue) |
| Enter | resolve with current set; falls back to `Continue` if empty |
| q / Q / Esc | resolve with `quit = true` |

### Status icons + colors

| status | icon | color |
|---|---|---|
| `Ok` | ✓ | green (not toggleable) |
| `Fail` | ✗ | red |
| `Warn` | ⚠ | yellow |

---

## `WizardModalRunner` additions

Two new entry points on the runner alongside `run_choice` /
`run_confirm` / `run_text_input` / `run_oauth_flow` /
`run_password_capture`:

```rust
impl<'a, B, H, K> WizardModalRunner<'a, B, H, K> {
    pub(crate) fn run_streaming_output(
        &mut self,
        _tag: &str,
        modal: StreamingOutputModal,
    ) -> Result<ModalAnswer, RunnerError>;

    pub(crate) fn run_health_probe(
        &mut self,
        tag: &str,
        modal: HealthProbeModal,
    ) -> Result<ModalAnswer, RunnerError>;
}
```

Both return `ModalAnswer` variants — `StreamingResult { exit_code,
output_tail }` and `HealthCheck { repair, quit }` respectively.

---

## New `ModalAnswer` variants

```rust
ModalAnswer::StreamingResult {
    exit_code: i32,         // 0 = success, -1 = cancelled, else subprocess code
    output_tail: Vec<String>,
}

ModalAnswer::HealthCheck {
    repair: Vec<usize>,     // indices of issues to repair (may be empty)
    quit: bool,             // user pressed q/Esc (treat as wizard-abort)
}
```

---

## What I did NOT do (deferred — explicit list per `feedback-no-silent-deferral.md`)

Nothing was silently deferred. All three deliverables landed:
1. `StreamingOutputModal` complete with subprocess spawn + cancel.
2. `WizardChoiceModal` extended with rich `Choice` + footer hint.
3. `HealthProbeModal` complete with all keys.

The 4 pre-existing `journal_*` snapshot test failures are a baseline
issue (verified by stashing my changes — they fail on `ad9cc1b` v2.2.17
release too). Not caused by my work and out of scope for A1.
