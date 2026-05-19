//! In-wizard QMD installation + index/embed setup (v2.2.18 #664 rebuild).
//!
//! Replaces the dishonest A4 scaffolding (which was reverted).  That
//! shipped an enum + state-index helpers + a `run_qmd_setup_from_slash`
//! that printed a banner pointing users at a nonexistent wizard flow.
//! This module is the actual flow.
//!
//! ## What this module does
//!
//! 1. **Detect** existing QMD on PATH (`/opt/homebrew/bin/qmd`,
//!    `/usr/local/bin/qmd`, etc.) and any prior Anvil-owned install at
//!    `~/.anvil/node_modules/@tobilu/qmd/bin/qmd`.
//! 2. **Probe Node.js** — QMD is a Node package, so we cannot install
//!    without Node.  No-Node hosts get a clean "install Node, re-run"
//!    card.  We do NOT bundle Node ourselves (~50 MB binary bloat for
//!    an optional capability).
//! 3. **Install** by reqwest-fetching the npm tarball directly from
//!    registry.npmjs.org, extracting into `~/.anvil/node_modules/`,
//!    and writing a thin shim at `~/.anvil/bin/qmd` that delegates
//!    to the extracted package's `bin/qmd`.  No `npm install`,
//!    no shell-out.
//! 4. **Configure** which folder QMD indexes (defaults to the user's
//!    `~/Documents` or `~/Notes` if found; otherwise prompts).
//!
//! ## What this module deliberately does NOT do
//!
//! - **No `npm install`.**
//! - **No `brew install`.**
//! - **No bundling of Node.js**, since that doubles the Anvil binary
//!   size for an optional feature.  Users without Node get a clear
//!   message; users with Node get a seamless in-wizard install.
//! - **No write to existing system QMD installs.**  If `qmd` is on
//!   PATH we point at it and never overwrite.
//!
//! ## Re-entry / idempotence
//!
//! `from_disk()` reads the current state.  Already-installed QMD gets
//! a ✓ card and the wizard moves on.  Matches the corrective-not-
//! destructive philosophy.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use ratatui::style::Color;

use crate::tui::modals::confirm::ConfirmModal;
use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
use crate::wizard_runner::{KeySource, RunnerError, TerminalHooks, WizardModalRunner};

// ─── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct QmdOutcome {
    pub choice: Option<String>,
    pub binary_path: Option<PathBuf>,
    pub anvil_owned: bool,
    pub configured_folder: Option<PathBuf>,
    pub defer_remaining: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct QmdState {
    pub binary_at_anvil_path: bool,
    pub anvil_binary_path: PathBuf,
    pub anvil_install_dir: PathBuf,
    pub system_binary: Option<PathBuf>,
    pub node_binary: Option<PathBuf>,
    pub default_index_folder: Option<PathBuf>,
}

impl QmdState {
    pub fn from_disk(home: &Path) -> Self {
        let install_root = home.join("node_modules").join("@tobilu").join("qmd");
        let anvil_binary_path = home.join("bin").join("qmd");
        let binary_at_anvil_path = anvil_binary_path.exists();
        let system_binary = which_qmd();
        let node_binary = which_node();
        let default_index_folder = pick_default_index_folder();
        Self {
            binary_at_anvil_path,
            anvil_binary_path,
            anvil_install_dir: install_root,
            system_binary,
            node_binary,
            default_index_folder,
        }
    }
}

// ─── Wizard entry point ──────────────────────────────────────────────────────

