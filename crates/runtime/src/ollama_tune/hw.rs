//! Host hardware introspection for the Ollama tuner.
//!
//! Detects RAM, GPU (Metal / CUDA / `ROCm`), CPU thread count, performance
//! cores, and AVX2 / AVX-512 availability across macOS, Linux, and Windows.
//!
//! Design notes:
//! - All OS-specific code paths live behind `#[cfg(target_os = ...)]`.
//! - All subprocess invocations go through [`run_with_timeout`], which kills
//!   any child that exceeds 2 seconds. On failure we log via `eprintln!`
//!   (the runtime crate has no `tracing` dep yet) and return safe defaults.
//! - Parsing is split out into pure functions so they can be exercised
//!   without touching real hardware (see `tests` below).
//! - No new crate dependencies: only `std`, `serde`, and the workspace's
//!   existing `serde_json` for one parse step.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Class of GPU acceleration the host exposes to Ollama.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuKind {
    /// Apple Metal (Apple Silicon or Intel Mac with discrete GPU).
    Metal,
    /// NVIDIA CUDA.
    Cuda,
    /// AMD `ROCm`.
    Rocm,
    /// CPU-only (no detected accelerator).
    None,
}

/// Snapshot of host hardware relevant to Ollama tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// Total physical RAM in bytes.
    pub ram_total_bytes: u64,
    /// Best-effort available RAM in bytes.
    pub ram_available_bytes: u64,
    /// Detected GPU class.
    pub gpu_kind: GpuKind,
    /// Detected GPU name (model string), if any.
    pub gpu_name: Option<String>,
    /// VRAM total. `0` when no discrete GPU is present. For Apple Silicon
    /// (Metal + unified memory), this equals `ram_total_bytes`.
    pub vram_total_bytes: u64,
    /// Best-effort free VRAM. `0` if the platform does not expose it.
    pub vram_free_bytes: u64,
    /// Logical CPU thread count.
    pub cpu_threads: u32,
    /// macOS performance-core count (`hw.perflevel0.physicalcpu`).
    /// `None` on Linux / Windows — neither exposes p-core data uniformly
    /// across distributions / vendor BIOSes, so we don't pretend to.
    pub perf_cores: Option<u32>,
    /// AVX2 support (`x86_64` only).
    pub has_avx2: bool,
    /// AVX-512F support (`x86_64` only).
    pub has_avx512: bool,
    /// `"macos"` | `"linux"` | `"windows"` | `"unknown"`.
    pub os: String,
    /// `"x86_64"` | `"aarch64"` | `std::env::consts::ARCH` fallback.
    pub arch: String,
}

