//! CLI-side wiring for `/ollama bench` (Task #369, "L7" follow-up).
//!
//! The api-side bench harness (`api::ollama_tune::bench`) owns the actual
//! `/api/chat` round-trip and produces a `BenchResult`. This module:
//!
//!   * Provides a sync entry point `cmd_bench_blocking` that callers in
//!     `main.rs` use under all three dispatch sites (resume, headless,
//!     TUI) — same pattern as `ollama_pull`/`ollama_requantize`.
//!   * Persists each `BenchResult` as JSON to `~/.anvil/benchmarks/`,
//!     atomic-write + 50-record rotation per model.
//!   * Exposes `latest_bench(model)` so `/ollama tune` can surface a
//!     "Last bench: 47.2 tok/s on YYYY-MM-DD" recall line; if the recorded
//!     `OllamaOptions` differ from the currently-tuned ones, the line is
//!     suffixed with "config differs from current".
//!
//! Local JSON is canonical; QMD ingest is intentionally out of scope here
//! (see TODO at end of file) until the QMD client surface stabilises.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use api::ollama_tune::bench::{
    format_bench_summary, model_slug, run_bench, BenchError, BenchResult, HostSummary,
};
use api::ollama_tune::tuner::OllamaOptions;
use api::ollama_tune::OllamaConfig;
use runtime::ollama_tune::hw::{detect_cached, GpuKind, HardwareProfile};

/// How many bench records to retain per model. Older ones are deleted on
/// the next successful save.
pub const MAX_BENCH_RECORDS_PER_MODEL: usize = 50;

const ANVIL_VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── Sync entry point (matches ollama_manage / ollama_requantize style) ──────

/// `/ollama bench <model>` — sync entry point used by all three dispatch
/// sites in `main.rs` (resume, headless REPL, TUI).
///
/// Pre-flight: empty model arg → usage; otherwise async-bridge into the
/// api-crate runner, persist on success, format a human-readable summary.
#[must_use]
pub fn cmd_bench_blocking(host: &str, rest: &str) -> String {
    let model = rest.trim();
    if model.is_empty() {
        return "Usage: /ollama bench <model>".to_string();
    }

    let hw = detect_cached();
    let host_summary = host_summary_from_hw(&hw);
    let config = OllamaConfig::load();
    let options = effective_options_for(model, &config, &hw);

    let result = match block_on(run_bench(
        host,
        model,
        &options,
        host_summary,
        ANVIL_VERSION.to_string(),
    )) {
        Ok(r) => r,
        Err(e) => return format_bench_error(&e),
    };

    let mut output = format_bench_summary(&result);
    match save_bench(&result) {
        Ok(path) => {
            output.push_str(&format!("\nSaved: {}\n", path.display()));
        }
        Err(e) => {
            output.push_str(&format!("\nSave failed (bench data still printed above): {e}\n"));
        }
    }
    output
}

fn block_on<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(fut),
    }
}

fn format_bench_error(e: &BenchError) -> String {
    match e {
        BenchError::DaemonUnreachable(s) => format!("Ollama daemon unreachable: {s}"),
        BenchError::ModelNotInstalled(m) => {
            format!("Model not installed: {m}\nUse /ollama pull {m} first.")
        }
        BenchError::StreamFailed(s) => format!("Bench stream failed: {s}"),
    }
}

// ─── Hardware → HostSummary ──────────────────────────────────────────────────

#[must_use]
pub fn host_summary_from_hw(hw: &HardwareProfile) -> HostSummary {
    HostSummary {
        os: hw.os.clone(),
        gpu_kind: gpu_kind_label(hw.gpu_kind).to_string(),
        gpu_name: hw.gpu_name.clone(),
        vram_total_gb: hw.vram_total_bytes / 1_073_741_824,
        ram_total_gb: hw.ram_total_bytes / 1_073_741_824,
    }
}

fn gpu_kind_label(k: GpuKind) -> &'static str {
    match k {
        GpuKind::Metal => "metal",
        GpuKind::Cuda => "cuda",
        GpuKind::Rocm => "rocm",
        GpuKind::None => "cpu",
    }
}

