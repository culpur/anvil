//! `/schedule` and `/daemon` slash-command handlers (v2.2.18 #657 D5).
//!
//! Both commands read live state from disk (the daemon writes
//! `~/.anvil/run/anvild.status.json` every 30 s) and the routines TOML
//! directory.  No IPC required — anvild is a separate OS process and these
//! commands operate on the shared filesystem.
//!
//! ## What's here
//!
//! - `/schedule list`              — table of every loaded routine
//! - `/schedule show <name>`       — full detail card for one routine
//! - `/schedule run-now <name>`    — execute a routine inline via the executor
//! - `/schedule status`            — daemon + per-routine next-fire snapshot
//! - `/schedule enable <name>`     — flip `enabled = true` in the TOML
//! - `/schedule disable <name>`    — flip `enabled = false` in the TOML
//!
//! - `/daemon status`              — mirror of `anvil daemon status`
//! - `/daemon start`               — spawn detached anvild
//! - `/daemon stop`                — SIGTERM the running anvild
//! - `/daemon install-service`     — generate per-platform service unit
//! - `/daemon uninstall-service`   — remove the service unit
//!
//! ## Output contract
//!
//! Every handler returns a `String` ready to push into the TUI scrollback.
//! No `println!` ever (`feedback-tui-stdout-anti-pattern`).  Long output is
//! truncated with a footer pointing the user at the archive path so they
//! can `cat` it later from outside the TUI.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime::routines::definition::{
    load_all, DeliveryTarget, LoadAllResult, RoutineDef, RoutinePermissionMode,
};
use runtime::routines::executor::{
    collect_context, delivery_summary, run_once, validate_anvil_binary, ExecRequest,
};
use runtime::routines::schedule::next_fire;
use serde::Deserialize;

// ─── Entry points ───────────────────────────────────────────────────────────

/// `/schedule <args>` handler.  Reads from disk; returns a rendered string.
pub fn run_schedule_command(args: Option<&str>) -> String {
    let home = anvil_home();
    let routines_dir = home.join("routines");
    let sub_raw = args.unwrap_or("").trim();
    let mut parts = sub_raw.split_whitespace();
    let sub = parts.next().unwrap_or("list");
    let rest: Vec<&str> = parts.collect();

    match sub {
        "" | "list" | "ls" => render_list(&routines_dir),
        "show" | "info" => match rest.first() {
            Some(name) => render_show(&routines_dir, &home, name),
            None => "/schedule show: missing <name>. Usage: /schedule show <routine>".into(),
        },
        "run-now" | "run" => match rest.first() {
            Some(name) => render_run_now(&routines_dir, &home, name),
            None => "/schedule run-now: missing <name>. Usage: /schedule run-now <routine>".into(),
        },
        "status" => render_status(&home, &routines_dir),
        "enable" => match rest.first() {
            Some(name) => render_toggle(&routines_dir, name, true),
            None => "/schedule enable: missing <name>".into(),
        },
        "disable" => match rest.first() {
            Some(name) => render_toggle(&routines_dir, name, false),
            None => "/schedule disable: missing <name>".into(),
        },
        other => format!(
            "/schedule: unknown subcommand `{other}` (try: list | show <name> | run-now <name> | status | enable <name> | disable <name>)"
        ),
    }
}

