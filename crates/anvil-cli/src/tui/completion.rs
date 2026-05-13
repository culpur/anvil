/// TUI implementation of [`commands::CompletionContext`].
///
/// Resolves every [`DynamicEnumSource`] variant using data available at
/// keystroke time:
///
/// - Vault credential types: static list matching the `CredentialType` enum
///   (same tokens used by `subcommands::VAULT_CREDENTIAL_TYPES`).
/// - Installed plugins: discovered from the plugin manager config directory.
/// - Installed themes: built-in names from `runtime::Theme` + any custom
///   themes stored in `~/.anvil/themes/`.
/// - Installed agents / skills: TODO — no registry yet; returns empty.
/// - MCP servers: read from the merged config (lazy, from disk).
/// - Sessions: from the session store directory.
/// - Models: static fallback list (Ollama cache + hard-coded cloud models).
/// - Providers: hard-coded constant list.
/// - Languages: hard-coded i18n codes.
///
/// Construction is cheap — all disk reads are deferred to the first call to
/// `resolve()` for each source, so typing `/` does not trigger any I/O.

use commands::{CompletionContext, DynamicEnumSource};

// ─── TuiCompletionContext ─────────────────────────────────────────────────────

/// A [`CompletionContext`] that resolves dynamic completion values from the
/// live TUI environment (installed plugins, MCP servers, sessions, models …).
pub struct TuiCompletionContext {
    /// Cached Ollama models (name, size) snapshot taken at startup.
    /// The popup is built per-keystroke so we avoid re-querying on each call.
    pub ollama_models: Vec<(String, String)>,
}

impl TuiCompletionContext {
    /// Build a new context, pulling the Ollama model cache from the global
    /// `OnceLock` populated by [`super::widgets::init_ollama_model_cache`].
    pub fn new() -> Self {
        Self {
            ollama_models: super::widgets::cached_ollama_models(),
        }
    }
}

impl Default for TuiCompletionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionContext for TuiCompletionContext {
    fn resolve(&self, source: DynamicEnumSource) -> Vec<String> {
        match source {
            // ── Vault credential types ─────────────────────────────────────
            // These are the snake_case tokens that the vault CLI accepts.
            // They match the VAULT_CREDENTIAL_TYPES constant in subcommands.rs.
            DynamicEnumSource::VaultCredentialTypes => vault_credential_type_tokens(),

            // ── Installed plugins ──────────────────────────────────────────
            DynamicEnumSource::InstalledPlugins => list_installed_plugins(),

            // ── Installed themes ───────────────────────────────────────────
            DynamicEnumSource::InstalledThemes => {
                let mut themes: Vec<String> = runtime::Theme::builtin_names()
                    .iter()
                    .map(|n| n.to_string())
                    .collect();
                // Append any user-installed themes from ~/.anvil/themes/
                themes.extend(list_custom_themes());
                themes
            }

            // ── Installed agents ───────────────────────────────────────────
            // TODO: No installed-agents registry exists yet.
            // Phase 3 / agent registry work will populate this.
            DynamicEnumSource::InstalledAgents => vec![],

            // ── Installed skills ───────────────────────────────────────────
            // Discover skills from all configured roots relative to cwd.
            DynamicEnumSource::InstalledSkills => list_installed_skills(),

            // ── MCP servers ────────────────────────────────────────────────
            DynamicEnumSource::McpServers => list_mcp_server_names(),

            // ── Sessions ──────────────────────────────────────────────────
            DynamicEnumSource::Sessions => list_session_ids(),

            // ── Models ─────────────────────────────────────────────────────
            // Hard-coded cloud models plus whatever Ollama reported at startup.
            DynamicEnumSource::Models => {
                let mut models: Vec<String> = vec![
                    "claude-opus-4-6".into(),
                    "claude-sonnet-4-6".into(),
                    "claude-haiku-4-5".into(),
                    "gpt-5.4".into(),
                    "gpt-5.4-mini".into(),
                    "gpt-5".into(),
                    "o3".into(),
                    "grok".into(),
                ];
                for (name, _size) in &self.ollama_models {
                    models.push(name.clone());
                }
                models
            }

            // ── Installed Ollama models ────────────────────────────────────
            DynamicEnumSource::InstalledOllamaModels => {
                super::widgets::cached_ollama_models()
                    .into_iter()
                    .map(|(name, _size)| name)
                    .collect()
            }

            // ── Providers ─────────────────────────────────────────────────
            DynamicEnumSource::Providers => vec![
                "anthropic".into(),
                "openai".into(),
                "ollama".into(),
                "xai".into(),
            ],

            // ── Languages ─────────────────────────────────────────────────
            DynamicEnumSource::Languages => vec![
                "en".into(),
                "de".into(),
                "es".into(),
                "fr".into(),
                "ja".into(),
                "zh-CN".into(),
                "ru".into(),
            ],

            // ── Output styles ──────────────────────────────────────────────
            DynamicEnumSource::OutputStyles => list_output_style_names(),

            // ── Goals (project goal IDs) ───────────────────────────────────
            DynamicEnumSource::Goals => vec![],

            // ── Named profiles (W4) ────────────────────────────────────────
            DynamicEnumSource::Profiles => list_profile_names(),
        }
    }

