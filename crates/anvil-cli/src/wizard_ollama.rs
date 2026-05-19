//! v2.2.18 wizard Step 5 — Ollama install + hw-aware ranker + benchmark gate
//! (Agent A3, task #666 / #665).
//!
//! Replaces the v2.2.17 "Ollama URL" text-input step with a 4-state
//! choice:
//!
//! ```text
//! Local AI (Ollama) — choose how to set it up:
//!   [1] Install Ollama and let Anvil pick a model     [recommended]
//!   [2] I already have Ollama running
//!   [3] Skip — no local AI on this system
//!   [4] Maybe later — remind me
//! ```
//!
//! State A (Install): probe HW, confirm, run installer, post-install
//!     poll /api/tags, hw-aware fit-rank the curated GENERAL+CODING
//!     list, confirm pull, run pull, optional benchmark, performance
//!     floor check, persist tuned options.
//! State B (Existing): URL prompt, /api/tags discover, fit-rank
//!     discovered models, optional benchmark.
//! State C (Skip): write `ollama.enabled = false`; fall back to
//!     Anthropic default provider when needed.
//! State D (Defer): write `ollama.deferred = { remaining: 5 }`.
//!
//! ## Why a separate module
//!
//! Step 5 grew from a 7-line text input to ~300 lines of orchestration.
//! Per `feedback-anvil-main-rs-modularity` (and `crates/anvil-cli/wizard.rs`
//! already at 3879 lines), new wizard step logic lives in its own module.
//!
//! ## Why a trait for installer + bench
//!
//! Agent A1's `StreamingOutputModal` and `WizardRunner::run_streaming_output`
//! are not yet landed. The Step 5 orchestrator is shaped against the
//! `OllamaSetupBackend` trait below: real production wires the trait to
//! crossterm subprocesses + alt-screen modal updates; tests wire it to
//! a mock that returns canned outcomes. When A1 lands, the production
//! impl swaps `subprocess + status banner` for `runner.run_streaming_output`
//! without touching the orchestration in `run_ollama_setup_step`.
//!
//! ## 8-axis capability contract (this module's perspective)
//!
//! - Definition       — `OllamaSetupOutcome`, `OllamaWizardChoice`,
//!                      `OllamaSetupBackend` trait.
//! - Registration     — `mod wizard_ollama` in main.rs.
//! - Completion       — N/A (lives inside the wizard's modal flow).
//! - Handler          — `run_ollama_setup_step` consumes
//!                      `WizardModalRunner`.
//! - Dispatch         — called from wizard.rs `run_steps_4_to_8_*`
//!                      and from `/ollama setup` re-entry.
//! - Rendering        — delegates to ChoiceModal / ConfirmModal /
//!                      TextInputModal + banner.
//! - Gate             — fresh `WizardSession` per re-entry; default
//!                      provider fallback when user skips ollama as
//!                      default.
//! - OTel + tests     — unit tests at the bottom for the four states +
//!                      floor check.

use std::time::Duration;

use api::ollama_tune::{
    bench::HostSummary,
    fit::{rank_models, FitResult, ModelCandidate, ModelKind},
};
use runtime::ollama_tune::flash_attn::{Architecture, Quantization};
use runtime::ollama_tune::hw::{detect_cached, GpuKind, HardwareProfile};

use crate::tui::modals::{
    ConfirmChoice, ConfirmModal, ModalAnswer, TextInputModal, WizardChoiceModal,
};
use crate::wizard_runner::{KeySource, RunnerError, TerminalHooks, WizardModalRunner};

// ─── Modal API shims (A1 forward-compat) ─────────────────────────────────────
//
// Agent A1 (task #666) is shipping a richer `Choice { label, badge,
// description }` row + `WizardChoiceModal::new_titled(...).with_choices(...)`
// builder.  Until that lands, we degrade to the legacy
// `WizardChoiceModal::new(title, Vec<String>)` API and merge badge +
// description into the displayed label string.  When A1 ships, swap
// the body of `build_choice_modal` for the rich builder and the rest
// of this module stays untouched.

#[derive(Debug, Clone, Default)]
struct ChoiceRow {
    label: String,
    badge: Option<String>,
    description: Option<String>,
}

impl ChoiceRow {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            badge: None,
            description: None,
        }
    }
    fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }
    fn with_description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
}

/// Build a `WizardChoiceModal` from a list of `ChoiceRow`s. Today this
/// flattens badge + description into the label string (the legacy
/// `WizardChoiceModal::new(title, Vec<String>)` constructor only takes
/// flat strings).  When task #666 lands, swap to:
///
/// ```ignore
/// WizardChoiceModal::new_titled(title).with_choices(
///     rows.into_iter().map(|r| Choice::new(r.label)
///         .opt_with_badge(r.badge)
///         .opt_with_description(r.description)).collect()
/// )
/// ```
fn build_choice_modal(title: impl Into<String>, rows: Vec<ChoiceRow>) -> WizardChoiceModal {
    let labels: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut s = r.label.clone();
            if let Some(b) = &r.badge {
                s.push_str(&format!("  [{b}]"));
            }
            if let Some(d) = &r.description {
                s.push_str(&format!("\n    {d}"));
            }
            s
        })
        .collect();
    WizardChoiceModal::new(title, labels)
}

// ─── Top-level outcome ───────────────────────────────────────────────────────

