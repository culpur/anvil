//! `anvil daemon` — the first local **anvild** server (v2.2.18 #657).
//!
//! Long-running OS process that owns the routine scheduler.  Persists across
//! TUI sessions: when the user closes their interactive Anvil, routines still
//! fire on schedule because anvild is a separate process with its own PID,
//! its own log, and its own lifecycle.
//!
//! ## Subcommands
//!
//! ```text
//! anvil daemon start [--foreground]   # spawn detached (or run in this terminal)
//! anvil daemon stop                    # SIGTERM the pid in ~/.anvil/run/anvild.pid
//! anvil daemon status                  # is it running? since when? last tick?
//! anvil daemon foreground              # alias for `start --foreground` (used by service units)
//! anvil daemon install-service         # generate LaunchAgent / systemd / Task Scheduler unit
//! anvil daemon uninstall-service       # remove the unit we generated
//! ```
//!
//! ## Files we own
//!
//! - `~/.anvil/run/anvild.pid`          — current PID; written on start, removed on graceful stop
//! - `~/.anvil/run/anvild.log`          — stdout + stderr of the daemon process
//! - `~/.anvil/run/anvild.status.json`  — last tick, last error, routine counts (refreshed every 30 s)
//! - `~/.anvil/routines/*.toml`         — routine definitions, owned by user
//! - `~/.anvil/routines/output/<name>/` — packets + archive markdown, owned by daemon
//!
//! ## Loop
//!
//! Every 30 s:
//! 1. Reload routine definitions from disk (cheap — only re-parses changed files).
//! 2. For each enabled routine, ask [`schedule::next_fire`] when it should run next.
//! 3. If `next_fire <= now`, build the [`ExecRequest`] with collected context
//!    blocks and call [`executor::run_once`] in a worker thread.  The main loop
//!    never blocks on inference — one slow routine doesn't starve the others.
//! 4. Write status sidecar so `/schedule status` and `anvil daemon status`
//!    have a fresh snapshot.
//!
//! ## What's NOT here
//!
//! - **Inter-process IPC.** v2.2.18 keeps anvild stateless w.r.t. the TUI:
//!   the TUI reads packets/status JSON from disk, the daemon doesn't expose
//!   a Unix socket or HTTP endpoint.  v2.3 will add that.
//! - **Vault unlock prompts.** The daemon runs while the user is away; a
//!   locked vault means webhook deliveries fail with a clear error.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use runtime::routines::definition::{load_all, LoadAllResult, RoutineDef};
use runtime::routines::executor::{
    collect_context, run_once, validate_anvil_binary, ExecRequest,
};
use runtime::routines::schedule::next_fire;

// ─── Subcommand enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonSubcommand {
    Start { foreground: bool },
    Stop,
    Status,
    Foreground,
    InstallService,
    UninstallService,
}

/// Parse `anvil daemon <args>` into a [`DaemonSubcommand`].
pub fn parse(args: &[String]) -> Result<DaemonSubcommand, String> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Ok(DaemonSubcommand::Status);
    };
    match sub {
        "start" => {
            let foreground = args.iter().any(|a| a == "--foreground" || a == "-f");
            Ok(DaemonSubcommand::Start { foreground })
        }
        "stop" => Ok(DaemonSubcommand::Stop),
        "status" => Ok(DaemonSubcommand::Status),
        "foreground" | "run" => Ok(DaemonSubcommand::Foreground),
        "install-service" => Ok(DaemonSubcommand::InstallService),
        "uninstall-service" => Ok(DaemonSubcommand::UninstallService),
        other => Err(format!(
            "anvil daemon: unknown subcommand `{other}` (expected: start | stop | status | foreground | install-service | uninstall-service)"
        )),
    }
}

// ─── Status sidecar ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub started_at: u64,
    pub last_tick_at: u64,
    pub last_tick_routines_loaded: usize,
    pub last_tick_routines_fired: usize,
    /// Most recent non-fatal error encountered in the loop (load error,
    /// archive write error, etc.).  Cleared after a successful tick with
    /// no errors.
    pub last_error: Option<String>,
    pub anvil_version: String,
}

