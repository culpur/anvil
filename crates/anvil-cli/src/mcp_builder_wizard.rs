//! TUI-modal wizard for `/mcp builder` (tasks #678 + #679).
//!
//! Replaces the legacy `run_mcp_builder` stdin/stdout flow with a
//! sequence of in-TUI modals driven by `WizardModalRunner`. Every
//! prompt — name, language, output dir, tools, inputs, summary,
//! disk-write, settings-wire, install — is a modal in the wizard's
//! own `WizardSession` alt-screen. Per
//! `feedback-tui-stdout-anti-pattern.md`, no `println!` /
//! `writeln!(io::stdout(), ...)` anywhere in this file.
//!
//! ## High-level flow
//!
//! 1. Welcome banner.
//! 2. Name (TextInput, loop on invalid).
//! 3. Language (Choice — Node / TypeScript / Python / AI generate).
//!    Branches:
//!      - AI branch: prompt → spec via active session model → confirm.
//!      - Manual branch: continue to tools loop.
//! 4. Output dir (TextInput with default = `~/mcp-servers/<name>`).
//! 5. Tools loop (skipped on AI branch).
//! 6. Summary (Choice — Scaffold / Cancel).
//! 7. Disk write (StreamingOutput).
//! 8. Settings wire (single-line status banner).
//! 9. Install (Yes/No → StreamingOutput).
//! 10. Done modal.
//!
//! On cancel at any step the wizard returns a single-line status
//! string for the chat scrollback.
//!
//! ## TUI / stdout discipline
//!
//! This file does NOT carry the `#![deny(clippy::print_stdout, ...)]`
//! crate-level attribute (`mcp_builder.rs` it lives next to does not
//! either — historically that file ran inline). Instead every prompt
//! goes through the modal runner, and every subprocess (install) is
//! attached to a `StreamingOutputModal` so its stdout/stderr is piped
//! into the ratatui buffer — never inherited.

#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{Event, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::style::Color;

use crate::mcp_builder::{
    self, InputKind, McpBuilderSpec, McpLanguage, McpToolInput, McpToolSpec, ObjectField,
    read_existing_mcp_names, validate_identifier, validate_name, write_scaffold_to_disk,
    wire_into_settings, install_command,
};
use crate::tui::modals::queue::{
    Choice, ModalAnswer, WizardChoiceModal,
};
use crate::tui::modals::streaming::StreamingOutputModal;
use crate::tui::modals::text_input::TextInputModal;
use crate::tui::modals::textarea::TextareaModal;
use crate::wizard_runner::{
    CrosstermHooks, KeySource, RunnerError, WizardModalRunner, WizardSession,
};

/// Cap for the manual `object { object { object … } }` nesting depth
/// (v1 contract per task spec).
const MAX_OBJECT_DEPTH: usize = 2;

/// Public entry point used by the `/mcp builder` slash command. Enters
/// its own alt-screen WizardSession, drives the modal sequence, and
/// returns a single-line status string for the chat scrollback.
///
/// `settings_path` is the path to `~/.anvil/settings.json` — passed in
/// so callers and tests can target a temp file.
///
/// `active_model` is `cli.model` at the moment `/mcp builder` was
/// dispatched. If the user picks "AI: generate from prompt", the
/// active session model is what generates the spec.
pub fn run_mcp_builder_wizard(settings_path: &Path, active_model: &str) -> String {
    // Build a fresh `WizardSession` that owns alt-screen for the
    // lifetime of the wizard. On drop the session restores cooked mode
    // — the main TUI's `restore_alt_screen` will re-enter alt-screen
    // afterwards so the conversation log paints clean.
    let backend = CrosstermBackend::new(io::stdout());
    let Ok(terminal) = Terminal::new(backend) else {
        return "MCP builder unavailable: failed to construct terminal.".to_string();
    };
    let mut session = match WizardSession::enter(terminal, CrosstermHooks::new()) {
        Ok(s) => s,
        Err(e) => return format!("MCP builder unavailable: {e}"),
    };

    let keys = WizardKeys {
        poll_timeout: Duration::from_millis(50),
    };
    let accent = Color::Cyan;
    let mut runner = WizardModalRunner::new(&mut session, keys, accent);

    let result = drive_wizard(&mut runner, settings_path, active_model);
    // Drop runner+session here (RAII restores cooked mode).
    drop(runner);
    drop(session);
    result
}

