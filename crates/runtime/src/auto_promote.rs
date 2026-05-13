//! Auto-promote engine — observes file reads, command runs, and stated facts
//! during a session, and automatically seeds the nominations queue when
//! a threshold is crossed.
//!
//! Design intent:
//!   - Auto-promote SUGGESTS, never commits.  Nominations are written to disk
//!     so `/memory promote <id>` is always the user action.
//!   - Counters are reset after each emission so the same item requires
//!     another full threshold cycle to re-nominate.
//!   - The engine is designed to be installed by `main.rs` via a callback;
//!     `file_ops.rs` and `bash.rs` are not hard-wired to it (avoids W11/W12
//!     conflicts).
//!   - Phase 3.3 (SECURITY): when `ANVIL_L5_AUTOROUTE=1`, the body of every
//!     nomination is classified by `vault::scan::classify_learning` before
//!     being persisted. Credentials are REJECTED; infrastructure routes to
//!     `PrivateProjectMemory` when the vault is unlocked; only Knowledge
//!     proceeds down the plaintext nomination path. Without the env var,
//!     behavior is unchanged from prior releases.

// Allow `unsafe` only in test code (env::set_var for ANVIL_L5_AUTOROUTE).
#![cfg_attr(test, allow(unsafe_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::nominations::{NominationCategory, NominationStore};

// ─── Public types ────────────────────────────────────────────────────────────

/// What kind of access was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    /// A file was read by a tool.
    FileRead,
    /// A shell command was executed.
    CommandRun,
    /// A fact was stated/observed in conversation ("I've learned…").
    FactStated,
}

/// A single observation recorded by the auto-promote engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedAccess {
    /// Category of the access.
    pub kind: AccessKind,
    /// Canonical identifier: file path, command string, or fact text.
    pub key: String,
    /// Unix timestamp (seconds) when the access occurred.
    pub timestamp: u64,
}

impl ObservedAccess {
    /// Construct a new `ObservedAccess` using the current wall-clock time.
    #[must_use]
    pub fn now(kind: AccessKind, key: impl Into<String>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self { kind, key: key.into(), timestamp }
    }
}

/// Aggregate stats returned by [`AutoPromoter::stats`].
#[derive(Debug, Clone, Default)]
pub struct AutoPromoterStats {
    /// How many file-read observations have been recorded this session.
    pub file_reads_observed: u64,
    /// How many command-run observations have been recorded this session.
    pub command_runs_observed: u64,
    /// How many fact-stated observations have been recorded this session.
    pub facts_stated_observed: u64,
    /// How many nominations were auto-emitted this session.
    pub nominations_emitted: u64,
}

/// Errors that can occur in the auto-promote engine.
#[derive(Debug)]
pub enum AutoPromoteError {
    /// A nomination could not be persisted to disk.
    NominationStore(std::io::Error),
    /// Phase 3.3: content was classified as a credential and was refused.
    /// The body was NEVER written to disk and is NEVER echoed to stderr
    /// or this error variant — the variant carries no payload.
    CredentialRejected,
    /// Phase 3.3: content was classified as infrastructure but the vault
    /// is locked, so it could not be encrypted. Suppressed silently.
    VaultLocked,
}

impl std::fmt::Display for AutoPromoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NominationStore(e) => write!(f, "nomination store error: {e}"),
            Self::CredentialRejected => {
                write!(f, "nomination rejected: credential pattern detected")
            }
            Self::VaultLocked => write!(f, "infrastructure detail suppressed: vault locked"),
        }
    }
}

impl std::error::Error for AutoPromoteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NominationStore(e) => Some(e),
            Self::CredentialRejected | Self::VaultLocked => None,
        }
    }
}

// ─── AutoPromoter ────────────────────────────────────────────────────────────

/// Session-scoped engine that watches access patterns and seeds nominations.
pub struct AutoPromoter {
    /// Number of file reads required before emitting a nomination.
    pub threshold_file_reads: u32,
    /// Number of command runs required before emitting a nomination.
    pub threshold_command_runs: u32,
    /// Number of fact repetitions required before emitting a nomination.
    pub threshold_fact_repeats: u32,

