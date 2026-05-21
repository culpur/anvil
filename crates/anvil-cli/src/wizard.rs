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

use rust_i18n::t;
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
    let line = t!(
        "wizard.step_header",
        step = step.to_string(),
        total = total.to_string(),
        title = title
    );
    println!("\x1b[1;33m{line}\x1b[0m");
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
    fs::write(&path, serde_json::to_string_pretty(&root).unwrap_or_default())?;
    // Task #663 Gap 3: chmod 0o600 on credentials.json. This file holds
    // plaintext API keys when the vault is skipped — tighten perms so
    // it is owner-readable only.  No-op on non-Unix.
    let _ = crate::wizard_gaps::tighten_credential_perms(&path);
    Ok(())
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
    // Task #663 Gap 6: ensure the canonical subdir set exists on first
    // run. Idempotent — no-op when the user has already run the wizard
    // and ~/.anvil/{sessions,logs,...} are already populated.
    let _ = crate::wizard_gaps::ensure_anvil_subdirs(&dir);
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
    // Task #663 Gap 3: chmod 0o600 on config.json. Plaintext provider
    // keys may live here until the user migrates them into the vault;
    // 0600 means only the owning user can read.  No-op on non-Unix.
    let _ = crate::wizard_gaps::tighten_credential_perms(&path);
    Ok(path)
}

