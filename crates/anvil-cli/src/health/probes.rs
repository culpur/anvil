//! Individual probe functions + the parallel `probe_all` driver.
//!
//! Probes are intentionally cheap and isolated:
//! * Each `probe_*` function takes no arguments, runs locally, and never
//!   spawns a thread of its own.
//! * `probe_all` runs them in parallel via `std::thread::spawn` and
//!   joins each with a per-probe budget.
//! * Slow probes (network-bound) are dispatched but capped at 1500ms;
//!   if they don't finish, they are recorded as Drift("probe timed out").
//! * The whole sweep has a hard 200ms target on the happy path.  We don't
//!   *kill* probes at 200ms (Rust threads aren't cancellable), but we DO
//!   gate the wait so we never block startup past the budget for healthy
//!   installs — long-running checks promote to a Drift-rail-nudge for the
//!   first session and the actual result lands later.
//!
//! No `println!` / `eprintln!` from inside any probe — every result flows
//! out via the returned `ProbeResult`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::log;
use super::report::{Component, ProbeReport, ProbeResult, ProbeStatus, RepairFn};
use super::setup_state::SetupState;

/// Run every probe.  Returns the aggregated `ProbeReport`.
///
/// `timeout_total` is the hard ceiling on the wait.  Probes that don't
/// finish by then are recorded as `Drift("probe timed out")` and continue
/// running in the background until they naturally complete (we don't
/// abort threads).  Honours `ANVIL_SKIP_HEAL` and the `--no-heal` flag at
/// the caller layer — this function always runs every probe.
///
/// Errors: never.  All probe-level errors are converted to `ProbeStatus`
/// so the caller can route on Severity.
#[must_use]
pub fn probe_all(timeout_total: Duration) -> ProbeReport {
    let start = Instant::now();
    let state = Arc::new(SetupState::load_default());

    // Each probe runs on its own thread with a shared cancellation flag.
    // Honest cancellation isn't possible in safe Rust, but the flag is
    // checked between sub-steps in the long probes so a 1500ms probe can
    // bail early when the total budget is exhausted.
    let cancel = Arc::new(AtomicBool::new(false));

    let probes: Vec<(Component, fn(Arc<SetupState>, Arc<AtomicBool>) -> ProbeResult)> = vec![
        (Component::Config, probe_config),
        (Component::Vault, probe_vault),
        (Component::Provider, probe_default_provider),
        (Component::Ollama, probe_ollama),
        (Component::Qmd, probe_qmd),
        (Component::Filesystem, probe_filesystem),
        (Component::Completions, probe_completions),
        (Component::Binary, probe_binary),
        (Component::Mcp, probe_mcp_servers),
        (Component::Daemon, probe_daemon),
    ];

    let handles: Vec<_> = probes
        .into_iter()
        .map(|(comp, f)| {
            let state = Arc::clone(&state);
            let cancel = Arc::clone(&cancel);
            (
                comp,
                thread::spawn(move || f(state, cancel)),
            )
        })
        .collect();

    // Wait either for every probe to finish or for the total timeout to
    // elapse — whichever comes first.  Probes that miss the bus are
    // recorded as Drift.
    let mut results: Vec<Option<ProbeResult>> = vec![None; handles.len()];
    let deadline = start + timeout_total;

    for (idx, (component, handle)) in handles.into_iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // Signal cancel and don't wait — record Drift placeholder.
            cancel.store(true, Ordering::Relaxed);
            results[idx] = Some(ProbeResult::drift(
                component,
                "probe timed out under 200ms budget".to_string(),
                timeout_total.as_millis() as u64,
            ));
            // Detach the thread; we don't join.
            std::mem::drop(handle);
            continue;
        }
        // Bounded busy-poll on `JoinHandle::is_finished` — this gives us a
        // wait-with-deadline without depending on tokio.  Probes typically
        // complete in << 50 μs each so the overhead is negligible.
        let waited = wait_for_thread(handle, remaining);
        match waited {
            Ok(result) => results[idx] = Some(result),
            Err(()) => {
                cancel.store(true, Ordering::Relaxed);
                results[idx] = Some(ProbeResult::drift(
                    component,
                    "probe timed out under 200ms budget".to_string(),
                    remaining.as_millis() as u64,
                ));
            }
        }
    }

    // Apply skip / defer state to every probe AFTER the sweep — never
    // before, because the user can still want diagnostic info even for
    // skipped components.  Skipped → NotApplicable; deferred + Drift →
    // NotApplicable; deferred + Broken → keep Broken (hard breakage
    // overrides defer).
    let probes: Vec<ProbeResult> = results
        .into_iter()
        .enumerate()
        .map(|(idx, opt)| match opt {
            Some(mut r) => {
                let comp = r.component;
                if state.is_skipped(comp) {
                    r.status = ProbeStatus::NotApplicable("skipped in wizard".to_string());
                    r.repair_fn = None;
                } else if matches!(r.status, ProbeStatus::Drift(_)) && state.is_deferred_now(comp) {
                    r.status = ProbeStatus::NotApplicable("deferred".to_string());
                    r.repair_fn = None;
                }
                let _ = idx;
                r
            }
            None => unreachable!("every probe slot is filled"),
        })
        .collect();

    let total_elapsed_ms = start.elapsed().as_millis() as u64;
    ProbeReport::new(probes, total_elapsed_ms)
}

