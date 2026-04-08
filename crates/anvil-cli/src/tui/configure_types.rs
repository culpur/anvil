/// Configure mode types: ConfigureState, ConfigureAction, ConfigureData,
/// and helper functions for state navigation.
use super::helpers::{next_char_boundary, prev_char_boundary};

// ─── ConfigureState ───────────────────────────────────────────────────────────

/// Which screen the configure mode is showing.
#[derive(Debug, Clone, PartialEq)]
#[derive(Default)]
pub(super) enum ConfigureState {
    #[default]
    Inactive,
    MainMenu { selected: usize },
    Providers { selected: usize },
    ProviderDetail { provider: String, selected: usize },
    Models { selected: usize },
    Context { selected: usize },
    Search { selected: usize },
    Permissions { selected: usize },
    Display { selected: usize },
    Integrations { selected: usize },
    LanguageTheme { selected: usize },
    Vault { selected: usize },
    Notifications { selected: usize },
    Failover { selected: usize },
    Ssh { selected: usize },
    DockerK8s { selected: usize },
    Database { selected: usize },
    MemoryArchive { selected: usize },
    PluginsCron { selected: usize },
    /// Inline text input for editing a single value.
    EditingValue {
        section: String,
        key: String,
        value: String,
        cursor: usize,
    },
}

// ─── ConfigureAction ──────────────────────────────────────────────────────────

/// Actions that can be triggered from the interactive configure menu.
/// The main REPL loop in main.rs handles each variant.
#[derive(Debug, Clone)]
pub enum ConfigureAction {
    RefreshAnthropicOAuth,
    SetApiKey { provider: String, key: String },
    SetDefaultModel { model: String },
    SetImageModel { model: String },
    SetContextSize { size: u64 },
    SetCompactThreshold { pct: u8 },
    SetQmdEnabled { enabled: bool },
    SetSearchKey { provider: String, key: String },
    SetDefaultSearch { provider: String },
    ToggleVim,
    ToggleChat,
    SetPermissionMode { mode: String },
    SetOllamaHost { url: String },
    SetLanguage { lang: String },
    SetTheme { theme: String },
    SetVaultSessionTtl { secs: u64 },
    ToggleVaultAutoLock,
    SetNotifyPlatform { platform: String },
    SetNotifyValue { key: String, value: String },
    SetFailoverCooldown { secs: u64 },
    SetFailoverBudget { budget: u64 },
    ToggleFailoverAutoRecovery,
    SetSshKeyPath { path: String },
    SetSshBastionHost { host: String },
    SetSshConfigPath { path: String },
    SetDockerComposeFile { path: String },
    SetDockerRegistry { url: String },
    SetK8sContext { ctx: String },
    SetK8sNamespace { ns: String },
    SetDbUrl { url: String },
    SetDbSchemaTool { tool: String },
    ToggleAutoSaveMemory,
    SetArchiveFrequency { n: u64 },
    SetArchiveRetention { days: u64 },
    SetMemoryDir { path: String },
    ToggleAutoEnablePlugins,
    ToggleCronEnabled,
    SetPluginSearchPaths { paths: String },
}

// ─── ConfigureData ────────────────────────────────────────────────────────────

/// Snapshot of live configuration values.
#[derive(Debug, Clone, Default)]
pub struct ConfigureData {
    pub anthropic_status: String,
    pub openai_status: String,
    pub ollama_status: String,
    pub ollama_host: String,
    pub xai_status: String,
    pub current_model: String,
    pub default_model: String,
    pub image_model: String,
    pub failover_chain: Vec<String>,
    pub context_size: u64,
    pub compact_threshold: u8,
    pub qmd_status: String,
    pub history_count: usize,
    pub pinned_count: usize,
    pub default_search: String,
    pub search_providers: Vec<(String, bool, bool)>,
    pub vim_mode: bool,
    pub chat_mode: bool,
    pub permission_mode: String,
    pub screensaver_timeout_mins: u64,
    pub screensaver_enabled: bool,
    pub anvilhub_url: String,
    pub wp_configured: bool,
    pub github_configured: bool,
    pub language: String,
    pub active_theme: String,
    pub vault_session_ttl: u64,
    pub vault_auto_lock: bool,
    pub vault_status: String,
    pub notify_platform: String,
    pub notify_discord_webhook: String,
    pub notify_slack_webhook: String,
    pub notify_telegram_token: String,
    pub notify_whatsapp_url: String,
    pub notify_whatsapp_token: String,
    pub notify_matrix_homeserver: String,
    pub notify_matrix_token: String,
    pub notify_signal_sender: String,
    pub notify_signal_cli_path: String,
    pub failover_cooldown: u64,
    pub failover_budget: u64,
    pub failover_auto_recovery: bool,
    pub ssh_key_path: String,
    pub ssh_bastion_host: String,
    pub ssh_config_path: String,
    pub docker_compose_file: String,
    pub docker_registry: String,
    pub k8s_context: String,
    pub k8s_namespace: String,
    pub db_url: String,
    pub db_schema_tool: String,
    pub auto_save_memory: bool,
    pub archive_frequency: u64,
    pub archive_retention_days: u64,
    pub memory_dir: String,
    pub plugin_search_paths: String,
    pub auto_enable_plugins: bool,
    pub cron_enabled: bool,
    pub active_cron_jobs: Vec<String>,
}

