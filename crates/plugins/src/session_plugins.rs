//! Session-scoped plugin sources for FEAT-42.
//!
//! Adds two surface flags to Anvil:
//!   * `--plugin-dir <path>` — when `<path>` ends in `.zip`, the archive is
//!     extracted to a session-scoped temp directory and that directory is
//!     loaded as an external plugin source.  Plain directories pass through
//!     unchanged (the existing loader already handles them).
//!   * `--plugin-url <https://...>` — fetches a `.zip` archive from an
//!     HTTPS URL, validates it, extracts it to a session-scoped temp
//!     directory, and loads it as an external plugin source.  Useful for
//!     "try this plugin without installing" + AnvilHub publish-then-test.
//!
//! Security posture (v1):
//!   * URLs MUST be HTTPS — `http://` is rejected at parse time.
//!   * Downloads validate the ZIP local-file-header magic bytes (`PK\x03\x04`)
//!     before unpacking.
//!   * Extraction is path-traversal safe: `..` components, absolute paths,
//!     and Windows drive-letter prefixes are rejected.
//!   * Extracted size is capped (default 50 MiB) to defend against zip
//!     bombs.  This is a safe default; a future revision will let users
//!     override it via config.
//!   * Optional SHA-256 verification is supported via `prepare_url_source`'s
//!     `expected_sha256` argument (`--plugin-sha256 HEX`).  When omitted the
//!     download is treated as trust-on-first-use; callers should warn the
//!     operator.  This is intentional for v1: AnvilHub publish loops want
//!     friction-free retries, but we want the hook ready for prod hardening.
//!
//! Lifetime: each call returns a `PreparedPluginSource` whose `Drop` impl
//! removes the temp directory.  The CLI keeps these alive for the duration
//! of the session and lets them clean up on exit.  A best-effort sweep at
//! startup removes any stale `anvil-session-plugins-*` directories from
//! previous crashes.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::PluginError;

/// Default cap on the total extracted size of a session plugin archive.
/// Defends against zip bombs.  50 MiB is generous for plugin manifests +
/// shell scripts but tight enough to fail fast on hostile archives.
pub const DEFAULT_MAX_EXTRACTED_BYTES: u64 = 50 * 1024 * 1024;

/// Cap on a single download's body size, matched to the extraction cap.
/// Prevents a slow-loris-style infinite-stream attack.
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = DEFAULT_MAX_EXTRACTED_BYTES;

/// Prefix used for session-scoped temp dirs.  Stable so a stale-sweep at
/// startup can find leftovers from a crashed previous run.
const SESSION_TMP_PREFIX: &str = "anvil-session-plugins-";

/// Local file header magic for a ZIP archive (`PK\x03\x04`).  We validate
/// this before handing bytes to the `zip` crate, so a non-zip file (e.g. an
/// HTML error page from a misconfigured CDN) fails with a clean error
/// instead of an opaque crate-internal panic.
const ZIP_MAGIC: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

/// A session-scoped plugin source whose extracted contents live in a temp
/// directory until this value is dropped.
#[derive(Debug)]
pub struct PreparedPluginSource {
    /// Root of the extracted plugin (the directory the loader scans).
    root: PathBuf,
    /// Owned temp dir; removed in `Drop`.  `None` when the source was a
    /// plain pass-through directory we don't own.
    owned_temp_dir: Option<PathBuf>,
}

impl PreparedPluginSource {
    /// Returns the directory the plugin loader should scan.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Wraps a pre-existing directory we don't own (no cleanup on drop).
    #[must_use]
    pub fn passthrough(root: PathBuf) -> Self {
        Self {
            root,
            owned_temp_dir: None,
        }
    }
}

