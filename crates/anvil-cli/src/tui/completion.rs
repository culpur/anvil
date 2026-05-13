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
/// - Models: live provider `/models` queries on first TAB, cached per-session
///   with a 10-minute TTL.  See `feedback-model-list-is-live-not-registry.md`.
/// - Providers: hard-coded constant list.
/// - Languages: hard-coded i18n codes.
///
/// Construction is cheap — all disk reads are deferred to the first call to
/// `resolve()` for each source, so typing `/` does not trigger any I/O.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use api::{ProviderCredentials, ProviderKind, ProviderModelsError};
use commands::{CompletionContext, DynamicEnumSource};

// ─── TuiCompletionContext ─────────────────────────────────────────────────────

/// Per-session cache for the live model list returned by
/// [`CompletionContext::model_choices`].
///
/// Populated on the first TAB-trigger and reused for the rest of the cache
/// window (10 min default).  Held behind an `Arc<Mutex>` so the same cache
/// survives every cheap `TuiCompletionContext::new()` rebuild while still
/// being safe to mutate from any thread that handles a completion request.
#[derive(Debug, Clone, Default)]
pub(crate) struct ModelChoicesCache {
    pub fetched_at: Option<Instant>,
    pub models: Vec<(String, String)>,
}

/// Cache TTL for the live model list. 10 minutes matches the contract in
/// `feedback-model-list-is-live-not-registry.md` (rule 1, "default 5–10 min").
pub(crate) const MODEL_CHOICES_CACHE_TTL: Duration = Duration::from_secs(600);

/// Shared cache slot. Static so every cheap `TuiCompletionContext::new()`
/// rebuilt by `suggest_completions()` hits the same memory.
fn model_choices_cache() -> &'static Arc<Mutex<ModelChoicesCache>> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Arc<Mutex<ModelChoicesCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Mutex::new(ModelChoicesCache::default())))
}

/// Drop the cached live model list so the next `/model <TAB>` re-fetches.
///
/// Intended for use after credential changes (`/provider login`, `/vault unlock`,
/// etc.) so the picker reflects the post-login state without forcing the user
/// to restart Anvil.  Currently called when the Ollama cache is invalidated;
/// future work can hook in additional callsites.
pub fn invalidate_model_choices_cache() {
    if let Ok(mut cache) = model_choices_cache().lock() {
        *cache = ModelChoicesCache::default();
    }
}

/// Hook used by tests to control the live-fetch path. When set, the live
/// fetch routine returns these models instead of hitting the network.
#[cfg(test)]
pub(crate) static MODEL_FETCH_OVERRIDE: std::sync::OnceLock<
    Mutex<Option<Box<dyn Fn() -> ModelFetchOutcome + Send + Sync>>>,
> = std::sync::OnceLock::new();

/// Pair of `(configured providers, per-provider fetch results)` returned by
/// the mocked fetcher hook. Mirrors what
/// [`live_provider_models`] would assemble from the real network.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct ModelFetchOutcome {
    pub configured: Vec<ProviderCredentials>,
    pub fetched: Vec<(ProviderKind, Result<Vec<(String, String)>, ProviderModelsError>)>,
    pub fetcher_calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
pub(crate) fn install_model_fetch_override<F>(f: F)
where
    F: Fn() -> ModelFetchOutcome + Send + Sync + 'static,
{
    let slot = MODEL_FETCH_OVERRIDE.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().expect("override mutex");
    *guard = Some(Box::new(f));
}

#[cfg(test)]
pub(crate) fn clear_model_fetch_override() {
    if let Some(slot) = MODEL_FETCH_OVERRIDE.get() {
        let mut guard = slot.lock().expect("override mutex");
        *guard = None;
    }
}

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

// ─── live model-list helpers ──────────────────────────────────────────────────

