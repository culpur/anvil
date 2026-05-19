//! Run one routine end-to-end.
//!
//! The executor is the bridge between scheduling (the daemon decides *when*)
//! and the user's model (Anvil itself decides *what*).  Each invocation:
//!
//! 1. Builds the prompt by combining the routine's `prompt` field with any
//!    `## Context From <name>` blocks loaded from the most recent successful
//!    packets of every routine in `context_from`.
//! 2. Spawns `anvil -p <prompt>` as a subprocess with `--permission-mode`
//!    and (optionally) `--model` set from the routine's TOML.  Subprocess
//!    runs in the routine's `cwd` if set, else the daemon's cwd.
//! 3. Captures stdout (the LLM answer) and stderr (any diagnostics), waits
//!    for exit, and classifies the result as Clean / Silent / Failed.
//! 4. Writes a [`RunRecord`] to the markdown archive and a [`RoutinePacket`]
//!    JSON sidecar so future routines that reference this one in their
//!    `context_from` can read the result.
//! 5. Dispatches the run to every [`DeliveryTarget`] on the routine — local
//!    archive is already on disk by step 4, webhooks are POSTed here.
//!
//! ## Why subprocess and not in-process
//!
//! Two reasons.  First, the daemon runs in a separate OS process from the
//! TUI (`anvild`), so it doesn't carry the user's interactive session state
//! — running `anvil -p` is exactly how a fresh non-interactive session
//! materialises.  Second, the subprocess boundary gives us a clean kill
//! signal: if a routine hangs we send SIGTERM to one PID and walk away,
//! rather than trying to cancel a half-streamed in-process turn.
//!
//! ## What this module deliberately does NOT do
//!
//! - **Vault unlock prompts.** A locked vault means `anvil -p` will error
//!   out; the executor reports `Failed` and the daemon logs the missing
//!   vault.  We never prompt for a password — the routine ran while the
//!   user was away.
//! - **OAuth refresh.** If the configured provider OAuth token has expired
//!   and the on-disk credentials don't have a refresh_token, `anvil -p`
//!   errors and we report Failed.  Background OAuth refresh is the v2.2.16
//!   keep-alive ticker's responsibility (#597), independent of routines.
//! - **Retries.** A failed run is logged and the schedule advances normally.
//!   Routines that need retry semantics encode them in the prompt
//!   ("if the API is down, reply [SILENT]").

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::routines::archive::{write_archive, RunRecord, RunStatus};
use crate::routines::definition::{DeliveryTarget, RoutineDef};
use crate::routines::delivery::{build_payload, dispatch, TargetOutcome};
use crate::routines::packet::{extract_summary, write_packet, PacketStatus, RoutinePacket};
use crate::routines::{is_silent_output, SILENT_MARKER};

// ─── Public types ────────────────────────────────────────────────────────────

/// Inputs to a single routine invocation.  Pure data — no live handles,
/// passable across threads, suitable for serialising into a job queue later
/// if the daemon ever grows one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    /// Routine definition snapshot to run.
    pub routine: RoutineDef,
    /// Path to the `anvil` binary the executor should spawn.  The daemon
    /// resolves this once at startup so multiple routines share the same
    /// binary even if the user `anvil --update`s mid-day.
    pub anvil_binary: PathBuf,
    /// `~/.anvil` (or `ANVIL_CONFIG_HOME` equivalent) the subprocess
    /// should use.  Passed through `ANVIL_CONFIG_HOME` so tests can target
    /// a sandbox.
    pub config_home: PathBuf,
    /// Anvil version string for the WebhookPayload envelope.
    pub anvil_version: String,
    /// Hard timeout for the whole run (default: 5 min).  The subprocess
    /// is sent SIGTERM (Unix) or kill (Windows) when this elapses.
    pub timeout: Duration,
    /// Pre-resolved context blocks keyed by upstream routine name.  The
    /// daemon resolves these by reading the latest sidecar packet for each
    /// upstream routine — keeps the executor pure (no filesystem walks for
    /// context injection happen in this module).
    pub context_blocks: Vec<ContextBlock>,
}

