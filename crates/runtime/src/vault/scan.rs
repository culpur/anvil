//! Credential scanner — detects API keys, SSH keys, TLS certs, and other secrets
//! across environment variables, dotfiles, project files, and common config locations.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// A detected credential with metadata about where it was found.
#[derive(Debug, Clone)]
pub struct DetectedCredential {
    /// Human-readable type (e.g., "OpenAI API Key", "SSH Private Key").
    pub kind: String,
    /// Suggested vault label.
    pub label: String,
    /// The actual secret value.
    pub value: String,
    /// Where it was found (e.g., "env:OPENAI_API_KEY", "file:~/.ssh/id_ed25519").
    pub source: String,
    /// Masked preview for display (e.g., "sk-proj-***...4f2k").
    pub masked: String,
    /// Whether this is a known provider key that should be auto-stored.
    pub auto_store: bool,
}

/// Known credential patterns for environment variable scanning.
const ENV_PATTERNS: &[(&str, &str, bool)] = &[
    ("ANTHROPIC_API_KEY", "Anthropic API Key", true),
    ("OPENAI_API_KEY", "OpenAI API Key", true),
    ("XAI_API_KEY", "xAI API Key", true),
    ("GITHUB_TOKEN", "GitHub Token", false),
    ("GITHUB_PAT", "GitHub PAT", false),
    ("AWS_ACCESS_KEY_ID", "AWS Access Key", false),
    ("AWS_SECRET_ACCESS_KEY", "AWS Secret Key", false),
    ("DOCKER_TOKEN", "Docker Token", false),
    ("SLACK_TOKEN", "Slack Token", false),
    ("SLACK_BOT_TOKEN", "Slack Bot Token", false),
    ("STRIPE_SECRET_KEY", "Stripe Secret Key", false),
    ("STRIPE_TEST_KEY", "Stripe Test Key", false),
    ("SENDGRID_API_KEY", "SendGrid API Key", false),
    ("CLOUDFLARE_API_TOKEN", "Cloudflare API Token", false),
    ("DIGITAL_OCEAN_TOKEN", "DigitalOcean Token", false),
    ("HEROKU_API_KEY", "Heroku API Key", false),
    ("NPM_TOKEN", "npm Token", false),
    ("PYPI_TOKEN", "PyPI Token", false),
    ("DATABASE_URL", "Database URL", false),
];

/// Mask a secret value for display.
fn mask_secret(s: &str) -> String {
    if s.len() <= 8 {
        return "****".to_string();
    }
    let prefix: String = s.chars().take(4).collect();
    let suffix: String = s.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{prefix}***...{suffix}")
}

/// Generate a vault label from an env var name or file path.
fn label_from_source(source: &str) -> String {
    source
        .replace("env:", "")
        .replace("file:", "")
        .replace('/', "-")
        .replace('~', "home")
        .replace('.', "-")
        .to_lowercase()
        .trim_matches('-')
        .to_string()
}

/// Quick scan: environment variables only. Fast enough for session start (<50ms).
pub fn quick_scan() -> Vec<DetectedCredential> {
    let mut found = Vec::new();
    for &(var_name, kind, auto_store) in ENV_PATTERNS {
        if let Ok(value) = env::var(var_name) {
            if !value.is_empty() && value.len() > 8 {
                found.push(DetectedCredential {
                    kind: kind.to_string(),
                    label: var_name.to_lowercase().replace('_', "-"),
                    value: value.clone(),
                    source: format!("env:{var_name}"),
                    masked: mask_secret(&value),
                    auto_store,
                });
            }
        }
    }
    found
}