impl HardwareProfile {
    /// Conservative all-zero default returned when detection fails entirely.
    fn safe_default() -> Self {
        Self {
            ram_total_bytes: 0,
            ram_available_bytes: 0,
            gpu_kind: GpuKind::None,
            gpu_name: None,
            vram_total_bytes: 0,
            vram_free_bytes: 0,
            cpu_threads: 1,
            perf_cores: None,
            has_avx2: false,
            has_avx512: false,
            os: detect_os().to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

/// Detect host hardware. Always re-runs (no caching).
///
/// Never panics; on detection failure individual fields fall back to safe
/// defaults and a `[ollama_tune::hw]` warning is printed to stderr.
#[must_use]
pub fn detect() -> HardwareProfile {
    let mut profile = HardwareProfile::safe_default();
    detect_into(&mut profile);
    profile
}

type CacheCell = RwLock<Option<(Instant, Arc<HardwareProfile>)>>;

/// Cached version of [`detect`]. Re-detection happens at most every
/// 5 minutes; concurrent callers within that window share the same `Arc`.
#[must_use]
pub fn detect_cached() -> Arc<HardwareProfile> {
    const TTL: Duration = Duration::from_secs(5 * 60);
    static CACHE: OnceLock<CacheCell> = OnceLock::new();
    let cell = CACHE.get_or_init(|| RwLock::new(None));

    if let Ok(guard) = cell.read()
        && let Some((stamp, ref profile)) = *guard
        && stamp.elapsed() < TTL
    {
        return Arc::clone(profile);
    }

    let fresh = Arc::new(detect());
    if let Ok(mut guard) = cell.write() {
        *guard = Some((Instant::now(), Arc::clone(&fresh)));
    }
    fresh
}

#[cfg(target_os = "macos")]
fn detect_into(p: &mut HardwareProfile) {
    macos::populate(p);
}

#[cfg(target_os = "linux")]
fn detect_into(p: &mut HardwareProfile) {
    linux::populate(p);
}

#[cfg(target_os = "windows")]
fn detect_into(p: &mut HardwareProfile) {
    windows::populate(p);
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn detect_into(_p: &mut HardwareProfile) {
    // Unsupported platform — leave the safe default in place.
}

fn detect_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Run a subprocess and return its stdout, killing it after `timeout`.
/// Returns `None` on spawn failure, non-zero exit, or timeout.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn run_with_timeout(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child: Child = match Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            eprintln!("[ollama_tune::hw] warn: failed to spawn `{cmd}`: {err}");
            return None;
        }
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return Some(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!("[ollama_tune::hw] warn: `{cmd}` timed out after {timeout:?}");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                eprintln!("[ollama_tune::hw] warn: wait on `{cmd}` failed: {err}");
                let _ = child.kill();
                return None;
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
const SUBPROC_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Pure parsing helpers (testable without real hardware)
// ---------------------------------------------------------------------------

/// Parse `/proc/meminfo` and return `(MemTotal_bytes, MemAvailable_bytes)`.
/// Lines look like `MemTotal:       16384000 kB`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_proc_meminfo(input: &str) -> (u64, u64) {
    let mut total_kb: u64 = 0;
    let mut avail_kb: u64 = 0;
    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("memtotal:") {
            total_kb = parse_meminfo_kb(rest);
        } else if let Some(rest) = lower.strip_prefix("memavailable:") {
            avail_kb = parse_meminfo_kb(rest);
        }
    }
    (total_kb.saturating_mul(1024), avail_kb.saturating_mul(1024))
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo_kb(rest: &str) -> u64 {
    rest.trim()
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse the first row of `nvidia-smi --query-gpu=name,memory.total,memory.free
/// --format=csv,noheader,nounits`. Returns (name, total_bytes, free_bytes).
/// Memory values from nvidia-smi are in MiB.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_nvidia_smi(stdout: &str) -> Option<(String, u64, u64)> {
    let first = stdout.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let mut parts = first.split(',').map(str::trim);
    let name = parts.next()?.to_string();
    let total_mib: u64 = parts.next()?.parse().ok()?;
    let free_mib: u64 = parts.next()?.parse().ok()?;
    let mib = 1024u64 * 1024;
    Some((name, total_mib * mib, free_mib * mib))
}

/// Parse `sysctl -n hw.memsize` output (a single integer in bytes).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_sysctl_u64(stdout: &str) -> Option<u64> {
    stdout.trim().parse::<u64>().ok()
}

/// Parse `system_profiler SPDisplaysDataType` output for the first
/// "Chipset Model" + "VRAM (Total)" pair. Returns `(name, vram_bytes)`.
/// Recognises units of MB and GB (1024-based).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_system_profiler_displays(stdout: &str) -> Option<(String, u64)> {
    let mut name: Option<String> = None;
    let mut vram: Option<u64> = None;
    for raw in stdout.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Chipset Model:")
            && name.is_none()
        {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("VRAM (Total):")
            && vram.is_none()
        {
            vram = parse_vram_amount(rest.trim());
        } else if let Some(rest) = line.strip_prefix("VRAM (Dynamic, Max):")
            && vram.is_none()
        {
            vram = parse_vram_amount(rest.trim());
        }
    }
    match (name, vram) {
        (Some(n), Some(v)) => Some((n, v)),
        _ => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_vram_amount(s: &str) -> Option<u64> {
    let mut iter = s.split_whitespace();
    let num: f64 = iter.next()?.parse().ok()?;
    let unit = iter.next().unwrap_or("MB").to_ascii_uppercase();
    let mult: u64 = match unit.as_str() {
        "B" => 1,
        "KB" => 1024,
        "MB" => 1024 * 1024,
        "GB" => 1024 * 1024 * 1024,
        "TB" => 1024u64.pow(4),
        _ => return None,
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let bytes = (num * mult as f64) as u64;
    Some(bytes)
}

/// Parse `/proc/cpuinfo` "flags" line(s), returning `(has_avx2, has_avx512f)`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_proc_cpuinfo_flags(input: &str) -> (bool, bool) {
    let mut avx2 = false;
    let mut avx512 = false;
    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.starts_with("flags") || lower.starts_with("features")) {
            continue;
        }
        let Some((_, rest)) = lower.split_once(':') else {
            continue;
        };
        for tok in rest.split_whitespace() {
            if tok == "avx2" {
                avx2 = true;
            } else if tok == "avx512f" {
                avx512 = true;
            }
        }
    }
    (avx2, avx512)
}

/// Parse macOS `sysctl -n machdep.cpu.leaf7_features` for AVX2/AVX-512F.
/// Output is space-separated like `RDWRFSGS TSC_THREAD_OFFSET BMI1 ...`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_macos_leaf7_features(input: &str) -> (bool, bool) {
    let upper = input.to_ascii_uppercase();
    let tokens: Vec<&str> = upper.split_whitespace().collect();
    let avx2 = tokens.contains(&"AVX2");
    let avx512 = tokens.contains(&"AVX512F");
    (avx2, avx512)
}

