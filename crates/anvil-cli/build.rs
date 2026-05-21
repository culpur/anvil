// Pulled in as a separate file so we can keep the clap-based CLI surface
// description outside of the runtime crate (clap is a build-only dep).  See
// build_cli_spec.rs for the full surface.
include!("build_cli_spec.rs");

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Set build date to today
    let date = chrono_lite_date();
    println!("cargo:rustc-env=BUILD_DATE={date}");

    // Set build target
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=TARGET={target}");

    // Set git SHA
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || "unknown".to_string(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        );
    println!("cargo:rustc-env=GIT_SHA={sha}");

    // Rerun if git HEAD or the ref it points at changes. .git/HEAD only changes
    // on branch switches; commits update refs/heads/<branch>, so watch both.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if let Ok(head) = std::fs::read_to_string("../../.git/HEAD") {
        if let Some(ref_path) = head.strip_prefix("ref: ").map(str::trim) {
            println!("cargo:rerun-if-changed=../../.git/{ref_path}");
        }
    }

    // Re-render the manpage whenever the CLI surface description or its
    // hand-curated tail changes.
    println!("cargo:rerun-if-changed=build_cli_spec.rs");
    println!("cargo:rerun-if-changed=../../man/anvil.1.tail");

    generate_manpage(&date);
}

/// Render the clap-described CLI surface to `OUT_DIR/anvil.1`, then append
/// `man/anvil.1.tail` (hand-curated free-form sections that clap can't model
/// — EXAMPLES, ENVIRONMENT, FILES, VAULT, NAVIGATION, SLASH COMMANDS, ...).
///
/// The runtime `--gen-man` handler simply prints this file via include_str!.
fn generate_manpage(build_date: &str) {
    let out_dir =
        std::env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR for build scripts");
    let out_path = PathBuf::from(&out_dir).join("anvil.1");

    let cmd = build_cli();
    let man = clap_mangen::Man::new(cmd)
        .title("ANVIL")
        .section("1")
        .date(build_date.to_string())
        .source(format!("Anvil {}", env!("CARGO_PKG_VERSION")))
        .manual("User Commands");

    let mut buffer: Vec<u8> = Vec::new();
    man.render(&mut buffer)
        .expect("clap_mangen render failed — check build_cli_spec.rs");

    // Append the hand-curated tail (EXAMPLES, ENVIRONMENT, FILES, ...).  We
    // resolve from CARGO_MANIFEST_DIR so this works under workspace builds
    // (cwd is the workspace root) and per-crate builds alike.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("cargo always sets CARGO_MANIFEST_DIR for build scripts");
    let tail_path = PathBuf::from(&manifest_dir)
        .join("..")
        .join("..")
        .join("man")
        .join("anvil.1.tail");

    if let Ok(tail) = std::fs::read_to_string(&tail_path) {
        buffer.extend_from_slice(b"\n");
        buffer.extend_from_slice(tail.as_bytes());
    } else {
        println!(
            "cargo:warning=man/anvil.1.tail not found at {} — manpage will be auto-section only",
            tail_path.display()
        );
    }

    std::fs::write(&out_path, &buffer).expect("failed to write OUT_DIR/anvil.1");
}

/// Get current date as YYYY-MM-DD without pulling in chrono crate
fn chrono_lite_date() -> String {
    let output = Command::new("date")
        .args(["+%Y-%m-%d"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    output.unwrap_or_else(|| "unknown".to_string())
}
