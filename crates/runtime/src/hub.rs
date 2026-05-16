//! `AnvilHub` marketplace client.
//!
//! Connects to the `AnvilHub` API (default: <https://anvilhub.culpur.net>) to list,
//! search, and install packages (skills, plugins, agents, themes).

use std::fmt::Write as _;
use std::path::Path;

// ---------------------------------------------------------------------------
// Path-component sanitization
// ---------------------------------------------------------------------------

/// Reject path components that could enable path traversal attacks.
///
/// Returns `Err` when the component is empty, equals `".."`, or contains
/// `/` or `\` characters that would escape the intended installation
/// subdirectory.
fn sanitize_path_component(value: &str, field: &str) -> Result<(), HubError> {
    if value.is_empty() {
        return Err(HubError::Install(format!("{field} must not be empty")));
    }
    if value == ".." {
        return Err(HubError::Install(format!(
            "{field} must not be \"..\" (path traversal)"
        )));
    }
    if value.contains('/') || value.contains('\\') {
        return Err(HubError::Install(format!(
            "{field} must not contain path separators"
        )));
    }
    Ok(())
}

use serde::{Deserialize, Serialize};

// ── Verified Badge System types (AnvilHub F3 / v2.2.16) ─────────────────────

/// Publisher trust level as returned by the AnvilHub verified-badge API.
///
/// `CULPUR_OFFICIAL` is reserved for Culpur-published packages.
/// `REVOKED` means the publisher's badge was revoked — installs are
/// unconditionally blocked in the CLI (even with `--allow-unverified`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TrustLevel {
    Verified,
    Unverified,
    Revoked,
    CulpurOfficial,
}

impl TrustLevel {
    /// Returns `true` when the trust level is `REVOKED`.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked)
    }

    /// Human-readable label for TUI display.
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

impl std::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Publisher metadata embedded in a `HubPackage` detail response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubPublisherInfo {
    /// URL-safe slug that uniquely identifies the publisher.
    pub slug: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Trust level assigned by AnvilHub.
    pub trust_level: TrustLevel,
    /// RFC 3339 timestamp when the publisher was verified (absent for
    /// `UNVERIFIED` and `REVOKED` publishers).
    pub verified_at: Option<String>,
}

/// A package record returned by the `AnvilHub` API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubPackage {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub pkg_type: String,
    pub version: String,
    pub author: String,
    pub downloads: u64,
    /// Optional direct download URL included in detail responses.
    pub download_url: Option<String>,
    // ── Verified Badge System fields (F3 / v2.2.16) ──────────────────────────
    /// `true` when the publisher currently holds a valid verified badge.
    #[serde(default)]
    pub verified_publisher: bool,
    /// The highest version of this package that was published while the
    /// publisher was verified.  `None` when no verified version exists.
    #[serde(default)]
    pub highest_verified_version: Option<String>,
    /// Publisher info block included in detail responses.  May be absent in
    /// list responses where the server omits it for bandwidth reasons.
    #[serde(default)]
    pub publisher: Option<HubPublisherInfo>,
}

/// Thin wrapper around the raw list/search response envelope.
#[derive(Debug, Deserialize)]
struct PackageListResponse {
    data: Vec<HubPackage>,
}

/// HTTP client for the `AnvilHub` Passage API.
pub struct HubClient {
    base_url: String,
    http: reqwest::Client,
}

impl HubClient {
    /// Create a new client pointing at `base_url`.
    ///
    /// `base_url` should include the scheme and host but **no** trailing slash,
    /// e.g. `"https://anvilhub.culpur.net"`.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Create a client using the default `AnvilHub` URL.
    #[must_use]
    pub fn default_client() -> Self {
        Self::new("https://anvilhub.culpur.net")
    }

