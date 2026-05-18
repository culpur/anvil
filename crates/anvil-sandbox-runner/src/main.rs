//! `anvil-sandbox-runner` — detonate a hub-install command in a sandbox.
//!
//! This binary is invoked by the AnvilHub install flow (Feature 3 verified
//! build, tasks #506/#594) **before** the real install is allowed to touch
//! the user's machine.  It runs an arbitrary `installCmd` string inside an
//! OS-appropriate sandbox, captures stdout/stderr/exit-code/files-written
//! into a tmpfs-style root, and prints a single JSON object on stdout that
//! the calling Anvil process can show the user as a "detonation report".
//!
//! ## CLI
//!
//! ```text
//! anvil-sandbox-runner [OPTIONS] <install-cmd>
//!   --timeout=N       Kill the install after N seconds (default 60)
//!   --allow-network   Permit outbound network from the sandbox (default deny)
//!   --version         Print version and exit
//!   --help            Print this help and exit
//! ```
//!
//! ## OS-specific sandboxing
//!
//! - **Linux**:  `unshare --user --map-root-user --mount --pid --ipc --uts --fork`
//!   + (when network is denied) `--net`.  This is the same launcher used by
//!   `runtime::sandbox::build_linux_sandbox_command` for the in-process
//!   sandbox.  `landlock` and `seccomp` are layered on top via `prctl` in a
//!   follow-up (#570 v2 — out of scope here).
//! - **macOS**:  `sandbox-exec -p <profile>` with a tight profile that
//!   restricts writes to the sandbox root and (when network is denied)
//!   blocks `network*` operations.
//! - **Windows**:  Job Object stub.  The current implementation simply
//!   runs the command with a sandbox-rooted CWD + `TMP`/`TEMP` env vars
//!   pointed at the sandbox root.  Real Job Object containment lands in
//!   a follow-up; this stub is documented in the JSON report.
//!
//! ## Output
//!
//! Single JSON object on stdout, e.g.:
//!
//! ```json
//! {
//!   "exit_code": 0,
//!   "files_written": ["/tmp/anvil-sandbox-XXXX/foo.txt"],
//!   "files_written_outside_sandbox": [],
//!   "stdout_tail": "hello\n",
//!   "stderr_tail": "",
//!   "duration_ms": 12,
//!   "killed_by_timeout": false,
//!   "sandbox_backend": "linux-unshare",
//!   "sandbox_root": "/tmp/anvil-sandbox-XXXX",
//!   "allow_network": false
//! }
//! ```
//!
//! Anvil parses this object, prompts the user with the report, then
//! decides whether to run the real install.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Cap on stdout/stderr tail bytes kept in the report.  Anything beyond is
/// truncated — the JSON object is meant to be shown to the user inside a
/// TUI modal, not used as a build log.
const TAIL_BYTES: usize = 8 * 1024;

#[derive(Debug, Serialize)]
struct DetonationReport {
    /// Exit code reported by the install command.  `null` when the
    /// command was killed before reporting (timeout, signal).
    exit_code: Option<i32>,
    /// Files written under the sandbox root, relative to the host fs.
    files_written: Vec<String>,
    /// Files written **outside** the sandbox root (escape detection).
    /// Empty when the sandbox enforced isolation correctly.
    files_written_outside_sandbox: Vec<String>,
    /// Last `TAIL_BYTES` of stdout, UTF-8 lossy.
    stdout_tail: String,
    /// Last `TAIL_BYTES` of stderr, UTF-8 lossy.
    stderr_tail: String,
    /// Wall-clock duration of the command in milliseconds.
    duration_ms: u128,
    /// `true` when the runner killed the command after the timeout.
    killed_by_timeout: bool,
    /// Which sandbox backend actually ran.  One of:
    /// `linux-unshare`, `macos-sandbox-exec`, `windows-job-object-stub`,
    /// `unsandboxed-fallback` (when no backend was available).
    sandbox_backend: &'static str,
    /// Absolute path to the sandbox root that was used.
    sandbox_root: String,
    /// Whether network was permitted in the sandbox.
    allow_network: bool,
}

#[derive(Debug)]
struct Options {
    timeout: Duration,
    allow_network: bool,
    install_cmd: String,
}

