// Task #626 — `utils.rs` is a mix of pure render helpers (String-returning,
// SAFE) and a single SAFE-HEADLESS subcommand (`run_init`).  The
// crate-level deny catches any future `println!` regression; `run_init`
// carries an explicit per-fn `#[allow]`.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use runtime::{
    load_system_prompt, ContentBlock, MemoryManager, MessageRole, Session, Theme, ConfigLoader, ConfigSource, ProjectContext,
};
use commands::{render_slash_command_help, suggest_slash_commands};
use crate::tui::AnvilTui;
use crate::providers::suggest_repl_commands;
use crate::{
    DEFAULT_DATE, VERSION, BUILD_TARGET, GIT_SHA,
    StatusContext, StatusUsage, parse_git_status_metadata,
};
use crate::init::initialize_repo;


/// Task #567 — CC parity: when `NO_COLOR=1` or `FORCE_COLOR=0` is set the
/// Anvil headless / `--print` paths must strip ANSI colors, but the
/// interactive TUI is a graphical application surface — it must keep its
/// UI chrome regardless. Pass `tui_active = true` from inside the TUI
/// runtime so the env vars never strip the layout's accent colors,
/// borders, or syntax highlight.
///
/// Resolution order (high → low priority):
/// 1. `tui_active == true` → always `true` (TUI keeps its colors).
/// 2. `FORCE_COLOR` set to a non-`0`, non-empty value → `true`.
/// 3. `NO_COLOR` set to any non-empty value → `false`.
/// 4. `FORCE_COLOR=0` → `false`.
/// 5. Default → `true`.
#[allow(dead_code, reason = "task #567 v2.2.17: helper exposed for future headless callers; gate exercised by color_env_tests")]
#[must_use]
pub fn should_use_color(tui_active: bool) -> bool {
    if tui_active {
        return true;
    }
    if let Ok(force) = env::var("FORCE_COLOR") {
        let trimmed = force.trim();
        if !trimmed.is_empty() {
            return trimmed != "0";
        }
    }
    if let Ok(no) = env::var("NO_COLOR") {
        if !no.trim().is_empty() {
            return false;
        }
    }
    true
}

pub(crate) fn render_repl_help() -> String {
    [
        "Interactive REPL".to_string(),
        "  Quick start          Ask a task in plain English or use one of the core commands below."
            .to_string(),
        "  Core commands        /help · /status · /model · /permissions · /compact".to_string(),
        "  Exit                 /exit or /quit".to_string(),
        "  Vim mode             /vim toggles modal editing".to_string(),
        "  History              Up/Down recalls previous prompts".to_string(),
        "  Completion           Tab cycles slash command matches".to_string(),
        "  Cancel               Ctrl-C clears input (or exits on an empty prompt)".to_string(),
        "  Multiline            Shift+Enter or Ctrl+J inserts a newline".to_string(),
        String::new(),
        render_slash_command_help(),
    ]
    .join(
        "
",
    )
}

pub(crate) fn append_slash_command_suggestions(lines: &mut Vec<String>, name: &str) {
    let suggestions = suggest_slash_commands(name, 3);
    if suggestions.is_empty() {
        lines.push("  Try              /help shows the full slash command map".to_string());
        return;
    }

    lines.push("  Try              /help shows the full slash command map".to_string());
    lines.push("Suggestions".to_string());
    lines.extend(
        suggestions
            .into_iter()
            .map(|suggestion| format!("  {suggestion}")),
    );
}

pub(crate) fn render_unknown_repl_command(name: &str) -> String {
    let mut lines = vec![
        "Unknown slash command".to_string(),
        format!("  Command          /{name}"),
    ];
    append_repl_command_suggestions(&mut lines, name);
    lines.join("\n")
}

pub(crate) fn append_repl_command_suggestions(lines: &mut Vec<String>, name: &str) {
    let suggestions = suggest_repl_commands(name);
    if suggestions.is_empty() {
        lines.push("  Try              /help shows the full slash command map".to_string());
        return;
    }

    lines.push("  Try              /help shows the full slash command map".to_string());
    lines.push("Suggestions".to_string());
    lines.extend(
        suggestions
            .into_iter()
            .map(|suggestion| format!("  {suggestion}")),
    );
}

pub(crate) fn render_mode_unavailable(command: &str, label: &str) -> String {
    [
        "Command unavailable in this REPL mode".to_string(),
        format!("  Command          /{command}"),
        format!("  Feature          {label}"),
        "  Tip              Use /help to find currently wired REPL commands".to_string(),
    ]
    .join("\n")
}


// ---------------------------------------------------------------------------
/// Return `~/.anvil/` as a `PathBuf`.
///
/// On Windows, "home" resolves via `%USERPROFILE%`; on macOS/Linux via `$HOME`.
/// If neither can be determined, falls back to the current working directory.
pub(crate) fn anvil_home_dir() -> PathBuf {
    if let Ok(config_home) = std::env::var("ANVIL_CONFIG_HOME") {
        return PathBuf::from(config_home);
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join(".anvil")
}

// ─── Standalone language command handler ─────────────────────────────────────
// ─── Feature 21 — Credential Vault free function ─────────────────────────────

/// Map a language name to its LSP server binary name.
pub(crate) fn lsp_binary_for_lang(lang: &str) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "rust" => "rust-analyzer",
        "typescript" | "ts" | "javascript" | "js" => "typescript-language-server",
        "python" | "py" => "pylsp",
        "go" => "gopls",
        "java" => "jdtls",
        "c" | "cpp" | "c++" => "clangd",
        other => other,
    }
    .to_string()
}

/// Extract a single cell from a Jupyter notebook JSON by 1-based index.
pub(crate) fn extract_notebook_cell(raw: &str, cell_n: usize) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let cells = v["cells"]
        .as_array()
        .ok_or_else(|| "No cells array in notebook".to_string())?;
    let cell = cells
        .get(cell_n.saturating_sub(1))
        .ok_or_else(|| format!("Cell {cell_n} not found (notebook has {} cells)", cells.len()))?;
    let source = cell["source"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .or_else(|| cell["source"].as_str().map(ToOwned::to_owned))
        .unwrap_or_default();
    Ok(source)
}

/// Escape a string for safe embedding inside a JSON string value.
///
/// Handles `\`, `"`, newlines, carriage returns, tabs, and all ASCII
/// control characters that would produce malformed JSON if left unescaped.
pub(crate) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"'  => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Remaining ASCII control characters encoded as \uXXXX.
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Convert a `Command::output()` result into a human-readable string.
pub(crate) fn shell_output_or_err(result: Result<std::process::Output, std::io::Error>, context: &str) -> String {
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else {
                format!("{context}: (no output)")
            }
        }
        Err(e) => format!("{context}: {e}"),
    }
}
// ─── CI/CD project-type detection ────────────────────────────────────────────

