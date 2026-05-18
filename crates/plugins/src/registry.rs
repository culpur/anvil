use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{PluginError, PluginKind};

// ---------------------------------------------------------------------------
// Install source
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginInstallSource {
    LocalPath { path: PathBuf },
    GitUrl { url: String },
}

// ---------------------------------------------------------------------------
// AnvilHub trust-level snapshot (F3 / v2.2.16)
// ---------------------------------------------------------------------------

/// Snapshot of the AnvilHub publisher trust level recorded at install time.
///
/// Stored inside `InstalledPluginRecord.hub_trust_level` so the `/plugin
/// update` handler can detect badge revocations that occurred after install.
///
/// This mirrors `runtime::hub::TrustLevel` but is re-declared here to avoid a
/// circular dependency between the `plugins` and `runtime` crates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PluginTrustLevel {
    Verified,
    #[default]
    Unverified,
    Revoked,
    CulpurOfficial,
}

impl PluginTrustLevel {
    /// Returns `true` when the stored trust level is `REVOKED`.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked)
    }

    /// Human-readable label for log/TUI display.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Verified => "VERIFIED",
            Self::Unverified => "UNVERIFIED",
            Self::Revoked => "REVOKED",
            Self::CulpurOfficial => "CULPUR_OFFICIAL",
        }
    }
}

// ---------------------------------------------------------------------------
// Installed registry record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPluginRecord {
    #[serde(default = "default_plugin_kind")]
    pub kind: PluginKind,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub install_path: PathBuf,
    pub source: PluginInstallSource,
    pub installed_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    /// Publisher trust level recorded at install time.  `None` for packages
    /// installed before v2.2.16 (forward-compat: treated as `Unverified`).
    #[serde(default)]
    pub hub_trust_level: Option<PluginTrustLevel>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPluginRegistry {
    #[serde(default)]
    pub plugins: BTreeMap<String, InstalledPluginRecord>,
}

const fn default_plugin_kind() -> PluginKind {
    PluginKind::External
}

// ---------------------------------------------------------------------------
// AnvilHub on-disk trust registry (v2.2.17 / task #569)
// ---------------------------------------------------------------------------

/// One entry in `~/.anvil/hub-registry.json`, the on-disk cache of AnvilHub
/// trust info populated by `anvil skill install` / `anvil plugin install`
/// from the `api.culpur.net/v1/hub/packages/<slug>` response.
///
/// Looked up by plugin **name** (the manifest `name`, which is the AnvilHub
/// slug) so it does not depend on Anvil's internal `name@marketplace` id
/// format.  A missing entry leaves `hub_trust_level = None` — the plugin
/// still loads fine; it just shows no verified badge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubRegistryEntry {
    /// Trust level captured from the AnvilHub API at install time.
    #[serde(default)]
    pub trust_level: Option<PluginTrustLevel>,
    /// `true` when the publisher currently holds a valid verified badge.
    /// Mirrors `HubPackage.verified_publisher` from the AnvilHub API.
    #[serde(default)]
    pub verified_publisher: Option<bool>,
    /// Highest version published while the publisher was verified.
    /// Mirrors `HubPackage.highest_verified_version`.
    #[serde(default)]
    pub highest_verified_version: Option<String>,
}

/// On-disk AnvilHub trust registry stored at `~/.anvil/hub-registry.json`.
///
/// `packages` is keyed by the AnvilHub package slug (which is also the
/// plugin manifest `name`).  Loading is **always** offline — registry
/// population happens on install, never at plugin-load time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubRegistry {
    #[serde(default)]
    pub packages: BTreeMap<String, HubRegistryEntry>,
}

impl HubRegistry {
    /// Read the hub registry from `path`.  Returns an empty registry when the
    /// file is missing or empty — Anvil works fine offline.
    ///
    /// # Errors
    /// Surfaces I/O errors other than `NotFound` and JSON parse errors so
    /// callers (e.g. `anvil hub status`) can decide whether to surface them.
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        match fs::read_to_string(path) {
            Ok(contents) if contents.trim().is_empty() => Ok(Self::default()),
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(PluginError::Io(error)),
        }
    }

    /// Read the hub registry, silently returning an empty registry on any
    /// failure (corrupt JSON, permission denied, etc.).  Used by the plugin
    /// loader which must never refuse to load a plugin because the on-disk
    /// trust cache is broken — the badge surface is purely informational.
    #[must_use]
    pub fn load_or_empty(path: &Path) -> Self {
        Self::load(path).unwrap_or_default()
    }

    /// Look up an entry by plugin name (AnvilHub slug).
    #[must_use]
    pub fn get(&self, plugin_name: &str) -> Option<&HubRegistryEntry> {
        self.packages.get(plugin_name)
    }

    /// Returns the trust level recorded for `plugin_name`, if any.
    #[must_use]
    pub fn trust_level_for(&self, plugin_name: &str) -> Option<PluginTrustLevel> {
        self.packages
            .get(plugin_name)
            .and_then(|entry| entry.trust_level.clone())
    }

    /// Persist the registry to `path` atomically (write to `<path>.tmp` then
    /// rename).  Used by the install pipeline to update the on-disk cache
    /// after a successful `api.culpur.net/v1/hub/packages/<slug>` call.
    ///
    /// # Errors
    /// Surfaces filesystem errors and JSON-serialization errors.
    pub fn store(&self, path: &Path) -> Result<(), PluginError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Source helpers
// ---------------------------------------------------------------------------

#[must_use]
pub fn describe_install_source(source: &PluginInstallSource) -> String {
    match source {
        PluginInstallSource::LocalPath { path } => path.display().to_string(),
        PluginInstallSource::GitUrl { url } => url.clone(),
    }
}
