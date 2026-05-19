//! In-wizard Ollama installation + model pull (v2.2.18 #662 rebuild).
//!
//! Replaces the dishonest A3 scaffolding from earlier in this branch.
//!
//! ## What this module does
//!
//! 1. **Detect** existing Ollama installs (system or our owned path).
//! 2. **Download** the official Ollama release directly via reqwest —
//!    no curl|sh, no brew, no package manager.
//! 3. **Extract** the binary into `~/.anvil/bin/ollama` so we own it.
//! 4. **Spawn** the daemon as Anvil's child if nothing is listening on
//!    `localhost:11434`.
//! 5. **Pull a model** by spawning our own ollama binary as a
//!    subprocess and streaming its progress through A1's
//!    `StreamingOutputModal`.  This is NOT a system shell-out — it's
//!    Anvil running its own owned binary.
//!
//! ## What this module deliberately does NOT do
//!
//! - **No `curl … | sh`.** Ollama publishes a shell installer; we
//!   ignore it and fetch the tarball directly.
//! - **No `brew install` / `pkg install`.**
//! - **No system path writes outside `~/.anvil/`.**
//! - **No system Ollama overwrite.**  If the user has a system Ollama
//!   we detect it and reuse it; we never write to its install
//!   location.
//!
//! ## Re-entry / idempotence
//!
//! `from_disk()` reads the current state.  The orchestrator skips
//! steps that are already done (binary present, daemon up, model
//! pulled).  Running `anvil --setup` a second time renders ✓ cards
//! instead of repeating work.  Matches the `/heal` "corrective, not
//! destructive" philosophy.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::style::Color;

use crate::tui::modals::confirm::ConfirmModal;
use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
use crate::tui::modals::streaming::StreamingOutputModal;
use crate::wizard_runner::{KeySource, RunnerError, TerminalHooks, WizardModalRunner};

// ─── Public types ────────────────────────────────────────────────────────────

/// Outcome from the Ollama wizard step.  Returned to the orchestrator
/// so it can be merged into `config.json` along with everything else.
#[derive(Debug, Clone, Default)]
pub struct OllamaOutcome {
    pub choice: Option<String>,
    pub binary_path: Option<PathBuf>,
    pub url: Option<String>,
    pub anvil_owned: bool,
    pub pulled_model: Option<String>,
    pub defer_remaining: Option<u32>,
}

/// Snapshot of on-disk + network state at wizard entry.
#[derive(Debug, Clone, Default)]
pub struct OllamaState {
    pub binary_at_anvil_path: bool,
    pub anvil_binary_path: PathBuf,
    pub system_binary: Option<PathBuf>,
    pub daemon_reachable: bool,
    pub url: String,
    pub installed_models: Vec<String>,
}

impl OllamaState {
    pub fn from_disk(home: &Path) -> Self {
        let bin_name = if cfg!(windows) { "ollama.exe" } else { "ollama" };
        let anvil_binary_path = home.join("bin").join(bin_name);
        let binary_at_anvil_path = anvil_binary_path.exists();
        let system_binary = which_ollama();
        let url = "http://localhost:11434".to_string();
        let (daemon_reachable, installed_models) = probe_daemon(&url);
        Self {
            binary_at_anvil_path,
            anvil_binary_path,
            system_binary,
            daemon_reachable,
            url,
            installed_models,
        }
    }
}

// ─── Curated model list ──────────────────────────────────────────────────────

pub struct ModelCandidate {
    pub tag: &'static str,
    pub label: &'static str,
}

/// Hardcoded curated list. Hardware-aware ranking is task #665 — not
/// in scope for this rebuild; we render the same list to everyone for
/// now.
pub fn curated_models() -> &'static [ModelCandidate] {
    &[
        ModelCandidate {
            tag: "qwen2.5-coder:7b",
            label: "Qwen2.5 Coder 7B — best general coding (~4.7 GB)",
        },
        ModelCandidate {
            tag: "llama3.2:3b",
            label: "Llama 3.2 3B — small + fast (~2.0 GB)",
        },
        ModelCandidate {
            tag: "qwen2.5-coder:1.5b",
            label: "Qwen2.5 Coder 1.5B — works on 8 GB RAM (~1.0 GB)",
        },
    ]
}

// ─── Wizard entry point ──────────────────────────────────────────────────────