impl Drop for PreparedPluginSource {
    fn drop(&mut self) {
        if let Some(dir) = self.owned_temp_dir.take() {
            // Best-effort: a leftover dir is harmless and a startup sweep
            // will catch it next run.
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

/// Resolve a `--plugin-dir <path>` argument.  If the path is a directory,
/// return it as a pass-through.  If it ends in `.zip` (case-insensitive)
/// and is a regular file, extract it to a session temp dir.
pub fn prepare_plugin_dir_source(input: &Path) -> Result<PreparedPluginSource, PluginError> {
    let metadata = fs::metadata(input).map_err(|error| {
        PluginError::NotFound(format!(
            "--plugin-dir {} could not be read: {error}",
            input.display()
        ))
    })?;

    if metadata.is_dir() {
        return Ok(PreparedPluginSource::passthrough(input.to_path_buf()));
    }

    if !is_zip_path(input) {
        return Err(PluginError::InvalidManifest(format!(
            "--plugin-dir {} is not a directory or .zip archive",
            input.display()
        )));
    }

    let bytes = read_file_capped(input, DEFAULT_MAX_DOWNLOAD_BYTES)?;
    extract_validated_zip(&bytes, "plugin-dir")
}

/// Resolve a `--plugin-url <url>` argument.  Downloads the archive over
/// HTTPS, optionally verifies its SHA-256, and extracts it to a session
/// temp dir.
pub fn prepare_plugin_url_source(
    url: &str,
    expected_sha256: Option<&str>,
) -> Result<PreparedPluginSource, PluginError> {
    validate_https_url(url)?;
    if let Some(hex) = expected_sha256 {
        validate_sha256_hex(hex)?;
    }

    let bytes = download_zip_capped(url, DEFAULT_MAX_DOWNLOAD_BYTES)?;

    if let Some(expected) = expected_sha256 {
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(PluginError::InvalidManifest(format!(
                "--plugin-url SHA-256 mismatch: expected {expected}, got {actual}",
            )));
        }
    }

    extract_validated_zip(&bytes, "plugin-url")
}

/// Process-global registry of prepared session plugin sources.
///
/// `parse_args` populates this; `build_plugin_manager` reads it and appends
/// the resulting paths to `PluginManagerConfig::external_dirs`.  Held in a
/// `Mutex<Vec>` so the temp dirs survive for the duration of the session
/// and are cleaned up via `Drop` at exit.
static SESSION_SOURCES: std::sync::Mutex<Vec<PreparedPluginSource>> =
    std::sync::Mutex::new(Vec::new());

/// Register a prepared session source.  The caller transfers ownership;
/// the registry keeps the value alive (and thus the temp dir extracted)
/// until the process exits.
pub fn register_session_source(source: PreparedPluginSource) {
    if let Ok(mut sources) = SESSION_SOURCES.lock() {
        sources.push(source);
    }
}

/// Snapshot the directories currently registered as session sources.  The
/// returned paths point inside still-alive temp dirs (or pass-through
/// directories) and are safe to add to `PluginManagerConfig::external_dirs`.
#[must_use]
pub fn session_source_dirs() -> Vec<PathBuf> {
    SESSION_SOURCES
        .lock()
        .map(|sources| sources.iter().map(|s| s.root().to_path_buf()).collect())
        .unwrap_or_default()
}

/// Best-effort cleanup of leftover session temp dirs from previous runs.
/// Called at session start; never fails the boot.
pub fn sweep_stale_session_dirs() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(SESSION_TMP_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn is_zip_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

fn validate_https_url(url: &str) -> Result<(), PluginError> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if url.starts_with("http://") {
        return Err(PluginError::InvalidManifest(
            "--plugin-url must be HTTPS; refusing to fetch over plaintext http://".to_string(),
        ));
    }
    Err(PluginError::InvalidManifest(format!(
        "--plugin-url must start with https:// (got `{url}`)",
    )))
}

fn validate_sha256_hex(hex: &str) -> Result<(), PluginError> {
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(PluginError::InvalidManifest(format!(
            "--plugin-sha256 must be 64 hex characters (got {} chars)",
            hex.len()
        )));
    }
    Ok(())
}

fn read_file_capped(path: &Path, cap: u64) -> Result<Vec<u8>, PluginError> {
    let mut file = File::open(path).map_err(PluginError::Io)?;
    let mut buf = Vec::new();
    let mut limited = (&mut file).take(cap.saturating_add(1));
    limited.read_to_end(&mut buf).map_err(PluginError::Io)?;
    if buf.len() as u64 > cap {
        return Err(PluginError::InvalidManifest(format!(
            "{} exceeds the {}-byte plugin archive cap",
            path.display(),
            cap
        )));
    }
    Ok(buf)
}

fn download_zip_capped(url: &str, cap: u64) -> Result<Vec<u8>, PluginError> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .build()
        .map_err(|error| {
            PluginError::CommandFailed(format!("failed to build https client: {error}"))
        })?;
    let mut response = client.get(url).send().map_err(|error| {
        PluginError::CommandFailed(format!("--plugin-url fetch failed: {error}"))
    })?;
    if !response.status().is_success() {
        return Err(PluginError::CommandFailed(format!(
            "--plugin-url fetch returned HTTP {}",
            response.status()
        )));
    }

    let mut buf = Vec::new();
    let mut limited = (&mut response).take(cap.saturating_add(1));
    io::copy(&mut limited, &mut buf).map_err(PluginError::Io)?;
    if buf.len() as u64 > cap {
        return Err(PluginError::InvalidManifest(format!(
            "--plugin-url body exceeds the {cap}-byte plugin archive cap",
        )));
    }
    Ok(buf)
}