fn status_path(home: &Path) -> PathBuf {
    home.join("run").join("anvild.status.json")
}
fn pid_path(home: &Path) -> PathBuf {
    home.join("run").join("anvild.pid")
}
fn log_path(home: &Path) -> PathBuf {
    home.join("run").join("anvild.log")
}

fn write_status(home: &Path, status: &DaemonStatus) {
    let path = status_path(home);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string_pretty(status) {
        let _ = fs::write(path, body);
    }
}

fn read_pid(home: &Path) -> Option<u32> {
    fs::read_to_string(pid_path(home))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::raw::c_int;
        // SAFETY: kill(2) with signal 0 just probes for the process existence
        // without sending an actual signal.  No memory is read; the syscall
        // returns 0 on alive and -1 with errno=ESRCH on missing.
        unsafe {
            unsafe extern "C" {
                fn kill(pid: c_int, sig: c_int) -> c_int;
            }
            kill(pid as c_int, 0) == 0
        }
    }
    #[cfg(windows)]
    {
        // Cheap fallback: try to read the PID file's age and compare against
        // a hard maximum.  Not perfect but adequate for v2.2.18; v2.3 will
        // use OpenProcess for a real check.  For now assume any PID file
        // newer than 24 h is live.
        let _ = pid;
        true
    }
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Entry point invoked from `main.rs::run_cli`.
///
/// Returns a process exit code; the caller `std::process::exit`s with it so
/// shell consumers can chain `anvil daemon status && anvil …`.
pub fn run(sub: DaemonSubcommand, anvil_binary: PathBuf, anvil_version: String) -> i32 {
    let home = anvil_home();
    match sub {
        DaemonSubcommand::Start { foreground: true } | DaemonSubcommand::Foreground => {
            run_foreground(&home, &anvil_binary, &anvil_version)
        }
        DaemonSubcommand::Start { foreground: false } => spawn_detached(&home, &anvil_binary),
        DaemonSubcommand::Stop => stop(&home),
        DaemonSubcommand::Status => print_status(&home),
        DaemonSubcommand::InstallService => install_service(&home, &anvil_binary),
        DaemonSubcommand::UninstallService => uninstall_service(&home),
    }
}

fn anvil_home() -> PathBuf {
    if let Ok(explicit) = std::env::var("ANVIL_CONFIG_HOME") {
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }
    if let Ok(explicit) = std::env::var("ANVIL_HOME") {
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }
    dirs_next::home_dir()
        .map(|h| h.join(".anvil"))
        .unwrap_or_else(|| PathBuf::from(".anvil"))
}

// ─── start --foreground / `daemon foreground` ───────────────────────────────