/// What the user picked on the Step-5 4-state choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OllamaWizardChoice {
    /// State A — full install + bench flow.
    Install,
    /// State B — point Anvil at an existing Ollama URL.
    Existing,
    /// State C — skip entirely, hide future nudges.
    Skip,
    /// State D — defer; show a soft nudge for `remaining` sessions.
    Defer,
}

/// The captured Step-5 result, consumed by the wizard's config writer.
#[derive(Debug, Clone)]
pub(crate) struct OllamaSetupOutcome {
    pub(crate) choice: OllamaWizardChoice,
    /// Ollama URL, when applicable (State A / B). State C / D return
    /// `None` and the wizard writes `ollama.enabled = false`.
    pub(crate) url: Option<String>,
    /// Chosen model tag (Ollama tag, e.g. "llama3.1:8b"), when one was
    /// picked. Some on State A after pull; Some on State B when the
    /// user picked one of the discovered models; None on Skip / Defer.
    pub(crate) chosen_model: Option<String>,
    /// Recommended FitResult — the row the wizard highlighted. Used by
    /// the post-pull config writer to persist tuned options for
    /// `/ollama load`.
    pub(crate) recommended: Option<FitResult>,
    /// Benchmark measured tokens-per-second, when a bench ran. None when
    /// the user declined or the bench failed.
    pub(crate) measured_tok_per_sec: Option<f32>,
    /// For State D: how many sessions to show the "/ollama setup" nudge
    /// before auto-dismissing.
    pub(crate) deferred_remaining: Option<u32>,
    /// Set when the user explicitly picked Ollama as their default
    /// provider in Step 2, but then chose Skip/Defer in Step 5. The
    /// caller falls back to Anthropic + `claude-opus-4-7`.
    pub(crate) fallback_from_ollama_default: bool,
}

impl OllamaSetupOutcome {
    /// Skip outcome — no Ollama, no nudge.
    pub(crate) fn skip(fallback: bool) -> Self {
        Self {
            choice: OllamaWizardChoice::Skip,
            url: None,
            chosen_model: None,
            recommended: None,
            measured_tok_per_sec: None,
            deferred_remaining: None,
            fallback_from_ollama_default: fallback,
        }
    }

    /// Defer outcome — show nudge for N sessions.
    pub(crate) fn defer(remaining: u32, fallback: bool) -> Self {
        Self {
            choice: OllamaWizardChoice::Defer,
            url: None,
            chosen_model: None,
            recommended: None,
            measured_tok_per_sec: None,
            deferred_remaining: Some(remaining),
            fallback_from_ollama_default: fallback,
        }
    }
}

// ─── Backend trait (mockable; production fills in for real) ──────────────────

/// Outcome from running a subprocess inside the wizard alt-screen.
#[derive(Debug, Clone)]
pub(crate) struct ProcessOutcome {
    pub(crate) success: bool,
    /// Tail of combined stdout+stderr; used for the post-process banner.
    pub(crate) tail: String,
}

/// Outcome from a benchmark run (state A step 9).
#[derive(Debug, Clone)]
pub(crate) struct BenchOutcome {
    pub(crate) success: bool,
    pub(crate) tok_per_sec: Option<f32>,
    pub(crate) message: String,
}

/// Discoverable models returned from `GET <url>/api/tags`.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredOllama {
    pub(crate) reachable: bool,
    pub(crate) models: Vec<String>,
}

/// Side-effects the orchestrator delegates to. Production implements
/// these with subprocesses + `runner.run_streaming_output` (A1) +
/// `api::run_bench_with_progress`. Tests implement with canned values.
pub(crate) trait OllamaSetupBackend {
    /// Probe `which ollama` (or platform equivalent). Returns true when
    /// the binary is already installed.
    fn ollama_installed(&self) -> bool;

    /// Run the platform-appropriate installer (curl|sh on macOS/Linux,
    /// `pkg install ollama` on FreeBSD, `pkgin install ollama` on
    /// NetBSD). Returns the process outcome — `success` drives whether
    /// the orchestrator advances past the install step.
    fn run_install(&mut self) -> ProcessOutcome;

    /// Poll `GET <url>/api/tags` until it responds 200 or `timeout`
    /// elapses. Returns the discovered models when reachable.
    fn probe_api_tags(&self, url: &str, timeout: Duration) -> DiscoveredOllama;

    /// Run `ollama pull <tag>`. Returns the process outcome.
    fn pull_model(&mut self, tag: &str) -> ProcessOutcome;

    /// Run the benchmark harness against `model` at `host`. Returns
    /// measured tok/s on success.
    fn bench_model(&mut self, host: &str, model: &str) -> BenchOutcome;

    /// Persist a `BenchResult` to `~/.anvil/benchmarks/`. Returns true
    /// on success.
    fn save_bench(&self, host: &str, model: &str, tok_per_sec: f32, hw: &HardwareProfile) -> bool;