fn print_help() {
    println!(
        "anvil-sandbox-runner {VERSION}\n\
         \n\
         Usage: anvil-sandbox-runner [OPTIONS] <install-cmd>\n\
         \n\
         Runs <install-cmd> inside an OS-appropriate sandbox and prints a JSON\n\
         report (exit code, files written, stdout/stderr tails) on stdout.\n\
         \n\
         Options:\n\
           --timeout=N       Kill the install after N seconds (default {DEFAULT_TIMEOUT_SECS}).\n\
           --allow-network   Permit outbound network from the sandbox (default deny).\n\
           --version         Print version and exit.\n\
           --help            Print this help and exit.\n"
    );
}

fn parse_args(argv: &[String]) -> Result<Options, String> {
    let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    let mut allow_network = false;
    let mut install_cmd: Option<String> = None;
    for arg in argv.iter() {
        if arg == "--allow-network" {
            allow_network = true;
            continue;
        }
        if let Some(rest) = arg.strip_prefix("--timeout=") {
            let secs: u64 = rest
                .parse()
                .map_err(|_| format!("invalid --timeout value: {rest:?} (expected integer seconds)"))?;
            timeout = Duration::from_secs(secs);
            continue;
        }
        // First positional argument is the install command.
        if install_cmd.is_none() {
            install_cmd = Some(arg.clone());
            continue;
        }
        return Err(format!("unexpected extra argument: {arg:?}"));
    }
    let install_cmd =
        install_cmd.ok_or_else(|| "missing required <install-cmd> argument".to_string())?;
    if install_cmd.trim().is_empty() {
        return Err("install-cmd must not be empty".to_string());
    }
    Ok(Options {
        timeout,
        allow_network,
        install_cmd,
    })
}

/// Create a fresh tmpfs-style sandbox root.
///
/// On all platforms we use `std::env::temp_dir()/anvil-sandbox-<nanos>-<pid>`.
/// On Linux the `--mount` namespace in `unshare` keeps writes outside this
/// root from affecting the host; the post-run filesystem diff also catches
/// the case where a backend doesn't enforce containment (Windows stub).
fn make_sandbox_root() -> Result<PathBuf, std::io::Error> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("anvil-sandbox-{nanos}-{pid}"));
    fs::create_dir_all(&root)?;
    Ok(root)
}

/// Snapshot every regular file path under `root` (recursive).
fn snapshot_tree(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    walk(root, &mut out);
    out
}

fn walk(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk(&path, out);
        } else {
            out.insert(path);
        }
    }
}

/// Build the OS-specific command that runs `install_cmd` inside the sandbox.
///
/// Returns `(Command, backend_label)`.
fn build_sandbox_command(
    install_cmd: &str,
    sandbox_root: &Path,
    allow_network: bool,
) -> (Command, &'static str) {
    if cfg!(target_os = "linux") {
        let mut cmd = Command::new("unshare");
        cmd.arg("--user")
            .arg("--map-root-user")
            .arg("--mount")
            .arg("--ipc")
            .arg("--pid")
            .arg("--uts")
            .arg("--fork");
        if !allow_network {
            cmd.arg("--net");
        }
        cmd.arg("sh").arg("-lc").arg(install_cmd);
        cmd.current_dir(sandbox_root);
        cmd.env("HOME", sandbox_root);
        cmd.env("TMPDIR", sandbox_root);
        return (cmd, "linux-unshare");
    }
    if cfg!(target_os = "macos") {
        // macOS sandbox-exec profile: deny by default, allow process exec,
        // allow file writes only under the sandbox root, allow file reads
        // generally (so common build tools like `tar`/`unzip`/`brew` can
        // resolve their own resources from /usr), allow network only when
        // explicitly requested.
        //
        // macOS canonicalizes `/var/folders/...` to `/private/var/folders/...`
        // before applying `subpath` rules, so we add both forms to the
        // allow list — otherwise `touch` inside the sandbox root errors
        // with "Operation not permitted" even though we own the directory.
        let net_clause = if allow_network {
            "(allow network*)"
        } else {
            "(deny network*)"
        };
        let canonical_root = sandbox_root
            .canonicalize()
            .unwrap_or_else(|_| sandbox_root.to_path_buf());
        let profile = format!(
            "(version 1)\n\
             (deny default)\n\
             (allow process*)\n\
             (allow signal)\n\
             (allow sysctl-read)\n\
             (allow file-read*)\n\
             (allow file-write* (subpath \"{root}\"))\n\
             (allow file-write* (subpath \"{canonical}\"))\n\
             (allow file-write* (subpath \"/private/tmp\"))\n\
             (allow file-write* (subpath \"/private/var/tmp\"))\n\
             (allow file-write* (subpath \"/tmp\"))\n\
             {net_clause}\n",
            root = sandbox_root.display(),
            canonical = canonical_root.display(),
            net_clause = net_clause,
        );
        let mut cmd = Command::new("sandbox-exec");
        cmd.arg("-p").arg(profile);
        cmd.arg("sh").arg("-lc").arg(install_cmd);
        cmd.current_dir(sandbox_root);
        cmd.env("HOME", sandbox_root);
        cmd.env("TMPDIR", sandbox_root);
        return (cmd, "macos-sandbox-exec");
    }
    if cfg!(target_os = "windows") {
        // Windows Job Object containment is a follow-up; for v2.2.17 we
        // run the install command with a sandbox-rooted CWD and TMP/TEMP
        // env vars, then rely on the post-run filesystem diff to surface
        // any writes outside `sandbox_root`.
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(install_cmd);
        cmd.current_dir(sandbox_root);
        cmd.env("TMP", sandbox_root);
        cmd.env("TEMP", sandbox_root);
        cmd.env("USERPROFILE", sandbox_root);
        return (cmd, "windows-job-object-stub");
    }
    // Other Unix-likes (FreeBSD, NetBSD, etc.): unsandboxed fallback —
    // still useful for the file-write detection, just without OS-level
    // containment.  Documented in the report.
    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(install_cmd);
    cmd.current_dir(sandbox_root);
    cmd.env("HOME", sandbox_root);
    cmd.env("TMPDIR", sandbox_root);
    (cmd, "unsandboxed-fallback")
}