/// Fan out the per-provider fetchers in parallel against every credential the
/// user actually has.  Hard-bounded by the per-provider 4 s timeout inside
/// the api crate, so worst-case wall time is one TLS round trip.
///
/// Returns a list of `(kind, result)` pairs in stable [`ProviderKind`] order.
/// The caller merges these with the static [`api::known_models`] fallback as
/// described in `feedback-model-list-is-live-not-registry.md`.
async fn fetch_models_for_credentials(
    creds: &[ProviderCredentials],
) -> Vec<(ProviderKind, Result<Vec<api::ProviderModel>, ProviderModelsError>)> {
    // Build the set of fetch futures matching each configured credential.
    let mut futures: Vec<
        std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = (ProviderKind, Result<Vec<api::ProviderModel>, ProviderModelsError>),
                    > + Send,
            >,
        >,
    > = Vec::with_capacity(creds.len());
    let mut emitted_ollama = false;
    for cred in creds {
        match cred {
            ProviderCredentials::Anthropic => {
                futures.push(Box::pin(async {
                    (ProviderKind::AnvilApi, api::fetch_anthropic_models().await)
                }));
            }
            ProviderCredentials::OpenAi => {
                futures.push(Box::pin(async {
                    (ProviderKind::OpenAi, api::fetch_openai_models().await)
                }));
            }
            ProviderCredentials::Xai => {
                futures.push(Box::pin(async {
                    (ProviderKind::Xai, api::fetch_xai_models().await)
                }));
            }
            ProviderCredentials::Gemini => {
                futures.push(Box::pin(async {
                    (ProviderKind::Gemini, api::fetch_gemini_models().await)
                }));
            }
            ProviderCredentials::OllamaLocal | ProviderCredentials::OllamaCloud => {
                // Both flavors share a single tags endpoint on the local daemon,
                // so de-dupe.
                if !emitted_ollama {
                    emitted_ollama = true;
                    futures.push(Box::pin(async {
                        (ProviderKind::Ollama, api::fetch_ollama_local_models().await)
                    }));
                }
            }
        }
    }

    // Concurrent-but-without-a-new-dep: drive each future to completion on
    // its own tokio task and harvest results in original order.
    let mut handles: Vec<
        tokio::task::JoinHandle<(ProviderKind, Result<Vec<api::ProviderModel>, ProviderModelsError>)>,
    > = Vec::with_capacity(futures.len());
    for fut in futures {
        handles.push(tokio::spawn(fut));
    }
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(pair) => out.push(pair),
            Err(error) => out.push((
                ProviderKind::Ollama,
                Err(ProviderModelsError::Other(error.to_string())),
            )),
        }
    }
    out
}