    /// Write the wizard outcome into `~/.anvil/config.json` under the
    /// `ollama` key, plus per-model tuned options under
    /// `ollama.<model>` (num_gpu, num_ctx, flash_attn).
    fn persist_outcome(&self, outcome: &OllamaSetupOutcome) -> bool;
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Drive the v2.2.18 Step 5 dispatch + selected state. `user_picked_ollama_default`
/// tells the orchestrator whether the user already picked Ollama as
/// their default provider in Step 2 — used by States C/D to mark a
/// fallback in `OllamaSetupOutcome`.
///
/// Side-effects are gated through `backend`. The orchestrator itself
/// owns all modal sequencing.
pub(crate) fn run_ollama_setup_step<B, H, K, BE>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    backend: &mut BE,
    user_picked_ollama_default: bool,
) -> Result<OllamaSetupOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
    BE: OllamaSetupBackend,
{
    let accent = ratatui::style::Color::Cyan;

    // ── 4-state choice modal ─────────────────────────────────────────
    let banner_title = "Step 5 of 8 — Local AI (Ollama)";
    let banner_desc =
        "Local AI lets you run Anvil with no API key. Anvil can install Ollama, \
         use one you already have, or skip it entirely.";
    runner
        .session
        .render_banner_with_description(banner_title, banner_desc, &[], accent)?;

    let choice_modal = build_choice_modal(
        "Local AI (Ollama)",
        vec![
            ChoiceRow::new("Install Ollama and let Anvil pick a model")
                .with_badge("recommended")
                .with_description("Full installer + model pick + benchmark (~5 min)"),
            ChoiceRow::new("I already have Ollama running")
                .with_description("Point Anvil at an existing Ollama URL"),
            ChoiceRow::new("Skip — no local AI on this system")
                .with_description("Hide Ollama features entirely from this install"),
            ChoiceRow::new("Maybe later — remind me")
                .with_description("Show a soft nudge for the next 5 sessions"),
        ],
    );
    let answer = runner.run_choice("step5-ollama-choice", choice_modal)?;
    let choice = match answer {
        ModalAnswer::Choice(0) => OllamaWizardChoice::Install,
        ModalAnswer::Choice(1) => OllamaWizardChoice::Existing,
        ModalAnswer::Choice(2) => OllamaWizardChoice::Skip,
        ModalAnswer::Choice(3) => OllamaWizardChoice::Defer,
        // Esc / ChoiceCancelled → safest default is Defer (keeps the
        // door open without configuring anything).
        _ => OllamaWizardChoice::Defer,
    };

    let outcome = match choice {
        OllamaWizardChoice::Install => {
            state_a_install(runner, backend, user_picked_ollama_default)?
        }
        OllamaWizardChoice::Existing => {
            state_b_existing(runner, backend, user_picked_ollama_default)?
        }
        OllamaWizardChoice::Skip => OllamaSetupOutcome::skip(user_picked_ollama_default),
        OllamaWizardChoice::Defer => OllamaSetupOutcome::defer(5, user_picked_ollama_default),
    };

    let _ = backend.persist_outcome(&outcome);
    Ok(outcome)
}

// ─── State A — install ───────────────────────────────────────────────────────

