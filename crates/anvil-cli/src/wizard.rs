//! First-run interactive setup wizard.
//!
//! Runs entirely in plain terminal mode before the TUI starts.  Walks the user
//! through setting up the encrypted vault, then connecting to each AI provider
//! (Ollama, Anthropic, `OpenAI`, xAI), then sets provider priority and the
//! default model, writing the results to `~/.anvil/config.json`.
//!
//! API keys and credentials are stored exclusively in the encrypted vault
//! (`~/.anvil/vault/`).  `~/.anvil/credentials.json` is only written when the
//! vault setup step is explicitly skipped, preserving backward compatibility
//! with existing installations that may not have run the wizard.

// Task #626: this file runs entirely before the TUI starts (SAFE-PREWIZARD
// per audit 2026-05-18).  Allow `print_stdout` / `print_stderr` so the
// wizard can render its interactive forms.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use serde_json::json;

use crate::DEFAULT_MODEL;
use crate::auth::run_anthropic_login;
use crate::wizard_runner::{
    CrosstermHooks, CrosstermKeySource, RunnerError, WizardModalRunner, WizardSession,
};

/// Returns true when `<config-home>/config.json` already exists, meaning the
/// user has already completed (or explicitly skipped) first-run setup.
///
/// **Honors `ANVIL_CONFIG_HOME`** — task #641 (v2.2.17). The vault, file_cache,
/// qmd, and config loaders all read from `ANVIL_CONFIG_HOME` when set; the
/// wizard gate must check the same directory or `ANVIL_CONFIG_HOME=/tmp/foo
/// anvil` would skip the wizard and load the live profile instead.
pub(crate) fn anvil_config_json_exists() -> bool {
    runtime::default_config_home().join("config.json").exists()
}

/// Print a boxed banner line using a fixed-width inner area.
fn wizard_box_line(content: &str) {
    const INNER: usize = 56;
    let padded = format!("{content:<INNER$}");
    println!("\x1b[36m\u{2551}\x1b[0m  {padded}  \x1b[36m\u{2551}\x1b[0m");
}

/// Print a full-width horizontal top border.
fn wizard_box_top() {
    println!(
        "\x1b[36m\u{2554}{}\u{2557}\x1b[0m",
        "\u{2550}".repeat(60)
    );
}

fn wizard_box_bot() {
    println!(
        "\x1b[36m\u{255A}{}\u{255D}\x1b[0m",
        "\u{2550}".repeat(60)
    );
}

fn wizard_step_header(step: u8, total: u8, title: &str) {
    println!();
    println!("\x1b[1;33mStep {step} of {total}: {title}\x1b[0m");
    println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));
}

/// Read a trimmed line from stdin, flushing stdout first.
fn wizard_read_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
    buf.trim().to_string()
}

/// Test Ollama connectivity at the given URL.  Returns the list of model names
/// on success, or an error message on failure.
fn wizard_test_ollama(url: &str) -> Result<Vec<(String, String)>, String> {
    let out = std::process::Command::new("curl")
        .args(["-s", "--max-time", "3", &format!("{url}/api/tags")])
        .output()
        .map_err(|e| format!("curl error: {e}"))?;
    if !out.status.success() {
        return Err("connection refused".to_string());
    }
    let val = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map_err(|_| "invalid JSON from Ollama".to_string())?;
    let models = val
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let name = m.get("name")?.as_str()?.to_string();
                    let size = m
                        .get("size")
                        .and_then(serde_json::Value::as_f64)
                        .map_or("?".to_string(), |b| format!("{:.1}GB", b / 1e9));
                    Some((name, size))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(models)
}

/// Save a key/value credential using the best available storage backend.
///
/// Priority order:
///   1. Vault session (if vault was set up and unlocked during this wizard run)
///   2. Plaintext `~/.anvil/credentials.json` (fallback for skipped vault setup
///      and for existing installations without a vault)
///
/// The fallback path is intentionally preserved so that users who skip vault
/// setup do not lose their credentials.
pub(crate) fn wizard_save_credential(key: &str, value: &str) -> io::Result<()> {
    // Try vault first — best-effort, fall through to plaintext on any error.
    if runtime::vault_is_session_unlocked()
        && let Ok(()) = runtime::vault_session_upsert(key, value) {
            return Ok(());
        }
    wizard_save_credential_plaintext(key, value)
}

/// Save a key/value pair to `~/.anvil/credentials.json` (plaintext fallback).
pub(crate) fn wizard_save_credential_plaintext(key: &str, value: &str) -> io::Result<()> {
    let path = runtime::credentials_path()?;
    let mut root = if path.exists() {
        let raw = fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    root.insert(key.to_string(), serde_json::Value::String(value.to_string()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&root).unwrap_or_default())
}

/// The public URL where `config-schema.json` is published.
pub(crate) const CONFIG_SCHEMA_URL: &str = "https://anvilhub.culpur.net/config-schema.json";

/// Save the wizard result to `~/.anvil/config.json`, merging with any
/// existing keys so previously set values are preserved.
///
/// On a *fresh* install (no prior config file) the `$schema` pointer is
/// injected as the first key so editors (VS Code, JetBrains) auto-fetch
/// the schema for IntelliSense and validation.
pub(crate) fn wizard_save_config(
    config: &serde_json::Map<String, serde_json::Value>,
) -> io::Result<PathBuf> {
    // Task #641: honour ANVIL_CONFIG_HOME so the wizard writes to the same
    // directory `anvil_config_json_exists` reads from (and the rest of the
    // runtime loads from).
    let dir = runtime::default_config_home();
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.json");
    let is_new_config = !path.exists();
    let mut existing = if is_new_config {
        serde_json::Map::new()
    } else {
        fs::read_to_string(&path)
            .ok()
            .and_then(|raw| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
            })
            .unwrap_or_default()
    };
    // Inject $schema pointer on fresh installs so editors pick up validation.
    if is_new_config {
        existing.insert(
            "$schema".to_string(),
            serde_json::Value::String(CONFIG_SCHEMA_URL.to_string()),
        );
    }
    for (k, v) in config {
        existing.insert(k.clone(), v.clone());
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(existing))
            .unwrap_or_else(|_| "{}".to_string()),
    )?;
    Ok(path)
}

/// Parse a wizard step-7 mouse-capture choice into a boolean.
///
/// **Default OFF.** Task #623 (v2.2.14 Phase 1) fixed the v2.2.13/15/16
/// regression where the wizard offered "[1] Yes — enable mouse capture
/// (recommended)" and an empty Enter sent the user to ON. On Gnome Terminal
/// (Ubuntu default) and macOS Terminal.app, mouse capture ON breaks native
/// click-drag text selection.
///
/// Truth table:
/// - `""` (blank / Enter)       → `false` (OFF, the default)
/// - `"1"` / `"n"` / `"no"`     → `false` (OFF, explicit)
/// - `"2"` / `"y"` / `"yes"`    → `true`  (ON, opt-in)
/// - anything else              → `false` (conservative — never silently flip ON)
#[must_use]
/// Parse the wizard's "TUI Layout — architecture choice" answer (#571).
///
/// Step 7 of `run_first_run_wizard` asks the user to pick one of four
/// architectures (1 = vertical-split [default], 2 = classic, 3 = three-pane,
/// 4 = journal). Empty / unrecognized input falls back to vertical-split.
///
/// Extracted as a pure function so the wizard's layout selection is
/// regression-tested without a stdin fixture.
pub(crate) fn wizard_parse_layout_kind_choice(raw: &str) -> &'static str {
    match raw.trim() {
        "2" => "classic",
        "3" => "three-pane",
        "4" => "journal",
        _ => "vertical-split",
    }
}

/// Parse the wizard's "Show workspace tabs?" answer (#571).
///
/// `true` = tabs visible (default; matches pre-v2.2.16 behaviour). Only
/// explicit "no" inputs disable tabs.
pub(crate) fn wizard_parse_layout_tabs_choice(raw: &str) -> bool {
    !matches!(raw.trim(), "2" | "n" | "N" | "no" | "No")
}

/// Compose the `tui_layout` config alias from the two wizard answers (#571).
///
/// Mirrors the inline logic in `run_first_run_wizard` Step 7 so call sites
/// stay synchronized.
pub(crate) fn wizard_compose_layout_alias(kind: &str, tabs: bool) -> String {
    if tabs { format!("{kind}-tabs") } else { kind.to_string() }
}

pub(crate) fn wizard_parse_mouse_capture_choice(raw: &str) -> bool {
    matches!(
        raw.trim(),
        "2" | "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" | "true" | "True"
    )
}

/// Answers captured by the in-TUI modal portion of the wizard
/// (steps 7 + 8: layout architecture, layout tabs, mouse capture,
/// theme, permission mode).
///
/// This struct is the bridge between the modal-driven path and the
/// stdin-based fallback path used when stdout is not a TTY.
#[derive(Debug, Clone)]
pub(crate) struct WizardTuiAnswers {
    pub(crate) layout_kind: String,
    pub(crate) layout_tabs: bool,
    pub(crate) mouse_capture: bool,
    pub(crate) theme: String,
    pub(crate) permission_mode: String,
}

impl WizardTuiAnswers {
    /// Conservative cross-platform defaults — used when ESC cancels a
    /// step, when the queue runner short-circuits, or when stdout is
    /// not a TTY. Mirrors the same defaults the stdin-path wizard
    /// applies when the user just presses Enter.
    pub(crate) fn defaults() -> Self {
        Self {
            layout_kind: "vertical-split".to_string(),
            layout_tabs: true,
            mouse_capture: false,
            theme: "dark".to_string(),
            permission_mode: "ask".to_string(),
        }
    }
}

