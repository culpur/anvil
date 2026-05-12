//! `anvil --uninstall` / `anvil uninstall` — remove the Anvil binary and
//! optionally the `~/.anvil/` data directory.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn prompt_yn(question: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("  {question} {hint} ");
    let _ = io::stdout().flush();
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
    let answer = buf.trim().to_ascii_lowercase();
    match answer.as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        "" => default_yes,
        _ => default_yes,
    }
}

fn anvil_home() -> PathBuf {
    if let Ok(h) = std::env::var("ANVIL_CONFIG_HOME") {
        return PathBuf::from(h);
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join(".anvil")
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run `anvil --uninstall`.
///
/// Exit codes:
/// - 0  uninstall complete
/// - 1  user declined / operation failed
pub(crate) fn run_uninstall() {
    println!();
    println!("\x1b[1mAnvil uninstall\x1b[0m");
    println!();

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  \x1b[31mError:\x1b[0m cannot resolve binary path: {e}");
            std::process::exit(1);
        }
    };

    let anvil_home = anvil_home();

    println!("  This will remove:");
    println!("    Binary : {}", current_exe.display());
    if anvil_home.exists() {
        println!("    Data   : {} (optional)", anvil_home.display());
    }
    println!();

    if !prompt_yn("Proceed with uninstall?", false) {
        println!("  Uninstall cancelled.");
        std::process::exit(0);
    }

    let mut removed: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // Remove the binary
    match fs::remove_file(&current_exe) {
        Ok(()) => {
            removed.push(current_exe.display().to_string());
        }
        Err(e) => {
            errors.push(format!("{}: {e}", current_exe.display()));
        }
    }

    // Optionally remove ~/.anvil/
    if anvil_home.exists() {
        println!();
        if prompt_yn(
            &format!("Also remove {} (vault, config, sessions)?", anvil_home.display()),
            false,
        ) {
            match fs::remove_dir_all(&anvil_home) {
                Ok(()) => {
                    removed.push(anvil_home.display().to_string());
                }
                Err(e) => {
                    errors.push(format!("{}: {e}", anvil_home.display()));
                }
            }
        } else {
            println!("  Keeping {}.", anvil_home.display());
        }
    }

    // Summary
    println!();
    if !removed.is_empty() {
        println!("  \x1b[32m\u{2714}\x1b[0m  Removed:");
        for path in &removed {
            println!("       {path}");
        }
    }
    if !errors.is_empty() {
        println!("  \x1b[31m\u{2718}\x1b[0m  Errors:");
        for err in &errors {
            println!("       {err}");
        }
        println!();
        eprintln!("  Uninstall completed with errors. You may need to remove remaining files manually.");
        std::process::exit(1);
    }

    println!();
    println!("  Anvil has been uninstalled.");
    println!("  Thank you for using Anvil. \u{1F44B}");
    println!();
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial(anvil_config_home)]
    fn anvil_home_from_env() {
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", "/tmp/anvil-test-home") };
        let home = anvil_home();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ANVIL_CONFIG_HOME", v),
                None => std::env::remove_var("ANVIL_CONFIG_HOME"),
            }
        }
        assert_eq!(home, PathBuf::from("/tmp/anvil-test-home"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn anvil_home_fallback_to_dot_anvil() {
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
        let home = anvil_home();
        if let Some(v) = prev {
            unsafe { std::env::set_var("ANVIL_CONFIG_HOME", v) };
        }
        assert_eq!(home.file_name().and_then(|n| n.to_str()), Some(".anvil"));
    }

    /// Simulate the uninstall binary-removal step without touching real paths.
    #[test]
    fn remove_binary_roundtrip() {
        use std::io::Write as IoWrite;

        let tmp = std::env::temp_dir()
            .join(format!("anvil-uninstall-test-{}", std::process::id()));
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(b"fake anvil binary").unwrap();
        drop(f);

        assert!(tmp.exists());
        fs::remove_file(&tmp).expect("remove_file should succeed");
        assert!(!tmp.exists(), "binary should be gone after remove_file");
    }
}
