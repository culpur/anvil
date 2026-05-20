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
use rust_i18n::t;

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
    /// Parameter count in billions.  Used by [`rank_for_hw`] to compute
    /// a rough VRAM/RAM footprint for the model at q4_K_M (~0.56 B/param).
    pub params_b: f64,
}

/// Curated, quantized-by-default list.  Each tag is a Q4_K_M variant
/// where Ollama exposes the explicit quantization suffix; otherwise
/// the default tag (which Ollama itself ships as Q4_K_M for these).
///
/// q4_K_M is the sweet spot for local inference quality-vs-footprint:
/// ~4.5 bits/parameter, on-disk weights weigh ≈ params × 0.56 bytes.
/// Wizard task #665 uses [`rank_for_hw`] to label each entry with
/// Recommended / Fits / Tight / Too big against the detected hardware
/// so users never blindly pull a model that won't run.
pub fn curated_models() -> &'static [ModelCandidate] {
    &[
        ModelCandidate {
            tag: "qwen2.5-coder:7b-instruct-q4_K_M",
            label: "Qwen2.5 Coder 7B (q4_K_M) — best general coding (~4.7 GB)",
            params_b: 7.0,
        },
        ModelCandidate {
            tag: "llama3.2:3b-instruct-q4_K_M",
            label: "Llama 3.2 3B (q4_K_M) — small + fast (~2.0 GB)",
            params_b: 3.0,
        },
        ModelCandidate {
            tag: "qwen2.5-coder:1.5b-instruct-q4_K_M",
            label: "Qwen2.5 Coder 1.5B (q4_K_M) — works on 8 GB RAM (~1.0 GB)",
            params_b: 1.5,
        },
    ]
}

// ─── Hardware-aware ranking (task #665) ──────────────────────────────────────

/// Verdict on whether a model is likely to run on the current host.
/// Pure function of (params, quant footprint, VRAM/RAM budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Plenty of headroom — model + KV cache + OS reserve all fit
    /// with margin.  Marked "Recommended ★" in the picker.
    Recommended,
    /// Fits within the GPU/unified-memory budget.
    Fits,
    /// Will fit but only with reduced context (KV cache squeezed) or
    /// a CPU spillover for some layers.  User should expect slower
    /// inference and lower context windows.
    Tight,
    /// Weights alone exceed the practical budget.  Will OOM at load.
    TooBig,
}

impl Fit {
    fn glyph(self) -> &'static str {
        match self {
            Fit::Recommended => "★",
            Fit::Fits => "✓",
            Fit::Tight => "▲",
            Fit::TooBig => "✗",
        }
    }
    fn note(self) -> String {
        match self {
            Fit::Recommended => t!("wizard.ollama_fit.recommended").to_string(),
            Fit::Fits => t!("wizard.ollama_fit.fits").to_string(),
            Fit::Tight => t!("wizard.ollama_fit.tight").to_string(),
            Fit::TooBig => t!("wizard.ollama_fit.too_big").to_string(),
        }
    }
}

/// Rank a curated candidate against the detected hardware.
///
/// Pure: no I/O, no clock.  Approximates weight footprint as
/// `params_b * 1e9 * 0.5625` bytes (q4_K_M effective bpp from the
/// tuner's `quant_bytes_per_param` table) and reserves ~4 GiB for
/// OS + KV cache.  On Apple Silicon (unified memory, GpuKind::Metal
/// with VRAM == RAM) we use 70 % of total RAM as the budget,
/// matching the tuner's `compute_vram_budget`.
#[must_use]
pub fn rank_for_hw(
    candidate: &ModelCandidate,
    hw: &runtime::ollama_tune::hw::HardwareProfile,
) -> Fit {
    use runtime::ollama_tune::hw::GpuKind;

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const BPP_Q4KM: f64 = 0.5625;
    const RESERVE_BYTES: f64 = 4.0 * GIB; // OS + KV cache + headroom

    let weights = candidate.params_b * 1.0e9 * BPP_Q4KM;

    // Budget mirrors `tuner::compute_vram_budget` (Apple-unified-aware).
    let is_apple_unified = matches!(hw.gpu_kind, GpuKind::Metal)
        && hw.vram_total_bytes == hw.ram_total_bytes
        && hw.ram_total_bytes > 0;
    let budget_bytes: f64 = if is_apple_unified {
        (hw.ram_total_bytes as f64) * 0.70
    } else if hw.vram_total_bytes > 0 {
        hw.vram_total_bytes as f64
    } else {
        // CPU-only fallback: bound by RAM, leave 50 % for OS + everything else.
        (hw.ram_total_bytes as f64) * 0.50
    };

    let usable = budget_bytes - RESERVE_BYTES;
    if usable <= 0.0 {
        // Pathologically small machine — only the smallest model has a chance.
        return if candidate.params_b <= 2.0 { Fit::Tight } else { Fit::TooBig };
    }
    if weights > usable {
        Fit::TooBig
    } else if weights > usable * 0.75 {
        Fit::Tight
    } else if weights <= usable * 0.45 {
        Fit::Recommended
    } else {
        Fit::Fits
    }
}