    /// Directory where nomination JSON files are written.
    nominations_dir: PathBuf,

    /// Per-key access counters for the current session.
    file_read_counts: HashMap<String, u32>,
    command_run_counts: HashMap<String, u32>,
    fact_stated_counts: HashMap<String, u32>,

    /// Cumulative stats for this session.
    stats: AutoPromoterStats,
}

impl AutoPromoter {
    /// Create an `AutoPromoter` with default thresholds.
    ///
    /// Default thresholds: 3 file reads, 3 command runs, 2 fact repeats.
    #[must_use]
    pub fn new(nominations_dir: PathBuf) -> Self {
        Self {
            threshold_file_reads: 3,
            threshold_command_runs: 3,
            threshold_fact_repeats: 2,
            nominations_dir,
            file_read_counts: HashMap::new(),
            command_run_counts: HashMap::new(),
            fact_stated_counts: HashMap::new(),
            stats: AutoPromoterStats::default(),
        }
    }

    /// Record an observation.
    ///
    /// Internally increments the counter for `access.key`.  If the relevant
    /// threshold is crossed, a nomination is written to disk and `Some(id)` is
    /// returned.  The counter is reset after emission so the same key needs
    /// to re-accumulate before a second nomination is created.
    ///
    /// # Errors
    ///
    /// Returns `AutoPromoteError::NominationStore` if disk persistence fails.
    pub fn record(
        &mut self,
        access: ObservedAccess,
    ) -> Result<Option<String>, AutoPromoteError> {
        match access.kind {
            AccessKind::FileRead => {
                let key = normalize_path(&access.key);
                self.stats.file_reads_observed += 1;
                let count = self.file_read_counts.entry(key.clone()).or_insert(0);
                *count += 1;
                if *count >= self.threshold_file_reads {
                    *count = 0; // reset
                    let id = self.emit_nomination(
                        NominationCategory::Pattern,
                        &format!("File read {threshold}+ times in session: {key}", threshold = self.threshold_file_reads),
                    )?;
                    self.stats.nominations_emitted += 1;
                    return Ok(Some(id));
                }
            }
            AccessKind::CommandRun => {
                let key = normalize_command(&access.key);
                self.stats.command_runs_observed += 1;
                let count = self.command_run_counts.entry(key.clone()).or_insert(0);
                *count += 1;
                if *count >= self.threshold_command_runs {
                    *count = 0;
                    let id = self.emit_nomination(
                        NominationCategory::Workflow,
                        &format!("Command run {threshold}+ times in session: {key}", threshold = self.threshold_command_runs),
                    )?;
                    self.stats.nominations_emitted += 1;
                    return Ok(Some(id));
                }
            }
            AccessKind::FactStated => {
                let key = access.key.trim().to_string();
                self.stats.facts_stated_observed += 1;
                let count = self.fact_stated_counts.entry(key.clone()).or_insert(0);
                *count += 1;
                if *count >= self.threshold_fact_repeats {
                    *count = 0;
                    let id = self.emit_nomination(
                        NominationCategory::Convention,
                        &format!("Fact repeated {threshold}+ times: {key}", threshold = self.threshold_fact_repeats),
                    )?;
                    self.stats.nominations_emitted += 1;
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }

    /// Return a snapshot of cumulative stats for this session.
    #[must_use]
    pub fn stats(&self) -> AutoPromoterStats {
        self.stats.clone()
    }

    /// Reset all counters (e.g. on session end or when starting a new
    /// observation window).  Does not affect already-emitted nominations.
    pub fn reset(&mut self) {
        self.file_read_counts.clear();
        self.command_run_counts.clear();
        self.fact_stated_counts.clear();
    }

    // ─── Internal helpers ─────────────────────────────────────────────────────

    fn emit_nomination(
        &self,
        category: NominationCategory,
        content: &str,
    ) -> Result<String, AutoPromoteError> {
        // Phase 3.3 (SECURITY): classify the body before persisting. Without
        // this gate, an API key the model echoed during the session could
        // end up plaintext in ~/.anvil/nominations/*.json. Routing:
        //
        //   Credential     -> REJECT (never write nomination, suppress echo)
        //   Infrastructure -> route to PrivateProjectMemory if vault unlocked;
        //                     suppress otherwise
        //   Knowledge      -> normal nomination path
        //
        // The classify_learning routing is gated behind ANVIL_L5_AUTOROUTE=1
        // for the first release cycle. Default OFF keeps behavior unchanged
        // while the user audits the routing in a real session.
        if std::env::var("ANVIL_L5_AUTOROUTE").map(|v| v == "1").unwrap_or(false) {
            match crate::vault::scan::classify_learning(content) {
                crate::vault::scan::SensitivityLevel::Credential => {
                    // Log the rejection WITHOUT echoing the content. Even
                    // truncated previews can leak secrets.
                    eprintln!(
                        "[warn] Nomination rejected: detected credential pattern. \
                         Use /vault store to record secrets explicitly."
                    );
                    return Err(AutoPromoteError::CredentialRejected);
                }
                crate::vault::scan::SensitivityLevel::Infrastructure => {
                    return route_infrastructure_to_private_memory(content);
                }
                crate::vault::scan::SensitivityLevel::Knowledge => {
                    // Fall through to the normal nomination path.
                }
            }
        }

        let store = NominationStore::with_dir(self.nominations_dir.clone());
        store
            .ensure_dir()
            .map_err(AutoPromoteError::NominationStore)?;
        let nom = store
            .create("auto-promote", category, content, 0.7)
            .map_err(AutoPromoteError::NominationStore)?;
        Ok(nom.id)
    }
}

/// Route infrastructure content to encrypted private project memory when the
/// vault is unlocked. When the vault is locked, suppress the nomination and
/// emit a one-line banner so the user knows to unlock and capture explicitly.
///
/// Never echoes the content to stderr or stdout — the body is treated as
/// sensitive and visible only through `/memory show identity` after unlock.
fn route_infrastructure_to_private_memory(
    content: &str,
) -> Result<String, AutoPromoteError> {
    use crate::vault_session::{vault_is_session_unlocked, vault_session_upsert_private_memory};

    if !vault_is_session_unlocked() {
        eprintln!(
            "[info] Infrastructure detail not recorded — vault is locked. \
             Unlock with /vault unlock to capture."
        );
        return Err(AutoPromoteError::VaultLocked);
    }

    // Use a stable timestamp-derived key so successive infra facts don't
    // overwrite each other. The full text is the value; the key is just
    // a coarse handle for /memory show identity listings.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("auto-promote-infra-{now}");

    let cwd =
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    match vault_session_upsert_private_memory(&cwd, &key, content) {
        Ok(()) => Ok(format!("private:{key}")),
        Err(_) => Err(AutoPromoteError::VaultLocked),
    }
}

// ─── Global engine (process-scoped) ──────────────────────────────────────────
//
// W15b wiring: main.rs installs an `AutoPromoter` once at startup and the
// runtime hot paths (read_file, bash) record observations via the
// best-effort helpers below. Recording errors are swallowed so a broken
// engine cannot kill a tool call.

use std::sync::Mutex;
use std::sync::OnceLock;

static GLOBAL: OnceLock<Mutex<AutoPromoter>> = OnceLock::new();

/// Install the global auto-promoter exactly once.  Subsequent calls are
/// no-ops, so callers can safely call this from multiple entry points
/// (e.g. main.rs init + integration test setup).
pub fn install_global(promoter: AutoPromoter) {
    let _ = GLOBAL.set(Mutex::new(promoter));
}

/// Convenience: install the engine pointed at the default
/// `~/.anvil/nominations/` directory with default thresholds.  No-op if
/// the engine is already installed.
pub fn install_default() {
    if is_installed() {
        return;
    }
    let dir = crate::config::default_config_home().join("nominations");
    install_global(AutoPromoter::new(dir));
}

/// `true` iff `install_global` has been called.
#[must_use]
pub fn is_installed() -> bool {
    GLOBAL.get().is_some()
}

/// Best-effort observation recording.  Silent on poisoned mutex, missing
/// engine, or disk failure — the engine is purely advisory.
pub fn observe(kind: AccessKind, key: impl Into<String>) {
    let Some(cell) = GLOBAL.get() else {
        return;
    };
    let Ok(mut guard) = cell.lock() else {
        return;
    };
    let _ = guard.record(ObservedAccess::now(kind, key));
}

/// Read-only access to the engine's stats. Returns `None` when the
/// engine isn't installed.
#[must_use]
pub fn stats() -> Option<AutoPromoterStats> {
    let cell = GLOBAL.get()?;
    let guard = cell.lock().ok()?;
    Some(guard.stats())
}

// ─── Normalisation helpers ────────────────────────────────────────────────────

/// Canonicalise a file path key.
///
/// Resolves `..` and `.` segments using `Path::components`, strips a trailing
/// slash, and lower-cases nothing (paths are case-sensitive on most systems).
fn normalize_path(raw: &str) -> String {
    let path = Path::new(raw);
    // Use `canonicalize` only if the path exists; otherwise clean it manually.
    if let Ok(canonical) = path.canonicalize() {
        return canonical.to_string_lossy().into_owned();
    }
    // Manual clean: collapse `.` and `..` without hitting the filesystem.
    let mut components: Vec<&str> = Vec::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop();
            }
            other => {
                let s = other.as_os_str().to_str().unwrap_or("");
                if !s.is_empty() {
                    components.push(s);
                }
            }
        }
    }
    let joined = components.join("/");
    if raw.starts_with('/') { format!("/{joined}") } else { joined }
}

