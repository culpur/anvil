//! User-policy persistence for the Ollama tuner.
//!
//! Reads and writes user preferences from `~/.anvil/settings.json` under an
//! `"ollama"` namespace, alongside the harness's other top-level config
//! keys (skills, hooks, mcpServers, etc.). The on-disk shape is:
//!
//! ```json
//! {
//!   "skills": { "..." },
//!   "ollama": {
//!     "policy": { "policy": "balanced", "vram_reserve_gb": 4, "context_min": 8192 },
//!     "models": {
//!       "qwen3:8b": { "num_ctx": 8192, "keep_alive_secs": 600 }
//!     }
//!   }
//! }
//! ```
//!
//! Defaults: `policy = balanced`, `vram_reserve_gb = 4`, `context_min = 8192`
//! (mirrors [`UserPolicy::default`]). Per-model overrides win over the
//! global policy when [`apply_override`] runs against a tuned
//! [`OllamaOptions`].
//!
//! ## Tolerance contract (CC parity BUG-34/35)
//!
//! Every `Deserialize` impl is `#[serde(default)]` so a single bad field
//! cannot destroy an otherwise-valid config. A malformed `"ollama"` value
//! (e.g. a string instead of an object) drops back to [`Default`] and logs
//! a warning to stderr — the rest of the user's settings are preserved.
//!
//! ## Why this lives in `api`, not `runtime`
//!
//! The override APIs reference [`OllamaOptions`] / [`KvCacheType`] from the
//! sibling `tuner` module, which itself depends on
//! [`api::providers::ollama_show::ModelMeta`]. `runtime` cannot depend on
//! `api`, so the tuner anchored here stays here, and the policy layer that
//! wraps it does too.
//!
//! [`apply_override`]: OllamaConfig::apply_override

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::ollama_tune::tuner::{KvCacheType, OllamaOptions, Policy, UserPolicy};

// ─── Wire format ─────────────────────────────────────────────────────────────
//
// `UserPolicy` (in the sibling `tuner` module) doesn't carry
// `#[serde(default)]` on its fields, so a partial `policy` value would fail
// to deserialize whole-cloth. We can't touch the tuner's contract — it's
// owned by the L3 spec — so the on-disk layer goes through these tolerant
// proxy types. CC parity BUG-34/35: a single missing field cannot drop the
// whole config.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct UserPolicyWire {
    policy: Policy,
    vram_reserve_gb: u32,
    context_min: u32,
}

impl Default for UserPolicyWire {
    fn default() -> Self {
        let p = UserPolicy::default();
        Self {
            policy: p.policy,
            vram_reserve_gb: p.vram_reserve_gb,
            context_min: p.context_min,
        }
    }
}

impl From<UserPolicy> for UserPolicyWire {
    fn from(p: UserPolicy) -> Self {
        Self {
            policy: p.policy,
            vram_reserve_gb: p.vram_reserve_gb,
            context_min: p.context_min,
        }
    }
}

