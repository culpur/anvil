//! Self-update logic: download the latest release binary and replace the
//! running executable in-place.
//!
//! Probe order matches the rail banner (`runtime::update_check`):
//!
//! 1. **anvilhub `/api/version`** — returns the exact asset filenames keyed by
//!    rust target triple. Preferred because it removes client-side guessing
//!    about whether a target ships `.exe` or has a `-gnu`/`-msvc` quirk.
//! 2. **GitHub `/releases/latest`** — fallback only. The URL is reconstructed
//!    from the tag using `release.sh`'s canonical asset naming.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use runtime::update_check;

use crate::VERSION;

/// Map the running `OS/ARCH` pair to the rust target triple used in our
/// release-asset filenames. Returns `None` for unsupported platforms.
///
/// Windows is `windows-gnu` (mingw build) to match `scripts/release.sh`; the
/// older `windows-msvc` triple was wrong and produced 404s.
pub(crate) fn platform_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-gnu"),
        ("freebsd", "x86_64") => Some("x86_64-unknown-freebsd"),
        ("netbsd", "x86_64") => Some("x86_64-unknown-netbsd"),
        _ => None,
    }
}

/// Canonical local binary name for a given rust target triple. Appends `.exe`
/// for Windows targets to match `release.sh` output.
pub(crate) fn asset_filename(target: &str) -> String {
    if target.contains("windows") {
        format!("anvil-{target}.exe")
    } else {
        format!("anvil-{target}")
    }
}

/// Download the latest release binary and replace the current executable.
/// Exits the process on success or failure — never returns.
pub(crate) fn run_self_update() {
    println!("Anvil self-update");
    println!("  Current version: {VERSION}");
    println!();

    print!("  Checking for updates... ");
    let latest_info = check_for_update(VERSION);
    if latest_info.is_none() {
        println!("already up to date!");
        return;
    }
    println!("update found!");

    let target = match platform_target() {
        Some(t) => t,
        None => {
            let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
            eprintln!("  Unsupported platform: {os}/{arch}");
            std::process::exit(1);
        }
    };

    let Some(meta) = update_check::fetch_release_metadata(target) else {
        eprintln!("  Failed to resolve release metadata for {target}");
        eprintln!("  (anvilhub and GitHub both unreachable or did not list this target)");
        std::process::exit(1);
    };

    println!(
        "  Downloading {} for {} (via {:?})...",
        meta.tag, target, meta.source
    );

    let tmp_dir = std::env::temp_dir().join("anvil-update");
    let _ = fs::create_dir_all(&tmp_dir);
    let new_binary = tmp_dir.join(asset_filename(target));

    let dl = Command::new("curl")
        .args(["-fSL", "--max-time", "120", "-o"])
        .arg(&new_binary)
        .arg(&meta.binary_url)
        .status();

    match dl {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("  Download failed from: {}", meta.binary_url);
            std::process::exit(1);
        }
    }
    if !new_binary.exists() {
        eprintln!("  Binary not found at {}", new_binary.display());
        std::process::exit(1);
    }

    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("anvil"));
    println!("  Replacing {}...", current_exe.display());

    let backup = current_exe.with_extension("bak");
    let _ = fs::rename(&current_exe, &backup);

    match fs::copy(&new_binary, &current_exe) {
        Ok(_) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755));
            }
            let _ = fs::remove_file(&backup);
            let _ = fs::remove_dir_all(&tmp_dir);
            println!();
            println!("  ✓ Updated to {}!", meta.tag);
            println!("  Restart Anvil to use the new version.");
        }
        Err(e) => {
            let _ = fs::rename(&backup, &current_exe);
            eprintln!("  Failed to replace binary: {e}");
            std::process::exit(1);
        }
    }
}

/// Returns `Some(message)` when an update is available, `None` when already
/// on the latest release or the check fails silently.
///
/// Reuses the rail-banner cache via `runtime::update_check::check`, which
/// itself probes anvilhub first and GitHub as fallback.
pub(crate) fn check_for_update(current_version: &str) -> Option<String> {
    let latest = update_check::check(current_version)?;
    Some(format!(
        "Update available! {current_version} → {latest}  Run: anvil --update"
    ))
}