// ─── Effective options resolution ────────────────────────────────────────────

fn effective_options_for(model: &str, config: &OllamaConfig, hw: &HardwareProfile) -> OllamaOptions {
    use api::fetch_model_meta_cached;
    use api::ollama_tune::tuner::tune;
    use api::read_ollama_base_url;

    let host = read_ollama_base_url();
    // Best-effort: fetch meta to drive the tuner. On failure (daemon down,
    // model not installed) fall back to a sensible default. The downstream
    // `run_bench` call will surface the "model not installed" error in
    // that case anyway.
    let meta = block_on(async { fetch_model_meta_cached(&host, model).await }).ok();
    if let Some(meta) = meta {
        if let Ok(result) = tune(hw, &meta, &config.policy) {
            return config.apply_override(model, result.options);
        }
    }
    // Conservative default — matches what /ollama bench would have used
    // before the tuner could speak.
    let fallback = OllamaOptions {
        num_gpu: -1,
        num_ctx: 8192,
        num_thread: hw.cpu_threads.saturating_sub(1).max(1),
        flash_attention: false,
        kv_cache_type: api::ollama_tune::tuner::KvCacheType::F16,
        low_vram: false,
        main_gpu: 0,
        keep_alive_secs: 300,
        mmap: !cfg!(target_os = "windows"),
        num_batch: 512,
    };
    config.apply_override(model, fallback)
}

// ─── Persistence ─────────────────────────────────────────────────────────────

/// Resolve the bench-records directory: `$ANVIL_HOME/benchmarks` if set,
/// else `$HOME/.anvil/benchmarks`. Created on demand by `save_bench`.
#[must_use]
pub fn benchmarks_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ANVIL_HOME") {
        return PathBuf::from(p).join("benchmarks");
    }
    if let Ok(p) = std::env::var("ANVIL_CONFIG_HOME") {
        return PathBuf::from(p).join("benchmarks");
    }
    if let Some(home) = dirs_next::home_dir() {
        return home.join(".anvil").join("benchmarks");
    }
    PathBuf::from(".anvil").join("benchmarks")
}

/// Atomic write of a `BenchResult` to `<benchmarks_dir>/<slug>-<ts>.json`.
/// Creates the parent dir on demand. After a successful write, prunes
/// older records for the same model so at most `MAX_BENCH_RECORDS_PER_MODEL`
/// remain.
pub fn save_bench(result: &BenchResult) -> io::Result<PathBuf> {
    let dir = benchmarks_dir();
    fs::create_dir_all(&dir)?;
    let slug = model_slug(&result.model);
    let final_name = format!("{slug}-{ts}.json", ts = result.timestamp);
    let final_path = dir.join(&final_name);
    let tmp_path = dir.join(format!("{final_name}.tmp"));

    let json = serde_json::to_string_pretty(result)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp_path, json)?;
    fs::rename(&tmp_path, &final_path)?;

    // Prune older records for this model only.
    prune_records_for_slug(&dir, &slug, MAX_BENCH_RECORDS_PER_MODEL).ok();

    Ok(final_path)
}

/// List records for a model, newest first. Bounded by `limit`.
#[must_use]
pub fn load_recent_benches(model: &str, limit: usize) -> Vec<BenchResult> {
    let dir = benchmarks_dir();
    let slug = model_slug(model);
    let mut paths = match read_records_for_slug(&dir, &slug) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    paths.sort_by(|a, b| timestamp_from_path(b).cmp(&timestamp_from_path(a)));
    paths
        .into_iter()
        .take(limit)
        .filter_map(|p| {
            let bytes = fs::read(&p).ok()?;
            serde_json::from_slice(&bytes).ok()
        })
        .collect()
}

/// The most recent `BenchResult` for `model`, or `None` if no record
/// exists or the file can't be parsed.
#[must_use]
pub fn latest_bench(model: &str) -> Option<BenchResult> {
    load_recent_benches(model, 1).into_iter().next()
}