/// Parse a `rocm-smi --showmeminfo vram --json` document, summing across
/// every GPU. Returns `(total_bytes, used_bytes)`.
///
/// rocm-smi shape (varies by version):
/// ```json
/// { "card0": { "VRAM Total Memory (B)": "17163091968",
///              "VRAM Total Used Memory (B)": "1073741824" } }
/// ```
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_rocm_smi_vram(input: &str) -> Option<(u64, u64)> {
    let val: serde_json::Value = serde_json::from_str(input).ok()?;
    let obj = val.as_object()?;
    let mut total: u64 = 0;
    let mut used: u64 = 0;
    for (_card, entry) in obj {
        let entry_obj = entry.as_object()?;
        for (k, v) in entry_obj {
            let key_lc = k.to_ascii_lowercase();
            let n = match v {
                serde_json::Value::String(s) => s.parse::<u64>().ok(),
                serde_json::Value::Number(num) => num.as_u64(),
                _ => None,
            };
            let Some(n) = n else { continue };
            // "VRAM Total Memory (B)" / "VRAM Total Used Memory (B)"
            if key_lc.contains("vram") && key_lc.contains("total") && key_lc.contains("used") {
                used = used.saturating_add(n);
            } else if key_lc.contains("vram") && key_lc.contains("total") {
                total = total.saturating_add(n);
            }
        }
    }
    if total == 0 {
        None
    } else {
        Some((total, used))
    }
}