/// Drive the Ollama wizard step inside the existing alt-screen.
pub(crate) fn run_ollama_step<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    home: &Path,
) -> Result<OllamaOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let accent = Color::Cyan;
    let state = OllamaState::from_disk(home);

    runner.session.render_banner_with_description(
        "Step 5 of 8 — Local AI (Ollama)",
        "Anvil can install + run Ollama directly so you have a local model that works \
         offline. Anvil downloads it, doesn't shell out to a system installer, \
         and keeps everything under ~/.anvil/.",
        &[],
        accent,
    )?;

    // Already fully set up? Render ✓ + offer to use it.
    if state.daemon_reachable && !state.installed_models.is_empty() {
        let body = format!(
            "Ollama is already running at {} with {} model(s):\n  {}\n\n\
             Anvil will reuse the existing setup. Press Enter to continue.",
            state.url,
            state.installed_models.len(),
            state.installed_models.join(", "),
        );
        let modal = ConfirmModal::new("Ollama already set up ✓", body);
        let _ = runner.run_confirm("step5-ollama-ready", modal)?;
        return Ok(OllamaOutcome {
            choice: Some("UseExisting".to_string()),
            binary_path: state.system_binary.or(Some(state.anvil_binary_path)),
            url: Some(state.url),
            anvil_owned: state.binary_at_anvil_path,
            pulled_model: state.installed_models.first().cloned(),
            defer_remaining: None,
        });
    }

    // Build the choice list. Top option depends on what's already on disk.
    let top_label = if state.binary_at_anvil_path {
        "Resume — re-use the Ollama Anvil installed previously".to_string()
    } else if state.system_binary.is_some() {
        "Use the system Ollama Anvil detected".to_string()
    } else {
        "Install Ollama now (Anvil downloads + manages it) — recommended".to_string()
    };

    let modal = WizardChoiceModal::new(
        "Local AI (Ollama)",
        vec![
            top_label,
            "I'll handle Ollama myself".to_string(),
            "Skip — no local AI".to_string(),
            "Maybe later".to_string(),
        ],
    );
    let answer = runner.run_choice("step5-ollama", modal)?;
    match answer {
        ModalAnswer::Choice(0) => run_install_branch(runner, home, &state),
        ModalAnswer::Choice(1) => run_existing_branch(runner, &state),
        ModalAnswer::Choice(2) => Ok(OllamaOutcome {
            choice: Some("Skip".to_string()),
            ..Default::default()
        }),
        _ => Ok(OllamaOutcome {
            choice: Some("Defer".to_string()),
            defer_remaining: Some(5),
            ..Default::default()
        }),
    }
}

// ─── "Use existing" branch ───────────────────────────────────────────────────

fn run_existing_branch<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    state: &OllamaState,
) -> Result<OllamaOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let body = format!(
        "Default Ollama URL: {}\nDaemon: {}\n\nUse /ollama setup later if you need a \
         different URL. Press Enter to continue.",
        state.url,
        if state.daemon_reachable {
            "reachable ✓"
        } else {
            "NOT reachable — start it before sending a message"
        },
    );
    let modal = ConfirmModal::new("Use existing Ollama", body);
    let _ = runner.run_confirm("step5-ollama-existing", modal)?;
    Ok(OllamaOutcome {
        choice: Some("UseExisting".to_string()),
        binary_path: state.system_binary.clone(),
        url: Some(state.url.clone()),
        anvil_owned: false,
        pulled_model: state.installed_models.first().cloned(),
        defer_remaining: None,
    })
}

// ─── "Install" branch ────────────────────────────────────────────────────────