/// Local copy of `CrosstermKeySource` — keeps this module from leaking
/// `wizard_runner` internals to callers, and gives us a place to swap
/// in scripted sources for headless tests (not exercised in v1).
struct WizardKeys {
    poll_timeout: Duration,
}

impl KeySource for WizardKeys {
    fn next_key(&mut self) -> Option<KeyEvent> {
        loop {
            match crossterm::event::poll(self.poll_timeout) {
                Ok(true) => match crossterm::event::read() {
                    Ok(Event::Key(key)) => return Some(key),
                    Ok(_) => continue,
                    Err(_) => return None,
                },
                Ok(false) => continue,
                Err(_) => return None,
            }
        }
    }

    fn try_next_key(&mut self, timeout: Duration) -> Option<KeyEvent> {
        match crossterm::event::poll(timeout) {
            Ok(true) => match crossterm::event::read() {
                Ok(Event::Key(key)) => Some(key),
                _ => None,
            },
            _ => None,
        }
    }
}

fn drive_wizard<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    settings_path: &Path,
    active_model: &str,
) -> String
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let accent = ratatui::style::Color::Cyan;

    // ─── 1. Welcome banner ────────────────────────────────────────────
    if let Err(_) = runner.session.render_welcome_card(
        "MCP Builder",
        "Scaffold a Model Context Protocol server.",
        "The wizard collects a name, language, output dir, and tools.",
        "Press any key to begin · Esc to cancel",
        accent,
    ) {
        return "MCP builder failed to render welcome screen.".to_string();
    }
    match runner.keys.next_key() {
        Some(k) if matches!(k.code, crossterm::event::KeyCode::Esc) => {
            return "MCP builder cancelled.".to_string();
        }
        None => return "MCP builder cancelled.".to_string(),
        _ => {}
    }

    let existing = read_existing_mcp_names(settings_path);

    // ─── 2. Name ─────────────────────────────────────────────────────
    let name = match prompt_name(runner, &existing) {
        Some(n) => n,
        None => return "MCP builder cancelled.".to_string(),
    };

    // ─── 3. Language (with AI branch) ────────────────────────────────
    let (language, ai_spec) = match prompt_language(runner, active_model) {
        Some(LanguagePick::Manual(lang)) => (lang, None),
        Some(LanguagePick::AiGenerated { language, tools, name: ai_name }) => {
            // The AI may have chosen a different name; honour the user's
            // earlier name input by keeping `name` (per the spec: AI
            // returns a spec, the user confirms — they already picked
            // the name).
            let _ = ai_name; // We discard the AI's suggested name.
            (language, Some(tools))
        }
        None => return "MCP builder cancelled.".to_string(),
    };

    // ─── 4. Output dir ───────────────────────────────────────────────
    let output_dir = match prompt_output_dir(runner, &name) {
        Some(p) => p,
        None => return "MCP builder cancelled.".to_string(),
    };

    // ─── 5. Tools (manual only) ──────────────────────────────────────
    let tools = if let Some(t) = ai_spec {
        t
    } else {
        match prompt_tools(runner) {
            Some(t) => t,
            None => return "MCP builder cancelled.".to_string(),
        }
    };

    let spec = McpBuilderSpec {
        name: name.clone(),
        language,
        tools,
        output_dir: output_dir.clone(),
    };

    // ─── 6. Summary modal ────────────────────────────────────────────
    if !confirm_summary(runner, &spec) {
        return "MCP builder cancelled at confirmation.".to_string();
    }

    // ─── 7. Disk write ───────────────────────────────────────────────
    let written = match write_scaffold_to_disk(&spec) {
        Ok(paths) => paths,
        Err(e) => return format!("MCP builder failed: {e}"),
    };

    // Show a brief streaming-style modal listing the files. We use a
    // pre-populated StreamingOutputModal with no subprocess so the
    // runner just shows the tail + waits for any key.
    let _ = show_file_list_modal(runner, &written);

    // ─── 8. Settings wire ────────────────────────────────────────────
    let settings_status = match wire_into_settings(settings_path, &spec) {
        Ok(true) => format!(
            "✓ Registered as MCP server in {}",
            settings_path.display()
        ),
        Ok(false) => {
            "(settings.json already had this entry — no change.)".to_string()
        }
        Err(e) => format!(
            "! Could not update settings.json: {e}. Add the entry manually with /mcp later."
        ),
    };

    // ─── 9. Install? ─────────────────────────────────────────────────
    let did_install = match prompt_install(runner, spec.language, &spec.output_dir, &settings_status)
    {
        InstallChoice::Skip => false,
        InstallChoice::Cancelled => {
            // Return success — disk was written even if install was skipped.
            return done_summary(&spec, &settings_status, false);
        }
        InstallChoice::Run => run_install_streaming(runner, &spec),
    };

    // ─── 10. Done ────────────────────────────────────────────────────
    let _ = show_done_modal(runner, &spec, &settings_status, did_install);

    done_summary(&spec, &settings_status, did_install)
}