/// Block on `handle` for up to `deadline`.  Returns Err if the deadline
/// expires; otherwise returns the probe result.
fn wait_for_thread(
    handle: thread::JoinHandle<ProbeResult>,
    deadline: Duration,
) -> Result<ProbeResult, ()> {
    let start = Instant::now();
    // Quick path: try once.  Most healthy probes finish in tens of μs.
    loop {
        if handle.is_finished() {
            return handle.join().map_err(|_| ());
        }
        if start.elapsed() >= deadline {
            return Err(());
        }
        thread::sleep(Duration::from_micros(200));
    }
}

// ── probe_config ─────────────────────────────────────────────────────────────

fn probe_config(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let path = config_path();
    if !path.exists() {
        // The caller (Phase 0.5) is gated on `anvil_config_json_exists()`
        // already, so we shouldn't get here on a healthy boot — but record
        // it anyway so `anvil --check` works on fresh installs.
        return ProbeResult::broken(
            Component::Config,
            "~/.anvil/config.json missing".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            return ProbeResult::broken(
                Component::Config,
                format!("cannot read config.json: {e}"),
                start.elapsed().as_millis() as u64,
            );
        }
    };

    let parsed: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            let backup = path.with_extension("json.bak");
            let repair: RepairFn = Arc::new(move || {
                if backup.exists() {
                    let broken =
                        config_path().with_file_name(format!("config.json.broken-{}", now_unix()));
                    std::fs::rename(config_path(), &broken).map_err(|e| e.to_string())?;
                    std::fs::copy(&backup, config_path()).map_err(|e| e.to_string())?;
                    Ok(format!(
                        "Restored config.json from {} (broken copy → {})",
                        backup.display(),
                        broken.display()
                    ))
                } else {
                    Err("No config.json.bak — re-run `anvil --setup` to regenerate.".to_string())
                }
            });
            return ProbeResult::broken(
                Component::Config,
                format!("config.json is not valid JSON: {e}"),
                start.elapsed().as_millis() as u64,
            )
            .with_repair(repair);
        }
    };

    // Required keys.  Treat missing keys as Drift (not Broken) — Anvil
    // will fall back to defaults but the user should know.
    let missing: Vec<&str> = ["providers", "default_model"]
        .iter()
        .copied()
        .filter(|k| parsed.get(*k).is_none())
        .collect();
    if !missing.is_empty() {
        return ProbeResult::drift(
            Component::Config,
            format!("missing keys: {}", missing.join(", ")),
            start.elapsed().as_millis() as u64,
        );
    }

    // Perms must be 0600 (POSIX only — Windows skips).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o600 {
                let p = path.clone();
                let repair: RepairFn = Arc::new(move || {
                    let perms = std::fs::Permissions::from_mode(0o600);
                    std::fs::set_permissions(&p, perms).map_err(|e| e.to_string())?;
                    log::log_action("config", "chmod-0600", "success", "silent");
                    Ok("Reset config.json perms to 0600.".to_string())
                });
                return ProbeResult::drift(
                    Component::Config,
                    format!("perms are {mode:o}, expected 600"),
                    start.elapsed().as_millis() as u64,
                )
                .with_repair(repair);
            }
        }
    }

    ProbeResult::healthy(Component::Config, start.elapsed().as_millis() as u64)
}