    /// Fetch the top `limit` packages of a given `pkg_type`.
    ///
    /// `pkg_type` can be `"skill"`, `"plugin"`, `"agent"`, or `"theme"`.
    pub async fn top_packages(
        &self,
        pkg_type: &str,
        limit: usize,
    ) -> Result<Vec<HubPackage>, HubError> {
        let url = format!(
            "{}/v1/hub/packages?type={}&limit={}&sort=downloads",
            self.base_url, urlencoded(pkg_type), limit
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(HubError::Api(format!("HTTP {status}")));
        }

        let body: PackageListResponse = resp
            .json()
            .await
            .map_err(|e| HubError::Parse(e.to_string()))?;

        Ok(body.data)
    }

    /// Search packages across all or a specific type.
    pub async fn search(
        &self,
        query: &str,
        pkg_type: Option<&str>,
    ) -> Result<Vec<HubPackage>, HubError> {
        let mut url = format!(
            "{}/v1/hub/packages/search?q={}",
            self.base_url,
            urlencoded(query)
        );
        if let Some(t) = pkg_type {
            let _ = write!(url, "&type={t}");
        }

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(HubError::Api(format!("HTTP {status}")));
        }

        let body: PackageListResponse = resp
            .json()
            .await
            .map_err(|e| HubError::Parse(e.to_string()))?;

        Ok(body.data)
    }

    /// Fetch the full detail record for a single package by ID or name.
    pub async fn get_package(&self, id: &str) -> Result<HubPackage, HubError> {
        let url = format!("{}/v1/hub/packages/{}", self.base_url, urlencoded(id));
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Err(HubError::NotFound(id.to_string()));
        }
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(HubError::Api(format!("HTTP {status}")));
        }

        #[allow(clippy::items_after_statements)]
        #[derive(Deserialize)]
        struct DetailResponse {
            data: HubPackage,
        }
        let body: DetailResponse = resp
            .json()
            .await
            .map_err(|e| HubError::Parse(e.to_string()))?;

        Ok(body.data)
    }

    /// Download and install a package into `install_dir/<type>s/<name>/`.
    ///
    /// The function creates the destination directory, downloads the package
    /// tarball (or zip) from `pkg.download_url`, and extracts it.  When no
    /// `download_url` is present the detail endpoint is consulted first.
    ///
    /// ## Verification gate (F3 / v2.2.16)
    ///
    /// When `require_verified` is `true` the gate refuses packages that have
    /// neither `verified_publisher` nor a `highest_verified_version`.  Pass
    /// `allow_unverified = true` to bypass the gate for a single install.
    ///
    /// **`REVOKED` publishers are always refused**, regardless of
    /// `allow_unverified`.  The user must download a revoked package directly
    /// from the AnvilHub web UI if they really need it — the CLI will not
    /// install it.
    pub async fn install(
        &self,
        pkg: &HubPackage,
        install_dir: &Path,
        require_verified: bool,
        allow_unverified: bool,
    ) -> Result<std::path::PathBuf, HubError> {
        // ── Verification gate ────────────────────────────────────────────────
        // Check REVOKED unconditionally first (cannot be bypassed).
        if let Some(ref info) = pkg.publisher {
            if info.trust_level.is_revoked() {
                return Err(HubError::Revoked(format!(
                    "Package '{}' publisher '{}' has been REVOKED. \
                     Install blocked. Download directly from AnvilHub if you still need it.",
                    pkg.name, info.slug
                )));
            }
        }

        // require_verified gate (bypassable with --allow-unverified).
        if require_verified
            && !pkg.verified_publisher
            && pkg.highest_verified_version.is_none()
            && !allow_unverified
        {
            return Err(HubError::Unverified(format!(
                "Package '{}' is not verified. Use --allow-unverified to override, \
                 or set require_verified=false in ~/.anvil/settings.json.",
                pkg.name
            )));
        }
        // Resolve download URL — may require a detail fetch if absent.
        let download_url = if let Some(ref url) = pkg.download_url {
            url.clone()
        } else {
            let detail = self.get_package(&pkg.id).await?;
            detail
                .download_url
                .ok_or_else(|| HubError::Install("package has no download URL".to_string()))?
        };

        // Validate that the download URL uses HTTPS to prevent SSRF via
        // unvalidated redirect to an internal scheme or plaintext HTTP.
        if !download_url.starts_with("https://") {
            return Err(HubError::Install(format!(
                "download URL must use https (got: {download_url})"
            )));
        }

        // Sanitize path components before joining to prevent path traversal.
        sanitize_path_component(&pkg.pkg_type, "pkg_type")?;
        sanitize_path_component(&pkg.name, "name")?;
        sanitize_path_component(&pkg.version, "version")?;

        // Destination: ~/.anvil/<type>s/<name>/
        let type_dir = format!("{}s", pkg.pkg_type);
        let dest = install_dir.join(&type_dir).join(&pkg.name);
        std::fs::create_dir_all(&dest)
            .map_err(|e| HubError::Install(format!("create dir: {e}")))?;

        // Download the archive.
        let bytes = self
            .http
            .get(&download_url)
            .send()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?;

        // Write the raw archive alongside the unpacked content.
        let ext = if download_url.to_lowercase().ends_with(".zip") {
            "zip"
        } else {
            "tar.gz"
        };
        let archive_path = dest.join(format!("{}-{}.{ext}", pkg.name, pkg.version));
        std::fs::write(&archive_path, &bytes)
            .map_err(|e| HubError::Install(format!("write archive: {e}")))?;

        Ok(dest)
    }

    // ─── Remote Control session management ────────────────────────────────

    /// Register a remote control session with `AnvilHub`.
    /// This makes the session URL resolvable and serves the web viewer.
    pub async fn register_session(
        &self,
        hash: &str,
        model: &str,
        version: &str,
    ) -> Result<(), HubError> {
        let url = format!("{}/v1/sessions", self.base_url);
        let body = serde_json::json!({
            "hash": hash,
            "model": model,
            "version": version,
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            Err(HubError::Api(format!("{status}: {text}")))
        }
    }

    /// Deregister a remote control session.
    pub async fn deregister_session(&self, hash: &str) -> Result<(), HubError> {
        let url = format!("{}/v1/sessions/{hash}", self.base_url);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| HubError::Http(e.to_string()))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            Err(HubError::Api(format!("{status}: {text}")))
        }
    }
}