pub(crate) fn detect_project_type_for_pipeline() -> &'static str {
    if Path::new("Cargo.toml").exists() { return "Rust (Cargo)"; }
    if Path::new("go.mod").exists() { return "Go"; }
    if Path::new("pyproject.toml").exists() || Path::new("setup.py").exists() { return "Python"; }
    if Path::new("pom.xml").exists() { return "Java (Maven)"; }
    if Path::new("build.gradle").exists() || Path::new("build.gradle.kts").exists() {
        return "Java/Kotlin (Gradle)";
    }
    if Path::new("package.json").exists() {
        // Check if it's a Next.js project
        if let Ok(c) = fs::read_to_string("package.json") {
            if c.contains("\"next\"") { return "Next.js"; }
            if c.contains("\"react\"") { return "React"; }
        }
        return "Node.js";
    }
    if Path::new("Dockerfile").exists() { return "Docker"; }
    "generic"
}

// ─── Desktop notification helper ─────────────────────────────────────────────

pub(crate) fn send_desktop_notification(title: &str, message: &str) -> String {
    // macOS
    if cfg!(target_os = "macos") {
        let script = format!(
            r#"display notification "{msg}" with title "{title}""#,
            msg = json_escape(message),
            title = json_escape(title),
        );
        let out = Command::new("osascript").args(["-e", &script]).output();
        return match out {
            Ok(o) if o.status.success() => format!("Notification sent: {message}"),
            Ok(o) => format!("osascript failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => format!("osascript not available: {e}"),
        };
    }
    // Linux (notify-send)
    let out = Command::new("notify-send").args([title, message]).output();
    match out {
        Ok(o) if o.status.success() => format!("Notification sent: {message}"),
        Ok(_) => {
            // Fall back to wall/echo
            format!("Desktop notification: [{title}] {message}")
        }
        Err(_) => format!(
            "notify-send not available. Install libnotify-bin (Linux) or use macOS.\n\
             Message: [{title}] {message}"
        ),
    }
}



/// Canonical Tier-1 language code list (single source of truth).
///
/// Used by:
///   * `run_language_command_static` (the `/language <code>` slash command),
///   * the `--lang <code>` CLI flag (`main::run`),
///   * the first-run wizard's language picker step (`wizard.rs`),
///   * the `/configure` Language entry (`tui::mod`),
///   * the drift-gate test that asserts every code here has a sibling
///     `locales/<code>.yml` (regression for the v2.2.19 pt-BR omission).
///
/// Order matches the i18n migration plan's Tier-1 ordering — wizard +
/// configure pickers display in this order.
pub(crate) const SUPPORTED_LANGUAGES: &[&str] = &[
    "en", "es", "zh-CN", "fr", "pt-BR", "ru", "ja", "de", "ko", "it", "tr", "vi", "pl", "id", "nl",
    "sv", "nb", "uk",
];

/// Persist `language: <code>` to `~/.anvil/config.json`, preserving all
/// other keys, and apply it to the live `rust_i18n` locale.
///
/// Shared by `run_language_command_static`, the wizard's language step,
/// and `/configure` Language picker — single persistence path (DRY) so
/// every entry point produces the same on-disk shape.
///
/// Returns `Ok(())` on success or the underlying I/O error.  Callers
/// format their own user-facing messages off the result.
pub(crate) fn save_language(code: &str) -> std::io::Result<()> {
    let anvil_dir = anvil_home_dir();
    let path = anvil_dir.join("config.json");
    let mut map = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|data| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data).ok()
            })
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    map.insert(
        "language".to_string(),
        serde_json::Value::String(code.to_string()),
    );

    fs::create_dir_all(&anvil_dir)?;
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default(),
    )?;
    rust_i18n::set_locale(code);
    Ok(())
}

pub(crate) fn run_language_command_static(lang: Option<&str>) -> String {
    let Some(lang) = lang else {
        let current = current_language_code();
        return format!(
            "Language: {current}\nAvailable: {}\nUsage: /language <code>",
            SUPPORTED_LANGUAGES.join(", ")
        );
    };

    let lang = lang.trim();
    if lang.is_empty() {
        return format!(
            "Language: {}\nAvailable: {}\nUsage: /language <code>",
            current_language_code(),
            SUPPORTED_LANGUAGES.join(", ")
        );
    }

    if !SUPPORTED_LANGUAGES.contains(&lang) {
        return format!(
            "Unsupported language '{lang}'. Available: {}",
            SUPPORTED_LANGUAGES.join(", ")
        );
    }

    match save_language(lang) {
        Ok(()) => format!("Language set to: {lang}"),
        Err(e) => format!("Failed to save language setting: {e}"),
    }
}

/// Write `output_style` to `~/.anvil/config.json`, preserving all other keys.
pub(crate) fn save_output_style(style: runtime::OutputStyle) {
    let anvil_dir = anvil_home_dir();
    let path = anvil_dir.join("config.json");
    let mut map = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data).ok())
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    map.insert("output_style".to_string(), serde_json::Value::String(style.as_str().to_string()));
    let _ = fs::create_dir_all(&anvil_dir);
    let _ = fs::write(&path, serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default());
}

/// Read `output_style` from `~/.anvil/config.json`, defaulting to `Precise`.
/// Only built-in names are resolved here (no disk I/O for custom styles).
pub(crate) fn load_output_style() -> runtime::OutputStyle {
    let path = anvil_home_dir().join("config.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|data| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data).ok())
        .and_then(|map| map.get("output_style").and_then(|v| v.as_str()).map(str::to_string))
        .map(|s| runtime::output_style_from_str_builtin_only(&s))
        .unwrap_or_default()
}

/// Return the currently configured language code, defaulting to "en".
pub(crate) fn current_language_code() -> String {
    let path = anvil_home_dir().join("config.json");
    if let Ok(data) = fs::read_to_string(&path)
        && let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
            && let Some(lang) = map.get("language").and_then(|v| v.as_str()) {
                return lang.to_string();
            }
    "en".to_string()
}


/// Static version of `/configure` for use in the `--resume` path, where no
/// `LiveCli` instance is available.  Produces the same output as the live
/// version for purely informational sub-commands; write operations advise
/// the user to run `/configure` in an active session.
pub(crate) fn render_configure_static(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    let mut parts = args.splitn(2, ' ');
    let section = parts.next().unwrap_or("").trim();

    match section {
        "" => [
            "Anvil Configuration",
            "",
            "  /configure providers    Providers & authentication",
            "  /configure models       Models & defaults",
            "  /configure context      Context & memory",
            "  /configure search       Search providers",
            "  /configure permissions  Permissions & security",
            "  /configure display      Display & interface",
            "  /configure integrations Integrations",
            "",
            "Note: start an active session to use setter sub-commands.",
        ]
        .join("\n"),
        "providers" => {
            let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty());
            let anthropic_oauth = runtime::load_oauth_credentials().ok().flatten().is_some();
            let anthropic_status = if anthropic_oauth { "[✓ OAuth]" } else if anthropic_key.is_some() { "[✓ API key]" } else { "[✗ not configured]" };
            let openai_status = if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_some() { "[✓ API key]" } else { "[✗ not configured]" };
            let ollama_host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
            let xai_status = if std::env::var("XAI_API_KEY").ok().filter(|s| !s.is_empty()).is_some() { "[✓ API key]" } else { "[✗ not configured]" };
            format!(
                "Providers & Authentication\n\n  Anthropic   {anthropic_status}\n  OpenAI      {openai_status}\n  Ollama      [{ollama_host}]\n  xAI         {xai_status}"
            )
        }
        "search" => {
            let engine = runtime::SearchEngine::from_env_and_config();
            let default_provider = engine.default_provider().to_string();
            format!("Default search provider: {default_provider}\n\nRun /configure search in an active session for full details.")
        }
        _ => format!(
            "Run /configure {section} in an active session to view and edit settings.\n\n\
             For a read-only overview: /configure (main menu)"
        ),
    }
}