// ─── Configure state helpers ──────────────────────────────────────────────────

/// Return the breadcrumb string shown in the footer while in configure mode.
pub(super) fn configure_breadcrumb(state: &ConfigureState) -> String {
    match state {
        ConfigureState::Inactive => String::new(),
        ConfigureState::MainMenu { .. } => "Configure".to_string(),
        ConfigureState::Providers { .. } => "Configure > Providers".to_string(),
        ConfigureState::ProviderDetail { provider, .. } => {
            let p = match provider.as_str() {
                "anthropic" => "Anthropic",
                "openai" => "OpenAI",
                "ollama" => "Ollama",
                "xai" => "xAI",
                other => other,
            };
            format!("Configure > Providers > {p}")
        }
        ConfigureState::Models { .. } => "Configure > Models".to_string(),
        ConfigureState::Context { .. } => "Configure > Context".to_string(),
        ConfigureState::Search { .. } => "Configure > Search".to_string(),
        ConfigureState::Permissions { .. } => "Configure > Permissions".to_string(),
        ConfigureState::Display { .. } => "Configure > Display".to_string(),
        ConfigureState::Integrations { .. } => "Configure > Integrations".to_string(),
        ConfigureState::LanguageTheme { .. } => "Configure > Language & Theme".to_string(),
        ConfigureState::Vault { .. } => "Configure > Vault".to_string(),
        ConfigureState::Notifications { .. } => "Configure > Notifications".to_string(),
        ConfigureState::Failover { .. } => "Configure > Failover".to_string(),
        ConfigureState::Ssh { .. } => "Configure > SSH".to_string(),
        ConfigureState::DockerK8s { .. } => "Configure > Docker & K8s".to_string(),
        ConfigureState::Database { .. } => "Configure > Database".to_string(),
        ConfigureState::MemoryArchive { .. } => "Configure > Memory & Archive".to_string(),
        ConfigureState::PluginsCron { .. } => "Configure > Plugins & Cron".to_string(),
        ConfigureState::EditingValue { section, key, .. } => {
            format!("Configure > {section} > edit:{key}")
        }
    }
}

/// Return the current selected index for a configure state.
pub(super) fn configure_selected(state: &ConfigureState) -> usize {
    match state {
        ConfigureState::MainMenu { selected }
        | ConfigureState::Providers { selected }
        | ConfigureState::Models { selected }
        | ConfigureState::Context { selected }
        | ConfigureState::Search { selected }
        | ConfigureState::Permissions { selected }
        | ConfigureState::Display { selected }
        | ConfigureState::Integrations { selected }
        | ConfigureState::LanguageTheme { selected }
        | ConfigureState::Vault { selected }
        | ConfigureState::Notifications { selected }
        | ConfigureState::Failover { selected }
        | ConfigureState::Ssh { selected }
        | ConfigureState::DockerK8s { selected }
        | ConfigureState::Database { selected }
        | ConfigureState::MemoryArchive { selected }
        | ConfigureState::PluginsCron { selected } => *selected,
        ConfigureState::ProviderDetail { selected, .. } => *selected,
        _ => 0,
    }
}

/// Update the selected index in a configure state.
pub(super) fn configure_set_selected(state: &mut ConfigureState, new: usize) {
    match state {
        ConfigureState::MainMenu { selected }
        | ConfigureState::Providers { selected }
        | ConfigureState::Models { selected }
        | ConfigureState::Context { selected }
        | ConfigureState::Search { selected }
        | ConfigureState::Permissions { selected }
        | ConfigureState::Display { selected }
        | ConfigureState::Integrations { selected }
        | ConfigureState::LanguageTheme { selected }
        | ConfigureState::Vault { selected }
        | ConfigureState::Notifications { selected }
        | ConfigureState::Failover { selected }
        | ConfigureState::Ssh { selected }
        | ConfigureState::DockerK8s { selected }
        | ConfigureState::Database { selected }
        | ConfigureState::MemoryArchive { selected }
        | ConfigureState::PluginsCron { selected } => *selected = new,
        ConfigureState::ProviderDetail { selected, .. } => *selected = new,
        _ => {}
    }
}