/// Map a successful per-provider fetch + the static [`api::known_models`]
/// fallback for transient failures into the final `(name, provider-label)`
/// list rendered by the picker.
///
/// Rules from `feedback-model-list-is-live-not-registry.md`:
/// - `Unauthorized` (401/403) → omit provider, log warning
/// - `Transient` → fall back to registry entries for that provider
/// - `InvalidResponse` / `Other` → treat as transient (registry fallback)
fn build_label_list(
    configured: &[ProviderCredentials],
    fetched: &[(ProviderKind, Result<Vec<api::ProviderModel>, ProviderModelsError>)],
    ollama_cache: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    let mut models: Vec<(String, String)> = Vec::new();

    let configured_kinds: std::collections::HashSet<ProviderKind> =
        configured.iter().map(|c| c.kind()).collect();
    let configured_includes_cloud = configured
        .iter()
        .any(|c| matches!(c, ProviderCredentials::OllamaCloud));

    for (kind, outcome) in fetched {
        match outcome {
            Ok(entries) => {
                for entry in entries {
                    let label = match (kind, api::is_ollama_cloud_model(&entry.id)) {
                        (ProviderKind::Ollama, true) => "Ollama Cloud",
                        (ProviderKind::Ollama, false) => "Ollama (local)",
                        (other, _) => api::provider_display_name(*other),
                    };
                    // When the daemon offered cloud-suffixed tags but cloud
                    // auth was determined unavailable, skip them — they would
                    // 401 at runtime.
                    if *kind == ProviderKind::Ollama
                        && api::is_ollama_cloud_model(&entry.id)
                        && !configured_includes_cloud
                    {
                        continue;
                    }
                    models.push((entry.id.clone(), label.to_string()));
                }
            }
            Err(ProviderModelsError::Unauthorized) => {
                warnings.push(format!(
                    "[warn] {} reported 401/403; hiding provider until credentials are refreshed.",
                    api::provider_display_name(*kind)
                ));
            }
            Err(error) => {
                warnings.push(format!(
                    "[warn] {} live model list unavailable ({error}); falling back to known-good entries.",
                    api::provider_display_name(*kind)
                ));
                // Fallback: registry entries matching this kind
                for (name, k) in api::known_models() {
                    if k == *kind {
                        models.push((name.to_string(), api::provider_display_name(k).to_string()));
                    }
                }
            }
        }
    }

    // Always include any locally-cached Ollama tags from the startup probe
    // (even if the live /api/tags call hadn't finished by the time the cache
    // was last populated).  De-dupes happen at the end so we never show
    // duplicate model names.
    if configured_kinds.contains(&ProviderKind::Ollama) {
        for (name, _size) in ollama_cache {
            let label = if api::is_ollama_cloud_model(name) {
                "Ollama Cloud"
            } else {
                "Ollama (local)"
            };
            models.push((name.clone(), label.to_string()));
        }
    }

    // Degraded mode: if NO provider returned anything (all failed or no
    // providers configured), fall back to the static registry so the picker
    // isn't empty.
    if models.is_empty() {
        warnings.push("[warn] No reachable providers — using offline model list".to_string());
        for (name, kind) in api::known_models() {
            models.push((name.to_string(), api::provider_display_name(kind).to_string()));
        }
    }

    // De-dupe on model id, last write wins (so a live Ollama tag overrides
    // a registry stub of the same name).
    let mut seen = std::collections::HashSet::new();
    let mut deduped: Vec<(String, String)> = Vec::with_capacity(models.len());
    for entry in models.into_iter().rev() {
        if seen.insert(entry.0.clone()) {
            deduped.push(entry);
        }
    }
    deduped.reverse();

    (deduped, warnings)
}