/// Handle the `/theme` slash command.
///
/// - `/theme`           — show the active theme name
/// - `/theme list`      — list all built-in themes
/// - `/theme set <n>`   — load built-in theme, persist, and optionally hot-apply
/// - `/theme reset`     — revert to culpur-defense default
///
/// When `tui` is `Some` the theme is applied to the live TUI immediately.
/// Distinguish between local plugin sources (paths, git URLs) and AnvilHub
/// slugs.  Used by `/plugin install` to route between
/// `PluginManager::install` (paths + git) and `HubClient::install` (slugs).
///
/// A target is local-or-git when ANY of:
///   * It starts with `http://`, `https://`, `git@`, `./`, `../`, `/`, or `~`.
///   * Its extension is `.git`.
///   * It contains a path separator (`/` or `\`).
///   * It resolves to an existing path on disk.
///
/// Otherwise (a bare token like `my-plugin`) it is treated as an AnvilHub slug.
pub(crate) fn is_local_or_git_install_source(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("git@")
        || t.starts_with("./")
        || t.starts_with("../")
        || t.starts_with('/')
        || t.starts_with('~')
    {
        return true;
    }
    if std::path::Path::new(t)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("git"))
    {
        return true;
    }
    if t.contains('/') || t.contains('\\') {
        return true;
    }
    std::path::Path::new(t).exists()
}

pub(crate) fn run_theme_command(action: Option<&str>, tui: Option<&mut AnvilTui>) -> String {
    let action = action.unwrap_or("").trim();
    let mut parts = action.splitn(2, ' ');
    let sub = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    match sub {
        "" => {
            let current = Theme::load();
            format!(
                "Theme\n  Active           {}\n\nNext\n  /theme list      List available themes\n  /theme set <n>   Switch theme",
                current.name
            )
        }
        "list" => {
            let names = Theme::builtin_names();
            let active = Theme::load().name;
            let mut lines = vec!["Available themes".to_string()];
            for name in names {
                let marker = if *name == active { "● " } else { "  " };
                lines.push(format!("  {marker}{name}"));
            }
            lines.push(String::new());
            lines.push("  /theme set <name>   Apply a theme".to_string());
            lines.join("\n")
        }
        "set" if !arg.is_empty() => {
            if let Some(theme) = Theme::builtin(arg) {
                let name = theme.name.clone();
                if let Err(e) = theme.save() {
                    return format!("Theme save error: {e}");
                }
                if let Some(tui) = tui {
                    tui.set_theme(Theme::builtin(&name).unwrap_or_else(Theme::default_theme));
                }
                format!(
                    "Theme changed\n  Active           {name}\n  Persisted        ~/.anvil/theme.json"
                )
            } else {
                let names = Theme::builtin_names().join(", ");
                format!("Unknown theme: {arg}\n  Available: {names}")
            }
        }
        "set" => "Usage: /theme set <name>  (try /theme list)".to_string(),
        "reset" => {
            let theme = Theme::default_theme();
            let name = theme.name.clone();
            if let Err(e) = theme.save() {
                return format!("Theme reset error: {e}");
            }
            if let Some(tui) = tui {
                tui.set_theme(Theme::default_theme());
            }
            format!(
                "Theme reset\n  Active           {name}\n  Persisted        ~/.anvil/theme.json"
            )
        }
        // Feature 18 — export current theme to a JSON file
        "export" => {
            let dest = if arg.is_empty() {
                let current = Theme::load();
                format!("{}.theme.json", current.name)
            } else {
                arg.to_string()
            };
            let theme = Theme::load();
            match theme.save() {
                Ok(()) => {
                    // Copy ~/.anvil/theme.json to the requested destination
                    let src = anvil_home_dir().join("theme.json");
                    match std::fs::copy(&src, &dest) {
                        Ok(_) => format!(
                            "Theme exported\n  Theme            {}\n  File             {dest}",
                            theme.name
                        ),
                        Err(e) => format!("Export error: {e}"),
                    }
                }
                Err(e) => format!("Export error: {e}"),
            }
        }
        // Feature 18 — import a theme from a JSON file and apply it
        "import" => {
            if arg.is_empty() {
                return "Usage: /theme import <file.json>".to_string();
            }
            match std::fs::read_to_string(arg) {
                Ok(text) => {
                    // Validate JSON structure before writing
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(_) => {
                            let dest = anvil_home_dir().join("theme.json");
                            if let Some(parent) = dest.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&dest, &text) {
                                Ok(()) => {
                                    let theme = Theme::load();
                                    if let Some(tui) = tui {
                                        tui.set_theme(Theme::load());
                                    }
                                    format!(
                                        "Theme imported\n  Active           {}\n  Source           {arg}",
                                        theme.name
                                    )
                                }
                                Err(e) => format!("Import error: {e}"),
                            }
                        }
                        Err(e) => format!("Invalid theme JSON: {e}"),
                    }
                }
                Err(e) => format!("Cannot read {arg}: {e}"),
            }
        }
        // Feature 18 — create a new custom theme interactively (AI-guided)
        "create" => {
            let name = if arg.is_empty() { "custom" } else { arg };
            format!(
                "Theme create — {name}\n\n\
                 To create a custom theme, edit or create a JSON file with this structure:\n\n\
                 {{\n\
                   \"name\": \"{name}\",\n\
                   \"colors\": {{\n\
                     \"bg_primary\":       \"#1e1e2e\",\n\
                     \"bg_card\":          \"#313244\",\n\
                     \"text_primary\":     \"#cad3f5\",\n\
                     \"text_secondary\":   \"#a5adce\",\n\
                     \"accent\":           \"#caa6f7\",\n\
                     \"accent_secondary\": \"#f5bde2\",\n\
                     \"success\":          \"#a6da95\",\n\
                     \"warning\":          \"#eed49f\",\n\
                     \"error\":            \"#ed8796\",\n\
                     \"border\":           \"#45475a\",\n\
                     \"header_bg\":        \"#181826\",\n\
                     \"thinking\":         \"#8bd5ca\"\n\
                   }}\n\
                 }}\n\n\
                 Then run:  /theme import <file.json>"
            )
        }
        other => format!(
            "Unknown theme action: {other}\n\n  \
             /theme              Show current theme\n  \
             /theme list         List themes\n  \
             /theme set <n>      Apply a theme\n  \
             /theme reset        Reset to default\n  \
             /theme create <n>   Show template for a custom theme\n  \
             /theme import <f>   Import theme from JSON file\n  \
             /theme export [f]   Export current theme to JSON file"
        ),
    }
}

