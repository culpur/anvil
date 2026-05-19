//! Node.js 18+ probe.
//!
//! QMD (`@tobilu/qmd`) runs on Node.js and lists Node 18+ as a hard
//! requirement. The wizard's State A (Install) step probes Node first
//! so we surface "your Node is too old" before the npm install runs
//! and fails halfway through.

use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Ok { major: u32, raw: String },
    TooOld { major: u32, raw: String },
    Missing,
    Unknown(String),
}

impl Probe {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Ok { raw, .. } => format!("{raw} (>=18 ok)"),
            Self::TooOld { raw, .. } => format!("{raw} (too old, need >=18)"),
            Self::Missing => "not installed".to_string(),
            Self::Unknown(s) => format!("unrecognized: {s}"),
        }
    }

    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

#[must_use]
pub fn probe() -> Probe {
    let cmd = Command::new("node").arg("--version").output();
    let out = match cmd {
        Ok(o) if o.status.success() => o,
        Ok(_) => return Probe::Missing,
        Err(_) => return Probe::Missing,
    };
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_version_line(&line)
}

#[must_use]
pub fn parse_version_line(line: &str) -> Probe {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Probe::Missing;
    }
    let stripped = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let major_str = stripped
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("");
    let Ok(major) = major_str.parse::<u32>() else {
        return Probe::Unknown(line.to_string());
    };
    if major >= 18 {
        Probe::Ok {
            major,
            raw: trimmed.to_string(),
        }
    } else {
        Probe::TooOld {
            major,
            raw: trimmed.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallCommand {
    pub label: String,
    pub command: String,
    pub requires: Option<String>,
}

impl InstallCommand {
    #[must_use]
    pub fn is_available(&self) -> bool {
        match &self.requires {
            None => true,
            Some(bin) => crate::schedule::binary_on_path(bin),
        }
    }
}

#[must_use]
pub fn install_command_for_os() -> Vec<InstallCommand> {
    let mut out: Vec<InstallCommand> = Vec::new();
    if cfg!(target_os = "macos") {
        out.push(InstallCommand {
            label: "Homebrew".to_string(),
            command: "brew install node".to_string(),
            requires: Some("brew".to_string()),
        });
        out.push(InstallCommand {
            label: "MacPorts".to_string(),
            command: "sudo port install nodejs20".to_string(),
            requires: Some("port".to_string()),
        });
        out.push(InstallCommand {
            label: "nvm".to_string(),
            command: "nvm install 20 && nvm use 20".to_string(),
            requires: Some("nvm".to_string()),
        });
        out.push(InstallCommand {
            label: "manual download".to_string(),
            command: "open https://nodejs.org/".to_string(),
            requires: None,
        });
    } else if cfg!(target_os = "linux") {
        out.push(InstallCommand {
            label: "apt (Debian / Ubuntu)".to_string(),
            command: "sudo apt-get install -y nodejs npm".to_string(),
            requires: Some("apt-get".to_string()),
        });
        out.push(InstallCommand {
            label: "dnf (Fedora)".to_string(),
            command: "sudo dnf install -y nodejs npm".to_string(),
            requires: Some("dnf".to_string()),
        });
        out.push(InstallCommand {
            label: "pacman (Arch)".to_string(),
            command: "sudo pacman -S --noconfirm nodejs npm".to_string(),
            requires: Some("pacman".to_string()),
        });
        out.push(InstallCommand {
            label: "apk (Alpine)".to_string(),
            command: "sudo apk add --no-cache nodejs npm".to_string(),
            requires: Some("apk".to_string()),
        });
    } else if cfg!(target_os = "freebsd") {
        out.push(InstallCommand {
            label: "pkg".to_string(),
            command: "sudo pkg install -y node npm".to_string(),
            requires: Some("pkg".to_string()),
        });
    } else if cfg!(target_os = "netbsd") {
        out.push(InstallCommand {
            label: "pkgin".to_string(),
            command: "sudo pkgin -y install nodejs".to_string(),
            requires: Some("pkgin".to_string()),
        });
    } else if cfg!(target_os = "openbsd") {
        out.push(InstallCommand {
            label: "pkg_add".to_string(),
            command: "doas pkg_add node".to_string(),
            requires: Some("pkg_add".to_string()),
        });
    } else if cfg!(windows) {
        out.push(InstallCommand {
            label: "winget".to_string(),
            command: "winget install OpenJS.NodeJS.LTS".to_string(),
            requires: Some("winget".to_string()),
        });
        out.push(InstallCommand {
            label: "Chocolatey".to_string(),
            command: "choco install -y nodejs-lts".to_string(),
            requires: Some("choco".to_string()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_v18_ok() {
        let p = parse_version_line("v18.17.0");
        assert!(matches!(p, Probe::Ok { major: 18, .. }));
    }

    #[test]
    fn parse_v22_ok() {
        let p = parse_version_line("v22.4.1");
        assert!(matches!(p, Probe::Ok { major: 22, .. }));
    }

    #[test]
    fn parse_v16_too_old() {
        let p = parse_version_line("v16.20.2");
        assert!(matches!(p, Probe::TooOld { major: 16, .. }));
    }

    #[test]
    fn parse_v12_too_old() {
        let p = parse_version_line("v12.22.12");
        assert!(matches!(p, Probe::TooOld { major: 12, .. }));
    }

    #[test]
    fn parse_garbage_unknown() {
        let p = parse_version_line("Node version: hello");
        assert!(matches!(p, Probe::Unknown(_)));
    }

    #[test]
    fn parse_empty_missing() {
        assert!(matches!(parse_version_line(""), Probe::Missing));
        assert!(matches!(parse_version_line("   "), Probe::Missing));
    }

    #[test]
    fn parse_without_v_prefix_also_works() {
        let p = parse_version_line("18.17.0");
        assert!(matches!(p, Probe::Ok { major: 18, .. }));
    }

    #[test]
    fn describe_returns_non_empty_for_every_variant() {
        for v in &[
            Probe::Ok {
                major: 18,
                raw: "v18.17.0".to_string(),
            },
            Probe::TooOld {
                major: 12,
                raw: "v12.22.12".to_string(),
            },
            Probe::Missing,
            Probe::Unknown("?".to_string()),
        ] {
            assert!(!v.describe().is_empty());
        }
    }

    #[test]
    fn install_command_for_os_returns_at_least_one_row() {
        let cmds = install_command_for_os();
        if cfg!(any(
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            windows
        )) {
            assert!(!cmds.is_empty());
        }
    }

    #[test]
    fn install_command_is_available_falls_back_to_true_when_no_requirement() {
        let cmd = InstallCommand {
            label: "manual".to_string(),
            command: "open https://nodejs.org/".to_string(),
            requires: None,
        };
        assert!(cmd.is_available());
    }
}