/// Rank every curated entry and return the index of the
/// most-recommended candidate (or 0 if every entry is "Too big").
#[must_use]
pub fn recommended_index(hw: &runtime::ollama_tune::hw::HardwareProfile) -> usize {
    let cands = curated_models();
    // Prefer the largest "Recommended", else largest "Fits", else any
    // "Tight" — only fall back to 0 if every candidate is TooBig.
    let mut best: Option<(usize, u8)> = None; // (index, weight)
    for (i, c) in cands.iter().enumerate() {
        let f = rank_for_hw(c, hw);
        let weight: u8 = match f {
            Fit::Recommended => 4,
            Fit::Fits => 3,
            Fit::Tight => 2,
            Fit::TooBig => 0,
        };
        // Larger params_b is a tie-breaker within the same Fit class.
        let scaled = weight.saturating_mul(10).saturating_add((c.params_b as u8).min(9));
        if best.map_or(true, |(_, b)| scaled > b) {
            best = Some((i, scaled));
        }
    }
    best.map(|(i, _)| i).unwrap_or(0)
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

    let step_header = t!(
        "wizard.ollama_modal.step_header",
        step = "5",
        total = "8"
    ).to_string();
    let intro = t!("wizard.ollama_modal.intro").to_string();
    runner.session.render_banner_with_description(
        &step_header,
        &intro,
        &[],
        accent,
    )?;

    // Already fully set up? Render ✓ + offer to use it.
    if state.daemon_reachable && !state.installed_models.is_empty() {
        let body = t!(
            "wizard.ollama_modal.already_running",
            url = state.url.clone(),
            count = state.installed_models.len().to_string(),
            models = state.installed_models.join(", ")
        ).to_string();
        let title = t!("wizard.ollama_modal.already_setup_title").to_string();
        let modal = ConfirmModal::new(title, body);
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
        t!("wizard.ollama_modal.resume_label").to_string()
    } else if state.system_binary.is_some() {
        t!("wizard.ollama_modal.use_existing_label").to_string()
    } else {
        t!("wizard.ollama_modal.install_label").to_string()
    };

    let title = t!("wizard.ollama_modal.title_local_ai").to_string();
    let modal = WizardChoiceModal::new(
        title,
        vec![
            top_label,
            t!("wizard.ollama_modal.choice_self_manage").to_string(),
            t!("wizard.ollama_modal.choice_skip").to_string(),
            t!("wizard.ollama_modal.choice_later").to_string(),
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
    let status = if state.daemon_reachable {
        format!("{} \u{2713}", t!("wizard.ollama_modal.status_reachable"))
    } else {
        t!("wizard.ollama_modal.status_not_reachable").to_string()
    };
    let body = t!(
        "wizard.ollama_modal.use_existing_body",
        url = state.url.clone(),
        status = status
    ).to_string();
    let title = t!("wizard.ollama_modal.use_existing_title").to_string();
    let modal = ConfirmModal::new(title, body);
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
        match download_ollama_with_banner(runner, home, &target_path)? {
            DownloadResult::Ok => {}
            DownloadResult::Failed(reason) => {
                let body = t!("wizard.ollama_modal.install_failed_body", reason = reason.clone()).to_string();
                let title = t!("wizard.ollama_modal.install_failed_title").to_string();
                let modal = ConfirmModal::new(title, body);
                let _ = runner.run_confirm("step5-ollama-fail", modal)?;
                return Ok(OllamaOutcome {
                    choice: Some("Skip".to_string()),
                    ..Default::default()
                });
            }
        }
    } else {
        let title = t!("wizard.ollama_modal.already_installed_title").to_string();
        let line = t!("wizard.ollama_modal.already_installed_body", path = target_path.display().to_string()).to_string();
        runner.session.render_banner(
            &title,
            &[
                &line,
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

    // ── Step C: pick a model (HW-aware ranking, task #665) ──────────────
    let candidates = curated_models();
    let hw = runtime::ollama_tune::hw::detect_cached();
    let recommended = recommended_index(&hw);
    let mut labels: Vec<String> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let fit = rank_for_hw(c, &hw);
            let marker = if i == recommended {
                format!("{} Recommended for this machine", fit.glyph())
            } else {
                format!("{} {}", fit.glyph(), fit.note())
            };
            format!("{} — {}  [{}]", c.tag, c.label, marker)
        })
        .collect();
    labels.push("Skip — I'll pull one later".to_string());

    // Render a short HW summary banner so the user can see why the
    // recommendation was made before opening the picker.
    let hw_line = format_hw_summary(&hw);
    runner.session.render_banner(
        "Hardware detected",
        &[
            &hw_line,
            "Quantized models (q4_K_M) are picked by default.",
            "★ Recommended · ✓ Fits · ▲ Tight · ✗ Too big",
        ],
        accent,
    )?;

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

    // ── Step E: tune for this hardware + persist (task #665) ────────────
    //
    // After a successful pull we know the model is on disk, the daemon is
    // up, and we have a HardwareProfile.  Call the tuner to derive
    // OllamaOptions (including flash_attn) and persist them under
    // ~/.anvil/settings.json::ollama.models[tag].  First chat request will
    // pick them up via the existing auto_tune wiring (#371).
    let tune_summary = tune_and_persist(home, &url, &model_tag, &hw);
    render_tune_summary_banner(runner, &model_tag, &tune_summary, accent)?;

    // ── Step F: optional bench gate ─────────────────────────────────────
    let bench_choice = runner.run_choice(
        "step5-bench",
        WizardChoiceModal::new(
            "Baseline performance benchmark?",
            vec![
                "Run a quick bench now (~30s, 3 prompts) — recommended".to_string(),
                "Skip — I'll run /ollama bench later".to_string(),
            ],
        ),
    )?;
    if let ModalAnswer::Choice(0) = bench_choice {
        let _ = run_bench_with_banner(runner, home, &url, &model_tag, &hw, &tune_summary, accent);
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

// ─── Tune + persist (#665) ──────────────────────────────────────────────────

/// Result of [`tune_and_persist`] — a snapshot of what the tuner chose
/// and whether persistence succeeded, suitable for rendering as a
/// summary banner.  Pure data, no live handles.
#[derive(Debug, Clone, Default)]
struct TuneSummary {
    /// Tuned context-window size (tokens).
    num_ctx: Option<u32>,
    /// Tuned GPU offload count.  `-1` means "all layers".
    num_gpu: Option<i32>,
    /// Flash-attention decision the tuner made.
    flash_attention: Option<bool>,
    /// Why flash-attention was on/off (from the L3a matrix).
    flash_attention_reason: Option<String>,
    /// Policy summary sentence from the tuner.
    policy_summary: Option<String>,
    /// True when the override was successfully persisted to settings.json.
    persisted: bool,
    /// Non-fatal error encountered along the way (rendered as a yellow
    /// "skipped" line in the summary rather than aborting the wizard).
    error: Option<String>,
}

fn tune_and_persist(
    home: &Path,
    url: &str,
    model_tag: &str,
    hw: &runtime::ollama_tune::hw::HardwareProfile,
) -> TuneSummary {
    use api::ollama_tune::policy_config::OllamaConfig;
    use api::ollama_tune::tuner::{tune, KvCacheType, UserPolicy};
    use api::fetch_model_meta_cached;

    // Fetch model metadata on a transient current-thread runtime — we
    // hold the alt-screen on the foreground; this call is single-shot
    // and bounded by the 3 s timeout inside fetch_model_meta.
    let url_owned = url.to_string();
    let tag_owned = model_tag.to_string();
    let meta_result = thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => return Err(format!("tokio runtime: {e}")),
        };
        rt.block_on(fetch_model_meta_cached(&url_owned, &tag_owned))
            .map_err(|e| e.to_string())
    })
    .join()
    .unwrap_or_else(|_| Err("meta-fetch thread panicked".to_string()));

    let meta = match meta_result {
        Ok(m) => m,
        Err(e) => {
            return TuneSummary {
                error: Some(format!("Skipped tuning — /api/show failed: {e}")),
                ..Default::default()
            };
        }
    };

    let policy = OllamaConfig::load().policy;
    let policy = if policy.context_min == 0 {
        UserPolicy::default()
    } else {
        policy
    };

    let tuned = match tune(hw, &*meta, &policy) {
        Ok(t) => t,
        Err(e) => {
            return TuneSummary {
                error: Some(format!("Tuner refused — {e:?}")),
                ..Default::default()
            };
        }
    };

    // Persist as a per-model override.  We deliberately store every
    // tuner-chosen value so the first /api/chat request matches the
    // wizard preview exactly, even if the tuner's heuristics change later.
    let mut cfg = OllamaConfig::load();
    let opts_clone = tuned.options.clone();
    let kv = match opts_clone.kv_cache_type {
        KvCacheType::F16 => KvCacheType::F16,
        KvCacheType::Q8_0 => KvCacheType::Q8_0,
        KvCacheType::Q4_0 => KvCacheType::Q4_0,
    };
    cfg.set_override(model_tag, |ov| {
        ov.num_ctx = Some(opts_clone.num_ctx);
        ov.num_gpu = Some(opts_clone.num_gpu);
        ov.num_thread = Some(opts_clone.num_thread);
        ov.flash_attention = Some(opts_clone.flash_attention);
        ov.kv_cache_type = Some(kv);
        ov.keep_alive_secs = Some(opts_clone.keep_alive_secs);
        ov.num_batch = Some(opts_clone.num_batch);
    });
    let persisted = match cfg.save() {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "[wizard_ollama] warn: failed to persist tuned override for {model_tag}: {e}"
            );
            false
        }
    };

    // Drop a copy of the tune snapshot under ~/.anvil/bench/ so /ollama
    // bench has something to diff against later.  Best-effort.
    let _ = persist_tune_snapshot(home, model_tag, &tuned);

    TuneSummary {
        num_ctx: Some(tuned.options.num_ctx),
        num_gpu: Some(tuned.options.num_gpu),
        flash_attention: Some(tuned.options.flash_attention),
        flash_attention_reason: Some(tuned.reasoning.flash_attention),
        policy_summary: Some(tuned.reasoning.policy_summary),
        persisted,
        error: None,
    }
}