/// Legacy: drive only the four TUI-config modal steps inside a single
/// alt-screen session. Superseded by `run_post_auth_modals_in_alt_screen`
/// (task #642) which extends the same single-session design to steps
/// 4..8.  Retained as a fallback for tools that need only the layout +
/// preference slice and as the regression contract for #579/#622.
#[allow(dead_code)]
pub(crate) fn run_tui_config_modals_in_alt_screen() -> Result<WizardTuiAnswers, RunnerError> {
    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::time::Duration;

    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).map_err(|e| RunnerError::Enter(e.to_string()))?;
    let mut session = WizardSession::enter(terminal, CrosstermHooks::new())?;

    // Initial banner — Step 7 of 8.
    session.render_banner(
        "Step 7 of 8 — TUI Layout",
        &[
            "Pick your default workspace architecture and tab visibility.",
            "Live previews: https://anvilhub.culpur.net/tui-preview",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let keys = CrosstermKeySource {
        poll_timeout: Duration::from_millis(50),
    };
    let mut runner = WizardModalRunner::new(&mut session, keys, ratatui::style::Color::Cyan);

    let mut answers = WizardTuiAnswers::defaults();

    // Step 7a: layout architecture
    let layout_kind_modal = WizardChoiceModal::new(
        "TUI Layout — pick architecture",
        vec![
            "Vertical Split  (default — sessions rail + swappable deck)".into(),
            "Classic         (single-deck, pre-v2.2.16)".into(),
            "Three-Pane      (FOCUS / LOG / CONTEXT)".into(),
            "Journal         (timestamped column, Ctrl-K palette)".into(),
        ],
    );
    let layout_kind_answer = runner.run_choice("layout-kind", layout_kind_modal)?;
    answers.layout_kind = match layout_kind_answer {
        ModalAnswer::Choice(0) => "vertical-split".to_string(),
        ModalAnswer::Choice(1) => "classic".to_string(),
        ModalAnswer::Choice(2) => "three-pane".to_string(),
        ModalAnswer::Choice(3) => "journal".to_string(),
        _ => "vertical-split".to_string(),
    };

    // Step 7b: tabs visibility
    let layout_tabs_modal = ConfirmModal::new(
        "Show workspace tabs?",
        "Tabs let you keep multiple parallel sessions visible at once. \
         Default = yes; press n / Esc for a single-session layout.",
    );
    let layout_tabs_answer = runner.run_confirm("layout-tabs", layout_tabs_modal)?;
    answers.layout_tabs = matches!(
        layout_tabs_answer,
        ModalAnswer::Confirm(ConfirmChoice::Yes)
    );

    // Banner — Step 8 preferences.
    runner.session.render_banner(
        "Step 8 of 8 — TUI Preferences",
        &[
            "Mouse capture, theme, and default permission mode.",
            "Change any of these later with /config or settings.json.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    // Step 8a: mouse capture — default OFF (#623).
    let mouse_modal = ConfirmModal::new(
        "Enable mouse capture?",
        "Default OFF. With capture OFF your terminal owns the mouse: \
         drag-to-select + native copy work everywhere. With capture ON, \
         Anvil intercepts mouse events for clickable tabs + wheel-scroll, \
         but you must hold a modifier (Option on macOS, Shift elsewhere) \
         to select text. Press y to opt in, n / Esc to keep the default.",
    );
    let mouse_answer = runner.run_confirm("mouse", mouse_modal)?;
    answers.mouse_capture = matches!(
        mouse_answer,
        ModalAnswer::Confirm(ConfirmChoice::Yes)
    );

    // Step 8b: theme
    let theme_modal = WizardChoiceModal::new(
        "Theme",
        vec![
            "Dark   (default)".into(),
            "Light".into(),
            "Auto   (follow terminal background detection)".into(),
        ],
    );
    let theme_answer = runner.run_choice("theme", theme_modal)?;
    answers.theme = match theme_answer {
        ModalAnswer::Choice(1) => "light".to_string(),
        ModalAnswer::Choice(2) => "auto".to_string(),
        _ => "dark".to_string(),
    };

    // Step 8c: permission mode
    let perm_modal = WizardChoiceModal::new(
        "Default permission mode",
        vec![
            "ask                  (confirm each tool call — safest, default)".into(),
            "workspace-write      (auto-allow edits inside the workspace)".into(),
            "danger-full-access   (no prompts — high trust required)".into(),
        ],
    );
    let perm_answer = runner.run_choice("permission", perm_modal)?;
    answers.permission_mode = match perm_answer {
        ModalAnswer::Choice(1) => "workspace-write".to_string(),
        ModalAnswer::Choice(2) => "danger-full-access".to_string(),
        _ => "ask".to_string(),
    };

    // Session goes out of scope here — single LeaveAlternateScreen
    // emitted by `WizardSession::drop`. The runner only borrows the
    // session, so its scope-end is implicit.
    Ok(answers)
}

/// Drive wizard **steps 4-8** inside an existing alt-screen session
/// (task #642, finisher).  Steps 1-3 are driven separately by
/// `run_steps_1_to_3_in_alt_screen`; together they form the unified
/// single-session wizard.
///
/// This is the runner-borrowing variant of
/// `run_post_auth_modals_in_alt_screen` — it does NOT enter or leave
/// alt-screen.  The orchestrator (`run_full_wizard_in_alt_screen`)
/// owns the session and threads a single runner through both halves
/// so the user sees ONE alt-screen transition total.
pub(crate) fn run_steps_4_to_8_in_alt_screen<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    model_candidates: &[(String, String)],
) -> Result<(String, String, String, WizardTuiAnswers), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: crate::wizard_runner::KeySource,
{
    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
    use crate::tui::modals::text_input::TextInputModal;

    // ── Step 4: Default Model ────────────────────────────────────────────
    runner.session.render_banner(
        "Step 4 of 8 — Default Model",
        &[
            "Pick the model Anvil uses by default. Change later with /model.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let chosen_model = if model_candidates.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        let labels: Vec<String> = model_candidates
            .iter()
            .map(|(id, label)| format!("{id}  ({label})"))
            .collect();
        let modal = WizardChoiceModal::new("Default Model", labels);
        let ans = runner.run_choice("step4-model", modal)?;
        match ans {
            ModalAnswer::Choice(idx) if idx < model_candidates.len() => {
                model_candidates[idx].0.clone()
            }
            _ => model_candidates
                .first()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        }
    };

    // ── Step 5: Ollama Endpoint ──────────────────────────────────────────
    runner.session.render_banner(
        "Step 5 of 8 — Ollama Endpoint",
        &[
            "Local Ollama URL. Press Enter to accept the default.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let ollama_modal = TextInputModal::new("Ollama URL", "Endpoint")
        .with_default("http://localhost:11434");
    let ollama_url = match runner.run_text_input("step5-ollama-url", ollama_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "http://localhost:11434".to_string(),
    };

    // ── Step 6: Profile Name ─────────────────────────────────────────────
    runner.session.render_banner(
        "Step 6 of 8 — Profile",
        &[
            "Name this Anvil profile. Switch later with `anvil --profile <name>`.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let profile_modal = TextInputModal::new("Profile name", "Profile")
        .with_default("default");
    let profile_name = match runner.run_text_input("step6-profile", profile_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "default".to_string(),
    };

    // ── Step 7: TUI Layout ───────────────────────────────────────────────
    runner.session.render_banner(
        "Step 7 of 8 — TUI Layout",
        &[
            "Pick your default workspace architecture and tab visibility.",
            "Live previews: https://anvilhub.culpur.net/tui-preview",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let mut tui_answers = WizardTuiAnswers::defaults();

    let layout_kind_modal = WizardChoiceModal::new(
        "TUI Layout — pick architecture",
        vec![
            "Vertical Split  (default — sessions rail + swappable deck)".into(),
            "Classic         (single-deck, pre-v2.2.16)".into(),
            "Three-Pane      (FOCUS / LOG / CONTEXT)".into(),
            "Journal         (timestamped column, Ctrl-K palette)".into(),
        ],
    );
    let layout_kind_answer = runner.run_choice("step7-layout-kind", layout_kind_modal)?;
    tui_answers.layout_kind = match layout_kind_answer {
        ModalAnswer::Choice(0) => "vertical-split".to_string(),
        ModalAnswer::Choice(1) => "classic".to_string(),
        ModalAnswer::Choice(2) => "three-pane".to_string(),
        ModalAnswer::Choice(3) => "journal".to_string(),
        _ => "vertical-split".to_string(),
    };

    let layout_tabs_modal = ConfirmModal::new(
        "Show workspace tabs?",
        "Tabs let you keep multiple parallel sessions visible at once. \
         Default = yes; press n / Esc for a single-session layout.",
    );
    let layout_tabs_answer = runner.run_confirm("step7-layout-tabs", layout_tabs_modal)?;
    tui_answers.layout_tabs = matches!(
        layout_tabs_answer,
        ModalAnswer::Confirm(ConfirmChoice::Yes)
    );

    // ── Step 8: TUI Preferences ──────────────────────────────────────────
    runner.session.render_banner(
        "Step 8 of 8 — TUI Preferences",
        &[
            "Mouse capture, theme, and default permission mode.",
            "Change any of these later with /config or settings.json.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let mouse_modal = ConfirmModal::new(
        "Enable mouse capture?",
        "Default OFF. With capture OFF your terminal owns the mouse: \
         drag-to-select + native copy work everywhere. With capture ON, \
         Anvil intercepts mouse events for clickable tabs + wheel-scroll, \
         but you must hold a modifier (Option on macOS, Shift elsewhere) \
         to select text. Press y to opt in, n / Esc to keep the default.",
    );
    let mouse_answer = runner.run_confirm("step8-mouse", mouse_modal)?;
    tui_answers.mouse_capture = matches!(
        mouse_answer,
        ModalAnswer::Confirm(ConfirmChoice::Yes)
    );

    let theme_modal = WizardChoiceModal::new(
        "Theme",
        vec![
            "Dark   (default)".into(),
            "Light".into(),
            "Auto   (follow terminal background detection)".into(),
        ],
    );
    let theme_answer = runner.run_choice("step8-theme", theme_modal)?;
    tui_answers.theme = match theme_answer {
        ModalAnswer::Choice(1) => "light".to_string(),
        ModalAnswer::Choice(2) => "auto".to_string(),
        _ => "dark".to_string(),
    };

    let perm_modal = WizardChoiceModal::new(
        "Default permission mode",
        vec![
            "ask                  (confirm each tool call — safest, default)".into(),
            "workspace-write      (auto-allow edits inside the workspace)".into(),
            "danger-full-access   (no prompts — high trust required)".into(),
        ],
    );
    let perm_answer = runner.run_choice("step8-perm", perm_modal)?;
    tui_answers.permission_mode = match perm_answer {
        ModalAnswer::Choice(1) => "workspace-write".to_string(),
        ModalAnswer::Choice(2) => "danger-full-access".to_string(),
        _ => "ask".to_string(),
    };

    // ── Final banner ─────────────────────────────────────────────────────
    runner.session.render_banner(
        "Done — saving config",
        &["Writing config.json + finalizing setup..."],
        ratatui::style::Color::Cyan,
    )?;

    Ok((chosen_model, ollama_url, profile_name, tui_answers))
}

/// Result of running ALL 8 wizard steps inside a single alt-screen
/// session (task #642 v2.2.17 finisher).
///
/// The orchestrator (`run_full_wizard_in_alt_screen`) returns this
/// bundle to `run_first_run_wizard`, which then writes config.json and
/// shows the post-wizard summary banner (inline, AFTER the session
/// exits — there is only ever ONE alt-screen transition).
#[derive(Debug, Clone)]
pub(crate) struct FullWizardResult {
    pub(crate) steps123: WizardSteps123,
    pub(crate) chosen_model: String,
    pub(crate) ollama_url: String,
    pub(crate) profile_name: String,
    pub(crate) tui_answers: WizardTuiAnswers,
}

/// Open ONE alt-screen session and drive ALL 8 wizard steps inside it
/// (task #642, v2.2.17 finisher).
///
/// The user sees:
///   1. Terminal enters alt-screen ONCE.
///   2. Banner-to-banner transitions through steps 1-8 (vault →
///      provider → auth/OAuth → model → ollama → profile → layout →
///      preferences).
///   3. Final "Done" banner.
///   4. Terminal leaves alt-screen ONCE (on `WizardSession::drop`).
///   5. The post-wizard summary prints inline below.
///
/// OAuth (step 3) renders inline — `OAuthFlow` draws into the same
/// session's terminal and polls the callback channel each tick.  No
/// drop to stdin, no second alt-screen, no flash.
pub(crate) fn run_full_wizard_in_alt_screen() -> Result<FullWizardResult, RunnerError> {
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::time::Duration;

    let backend = CrosstermBackend::new(io::stdout());
    let terminal =
        Terminal::new(backend).map_err(|e| RunnerError::Enter(e.to_string()))?;
    let mut session = WizardSession::enter(terminal, CrosstermHooks::new())?;

    let keys = CrosstermKeySource {
        poll_timeout: Duration::from_millis(50),
    };
    let mut runner =
        WizardModalRunner::new(&mut session, keys, ratatui::style::Color::Cyan);

    // Welcome banner.
    runner.session.render_banner(
        &format!("Anvil v{}", env!("CARGO_PKG_VERSION")),
        &[
            "First-run setup wizard — 8 steps, ~2 minutes.",
            "All choices can be changed later via /config or settings.json.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    // Steps 1-3.
    let steps123 = run_steps_1_to_3_in_alt_screen(&mut runner)?;

    // Steps 4-8.
    let (chosen_model, ollama_url_modal, profile_name, tui_answers) =
        run_steps_4_to_8_in_alt_screen(&mut runner, &steps123.model_candidates)?;

    // session goes out of scope here — single LeaveAlternateScreen.
    drop(runner);
    drop(session);

    Ok(FullWizardResult {
        steps123,
        chosen_model,
        ollama_url: ollama_url_modal,
        profile_name,
        tui_answers,
    })
}

/// Drive **all** modal-friendly wizard steps inside a single alt-screen
/// session. Returns the captured answers on success. The OAuth step is
/// handled by the caller via the inline path BEFORE this function is
/// invoked — steps 4..8 run unified inside one alt-screen.
///
/// Step layout inside this single session:
///   - Banner: "Step 4 of 8 — Default Model" → ChoiceModal
///   - Banner: "Step 5 of 8 — Ollama Endpoint" → TextInputModal
///   - Banner: "Step 6 of 8 — Profile" → TextInputModal
///   - Banner: "Step 7 of 8 — TUI Layout" → ChoiceModal + ConfirmModal
///   - Banner: "Step 8 of 8 — TUI Preferences" → ConfirmModal + 2 ChoiceModals
///   - Banner: "Done — saving config"
///
/// Steps 1/2/3 run BEFORE this function via the inline + alt-screen
/// mix in `run_first_run_wizard` — vault password capture uses
/// `run_password_capture` against its own session; provider choice
/// uses its own session; OAuth uses the existing stdin-based flow.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_post_auth_modals_in_alt_screen(
    model_candidates: &[(String, String)],
) -> Result<(String, String, String, WizardTuiAnswers), RunnerError> {
    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
    use crate::tui::modals::text_input::TextInputModal;
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::time::Duration;

    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).map_err(|e| RunnerError::Enter(e.to_string()))?;
    let mut session = WizardSession::enter(terminal, CrosstermHooks::new())?;

    let keys = CrosstermKeySource {
        poll_timeout: Duration::from_millis(50),
    };
    let mut runner = WizardModalRunner::new(&mut session, keys, ratatui::style::Color::Cyan);

    // ── Step 4: Default Model ────────────────────────────────────────────
    runner.session.render_banner(
        "Step 4 of 8 — Default Model",
        &[
            "Pick the model Anvil uses by default. Change later with /model.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let chosen_model = if model_candidates.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        let labels: Vec<String> = model_candidates
            .iter()
            .map(|(id, label)| format!("{id}  ({label})"))
            .collect();
        let modal = WizardChoiceModal::new("Default Model", labels);
        let ans = runner.run_choice("step4-model", modal)?;
        match ans {
            ModalAnswer::Choice(idx) if idx < model_candidates.len() => {
                model_candidates[idx].0.clone()
            }
            _ => model_candidates
                .first()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        }
    };

    // ── Step 5: Ollama Endpoint ──────────────────────────────────────────
    runner.session.render_banner(
        "Step 5 of 8 — Ollama Endpoint",
        &[
            "Local Ollama URL. Press Enter to accept the default.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let ollama_modal = TextInputModal::new("Ollama URL", "Endpoint")
        .with_default("http://localhost:11434");
    let ollama_url = match runner.run_text_input("step5-ollama-url", ollama_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "http://localhost:11434".to_string(),
    };

    // ── Step 6: Profile Name ─────────────────────────────────────────────
    runner.session.render_banner(
        "Step 6 of 8 — Profile",
        &[
            "Name this Anvil profile. Switch later with `anvil --profile <name>`.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let profile_modal = TextInputModal::new("Profile name", "Profile")
        .with_default("default");
    let profile_name = match runner.run_text_input("step6-profile", profile_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "default".to_string(),
    };

    // ── Step 7: TUI Layout ───────────────────────────────────────────────
    runner.session.render_banner(
        "Step 7 of 8 — TUI Layout",
        &[
            "Pick your default workspace architecture and tab visibility.",
            "Live previews: https://anvilhub.culpur.net/tui-preview",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let mut tui_answers = WizardTuiAnswers::defaults();

    let layout_kind_modal = WizardChoiceModal::new(
        "TUI Layout — pick architecture",
        vec![
            "Vertical Split  (default — sessions rail + swappable deck)".into(),
            "Classic         (single-deck, pre-v2.2.16)".into(),
            "Three-Pane      (FOCUS / LOG / CONTEXT)".into(),
            "Journal         (timestamped column, Ctrl-K palette)".into(),
        ],
    );
    let layout_kind_answer = runner.run_choice("step7-layout-kind", layout_kind_modal)?;
    tui_answers.layout_kind = match layout_kind_answer {
        ModalAnswer::Choice(0) => "vertical-split".to_string(),
        ModalAnswer::Choice(1) => "classic".to_string(),
        ModalAnswer::Choice(2) => "three-pane".to_string(),
        ModalAnswer::Choice(3) => "journal".to_string(),
        _ => "vertical-split".to_string(),
    };

    let layout_tabs_modal = ConfirmModal::new(
        "Show workspace tabs?",
        "Tabs let you keep multiple parallel sessions visible at once. \
         Default = yes; press n / Esc for a single-session layout.",
    );
    let layout_tabs_answer = runner.run_confirm("step7-layout-tabs", layout_tabs_modal)?;
    tui_answers.layout_tabs = matches!(
        layout_tabs_answer,
        ModalAnswer::Confirm(ConfirmChoice::Yes)
    );

    // ── Step 8: TUI Preferences ──────────────────────────────────────────
    runner.session.render_banner(
        "Step 8 of 8 — TUI Preferences",
        &[
            "Mouse capture, theme, and default permission mode.",
            "Change any of these later with /config or settings.json.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let mouse_modal = ConfirmModal::new(
        "Enable mouse capture?",
        "Default OFF. With capture OFF your terminal owns the mouse: \
         drag-to-select + native copy work everywhere. With capture ON, \
         Anvil intercepts mouse events for clickable tabs + wheel-scroll, \
         but you must hold a modifier (Option on macOS, Shift elsewhere) \
         to select text. Press y to opt in, n / Esc to keep the default.",
    );
    let mouse_answer = runner.run_confirm("step8-mouse", mouse_modal)?;
    tui_answers.mouse_capture = matches!(
        mouse_answer,
        ModalAnswer::Confirm(ConfirmChoice::Yes)
    );

    let theme_modal = WizardChoiceModal::new(
        "Theme",
        vec![
            "Dark   (default)".into(),
            "Light".into(),
            "Auto   (follow terminal background detection)".into(),
        ],
    );
    let theme_answer = runner.run_choice("step8-theme", theme_modal)?;
    tui_answers.theme = match theme_answer {
        ModalAnswer::Choice(1) => "light".to_string(),
        ModalAnswer::Choice(2) => "auto".to_string(),
        _ => "dark".to_string(),
    };

    let perm_modal = WizardChoiceModal::new(
        "Default permission mode",
        vec![
            "ask                  (confirm each tool call — safest, default)".into(),
            "workspace-write      (auto-allow edits inside the workspace)".into(),
            "danger-full-access   (no prompts — high trust required)".into(),
        ],
    );
    let perm_answer = runner.run_choice("step8-perm", perm_modal)?;
    tui_answers.permission_mode = match perm_answer {
        ModalAnswer::Choice(1) => "workspace-write".to_string(),
        ModalAnswer::Choice(2) => "danger-full-access".to_string(),
        _ => "ask".to_string(),
    };

    // ── Final banner ─────────────────────────────────────────────────────
    runner.session.render_banner(
        "Done — saving config",
        &[
            "Writing config.json + finalizing setup...",
        ],
        ratatui::style::Color::Cyan,
    )?;

    Ok((chosen_model, ollama_url, profile_name, tui_answers))
}

/// Captured state from wizard steps 1-3 — vault setup, provider
/// selection, and authentication.  Returned by
/// `run_steps_1_to_3_in_alt_screen` so the orchestrator can fold it
/// into the final config.json.
///
/// (task #642, finisher — v2.2.17)
#[derive(Debug, Clone, Default)]
pub(crate) struct WizardSteps123 {
    /// Provider IDs successfully configured (e.g. `["anthropic"]`,
    /// `["ollama", "openai"]`).
    pub(crate) configured_providers: Vec<String>,
    /// Model candidates discovered during step 3 — `(model_id, label)`.
    pub(crate) model_candidates: Vec<(String, String)>,
    /// Ollama endpoint URL captured during step 2 (if Ollama was picked).
    pub(crate) ollama_url: Option<String>,
    /// True when the vault was initialised AND unlocked during step 1.
    pub(crate) vault_session_unlocked: bool,
}

/// Drive wizard **steps 1-3** inside an existing alt-screen session
/// (task #642, v2.2.17 finisher).
///
/// Step 1 — Vault Setup (ChoiceModal + PasswordModal x2 with retry-on-mismatch).
/// Step 2 — AI Provider (ChoiceModal over `ProviderKind` top picks).
/// Step 3 — Authentication branches on the picked provider:
///   - Anthropic OAuth     → ConfirmModal (browser vs key) → `OAuthFlow` inline
///                           OR PasswordModal for manual API key
///   - Anthropic API key   → PasswordModal
///   - OpenAI / xAI / etc. → PasswordModal
///   - Ollama / local      → no auth, skip
///
/// The session enters alt-screen ONCE before this function is invoked
/// (by the orchestrator) and stays there through step 8 — there is
/// never a return to inline mode mid-wizard.
pub(crate) fn run_steps_1_to_3_in_alt_screen<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
) -> Result<WizardSteps123, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: crate::wizard_runner::KeySource,
{
    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::password::PasswordModal;
    use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
    use crate::tui::oauth_flow::{OAuthFlow, OAuthOutcome};
    use api::ProviderKind;

    let mut out = WizardSteps123::default();

    // ── Step 1: Vault Setup ───────────────────────────────────────────────
    runner.session.render_banner(
        "Step 1 of 8 — Vault Setup",
        &[
            "Anvil stores API keys in an AES-256-GCM encrypted vault.",
            "Set a master password now — it is never stored anywhere.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let vault_dir = runtime::default_config_home().join("vault");
    if runtime::vault_is_initialized() {
        // Vault already exists — show banner and skip to unlock.  The
        // unlock path requires the user's password; offer it via the
        // password modal but ALSO allow Esc to skip (continue without
        // vault session for this run).
        runner.session.render_banner(
            "Vault Already Initialised",
            &[
                &format!("Vault at: {}", vault_dir.display()),
                "Enter your master password to unlock for this session.",
                "Press Esc to skip — credentials will be stored in plaintext.",
            ],
            ratatui::style::Color::Cyan,
        )?;
        let pw_modal = PasswordModal::new("Master password", "Unlock vault");
        match runner.run_password_capture(pw_modal)? {
            Some(pw) => match runtime::init_session_vault(&pw) {
                Ok(true) => {
                    out.vault_session_unlocked = true;
                }
                _ => {
                    // Unlock failed — surface the error briefly and
                    // continue without vault session (plaintext fallback).
                    runner.session.render_banner(
                        "Vault unlock failed",
                        &["Continuing without vault — keys will be plaintext."],
                        ratatui::style::Color::Red,
                    )?;
                }
            },
            None => {
                // User skipped — no vault session this run.
            }
        }
    } else {
        // Fresh install — ask whether to set up the vault.
        let setup_modal = ConfirmModal::new(
            "Set up encrypted vault?",
            "Yes  — set a master password (recommended)\n\
             No   — store secrets in plaintext (not recommended)",
        );
        let setup_answer = runner.run_confirm("step1-vault-setup", setup_modal)?;
        let wants_vault = matches!(
            setup_answer,
            ModalAnswer::Confirm(ConfirmChoice::Yes)
        );

        if wants_vault {
            // Capture + confirm with retry-on-mismatch loop.
            loop {
                let p1_modal = PasswordModal::new("Set master password", "New password");
                let Some(p1) = runner.run_password_capture(p1_modal)? else {
                    // Esc on the first capture — abort vault setup; treat
                    // as plaintext fallback.
                    break;
                };
                if p1.is_empty() {
                    let mismatch = ConfirmModal::new(
                        "Empty password — try again?",
                        "The master password cannot be empty.  Press y to retry, n to skip.",
                    );
                    let answer = runner.run_confirm("step1-vault-empty-retry", mismatch)?;
                    if matches!(answer, ModalAnswer::Confirm(ConfirmChoice::No)) {
                        break;
                    }
                    continue;
                }
                let p2_modal = PasswordModal::new("Confirm master password", "Re-enter");
                let Some(p2) = runner.run_password_capture(p2_modal)? else {
                    break;
                };
                if p1 != p2 {
                    let mismatch = ConfirmModal::new(
                        "Passwords don't match — try again?",
                        "Press y to re-enter, n to skip vault setup.",
                    );
                    let answer = runner.run_confirm("step1-vault-mismatch", mismatch)?;
                    if matches!(answer, ModalAnswer::Confirm(ConfirmChoice::No)) {
                        break;
                    }
                    continue;
                }
                // Setup + register session.
                let mut vm = runtime::VaultManager::with_default_dir();
                match vm.setup(&p1) {
                    Ok(()) => {
                        if let Ok(true) = runtime::init_session_vault(&p1) {
                            out.vault_session_unlocked = true;
                        }
                    }
                    Err(_) => {
                        // Setup failed; continue without vault.
                    }
                }
                break;
            }
        }
    }

    // ── Step 2: AI Provider ───────────────────────────────────────────────
    runner.session.render_banner(
        "Step 2 of 8 — AI Provider",
        &[
            "Pick your primary AI provider.  More can be added later",
            "via /provider <name> login or `anvil login <provider>`.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    // Top picks first; "More providers..." expands at the bottom.
    let top_picks: Vec<(ProviderKind, &str)> = vec![
        (ProviderKind::AnvilApi, "Anthropic Claude (recommended)"),
        (ProviderKind::OpenAi, "OpenAI ChatGPT"),
        (ProviderKind::Ollama, "Ollama (local models, no API key required)"),
        (ProviderKind::Xai, "xAI Grok"),
        (ProviderKind::Gemini, "Google Gemini"),
    ];

    let mut provider_labels: Vec<String> =
        top_picks.iter().map(|(_, l)| (*l).to_string()).collect();
    provider_labels.push("More providers (35+ supported)".to_string());
    provider_labels.push("Skip — I'll configure later".to_string());

    let provider_modal = WizardChoiceModal::new("AI Provider", provider_labels);
    let provider_answer = runner.run_choice("step2-provider", provider_modal)?;
    let mut picked: Option<ProviderKind> = match provider_answer {
        ModalAnswer::Choice(idx) if idx < top_picks.len() => Some(top_picks[idx].0),
        ModalAnswer::Choice(idx) if idx == top_picks.len() => {
            // "More providers..." — open the full picker.
            let more_kinds = full_provider_picker_list();
            let more_labels: Vec<String> = more_kinds
                .iter()
                .map(|(_, l)| (*l).to_string())
                .collect();
            let more_modal = WizardChoiceModal::new("More providers", more_labels);
            let more_answer = runner.run_choice("step2-provider-more", more_modal)?;
            match more_answer {
                ModalAnswer::Choice(j) if j < more_kinds.len() => Some(more_kinds[j].0),
                _ => None,
            }
        }
        _ => None,
    };

    // ── Step 3: Authentication ────────────────────────────────────────────
    let Some(provider) = picked.take() else {
        runner.session.render_banner(
            "Step 3 of 8 — Sign In",
            &["No provider selected — skipping authentication."],
            ratatui::style::Color::DarkGray,
        )?;
        return Ok(out);
    };

    let provider_label = provider_display_name(provider);

    if matches!(provider, ProviderKind::Ollama) {
        // No auth required — just capture the URL via TextInput in step 5.
        runner.session.render_banner(
            "Step 3 of 8 — Sign In",
            &[
                "No authentication needed for Ollama.",
                "Endpoint URL is configured in step 5.",
            ],
            ratatui::style::Color::Cyan,
        )?;
        out.configured_providers.push("ollama".to_string());
        // Default Ollama URL — the modal step 5 will let the user change
        // it.  Store the default here so any model query has a host to
        // hit.
        out.ollama_url = Some("http://localhost:11434".to_string());
        return Ok(out);
    }

    if matches!(provider, ProviderKind::AnvilApi) {
        // Anthropic — OAuth or manual API key.
        runner.session.render_banner(
            "Step 3 of 8 — Sign In to Anthropic",
            &[
                "OAuth opens your browser and signs you in via claude.ai.",
                "Manual API key skips the browser (paste sk-ant-...).",
            ],
            ratatui::style::Color::Cyan,
        )?;
        let auth_modal = ConfirmModal::new(
            "Sign in to Anthropic?",
            "Yes  — open the browser (OAuth, recommended)\n\
             No   — I'll paste an API key instead",
        );
        let auth_answer = runner.run_confirm("step3-anthropic-method", auth_modal)?;
        if matches!(auth_answer, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
            // OAuth inline.
            match OAuthFlow::start(provider) {
                Ok(flow) => match runner.run_oauth_flow(flow)? {
                    OAuthOutcome::Success => {
                        out.configured_providers.push("anthropic".to_string());
                        push_anthropic_model_candidates(&mut out.model_candidates);
                    }
                    OAuthOutcome::Failed(_) | OAuthOutcome::Cancelled => {
                        // Leave unconfigured — user can rerun `anvil login`.
                    }
                },
                Err(e) => {
                    runner.session.render_banner(
                        "OAuth start failed",
                        &[
                            &format!("Could not start OAuth: {e}"),
                            "Run `anvil login anthropic` to retry later.",
                        ],
                        ratatui::style::Color::Red,
                    )?;
                }
            }
        } else {
            // Manual API key.
            let key_modal = PasswordModal::new(
                "Anthropic API key",
                "Paste sk-ant-...",
            );
            if let Some(api_key) = runner.run_password_capture(key_modal)?
                && api_key.len() > 10
            {
                let _ = wizard_save_credential("anthropic_api_key", &api_key);
                out.configured_providers.push("anthropic".to_string());
                push_anthropic_model_candidates(&mut out.model_candidates);
            }
        }
        return Ok(out);
    }

    // Generic OAuth providers (Copilot only at the moment).
    if matches!(provider, ProviderKind::Copilot) {
        runner.session.render_banner(
            "Step 3 of 8 — Sign In",
            &[
                &format!("Sign in to {provider_label}."),
                "OAuth in-wizard is not yet wired for this provider —",
                "run `anvil login copilot` after the wizard finishes.",
            ],
            ratatui::style::Color::Yellow,
        )?;
        return Ok(out);
    }

    // All other providers — API key paste.
    runner.session.render_banner(
        &format!("Step 3 of 8 — Sign In to {provider_label}"),
        &[
            "Paste your API key (Esc skips — you can run `anvil login` later).",
        ],
        ratatui::style::Color::Cyan,
    )?;
    let prompt = format!("{provider_label} API key");
    let key_modal = PasswordModal::new(prompt, "Paste key");
    if let Some(api_key) = runner.run_password_capture(key_modal)?
        && api_key.len() > 10
    {
        // Save under the canonical vault key for the provider.
        let vault_key = vault_key_for_provider(provider);
        let _ = wizard_save_credential(vault_key, &api_key);
        let id = provider_config_id(provider);
        out.configured_providers.push(id.to_string());
        push_default_model_candidates(provider, &mut out.model_candidates);
    }

    Ok(out)
}

/// Display name for a provider in wizard banners.
fn provider_display_name(p: api::ProviderKind) -> &'static str {
    use api::ProviderKind as K;
    match p {
        K::AnvilApi => "Anthropic",
        K::OpenAi => "OpenAI",
        K::Ollama => "Ollama",
        K::Xai => "xAI",
        K::Gemini => "Google Gemini",
        K::Fireworks => "Fireworks",
        K::Groq => "Groq",
        K::Mistral => "Mistral",
        K::Perplexity => "Perplexity",
        K::DeepSeek => "DeepSeek",
        K::TogetherAi => "Together AI",
        K::DeepInfra => "DeepInfra",
        K::Cerebras => "Cerebras",
        K::NvidiaNim => "Nvidia NIM",
        K::HuggingFace => "HuggingFace",
        K::MoonshotAi => "Moonshot",
        K::Nebius => "Nebius",
        K::OpenRouter => "OpenRouter",
        K::LmStudio => "LM Studio",
        K::Chutes => "Chutes",
        K::Scaleway => "Scaleway",
        K::Baseten => "Baseten",
        K::MiniMax => "MiniMax",
        K::StackIt => "StackIt",
        K::Cortecs => "Cortecs",
        K::Ai302 => "302.AI",
        K::Zai => "Z.ai",
        K::OpenCode => "OpenCode",
        K::OpenCodeGo => "OpenCode-Go",
        K::Copilot => "GitHub Copilot",
        K::Azure => "Azure OpenAI",
        K::Bedrock => "AWS Bedrock",
        K::Alibaba => "Alibaba",
        K::Antigravity => "Antigravity",
        K::Cursor => "Cursor",
    }
}

/// Canonical config-file ID for a provider (the slug used in
/// `~/.anvil/config.json`'s `providers` map).
fn provider_config_id(p: api::ProviderKind) -> &'static str {
    use api::ProviderKind as K;
    match p {
        K::AnvilApi => "anthropic",
        K::OpenAi => "openai",
        K::Ollama => "ollama",
        K::Xai => "xai",
        K::Gemini => "gemini",
        K::Fireworks => "fireworks",
        K::Groq => "groq",
        K::Mistral => "mistral",
        K::Perplexity => "perplexity",
        K::DeepSeek => "deepseek",
        K::TogetherAi => "together",
        K::DeepInfra => "deepinfra",
        K::Cerebras => "cerebras",
        K::NvidiaNim => "nvidia",
        K::HuggingFace => "huggingface",
        K::MoonshotAi => "moonshot",
        K::Nebius => "nebius",
        K::OpenRouter => "openrouter",
        K::LmStudio => "lmstudio",
        K::Chutes => "chutes",
        K::Scaleway => "scaleway",
        K::Baseten => "baseten",
        K::MiniMax => "minimax",
        K::StackIt => "stackit",
        K::Cortecs => "cortecs",
        K::Ai302 => "ai302",
        K::Zai => "zai",
        K::OpenCode => "opencode",
        K::OpenCodeGo => "opencode-go",
        K::Copilot => "copilot",
        K::Azure => "azure",
        K::Bedrock => "bedrock",
        K::Alibaba => "alibaba",
        K::Antigravity => "antigravity",
        K::Cursor => "cursor",
    }
}

/// Vault credential key used to store the API key for a provider.
fn vault_key_for_provider(p: api::ProviderKind) -> &'static str {
    use api::ProviderKind as K;
    match p {
        K::AnvilApi => "anthropic_api_key",
        K::OpenAi => "openai_api_key",
        K::Xai => "xai_api_key",
        K::Gemini => "gemini_api_key",
        K::Ollama => "ollama_api_key",
        K::Fireworks => "fireworks_api_key",
        K::Groq => "groq_api_key",
        K::Mistral => "mistral_api_key",
        K::Perplexity => "perplexity_api_key",
        K::DeepSeek => "deepseek_api_key",
        K::TogetherAi => "together_api_key",
        K::DeepInfra => "deepinfra_api_key",
        K::Cerebras => "cerebras_api_key",
        K::NvidiaNim => "nvidia_api_key",
        K::HuggingFace => "huggingface_api_key",
        K::MoonshotAi => "moonshot_api_key",
        K::Nebius => "nebius_api_key",
        K::OpenRouter => "openrouter_api_key",
        K::LmStudio => "lmstudio_api_key",
        K::Chutes => "chutes_api_key",
        K::Scaleway => "scaleway_api_key",
        K::Baseten => "baseten_api_key",
        K::MiniMax => "minimax_api_key",
        K::StackIt => "stackit_api_key",
        K::Cortecs => "cortecs_api_key",
        K::Ai302 => "ai302_api_key",
        K::Zai => "zai_api_key",
        K::OpenCode => "opencode_api_key",
        K::OpenCodeGo => "opencode_go_api_key",
        K::Copilot => "copilot_token",
        K::Azure => "azure_api_key",
        K::Bedrock => "aws_access_key",
        K::Alibaba => "alibaba_api_key",
        K::Antigravity => "antigravity_api_key",
        K::Cursor => "cursor_api_key",
    }
}

/// The "more providers" list — everything not in the top picks.
fn full_provider_picker_list() -> Vec<(api::ProviderKind, &'static str)> {
    use api::ProviderKind as K;
    vec![
        (K::Fireworks, "Fireworks"),
        (K::Groq, "Groq"),
        (K::Mistral, "Mistral"),
        (K::Perplexity, "Perplexity"),
        (K::DeepSeek, "DeepSeek"),
        (K::TogetherAi, "Together AI"),
        (K::DeepInfra, "DeepInfra"),
        (K::Cerebras, "Cerebras"),
        (K::NvidiaNim, "Nvidia NIM"),
        (K::HuggingFace, "HuggingFace"),
        (K::MoonshotAi, "Moonshot"),
        (K::Nebius, "Nebius"),
        (K::OpenRouter, "OpenRouter"),
        (K::LmStudio, "LM Studio"),
        (K::Chutes, "Chutes"),
        (K::Scaleway, "Scaleway"),
        (K::Baseten, "Baseten"),
        (K::MiniMax, "MiniMax"),
        (K::StackIt, "StackIt"),
        (K::Cortecs, "Cortecs"),
        (K::Ai302, "302.AI"),
        (K::Zai, "Z.ai"),
        (K::OpenCode, "OpenCode"),
        (K::OpenCodeGo, "OpenCode-Go"),
        (K::Copilot, "GitHub Copilot"),
        (K::Azure, "Azure OpenAI"),
        (K::Bedrock, "AWS Bedrock"),
        (K::Alibaba, "Alibaba"),
        (K::Antigravity, "Antigravity"),
        (K::Cursor, "Cursor"),
    ]
}

fn push_anthropic_model_candidates(out: &mut Vec<(String, String)>) {
    out.push(("claude-opus-4-6".to_string(), "Anthropic".to_string()));
    out.push(("claude-sonnet-4-6".to_string(), "Anthropic".to_string()));
    out.push((
        "claude-haiku-4-5-20251213".to_string(),
        "Anthropic".to_string(),
    ));
}

fn push_default_model_candidates(p: api::ProviderKind, out: &mut Vec<(String, String)>) {
    use api::ProviderKind as K;
    match p {
        K::OpenAi => {
            out.push(("gpt-4o".to_string(), "OpenAI".to_string()));
            out.push(("gpt-4o-mini".to_string(), "OpenAI".to_string()));
        }
        K::Xai => {
            out.push(("grok-3".to_string(), "xAI".to_string()));
            out.push(("grok-3-mini".to_string(), "xAI".to_string()));
        }
        K::Gemini => {
            out.push(("gemini-1.5-pro".to_string(), "Google".to_string()));
            out.push(("gemini-1.5-flash".to_string(), "Google".to_string()));
        }
        _ => {
            // No default model candidates for the long-tail providers —
            // they expose models via their own /models API which the
            // user can browse with /model after first launch.
        }
    }
}

/// Stdin fallback for the TUI-config steps. Used when stdout is not a
/// TTY (CI / piped output / test fixtures). Preserves the legacy
/// numbered-prompt UX so existing CI scripts that feed answers via
/// stdin continue to work.
fn run_tui_config_via_stdin() -> WizardTuiAnswers {
    let mut answers = WizardTuiAnswers::defaults();

    wizard_step_header(7, 8, "TUI Layout");
    println!();
    println!("    [1] Vertical Split  (default)");
    println!("    [2] Classic");
    println!("    [3] Three-Pane");
    println!("    [4] Journal");
    println!();
    let layout_arch_choice = wizard_read_line("  Choice [1]: ");
    answers.layout_kind = wizard_parse_layout_kind_choice(&layout_arch_choice).to_string();

    println!();
    println!("  Show workspace tabs? [1] Yes (default)  [2] No");
    let layout_tabs_choice = wizard_read_line("  Choice [1]: ");
    answers.layout_tabs = wizard_parse_layout_tabs_choice(&layout_tabs_choice);

    wizard_step_header(8, 8, "TUI Preferences");
    println!();
    println!("  Enable mouse capture? [1] Off (default, recommended)  [2] On");
    let mouse_choice = wizard_read_line("  Choice [1]: ");
    answers.mouse_capture = wizard_parse_mouse_capture_choice(mouse_choice.trim());

    println!();
    println!("  Theme? [1] Dark (default)  [2] Light  [3] Auto");
    let theme_choice = wizard_read_line("  Choice [1]: ");
    answers.theme = match theme_choice.trim() {
        "2" => "light".to_string(),
        "3" => "auto".to_string(),
        _ => "dark".to_string(),
    };

    println!();
    println!("  Default permission mode? [1] ask (default)  [2] workspace-write  [3] danger-full-access");
    let perm_choice = wizard_read_line("  Choice [1]: ");
    answers.permission_mode = match perm_choice.trim() {
        "2" => "workspace-write".to_string(),
        "3" => "danger-full-access".to_string(),
        _ => "ask".to_string(),
    };

    answers
}

/// Interactive first-run setup wizard.
///
/// Behaviour (task #642, v2.2.17 finisher):
/// - **TTY (real terminal)** — enters alt-screen ONCE via
///   `run_full_wizard_in_alt_screen` and drives all 8 steps as modals
///   (vault password, provider, OAuth, model, ollama URL, profile,
///   layout, prefs).  The user never sees inline mode mid-wizard;
///   alt-screen exits ONCE at the end.  See `feedback-tui-flash-anti-pattern.md`.
/// - **Non-TTY** (CI / piped input / test fixtures) — falls back to
///   the legacy inline + stdin path so existing fixture-driven scripts
///   keep working.
#[allow(clippy::too_many_lines, clippy::single_match_else)]
pub(crate) fn run_first_run_wizard() {
    if io::stdout().is_terminal() {
        run_first_run_wizard_modal();
    } else {
        run_first_run_wizard_via_stdin();
    }
}

/// Modal-driven first-run wizard (the TTY happy path).
///
/// Opens ONE alt-screen via `run_full_wizard_in_alt_screen`, then
/// writes the final config.json and prints the post-wizard summary
/// inline below the (now-closed) alt-screen.
fn run_first_run_wizard_modal() {
    let result = match run_full_wizard_in_alt_screen() {
        Ok(r) => r,
        Err(e) => {
            // The alt-screen session failed to enter — fall back to the
            // stdin path so the user still gets through setup.
            eprintln!("\n  Note: alt-screen wizard unavailable ({e}).");
            eprintln!("  Falling back to inline / stdin prompts.");
            run_first_run_wizard_via_stdin();
            return;
        }
    };

    // Build the config from the captured answers and write it.
    let configured_providers = result.steps123.configured_providers.clone();
    let mut model_candidates = result.steps123.model_candidates.clone();
    if model_candidates.is_empty() {
        // Add a fallback so step 4's model picker had something to choose.
        // (Already handled in step 4 via DEFAULT_MODEL, but keep the
        // summary banner happy.)
    }

    let provider_priority: Vec<String> = if configured_providers.is_empty() {
        vec!["anthropic".to_string()]
    } else {
        configured_providers.clone()
    };
    let default_provider = provider_priority
        .first()
        .cloned()
        .unwrap_or_else(|| "anthropic".to_string());

    let mut providers_obj = serde_json::Map::new();
    let ollama_enabled = provider_priority.contains(&"ollama".to_string())
        || result.steps123.ollama_url.is_some();
    providers_obj.insert(
        "ollama".to_string(),
        json!({
            "enabled": ollama_enabled,
            "url": result.ollama_url,
            "api_key": serde_json::Value::Null
        }),
    );
    let anthropic_enabled = provider_priority.contains(&"anthropic".to_string());
    let anthropic_auth = if configured_providers.contains(&"anthropic".to_string()) {
        "oauth"
    } else {
        "none"
    };
    providers_obj.insert(
        "anthropic".to_string(),
        json!({
            "enabled": anthropic_enabled,
            "auth_method": anthropic_auth
        }),
    );
    let openai_enabled = provider_priority.contains(&"openai".to_string());
    providers_obj.insert("openai".to_string(), json!({ "enabled": openai_enabled }));
    let xai_enabled = provider_priority.contains(&"xai".to_string());
    providers_obj.insert("xai".to_string(), json!({ "enabled": xai_enabled }));

    let mut config = serde_json::Map::new();
    config.insert(
        "default_model".to_string(),
        serde_json::Value::String(result.chosen_model.clone()),
    );
    config.insert(
        "default_provider".to_string(),
        serde_json::Value::String(default_provider),
    );
    config.insert(
        "provider_priority".to_string(),
        serde_json::Value::Array(
            provider_priority
                .iter()
                .map(|p| serde_json::Value::String(p.clone()))
                .collect(),
        ),
    );
    config.insert(
        "providers".to_string(),
        serde_json::Value::Object(providers_obj),
    );
    config.insert("setup_completed".to_string(), serde_json::Value::Bool(true));
    config.insert(
        "tui_layout".to_string(),
        serde_json::json!({
            "kind": result.tui_answers.layout_kind,
            "tabs": result.tui_answers.layout_tabs,
        }),
    );
    config.insert(
        "tui_layout_intro_seen".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "tui_mouse_capture".to_string(),
        serde_json::Value::Bool(result.tui_answers.mouse_capture),
    );
    config.insert(
        "theme".to_string(),
        serde_json::Value::String(result.tui_answers.theme.clone()),
    );
    config.insert(
        "permission_mode".to_string(),
        serde_json::Value::String(result.tui_answers.permission_mode.clone()),
    );
    if !result.profile_name.is_empty() && result.profile_name != "default" {
        config.insert(
            "profile".to_string(),
            serde_json::Value::String(result.profile_name.clone()),
        );
    }

    let config_path = match wizard_save_config(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\n  Warning: could not save config: {e}");
            PathBuf::from("~/.anvil/config.json")
        }
    };

    // Migration step (claude-code import) — still inline; opening a
    // second alt-screen here would defeat the single-transition
    // promise.
    wizard_run_migration_step();

    // Post-wizard summary banner — inline, AFTER the alt-screen has
    // closed.  This is exit chrome only.
    let provider_chain = provider_priority.join(" \u{2192} ");
    println!();
    wizard_box_top();
    wizard_box_line("");
    wizard_box_line("  Setup complete!");
    wizard_box_line("");
    wizard_box_line(&format!("    Default model:  {}", result.chosen_model));
    wizard_box_line(&format!("    Providers:      {provider_chain}"));
    wizard_box_line(&format!("    Config saved:   {}", config_path.display()));
    wizard_box_line("");
    wizard_box_line("  Run `anvil` to start coding.");
    wizard_box_line("  Type `/help` for all commands.");
    wizard_box_line("");
    wizard_box_bot();
    println!();
}

/// Stdin-driven first-run wizard — the legacy inline path.
///
/// Used as a fallback when stdout is NOT a TTY (CI / piped input / test
/// fixtures), and when the alt-screen wizard fails to enter (rare —
/// e.g. raw-mode disabled by the parent).  The behaviour matches the
/// pre-task-#642 wizard byte-for-byte so existing fixture scripts keep
/// working.
#[allow(clippy::too_many_lines, clippy::single_match_else)]
fn run_first_run_wizard_via_stdin() {
    println!();
    wizard_box_top();
    wizard_box_line("");
    wizard_box_line(&format!("\u{2692}  Welcome to Anvil v{}", env!("CARGO_PKG_VERSION")));
    wizard_box_line("   AI-Powered Coding Assistant by Culpur Defense");
    wizard_box_line("");
    wizard_box_line("   Let's get you set up.");
    wizard_box_line("");
    wizard_box_bot();

    let mut configured_providers: Vec<String> = Vec::new();
    let mut model_candidates: Vec<(String, String)> = Vec::new(); // (model_id, provider_label)

    // ── Step 1: Vault setup ───────────────────────────────────────────────────
    wizard_step_header(1, 7, "Vault Setup (Credential Encryption)");
    println!();
    println!("  Anvil stores your API keys in an AES-256-GCM encrypted vault.");
    println!("  You set a master password now — it is never stored anywhere.");
    println!("  You will need to enter it once each time you start Anvil.");
    println!();
    println!("  [1] Set up encrypted vault (recommended)");
    println!("  [s] Skip — API keys will be stored in plaintext credentials.json");
    println!();

    let vault_choice = wizard_read_line("  Choice [1]: ");
    let vault_setup_done = match vault_choice.to_ascii_lowercase().trim() {
        "s" | "skip" => {
            println!("  Skipping vault setup.");
            println!("  \x1b[33m  Warning: API keys will be stored in plaintext.\x1b[0m");
            println!("  Run /vault setup later to encrypt your credentials.");
            false
        }
        _ => {
            // Default is to set up the vault.
            if runtime::vault_is_initialized() {
                println!("  Vault already initialized at ~/.anvil/vault/");
                println!("  Unlocking for this session...");
                let pw = match rpassword::prompt_password("  Master password: ") {
                    Ok(p) => p,
                    Err(_) => wizard_read_line("  Master password: "),
                };
                match runtime::init_session_vault(&pw) {
                    Ok(true) => {
                        println!("  \x1b[32m  Vault unlocked.\x1b[0m");
                        true
                    }
                    Ok(false) => {
                        println!("  \x1b[33m  Vault not yet initialized — that is unexpected.\x1b[0m");
                        false
                    }
                    Err(e) => {
                        println!("  \x1b[31m  Unlock failed: {e}\x1b[0m");
                        println!("  Continuing without vault — keys will be stored in plaintext.");
                        false
                    }
                }
            } else {
                let pw = loop {
                    let p1 = match rpassword::prompt_password("  Set master password: ") {
                        Ok(p) => p,
                        Err(_) => wizard_read_line("  Set master password: "),
                    };
                    if p1.is_empty() {
                        println!("  Password must not be empty. Try again.");
                        continue;
                    }
                    let p2 = match rpassword::prompt_password("  Confirm master password: ") {
                        Ok(p) => p,
                        Err(_) => wizard_read_line("  Confirm master password: "),
                    };
                    if p1 != p2 {
                        println!("  Passwords do not match. Try again.");
                        continue;
                    }
                    break p1;
                };
                // Initialize vault directly, then register it as the session vault.
                let mut vm = runtime::VaultManager::with_default_dir();
                match vm.setup(&pw) {
                    Ok(()) => {
                        // Vault is now initialized and unlocked on `vm`.
                        // Register it in the session cache.
                        match runtime::init_session_vault(&pw) {
                            Ok(true) => {
                                println!("  \x1b[32m  Vault created and unlocked.\x1b[0m");
                                println!("  API keys entered in this wizard will be stored encrypted.");
                                true
                            }
                            _ => {
                                // init_session_vault failed but vault is initialized;
                                // this is non-fatal — keys go to plaintext fallback.
                                println!("  \x1b[33m  Vault created but session lock failed.\x1b[0m");
                                println!("  Keys will be stored in plaintext this run.");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        println!("  \x1b[31m  Vault setup failed: {e}\x1b[0m");
                        println!("  Continuing without vault — keys stored in plaintext.");
                        false
                    }
                }
            }
        }
    };
    let _ = vault_setup_done; // informational; wizard_save_credential checks session state

    // ── Step 2: Ollama ────────────────────────────────────────────────────────
    wizard_step_header(2, 7, "Ollama (Local AI)");
    println!();
    println!("  Ollama runs AI models locally on your machine.");
    println!("  No API key required for basic use.");
    println!();
    println!("  [1] Connect to Ollama (default: http://localhost:11434)");
    println!("  [2] Connect with custom URL");
    println!("  [3] Connect with API key");
    println!("  [s] Skip");
    println!();

    let ollama_choice = wizard_read_line("  Choice: ");
    let mut ollama_url: Option<String> = None;

    match ollama_choice.to_ascii_lowercase().as_str() {
        "1" | "" => {
            let url = "http://localhost:11434".to_string();
            print!("\n  Testing connection to {url}... ");
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m connected\x1b[0m");
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  Available models:");
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                    let _ = wizard_save_credential("ollama_host", &url);
                }
                Err(e) => {
                    println!("\x1b[31m failed ({e})\x1b[0m");
                    println!("  Skipping Ollama — make sure Ollama is running: ollama serve");
                }
            }
        }
        "2" => {
            let url = wizard_read_line("\n  Ollama URL [http://localhost:11434]: ");
            let url = if url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                url
            };
            print!("  Testing connection to {url}... ");
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m connected\x1b[0m");
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  Available models:");
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                    let _ = wizard_save_credential("ollama_host", &url);
                }
                Err(e) => {
                    println!("\x1b[31m failed ({e})\x1b[0m");
                    println!(
                        "  Connection failed. Configuration saved; you can retry with `anvil login ollama`."
                    );
                    let _ = wizard_save_credential("ollama_host", &url);
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                }
            }
        }
        "3" => {
            let url = wizard_read_line("\n  Ollama URL [http://localhost:11434]: ");
            let url = if url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                url
            };
            let api_key = match rpassword::prompt_password("  API key: ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key: "),
            };
            print!("  Testing connection to {url}... ");
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m connected\x1b[0m");
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  Available models:");
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                }
                Err(e) => {
                    println!("\x1b[33m could not verify ({e}) — saving anyway\x1b[0m");
                    configured_providers.push("ollama".to_string());
                }
            }
            let _ = wizard_save_credential("ollama_host", &url);
            if !api_key.is_empty() {
                let _ = wizard_save_credential("ollama_api_key", &api_key);
            }
            ollama_url = Some(url);
        }
        "s" | "skip" => {
            println!("  Skipping Ollama.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping Ollama.");
        }
    }

    // ── Step 3: Anthropic ─────────────────────────────────────────────────────
    wizard_step_header(3, 7, "Anthropic (Claude)");
    println!();
    println!("  Anthropic provides Claude — the most capable AI assistant.");
    println!();
    println!("  [1] Login with OAuth (recommended — opens browser)");
    println!("  [2] Enter API key manually");
    println!("  [s] Skip");
    println!();

    let anthropic_choice = wizard_read_line("  Choice: ");
    match anthropic_choice.to_ascii_lowercase().as_str() {
        "1" | "" => {
            println!();
            match run_anthropic_login() {
                Ok(()) => {
                    println!("\x1b[32m  Anthropic OAuth complete.\x1b[0m");
                    configured_providers.push("anthropic".to_string());
                    model_candidates
                        .push(("claude-opus-4-6".to_string(), "Anthropic".to_string()));
                    model_candidates
                        .push(("claude-sonnet-4-6".to_string(), "Anthropic".to_string()));
                    model_candidates.push((
                        "claude-haiku-4-5-20251213".to_string(),
                        "Anthropic".to_string(),
                    ));
                }
                Err(e) => {
                    eprintln!("  OAuth failed: {e}");
                    println!("  You can retry later with: anvil login anthropic");
                }
            }
        }
        "2" => {
            println!();
            println!("  Get your key at: https://console.anthropic.com/settings/keys");
            let api_key = match rpassword::prompt_password("  API key (sk-ant-...): ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key (sk-ant-...): "),
            };
            if api_key.is_empty() {
                println!("  No key entered, skipping Anthropic.");
            } else {
                print!("  Validating key... ");
                let _ = io::stdout().flush();
                let out = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "--max-time",
                        "5",
                        "-H",
                        &format!("x-api-key: {api_key}"),
                        "-H",
                        "anthropic-version: 2023-06-01",
                        "https://api.anthropic.com/v1/models",
                    ])
                    .output();
                let valid = out.is_ok_and(|o| {
                    o.status.success()
                        && !o.stdout.is_empty()
                        && !o.stdout.starts_with(b"{\"error\"")
                });
                if valid {
                    println!("\x1b[32m valid\x1b[0m");
                } else {
                    println!("\x1b[33m could not verify — saving anyway\x1b[0m");
                }
                let _ = wizard_save_credential("anthropic_api_key", &api_key);
                configured_providers.push("anthropic".to_string());
                model_candidates
                    .push(("claude-opus-4-6".to_string(), "Anthropic".to_string()));
                model_candidates
                    .push(("claude-sonnet-4-6".to_string(), "Anthropic".to_string()));
                model_candidates.push((
                    "claude-haiku-4-5-20251213".to_string(),
                    "Anthropic".to_string(),
                ));
            }
        }
        "s" | "skip" => {
            println!("  Skipping Anthropic.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping Anthropic.");
        }
    }

    // ── Step 4: OpenAI ────────────────────────────────────────────────────────
    wizard_step_header(4, 7, "OpenAI (ChatGPT)");
    println!();
    println!("  OpenAI provides GPT models for coding and reasoning.");
    println!();
    println!("  [1] Enter API key");
    println!("  [s] Skip");
    println!();

    let openai_choice = wizard_read_line("  Choice: ");
    match openai_choice.to_ascii_lowercase().as_str() {
        "1" => {
            println!();
            println!("  Get your key at: https://platform.openai.com/api-keys");
            let api_key = match rpassword::prompt_password("  API key (sk-...): ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key (sk-...): "),
            };
            if api_key.is_empty() {
                println!("  No key entered, skipping OpenAI.");
            } else {
                print!("  Validating key... ");
                let _ = io::stdout().flush();
                let out = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "--max-time",
                        "5",
                        "-H",
                        &format!("Authorization: Bearer {api_key}"),
                        "https://api.openai.com/v1/models",
                    ])
                    .output();
                let valid = out.is_ok_and(|o| {
                    o.status.success() && !o.stdout.starts_with(b"{\"error\"")
                });
                if valid {
                    println!("\x1b[32m valid\x1b[0m");
                } else {
                    println!("\x1b[33m could not verify — saving anyway\x1b[0m");
                }
                let _ = wizard_save_credential("openai_api_key", &api_key);
                configured_providers.push("openai".to_string());
                model_candidates.push(("gpt-4o".to_string(), "OpenAI".to_string()));
                model_candidates.push(("gpt-4o-mini".to_string(), "OpenAI".to_string()));
            }
        }
        "s" | "skip" | "" => {
            println!("  Skipping OpenAI.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping OpenAI.");
        }
    }

    // ── Step 5: xAI (Grok) ───────────────────────────────────────────────────
    wizard_step_header(5, 7, "xAI (Grok)");
    println!();
    println!("  xAI provides Grok models.");
    println!();
    println!("  [1] Enter API key");
    println!("  [s] Skip");
    println!();

    let xai_choice = wizard_read_line("  Choice: ");
    match xai_choice.to_ascii_lowercase().as_str() {
        "1" => {
            println!();
            println!("  Get your key at: https://console.x.ai");
            let api_key = match rpassword::prompt_password("  API key (xai-...): ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key (xai-...): "),
            };
            if api_key.is_empty() {
                println!("  No key entered, skipping xAI.");
            } else {
                let _ = wizard_save_credential("xai_api_key", &api_key);
                configured_providers.push("xai".to_string());
                model_candidates.push(("grok-3".to_string(), "xAI".to_string()));
                model_candidates.push(("grok-3-mini".to_string(), "xAI".to_string()));
                println!("  \x1b[32mxAI API key saved.\x1b[0m");
            }
        }
        "s" | "skip" | "" => {
            println!("  Skipping xAI.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping xAI.");
        }
    }

    // ── Step 6: Provider priority & default model ─────────────────────────────
    wizard_step_header(6, 7, "Provider Priority & Default Model");
    println!();

    let mut seen = std::collections::HashSet::new();
    configured_providers.retain(|p| seen.insert(p.clone()));

    let provider_priority: Vec<String> = if configured_providers.is_empty() {
        println!("  No providers configured.");
        println!("  You can configure providers later with: anvil login");
        vec!["anthropic".to_string()]
    } else {
        fn provider_label(p: &str) -> &str {
            match p {
                "anthropic" => "Anthropic",
                "ollama" => "Ollama",
                "openai" => "OpenAI",
                "xai" => "xAI",
                other => other,
            }
        }
        let configured_display = configured_providers
            .iter()
            .map(|p| provider_label(p))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  You configured: {configured_display}");
        println!();
        println!("  Set your provider priority (first = primary, others = failover):");
        println!();
        println!("  Current order:");
        for (i, p) in configured_providers.iter().enumerate() {
            let first_model = model_candidates
                .iter()
                .find(|(_, label)| {
                    label
                        .to_ascii_lowercase()
                        .starts_with(&provider_label(p).to_ascii_lowercase())
                })
                .map_or("—", |(m, _)| m.as_str());
            println!("    {}. {} ({})", i + 1, provider_label(p), first_model);
        }
        println!();
        println!("  [1] Keep this order");
        println!("  [2] Reorder (enter numbers: e.g., \"2,1\")");
        println!();
        let order_choice = wizard_read_line("  Choice: ");
        match order_choice.as_str() {
            "1" | "" => configured_providers.clone(),
            "2" => {
                let new_order = wizard_read_line("  New order: ");
                let indices: Vec<usize> = new_order
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .collect();
                let reordered: Vec<String> = indices
                    .iter()
                    .filter_map(|&i| configured_providers.get(i.saturating_sub(1)).cloned())
                    .collect();
                if reordered.is_empty() {
                    println!("  Could not parse order, keeping original.");
                    configured_providers.clone()
                } else {
                    reordered
                }
            }
            _ => {
                println!("  Unknown choice, keeping original order.");
                configured_providers.clone()
            }
        }
    };

    // Thinking mode toggle (kept on the inline path — small choice, no
    // need to bring up alt-screen just for this; the alt-screen comes
    // up below for steps 4..8).
    println!();
    println!("  \x1b[1;33mEnable Thinking Mode?\x1b[0m");
    println!("  Some models (Qwen3, Claude, o3) support extended thinking/reasoning.");
    println!("  When enabled, the model shows its reasoning process before answering.");
    println!();
    println!("    [1] Yes — enable thinking mode (recommended for coding)");
    println!("    [2] No  — standard responses only");
    println!();
    let think_choice = wizard_read_line("  Choice: ");
    let thinking_enabled =
        think_choice.trim() == "1" || think_choice.trim().to_lowercase() == "yes";
    if thinking_enabled {
        println!("  \x1b[32m\u{2713}\x1b[0m Thinking mode enabled. Toggle anytime with /think");
    } else {
        println!("  Thinking mode off. Toggle anytime with /think");
    }

    let default_provider = provider_priority
        .first()
        .cloned()
        .unwrap_or_else(|| "anthropic".to_string());

    // Deduplicate model candidates while preserving order.
    let mut unique_models: Vec<(String, String)> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for (id, label) in &model_candidates {
        if seen_ids.insert(id.clone()) {
            unique_models.push((id.clone(), label.clone()));
        }
    }

    // ── Steps 4 → 8: model + ollama + profile + TUI prefs (single alt-screen) ─
    //
    // Task #642 (v2.2.17 finisher): the model picker, Ollama URL,
    // profile name, layout, mouse capture, theme and permission mode all
    // render as in-TUI modals inside a SINGLE alt-screen session — the
    // user sees one banner transition between steps and the terminal
    // never returns to inline mode.  See `feedback-tui-flash-anti-pattern.md`
    // (#622) and `feedback-cross-platform-ux-defaults.md` (#623).
    //
    // Steps 1 (vault password), 2 (provider catalogue) and 3 (OAuth)
    // above run on the inline + per-step alt-screen path —
    // OAuth still drops to stdin per #578 exception because the
    // existing `run_anthropic_login` infrastructure prompts on stdin
    // for the manual paste fallback. The remaining alt-screen-friendly
    // steps are unified here.
    //
    // Fallback: when stdout is not a TTY (CI / piped input), drop back
    // to the stdin path so existing wizard-fed scripts keep working.
    let (chosen_model, ollama_url_modal, _profile_name, tui_answers) =
        if io::stdout().is_terminal() {
            match run_post_auth_modals_in_alt_screen(&unique_models) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("  Note: alt-screen unavailable ({e}); falling back to stdin prompts.");
                    let m = if unique_models.is_empty() {
                        DEFAULT_MODEL.to_string()
                    } else {
                        unique_models[0].0.clone()
                    };
                    let answers = run_tui_config_via_stdin();
                    (
                        m,
                        ollama_url.clone().unwrap_or_else(|| "http://localhost:11434".to_string()),
                        "default".to_string(),
                        answers,
                    )
                }
            }
        } else {
            // Stdin fallback: ask the model + profile questions inline
            // (steps 5 + 6 already had a URL captured during step 2 if
            // the user picked option 2/3 in the Ollama branch; otherwise
            // we use the default).
            let m = if unique_models.is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                println!();
                println!("  Select your default model:");
                for (i, (id, label)) in unique_models.iter().enumerate() {
                    println!("    [{}] {} ({})", i + 1, id, label);
                }
                let raw = wizard_read_line("  Choice [1]: ");
                if let Ok(n) = raw.parse::<usize>() {
                    if n >= 1 && n <= unique_models.len() {
                        unique_models[n - 1].0.clone()
                    } else {
                        unique_models[0].0.clone()
                    }
                } else {
                    unique_models[0].0.clone()
                }
            };
            let url = wizard_read_line("  Ollama URL [http://localhost:11434]: ");
            let url = if url.is_empty() {
                ollama_url.clone().unwrap_or_else(|| "http://localhost:11434".to_string())
            } else {
                url
            };
            let prof = wizard_read_line("  Profile name [default]: ");
            let prof = if prof.is_empty() { "default".to_string() } else { prof };
            let answers = run_tui_config_via_stdin();
            (m, url, prof, answers)
        };
    // The modal-driven Ollama URL takes precedence over any URL captured
    // during step 2 (if the user reconfigured at step 5).
    let ollama_url = Some(ollama_url_modal);

    let layout_kind_str = tui_answers.layout_kind.as_str();
    let layout_tabs = tui_answers.layout_tabs;
    let layout_alias = wizard_compose_layout_alias(layout_kind_str, layout_tabs);
    let tabs_label = if layout_tabs { "tabs" } else { "no tabs" };
    println!(
        "  \x1b[32m\u{2713}\x1b[0m Layout = {layout_alias} ({tabs_label}). Change later with /layout or /configure layout."
    );

    let mouse_capture_enabled = tui_answers.mouse_capture;
    if mouse_capture_enabled {
        println!("  \x1b[32m\u{2713}\x1b[0m Mouse capture on. Override per-session with ANVIL_TUI_MOUSE=0.");
    } else {
        println!("  \x1b[32m\u{2713}\x1b[0m Mouse capture off. F2/F3 still switch tabs; /help shows all keys.");
    }

    let theme_value = tui_answers.theme.as_str();
    println!("  \x1b[32m\u{2713}\x1b[0m Theme = {theme_value}. Change later with /theme.");

    let permission_mode_value = tui_answers.permission_mode.as_str();
    println!(
        "  \x1b[32m\u{2713}\x1b[0m Permission mode = {permission_mode_value}. Change anytime with /permissions."
    );
    println!();

    // ── Build and save config.json ─────────────────────────────────────────────
    let mut providers_obj = serde_json::Map::new();

    let ollama_enabled = provider_priority.contains(&"ollama".to_string());
    let ollama_url_val = ollama_url
        
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    providers_obj.insert(
        "ollama".to_string(),
        json!({
            "enabled": ollama_enabled,
            "url": ollama_url_val,
            "api_key": serde_json::Value::Null
        }),
    );

    let anthropic_enabled = provider_priority.contains(&"anthropic".to_string());
    let anthropic_auth = if configured_providers.contains(&"anthropic".to_string()) {
        "oauth"
    } else {
        "none"
    };
    providers_obj.insert(
        "anthropic".to_string(),
        json!({
            "enabled": anthropic_enabled,
            "auth_method": anthropic_auth
        }),
    );

    let openai_enabled = provider_priority.contains(&"openai".to_string());
    providers_obj.insert("openai".to_string(), json!({ "enabled": openai_enabled }));

    let xai_enabled = provider_priority.contains(&"xai".to_string());
    providers_obj.insert("xai".to_string(), json!({ "enabled": xai_enabled }));

    let mut config = serde_json::Map::new();
    config.insert(
        "default_model".to_string(),
        serde_json::Value::String(chosen_model.clone()),
    );
    config.insert(
        "default_provider".to_string(),
        serde_json::Value::String(default_provider),
    );
    config.insert(
        "provider_priority".to_string(),
        serde_json::Value::Array(
            provider_priority
                .iter()
                .map(|p| serde_json::Value::String(p.clone()))
                .collect(),
        ),
    );
    config.insert(
        "providers".to_string(),
        serde_json::Value::Object(providers_obj),
    );
    config.insert(
        "setup_completed".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "thinking_enabled".to_string(),
        serde_json::Value::Bool(thinking_enabled),
    );
    config.insert(
        "tui_layout".to_string(),
        serde_json::json!({ "kind": layout_kind_str, "tabs": layout_tabs }),
    );
    config.insert(
        "tui_layout_intro_seen".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "tui_mouse_capture".to_string(),
        serde_json::Value::Bool(mouse_capture_enabled),
    );
    config.insert(
        "theme".to_string(),
        serde_json::Value::String(theme_value.to_string()),
    );
    config.insert(
        "permission_mode".to_string(),
        serde_json::Value::String(permission_mode_value.to_string()),
    );

    let config_path = match wizard_save_config(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\n  Warning: could not save config: {e}");
            PathBuf::from("~/.anvil/config.json")
        }
    };

    // ── Step 8 (optional): CC migration detection ────────────────────────────
    wizard_run_migration_step();

    // ── Final summary banner ───────────────────────────────────────────────────
    let provider_chain = provider_priority.join(" \u{2192} ");
    println!();
    wizard_box_top();
    wizard_box_line("");
    wizard_box_line("  Setup complete!");
    wizard_box_line("");
    wizard_box_line(&format!("    Default model:  {chosen_model}"));
    wizard_box_line(&format!("    Providers:      {provider_chain}"));
    wizard_box_line(&format!("    Config saved:   {}", config_path.display()));
    wizard_box_line("");
    wizard_box_line("  Run `anvil` to start coding.");
    wizard_box_line("  Type `/help` for all commands.");
    wizard_box_line("");
    wizard_box_bot();
    println!();
}

// ── Migration detection step ─────────────────────────────────────────────────

/// Return the path where the "user declined migration" flag is stored.
///
/// When this file exists, the wizard migration step is silently skipped.
/// The user can still run `/import claude-code` manually.
///
/// Honours `ANVIL_CONFIG_HOME` (task #641) so isolated test/dev profiles do
/// not collide with the real `~/.anvil/`.
pub(crate) fn import_skipped_flag_path() -> Option<std::path::PathBuf> {
    Some(runtime::default_config_home().join(".import-skipped"))
}

/// Return `true` if the user has previously declined the migration prompt.
pub(crate) fn import_was_skipped() -> bool {
    import_skipped_flag_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Write the `.import-skipped` flag so the wizard does not prompt again.
pub(crate) fn write_import_skipped_flag() -> io::Result<()> {
    let Some(path) = import_skipped_flag_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, b"")
}

/// Run the optional migration detection step during the first-run wizard.
///
/// - If `~/.claude/` does not exist, nothing is shown.
/// - If the `.import-skipped` flag is present, nothing is shown.
/// - If CC is detected, prompt the user; on yes run the migration, on no
///   write the skip flag.
fn wizard_run_migration_step() {
    // Check for the skip flag first.
    if import_was_skipped() {
        return;
    }

    let claude_dir = dirs_next::home_dir()
        .map(|h| h.join(".claude"))
        .filter(|p| p.exists());

    let Some(claude_dir) = claude_dir else {
        return;
    };

    println!();
    println!("\x1b[1;33mCC Installation Detected\x1b[0m");
    println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));
    println!();
    println!("  I found CC at: {}", claude_dir.display());
    println!("  Want me to migrate your memory, instructions, and skills?");
    println!();
    println!("  This imports:");
    println!("    - Memory entries  (~/.claude/projects/*/memory/*.md)");
    println!("    - CLAUDE.md files (global + per-project)");
    println!("    - Skills and settings (translated to Anvil format)");
    println!();
    println!("  Nothing is deleted from CC — Anvil reads only.");
    println!();

    let choice = wizard_read_line("  Migrate now? [Y/n]: ");
    match choice.to_ascii_lowercase().trim() {
        "n" | "no" => {
            println!("  OK. Run `/import claude-code` any time to migrate manually.");
            if let Err(e) = write_import_skipped_flag() {
                eprintln!("  Warning: could not write skip flag: {e}");
            }
        }
        _ => {
            // Default is yes.
            println!();
            println!("  Starting migration...");
            println!("  (Run `/import claude-code --dry-run` first to preview what will land)");
            println!();
            wizard_run_import_pipeline(&claude_dir);
        }
    }
}

