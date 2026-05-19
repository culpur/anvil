//! Routine definition loader: parse `~/.anvil/routines/*.toml` files into
//! validated `RoutineDef` values that the executor and `/schedule` slash
//! command can consume.
//!
//! The TOML shape is intentionally small:
//!
//! ```toml
//! # ~/.anvil/routines/release-watch.toml
//! name = "release-watch"
//! schedule = "every 30m"            # any string accepted by parse_schedule
//! prompt = "Check the v2.2.18 release pipeline at /tmp/anvil-release.log. \
//!          If complete, post a summary. Otherwise reply `[SILENT]`."
//! enabled = true                     # default true
//! model = "qwen2.5-coder:7b-instruct-q4_K_M"   # optional
//! permission_mode = "auto"           # optional; one of accept|plan|auto|danger
//! cwd = "~/projects/anvil-dev"       # optional; ~ + $VARS expanded at run time
//!
//! # Optional: pull most-recent output from other routines as context.  Each
//! # entry references the routine `name` field of another file.  Cyclic
//! # references are reported by `validate_no_cycles` at load time.
//! context_from = ["fleet-health"]
//!
//! # Optional: zero or more delivery targets.  See routines::delivery.
//! [[delivery]]
//! kind = "local"                     # always-on local archive; can be omitted
//!
//! [[delivery]]
//! kind = "webhook"
//! url = "vault://routines/release-watch/webhook"   # vault:// or https://
//! method = "POST"                    # default POST
//! ```
//!
//! ## Why TOML, not JSON
//! Routines are user-authored files that benefit from comments and trailing
//! commas. TOML is already in the runtime crate (for settings) so no new
//! dependency. We deliberately do NOT support YAML — multiple delimiters
//! invite trivial parsing differences across implementations.
//!
//! ## Sensitive material
//! Secrets (API tokens, webhook URLs) MUST come from `vault://` references,
//! never inline. The loader does not resolve `vault://` paths itself — the
//! executor and delivery layer handle that with the unlocked vault. This
//! keeps the loader pure and testable without the vault.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::routines::schedule::{parse_schedule, Schedule};

// ─── Wire types (what TOML serde sees) ──────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct RoutineWire {
    name: String,
    schedule: String,
    prompt: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    context_from: Vec<String>,
    #[serde(default)]
    delivery: Vec<DeliveryWire>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum DeliveryWire {
    Local,
    Webhook {
        url: String,
        #[serde(default = "default_webhook_method")]
        method: String,
    },
}

fn default_true() -> bool {
    true
}
fn default_webhook_method() -> String {
    "POST".to_string()
}

// ─── Public, validated types ────────────────────────────────────────────────

/// Permission mode a routine runs under.  Mirrors the values accepted by the
/// `--permission-mode` CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutinePermissionMode {
    /// Approve every tool call interactively. Routines should rarely use this
    /// (there's no human to approve), but the option exists for tests.
    Accept,
    /// Plan-only — read-only tools allowed, edits/bash blocked.
    Plan,
    /// Auto-allow safe operations within the workspace.  Default.
    Auto,
    /// Full access. Routines that touch the network or write outside the
    /// workspace need this — they need vault unlock too.
    Danger,
}

impl Default for RoutinePermissionMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl RoutinePermissionMode {
    /// String form passed to `anvil -p --permission-mode <…>`.
    #[must_use]
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Plan => "plan",
            Self::Auto => "auto",
            Self::Danger => "danger",
        }
    }

    /// Tier derived from the permission mode.  `Plan` and `Auto` are
    /// considered safe enough for the daemon to fire on its own; `Accept`
    /// (every tool needs approval) and `Danger` (full access) both demand
    /// a human pre-approving the run.
    #[must_use]
    pub fn tier(self) -> RoutineTier {
        match self {
            Self::Plan | Self::Auto => RoutineTier::Auto,
            Self::Accept | Self::Danger => RoutineTier::Ask,
        }
    }
}

/// Tier the daemon applies to a routine when its `next_fire` arrives.
///
/// - `Auto` — the routine runs immediately (its permission mode is `plan`
///   or `auto`, both of which the workspace already considers safe for
///   tool execution).
/// - `Ask` — the routine has elevated tool access (`accept` requires
///   per-tool approval; `danger` grants full access). The daemon writes a
///   proposal to `~/.anvil/routines/pending/` and surfaces it in the TUI
///   + remote viewer; the user runs `/schedule approve <name>` to fire it
///   once, or `/schedule reject <name>` to drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutineTier {
    Auto,
    Ask,
}