/// One upstream routine's recent output injected into the prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub routine: String,
    pub summary: String,
    pub body: String,
}

/// Outcome of one routine run.  Includes everything the daemon needs to
/// log the result and the per-delivery outcomes for the user to inspect
/// via `/schedule show`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecOutcome {
    pub routine: String,
    pub run_id: String,
    pub status: RunStatus,
    pub started_at: u64,
    pub ended_at: u64,
    pub duration_ms: u64,
    /// Captured stdout of the subprocess (the LLM body).  Empty on Failed.
    pub body: String,
    /// First paragraph of the body, suitable for `/schedule show <name>`.
    pub summary: String,
    /// Subprocess exit code; `None` on timeout-kill.
    pub exit_code: Option<i32>,
    /// First line of subprocess stderr; truncated to 400 chars so logs stay
    /// readable.  Only populated when status == Failed.
    pub error: Option<String>,
    /// Per-delivery outcomes from [`dispatch`].
    pub deliveries: Vec<TargetOutcome>,
    /// On-disk path the archive was written to (None if archive write failed).
    pub archive_path: Option<PathBuf>,
    /// On-disk path the packet sidecar was written to.
    pub packet_path: Option<PathBuf>,
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Run one routine to completion, writing archive + packet + dispatching
/// deliveries.  Synchronous: blocks the current thread until the subprocess
/// exits, the archive is written, and every delivery has been attempted.
///
/// `vault_resolver` is invoked once per `vault://` URL referenced by a
/// webhook target.  Pass `|_| None` from contexts that haven't unlocked the
/// vault — those webhooks will fail-fast with a clear error message instead
/// of POSTing literal `vault://` URLs.
pub fn run_once<F>(req: &ExecRequest, vault_resolver: F) -> ExecOutcome
where
    F: Fn(&str) -> Option<String>,
{
    let started_at = unix_now();
    let started_instant = Instant::now();
    let run_id = mint_run_id(started_at, &req.routine.name);

    let prompt = assemble_prompt(&req.routine.prompt, &req.context_blocks);

    let spawn_outcome = spawn_anvil_subprocess(req, &prompt, req.timeout);

    let duration_ms = u64::try_from(started_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
    let ended_at = unix_now();

    let (status, body, exit_code, error) = classify_outcome(spawn_outcome);

    let summary = extract_summary(&body);

    // 1. Write the markdown archive.
    let record = RunRecord {
        routine_id: req.routine.name.clone(),
        routine_name: req.routine.name.clone(),
        run_id: run_id.clone(),
        started_at,
        ended_at,
        duration_ms,
        status,
        schedule_display: req.routine.schedule_raw.clone(),
        model: req.routine.model.clone().unwrap_or_default(),
        tokens_in: 0,  // not tracked from headless subprocess; left for future hook
        tokens_out: 0, // ditto
        output: body.clone(),
        error: error.clone(),
    };
    let archive_path = match write_archive(&record) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!(
                "[routines::executor] warn: archive write failed for {}: {e}",
                req.routine.name
            );
            None
        }
    };

    // 2. Write the packet sidecar (input_hash-keyed, used by chained
    //    routines as context).
    let packet_status = match status {
        RunStatus::Clean => PacketStatus::Clean,
        RunStatus::Silent => PacketStatus::Silent,
        RunStatus::Failed => PacketStatus::Failed,
    };
    let input_hash = crate::routines::packet::compute_input_hash(
        "anvil routine runner",
        &prompt,
        None,
    );
    let packet = RoutinePacket {
        routine_id: req.routine.name.clone(),
        run_id: run_id.clone(),
        started_at,
        ended_at,
        status: packet_status,
        summary: summary.clone(),
        body: body.clone(),
        input_hash: input_hash.clone(),
    };
    let packet_path = match write_packet(&packet) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!(
                "[routines::executor] warn: packet write failed for {}: {e}",
                req.routine.name
            );
            None
        }
    };

    // 3. Dispatch deliveries.  Local is always synthetically OK (the archive
    //    above is the local delivery).  Webhooks fire here; we skip them
    //    entirely when status == Silent.
    let silent = matches!(status, RunStatus::Silent);
    let payload = build_payload(
        &req.routine.name,
        started_at,
        ended_at,
        status.into(),
        &summary,
        &body,
        "anvil routine runner",
        &prompt,
        None,
        &req.anvil_version,
    );
    let deliveries = dispatch(&req.routine.delivery, &payload, silent, &vault_resolver);

    ExecOutcome {
        routine: req.routine.name.clone(),
        run_id,
        status,
        started_at,
        ended_at,
        duration_ms,
        body,
        summary,
        exit_code,
        error,
        deliveries,
        archive_path,
        packet_path,
    }
}