// ─── Step 2: name ───────────────────────────────────────────────────────────

fn prompt_name<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    existing: &BTreeSet<String>,
) -> Option<String>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let mut prompt = "Project name (kebab-case, e.g. weather-api)".to_string();
    loop {
        let modal = TextInputModal::new("MCP project name", prompt.clone());
        let answer = runner.run_text_input("mcp-name", modal).ok()?;
        let value = match answer {
            ModalAnswer::TextInput(s) => s,
            _ => return None,
        };
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        match validate_name(&trimmed, existing) {
            Ok(()) => return Some(trimmed),
            Err(e) => {
                prompt = format!("Invalid: {e}. Try again");
            }
        }
    }
}

// ─── Step 3: language (+ AI branch) ─────────────────────────────────────────

enum LanguagePick {
    Manual(McpLanguage),
    AiGenerated {
        name: String,
        language: McpLanguage,
        tools: Vec<McpToolSpec>,
    },
}

fn prompt_language<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    active_model: &str,
) -> Option<LanguagePick>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    loop {
        let modal = WizardChoiceModal::new_titled("Language")
            .with_choices(vec![
                Choice::new("Node.js")
                    .with_description("McpServer + zod via @modelcontextprotocol/sdk"),
                Choice::new("TypeScript")
                    .with_description("Typed Node — tsconfig + tsx"),
                Choice::new("Python")
                    .with_description("FastMCP"),
                Choice::new("AI: generate from prompt")
                    .with_badge("experimental")
                    .with_description("Describe the MCP, the active model fills in the spec"),
            ])
            .with_footer_hint("↑↓ + Enter · Esc to cancel");
        let answer = runner.run_choice("mcp-lang", modal).ok()?;
        match answer {
            ModalAnswer::Choice(0) => return Some(LanguagePick::Manual(McpLanguage::Node)),
            ModalAnswer::Choice(1) => {
                return Some(LanguagePick::Manual(McpLanguage::TypeScript));
            }
            ModalAnswer::Choice(2) => return Some(LanguagePick::Manual(McpLanguage::Python)),
            ModalAnswer::Choice(3) => match run_ai_generation_flow(runner, active_model) {
                AiOutcome::Generated { name, language, tools } => {
                    return Some(LanguagePick::AiGenerated { name, language, tools });
                }
                AiOutcome::SwitchToManual => continue,
                AiOutcome::Cancelled => return None,
            },
            _ => return None,
        }
    }
}

// ─── Step 4: output dir ─────────────────────────────────────────────────────

fn prompt_output_dir<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    name: &str,
) -> Option<PathBuf>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let default = default_output_dir(name).display().to_string();
    let mut prompt = "Where to scaffold the project".to_string();
    loop {
        let modal = TextInputModal::new("Output directory", prompt.clone())
            .with_default(default.clone());
        let answer = runner.run_text_input("mcp-output", modal).ok()?;
        let raw = match answer {
            ModalAnswer::TextInput(s) => s,
            _ => return None,
        };
        let expanded = expand_tilde(&raw);
        let path = if expanded.is_empty() {
            PathBuf::from(default.clone())
        } else {
            let p = PathBuf::from(&expanded);
            if p.is_absolute() {
                p
            } else {
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(p),
                    Err(_) => p,
                }
            }
        };
        // Validate: must not exist, OR exist + be empty.
        if path.exists() {
            match std::fs::read_dir(&path) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        prompt = format!(
                            "{} is not empty — pick another path",
                            path.display()
                        );
                        continue;
                    }
                }
                Err(e) => {
                    prompt = format!("read {}: {e}", path.display());
                    continue;
                }
            }
        }
        return Some(path);
    }
}

fn default_output_dir(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("mcp-servers").join(name)
}

fn expand_tilde(s: &str) -> String {
    let s = s.trim();
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped).display().to_string();
        }
    }
    s.to_string()
}

// ─── Step 5: tools loop ─────────────────────────────────────────────────────