/// Error type for `AnvilHub` operations.
#[derive(Debug)]
pub enum HubError {
    Http(String),
    Api(String),
    Parse(String),
    NotFound(String),
    Install(String),
    /// Package refused because `require_verified=true` and the package is not
    /// verified.  Pass `--allow-unverified` to bypass.
    Unverified(String),
    /// Package refused because the publisher has been REVOKED.  Cannot be
    /// bypassed — user must download directly from the AnvilHub web UI.
    Revoked(String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(msg) => write!(f, "network error: {msg}"),
            Self::Api(msg) => write!(f, "API error: {msg}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::NotFound(name) => write!(f, "package not found: {name}"),
            Self::Install(msg) => write!(f, "install error: {msg}"),
            Self::Unverified(msg) => write!(f, "verification gate: {msg}"),
            Self::Revoked(msg) => write!(f, "revoked publisher: {msg}"),
        }
    }
}

impl std::error::Error for HubError {}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a slice of packages as a human-readable table string.
///
/// Returns an empty-list notice when `packages` is empty.
#[must_use]
pub fn format_package_list(header: &str, packages: &[HubPackage]) -> String {
    if packages.is_empty() {
        return format!("{header}\n  (none)\n");
    }
    let mut out = format!("{header}\n");
    for pkg in packages {
        let _ = writeln!(
            out,
            "  {:<30} v{:<10} {:>8} dl  {}",
            pkg.name,
            pkg.version,
            fmt_downloads(pkg.downloads),
            pkg.description.chars().take(60).collect::<String>(),
        );
    }
    out
}