/// Parse the `key=value` style `wmic` "/value" output and return the first
/// value matching `key` (case-insensitive).
#[cfg(any(target_os = "windows", test))]
pub(crate) fn parse_wmic_value(input: &str, key: &str) -> Option<String> {
    let key_lc = key.to_ascii_lowercase();
    for line in input.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().to_ascii_lowercase() == key_lc {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Parse `wmic path win32_VideoController get Name,AdapterRAM /value` output,
/// which contains repeated key=value blocks separated by blank lines. Returns
/// `(name, adapter_ram_bytes)` for the first GPU with a non-empty name.
///
/// Note: AdapterRAM is reported as a 32-bit unsigned int by WMI and caps at
/// ~4 GiB. Callers should treat very-large GPUs as "size unknown".
#[cfg(any(target_os = "windows", test))]
pub(crate) fn parse_wmic_video_controllers(input: &str) -> Option<(String, u64)> {
    let mut name: Option<String> = None;
    let mut ram: u64 = 0;
    for raw in input.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() {
            if let Some(n) = name.clone() {
                return Some((n, ram));
            }
            // reset between blocks
            name = None;
            ram = 0;
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        if k == "name" {
            name = Some(v.to_string());
        } else if k == "adapterram" {
            ram = v.parse::<u64>().unwrap_or(0);
        }
    }
    name.map(|n| (n, ram))
}

// ---------------------------------------------------------------------------
// Per-OS detection
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        detect_os, parse_macos_leaf7_features, parse_system_profiler_displays, parse_sysctl_u64,
        run_with_timeout, GpuKind, HardwareProfile, SUBPROC_TIMEOUT,
    };

    pub(super) fn populate(p: &mut HardwareProfile) {
        p.os = detect_os().to_string();
        p.arch = std::env::consts::ARCH.to_string();

        // RAM total
        if let Some(out) = run_with_timeout("sysctl", &["-n", "hw.memsize"], SUBPROC_TIMEOUT)
            && let Some(bytes) = parse_sysctl_u64(&out)
        {
            p.ram_total_bytes = bytes;
        }
        // RAM available — try vm_stat (page count × 4096); fall back to
        // ram_total / 2 (a deliberately conservative estimate so callers
        // never claim "all RAM is free").
        p.ram_available_bytes = vm_stat_available_bytes().unwrap_or(p.ram_total_bytes / 2);

        // Threads
        if let Some(out) = run_with_timeout("sysctl", &["-n", "hw.ncpu"], SUBPROC_TIMEOUT)
            && let Some(n) = parse_sysctl_u64(&out)
        {
            p.cpu_threads = u32::try_from(n).unwrap_or(1).max(1);
        }
        // Performance cores (Apple Silicon only)
        if let Some(out) = run_with_timeout(
            "sysctl",
            &["-n", "hw.perflevel0.physicalcpu"],
            SUBPROC_TIMEOUT,
        ) && let Some(n) = parse_sysctl_u64(&out)
            && n > 0
        {
            p.perf_cores = Some(u32::try_from(n).unwrap_or(0));
        }

        // Apple Silicon detection.
        let is_apple_silicon = detect_apple_silicon();

        if is_apple_silicon {
            p.gpu_kind = GpuKind::Metal;
            p.gpu_name = sysctl_string("machdep.cpu.brand_string");
            // Unified memory: GPU shares system RAM. Metal is generally
            // capped at ~70% of total physical for VRAM-equivalent usage.
            p.vram_total_bytes = p.ram_total_bytes;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                p.vram_free_bytes = (p.ram_available_bytes as f64 * 0.70) as u64;
            }
            // ARM — no AVX.
            p.has_avx2 = false;
            p.has_avx512 = false;
        } else {
            // Intel Mac — try system_profiler for a discrete/integrated GPU.
            if let Some(out) = run_with_timeout(
                "system_profiler",
                &["SPDisplaysDataType"],
                SUBPROC_TIMEOUT,
            ) && let Some((name, vram_bytes)) = parse_system_profiler_displays(&out)
            {
                // Anvil's tuner treats Intel-Mac GPUs as Metal even when
                // they're discrete AMD cards — Ollama uses Metal there.
                p.gpu_kind = GpuKind::Metal;
                p.gpu_name = Some(name);
                p.vram_total_bytes = vram_bytes;
                // Intel Macs don't expose live VRAM-free numbers; leave 0.
                p.vram_free_bytes = 0;
            }
            // AVX feature sniffing on Intel
            if let Some(out) = run_with_timeout(
                "sysctl",
                &["-n", "machdep.cpu.leaf7_features"],
                SUBPROC_TIMEOUT,
            ) {
                let (avx2, avx512) = parse_macos_leaf7_features(&out);
                p.has_avx2 = avx2;
                p.has_avx512 = avx512;
            }
        }
    }

    fn detect_apple_silicon() -> bool {
        if std::env::consts::ARCH == "aarch64" {
            return true;
        }
        if let Some(out) = run_with_timeout(
            "sysctl",
            &["-n", "hw.optional.arm64"],
            SUBPROC_TIMEOUT,
        ) && out.trim() == "1"
        {
            return true;
        }
        if let Some(brand) = sysctl_string("machdep.cpu.brand_string")
            && brand.contains("Apple")
        {
            return true;
        }
        false
    }

    fn sysctl_string(key: &str) -> Option<String> {
        let out = run_with_timeout("sysctl", &["-n", key], SUBPROC_TIMEOUT)?;
        let s = out.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    fn vm_stat_available_bytes() -> Option<u64> {
        let out = run_with_timeout("vm_stat", &[], SUBPROC_TIMEOUT)?;
        super::parse_vm_stat_available(&out)
    }
}