fn run_install_branch<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    home: &Path,
    state: &OllamaState,
) -> Result<OllamaOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let accent = Color::Cyan;
    let bin_dir = home.join("bin");
    let _ = fs::create_dir_all(&bin_dir);
    let bin_name = if cfg!(windows) { "ollama.exe" } else { "ollama" };
    let target_path = bin_dir.join(bin_name);

    // ── Step A: download binary (skip if already at target) ─────────────
    if !state.binary_at_anvil_path {
        match download_ollama_with_banner(runner, &target_path)? {
            DownloadResult::Ok => {}
            DownloadResult::Failed(reason) => {
                let body = format!(
                    "Could not install Ollama:\n  {reason}\n\nRetry from /ollama \
                     setup later. Press Enter to continue without local AI."
                );
                let modal = ConfirmModal::new("Install failed", body);
                let _ = runner.run_confirm("step5-ollama-fail", modal)?;
                return Ok(OllamaOutcome {
                    choice: Some("Skip".to_string()),
                    ..Default::default()
                });
            }
        }
    } else {
        runner.session.render_banner(
            "Ollama binary already installed ✓",
            &[
                &format!("Found {} from a prior install.", target_path.display()),
                "Press Enter to continue.",
            ],
            accent,
        )?;
    }

    // ── Step B: start the daemon if not already running ─────────────────
    let url = "http://localhost:11434".to_string();
    let (already_up, _) = probe_daemon(&url);
    if !already_up {
        runner.session.render_banner(
            "Starting Ollama daemon…",
            &["Spawning `ollama serve` as an Anvil-owned background process."],
            accent,
        )?;
        if let Err(e) = spawn_owned_daemon(home, &target_path) {
            let body = format!(
                "Could not start the daemon: {e}\n\nStart it yourself with `ollama \
                 serve` and rerun the wizard. Press Enter to continue."
            );
            let modal = ConfirmModal::new("Daemon failed", body);
            let _ = runner.run_confirm("step5-daemon-fail", modal)?;
            return Ok(OllamaOutcome {
                choice: Some("Install".to_string()),
                binary_path: Some(target_path),
                url: Some(url),
                anvil_owned: true,
                pulled_model: None,
                defer_remaining: None,
            });
        }
        wait_for_daemon(&url, Duration::from_secs(15));
    }

    // ── Step C: pick a model ────────────────────────────────────────────
    let candidates = curated_models();
    let mut labels: Vec<String> = candidates
        .iter()
        .map(|c| format!("{} — {}", c.tag, c.label))
        .collect();
    labels.push("Skip — I'll pull one later".to_string());

    let modal = WizardChoiceModal::new("Pick a model", labels);
    let answer = runner.run_choice("step5-model", modal)?;
    let model_tag = match answer {
        ModalAnswer::Choice(i) if i < candidates.len() => candidates[i].tag.to_string(),
        _ => {
            return Ok(OllamaOutcome {
                choice: Some("Install".to_string()),
                binary_path: Some(target_path),
                url: Some(url),
                anvil_owned: true,
                pulled_model: None,
                defer_remaining: None,
            });
        }
    };

    // ── Step D: pull the model via our owned binary (subprocess) ────────
    let mut cmd = Command::new(&target_path);
    cmd.arg("pull").arg(&model_tag);
    let modal = StreamingOutputModal::new(
        format!("Pulling {model_tag}"),
        "Anvil is running its own `ollama pull` subprocess",
    )
    .with_subprocess(cmd);

    let pull_result = runner.run_streaming_output("step5-pull", modal)?;
    let pulled_ok = matches!(
        pull_result,
        ModalAnswer::StreamingResult { exit_code: 0, .. }
    );

    if !pulled_ok {
        let body = format!(
            "`ollama pull {model_tag}` did not finish successfully.\n\n\
             You can retry with /ollama pull {model_tag} later. Press Enter to continue."
        );
        let modal = ConfirmModal::new("Pull failed", body);
        let _ = runner.run_confirm("step5-pull-fail", modal)?;
        return Ok(OllamaOutcome {
            choice: Some("Install".to_string()),
            binary_path: Some(target_path),
            url: Some(url),
            anvil_owned: true,
            pulled_model: None,
            defer_remaining: None,
        });
    }

    Ok(OllamaOutcome {
        choice: Some("Install".to_string()),
        binary_path: Some(target_path),
        url: Some(url),
        anvil_owned: true,
        pulled_model: Some(model_tag),
        defer_remaining: None,
    })
}

// ─── Binary download — in-process reqwest, banner-as-progress ───────────────

enum DownloadResult {
    Ok,
    Failed(String),
}

/// Download Ollama via reqwest in a background thread.  The wizard
/// foreground thread re-renders the banner with current bytes every
/// 250ms so the user sees live progress without any subprocess or
/// shell-out.
fn download_ollama_with_banner<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    target_path: &Path,
) -> Result<DownloadResult, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let url = match release_url() {
        Ok(u) => u,
        Err(e) => return Ok(DownloadResult::Failed(e)),
    };

    let bytes_done = Arc::new(AtomicU64::new(0));
    let total_bytes = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let result_slot: Arc<std::sync::Mutex<Option<Result<Vec<u8>, String>>>> =
        Arc::new(std::sync::Mutex::new(None));

    // Spawn the download.
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

    // Re-render the banner with live bytes every 250ms.
    let accent = Color::Cyan;
    let title = "Installing Ollama";
    while !finished.load(Ordering::Acquire) {
        let done = bytes_done.load(Ordering::Relaxed);
        let total = total_bytes.load(Ordering::Relaxed);
        let body_line_1 = format!("Downloading from {}", short_url(&url));
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
        let body_line_3 = "  Anvil downloads via reqwest — no curl, no system installer.";
        let body: &[&str] = &[&body_line_1, &body_line_2, body_line_3];
        runner.session.render_banner(title, body, accent)?;
        thread::sleep(Duration::from_millis(250));
    }
    let _ = handle.join();

    let bytes = match result_slot.lock().unwrap().take() {
        Some(Ok(b)) => b,
        Some(Err(e)) => return Ok(DownloadResult::Failed(e)),
        None => return Ok(DownloadResult::Failed("download thread panicked".into())),
    };

    // Extract / place on disk.
    runner.session.render_banner(
        title,
        &[
            "Download complete ✓",
            "Extracting + placing binary at ~/.anvil/bin/ollama…",
        ],
        accent,
    )?;
    if let Err(e) = extract_and_place(&bytes, &url, target_path) {
        return Ok(DownloadResult::Failed(e));
    }

    Ok(DownloadResult::Ok)
}

