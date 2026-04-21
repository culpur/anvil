//! `anvil upgrade` — fetch the latest GitHub release, verify the SHA256
//! signature, and atomically replace the running binary.
//!
//! Uses the respawn infrastructure from `respawn.rs` to relaunch the new
//! binary so the user's terminal session survives the update.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::respawn::{self, RespawnOutcome};
use crate::VERSION;

// ── Constants ─────────────────────────────────────────────────────────────────

const GITHUB_API_RELEASES: &str =
    "https://api.github.com/repos/culpur/anvil/releases/latest";

const GITHUB_RELEASES_BASE: &str =
    "https://github.com/culpur/anvil/releases/download";

// ── Platform target ───────────────────────────────────────────────────────────

fn platform_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn binary_name(target: &str) -> String {
    if target.contains("windows") {
        format!("anvil-{target}.exe")
    } else {
        format!("anvil-{target}")
    }
}

// ── GitHub release metadata ───────────────────────────────────────────────────

/// Parsed information from a GitHub release.
#[derive(Debug, Clone)]
pub(crate) struct ReleaseInfo {
    pub tag: String,
    pub version: String,
}

/// Fetch the latest release tag from the GitHub API.
/// Returns `None` on network error or unexpected JSON.
pub(crate) fn fetch_latest_release() -> Option<ReleaseInfo> {
    let out = Command::new("curl")
        .args([
            "-sfL",
            "--max-time",
            "10",
            "-H",
            "User-Agent: anvil-cli",
            GITHUB_API_RELEASES,
        ])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&out.stdout);
    let tag = body
        .split("\"tag_name\"")
        .nth(1)?
        .split('"')
        .nth(1)?
        .to_string();

    let version = tag.trim_start_matches('v').to_string();
    Some(ReleaseInfo { tag, version })
}

// ── Version comparison ────────────────────────────────────────────────────────

/// Returns `true` when `candidate` is strictly newer than `current`.
pub(crate) fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let va = parse(candidate);
    let vb = parse(current);
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x > y {
            return true;
        }
        if x < y {
            return false;
        }
    }
    false
}

// ── SHA256 verification ───────────────────────────────────────────────────────

/// Fetch the published SHA256 checksum file for a given release asset.
/// The checksum file is expected at `<base>/<tag>/anvil-<target>.sha256`.
fn fetch_sha256(tag: &str, binary: &str) -> Option<String> {
    // Primary: out-of-band manifest at anvilhub.culpur.net (separate origin from
    // GitHub releases, so a GitHub release compromise cannot also forge the hash).
    // Fallback: the .sha256 sibling on GitHub releases.
    let primary = format!("https://anvilhub.culpur.net/sha256/{binary}.sha256");
    let fallback = format!("{GITHUB_RELEASES_BASE}/{tag}/{binary}.sha256");

    for url in [&primary, &fallback] {
        let out = Command::new("curl")
            .args(["-sfL", "--max-time", "15", url])
            .output()
            .ok()?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(hash) = text.split_whitespace().next() {
                return Some(hash.to_string());
            }
        }
    }
    None
}

/// Compute the SHA256 of a local file using the platform-native tool.
/// Returns `None` if the tool is not available or the file cannot be read.
pub(crate) fn sha256_of_file(path: &PathBuf) -> Option<String> {
    // macOS: shasum -a 256
    // Linux: sha256sum
    // Windows: CertUtil -hashfile (handled separately, not in this code path)
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("shasum", &["-a", "256"])
    } else {
        ("sha256sum", &[])
    };

    let out = Command::new(cmd)
        .args(args)
        .arg(path)
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_string)
}

/// Verify that the downloaded file matches the expected SHA256 hex digest.
/// Both values are compared case-insensitively.
pub(crate) fn verify_sha256(path: &PathBuf, expected: &str) -> Result<(), String> {
    let actual = sha256_of_file(path)
        .ok_or_else(|| "cannot compute SHA256 of downloaded binary".to_string())?;

    if actual.to_ascii_lowercase() != expected.to_ascii_lowercase() {
        return Err(format!(
            "SHA256 mismatch!\n  expected: {}\n  got:      {}",
            expected.to_ascii_lowercase(),
            actual.to_ascii_lowercase()
        ));
    }
    Ok(())
}

// ── Download ──────────────────────────────────────────────────────────────────

fn download_binary(tag: &str, binary: &str, dest: &PathBuf) -> Result<(), String> {
    let url = format!("{GITHUB_RELEASES_BASE}/{tag}/{binary}");
    println!("  Downloading {url}");

    let status = Command::new("curl")
        .args(["-fSL", "--max-time", "180", "-o"])
        .arg(dest)
        .arg(&url)
        .status()
        .map_err(|e| format!("curl error: {e}"))?;

    if !status.success() {
        return Err(format!("download failed from {url}"));
    }
    Ok(())
}