/// Sum free + inactive + speculative pages from `vm_stat` and convert to
/// bytes. macOS reports "page size of N bytes" in the header; we honour it
/// when present, otherwise default to 4096.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_vm_stat_available(input: &str) -> Option<u64> {
    let mut page_size: u64 = 4096;
    let mut free: u64 = 0;
    let mut inactive: u64 = 0;
    let mut speculative: u64 = 0;
    let mut saw_any = false;

    for line in input.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Mach Virtual Memory Statistics:") {
            // Header line. Look for "page size of N bytes" in the same line
            // or fall through (the size keyword may live on a continuation).
            if let Some(idx) = rest.find("page size of ") {
                let tail = &rest[idx + "page size of ".len()..];
                if let Some(num) = tail.split_whitespace().next()
                    && let Ok(n) = num.parse::<u64>()
                {
                    page_size = n;
                }
            }
            continue;
        }
        if trimmed.starts_with("Pages free:") {
            free = parse_vm_stat_pages(trimmed);
            saw_any = true;
        } else if trimmed.starts_with("Pages inactive:") {
            inactive = parse_vm_stat_pages(trimmed);
            saw_any = true;
        } else if trimmed.starts_with("Pages speculative:") {
            speculative = parse_vm_stat_pages(trimmed);
            saw_any = true;
        }
    }
    if !saw_any {
        return None;
    }
    Some(
        free.saturating_add(inactive)
            .saturating_add(speculative)
            .saturating_mul(page_size),
    )
}