pub(crate) fn status_context(
    session_path: Option<&Path>,
) -> Result<StatusContext, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered_config_files = loader.discover().len();
    let runtime_config = loader.load()?;
    let project_context = ProjectContext::discover_with_git(&cwd, DEFAULT_DATE)?;
    let (project_root, git_branch) =
        parse_git_status_metadata(project_context.git_status.as_deref());
    Ok(StatusContext {
        cwd,
        session_path: session_path.map(Path::to_path_buf),
        loaded_config_files: runtime_config.loaded_entries().len(),
        discovered_config_files,
        memory_file_count: project_context.instruction_files.len(),
        project_root,
        git_branch,
    })
}

pub(crate) fn format_status_report(
    model: &str,
    usage: StatusUsage,
    permission_mode: &str,
    context: &StatusContext,
) -> String {
    [
        format!(
            "Session
  Model            {model}
  Permissions      {permission_mode}
  Activity         {} messages · {} turns
  Tokens           est {} · latest {} · total {}",
            usage.message_count,
            usage.turns,
            usage.estimated_tokens,
            usage.latest.total_tokens(),
            usage.cumulative.total_tokens(),
        ),
        format!(
            "Usage
  Cumulative input {}
  Cumulative output {}
  Cache create     {}
  Cache read       {}",
            usage.cumulative.input_tokens,
            usage.cumulative.output_tokens,
            usage.cumulative.cache_creation_input_tokens,
            usage.cumulative.cache_read_input_tokens,
        ),
        format!(
            "Workspace
  Folder           {}
  Project root     {}
  Git branch       {}
  Session file     {}
  Config files     loaded {}/{}
  Memory files     {}

Next
  /help            Browse commands
  /session list    Inspect saved sessions
  /diff            Review current workspace changes",
            context.cwd.display(),
            context
                .project_root
                .as_ref()
                .map_or_else(|| "unknown".to_string(), |path| path.display().to_string()),
            context.git_branch.as_deref().unwrap_or("unknown"),
            context.session_path.as_ref().map_or_else(
                || "live-repl".to_string(),
                |path| path.display().to_string()
            ),
            context.loaded_config_files,
            context.discovered_config_files,
            context.memory_file_count,
        ),
    ]
    .join(
        "

",
    )
}

pub(crate) fn render_config_report(section: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered = loader.discover();
    let runtime_config = loader.load()?;

    let mut lines = vec![
        format!(
            "Config
  Working directory {}
  Loaded files      {}
  Merged keys       {}",
            cwd.display(),
            runtime_config.loaded_entries().len(),
            runtime_config.merged().len()
        ),
        "Discovered files".to_string(),
    ];
    for entry in discovered {
        let source = match entry.source {
            ConfigSource::User => "user",
            ConfigSource::Project => "project",
            ConfigSource::Local => "local",
        };
        let status = if runtime_config
            .loaded_entries()
            .iter()
            .any(|loaded_entry| loaded_entry.path == entry.path)
        {
            "loaded"
        } else {
            "missing"
        };
        lines.push(format!(
            "  {source:<7} {status:<7} {}",
            entry.path.display()
        ));
    }

    if let Some(section) = section {
        lines.push(format!("Merged section: {section}"));
        let value = match section {
            "env" => runtime_config.get("env"),
            "hooks" => runtime_config.get("hooks"),
            "model" => runtime_config.get("model"),
            "plugins" => runtime_config
                .get("plugins")
                .or_else(|| runtime_config.get("enabledPlugins")),
            other => {
                lines.push(format!(
                    "  Unsupported config section '{other}'. Use env, hooks, model, or plugins."
                ));
                return Ok(lines.join(
                    "
",
                ));
            }
        };
        lines.push(format!(
            "  {}",
            match value {
                Some(value) => value.render(),
                None => "<unset>".to_string(),
            }
        ));
        return Ok(lines.join(
            "
",
        ));
    }

    lines.push("Merged JSON".to_string());
    lines.push(format!("  {}", runtime_config.as_json().render()));
    Ok(lines.join(
        "
",
    ))
}

/// Parse a CLI-style boolean token (case-insensitive) into a real `bool`.
///
/// Accepted tokens: `on`/`off`, `true`/`false`, `yes`/`no`, `1`/`0`. Anything
/// else returns `Err` so callers can surface a usage hint instead of
/// silently treating unknown input as `false`.
///
/// Task #623 (v2.2.14 Phase 1): used by `/config tui_mouse_capture <on|off>`.
pub(crate) fn parse_cli_bool(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "enable" | "enabled" => Ok(true),
        "off" | "false" | "no" | "0" | "disable" | "disabled" => Ok(false),
        other => Err(format!(
            "expected on/off (also true/false, yes/no, 1/0); got {other:?}"
        )),
    }
}

/// Write a boolean value into `~/.anvil/config.json` under `key`, preserving
/// every other field already in the file.
///
/// Returns the parsed boolean on success so the caller can echo it back to
/// the user with the canonical token. Used by the live TUI's `/config`
/// write path (task #623 / v2.2.14 Phase 1).
pub(crate) fn set_config_bool_value(key: &str, raw_value: &str) -> Result<bool, String> {
    let parsed = parse_cli_bool(raw_value)?;
    let home = dirs_next_home().ok_or_else(|| "could not determine $HOME".to_string())?;
    let anvil_dir = home.join(".anvil");
    std::fs::create_dir_all(&anvil_dir)
        .map_err(|e| format!("could not create {}: {e}", anvil_dir.display()))?;
    let config_path = anvil_dir.join("config.json");

    let mut existing: serde_json::Map<String, serde_json::Value> = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("could not read {}: {e}", config_path.display()))?;
        if raw.trim().is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!("config.json is not valid JSON: {e}"))?
                .as_object()
                .cloned()
                .ok_or_else(|| "config.json root must be an object".to_string())?
        }
    } else {
        serde_json::Map::new()
    };
    existing.insert(key.to_string(), serde_json::Value::Bool(parsed));
    let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(existing))
        .map_err(|e| format!("could not serialise config.json: {e}"))?;
    std::fs::write(&config_path, serialized)
        .map_err(|e| format!("could not write {}: {e}", config_path.display()))?;
    Ok(parsed)
}

pub(crate) fn render_memory_report() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let project_context = ProjectContext::discover(&cwd, DEFAULT_DATE)?;
    let memory_mgr = MemoryManager::new(&cwd);
    let memory_files = memory_mgr.discover();

    let mut lines = vec![format!(
        "Memory
  Working directory {}
  Instruction files {}
  Persistent memory files {}",
        cwd.display(),
        project_context.instruction_files.len(),
        memory_files.len(),
    )];

    lines.push("Instruction files".to_string());
    if project_context.instruction_files.is_empty() {
        lines.push(
            "  No ANVIL instruction files discovered in the current directory ancestry.".to_string(),
        );
    } else {
        for (index, file) in project_context.instruction_files.iter().enumerate() {
            let preview = file.content.lines().next().unwrap_or("").trim();
            let preview = if preview.is_empty() {
                "<empty>"
            } else {
                preview
            };
            lines.push(format!("  {}. {}", index + 1, file.path.display()));
            lines.push(format!(
                "     lines={} preview={}",
                file.content.lines().count(),
                preview
            ));
        }
    }

    lines.push("Persistent memory".to_string());
    lines.push(format!("  Directory  {}", memory_mgr.memory_dir().display()));
    if memory_files.is_empty() {
        lines.push("  No persistent memory files saved for this project.".to_string());
    } else {
        for (index, file) in memory_files.iter().enumerate() {
            lines.push(format!(
                "  {}. {} ({})",
                index + 1,
                file.name,
                file.memory_type
            ));
            lines.push(format!("     {}", file.description));
        }
    }

    Ok(lines.join("
"))
}

pub(crate) fn init_anvil_md() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    Ok(initialize_repo(&cwd)?.render())
}