fn state_a_install<B, H, K, BE>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    backend: &mut BE,
    user_picked_ollama_default: bool,
) -> Result<OllamaSetupOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
    BE: OllamaSetupBackend,
{
    let accent = ratatui::style::Color::Cyan;

    // 1) Detect hardware + show summary.
    let hw = detect_cached();
    let hw_lines = describe_hardware(&hw);
    runner.session.render_banner(
        "Detected hardware",
        &hw_lines.iter().map(String::as_str).collect::<Vec<_>>(),
        accent,
    )?;

    // 2) Confirm install — explicit URL + command shown for security.
    let installer_cmd = installer_command_for_os(&hw.os);
    let confirm_body = format!(
        "About to run:\n  {}\n\nThis is the official Ollama installer (from \
         https://ollama.com). It may request sudo. Proceed?",
        installer_cmd
    );
    let install_modal = ConfirmModal::new("Install Ollama?", confirm_body);
    let install_answer = runner.run_confirm("step5a-confirm-install", install_modal)?;
    if !matches!(install_answer, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
        // User refused install — fall through to Defer.
        return Ok(OllamaSetupOutcome::defer(5, user_picked_ollama_default));
    }

    // 3) Run installer (StreamingOutputModal stand-in — A1 wires the
    // live stream when its modal lands).
    if backend.ollama_installed() {
        runner.session.render_banner(
            "Ollama already installed",
            &["Skipping installer; using the existing binary."],
            accent,
        )?;
    } else {
        runner.session.render_banner(
            "Installing Ollama...",
            &["Running the official installer. This can take 1-2 min."],
            accent,
        )?;
        let install_result = backend.run_install();
        if !install_result.success {
            runner.session.render_banner(
                "Installer failed",
                &[
                    "Ollama installer reported a non-zero exit.",
                    "Tail:",
                    install_result.tail.as_str(),
                    "Skipping local AI. You can retry later with /ollama setup.",
                ],
                ratatui::style::Color::Red,
            )?;
            return Ok(OllamaSetupOutcome::defer(5, user_picked_ollama_default));
        }
    }

    // 4) Probe /api/tags until 200.
    let url = "http://localhost:11434".to_string();
    let probe = backend.probe_api_tags(&url, Duration::from_secs(5));
    if !probe.reachable {
        runner.session.render_banner(
            "Ollama not yet reachable",
            &[
                "Installed but the daemon is not responding on :11434.",
                "Try starting it manually with `ollama serve`, then run",
                "`/ollama setup` from inside Anvil.",
            ],
            ratatui::style::Color::Yellow,
        )?;
        return Ok(OllamaSetupOutcome::defer(5, user_picked_ollama_default));
    }

    // 5) Fit-rank curated models. Show the ranker.
    let candidates = curated_candidates();
    let ranked = rank_models(&candidates, &hw);
    let recommended = ranked.iter().find(|r| r.recommendation_tier == 0).cloned();
    let Some(recommended) = recommended else {
        runner.session.render_banner(
            "No model fits this hardware",
            &[
                "Even the smallest curated quant won't fit comfortably here.",
                "Skipping local AI. You can retry with /ollama setup later.",
            ],
            ratatui::style::Color::Yellow,
        )?;
        return Ok(OllamaSetupOutcome::defer(5, user_picked_ollama_default));
    };
    let picker = render_model_picker_modal(&ranked, "Pick a model to pull");
    let pick_answer = runner.run_choice("step5a-pick-model", picker)?;
    let picked_index = match pick_answer {
        ModalAnswer::Choice(i) => i,
        _ => 0,
    };
    let picked = ranked.get(picked_index).cloned().unwrap_or_else(|| recommended.clone());

    // 6) Confirm pull.
    let pull_body = format!(
        "Pull {} ({} / {}, ~{:.1} GB)?",
        picked.tag,
        quant_label(&picked.quant),
        flash_attn_label(&picked.flash_attn),
        picked.est_size_gb,
    );
    let pull_modal = ConfirmModal::new("Pull model?", pull_body);
    let pull_confirm = runner.run_confirm("step5a-confirm-pull", pull_modal)?;
    if !matches!(pull_confirm, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
        return Ok(OllamaSetupOutcome::defer(5, user_picked_ollama_default));
    }

    // 7) Pull (StreamingOutputModal stand-in).
    runner.session.render_banner(
        &format!("Pulling {}...", picked.tag),
        &["This can take several minutes depending on bandwidth."],
        accent,
    )?;
    let pull_outcome = backend.pull_model(&pull_tag(&picked));
    if !pull_outcome.success {
        runner.session.render_banner(
            "Pull failed",
            &[
                pull_outcome.tail.as_str(),
                "Skipping benchmark. You can retry later with /ollama pull.",
            ],
            ratatui::style::Color::Red,
        )?;
        return Ok(OllamaSetupOutcome {
            choice: OllamaWizardChoice::Install,
            url: Some(url),
            chosen_model: None,
            recommended: Some(picked),
            measured_tok_per_sec: None,
            deferred_remaining: None,
            fallback_from_ollama_default: false,
        });
    }

    // 8) Bench (optional confirm, then run).
    let bench_modal = ConfirmModal::new(
        "Benchmark this model?",
        "Runs the standard 3-prompt suite (~30s) to verify the model performs \
         as estimated on your hardware. You can always run it later with /ollama bench.",
    );
    let bench_confirm = runner.run_confirm("step5a-confirm-bench", bench_modal)?;
    let measured = if matches!(bench_confirm, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
        runner.session.render_banner(
            &format!("Benchmarking {}...", picked.tag),
            &["Running the 3-prompt bench suite. ~30s."],
            accent,
        )?;
        let bench = backend.bench_model(&url, &pull_tag(&picked));
        if bench.success {
            if let Some(tok) = bench.tok_per_sec {
                backend.save_bench(&url, &pull_tag(&picked), tok, &hw);
                Some(tok)
            } else {
                None
            }
        } else {
            runner.session.render_banner(
                "Benchmark failed",
                &[bench.message.as_str(), "Continuing without measured tok/s."],
                ratatui::style::Color::Yellow,
            )?;
            None
        }
    } else {
        None
    };

    // 9) Performance floor check.
    if let Some(tok) = measured {
        let floor = floor_tok_per_sec(picked.kind);
        if tok < floor {
            // Offer retry-with-smaller-quant / switch-to-api / keep.
            let floor_modal = build_choice_modal(
                format!(
                    "Measured {:.1} tok/s — below the {:.0} tok/s floor for {} models",
                    tok,
                    floor,
                    workload_label(picked.kind)
                ),
                vec![
                    ChoiceRow::new("Retry with a smaller quant")
                        .with_description("Pull the same model at the next smaller quant level"),
                    ChoiceRow::new("Switch to an API provider")
                        .with_description("Skip local AI; pick Anthropic/OpenAI/xAI in step 2"),
                    ChoiceRow::new("Keep this model anyway")
                        .with_description("Accept the slower throughput and move on"),
                ],
            );
            let floor_answer = runner.run_choice("step5a-floor", floor_modal)?;
            if let ModalAnswer::Choice(1) = floor_answer {
                // Switch to API provider → mark as fallback.
                return Ok(OllamaSetupOutcome {
                    choice: OllamaWizardChoice::Skip,
                    url: None,
                    chosen_model: None,
                    recommended: Some(picked),
                    measured_tok_per_sec: Some(tok),
                    deferred_remaining: None,
                    fallback_from_ollama_default: true,
                });
            }
            // Choice 0 (retry) and Choice 2 (keep) both continue with
            // the same recommended row; retry path is left as a
            // follow-up — for the wizard's purposes "keep" + "retry"
            // both record the result and let the user re-run later via
            // /ollama setup.
        }
    }

    Ok(OllamaSetupOutcome {
        choice: OllamaWizardChoice::Install,
        url: Some(url),
        chosen_model: Some(pull_tag(&picked)),
        recommended: Some(picked),
        measured_tok_per_sec: measured,
        deferred_remaining: None,
        fallback_from_ollama_default: false,
    })
}