/// Normalise a shell command for counting purposes.
///
/// Collapses internal whitespace to single spaces and trims leading/trailing
/// whitespace so that `"  git status  "` and `"git status"` count as the same
/// command.
fn normalize_command(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_nominations_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("anvil-ap-test-{nanos}-{seq}"));
        std::fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    fn make_promoter() -> AutoPromoter {
        AutoPromoter::new(tmp_nominations_dir())
    }

    // ── threshold tests ───────────────────────────────────────────────────────

    #[test]
    fn auto_promoter_records_file_read_below_threshold_does_not_emit() {
        let mut ap = make_promoter();
        // Threshold is 3; recording 2 should not emit.
        for _ in 0..2 {
            let result = ap.record(ObservedAccess::now(AccessKind::FileRead, "/tmp/foo.rs")).unwrap();
            assert!(result.is_none(), "should not emit below threshold");
        }
    }

    #[test]
    fn auto_promoter_records_file_read_at_threshold_emits_nomination() {
        let mut ap = make_promoter();
        let path = "/tmp/bar.rs";
        let mut emitted_id: Option<String> = None;
        for i in 0..3 {
            let result = ap.record(ObservedAccess::now(AccessKind::FileRead, path)).unwrap();
            if i == 2 {
                emitted_id = result;
            } else {
                assert!(result.is_none());
            }
        }
        assert!(emitted_id.is_some(), "should emit on 3rd read");
        // Verify nomination exists on disk.
        let id = emitted_id.unwrap();
        let nom_file = ap.nominations_dir.join(format!("{id}.json"));
        assert!(nom_file.exists(), "nomination file should exist at {nom_file:?}");
    }

    #[test]
    fn auto_promoter_records_command_run_at_threshold_emits_nomination() {
        let mut ap = make_promoter();
        let cmd = "cargo test";
        let mut last = None;
        for _ in 0..3 {
            last = ap.record(ObservedAccess::now(AccessKind::CommandRun, cmd)).unwrap();
        }
        assert!(last.is_some(), "should emit after 3 command runs");
    }

    #[test]
    fn auto_promoter_records_fact_stated_at_threshold_2_emits() {
        let mut ap = make_promoter();
        let fact = "Deploy uses git pull only";
        let first = ap.record(ObservedAccess::now(AccessKind::FactStated, fact)).unwrap();
        assert!(first.is_none(), "should not emit on first fact");
        let second = ap.record(ObservedAccess::now(AccessKind::FactStated, fact)).unwrap();
        assert!(second.is_some(), "should emit on second fact");
    }

    #[test]
    fn auto_promoter_threshold_resets_after_emit() {
        let mut ap = make_promoter();
        let path = "/tmp/reset_test.rs";
        // Cross threshold once.
        for _ in 0..3 {
            ap.record(ObservedAccess::now(AccessKind::FileRead, path)).unwrap();
        }
        // Counter should be reset; next 2 reads should not emit.
        for _ in 0..2 {
            let r = ap.record(ObservedAccess::now(AccessKind::FileRead, path)).unwrap();
            assert!(r.is_none(), "counter should have reset after emit");
        }
    }

    #[test]
    fn auto_promoter_stats_returns_correct_counts() {
        let mut ap = make_promoter();
        ap.record(ObservedAccess::now(AccessKind::FileRead, "/a.rs")).unwrap();
        ap.record(ObservedAccess::now(AccessKind::FileRead, "/b.rs")).unwrap();
        ap.record(ObservedAccess::now(AccessKind::CommandRun, "cargo build")).unwrap();
        ap.record(ObservedAccess::now(AccessKind::FactStated, "some fact")).unwrap();

        let stats = ap.stats();
        assert_eq!(stats.file_reads_observed, 2);
        assert_eq!(stats.command_runs_observed, 1);
        assert_eq!(stats.facts_stated_observed, 1);
        assert_eq!(stats.nominations_emitted, 0);
    }

    #[test]
    fn auto_promoter_reset_clears_counters() {
        let mut ap = make_promoter();
        ap.record(ObservedAccess::now(AccessKind::FileRead, "/a.rs")).unwrap();
        ap.record(ObservedAccess::now(AccessKind::FileRead, "/a.rs")).unwrap();
        ap.reset();
        // After reset, two more reads should still not emit (counter starts fresh).
        for _ in 0..2 {
            let r = ap.record(ObservedAccess::now(AccessKind::FileRead, "/a.rs")).unwrap();
            assert!(r.is_none(), "post-reset reads should not emit below threshold");
        }
    }

    #[test]
    fn auto_promoter_normalizes_file_paths_canonical() {
        let mut ap = make_promoter();
        // Both forms should count as the same key.
        ap.record(ObservedAccess::now(AccessKind::FileRead, "/tmp/./foo.rs")).unwrap();
        ap.record(ObservedAccess::now(AccessKind::FileRead, "/tmp/foo.rs")).unwrap();
        let third = ap.record(ObservedAccess::now(AccessKind::FileRead, "/tmp/foo.rs")).unwrap();
        assert!(third.is_some(), "normalised paths should accumulate together");
    }

    #[test]
    fn auto_promoter_normalizes_command_whitespace() {
        let mut ap = make_promoter();
        // Commands with extra whitespace should count as the same key.
        ap.record(ObservedAccess::now(AccessKind::CommandRun, "  git  status  ")).unwrap();
        ap.record(ObservedAccess::now(AccessKind::CommandRun, "git status")).unwrap();
        let third = ap.record(ObservedAccess::now(AccessKind::CommandRun, "git  status")).unwrap();
        assert!(third.is_some(), "normalised commands should accumulate together");
    }

    #[test]
    fn observe_is_silent_when_global_not_installed() {
        // We cannot install/uninstall a OnceLock-backed global, so this
        // test only asserts that calling `observe` and `stats` doesn't
        // panic when the engine is absent or present.
        super::observe(AccessKind::CommandRun, "ls");
        // stats may be Some or None depending on test ordering; just
        // ensure no panic.
        let _ = super::stats();
    }

    // ── Phase 3.3 SECURITY: classify_learning routing ─────────────────────────

    /// Process-local lock for L5_AUTOROUTE env-mutating tests.
    fn autoroute_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct AutorouteGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl AutorouteGuard {
        fn enabled() -> Self {
            let lock = autoroute_lock();
            let prev = std::env::var_os("ANVIL_L5_AUTOROUTE");
            unsafe { std::env::set_var("ANVIL_L5_AUTOROUTE", "1"); }
            Self { _lock: lock, prev }
        }
        fn disabled() -> Self {
            let lock = autoroute_lock();
            let prev = std::env::var_os("ANVIL_L5_AUTOROUTE");
            unsafe { std::env::remove_var("ANVIL_L5_AUTOROUTE"); }
            Self { _lock: lock, prev }
        }
    }

    impl Drop for AutorouteGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("ANVIL_L5_AUTOROUTE", v); },
                None => unsafe { std::env::remove_var("ANVIL_L5_AUTOROUTE"); },
            }
        }
    }

    #[test]
    fn emit_credential_is_rejected_when_autoroute_enabled() {
        let _g = AutorouteGuard::enabled();
        let dir = tmp_nominations_dir();
        let ap = AutoPromoter::new(dir.clone());

        // Use a string that matches `sk-ant-` prefix in classify_learning.
        let result = ap.emit_nomination(
            NominationCategory::Pattern,
            "key is sk-ant-api03-thiswouldbearealkey",
        );
        assert!(
            matches!(result, Err(AutoPromoteError::CredentialRejected)),
            "credential bodies must be rejected; got: {result:?}"
        );

        // No JSON files must exist in the nominations dir.
        let count = std::fs::read_dir(&dir)
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or(0);
        assert_eq!(count, 0, "no nomination file should have been written");
    }

    #[test]
    fn emit_knowledge_proceeds_normally_when_autoroute_enabled() {
        let _g = AutorouteGuard::enabled();
        let dir = tmp_nominations_dir();
        let ap = AutoPromoter::new(dir.clone());

        let result = ap.emit_nomination(
            NominationCategory::Convention,
            "Tests live in __tests__/",
        );
        assert!(result.is_ok(), "knowledge content must pass through; got: {result:?}");
        let id = result.unwrap();
        let path = dir.join(format!("{id}.json"));
        assert!(path.exists(), "nomination file must be on disk at {path:?}");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("__tests__"));
    }

    #[test]
    fn emit_credential_proceeds_when_autoroute_disabled_default() {
        let _g = AutorouteGuard::disabled();
        let dir = tmp_nominations_dir();
        let ap = AutoPromoter::new(dir.clone());

        // SECURITY ROLLOUT NOTE: with the env var OFF (default), the legacy
        // plaintext path is preserved. The test asserts the OLD behavior
        // still works so the rollout is safe — users who haven't opted in
        // see no change. Phase 4 will flip the default and remove this test.
        let result = ap.emit_nomination(
            NominationCategory::Pattern,
            "key is sk-ant-api03-thiswouldbearealkey",
        );
        assert!(result.is_ok(), "default-off must preserve legacy path");
    }

    #[test]
    fn emit_infrastructure_with_locked_vault_suppresses_when_autoroute_enabled() {
        let _g = AutorouteGuard::enabled();
        let dir = tmp_nominations_dir();
        let ap = AutoPromoter::new(dir.clone());

        // 10.0.70.80 hits the IPv4 infrastructure detector. The vault is
        // not initialised in this test process, so we expect VaultLocked.
        let result = ap.emit_nomination(
            NominationCategory::Pattern,
            "Deploy to 10.0.70.80 via ssh",
        );
        // VaultLocked is the safe-failure: nothing on disk, nothing in private mem.
        assert!(
            matches!(result, Err(AutoPromoteError::VaultLocked)),
            "infra + locked vault must suppress; got: {result:?}"
        );

        let count = std::fs::read_dir(&dir)
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or(0);
        assert_eq!(count, 0, "no nomination file should have been written");
    }
}