/// `/daemon <args>` handler.  Shells out to `anvil daemon …` via the user's
/// own binary so the slash command and the headless subcommand share one code
/// path.  Returns the captured stdout/stderr.
pub fn run_daemon_command(args: Option<&str>) -> String {
    let sub = args.unwrap_or("").trim();
    let sub = if sub.is_empty() { "status" } else { sub };

    let anvil_binary = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("anvil"));

    // Split sub on whitespace for argv.
    let argv: Vec<&str> = sub.split_whitespace().collect();
    if !matches!(
        argv.first().copied(),
        Some("start" | "stop" | "status" | "foreground" | "install-service" | "uninstall-service")
    ) {
        return format!(
            "/daemon: unknown subcommand `{sub}` (try: start | stop | status | install-service | uninstall-service)"
        );
    }

    let mut cmd = std::process::Command::new(&anvil_binary);
    cmd.arg("daemon");
    for a in &argv {
        cmd.arg(a);
    }
    match cmd.output() {
        Ok(o) => {
            let mut out = String::new();
            if !o.stdout.is_empty() {
                out.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            if !o.stderr.is_empty() {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if out.trim().is_empty() {
                format!("anvil daemon {sub}: (exited code {})", o.status.code().unwrap_or(-1))
            } else {
                out.trim_end().to_string()
            }
        }
        Err(e) => format!(
            "/daemon: failed to invoke `{} daemon {sub}`: {e}",
            anvil_binary.display()
        ),
    }
}

// ─── /schedule list ─────────────────────────────────────────────────────────

fn render_list(routines_dir: &Path) -> String {
    let LoadAllResult { defs, errors } = load_all(routines_dir);
    if defs.is_empty() && errors.is_empty() {
        return format!(
            "/schedule list: no routines installed.\n\nCreate one at {}/<name>.toml:\n\n  name = \"my-routine\"\n  schedule = \"every 30m\"\n  prompt = \"check things\"\n",
            routines_dir.display()
        );
    }
    let now = unix_now();
    let mut out = String::new();
    out.push_str("ROUTINES\n");
    out.push_str("--------\n");
    for d in &defs {
        let next = if d.enabled {
            next_fire(&d.schedule, now)
                .map(|t| format!("in {}", fmt_relative(t.saturating_sub(now))))
                .unwrap_or_else(|| "one-shot consumed".into())
        } else {
            "disabled".into()
        };
        let dot = if d.enabled { "●" } else { "○" };
        out.push_str(&format!(
            "  {dot} {:<24}  {:<18}  next: {}\n",
            d.name, d.schedule_raw, next
        ));
    }
    if !errors.is_empty() {
        out.push_str("\nLOAD ERRORS\n");
        out.push_str("-----------\n");
        for e in errors.iter().take(8) {
            out.push_str(&format!("  {e}\n"));
        }
        if errors.len() > 8 {
            out.push_str(&format!("  …and {} more\n", errors.len() - 8));
        }
    }
    out.push_str("\nTry: /schedule show <name>  ·  /schedule run-now <name>  ·  /schedule status\n");
    out
}

// ─── /schedule show ─────────────────────────────────────────────────────────

fn render_show(routines_dir: &Path, home: &Path, name: &str) -> String {
    let LoadAllResult { defs, .. } = load_all(routines_dir);
    let Some(def) = defs.iter().find(|d| d.name == name) else {
        return format!("/schedule show: no routine named `{name}` (try /schedule list)");
    };
    let mut out = String::new();
    out.push_str(&format!("ROUTINE: {}\n", def.name));
    out.push_str(&"-".repeat(8 + def.name.len()));
    out.push('\n');
    out.push_str(&format!(
        "  enabled:        {}\n",
        if def.enabled { "yes" } else { "no" }
    ));
    out.push_str(&format!("  schedule:       {}\n", def.schedule_raw));
    out.push_str(&format!(
        "  permission:     {}\n",
        def.permission_mode.as_cli_arg()
    ));
    if let Some(model) = &def.model {
        out.push_str(&format!("  model:          {model}\n"));
    }
    if let Some(cwd) = &def.cwd {
        out.push_str(&format!("  cwd:            {cwd}\n"));
    }
    if !def.context_from.is_empty() {
        out.push_str(&format!(
            "  context_from:   {}\n",
            def.context_from.join(", ")
        ));
    }
    out.push_str(&format!(
        "  delivery:       {}\n",
        delivery_summary(&def.delivery)
    ));
    out.push_str(&format!(
        "  source:         {}\n",
        def.source_path.display()
    ));
    out.push_str("\nPROMPT\n------\n");
    let prompt = def.prompt.trim();
    let preview: String = prompt.lines().take(30).collect::<Vec<_>>().join("\n");
    out.push_str(&preview);
    if prompt.lines().count() > 30 {
        out.push_str(&format!(
            "\n…{} more lines (cat {} for full prompt)",
            prompt.lines().count() - 30,
            def.source_path.display()
        ));
    }
    let _ = home; // reserved for future "most recent archive" inline preview
    out.push('\n');
    out
}

// ─── /schedule run-now ──────────────────────────────────────────────────────

fn render_run_now(routines_dir: &Path, home: &Path, name: &str) -> String {
    let LoadAllResult { defs, .. } = load_all(routines_dir);
    let Some(def) = defs.iter().find(|d| d.name == name).cloned() else {
        return format!("/schedule run-now: no routine named `{name}`");
    };
    let anvil_binary = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("anvil"));
    if let Err(e) = validate_anvil_binary(&anvil_binary) {
        return format!("/schedule run-now: {e}");
    }
    let req = ExecRequest {
        routine: def.clone(),
        anvil_binary,
        config_home: home.to_path_buf(),
        anvil_version: env!("CARGO_PKG_VERSION").to_string(),
        timeout: Duration::from_secs(300),
        context_blocks: collect_context(&home.join("routines").join("output"), &def),
    };
    let outcome = run_once(&req, |_| None);

    let mut out = String::new();
    out.push_str(&format!("RUN: {} ({})\n", outcome.routine, outcome.run_id));
    out.push_str(&"-".repeat(8 + outcome.routine.len()));
    out.push('\n');
    out.push_str(&format!("  status:      {:?}\n", outcome.status));
    out.push_str(&format!("  duration:    {} ms\n", outcome.duration_ms));
    if let Some(code) = outcome.exit_code {
        out.push_str(&format!("  exit_code:   {code}\n"));
    }
    if let Some(err) = &outcome.error {
        out.push_str(&format!("  error:       {err}\n"));
    }
    if let Some(p) = &outcome.archive_path {
        out.push_str(&format!("  archive:     {}\n", p.display()));
    }
    let oks = outcome.deliveries.iter().filter(|d| d.ok).count();
    out.push_str(&format!(
        "  deliveries:  {}/{} ok\n",
        oks,
        outcome.deliveries.len()
    ));
    if !outcome.summary.is_empty() {
        out.push_str("\nSUMMARY\n-------\n");
        out.push_str(&outcome.summary);
        out.push('\n');
    }
    out
}