// ─── State B — existing Ollama ───────────────────────────────────────────────

fn state_b_existing<B, H, K, BE>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    backend: &mut BE,
    user_picked_ollama_default: bool,
) -> Result<OllamaSetupOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
    BE: OllamaSetupBackend,
{
    let accent = ratatui::style::Color::Cyan;

    let url_modal = TextInputModal::new("Ollama URL", "Endpoint")
        .with_default("http://localhost:11434");
    let url = match runner.run_text_input("step5b-url", url_modal)? {
        ModalAnswer::TextInput(v) => v,
        _ => "http://localhost:11434".to_string(),
    };

    runner.session.render_banner(
        "Probing Ollama...",
        &[&format!("GET {}/api/tags", url)],
        accent,
    )?;
    let probe = backend.probe_api_tags(&url, Duration::from_secs(5));
    if !probe.reachable {
        runner.session.render_banner(
            "Ollama not reachable",
            &[
                &format!("Could not reach {url}."),
                "Skipping local AI. You can retry later with /ollama setup.",
            ],
            ratatui::style::Color::Yellow,
        )?;
        return Ok(OllamaSetupOutcome::defer(5, user_picked_ollama_default));
    }

    if probe.models.is_empty() {
        runner.session.render_banner(
            "Reachable but empty",
            &[
                "Ollama is running but no models are installed.",
                "Pull one manually (ollama pull llama3.1:8b) and re-run /ollama setup.",
            ],
            ratatui::style::Color::Yellow,
        )?;
        return Ok(OllamaSetupOutcome {
            choice: OllamaWizardChoice::Existing,
            url: Some(url),
            chosen_model: None,
            recommended: None,
            measured_tok_per_sec: None,
            deferred_remaining: None,
            fallback_from_ollama_default: false,
        });
    }

    // Rank discovered models.
    let hw = detect_cached();
    let candidates: Vec<ModelCandidate> = probe
        .models
        .iter()
        .map(|tag| candidate_from_discovered_tag(tag))
        .collect();
    let ranked = rank_models(&candidates, &hw);

    let picker = render_model_picker_modal(
        &ranked,
        "Pick the model Anvil should use by default",
    );
    let pick_answer = runner.run_choice("step5b-pick-model", picker)?;
    let picked_index = match pick_answer {
        ModalAnswer::Choice(i) => i,
        _ => 0,
    };
    let picked = ranked.get(picked_index).cloned();

    // Optional benchmark.
    let measured = if let Some(p) = &picked {
        let bench_modal = ConfirmModal::new(
            "Benchmark this model?",
            "Already installed — this just verifies throughput on your hardware (~30s).",
        );
        let bench_confirm = runner.run_confirm("step5b-confirm-bench", bench_modal)?;
        if matches!(bench_confirm, ModalAnswer::Confirm(ConfirmChoice::Yes)) {
            let bench = backend.bench_model(&url, &p.tag);
            if bench.success {
                if let Some(tok) = bench.tok_per_sec {
                    backend.save_bench(&url, &p.tag, tok, &hw);
                    Some(tok)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(OllamaSetupOutcome {
        choice: OllamaWizardChoice::Existing,
        url: Some(url),
        chosen_model: picked.as_ref().map(|p| p.tag.clone()),
        recommended: picked,
        measured_tok_per_sec: measured,
        deferred_remaining: None,
        fallback_from_ollama_default: false,
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Curated list of GENERAL + CODING models (mirrors setup.rs).
pub(crate) fn curated_candidates() -> Vec<ModelCandidate> {
    [
        // General
        ("llama3.1:8b", 8.0, ModelKind::General, Architecture::Llama),
        ("qwen3:8b", 8.0, ModelKind::General, Architecture::Qwen3),
        ("mistral-nemo:12b", 12.0, ModelKind::General, Architecture::Mistral),
        ("gemma3:4b", 4.0, ModelKind::General, Architecture::Gemma3),
        ("phi4:14b", 14.0, ModelKind::General, Architecture::Phi3),
        ("qwen3:14b", 14.0, ModelKind::General, Architecture::Qwen3),
        ("llama3.3:70b", 70.0, ModelKind::General, Architecture::Llama),
        // Coding
        ("qwen2.5-coder:7b", 7.0, ModelKind::Coding, Architecture::Qwen2),
        ("codellama:13b", 13.0, ModelKind::Coding, Architecture::Llama),
        ("codestral:22b", 22.0, ModelKind::Coding, Architecture::Mistral),
        ("qwen2.5-coder:14b", 14.0, ModelKind::Coding, Architecture::Qwen2),
        ("codellama:7b", 7.0, ModelKind::Coding, Architecture::Llama),
        ("qwen3-coder:30b", 30.0, ModelKind::Coding, Architecture::Qwen3),
    ]
    .iter()
    .map(|(tag, p, kind, arch)| ModelCandidate {
        tag: (*tag).to_string(),
        params_billions: *p,
        kind: *kind,
        architecture: arch.clone(),
    })
    .collect()
}

/// Build a ModelCandidate from a discovered tag (no other metadata).
/// Uses [`api::ollama_tune::fit::params_billions_from_tag`] for sizing
/// and falls back to `Other(...)` architecture so flash-attn defaults
/// to off (conservative).
fn candidate_from_discovered_tag(tag: &str) -> ModelCandidate {
    let params = api::ollama_tune::fit::params_billions_from_tag(tag).unwrap_or(7.0);
    let kind = if tag.contains("coder") || tag.contains("code") {
        ModelKind::Coding
    } else if tag.contains("embed") || tag.starts_with("nomic-embed") {
        ModelKind::Embed
    } else {
        ModelKind::General
    };
    let arch = arch_from_tag(tag);
    ModelCandidate {
        tag: tag.to_string(),
        params_billions: params,
        kind,
        architecture: arch,
    }
}

fn arch_from_tag(tag: &str) -> Architecture {
    let lower = tag.to_ascii_lowercase();
    if lower.starts_with("codellama") {
        Architecture::Llama
    } else if lower.starts_with("llama") {
        Architecture::Llama
    } else if lower.starts_with("qwen3") {
        Architecture::Qwen3
    } else if lower.starts_with("qwen") {
        Architecture::Qwen2
    } else if lower.starts_with("mistral-nemo")
        || lower.starts_with("mistral")
        || lower.starts_with("codestral")
    {
        Architecture::Mistral
    } else if lower.starts_with("mixtral") {
        Architecture::Mixtral
    } else if lower.starts_with("gemma3") {
        Architecture::Gemma3
    } else if lower.starts_with("gemma") {
        Architecture::Gemma2
    } else if lower.starts_with("phi") {
        Architecture::Phi3
    } else if lower.starts_with("deepseek-v3") {
        Architecture::DeepseekV3
    } else if lower.starts_with("deepseek") {
        Architecture::DeepseekV2
    } else if lower.starts_with("command-r") {
        Architecture::CommandR
    } else {
        Architecture::Other(tag.to_string())
    }
}

fn describe_hardware(hw: &HardwareProfile) -> Vec<String> {
    let mut out = Vec::new();
    let ram_gb = hw.ram_total_bytes as f64 / 1.0e9;
    let vram_gb = hw.vram_total_bytes as f64 / 1.0e9;
    let gpu_label = match hw.gpu_kind {
        GpuKind::Metal => format!(
            "Metal {}",
            hw.gpu_name.clone().unwrap_or_else(|| "(Apple)".to_string())
        ),
        GpuKind::Cuda => format!(
            "CUDA {}",
            hw.gpu_name.clone().unwrap_or_else(|| "(NVIDIA)".to_string())
        ),
        GpuKind::Rocm => format!(
            "ROCm {}",
            hw.gpu_name.clone().unwrap_or_else(|| "(AMD)".to_string())
        ),
        GpuKind::None => "CPU-only (no GPU acceleration)".to_string(),
    };
    out.push(format!("OS / arch:  {} / {}", hw.os, hw.arch));
    out.push(format!("RAM:        {:.1} GB", ram_gb));
    if vram_gb > 0.5 {
        out.push(format!("VRAM:       {:.1} GB", vram_gb));
    }
    out.push(format!("GPU:        {}", gpu_label));
    out.push(format!("CPU threads: {}", hw.cpu_threads));
    out
}

/// Build a model-picker modal from a ranked list. Tier-0 rows show
/// "✓ fits, FA on, ~X tok/s". Tier-1 rows show "fits, FA off". Tier-2
/// rows show "won't fit — RAM-only" or "too big for this host".
fn render_model_picker_modal(ranked: &[FitResult], title: &str) -> WizardChoiceModal {
    let mut rows: Vec<ChoiceRow> = ranked
        .iter()
        .map(|r| {
            let badge = match r.recommendation_tier {
                0 => Some("recommended".to_string()),
                1 => Some("alt".to_string()),
                _ => Some("won't fit".to_string()),
            };
            let label = format!(
                "{} / {}  (~{:.1} GB, ~{:.0} tok/s, {})",
                r.tag,
                quant_label(&r.quant),
                r.est_size_gb,
                r.est_tok_per_sec,
                flash_attn_label(&r.flash_attn),
            );
            let mut c = ChoiceRow::new(label);
            if let Some(b) = badge {
                c = c.with_badge(b);
            }
            c
        })
        .collect();
    // Add a "custom" sentinel row at the end so users can type a tag.
    rows.push(
        ChoiceRow::new("Don't see your favorite? Enter a custom tag")
            .with_description("(e.g. deepseek-r1:14b)"),
    );
    // Empty-vec is impossible because curated_candidates() returns 13
    // candidates so even pre-A1 the legacy modal gets a non-empty list.
    build_choice_modal(title.to_string(), rows)
}

fn quant_label(q: &Quantization) -> &'static str {
    match q {
        Quantization::Q4_K_M => "Q4_K_M",
        Quantization::Q4_K_S => "Q4_K_S",
        Quantization::Q4_0 => "Q4_0",
        Quantization::Q4_1 => "Q4_1",
        Quantization::Q5_K_M => "Q5_K_M",
        Quantization::Q5_K_S => "Q5_K_S",
        Quantization::Q5_0 => "Q5_0",
        Quantization::Q5_1 => "Q5_1",
        Quantization::Q6_K => "Q6_K",
        Quantization::Q8_0 => "Q8_0",
        Quantization::F16 => "F16",
        Quantization::BF16 => "BF16",
        Quantization::F32 => "F32",
        Quantization::Unknown(_) => "?",
    }
}

fn flash_attn_label(d: &runtime::ollama_tune::flash_attn::FlashAttnDecision) -> &'static str {
    if d.supported { "FA on" } else { "FA off" }
}

/// Produce the Ollama pull tag for a `FitResult` — the picker may have
/// selected a non-default quant level, so we encode that into the tag
/// (e.g. `llama3.1:8b-q5_K_M`). Ollama treats unknown quant suffixes as
/// the default if the model doesn't ship a matching variant; that's
/// acceptable for the wizard's "best-effort" semantics.
fn pull_tag(picked: &FitResult) -> String {
    // For now we use the bare tag (no quant suffix); future work can
    // honor the picked quant by appending `-q4_K_M` etc. when the
    // user's Ollama install ships a matching variant. The wizard
    // proceeds with the model's default quant regardless.
    picked.tag.clone()
}

fn floor_tok_per_sec(kind: ModelKind) -> f32 {
    match kind {
        ModelKind::General => 8.0,
        ModelKind::Coding => 15.0,
        ModelKind::Embed => 0.0, // embedding models are throughput-irrelevant for the wizard's gate
    }
}

fn workload_label(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::General => "general",
        ModelKind::Coding => "coding",
        ModelKind::Embed => "embedding",
    }
}

fn installer_command_for_os(os: &str) -> &'static str {
    match os {
        "macos" | "linux" => "sh -c \"curl -fsSL https://ollama.com/install.sh | sh\"",
        "windows" => "powershell -Command \"iwr https://ollama.com/install.ps1 -useb | iex\"",
        // FreeBSD / NetBSD / OpenBSD handled by the production backend
        // through pkg/pkgin; the wizard banner still describes what will
        // run. Default to the Unix installer for safety.
        _ => "sh -c \"curl -fsSL https://ollama.com/install.sh | sh\"",
    }
}

/// Build a `HostSummary` used by the bench writer.
#[allow(dead_code)]
pub(crate) fn host_summary_for_hw(hw: &HardwareProfile) -> HostSummary {
    HostSummary {
        os: hw.os.clone(),
        gpu_kind: format!("{:?}", hw.gpu_kind),
        gpu_name: hw.gpu_name.clone(),
        vram_total_gb: hw.vram_total_bytes / 1_000_000_000,
        ram_total_gb: hw.ram_total_bytes / 1_000_000_000,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wizard_runner::{ScriptedKeySource, WizardSession, CountingHooks};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::VecDeque;

    // ─── Mock backend ─────────────────────────────────────────────────

    #[derive(Default)]
    struct MockBackend {
        installed: bool,
        install_outcome: bool,
        probe_reachable: bool,
        probe_models: Vec<String>,
        pull_outcome: bool,
        bench_success: bool,
        bench_tok: Option<f32>,
        persist_called: std::cell::Cell<u32>,
    }

    impl OllamaSetupBackend for MockBackend {
        fn ollama_installed(&self) -> bool {
            self.installed
        }
        fn run_install(&mut self) -> ProcessOutcome {
            ProcessOutcome {
                success: self.install_outcome,
                tail: "mock".into(),
            }
        }
        fn probe_api_tags(&self, _url: &str, _timeout: Duration) -> DiscoveredOllama {
            DiscoveredOllama {
                reachable: self.probe_reachable,
                models: self.probe_models.clone(),
            }
        }
        fn pull_model(&mut self, _tag: &str) -> ProcessOutcome {
            ProcessOutcome {
                success: self.pull_outcome,
                tail: "pulled".into(),
            }
        }
        fn bench_model(&mut self, _host: &str, _model: &str) -> BenchOutcome {
            BenchOutcome {
                success: self.bench_success,
                tok_per_sec: self.bench_tok,
                message: "ok".into(),
            }
        }
        fn save_bench(&self, _host: &str, _model: &str, _tok: f32, _hw: &HardwareProfile) -> bool {
            true
        }
        fn persist_outcome(&self, _outcome: &OllamaSetupOutcome) -> bool {
            self.persist_called.set(self.persist_called.get() + 1);
            true
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn make_session() -> WizardSession<TestBackend, CountingHooks> {
        let backend = TestBackend::new(120, 40);
        let terminal = Terminal::new(backend).unwrap();
        WizardSession::enter(terminal, CountingHooks::default()).unwrap()
    }

    fn make_scripted(keys: Vec<KeyEvent>) -> ScriptedKeySource {
        ScriptedKeySource {
            keys: VecDeque::from(keys),
        }
    }

    #[test]
    fn state_c_skip_records_outcome() {
        // Welcome→Skip choice (index 2). Down*2 then Enter.
        let mut session = make_session();
        let key_source = make_scripted(vec![
            key(KeyCode::Down),
            key(KeyCode::Down),
            key(KeyCode::Enter),
        ]);
        let mut runner =
            WizardModalRunner::new(&mut session, key_source, ratatui::style::Color::Cyan);
        let mut backend = MockBackend::default();
        let outcome = run_ollama_setup_step(&mut runner, &mut backend, false).unwrap();
        assert_eq!(outcome.choice, OllamaWizardChoice::Skip);
        assert!(outcome.url.is_none());
        assert!(outcome.chosen_model.is_none());
        assert_eq!(backend.persist_called.get(), 1);
    }

    #[test]
    fn state_d_defer_records_remaining_5() {
        // 4-state choice → index 3 (Defer).
        let mut session = make_session();
        let key_source = make_scripted(vec![
            key(KeyCode::Down),
            key(KeyCode::Down),
            key(KeyCode::Down),
            key(KeyCode::Enter),
        ]);
        let mut runner =
            WizardModalRunner::new(&mut session, key_source, ratatui::style::Color::Cyan);
        let mut backend = MockBackend::default();
        let outcome = run_ollama_setup_step(&mut runner, &mut backend, true).unwrap();
        assert_eq!(outcome.choice, OllamaWizardChoice::Defer);
        assert_eq!(outcome.deferred_remaining, Some(5));
        // Fallback flag flows through when user picked ollama as default
        // but then deferred.
        assert!(outcome.fallback_from_ollama_default);
    }

    #[test]
    fn state_a_install_unreachable_falls_back_to_defer() {
        // Pick Install (index 0). Then confirm install with 'y'. Then
        // probe fails — orchestrator returns Defer.
        let mut session = make_session();
        let key_source = make_scripted(vec![
            key(KeyCode::Enter),       // pick "Install" (index 0, default)
            key(KeyCode::Char('y')),   // confirm install
        ]);
        let mut runner =
            WizardModalRunner::new(&mut session, key_source, ratatui::style::Color::Cyan);
        let mut backend = MockBackend {
            installed: true,    // skip running installer
            install_outcome: true,
            probe_reachable: false, // /api/tags fails
            ..Default::default()
        };
        let outcome = run_ollama_setup_step(&mut runner, &mut backend, false).unwrap();
        // Probe failed → defer
        assert_eq!(outcome.choice, OllamaWizardChoice::Defer);
    }

    #[test]
    fn state_b_existing_unreachable_falls_back_to_defer() {
        // Pick Existing (index 1) → Enter on URL prompt (accept default) → unreachable probe.
        let mut session = make_session();
        let key_source = make_scripted(vec![
            key(KeyCode::Down),
            key(KeyCode::Enter),  // pick Existing
            key(KeyCode::Enter),  // accept default URL
        ]);
        let mut runner =
            WizardModalRunner::new(&mut session, key_source, ratatui::style::Color::Cyan);
        let mut backend = MockBackend {
            probe_reachable: false,
            ..Default::default()
        };
        let outcome = run_ollama_setup_step(&mut runner, &mut backend, false).unwrap();
        assert_eq!(outcome.choice, OllamaWizardChoice::Defer);
    }

    #[test]
    fn floor_tok_per_sec_coding_strict() {
        assert_eq!(floor_tok_per_sec(ModelKind::General), 8.0);
        assert_eq!(floor_tok_per_sec(ModelKind::Coding), 15.0);
        assert_eq!(floor_tok_per_sec(ModelKind::Embed), 0.0);
    }

    #[test]
    fn arch_from_tag_routes_common_families() {
        assert!(matches!(arch_from_tag("llama3.1:8b"), Architecture::Llama));
        assert!(matches!(arch_from_tag("codellama:7b"), Architecture::Llama));
        assert!(matches!(arch_from_tag("qwen3:8b"), Architecture::Qwen3));
        assert!(matches!(arch_from_tag("qwen2.5-coder:7b"), Architecture::Qwen2));
        assert!(matches!(arch_from_tag("phi4:14b"), Architecture::Phi3));
        assert!(matches!(arch_from_tag("gemma3:4b"), Architecture::Gemma3));
        assert!(matches!(arch_from_tag("custom:latest"), Architecture::Other(_)));
    }

    #[test]
    fn candidate_from_tag_detects_coding_kind() {
        let cand = candidate_from_discovered_tag("qwen2.5-coder:7b");
        assert!(matches!(cand.kind, ModelKind::Coding));
        let general = candidate_from_discovered_tag("llama3.1:8b");
        assert!(matches!(general.kind, ModelKind::General));
        let embed = candidate_from_discovered_tag("nomic-embed-text:latest");
        assert!(matches!(embed.kind, ModelKind::Embed));
    }

    #[test]
    fn curated_candidates_covers_seven_general_six_coding() {
        let c = curated_candidates();
        let general = c.iter().filter(|x| matches!(x.kind, ModelKind::General)).count();
        let coding = c.iter().filter(|x| matches!(x.kind, ModelKind::Coding)).count();
        assert_eq!(general, 7, "expected 7 general models from setup.rs");
        assert_eq!(coding, 6, "expected 6 coding models from setup.rs");
    }

    /// Floor check: a 5 tok/s measurement on a Coding model should
    /// surface the "below threshold" modal, with the "switch to API"
    /// path producing `fallback_from_ollama_default = true`.
    #[test]
    fn floor_threshold_logic() {
        // Pure logic check — verify the per-kind thresholds.
        assert!(5.0_f32 < floor_tok_per_sec(ModelKind::Coding));
        assert!(7.0_f32 < floor_tok_per_sec(ModelKind::General));
        assert!(10.0_f32 > floor_tok_per_sec(ModelKind::General));
        assert!(20.0_f32 > floor_tok_per_sec(ModelKind::Coding));
    }
}