fn read_records_for_slug(dir: &Path, slug: &str) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.ends_with(".json") {
            continue;
        }
        // Match `<slug>-<digits>.json`.
        if let Some(stripped) = name.strip_suffix(".json") {
            if let Some(rest) = stripped.strip_prefix(slug) {
                if let Some(ts_part) = rest.strip_prefix('-') {
                    if !ts_part.is_empty() && ts_part.chars().all(|c| c.is_ascii_digit()) {
                        out.push(path);
                    }
                }
            }
        }
    }
    Ok(out)
}

fn timestamp_from_path(p: &Path) -> i64 {
    p.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit('-').next())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

fn prune_records_for_slug(dir: &Path, slug: &str, keep: usize) -> io::Result<()> {
    let mut paths = read_records_for_slug(dir, slug)?;
    if paths.len() <= keep {
        return Ok(());
    }
    paths.sort_by(|a, b| timestamp_from_path(b).cmp(&timestamp_from_path(a)));
    for old in paths.into_iter().skip(keep) {
        let _ = fs::remove_file(&old);
    }
    Ok(())
}

// ─── Recall: the line we splice into /ollama tune ─────────────────────────────

/// Returns the recall line for `/ollama tune <model>`, or `None` when
/// no prior bench exists. Compares the stored `OllamaOptions` to `current`
/// and appends "(config differs from current)" when they don't match.
#[must_use]
pub fn format_recall_line(model: &str, current: &OllamaOptions) -> Option<String> {
    let last = latest_bench(model)?;
    let age = format_age(last.timestamp);
    let line = if &last.options == current {
        format!(
            "Last bench: {:.1} tok/s on unix-{} ({})",
            last.aggregate.mean_tokens_per_sec, last.timestamp, age
        )
    } else {
        format!(
            "Last bench: {:.1} tok/s on unix-{} ({}) — config differs from current",
            last.aggregate.mean_tokens_per_sec, last.timestamp, age
        )
    };
    Some(line)
}

fn format_age(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let delta = now.saturating_sub(ts);
    match delta {
        d if d < 60 => format!("{d}s ago"),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86_400 => format!("{}h ago", d / 3600),
        d if d < 86_400 * 30 => format!("{}d ago", d / 86_400),
        d => format!("{}mo ago", d / (86_400 * 30)),
    }
}

// `Ordering` is used by `read_records_for_slug` sort comparator; suppress
// dead-code warnings if an optimisation pass elides the explicit use.
#[allow(dead_code)]
fn _ord_marker(a: i64, b: i64) -> Ordering {
    a.cmp(&b)
}

// ─── Background-thread bench with progress + recommend/confirm ───────────────

/// Spawn the bench on a detached thread so the TUI stays responsive.
/// Status lines are pushed via `tui_slot` as each prompt starts/ends, the
/// final summary is pushed when the suite completes, and a recommendation
/// (with a 60-second confirm gate) follows if the bench produced different
/// `OllamaOptions` than the user's currently-active config.
///
/// Returns a one-line "Bench started…" message immediately; all real
/// output flows through `tui_slot` after the thread completes each step.
pub fn cmd_bench_in_background(host: &str, rest: &str, tui_slot: crate::TuiSenderSlot) -> String {
    let model = rest.trim();
    if model.is_empty() {
        return "Usage: /ollama bench <model>".to_string();
    }
    let host_owned = host.to_string();
    let model_owned = model.to_string();

    std::thread::spawn(move || {
        run_bench_thread(host_owned, model_owned, tui_slot);
    });

    format!(
        "Bench started for {model}.\n\
         Running 3 prompts (short Q&A, code-gen, summarization). This typically\n\
         takes 30-90s on local models. Status updates will appear as each prompt\n\
         completes; you can keep using Anvil while it runs."
    )
}