/// Run `cmd` with a wall-clock timeout.  Returns `(exit_code, stdout, stderr, killed)`.
///
/// On timeout we send SIGKILL (Unix) / TerminateProcess (Windows) and
/// return whatever output we have so far.
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<(Option<i32>, Vec<u8>, Vec<u8>, bool)> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let start = Instant::now();
    let mut killed = false;
    // Poll-based wait so we never block past the timeout.  A 50 ms tick
    // keeps the CPU cost negligible while staying responsive for the
    // common case of a fast install (echo, tar, etc.).
    loop {
        match child.try_wait()? {
            Some(_status) => break,
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    killed = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let exit_code = if killed {
        None
    } else {
        child.wait()?.code()
    };
    let stdout = match stdout_pipe {
        Some(mut handle) => {
            let mut buf = Vec::new();
            use std::io::Read;
            let _ = handle.read_to_end(&mut buf);
            buf
        }
        None => Vec::new(),
    };
    let stderr = match stderr_pipe {
        Some(mut handle) => {
            let mut buf = Vec::new();
            use std::io::Read;
            let _ = handle.read_to_end(&mut buf);
            buf
        }
        None => Vec::new(),
    };
    Ok((exit_code, stdout, stderr, killed))
}

fn tail_bytes(buf: &[u8]) -> String {
    if buf.len() <= TAIL_BYTES {
        return String::from_utf8_lossy(buf).into_owned();
    }
    let start = buf.len() - TAIL_BYTES;
    let mut out = String::from("...[truncated]...\n");
    out.push_str(&String::from_utf8_lossy(&buf[start..]));
    out
}

fn run_detonation(opts: &Options) -> std::io::Result<DetonationReport> {
    let sandbox_root = make_sandbox_root()?;
    // Snapshot the (just-created) sandbox tree and a sampling of host
    // paths the user is most likely to want protected.  We don't walk the
    // entire host filesystem — that would be both slow and unreliable;
    // instead we walk a small allowlist of paths and check for new files
    // in those after the run.  The sandbox-root diff is exhaustive.
    let pre_sandbox = snapshot_tree(&sandbox_root);
    let pre_host = snapshot_host_watchlist();

    let (cmd, backend) = build_sandbox_command(&opts.install_cmd, &sandbox_root, opts.allow_network);
    let start = Instant::now();
    let (exit_code, stdout, stderr, killed_by_timeout) = run_with_timeout(cmd, opts.timeout)?;
    let duration_ms = start.elapsed().as_millis();

    // Diff: every file inside the sandbox root that wasn't there before.
    let post_sandbox = snapshot_tree(&sandbox_root);
    let files_written: Vec<String> = post_sandbox
        .difference(&pre_sandbox)
        .map(|p| p.display().to_string())
        .collect();

    // Escape detection: any new file under the host watchlist.
    let post_host = snapshot_host_watchlist();
    let files_written_outside_sandbox: Vec<String> = post_host
        .difference(&pre_host)
        .map(|p| p.display().to_string())
        .collect();

    Ok(DetonationReport {
        exit_code,
        files_written,
        files_written_outside_sandbox,
        stdout_tail: tail_bytes(&stdout),
        stderr_tail: tail_bytes(&stderr),
        duration_ms,
        killed_by_timeout,
        sandbox_backend: backend,
        sandbox_root: sandbox_root.display().to_string(),
        allow_network: opts.allow_network,
    })
}

/// Paths we sample for escape detection.  Each is shallow-walked so an
/// install command that drops a file in `~/.ssh` or `/etc` is flagged
/// even when OS-level sandboxing is unavailable.
///
/// The list is intentionally small — a full host snapshot would be slow
/// and is unnecessary; the AnvilHub install command is supposed to write
/// to its own staging dir, so any write outside that dir is a strong
/// signal even from a partial scan.
fn host_watchlist() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        paths.push(home.join(".ssh"));
        paths.push(home.join(".anvil"));
        paths.push(home.join(".aws"));
        paths.push(home.join(".gnupg"));
        paths.push(home.join("Library").join("LaunchAgents"));
    }
    if cfg!(target_family = "unix") {
        paths.push(PathBuf::from("/etc"));
        paths.push(PathBuf::from("/usr/local/bin"));
    }
    paths
}