/// Return the number of navigable items for a given configure state.
pub(super) fn configure_item_count(state: &ConfigureState, data: &ConfigureData) -> usize {
    match state {
        ConfigureState::MainMenu { .. } => 16,
        ConfigureState::Providers { .. } => 4,
        ConfigureState::ProviderDetail { provider, .. } => match provider.as_str() {
            "anthropic" => 2,
            "openai" => 1,
            "ollama" => 1,
            "xai" => 1,
            _ => 0,
        },
        ConfigureState::Models { .. } => 2,
        ConfigureState::Context { .. } => 3,
        ConfigureState::Search { .. } => 1 + data.search_providers.len(),
        ConfigureState::Permissions { .. } => 3,
        ConfigureState::Display { .. } => 2,
        ConfigureState::Integrations { .. } => 3,
        ConfigureState::LanguageTheme { .. } => 2,
        ConfigureState::Vault { .. } => 3,
        ConfigureState::Notifications { .. } => 10,
        ConfigureState::Failover { .. } => 3,
        ConfigureState::Ssh { .. } => 3,
        ConfigureState::DockerK8s { .. } => 4,
        ConfigureState::Database { .. } => 2,
        ConfigureState::MemoryArchive { .. } => 4,
        ConfigureState::PluginsCron { .. } => 3 + data.active_cron_jobs.len(),
        _ => 0,
    }
}

/// Given a section name string, return the corresponding menu state.
pub(super) fn section_state_from_name(section: &str, selected: usize) -> ConfigureState {
    match section {
        "Providers" => ConfigureState::Providers { selected },
        "Models" => ConfigureState::Models { selected },
        "Context" => ConfigureState::Context { selected },
        "Search" => ConfigureState::Search { selected },
        "Permissions" => ConfigureState::Permissions { selected },
        "Display" => ConfigureState::Display { selected },
        "Integrations" => ConfigureState::Integrations { selected },
        "LanguageTheme" | "Language & Theme" => ConfigureState::LanguageTheme { selected },
        "Vault" => ConfigureState::Vault { selected },
        "Notifications" => ConfigureState::Notifications { selected },
        "Failover" => ConfigureState::Failover { selected },
        "SSH" => ConfigureState::Ssh { selected },
        "DockerK8s" | "Docker & K8s" => ConfigureState::DockerK8s { selected },
        "Database" => ConfigureState::Database { selected },
        "MemoryArchive" | "Memory & Archive" => ConfigureState::MemoryArchive { selected },
        "PluginsCron" | "Plugins & Cron" => ConfigureState::PluginsCron { selected },
        _ => ConfigureState::MainMenu { selected },
    }
}

/// Parse a context-size string that may carry a 'k' or 'm' suffix.
pub(super) fn parse_context_size(v: &str) -> Option<u32> {
    let v = v.trim().to_lowercase();
    if let Some(num) = v.strip_suffix('k') {
        num.parse::<u32>().ok().map(|n| n * 1_000)
    } else if let Some(num) = v.strip_suffix('m') {
        num.parse::<u32>().ok().map(|n| n * 1_000_000)
    } else {
        v.parse::<u32>().ok()
    }
}

/// Look up a notification field value from `ConfigureData` by config key name.
pub(super) fn configure_data_notify_value(data: &ConfigureData, key: &str) -> String {
    match key {
        "notify_discord_webhook" => data.notify_discord_webhook.clone(),
        "notify_slack_webhook" => data.notify_slack_webhook.clone(),
        "notify_telegram_token" => data.notify_telegram_token.clone(),
        "notify_whatsapp_url" => data.notify_whatsapp_url.clone(),
        "notify_whatsapp_token" => data.notify_whatsapp_token.clone(),
        "notify_matrix_homeserver" => data.notify_matrix_homeserver.clone(),
        "notify_matrix_token" => data.notify_matrix_token.clone(),
        "notify_signal_sender" => data.notify_signal_sender.clone(),
        "notify_signal_cli_path" => data.notify_signal_cli_path.clone(),
        _ => String::new(),
    }
}