// ─── Prompt assembly ─────────────────────────────────────────────────────────

/// Combine the routine's authored prompt with `## Context From <name>`
/// blocks loaded from upstream routines.  Pure: testable without a daemon.
///
/// Layout:
/// ```text
/// ## Context From <name-of-upstream-1>
///
/// <body of upstream-1's most recent packet>
///
/// ---
///
/// ## Context From <name-of-upstream-2>
///
/// <body of upstream-2's most recent packet>
///
/// ---
///
/// <routine's own prompt>
/// ```
///
/// Context blocks come first so the user's prompt is the last thing in the
/// model's working memory — that's where instructions belong.  Each block
/// is separated by `---` so terminal-style renderers display clean breaks.
#[must_use]
pub fn assemble_prompt(authored: &str, context: &[ContextBlock]) -> String {
    if context.is_empty() {
        return authored.to_string();
    }
    let mut out = String::with_capacity(authored.len() + 256 * context.len());
    for block in context {
        out.push_str("## Context From ");
        out.push_str(&block.routine);
        out.push_str("\n\n");
        if !block.summary.is_empty() {
            out.push_str("**Summary:** ");
            out.push_str(&block.summary);
            out.push_str("\n\n");
        }
        out.push_str(&block.body);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n---\n\n");
    }
    out.push_str(authored);
    out
}

// ─── Subprocess plumbing ─────────────────────────────────────────────────────

struct SpawnOutcome {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
    spawn_error: Option<String>,
}

fn spawn_anvil_subprocess(req: &ExecRequest, prompt: &str, timeout: Duration) -> SpawnOutcome {
    let mut cmd = Command::new(&req.anvil_binary);
    cmd.arg("-p")
        .arg(prompt)
        .arg("--permission-mode")
        .arg(req.routine.permission_mode.as_cli_arg())
        .env("ANVIL_CONFIG_HOME", &req.config_home)
        .env("ANVIL_ROUTINE_NAME", &req.routine.name)
        // Headless: never open the alt-screen even if a tty happens to be
        // attached.  Avoids the feedback-tui-stdout class of bugs.
        .env("ANVIL_DISABLE_ALTERNATE_SCREEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(model) = &req.routine.model {
        cmd.arg("--model").arg(model);
    }
    if let Some(cwd) = &req.routine.cwd {
        cmd.current_dir(expand_cwd(cwd, &req.config_home));
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SpawnOutcome {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                timed_out: false,
                spawn_error: Some(format!(
                    "failed to spawn `{}`: {e}",
                    req.anvil_binary.display()
                )),
            };
        }
    };

    // Capture pipes on background threads so we never deadlock if the child
    // outputs more than the pipe buffer can hold (~64 KB on Linux).
    let stdout_handle = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || -> String {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || -> String {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break None,
        }
    };

    let stdout = stdout_handle.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    let stderr = stderr_handle.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    let exit_code = exit_status.and_then(|s| s.code());

    SpawnOutcome {
        stdout,
        stderr,
        exit_code,
        timed_out,
        spawn_error: None,
    }
}