fn run_bench_thread(host: String, model: String, tui_slot: crate::TuiSenderSlot) {
    use api::ollama_tune::bench::run_bench_with_progress;

    let send = |msg: String| {
        if let Ok(guard) = tui_slot.lock() {
            if let Some(tx) = guard.as_ref() {
                tx.send(crate::tui::TuiEvent::System(msg));
            }
        }
    };

    let hw = detect_cached();
    let host_summary = host_summary_from_hw(&hw);
    let config = OllamaConfig::load();
    let options = effective_options_for(&model, &config, &hw);

    send(format!("[bench] {model}: starting prompt suite"));

    // Adapter: capture tui_slot in a closure so each progress event
    // becomes a system push.
    let tui_slot_for_progress = tui_slot.clone();
    let progress = move |idx: usize, total: usize, msg: &str| {
        if let Ok(guard) = tui_slot_for_progress.lock() {
            if let Some(tx) = guard.as_ref() {
                tx.send(crate::tui::TuiEvent::System(format!("[bench {idx}/{total}] {msg}")));
            }
        }
    };

    let host_for_run = host.clone();
    let model_for_run = model.clone();
    let opts_for_run = options.clone();
    let host_summary_for_run = host_summary.clone();

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            send(format!("[bench] failed to start tokio runtime: {e}"));
            return;
        }
    };

    let result = rt.block_on(run_bench_with_progress(
        &host_for_run,
        &model_for_run,
        &opts_for_run,
        host_summary_for_run,
        ANVIL_VERSION.to_string(),
        &progress,
    ));

    match result {
        Ok(bench) => {
            send(format_bench_summary(&bench));
            match save_bench(&bench) {
                Ok(path) => send(format!("[bench] saved: {}", path.display())),
                Err(e) => send(format!("[bench] save failed: {e}")),
            }
            // Build recommendation from the tuner output (which already
            // accounts for hardware + policy + bench history). If the
            // user's currently-active config matches, nothing to recommend.
            let current = options.clone();
            let recommended = effective_options_for(&model, &OllamaConfig::load(), &hw);
            let diffs = options_diff(&current, &recommended);
            if !diffs.is_empty() {
                stash_pending_apply(&model, &recommended);
                send(format_recommendation_prompt(&model, &current, &recommended, &diffs));
            }
        }
        Err(e) => {
            send(format!("[bench] failed: {e}"));
        }
    }
}

// ─── Recommendation diff + apply gate ────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionDiff {
    pub field: &'static str,
    pub current: String,
    pub recommended: String,
}

/// Compare two `OllamaOptions` and return the per-field differences.
#[must_use]
pub fn options_diff(current: &OllamaOptions, recommended: &OllamaOptions) -> Vec<OptionDiff> {
    let mut out = Vec::new();
    if current.num_gpu != recommended.num_gpu {
        out.push(OptionDiff {
            field: "num_gpu",
            current: current.num_gpu.to_string(),
            recommended: recommended.num_gpu.to_string(),
        });
    }
    if current.num_ctx != recommended.num_ctx {
        out.push(OptionDiff {
            field: "num_ctx",
            current: current.num_ctx.to_string(),
            recommended: recommended.num_ctx.to_string(),
        });
    }
    if current.num_thread != recommended.num_thread {
        out.push(OptionDiff {
            field: "num_thread",
            current: current.num_thread.to_string(),
            recommended: recommended.num_thread.to_string(),
        });
    }
    if current.flash_attention != recommended.flash_attention {
        out.push(OptionDiff {
            field: "flash_attention",
            current: current.flash_attention.to_string(),
            recommended: recommended.flash_attention.to_string(),
        });
    }
    if current.kv_cache_type != recommended.kv_cache_type {
        out.push(OptionDiff {
            field: "kv_cache_type",
            current: format!("{:?}", current.kv_cache_type).to_lowercase(),
            recommended: format!("{:?}", recommended.kv_cache_type).to_lowercase(),
        });
    }
    if current.num_batch != recommended.num_batch {
        out.push(OptionDiff {
            field: "num_batch",
            current: current.num_batch.to_string(),
            recommended: recommended.num_batch.to_string(),
        });
    }
    out
}

