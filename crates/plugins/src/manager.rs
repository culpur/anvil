use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use include_dir::{include_dir, Dir};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::diagnostics::PluginLoadDiagnostic;
use crate::loader::{
    builtin_plugins, copy_dir_all, discover_plugin_dirs, load_plugin_definition_with_diagnostics,
    plugin_id, sanitize_plugin_id, BUNDLED_MARKETPLACE, EXTERNAL_MARKETPLACE,
};
use crate::manifest::{load_plugin_from_directory, PluginManifest};
use crate::marketplace::{materialize_source, parse_install_source, resolve_local_source};
use crate::registry::{
    describe_install_source, InstalledPluginRecord, InstalledPluginRegistry, PluginInstallSource,
};
use crate::tools::PluginTool;
use crate::{
    Plugin, PluginDefinition, PluginError, PluginHooks, PluginKind, PluginMetadata, PluginRegistry,
    PluginSummary, RegisteredPlugin,
};

// ---------------------------------------------------------------------------
// Embedded bundled plugin tree
// ---------------------------------------------------------------------------

/// The bundled plugin tree embedded in the binary at compile time.
///
/// `env!("CARGO_MANIFEST_DIR")` is resolved at compile time to the crate
/// root, so the bundled directory is baked into the binary and the produced
/// binary never relies on the developer's source path existing at runtime.
/// On first launch (or when the tree fingerprint changes) the embedded tree
/// is extracted to `<config_home>/plugins/bundled/` by
/// `materialize_bundled_plugins`.
static BUNDLED_PLUGINS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/bundled");

/// Filename of the SHA-256 fingerprint stored inside the materialized tree.
const BUNDLED_FINGERPRINT_FILE: &str = ".fingerprint";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SETTINGS_FILE_NAME: &str = "settings.json";
const REGISTRY_FILE_NAME: &str = "installed.json";

// ---------------------------------------------------------------------------
// Config and outcome types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManagerConfig {
    pub config_home: PathBuf,
    pub enabled_plugins: BTreeMap<String, bool>,
    pub external_dirs: Vec<PathBuf>,
    pub install_root: Option<PathBuf>,
    pub registry_path: Option<PathBuf>,
    pub bundled_root: Option<PathBuf>,
}