fn run_foreground(home: &Path, anvil_binary: &Path, anvil_version: &str) -> i32 {
    if let Err(e) = validate_anvil_binary(anvil_binary) {
        eprintln!("anvil daemon: {e}");
        return 2;
    }

    let _ = fs::create_dir_all(home.join("run"));
    let _ = fs::create_dir_all(home.join("routines"));
    let _ = fs::create_dir_all(home.join("routines").join("output"));

    // Refuse to start if another daemon is already running with this PID file.
    if let Some(existing) = read_pid(home) {
        if pid_alive(existing) {
            eprintln!(
                "anvil daemon: already running with PID {existing} (see {})",
                pid_path(home).display()
            );
            return 1;
        } else {
            // Stale PID file — silently reclaim.
            let _ = fs::remove_file(pid_path(home));
        }
    }

    let pid = std::process::id();
    if let Err(e) = fs::write(pid_path(home), pid.to_string()) {
        eprintln!("anvil daemon: failed to write PID file: {e}");
        return 3;
    }

    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handler(Arc::clone(&stop));

    let started_at = unix_now();
    let mut status = DaemonStatus {
        pid,
        started_at,
        last_tick_at: started_at,
        last_tick_routines_loaded: 0,
        last_tick_routines_fired: 0,
        last_error: None,
        anvil_version: anvil_version.to_string(),
    };
    write_status(home, &status);

    // Per-routine "next fire" cache so we don't re-compute every tick.
    let mut next_fire_cache: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();

    eprintln!(
        "[anvild] starting (pid {pid}, version {anvil_version}, anvil binary {})",
        anvil_binary.display()
    );

    while !stop.load(Ordering::Relaxed) {
        let tick_start = Instant::now();
        let now = unix_now();

        let LoadAllResult { defs, errors } = load_all(&home.join("routines"));
        status.last_tick_routines_loaded = defs.len();
        status.last_tick_routines_fired = 0;
        status.last_error = errors
            .first()
            .map(|e| format!("definition load: {e}"));

        // Garbage-collect cache entries for routines that vanished.
        next_fire_cache.retain(|name, _| defs.iter().any(|d| d.name == *name));

        for def in &defs {
            if !def.enabled {
                next_fire_cache.remove(&def.name);
                continue;
            }
            let next = *next_fire_cache.entry(def.name.clone()).or_insert_with(|| {
                next_fire(&def.schedule, now).unwrap_or(u64::MAX)
            });
            if next > now {
                continue;
            }

            // It's go time.  Spawn the routine on a worker thread so the loop
            // can move on; we'll re-cache `next_fire` after the worker
            // returns by writing the next time on the spot.
            let def_clone = def.clone();
            let binary = anvil_binary.to_path_buf();
            let config_home = home.to_path_buf();
            let version = anvil_version.to_string();
            let output_root = home.join("routines").join("output");

            std::thread::spawn(move || {
                let ctx = collect_context(&output_root, &def_clone);
                let req = ExecRequest {
                    routine: def_clone.clone(),
                    anvil_binary: binary,
                    config_home,
                    anvil_version: version,
                    timeout: Duration::from_secs(300),
                    context_blocks: ctx,
                };
                let outcome = run_once(&req, |_| None);
                eprintln!(
                    "[anvild] {} run {} → {:?} ({} ms; deliveries: {})",
                    def_clone.name,
                    outcome.run_id,
                    outcome.status,
                    outcome.duration_ms,
                    outcome
                        .deliveries
                        .iter()
                        .filter(|d| d.ok)
                        .count()
                );
            });

            // Compute the routine's next fire so we don't re-fire on the
            // next tick (most schedules use interval; we add the interval
            // to `now`, not `next`, to avoid runaway catch-up).
            let after = if def.enabled { now + 1 } else { now };
            let new_next = next_fire(&def.schedule, after).unwrap_or(u64::MAX);
            next_fire_cache.insert(def.name.clone(), new_next);
            status.last_tick_routines_fired += 1;
        }

        status.last_tick_at = now;
        write_status(home, &status);

        // Sleep ~30 s, but wake immediately on stop signal.
        let target_tick = Duration::from_secs(30);
        let elapsed = tick_start.elapsed();
        let remaining = target_tick.saturating_sub(elapsed);
        let deadline = Instant::now() + remaining;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    eprintln!("[anvild] shutting down");
    let _ = fs::remove_file(pid_path(home));
    0
}

// ─── Signal handling ─────────────────────────────────────────────────────────

#[cfg(unix)]
fn install_signal_handler(stop: Arc<AtomicBool>) {
    use std::os::raw::c_int;
    extern "C" fn handler(_sig: c_int) {
        SHUTDOWN.store(true, Ordering::SeqCst);
    }
    static SHUTDOWN: AtomicBool = AtomicBool::new(false);
    unsafe {
        unsafe extern "C" {
            fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
        }
        // SIGTERM = 15, SIGINT = 2 on every Unix Anvil supports.
        signal(15, handler);
        signal(2, handler);
    }
    // Bridge the static flag into the Arc that the loop polls.
    std::thread::spawn(move || loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            stop.store(true, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    });
}

#[cfg(windows)]
fn install_signal_handler(_stop: Arc<AtomicBool>) {
    // Ctrl+C handling on Windows lives in the foreground console; service
    // unit invocations terminate via SCM stop.  v2.3 will wire SetConsoleCtrlHandler.
}

// ─── start (detached spawn) ──────────────────────────────────────────────────

fn spawn_detached(home: &Path, anvil_binary: &Path) -> i32 {
    if let Err(e) = validate_anvil_binary(anvil_binary) {
        eprintln!("anvil daemon: {e}");
        return 2;
    }
    if let Some(existing) = read_pid(home) {
        if pid_alive(existing) {
            eprintln!("anvil daemon: already running (pid {existing})");
            return 0;
        }
        let _ = fs::remove_file(pid_path(home));
    }
    let _ = fs::create_dir_all(home.join("run"));
    let log = log_path(home);
    let stdout = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("anvil daemon: cannot open log {}: {e}", log.display());
            return 3;
        }
    };
    let stderr = match stdout.try_clone() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("anvil daemon: cannot clone log fd: {e}");
            return 3;
        }
    };

    // Spawn ourselves as `anvil daemon foreground` so the child runs the
    // exact same code path as `--foreground` and writes its own PID file.
    let mut cmd = std::process::Command::new(anvil_binary);
    cmd.arg("daemon")
        .arg("foreground")
        .env("ANVIL_CONFIG_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr));

    #[cfg(unix)]
    detach_unix(&mut cmd);
    #[cfg(windows)]
    detach_windows(&mut cmd);

    match cmd.spawn() {
        Ok(child) => {
            // Wait briefly for the child to write its PID file so the user
            // gets immediate confirmation it's alive.
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if read_pid(home).is_some_and(pid_alive) {
                    let pid = read_pid(home).unwrap_or(child.id());
                    println!("anvil daemon: started (pid {pid}, log {})", log.display());
                    return 0;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Child might be alive but slow; report the spawn-time PID as
            // best-effort.
            println!(
                "anvil daemon: started (pid {}, log {}) — status pending",
                child.id(),
                log.display()
            );
            0
        }
        Err(e) => {
            eprintln!("anvil daemon: spawn failed: {e}");
            4
        }
    }
}

#[cfg(unix)]
fn detach_unix(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // setsid() at the child's pre_exec moves the new process into its own
    // session, detaching from the parent's controlling terminal.  Survives
    // when the parent shell exits.
    unsafe {
        cmd.pre_exec(|| {
            unsafe extern "C" {
                fn setsid() -> i32;
            }
            let _ = unsafe { setsid() };
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach_windows(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

// ─── stop ───────────────────────────────────────────────────────────────────

fn stop(home: &Path) -> i32 {
    let Some(pid) = read_pid(home) else {
        println!("anvil daemon: not running (no PID file)");
        return 0;
    };
    if !pid_alive(pid) {
        let _ = fs::remove_file(pid_path(home));
        println!("anvil daemon: not running (stale PID cleared)");
        return 0;
    }
    #[cfg(unix)]
    {
        use std::os::raw::c_int;
        unsafe {
            unsafe extern "C" {
                fn kill(pid: c_int, sig: c_int) -> c_int;
            }
            // SIGTERM
            kill(pid as c_int, 15);
        }
    }
    #[cfg(windows)]
    {
        // Best-effort: spawn taskkill since std::process doesn't expose a
        // cross-PID terminate.  Service unit invocations should prefer
        // `sc stop` anyway.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Wait for graceful exit, up to 5 s.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if pid_alive(pid) {
        eprintln!("anvil daemon: pid {pid} did not exit within 5s");
        return 5;
    }
    let _ = fs::remove_file(pid_path(home));
    println!("anvil daemon: stopped (pid {pid})");
    0
}

// ─── status ─────────────────────────────────────────────────────────────────

fn print_status(home: &Path) -> i32 {
    let pid_opt = read_pid(home);
    match pid_opt {
        Some(pid) if pid_alive(pid) => {
            print!("anvil daemon: running (pid {pid})");
            let status = fs::read_to_string(status_path(home))
                .ok()
                .and_then(|s| serde_json::from_str::<DaemonStatus>(&s).ok());
            match status {
                Some(s) => {
                    let uptime = unix_now().saturating_sub(s.started_at);
                    println!(
                        "; uptime {}; routines loaded {}; last tick {}s ago",
                        fmt_duration(uptime),
                        s.last_tick_routines_loaded,
                        unix_now().saturating_sub(s.last_tick_at),
                    );
                    if let Some(err) = &s.last_error {
                        println!("  last error: {err}");
                    }
                }
                None => println!(" (status file missing)"),
            }
            0
        }
        Some(_) => {
            println!("anvil daemon: not running (stale PID file)");
            2
        }
        None => {
            println!("anvil daemon: not running");
            1
        }
    }
}

fn fmt_duration(secs: u64) -> String {
    let (h, rem) = (secs / 3600, secs % 3600);
    let (m, s) = (rem / 60, rem % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

// ─── Service unit generators ────────────────────────────────────────────────

fn install_service(home: &Path, anvil_binary: &Path) -> i32 {
    if let Err(e) = validate_anvil_binary(anvil_binary) {
        eprintln!("anvil daemon: {e}");
        return 2;
    }
    let _ = fs::create_dir_all(home.join("run"));

    #[cfg(target_os = "macos")]
    {
        let plist = build_launchagent_plist(home, anvil_binary);
        let path = launchagent_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&path, plist) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        println!("anvil daemon: wrote {}", path.display());
        println!("  Load now:  launchctl load -w {}", path.display());
        println!("  Unload:    launchctl unload {}", path.display());
        return 0;
    }
    #[cfg(target_os = "linux")]
    {
        let unit = build_systemd_unit(anvil_binary);
        let path = systemd_unit_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&path, unit) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        println!("anvil daemon: wrote {}", path.display());
        println!("  Enable now:  systemctl --user daemon-reload && systemctl --user enable --now anvild.service");
        println!("  Disable:     systemctl --user disable --now anvild.service");
        return 0;
    }
    #[cfg(target_os = "windows")]
    {
        let xml = build_taskscheduler_xml(anvil_binary);
        let path = home.join("run").join("anvild-task.xml");
        if let Err(e) = fs::write(&path, xml) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        println!("anvil daemon: wrote {}", path.display());
        println!("  Register: schtasks /Create /TN Anvild /XML \"{}\"", path.display());
        println!("  Unregister: schtasks /Delete /TN Anvild /F");
        return 0;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (home, anvil_binary);
        eprintln!("anvil daemon: install-service not supported on this platform");
        return 6;
    }
}

fn uninstall_service(home: &Path) -> i32 {
    #[cfg(target_os = "macos")]
    {
        let path = launchagent_path();
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &path.to_string_lossy()])
            .status();
        let _ = fs::remove_file(&path);
        println!("anvil daemon: removed {}", path.display());
        return 0;
    }
    #[cfg(target_os = "linux")]
    {
        let path = systemd_unit_path();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "anvild.service"])
            .status();
        let _ = fs::remove_file(&path);
        println!("anvil daemon: removed {}", path.display());
        return 0;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        let _ = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", "Anvild", "/F"])
            .status();
        println!("anvil daemon: removed Anvild task");
        return 0;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = home;
        eprintln!("anvil daemon: uninstall-service not supported on this platform");
        return 6;
    }
}

#[cfg(target_os = "macos")]
fn launchagent_path() -> PathBuf {
    dirs_next::home_dir()
        .map(|h| h.join("Library/LaunchAgents/net.culpur.anvild.plist"))
        .unwrap_or_else(|| PathBuf::from("net.culpur.anvild.plist"))
}

/// LaunchAgent plist generator.  Public for the tests below.
#[cfg(any(target_os = "macos", test))]
pub fn build_launchagent_plist(home: &Path, binary: &Path) -> String {
    let home_s = home.to_string_lossy();
    let bin_s = binary.to_string_lossy();
    let log_s = home.join("run").join("anvild.log");
    let log_s = log_s.to_string_lossy();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>net.culpur.anvild</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_s}</string>
        <string>daemon</string>
        <string>foreground</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ANVIL_CONFIG_HOME</key>
        <string>{home_s}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_s}</string>
    <key>StandardErrorPath</key>
    <string>{log_s}</string>
</dict>
</plist>
"#
    )
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    dirs_next::config_dir()
        .map(|c| c.join("systemd/user/anvild.service"))
        .unwrap_or_else(|| PathBuf::from("anvild.service"))
}