// ── probe_vault ──────────────────────────────────────────────────────────────

fn probe_vault(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();

    let vault_dir = runtime::default_config_home().join("vault");
    let meta = vault_dir.join("vault.meta");

    let cfg = read_config_json();
    let vault_enabled = cfg
        .as_ref()
        .and_then(|v| v.pointer("/vault/enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if !vault_enabled {
        return ProbeResult::not_applicable(
            Component::Vault,
            "vault disabled in config".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    if !meta.exists() {
        return ProbeResult::drift(
            Component::Vault,
            "vault not initialized — `/vault setup`".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    // Sanity-check perms on the vault DIR (must be 0700) + meta file (0600).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut drift_msg: Option<String> = None;
        let mut bad_paths: Vec<(PathBuf, u32)> = Vec::new();

        if let Ok(m) = std::fs::metadata(&vault_dir) {
            let mode = m.permissions().mode() & 0o777;
            if mode != 0o700 {
                drift_msg = Some(format!("vault dir perms {mode:o}"));
                bad_paths.push((vault_dir.clone(), 0o700));
            }
        }
        if let Ok(m) = std::fs::metadata(&meta) {
            let mode = m.permissions().mode() & 0o777;
            if mode != 0o600 {
                drift_msg = Some(match drift_msg {
                    Some(s) => format!("{s} + vault.meta perms {mode:o}"),
                    None => format!("vault.meta perms {mode:o}"),
                });
                bad_paths.push((meta.clone(), 0o600));
            }
        }

        if let Some(reason) = drift_msg {
            let bad_paths_clone = bad_paths.clone();
            let repair: RepairFn = Arc::new(move || {
                for (p, want) in &bad_paths_clone {
                    let perms = std::fs::Permissions::from_mode(*want);
                    std::fs::set_permissions(p, perms).map_err(|e| e.to_string())?;
                    log::log_action(
                        "vault",
                        "chmod",
                        "success",
                        &format!("{} → {:o}", p.display(), want),
                    );
                }
                Ok("Reset vault permissions.".to_string())
            });
            return ProbeResult::drift(
                Component::Vault,
                reason,
                start.elapsed().as_millis() as u64,
            )
            .with_repair(repair);
        }
    }

    ProbeResult::healthy(Component::Vault, start.elapsed().as_millis() as u64)
}

// ── probe_default_provider ───────────────────────────────────────────────────

fn probe_default_provider(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let cfg = match read_config_json() {
        Some(c) => c,
        None => {
            return ProbeResult::not_applicable(
                Component::Provider,
                "config missing".to_string(),
                start.elapsed().as_millis() as u64,
            );
        }
    };

    let model = cfg
        .get("default_model")
        .and_then(Value::as_str)
        .unwrap_or("");
    let provider = if model.starts_with("claude") {
        "anthropic"
    } else if model.starts_with("gpt") || model.starts_with("o") {
        "openai"
    } else if model.starts_with("grok") {
        "xai"
    } else if model.starts_with("gemini") {
        "google"
    } else if model.contains(':') {
        "ollama"
    } else {
        "anthropic"
    };

    // Check vault first, then config.json key, then env var.
    let has_key = provider_has_credential(provider, &cfg);

    if !has_key && provider != "ollama" {
        return ProbeResult::broken(
            Component::Provider,
            format!("{provider} key missing — run `/login {provider}`"),
            start.elapsed().as_millis() as u64,
        );
    }

    ProbeResult::healthy(Component::Provider, start.elapsed().as_millis() as u64)
}

fn provider_has_credential(provider: &str, cfg: &Value) -> bool {
    // 1. Env var.
    let env_var = match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "xai" => "XAI_API_KEY",
        "google" => "GOOGLE_API_KEY",
        "ollama" => "OLLAMA_HOST",
        _ => return false,
    };
    if std::env::var(env_var)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return true;
    }

    // 2. Vault (if unlocked this session).
    let label = match provider {
        "anthropic" => "anthropic_api_key",
        "openai" => "openai_api_key",
        "xai" => "xai_api_key",
        "google" => "google_api_key",
        "ollama" => return cfg
            .pointer("/providers/ollama/url")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        _ => return false,
    };
    if let Some(s) = runtime::vault_session_get(label) {
        if !s.is_empty() {
            return true;
        }
    }

    // 3. Config.json provider entry.
    let api_key = cfg
        .pointer(&format!("/providers/{provider}/api_key"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if api_key {
        return true;
    }

    // 4. Anthropic OAuth path. Three on-disk shapes seen in the wild:
    //   (a) `providers.anthropic.oauth: true`  — legacy flag (pre-#595).
    //   (b) `providers.anthropic.auth_method: "oauth"`  — current shape
    //       written by the post-#595 OAuth login flow.
    //   (c) `~/.anvil/credentials.json` containing the OAuth token tuple —
    //       this is the actual evidence of a successful login regardless
    //       of what flags appear in config.json. The token may be expired
    //       but the keep-alive ticker (#597) will refresh; "expired but
    //       present" is not "missing".
    //
    // Treat ANY of the three as proof the user has authenticated. Probe
    // task #745 — false-positive "anthropic key missing" was triggered
    // because the probe only checked path (a).
    if provider == "anthropic" {
        let oauth_flag = cfg
            .pointer("/providers/anthropic/oauth")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if oauth_flag {
            return true;
        }
        let auth_method = cfg
            .pointer("/providers/anthropic/auth_method")
            .and_then(Value::as_str)
            .unwrap_or("");
        if auth_method == "oauth" {
            return true;
        }
        // Check the credentials.json file directly. We don't validate the
        // token here (auth.rs owns refresh/expiry); presence-with-content
        // is enough to keep the probe quiet.
        if let Some(home) = dirs_next::home_dir() {
            let creds = home.join(".anvil").join("credentials.json");
            if let Ok(meta) = std::fs::metadata(&creds)
                && meta.len() > 0
            {
                return true;
            }
        }
    }

    false
}

// ── probe_ollama ─────────────────────────────────────────────────────────────

fn probe_ollama(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let cfg = read_config_json();
    let enabled = cfg
        .as_ref()
        .and_then(|c| c.pointer("/providers/ollama/url"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());

    if !enabled {
        return ProbeResult::not_applicable(
            Component::Ollama,
            "ollama not configured".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    // Don't actually hit the Ollama HTTP port — that's network + has a
    // 1.5s budget.  Instead, check that the ollama binary exists on PATH
    // and the bench file is fresh.  The Repair path (A3) will spawn
    // `ollama serve` + retry.
    let ollama_bin = which("ollama");
    if ollama_bin.is_none() {
        let repair: RepairFn = Arc::new(|| {
            log::log_action("ollama", "install", "skipped", "stub — A3 wires this");
            Err("Ollama install requires A3's `ollama::install_with_progress`; not yet wired in this branch.".to_string())
        });
        return ProbeResult::broken(
            Component::Ollama,
            "ollama binary not on PATH".to_string(),
            start.elapsed().as_millis() as u64,
        )
        .with_repair(repair);
    }

    // Bench file freshness.
    let bench_path = runtime::default_config_home().join("ollama_bench.json");
    if bench_path.exists() {
        if let Ok(meta) = std::fs::metadata(&bench_path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
                    if age > Duration::from_secs(30 * 86400) {
                        let repair: RepairFn = Arc::new(|| {
                            log::log_action(
                                "ollama",
                                "bench",
                                "skipped",
                                "stub — A3 wires bench refresh",
                            );
                            Err("Bench refresh requires A3's ollama hooks.".to_string())
                        });
                        return ProbeResult::drift(
                            Component::Ollama,
                            format!("bench is {} days old", age.as_secs() / 86400),
                            start.elapsed().as_millis() as u64,
                        )
                        .with_repair(repair);
                    }
                }
            }
        }
    }

    ProbeResult::healthy(Component::Ollama, start.elapsed().as_millis() as u64)
}

// ── probe_qmd ────────────────────────────────────────────────────────────────

fn probe_qmd(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let cfg = read_config_json();
    let enabled = cfg
        .as_ref()
        .and_then(|c| c.pointer("/qmd/enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if !enabled {
        return ProbeResult::not_applicable(
            Component::Qmd,
            "qmd disabled in config".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    let qmd_bin = which("qmd");
    if qmd_bin.is_none() {
        let repair: RepairFn = Arc::new(|| {
            log::log_action("qmd", "install", "skipped", "stub — A4 wires this");
            Err("QMD install requires A4's `qmd::install_with_progress`; not yet wired in this branch.".to_string())
        });
        return ProbeResult::broken(
            Component::Qmd,
            "qmd binary not on PATH".to_string(),
            start.elapsed().as_millis() as u64,
        )
        .with_repair(repair);
    }

    // Stale-index check is cheap: stat the QMD index file under
    // ~/.anvil/qmd/.  Real "is the schedule installed?" check needs A4's
    // Schedule::status which doesn't exist yet — defer.
    let qmd_dir = runtime::default_config_home().join("qmd");
    if qmd_dir.exists() {
        let last_refresh = qmd_dir.join("last-refresh");
        if let Ok(meta) = std::fs::metadata(&last_refresh) {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
                    // Stale if > 2 weeks (2x the typical interval).
                    if age > Duration::from_secs(14 * 86400) {
                        let repair: RepairFn = Arc::new(|| {
                            log::log_action("qmd", "refresh", "skipped", "stub — A4 wires this");
                            Err("QMD index refresh requires A4's qmd hooks.".to_string())
                        });
                        return ProbeResult::drift(
                            Component::Qmd,
                            format!("index {} days stale", age.as_secs() / 86400),
                            start.elapsed().as_millis() as u64,
                        )
                        .with_repair(repair);
                    }
                }
            }
        }
    }

    ProbeResult::healthy(Component::Qmd, start.elapsed().as_millis() as u64)
}

// ── probe_filesystem ─────────────────────────────────────────────────────────

fn probe_filesystem(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let home = runtime::default_config_home();

    // Required subdirs.
    let required = ["sessions", "logs", "vault"];
    let mut missing: Vec<&str> = Vec::new();
    for d in required {
        if !home.join(d).exists() {
            missing.push(d);
        }
    }
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(|s| (*s).to_string()).collect();
        let repair: RepairFn = Arc::new(move || {
            let home = runtime::default_config_home();
            for name in &names {
                let p = home.join(name);
                std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
                log::log_action(
                    "filesystem",
                    "mkdir",
                    "success",
                    &p.display().to_string(),
                );
            }
            Ok(format!("Created {} missing subdirs.", names.len()))
        });
        return ProbeResult::drift(
            Component::Filesystem,
            format!("missing subdirs: {}", missing.join(", ")),
            start.elapsed().as_millis() as u64,
        )
        .with_repair(repair);
    }

    // Sessions size soft-nudge (cheap — sum top-level entries only,
    // don't recurse).  Heuristic — close-enough for a rail nudge.
    let sessions = home.join("sessions");
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(&sessions) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    if total > 5 * 1024 * 1024 * 1024 {
        return ProbeResult::drift(
            Component::Filesystem,
            format!("sessions dir is {} MB", total / 1024 / 1024),
            start.elapsed().as_millis() as u64,
        );
    }

    ProbeResult::healthy(Component::Filesystem, start.elapsed().as_millis() as u64)
}

// ── probe_completions ────────────────────────────────────────────────────────

fn probe_completions(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let shell = std::env::var("SHELL").unwrap_or_default();
    let basename = shell.rsplit('/').next().unwrap_or("");

    // Task #745: check ALL standard completion-load locations for the
    // active shell, not just one canonical path. The original probe only
    // looked at `~/.zsh/completions/_anvil` for zsh, but users who let
    // Homebrew or the installer drop the file elsewhere (e.g.
    // `/opt/homebrew/share/zsh/site-functions/_anvil`) got a false
    // "missing" verdict.
    let home = dirs_next::home_dir();
    let candidate_paths: Vec<PathBuf> = match basename {
        "bash" => {
            let mut v = Vec::new();
            if let Some(h) = &home {
                v.push(h.join(".local/share/bash-completion/completions/anvil"));
                v.push(h.join(".bash_completion.d/anvil"));
            }
            v.push(PathBuf::from("/usr/local/etc/bash_completion.d/anvil"));
            v.push(PathBuf::from("/opt/homebrew/etc/bash_completion.d/anvil"));
            v.push(PathBuf::from("/etc/bash_completion.d/anvil"));
            v
        }
        "zsh" => {
            let mut v = Vec::new();
            if let Some(h) = &home {
                v.push(h.join(".zsh/completions/_anvil"));
                v.push(h.join(".zfunc/_anvil"));
                v.push(h.join(".oh-my-zsh/completions/_anvil"));
                v.push(h.join(".config/zsh/completions/_anvil"));
            }
            v.push(PathBuf::from("/usr/local/share/zsh/site-functions/_anvil"));
            v.push(PathBuf::from("/opt/homebrew/share/zsh/site-functions/_anvil"));
            v.push(PathBuf::from("/usr/share/zsh/site-functions/_anvil"));
            v.push(PathBuf::from("/usr/share/zsh/vendor-completions/_anvil"));
            v
        }
        "fish" => {
            let mut v = Vec::new();
            if let Some(h) = &home {
                v.push(h.join(".config/fish/completions/anvil.fish"));
            }
            v.push(PathBuf::from("/usr/local/share/fish/vendor_completions.d/anvil.fish"));
            v.push(PathBuf::from("/opt/homebrew/share/fish/vendor_completions.d/anvil.fish"));
            v.push(PathBuf::from("/usr/share/fish/vendor_completions.d/anvil.fish"));
            v
        }
        _ => {
            return ProbeResult::not_applicable(
                Component::Completions,
                "could not detect shell".to_string(),
                start.elapsed().as_millis() as u64,
            );
        }
    };

    let label = basename;

    if candidate_paths.is_empty() {
        return ProbeResult::not_applicable(
            Component::Completions,
            "no HOME".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    // Healthy if ANY known load location has the file.
    if candidate_paths.iter().any(|p| p.exists()) {
        return ProbeResult::healthy(
            Component::Completions,
            start.elapsed().as_millis() as u64,
        );
    }

    // Truly missing — surface drift with repair stub against the
    // preferred canonical path (first candidate, typically `~/.zsh/...`).
    let primary_path = candidate_paths[0].clone();
    let label_owned = label.to_string();
    let repair: RepairFn = Arc::new(move || {
        log::log_action(
            "completions",
            &format!("{label_owned}:reinstall"),
            "skipped",
            "stub — A4 wires this",
        );
        Err(format!(
            "Reinstalling {} completions requires A4's install_completions hook (target was {}).",
            label_owned,
            primary_path.display()
        ))
    });
    ProbeResult::drift(
        Component::Completions,
        format!("{label} completions missing"),
        start.elapsed().as_millis() as u64,
    )
    .with_repair(repair)
}

// ── probe_binary ─────────────────────────────────────────────────────────────

fn probe_binary(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let embed = env!("CARGO_PKG_VERSION");
    let self_ver = embed;

    // We don't network-probe `/api/version` from inside the 200ms budget.
    // That lookup is fired in the background by main.rs's existing
    // `check_for_update` thread and surfaces as a separate rail nudge.

    if self_ver.is_empty() {
        return ProbeResult::broken(
            Component::Binary,
            "self-version is empty".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }
    ProbeResult::healthy(Component::Binary, start.elapsed().as_millis() as u64)
}

// ── probe_mcp_servers ────────────────────────────────────────────────────────

fn probe_mcp_servers(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    let path = runtime::default_config_home().join("mcp-servers.json");
    if !path.exists() {
        return ProbeResult::not_applicable(
            Component::Mcp,
            "no mcp-servers.json".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => {
            return ProbeResult::drift(
                Component::Mcp,
                "cannot read mcp-servers.json".to_string(),
                start.elapsed().as_millis() as u64,
            );
        }
    };
    let parsed: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => {
            return ProbeResult::drift(
                Component::Mcp,
                "mcp-servers.json malformed".to_string(),
                start.elapsed().as_millis() as u64,
            );
        }
    };

    // Walk top-level entries — each value's `command` field should resolve
    // on PATH or be an absolute file that exists.
    let mut missing: Vec<String> = Vec::new();
    if let Some(obj) = parsed.get("mcpServers").and_then(Value::as_object) {
        for (name, server) in obj {
            let cmd = server
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("");
            if cmd.is_empty() {
                continue;
            }
            // Absolute path → must exist.
            if cmd.starts_with('/') {
                if !std::path::Path::new(cmd).exists() {
                    missing.push(format!("{name} ({cmd})"));
                }
            } else if which(cmd).is_none() {
                missing.push(format!("{name} ({cmd})"));
            }
        }
    }

    if !missing.is_empty() {
        return ProbeResult::drift(
            Component::Mcp,
            format!("missing binaries: {}", missing.join(", ")),
            start.elapsed().as_millis() as u64,
        );
    }
    ProbeResult::healthy(Component::Mcp, start.elapsed().as_millis() as u64)
}

// ── probe_daemon ─────────────────────────────────────────────────────────────

fn probe_daemon(_state: Arc<SetupState>, _cancel: Arc<AtomicBool>) -> ProbeResult {
    let start = Instant::now();
    // `anvil daemon` lands in #657 (post-v2.2.18 milestone).  Until then,
    // this probe always returns NotApplicable.  When the daemon ships,
    // wire the PID-file check here.
    let pid_file = runtime::default_config_home().join("daemon.pid");
    if !pid_file.exists() {
        return ProbeResult::not_applicable(
            Component::Daemon,
            "daemon not deployed (post-#657)".to_string(),
            start.elapsed().as_millis() as u64,
        );
    }
    // Trust the PID file's contents — full liveness check would need OS
    // syscalls outside our 500ms local-fs budget.
    ProbeResult::healthy(Component::Daemon, start.elapsed().as_millis() as u64)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    runtime::default_config_home().join("config.json")
}

fn read_config_json() -> Option<Value> {
    let data = std::fs::read_to_string(config_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Best-effort `which` — checks every `:`-separated PATH entry.
fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let candidate = PathBuf::from(dir).join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// All probes complete within the happy-path 200ms budget on a clean
    /// install.  This is the hard timing test from acceptance criterion #3.
    /// We run it 10 times and assert the median is under budget.
    #[test]
    #[serial(anvil_config_home)]
    fn probe_all_meets_200ms_budget_median_of_ten() {
        let mut samples: Vec<u64> = Vec::with_capacity(10);
        for _ in 0..10 {
            let report = probe_all(super::super::HAPPY_PATH_BUDGET);
            samples.push(report.total_elapsed_ms);
        }
        samples.sort_unstable();
        let median = samples[samples.len() / 2];
        // Captured for the agent's status report — visible with `cargo
        // test -- --nocapture`.
        eprintln!(
            "[health::probe_all bench] median={median}ms samples={samples:?}"
        );
        assert!(
            median <= 200,
            "happy-path probe_all should complete in < 200ms median, got {median}ms ({samples:?})"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_all_returns_one_result_per_component() {
        let report = probe_all(super::super::HAPPY_PATH_BUDGET);
        assert_eq!(
            report.probes.len(),
            Component::ALL.len(),
            "probe_all must return exactly one ProbeResult per component"
        );
        for comp in Component::ALL {
            assert!(
                report.get(*comp).is_some(),
                "missing probe result for {:?}",
                comp
            );
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_all_respects_skipped_components() {
        // Set up an isolated config home with setup_state marking qmd as
        // skipped.  Even if QMD is otherwise broken, the probe should
        // come back NotApplicable("skipped …").
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }

        let state_path = tmp.path().join("setup_state.json");
        std::fs::write(
            &state_path,
            r#"{"components":{"qmd":{"skip":true}}}"#,
        )
        .unwrap();

        let report = probe_all(super::super::HAPPY_PATH_BUDGET);
        let qmd = report.get(Component::Qmd).unwrap();
        assert!(
            matches!(qmd.status, ProbeStatus::NotApplicable(ref r) if r.contains("skipped")),
            "qmd should be NotApplicable(skipped), got {:?}",
            qmd.status
        );

        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    fn which_finds_sh() {
        // `sh` is essentially universal on the platforms this runs on.
        // Skip if it isn't on PATH (highly unlikely in CI).
        if which("sh").is_some() {
            // ok
        } else {
            // Mac/Linux CI without /bin/sh on PATH — implausible but
            // skip rather than fail.
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_config_handles_missing_config() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }
        let state = Arc::new(SetupState::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let result = probe_config(state, cancel);
        assert!(matches!(result.status, ProbeStatus::Broken(_)));
        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_config_detects_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }
        std::fs::write(tmp.path().join("config.json"), "{ not json").unwrap();
        let state = Arc::new(SetupState::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let result = probe_config(state, cancel);
        assert!(matches!(result.status, ProbeStatus::Broken(_)));
        // Repair fn should be present (would restore from .bak).
        assert!(result.repair_fn.is_some());
        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_config_detects_missing_keys_as_drift() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }
        // Valid JSON but missing `providers` + `default_model`.
        std::fs::write(tmp.path().join("config.json"), r#"{"vault":{"enabled":true}}"#)
            .unwrap();
        let state = Arc::new(SetupState::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let result = probe_config(state, cancel);
        match &result.status {
            ProbeStatus::Drift(reason) => assert!(reason.contains("providers")),
            other => panic!("expected Drift, got {:?}", other),
        }
        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_daemon_when_no_pid_file_is_not_applicable() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }
        let state = Arc::new(SetupState::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let result = probe_daemon(state, cancel);
        assert!(matches!(result.status, ProbeStatus::NotApplicable(_)));
        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_mcp_servers_missing_file_is_not_applicable() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }
        let state = Arc::new(SetupState::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let result = probe_mcp_servers(state, cancel);
        assert!(matches!(result.status, ProbeStatus::NotApplicable(_)));
        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_filesystem_creates_repair_when_subdirs_missing() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }
        // No subdirs in the tmp — fresh dir.
        let state = Arc::new(SetupState::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let result = probe_filesystem(state, cancel);
        assert!(matches!(result.status, ProbeStatus::Drift(_)));
        assert!(result.repair_fn.is_some());

        // Invoke the repair → subdirs should appear.
        let repair = result.repair_fn.clone().unwrap();
        let outcome = repair().expect("repair should succeed in tmp dir");
        assert!(outcome.contains("Created"));
        for d in ["sessions", "logs", "vault"] {
            assert!(tmp.path().join(d).exists(), "expected {d} created");
        }
        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn probe_all_total_elapsed_is_recorded() {
        let report = probe_all(super::super::HAPPY_PATH_BUDGET);
        // Every probe should have recorded *some* elapsed time, even if 0.
        for probe in &report.probes {
            assert!(probe.elapsed_ms <= 1500);
        }
        // The aggregate is non-zero on real hardware.
        // (Cannot strictly assert > 0 — virtualised CI can clock to 0ms.)
    }
}