fn prompt_tools<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
) -> Option<Vec<McpToolSpec>>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let mut tools: Vec<McpToolSpec> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let tool = match prompt_one_tool(runner, &seen, tools.len()) {
            ToolPromptOutcome::Tool(t) => t,
            ToolPromptOutcome::Done => return Some(tools),
            ToolPromptOutcome::Cancelled => return None,
        };
        seen.insert(tool.name.clone());
        tools.push(tool);

        // "Add another tool?" choice.
        let modal = WizardChoiceModal::new_titled("Add another tool?")
            .with_choices(vec![
                Choice::new("Yes — add another"),
                Choice::new("No — done with tools"),
            ]);
        let answer = runner.run_choice("mcp-add-more", modal).ok()?;
        match answer {
            ModalAnswer::Choice(0) => continue,
            ModalAnswer::Choice(1) => return Some(tools),
            _ => return None,
        }
    }
}

enum ToolPromptOutcome {
    Tool(McpToolSpec),
    Done,
    Cancelled,
}

fn prompt_one_tool<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    seen: &BTreeSet<String>,
    index: usize,
) -> ToolPromptOutcome
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    // Tool name.
    let name = loop {
        let prompt = format!(
            "Tool #{} name (snake_case, blank to finish)",
            index + 1
        );
        let modal = TextInputModal::new("Tool name", prompt);
        let answer = match runner.run_text_input("mcp-tool-name", modal) {
            Ok(a) => a,
            Err(_) => return ToolPromptOutcome::Cancelled,
        };
        let raw = match answer {
            ModalAnswer::TextInput(s) => s,
            _ => return ToolPromptOutcome::Cancelled,
        };
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            if index == 0 {
                // No tools yet — emit nothing; the scaffolder injects a
                // stub `hello` tool.
                return ToolPromptOutcome::Done;
            }
            return ToolPromptOutcome::Done;
        }
        if let Err(e) = validate_identifier(&raw) {
            show_error_banner(runner, &format!("Invalid tool name: {e}"));
            continue;
        }
        if seen.contains(&raw) {
            show_error_banner(runner, &format!("Duplicate tool name '{raw}'"));
            continue;
        }
        break raw;
    };

    // Tool description — textarea so users can write more than one sentence
    // when a tool has complex behaviour (task #684).
    let description = loop {
        let modal = TextareaModal::new(
            format!("Description for '{name}'"),
            "Describe what this tool does (Ctrl+Enter to submit)".to_string(),
        )
        .with_max_rows(8);
        let answer = match runner.run_textarea_input("mcp-tool-desc", modal) {
            Ok(a) => a,
            Err(_) => return ToolPromptOutcome::Cancelled,
        };
        let s = match answer {
            ModalAnswer::TextareaInput(s) => s.trim().to_string(),
            ModalAnswer::TextareaInputCancelled => return ToolPromptOutcome::Cancelled,
            _ => return ToolPromptOutcome::Cancelled,
        };
        if s.is_empty() {
            show_error_banner(runner, "Description is required");
            continue;
        }
        break s;
    };

    // Inputs.
    let inputs = match prompt_inputs(runner, &name) {
        Some(v) => v,
        None => return ToolPromptOutcome::Cancelled,
    };

    ToolPromptOutcome::Tool(McpToolSpec {
        name,
        description,
        inputs,
    })
}

fn prompt_inputs<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    tool_name: &str,
) -> Option<Vec<McpToolInput>>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let mut inputs: Vec<McpToolInput> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let prompt = format!(
            "Input #{} name for '{tool_name}' (blank to finish)",
            inputs.len() + 1
        );
        let modal = TextInputModal::new("Input name", prompt);
        let answer = runner.run_text_input("mcp-input-name", modal).ok()?;
        let raw = match answer {
            ModalAnswer::TextInput(s) => s.trim().to_string(),
            _ => return None,
        };
        if raw.is_empty() {
            return Some(inputs);
        }
        if let Err(e) = validate_identifier(&raw) {
            show_error_banner(runner, &format!("Invalid input name: {e}"));
            continue;
        }
        if seen.contains(&raw) {
            show_error_banner(runner, &format!("Duplicate input name '{raw}'"));
            continue;
        }

        // Description.
        let desc_modal = TextInputModal::new(
            format!("Description for input '{raw}'"),
            "Short description".to_string(),
        );
        let answer = runner.run_text_input("mcp-input-desc", desc_modal).ok()?;
        let description = match answer {
            ModalAnswer::TextInput(s) => s.trim().to_string(),
            _ => return None,
        };
        if description.is_empty() {
            show_error_banner(runner, "Input description is required");
            continue;
        }

        // Type.
        let kind = match prompt_input_kind(runner, 0) {
            Some(k) => k,
            None => return None,
        };

        // Optional?
        let opt_modal = WizardChoiceModal::new_titled(format!("Is '{raw}' optional?"))
            .with_choices(vec![Choice::new("Required"), Choice::new("Optional")]);
        let answer = runner.run_choice("mcp-input-opt", opt_modal).ok()?;
        let optional = matches!(answer, ModalAnswer::Choice(1));

        seen.insert(raw.clone());
        inputs.push(McpToolInput {
            name: raw,
            description,
            kind,
            optional,
        });
    }
}