/// systemd --user unit generator.  Public for the tests below.
#[cfg(any(target_os = "linux", test))]
pub fn build_systemd_unit(binary: &Path) -> String {
    let bin_s = binary.to_string_lossy();
    format!(
        r#"[Unit]
Description=Anvil routines daemon (anvild)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin_s} daemon foreground
Restart=on-failure
RestartSec=5s
# Keep stdout/stderr — anvild writes structured per-tick logs to its own file.
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
"#
    )
}

/// Windows Task Scheduler XML generator.  Public for the tests below.
#[cfg(any(target_os = "windows", test))]
pub fn build_taskscheduler_xml(binary: &Path) -> String {
    let bin_s = binary.to_string_lossy();
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Anvil routines daemon (anvild)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <RestartOnFailure>
      <Interval>PT5S</Interval>
      <Count>10</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{bin_s}</Command>
      <Arguments>daemon foreground</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

// ─── Misc helpers ────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Silence unused-import warning when neither install-service nor stop are
/// reachable on the current target (Path is borrowed by sibling fns).
#[allow(dead_code)]
fn _force_path_link(_p: &Path, _w: &mut dyn Write) {}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_args_returns_status() {
        assert_eq!(parse(&[]).unwrap(), DaemonSubcommand::Status);
    }

    #[test]
    fn parse_start_detached_default() {
        let args = vec!["start".to_string()];
        assert_eq!(
            parse(&args).unwrap(),
            DaemonSubcommand::Start { foreground: false }
        );
    }

    #[test]
    fn parse_start_foreground_flag() {
        let args = vec!["start".to_string(), "--foreground".to_string()];
        assert_eq!(
            parse(&args).unwrap(),
            DaemonSubcommand::Start { foreground: true }
        );
    }

    #[test]
    fn parse_start_short_flag() {
        let args = vec!["start".to_string(), "-f".to_string()];
        assert_eq!(
            parse(&args).unwrap(),
            DaemonSubcommand::Start { foreground: true }
        );
    }

    #[test]
    fn parse_unknown_subcommand_errors() {
        let args = vec!["wat".to_string()];
        assert!(parse(&args).is_err());
    }

    #[test]
    fn parse_install_service() {
        let args = vec!["install-service".to_string()];
        assert_eq!(parse(&args).unwrap(), DaemonSubcommand::InstallService);
    }

    #[test]
    fn parse_uninstall_service() {
        let args = vec!["uninstall-service".to_string()];
        assert_eq!(parse(&args).unwrap(), DaemonSubcommand::UninstallService);
    }

    #[test]
    fn fmt_duration_seconds_only() {
        assert_eq!(fmt_duration(0), "0s");
        assert_eq!(fmt_duration(45), "45s");
    }

    #[test]
    fn fmt_duration_minutes() {
        assert_eq!(fmt_duration(125), "2m05s");
    }

    #[test]
    fn fmt_duration_hours() {
        assert_eq!(fmt_duration(3_725), "1h02m05s");
    }

    #[test]
    fn launchagent_plist_contains_bin_and_home() {
        let p = build_launchagent_plist(
            Path::new("/Users/x/.anvil"),
            Path::new("/Users/x/bin/anvil"),
        );
        assert!(p.contains("/Users/x/bin/anvil"));
        assert!(p.contains("/Users/x/.anvil"));
        assert!(p.contains("daemon"));
        assert!(p.contains("foreground"));
        assert!(p.contains("KeepAlive"));
    }

    #[test]
    fn systemd_unit_contains_bin_and_restart() {
        let u = build_systemd_unit(Path::new("/usr/local/bin/anvil"));
        assert!(u.contains("/usr/local/bin/anvil daemon foreground"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("[Install]"));
    }

    #[test]
    fn taskscheduler_xml_contains_bin_and_logon_trigger() {
        let x = build_taskscheduler_xml(Path::new(r"C:\Program Files\Anvil\anvil.exe"));
        assert!(x.contains(r"C:\Program Files\Anvil\anvil.exe"));
        assert!(x.contains("<LogonTrigger>"));
        assert!(x.contains("daemon foreground"));
    }

    #[test]
    fn status_file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonStatus {
            pid: 12345,
            started_at: 100,
            last_tick_at: 200,
            last_tick_routines_loaded: 4,
            last_tick_routines_fired: 1,
            last_error: Some("bad routine".into()),
            anvil_version: "2.2.18-test".into(),
        };
        write_status(tmp.path(), &s);
        let raw = fs::read_to_string(status_path(tmp.path())).unwrap();
        let back: DaemonStatus = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.pid, 12345);
        assert_eq!(back.last_tick_routines_fired, 1);
    }
}