// ─── /schedule status ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DaemonStatusOnDisk {
    pid: u32,
    started_at: u64,
    last_tick_at: u64,
    last_tick_routines_loaded: usize,
    last_tick_routines_fired: usize,
    #[serde(default)]
    last_error: Option<String>,
    anvil_version: String,
}

fn render_status(home: &Path, routines_dir: &Path) -> String {
    let mut out = String::new();
    let status_path = home.join("run").join("anvild.status.json");
    let pid_path = home.join("run").join("anvild.pid");

    out.push_str("ANVILD STATUS\n");
    out.push_str("-------------\n");

    if pid_path.exists() {
        let pid = fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        match pid {
            Some(p) => out.push_str(&format!("  pid:           {p}\n")),
            None => out.push_str("  pid:           (PID file present but unreadable)\n"),
        }
    } else {
        out.push_str("  pid:           NOT RUNNING (no PID file)\n");
        out.push_str("                 Start with: /daemon start\n");
    }

    if let Ok(raw) = fs::read_to_string(&status_path) {
        if let Ok(s) = serde_json::from_str::<DaemonStatusOnDisk>(&raw) {
            let now = unix_now();
            out.push_str(&format!(
                "  uptime:        {}\n",
                fmt_relative(now.saturating_sub(s.started_at))
            ));
            out.push_str(&format!(
                "  last tick:     {} ago\n",
                fmt_relative(now.saturating_sub(s.last_tick_at))
            ));
            out.push_str(&format!(
                "  routines:      {} loaded, {} fired last tick\n",
                s.last_tick_routines_loaded, s.last_tick_routines_fired
            ));
            out.push_str(&format!("  version:       {}\n", s.anvil_version));
            if let Some(err) = &s.last_error {
                out.push_str(&format!("  last error:    {err}\n"));
            }
        }
    } else {
        out.push_str("  status sidecar: not yet written (daemon still warming up?)\n");
    }

    let LoadAllResult { defs, .. } = load_all(routines_dir);
    if !defs.is_empty() {
        out.push_str("\nNEXT FIRES\n----------\n");
        let now = unix_now();
        let mut rows: Vec<(String, Option<u64>)> = defs
            .iter()
            .map(|d| {
                let nf = if d.enabled {
                    next_fire(&d.schedule, now)
                } else {
                    None
                };
                (d.name.clone(), nf)
            })
            .collect();
        rows.sort_by_key(|(_, nf)| nf.unwrap_or(u64::MAX));
        for (name, nf) in rows.iter().take(6) {
            let when = nf
                .map(|t| format!("in {}", fmt_relative(t.saturating_sub(now))))
                .unwrap_or_else(|| "disabled".into());
            out.push_str(&format!("  {:<24}  {}\n", name, when));
        }
        if rows.len() > 6 {
            out.push_str(&format!("  …and {} more routines\n", rows.len() - 6));
        }
    }
    out
}