fn prompt_input_kind<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    depth: usize,
) -> Option<InputKind>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let mut choices = vec![
        Choice::new("string"),
        Choice::new("number"),
        Choice::new("boolean"),
        Choice::new("enum of strings"),
    ];
    // Only offer array/object if we haven't hit the depth cap.
    let array_index = if depth + 1 <= MAX_OBJECT_DEPTH {
        choices.push(Choice::new("array (of …)"));
        Some(choices.len() - 1)
    } else {
        None
    };
    let object_index = if depth + 1 <= MAX_OBJECT_DEPTH {
        choices.push(Choice::new("object (named fields)"));
        Some(choices.len() - 1)
    } else {
        None
    };

    let modal = WizardChoiceModal::new_titled("Input type").with_choices(choices);
    let answer = runner.run_choice("mcp-kind", modal).ok()?;
    let idx = match answer {
        ModalAnswer::Choice(i) => i,
        _ => return None,
    };
    if idx == 0 {
        return Some(InputKind::String);
    }
    if idx == 1 {
        return Some(InputKind::Number);
    }
    if idx == 2 {
        return Some(InputKind::Boolean);
    }
    if idx == 3 {
        // Enum: prompt for comma-separated values.
        loop {
            let modal = TextInputModal::new(
                "Enum values",
                "Comma-separated values, e.g. metric, imperial".to_string(),
            );
            let answer = runner.run_text_input("mcp-enum", modal).ok()?;
            let raw = match answer {
                ModalAnswer::TextInput(s) => s,
                _ => return None,
            };
            let values: Vec<String> = raw
                .split(',')
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
            if values.is_empty() {
                show_error_banner(runner, "Enum needs at least one value");
                continue;
            }
            return Some(InputKind::Enum(values));
        }
    }
    if Some(idx) == array_index {
        let inner = prompt_input_kind(runner, depth + 1)?;
        return Some(InputKind::Array(Box::new(inner)));
    }
    if Some(idx) == object_index {
        let fields = prompt_object_fields(runner, depth + 1)?;
        return Some(InputKind::Object(fields));
    }
    None
}

fn prompt_object_fields<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    depth: usize,
) -> Option<Vec<ObjectField>>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    if depth > MAX_OBJECT_DEPTH {
        // v1 cap: at deeper nestings emit an empty object {} so the
        // scaffold still builds. The user is told via the README that
        // they need to extend by hand.
        return Some(Vec::new());
    }
    let mut fields: Vec<ObjectField> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let prompt = format!(
            "Object field #{} name (blank to finish)",
            fields.len() + 1
        );
        let modal = TextInputModal::new("Object field", prompt);
        let answer = runner.run_text_input("mcp-field-name", modal).ok()?;
        let raw = match answer {
            ModalAnswer::TextInput(s) => s.trim().to_string(),
            _ => return None,
        };
        if raw.is_empty() {
            return Some(fields);
        }
        if let Err(e) = validate_identifier(&raw) {
            show_error_banner(runner, &format!("Invalid field name: {e}"));
            continue;
        }
        if seen.contains(&raw) {
            show_error_banner(runner, &format!("Duplicate field name '{raw}'"));
            continue;
        }
        let kind = prompt_input_kind(runner, depth)?;
        let opt_modal = WizardChoiceModal::new_titled(format!("Is '{raw}' optional?"))
            .with_choices(vec![Choice::new("Required"), Choice::new("Optional")]);
        let answer = runner.run_choice("mcp-field-opt", opt_modal).ok()?;
        let optional = matches!(answer, ModalAnswer::Choice(1));
        seen.insert(raw.clone());
        fields.push(ObjectField {
            name: raw,
            kind,
            optional,
        });
    }
}

// ─── Step 6: summary ────────────────────────────────────────────────────────

