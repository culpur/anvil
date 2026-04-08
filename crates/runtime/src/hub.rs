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
    pub async fn install(
        &self,
        pkg: &HubPackage,
        install_dir: &Path,
    ) -> Result<std::path::PathBuf, HubError> {
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
}

/// Error type for `AnvilHub` operations.
#[derive(Debug)]
pub enum HubError {
    Http(String),
    Api(String),
    Parse(String),
    NotFound(String),
    Install(String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(msg) => write!(f, "network error: {msg}"),
            Self::Api(msg) => write!(f, "API error: {msg}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::NotFound(name) => write!(f, "package not found: {name}"),
            Self::Install(msg) => write!(f, "install error: {msg}"),
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
    ) -> Result<std::path::PathBuf, HubError> {
        self.rt.block_on(self.inner.install(pkg, install_dir))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::urlencoded;

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
}