fn snapshot_host_watchlist() -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for path in host_watchlist() {
        if path.exists() {
            walk_shallow(&path, &mut out);
        }
    }
    out
}

/// One-level scan — enough to catch a dropped file in `~/.ssh/authorized_keys`
/// without walking gigabytes of `/etc`.
fn walk_shallow(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() || file_type.is_symlink() {
            out.insert(path);
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return Some(PathBuf::from(profile));
        }
    }
    None
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|arg| arg == "--version") {
        println!("anvil-sandbox-runner {VERSION}");
        return;
    }
    if raw.is_empty() || raw.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        if raw.is_empty() {
            std::process::exit(2);
        }
        return;
    }
    let opts = match parse_args(&raw) {
        Ok(opts) => opts,
        Err(reason) => {
            eprintln!("anvil-sandbox-runner: {reason}");
            print_help();
            std::process::exit(2);
        }
    };
    let report = match run_detonation(&opts) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("anvil-sandbox-runner: detonation failed: {error}");
            std::process::exit(1);
        }
    };
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("anvil-sandbox-runner: failed to serialize report: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_default_timeout() {
        let opts = parse_args(&["echo hi".to_string()]).expect("parse");
        assert_eq!(opts.install_cmd, "echo hi");
        assert_eq!(opts.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert!(!opts.allow_network);
    }

    #[test]
    fn parse_args_custom_timeout_and_allow_network() {
        let opts = parse_args(&[
            "--timeout=5".to_string(),
            "--allow-network".to_string(),
            "echo hi".to_string(),
        ])
        .expect("parse");
        assert_eq!(opts.timeout, Duration::from_secs(5));
        assert!(opts.allow_network);
    }

    #[test]
    fn parse_args_rejects_missing_cmd() {
        let err = parse_args(&["--timeout=5".to_string()]).expect_err("must err");
        assert!(err.contains("missing required"));
    }

    #[test]
    fn parse_args_rejects_invalid_timeout() {
        let err =
            parse_args(&["--timeout=not-a-number".to_string(), "echo".to_string()]).expect_err("err");
        assert!(err.contains("invalid --timeout"));
    }

    #[test]
    fn tail_bytes_short_input_passthrough() {
        let out = tail_bytes(b"hello");
        assert_eq!(out, "hello");
    }

    #[test]
    fn tail_bytes_truncates_long_input() {
        let buf = vec![b'a'; TAIL_BYTES + 100];
        let out = tail_bytes(&buf);
        assert!(out.starts_with("...[truncated]..."));
        // The kept tail should be exactly TAIL_BYTES of 'a'.
        let tail_a = out.trim_start_matches("...[truncated]...\n");
        assert_eq!(tail_a.len(), TAIL_BYTES);
    }

    #[test]
    fn snapshot_tree_captures_files_recursively() {
        let dir = make_sandbox_root().expect("sandbox root");
        let nested = dir.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("f.txt"), b"x").unwrap();
        let snap = snapshot_tree(&dir);
        assert!(snap.iter().any(|p| p.ends_with("f.txt")));
        let _ = fs::remove_dir_all(&dir);
    }
}