/// Simple semver comparison: returns `true` if `a` is strictly newer than `b`.
///
/// Kept here for backward compatibility with call sites that imported it from
/// this module; the canonical implementation lives in
/// `runtime::update_check::version_is_newer`.
pub(crate) fn version_is_newer(a: &str, b: &str) -> bool {
    update_check::version_is_newer(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── target mapping ───────────────────────────────────────────────────

    #[test]
    fn update_target_for_windows_is_gnu_with_exe_extension() {
        // Layer A: the historical bug. release.sh ships `*-windows-gnu.exe`,
        // not `*-windows-msvc`. The mapping AND the asset filename must agree.
        let asset = asset_filename("x86_64-pc-windows-gnu");
        assert_eq!(asset, "anvil-x86_64-pc-windows-gnu.exe");

        // Sanity: any non-windows target must NOT acquire .exe.
        assert_eq!(
            asset_filename("x86_64-unknown-linux-gnu"),
            "anvil-x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            asset_filename("aarch64-apple-darwin"),
            "anvil-aarch64-apple-darwin"
        );
    }

    #[test]
    fn platform_target_windows_is_gnu_not_msvc() {
        // Compile-time check: the Windows match arm must say `gnu`, not
        // `msvc`. We can only directly assert this when actually built on
        // Windows; on other hosts, the function returns the appropriate
        // triple for THIS host. Cover both axes:
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        assert_eq!(platform_target(), Some("x86_64-pc-windows-gnu"));

        // Universally: whatever the host returns, it must NOT be the broken
        // windows-msvc value. (Belt-and-suspenders for accidental reverts.)
        if let Some(t) = platform_target() {
            assert!(
                !t.contains("windows-msvc"),
                "platform_target() must never return windows-msvc — release.sh ships windows-gnu"
            );
        }
    }

    #[test]
    fn version_is_newer_delegates_to_runtime() {
        assert!(version_is_newer("2.2.17", "2.2.16"));
        assert!(!version_is_newer("2.2.16", "2.2.16"));
        assert!(!version_is_newer("2.2.15", "2.2.16"));
    }

    // ── structural: prefer anvilhub, fall back to GitHub ─────────────────
    //
    // These tests exercise `runtime::update_check::fetch_release_metadata_from`
    // through the same code path the update flow uses. The runtime crate has
    // its own tests for the parser primitives; here we confirm the integration
    // (3-stage server: anvilhub status → fallback decision → URL handed back).

    fn spawn_one_shot_http(status_line: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n\
                     {body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[test]
    fn update_prefers_anvilhub_api_version_when_200() {
        let body = r#"{
            "latest_version": "2.5.0",
            "binaries": {
                "x86_64-unknown-linux-gnu": "https://example/anvil-x86_64-unknown-linux-gnu"
            }
        }"#;
        let anvilhub = spawn_one_shot_http("200 OK", body);
        let github = "http://127.0.0.1:1/";
        let meta = runtime::update_check::fetch_release_metadata_from(
            &anvilhub,
            github,
            "x86_64-unknown-linux-gnu",
        )
        .expect("anvilhub answer");
        assert_eq!(meta.source, runtime::update_check::UpdateSource::Anvilhub);
        assert_eq!(meta.version, "2.5.0");
        assert_eq!(meta.binary_url, "https://example/anvil-x86_64-unknown-linux-gnu");
    }

    #[test]
    fn update_falls_back_to_github_releases_when_anvilhub_500() {
        let anvilhub = spawn_one_shot_http("500 Internal Server Error", "boom");
        let github = spawn_one_shot_http("200 OK", "{\"tag_name\":\"v2.5.1\"}");
        let meta = runtime::update_check::fetch_release_metadata_from(
            &anvilhub,
            &github,
            "x86_64-pc-windows-gnu",
        )
        .expect("github fallback answer");
        assert_eq!(meta.source, runtime::update_check::UpdateSource::Github);
        // Critical: GitHub fallback must reconstruct the Windows URL with .exe.
        assert!(
            meta.binary_url.ends_with("anvil-x86_64-pc-windows-gnu.exe"),
            "windows fallback URL must end in .exe, got {}",
            meta.binary_url
        );
    }

    #[test]
    fn update_falls_back_to_github_when_anvilhub_missing_target_key() {
        let body_no_target = r#"{
            "latest_version": "2.5.2",
            "binaries": { "x86_64-apple-darwin": "https://example/macos" }
        }"#;
        let anvilhub = spawn_one_shot_http("200 OK", body_no_target);
        let github = spawn_one_shot_http("200 OK", "{\"tag_name\":\"v2.5.2\"}");
        let meta = runtime::update_check::fetch_release_metadata_from(
            &anvilhub,
            &github,
            "aarch64-unknown-linux-gnu",
        )
        .expect("github fallback when target missing");
        assert_eq!(meta.source, runtime::update_check::UpdateSource::Github);
        assert!(
            meta.binary_url.ends_with("anvil-aarch64-unknown-linux-gnu"),
            "fallback URL must point at the requested target, got {}",
            meta.binary_url
        );
    }
}