/// Format the recommendation block we push after a successful bench.
#[must_use]
pub fn format_recommendation_prompt(
    model: &str,
    _current: &OllamaOptions,
    _recommended: &OllamaOptions,
    diffs: &[OptionDiff],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n[bench] Recommended settings for {model} differ from current config:\n"));
    for d in diffs {
        out.push_str(&format!(
            "  {:<16}  {} → {}\n",
            d.field, d.current, d.recommended
        ));
    }
    out.push_str(&format!(
        "\nTo apply these recommendations, send `apply` as your next message (60s window).\n\
         Anything else cancels — your current config stays in place.\n\
         You can also tune individual fields with /ollama option <key> <value>."
    ));
    out
}

// ─── Pending-apply intent (mirrors the rm pending-delete pattern) ────────────

#[derive(Debug, Clone)]
struct PendingApply {
    model: String,
    options: OllamaOptions,
    expires_at: std::time::Instant,
}

const PENDING_APPLY_TTL: std::time::Duration = std::time::Duration::from_secs(60);

fn pending_apply_slot() -> &'static std::sync::Mutex<Option<PendingApply>> {
    use std::sync::OnceLock;
    static SLOT: OnceLock<std::sync::Mutex<Option<PendingApply>>> = OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(None))
}

fn stash_pending_apply(model: &str, options: &OllamaOptions) {
    if let Ok(mut slot) = pending_apply_slot().lock() {
        *slot = Some(PendingApply {
            model: model.to_string(),
            options: options.clone(),
            expires_at: std::time::Instant::now() + PENDING_APPLY_TTL,
        });
    }
}

/// Called by the run-turn dispatcher BEFORE a free-text submission becomes
/// a chat turn. Returns:
///   * `Some(message)` — there was a pending intent and we handled it
///     (either applied or cancelled). Caller pushes the message and
///     short-circuits the turn.
///   * `None` — no pending intent active; let the submission flow as usual.
#[must_use]
pub fn intercept_pending_apply(submission: &str) -> Option<String> {
    let pending = {
        let mut slot = pending_apply_slot().lock().ok()?;
        let p = slot.take()?;
        if std::time::Instant::now() >= p.expires_at {
            return Some(format!(
                "Recommendation expired for {} (60s window passed).",
                p.model
            ));
        }
        p
    };
    let typed = submission.trim();
    match typed.to_ascii_lowercase().as_str() {
        "apply" | "y" | "yes" => match apply_options_to_settings(&pending.model, &pending.options) {
            Ok(()) => {
                api::ollama_tune::invalidate_cache(Some(&pending.model));
                Some(format!(
                    "Applied recommended settings for {}. The next chat request will use them.",
                    pending.model
                ))
            }
            Err(e) => Some(format!("Failed to write settings.json: {e}")),
        },
        "" => Some(format!(
            "Recommendation cancelled (empty input) for {}.",
            pending.model
        )),
        _ => Some(format!(
            "Recommendation declined for {}. Current config unchanged.",
            pending.model
        )),
    }
}

fn apply_options_to_settings(model: &str, opts: &OllamaOptions) -> Result<(), String> {
    let mut config = OllamaConfig::load();
    config.set_override(model, |o| {
        o.num_gpu = Some(opts.num_gpu);
        o.num_ctx = Some(opts.num_ctx);
        o.num_thread = Some(opts.num_thread);
        o.flash_attention = Some(opts.flash_attention);
        o.kv_cache_type = Some(opts.kv_cache_type.clone());
        o.num_batch = Some(opts.num_batch);
    });
    config.save().map_err(|e| e.to_string())
}