#[cfg(any(target_os = "macos", test))]
fn parse_vm_stat_pages(line: &str) -> u64 {
    // e.g. "Pages free:                         123456."
    let after_colon = line.split_once(':').map_or("", |(_, r)| r);
    let cleaned: String = after_colon
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    cleaned.parse::<u64>().unwrap_or(0)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        detect_os, parse_nvidia_smi, parse_proc_cpuinfo_flags, parse_proc_meminfo,
        parse_rocm_smi_vram, run_with_timeout, GpuKind, HardwareProfile, SUBPROC_TIMEOUT,
    };

    pub(super) fn populate(p: &mut HardwareProfile) {
        p.os = detect_os().to_string();
        p.arch = std::env::consts::ARCH.to_string();

        // RAM
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            let (total, avail) = parse_proc_meminfo(&meminfo);
            p.ram_total_bytes = total;
            p.ram_available_bytes = avail;
        } else {
            eprintln!("[ollama_tune::hw] warn: failed to read /proc/meminfo");
        }

        // CPU threads — std reports logical CPUs reliably on Linux.
        if let Ok(n) = std::thread::available_parallelism() {
            p.cpu_threads = u32::try_from(n.get()).unwrap_or(1);
        }

        // p-cores: Linux exposes nothing portable. The kernel surface for
        // hybrid x86 cores (intel_pstate "type" file) varies by kernel
        // version, distro, and BIOS. Leave None rather than mislead.
        p.perf_cores = None;

        // CPU flags
        if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
            let (avx2, avx512) = parse_proc_cpuinfo_flags(&cpuinfo);
            p.has_avx2 = avx2;
            p.has_avx512 = avx512;
        }

        // GPU detection priority: NVIDIA → ROCm CLI → AMD sysfs → none.
        if let Some(out) = run_with_timeout(
            "nvidia-smi",
            &[
                "--query-gpu=name,memory.total,memory.free",
                "--format=csv,noheader,nounits",
            ],
            SUBPROC_TIMEOUT,
        ) {
            if let Some((name, total, free)) = parse_nvidia_smi(&out) {
                p.gpu_kind = GpuKind::Cuda;
                p.gpu_name = Some(name);
                p.vram_total_bytes = total;
                p.vram_free_bytes = free;
                return;
            }
        }
        if let Some(out) =
            run_with_timeout("rocm-smi", &["--showmeminfo", "vram", "--json"], SUBPROC_TIMEOUT)
        {
            if let Some((total, used)) = parse_rocm_smi_vram(&out) {
                p.gpu_kind = GpuKind::Rocm;
                p.gpu_name = read_amd_gpu_name();
                p.vram_total_bytes = total;
                p.vram_free_bytes = total.saturating_sub(used);
                return;
            }
        }
        // AMD sysfs fallback (no rocm-smi installed but AMD card present).
        if let Some((total, used)) = read_amd_sysfs_vram() {
            p.gpu_kind = GpuKind::Rocm;
            p.gpu_name = read_amd_gpu_name();
            p.vram_total_bytes = total;
            p.vram_free_bytes = total.saturating_sub(used);
        }
    }

    fn read_amd_sysfs_vram() -> Option<(u64, u64)> {
        let total = std::fs::read_to_string("/sys/class/drm/card0/device/mem_info_vram_total")
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        let used = std::fs::read_to_string("/sys/class/drm/card0/device/mem_info_vram_used")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        Some((total, used))
    }

    fn read_amd_gpu_name() -> Option<String> {
        let raw = std::fs::read_to_string("/sys/class/drm/card0/device/product_name").ok()?;
        let s = raw.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{
        detect_os, parse_wmic_value, parse_wmic_video_controllers, run_with_timeout, GpuKind,
        HardwareProfile, SUBPROC_TIMEOUT,
    };

    pub(super) fn populate(p: &mut HardwareProfile) {
        p.os = detect_os().to_string();
        p.arch = std::env::consts::ARCH.to_string();

        // RAM total
        if let Some(out) = run_with_timeout(
            "wmic",
            &["computersystem", "get", "TotalPhysicalMemory", "/value"],
            SUBPROC_TIMEOUT,
        ) {
            if let Some(v) = parse_wmic_value(&out, "TotalPhysicalMemory") {
                p.ram_total_bytes = v.parse::<u64>().unwrap_or(0);
            }
        } else {
            eprintln!("[ollama_tune::hw] warn: wmic computersystem unavailable");
        }
        // Free physical memory (KB → bytes)
        if let Some(out) = run_with_timeout(
            "wmic",
            &["OS", "get", "FreePhysicalMemory", "/value"],
            SUBPROC_TIMEOUT,
        ) {
            if let Some(v) = parse_wmic_value(&out, "FreePhysicalMemory") {
                let kb = v.parse::<u64>().unwrap_or(0);
                p.ram_available_bytes = kb.saturating_mul(1024);
            }
        }
        if p.ram_available_bytes == 0 && p.ram_total_bytes > 0 {
            p.ram_available_bytes = p.ram_total_bytes / 2;
        }

        // CPU threads
        if let Ok(s) = std::env::var("NUMBER_OF_PROCESSORS") {
            if let Ok(n) = s.trim().parse::<u32>() {
                p.cpu_threads = n.max(1);
            }
        }
        if p.cpu_threads == 1 {
            if let Ok(n) = std::thread::available_parallelism() {
                p.cpu_threads = u32::try_from(n.get()).unwrap_or(1);
            }
        }
        p.perf_cores = None;

        // GPU
        if let Some(out) = run_with_timeout(
            "wmic",
            &["path", "win32_VideoController", "get", "Name,AdapterRAM", "/value"],
            SUBPROC_TIMEOUT,
        ) {
            if let Some((name, ram)) = parse_wmic_video_controllers(&out) {
                let lower = name.to_ascii_lowercase();
                p.gpu_kind = if lower.contains("nvidia") {
                    GpuKind::Cuda
                } else if lower.contains("amd") || lower.contains("radeon") {
                    GpuKind::Rocm
                } else {
                    GpuKind::None
                };
                p.gpu_name = Some(name);
                // AdapterRAM is a uint32 in WMI and silently caps at 4 GiB
                // for GPUs with more memory. Treat 0 as "unknown" for any
                // detected discrete GPU.
                p.vram_total_bytes = ram;
                p.vram_free_bytes = 0;
            }
        }

        // AVX2/AVX-512 on Windows requires a cpuid crate (raw-cpuid) or
        // unsafe inline asm. Both are out of scope for v1; we'd add a
        // dependency on cpufeatures for v2. Leave both false for now.
        p.has_avx2 = false;
        p.has_avx512 = false;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_meminfo_extracts_total_and_available() {
        let sample = "\
MemTotal:       16384000 kB
MemFree:         1024000 kB
MemAvailable:    8192000 kB
Buffers:          512000 kB
";
        let (total, avail) = parse_proc_meminfo(sample);
        assert_eq!(total, 16_384_000u64 * 1024);
        assert_eq!(avail, 8_192_000u64 * 1024);
    }

    #[test]
    fn parse_proc_meminfo_handles_missing_available() {
        let sample = "MemTotal:       1000 kB\n";
        let (total, avail) = parse_proc_meminfo(sample);
        assert_eq!(total, 1000 * 1024);
        assert_eq!(avail, 0);
    }

    #[test]
    fn parse_nvidia_smi_csv_first_gpu() {
        let sample = "NVIDIA GeForce RTX 4090, 24564, 23012\nNVIDIA RTX A5000, 24576, 24000\n";
        let (name, total, free) = parse_nvidia_smi(sample).expect("first row parses");
        assert_eq!(name, "NVIDIA GeForce RTX 4090");
        let mib = 1024u64 * 1024;
        assert_eq!(total, 24_564 * mib);
        assert_eq!(free, 23_012 * mib);
    }

    #[test]
    fn parse_nvidia_smi_handles_empty() {
        assert!(parse_nvidia_smi("").is_none());
        assert!(parse_nvidia_smi("\n").is_none());
    }

    #[test]
    fn parse_sysctl_memsize() {
        assert_eq!(parse_sysctl_u64("68719476736\n"), Some(68_719_476_736));
        assert_eq!(parse_sysctl_u64("  42  "), Some(42));
        assert_eq!(parse_sysctl_u64("not-a-number"), None);
    }

    #[test]
    fn parse_system_profiler_displays_finds_vram_gb() {
        let sample = "\
Graphics/Displays:

    AMD Radeon Pro 5500M:

      Chipset Model: AMD Radeon Pro 5500M
      Type: GPU
      Bus: PCIe
      VRAM (Total): 8 GB
      Vendor: AMD (0x1002)
";
        let (name, vram) = parse_system_profiler_displays(sample).expect("parse");
        assert_eq!(name, "AMD Radeon Pro 5500M");
        assert_eq!(vram, 8 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_system_profiler_displays_finds_vram_mb() {
        let sample = "\
      Chipset Model: Intel Iris Plus Graphics
      VRAM (Dynamic, Max): 1536 MB
";
        let (_, vram) = parse_system_profiler_displays(sample).expect("parse");
        assert_eq!(vram, 1536u64 * 1024 * 1024);
    }

    #[test]
    fn parse_proc_cpuinfo_flags_detects_avx2_and_avx512() {
        let sample = "\
processor   : 0
vendor_id   : GenuineIntel
flags       : fpu vme de pse tsc msr pae mce sse4_1 sse4_2 avx avx2 fma popcnt avx512f avx512dq
";
        let (avx2, avx512) = parse_proc_cpuinfo_flags(sample);
        assert!(avx2);
        assert!(avx512);
    }

    #[test]
    fn parse_proc_cpuinfo_flags_no_avx512() {
        let sample = "flags : fpu sse4_2 avx avx2 fma\n";
        let (avx2, avx512) = parse_proc_cpuinfo_flags(sample);
        assert!(avx2);
        assert!(!avx512);
    }

    #[test]
    fn parse_macos_leaf7_features_detects_avx() {
        let sample = "RDWRFSGS TSC_THREAD_OFFSET BMI1 AVX2 BMI2 ERMS INVPCID FPU_CSDS\n";
        let (avx2, avx512) = parse_macos_leaf7_features(sample);
        assert!(avx2);
        assert!(!avx512);

        let with_512 = "AVX2 AVX512F AVX512DQ\n";
        let (avx2b, avx512b) = parse_macos_leaf7_features(with_512);
        assert!(avx2b);
        assert!(avx512b);
    }

    #[test]
    fn parse_rocm_smi_vram_sums_cards() {
        let sample = r#"{
            "card0": {
                "VRAM Total Memory (B)": "17163091968",
                "VRAM Total Used Memory (B)": "1073741824"
            },
            "card1": {
                "VRAM Total Memory (B)": "17163091968",
                "VRAM Total Used Memory (B)": "0"
            }
        }"#;
        let (total, used) = parse_rocm_smi_vram(sample).expect("parses");
        assert_eq!(total, 17_163_091_968u64 * 2);
        assert_eq!(used, 1_073_741_824);
    }

    #[test]
    fn parse_wmic_value_extracts_field() {
        let sample = "\r\n\r\nTotalPhysicalMemory=17179869184\r\n\r\n";
        assert_eq!(
            parse_wmic_value(sample, "TotalPhysicalMemory"),
            Some("17179869184".to_string())
        );
        assert_eq!(parse_wmic_value(sample, "Missing"), None);
    }

    #[test]
    fn parse_wmic_video_controllers_first_device() {
        let sample = "\r\n\r\nAdapterRAM=4293918720\r\nName=NVIDIA GeForce RTX 3080\r\n\r\n\r\nAdapterRAM=0\r\nName=Microsoft Basic Display Adapter\r\n\r\n";
        let (name, ram) = parse_wmic_video_controllers(sample).expect("parsed");
        assert_eq!(name, "NVIDIA GeForce RTX 3080");
        assert_eq!(ram, 4_293_918_720);
    }

    #[test]
    fn parse_vm_stat_available_sums_free_inactive_speculative() {
        let sample = "\
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                              100.
Pages active:                          50000.
Pages inactive:                         200.
Pages speculative:                       50.
Pages throttled:                            0.
";
        let bytes = parse_vm_stat_available(sample).expect("parse");
        // (100 + 200 + 50) * 16384
        assert_eq!(bytes, 350u64 * 16_384);
    }

    #[test]
    fn parse_vm_stat_available_default_page_size() {
        let sample = "\
Mach Virtual Memory Statistics:
Pages free:    10.
Pages inactive: 20.
Pages speculative: 5.
";
        // No page size in header → default 4096.
        let bytes = parse_vm_stat_available(sample).expect("parse");
        assert_eq!(bytes, 35u64 * 4096);
    }

    #[test]
    fn gpu_kind_serde_round_trip() {
        let kinds = [GpuKind::Metal, GpuKind::Cuda, GpuKind::Rocm, GpuKind::None];
        let expected = ["\"metal\"", "\"cuda\"", "\"rocm\"", "\"none\""];
        for (k, want) in kinds.iter().zip(expected.iter()) {
            let s = serde_json::to_string(k).unwrap();
            assert_eq!(&s, want);
            let back: GpuKind = serde_json::from_str(&s).unwrap();
            assert_eq!(*k, back);
        }
    }

    #[test]
    fn safe_default_is_sane() {
        let p = HardwareProfile::safe_default();
        assert_eq!(p.gpu_kind, GpuKind::None);
        assert!(p.cpu_threads >= 1);
        assert!(matches!(p.os.as_str(), "macos" | "linux" | "windows" | "unknown"));
    }

    #[test]
    fn detect_returns_a_profile() {
        // We can't assert specific values, but `detect` must always
        // return a structurally-valid profile without panicking.
        let p = detect();
        assert!(p.cpu_threads >= 1);
    }
}