/// `anvil --init` entry point.  Task #626 SAFE-HEADLESS:
/// `CliAction::Init` runs before the TUI ever starts.
#[allow(clippy::print_stdout, reason = "headless `anvil --init` subcommand; TUI never active")]
pub(crate) fn run_init() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", init_anvil_md()?);
    Ok(())
}

pub(crate) fn normalize_permission_mode(mode: &str) -> Option<&'static str> {
    match mode.trim() {
        "read-only" => Some("read-only"),
        "workspace-write" => Some("workspace-write"),
        "danger-full-access" => Some("danger-full-access"),
        _ => None,
    }
}

pub(crate) fn render_diff_report() -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["diff", "--", ":(exclude).omx"])
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git diff failed: {stderr}").into());
    }
    let diff = String::from_utf8(output.stdout)?;
    if diff.trim().is_empty() {
        return Ok(
            "Diff\n  Result           clean working tree\n  Detail           no current changes"
                .to_string(),
        );
    }
    Ok(format!("Diff\n\n{}", diff.trim_end()))
}

pub(crate) fn render_teleport_report(target: &str) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;

    let file_list = Command::new("rg")
        .args(["--files"])
        .current_dir(&cwd)
        .output()?;
    let file_matches = if file_list.status.success() {
        String::from_utf8(file_list.stdout)?
            .lines()
            .filter(|line| line.contains(target))
            .take(10)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let content_output = Command::new("rg")
        .args(["-n", "-S", "--color", "never", target, "."])
        .current_dir(&cwd)
        .output()?;

    let mut lines = vec![format!("Teleport\n  Target           {target}")];
    if !file_matches.is_empty() {
        lines.push(String::new());
        lines.push("File matches".to_string());
        lines.extend(file_matches.into_iter().map(|path| format!("  {path}")));
    }

    if content_output.status.success() {
        let matches = String::from_utf8(content_output.stdout)?;
        if !matches.trim().is_empty() {
            lines.push(String::new());
            lines.push("Content matches".to_string());
            lines.push(truncate_for_prompt(&matches, 4_000));
        }
    }

    if lines.len() == 1 {
        lines.push("  Result           no matches found".to_string());
    }

    Ok(lines.join("\n"))
}

pub(crate) fn render_last_tool_debug_report(session: &Session) -> Result<String, Box<dyn std::error::Error>> {
    let last_tool_use = session
        .messages
        .iter()
        .rev()
        .find_map(|message| {
            message.blocks.iter().rev().find_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
        })
        .ok_or_else(|| "no prior tool call found in session".to_string())?;

    let tool_result = session.messages.iter().rev().find_map(|message| {
        message.blocks.iter().rev().find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } if tool_use_id == &last_tool_use.0 => {
                Some((tool_name.clone(), output.clone(), *is_error))
            }
            _ => None,
        })
    });

    let mut lines = vec![
        "Debug tool call".to_string(),
        format!("  Tool id          {}", last_tool_use.0),
        format!("  Tool name        {}", last_tool_use.1),
        "  Input".to_string(),
        indent_block(&last_tool_use.2, 4),
    ];

    match tool_result {
        Some((tool_name, output, is_error)) => {
            lines.push("  Result".to_string());
            lines.push(format!("    name           {tool_name}"));
            lines.push(format!(
                "    status         {}",
                if is_error { "error" } else { "ok" }
            ));
            lines.push(indent_block(&output, 4));
        }
        None => lines.push("  Result           missing tool result".to_string()),
    }

    Ok(lines.join("\n"))
}