fn confirm_summary<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    spec: &McpBuilderSpec,
) -> bool
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let mut body_lines: Vec<String> = Vec::new();
    body_lines.push(format!("name:     {}", spec.name));
    body_lines.push(format!("language: {}", spec.language.label()));
    body_lines.push(format!("output:   {}", spec.output_dir.display()));
    body_lines.push(format!("tools:    {}", spec.tools.len()));
    for tool in &spec.tools {
        body_lines.push(format!(
            "  - {} ({} input{})",
            tool.name,
            tool.inputs.len(),
            if tool.inputs.len() == 1 { "" } else { "s" }
        ));
    }
    let body_refs: Vec<&str> = body_lines.iter().map(String::as_str).collect();
    let _ = runner.session.render_banner(
        "Summary — review before scaffolding",
        &body_refs,
        ratatui::style::Color::Cyan,
    );

    let modal = WizardChoiceModal::new_titled("Scaffold this MCP?")
        .with_choices(vec![Choice::new("Yes — scaffold it"), Choice::new("Cancel")])
        .with_footer_hint("↑↓ + Enter · Esc to cancel");
    match runner.run_choice("mcp-confirm", modal) {
        Ok(ModalAnswer::Choice(0)) => true,
        _ => false,
    }
}

// ─── Step 7: file list display ──────────────────────────────────────────────

fn show_file_list_modal<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    written: &[PathBuf],
) -> Result<(), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let lines: Vec<String> = written
        .iter()
        .map(|p| format!("✓ {}", p.display()))
        .collect();
    let body_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    runner.session.render_banner(
        "Scaffold written",
        &body_refs,
        ratatui::style::Color::Green,
    )?;
    // Pause briefly so the user can read the list before moving on.
    let _ = runner.keys.try_next_key(Duration::from_millis(1500));
    Ok(())
}

// ─── Step 9: install ────────────────────────────────────────────────────────

enum InstallChoice {
    Run,
    Skip,
    Cancelled,
}

fn prompt_install<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    lang: McpLanguage,
    output_dir: &Path,
    settings_status: &str,
) -> InstallChoice
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let (program, args) = install_command(lang);
    let cmdline = format!("{program} {}", args.join(" "));
    let body = vec![
        settings_status.to_string(),
        format!("cd {}", output_dir.display()),
        cmdline.clone(),
    ];
    let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let _ = runner.session.render_banner(
        "Install dependencies?",
        &body_refs,
        ratatui::style::Color::Cyan,
    );

    let modal = WizardChoiceModal::new_titled(format!("Run `{cmdline}` now?"))
        .with_choices(vec![Choice::new("Yes — install now"), Choice::new("Skip")]);
    match runner.run_choice("mcp-install", modal) {
        Ok(ModalAnswer::Choice(0)) => InstallChoice::Run,
        Ok(ModalAnswer::Choice(1)) => InstallChoice::Skip,
        _ => InstallChoice::Cancelled,
    }
}

fn run_install_streaming<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    spec: &McpBuilderSpec,
) -> bool
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let (program, args) = install_command(spec.language);
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args).current_dir(&spec.output_dir);
    let modal = StreamingOutputModal::new(
        "Installing dependencies",
        format!("{program} {}", args.join(" ")),
    )
    .with_subprocess(cmd);
    match runner.run_streaming_output("mcp-install-run", modal) {
        Ok(ModalAnswer::StreamingResult { exit_code, .. }) => exit_code == 0,
        _ => false,
    }
}

// ─── Step 10: done modal + final summary string ─────────────────────────────

fn show_done_modal<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    spec: &McpBuilderSpec,
    settings_status: &str,
    did_install: bool,
) -> Option<()>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let mut body: Vec<String> = Vec::new();
    body.push(format!("path:       {}", spec.output_dir.display()));
    body.push(settings_status.to_string());
    body.push(format!(
        "install:    {}",
        if did_install { "ran" } else { "skipped" }
    ));
    body.push(String::new());
    body.push(next_steps_hint(spec.language, &spec.output_dir, did_install));
    let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let _ = runner
        .session
        .render_banner("MCP scaffolded", &body_refs, ratatui::style::Color::Green);

    let modal = WizardChoiceModal::new_titled("All done")
        .with_choices(vec![Choice::new("Close wizard")]);
    let _ = runner.run_choice("mcp-done", modal).ok()?;
    Some(())
}