impl RoutineTier {
    /// Short human-readable label for `/schedule status` and proposal banners.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
        }
    }
}

/// Delivery target for a routine's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DeliveryTarget {
    /// Always-on local archive at `~/.anvil/routines/output/{id}/{ts}.md`.
    /// The archive is also written for every other target (it's not optional
    /// — this entry exists so users can explicitly list it).
    Local,
    /// HTTPS POST to a URL.  The URL may be a literal `https://…` or a
    /// `vault://path/to/secret` reference resolved by the executor at run
    /// time with the unlocked vault.
    Webhook { url: String, method: String },
}

/// A validated routine definition.  Produced by [`load_routine`] and
/// [`load_all`]; consumed by the executor and `/schedule` slash command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutineDef {
    /// Routine identifier (matches the `name = …` field in the TOML file
    /// and the filename without `.toml`).
    pub name: String,
    /// Parsed schedule expression.
    pub schedule: Schedule,
    /// Raw schedule string as written by the user — kept for `/schedule
    /// show` display so the user sees what they typed, not the parsed form.
    pub schedule_raw: String,
    /// Prompt body sent to the model on each run.
    pub prompt: String,
    /// Disabled routines stay loaded so the user can re-enable via
    /// `/schedule enable <name>`, but `next_fire` never schedules them.
    pub enabled: bool,
    /// Optional model override (e.g. `"qwen2.5-coder:7b"`).
    pub model: Option<String>,
    pub permission_mode: RoutinePermissionMode,
    /// Working directory the headless run executes in.  `None` means the
    /// daemon's startup cwd; a value with `~` is expanded to `$HOME` at
    /// run time (NOT at load time, so the file stays portable).
    pub cwd: Option<String>,
    /// Names of other routines whose most-recent packet should be injected
    /// into the prompt as `## Context From <name>` before each run.
    pub context_from: Vec<String>,
    /// Delivery targets.  Always contains at least `DeliveryTarget::Local`
    /// — if the user omits `[[delivery]]` entirely we synthesise one so
    /// downstream code never has to check for an empty Vec.
    pub delivery: Vec<DeliveryTarget>,
    /// On-disk path the definition was loaded from.  Used by `/schedule
    /// show` and reload-on-change watchers.
    pub source_path: PathBuf,
}