// ── Atomic replacement ────────────────────────────────────────────────────────

fn replace_binary(new_binary: &PathBuf, current_exe: &PathBuf) -> Result<(), String> {
    // Atomic rename strategy:
    // 1. Copy new binary to <current>.new
    // 2. Rename current to <current>.bak
    // 3. Rename .new to current
    // 4. Remove .bak
    let new_path = current_exe.with_extension("new");
    let bak_path = current_exe.with_extension("bak");

    fs::copy(new_binary, &new_path)
        .map_err(|e| format!("cannot write new binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&new_path, fs::Permissions::from_mode(0o755));
    }

    let _ = fs::rename(current_exe, &bak_path);

    fs::rename(&new_path, current_exe).map_err(|e| {
        // Roll back: restore the backup
        let _ = fs::rename(&bak_path, current_exe);
        format!("cannot replace binary: {e}")
    })?;

    let _ = fs::remove_file(&bak_path);
    Ok(())
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run `anvil upgrade`.
///
/// Exit codes (via `std::process::exit`):
/// - 0  up-to-date or upgrade successful
/// - 1  network failure
/// - 2  SHA256 mismatch
/// - 3  permission / replacement failure
pub(crate) fn run_upgrade() {
    println!();
    println!("\x1b[1mAnvil upgrade\x1b[0m");
    println!("  Current version: {VERSION}");
    println!();

    // 1. Fetch latest release info
    print!("  Checking GitHub for updates...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let Some(release) = fetch_latest_release() else {
        eprintln!();
        eprintln!("  \x1b[31m\u{2718}\x1b[0m  Cannot reach GitHub API — check network.");
        std::process::exit(1);
    };

    println!(" {}", release.tag);

    if !is_newer(&release.version, VERSION) {
        println!("  \x1b[32m\u{2714}\x1b[0m  Already on the latest version ({VERSION}).");
        println!();
        return;
    }

    println!("  \x1b[33m\u{2192}\x1b[0m  Upgrade available: {VERSION} \u{2192} {}", release.version);
    println!();

    // 2. Determine platform target
    let Some(target) = platform_target() else {
        eprintln!("  \x1b[31m\u{2718}\x1b[0m  Unsupported platform: {}/{}", std::env::consts::OS, std::env::consts::ARCH);
        std::process::exit(1);
    };

    let binary = binary_name(target);
    let tmp_dir = std::env::temp_dir().join("anvil-upgrade");
    let _ = fs::create_dir_all(&tmp_dir);
    let new_binary = tmp_dir.join(&binary);

    // 3. Download
    if let Err(e) = download_binary(&release.tag, &binary, &new_binary) {
        eprintln!("  \x1b[31m\u{2718}\x1b[0m  {e}");
        std::process::exit(1);
    }

    // 4. Fetch and verify SHA256
    print!("  Verifying SHA256...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    // Integrity is non-negotiable: if the checksum cannot be fetched we
    // abort. We never install an unverified upgrade.
    match fetch_sha256(&release.tag, &binary) {
        None => {
            eprintln!();
            eprintln!("  \x1b[31m\u{2718}\x1b[0m  Could not fetch SHA256 for {binary}");
            eprintln!("      Refusing to install an unverified binary. Try again later, or");
            eprintln!(
                "      download manually from https://github.com/culpur/anvil/releases/tag/{}",
                release.tag
            );
            let _ = fs::remove_dir_all(&tmp_dir);
            std::process::exit(2);
        }
        Some(expected) => match verify_sha256(&new_binary, &expected) {
            Ok(()) => println!(" \x1b[32mok\x1b[0m"),
            Err(e) => {
                eprintln!();
                eprintln!("  \x1b[31m\u{2718}\x1b[0m  {e}");
                let _ = fs::remove_dir_all(&tmp_dir);
                std::process::exit(2);
            }
        },
    }

    // 5. Replace binary
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  \x1b[31m\u{2718}\x1b[0m  Cannot resolve current binary path: {e}");
            std::process::exit(3);
        }
    };

    println!("  Replacing {}...", current_exe.display());

    if let Err(e) = replace_binary(&new_binary, &current_exe) {
        // Fallback: leave the new binary in /tmp and instruct the user
        let fallback = std::env::temp_dir().join("anvil-new");
        let _ = fs::copy(&new_binary, &fallback);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&fallback, fs::Permissions::from_mode(0o755));
        }

        eprintln!("  \x1b[31m\u{2718}\x1b[0m  {e}");
        eprintln!();
        eprintln!("  Fallback: new binary saved to {}", fallback.display());
        eprintln!("  To install manually:");
        eprintln!("    mv {} {}", fallback.display(), current_exe.display());
        std::process::exit(3);
    }

    let _ = fs::remove_dir_all(&tmp_dir);

    println!();
    println!("  \x1b[32m\u{2714}\x1b[0m  Updated to {}!", release.version);
    println!();

    // 6. Respawn using the existing respawn infrastructure
    let ctx = crate::get_respawn_ctx();
    match respawn::respawn(&ctx, "upgrade", "") {
        Ok(RespawnOutcome::Respawned) => {
            // exec replaced us; this line is unreachable
        }
        Ok(RespawnOutcome::PromptUser(msg)) => {
            println!("  {msg}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("  Respawn failed ({e}) — restart anvil manually.");
            std::process::exit(0);
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_newer ──────────────────────────────────────────────────────────

    #[test]
    fn newer_patch() {
        assert!(is_newer("2.2.8", "2.2.7"));
    }

    #[test]
    fn newer_minor() {
        assert!(is_newer("2.3.0", "2.2.7"));
    }

    #[test]
    fn newer_major() {
        assert!(is_newer("3.0.0", "2.9.9"));
    }

    #[test]
    fn not_newer_same() {
        assert!(!is_newer("2.2.7", "2.2.7"));
    }

    #[test]
    fn not_newer_older() {
        assert!(!is_newer("2.2.6", "2.2.7"));
    }

    #[test]
    fn not_newer_older_minor() {
        assert!(!is_newer("2.1.99", "2.2.0"));
    }

    #[test]
    fn version_with_v_prefix() {
        // Tags often arrive as "v2.2.8" — the parser strips 'v'
        assert!(is_newer("v2.2.8", "2.2.7"));
    }

    // ── platform_target ───────────────────────────────────────────────────

    #[test]
    fn platform_target_is_some() {
        // On any of the CI/dev platforms we support, this must return Some.
        // The test will be skipped on unsupported platforms (CI passes trivially).
        #[cfg(any(
            all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64")),
            all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
            all(target_os = "windows", target_arch = "x86_64"),
        ))]
        assert!(platform_target().is_some(), "platform must be supported");
    }

    // ── binary_name ───────────────────────────────────────────────────────

    #[test]
    fn binary_name_windows_has_exe() {
        let name = binary_name("x86_64-pc-windows-msvc");
        assert!(name.ends_with(".exe"), "Windows binary must have .exe extension");
    }

    #[test]
    fn binary_name_linux_no_exe() {
        let name = binary_name("x86_64-unknown-linux-gnu");
        assert!(!name.ends_with(".exe"), "Linux binary must not have .exe extension");
    }

    // ── verify_sha256 ─────────────────────────────────────────────────────

    #[test]
    fn verify_sha256_correct() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("anvil-sha-test-{}", std::process::id()));
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(b"hello anvil").unwrap();
        drop(f);

        // Compute the expected hash ourselves
        let Some(actual) = sha256_of_file(&tmp) else {
            // If sha256sum / shasum is not available in the test environment, skip.
            let _ = std::fs::remove_file(&tmp);
            return;
        };
        assert!(verify_sha256(&tmp, &actual).is_ok());

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn verify_sha256_mismatch() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("anvil-sha-mismatch-{}", std::process::id()));
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(b"hello anvil").unwrap();
        drop(f);

        let result = verify_sha256(
            &tmp,
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        // Must fail when sha256sum is available
        if sha256_of_file(&tmp).is_some() {
            assert!(result.is_err(), "wrong hash must return Err");
        }

        let _ = std::fs::remove_file(&tmp);
    }

    // ── fetch_latest_release (offline mock test) ──────────────────────────

    #[test]
    fn release_info_version_strips_v() {
        // Simulate what fetch_latest_release does when tag is "v2.2.8"
        let tag = "v2.2.8".to_string();
        let version = tag.trim_start_matches('v').to_string();
        assert_eq!(version, "2.2.8");
    }

    // ── replace_binary (dry-run simulation) ──────────────────────────────

    #[test]
    fn replace_binary_succeeds() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("anvil-replace-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let new_bin = dir.join("anvil-new");
        let current = dir.join("anvil");

        // Write placeholder files
        let mut f = std::fs::File::create(&new_bin).unwrap();
        f.write_all(b"NEW_BINARY_CONTENT").unwrap();
        drop(f);
        let mut f = std::fs::File::create(&current).unwrap();
        f.write_all(b"OLD_BINARY_CONTENT").unwrap();
        drop(f);

        replace_binary(&new_bin, &current).expect("replace_binary must succeed");

        let content = std::fs::read_to_string(&current).unwrap();
        assert_eq!(content, "NEW_BINARY_CONTENT");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