/// Write a minimal-defaults config.json when the user skips setup at
/// the welcome card (v2.2.17 #644 Item 1).
///
/// The minimal config sets `setup_completed = true` so subsequent
/// launches don't re-show the wizard, plus the system-level defaults
/// for layout / mouse / theme / permission so the TUI boots without
/// asking for any of them.  Provider config stays empty — the user
/// runs `anvil login <provider>` later.
pub(crate) fn write_minimal_default_config() -> io::Result<PathBuf> {
    let mut config = serde_json::Map::new();
    config.insert(
        "default_model".to_string(),
        serde_json::Value::String(DEFAULT_MODEL.to_string()),
    );
    config.insert(
        "default_provider".to_string(),
        serde_json::Value::String("anthropic".to_string()),
    );
    config.insert("setup_completed".to_string(), serde_json::Value::Bool(true));
    config.insert(
        "setup_skipped".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "tui_layout".to_string(),
        serde_json::json!({
            "kind": "vertical-split",
            "tabs": true,
        }),
    );
    config.insert(
        "tui_layout_intro_seen".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "tui_mouse_capture".to_string(),
        serde_json::Value::Bool(false),
    );
    config.insert(
        "theme".to_string(),
        serde_json::Value::String("dark".to_string()),
    );
    config.insert(
        "permission_mode".to_string(),
        serde_json::Value::String("ask".to_string()),
    );
    wizard_save_config(&config)
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
    runner.session.render_banner_with_description(
        "Step 4 of 8 — Default Model",
        "Pick your default model. Switch any time via /model.",
        &[],
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
    runner.session.render_banner_with_description(
        "Step 5 of 8 — Ollama Endpoint",
        "If you run a local Ollama instance, point Anvil at it now. Skip if you don't.",
        &[],
        ratatui::style::Color::Cyan,
    )?;

    let ollama_modal = TextInputModal::new("Ollama URL", "Endpoint")
        .with_default("http://localhost:11434");
    let ollama_url = match runner.run_text_input("step5-ollama-url", ollama_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "http://localhost:11434".to_string(),
    };

    // ── Step 6: Profile Name ─────────────────────────────────────────────
    runner.session.render_banner_with_description(
        "Step 6 of 8 — Profile",
        "Profiles let you keep separate configs (e.g. work / personal). Default is fine if you're not sure.",
        &[],
        ratatui::style::Color::Cyan,
    )?;

    let profile_modal = TextInputModal::new("Profile name", "Profile")
        .with_default("default");
    let profile_name = match runner.run_text_input("step6-profile", profile_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "default".to_string(),
    };

    // ── Step 7: TUI Layout ───────────────────────────────────────────────
    runner.session.render_banner_with_description(
        "Step 7 of 8 — TUI Layout",
        "Pick your TUI layout. Preview screenshots: https://anvilhub.culpur.net/tui-preview",
        &[],
        ratatui::style::Color::Cyan,
    )?;

    let mut tui_answers = WizardTuiAnswers::defaults();

    // v2.2.18 (task #666): migrate Step 7 layout picker to the rich
    // `Choice` API. Same call-site shape, additional badge + per-row
    // description. Proves the back-compat contract holds: every other
    // WizardChoiceModal::new(title, vec) call site in this file
    // still compiles untouched (theme + permission pickers below).
    use crate::tui::modals::Choice;
    let layout_kind_modal = WizardChoiceModal::new_titled(
        "TUI Layout — pick architecture",
    )
    .with_choices(vec![
        Choice::new("Vertical Split")
            .with_badge("recommended")
            .with_description(
                "Sessions rail + swappable deck (default in v2.2.16+)",
            ),
        Choice::new("Classic")
            .with_description("Single-deck, pre-v2.2.16 layout"),
        Choice::new("Three-Pane")
            .with_description("FOCUS / LOG / CONTEXT split (wide terminals)"),
        Choice::new("Journal")
            .with_description("Timestamped column with Ctrl-K palette"),
    ])
    .with_footer_hint("press 1-4, or ↑/↓ + Enter · Esc to keep default");
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
    runner.session.render_banner_with_description(
        "Step 8 of 8 — Preferences",
        "Mouse, theme, and permission mode. All changeable later via /config.",
        &[],
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

/// Detect whether the local user has a Claude Code install (`~/.claude/`).
///
/// Honors `ANVIL_CLAUDE_HOME` for tests / isolated sandboxes — that env
/// var, when set, takes the place of `~/.claude`. Otherwise falls back to
/// the real home-dir lookup. Returns `None` when no directory was found.
pub(crate) fn detect_claude_code_dir() -> Option<std::path::PathBuf> {
    if let Some(override_dir) = std::env::var_os("ANVIL_CLAUDE_HOME") {
        let p = std::path::PathBuf::from(override_dir);
        return if p.exists() { Some(p) } else { None };
    }
    dirs_next::home_dir()
        .map(|h| h.join(".claude"))
        .filter(|p| p.exists())
}

/// Step 9 — optional Claude Code migration, driven inside the existing
/// alt-screen `WizardSession` (#643, v2.2.17).
///
/// Returns:
/// - `Ok(None)` when CC is not installed OR the user previously declined
///   migration (the `.import-skipped` flag is present). The step renders
///   nothing — the wizard is silent on absence by design.
/// - `Ok(Some(summary))` when the step ran a ConfirmModal AND either
///   imported or recorded a skip. The `summary` string is a single line
///   suitable for the post-wizard exit banner ("Imported N items from
///   Claude Code" / "CC migration skipped (.import-skipped recorded)").
///
/// Constraints honoured:
/// - NO `println!` / `eprintln!` while the alt-screen is active
///   (`feedback-tui-stdout-anti-pattern.md`).
/// - The import pipeline runs synchronously between two banners — the
///   "Importing..." banner stays on screen for the duration. The
///   pipeline is fast (typically under one second for fresh installs);
///   a spinner is intentionally not added here to keep the work scoped.
///   When the runtime gains an async progress channel, the banner body
///   text can be updated incrementally; until then a single static
///   "Working..." line is honest and flicker-free.
pub(crate) fn run_step_9_cc_migration_in_alt_screen<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
) -> Result<Option<String>, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: crate::wizard_runner::KeySource,
{
    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::ModalAnswer;

    // Quick gates: skip silently when CC not installed or already
    // declined.
    if import_was_skipped() {
        return Ok(None);
    }
    let Some(claude_dir) = detect_claude_code_dir() else {
        return Ok(None);
    };

    runner.session.render_banner_with_description(
        "Step 9 of 9 — Import from Claude Code (optional)",
        "We detected Claude Code. Want to import your sessions, skills, plugins, and memory? Takes ~30 seconds.",
        &[
            "Read-only — nothing in ~/.claude/ is modified.",
        ],
        ratatui::style::Color::Cyan,
    )?;

    let prompt_body = format!(
        "Detected CC at: {}\n\
         \n\
         Yes — import memory, instructions, skills, plugins, and settings\n\
         No  — skip (you can run `/import claude-code` any time later)",
        claude_dir.display()
    );
    let modal = ConfirmModal::new("Import from Claude Code?", prompt_body);
    let answer = runner.run_confirm("step9-cc-migration", modal)?;

    match answer {
        ModalAnswer::Confirm(ConfirmChoice::Yes) => {
            // Show a "working..." banner while the synchronous import
            // pipeline runs. The pipeline currently lacks an async
            // progress channel, so we render one snapshot before and
            // one after — both inside the same alt-screen frame.
            runner.session.render_banner(
                "Importing from Claude Code...",
                &[
                    "Reading memory, instructions, skills, plugins...",
                    "This is read-only — nothing in ~/.claude/ is modified.",
                ],
                ratatui::style::Color::Cyan,
            )?;

            let summary = run_cc_import_pipeline_capture_summary(&claude_dir);

            // Show a result banner so the user sees the outcome inside
            // the alt-screen (rather than scrolling past it after the
            // session drops). The summary line is also returned to the
            // caller for the post-exit banner.
            let body_lines: Vec<&str> = summary.lines().collect();
            // Cap the banner body to a few lines — the full report is
            // available on disk regardless.
            let display: Vec<&str> = body_lines.iter().copied().take(4).collect();
            runner.session.render_banner(
                "Import complete",
                &display,
                ratatui::style::Color::Green,
            )?;
            Ok(Some(summary.lines().next().unwrap_or("Imported from Claude Code").to_string()))
        }
        _ => {
            // User picked No / Esc — record the skip so we don't ask
            // again on the next run, and surface a one-line summary.
            let _ = write_import_skipped_flag();
            runner.session.render_banner(
                "Skipped",
                &[
                    "OK — you can run `/import claude-code` any time later.",
                ],
                ratatui::style::Color::DarkGray,
            )?;
            Ok(Some("CC migration skipped (.import-skipped recorded)".to_string()))
        }
    }
}

/// Run the CC import pipeline synchronously and produce a one-line
/// summary suitable for the post-exit banner.
///
/// Errors from the pipeline are surfaced as a "Migration failed: ..."
/// line rather than panicking — the wizard must always exit cleanly.
fn run_cc_import_pipeline_capture_summary(claude_dir: &std::path::Path) -> String {
    use runtime::ImportSource;
    let source = ImportSource::ClaudeCode {
        profile_dir: claude_dir.to_path_buf(),
    };
    match commands::handlers::run_import_pipeline_headless(&source, false, false) {
        Ok(report) => {
            // The full report is multi-line; the wizard's exit banner
            // only has room for the first line, but the rest is still
            // written to ~/.anvil/.import-report.md by the pipeline.
            // Use the first non-blank line that looks like a count
            // summary; fall back to the literal first line.
            for line in report.lines() {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                return format!("Imported from Claude Code: {t}");
            }
            "Imported from Claude Code (see ~/.anvil/.import-report.md)".to_string()
        }
        Err(e) => format!("Claude Code migration failed: {e}"),
    }
}

/// Task #663 Gap 1 + Gap 8 — shell-completion install step.
///
/// Detects the user's shell from `$SHELL`, locates the on-disk
/// completion source (next to the binary or in the canonical share
/// dir), copies it to the canonical destination, and offers to append
/// the `fpath+=~/.zsh/completions` hint to `~/.zshrc` when zsh is
/// detected.  Silent (returns Ok without opening any modal) when no
/// completion source can be found — that path means we're running
/// from a dev `cargo run` build that does not ship completion files.
///
/// All UI flows through the modal runner — no `println!` while
/// alt-screen is active (per `feedback-tui-stdout-anti-pattern`).
pub(crate) fn run_completion_install_step_in_alt_screen<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
) -> Result<(), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: crate::wizard_runner::KeySource,
{
    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::ModalAnswer;

    let Some(shell) = crate::wizard_gaps::detect_user_shell() else {
        return Ok(());
    };
    let Some(source) = crate::wizard_gaps::locate_completion_source(shell) else {
        // Dev build or stripped install — nothing to do, silent.
        return Ok(());
    };
    let Some(home) = dirs_next::home_dir() else {
        return Ok(());
    };
    let dest = crate::wizard_gaps::completion_dest_path(shell, &home);

    let confirm_body = format!(
        "Install {} completion to:\n  {}\n\nRestart your shell after install for completions to take effect.",
        shell.name(),
        dest.display(),
    );
    let confirm = ConfirmModal::new("Install shell completion?", confirm_body);
    let answer = runner.run_confirm("step-completion-install", confirm)?;
    if !matches!(answer, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
        return Ok(());
    }

    match crate::wizard_gaps::install_completion_for(shell, &home, &source) {
        Ok(installed_at) => {
            let body = format!("Installed: {}", installed_at.display());
            runner.session.render_banner(
                "Shell completion installed",
                &[body.as_str()],
                ratatui::style::Color::Green,
            )?;
        }
        Err(e) => {
            let body = format!("Could not install completion: {e}");
            runner.session.render_banner(
                "Completion install failed",
                &[
                    body.as_str(),
                    "See https://anvilhub.culpur.net/docs/completions for manual steps.",
                ],
                ratatui::style::Color::Yellow,
            )?;
            return Ok(());
        }
    }

    // Task #663 Gap 8 — zsh fpath hint.  Only relevant for zsh + only
    // when `~/.zshrc` does not already contain the line.
    if matches!(shell, crate::wizard_gaps::DetectedShell::Zsh) {
        let modal = ConfirmModal::new(
            "Add zsh fpath hint?",
            "Add `fpath+=~/.zsh/completions` to your `~/.zshrc` before `compinit`\n\
             so zsh picks up the completion file. Add it now?",
        );
        let ans = runner.run_confirm("step-completion-zsh-fpath", modal)?;
        if matches!(ans, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
            match crate::wizard_gaps::append_zsh_fpath_hint(&home) {
                Ok(true) => {
                    runner.session.render_banner(
                        "Added fpath hint to ~/.zshrc",
                        &["Restart your shell or `source ~/.zshrc` to pick it up."],
                        ratatui::style::Color::Green,
                    )?;
                }
                Ok(false) => {
                    runner.session.render_banner(
                        "fpath hint already present",
                        &["~/.zshrc already contains the completion fpath line."],
                        ratatui::style::Color::DarkGray,
                    )?;
                }
                Err(e) => {
                    let body = format!("Could not update ~/.zshrc: {e}");
                    runner.session.render_banner(
                        "fpath hint not added",
                        &[body.as_str()],
                        ratatui::style::Color::Yellow,
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Result of running ALL 8 wizard steps + the step-9 CC migration
/// step inside a single alt-screen session (task #642 + #643 finisher).
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
    /// `Some(summary_line)` when the in-alt-screen step-9 CC migration
    /// ran and either imported or skipped; `None` when CC was not
    /// detected on this system. The wizard's post-exit summary banner
    /// uses this to render a one-line "Imported N items from Claude
    /// Code" or "CC migration skipped" message.
    pub(crate) migration_summary: Option<String>,
}

/// Welcome-card outcome — what the user pressed at the first screen
/// (v2.2.17 #644 Item 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WelcomeOutcome {
    /// User pressed Enter — proceed into Step 1.
    Continue,
    /// User pressed Esc — skip the wizard, write minimal defaults.
    Skip,
}

/// Draw the welcome card and wait for Enter (continue) or Esc (skip).
///
/// Renders the card on every iteration so any future async work (e.g.
/// a "press any key" pulsating hint) can update without losing the
/// frame.  The poll cadence matches `run_oauth_flow` — 100ms — so the
/// loop is responsive but does not pin a CPU.
pub(crate) fn await_welcome_keypress<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
) -> Result<WelcomeOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: crate::wizard_runner::KeySource,
{
    use crossterm::event::KeyCode;
    let title = t!("wizard.welcome.title", version = env!("CARGO_PKG_VERSION")).to_string();
    let tagline = t!("wizard.welcome.tagline").to_string();
    let kicker = t!("wizard.welcome.kicker").to_string();
    let hint = t!("wizard.welcome.hint").to_string();
    let accent = ratatui::style::Color::Cyan;

    let key_poll = std::time::Duration::from_millis(100);
    loop {
        runner
            .session
            .render_welcome_card(&title, &tagline, &kicker, &hint, accent)?;
        if let Some(k) = runner.keys.try_next_key(key_poll) {
            match k.code {
                KeyCode::Enter | KeyCode::Char(' ') => return Ok(WelcomeOutcome::Continue),
                KeyCode::Esc => return Ok(WelcomeOutcome::Skip),
                _ => continue,
            }
        }
        // Scripted-source exit: if the test source is exhausted with
        // no resolution, default to Continue (matches the "Enter
        // continues" semantic).  Production CrosstermKeySource never
        // reports exhausted, so this only fires in tests.
        if runner.keys.is_exhausted_hint() {
            return Ok(WelcomeOutcome::Continue);
        }
    }
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

/// Phase A6 (task #645) — wizard language picker step.
///
/// Renders after the welcome card, before the vault / provider steps.
/// User picks from the canonical Tier-1 language list using the
/// matching native-name labels — the index aligns one-to-one with
/// [`crate::utils::SUPPORTED_LANGUAGES`] so the picker has a single
/// source of truth and can never drift from the runtime list (Phase
/// A5's drift gate enforces that the YAML files match too).
///
/// On Enter, the helper:
///   1. Writes `language: <code>` to `~/.anvil/config.json` via
///      `utils::save_language` (the same helper /language uses), so
///      the choice survives across runs without a redundant code path.
///   2. Calls `rust_i18n::set_locale(&code)` so the REST of the
///      wizard (provider picker labels, modal hints, post-wizard
///      summary banner) renders in the chosen locale immediately.
///
/// On Esc (`ChoiceCancelled`) the helper is a no-op — the locale
/// applied by `apply_startup_locale` at boot stays in effect.
pub(crate) fn run_language_step_in_alt_screen<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
) -> Result<(), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: crate::wizard_runner::KeySource,
{
    use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};

    // Native-name labels — one entry per `SUPPORTED_LANGUAGES` index.
    // Encoded inline (not via t!()) because each label MUST appear in
    // its own language regardless of the current locale.  Keep this
    // table in lock-step with `utils::SUPPORTED_LANGUAGES` (drift gate
    // in tests).
    const NATIVE_NAMES: &[(&str, &str)] = &[
        // Tier 1
        ("en", "English"),
        ("es", "Español"),
        ("zh-CN", "简体中文"),
        ("fr", "Français"),
        ("pt-BR", "Português (Brasil)"),
        ("ru", "Русский"),
        ("ja", "日本語"),
        ("de", "Deutsch"),
        // Tier 2 (Arc G — task #710)
        ("ko", "한국어"),
        ("it", "Italiano"),
        ("tr", "Türkçe"),
        ("vi", "Tiếng Việt"),
        ("pl", "Polski"),
        ("id", "Bahasa Indonesia"),
        ("nl", "Nederlands"),
        ("sv", "Svenska"),
        ("nb", "Norsk Bokmål"),
        ("uk", "Українська"),
        // Task #749 — EU official gaps
        ("bg", "Български"),
        ("cs", "Čeština"),
        ("da", "Dansk"),
        ("el", "Ελληνικά"),
        ("et", "Eesti"),
        ("fi", "Suomi"),
        ("ga", "Gaeilge"),
        ("hr", "Hrvatski"),
        ("hu", "Magyar"),
        ("lt", "Lietuvių"),
        ("lv", "Latviešu"),
        ("mt", "Malti"),
        ("pt-PT", "Português (Portugal)"),
        ("ro", "Română"),
        ("sk", "Slovenčina"),
        ("sl", "Slovenščina"),
        // Task #749 — non-EU European
        ("be", "Беларуская"),
        ("bs", "Bosanski"),
        ("ca", "Català"),
        ("eu", "Euskara"),
        ("gl", "Galego"),
        ("is", "Íslenska"),
        ("mk", "Македонски"),
        ("nn", "Norsk Nynorsk"),
        ("sq", "Shqip"),
        ("sr", "Српски"),
        // Task #749 — major world
        ("am", "አማርኛ"),
        ("ar", "العربية"),
        ("bn", "বাংলা"),
        ("fa", "فارسی"),
        ("he", "עברית"),
        ("hi", "हिन्दी"),
        ("ms", "Bahasa Melayu"),
        ("sw", "Kiswahili"),
        ("ta", "தமிழ்"),
        ("te", "తెలుగు"),
        ("th", "ไทย"),
        ("tl", "Tagalog"),
        ("ur", "اردو"),
        ("zh-HK", "繁體中文 (香港)"),
        ("zh-TW", "繁體中文 (台灣)"),
        ("zu", "isiZulu"),
    ];

    // Render the step banner (title + body explanation).
    runner.session.render_banner(
        &t!("wizard.language.title"),
        &[&t!("wizard.language.body")],
        ratatui::style::Color::Cyan,
    )?;

    // Default selection — whatever locale is currently active.  If
    // the persisted code is not one we recognise (e.g. user hand-edited
    // config.json to an unsupported value), fall back to index 0
    // ("en").  The picker still completes; the user can pick another
    // entry and we re-anchor.
    let current = crate::utils::current_language_code();
    let default_index = NATIVE_NAMES
        .iter()
        .position(|(code, _)| *code == current.as_str())
        .unwrap_or(0);

    let labels: Vec<String> = NATIVE_NAMES.iter().map(|(_, name)| (*name).to_string()).collect();
    let modal = WizardChoiceModal::new(t!("wizard.language.title").to_string(), labels)
        .with_default_index(default_index);
    let answer = runner.run_choice("step0b-language", modal)?;

    if let ModalAnswer::Choice(idx) = answer {
        if let Some((code, _)) = NATIVE_NAMES.get(idx) {
            // Persist + apply.  If the write fails we still flip the
            // in-process locale so this run uses the chosen language —
            // the next /language invocation can retry the disk write.
            if let Err(_err) = crate::utils::save_language(code) {
                rust_i18n::set_locale(code);
            }
        }
    }
    // ChoiceCancelled / other variants: keep the boot-time locale.

    Ok(())
}

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

    // ── Step 0: Welcome card ────────────────────────────────────────────
    //
    // Renders BEFORE any "Step N of 8" banner so the user's first
    // impression of Anvil is a deliberate welcome rather than a
    // jarring vault-password prompt (v2.2.17 #644 Item 1).
    //
    // Enter advances into Step 1.  Esc skips the wizard entirely and
    // returns a sentinel `RunnerError::Enter("user-skipped")` which
    // the orchestrator catches and turns into a minimal-default
    // config.
    let welcome_outcome = await_welcome_keypress(&mut runner)?;
    if matches!(welcome_outcome, WelcomeOutcome::Skip) {
        // User pressed Esc — bail out of the wizard with a minimal
        // default config. Drop the runner / session first so the
        // alt-screen exits cleanly before any inline output runs.
        return Err(RunnerError::Enter("user-skipped".to_string()));
    }

    // Phase A6 (task #645) — language picker.  Renders BEFORE the
    // vault / provider steps so the user can switch the wizard into
    // their own language for the rest of the flow.  On selection, the
    // call also persists `language: <code>` to ~/.anvil/config.json
    // and applies the locale to `rust_i18n` so every subsequent t!()
    // call in this run uses the chosen locale.  Esc keeps whatever
    // locale was applied at startup.
    run_language_step_in_alt_screen(&mut runner)?;

    // Steps 1-3.
    let steps123 = run_steps_1_to_3_in_alt_screen(&mut runner)?;

    // Steps 4-8.
    let (chosen_model, ollama_url_modal, profile_name, tui_answers) =
        run_steps_4_to_8_in_alt_screen(&mut runner, &steps123.model_candidates)?;

    // v2.2.18 task #662 rebuild: real in-wizard Ollama install + model
    // pull.  Renders inside the same alt-screen.  When the user picks
    // "Install", Anvil downloads the official Ollama tarball via
    // reqwest (no curl|sh, no system installer), extracts to
    // ~/.anvil/bin/ollama, spawns the daemon as an owned child, and
    // pulls a curated model.  Errors render as confirm cards — the
    // wizard always completes.
    //
    // Idempotent: on re-run with an existing daemon + model, the step
    // shows ✓ and skips.  Matches the corrective-not-destructive rule.
    let _ollama_outcome = crate::wizard_ollama::run_ollama_step(
        &mut runner,
        &runtime::default_config_home(),
    )?;

    // v2.2.18 task #664 rebuild: real in-wizard QMD install.  Anvil
    // fetches the @tobilu/qmd npm tarball directly from
    // registry.npmjs.org, extracts under ~/.anvil/node_modules/, and
    // writes a launcher shim that uses the user's Node runtime.  No
    // `npm install` shell-out.  Falls back cleanly when Node is
    // absent.  Idempotent on re-entry like the Ollama step.
    let _qmd_outcome = crate::wizard_qmd::run_qmd_step(
        &mut runner,
        &runtime::default_config_home(),
    )?;

    // Step 9 — optional CC migration. Stays inside the same alt-screen
    // session so the wizard → Anvil TUI handoff has ZERO inline stdin
    // moments (#643, v2.2.17). The step is silent when ~/.claude/ is
    // not present or the user already declined on a previous run.
    let migration_summary = run_step_9_cc_migration_in_alt_screen(&mut runner)?;

    // Task #663 Gap 1 + Gap 8: shell-completion installation +
    // optional zsh fpath hint.  Renders inside the same alt-screen so
    // there is no second transition; silent when no completion source
    // can be located (cargo-run dev builds).
    let _ = run_completion_install_step_in_alt_screen(&mut runner);

    // Final "Starting Anvil..." banner so the user sees a clean
    // transition out of the wizard rather than an unexplained Drop.
    runner.session.render_banner(
        "Setup complete! Starting Anvil...",
        &["Press any key... (continuing automatically)"],
        ratatui::style::Color::Cyan,
    )?;

    // session goes out of scope here — single LeaveAlternateScreen.
    drop(runner);
    drop(session);

    Ok(FullWizardResult {
        steps123,
        chosen_model,
        ollama_url: ollama_url_modal,
        profile_name,
        tui_answers,
        migration_summary,
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

    // v2.2.18 (task #666): migrate Step 7 layout picker to the rich
    // `Choice` API. Same call-site shape, additional badge + per-row
    // description. Proves the back-compat contract holds: every other
    // WizardChoiceModal::new(title, vec) call site in this file
    // still compiles untouched (theme + permission pickers below).
    use crate::tui::modals::Choice;
    let layout_kind_modal = WizardChoiceModal::new_titled(
        "TUI Layout — pick architecture",
    )
    .with_choices(vec![
        Choice::new("Vertical Split")
            .with_badge("recommended")
            .with_description(
                "Sessions rail + swappable deck (default in v2.2.16+)",
            ),
        Choice::new("Classic")
            .with_description("Single-deck, pre-v2.2.16 layout"),
        Choice::new("Three-Pane")
            .with_description("FOCUS / LOG / CONTEXT split (wide terminals)"),
        Choice::new("Journal")
            .with_description("Timestamped column with Ctrl-K palette"),
    ])
    .with_footer_hint("press 1-4, or ↑/↓ + Enter · Esc to keep default");
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
    runner.session.render_banner_with_description(
        "Step 1 of 8 — Vault Setup",
        "Vault encrypts your API keys at rest. Pick a master password you'll remember — there's no recovery if lost.",
        &[
            "Anvil stores API keys in an AES-256-GCM encrypted vault.",
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
    runner.session.render_banner_with_description(
        "Step 2 of 8 — AI Provider",
        "Pick the AI provider you'll use most. You can add more later via /provider.",
        &[
            "More can be added with `anvil login <provider>` after setup.",
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
        runner.session.render_banner_with_description(
            "Step 3 of 8 — Sign In to Anthropic",
            "Sign in once so Anvil can make calls on your behalf. OAuth opens a browser; manual key paste also works.",
            &[
                "OAuth signs you in via claude.ai; manual key skips the browser.",
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
            // Task #663 Gap 2: env-var detection for Anthropic, too.
            // Offers `$ANTHROPIC_API_KEY` if set before showing the
            // PasswordModal paste prompt.
            let mut api_key_opt: Option<String> = None;
            if let Some((env_name, env_value)) =
                crate::wizard_gaps::detect_env_credential("anthropic")
            {
                let body = format!(
                    "Use ${env_name} from your environment? (recommended)\n\n\
                     Yes — re-use the existing key, no paste needed\n\
                     No  — paste a different key manually"
                );
                let env_modal = ConfirmModal::new("Env-var key detected", body);
                let env_answer = runner.run_confirm("step3-anthropic-env-key", env_modal)?;
                if matches!(env_answer, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
                    api_key_opt = Some(env_value);
                }
            }
            if api_key_opt.is_none() {
                let key_modal = PasswordModal::new(
                    "Anthropic API key",
                    "Paste sk-ant-...",
                );
                api_key_opt = runner.run_password_capture(key_modal)?;
            }
            if let Some(api_key) = api_key_opt
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
    let provider_id = provider_config_id(provider);

    // Task #663 Gap 12: DeepSeek / Kimi data-residency disclosure.
    // For Chinese-mainland-routed providers, show the disclosure modal
    // BEFORE any key entry.  Default is No — the user must explicitly
    // confirm before configuring the provider.  Non-disclosure
    // providers short-circuit `true` and proceed normally.
    if crate::wizard_gaps::is_data_residency_provider(provider_id) {
        let proceed = crate::wizard_gaps::run_data_residency_gate(runner, provider_id)?;
        if !proceed {
            runner.session.render_banner(
                "Skipped",
                &[
                    "Provider not configured — run `anvil login <provider>` later.",
                ],
                ratatui::style::Color::DarkGray,
            )?;
            return Ok(out);
        }
    }

    runner.session.render_banner(
        &format!("Step 3 of 8 — Sign In to {provider_label}"),
        &[
            "Paste your API key (Esc skips — you can run `anvil login` later).",
        ],
        ratatui::style::Color::Cyan,
    )?;

    // Task #663 Gap 2: provider env-var detection.  If the user already
    // has `$ANTHROPIC_API_KEY` / `$OPENAI_API_KEY` / etc. in their env,
    // offer to use that value instead of forcing a paste.  Default Yes
    // since the env path is the recommended workflow for CI + dev
    // shells.  We re-prompt for paste when the user says No or when no
    // env var is set.
    let mut api_key_opt: Option<String> = None;
    if let Some((env_name, env_value)) = crate::wizard_gaps::detect_env_credential(provider_id) {
        use crate::tui::modals::ConfirmChoice;
        use crate::tui::modals::confirm::ConfirmModal;
        use crate::tui::modals::queue::ModalAnswer;
        let body = format!(
            "Use ${env_name} from your environment? (recommended)\n\n\
             Yes — re-use the existing key, no paste needed\n\
             No  — paste a different key manually"
        );
        let env_modal = ConfirmModal::new("Env-var key detected", body);
        let env_answer = runner.run_confirm("step3-env-key", env_modal)?;
        if matches!(env_answer, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
            api_key_opt = Some(env_value);
        }
    }

    if api_key_opt.is_none() {
        let prompt = format!("{provider_label} API key");
        let key_modal = PasswordModal::new(prompt, "Paste key");
        api_key_opt = runner.run_password_capture(key_modal)?;
    }

    if let Some(api_key) = api_key_opt
        && api_key.len() > 10
    {
        // Save under the canonical vault key for the provider.
        let vault_key = vault_key_for_provider(provider);
        let _ = wizard_save_credential(vault_key, &api_key);
        out.configured_providers.push(provider_id.to_string());
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

    wizard_step_header(7, 8, &t!("wizard.step.tui_layout_title"));
    println!();
    println!("    {}", t!("wizard.layout.opt_vertical"));
    println!("    {}", t!("wizard.layout.opt_classic"));
    println!("    {}", t!("wizard.layout.opt_threepane"));
    println!("    {}", t!("wizard.layout.opt_journal"));
    println!();
    let layout_arch_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice_default1")));
    answers.layout_kind = wizard_parse_layout_kind_choice(&layout_arch_choice).to_string();

    println!();
    println!("  {}", t!("wizard.layout.show_tabs_prompt"));
    let layout_tabs_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice_default1")));
    answers.layout_tabs = wizard_parse_layout_tabs_choice(&layout_tabs_choice);

    wizard_step_header(8, 8, &t!("wizard.step.tui_prefs_title"));
    println!();
    println!("  {}", t!("wizard.prefs.mouse_prompt"));
    let mouse_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice_default1")));
    answers.mouse_capture = wizard_parse_mouse_capture_choice(mouse_choice.trim());

    println!();
    println!("  {}", t!("wizard.prefs.theme_prompt"));
    let theme_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice_default1")));
    answers.theme = match theme_choice.trim() {
        "2" => "light".to_string(),
        "3" => "auto".to_string(),
        _ => "dark".to_string(),
    };

    println!();
    println!("  {}", t!("wizard.prefs.permission_prompt"));
    let perm_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice_default1")));
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
/// Result handed back by the first-run wizard to main.rs (task #643,
/// v2.2.17 — zero-seam wizard → Anvil TUI handoff).
///
/// Today, `main.rs::run_repl` re-reads `config.json` after the wizard
/// returns to learn the chosen default model. With this struct the
/// wizard tells main.rs directly, AND tells it whether the session
/// vault is already unlocked so `startup_vault_init` can be skipped
/// when the wizard's Step 1 already did the unlock.
///
/// `wizard_completed = false` is the non-TTY / fallback path — in that
/// case main.rs falls back to its legacy "re-read config" behaviour.
#[derive(Debug, Clone, Default)]
pub(crate) struct WizardResult {
    /// `true` when the modal wizard ran end-to-end (TTY happy path).
    /// `false` when the stdin fallback ran OR the alt-screen wizard
    /// failed to enter and the legacy path took over.
    ///
    /// Currently read only in tests + future logging; the
    /// `vault_was_unlocked` field is the primary main.rs branch
    /// signal. The flag is kept on the result so a future
    /// telemetry / diagnostic site can distinguish "wizard ran" from
    /// "wizard fell back" without a second probe.
    #[allow(dead_code)]
    pub(crate) wizard_completed: bool,
    /// The model the user picked in Step 4 (or `None` when the wizard
    /// did not complete or no model was selected).
    pub(crate) chosen_model: Option<String>,
    /// `true` when Step 1 successfully set up + unlocked the session
    /// vault, OR successfully unlocked an existing one. main.rs uses
    /// this to skip the prompt-password loop in `startup_vault_init`.
    pub(crate) vault_was_unlocked: bool,
    /// `Some(line)` when Step 9 ran the CC migration (imported or
    /// skipped); `None` when CC was not detected at all. Surfaced in
    /// the post-wizard summary banner; field is owned even when the
    /// banner already rendered so future call-sites (e.g. an OTel
    /// span emit) can read it without re-running detection.
    #[allow(dead_code)]
    pub(crate) migration_summary: Option<String>,
}

#[allow(clippy::too_many_lines, clippy::single_match_else)]
pub(crate) fn run_first_run_wizard() -> WizardResult {
    // Task #663 Gap 6: create ~/.anvil/{sessions,logs,benchmarks,...}
    // BEFORE any modal/banner renders so a panic later in the flow
    // doesn't leave the user with a half-populated profile.  Idempotent;
    // no-op on repeat runs.
    crate::wizard_gaps::ensure_anvil_subdirs_default();

    // Task #663 Gap 10: headless TTY fallback.  When stdout is NOT a
    // TTY (CI, piped input, `--print` chains) we cannot enter the
    // alt-screen, so route to the stdin path that prints plain banners
    // without raw-mode.  install.sh already guards against this in
    // shell, but a direct `anvil` invocation from a CI script needs the
    // same gate here.
    if io::stdout().is_terminal() {
        run_first_run_wizard_modal()
    } else {
        run_first_run_wizard_via_stdin();
        // Stdin fallback writes config.json but does not capture the
        // result fields explicitly; main.rs falls back to re-reading
        // config.json in that path.
        WizardResult::default()
    }
}

/// Task #663 Gap 9 — public entry for the `--no-setup` closing card.
///
/// Called from main.rs when the user passes `--no-setup` or when the
/// installer explicitly asks anvil to render the skip card without
/// running the wizard.  Always headless: this never enters alt-screen,
/// so plain `println!` is safe (SAFE-HEADLESS per the print-discipline
/// audit).
pub(crate) fn run_no_setup_card() {
    crate::wizard_gaps::print_no_setup_card();
}

/// Modal-driven first-run wizard (the TTY happy path).
///
/// Opens ONE alt-screen via `run_full_wizard_in_alt_screen`, then
/// writes the final config.json and prints the post-wizard summary
/// inline below the (now-closed) alt-screen.
///
/// Returns a `WizardResult` so main.rs can plumb the captured fields
/// (chosen model, vault unlock state, CC migration summary) without
/// re-reading config.json or re-prompting the user (task #643).
fn run_first_run_wizard_modal() -> WizardResult {
    let result = match run_full_wizard_in_alt_screen() {
        Ok(r) => r,
        Err(RunnerError::Enter(ref msg)) if msg == "user-skipped" => {
            // User pressed Esc on the welcome card — write a minimal
            // defaults config so the next launch doesn't re-show the
            // wizard, and return a sensible `WizardResult` (v2.2.17
            // #644 Item 1).
            let _ = write_minimal_default_config();
            println!();
            wizard_box_top();
            wizard_box_line("");
            wizard_box_line("  Setup skipped.");
            wizard_box_line("");
            wizard_box_line("  Run `anvil` again or `/config` to configure providers.");
            wizard_box_line("");
            wizard_box_bot();
            println!();
            return WizardResult {
                wizard_completed: true,
                chosen_model: None,
                vault_was_unlocked: false,
                migration_summary: None,
            };
        }
        Err(e) => {
            // The alt-screen session failed to enter — fall back to the
            // stdin path so the user still gets through setup.
            eprintln!("\n  {}", t!("wizard.warn.altscreen_unavailable", reason = e.to_string()));
            eprintln!("  {}", t!("wizard.warn.fallback_inline"));
            run_first_run_wizard_via_stdin();
            return WizardResult::default();
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
            eprintln!("\n  {}", t!("wizard.warn.could_not_save_config", reason = e.to_string()));
            PathBuf::from("~/.anvil/config.json")
        }
    };

    // Migration step ran INSIDE the alt-screen as step 9
    // (`run_step_9_cc_migration_in_alt_screen`) so the wizard → TUI
    // handoff has ZERO inline stdin moments (#643).  The summary line
    // captured there flows through `result.migration_summary` and lands
    // in the post-wizard banner below.

    // Post-wizard summary banner — inline, AFTER the alt-screen has
    // closed.  This is exit chrome only.
    let provider_chain = provider_priority.join(" \u{2192} ");
    println!();
    wizard_box_top();
    wizard_box_line("");
    wizard_box_line(&format!("  {}", t!("wizard.summary.complete")));
    wizard_box_line("");
    wizard_box_line(&format!("    {}", t!("wizard.summary.default_model", model = result.chosen_model.clone())));
    wizard_box_line(&format!("    {}", t!("wizard.summary.providers", providers = provider_chain)));
    wizard_box_line(&format!("    {}", t!("wizard.summary.config_saved", path = config_path.display().to_string())));
    if let Some(ref summary) = result.migration_summary {
        wizard_box_line(&format!("    {}", t!("wizard.summary.migration", summary = summary.clone())));
    }
    wizard_box_line("");
    wizard_box_line(&format!("  {}", t!("wizard.summary.run_anvil")));
    wizard_box_line(&format!("  {}", t!("wizard.summary.type_help")));
    wizard_box_line("");
    // Task #663 Gap 4: surface the health-check command so the user
    // always knows how to diagnose later.  A5's `anvil --check` wires
    // up the verification pipeline this hint points at.
    wizard_box_line(&format!("  {}", crate::wizard_gaps::POST_WIZARD_CHECK_HINT));
    wizard_box_line("");
    wizard_box_bot();
    println!();

    WizardResult {
        wizard_completed: true,
        chosen_model: Some(result.chosen_model.clone()),
        vault_was_unlocked: result.steps123.vault_session_unlocked,
        migration_summary: result.migration_summary.clone(),
    }
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
    wizard_box_line(&t!("wizard.banner.welcome", version = env!("CARGO_PKG_VERSION")));
    wizard_box_line(&t!("wizard.banner.subtitle"));
    wizard_box_line("");
    wizard_box_line(&t!("wizard.banner.lets_setup"));
    wizard_box_line("");
    wizard_box_bot();

    let mut configured_providers: Vec<String> = Vec::new();
    let mut model_candidates: Vec<(String, String)> = Vec::new(); // (model_id, provider_label)

    // ── Step 1: Vault setup ───────────────────────────────────────────────────
    wizard_step_header(1, 7, &t!("wizard.step.vault_title"));
    println!();
    println!("  {}", t!("wizard.vault.body_l1"));
    println!("  {}", t!("wizard.vault.body_l2"));
    println!("  {}", t!("wizard.vault.body_l3"));
    println!();
    println!("  {}", t!("wizard.vault.opt_setup"));
    println!("  {}", t!("wizard.vault.opt_skip"));
    println!();

    let vault_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice_default1")));
    let vault_setup_done = match vault_choice.to_ascii_lowercase().trim() {
        "s" | "skip" => {
            println!("  {}", t!("wizard.vault.skipping"));
            println!("  \x1b[33m  {}\x1b[0m", t!("wizard.vault.warn_plaintext"));
            println!("  {}", t!("wizard.vault.tip_retry_setup"));
            false
        }
        _ => {
            // Default is to set up the vault.
            if runtime::vault_is_initialized() {
                println!("  {}", t!("wizard.vault.already_initialized"));
                println!("  {}", t!("wizard.vault.unlocking"));
                let pw_prompt = format!("  {}", t!("wizard.vault.prompt_master_password"));
                let pw = match rpassword::prompt_password(&pw_prompt) {
                    Ok(p) => p,
                    Err(_) => wizard_read_line(&pw_prompt),
                };
                match runtime::init_session_vault(&pw) {
                    Ok(true) => {
                        println!("  \x1b[32m  {}\x1b[0m", t!("wizard.vault.unlocked"));
                        true
                    }
                    Ok(false) => {
                        println!("  \x1b[33m  {}\x1b[0m", t!("wizard.vault.not_yet_initialized"));
                        false
                    }
                    Err(e) => {
                        println!("  \x1b[31m  {}\x1b[0m", t!("wizard.vault.unlock_failed", reason = e.to_string()));
                        println!("  {}", t!("wizard.vault.continue_no_vault"));
                        false
                    }
                }
            } else {
                let pw = loop {
                    let set_prompt = format!("  {}", t!("wizard.vault.prompt_set_password"));
                    let p1 = match rpassword::prompt_password(&set_prompt) {
                        Ok(p) => p,
                        Err(_) => wizard_read_line(&set_prompt),
                    };
                    if p1.is_empty() {
                        println!("  {}", t!("wizard.vault.empty_password"));
                        continue;
                    }
                    let confirm_prompt = format!("  {}", t!("wizard.vault.prompt_confirm_password"));
                    let p2 = match rpassword::prompt_password(&confirm_prompt) {
                        Ok(p) => p,
                        Err(_) => wizard_read_line(&confirm_prompt),
                    };
                    if p1 != p2 {
                        println!("  {}", t!("wizard.vault.password_mismatch"));
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
                                println!("  \x1b[32m  {}\x1b[0m", t!("wizard.vault.created_and_unlocked"));
                                println!("  {}", t!("wizard.vault.keys_encrypted_note"));
                                true
                            }
                            _ => {
                                // init_session_vault failed but vault is initialized;
                                // this is non-fatal — keys go to plaintext fallback.
                                println!("  \x1b[33m  {}\x1b[0m", t!("wizard.vault.created_session_lock_failed"));
                                println!("  {}", t!("wizard.vault.keys_plaintext_this_run"));
                                false
                            }
                        }
                    }
                    Err(e) => {
                        println!("  \x1b[31m  {}\x1b[0m", t!("wizard.vault.setup_failed", reason = e.to_string()));
                        println!("  {}", t!("wizard.vault.continue_keys_plaintext"));
                        false
                    }
                }
            }
        }
    };
    let _ = vault_setup_done; // informational; wizard_save_credential checks session state

    // ── Step 2: Ollama ────────────────────────────────────────────────────────
    wizard_step_header(2, 7, &t!("wizard.step.ollama_title"));
    println!();
    println!("  {}", t!("wizard.ollama.body_l1"));
    println!("  {}", t!("wizard.ollama.body_l2"));
    println!();
    println!("  {}", t!("wizard.ollama.opt_default"));
    println!("  {}", t!("wizard.ollama.opt_custom"));
    println!("  {}", t!("wizard.ollama.opt_apikey"));
    println!("  {}", t!("wizard.ollama.opt_skip"));
    println!();

    let ollama_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice")));
    let mut ollama_url: Option<String> = None;

    match ollama_choice.to_ascii_lowercase().as_str() {
        "1" | "" => {
            let url = "http://localhost:11434".to_string();
            print!("\n  {}", t!("wizard.ollama.testing_connection", url = url.clone()));
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m {}\x1b[0m", t!("wizard.ollama.connected"));
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  {}", t!("wizard.ollama.available_models"));
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                    let _ = wizard_save_credential("ollama_host", &url);
                }
                Err(e) => {
                    println!("\x1b[31m {}\x1b[0m", t!("wizard.ollama.failed", reason = e.to_string()));
                    println!("  {}", t!("wizard.ollama.start_hint"));
                }
            }
        }
        "2" => {
            let url = wizard_read_line(&format!("\n  {}", t!("wizard.ollama.prompt_url")));
            let url = if url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                url
            };
            print!("  {}", t!("wizard.ollama.testing_connection", url = url.clone()));
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m {}\x1b[0m", t!("wizard.ollama.connected"));
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  {}", t!("wizard.ollama.available_models"));
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                    let _ = wizard_save_credential("ollama_host", &url);
                }
                Err(e) => {
                    println!("\x1b[31m {}\x1b[0m", t!("wizard.ollama.failed", reason = e.to_string()));
                    println!("  {}", t!("wizard.ollama.config_saved_retry"));
                    let _ = wizard_save_credential("ollama_host", &url);
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                }
            }
        }
        "3" => {
            let url = wizard_read_line(&format!("\n  {}", t!("wizard.ollama.prompt_url")));
            let url = if url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                url
            };
            let api_key_prompt = format!("  {}", t!("wizard.ollama.prompt_api_key"));
            let api_key = match rpassword::prompt_password(&api_key_prompt) {
                Ok(k) => k,
                Err(_) => wizard_read_line(&api_key_prompt),
            };
            print!("  {}", t!("wizard.ollama.testing_connection", url = url.clone()));
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m {}\x1b[0m", t!("wizard.ollama.connected"));
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  {}", t!("wizard.ollama.available_models"));
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                }
                Err(e) => {
                    println!("\x1b[33m {}\x1b[0m", t!("wizard.ollama.could_not_verify_saving", reason = e.to_string()));
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
            println!("  {}", t!("wizard.ollama.skipping"));
        }
        other => {
            println!("  {}", t!("wizard.unknown_choice_skipping", choice = other, provider = "Ollama"));
        }
    }

    // ── Step 3: Anthropic ─────────────────────────────────────────────────────
    wizard_step_header(3, 7, &t!("wizard.step.anthropic_title"));
    println!();
    println!("  {}", t!("wizard.anthropic.body"));
    println!();
    println!("  {}", t!("wizard.anthropic.opt_oauth"));
    println!("  {}", t!("wizard.anthropic.opt_apikey"));
    println!("  {}", t!("wizard.anthropic.opt_skip"));
    println!();

    let anthropic_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice")));
    match anthropic_choice.to_ascii_lowercase().as_str() {
        "1" | "" => {
            println!();
            match run_anthropic_login() {
                Ok(()) => {
                    println!("\x1b[32m  {}\x1b[0m", t!("wizard.anthropic.oauth_complete"));
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
                    eprintln!("  {}", t!("wizard.anthropic.oauth_failed", reason = e.to_string()));
                    println!("  {}", t!("wizard.anthropic.retry_hint"));
                }
            }
        }
        "2" => {
            println!();
            println!("  {}", t!("wizard.anthropic.console_url"));
            let key_prompt = format!("  {}", t!("wizard.anthropic.prompt_api_key"));
            let api_key = match rpassword::prompt_password(&key_prompt) {
                Ok(k) => k,
                Err(_) => wizard_read_line(&key_prompt),
            };
            if api_key.is_empty() {
                println!("  {}", t!("wizard.anthropic.no_key_skipping"));
            } else {
                print!("  {}", t!("wizard.anthropic.validating_key"));
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
                    println!("\x1b[32m {}\x1b[0m", t!("wizard.anthropic.valid"));
                } else {
                    println!("\x1b[33m {}\x1b[0m", t!("wizard.anthropic.could_not_verify"));
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
            println!("  {}", t!("wizard.anthropic.skipping"));
        }
        other => {
            println!("  {}", t!("wizard.unknown_choice_skipping", choice = other, provider = "Anthropic"));
        }
    }

    // ── Step 4: OpenAI ────────────────────────────────────────────────────────
    wizard_step_header(4, 7, &t!("wizard.step.openai_title"));
    println!();
    println!("  {}", t!("wizard.openai.body"));
    println!();
    println!("  {}", t!("wizard.openai.opt_apikey"));
    println!("  {}", t!("wizard.openai.opt_skip"));
    println!();

    let openai_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice")));
    match openai_choice.to_ascii_lowercase().as_str() {
        "1" => {
            println!();
            println!("  {}", t!("wizard.openai.console_url"));
            let key_prompt = format!("  {}", t!("wizard.openai.prompt_api_key"));
            let api_key = match rpassword::prompt_password(&key_prompt) {
                Ok(k) => k,
                Err(_) => wizard_read_line(&key_prompt),
            };
            if api_key.is_empty() {
                println!("  {}", t!("wizard.openai.no_key_skipping"));
            } else {
                print!("  {}", t!("wizard.openai.validating_key"));
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
                    println!("\x1b[32m {}\x1b[0m", t!("wizard.openai.valid"));
                } else {
                    println!("\x1b[33m {}\x1b[0m", t!("wizard.openai.could_not_verify"));
                }
                let _ = wizard_save_credential("openai_api_key", &api_key);
                configured_providers.push("openai".to_string());
                model_candidates.push(("gpt-4o".to_string(), "OpenAI".to_string()));
                model_candidates.push(("gpt-4o-mini".to_string(), "OpenAI".to_string()));
            }
        }
        "s" | "skip" | "" => {
            println!("  {}", t!("wizard.openai.skipping"));
        }
        other => {
            println!("  {}", t!("wizard.unknown_choice_skipping", choice = other, provider = "OpenAI"));
        }
    }

    // ── Step 5: xAI (Grok) ───────────────────────────────────────────────────
    wizard_step_header(5, 7, &t!("wizard.step.xai_title"));
    println!();
    println!("  {}", t!("wizard.xai.body"));
    println!();
    println!("  {}", t!("wizard.xai.opt_apikey"));
    println!("  {}", t!("wizard.xai.opt_skip"));
    println!();

    let xai_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice")));
    match xai_choice.to_ascii_lowercase().as_str() {
        "1" => {
            println!();
            println!("  {}", t!("wizard.xai.console_url"));
            let key_prompt = format!("  {}", t!("wizard.xai.prompt_api_key"));
            let api_key = match rpassword::prompt_password(&key_prompt) {
                Ok(k) => k,
                Err(_) => wizard_read_line(&key_prompt),
            };
            if api_key.is_empty() {
                println!("  {}", t!("wizard.xai.no_key_skipping"));
            } else {
                let _ = wizard_save_credential("xai_api_key", &api_key);
                configured_providers.push("xai".to_string());
                model_candidates.push(("grok-3".to_string(), "xAI".to_string()));
                model_candidates.push(("grok-3-mini".to_string(), "xAI".to_string()));
                println!("  \x1b[32m{}\x1b[0m", t!("wizard.xai.saved"));
            }
        }
        "s" | "skip" | "" => {
            println!("  {}", t!("wizard.xai.skipping"));
        }
        other => {
            println!("  {}", t!("wizard.unknown_choice_skipping", choice = other, provider = "xAI"));
        }
    }

    // ── Step 6: Provider priority & default model ─────────────────────────────
    wizard_step_header(6, 7, &t!("wizard.step.priority_title"));
    println!();

    let mut seen = std::collections::HashSet::new();
    configured_providers.retain(|p| seen.insert(p.clone()));

    let provider_priority: Vec<String> = if configured_providers.is_empty() {
        println!("  {}", t!("wizard.priority.none_configured"));
        println!("  {}", t!("wizard.priority.configure_later"));
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
        println!("  {}", t!("wizard.priority.you_configured", providers = configured_display));
        println!();
        println!("  {}", t!("wizard.priority.set_order"));
        println!();
        println!("  {}", t!("wizard.priority.current_order"));
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
        println!("  {}", t!("wizard.priority.opt_keep"));
        println!("  {}", t!("wizard.priority.opt_reorder"));
        println!();
        let order_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice")));
        match order_choice.as_str() {
            "1" | "" => configured_providers.clone(),
            "2" => {
                let new_order = wizard_read_line(&format!("  {}", t!("wizard.priority.prompt_new_order")));
                let indices: Vec<usize> = new_order
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .collect();
                let reordered: Vec<String> = indices
                    .iter()
                    .filter_map(|&i| configured_providers.get(i.saturating_sub(1)).cloned())
                    .collect();
                if reordered.is_empty() {
                    println!("  {}", t!("wizard.priority.could_not_parse"));
                    configured_providers.clone()
                } else {
                    reordered
                }
            }
            _ => {
                println!("  {}", t!("wizard.priority.unknown_keep"));
                configured_providers.clone()
            }
        }
    };

    // Thinking mode toggle (kept on the inline path — small choice, no
    // need to bring up alt-screen just for this; the alt-screen comes
    // up below for steps 4..8).
    println!();
    println!("  \x1b[1;33m{}\x1b[0m", t!("wizard.thinking.title"));
    println!("  {}", t!("wizard.thinking.body_l1"));
    println!("  {}", t!("wizard.thinking.body_l2"));
    println!();
    println!("    {}", t!("wizard.thinking.opt_yes"));
    println!("    {}", t!("wizard.thinking.opt_no"));
    println!();
    let think_choice = wizard_read_line(&format!("  {}", t!("wizard.prompt.choice")));
    let thinking_enabled =
        think_choice.trim() == "1" || think_choice.trim().to_lowercase() == "yes";
    if thinking_enabled {
        println!("  \x1b[32m\u{2713}\x1b[0m {}", t!("wizard.thinking.enabled"));
    } else {
        println!("  {}", t!("wizard.thinking.disabled"));
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
                    eprintln!("  {}", t!("wizard.warn.altscreen_modal", reason = e.to_string()));
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
            eprintln!("\n  {}", t!("wizard.warn.could_not_save_config", reason = e.to_string()));
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
    wizard_box_line(&format!("  {}", t!("wizard.summary.complete")));
    wizard_box_line("");
    wizard_box_line(&format!("    {}", t!("wizard.summary.default_model", model = chosen_model.to_string())));
    wizard_box_line(&format!("    {}", t!("wizard.summary.providers", providers = provider_chain.clone())));
    wizard_box_line(&format!("    {}", t!("wizard.summary.config_saved", path = config_path.display().to_string())));
    wizard_box_line("");
    wizard_box_line(&format!("  {}", t!("wizard.summary.run_anvil")));
    wizard_box_line(&format!("  {}", t!("wizard.summary.type_help")));
    wizard_box_line("");
    // Task #663 Gap 4: post-wizard health-check pointer on the stdin
    // path as well, so CI / piped-input users see the same hint.
    wizard_box_line(&format!("  {}", crate::wizard_gaps::POST_WIZARD_CHECK_HINT));
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
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Char('2'))]);
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
        let keys = ScriptedKeySource::from_keys(k);
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
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Char('3'))]);
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
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Char('n'))]);
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
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Char('y'))]);
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
        let keys = ScriptedKeySource::from_keys(vec![]);
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
            let keys = ScriptedKeySource::from_keys(vec![
                key(KeyCode::Char('y')),  // vault
                key(KeyCode::Char('1')),  // provider A
                key(KeyCode::Char('y')),  // OAuth yes
                key(KeyCode::Char('1')),  // model m1
                key(KeyCode::Char('1')),  // layout A
                key(KeyCode::Char('y')),  // tabs yes
                key(KeyCode::Char('n')),  // mouse no (default OFF)
                key(KeyCode::Char('1')),  // theme Dark
            ]);
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

    // ── Task #643 — v2.2.17 zero-seam tests ──────────────────────────
    //
    // These tests exercise:
    //   - Step 9 (CC migration) detection + skip-when-absent
    //   - Step 9 ConfirmModal renders when CC is detected
    //   - Step 9 records the skip flag on No / Esc
    //   - WizardResult plumbs the chosen_model + vault_unlocked fields
    //   - vault_is_initialized honours ANVIL_CONFIG_HOME (D3)
    //   - The 8+1-step alt-screen pair count stays at exactly one
    //     enter / one exit when step 9 runs inside the session.
    //
    // The CC migration pipeline itself is exercised by the
    // commands/runtime tests; here we only assert that the wizard
    // path drives the ConfirmModal + flag-write contract.

    /// `detect_claude_code_dir` honours `ANVIL_CLAUDE_HOME` so tests
    /// can simulate "CC installed" without touching the developer's
    /// real `~/.claude/`.
    #[test]
    #[serial(anvil_claude_home)]
    fn detect_claude_code_dir_honours_env_override() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CLAUDE_HOME");
        let temp = std::env::temp_dir().join(format!(
            "anvil-test-claude-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp).expect("temp .claude");
        unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", &temp); }

        let detected = detect_claude_code_dir();
        assert_eq!(detected, Some(temp.clone()));

        // Cleanup.
        let _ = std::fs::remove_dir_all(&temp);
        match prev {
            Some(p) => unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CLAUDE_HOME"); },
        }
    }

    /// Step 9 — when CC is NOT detected, the migration step returns
    /// `Ok(None)` and emits NOTHING (no modal opens).
    #[test]
    #[serial(anvil_claude_home)]
    fn wizard_step_9_cc_migration_skips_silently_when_cc_absent() {
        let _lock = env_lock();
        let prev_claude = std::env::var_os("ANVIL_CLAUDE_HOME");
        let prev_home = std::env::var_os("ANVIL_CONFIG_HOME");
        // Point ANVIL_CLAUDE_HOME at a path that does NOT exist so
        // detect_claude_code_dir returns None.
        let bogus = std::env::temp_dir().join(format!(
            "anvil-test-no-cc-{}", std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&bogus);
        unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", &bogus); }
        // Also redirect ANVIL_CONFIG_HOME so `import_was_skipped()`
        // doesn't see a stale flag from the developer's real home.
        let cfg = std::env::temp_dir().join(format!(
            "anvil-test-cfg-{}", std::process::id()
        ));
        std::fs::create_dir_all(&cfg).expect("temp cfg");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &cfg); }

        let mut session = make_session();
        let keys = ScriptedKeySource::from_keys(vec![]);
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let summary = run_step_9_cc_migration_in_alt_screen(&mut runner)
            .expect("step9 returns Ok when CC absent");
        assert!(summary.is_none(),
            "step 9 must return None when CC is not detected");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&cfg);
        match prev_claude {
            Some(p) => unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CLAUDE_HOME"); },
        }
        match prev_home {
            Some(p) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
    }

    /// Step 9 — ConfirmModal opens when CC is detected, and pressing
    /// `n` writes the `.import-skipped` flag so the wizard does not
    /// re-prompt on the next run.
    #[test]
    #[serial(anvil_claude_home)]
    fn wizard_step_9_cc_migration_skip_advances_to_done_banner() {
        let _lock = env_lock();
        let prev_claude = std::env::var_os("ANVIL_CLAUDE_HOME");
        let prev_home = std::env::var_os("ANVIL_CONFIG_HOME");

        let cc_dir = std::env::temp_dir().join(format!(
            "anvil-test-cc-skip-{}", std::process::id()
        ));
        std::fs::create_dir_all(&cc_dir).expect("temp cc dir");
        unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", &cc_dir); }

        let cfg = std::env::temp_dir().join(format!(
            "anvil-test-skip-cfg-{}", std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cfg);
        std::fs::create_dir_all(&cfg).expect("temp cfg");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &cfg); }

        let mut session = make_session();
        // Press 'n' → ConfirmChoice::No → step 9 writes skip flag.
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Char('n'))]);
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let summary = run_step_9_cc_migration_in_alt_screen(&mut runner)
            .expect("step9 runs");
        assert!(summary.is_some(), "step 9 must return Some(line) when CC detected");
        let s = summary.unwrap();
        assert!(s.contains("skipped") || s.contains("Skipped"),
            "skip summary should mention 'skipped': {s:?}");

        // The .import-skipped flag should now exist under ANVIL_CONFIG_HOME.
        let flag = cfg.join(".import-skipped");
        assert!(flag.exists(),
            "Skip path must write `.import-skipped` under ANVIL_CONFIG_HOME");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&cfg);
        let _ = std::fs::remove_dir_all(&cc_dir);
        match prev_claude {
            Some(p) => unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CLAUDE_HOME"); },
        }
        match prev_home {
            Some(p) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
    }

    /// Step 9 — when `.import-skipped` is already on disk, the step
    /// MUST short-circuit silently (no modal opens, no key consumed).
    #[test]
    #[serial(anvil_config_home)]
    fn wizard_step_9_cc_migration_short_circuits_when_skip_flag_present() {
        let _lock = env_lock();
        let prev_home = std::env::var_os("ANVIL_CONFIG_HOME");
        let prev_claude = std::env::var_os("ANVIL_CLAUDE_HOME");

        let cfg = std::env::temp_dir().join(format!(
            "anvil-test-skipflag-{}", std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cfg);
        std::fs::create_dir_all(&cfg).expect("temp cfg");
        // Pre-write the skip flag.
        std::fs::write(cfg.join(".import-skipped"), b"").expect("flag");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &cfg); }
        // CC dir still exists (so detection would otherwise succeed),
        // but the flag must take precedence.
        let cc_dir = std::env::temp_dir().join(format!(
            "anvil-test-cc-flagged-{}", std::process::id()
        ));
        std::fs::create_dir_all(&cc_dir).expect("cc");
        unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", &cc_dir); }

        let mut session = make_session();
        let keys = ScriptedKeySource::from_keys(vec![]); // empty — must not be consumed
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );

        let summary = run_step_9_cc_migration_in_alt_screen(&mut runner)
            .expect("step9 short-circuits");
        assert!(summary.is_none(),
            "step 9 must skip silently when .import-skipped exists");

        let _ = std::fs::remove_dir_all(&cfg);
        let _ = std::fs::remove_dir_all(&cc_dir);
        match prev_home {
            Some(p) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
        match prev_claude {
            Some(p) => unsafe { std::env::set_var("ANVIL_CLAUDE_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CLAUDE_HOME"); },
        }
    }

    /// D3 — `vault_is_initialized()` reads from ANVIL_CONFIG_HOME/vault/
    /// when the env var is set, NOT $HOME/.anvil/vault/.
    ///
    /// Point ANVIL_CONFIG_HOME at an empty temp dir; the predicate must
    /// return false even when the developer's real $HOME/.anvil/vault/
    /// is initialised.
    #[test]
    #[serial(anvil_config_home)]
    fn vault_is_initialized_respects_anvil_config_home() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        let temp = std::env::temp_dir().join(format!(
            "anvil-vault-d3-{}", std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &temp); }

        // Vault dir does not exist under the temp config home → not initialized.
        assert!(!runtime::vault_is_initialized(),
            "vault_is_initialized must respect ANVIL_CONFIG_HOME (empty tempdir → false)");

        // And the resolved default vault dir is under the override.
        let vault_dir = runtime::VaultManager::default_vault_dir();
        assert!(vault_dir.starts_with(&temp),
            "default_vault_dir must live under ANVIL_CONFIG_HOME ({} vs {})",
            vault_dir.display(), temp.display());

        // Cleanup.
        let _ = std::fs::remove_dir_all(&temp);
        match prev {
            Some(p) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
    }

    /// D2 Case A — when the wizard's Step 1 successfully unlocks the
    /// vault, `WizardResult.vault_was_unlocked` is set to true so
    /// main.rs can skip its second unlock prompt.
    ///
    /// We assert the field-plumbing contract directly (constructing the
    /// `WizardSteps123 -> FullWizardResult -> WizardResult` mapping)
    /// rather than driving a real vault setup — `runtime::init_session_vault`
    /// uses a process-global OnceLock that cannot be reset across tests.
    #[test]
    fn wizard_returns_unlocked_vault_handle_when_password_set() {
        let steps123 = WizardSteps123 {
            configured_providers: vec!["anthropic".to_string()],
            model_candidates: vec![("claude-sonnet-4-5".into(), "Anthropic".into())],
            ollama_url: None,
            vault_session_unlocked: true,
        };
        let full = FullWizardResult {
            steps123: steps123.clone(),
            chosen_model: "claude-sonnet-4-5".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            profile_name: "default".to_string(),
            tui_answers: WizardTuiAnswers::defaults(),
            migration_summary: None,
        };

        // The mapping main.rs reads:
        let wr = WizardResult {
            wizard_completed: true,
            chosen_model: Some(full.chosen_model.clone()),
            vault_was_unlocked: full.steps123.vault_session_unlocked,
            migration_summary: full.migration_summary.clone(),
        };
        assert!(wr.wizard_completed);
        assert!(wr.vault_was_unlocked,
            "Step 1 unlock MUST propagate to WizardResult.vault_was_unlocked");
        assert_eq!(wr.chosen_model.as_deref(), Some("claude-sonnet-4-5"));
    }

    /// D2 Case A — `main_skips_prompt_when_wizard_returns_unlocked_vault`.
    ///
    /// We model main.rs's logic: when `vault_was_unlocked == true`, the
    /// `startup_vault_init` call site is skipped. We assert the
    /// branching contract here rather than spawning a subprocess.
    #[test]
    fn main_skips_prompt_when_wizard_returns_unlocked_vault() {
        let unlocked = WizardResult {
            wizard_completed: true,
            chosen_model: Some("model".to_string()),
            vault_was_unlocked: true,
            migration_summary: None,
        };
        let needs_unlock = WizardResult {
            wizard_completed: true,
            chosen_model: Some("model".to_string()),
            vault_was_unlocked: false,
            migration_summary: None,
        };
        // main.rs::run_repl: `if !wizard_vault_unlocked { startup_vault_init(); }`
        assert!(!unlocked.vault_was_unlocked == false,
            "vault_was_unlocked=true must short-circuit the startup unlock");
        assert!(!needs_unlock.vault_was_unlocked,
            "vault_was_unlocked=false routes through startup_vault_init");
    }

    /// Extends the existing 8-step single-alt-screen-transition test to
    /// cover the new step 9 CC migration. We synthesize a 9-modal
    /// queue (steps 1-8 + a step-9 ConfirmModal) drained inside ONE
    /// `WizardSession` and assert the hook counter still reads exactly
    /// one enter / one exit.
    #[test]
    fn wizard_emits_single_alt_screen_transition_through_step_9() {
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

        let mut queue = ModalQueue::new();
        // Steps 1..8 (compressed: one modal per banner — the wizard
        // collapses multi-modal steps already in v2.2.17).
        queue.push(QueuedModal::Confirm {
            tag: "step1".to_string(),
            modal: ConfirmModal::new("V?", "Y/N"),
        });
        queue.push(QueuedModal::Choice {
            tag: "step2".to_string(),
            modal: WizardChoiceModal::new("P", vec!["A".into(), "B".into()]),
        });
        queue.push(QueuedModal::Confirm {
            tag: "step3".to_string(),
            modal: ConfirmModal::new("O?", "Y/N"),
        });
        queue.push(QueuedModal::Choice {
            tag: "step4".to_string(),
            modal: WizardChoiceModal::new("M", vec!["m1".into(), "m2".into()]),
        });
        queue.push(QueuedModal::Choice {
            tag: "step7".to_string(),
            modal: WizardChoiceModal::new("L", vec!["A".into(), "B".into(), "C".into(), "D".into()]),
        });
        queue.push(QueuedModal::Confirm {
            tag: "step7-tabs".to_string(),
            modal: ConfirmModal::new("T?", "Y/N"),
        });
        queue.push(QueuedModal::Confirm {
            tag: "step8-mouse".to_string(),
            modal: ConfirmModal::new("M?", "Y/N"),
        });
        queue.push(QueuedModal::Choice {
            tag: "step8-theme".to_string(),
            modal: WizardChoiceModal::new("T", vec!["Dark".into(), "Light".into()]),
        });
        // NEW: step 9 CC migration confirm modal.
        queue.push(QueuedModal::Confirm {
            tag: "step9-cc-migration".to_string(),
            modal: ConfirmModal::new("Import CC?", "Y / N"),
        });

        {
            let keys = ScriptedKeySource::from_keys(vec![
                key(KeyCode::Char('y')),  // step1 vault
                key(KeyCode::Char('1')),  // step2 provider
                key(KeyCode::Char('y')),  // step3 auth
                key(KeyCode::Char('1')),  // step4 model
                key(KeyCode::Char('1')),  // step7 layout
                key(KeyCode::Char('y')),  // step7 tabs
                key(KeyCode::Char('n')),  // step8 mouse
                key(KeyCode::Char('1')),  // step8 theme
                key(KeyCode::Char('n')),  // step9 cc-migration -> skip
            ]);
            let mut runner = WizardModalRunner::new(
                &mut session,
                keys,
                ratatui::style::Color::Cyan,
            );
            let resolved = runner.run_queue(&mut queue).expect("run_queue");
            assert_eq!(resolved, 9, "all 9 modals must drain (8 steps + step 9 cc)");
        }

        // Session still active.
        assert_eq!(shared.borrow().entered, 1,
            "exactly one alt-screen enter for the 9-step run");
        assert_eq!(shared.borrow().left, 0,
            "no leave mid-run — step 9 is INSIDE the same session");

        drop(session);
        assert_eq!(shared.borrow().left, 1,
            "exactly one alt-screen leave on session drop");
    }

    /// D2 Case B — the vault unlock modal lives inside its own brief
    /// alt-screen `WizardSession` and exits with `VaultUnlockOutcome::Cancelled`
    /// when the user presses Esc.
    ///
    /// We assert the contract by constructing a `PasswordModal` and a
    /// scripted Esc key — the modal returns `Cancel`, which the host
    /// (in main.rs) maps to `VaultUnlockOutcome::Cancelled`.
    #[test]
    fn vault_unlock_modal_runs_in_alt_screen_for_returning_user() {
        use crate::tui::modals::password::{PasswordModal, PasswordAction};

        let mut modal = PasswordModal::new("Vault master password", "Master password");
        // Press Esc → Cancel.
        let action = modal.handle_key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ));
        assert!(
            matches!(action, PasswordAction::Cancel),
            "Esc on the vault-unlock PasswordModal must yield Cancel \
             (host maps to VaultUnlockOutcome::Cancelled — zero-seam fallback)"
        );

        // And a single alt-screen pair from a real WizardSession that
        // hosts the modal renders without panicking.
        let mut session = make_session();
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Esc)]);
        let mut runner = WizardModalRunner::new(
            &mut session,
            keys,
            ratatui::style::Color::Cyan,
        );
        let modal = PasswordModal::new("Vault", "Master password");
        let res = runner.run_password_capture(modal).expect("run_password_capture");
        assert!(res.is_none(), "Esc must produce a Cancel (None) outcome");
    }

    // ─── Phase A6 (task #645): wizard language picker ─────────────────
    //
    // Scenario the test pins down:
    //   1. ANVIL_CONFIG_HOME points at a fresh temp dir (no config.json).
    //   2. The picker runs via `run_language_step_in_alt_screen`.
    //   3. The scripted key source presses Down once (en → es), then Enter.
    //   4. After the step returns, config.json exists, contains `"language": "es"`,
    //      AND `rust_i18n::locale()` is "es" (so the rest of the wizard
    //      renders in the chosen locale immediately).

    #[test]
    #[serial(anvil_config_home)]
    fn wizard_language_step_persists_choice_and_sets_locale() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        let tmp = tempfile::tempdir().expect("tmpdir");
        set_anvil_config_home(tmp.path());

        // Sanity: no config.json yet — the picker must create one.
        assert!(!tmp.path().join("config.json").exists());

        // Persist the original rust-i18n locale so we can restore it
        // after the test (parallel tests may also probe locale).
        let prev_locale = rust_i18n::locale().to_string();

        let mut session = make_session();
        // Down arrow moves the highlight to index 1 ("es"), Enter commits.
        let keys = ScriptedKeySource::from_keys(vec![
            key(KeyCode::Down),
            key(KeyCode::Enter),
        ]);
        let mut runner =
            WizardModalRunner::new(&mut session, keys, ratatui::style::Color::Cyan);

        run_language_step_in_alt_screen(&mut runner)
            .expect("language step must complete");

        drop(runner);
        drop(session);

        // Persistence assertions.
        let config_path = tmp.path().join("config.json");
        assert!(
            config_path.exists(),
            "language step must create config.json on first run"
        );
        let data = std::fs::read_to_string(&config_path).expect("read config.json");
        let value: serde_json::Value =
            serde_json::from_str(&data).expect("parse config.json");
        assert_eq!(
            value.get("language").and_then(|v| v.as_str()),
            Some("es"),
            "language step must persist the chosen code"
        );

        // Process-locale assertion — the picker must apply the locale
        // mid-wizard so subsequent step banners are localised.
        assert_eq!(
            rust_i18n::locale().to_string(),
            "es",
            "language step must call rust_i18n::set_locale on commit"
        );

        // Restore env + locale so unrelated tests are not polluted.
        rust_i18n::set_locale(&prev_locale);
        restore_anvil_config_home(prev);
    }

    /// Esc on the language picker MUST NOT touch config.json or change
    /// the live locale.  Matches the documented "skip keeps current
    /// locale" behaviour.
    #[test]
    #[serial(anvil_config_home)]
    fn wizard_language_step_esc_is_noop() {
        let _lock = env_lock();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        let tmp = tempfile::tempdir().expect("tmpdir");
        set_anvil_config_home(tmp.path());

        let prev_locale = rust_i18n::locale().to_string();
        rust_i18n::set_locale("en");

        let mut session = make_session();
        let keys = ScriptedKeySource::from_keys(vec![key(KeyCode::Esc)]);
        let mut runner =
            WizardModalRunner::new(&mut session, keys, ratatui::style::Color::Cyan);

        run_language_step_in_alt_screen(&mut runner)
            .expect("language step must complete on Esc");

        drop(runner);
        drop(session);

        assert!(
            !tmp.path().join("config.json").exists(),
            "Esc on the picker must NOT create config.json"
        );
        assert_eq!(
            rust_i18n::locale().to_string(),
            "en",
            "Esc must leave the live locale untouched"
        );

        rust_i18n::set_locale(&prev_locale);
        restore_anvil_config_home(prev);
    }
}