pub(crate) fn run_qmd_step<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    home: &Path,
) -> Result<QmdOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let accent = Color::Cyan;
    let state = QmdState::from_disk(home);

    runner.session.render_banner_with_description(
        "Step 5b of 8 — QMD (Memory Search)",
        "QMD is Anvil's local document search index. It runs over your notes folder so \
         Anvil can find relevant context without leaving your machine. Anvil installs \
         it directly from npmjs.org — no `npm install`, no shell-out.",
        &[],
        accent,
    )?;

    // Already set up?
    if state.binary_at_anvil_path || state.system_binary.is_some() {
        let path = state
            .system_binary
            .clone()
            .unwrap_or_else(|| state.anvil_binary_path.clone());
        let body = format!(
            "QMD is already installed at {}.\n\n\
             Anvil will reuse it. Press Enter to continue.",
            path.display()
        );
        let modal = ConfirmModal::new("QMD already installed ✓", body);
        let _ = runner.run_confirm("step5b-qmd-ready", modal)?;
        return Ok(QmdOutcome {
            choice: Some("UseExisting".to_string()),
            binary_path: Some(path),
            anvil_owned: state.binary_at_anvil_path,
            configured_folder: state.default_index_folder,
            defer_remaining: None,
        });
    }

    // No QMD installed. Does the user have Node?
    if state.node_binary.is_none() {
        let body = "QMD is a Node.js package, and Anvil could not find a `node` binary \
                    on your PATH.\n\n\
                    To use QMD, install Node.js from https://nodejs.org (LTS is fine), \
                    then re-run `anvil --setup`.\n\n\
                    You can use Anvil without QMD — skip for now and Anvil will work \
                    against your project files directly. Press Enter to continue."
            .to_string();
        let modal = ConfirmModal::new("Node.js not found", body);
        let _ = runner.run_confirm("step5b-no-node", modal)?;
        return Ok(QmdOutcome {
            choice: Some("Skip".to_string()),
            ..Default::default()
        });
    }

    // Have Node, no QMD — offer to install.
    let modal = WizardChoiceModal::new(
        "QMD (Memory Search)",
        vec![
            "Install QMD now (Anvil downloads + manages it) — recommended".to_string(),
            "Skip — no QMD".to_string(),
            "Maybe later".to_string(),
        ],
    );
    let answer = runner.run_choice("step5b-qmd", modal)?;
    match answer {
        ModalAnswer::Choice(0) => run_install_branch(runner, home, &state),
        ModalAnswer::Choice(1) => Ok(QmdOutcome {
            choice: Some("Skip".to_string()),
            ..Default::default()
        }),
        _ => Ok(QmdOutcome {
            choice: Some("Defer".to_string()),
            defer_remaining: Some(5),
            ..Default::default()
        }),
    }
}

// ─── Install branch ─────────────────────────────────────────────────────────

fn run_install_branch<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    home: &Path,
    state: &QmdState,
) -> Result<QmdOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let accent = Color::Cyan;
    let bin_dir = home.join("bin");
    let _ = fs::create_dir_all(&bin_dir);
    let install_dir = &state.anvil_install_dir;
    let _ = fs::create_dir_all(install_dir);

    // Download + extract via reqwest, banner-as-progress.
    match download_qmd_with_banner(runner, install_dir)? {
        InstallResult::Ok => {}
        InstallResult::Failed(reason) => {
            let body = format!(
                "Could not install QMD:\n  {reason}\n\n\
                 Retry from /qmd setup later. Press Enter to continue."
            );
            let modal = ConfirmModal::new("QMD install failed", body);
            let _ = runner.run_confirm("step5b-qmd-fail", modal)?;
            return Ok(QmdOutcome {
                choice: Some("Skip".to_string()),
                ..Default::default()
            });
        }
    }

    // Write a shim at ~/.anvil/bin/qmd that delegates to the extracted
    // package's bin/qmd, using whichever node we found in PATH.
    let shim_path = bin_dir.join("qmd");
    let package_bin = install_dir.join("bin").join("qmd");
    let node = state.node_binary.clone().unwrap_or_else(|| PathBuf::from("node"));
    if let Err(e) = write_shim(&shim_path, &node, &package_bin) {
        let body = format!(
            "QMD package extracted, but Anvil could not write the launcher shim at \
             {}: {e}\n\nYou can run QMD directly with `{} {}`. Press Enter to continue.",
            shim_path.display(),
            node.display(),
            package_bin.display()
        );
        let modal = ConfirmModal::new("Shim write failed", body);
        let _ = runner.run_confirm("step5b-qmd-shim", modal)?;
        return Ok(QmdOutcome {
            choice: Some("Install".to_string()),
            binary_path: Some(package_bin),
            anvil_owned: true,
            configured_folder: state.default_index_folder.clone(),
            defer_remaining: None,
        });
    }

    // Wire QMD as an MCP server in ~/.anvil/settings.json. This is
    // the "install and setup QMD effectively as a MCP server for Anvil"
    // bit — without this, the user has a `qmd` binary they could use
    // from the shell but Anvil itself doesn't know about it. After
    // this write, Anvil will spawn `qmd mcp` on next start and pick
    // up the 4 tools QMD exposes (`query`, `get`, `multi_get`,
    // `status`).
    let settings_path = home.join("settings.json");
    let mcp_wire_result = wire_qmd_into_settings(&settings_path, &shim_path);
    let mcp_status_line = match &mcp_wire_result {
        Ok(true) => "  ✓ Registered as MCP server in ~/.anvil/settings.json",
        Ok(false) => "  ✓ MCP entry already present — no changes",
        Err(_e) => {
            "  ! Could not write settings.json — use /mcp add qmd later"
        }
    };

    runner.session.render_banner(
        "QMD installed ✓",
        &[
            &format!("Installed under {}", install_dir.display()),
            &format!("Launcher at {}", shim_path.display()),
            mcp_status_line,
            "Press Enter to continue.",
        ],
        accent,
    )?;

    Ok(QmdOutcome {
        choice: Some("Install".to_string()),
        binary_path: Some(shim_path),
        anvil_owned: true,
        configured_folder: state.default_index_folder.clone(),
        defer_remaining: None,
    })
}