// ─── Errors ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LoadError {
    Io(PathBuf, std::io::Error),
    Parse(PathBuf, String),
    Validate(PathBuf, String),
    Cycle(Vec<String>),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
            Self::Parse(p, msg) => write!(f, "{}: {msg}", p.display()),
            Self::Validate(p, msg) => write!(f, "{}: {msg}", p.display()),
            Self::Cycle(chain) => {
                write!(f, "routines have circular context_from references: ")?;
                for (i, n) in chain.iter().enumerate() {
                    if i > 0 {
                        write!(f, " → ")?;
                    }
                    write!(f, "{n}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LoadError {}

// ─── Loaders ────────────────────────────────────────────────────────────────

/// Load a single routine definition from disk.
///
/// Pure validation: parses TOML, validates the schedule expression, normalises
/// the delivery list, validates `permission_mode`.  Does NOT resolve
/// `vault://` URLs or `~` in `cwd` — those happen at run time.
pub fn load_routine(path: &Path) -> Result<RoutineDef, LoadError> {
    let raw = fs::read_to_string(path).map_err(|e| LoadError::Io(path.to_path_buf(), e))?;
    parse_routine_str(&raw, path)
}

/// Parse a TOML string against `path` (used for error messages).  Public so
/// tests can exercise validation without touching the filesystem.
pub fn parse_routine_str(raw: &str, path: &Path) -> Result<RoutineDef, LoadError> {
    let wire: RoutineWire = toml::from_str(raw)
        .map_err(|e| LoadError::Parse(path.to_path_buf(), e.to_string()))?;

    if wire.name.trim().is_empty() {
        return Err(LoadError::Validate(
            path.to_path_buf(),
            "`name` is empty".to_string(),
        ));
    }
    if !is_valid_routine_name(&wire.name) {
        return Err(LoadError::Validate(
            path.to_path_buf(),
            format!(
                "`name = \"{}\"` is not a valid routine identifier (allowed: [a-z0-9_-], 1..=64 chars)",
                wire.name
            ),
        ));
    }
    if wire.prompt.trim().is_empty() {
        return Err(LoadError::Validate(
            path.to_path_buf(),
            "`prompt` is empty".to_string(),
        ));
    }

    let schedule = parse_schedule(&wire.schedule).map_err(|e| {
        LoadError::Validate(path.to_path_buf(), format!("invalid schedule: {e}"))
    })?;

    let permission_mode = match wire.permission_mode.as_deref() {
        None | Some("auto") => RoutinePermissionMode::Auto,
        Some("accept") => RoutinePermissionMode::Accept,
        Some("plan") => RoutinePermissionMode::Plan,
        Some("danger") => RoutinePermissionMode::Danger,
        Some(other) => {
            return Err(LoadError::Validate(
                path.to_path_buf(),
                format!(
                    "`permission_mode = \"{other}\"` invalid (allowed: accept|plan|auto|danger)"
                ),
            ));
        }
    };

    let mut delivery: Vec<DeliveryTarget> = wire
        .delivery
        .into_iter()
        .map(|d| match d {
            DeliveryWire::Local => DeliveryTarget::Local,
            DeliveryWire::Webhook { url, method } => DeliveryTarget::Webhook {
                url,
                method: method.to_uppercase(),
            },
        })
        .collect();
    if !delivery.iter().any(|d| matches!(d, DeliveryTarget::Local)) {
        // Local archive is non-negotiable — synthesise one if absent.
        delivery.insert(0, DeliveryTarget::Local);
    }

    // Validate webhook URLs syntactically (the executor resolves vault://
    // at run time, so we only reject obviously-bad inputs here).
    for d in &delivery {
        if let DeliveryTarget::Webhook { url, method } = d {
            if url.trim().is_empty() {
                return Err(LoadError::Validate(
                    path.to_path_buf(),
                    "webhook delivery has empty `url`".to_string(),
                ));
            }
            if !url.starts_with("https://")
                && !url.starts_with("http://localhost")
                && !url.starts_with("http://127.")
                && !url.starts_with("vault://")
            {
                return Err(LoadError::Validate(
                    path.to_path_buf(),
                    format!(
                        "webhook `url = \"{url}\"` must be https://, http://localhost…, http://127.…, or vault://"
                    ),
                ));
            }
            match method.as_str() {
                "GET" | "POST" | "PUT" | "PATCH" => {}
                other => {
                    return Err(LoadError::Validate(
                        path.to_path_buf(),
                        format!("webhook `method = \"{other}\"` invalid"),
                    ));
                }
            }
        }
    }

    Ok(RoutineDef {
        name: wire.name,
        schedule,
        schedule_raw: wire.schedule,
        prompt: wire.prompt,
        enabled: wire.enabled,
        model: wire.model,
        permission_mode,
        cwd: wire.cwd,
        context_from: wire.context_from,
        delivery,
        source_path: path.to_path_buf(),
    })
}

/// Load every `*.toml` file in `dir`.  Files that fail to parse are returned
/// in the `errors` Vec; this never aborts the whole load — one bad routine
/// must not lock the user out of their other routines (CC parity BUG-34/35
/// applied to routines).  Cycle detection runs once at the end across all
/// successfully-loaded files.
pub fn load_all(dir: &Path) -> LoadAllResult {
    let mut defs: Vec<RoutineDef> = Vec::new();
    let mut errors: Vec<LoadError> = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LoadAllResult { defs, errors };
        }
        Err(e) => {
            errors.push(LoadError::Io(dir.to_path_buf(), e));
            return LoadAllResult { defs, errors };
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_routine(&path) {
            Ok(def) => defs.push(def),
            Err(e) => errors.push(e),
        }
    }

    // Detect duplicate names — same name in two different files is a config
    // error, not "last one wins".
    {
        let mut seen: HashMap<String, PathBuf> = HashMap::new();
        let mut dup_errs: Vec<LoadError> = Vec::new();
        defs.retain(|def| {
            if let Some(existing) = seen.get(&def.name) {
                dup_errs.push(LoadError::Validate(
                    def.source_path.clone(),
                    format!(
                        "duplicate routine name `{}` — already defined at {}",
                        def.name,
                        existing.display()
                    ),
                ));
                false
            } else {
                seen.insert(def.name.clone(), def.source_path.clone());
                true
            }
        });
        errors.extend(dup_errs);
    }

    if let Err(cycle) = validate_no_cycles(&defs) {
        errors.push(LoadError::Cycle(cycle));
    }

    defs.sort_by(|a, b| a.name.cmp(&b.name));
    LoadAllResult { defs, errors }
}

/// Outcome of [`load_all`]: every successfully-loaded routine plus every
/// per-file error encountered.  The caller (typically the daemon or
/// `/schedule list`) renders errors alongside the working routines.
pub struct LoadAllResult {
    pub defs: Vec<RoutineDef>,
    pub errors: Vec<LoadError>,
}

// ─── Cycle detection ─────────────────────────────────────────────────────────

/// Walk every `context_from` edge and reject the load if any cycle exists.
/// Returns the offending chain (names in order, last == first) so the error
/// message can show exactly where the loop closes.
pub fn validate_no_cycles(defs: &[RoutineDef]) -> Result<(), Vec<String>> {
    let by_name: HashMap<&str, &RoutineDef> =
        defs.iter().map(|d| (d.name.as_str(), d)).collect();

    enum Color {
        Gray,
        Black,
    }
    let mut state: HashMap<String, Color> = HashMap::new();

    fn dfs(
        name: &str,
        by_name: &HashMap<&str, &RoutineDef>,
        state: &mut HashMap<String, Color>,
        stack: &mut Vec<String>,
    ) -> Result<(), Vec<String>> {
        if matches!(state.get(name), Some(Color::Black)) {
            return Ok(());
        }
        if matches!(state.get(name), Some(Color::Gray)) {
            // Found a back-edge to a gray node — close the cycle.
            let mut chain: Vec<String> = stack
                .iter()
                .skip_while(|n| n.as_str() != name)
                .cloned()
                .collect();
            chain.push(name.to_string());
            return Err(chain);
        }
        state.insert(name.to_string(), Color::Gray);
        stack.push(name.to_string());
        if let Some(def) = by_name.get(name) {
            for child in &def.context_from {
                // Ignore references to unknown routines — they're a warning
                // (the executor will skip the missing context_from entry),
                // not a cycle.
                if by_name.contains_key(child.as_str()) {
                    dfs(child, by_name, state, stack)?;
                }
            }
        }
        stack.pop();
        state.insert(name.to_string(), Color::Black);
        Ok(())
    }

    for def in defs {
        if !matches!(state.get(&def.name), Some(Color::Black)) {
            dfs(&def.name, &by_name, &mut state, &mut Vec::new())?;
        }
    }
    Ok(())
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn is_valid_routine_name(name: &str) -> bool {
    let len = name.len();
    if !(1..=64).contains(&len) {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Set of routine names referenced by every `context_from` field across the
/// supplied defs.  Useful for `/schedule lint` follow-up work.
#[must_use]
pub fn context_references(defs: &[RoutineDef]) -> HashSet<String> {
    let mut out = HashSet::new();
    for d in defs {
        for c in &d.context_from {
            out.insert(c.clone());
        }
    }
    out
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_path() -> PathBuf {
        PathBuf::from("/tmp/test-routine.toml")
    }

    fn parse(s: &str) -> Result<RoutineDef, LoadError> {
        parse_routine_str(s, &fake_path())
    }

    #[test]
    fn minimal_routine_parses() {
        let raw = r#"
            name = "x"
            schedule = "every 30m"
            prompt = "do the thing"
        "#;
        let def = parse(raw).unwrap();
        assert_eq!(def.name, "x");
        assert_eq!(def.schedule, Schedule::Interval(1800));
        assert_eq!(def.prompt, "do the thing");
        assert!(def.enabled);
        assert_eq!(def.permission_mode, RoutinePermissionMode::Auto);
        // Local archive is always synthesised when not present.
        assert_eq!(def.delivery, vec![DeliveryTarget::Local]);
    }

    #[test]
    fn rejects_empty_name() {
        let raw = r#"
            name = ""
            schedule = "every 30m"
            prompt = "x"
        "#;
        let err = parse(raw).unwrap_err();
        assert!(matches!(err, LoadError::Validate(_, _)));
    }

    #[test]
    fn rejects_invalid_name_chars() {
        let raw = r#"
            name = "BadName"
            schedule = "every 30m"
            prompt = "x"
        "#;
        let err = parse(raw).unwrap_err();
        assert!(matches!(err, LoadError::Validate(_, _)));
    }

    #[test]
    fn rejects_bad_schedule() {
        let raw = r#"
            name = "x"
            schedule = "lol-not-real"
            prompt = "x"
        "#;
        let err = parse(raw).unwrap_err();
        match err {
            LoadError::Validate(_, m) => assert!(m.contains("invalid schedule")),
            _ => panic!("expected Validate"),
        }
    }

    #[test]
    fn permission_modes_round_trip() {
        for mode in ["accept", "plan", "auto", "danger"] {
            let raw = format!(
                r#"
                name = "x"
                schedule = "every 30m"
                prompt = "x"
                permission_mode = "{mode}"
                "#
            );
            let def = parse(&raw).unwrap();
            assert_eq!(def.permission_mode.as_cli_arg(), mode);
        }
    }

    #[test]
    fn permission_mode_tier_mapping() {
        assert_eq!(RoutinePermissionMode::Plan.tier(), RoutineTier::Auto);
        assert_eq!(RoutinePermissionMode::Auto.tier(), RoutineTier::Auto);
        assert_eq!(RoutinePermissionMode::Accept.tier(), RoutineTier::Ask);
        assert_eq!(RoutinePermissionMode::Danger.tier(), RoutineTier::Ask);
    }

    #[test]
    fn webhook_target_parses_and_normalises_method() {
        let raw = r#"
            name = "x"
            schedule = "every 30m"
            prompt = "x"
            [[delivery]]
            kind = "webhook"
            url = "https://example.com/hook"
            method = "post"
        "#;
        let def = parse(raw).unwrap();
        assert!(def
            .delivery
            .iter()
            .any(|d| matches!(d, DeliveryTarget::Webhook { method, .. } if method == "POST")));
        // Local target was inserted at index 0.
        assert_eq!(def.delivery[0], DeliveryTarget::Local);
    }

    #[test]
    fn webhook_rejects_plain_http_to_internet() {
        let raw = r#"
            name = "x"
            schedule = "every 30m"
            prompt = "x"
            [[delivery]]
            kind = "webhook"
            url = "http://example.com/hook"
        "#;
        let err = parse(raw).unwrap_err();
        match err {
            LoadError::Validate(_, m) => assert!(m.contains("https://") || m.contains("localhost")),
            _ => panic!("expected Validate"),
        }
    }

    #[test]
    fn webhook_accepts_vault_url() {
        let raw = r#"
            name = "x"
            schedule = "every 30m"
            prompt = "x"
            [[delivery]]
            kind = "webhook"
            url = "vault://routines/x/webhook"
        "#;
        let def = parse(raw).unwrap();
        assert!(def.delivery.iter().any(|d| matches!(
            d,
            DeliveryTarget::Webhook { url, .. } if url == "vault://routines/x/webhook"
        )));
    }

    #[test]
    fn cycle_detection_two_node_loop() {
        let a = mk_def("a", &["b"]);
        let b = mk_def("b", &["a"]);
        let err = validate_no_cycles(&[a, b]).unwrap_err();
        assert!(err.contains(&"a".to_string()));
        assert!(err.contains(&"b".to_string()));
    }

    #[test]
    fn cycle_detection_ignores_unknown_refs() {
        // `a` references a non-existent routine `ghost`.  That's a warning
        // (executor will skip), not a cycle.
        let a = mk_def("a", &["ghost"]);
        assert!(validate_no_cycles(&[a]).is_ok());
    }

    #[test]
    fn cycle_detection_dag_ok() {
        let a = mk_def("a", &["b", "c"]);
        let b = mk_def("b", &["c"]);
        let c = mk_def("c", &[]);
        assert!(validate_no_cycles(&[a, b, c]).is_ok());
    }

    #[test]
    fn duplicate_names_reported_via_load_all() {
        let dir = tempfile::tempdir().unwrap();
        let body = |n: &str| {
            format!(
                r#"
                name = "{n}"
                schedule = "every 30m"
                prompt = "x"
                "#
            )
        };
        fs::write(dir.path().join("a.toml"), body("dup")).unwrap();
        fs::write(dir.path().join("b.toml"), body("dup")).unwrap();
        let res = load_all(dir.path());
        assert_eq!(res.defs.len(), 1);
        assert!(res
            .errors
            .iter()
            .any(|e| matches!(e, LoadError::Validate(_, m) if m.contains("duplicate"))));
    }

    #[test]
    fn load_all_returns_empty_on_missing_dir() {
        let res = load_all(Path::new("/definitely/does/not/exist/anvil-test-12345"));
        assert!(res.defs.is_empty());
        assert!(res.errors.is_empty());
    }

    fn mk_def(name: &str, ctx_from: &[&str]) -> RoutineDef {
        RoutineDef {
            name: name.to_string(),
            schedule: Schedule::Interval(60),
            schedule_raw: "every 1m".to_string(),
            prompt: "x".to_string(),
            enabled: true,
            model: None,
            permission_mode: RoutinePermissionMode::Auto,
            cwd: None,
            context_from: ctx_from.iter().map(|s| s.to_string()).collect(),
            delivery: vec![DeliveryTarget::Local],
            source_path: PathBuf::from(format!("/tmp/{name}.toml")),
        }
    }
}