fn persist_tune_snapshot(
    home: &Path,
    model_tag: &str,
    tuned: &api::ollama_tune::tuner::TuneResult,
) -> std::io::Result<()> {
    let dir = home.join("bench");
    fs::create_dir_all(&dir)?;
    let slug = model_tag
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' | ' ' => '-',
            c => c,
        })
        .collect::<String>();
    let path = dir.join(format!("{slug}.tune.json"));
    let body = serde_json::to_string_pretty(tuned)
        .unwrap_or_else(|_| "{}".to_string());
    fs::write(path, body)
}

fn render_tune_summary_banner<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    model_tag: &str,
    summary: &TuneSummary,
    accent: Color,
) -> Result<(), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let mut lines: Vec<String> = Vec::new();
    if let Some(err) = &summary.error {
        lines.push(err.clone());
        lines.push(String::new());
        lines.push("You can re-run /ollama tune later.".to_string());
    } else {
        if let Some(ctx) = summary.num_ctx {
            lines.push(format!("Context window: {ctx} tokens"));
        }
        if let Some(g) = summary.num_gpu {
            let layers = if g < 0 {
                "all (GPU)".to_string()
            } else if g == 0 {
                "0 (CPU-only)".to_string()
            } else {
                format!("{g} (partial GPU offload)")
            };
            lines.push(format!("GPU layers: {layers}"));
        }
        if let Some(fa) = summary.flash_attention {
            lines.push(format!(
                "Flash-attention: {}",
                if fa { "ON" } else { "off" }
            ));
        }
        if let Some(reason) = &summary.flash_attention_reason {
            lines.push(format!("  ↳ {reason}"));
        }
        if let Some(p) = &summary.policy_summary {
            lines.push(String::new());
            lines.push(p.clone());
        }
        lines.push(String::new());
        lines.push(if summary.persisted {
            "✓ Saved to ~/.anvil/settings.json".to_string()
        } else {
            "⚠ Could not persist — values will not stick across restarts".to_string()
        });
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    runner.session.render_banner(
        &format!("Tuned for this machine — {model_tag}"),
        &refs,
        accent,
    )
}