// ─── /schedule enable/disable ───────────────────────────────────────────────

fn render_toggle(routines_dir: &Path, name: &str, target: bool) -> String {
    let LoadAllResult { defs, .. } = load_all(routines_dir);
    let Some(def) = defs.iter().find(|d| d.name == name) else {
        return format!("/schedule {}: no routine named `{name}`", verb(target));
    };
    if def.enabled == target {
        return format!(
            "/schedule {}: routine `{name}` is already {}",
            verb(target),
            if target { "enabled" } else { "disabled" }
        );
    }
    match toggle_enabled_field(&def.source_path, target) {
        Ok(()) => format!(
            "/schedule {}: routine `{name}` is now {}.\n  Source: {}\n  The daemon will pick this up on its next tick (~30s).",
            verb(target),
            if target { "enabled" } else { "disabled" },
            def.source_path.display()
        ),
        Err(e) => format!(
            "/schedule {}: failed to update {}: {e}",
            verb(target),
            def.source_path.display()
        ),
    }
}

fn verb(target: bool) -> &'static str {
    if target { "enable" } else { "disable" }
}

/// Surgical in-place edit of the TOML's `enabled = …` line.  We avoid a full
/// re-serialize because the user's comments, ordering, and formatting are
/// load-bearing for them (this is *their* file).
fn toggle_enabled_field(path: &Path, target: bool) -> Result<(), String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = raw.lines().map(String::from).collect();
    let target_str = if target { "true" } else { "false" };
    let mut updated = false;
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("enabled") && trimmed.contains('=') {
            *line = format!("enabled = {target_str}");
            updated = true;
            break;
        }
    }
    if !updated {
        // No `enabled = …` line present (treated as default-true). Append one.
        // Place it right after the `name = …` line for cleanliness.
        let mut new_lines: Vec<String> = Vec::with_capacity(lines.len() + 1);
        for line in lines.into_iter() {
            new_lines.push(line.clone());
            if line.trim_start().starts_with("name") && line.contains('=') {
                new_lines.push(format!("enabled = {target_str}"));
            }
        }
        lines = new_lines;
    }
    let body = lines.join("\n");
    let body = if raw.ends_with('\n') && !body.ends_with('\n') {
        format!("{body}\n")
    } else {
        body
    };
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, body.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn anvil_home() -> PathBuf {
    if let Ok(v) = std::env::var("ANVIL_CONFIG_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(v) = std::env::var("ANVIL_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    dirs_next::home_dir()
        .map(|h| h.join(".anvil"))
        .unwrap_or_else(|| PathBuf::from(".anvil"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn fmt_relative(secs: u64) -> String {
    if secs == 0 {
        return "now".to_string();
    }
    let (h, rem) = (secs / 3600, secs % 3600);
    let (m, s) = (rem / 60, rem % 60);
    if h >= 24 {
        format!("{}d{}h", h / 24, h % 24)
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[allow(dead_code)]
fn _link_types(_d: &DeliveryTarget, _p: &RoutinePermissionMode, _r: &RoutineDef) {}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_empty_dir_returns_helpful_message() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_schedule_command(Some("list"));
        // The handler uses anvil_home() which we can't override mid-process
        // without env munging; just verify the empty-dir path is hit when
        // we point at our temp dir directly.
        let direct = render_list(tmp.path());
        assert!(direct.contains("no routines installed"));
        let _ = out; // keep handler reachable from tests
    }

    fn write_routine(dir: &Path, name: &str, enabled: bool) {
        let body = format!(
            "name = \"{name}\"\nenabled = {enabled}\nschedule = \"every 30m\"\nprompt = \"test prompt body\"\n"
        );
        fs::write(dir.join(format!("{name}.toml")), body).unwrap();
    }

    #[test]
    fn list_renders_routines() {
        let tmp = tempfile::tempdir().unwrap();
        write_routine(tmp.path(), "alpha", true);
        write_routine(tmp.path(), "beta", false);
        let out = render_list(tmp.path());
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
        assert!(out.contains("every 30m"));
        assert!(out.contains("● alpha")); // enabled marker
        assert!(out.contains("○ beta")); // disabled marker
    }

    #[test]
    fn show_renders_routine_detail() {
        let tmp = tempfile::tempdir().unwrap();
        write_routine(tmp.path(), "release-watch", true);
        let out = render_show(tmp.path(), tmp.path(), "release-watch");
        assert!(out.contains("ROUTINE: release-watch"));
        assert!(out.contains("schedule:       every 30m"));
        assert!(out.contains("test prompt body"));
    }

    #[test]
    fn show_missing_routine_friendly_error() {
        let tmp = tempfile::tempdir().unwrap();
        let out = render_show(tmp.path(), tmp.path(), "nope");
        assert!(out.contains("no routine named `nope`"));
    }

    #[test]
    fn disable_then_enable_round_trips_via_toml() {
        let tmp = tempfile::tempdir().unwrap();
        write_routine(tmp.path(), "x", true);
        let path = tmp.path().join("x.toml");

        let out_disable = render_toggle(tmp.path(), "x", false);
        assert!(out_disable.contains("disabled"));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("enabled = false"));

        let out_enable = render_toggle(tmp.path(), "x", true);
        assert!(out_enable.contains("enabled"));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("enabled = true"));
    }

    #[test]
    fn enable_appends_field_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "name = \"y\"\nschedule = \"every 1h\"\nprompt = \"p\"\n";
        let path = tmp.path().join("y.toml");
        fs::write(&path, body).unwrap();
        // Routine defaults to enabled=true (default_true).  Toggle to false:
        let out = render_toggle(tmp.path(), "y", false);
        assert!(out.contains("disabled"));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("enabled = false"));
        // And the `name` line is still first (we appended right after it).
        assert!(raw.starts_with("name = \"y\""));
    }

    #[test]
    fn fmt_relative_brackets() {
        assert_eq!(fmt_relative(0), "now");
        assert_eq!(fmt_relative(45), "45s");
        assert_eq!(fmt_relative(125), "2m05s");
        assert_eq!(fmt_relative(3_725), "1h02m");
        assert_eq!(fmt_relative(90_000), "1d1h");
    }

    #[test]
    fn unknown_subcommand_friendly() {
        let out = run_schedule_command(Some("wat"));
        assert!(out.contains("unknown subcommand"));
    }

    #[test]
    fn daemon_unknown_subcommand_friendly() {
        let out = run_daemon_command(Some("wat"));
        assert!(out.contains("unknown subcommand"));
    }
}