// TODO(qmd-ingest): once QMD's runtime client exposes a stable `ingest(collection,
// key, content)` API, drop `format_bench_qmd_doc(&result)` from
// `api::ollama_tune::bench` into the QMD "ollama-perf" collection here. The
// local JSON store remains canonical regardless.

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use api::ollama_tune::bench::{Aggregate, PromptResult};
    use api::ollama_tune::tuner::KvCacheType;
    use serial_test::serial;
    use tempfile::tempdir;

    fn fake_options() -> OllamaOptions {
        OllamaOptions {
            num_gpu: -1,
            num_ctx: 32768,
            num_thread: 11,
            flash_attention: true,
            kv_cache_type: KvCacheType::F16,
            low_vram: false,
            main_gpu: 0,
            keep_alive_secs: 300,
            mmap: true,
            num_batch: 1024,
        }
    }

    fn fake_host() -> HostSummary {
        HostSummary {
            os: "macos".to_string(),
            gpu_kind: "metal".to_string(),
            gpu_name: Some("Apple M2 Pro".to_string()),
            vram_total_gb: 32,
            ram_total_gb: 32,
        }
    }

    fn fake_result(model: &str, ts: i64) -> BenchResult {
        let prompts = vec![PromptResult {
            prompt_label: "short_qa".into(),
            time_to_first_token_ms: 89,
            total_time_ms: 230,
            prompt_tokens: 18,
            completion_tokens: 12,
            tokens_per_sec: 47.2,
        }];
        BenchResult {
            model: model.to_string(),
            timestamp: ts,
            anvil_version: "test".to_string(),
            host_summary: fake_host(),
            options: fake_options(),
            prompts,
            aggregate: Aggregate {
                mean_tokens_per_sec: 47.2,
                median_ttft_ms: 89,
                max_completion_tokens: 12,
            },
        }
    }

    fn with_anvil_home<F: FnOnce()>(f: F) {
        let tmp = tempdir().expect("tempdir");
        let prev = std::env::var("ANVIL_HOME").ok();
        // SAFETY: env vars are process-global; the #[serial] guard on each
        // test prevents concurrent mutation. Edition 2024 marks set_var
        // unsafe but the contract is unchanged.
        unsafe {
            std::env::set_var("ANVIL_HOME", tmp.path());
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(v) => unsafe { std::env::set_var("ANVIL_HOME", v) },
            None => unsafe { std::env::remove_var("ANVIL_HOME") },
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    #[serial]
    fn save_bench_creates_parent_dir_and_writes() {
        with_anvil_home(|| {
            let r = fake_result("qwen3:8b", 1_715_200_000);
            let path = save_bench(&r).expect("save");
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("qwen3-8b-1715200000.json"));
        });
    }

    #[test]
    #[serial]
    fn save_bench_atomic_no_lingering_tmp() {
        with_anvil_home(|| {
            let r = fake_result("qwen3:8b", 1_715_200_000);
            save_bench(&r).expect("save");
            let dir = benchmarks_dir();
            let tmp_files: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().to_string_lossy().ends_with(".tmp"))
                .collect();
            assert!(tmp_files.is_empty(), "lingering .tmp file");
        });
    }

    #[test]
    #[serial]
    fn latest_bench_returns_most_recent() {
        with_anvil_home(|| {
            for ts in &[100_i64, 200, 150] {
                save_bench(&fake_result("qwen3:8b", *ts)).expect("save");
            }
            let latest = latest_bench("qwen3:8b").expect("some");
            assert_eq!(latest.timestamp, 200);
        });
    }

    #[test]
    #[serial]
    fn latest_bench_returns_none_when_no_history() {
        with_anvil_home(|| {
            assert!(latest_bench("qwen3:8b").is_none());
        });
    }

    #[test]
    #[serial]
    fn latest_bench_isolates_by_model_slug() {
        with_anvil_home(|| {
            save_bench(&fake_result("qwen3:8b", 100)).expect("save");
            save_bench(&fake_result("llama3.2:3b", 200)).expect("save");
            assert_eq!(latest_bench("qwen3:8b").unwrap().timestamp, 100);
            assert_eq!(latest_bench("llama3.2:3b").unwrap().timestamp, 200);
        });
    }

    #[test]
    #[serial]
    fn rotation_keeps_newest_50() {
        with_anvil_home(|| {
            for ts in 0..60_i64 {
                save_bench(&fake_result("qwen3:8b", ts)).expect("save");
            }
            let dir = benchmarks_dir();
            let count = fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .to_string_lossy()
                        .ends_with(".json")
                })
                .count();
            assert_eq!(count, MAX_BENCH_RECORDS_PER_MODEL);
            // The earliest surviving timestamp should be 60 - 50 = 10.
            let recent = load_recent_benches("qwen3:8b", 100);
            assert_eq!(recent.len(), 50);
            let oldest = recent.last().unwrap().timestamp;
            assert_eq!(oldest, 10);
        });
    }

    #[test]
    #[serial]
    fn rotation_does_not_touch_other_model_records() {
        with_anvil_home(|| {
            for ts in 0..60_i64 {
                save_bench(&fake_result("qwen3:8b", ts)).expect("save");
            }
            for ts in 0..3_i64 {
                save_bench(&fake_result("llama3.2:3b", ts)).expect("save");
            }
            assert_eq!(load_recent_benches("qwen3:8b", 100).len(), 50);
            assert_eq!(load_recent_benches("llama3.2:3b", 100).len(), 3);
        });
    }

    #[test]
    #[serial]
    fn format_recall_line_returns_none_when_no_bench() {
        with_anvil_home(|| {
            let opts = fake_options();
            assert!(format_recall_line("qwen3:8b", &opts).is_none());
        });
    }

    #[test]
    #[serial]
    fn format_recall_line_when_options_match() {
        with_anvil_home(|| {
            save_bench(&fake_result("qwen3:8b", 100)).expect("save");
            let opts = fake_options();
            let line = format_recall_line("qwen3:8b", &opts).expect("some");
            assert!(line.contains("Last bench"));
            assert!(line.contains("47.2"));
            assert!(!line.contains("config differs"));
        });
    }

    #[test]
    #[serial]
    fn format_recall_line_when_options_differ() {
        with_anvil_home(|| {
            save_bench(&fake_result("qwen3:8b", 100)).expect("save");
            let mut opts = fake_options();
            opts.num_ctx = 8192; // changed from 32768
            let line = format_recall_line("qwen3:8b", &opts).expect("some");
            assert!(line.contains("config differs from current"));
        });
    }

    #[test]
    #[serial]
    fn benchmarks_dir_honors_anvil_home() {
        with_anvil_home(|| {
            let home = std::env::var("ANVIL_HOME").unwrap();
            let dir = benchmarks_dir();
            assert_eq!(dir, PathBuf::from(home).join("benchmarks"));
        });
    }

    #[test]
    fn timestamp_from_path_extracts_trailing_digits() {
        let p = PathBuf::from("/tmp/qwen3-8b-1715200000.json");
        assert_eq!(timestamp_from_path(&p), 1_715_200_000);
        let p = PathBuf::from("/tmp/llama-3-2-3b-100.json");
        assert_eq!(timestamp_from_path(&p), 100);
    }

    #[test]
    fn timestamp_from_path_returns_zero_on_garbage() {
        let p = PathBuf::from("/tmp/no-digits-here.json");
        assert_eq!(timestamp_from_path(&p), 0);
    }

    #[test]
    fn host_summary_reports_gpu_kind_label() {
        let mut hw = HardwareProfile {
            ram_total_bytes: 32 * 1024 * 1024 * 1024,
            ram_available_bytes: 16 * 1024 * 1024 * 1024,
            gpu_kind: GpuKind::Metal,
            gpu_name: Some("Apple M2".to_string()),
            vram_total_bytes: 32 * 1024 * 1024 * 1024,
            vram_free_bytes: 22 * 1024 * 1024 * 1024,
            cpu_threads: 12,
            perf_cores: Some(8),
            has_avx2: false,
            has_avx512: false,
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        };
        let s = host_summary_from_hw(&hw);
        assert_eq!(s.gpu_kind, "metal");
        assert_eq!(s.vram_total_gb, 32);

        hw.gpu_kind = GpuKind::None;
        assert_eq!(host_summary_from_hw(&hw).gpu_kind, "cpu");

        hw.gpu_kind = GpuKind::Cuda;
        assert_eq!(host_summary_from_hw(&hw).gpu_kind, "cuda");
    }

    #[test]
    fn cmd_bench_blocking_empty_arg_shows_usage() {
        let out = cmd_bench_blocking("http://localhost:11434", "");
        assert!(out.contains("Usage:"));
    }

    #[test]
    fn cmd_bench_blocking_whitespace_arg_shows_usage() {
        let out = cmd_bench_blocking("http://localhost:11434", "   ");
        assert!(out.contains("Usage:"));
    }
}