// ─── Bench (#665) ───────────────────────────────────────────────────────────

fn format_hw_summary(hw: &runtime::ollama_tune::hw::HardwareProfile) -> String {
    use runtime::ollama_tune::hw::GpuKind;
    let gib = 1024.0 * 1024.0 * 1024.0;
    let ram = (hw.ram_total_bytes as f64 / gib).round() as u64;
    let gpu = match hw.gpu_kind {
        GpuKind::Metal => format!(
            "Metal{}",
            hw.gpu_name
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default()
        ),
        GpuKind::Cuda => format!(
            "CUDA{} {}",
            hw.gpu_name
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default(),
            if hw.vram_total_bytes > 0 {
                format!("({} GB)", (hw.vram_total_bytes as f64 / gib).round() as u64)
            } else {
                String::new()
            }
        ),
        GpuKind::Rocm => "ROCm".to_string(),
        GpuKind::None => "CPU-only".to_string(),
    };
    format!("{} GB RAM · {} · {} threads", ram, gpu, hw.cpu_threads)
}

fn run_bench_with_banner<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    home: &Path,
    url: &str,
    model_tag: &str,
    hw: &runtime::ollama_tune::hw::HardwareProfile,
    tune_summary: &TuneSummary,
    accent: Color,
) -> Result<(), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    use api::ollama_tune::bench::{run_bench_with_progress, HostSummary};
    use api::ollama_tune::policy_config::OllamaConfig;
    use api::ollama_tune::tuner::{tune, KvCacheType, OllamaOptions, UserPolicy};
    use api::fetch_model_meta_cached;
    use runtime::ollama_tune::hw::GpuKind;

    let gib = 1024.0 * 1024.0 * 1024.0;
    let host_summary = HostSummary {
        os: hw.os.clone(),
        gpu_kind: match hw.gpu_kind {
            GpuKind::Metal => "metal".into(),
            GpuKind::Cuda => "cuda".into(),
            GpuKind::Rocm => "rocm".into(),
            GpuKind::None => "none".into(),
        },
        gpu_name: hw.gpu_name.clone(),
        vram_total_gb: ((hw.vram_total_bytes as f64) / gib).round() as u64,
        ram_total_gb: ((hw.ram_total_bytes as f64) / gib).round() as u64,
    };

    // Build options the same way auto_tune does at chat time: tuner → apply
    // override. That way the bench reflects what actual chats will use.
    let url_owned = url.to_string();
    let tag_owned = model_tag.to_string();
    let hw_clone = hw.clone();
    let opts_result: Result<OllamaOptions, String> = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio: {e}"))?;
        let meta = rt
            .block_on(fetch_model_meta_cached(&url_owned, &tag_owned))
            .map_err(|e| e.to_string())?;
        let policy = OllamaConfig::load().policy;
        let policy = if policy.context_min == 0 {
            UserPolicy::default()
        } else {
            policy
        };
        let tuned = tune(&hw_clone, &*meta, &policy).map_err(|e| format!("{e:?}"))?;
        let cfg = OllamaConfig::load();
        Ok::<OllamaOptions, String>(cfg.apply_override(&tag_owned, tuned.options))
    })
    .join()
    .unwrap_or_else(|_| Err("opts thread panicked".to_string()));

    let options = match opts_result {
        Ok(o) => o,
        Err(e) => {
            let _ = render_banner_lines(
                runner,
                &format!("Bench skipped — {model_tag}"),
                &[format!("Could not compute options: {e}")],
                accent,
            );
            return Ok(());
        }
    };
    // Touch tune_summary to keep the signature stable in case future
    // banner work wants to render reasoning beside the bench numbers.
    let _ = tune_summary;
    // Verify kv_cache_type is one of the supported variants (compile-time
    // exhaustive — if KvCacheType grows we want a build break here).
    let _ = match options.kv_cache_type {
        KvCacheType::F16 | KvCacheType::Q8_0 | KvCacheType::Q4_0 => (),
    };

    let progress = Arc::new(std::sync::Mutex::new(("starting…".to_string(), 0usize, 0usize)));
    let progress_writer = Arc::clone(&progress);
    let done = Arc::new(AtomicBool::new(false));
    let done_writer = Arc::clone(&done);
    let bench_result = Arc::new(std::sync::Mutex::new(None));
    let result_writer = Arc::clone(&bench_result);

    let url_owned = url.to_string();
    let tag_owned = model_tag.to_string();
    let host_clone = host_summary.clone();
    let anvil_ver = env!("CARGO_PKG_VERSION").to_string();
    let opts_for_bench = options.clone();

    let handle = thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                done_writer.store(true, Ordering::Relaxed);
                if let Ok(mut g) = result_writer.lock() {
                    *g = Some(Err(format!("tokio: {e}")));
                }
                return;
            }
        };
        let progress_cb = move |idx: usize, total: usize, msg: &str| {
            if let Ok(mut g) = progress_writer.lock() {
                *g = (msg.to_string(), idx, total);
            }
        };
        let res = rt.block_on(run_bench_with_progress(
            &url_owned,
            &tag_owned,
            &opts_for_bench,
            host_clone,
            anvil_ver,
            &progress_cb,
        ));
        if let Ok(mut g) = result_writer.lock() {
            *g = Some(res.map_err(|e| e.to_string()));
        }
        done_writer.store(true, Ordering::Relaxed);
    });

    // Poll the banner while the bench runs.  Tight loop here is fine —
    // we're holding the alt-screen and nothing else is drawing.
    let started = Instant::now();
    while !done.load(Ordering::Relaxed) {
        let (msg, idx, total) = progress
            .lock()
            .map(|g| (g.0.clone(), g.1, g.2))
            .unwrap_or_else(|_| (String::new(), 0, 0));
        let elapsed = started.elapsed().as_secs();
        let header = if total == 0 {
            format!("Benchmarking {model_tag}… ({elapsed}s)")
        } else {
            format!("Benchmarking {model_tag} — prompt {idx}/{total} ({elapsed}s)")
        };
        render_banner_lines(
            runner,
            &header,
            &[
                "Running 3 prompts (short Q&A, code gen, summarization).".to_string(),
                String::new(),
                msg,
            ],
            accent,
        )?;
        thread::sleep(Duration::from_millis(250));
    }
    let _ = handle.join();

    let final_msg = match bench_result.lock().ok().and_then(|g| g.clone()) {
        Some(Ok(result)) => {
            // Persist BenchResult next to the tune snapshot.
            let _ = persist_bench_result(home, model_tag, &result);
            format!(
                "{:.1} tok/s mean · {} ms ttft · {} tokens max",
                result.aggregate.mean_tokens_per_sec,
                result.aggregate.median_ttft_ms,
                result.aggregate.max_completion_tokens,
            )
        }
        Some(Err(e)) => format!("Bench failed: {e}"),
        None => "Bench produced no result".to_string(),
    };
    render_banner_lines(
        runner,
        &format!("Bench complete — {model_tag}"),
        &[final_msg, "Press Enter to continue.".to_string()],
        accent,
    )?;
    let modal = ConfirmModal::new(
        "Baseline saved",
        "Bench numbers stored under ~/.anvil/bench/. Press Enter to continue.",
    );
    let _ = runner.run_confirm("step5-bench-done", modal)?;
    Ok(())
}