/// Run [`fetch_models_for_credentials`] on the api-crate runtime, blocking the
/// caller until every provider responds (or the per-provider 4 s timeout
/// fires).  Honors the test-only `MODEL_FETCH_OVERRIDE` hook for unit tests.
fn live_provider_models(
    ollama_cache: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    #[cfg(test)]
    {
        if let Some(slot) = MODEL_FETCH_OVERRIDE.get() {
            if let Ok(guard) = slot.lock() {
                if let Some(hook) = guard.as_ref() {
                    let outcome = hook();
                    outcome
                        .fetcher_calls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let fetched: Vec<(
                        ProviderKind,
                        Result<Vec<api::ProviderModel>, ProviderModelsError>,
                    )> = outcome
                        .fetched
                        .into_iter()
                        .map(|(kind, result)| {
                            let mapped = result.map(|pairs| {
                                pairs
                                    .into_iter()
                                    .map(|(id, _label)| api::ProviderModel {
                                        id,
                                        provider: kind,
                                        display_name: None,
                                        context_window: None,
                                        deprecated: false,
                                    })
                                    .collect()
                            });
                            (kind, mapped)
                        })
                        .collect();
                    return build_label_list(&outcome.configured, &fetched, ollama_cache);
                }
            }
        }
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            return (
                api::known_models()
                    .into_iter()
                    .map(|(name, kind)| (name.to_string(), api::provider_display_name(kind).to_string()))
                    .collect(),
                vec!["[warn] Unable to build async runtime — using offline model list".to_string()],
            );
        }
    };
    let configured = runtime.block_on(api::enumerate_configured_providers());
    let fetched = runtime.block_on(fetch_models_for_credentials(&configured));
    build_label_list(&configured, &fetched, ollama_cache)
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
    /// On first TAB per cache window, fans out parallel `/models` calls
    /// against every configured provider (Anthropic, OpenAI, xAI, Gemini,
    /// local Ollama) and caches the merged list for [`MODEL_CHOICES_CACHE_TTL`]
    /// (10 min). Subsequent TABs hit the cache, never the network.
    ///
    /// Unconfigured providers are hidden entirely. Providers that 401/403 are
    /// hidden with a warning logged to scrollback. Transient failures
    /// (5xx, network, timeout) fall back to the static
    /// [`api::known_models`] entries for that provider so the picker is
    /// never blank.
    ///
    /// See `feedback-model-list-is-live-not-registry.md` for the user contract.
    fn model_choices(&self) -> Vec<(String, String)> {
        let cache_slot = model_choices_cache();
        if let Ok(cache) = cache_slot.lock() {
            if let Some(fetched_at) = cache.fetched_at {
                if fetched_at.elapsed() < MODEL_CHOICES_CACHE_TTL && !cache.models.is_empty() {
                    return cache.models.clone();
                }
            }
        }

        let (models, warnings) = live_provider_models(&self.ollama_models);

        // Surface warnings to TUI scrollback so the user knows a provider
        // dropped out. We can't reach the TUI sender from here (this runs on
        // the keystroke thread), so we write to stderr — the TuiTracingLayer
        // will pick it up if active; otherwise the message lands in the
        // terminal scrollback above the popup.
        for warning in &warnings {
            eprintln!("{warning}");
        }

        if let Ok(mut cache) = cache_slot.lock() {
            *cache = ModelChoicesCache {
                fetched_at: Some(Instant::now()),
                models: models.clone(),
            };
        }

        models
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

    // ─────────────────────────────────────────────────────────────────────────
    // Live model-list integration: cache + fallback + warnings
    // ─────────────────────────────────────────────────────────────────────────

    use std::sync::atomic::Ordering;
    use std::sync::Mutex as StdMutex;

    /// Coarse serial guard for the live-model-list tests because they all
    /// share the singleton `model_choices_cache` and `MODEL_FETCH_OVERRIDE`
    /// hook. Running them in parallel would interleave cache state.
    fn live_test_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::OnceLock;
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reset_cache_and_override() {
        invalidate_model_choices_cache();
        clear_model_fetch_override();
    }

    #[test]
    fn live_list_merges_anthropic_openai_and_ollama() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![
                ProviderCredentials::Anthropic,
                ProviderCredentials::OpenAi,
                ProviderCredentials::OllamaLocal,
            ],
            fetched: vec![
                (
                    ProviderKind::AnvilApi,
                    Ok(vec![("claude-sonnet-4-7".into(), "Anthropic".into())]),
                ),
                (
                    ProviderKind::OpenAi,
                    Ok(vec![("gpt-5.5".into(), "OpenAI".into())]),
                ),
                (
                    ProviderKind::Ollama,
                    Ok(vec![("llama4:8b".into(), "Ollama (local)".into())]),
                ),
            ],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let models = ctx.model_choices();
        let ids: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert!(ids.contains(&"claude-sonnet-4-7"), "anthropic entry missing: {ids:?}");
        assert!(ids.contains(&"gpt-5.5"), "openai entry missing: {ids:?}");
        assert!(ids.contains(&"llama4:8b"), "ollama entry missing: {ids:?}");
        let llama_label = models
            .iter()
            .find(|(n, _)| n == "llama4:8b")
            .map(|(_, l)| l.as_str())
            .unwrap_or("");
        assert_eq!(llama_label, "Ollama (local)");
        reset_cache_and_override();
    }

    #[test]
    fn anthropic_401_hides_provider_but_keeps_others() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![
                ProviderCredentials::Anthropic,
                ProviderCredentials::OpenAi,
                ProviderCredentials::OllamaLocal,
            ],
            fetched: vec![
                (ProviderKind::AnvilApi, Err(ProviderModelsError::Unauthorized)),
                (
                    ProviderKind::OpenAi,
                    Ok(vec![("gpt-5.5".into(), "OpenAI".into())]),
                ),
                (
                    ProviderKind::Ollama,
                    Ok(vec![("llama4:8b".into(), "Ollama (local)".into())]),
                ),
            ],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let models = ctx.model_choices();
        let ids: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            !ids.iter().any(|name| name.starts_with("claude")),
            "no Anthropic models should appear after 401: {ids:?}"
        );
        assert!(ids.contains(&"gpt-5.5"));
        assert!(ids.contains(&"llama4:8b"));
        reset_cache_and_override();
    }

    #[test]
    fn anthropic_transient_falls_back_to_registry_entries() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![ProviderCredentials::Anthropic],
            fetched: vec![(
                ProviderKind::AnvilApi,
                Err(ProviderModelsError::Transient("boom".into())),
            )],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let models = ctx.model_choices();
        let ids: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            ids.iter().any(|name| name.starts_with("claude")),
            "transient failure should fall back to registry entries: {ids:?}"
        );
        reset_cache_and_override();
    }

    #[test]
    fn no_providers_configured_falls_back_to_offline_registry() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![],
            fetched: vec![],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let models = ctx.model_choices();
        // Offline fallback: every registry entry shows up.
        assert!(
            !models.is_empty(),
            "empty configured list should still produce offline registry"
        );
        let ids: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert!(ids.iter().any(|n| n.starts_with("claude")));
        assert!(ids.iter().any(|n| n.starts_with("gpt-")));
        reset_cache_and_override();
    }

    #[test]
    fn cache_hits_within_ttl_skip_fetcher() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![ProviderCredentials::OpenAi],
            fetched: vec![(
                ProviderKind::OpenAi,
                Ok(vec![("gpt-5.5".into(), "OpenAI".into())]),
            )],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let _first = ctx.model_choices();
        let after_first = calls.load(Ordering::SeqCst);
        let _second = ctx.model_choices();
        let after_second = calls.load(Ordering::SeqCst);
        assert_eq!(
            after_first, after_second,
            "second TAB within TTL should hit cache (calls: {after_first} → {after_second})"
        );
        assert!(after_first >= 1, "first call should have hit the fetcher");
        reset_cache_and_override();
    }

    #[test]
    fn cache_invalidation_refires_fetcher() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![ProviderCredentials::OpenAi],
            fetched: vec![(
                ProviderKind::OpenAi,
                Ok(vec![("gpt-5.5".into(), "OpenAI".into())]),
            )],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let _first = ctx.model_choices();
        let before_invalidate = calls.load(Ordering::SeqCst);
        invalidate_model_choices_cache();
        let _second = ctx.model_choices();
        let after_invalidate = calls.load(Ordering::SeqCst);
        assert!(
            after_invalidate > before_invalidate,
            "post-invalidation fetch should re-hit the fetcher (calls: {before_invalidate} → {after_invalidate})"
        );
        reset_cache_and_override();
    }

    #[test]
    fn ollama_cloud_filtered_when_only_local_configured() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![ProviderCredentials::OllamaLocal],
            fetched: vec![(
                ProviderKind::Ollama,
                Ok(vec![
                    ("llama4:8b".into(), "Ollama (local)".into()),
                    ("kimi-k2.6:cloud".into(), "Ollama Cloud".into()),
                ]),
            )],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let models = ctx.model_choices();
        let ids: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert!(ids.contains(&"llama4:8b"), "local model must be present: {ids:?}");
        assert!(
            !ids.contains(&"kimi-k2.6:cloud"),
            "cloud-suffixed model must be filtered when only local creds: {ids:?}"
        );
        reset_cache_and_override();
    }

    #[test]
    fn ollama_cloud_kept_when_cloud_configured() {
        let _g = live_test_lock();
        reset_cache_and_override();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        install_model_fetch_override(move || ModelFetchOutcome {
            configured: vec![ProviderCredentials::OllamaCloud],
            fetched: vec![(
                ProviderKind::Ollama,
                Ok(vec![
                    ("llama4:8b".into(), "Ollama (local)".into()),
                    ("kimi-k2.6:cloud".into(), "Ollama Cloud".into()),
                ]),
            )],
            fetcher_calls: Arc::clone(&calls_for_hook),
        });

        let ctx = TuiCompletionContext::new();
        let models = ctx.model_choices();
        let ids: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert!(ids.contains(&"llama4:8b"));
        assert!(ids.contains(&"kimi-k2.6:cloud"));
        let cloud_label = models
            .iter()
            .find(|(n, _)| n == "kimi-k2.6:cloud")
            .map(|(_, l)| l.as_str())
            .unwrap_or("");
        assert_eq!(cloud_label, "Ollama Cloud");
        reset_cache_and_override();
    }
}