/// Full scan: environment variables + dotfiles + project files + SSH keys + TLS certs.
pub fn full_scan(project_root: Option<&Path>) -> Vec<DetectedCredential> {
    let mut found = quick_scan();
    let home = dirs_next::home_dir();

    // Scan .env files in project root
    if let Some(root) = project_root {
        for name in &[".env", ".env.local", ".env.production", ".env.development"] {
            let path = root.join(name);
            if path.exists() {
                found.extend(scan_env_file(&path));
            }
        }
    }

    if let Some(ref home) = home {
        // Scan SSH keys
        let ssh_dir = home.join(".ssh");
        if ssh_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&ssh_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_private_key_file(&path) {
                        if let Ok(content) = fs::read_to_string(&path) {
                            if content.contains("PRIVATE KEY") {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                found.push(DetectedCredential {
                                    kind: "SSH Private Key".to_string(),
                                    label: format!("ssh-{name}"),
                                    value: content,
                                    source: format!("file:{}", path.display()),
                                    masked: format!("[SSH key: {name}]"),
                                    auto_store: false,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Scan common credential files
        let cred_files: Vec<(PathBuf, &str)> = vec![
            (home.join(".aws/credentials"), "AWS Credentials"),
            (home.join(".npmrc"), "npm Config"),
            (home.join(".pypirc"), "PyPI Config"),
        ];
        for (path, kind) in cred_files {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    // Look for key-like values
                    for line in content.lines() {
                        let line = line.trim();
                        if let Some((key, val)) = line.split_once('=') {
                            let key = key.trim();
                            let val = val.trim().trim_matches('"').trim_matches('\'');
                            if val.len() > 16 && looks_like_secret(key) {
                                found.push(DetectedCredential {
                                    kind: format!("{kind} ({key})"),
                                    label: label_from_source(&format!("{kind}-{key}")),
                                    value: val.to_string(),
                                    source: format!("file:{}", path.display()),
                                    masked: mask_secret(val),
                                    auto_store: false,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Scan for TLS certificates and keys in common locations
    if let Some(ref home) = home {
        for dir_name in &[".ssl", ".tls", "certs"] {
            let dir = home.join(dir_name);
            if dir.is_dir() {
                found.extend(scan_tls_directory(&dir));
            }
        }
    }
    if let Some(root) = project_root {
        let certs_dir = root.join("certs");
        if certs_dir.is_dir() {
            found.extend(scan_tls_directory(&certs_dir));
        }
    }

    // Deduplicate by value hash
    let mut seen = HashMap::new();
    found.retain(|cred| {
        let hash = cred.value.len().to_string() + &cred.value[..4.min(cred.value.len())];
        seen.insert(hash, true).is_none()
    });

    found
}

/// Scan an .env file for credential-like values.
fn scan_env_file(path: &Path) -> Vec<DetectedCredential> {
    let mut found = Vec::new();
    if let Ok(content) = fs::read_to_string(path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some((key, val)) = line.split_once('=') {
                let key = key.trim().trim_start_matches("export ");
                let val = val.trim().trim_matches('"').trim_matches('\'');
                if val.len() > 16 && looks_like_secret(key) {
                    let is_provider = matches!(
                        key,
                        "ANTHROPIC_API_KEY" | "OPENAI_API_KEY" | "XAI_API_KEY"
                    );
                    found.push(DetectedCredential {
                        kind: format!("Env ({key})"),
                        label: key.to_lowercase().replace('_', "-"),
                        value: val.to_string(),
                        source: format!("file:{}", path.display()),
                        masked: mask_secret(val),
                        auto_store: is_provider,
                    });
                }
            }
        }
    }
    found
}

/// Check if a filename looks like a private key.
fn is_private_key_file(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    (name.starts_with("id_") && !name.ends_with(".pub"))
        || ext == "pem"
        || ext == "key"
        || ext == "p12"
        || ext == "pfx"
}

/// Scan a directory for TLS cert/key files.
fn scan_tls_directory(dir: &Path) -> Vec<DetectedCredential> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().unwrap_or_default().to_string_lossy();
            if matches!(ext.as_ref(), "pem" | "key" | "crt" | "p12" | "pfx") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if content.contains("PRIVATE KEY") {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        found.push(DetectedCredential {
                            kind: "TLS Private Key".to_string(),
                            label: format!("tls-{name}"),
                            value: content,
                            source: format!("file:{}", path.display()),
                            masked: format!("[TLS key: {name}]"),
                            auto_store: false,
                        });
                    } else if content.contains("CERTIFICATE") {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        found.push(DetectedCredential {
                            kind: "TLS Certificate".to_string(),
                            label: format!("tls-cert-{name}"),
                            value: content,
                            source: format!("file:{}", path.display()),
                            masked: format!("[TLS cert: {name}]"),
                            auto_store: false,
                        });
                    }
                }
            }
        }
    }
    found
}

/// Heuristic: does a key name look like it holds a secret?
fn looks_like_secret(key: &str) -> bool {
    let k = key.to_uppercase();
    k.contains("KEY") || k.contains("SECRET") || k.contains("TOKEN")
        || k.contains("PASSWORD") || k.contains("PASS") || k.contains("AUTH")
        || k.contains("CREDENTIAL") || k.contains("API_") || k.contains("DATABASE_URL")
        || k.contains("PRIVATE")
}