/// Mask a sensitive string for display.
pub(super) fn mask_sensitive(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 10 {
        return "•".repeat(chars.len());
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

/// Map a (section, key, value) triple from the inline editor to a `ConfigureAction`.
pub(super) fn configure_action_for(section: &str, key: &str, value: &str) -> Option<ConfigureAction> {
    let v = value.trim().to_string();
    if v.is_empty() {
        return None;
    }
    match (section, key) {
        ("Models", "default_model") => Some(ConfigureAction::SetDefaultModel { model: v }),
        ("Models", "image_model") => Some(ConfigureAction::SetImageModel { model: v }),
        ("Context", "context_size") => {
            let n = parse_context_size(&v)?;
            Some(ConfigureAction::SetContextSize { size: u64::from(n) })
        }
        ("Context", "compact_threshold") => {
            let n = v.parse::<u8>().ok()?;
            Some(ConfigureAction::SetCompactThreshold { pct: n })
        }
        ("Providers", "ollama_host") => Some(ConfigureAction::SetOllamaHost { url: v }),
        ("Providers", key) if key.ends_with("_api_key") => {
            let provider = key.trim_end_matches("_api_key").to_string();
            Some(ConfigureAction::SetApiKey { provider, key: v })
        }
        ("Search", "default_search") => Some(ConfigureAction::SetDefaultSearch { provider: v }),
        ("Search", key) if key.ends_with("_key") => {
            let provider = key.trim_end_matches("_key").to_string();
            Some(ConfigureAction::SetSearchKey { provider, key: v })
        }
        ("Vault", "vault_session_ttl") => {
            let n = v.parse::<u64>().ok()?;
            Some(ConfigureAction::SetVaultSessionTtl { secs: n })
        }
        ("Notifications", key) if key.starts_with("notify_") => {
            Some(ConfigureAction::SetNotifyValue { key: key.to_string(), value: v })
        }
        ("Failover", "failover_cooldown") => {
            let n = v.parse::<u64>().ok()?;
            Some(ConfigureAction::SetFailoverCooldown { secs: n })
        }
        ("Failover", "failover_budget") => {
            let n = v.parse::<u64>().ok()?;
            Some(ConfigureAction::SetFailoverBudget { budget: n })
        }
        ("SSH", "ssh_key_path") => Some(ConfigureAction::SetSshKeyPath { path: v }),
        ("SSH", "ssh_bastion_host") => Some(ConfigureAction::SetSshBastionHost { host: v }),
        ("SSH", "ssh_config_path") => Some(ConfigureAction::SetSshConfigPath { path: v }),
        ("DockerK8s", "docker_compose_file") => Some(ConfigureAction::SetDockerComposeFile { path: v }),
        ("DockerK8s", "docker_registry") => Some(ConfigureAction::SetDockerRegistry { url: v }),
        ("DockerK8s", "k8s_context") => Some(ConfigureAction::SetK8sContext { ctx: v }),
        ("DockerK8s", "k8s_namespace") => Some(ConfigureAction::SetK8sNamespace { ns: v }),
        ("Database", "db_url") => Some(ConfigureAction::SetDbUrl { url: v }),
        ("MemoryArchive", "archive_frequency") => {
            let n = v.parse::<u64>().ok()?;
            Some(ConfigureAction::SetArchiveFrequency { n })
        }
        ("MemoryArchive", "archive_retention_days") => {
            let n = v.parse::<u64>().ok()?;
            Some(ConfigureAction::SetArchiveRetention { days: n })
        }
        ("MemoryArchive", "memory_dir") => Some(ConfigureAction::SetMemoryDir { path: v }),
        ("PluginsCron", "plugin_search_paths") => Some(ConfigureAction::SetPluginSearchPaths { paths: v }),
        _ => None,
    }
}

/// Handle character-level editing for `EditingValue` state.
/// Returns true if the event was handled.
pub(super) fn editing_value_handle_key(
    key: crossterm::event::KeyEvent,
    value: &mut String,
    cursor: &mut usize,
) {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Char(ch) => {
            value.insert(*cursor, ch);
            *cursor += ch.len_utf8();
        }
        KeyCode::Backspace => {
            if *cursor > 0 {
                let prev = prev_char_boundary(value, *cursor);
                value.drain(prev..*cursor);
                *cursor = prev;
            }
        }
        KeyCode::Delete => {
            if *cursor < value.len() {
                let next = next_char_boundary(value, *cursor);
                value.drain(*cursor..next);
            }
        }
        KeyCode::Left => {
            if *cursor > 0 {
                *cursor = prev_char_boundary(value, *cursor);
            }
        }
        KeyCode::Right => {
            if *cursor < value.len() {
                *cursor = next_char_boundary(value, *cursor);
            }
        }
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = value.len(),
        _ => {}
    }
}