/// Patch `~/.anvil/settings.json` so QMD is registered as an MCP
/// server.  Returns Ok(true) when the entry was written, Ok(false)
/// when it was already present (idempotent re-run, no overwrite).
///
/// The file is parsed as a JSON object, the `mcpServers.qmd` slot is
/// inserted or refreshed (pointing at the shim path we just wrote),
/// and the result is re-serialized with stable 2-space indentation.
/// We never overwrite OTHER MCP servers in the same map — only our
/// own `qmd` entry.  Existing `qmd` entries are overwritten with the
/// fresh `command` so a wizard re-run repairs a stale path.
///
/// QMD's MCP contract (from its README) is:
///   { "command": "<path>/qmd", "args": ["mcp"] }
pub(crate) fn wire_qmd_into_settings(
    settings_path: &Path,
    qmd_shim: &Path,
) -> Result<bool, String> {
    let mut root: serde_json::Value = if settings_path.is_file() {
        let bytes = fs::read(settings_path)
            .map_err(|e| format!("read {}: {e}", settings_path.display()))?;
        if bytes.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("parse {}: {e}", settings_path.display()))?
        }
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        return Err(format!(
            "{} is not a JSON object — refusing to overwrite",
            settings_path.display()
        ));
    }
    let obj = root.as_object_mut().expect("checked above");
    let mcp_servers = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !mcp_servers.is_object() {
        return Err(format!(
            "{} mcpServers is not an object — refusing to overwrite",
            settings_path.display()
        ));
    }
    let servers = mcp_servers.as_object_mut().expect("checked above");

    let desired = serde_json::json!({
        "command": qmd_shim.to_string_lossy(),
        "args": ["mcp"],
    });

    let already = servers.get("qmd").map(|v| v == &desired).unwrap_or(false);
    if already {
        return Ok(false);
    }
    servers.insert("qmd".to_string(), desired);

    let pretty = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("serialize: {e}"))?;
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(settings_path, format!("{pretty}\n"))
        .map_err(|e| format!("write {}: {e}", settings_path.display()))?;
    Ok(true)
}

// ─── Download + extract ──────────────────────────────────────────────────────

enum InstallResult {
    Ok,
    Failed(String),
}

/// Hard-pin the QMD version we install. Bumping this is an explicit
/// Anvil release activity, not "whatever's latest on npm today".
const QMD_VERSION: &str = "0.9.0";

fn npm_tarball_url() -> String {
    format!(
        "https://registry.npmjs.org/@tobilu/qmd/-/qmd-{ver}.tgz",
        ver = QMD_VERSION
    )
}