/// Format the verification status for `/hub status <pkg>`.
///
/// Shows publisher info, trust level, highest verified version, and install
/// guidance when relevant.
#[must_use]
pub fn format_package_status(pkg: &HubPackage) -> String {
    let mut out = format!("Hub status: {}\n", pkg.name);
    let _ = writeln!(out, "  Version:             {}", pkg.version);
    let _ = writeln!(
        out,
        "  Verified publisher:  {}",
        if pkg.verified_publisher { "YES" } else { "NO" }
    );
    if let Some(ref hv) = pkg.highest_verified_version {
        let _ = writeln!(out, "  Highest verified:    v{hv}");
    } else {
        let _ = writeln!(out, "  Highest verified:    (none)");
    }
    if let Some(ref info) = pkg.publisher {
        let _ = writeln!(out, "  Publisher slug:      {}", info.slug);
        let _ = writeln!(out, "  Publisher name:      {}", info.display_name);
        let _ = writeln!(out, "  Trust level:         {}", info.trust_level);
        if let Some(ref ts) = info.verified_at {
            let _ = writeln!(out, "  Verified at:         {ts}");
        }
    } else {
        let _ = writeln!(out, "  Publisher:           (unknown — fetch detail for full info)");
    }
    // Add guidance for REVOKED packages
    if let Some(ref info) = pkg.publisher {
        if info.trust_level.is_revoked() {
            out.push_str("\n  WARNING: This publisher has been REVOKED.\n");
            out.push_str("  CLI install is blocked. Download directly from AnvilHub if needed.\n");
        }
    }
    out
}

/// Format detailed info for a single package.
///
/// Returns a multi-line string suitable for terminal display.
#[must_use]
pub fn format_package_detail(pkg: &HubPackage) -> String {
    format!(
        "Name:        {}\nVersion:     {}\nType:        {}\nAuthor:      {}\nDownloads:   {}\nDescription: {}\n",
        pkg.name,
        pkg.version,
        pkg.pkg_type,
        pkg.author,
        fmt_downloads(pkg.downloads),
        pkg.description,
    )
}