    /// Live `/model <TAB>` choices.
    ///
    /// Aggregates:
    /// - Every alias from the static `api::known_models()` registry
    ///   (Anthropic, OpenAI, xAI, Google Gemini), labelled with the
    ///   provider display name.
    /// - Every locally-discovered Ollama model from the startup cache,
    ///   distinguishing Ollama Cloud (`:cloud` / `-cloud` suffix) from
    ///   the local daemon.
    ///
    /// Dedupes by model name (Ollama-side names like `llama3.2` take the
    /// real local size from the cache rather than the registry stub).
    fn model_choices(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = api::known_models()
            .into_iter()
            .map(|(name, kind)| (name.to_string(), api::provider_display_name(kind).to_string()))
            .collect();

        for (name, _size) in &self.ollama_models {
            // Drop any registry entry the user actually has installed locally
            // so the live entry (with provider label distinguishing local vs
            // cloud) wins.
            out.retain(|(existing, _)| existing != name);
            let label = if api::is_ollama_cloud_model(name) {
                "Ollama Cloud"
            } else {
                "Ollama (local)"
            };
            out.push((name.clone(), label.to_string()));
        }

        out
    }
}

// ─── Helper: vault credential type tokens ────────────────────────────────────

/// Returns the 21 snake_case credential type tokens accepted by the vault CLI.
/// These must stay in sync with `runtime::vault::CredentialType`.
pub fn vault_credential_type_tokens() -> Vec<String> {
    vec![
        "api_key".into(),
        "ssh_key".into(),
        "tls_cert".into(),
        "totp".into(),
        "database_url".into(),
        "oauth_token".into(),
        "encryption_key".into(),
        "webhook_secret".into(),
        "license_key".into(),
        "secret_text".into(),
        "username_password".into(),
        "cloud_credential".into(),
        "host_credential".into(),
        "docker_registry".into(),
        "kube_config".into(),
        "vpn_config".into(),
        "client_cert".into(),
        "signing_key".into(),
        "recovery_code".into(),
        "env_file".into(),
        "config_blob".into(),
    ]
}

// ─── Helper: installed plugins ────────────────────────────────────────────────

fn list_installed_plugins() -> Vec<String> {
    let config_home = match dirs_home() {
        Some(h) => h,
        None => return vec![],
    };
    let plugin_config = plugins::PluginManagerConfig::new(config_home);
    let mut manager = plugins::PluginManager::new(plugin_config);
    manager
        .list_installed_plugins()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.metadata.id)
        .collect()
}

// ─── Helper: installed skills ─────────────────────────────────────────────────

/// Discover and return the names of all installed skills for tab completion.
/// Skills are loaded from all configured skill roots relative to the current
/// working directory.  Returns an empty vec on any error (completion is
/// best-effort — errors must not block the TUI keystroke path).
fn list_installed_skills() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let roots = commands::discover_skill_roots(&cwd);
    match commands::load_skills_from_roots(&roots) {
        Ok(skills) => skills
            .into_iter()
            .filter(|s| s.shadowed_by.is_none())
            .map(|s| s.name)
            .collect(),
        Err(_) => vec![],
    }
}

// ─── Helper: custom (user-installed) themes ───────────────────────────────────