pub(crate) fn indent_block(value: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn git_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub(crate) fn git_status_ok(args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(())
}

/// Returns `true` when `name` resolves to an executable on the current PATH.
///
/// Uses a cross-platform PATH walk: on Windows each directory entry is also
/// checked with the extensions listed in `PATHEXT` (e.g. `.exe`, `.cmd`).
/// Unlike the Unix-only `which` shell built-in, this works on every platform.
pub(crate) fn command_exists(name: &str) -> bool {
    let path_os = match std::env::var_os("PATH") {
        Some(v) => v,
        None => return false,
    };
    for dir in std::env::split_paths(&path_os) {
        if dir.join(name).is_file() {
            return true;
        }
        #[cfg(target_os = "windows")]
        if let Ok(pathext) = std::env::var("PATHEXT") {
            for ext in pathext.split(';') {
                let candidate = dir.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

/// Return the path to `~/.anvil/pinned.json`, creating `~/.anvil/` if needed.
pub(crate) fn anvil_pinned_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs_next_home().ok_or("could not determine home directory")?;
    let dir = home.join(".anvil");
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    Ok(dir.join("pinned.json"))
}

/// Portable home directory lookup — returns the platform home directory.
///
/// Uses `dirs_next::home_dir()` which resolves `$HOME` on Unix and
/// `%USERPROFILE%` on Windows.
pub(crate) fn dirs_next_home() -> Option<PathBuf> {
    dirs_next::home_dir()
}

/// Load pinned paths from `~/.anvil/pinned.json`.  Returns an empty vec if
/// the file does not exist yet.
pub(crate) fn load_pinned_paths(path: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    let strings: Vec<String> = serde_json::from_str(&raw)?;
    Ok(strings.into_iter().map(PathBuf::from).collect())
}

/// Persist pinned paths to `~/.anvil/pinned.json`.
pub(crate) fn save_pinned_paths(path: &Path, paths: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    let strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let json = serde_json::to_string_pretty(&strings)?;
    fs::write(path, json)?;
    Ok(())
}

/// Format a large number with commas: 1000000 → "1,000,000".
pub(crate) fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Parse a human token count like "200K", "1M", "2M", "500000" into a `u64`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn parse_token_count(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        rest.trim().parse::<f64>().ok().map(|f| (f * 1_000_000.0) as u64)
    } else if let Some(rest) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
        rest.trim().parse::<f64>().ok().map(|f| (f * 1_000.0) as u64)
    } else {
        s.parse::<u64>().ok()
    }
}

pub(crate) fn write_temp_text_file(
    filename: &str,
    contents: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::temp_dir().join(filename);
    fs::write(&path, contents)?;
    Ok(path)
}

pub(crate) fn recent_user_context(session: &Session, limit: usize) -> String {
    let requests = session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .filter_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
        })
        .rev()
        .take(limit)
        .collect::<Vec<_>>();

    if requests.is_empty() {
        "<no prior user messages>".to_string()
    } else {
        requests
            .into_iter()
            .rev()
            .enumerate()
            .map(|(index, text)| format!("{}. {}", index + 1, text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Minimal POSIX single-quote escaping for shell export instructions.
pub(crate) fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub(crate) fn truncate_for_prompt(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.trim().to_string()
    } else {
        let truncated = value.chars().take(limit).collect::<String>();
        format!("{}\n…[truncated]", truncated.trim_end())
    }
}

/// Escape a string for use as a literal in a regex pattern.
pub(crate) fn regex_escape(s: &str) -> String {
    let special = r"\.+*?()|[]{}^$#";
    s.chars()
        .flat_map(|c| {
            if special.contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}


// ─── Feature 5 helpers ────────────────────────────────────────────────────────

/// Detect the project type and run its test suite.
pub(crate) fn run_test_suite(coverage: bool) -> String {
    let cwd = env::current_dir().unwrap_or_default();

    let (cmd, args): (&str, Vec<String>) = if cwd.join("Cargo.toml").exists() {
        if coverage {
            (
                "cargo",
                vec![
                    "llvm-cov".to_string(),
                    "--text".to_string(),
                    "--ignore-filename-regex".to_string(),
                    "tests/".to_string(),
                ],
            )
        } else {
            ("cargo", vec!["test".to_string()])
        }
    } else if cwd.join("package.json").exists() {
        if coverage {
            (
                "npx",
                vec![
                    "vitest".to_string(),
                    "run".to_string(),
                    "--coverage".to_string(),
                ],
            )
        } else {
            (
                "npm",
                vec![
                    "test".to_string(),
                    "--".to_string(),
                    "--passWithNoTests".to_string(),
                ],
            )
        }
    } else if cwd.join("pyproject.toml").exists() || cwd.join("setup.py").exists() {
        if coverage {
            (
                "python",
                vec![
                    "-m".to_string(),
                    "pytest".to_string(),
                    "--cov".to_string(),
                    "--cov-report=term-missing".to_string(),
                ],
            )
        } else {
            ("python", vec!["-m".to_string(), "pytest".to_string()])
        }
    } else if cwd.join("go.mod").exists() {
        if coverage {
            (
                "go",
                vec!["test".to_string(), "./...".to_string(), "-cover".to_string()],
            )
        } else {
            ("go", vec!["test".to_string(), "./...".to_string()])
        }
    } else {
        return "Could not detect project type (no Cargo.toml, package.json, pyproject.toml, or go.mod found).".to_string();
    };

    let out = Command::new(cmd).args(&args).current_dir(&cwd).output();

    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let log = truncate_for_prompt(
                &[stdout.as_str(), stderr_text.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n"),
                6_000,
            );
            if o.status.success() {
                format!("Tests passed.\n\n{log}")
            } else {
                format!("Tests failed (exit {}).\n\n{log}", o.status)
            }
        }
        Err(e) => format!("Cannot run {cmd}: {e}"),
    }
}

// ─── Feature 6 helpers ────────────────────────────────────────────────────────

pub(crate) fn run_git_stash_list() -> String {
    match git_output(&["stash", "list"]) {
        Ok(s) if s.is_empty() => "Stash is empty.".to_string(),
        Ok(s) => s,
        Err(e) => format!("git stash list failed: {e}"),
    }
}

pub(crate) fn run_git_stash_op(args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(env::current_dir().unwrap_or_default())
        .output();
    match out {
        Ok(o) => {
            let out_text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let err_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if o.status.success() {
                if out_text.is_empty() {
                    err_text
                } else {
                    out_text
                }
            } else {
                let msg = if err_text.is_empty() { out_text } else { err_text };
                format!("git {} failed: {msg}", args.join(" "))
            }
        }
        Err(e) => format!("git {} failed: {e}", args.join(" ")),
    }
}

// ─── Feature 7 helpers ────────────────────────────────────────────────────────

/// Parse a line range like "10-25" or "10" into (start, end).  end=0 means open-ended.
pub(crate) fn parse_line_range(s: &str) -> (usize, usize) {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('-') {
        let start = a.trim().parse().unwrap_or(1);
        let end = b.trim().parse().unwrap_or(0);
        (start, end)
    } else {
        let n = s.parse().unwrap_or(1);
        (n, n)
    }
}


pub(crate) fn sanitize_generated_message(value: &str) -> String {
    value.trim().trim_matches('`').trim().replace("\r\n", "\n")
}

pub(crate) fn parse_titled_body(value: &str) -> Option<(String, String)> {
    let normalized = sanitize_generated_message(value);
    let title = normalized
        .lines()
        .find_map(|line| line.strip_prefix("TITLE:").map(str::trim))?;
    let body_start = normalized.find("BODY:")?;
    let body = normalized[body_start + "BODY:".len()..].trim();
    Some((title.to_string(), body.to_string()))
}

pub(crate) fn render_version_report() -> String {
    let git_sha = GIT_SHA;
    let target = BUILD_TARGET;
    format!(
        "Anvil CLI\n  Version          {VERSION}\n  Git SHA          {git_sha}\n  Target           {target}\n  Build date       {DEFAULT_DATE}\n\nSupport\n  Help             anvil --help\n  REPL             /help"
    )
}

/// Render a session as clean, readable Markdown with proper formatting.
pub(crate) fn render_export_markdown(session: &Session) -> String {
    let mut out = String::from("# Anvil Session Export\n\n");
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut turn = 0usize;

    for message in &session.messages {
        if let Some(ref u) = message.usage {
            total_input += u64::from(u.input_tokens);
            total_output += u64::from(u.output_tokens);
        }
        match message.role {
            MessageRole::System => continue, // skip system prompts
            MessageRole::User => {
                turn += 1;
                out.push_str(&format!("---\n\n### Turn {turn} — User\n\n"));
                for block in &message.blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            out.push_str(text);
                            out.push_str("\n\n");
                        }
                        ContentBlock::Image { media_type, data } => {
                            out.push_str(&format!("*[Image: {media_type}, {} bytes]*\n\n", data.len()));
                        }
                        _ => {}
                    }
                }
            }
            MessageRole::Assistant => {
                out.push_str("### Assistant\n\n");
                for block in &message.blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            out.push_str(text);
                            out.push_str("\n\n");
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            out.push_str(&format!("> **Tool: {name}**\n"));
                            // Show first 200 chars of input as context
                            let preview: String = input.chars().take(200).collect();
                            if !preview.is_empty() {
                                out.push_str(&format!("> ```\n> {}\n> ```\n", preview.replace('\n', "\n> ")));
                            }
                            out.push('\n');
                        }
                        _ => {}
                    }
                }
            }
            MessageRole::Tool => {
                for block in &message.blocks {
                    if let ContentBlock::ToolResult { tool_name, output, is_error, .. } = block {
                        let status = if *is_error { "error" } else { "ok" };
                        let preview: String = output.lines().take(5).collect::<Vec<_>>().join("\n");
                        out.push_str(&format!("> **Result ({tool_name})** [{status}]\n"));
                        if !preview.is_empty() {
                            out.push_str(&format!("> ```\n> {}\n> ```\n", preview.replace('\n', "\n> ")));
                        }
                        out.push('\n');
                    }
                }
            }
        }
    }

    out.push_str("---\n\n");
    out.push_str(&format!(
        "**Session summary:** {} turns, {} messages, {} input tokens, {} output tokens\n",
        turn,
        session.messages.len(),
        total_input,
        total_output,
    ));
    out
}