fn extract_validated_zip(bytes: &[u8], label: &str) -> Result<PreparedPluginSource, PluginError> {
    if bytes.len() < ZIP_MAGIC.len() || &bytes[..ZIP_MAGIC.len()] != ZIP_MAGIC {
        return Err(PluginError::InvalidManifest(format!(
            "--{label} payload is not a ZIP archive (missing PK magic bytes)"
        )));
    }

    let dest = create_session_temp_dir(label)?;
    if let Err(error) = extract_zip_into(bytes, &dest, DEFAULT_MAX_EXTRACTED_BYTES) {
        // Best-effort cleanup of a half-extracted dest before bubbling up.
        let _ = fs::remove_dir_all(&dest);
        return Err(error);
    }

    Ok(PreparedPluginSource {
        root: dest.clone(),
        owned_temp_dir: Some(dest),
    })
}

fn create_session_temp_dir(label: &str) -> Result<PathBuf, PluginError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("{SESSION_TMP_PREFIX}{label}-{pid}-{nanos}"));
    fs::create_dir_all(&dir).map_err(PluginError::Io)?;
    Ok(dir)
}

/// Path-traversal-safe ZIP extraction with a total-size cap.
fn extract_zip_into(bytes: &[u8], dest: &Path, max_bytes: u64) -> Result<(), PluginError> {
    let cursor = io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|error| {
        PluginError::InvalidManifest(format!("zip archive could not be opened: {error}"))
    })?;

    let mut extracted: u64 = 0;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            PluginError::InvalidManifest(format!("zip entry {index} unreadable: {error}"))
        })?;

        let raw_name = entry.name().to_string();
        let safe_path = sanitize_zip_entry_path(&raw_name).ok_or_else(|| {
            PluginError::InvalidManifest(format!(
                "zip entry `{raw_name}` is unsafe (absolute, traversal, or non-utf8)"
            ))
        })?;
        let target = dest.join(&safe_path);

        // After joining, target must still live under dest.  This is
        // belt-and-braces: sanitize_zip_entry_path already rejects `..`.
        if !target.starts_with(dest) {
            return Err(PluginError::InvalidManifest(format!(
                "zip entry `{raw_name}` escapes the extraction root"
            )));
        }

        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(PluginError::Io)?;
            continue;
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(PluginError::Io)?;
        }

        // Stream the entry, enforcing the cumulative cap as we go.
        let mut out = File::create(&target).map_err(PluginError::Io)?;
        let mut buffer = [0u8; 8192];
        loop {
            let read = entry.read(&mut buffer).map_err(PluginError::Io)?;
            if read == 0 {
                break;
            }
            extracted = extracted.saturating_add(read as u64);
            if extracted > max_bytes {
                return Err(PluginError::InvalidManifest(format!(
                    "zip extraction exceeded {max_bytes}-byte cap"
                )));
            }
            out.write_all(&buffer[..read]).map_err(PluginError::Io)?;
        }
    }

    Ok(())
}