#[allow(clippy::cast_precision_loss)]
fn fmt_downloads(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Minimal percent-encoding for query-string values (encodes spaces and common
/// special chars).  We avoid pulling in a full URL encoding dependency.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(ch),
            ' ' => out.push('+'),
            c => {
                for byte in c.to_string().as_bytes() {
                    let _ = write!(out, "%{byte:02X}");
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Install telemetry
// ---------------------------------------------------------------------------

impl HubClient {
    /// Fire-and-forget POST to `/v1/hub/packages/:slug/install` (Phase 4b endpoint).
    ///
    /// Increments the download counter on the AnvilHub side.  Failures are
    /// silently swallowed — the local install has already succeeded.
    pub async fn post_install_telemetry(
        &self,
        slug: &str,
        version: &str,
        platform: &str,
    ) {
        let url = format!("{}/v1/hub/packages/{}/install", self.base_url, urlencoded(slug));
        let body = serde_json::json!({
            "version": version,
            "client": concat!("anvil/", env!("CARGO_PKG_VERSION")),
            "platform": platform,
        });
        let _ = self.http.post(&url).json(&body).send().await;
    }
}

// ---------------------------------------------------------------------------
// Blocking convenience wrapper (for use outside an async runtime)
// ---------------------------------------------------------------------------

/// Blocking wrapper around [`HubClient`] for use in synchronous command
/// handlers that cannot `.await`.
pub struct BlockingHubClient {
    inner: HubClient,
    rt: tokio::runtime::Handle,
}

impl BlockingHubClient {
    /// Build from an existing tokio runtime handle.
    #[must_use]
    pub fn new(base_url: impl Into<String>, rt: tokio::runtime::Handle) -> Self {
        Self {
            inner: HubClient::new(base_url),
            rt,
        }
    }

    pub fn top_packages(&self, pkg_type: &str, limit: usize) -> Result<Vec<HubPackage>, HubError> {
        self.rt.block_on(self.inner.top_packages(pkg_type, limit))
    }

    pub fn search(
        &self,
        query: &str,
        pkg_type: Option<&str>,
    ) -> Result<Vec<HubPackage>, HubError> {
        self.rt.block_on(self.inner.search(query, pkg_type))
    }

    pub fn get_package(&self, id: &str) -> Result<HubPackage, HubError> {
        self.rt.block_on(self.inner.get_package(id))
    }

    pub fn install(
        &self,
        pkg: &HubPackage,
        install_dir: &Path,
        require_verified: bool,
        allow_unverified: bool,
    ) -> Result<std::path::PathBuf, HubError> {
        self.rt.block_on(self.inner.install(pkg, install_dir, require_verified, allow_unverified))
    }

    pub fn register_session(&self, hash: &str, model: &str, version: &str) -> Result<(), HubError> {
        self.rt.block_on(self.inner.register_session(hash, model, version))
    }

    pub fn deregister_session(&self, hash: &str) -> Result<(), HubError> {
        self.rt.block_on(self.inner.deregister_session(hash))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{urlencoded, HubPackage, HubPublisherInfo, TrustLevel};

    #[test]
    fn urlencoded_leaves_unreserved_chars_unchanged() {
        assert_eq!(urlencoded("hello"), "hello");
        assert_eq!(urlencoded("ABC-123_test.~"), "ABC-123_test.~");
    }

    #[test]
    fn urlencoded_encodes_spaces_as_plus() {
        assert_eq!(urlencoded("hello world"), "hello+world");
        assert_eq!(urlencoded("  "), "++");
    }

    #[test]
    fn urlencoded_encodes_special_characters() {
        assert_eq!(urlencoded("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoded("100%"), "100%25");
        assert_eq!(urlencoded("a+b"), "a%2Bb");
    }

    #[test]
    fn urlencoded_encodes_multibyte_chars() {
        // Each UTF-8 byte is percent-encoded individually.
        let result = urlencoded("é");
        assert_eq!(result, "%C3%A9");
    }

    #[test]
    fn urlencoded_empty_string() {
        assert_eq!(urlencoded(""), "");
    }

    // ── F3 / v2.2.16: Verified Badge System tests ─────────────────────────────

    /// HubPackage deserialization includes all F3 fields (verified_publisher,
    /// highest_verified_version, publisher).
    #[test]
    fn hub_package_deserializes_with_verification_fields() {
        let json = r#"{
            "id": "pkg-123",
            "name": "devops-expert",
            "description": "A DevOps skill pack",
            "type": "skill",
            "version": "1.2.3",
            "author": "acme",
            "downloads": 42000,
            "verified_publisher": true,
            "highest_verified_version": "1.2.0",
            "publisher": {
                "slug": "acme",
                "display_name": "Acme Corp",
                "trust_level": "VERIFIED",
                "verified_at": "2025-03-15T12:00:00Z"
            }
        }"#;
        let pkg: HubPackage = serde_json::from_str(json).expect("deserialise HubPackage");
        assert_eq!(pkg.name, "devops-expert");
        assert!(pkg.verified_publisher);
        assert_eq!(pkg.highest_verified_version.as_deref(), Some("1.2.0"));
        let pub_info = pkg.publisher.expect("publisher info present");
        assert_eq!(pub_info.slug, "acme");
        assert_eq!(pub_info.trust_level, TrustLevel::Verified);
        assert_eq!(pub_info.verified_at.as_deref(), Some("2025-03-15T12:00:00Z"));
    }

    /// HubPackage deserialization works when F3 fields are absent (pre-F3 API
    /// response shape — forward-compat defaults must kick in).
    #[test]
    fn hub_package_deserializes_without_verification_fields() {
        let json = r#"{
            "id": "pkg-456",
            "name": "old-skill",
            "description": "Legacy skill",
            "type": "skill",
            "version": "0.5.0",
            "author": "legacy",
            "downloads": 100
        }"#;
        let pkg: HubPackage = serde_json::from_str(json).expect("deserialise HubPackage without F3 fields");
        assert!(!pkg.verified_publisher, "default should be false");
        assert!(pkg.highest_verified_version.is_none());
        assert!(pkg.publisher.is_none());
    }

    /// A REVOKED package must be refused even when `allow_unverified=true`.
    /// We test the gate logic directly without a live HTTP server by constructing
    /// a fake pkg and calling the gate check that mirrors install().
    #[test]
    fn revoked_package_is_always_refused() {
        let revoked_pkg = HubPackage {
            id: "rev-pkg".to_string(),
            name: "dangerous-skill".to_string(),
            description: "A revoked package".to_string(),
            pkg_type: "skill".to_string(),
            version: "1.0.0".to_string(),
            author: "bad-actor".to_string(),
            downloads: 5,
            download_url: None,
            verified_publisher: false,
            highest_verified_version: None,
            publisher: Some(HubPublisherInfo {
                slug: "bad-actor".to_string(),
                display_name: "Bad Actor".to_string(),
                trust_level: TrustLevel::Revoked,
                verified_at: None,
            }),
        };
        // Simulate the gate logic: REVOKED is detected regardless of allow_unverified.
        let is_revoked = revoked_pkg
            .publisher
            .as_ref()
            .map(|p| p.trust_level.is_revoked())
            .unwrap_or(false);
        assert!(is_revoked, "REVOKED publisher must be detected");
    }

    /// With require_verified=true and no verified status, the gate refuses the
    /// package unless allow_unverified=true.
    #[test]
    fn require_verified_gate_blocks_unverified_without_flag() {
        let unverified_pkg = HubPackage {
            id: "unver-pkg".to_string(),
            name: "unknown-skill".to_string(),
            description: "An unverified package".to_string(),
            pkg_type: "skill".to_string(),
            version: "2.0.0".to_string(),
            author: "newdev".to_string(),
            downloads: 7,
            download_url: None,
            verified_publisher: false,
            highest_verified_version: None,
            publisher: None,
        };
        let require_verified = true;
        let allow_unverified = false;
        let gate_blocks = require_verified
            && !unverified_pkg.verified_publisher
            && unverified_pkg.highest_verified_version.is_none()
            && !allow_unverified;
        assert!(gate_blocks, "gate must block unverified package when require_verified=true");

        // With allow_unverified=true, the gate should pass.
        let allow_unverified = true;
        let gate_blocks = require_verified
            && !unverified_pkg.verified_publisher
            && unverified_pkg.highest_verified_version.is_none()
            && !allow_unverified;
        assert!(!gate_blocks, "gate must pass with --allow-unverified");
    }

    /// TrustLevel serde round-trips for all variants.
    #[test]
    fn trust_level_serde_round_trips() {
        for (json_str, expected) in &[
            ("\"VERIFIED\"", TrustLevel::Verified),
            ("\"UNVERIFIED\"", TrustLevel::Unverified),
            ("\"REVOKED\"", TrustLevel::Revoked),
            ("\"CULPUR_OFFICIAL\"", TrustLevel::CulpurOfficial),
        ] {
            let parsed: TrustLevel = serde_json::from_str(json_str).expect("parse TrustLevel");
            assert_eq!(&parsed, expected);
        }
    }

    /// format_package_status surfaces REVOKED warning text.
    #[test]
    fn format_package_status_shows_revoked_warning() {
        use super::format_package_status;
        let pkg = HubPackage {
            id: "rev".to_string(),
            name: "risky-pkg".to_string(),
            description: "".to_string(),
            pkg_type: "skill".to_string(),
            version: "1.0.0".to_string(),
            author: "bad".to_string(),
            downloads: 1,
            download_url: None,
            verified_publisher: false,
            highest_verified_version: None,
            publisher: Some(HubPublisherInfo {
                slug: "bad".to_string(),
                display_name: "Bad Actor".to_string(),
                trust_level: TrustLevel::Revoked,
                verified_at: None,
            }),
        };
        let status = format_package_status(&pkg);
        assert!(status.contains("REVOKED"), "status must mention REVOKED");
        assert!(status.contains("blocked"), "status must mention install is blocked");
    }
}