fn next_steps_hint(lang: McpLanguage, output_dir: &Path, did_install: bool) -> String {
    let cd = format!("cd {}", output_dir.display());
    match lang {
        McpLanguage::Node => {
            if did_install {
                format!("Next: {cd} && npm test")
            } else {
                format!("Next: {cd} && npm install && npm test")
            }
        }
        McpLanguage::TypeScript => {
            if did_install {
                format!("Next: {cd} && npm test")
            } else {
                format!("Next: {cd} && npm install && npm test")
            }
        }
        McpLanguage::Python => {
            if did_install {
                format!("Next: {cd} && pytest -v")
            } else {
                format!(
                    "Next: {cd} && python -m venv .venv && source .venv/bin/activate && pip install -e '.[dev]' && pytest -v"
                )
            }
        }
    }
}

fn done_summary(spec: &McpBuilderSpec, settings_status: &str, did_install: bool) -> String {
    let install_str = if did_install { "installed" } else { "install skipped" };
    let _ = settings_status; // surface in TUI; in the return string keep concise.
    format!(
        "MCP \"{}\" scaffolded at {} ({install_str})",
        spec.name,
        spec.output_dir.display()
    )
}

// ─── Error banner helper ────────────────────────────────────────────────────

fn show_error_banner<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    msg: &str,
) where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    let _ = runner.session.render_banner(
        "Validation",
        &[msg],
        ratatui::style::Color::Red,
    );
    let _ = runner.keys.try_next_key(Duration::from_millis(900));
}

// ─── AI generation flow (task #679) ─────────────────────────────────────────

enum AiOutcome {
    Generated {
        name: String,
        language: McpLanguage,
        tools: Vec<McpToolSpec>,
    },
    SwitchToManual,
    Cancelled,
}

fn run_ai_generation_flow<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    active_model: &str,
) -> AiOutcome
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    // 1. Capture user prompt — multi-line textarea so the user can write
    //    a 200–500 word description (task #684).
    let modal = TextareaModal::new(
        "AI: describe the MCP",
        format!(
            "Describe what the MCP should do (Enter=newline, Ctrl+Enter=submit). Model: {active_model}"
        ),
    );
    let answer = match runner.run_textarea_input("mcp-ai-prompt", modal) {
        Ok(a) => a,
        Err(_) => return AiOutcome::Cancelled,
    };
    let prompt = match answer {
        ModalAnswer::TextareaInput(s) => s.trim().to_string(),
        ModalAnswer::TextareaInputCancelled => return AiOutcome::Cancelled,
        _ => return AiOutcome::Cancelled,
    };
    if prompt.is_empty() {
        return AiOutcome::SwitchToManual;
    }

    // 2. Render a "Generating..." banner — keep this lightweight; the
    // network call below blocks until the provider returns.
    let _ = runner.session.render_banner(
        "Generating MCP spec",
        &[
            &format!("Active model: {active_model}"),
            "Sending your description to the model…",
            "(this may take a few seconds)",
        ],
        ratatui::style::Color::Cyan,
    );

    // 3. Call the active model.
    let raw_response = match call_active_model(active_model, &prompt) {
        Ok(r) => r,
        Err(e) => return offer_retry_or_manual(runner, &format!("Model call failed: {e}"), ""),
    };

    // 4. Parse + validate.
    let ai_spec = match runtime::mcp_builder_ai::parse_spec(&raw_response) {
        Ok(s) => s,
        Err(e) => return offer_retry_or_manual(runner, &e, &raw_response),
    };

    // 5. Convert to McpBuilderSpec tools list.
    let language = match ai_spec.language {
        runtime::mcp_builder_ai::AiLanguage::Node => McpLanguage::Node,
        runtime::mcp_builder_ai::AiLanguage::Typescript => McpLanguage::TypeScript,
        runtime::mcp_builder_ai::AiLanguage::Python => McpLanguage::Python,
    };
    let tools: Result<Vec<McpToolSpec>, String> = ai_spec
        .tools
        .into_iter()
        .map(convert_tool)
        .collect();
    let tools = match tools {
        Ok(t) => t,
        Err(e) => {
            return offer_retry_or_manual(
                runner,
                &format!("Spec failed CLI-side validation: {e}"),
                &raw_response,
            );
        }
    };

    AiOutcome::Generated {
        name: ai_spec.name,
        language,
        tools,
    }
}