impl From<UserPolicyWire> for UserPolicy {
    fn from(w: UserPolicyWire) -> Self {
        Self {
            policy: w.policy,
            vram_reserve_gb: w.vram_reserve_gb,
            context_min: w.context_min,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct OllamaConfigWire {
    policy: UserPolicyWire,
    models: HashMap<String, OllamaModelOverride>,
}

// ─── Public types ────────────────────────────────────────────────────────────

/// Per-model override. Sparse — every field is `Option<_>` so the user only
/// stores knobs they actually want to override; everything else falls
/// through to the tuner's auto-tuned value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaModelOverride {
    pub num_ctx: Option<u32>,
    pub num_gpu: Option<i32>,
    pub num_thread: Option<u32>,
    pub flash_attention: Option<bool>,
    pub kv_cache_type: Option<KvCacheType>,
    pub keep_alive_secs: Option<i64>,
    pub num_batch: Option<u32>,
}

/// Whole config blob persisted under the `"ollama"` key in
/// `~/.anvil/settings.json`. Holds the global [`UserPolicy`] and a map of
/// per-model overrides keyed by Ollama model name (e.g. `"qwen3:8b"`).
///
/// Note: we deliberately do not derive `PartialEq`/`Eq` here — `UserPolicy`
/// (defined in the sibling `tuner` module) doesn't derive them and the
/// tuner contract owns that decision. Tests compare per-field instead.
/// Serde routes through [`OllamaConfigWire`] so per-field tolerance survives
/// the lack of `#[serde(default)]` on `UserPolicy`. `UserPolicy::default()`
/// already gives us `(Balanced, 4 GiB, 8192)`, which propagates here.
#[derive(Debug, Clone, Default)]
pub struct OllamaConfig {
    pub policy: UserPolicy,
    pub models: HashMap<String, OllamaModelOverride>,
}

impl From<OllamaConfigWire> for OllamaConfig {
    fn from(w: OllamaConfigWire) -> Self {
        Self {
            policy: w.policy.into(),
            models: w.models,
        }
    }
}

impl From<&OllamaConfig> for OllamaConfigWire {
    fn from(c: &OllamaConfig) -> Self {
        Self {
            policy: c.policy.clone().into(),
            models: c.models.clone(),
        }
    }
}

/// Errors returned by [`OllamaConfig::save`]. Read-side errors are
/// swallowed and reported via stderr per the tolerance contract above.
#[derive(Debug)]
pub enum OllamaConfigError {
    Read(String),
    Parse(String),
    Write(String),
    NoHome,
}

impl std::fmt::Display for OllamaConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(msg) => write!(f, "failed to read settings.json: {msg}"),
            Self::Parse(msg) => write!(f, "failed to parse settings.json: {msg}"),
            Self::Write(msg) => write!(f, "failed to write settings.json: {msg}"),
            Self::NoHome => write!(
                f,
                "could not resolve config home (set ANVIL_HOME or HOME)"
            ),
        }
    }
}

impl std::error::Error for OllamaConfigError {}

// ─── Public API ──────────────────────────────────────────────────────────────

impl OllamaConfig {
    /// Load the config from `<home>/settings.json`. Never panics; returns
    /// [`Default`] for any of:
    ///   * settings.json doesn't exist
    ///   * settings.json exists but has no `"ollama"` key
    ///   * the `"ollama"` value is malformed (a warning is logged to stderr)
    ///
    /// Per-field tolerance is provided by `#[serde(default)]` on every
    /// nested struct, so a single bad field doesn't drop the whole config.
    #[must_use]
    pub fn load() -> Self {
        let path = Self::settings_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(err) => {
                eprintln!(
                    "[ollama_tune::policy_config] warn: failed to read {}: {err}",
                    path.display()
                );
                return Self::default();
            }
        };