fn classify_outcome(s: SpawnOutcome) -> (RunStatus, String, Option<i32>, Option<String>) {
    if let Some(err) = s.spawn_error {
        return (RunStatus::Failed, String::new(), None, Some(err));
    }
    if s.timed_out {
        return (
            RunStatus::Failed,
            s.stdout,
            None,
            Some("subprocess timed out".to_string()),
        );
    }
    let exit_ok = matches!(s.exit_code, Some(0));
    if !exit_ok {
        let preview: String = s.stderr.lines().next().unwrap_or("").chars().take(400).collect();
        return (
            RunStatus::Failed,
            s.stdout,
            s.exit_code,
            Some(format!(
                "exit_code={}: {}",
                s.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                preview
            )),
        );
    }
    if is_silent_output(&s.stdout) {
        // Strip the SILENT marker from the body so downstream consumers
        // don't see it in summaries.  We keep everything around it so the
        // operator can still inspect why the agent decided to be silent.
        let cleaned = s.stdout.replace(SILENT_MARKER, "").trim().to_string();
        return (RunStatus::Silent, cleaned, s.exit_code, None);
    }
    (RunStatus::Clean, s.stdout, s.exit_code, None)
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mint_run_id(started_at: u64, routine: &str) -> String {
    // 8 chars: 4 from started_at low bits, 4 from a small hash of the
    // routine name.  No randomness required — the (started_at, routine)
    // tuple is unique per run.
    let lo = started_at as u32;
    let mut h: u32 = 2166136261;
    for b in routine.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16777619);
    }
    format!("{:04x}{:04x}", lo & 0xffff, (h ^ (h >> 16)) & 0xffff)
}

/// Expand `~` and `$ANVIL_CONFIG_HOME` in a user-supplied cwd string.
fn expand_cwd(raw: &str, anvil_home: &Path) -> PathBuf {
    if raw == "~" {
        return dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs_next::home_dir() {
            return home.join(rest);
        }
    }
    if raw.contains("$ANVIL_CONFIG_HOME") {
        return PathBuf::from(
            raw.replace("$ANVIL_CONFIG_HOME", &anvil_home.to_string_lossy()),
        );
    }
    PathBuf::from(raw)
}

/// Read the most-recent `<name>.json` sidecar packet from `<output_root>/<name>/`
/// and return its `summary` + `body`.  Used by the daemon to build
/// [`ContextBlock`]s before calling [`run_once`].  Returns `None` when the
/// routine has never run successfully.
#[must_use]
pub fn latest_context_block(output_root: &Path, routine: &str) -> Option<ContextBlock> {
    let dir = output_root.join(routine);
    let entries = fs::read_dir(&dir).ok()?;
    let mut newest: Option<(String, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = path.file_stem()?.to_str()?.to_string();
        if newest.as_ref().is_none_or(|(s, _)| &stem > s) {
            newest = Some((stem, path));
        }
    }
    let (_, path) = newest?;
    let raw = fs::read_to_string(&path).ok()?;
    let packet: RoutinePacket = serde_json::from_str(&raw).ok()?;
    // Silent / failed packets contribute no useful context.
    if !matches!(packet.status, PacketStatus::Clean) {
        return None;
    }
    Some(ContextBlock {
        routine: routine.to_string(),
        summary: packet.summary,
        body: packet.body,
    })
}

/// Convenience: assemble every `ContextBlock` referenced by a routine's
/// `context_from` field, in declaration order.  Missing upstreams are
/// silently skipped (definitions::validate_no_cycles flagged unknown refs
/// as warnings, not errors).
#[must_use]
pub fn collect_context(output_root: &Path, def: &RoutineDef) -> Vec<ContextBlock> {
    def.context_from
        .iter()
        .filter_map(|r| latest_context_block(output_root, r))
        .collect()
}

/// Sanity helper: routine TOML promised a binary, but the daemon resolves
/// the path once at startup.  This wraps `Path::exists` so the daemon can
/// fail-fast with a helpful error when the user's `anvil` was uninstalled.
pub fn validate_anvil_binary(p: &Path) -> Result<(), String> {
    if !p.exists() {
        return Err(format!("anvil binary not found at {}", p.display()));
    }
    if !p.is_file() {
        return Err(format!("not a regular file: {}", p.display()));
    }
    Ok(())
}