impl PluginManagerConfig {
    #[must_use]
    pub fn new(config_home: impl Into<PathBuf>) -> Self {
        Self {
            config_home: config_home.into(),
            enabled_plugins: BTreeMap::new(),
            external_dirs: Vec::new(),
            install_root: None,
            registry_path: None,
            bundled_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub plugin_id: String,
    pub version: String,
    pub install_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub plugin_id: String,
    pub old_version: String,
    pub new_version: String,
    pub install_path: PathBuf,
}

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManager {
    pub(crate) config: PluginManagerConfig,
    /// Diagnostics accumulated during the most recent `discover_plugins` run.
    diagnostics: Vec<PluginLoadDiagnostic>,
    /// Keys already printed to stderr; prevents duplicate stderr output.
    emitted_keys: BTreeSet<String>,
}

impl PluginManager {
    #[must_use]
    pub fn new(config: PluginManagerConfig) -> Self {
        Self {
            config,
            diagnostics: Vec::new(),
            emitted_keys: BTreeSet::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Diagnostic API
    // -----------------------------------------------------------------------

    /// Drain and return diagnostics accumulated since the last call.
    ///
    /// Each unique `dedup_key()` is returned at most once across the lifetime
    /// of this `PluginManager`.
    #[must_use]
    pub fn take_diagnostics(&mut self) -> Vec<PluginLoadDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Print accumulated diagnostics to stderr with `[plugin warning]` prefix.
    /// Each unique key is printed at most once per `PluginManager` lifetime.
    pub fn print_startup_diagnostics(&mut self) {
        let diags = std::mem::take(&mut self.diagnostics);
        for diag in diags {
            let key = diag.dedup_key();
            if self.emitted_keys.insert(key) {
                eprintln!("{}", diag.display_line());
            }
        }
    }

    fn push_diagnostic(&mut self, diag: PluginLoadDiagnostic) {
        let key = diag.dedup_key();
        if !self.emitted_keys.contains(&key) {
            // Check for duplicate within current pending batch too.
            if !self.diagnostics.iter().any(|d| d.dedup_key() == key) {
                self.diagnostics.push(diag);
            }
        }
    }

    fn push_diagnostics(&mut self, diags: Vec<PluginLoadDiagnostic>) {
        for diag in diags {
            self.push_diagnostic(diag);
        }
    }

    // -----------------------------------------------------------------------
    // Path helpers
    // -----------------------------------------------------------------------

    #[must_use]
    pub fn install_root(&self) -> PathBuf {
        self.config
            .install_root
            .clone()
            .unwrap_or_else(|| self.config.config_home.join("plugins").join("installed"))
    }

    #[must_use]
    pub fn registry_path(&self) -> PathBuf {
        self.config.registry_path.clone().unwrap_or_else(|| {
            self.config
                .config_home
                .join("plugins")
                .join(REGISTRY_FILE_NAME)
        })
    }

    #[must_use]
    pub fn settings_path(&self) -> PathBuf {
        self.config.config_home.join(SETTINGS_FILE_NAME)
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    pub fn plugin_registry(&mut self) -> Result<PluginRegistry, PluginError> {
        let plugins = self.discover_plugins()?;
        Ok(PluginRegistry::new(
            plugins
                .into_iter()
                .map(|plugin| {
                    let enabled = self.is_enabled(plugin.metadata());
                    RegisteredPlugin::new(plugin, enabled)
                })
                .collect(),
        ))
    }

    pub fn list_plugins(&mut self) -> Result<Vec<PluginSummary>, PluginError> {
        Ok(self.plugin_registry()?.summaries())
    }

    pub fn list_installed_plugins(&mut self) -> Result<Vec<PluginSummary>, PluginError> {
        Ok(self.installed_plugin_registry()?.summaries())
    }

    /// Discover all plugins, collecting per-plugin diagnostics rather than
    /// failing the entire run when a single manifest is broken.
    ///
    /// Returns `Err` only for system-level failures (I/O on the plugin root
    /// directory itself, registry corruption, etc.).
    pub fn discover_plugins(&mut self) -> Result<Vec<PluginDefinition>, PluginError> {
        self.diagnostics.clear();
        self.sync_bundled_plugins()?;
        let mut plugins = builtin_plugins();
        plugins.extend(self.discover_installed_plugins()?);
        plugins.extend(self.discover_external_directory_plugins(&plugins)?);
        Ok(plugins)
    }

    pub fn aggregated_hooks(&mut self) -> Result<PluginHooks, PluginError> {
        self.plugin_registry()?.aggregated_hooks()
    }

    pub fn aggregated_tools(&mut self) -> Result<Vec<PluginTool>, PluginError> {
        self.plugin_registry()?.aggregated_tools()
    }

    pub fn validate_plugin_source(&self, source: &str) -> Result<PluginManifest, PluginError> {
        let path = resolve_local_source(source)?;
        load_plugin_from_directory(&path)
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    pub fn install(&mut self, source: &str) -> Result<InstallOutcome, PluginError> {
        let install_source = parse_install_source(source)?;
        let temp_root = self.install_root().join(".tmp");
        let staged_source = materialize_source(&install_source, &temp_root)?;
        let cleanup_source = matches!(install_source, PluginInstallSource::GitUrl { .. });
        let manifest = load_plugin_from_directory(&staged_source)?;

        let plugin_id = plugin_id(&manifest.name, EXTERNAL_MARKETPLACE);
        let install_path = self.install_root().join(sanitize_plugin_id(&plugin_id));
        if install_path.exists() {
            fs::remove_dir_all(&install_path)?;
        }
        copy_dir_all(&staged_source, &install_path)?;
        if cleanup_source {
            let _ = fs::remove_dir_all(&staged_source);
        }

        let now = unix_time_ms();
        let record = InstalledPluginRecord {
            kind: PluginKind::External,
            id: plugin_id.clone(),
            name: manifest.name,
            version: manifest.version.clone(),
            description: manifest.description,
            install_path: install_path.clone(),
            source: install_source,
            installed_at_unix_ms: now,
            updated_at_unix_ms: now,
            // Trust level is populated by the caller (hub install) after
            // verifying the AnvilHub badge.  Local-path installs default to None.
            hub_trust_level: None,
        };

        let mut registry = self.load_registry()?;
        registry.plugins.insert(plugin_id.clone(), record);
        self.store_registry(&registry)?;
        self.write_enabled_state(&plugin_id, Some(true))?;
        self.config.enabled_plugins.insert(plugin_id.clone(), true);

        Ok(InstallOutcome {
            plugin_id,
            version: manifest.version,
            install_path,
        })
    }

    pub fn enable(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        self.ensure_known_plugin(plugin_id)?;
        self.write_enabled_state(plugin_id, Some(true))?;
        self.config
            .enabled_plugins
            .insert(plugin_id.to_string(), true);
        Ok(())
    }

    pub fn disable(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        self.ensure_known_plugin(plugin_id)?;
        self.write_enabled_state(plugin_id, Some(false))?;
        self.config
            .enabled_plugins
            .insert(plugin_id.to_string(), false);
        Ok(())
    }

    pub fn uninstall(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        let mut registry = self.load_registry()?;
        let record = registry.plugins.remove(plugin_id).ok_or_else(|| {
            PluginError::NotFound(format!("plugin `{plugin_id}` is not installed"))
        })?;
        if record.kind == PluginKind::Bundled {
            registry.plugins.insert(plugin_id.to_string(), record);
            return Err(PluginError::CommandFailed(format!(
                "plugin `{plugin_id}` is bundled and managed automatically; disable it instead"
            )));
        }
        if record.install_path.exists() {
            fs::remove_dir_all(&record.install_path)?;
        }
        self.store_registry(&registry)?;
        self.write_enabled_state(plugin_id, None)?;
        self.config.enabled_plugins.remove(plugin_id);
        Ok(())
    }

    pub fn update(&mut self, plugin_id: &str) -> Result<UpdateOutcome, PluginError> {
        let mut registry = self.load_registry()?;
        let record = registry.plugins.get(plugin_id).cloned().ok_or_else(|| {
            PluginError::NotFound(format!("plugin `{plugin_id}` is not installed"))
        })?;

        let temp_root = self.install_root().join(".tmp");
        let staged_source = materialize_source(&record.source, &temp_root)?;
        let cleanup_source = matches!(record.source, PluginInstallSource::GitUrl { .. });
        let manifest = load_plugin_from_directory(&staged_source)?;

        if record.install_path.exists() {
            fs::remove_dir_all(&record.install_path)?;
        }
        copy_dir_all(&staged_source, &record.install_path)?;
        if cleanup_source {
            let _ = fs::remove_dir_all(&staged_source);
        }

        let updated_record = InstalledPluginRecord {
            version: manifest.version.clone(),
            description: manifest.description,
            updated_at_unix_ms: unix_time_ms(),
            ..record.clone()
        };
        registry
            .plugins
            .insert(plugin_id.to_string(), updated_record);
        self.store_registry(&registry)?;

        Ok(UpdateOutcome {
            plugin_id: plugin_id.to_string(),
            old_version: record.version,
            new_version: manifest.version,
            install_path: record.install_path,
        })
    }

    // -----------------------------------------------------------------------
    // Internal discovery helpers (fault-isolated)
    // -----------------------------------------------------------------------

    fn discover_installed_plugins(&mut self) -> Result<Vec<PluginDefinition>, PluginError> {
        let mut registry = self.load_registry()?;
        let mut plugins = Vec::new();
        let mut seen_ids = BTreeSet::<String>::new();
        let mut seen_paths = BTreeSet::<PathBuf>::new();
        let mut stale_registry_ids = Vec::new();

        for install_path in discover_plugin_dirs(&self.install_root())? {
            let matched_record = registry
                .plugins
                .values()
                .find(|record| record.install_path == install_path);
            let kind = matched_record.map_or(PluginKind::External, |record| record.kind);
            let source = matched_record.map_or_else(
                || install_path.display().to_string(),
                |record| describe_install_source(&record.source),
            );
            match load_plugin_definition_with_diagnostics(
                &install_path,
                kind,
                source,
                kind.marketplace(),
            ) {
                Ok((plugin, diags)) => {
                    self.push_diagnostics(diags);
                    if seen_ids.insert(plugin.metadata().id.clone()) {
                        seen_paths.insert(install_path);
                        plugins.push(plugin);
                    }
                }
                Err(PluginError::NotFound(_)) => {
                    self.push_diagnostic(PluginLoadDiagnostic::ManifestMissing {
                        dir: install_path,
                    });
                }
                Err(error) => {
                    self.push_diagnostic(PluginLoadDiagnostic::ManifestParse {
                        dir: install_path,
                        detail: error.to_string(),
                    });
                }
            }
        }

        for record in registry.plugins.values() {
            if seen_paths.contains(&record.install_path) {
                continue;
            }
            if !record.install_path.exists()
                || crate::manifest::plugin_manifest_path(&record.install_path).is_err()
            {
                stale_registry_ids.push(record.id.clone());
                continue;
            }
            match load_plugin_definition_with_diagnostics(
                &record.install_path,
                record.kind,
                describe_install_source(&record.source),
                record.kind.marketplace(),
            ) {
                Ok((plugin, diags)) => {
                    self.push_diagnostics(diags);
                    if seen_ids.insert(plugin.metadata().id.clone()) {
                        seen_paths.insert(record.install_path.clone());
                        plugins.push(plugin);
                    }
                }
                Err(PluginError::NotFound(_)) => {
                    self.push_diagnostic(PluginLoadDiagnostic::ManifestMissing {
                        dir: record.install_path.clone(),
                    });
                }
                Err(error) => {
                    self.push_diagnostic(PluginLoadDiagnostic::ManifestParse {
                        dir: record.install_path.clone(),
                        detail: error.to_string(),
                    });
                }
            }
        }

        if !stale_registry_ids.is_empty() {
            for plugin_id in stale_registry_ids {
                registry.plugins.remove(&plugin_id);
            }
            self.store_registry(&registry)?;
        }

        Ok(plugins)
    }

    fn discover_external_directory_plugins(
        &mut self,
        existing_plugins: &[PluginDefinition],
    ) -> Result<Vec<PluginDefinition>, PluginError> {
        let mut plugins = Vec::new();

        for directory in &self.config.external_dirs.clone() {
            for root in discover_plugin_dirs(directory)? {
                match load_plugin_definition_with_diagnostics(
                    &root,
                    PluginKind::External,
                    root.display().to_string(),
                    EXTERNAL_MARKETPLACE,
                ) {
                    Ok((plugin, diags)) => {
                        self.push_diagnostics(diags);
                        if existing_plugins
                            .iter()
                            .chain(plugins.iter())
                            .all(|existing| existing.metadata().id != plugin.metadata().id)
                        {
                            plugins.push(plugin);
                        }
                    }
                    Err(PluginError::NotFound(_)) => {
                        self.push_diagnostic(PluginLoadDiagnostic::ManifestMissing { dir: root });
                    }
                    Err(error) => {
                        self.push_diagnostic(PluginLoadDiagnostic::ManifestParse {
                            dir: root,
                            detail: error.to_string(),
                        });
                    }
                }
            }
        }

        Ok(plugins)
    }

    fn installed_plugin_registry(&mut self) -> Result<PluginRegistry, PluginError> {
        self.sync_bundled_plugins()?;
        let plugins = self.discover_installed_plugins()?;
        Ok(PluginRegistry::new(
            plugins
                .into_iter()
                .map(|plugin| {
                    let enabled = self.is_enabled(plugin.metadata());
                    RegisteredPlugin::new(plugin, enabled)
                })
                .collect(),
        ))
    }

    fn sync_bundled_plugins(&mut self) -> Result<(), PluginError> {
        // When a test or caller supplies an explicit bundled_root override, use
        // it directly (points at a real directory on disk, e.g. a temp dir).
        // Otherwise, materialize the embedded tree from the binary into the
        // user's config directory — this is the normal production path.
        let bundled_root = if let Some(override_root) = &self.config.bundled_root {
            override_root.clone()
        } else {
            materialize_bundled_plugins(&self.config.config_home)?
        };
        let bundled_plugins = discover_plugin_dirs(&bundled_root)?;
        let mut registry = self.load_registry()?;
        let mut changed = false;
        let install_root = self.install_root();
        let mut active_bundled_ids = BTreeSet::new();

        for source_root in bundled_plugins {
            let manifest = match load_plugin_from_directory(&source_root) {
                Ok(m) => m,
                Err(PluginError::NotFound(_)) => {
                    self.push_diagnostic(PluginLoadDiagnostic::ManifestMissing {
                        dir: source_root,
                    });
                    continue;
                }
                Err(error) => {
                    self.push_diagnostic(PluginLoadDiagnostic::ManifestParse {
                        dir: source_root,
                        detail: error.to_string(),
                    });
                    continue;
                }
            };
            let plugin_id = plugin_id(&manifest.name, BUNDLED_MARKETPLACE);
            active_bundled_ids.insert(plugin_id.clone());
            let install_path = install_root.join(sanitize_plugin_id(&plugin_id));
            let now = unix_time_ms();
            let existing_record = registry.plugins.get(&plugin_id);
            let installed_copy_is_valid =
                install_path.exists() && load_plugin_from_directory(&install_path).is_ok();
            let needs_sync = existing_record.is_none_or(|record| {
                record.kind != PluginKind::Bundled
                    || record.version != manifest.version
                    || record.name != manifest.name
                    || record.description != manifest.description
                    || record.install_path != install_path
                    || !record.install_path.exists()
                    || !installed_copy_is_valid
            });

            if !needs_sync {
                continue;
            }

            if install_path.exists() {
                fs::remove_dir_all(&install_path)?;
            }
            copy_dir_all(&source_root, &install_path)?;

            let installed_at_unix_ms =
                existing_record.map_or(now, |record| record.installed_at_unix_ms);
            registry.plugins.insert(
                plugin_id.clone(),
                InstalledPluginRecord {
                    kind: PluginKind::Bundled,
                    id: plugin_id,
                    name: manifest.name,
                    version: manifest.version,
                    description: manifest.description,
                    install_path,
                    source: PluginInstallSource::LocalPath { path: source_root },
                    installed_at_unix_ms,
                    updated_at_unix_ms: now,
                    hub_trust_level: None,
                },
            );
            changed = true;
        }

        let stale_bundled_ids = registry
            .plugins
            .iter()
            .filter_map(|(plugin_id, record)| {
                (record.kind == PluginKind::Bundled && !active_bundled_ids.contains(plugin_id))
                    .then_some(plugin_id.clone())
            })
            .collect::<Vec<_>>();

        for plugin_id in stale_bundled_ids {
            if let Some(record) = registry.plugins.remove(&plugin_id) {
                if record.install_path.exists() {
                    fs::remove_dir_all(&record.install_path)?;
                }
                changed = true;
            }
        }

        if changed {
            self.store_registry(&registry)?;
        }

        Ok(())
    }

    fn is_enabled(&self, metadata: &PluginMetadata) -> bool {
        self.config
            .enabled_plugins
            .get(&metadata.id)
            .copied()
            .unwrap_or(match metadata.kind {
                PluginKind::External => false,
                PluginKind::Builtin | PluginKind::Bundled => metadata.default_enabled,
            })
    }

    fn ensure_known_plugin(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        if self.plugin_registry()?.contains(plugin_id) {
            Ok(())
        } else {
            Err(PluginError::NotFound(format!(
                "plugin `{plugin_id}` is not installed or discoverable"
            )))
        }
    }

    pub(crate) fn load_registry(&self) -> Result<InstalledPluginRegistry, PluginError> {
        let path = self.registry_path();
        match fs::read_to_string(&path) {
            Ok(contents) if contents.trim().is_empty() => Ok(InstalledPluginRegistry::default()),
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(InstalledPluginRegistry::default())
            }
            Err(error) => Err(PluginError::Io(error)),
        }
    }

    pub(crate) fn store_registry(
        &self,
        registry: &InstalledPluginRegistry,
    ) -> Result<(), PluginError> {
        let path = self.registry_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, serde_json::to_string_pretty(registry)?)?;
        fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    pub(crate) fn write_enabled_state(
        &self,
        plugin_id: &str,
        enabled: Option<bool>,
    ) -> Result<(), PluginError> {
        update_settings_json(&self.settings_path(), |root| {
            let enabled_plugins = ensure_object(root, "enabledPlugins");
            match enabled {
                Some(value) => {
                    enabled_plugins.insert(plugin_id.to_string(), Value::Bool(value));
                }
                None => {
                    enabled_plugins.remove(plugin_id);
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Bundled-plugin materialization
// ---------------------------------------------------------------------------

/// Extract the embedded bundled plugin tree to `<config_home>/plugins/bundled/`
/// if the tree has changed since the last extraction (detected via SHA-256
/// fingerprint) or has not been extracted yet.
///
/// Returns the path to the materialized directory. Repeated calls with the
/// same embedded tree are near-zero-cost (fingerprint read + compare only).
///
/// Extraction is atomic: the new tree is written to a sibling
/// `.tmp-<pid>` directory and renamed on success so a partial write never
/// leaves the bundled directory in a corrupt state.
///
/// # Errors
///
/// Returns a `PluginError::Io` if filesystem operations fail.
pub fn materialize_bundled_plugins(config_home: &Path) -> Result<PathBuf, PluginError> {
    let dest = config_home.join("plugins").join("bundled");
    let fingerprint_path = dest.join(BUNDLED_FINGERPRINT_FILE);
    let current_fp = compute_embedded_fingerprint();

    // Fast path: the tree is already up to date.
    if let Ok(stored) = fs::read_to_string(&fingerprint_path) {
        if stored.trim() == current_fp {
            return Ok(dest);
        }
    }

    // Slow path: extract to a temp directory then atomically rename.
    let tmp_dest = config_home
        .join("plugins")
        .join(format!(".tmp-{}", std::process::id()));

    if tmp_dest.exists() {
        fs::remove_dir_all(&tmp_dest)?;
    }
    extract_dir(&BUNDLED_PLUGINS, &tmp_dest, &PathBuf::new())?;

    // Write the fingerprint inside the temp tree before renaming.
    fs::write(tmp_dest.join(BUNDLED_FINGERPRINT_FILE), &current_fp)?;

    // Atomically replace the old tree.
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&tmp_dest, &dest)?;

    Ok(dest)
}

/// Compute a deterministic SHA-256 fingerprint over the embedded file tree.
///
/// Files are visited in sorted path order so the fingerprint is stable across
/// platforms and does not depend on directory enumeration order.
fn compute_embedded_fingerprint() -> String {
    let mut hasher = Sha256::new();
    let mut paths: Vec<&include_dir::File<'_>> = BUNDLED_PLUGINS.files().collect();
    // Sort by path for determinism.
    paths.sort_by_key(|f| f.path());
    for file in paths {
        hasher.update(file.path().to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(file.contents());
        hasher.update(b"\0");
    }
    // Recurse into subdirectories (depth-first, sorted).
    fn hash_dir(dir: &Dir<'_>, hasher: &mut Sha256) {
        let mut subdirs: Vec<&Dir<'_>> = dir.dirs().collect();
        subdirs.sort_by_key(|d| d.path());
        for subdir in subdirs {
            let mut files: Vec<&include_dir::File<'_>> = subdir.files().collect();
            files.sort_by_key(|f| f.path());
            for file in files {
                hasher.update(file.path().to_string_lossy().as_bytes());
                hasher.update(b"\0");
                hasher.update(file.contents());
                hasher.update(b"\0");
            }
            hash_dir(subdir, hasher);
        }
    }
    hash_dir(&BUNDLED_PLUGINS, &mut hasher);
    format!("{:x}", hasher.finalize())
}

/// Recursively write the contents of an embedded `Dir` to `dest / rel_path`.
fn extract_dir(dir: &Dir<'_>, base: &Path, _rel: &PathBuf) -> Result<(), PluginError> {
    // Write files in this directory level.
    for file in dir.files() {
        let out_path = base.join(file.path());
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out_path, file.contents())?;
        // Restore executable bit for shell scripts on Unix.
        #[cfg(unix)]
        if out_path
            .extension()
            .is_some_and(|ext| ext == "sh")
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out_path, fs::Permissions::from_mode(0o755))?;
        }
    }
    // Recurse.
    for subdir in dir.dirs() {
        extract_dir(subdir, base, &PathBuf::new())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Settings JSON helpers
// ---------------------------------------------------------------------------

fn update_settings_json(
    path: &Path,
    mut update: impl FnMut(&mut Map<String, Value>),
) -> Result<(), PluginError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut root = match fs::read_to_string(path) {
        Ok(contents) if !contents.trim().is_empty() => serde_json::from_str::<Value>(&contents)?,
        Ok(_) => Value::Object(Map::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => return Err(PluginError::Io(error)),
    };

    let object = root.as_object_mut().ok_or_else(|| {
        PluginError::InvalidManifest(format!(
            "settings file {} must contain a JSON object",
            path.display()
        ))
    })?;
    update(object);
    fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn ensure_object<'a>(root: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !root.get(key).is_some_and(Value::is_object) {
        root.insert(key.to_string(), Value::Object(Map::new()));
    }
    root.get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object should exist")
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be after epoch")
        .as_millis()
}