        let root: JsonValue = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                eprintln!(
                    "[ollama_tune::policy_config] warn: settings.json at {} is not valid JSON: {err}",
                    path.display()
                );
                return Self::default();
            }
        };

        let Some(ollama_value) = root.get("ollama") else {
            return Self::default();
        };

        match serde_json::from_value::<OllamaConfigWire>(ollama_value.clone()) {
            Ok(wire) => wire.into(),
            Err(err) => {
                eprintln!(
                    "[ollama_tune::policy_config] warn: malformed \"ollama\" value in {}: {err}; falling back to defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Persist the config to `<home>/settings.json` atomically. Other
    /// top-level keys (skills, hooks, mcpServers, …) are preserved.
    ///
    /// Atomicity: writes to `settings.json.tmp` in the same directory and
    /// then renames into place. Standard same-FS guarantee.
    pub fn save(&self) -> Result<(), OllamaConfigError> {
        let path = Self::settings_path();
        let parent = path
            .parent()
            .ok_or_else(|| OllamaConfigError::Write("settings path has no parent".into()))?
            .to_path_buf();

        if !parent.exists() {
            std::fs::create_dir_all(&parent)
                .map_err(|err| OllamaConfigError::Write(format!("create_dir_all: {err}")))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                if let Err(err) = std::fs::set_permissions(&parent, perms) {
                    eprintln!(
                        "[ollama_tune::policy_config] warn: failed to chmod 0700 {}: {err}",
                        parent.display()
                    );
                }
            }
        }

        // Read existing settings.json into a JSON Value so we don't clobber
        // unrelated top-level keys (skills, hooks, mcpServers, …).
        let mut root = match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<JsonValue>(&s) {
                Ok(JsonValue::Object(_)) => serde_json::from_str(&s).unwrap_or(JsonValue::Object(
                    serde_json::Map::new(),
                )),
                Ok(_) => {
                    eprintln!(
                        "[ollama_tune::policy_config] warn: settings.json at {} is not a JSON object; replacing root",
                        path.display()
                    );
                    JsonValue::Object(serde_json::Map::new())
                }
                Err(err) => {
                    eprintln!(
                        "[ollama_tune::policy_config] warn: settings.json at {} is not valid JSON ({err}); rewriting",
                        path.display()
                    );
                    JsonValue::Object(serde_json::Map::new())
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                JsonValue::Object(serde_json::Map::new())
            }
            Err(err) => {
                return Err(OllamaConfigError::Read(err.to_string()));
            }
        };

        let wire: OllamaConfigWire = self.into();
        let serialized = serde_json::to_value(&wire)
            .map_err(|err| OllamaConfigError::Parse(err.to_string()))?;
        if let Some(obj) = root.as_object_mut() {
            obj.insert("ollama".to_string(), serialized);
        } else {
            // Already coerced to an object above — this branch is defensive.
            let mut map = serde_json::Map::new();
            map.insert("ollama".to_string(), serialized);
            root = JsonValue::Object(map);
        }

        let pretty = serde_json::to_string_pretty(&root)
            .map_err(|err| OllamaConfigError::Parse(err.to_string()))?;

        // Atomic rename: write to <path>.tmp in the same directory, then
        // rename. Same-FS only; that's the standard tempfile pattern.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, pretty.as_bytes())
            .map_err(|err| OllamaConfigError::Write(format!("write tmp: {err}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|err| OllamaConfigError::Write(format!("rename tmp -> settings.json: {err}")))?;
        Ok(())
    }

    /// Return the override block for `model`, or `None` if no override
    /// exists. Lookup is exact-match on model name.
    #[must_use]
    pub fn override_for(&self, model: &str) -> Option<&OllamaModelOverride> {
        self.models.get(model)
    }

    /// Apply per-model overrides to a tuner-produced [`OllamaOptions`].
    /// Pure: no I/O, no clock. Each `Some(_)` field replaces the matching
    /// field on `opts`; `None` fields fall through unchanged.
    #[must_use]
    pub fn apply_override(&self, model: &str, mut opts: OllamaOptions) -> OllamaOptions {
        let Some(ov) = self.models.get(model) else {
            return opts;
        };
        if let Some(v) = ov.num_ctx {
            opts.num_ctx = v;
        }
        if let Some(v) = ov.num_gpu {
            opts.num_gpu = v;
        }
        if let Some(v) = ov.num_thread {
            opts.num_thread = v;
        }
        if let Some(v) = ov.flash_attention {
            opts.flash_attention = v;
        }
        if let Some(v) = ov.kv_cache_type {
            opts.kv_cache_type = v;
        }
        if let Some(v) = ov.keep_alive_secs {
            opts.keep_alive_secs = v;
        }
        if let Some(v) = ov.num_batch {
            opts.num_batch = v;
        }
        opts
    }

    /// Set or merge an override for `model`. The closure receives the
    /// existing override (or a fresh `Default` if none) so callers can
    /// merge new fields without clobbering previously-set ones.
    pub fn set_override<F: FnOnce(&mut OllamaModelOverride)>(&mut self, model: &str, f: F) {
        let entry = self.models.entry(model.to_string()).or_default();
        f(entry);
    }

    /// Remove the entire override block for `model`. No-op if absent.
    pub fn clear_override(&mut self, model: &str) {
        self.models.remove(model);
    }

    /// Resolve `<home>/settings.json`. Resolution order:
    ///
    ///   1. `$ANVIL_HOME` — explicit override (used by tests and CI).
    ///   2. `$ANVIL_CONFIG_HOME` — matches `runtime::config::default_config_home`.
    ///   3. `$HOME/.anvil` — production default.
    ///   4. `./.anvil` — last-ditch fallback (matches runtime).
    ///
    /// NOTE: this duplicates `runtime::config::default_config_home()`. A
    /// future refactor should consolidate; see the L4 review for details.
    fn settings_path() -> PathBuf {
        let home = std::env::var_os("ANVIL_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("ANVIL_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".anvil")))
            .unwrap_or_else(|| PathBuf::from(".anvil"));
        home.join("settings.json")
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(unsafe_code)] // Edition 2024 std::env::{set,remove}_var require unsafe; tests serialise.
mod tests {
    use super::*;
    use crate::ollama_tune::tuner::{KvCacheType, Policy};
    use serde_json::json;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Set `ANVIL_HOME` to `dir` for the duration of a test. Caller must
    /// hold the returned guard until the assertions are done; on drop the
    /// env var is removed. Tests are gated with `#[serial]` because env
    /// vars are process-global and races would corrupt assertions.
    struct EnvGuard;
    impl EnvGuard {
        fn set(dir: &std::path::Path) -> Self {
            // SAFETY: serial_test serialises every test that touches env;
            // this is the only writer to ANVIL_HOME in the api crate.
            unsafe { std::env::set_var("ANVIL_HOME", dir) };
            Self
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::set.
            unsafe { std::env::remove_var("ANVIL_HOME") };
        }
    }

    fn fresh_home() -> (TempDir, EnvGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let guard = EnvGuard::set(dir.path());
        (dir, guard)
    }

    fn write_settings(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("settings.json"), body).expect("write settings.json");
    }

    fn baseline_options() -> OllamaOptions {
        OllamaOptions {
            num_gpu: -1,
            num_ctx: 32_768,
            num_thread: 8,
            flash_attention: true,
            kv_cache_type: KvCacheType::F16,
            low_vram: false,
            main_gpu: 0,
            keep_alive_secs: 300,
            mmap: true,
            num_batch: 512,
        }
    }

    fn assert_is_default(cfg: &OllamaConfig) {
        assert!(matches!(cfg.policy.policy, Policy::Balanced));
        assert_eq!(cfg.policy.vram_reserve_gb, 4);
        assert_eq!(cfg.policy.context_min, 8192);
        assert!(cfg.models.is_empty());
    }

    #[test]
    #[serial]
    fn load_returns_default_when_no_settings_file() {
        let (_dir, _g) = fresh_home();
        let cfg = OllamaConfig::load();
        assert_is_default(&cfg);
    }

    #[test]
    #[serial]
    fn load_returns_default_when_no_ollama_key() {
        let (dir, _g) = fresh_home();
        write_settings(dir.path(), r#"{ "skills": {}, "hooks": {} }"#);
        let cfg = OllamaConfig::load();
        assert_is_default(&cfg);
    }

    #[test]
    #[serial]
    fn load_parses_full_config() {
        let (dir, _g) = fresh_home();
        let body = json!({
            "ollama": {
                "policy": {
                    "policy": "quality",
                    "vram_reserve_gb": 8,
                    "context_min": 16384
                },
                "models": {
                    "qwen3:8b": {
                        "num_ctx": 8192,
                        "kv_cache_type": "q8_0",
                        "keep_alive_secs": 600
                    }
                }
            }
        });
        write_settings(dir.path(), &body.to_string());
        let cfg = OllamaConfig::load();
        assert!(matches!(cfg.policy.policy, Policy::Quality));
        assert_eq!(cfg.policy.vram_reserve_gb, 8);
        assert_eq!(cfg.policy.context_min, 16_384);
        let ov = cfg.override_for("qwen3:8b").expect("override exists");
        assert_eq!(ov.num_ctx, Some(8192));
        assert_eq!(ov.kv_cache_type, Some(KvCacheType::Q8_0));
        assert_eq!(ov.keep_alive_secs, Some(600));
        assert_eq!(ov.num_gpu, None);
    }

    #[test]
    #[serial]
    fn load_handles_missing_optional_fields() {
        let (dir, _g) = fresh_home();
        // Only `policy.policy` present; everything else falls through to
        // the per-field defaults thanks to #[serde(default)].
        write_settings(
            dir.path(),
            r#"{ "ollama": { "policy": { "policy": "speed" } } }"#,
        );
        let cfg = OllamaConfig::load();
        assert!(matches!(cfg.policy.policy, Policy::Speed));
        assert_eq!(cfg.policy.vram_reserve_gb, 4);
        assert_eq!(cfg.policy.context_min, 8192);
        assert!(cfg.models.is_empty());
    }

    #[test]
    #[serial]
    fn load_handles_malformed_value_falls_back_to_default() {
        let (dir, _g) = fresh_home();
        // "ollama" is a string, not an object → should fall back to default
        // and emit a warning to stderr (we don't capture stderr here, but
        // the load contract is what matters for downstream callers).
        write_settings(dir.path(), r#"{ "ollama": "not-an-object" }"#);
        let cfg = OllamaConfig::load();
        assert_is_default(&cfg);
    }

    #[test]
    #[serial]
    fn save_writes_atomic() {
        let (dir, _g) = fresh_home();
        let cfg = OllamaConfig::default();
        cfg.save().expect("save ok");
        // .tmp must not linger after a successful save.
        let tmp = dir.path().join("settings.json.tmp");
        assert!(!tmp.exists(), "stale .tmp at {}", tmp.display());
        assert!(dir.path().join("settings.json").exists());
    }

    #[test]
    #[serial]
    fn save_preserves_other_top_level_keys() {
        let (dir, _g) = fresh_home();
        let pre = json!({
            "skills": { "foo": { "enabled": true } },
            "hooks": { "PreToolUse": [] },
            "mcpServers": {}
        });
        write_settings(dir.path(), &pre.to_string());

        let mut cfg = OllamaConfig::default();
        cfg.policy.policy = Policy::Speed;
        cfg.save().expect("save");

        let raw = std::fs::read_to_string(dir.path().join("settings.json")).expect("read back");
        let v: JsonValue = serde_json::from_str(&raw).expect("parse");
        let obj = v.as_object().expect("object root");
        assert!(obj.contains_key("skills"), "skills preserved");
        assert!(obj.contains_key("hooks"), "hooks preserved");
        assert!(obj.contains_key("mcpServers"), "mcpServers preserved");
        assert_eq!(
            obj.get("skills").unwrap(),
            pre.get("skills").unwrap(),
            "skills unchanged"
        );
        assert_eq!(
            obj.get("hooks").unwrap(),
            pre.get("hooks").unwrap(),
            "hooks unchanged"
        );
        assert!(obj.contains_key("ollama"));
    }

    #[test]
    #[serial]
    fn save_creates_parent_dir() {
        // ANVIL_HOME points at a path that doesn't exist yet; save() must
        // create it (with mode 0o700 on unix). Reuse EnvGuard for cleanup.
        let outer = tempfile::tempdir().expect("tempdir");
        let nested = outer.path().join("not-yet-created/.anvil");
        let _g = EnvGuard::set(&nested);

        assert!(!nested.exists());
        OllamaConfig::default().save().expect("save");
        assert!(nested.exists(), "parent dir created");
        assert!(nested.join("settings.json").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&nested)
                .expect("stat dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "parent dir should be 0700");
        }
    }

    #[test]
    #[serial]
    fn roundtrip_full_config() {
        let (_dir, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.policy.policy = Policy::Quality;
        cfg.policy.vram_reserve_gb = 6;
        cfg.policy.context_min = 4096;
        cfg.set_override("qwen3:8b", |ov| {
            ov.num_ctx = Some(16_384);
            ov.kv_cache_type = Some(KvCacheType::Q8_0);
            ov.keep_alive_secs = Some(900);
        });
        cfg.save().expect("save");

        let loaded = OllamaConfig::load();
        // UserPolicy doesn't derive PartialEq, so compare field-by-field.
        assert!(matches!(loaded.policy.policy, Policy::Quality));
        assert_eq!(loaded.policy.vram_reserve_gb, 6);
        assert_eq!(loaded.policy.context_min, 4096);
        assert_eq!(loaded.models, cfg.models);
        let ov = loaded.override_for("qwen3:8b").expect("override survives");
        assert_eq!(ov.num_ctx, Some(16_384));
        assert_eq!(ov.kv_cache_type, Some(KvCacheType::Q8_0));
        assert_eq!(ov.keep_alive_secs, Some(900));
    }

    #[test]
    #[serial]
    fn apply_override_replaces_only_set_fields() {
        let (_dir, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override("qwen3:8b", |ov| {
            ov.num_ctx = Some(4096);
            ov.kv_cache_type = Some(KvCacheType::Q4_0);
        });
        let opts = cfg.apply_override("qwen3:8b", baseline_options());
        // overridden:
        assert_eq!(opts.num_ctx, 4096);
        assert_eq!(opts.kv_cache_type, KvCacheType::Q4_0);
        // unchanged:
        assert_eq!(opts.num_gpu, -1);
        assert_eq!(opts.num_thread, 8);
        assert!(opts.flash_attention);
        assert_eq!(opts.keep_alive_secs, 300);
        assert_eq!(opts.num_batch, 512);
    }

    #[test]
    #[serial]
    fn apply_override_no_match_returns_unchanged() {
        let (_dir, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override("qwen3:8b", |ov| ov.num_ctx = Some(4096));
        let baseline = baseline_options();
        let opts = cfg.apply_override("llama3:70b", baseline.clone());
        assert_eq!(opts.num_ctx, baseline.num_ctx);
        assert_eq!(opts.num_gpu, baseline.num_gpu);
        assert_eq!(opts.kv_cache_type, baseline.kv_cache_type);
    }

    #[test]
    #[serial]
    fn set_override_merges_existing() {
        let (_dir, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override("qwen3:8b", |ov| ov.num_ctx = Some(8192));
        cfg.set_override("qwen3:8b", |ov| ov.keep_alive_secs = Some(600));
        let ov = cfg.override_for("qwen3:8b").expect("present");
        assert_eq!(ov.num_ctx, Some(8192), "first set survives");
        assert_eq!(ov.keep_alive_secs, Some(600), "second set merged in");
    }

    #[test]
    #[serial]
    fn clear_override_removes_entry() {
        let (_dir, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override("qwen3:8b", |ov| ov.num_ctx = Some(4096));
        assert!(cfg.override_for("qwen3:8b").is_some());
        cfg.clear_override("qwen3:8b");
        assert!(cfg.override_for("qwen3:8b").is_none());
        // Idempotent.
        cfg.clear_override("qwen3:8b");
    }

    #[test]
    #[serial]
    fn policy_serde_lowercase() {
        let s = serde_json::to_string(&Policy::Balanced).expect("ser");
        assert_eq!(s, "\"balanced\"");
        let back: Policy = serde_json::from_str("\"speed\"").expect("deser");
        assert!(matches!(back, Policy::Speed));
    }

    #[test]
    #[serial]
    fn kv_cache_type_serde_snake_case() {
        let s = serde_json::to_string(&KvCacheType::Q8_0).expect("ser");
        assert_eq!(s, "\"q8_0\"");
        let back: KvCacheType = serde_json::from_str("\"q4_0\"").expect("deser");
        assert_eq!(back, KvCacheType::Q4_0);
    }
}
