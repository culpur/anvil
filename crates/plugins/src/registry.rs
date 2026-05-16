use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::PluginKind;

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
// Source helpers
// ---------------------------------------------------------------------------

#[must_use]
pub fn describe_install_source(source: &PluginInstallSource) -> String {
    match source {
        PluginInstallSource::LocalPath { path } => path.display().to_string(),
        PluginInstallSource::GitUrl { url } => url.clone(),
    }
}