/// Convenience: re-export deliverable target count so /schedule show can
/// quickly summarise without re-walking the definition.
#[must_use]
pub fn delivery_summary(targets: &[DeliveryTarget]) -> String {
    let mut local = 0usize;
    let mut webhooks: Vec<&str> = Vec::new();
    for t in targets {
        match t {
            DeliveryTarget::Local => local += 1,
            DeliveryTarget::Webhook { url, .. } => webhooks.push(url.as_str()),
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if local > 0 {
        parts.push("local archive".into());
    }
    for w in webhooks {
        parts.push(format!("webhook → {w}"));
    }
    if parts.is_empty() {
        "no targets".into()
    } else {
        parts.join(", ")
    }
}

// Silence unused-import warning if a future refactor stops using Write
// while keeping the io prelude.
#[allow(dead_code)]
fn _force_io_link(_w: &mut dyn Write) {}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::definition::{DeliveryTarget, RoutinePermissionMode};
    use crate::routines::schedule::Schedule;

    fn def(name: &str) -> RoutineDef {
        RoutineDef {
            name: name.to_string(),
            schedule: Schedule::Interval(60),
            schedule_raw: "every 1m".into(),
            prompt: "say hi".into(),
            enabled: true,
            model: None,
            permission_mode: RoutinePermissionMode::Auto,
            cwd: None,
            context_from: Vec::new(),
            delivery: vec![DeliveryTarget::Local],
            source_path: PathBuf::from("/tmp/x.toml"),
        }
    }

    #[test]
    fn assemble_prompt_no_context_returns_authored() {
        assert_eq!(assemble_prompt("hi", &[]), "hi");
    }

    #[test]
    fn assemble_prompt_with_context_orders_blocks_first() {
        let ctx = vec![
            ContextBlock {
                routine: "a".into(),
                summary: "asum".into(),
                body: "abody".into(),
            },
            ContextBlock {
                routine: "b".into(),
                summary: String::new(),
                body: "bbody".into(),
            },
        ];
        let out = assemble_prompt("real prompt here", &ctx);
        let a_pos = out.find("## Context From a").unwrap();
        let b_pos = out.find("## Context From b").unwrap();
        let p_pos = out.find("real prompt here").unwrap();
        assert!(a_pos < b_pos);
        assert!(b_pos < p_pos);
        assert!(out.contains("**Summary:** asum"));
        assert!(out.contains("abody"));
        assert!(out.contains("bbody"));
    }

    #[test]
    fn classify_silent_strips_marker() {
        let s = SpawnOutcome {
            stdout: "hello world [SILENT]".into(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            spawn_error: None,
        };
        let (status, body, code, err) = classify_outcome(s);
        assert_eq!(status, RunStatus::Silent);
        assert_eq!(body, "hello world");
        assert_eq!(code, Some(0));
        assert!(err.is_none());
    }

    #[test]
    fn classify_failure_captures_stderr_first_line() {
        let s = SpawnOutcome {
            stdout: String::new(),
            stderr: "Error: provider 500\nbacktrace…".into(),
            exit_code: Some(1),
            timed_out: false,
            spawn_error: None,
        };
        let (status, _, code, err) = classify_outcome(s);
        assert_eq!(status, RunStatus::Failed);
        assert_eq!(code, Some(1));
        assert!(err.unwrap().contains("provider 500"));
    }

    #[test]
    fn classify_timeout_reports_failed_without_exit_code() {
        let s = SpawnOutcome {
            stdout: "partial".into(),
            stderr: String::new(),
            exit_code: None,
            timed_out: true,
            spawn_error: None,
        };
        let (status, body, code, err) = classify_outcome(s);
        assert_eq!(status, RunStatus::Failed);
        assert_eq!(body, "partial");
        assert!(code.is_none());
        assert_eq!(err.as_deref(), Some("subprocess timed out"));
    }

    #[test]
    fn classify_spawn_failure_explains() {
        let s = SpawnOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: false,
            spawn_error: Some("not found".into()),
        };
        let (status, _, _, err) = classify_outcome(s);
        assert_eq!(status, RunStatus::Failed);
        assert!(err.unwrap().contains("not found"));
    }

    #[test]
    fn mint_run_id_is_eight_hex_chars() {
        let id = mint_run_id(1_700_000_000, "release-watch");
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mint_run_id_stable_for_same_inputs() {
        let a = mint_run_id(1_700_000_000, "x");
        let b = mint_run_id(1_700_000_000, "x");
        assert_eq!(a, b);
    }

    #[test]
    fn expand_cwd_tilde() {
        let home = dirs_next::home_dir().unwrap();
        let out = expand_cwd("~", &PathBuf::from("/tmp"));
        assert_eq!(out, home);
        let out = expand_cwd("~/projects", &PathBuf::from("/tmp"));
        assert_eq!(out, home.join("projects"));
    }

    #[test]
    fn expand_cwd_anvil_home_var() {
        let out = expand_cwd(
            "$ANVIL_CONFIG_HOME/routines",
            &PathBuf::from("/tmp/anvil-sandbox"),
        );
        assert_eq!(out, PathBuf::from("/tmp/anvil-sandbox/routines"));
    }

    #[test]
    fn delivery_summary_lists_targets() {
        let s = delivery_summary(&[
            DeliveryTarget::Local,
            DeliveryTarget::Webhook {
                url: "https://x".into(),
                method: "POST".into(),
            },
        ]);
        assert!(s.contains("local archive"));
        assert!(s.contains("webhook → https://x"));
    }

    #[test]
    fn latest_context_block_returns_none_on_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(latest_context_block(tmp.path(), "nonexistent").is_none());
    }

    #[test]
    fn latest_context_block_reads_newest_clean_packet() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        fs::create_dir_all(&dir).unwrap();
        let mk = |stem: &str, status: PacketStatus, body: &str| {
            let p = RoutinePacket {
                routine_id: "r".into(),
                run_id: "abcd0001".into(),
                started_at: 100,
                ended_at: 200,
                status,
                summary: format!("sum for {body}"),
                body: body.into(),
                input_hash: "deadbeef".into(),
            };
            fs::write(
                dir.join(format!("{stem}.json")),
                serde_json::to_string(&p).unwrap(),
            )
            .unwrap();
        };
        mk("20260519T000000Z", PacketStatus::Clean, "older");
        mk("20260519T120000Z", PacketStatus::Clean, "newer");
        let cb = latest_context_block(tmp.path(), "r").unwrap();
        assert_eq!(cb.body, "newer");
    }

    #[test]
    fn latest_context_block_skips_silent_and_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        fs::create_dir_all(&dir).unwrap();
        let p = RoutinePacket {
            routine_id: "r".into(),
            run_id: "abcd0001".into(),
            started_at: 100,
            ended_at: 200,
            status: PacketStatus::Silent,
            summary: String::new(),
            body: "shh".into(),
            input_hash: "deadbeef".into(),
        };
        fs::write(
            dir.join("20260519T000000Z.json"),
            serde_json::to_string(&p).unwrap(),
        )
        .unwrap();
        assert!(latest_context_block(tmp.path(), "r").is_none());
    }

    #[test]
    fn run_once_records_spawn_failure_as_failed_status() {
        // Point at a binary that definitely does not exist.
        let tmp = tempfile::tempdir().unwrap();
        let req = ExecRequest {
            routine: def("nope"),
            anvil_binary: PathBuf::from("/definitely/not/a/binary/zz-anvil"),
            config_home: tmp.path().to_path_buf(),
            anvil_version: "test".into(),
            timeout: Duration::from_secs(2),
            context_blocks: Vec::new(),
        };
        let out = run_once(&req, |_| None);
        assert_eq!(out.status, RunStatus::Failed);
        assert!(out.error.unwrap().contains("failed to spawn"));
        // Delivery dispatch should have at least the Local target marked OK
        // (archive write is best-effort and may have failed too, but the
        // dispatcher always reports the Local target as ok).
        assert!(out.deliveries.iter().any(|d| d.kind == "local"));
    }
}