pub(crate) fn render_export_text(session: &Session) -> String {
    let mut lines = vec!["# Conversation Export".to_string(), String::new()];
    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        lines.push(format!("## {}. {role}", index + 1));
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => lines.push(text.clone()),
                ContentBlock::Image { media_type, data } => {
                    lines.push(format!("[image {media_type} {} bytes]", data.len()));
                }
                ContentBlock::Document {
                    media_type, data, title, ..
                } => {
                    let name = title.as_deref().unwrap_or("document");
                    lines.push(format!(
                        "[document {name} {media_type} {} bytes]",
                        data.len()
                    ));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    lines.push(format!("[tool_use id={id} name={name}] {input}"));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    tool_name,
                    output,
                    is_error,
                } => {
                    lines.push(format!(
                        "[tool_result id={tool_use_id} name={tool_name} error={is_error}] {output}"
                    ));
                }
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

pub(crate) fn default_export_filename(session: &Session) -> String {
    let stem = session
        .messages
        .iter()
        .find_map(|message| match message.role {
            MessageRole::User => message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .map_or("conversation", |text| {
            text.lines().next().unwrap_or("conversation")
        })
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    let fallback = if stem.is_empty() {
        "conversation"
    } else {
        &stem
    };
    format!("{fallback}.txt")
}

pub(crate) fn resolve_export_path(
    requested_path: Option<&str>,
    session: &Session,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let file_name =
        requested_path.map_or_else(|| default_export_filename(session), ToOwned::to_owned);
    let final_name = if Path::new(&file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        file_name
    } else {
        format!("{file_name}.txt")
    };
    Ok(cwd.join(final_name))
}

pub(crate) fn build_system_prompt() -> Result<Vec<runtime::PromptSection>, Box<dyn std::error::Error>> {
    build_system_prompt_with_identity(None, None, None)
}

/// Best-effort friendly label for the provider serving a given model name.
/// Returns None when the model name doesn't match a known provider pattern.
pub(crate) fn friendly_provider_label(model: &str) -> Option<String> {
    let kind = api::detect_provider_kind(model);
    use api::ProviderKind;
    match kind {
        ProviderKind::AnvilApi => Some("Anthropic (via Anvil API)".to_string()),
        ProviderKind::OpenAi => Some("OpenAI".to_string()),
        ProviderKind::Gemini => Some("Gemini".to_string()),
        ProviderKind::Xai => Some("xAI".to_string()),
        ProviderKind::Ollama => {
            if model.ends_with(":cloud") || model.contains("-cloud") {
                Some("Ollama Cloud".to_string())
            } else {
                Some("Ollama (local)".to_string())
            }
        }
        other => Some(api::provider_display_name(other).to_string()),
    }
}

/// Like `build_system_prompt`, but threads the active model name, provider
/// label, and tab id into the runtime's environment-context block so the agent
/// can correctly answer "what version are you?" / "what model are you running?".
///
/// Returns the typed [`runtime::PromptSection`] vector. The wire format is
/// projected at the API boundary in `crates/anvil-cli/src/providers.rs`; in
/// memory we keep the typed representation so that fast-mode toggle, output-style
/// switching, `/skill load`, and `/goal` can identify their sections by kind
/// rather than scanning for inline markers.
pub(crate) fn build_system_prompt_with_identity(
    model_name: Option<String>,
    provider_name: Option<String>,
    tab_id: Option<String>,
) -> Result<Vec<runtime::PromptSection>, Box<dyn std::error::Error>> {
    use runtime::{PromptSection, PromptSectionKind, PromptSectionsExt};
    let cwd = env::current_dir()?;
    let mut sections = runtime::load_system_prompt_sections_with_identity(
        cwd.clone(),
        DEFAULT_DATE,
        env::consts::OS,
        "unknown",
        model_name,
        provider_name,
        tab_id,
    )?;

    // Prepend the active-goal fragment when a goal is active for this project.
    // upsert_by_kind() on `Goal` prepends to position 0 when no Goal exists
    // yet (matching the legacy `insert(0, ...)` behavior); re-running with a
    // changed goal body replaces in place rather than stacking duplicates.
    let mgr = runtime::GoalManager::new(cwd);
    if let Ok(Some(goal)) = mgr.active_goal() {
        let fragment = runtime::build_active_goal_prompt_fragment(&goal);
        sections.upsert_by_kind(PromptSection::new(PromptSectionKind::Goal, fragment));
    }

    Ok(sections)
}

// ─── Task #567: NO_COLOR / FORCE_COLOR must not strip TUI colors ─────────
#[cfg(test)]
#[allow(unsafe_code)] // env::set_var/remove_var require unsafe in edition 2024
mod color_env_tests {
    use super::should_use_color;
    use serial_test::serial;

    /// Helper that scopes env mutations to this test only.
    fn with_env<F: FnOnce()>(no_color: Option<&str>, force_color: Option<&str>, f: F) {
        let prev_no = std::env::var("NO_COLOR").ok();
        let prev_force = std::env::var("FORCE_COLOR").ok();
        unsafe {
            match no_color {
                Some(v) => std::env::set_var("NO_COLOR", v),
                None => std::env::remove_var("NO_COLOR"),
            }
            match force_color {
                Some(v) => std::env::set_var("FORCE_COLOR", v),
                None => std::env::remove_var("FORCE_COLOR"),
            }
        }
        f();
        unsafe {
            match prev_no {
                Some(v) => std::env::set_var("NO_COLOR", v),
                None => std::env::remove_var("NO_COLOR"),
            }
            match prev_force {
                Some(v) => std::env::set_var("FORCE_COLOR", v),
                None => std::env::remove_var("FORCE_COLOR"),
            }
        }
    }

    #[test]
    #[serial]
    fn tui_always_keeps_color_regardless_of_no_color() {
        with_env(Some("1"), None, || {
            assert!(should_use_color(true), "TUI must keep color even with NO_COLOR=1");
        });
    }

    #[test]
    #[serial]
    fn tui_always_keeps_color_regardless_of_force_color_zero() {
        with_env(None, Some("0"), || {
            assert!(should_use_color(true), "TUI must keep color even with FORCE_COLOR=0");
        });
    }

    #[test]
    #[serial]
    fn headless_strips_color_under_no_color() {
        with_env(Some("1"), None, || {
            assert!(!should_use_color(false), "headless must respect NO_COLOR=1");
        });
    }

    #[test]
    #[serial]
    fn headless_strips_color_under_force_color_zero() {
        with_env(None, Some("0"), || {
            assert!(!should_use_color(false), "headless must respect FORCE_COLOR=0");
        });
    }

    #[test]
    #[serial]
    fn headless_default_keeps_color() {
        with_env(None, None, || {
            assert!(should_use_color(false), "default headless must keep color");
        });
    }

    #[test]
    #[serial]
    fn force_color_one_overrides_no_color() {
        // FORCE_COLOR is checked first so explicit force wins.
        with_env(Some("1"), Some("1"), || {
            assert!(should_use_color(false), "FORCE_COLOR=1 must beat NO_COLOR=1");
        });
    }
}

// ─── Phase A5 (task #645): SUPPORTED_LANGUAGES drift gate ────────────────
//
// Two regression gates that catch the exact failure mode that occurred
// on v2.2.19 commit 854a23c: the translator agent shipped pt-BR.yml
// but the SUPPORTED list in `run_language_command_static` was not
// updated, so /language pt-BR rejected the code as unsupported even
// though every key was already translated.
//
// 1. `supported_languages_match_locale_files` — every entry in
//    `SUPPORTED_LANGUAGES` must have a sibling `locales/<code>.yml`,
//    and every `.yml` under `locales/` must appear in the constant.
//    Both directions are enforced so neither "translated but
//    unreachable" (the 854a23c bug) nor "advertised but missing
//    translations" (silent fall-back to en) can slip through.
//
// 2. `current_language_code_falls_back_to_en` — the boot-time loader
//    must return `"en"` when config.json is missing, malformed, or
//    lacks the `language` field, NOT panic.  Any panic here would
//    crash `anvil` before the wizard could even start.
#[cfg(test)]
#[allow(unsafe_code)] // env::set_var/remove_var require unsafe in edition 2024
mod supported_languages_drift_tests {
    use super::{current_language_code, SUPPORTED_LANGUAGES};
    use serial_test::serial;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    /// Resolve the workspace `locales/` directory from the crate's
    /// CARGO_MANIFEST_DIR (which points at `crates/anvil-cli/`).
    fn locales_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../locales")
            .canonicalize()
            .expect("locales/ directory should exist at workspace root")
    }

    #[test]
    fn supported_languages_match_locale_files() {
        let dir = locales_dir();
        let mut yaml_codes: BTreeSet<String> = BTreeSet::new();
        for entry in fs::read_dir(&dir).expect("read locales/") {
            let entry = entry.expect("read locales/ entry");
            let path = entry.path();
            // Accept .yml only (we don't ship .yaml today; if that
            // changes, this gate will flag it intentionally so the
            // implementer adds the dual-extension rule consciously).
            if path.extension().and_then(|s| s.to_str()) == Some("yml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    yaml_codes.insert(stem.to_string());
                }
            }
        }

        let advertised: BTreeSet<String> = SUPPORTED_LANGUAGES
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Forward: every advertised code must have a YAML file.
        let missing_yaml: Vec<&String> = advertised.difference(&yaml_codes).collect();
        assert!(
            missing_yaml.is_empty(),
            "SUPPORTED_LANGUAGES advertises codes with no locales/<code>.yml: {missing_yaml:?}"
        );

        // Reverse: every YAML file must be advertised in SUPPORTED_LANGUAGES.
        // This catches the v2.2.19 854a23c bug (pt-BR.yml landed without
        // SUPPORTED being updated).
        let orphan_yaml: Vec<&String> = yaml_codes.difference(&advertised).collect();
        assert!(
            orphan_yaml.is_empty(),
            "locales/ has YAML files not advertised in SUPPORTED_LANGUAGES: {orphan_yaml:?}"
        );
    }

    /// Scope an env override + temp ANVIL_CONFIG_HOME to one test.
    /// Restores the prior environment on drop so neighbouring tests
    /// (which also touch `ANVIL_CONFIG_HOME` via `#[serial(anvil_config_home)]`)
    /// see a clean slate.
    fn with_anvil_home<F: FnOnce(&std::path::Path)>(label: &str, f: F) {
        let tmp = std::env::temp_dir().join(format!(
            "anvil-i18n-fallback-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create temp anvil home");
        let prev = std::env::var("ANVIL_CONFIG_HOME").ok();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", &tmp);
        }
        f(&tmp);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ANVIL_CONFIG_HOME", v),
                None => std::env::remove_var("ANVIL_CONFIG_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn current_language_code_falls_back_when_config_missing() {
        with_anvil_home("missing", |_dir| {
            assert_eq!(current_language_code(), "en");
        });
    }

    #[test]
    #[serial(anvil_config_home)]
    fn current_language_code_falls_back_when_language_absent() {
        with_anvil_home("no-field", |dir| {
            let path = dir.join("config.json");
            std::fs::write(&path, r#"{"default_model": "claude-opus-4-6"}"#)
                .expect("seed config.json");
            assert_eq!(current_language_code(), "en");
        });
    }

    #[test]
    #[serial(anvil_config_home)]
    fn current_language_code_returns_persisted_value() {
        with_anvil_home("persisted", |dir| {
            let path = dir.join("config.json");
            std::fs::write(&path, r#"{"language": "pt-BR"}"#).expect("seed config.json");
            // The loader does NOT validate against SUPPORTED_LANGUAGES —
            // it simply returns the persisted string.  The /language
            // command and the wizard picker are the validation choke
            // points (drift gate above ensures pt-BR is reachable).
            assert_eq!(current_language_code(), "pt-BR");
        });
    }

    #[test]
    #[serial(anvil_config_home)]
    fn current_language_code_falls_back_on_malformed_json() {
        with_anvil_home("malformed", |dir| {
            let path = dir.join("config.json");
            std::fs::write(&path, "this is not json {").expect("seed config.json");
            assert_eq!(current_language_code(), "en");
        });
    }

    // ─── Phase A6 (task #645): save_language round-trip ──────────────
    //
    // The single persistence path used by /language, the wizard, and
    // /configure must:
    //   1. Write `language: <code>` to config.json (preserving other keys),
    //   2. Apply the locale to rust_i18n,
    //   3. Be readable back by `current_language_code`.
    //
    // The wizard test in `wizard.rs` exercises the modal path on top of
    // this; the configure-menu test in tests.rs exercises the
    // ConfigureAction::SetLanguage dispatch.

    #[test]
    #[serial(anvil_config_home)]
    fn save_language_writes_config_and_applies_locale() {
        with_anvil_home("save-roundtrip", |dir| {
            // Seed an existing key to verify preserve-other-keys semantics.
            let path = dir.join("config.json");
            std::fs::write(&path, r#"{"default_model": "claude-opus-4-6"}"#)
                .expect("seed config.json");

            super::save_language("pt-BR").expect("save_language pt-BR");

            // Re-read and assert both fields survived.
            let data = std::fs::read_to_string(&path).expect("read config.json");
            let value: serde_json::Value =
                serde_json::from_str(&data).expect("parse config.json");
            assert_eq!(
                value.get("language").and_then(|v| v.as_str()),
                Some("pt-BR")
            );
            assert_eq!(
                value.get("default_model").and_then(|v| v.as_str()),
                Some("claude-opus-4-6")
            );
            assert_eq!(super::current_language_code(), "pt-BR");
            assert_eq!(rust_i18n::locale().to_string(), "pt-BR");
        });
    }

    #[test]
    #[serial(anvil_config_home)]
    fn save_language_creates_config_when_missing() {
        with_anvil_home("save-fresh", |dir| {
            let path = dir.join("config.json");
            assert!(!path.exists(), "config.json should be absent for this case");

            super::save_language("ja").expect("save_language ja");

            assert!(path.exists(), "save_language must create config.json");
            assert_eq!(super::current_language_code(), "ja");
        });
    }
}