/// Pinned to a known good Ollama release.  See `feedback-anvil-capability-contract`:
/// every dependency must be deliberate.  Bumping this string is an
/// explicit Anvil release activity, not a "latest" surprise.
const OLLAMA_VERSION: &str = "v0.5.1";

fn release_url() -> Result<String, String> {
    let base = "https://github.com/ollama/ollama/releases/download";
    let url = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => format!("{base}/{OLLAMA_VERSION}/ollama-linux-amd64.tgz"),
        ("linux", "aarch64") => format!("{base}/{OLLAMA_VERSION}/ollama-linux-arm64.tgz"),
        ("macos", _) => {
            return Err(
                "macOS Ollama ships as a .app bundle that must be installed via Finder. \
                 Download from https://ollama.com/download manually, then re-run \
                 the wizard and pick 'I'll handle Ollama myself'."
                    .into(),
            );
        }
        ("windows", _) => {
            return Err(
                "Windows Ollama ships as an .exe installer. Download from \
                 https://ollama.com/download, run the installer, then re-run \
                 the wizard and pick 'I'll handle Ollama myself'."
                    .into(),
            );
        }
        (os, arch) => return Err(format!("no Ollama prebuilt for {os}/{arch}")),
    };
    Ok(url)
}

fn blocking_download(
    url: &str,
    bytes_done: &Arc<AtomicU64>,
    total_bytes: &Arc<AtomicU64>,
) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
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

fn extract_and_place(bytes: &[u8], url: &str, target: &Path) -> Result<(), String> {
    if url.ends_with(".tgz") || url.ends_with(".tar.gz") {
        let gz = flate2::read::GzDecoder::new(bytes);
        let mut ar = tar::Archive::new(gz);
        for entry in ar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            if path.file_name().map(|n| n == "ollama").unwrap_or(false) {
                let mut f = fs::File::create(target).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
                set_executable(target).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
        Err("ollama binary not found inside tarball".into())
    } else {
        // Raw binary fallback — write bytes directly.
        let mut f = fs::File::create(target).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        set_executable(target).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

// ─── Daemon lifecycle ────────────────────────────────────────────────────────

fn spawn_owned_daemon(home: &Path, binary: &Path) -> Result<(), String> {
    let run_dir = home.join("run");
    let _ = fs::create_dir_all(&run_dir);
    let log_path = run_dir.join("ollama.log");
    let log = fs::File::create(&log_path)
        .map_err(|e| format!("open {}: {e}", log_path.display()))?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new(binary)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .map_err(|e| format!("spawn ollama serve: {e}"))?;
    let pid_path = run_dir.join("ollama.pid");
    fs::write(&pid_path, child.id().to_string())
        .map_err(|e| format!("write {}: {e}", pid_path.display()))?;
    Ok(())
}

fn wait_for_daemon(url: &str, max: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < max {
        let (up, _) = probe_daemon(url);
        if up {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    false
}

// ─── Probes / utilities ──────────────────────────────────────────────────────

fn probe_daemon(url: &str) -> (bool, Vec<String>) {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1000))
        .build()
    else {
        return (false, vec![]);
    };
    let Ok(resp) = client.get(format!("{url}/api/tags")).send() else {
        return (false, vec![]);
    };
    if !resp.status().is_success() {
        return (true, vec![]);
    }
    let Ok(json) = resp.json::<serde_json::Value>() else {
        return (true, vec![]);
    };
    let models = json
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (true, models)
}

fn which_ollama() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "ollama.exe" } else { "ollama" };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn short_url(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
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
    fn state_from_empty_home_has_no_anvil_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let s = OllamaState::from_disk(tmp.path());
        assert!(!s.binary_at_anvil_path);
        assert_eq!(s.url, "http://localhost:11434");
    }

    #[test]
    fn fmt_bytes_scales() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn curated_models_present() {
        let m = curated_models();
        assert!(!m.is_empty());
        assert!(m.iter().any(|c| c.tag.starts_with("qwen2.5-coder")));
    }

    #[test]
    fn release_url_known_platforms() {
        let url = release_url();
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") | ("linux", "aarch64") => {
                assert!(url.is_ok());
                assert!(url.unwrap().ends_with(".tgz"));
            }
            ("macos", _) | ("windows", _) => {
                // Both currently return Err pointing the user at the manual download.
                assert!(url.is_err());
            }
            _ => {}
        }
    }

    #[test]
    fn short_url_takes_basename() {
        assert_eq!(short_url("https://example.com/path/file.tgz"), "file.tgz");
    }
}