/// Reject any zip entry path that is absolute, drive-prefixed, contains
/// `..` components, or is non-UTF-8.  Returns the cleaned relative path on
/// success.
fn sanitize_zip_entry_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    // Reject Windows drive-letter prefixes ("C:foo") and absolute paths.
    if raw.starts_with('/') || raw.starts_with('\\') {
        return None;
    }
    if raw.len() >= 2 {
        let bytes = raw.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return None;
        }
    }

    let candidate = PathBuf::from(raw);
    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    if clean.as_os_str().is_empty() {
        return None;
    }
    Some(clean)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::FileOptions;
    use zip::ZipWriter;

    use crate::manifest;

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let pid = std::process::id();
        std::env::temp_dir().join(format!("session-plugins-test-{label}-{pid}-{nanos}"))
    }

    fn write_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let buf = Cursor::new(Vec::<u8>::new());
        let mut writer = ZipWriter::new(buf);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start file");
            writer.write_all(body).expect("write body");
        }
        let cursor = writer.finish().expect("finish zip");
        cursor.into_inner()
    }

    fn benign_manifest_zip() -> Vec<u8> {
        let manifest = br#"{
  "name": "session-zip-demo",
  "version": "0.1.0",
  "description": "session zip plugin"
}"#;
        write_zip(&[("plugin.json", manifest)])
    }

    #[test]
    fn rejects_http_url() {
        let error = prepare_plugin_url_source("http://example.com/plugin.zip", None)
            .expect_err("http should be rejected");
        let message = error.to_string();
        assert!(
            message.contains("HTTPS") || message.contains("https"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn rejects_non_url_scheme() {
        let error = prepare_plugin_url_source("file:///tmp/plugin.zip", None)
            .expect_err("non-https schemes must be rejected");
        assert!(error.to_string().contains("https://"));
    }

    #[test]
    fn rejects_invalid_sha256_format() {
        let error = prepare_plugin_url_source("https://example.com/p.zip", Some("not-hex"))
            .expect_err("bad sha must be rejected before fetch");
        assert!(error.to_string().contains("64 hex"));
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_zip_entry_path("../etc/passwd").is_none());
        assert!(sanitize_zip_entry_path("a/../b").is_none());
        assert!(sanitize_zip_entry_path("/etc/passwd").is_none());
        assert!(sanitize_zip_entry_path("\\windows\\foo").is_none());
        assert!(sanitize_zip_entry_path("C:foo").is_none());
        assert!(sanitize_zip_entry_path("").is_none());
    }

    #[test]
    fn sanitize_accepts_normal_paths() {
        assert_eq!(
            sanitize_zip_entry_path("hooks/pre.sh"),
            Some(PathBuf::from("hooks").join("pre.sh"))
        );
        assert_eq!(
            sanitize_zip_entry_path("./plugin.json"),
            Some(PathBuf::from("plugin.json"))
        );
    }

    #[test]
    fn extract_zip_with_traversal_entry_is_rejected() {
        // Build a zip that *literally* names `../etc/passwd`.  The `zip`
        // crate writes whatever name we give it.
        let bytes = write_zip(&[("../etc/passwd", b"pwn")]);
        let dest = temp_dir("traversal");
        fs::create_dir_all(&dest).expect("dest");
        let error = extract_zip_into(&bytes, &dest, DEFAULT_MAX_EXTRACTED_BYTES)
            .expect_err("traversal entry must be rejected");
        assert!(
            error.to_string().contains("unsafe")
                || error.to_string().contains("escapes"),
            "expected sanitize error, got: {error}"
        );
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn extract_zip_enforces_size_cap() {
        // A 200-byte stored entry, cap of 100 → must reject.
        let bytes = write_zip(&[("plugin.json", &[b'x'; 200][..])]);
        let dest = temp_dir("cap");
        fs::create_dir_all(&dest).expect("dest");
        let error = extract_zip_into(&bytes, &dest, 100)
            .expect_err("size cap must reject oversize archive");
        assert!(error.to_string().contains("cap"), "got: {error}");
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn prepare_plugin_dir_source_extracts_zip_with_benign_manifest() {
        let staging = temp_dir("zip-staging");
        fs::create_dir_all(&staging).expect("staging");
        let zip_path = staging.join("plugin.zip");
        fs::write(&zip_path, benign_manifest_zip()).expect("write zip");

        let prepared =
            prepare_plugin_dir_source(&zip_path).expect("benign zip should extract");
        let manifest_path = prepared.root().join(manifest::MANIFEST_FILE_NAME);
        assert!(manifest_path.exists(), "extracted manifest should exist");

        // The existing plugin loader must accept the extracted tree.
        let loaded = crate::manifest::load_plugin_from_directory(prepared.root())
            .expect("loader should accept extracted manifest");
        assert_eq!(loaded.name, "session-zip-demo");

        drop(prepared);
        // After drop, the extracted dir should be gone.
        assert!(!manifest_path.exists(), "drop must clean up temp dir");
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn prepare_plugin_dir_source_passes_through_directories() {
        let dir = temp_dir("passthrough");
        fs::create_dir_all(&dir).expect("dir");
        let prepared = prepare_plugin_dir_source(&dir).expect("dir should pass through");
        assert_eq!(prepared.root(), dir.as_path());
        // Drop must NOT remove a passthrough directory.
        drop(prepared);
        assert!(dir.exists(), "passthrough dir must not be removed on drop");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prepare_plugin_dir_source_rejects_non_zip_file() {
        let staging = temp_dir("not-zip");
        fs::create_dir_all(&staging).expect("staging");
        let path = staging.join("plugin.tar");
        fs::write(&path, b"not a zip").expect("write");
        let error = prepare_plugin_dir_source(&path).expect_err("non-zip file must be rejected");
        assert!(error.to_string().contains("not a directory or .zip"));
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn extract_validated_zip_rejects_non_zip_payload() {
        let error = extract_validated_zip(b"<html>nope</html>", "plugin-url")
            .expect_err("non-zip body must be rejected");
        assert!(error.to_string().contains("PK magic"));
    }
}