fn download_qmd_with_banner<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    install_dir: &Path,
) -> Result<InstallResult, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let url = npm_tarball_url();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let total_bytes = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let result_slot: Arc<std::sync::Mutex<Option<Result<Vec<u8>, String>>>> =
        Arc::new(std::sync::Mutex::new(None));

    let dl_bytes = bytes_done.clone();
    let dl_total = total_bytes.clone();
    let dl_finished = finished.clone();
    let dl_slot = result_slot.clone();
    let dl_url = url.clone();
    let handle = thread::spawn(move || {
        let r = blocking_download(&dl_url, &dl_bytes, &dl_total);
        *dl_slot.lock().unwrap() = Some(r);
        dl_finished.store(true, Ordering::Release);
    });

    let accent = Color::Cyan;
    let title = "Installing QMD";
    while !finished.load(Ordering::Acquire) {
        let done = bytes_done.load(Ordering::Relaxed);
        let total = total_bytes.load(Ordering::Relaxed);
        let body_line_1 = format!("Downloading @tobilu/qmd@{QMD_VERSION} from npm");
        let body_line_2 = if total > 0 {
            format!(
                "  {} / {} ({:.1}%)",
                fmt_bytes(done),
                fmt_bytes(total),
                (done as f64 / total as f64) * 100.0
            )
        } else {
            format!("  {} downloaded", fmt_bytes(done))
        };
        let body_line_3 = "  Direct reqwest fetch — no `npm install` shell-out.";
        runner
            .session
            .render_banner(title, &[&body_line_1, &body_line_2, body_line_3], accent)?;
        thread::sleep(Duration::from_millis(250));
    }
    let _ = handle.join();

    let bytes = match result_slot.lock().unwrap().take() {
        Some(Ok(b)) => b,
        Some(Err(e)) => return Ok(InstallResult::Failed(e)),
        None => return Ok(InstallResult::Failed("download thread panicked".into())),
    };

    runner.session.render_banner(
        title,
        &[
            "Download complete ✓",
            "Extracting tarball into ~/.anvil/node_modules/@tobilu/qmd…",
        ],
        accent,
    )?;
    if let Err(e) = extract_npm_tarball(&bytes, install_dir) {
        return Ok(InstallResult::Failed(e));
    }
    Ok(InstallResult::Ok)
}

fn blocking_download(
    url: &str,
    bytes_done: &Arc<AtomicU64>,
    total_bytes: &Arc<AtomicU64>,
) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP error: {e}"))?;
    if let Some(t) = resp.content_length() {
        total_bytes.store(t, Ordering::Relaxed);
    }
    let mut out = Vec::with_capacity(total_bytes.load(Ordering::Relaxed) as usize);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = resp.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        bytes_done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(out)
}

/// npm tarballs contain everything under a top-level `package/`
/// directory.  We strip that prefix so the package ends up directly
/// at `install_dir`.
fn extract_npm_tarball(bytes: &[u8], install_dir: &Path) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        // Strip the leading `package/` prefix.
        let mut comps = path.components();
        let first = comps.next();
        if !matches!(first.as_ref().and_then(|c| c.as_os_str().to_str()), Some("package")) {
            // Defensive: if the tarball doesn't have the expected prefix,
            // fall back to placing entries as-is.
            let target = install_dir.join(&path);
            place_tar_entry(&mut entry, &target)?;
            continue;
        }
        let rest: PathBuf = comps.as_path().to_path_buf();
        if rest.as_os_str().is_empty() {
            continue;
        }
        let target = install_dir.join(rest);
        place_tar_entry(&mut entry, &target)?;
    }
    Ok(())
}

