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
pub(crate) fn anvil_home_dir() -> PathBuf {
    if let Ok(config_home) = std::env::var("ANVIL_CONFIG_HOME") {
        return PathBuf::from(config_home);
    }
    std::env::var("HOME").map_or_else(|_| std::env::current_dir().unwrap_or_default(), PathBuf::from)
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



pub(crate) fn run_language_command_static(lang: Option<&str>) -> String {
    const SUPPORTED: &[&str] = &["en", "de", "es", "fr", "ja", "zh-CN", "ru"];

    let Some(lang) = lang else {
        let current = current_language_code();
        return format!(
            "Language: {current}\nAvailable: {}\nUsage: /language <code>",
            SUPPORTED.join(", ")
        );
    };

    let lang = lang.trim();
    if lang.is_empty() {
        return format!(
            "Language: {}\nAvailable: {}\nUsage: /language <code>",
            current_language_code(),
            SUPPORTED.join(", ")
        );
    }

    if !SUPPORTED.contains(&lang) {
        return format!(
            "Unsupported language '{lang}'. Available: {}",
            SUPPORTED.join(", ")
        );
    }

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
    map.insert("language".to_string(), serde_json::Value::String(lang.to_string()));

    let _ = fs::create_dir_all(&anvil_dir);
    match fs::write(&path, serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()) {
        Ok(()) => {
            rust_i18n::set_locale(lang);
            format!("Language set to: {lang}")
        }
        Err(e) => format!("Failed to save language setting: {e}"),
    }
}

/// Return the currently configured language code, defaulting to "en".
pub(crate) fn current_language_code() -> String {
    let path = anvil_home_dir().join("config.json");
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data) {
            if let Some(lang) = map.get("language").and_then(|v| v.as_str()) {
                return lang.to_string();
            }
        }
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

pub(crate) fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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

/// Portable home directory lookup (no external crate needed).
pub(crate) fn dirs_next_home() -> Option<PathBuf> {
    env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::var("USERPROFILE").ok().map(PathBuf::from))
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

pub(crate) fn build_system_prompt() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(load_system_prompt(
        env::current_dir()?,
        DEFAULT_DATE,
        env::consts::OS,
        "unknown",
    )?)
}