fn list_custom_themes() -> Vec<String> {
    let home = match dirs_home() {
        Some(h) => h,
        None => return vec![],
    };
    let themes_dir = home.join(".anvil").join("themes");
    let Ok(entries) = std::fs::read_dir(&themes_dir) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("json") {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

// ─── Helper: MCP server names ─────────────────────────────────────────────────

/// Reads MCP server names from the merged config file.
/// Uses the same config loader that the runtime uses at startup.
fn list_mcp_server_names() -> Vec<String> {
    let home = match dirs_home() {
        Some(h) => h,
        None => return vec![],
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    match runtime::ConfigLoader::new(cwd, home).load() {
        Ok(config) => config
            .feature_config()
            .mcp()
            .servers()
            .keys()
            .cloned()
            .collect(),
        Err(_) => vec![],
    }
}

// ─── Helper: session IDs ──────────────────────────────────────────────────────

fn list_session_ids() -> Vec<String> {
    let home = match dirs_home() {
        Some(h) => h,
        None => return vec![],
    };
    let sessions_dir = home.join(".anvil").join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return vec![];
    };
    let mut sessions: Vec<(u64, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let modified = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some((modified, id))
        })
        .collect();
    // Most-recent first
    sessions.sort_by(|a, b| b.0.cmp(&a.0));
    sessions.into_iter().map(|(_, id)| id).take(20).collect()
}

// ─── Helper: output style names ──────────────────────────────────────────────

/// Returns all available output style names for tab completion.
/// Includes built-in names, user styles from `~/.anvil/output-styles/`,
/// and the control tokens `list` and `reset`.
pub fn list_output_style_names() -> Vec<String> {
    let home = match dirs_home() {
        Some(h) => h,
        None => {
            return vec![
                "precise".into(),
                "condensed".into(),
                "list".into(),
                "reset".into(),
            ];
        }
    };
    let styles_dir = home.join(".anvil").join("output-styles");
    let mut registry = runtime::OutputStyleRegistry::new();
    registry.ensure_loaded(&styles_dir);
    registry.all_names()
}

/// List named profile names from `~/.anvil/settings.json` (user-level config).
fn list_profile_names() -> Vec<String> {
    let home = match dirs_home() {
        Some(h) => h,
        None => return vec![],
    };
    let settings_path = home.join(".anvil").join("settings.json");
    let Ok(contents) = std::fs::read_to_string(&settings_path) else {
        return vec![];
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return vec![];
    };
    val.get("profiles")
        .and_then(|p| p.as_object())
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

// ─── dirs helper ─────────────────────────────────────────────────────────────

fn dirs_home() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOMEDRIVE").and_then(|d| {
                std::env::var_os("HOMEPATH").map(|p| {
                    let mut full = d;
                    full.push(p);
                    full
                })
            }))
            .map(std::path::PathBuf::from)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use commands::{suggest_completions, NoopCompletionContext};

    /// Simple mock that has a fixed set of values for each source.
    struct MockCompletionContext {
        mcp_servers: Vec<String>,
        themes: Vec<String>,
        plugins: Vec<String>,
        models: Vec<String>,
    }

    impl MockCompletionContext {
        fn new() -> Self {
            Self {
                mcp_servers: vec!["claude-in-chrome".into(), "qmd".into(), "gmail".into()],
                themes: vec!["nord".into(), "dracula".into(), "cyberpunk".into()],
                plugins: vec!["git-plugin".into(), "docker-plugin".into()],
                models: vec![
                    "claude-sonnet-4-6".into(),
                    "gpt-5.4".into(),
                    "llama3.2".into(),
                ],
            }
        }
    }

    impl commands::CompletionContext for MockCompletionContext {
        fn model_choices(&self) -> Vec<(String, String)> {
            self.models
                .iter()
                .map(|name| {
                    let provider = if name.starts_with("claude") {
                        "Anthropic"
                    } else if name.starts_with("gpt") || name.starts_with("o3") {
                        "OpenAI"
                    } else {
                        "Ollama (local)"
                    };
                    (name.clone(), provider.to_string())
                })
                .collect()
        }

        fn resolve(&self, source: DynamicEnumSource) -> Vec<String> {
            match source {
                DynamicEnumSource::VaultCredentialTypes => vault_credential_type_tokens(),
                DynamicEnumSource::McpServers => self.mcp_servers.clone(),
                DynamicEnumSource::InstalledThemes => self.themes.clone(),
                DynamicEnumSource::InstalledPlugins => self.plugins.clone(),
                DynamicEnumSource::Models => self.models.clone(),
                DynamicEnumSource::Providers => {
                    vec!["anthropic".into(), "openai".into(), "ollama".into(), "xai".into()]
                }
                DynamicEnumSource::Languages => vec![
                    "en".into(), "de".into(), "es".into(), "fr".into(),
                    "ja".into(), "zh-CN".into(), "ru".into(),
                ],
                DynamicEnumSource::InstalledAgents
                | DynamicEnumSource::InstalledSkills
                | DynamicEnumSource::Sessions
                | DynamicEnumSource::InstalledOllamaModels
                | DynamicEnumSource::Goals
                | DynamicEnumSource::Profiles => vec![],
                DynamicEnumSource::OutputStyles => {
                    vec!["precise".into(), "condensed".into(), "list".into(), "reset".into()]
                }
            }
        }
    }

    // ── Input: "" → full palette ─────────────────────────────────────────────
    // Empty input is treated the same as "/" — user opened the completion
    // palette with no partial, show everything so they can browse.

    #[test]
    fn empty_input_returns_all_root_commands() {
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("", &ctx);
        assert!(
            completions.len() >= 90,
            "empty input should return full palette of root commands; got {}",
            completions.len()
        );
    }

    // ── Input: "/" → all root commands ────────────────────────────────────────

    #[test]
    fn slash_alone_returns_all_root_commands() {
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/", &ctx);
        // Phase 0 reported 101 specs in the registry.
        assert!(
            completions.len() >= 100,
            "expected ≥100 completions for '/', got {}",
            completions.len()
        );
        // All should start with "/"
        for c in &completions {
            assert!(c.text.starts_with('/'), "completion text should start with /: {}", c.text);
        }
    }

    // ── Input: "/v" → vault, version, vim, voice ──────────────────────────────

    #[test]
    fn prefix_v_returns_vault_version_vim_voice() {
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/v", &ctx);
        assert!(
            completions.len() >= 4,
            "'/v' should match at least vault/version/vim/voice, got {}",
            completions.len()
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"/vault"), "missing /vault");
        assert!(texts.contains(&"/version"), "missing /version");
        assert!(texts.contains(&"/vim"), "missing /vim");
        assert!(texts.contains(&"/voice"), "missing /voice");
    }

    // ── Input: "/vault " → ≥12 subcommands ───────────────────────────────────

    #[test]
    fn vault_space_shows_subcommands() {
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/vault ", &ctx);
        assert!(
            completions.len() >= 12,
            "'/vault ' should show ≥12 subcommands, got {}",
            completions.len()
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        for expected in &["setup", "unlock", "lock", "store", "get", "list", "delete", "totp"] {
            assert!(texts.contains(expected), "missing vault subcommand: {expected}");
        }
    }

    // ── Input: "/vault store " → 21 credential types ─────────────────────────

    #[test]
    fn vault_store_shows_credential_types() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/vault store ", &ctx);
        assert_eq!(
            completions.len(),
            21,
            "'/vault store ' should return exactly 21 credential types, got {}",
            completions.len()
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"api_key"));
        assert!(texts.contains(&"ssh_key"));
        assert!(texts.contains(&"tls_cert"));
        assert!(texts.contains(&"totp"));
        assert!(texts.contains(&"database_url"));
    }

    // ── Input: "/mcp " → 3 subcommands ───────────────────────────────────────

    #[test]
    fn mcp_space_shows_subcommands() {
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/mcp ", &ctx);
        assert!(
            completions.len() >= 3,
            "'/mcp ' should show ≥3 subcommands, got {}",
            completions.len()
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"list"), "missing mcp list");
        assert!(texts.contains(&"status"), "missing mcp status");
        assert!(texts.contains(&"tools"), "missing mcp tools");
    }

    // ── Input: "/mcp tools " → mocked MCP servers ────────────────────────────

    #[test]
    fn mcp_tools_shows_mcp_servers() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/mcp tools ", &ctx);
        assert_eq!(
            completions.len(),
            3,
            "'/mcp tools ' should return 3 mocked MCP servers, got {}",
            completions.len()
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"claude-in-chrome"));
        assert!(texts.contains(&"qmd"));
        assert!(texts.contains(&"gmail"));
    }

    // ── Input: "/theme set " → mocked themes ─────────────────────────────────

    #[test]
    fn theme_set_shows_themes() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/theme set ", &ctx);
        assert!(
            !completions.is_empty(),
            "'/theme set ' should show themes, got none"
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"nord"), "missing nord");
        assert!(texts.contains(&"dracula"), "missing dracula");
    }

    // ── Input: "/provider " → anthropic/openai/ollama/xai + list/add/remove/login

    #[test]
    fn provider_space_shows_providers_and_subcommands() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/provider ", &ctx);
        assert!(
            !completions.is_empty(),
            "'/provider ' should show completions"
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        // provider subcommands from specs.rs include list/add/remove/login/anthropic/openai/etc.
        let has_provider_names = texts.contains(&"anthropic")
            || texts.contains(&"openai")
            || texts.contains(&"list")
            || texts.contains(&"add")
            || texts.contains(&"login");
        assert!(
            has_provider_names,
            "'/provider ' completions should include known provider names or subcommands; got: {texts:?}"
        );
    }

    // ── Input: "/model " — live provider-aware completions ───────────────────
    //
    // `/model` has no static subcommand tree. It routes through
    // `CompletionContext::model_choices()` so the popup reflects whatever
    // providers are actually configured in this session (Anthropic, OpenAI,
    // local Ollama, Ollama Cloud, etc.). Each entry carries the provider
    // display name in the `description` field so the user can distinguish
    // `llama3.2` (Ollama local) from `claude-sonnet-4-6` (Anthropic) at a
    // glance.

    #[test]
    fn model_space_shows_models() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/model ", &ctx);
        assert_eq!(
            completions.len(),
            3,
            "'/model ' should return all three mocked models, got {}: {completions:?}",
            completions.len()
        );
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"claude-sonnet-4-6"));
        assert!(texts.contains(&"gpt-5.4"));
        assert!(texts.contains(&"llama3.2"));
        // Provider labels surface as the description field.
        let claude = completions
            .iter()
            .find(|c| c.text == "claude-sonnet-4-6")
            .expect("claude-sonnet-4-6 present");
        assert_eq!(claude.description, "Anthropic");
        let llama = completions
            .iter()
            .find(|c| c.text == "llama3.2")
            .expect("llama3.2 present");
        assert_eq!(llama.description, "Ollama (local)");
    }

    #[test]
    fn model_partial_filters_substring() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/model llama", &ctx);
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["llama3.2"]);
    }

    #[test]
    fn model_partial_filters_case_insensitive() {
        let ctx = MockCompletionContext::new();
        let completions = suggest_completions("/model CLAUDE", &ctx);
        let texts: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["claude-sonnet-4-6"]);
    }

    // ── vault_credential_type_tokens() count ─────────────────────────────────

    #[test]
    fn vault_credential_type_tokens_has_21_entries() {
        assert_eq!(
            vault_credential_type_tokens().len(),
            21,
            "vault_credential_type_tokens() must return exactly 21 entries"
        );
    }

    // ── TuiCompletionContext default construction ─────────────────────────────

    #[test]
    fn tui_completion_context_constructs() {
        let ctx = TuiCompletionContext::new();
        // Providers should always resolve even without Ollama running.
        let providers = ctx.resolve(DynamicEnumSource::Providers);
        assert!(providers.contains(&"anthropic".to_string()));
        assert!(providers.contains(&"openai".to_string()));
        assert!(providers.contains(&"ollama".to_string()));
        assert!(providers.contains(&"xai".to_string()));
    }

    // ── Languages resolver ────────────────────────────────────────────────────

    #[test]
    fn languages_resolver_returns_seven_locales() {
        let ctx = TuiCompletionContext::new();
        let langs = ctx.resolve(DynamicEnumSource::Languages);
        assert_eq!(langs.len(), 7, "expected 7 language codes, got {}", langs.len());
        assert!(langs.contains(&"en".to_string()));
        assert!(langs.contains(&"zh-CN".to_string()));
    }
}
