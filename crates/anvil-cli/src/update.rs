//! Self-update logic: download the latest GitHub release binary and replace the
//! running executable in-place.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::VERSION;

/// Download the latest release from GitHub and replace the current binary.
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

    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => {
            eprintln!("  Unsupported platform: {os}/{arch}");
            std::process::exit(1);
        }
    };

    let tag_output = Command::new("curl")
        .args([
            "-sfL",
            "--max-time",
            "10",
            "-H",
            "User-Agent: anvil-cli",
            "https://api.github.com/repos/culpur/anvil/releases/latest",
        ])
        .output();
    let tag = match tag_output {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            body.split("\"tag_name\"")
                .nth(1)
                .and_then(|s| s.split('"').nth(1))
                .unwrap_or("latest")
                .to_string()
        }
        _ => {
            eprintln!("  Failed to check GitHub releases");
            std::process::exit(1);
        }
    };

    let url = format!(
        "https://github.com/culpur/anvil/releases/download/{tag}/anvil-{target}"
    );
    println!("  Downloading {tag} for {target}...");

    let tmp_dir = std::env::temp_dir().join("anvil-update");
    let _ = fs::create_dir_all(&tmp_dir);
    let new_binary = tmp_dir.join("anvil");

    let dl = Command::new("curl")
        .args(["-fSL", "--max-time", "120", "-o"])
        .arg(&new_binary)
        .arg(&url)
        .status();

    match dl {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("  Download failed from: {url}");
            std::process::exit(1);
        }
    }
    if !new_binary.exists() {
        eprintln!("  Binary not found in archive");
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
            println!("  ✓ Updated to {tag}!");
            println!("  Restart Anvil to use the new version.");
        }
        Err(e) => {
            let _ = fs::rename(&backup, &current_exe);
            eprintln!("  Failed to replace binary: {e}");
            std::process::exit(1);
        }
    }
}

/// Check GitHub Releases for a newer version of Anvil.
///
/// Returns `Some(message)` when an update is available, `None` when already
/// on the latest release or the check fails silently.
pub(crate) fn check_for_update(current_version: &str) -> Option<String> {
    let urls = ["https://api.github.com/repos/culpur/anvil/releases/latest"];

    for url in &urls {
        let output = Command::new("curl")
            .args(["-sfL", "--max-time", "5", "-H", "User-Agent: anvil-cli", url])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }
        let body = String::from_utf8_lossy(&output.stdout);
        let tag = body.split("\"tag_name\"").nth(1)?.split('"').nth(1)?;
        let latest = tag.trim_start_matches('v');
        if latest != current_version && version_is_newer(latest, current_version) {
            return Some(format!(
                "Update available! {current_version} → {latest}  Run: anvil --update"
            ));
        }
        return None;
    }
    None
}

/// Simple semver comparison: returns `true` if `a` is strictly newer than `b`.
pub(crate) fn version_is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let va = parse(a);
    let vb = parse(b);
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