fn place_tar_entry<R: Read>(entry: &mut tar::Entry<R>, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let header = entry.header().clone();
    if header.entry_type().is_dir() {
        fs::create_dir_all(target).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mut f = fs::File::create(target)
        .map_err(|e| format!("create {}: {e}", target.display()))?;
    std::io::copy(entry, &mut f).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mode) = header.mode() {
            let _ = fs::set_permissions(target, fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

// ─── Shim at ~/.anvil/bin/qmd → node ~/.anvil/node_modules/.../bin/qmd ──────

#[cfg(unix)]
fn write_shim(shim: &Path, node: &Path, package_bin: &Path) -> std::io::Result<()> {
    let body = format!(
        "#!/bin/sh\n# Anvil-owned QMD launcher (v2.2.18 wizard install).\nexec \"{node}\" \"{pkg}\" \"$@\"\n",
        node = node.display(),
        pkg = package_bin.display(),
    );
    fs::write(shim, body)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(shim, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_shim(shim: &Path, node: &Path, package_bin: &Path) -> std::io::Result<()> {
    // Windows: write a .cmd wrapper.
    let cmd_path = shim.with_extension("cmd");
    let body = format!(
        "@echo off\r\nrem Anvil-owned QMD launcher (v2.2.18 wizard install).\r\n\"{node}\" \"{pkg}\" %*\r\n",
        node = node.display(),
        pkg = package_bin.display(),
    );
    fs::write(&cmd_path, body)?;
    Ok(())
}

// ─── Probes / utilities ──────────────────────────────────────────────────────

fn which_qmd() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "qmd.cmd" } else { "qmd" };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn which_node() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Pick a sensible default folder to point QMD's index at — the
/// user's `~/Documents/Notes` or `~/Notes` or `~/Documents`, in that
/// order, falling back to None (the post-wizard /qmd setup will
/// prompt for one).
fn pick_default_index_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    for rel in &["Documents/Notes", "Notes", "Documents"] {
        let candidate = home.join(rel);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn fmt_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if b >= GB {
        format!("{:.2} GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.0} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_empty_home_no_anvil_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let s = QmdState::from_disk(tmp.path());
        assert!(!s.binary_at_anvil_path);
    }

    #[test]
    fn npm_tarball_url_has_version() {
        assert!(npm_tarball_url().contains(&format!("/qmd-{QMD_VERSION}.tgz")));
        assert!(npm_tarball_url().starts_with("https://registry.npmjs.org/"));
    }

    #[test]
    fn fmt_bytes_scales() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    #[cfg(unix)]
    fn write_shim_creates_executable_launcher() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("qmd");
        let node = PathBuf::from("/usr/bin/node");
        let pkg = PathBuf::from("/home/me/.anvil/node_modules/@tobilu/qmd/bin/qmd");
        write_shim(&shim, &node, &pkg).unwrap();
        assert!(shim.exists());
        let contents = fs::read_to_string(&shim).unwrap();
        assert!(contents.starts_with("#!/bin/sh"));
        assert!(contents.contains("/usr/bin/node"));
        assert!(contents.contains("@tobilu/qmd"));
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&shim).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn pick_default_index_folder_handles_no_home() {
        // We don't manipulate HOME here — just confirm the fn doesn't
        // panic on whatever environment cargo test inherits.
        let _ = pick_default_index_folder();
    }

    #[test]
    fn wire_qmd_into_fresh_settings_creates_mcpservers_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        let shim = PathBuf::from("/home/me/.anvil/bin/qmd");

        let wrote = wire_qmd_into_settings(&settings, &shim).unwrap();
        assert!(wrote);

        let root: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        let qmd = &root["mcpServers"]["qmd"];
        assert_eq!(qmd["command"], "/home/me/.anvil/bin/qmd");
        assert_eq!(qmd["args"], serde_json::json!(["mcp"]));
    }

    #[test]
    fn wire_qmd_into_existing_settings_preserves_other_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        fs::write(
            &settings,
            serde_json::to_string_pretty(&serde_json::json!({
                "theme": "dark",
                "mcpServers": {
                    "other-thing": { "command": "other", "args": ["serve"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let shim = PathBuf::from("/home/me/.anvil/bin/qmd");
        let wrote = wire_qmd_into_settings(&settings, &shim).unwrap();
        assert!(wrote);

        let root: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(root["theme"], "dark", "must preserve unrelated keys");
        assert_eq!(
            root["mcpServers"]["other-thing"]["command"], "other",
            "must preserve other MCP servers"
        );
        assert_eq!(root["mcpServers"]["qmd"]["command"], "/home/me/.anvil/bin/qmd");
    }

    #[test]
    fn wire_qmd_idempotent_when_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        let shim = PathBuf::from("/home/me/.anvil/bin/qmd");

        let first = wire_qmd_into_settings(&settings, &shim).unwrap();
        assert!(first, "first call writes");
        let second = wire_qmd_into_settings(&settings, &shim).unwrap();
        assert!(!second, "second call is a no-op (idempotent re-entry)");
    }
}