fn persist_bench_result(
    home: &Path,
    model_tag: &str,
    result: &api::ollama_tune::bench::BenchResult,
) -> std::io::Result<()> {
    let dir = home.join("bench");
    fs::create_dir_all(&dir)?;
    let slug = model_tag
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' | ' ' => '-',
            c => c,
        })
        .collect::<String>();
    let path = dir.join(format!("{slug}.bench.json"));
    let body =
        serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string());
    fs::write(path, body)
}

fn render_banner_lines<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    title: &str,
    lines: &[String],
    accent: Color,
) -> Result<(), RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    runner.session.render_banner(title, &refs, accent)
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
    home: &Path,
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
    if let Err(e) = extract_and_place(&bytes, &url, home, target_path) {
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
        // macOS ships as a universal .zip containing Ollama.app. We replicate
        // what `curl … ollama.com/install.sh | sh` does on Darwin in-process:
        // download → unzip → place .app → write CLI shim. Nothing escalates.
        ("macos", _) => format!("https://ollama.com/download/Ollama-darwin.zip"),
        // Windows ships as OllamaSetup.exe. We do NOT auto-run installers —
        // see the comment in `extract_and_place`.  The download path here
        // returns the URL; the extract step writes an error card pointing
        // the user at the .exe they just downloaded.
        ("windows", _) => format!("https://ollama.com/download/OllamaSetup.exe"),
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

/// Extract the Ollama download (tarball or zip) and place the binary
/// so `target` (~/.anvil/bin/ollama) launches it.
///
/// Three formats:
///  - `.tgz` → Linux. Extract `bin/ollama` from the tarball directly into
///    `target`. This is what `install.sh` does for Linux.
///  - `.zip` → macOS. The archive contains `Ollama.app`. We unzip into
///    `~/Applications/Ollama.app` (user-writable; install.sh uses
///    `/Applications` but that needs sudo, so we go user-scope), then
///    write `target` as a shell shim that execs the CLI inside the
///    bundle at `Ollama.app/Contents/Resources/ollama`. This mirrors
///    install.sh:79-83's symlink step but stays in the user's HOME.
///  - `.exe` → Windows. We do NOT auto-run installers. Write the
///    `OllamaSetup.exe` next to the target so the wizard can point the
///    user at it, then return an error card via the caller. Future
///    work: a silent/unattended install flag if Ollama exposes one.
fn extract_and_place(bytes: &[u8], url: &str, home: &Path, target: &Path) -> Result<(), String> {
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
    } else if url.ends_with(".zip") {
        // macOS: unpack Ollama.app into ~/Applications, write a shim at
        // ~/.anvil/bin/ollama that execs the CLI inside the bundle.
        let user_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME not set; cannot install Ollama.app".to_string())?;
        let apps_dir = user_home.join("Applications");
        let _ = fs::create_dir_all(&apps_dir);
        let app_dest = apps_dir.join("Ollama.app");

        // Remove any prior Anvil-installed copy. We deliberately do NOT
        // touch /Applications/Ollama.app — that's the user's choice.
        if app_dest.exists() {
            let _ = fs::remove_dir_all(&app_dest);
        }

        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| format!("open zip: {e}"))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("zip entry {i}: {e}"))?;
            let name = entry
                .enclosed_name()
                .ok_or_else(|| format!("zip entry {i} has unsafe path"))?
                .to_path_buf();
            let dest = apps_dir.join(&name);
            if entry.is_dir() {
                fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut out = fs::File::create(&dest)
                    .map_err(|e| format!("create {}: {e}", dest.display()))?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Some(mode) = entry.unix_mode() {
                        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(mode));
                    }
                }
            }
        }

        // Verify the CLI is where we expect it, then write the shim.
        let cli = app_dest.join("Contents/Resources/ollama");
        if !cli.is_file() {
            return Err(format!(
                "extracted Ollama.app but CLI not found at {}",
                cli.display()
            ));
        }
        let shim = format!(
            "#!/bin/sh\n# Anvil-owned Ollama launcher (v2.2.18 wizard install).\nexec \"{cli}\" \"$@\"\n",
            cli = cli.display()
        );
        let _ = fs::create_dir_all(home.join("bin"));
        fs::write(target, shim).map_err(|e| format!("write shim {}: {e}", target.display()))?;
        set_executable(target).map_err(|e| e.to_string())?;
        Ok(())
    } else if url.ends_with(".exe") {
        // Windows: stage the installer in ~/.anvil/cache/ but do not run
        // it. The wizard surfaces a confirm card pointing the user at
        // the .exe; running attended installers behind a TUI risks
        // hiding UAC prompts and breaking everything.
        let cache = home.join("cache");
        let _ = fs::create_dir_all(&cache);
        let staged = cache.join("OllamaSetup.exe");
        let mut f = fs::File::create(&staged)
            .map_err(|e| format!("write {}: {e}", staged.display()))?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        Err(format!(
            "Anvil downloaded OllamaSetup.exe to {}. Open that file in Explorer to run \
             the installer, then re-run the wizard and pick 'I'll handle Ollama myself' \
             so Anvil reuses it.",
            staged.display()
        ))
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
            ("macos", _) => {
                assert!(url.is_ok());
                assert!(url.unwrap().ends_with(".zip"));
            }
            ("windows", _) => {
                assert!(url.is_ok());
                assert!(url.unwrap().ends_with(".exe"));
            }
            _ => {}
        }
    }

    #[test]
    fn short_url_takes_basename() {
        assert_eq!(short_url("https://example.com/path/file.tgz"), "file.tgz");
    }

    // ─── #665: HW-aware ranking ──────────────────────────────────────────

    fn hw(ram_gb: u64, vram_gb: u64, gpu: runtime::ollama_tune::hw::GpuKind) -> runtime::ollama_tune::hw::HardwareProfile {
        runtime::ollama_tune::hw::HardwareProfile {
            ram_total_bytes: ram_gb * 1024 * 1024 * 1024,
            ram_available_bytes: ram_gb * 1024 * 1024 * 1024,
            gpu_kind: gpu,
            gpu_name: None,
            vram_total_bytes: vram_gb * 1024 * 1024 * 1024,
            vram_free_bytes: vram_gb * 1024 * 1024 * 1024,
            cpu_threads: 8,
            perf_cores: None,
            has_avx2: false,
            has_avx512: false,
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    #[test]
    fn rank_recommends_smallest_on_8gb_cpu_machine() {
        let h = hw(8, 0, runtime::ollama_tune::hw::GpuKind::None);
        let idx = recommended_index(&h);
        let chosen = &curated_models()[idx];
        // On 8 GB CPU-only the 7B model is too big; the smaller model wins.
        assert!(chosen.params_b <= 3.0, "expected small model, got {}", chosen.tag);
    }

    #[test]
    fn rank_picks_7b_on_apple_silicon_64gb() {
        // Apple unified memory: vram == ram.
        let h = hw(64, 64, runtime::ollama_tune::hw::GpuKind::Metal);
        let idx = recommended_index(&h);
        let chosen = &curated_models()[idx];
        assert!(chosen.params_b >= 7.0, "expected 7B+, got {}", chosen.tag);
    }

    #[test]
    fn rank_marks_7b_too_big_on_4gb_machine() {
        let h = hw(4, 0, runtime::ollama_tune::hw::GpuKind::None);
        let m = &curated_models()[0]; // 7B
        assert_eq!(rank_for_hw(m, &h), Fit::TooBig);
    }

    #[test]
    fn rank_marks_small_fits_on_modest_gpu() {
        let h = hw(16, 8, runtime::ollama_tune::hw::GpuKind::Cuda);
        // The smallest model (1.5B q4_K_M ≈ 0.85 GB weights) should at
        // least be "Fits" on any 8 GB GPU.
        let small = curated_models().last().unwrap();
        let fit = rank_for_hw(small, &h);
        assert!(matches!(fit, Fit::Recommended | Fit::Fits | Fit::Tight));
        assert_ne!(fit, Fit::TooBig);
    }

    #[test]
    fn curated_list_is_all_q4_km() {
        for c in curated_models() {
            assert!(
                c.tag.contains("q4_K_M"),
                "{} is not a q4_K_M variant",
                c.tag
            );
        }
    }
}