fn convert_tool(ai: runtime::mcp_builder_ai::AiToolSpec) -> Result<McpToolSpec, String> {
    let _ = mcp_builder::validate_identifier(&ai.name)
        .map_err(|e| format!("tool {:?}: {e}", ai.name))?;
    let inputs: Vec<McpToolInput> = ai
        .inputs
        .into_iter()
        .map(|i| {
            mcp_builder::validate_identifier(&i.name)
                .map_err(|e| format!("input {:?}: {e}", i.name))?;
            Ok::<_, String>(McpToolInput {
                name: i.name,
                description: i.description,
                kind: convert_kind(i.kind)?,
                optional: i.optional,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(McpToolSpec {
        name: ai.name,
        description: ai.description,
        inputs,
    })
}

fn convert_kind(ai: runtime::mcp_builder_ai::AiInputKind) -> Result<InputKind, String> {
    use runtime::mcp_builder_ai::AiInputKind as A;
    Ok(match ai {
        A::String => InputKind::String,
        A::Number => InputKind::Number,
        A::Boolean => InputKind::Boolean,
        A::Enum { values } => InputKind::Enum(values),
        A::Array { items } => InputKind::Array(Box::new(convert_kind(*items)?)),
        A::Object { fields } => {
            let converted: Result<Vec<ObjectField>, String> = fields
                .into_iter()
                .map(|f| {
                    mcp_builder::validate_identifier(&f.name)
                        .map_err(|e| format!("object field {:?}: {e}", f.name))?;
                    Ok::<_, String>(ObjectField {
                        name: f.name,
                        kind: convert_kind(f.kind)?,
                        optional: f.optional,
                    })
                })
                .collect();
            InputKind::Object(converted?)
        }
    })
}

fn offer_retry_or_manual<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    err: &str,
    raw_response: &str,
) -> AiOutcome
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: crate::wizard_runner::TerminalHooks,
    K: KeySource,
{
    // Show the error + truncated raw response.
    let truncated: String = raw_response.chars().take(400).collect();
    let mut body: Vec<String> = Vec::new();
    body.push(format!("Error: {err}"));
    if !truncated.is_empty() {
        body.push(String::new());
        body.push("Model response (truncated):".to_string());
        for line in truncated.lines().take(8) {
            body.push(format!("  {line}"));
        }
    }
    let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let _ = runner.session.render_banner(
        "AI spec generation failed",
        &body_refs,
        ratatui::style::Color::Red,
    );
    let modal = WizardChoiceModal::new_titled("What now?").with_choices(vec![
        Choice::new("Try again — re-prompt the model"),
        Choice::new("Switch to manual entry"),
        Choice::new("Cancel wizard"),
    ]);
    match runner.run_choice("mcp-ai-retry", modal) {
        Ok(ModalAnswer::Choice(0)) => AiOutcome::SwitchToManual, // caller loops the language picker
        Ok(ModalAnswer::Choice(1)) => AiOutcome::SwitchToManual,
        _ => AiOutcome::Cancelled,
    }
}

/// One-shot non-streaming call to the active provider for spec
/// generation. Uses `api::ProviderClient::from_model` + the
/// `MessageRequest` envelope, identical to `DefaultRuntimeClient` but
/// without all the tool / TUI / streaming plumbing — we only need a
/// single textual completion.
fn call_active_model(model: &str, user_prompt: &str) -> Result<String, String> {
    use api::{InputMessage, MessageRequest, ProviderClient, max_tokens_for_model};

    let system = runtime::mcp_builder_ai::build_system_prompt();
    let request = MessageRequest {
        model: model.to_string(),
        max_tokens: max_tokens_for_model(model),
        messages: vec![InputMessage::user_text(user_prompt)],
        system: Some(system),
        tools: None,
        tool_choice: None,
        stream: false,
    };

    let client = ProviderClient::from_model(model).map_err(|e| e.to_string())?;
    // Lightweight tokio runtime — single-thread is fine for a one-shot
    // call.  Identical to how `DefaultRuntimeClient` creates its own
    // tokio runtime in `providers.rs`.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio init: {e}"))?;
    let response = rt
        .block_on(async { client.send_message(&request).await })
        .map_err(|e| format!("provider: {e}"))?;

    // Concatenate every Text block in the response.
    let mut out = String::new();
    for block in &response.content {
        if let api::OutputContentBlock::Text { text } = block {
            out.push_str(text);
        }
    }
    if out.trim().is_empty() {
        return Err("model returned no text content".to_string());
    }
    Ok(out)
}

// ─── Internal small helper: VecDeque import keeps a future scripted test path open ──

#[allow(dead_code)]
fn _vecdeque_anchor() -> VecDeque<KeyEvent> {
    VecDeque::new()
}
