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
        .filter(|o| o.status.success()).map_or_else(|| "unknown".to_string(), |o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    println!("cargo:rustc-env=GIT_SHA={sha}");

    // Rerun if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
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