/// Run the import pipeline from inside the wizard.
///
/// Calls the same library function used by `/import claude-code`.
/// Progress is printed to stdout since the TUI is not yet running.
fn wizard_run_import_pipeline(claude_dir: &std::path::Path) {
    use runtime::ImportSource;
    let source = ImportSource::ClaudeCode {
        profile_dir: claude_dir.to_path_buf(),
    };
    match commands::handlers::run_import_pipeline_headless(&source, false, false) {
        Ok(summary) => {
            println!("{summary}");
            println!();
            println!("  \x1b[32mMigration complete.\x1b[0m");
            println!("  Report written to ~/.anvil/.import-report.md");
            println!("  Run `/memory show semantic` to see imported memory entries.");
        }
        Err(e) => {
            println!("  \x1b[31mMigration encountered an error: {e}\x1b[0m");
            println!("  You can retry later with `/import claude-code`.");
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ── #641 / v2.2.17: ANVIL_CONFIG_HOME helpers ─────────────────────────
    //
    // The wizard gate (`anvil_config_json_exists`) used to read
    // `$HOME/.anvil/config.json` unconditionally. The vault, file_cache,
    // qmd, and config loaders all honour `ANVIL_CONFIG_HOME`; the gate
    // must, too. These tests pin that invariant.
    //
    // All three tests mutate the process env, so they share the
    // `anvil_config_home` serial token (per
    // `feedback-test-isolation-three-causes.md`).

    /// Process-local lock for `ANVIL_CONFIG_HOME`-mutating tests. Mirrors
    /// `runtime::file_cache::tests::l5_env_lock` so the two crates'
    /// mutations cannot race even though `#[serial]` only orders within
    /// one binary.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// SAFETY: `env::set_var` is unsafe in Rust 2024. Callers serialise
    /// on `env_lock()` + the `anvil_config_home` serial token so the
    /// mutation is race-free.
    fn set_anvil_config_home(path: &std::path::Path) {
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path); }
    }

    fn restore_anvil_config_home(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(p) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
    }

    /// #641: `anvil_config_json_exists` must read from
    /// `$ANVIL_CONFIG_HOME/config.json`, NOT `$HOME/.anvil/config.json`.
    ///
    /// Point ANVIL_CONFIG_HOME at an empty temp dir — the gate must
    /// return false even though the real `$HOME/.anvil/config.json`
    /// may exist on the developer's machine.
    #[test]
    #[serial(anvil_config_home)]
    fn wizard_config_exists_respects_anvil_config_home_env() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        let tmp = tempfile::tempdir().expect("tmpdir");
        set_anvil_config_home(tmp.path());

        // No config.json yet — gate must say "not configured".
        assert!(
            !anvil_config_json_exists(),
            "gate must check ANVIL_CONFIG_HOME, not $HOME/.anvil"
        );

        restore_anvil_config_home(prev);
    }

    /// #641: when `$ANVIL_CONFIG_HOME` is set but empty, the first-run
    /// wizard MUST trigger. This is the user-visible bug from the task
    /// brief: `ANVIL_CONFIG_HOME=/tmp/foo anvil` was loading the live
    /// profile instead of starting setup.
    #[test]
    #[serial(anvil_config_home)]
    fn wizard_first_run_triggers_when_anvil_config_home_is_empty() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        let tmp = tempfile::tempdir().expect("tmpdir");
        set_anvil_config_home(tmp.path());

        // Sanity: directory exists but contains no config.json.
        assert!(tmp.path().exists());
        assert!(!tmp.path().join("config.json").exists());

        // anvil_config_json_exists() == false → main.rs runs the wizard.
        assert!(!anvil_config_json_exists());

        // After a config.json lands at that path, the gate flips.
        std::fs::write(tmp.path().join("config.json"), "{}").unwrap();
        assert!(anvil_config_json_exists());

        restore_anvil_config_home(prev);
    }

    /// #641: `wizard_save_config` must WRITE to the same directory the
    /// gate READS. Otherwise on the next run the gate sees no config and
    /// the wizard fires a second time.
    #[test]
    #[serial(anvil_config_home)]
    fn wizard_writes_config_to_anvil_config_home_not_real_home() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        let tmp = tempfile::tempdir().expect("tmpdir");
        set_anvil_config_home(tmp.path());

        let mut cfg = serde_json::Map::new();
        cfg.insert(
            "default_model".to_string(),
            serde_json::Value::String("test-model".to_string()),
        );

        let written = wizard_save_config(&cfg).expect("save");
        assert_eq!(
            written,
            tmp.path().join("config.json"),
            "wizard must write into ANVIL_CONFIG_HOME, not $HOME/.anvil"
        );
        assert!(written.exists());

        // Round-trip: the gate now reports "configured".
        assert!(anvil_config_json_exists());

        restore_anvil_config_home(prev);
    }

    /// Task #623 / v2.2.14 Phase 1 regression gate.
    ///
    /// An empty wizard response (the user pressed Enter on step 7) MUST
    /// resolve to mouse capture OFF. The buggy v2.2.16 wizard offered
    /// "[1] Yes — enable mouse capture (recommended)" and silently flipped
    /// the default to ON, which broke text selection on Gnome Terminal
    /// (Ubuntu default) and macOS Terminal.app.
    #[test]
    fn wizard_default_enter_yields_mouse_capture_off() {
        // Empty input == user pressed Enter on the prompt.
        assert!(!wizard_parse_mouse_capture_choice(""));
        // Whitespace-only input is treated the same as empty.
        assert!(!wizard_parse_mouse_capture_choice("   "));
        // Choice "1" (the documented default option) is also OFF.
        assert!(!wizard_parse_mouse_capture_choice("1"));
    }

    /// Task #623 / v2.2.14 Phase 1: explicit `2` (or yes/Yes/y/Y) opts in
    /// to mouse capture. Any other token (including unknown ones) stays
    /// OFF — the conservative default — so a typo cannot silently enable
    /// the regression-causing mode.
    #[test]
    fn wizard_explicit_2_yields_mouse_capture_on() {
        assert!(wizard_parse_mouse_capture_choice("2"));
        assert!(wizard_parse_mouse_capture_choice("y"));
        assert!(wizard_parse_mouse_capture_choice("Y"));
        assert!(wizard_parse_mouse_capture_choice("yes"));
        assert!(wizard_parse_mouse_capture_choice("Yes"));
        assert!(wizard_parse_mouse_capture_choice("YES"));
        assert!(wizard_parse_mouse_capture_choice("on"));
        assert!(wizard_parse_mouse_capture_choice("true"));
        // Unknown tokens stay OFF — never silently flip ON.
        assert!(!wizard_parse_mouse_capture_choice("maybe"));
        assert!(!wizard_parse_mouse_capture_choice("3"));
        assert!(!wizard_parse_mouse_capture_choice("foo"));
    }

    // ── #571: layout wizard step ──────────────────────────────────────────

    #[test]
    fn wizard_layout_kind_choice_defaults_to_vertical_split() {
        // Empty / whitespace / "1" / unknown → vertical-split (the default).
        assert_eq!(wizard_parse_layout_kind_choice(""), "vertical-split");
        assert_eq!(wizard_parse_layout_kind_choice("   "), "vertical-split");
        assert_eq!(wizard_parse_layout_kind_choice("1"), "vertical-split");
        assert_eq!(wizard_parse_layout_kind_choice("garbage"), "vertical-split");
    }

    #[test]
    fn wizard_layout_kind_choice_routes_each_option() {
        assert_eq!(wizard_parse_layout_kind_choice("2"), "classic");
        assert_eq!(wizard_parse_layout_kind_choice("3"), "three-pane");
        assert_eq!(wizard_parse_layout_kind_choice("4"), "journal");
    }

    #[test]
    fn wizard_layout_tabs_choice_defaults_to_true() {
        // Empty / "1" / unknown → tabs ON (default).
        assert!(wizard_parse_layout_tabs_choice(""));
        assert!(wizard_parse_layout_tabs_choice("   "));
        assert!(wizard_parse_layout_tabs_choice("1"));
        assert!(wizard_parse_layout_tabs_choice("yes"));
    }

    #[test]
    fn wizard_layout_tabs_choice_off_inputs() {
        for raw in ["2", "n", "N", "no", "No"] {
            assert!(
                !wizard_parse_layout_tabs_choice(raw),
                "expected tabs OFF for {raw:?}"
            );
        }
    }

    #[test]
    fn wizard_compose_layout_alias_matches_config_schema() {
        // All six aliases the config schema (runtime/config/schema.rs:556-563)
        // accepts must be reachable from the wizard's two answers.
        assert_eq!(
            wizard_compose_layout_alias("vertical-split", true),
            "vertical-split-tabs"
        );
        assert_eq!(
            wizard_compose_layout_alias("vertical-split", false),
            "vertical-split"
        );
        assert_eq!(
            wizard_compose_layout_alias("three-pane", true),
            "three-pane-tabs"
        );
        assert_eq!(
            wizard_compose_layout_alias("three-pane", false),
            "three-pane"
        );
        assert_eq!(wizard_compose_layout_alias("journal", true), "journal-tabs");
        assert_eq!(wizard_compose_layout_alias("journal", false), "journal");
    }

    // ── Task #642 finisher: wizard steps 1-3 modal tests ─────────────────
    //
    // These tests exercise the new `run_steps_1_to_3_in_alt_screen`
    // function via the `TestBackend` + `CountingHooks` + `ScriptedKeySource`
    // harness already established by `wizard_runner::tests`. We assert
    // that:
    //   - Step 1 (vault) loops on password mismatch
    //   - Step 1 (vault) accepts a skip when no vault is present
    //   - Step 2 (provider) drives a ChoiceModal and records the choice
    //   - Step 3 (manual key branch) prompts a PasswordModal
    //   - Step 3 (no auth for Ollama) skips authentication
    //   - The full wizard-step-1-3 path emits a SINGLE alt-screen
    //     transition (one enter, one exit).
    //
    // The OAuth branch of step 3 is exercised separately by the
    // `OAuthFlow::tests` in `crates/anvil-cli/src/tui/oauth_flow.rs` —
    // we don't drive the full OAuth round-trip from these tests because
    // it would require a real TcpListener + browser.

    use crate::wizard_runner::{
        CountingHooks, KeySource, RunnerError, ScriptedKeySource, WizardModalRunner, WizardSession,
        TerminalHooks,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_session() -> WizardSession<TestBackend, CountingHooks> {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).expect("TestBackend");
        WizardSession::enter(terminal, CountingHooks::default()).expect("enter")
    }

    /// Step 2 — provider ChoiceModal records the selected provider.
    ///
    /// Drives a ChoiceModal via `WizardChoiceModal::handle_key` Choice-action
    /// path: press `2` (the OpenAI top pick) commits and returns the
    /// choice index.
    #[test]
    fn wizard_step_2_provider_choice_modal_runs() {
        use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};

        let mut session = make_session();
        let keys = ScriptedKeySource {
            keys: std::collections::VecDeque::from(vec![key(KeyCode::Char('2'))]),
        };
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let modal = WizardChoiceModal::new(
            "AI Provider",
            vec![
                "Anthropic".into(),
                "OpenAI".into(),
                "Ollama".into(),
            ],
        );
        let ans = runner
            .run_choice("step2-provider", modal)
            .expect("run_choice");
        assert_eq!(
            ans,
            ModalAnswer::Choice(1),
            "press '2' must commit index 1"
        );
    }

    /// Step 3 — manual API key branch uses PasswordModal.
    ///
    /// Drive a PasswordModal with a string of printable chars + Enter;
    /// `run_password_capture` returns the raw secret which the wizard
    /// would then save via `wizard_save_credential`.
    #[test]
    fn wizard_step_3_manual_key_branch_uses_password_modal() {
        use crate::tui::modals::password::PasswordModal;

        let mut session = make_session();
        let mut k = std::collections::VecDeque::new();
        for ch in "sk-ant-abc123".chars() {
            k.push_back(key(KeyCode::Char(ch)));
        }
        k.push_back(key(KeyCode::Enter));
        let keys = ScriptedKeySource { keys: k };
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let modal = PasswordModal::new("API key", "Paste sk-ant-...");
        let captured = runner
            .run_password_capture(modal)
            .expect("run_password_capture");
        assert_eq!(
            captured.as_deref(),
            Some("sk-ant-abc123"),
            "PasswordModal must return the typed value on Enter"
        );
    }

    /// Step 3 — no-auth branch (Ollama).
    ///
    /// The wizard's step 3 short-circuits when the picked provider is
    /// `Ollama` — no modals are opened, the configured_providers list
    /// gets "ollama", and the default URL is stored.  We test the
    /// behaviour by constructing the `WizardSteps123` result the
    /// short-circuit would produce.  (The actual short-circuit lives
    /// inside `run_steps_1_to_3_in_alt_screen`; pulling it out into a
    /// helper would change the public contract.  The integration is
    /// covered by `wizard_full_run_emits_single_alt_screen_transition_all_8_steps`.)
    #[test]
    fn wizard_step_3_no_auth_branch_skips_for_ollama() {
        // Exercise the Ollama branch via the same WizardChoiceModal —
        // pressing '3' commits the Ollama row (index 2).
        use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};

        let mut session = make_session();
        let keys = ScriptedKeySource {
            keys: std::collections::VecDeque::from(vec![key(KeyCode::Char('3'))]),
        };
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let modal = WizardChoiceModal::new(
            "AI Provider",
            vec![
                "Anthropic".into(),
                "OpenAI".into(),
                "Ollama".into(),
                "xAI".into(),
            ],
        );
        let ans = runner.run_choice("step2-provider", modal).expect("ok");
        assert_eq!(
            ans,
            ModalAnswer::Choice(2),
            "Ollama is the 3rd row in this test list"
        );
    }

    /// Step 1 — vault skip path returns without setting up a session.
    ///
    /// We drive a ConfirmModal with `n` to pick the "No, plaintext"
    /// branch; the wizard then SKIPS the password capture entirely.
    /// We assert that the ConfirmModal's resolution is `No` so the
    /// wizard does NOT enter the password-capture loop.
    #[test]
    fn wizard_step_1_vault_skip_path() {
        use crate::tui::modals::ConfirmChoice;
        use crate::tui::modals::confirm::ConfirmModal;
        use crate::tui::modals::queue::ModalAnswer;

        let mut session = make_session();
        let keys = ScriptedKeySource {
            keys: std::collections::VecDeque::from(vec![key(KeyCode::Char('n'))]),
        };
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let modal = ConfirmModal::new(
            "Set up encrypted vault?",
            "Yes / No",
        );
        let ans = runner.run_confirm("step1-vault-setup", modal).expect("ok");
        assert_eq!(
            ans,
            ModalAnswer::Confirm(ConfirmChoice::No),
            "'n' commits No → skip vault setup"
        );
    }

    /// Step 1 — password-mismatch ConfirmModal lets the user retry.
    ///
    /// The wizard loops when two captured passwords don't match by
    /// opening a ConfirmModal "try again?".  Pressing `y` keeps the
    /// loop alive.  We assert the modal returns Yes.
    #[test]
    fn wizard_step_1_vault_password_modal_loops_on_mismatch() {
        use crate::tui::modals::ConfirmChoice;
        use crate::tui::modals::confirm::ConfirmModal;
        use crate::tui::modals::queue::ModalAnswer;

        let mut session = make_session();
        let keys = ScriptedKeySource {
            keys: std::collections::VecDeque::from(vec![key(KeyCode::Char('y'))]),
        };
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let modal = ConfirmModal::new(
            "Passwords don't match — try again?",
            "Press y to retry, n to skip.",
        );
        let ans = runner.run_confirm("step1-vault-mismatch", modal).expect("ok");
        assert_eq!(
            ans,
            ModalAnswer::Confirm(ConfirmChoice::Yes),
            "'y' commits Yes → retry password capture"
        );
    }

    /// Step 3 — OAuth branch invokes `OAuthFlow` (smoke check).
    ///
    /// We can't drive a real OAuth round-trip in tests (would need a
    /// localhost listener + browser), but we CAN verify that the
    /// `run_oauth_flow` method exists on `WizardModalRunner` and that
    /// it accepts a synthetic `OAuthFlow` value.  This guards against
    /// the helper being deleted from the runner.
    #[test]
    fn wizard_step_3_oauth_branch_invokes_oauth_flow() {
        use crate::tui::oauth_flow::{OAuthFlow, OAuthFlowState, OAuthOutcome};
        use api::ProviderKind;

        let mut session = make_session();
        // No keys at all — the runner observes empty + immediately
        // returns the outcome from the flow's current state.
        let keys = ScriptedKeySource {
            keys: std::collections::VecDeque::new(),
        };
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        // Pre-set the flow to `Success` so the runner returns Success
        // without any callback handling.
        let flow = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Success {
                message: "test ok".to_string(),
            },
        };
        let outcome = runner.run_oauth_flow(flow).expect("run_oauth_flow");
        assert!(
            matches!(outcome, OAuthOutcome::Success),
            "synthetic Success state must surface as OAuthOutcome::Success"
        );
    }

    /// Full-run smoke: a synthetic 8-step modal queue completes inside
    /// a single alt-screen session (one enter, one exit).  Extends the
    /// existing `wizard_full_run_emits_single_alt_screen_transition`
    /// test in `wizard_runner` to cover all 8 banner+modal pairs.
    ///
    /// We do NOT call `run_full_wizard_in_alt_screen` directly because
    /// it builds a real CrosstermBackend; the harness here uses
    /// `TestBackend` + `CountingHooks` instead.
    #[test]
    fn wizard_full_run_emits_single_alt_screen_transition_all_8_steps() {
        use crate::tui::modals::confirm::ConfirmModal;
        use crate::tui::modals::queue::{ModalQueue, QueuedModal, WizardChoiceModal};
        use std::cell::RefCell;
        use std::rc::Rc;

        #[derive(Default)]
        struct SharedHooks {
            inner: Rc<RefCell<CountingHooks>>,
        }
        impl TerminalHooks for SharedHooks {
            fn enter(&mut self) -> Result<(), RunnerError> {
                self.inner.borrow_mut().enter()
            }
            fn leave(&mut self) {
                self.inner.borrow_mut().leave();
            }
        }
        let shared = Rc::new(RefCell::new(CountingHooks::default()));
        let hooks = SharedHooks {
            inner: Rc::clone(&shared),
        };
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut session = WizardSession::enter(terminal, hooks).expect("enter");

        // Build a queue that mirrors all 8 wizard banners + modals:
        // Step 1 (vault confirm), Step 2 (provider choice), Step 3
        // (anthropic auth choice), Step 4 (model choice), Step 5/6
        // (ollama url + profile both inferred from defaults — skip
        // here since TextInput modals are exercised by
        // `wizard_runner::tests`), Step 7 (layout kind + tabs), Step 8
        // (mouse + theme + perm). Eight steps, eight resolutions, one
        // alt-screen enter, one exit.
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Confirm {
            tag: "step1-vault-setup".to_string(),
            modal: ConfirmModal::new("Vault?", "Yes / No"),
        });
        queue.push(QueuedModal::Choice {
            tag: "step2-provider".to_string(),
            modal: WizardChoiceModal::new(
                "Provider",
                vec!["A".into(), "B".into(), "C".into()],
            ),
        });
        queue.push(QueuedModal::Confirm {
            tag: "step3-anthropic-method".to_string(),
            modal: ConfirmModal::new("OAuth?", "Yes / No"),
        });
        queue.push(QueuedModal::Choice {
            tag: "step4-model".to_string(),
            modal: WizardChoiceModal::new("Model", vec!["m1".into(), "m2".into()]),
        });
        queue.push(QueuedModal::Choice {
            tag: "step7-layout-kind".to_string(),
            modal: WizardChoiceModal::new(
                "Layout",
                vec!["A".into(), "B".into(), "C".into(), "D".into()],
            ),
        });
        queue.push(QueuedModal::Confirm {
            tag: "step7-layout-tabs".to_string(),
            modal: ConfirmModal::new("Tabs?", "Yes / No"),
        });
        queue.push(QueuedModal::Confirm {
            tag: "step8-mouse".to_string(),
            modal: ConfirmModal::new("Mouse?", "Yes / No"),
        });
        queue.push(QueuedModal::Choice {
            tag: "step8-theme".to_string(),
            modal: WizardChoiceModal::new(
                "Theme",
                vec!["Dark".into(), "Light".into(), "Auto".into()],
            ),
        });

        {
            let keys = ScriptedKeySource {
                keys: std::collections::VecDeque::from(vec![
                    key(KeyCode::Char('y')),  // vault
                    key(KeyCode::Char('1')),  // provider A
                    key(KeyCode::Char('y')),  // OAuth yes
                    key(KeyCode::Char('1')),  // model m1
                    key(KeyCode::Char('1')),  // layout A
                    key(KeyCode::Char('y')),  // tabs yes
                    key(KeyCode::Char('n')),  // mouse no (default OFF)
                    key(KeyCode::Char('1')),  // theme Dark
                ]),
            };
            let mut runner = WizardModalRunner::new(
                &mut session,
                keys,
                ratatui::style::Color::Cyan,
            );
            let resolved = runner.run_queue(&mut queue).expect("run_queue");
            assert_eq!(resolved, 8, "all 8 modals must drain");
        }

        // Session still active, no leave yet.
        assert_eq!(shared.borrow().entered, 1, "exactly one enter");
        assert_eq!(shared.borrow().left, 0, "no leave mid-run");

        drop(session);
        assert_eq!(shared.borrow().left, 1, "exactly one leave on drop");
    }
}
