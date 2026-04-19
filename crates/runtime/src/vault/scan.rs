//! Credential scanner — detects API keys, SSH keys, TLS certs, and other secrets
//! across environment variables, dotfiles, project files, and common config locations.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use super::CredentialType;

/// A detected credential with metadata about where it was found.
#[derive(Debug, Clone)]
pub struct DetectedCredential {
    /// Human-readable type (e.g., "`OpenAI` API Key", "SSH Private Key").
    pub kind: String,
    /// The typed credential category for vault storage.
    pub credential_type: CredentialType,
    /// Suggested vault label.
    pub label: String,
    /// The actual secret value.
    pub value: String,
    /// Where it was found (e.g., "`env:OPENAI_API_KEY`", "file:~/.`ssh/id_ed25519`").
    pub source: String,
    /// Masked preview for display (e.g., "sk-proj-***...4f2k").
    pub masked: String,
    /// Whether this is a known provider key that should be auto-stored.
    pub auto_store: bool,
    /// Type-specific metadata parsed from the detected secret.
    pub metadata: serde_json::Value,
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
#[must_use] 
pub fn quick_scan() -> Vec<DetectedCredential> {
    let mut found = Vec::new();
    for &(var_name, kind, auto_store) in ENV_PATTERNS {
        if let Ok(value) = env::var(var_name)
            && !value.is_empty() && value.len() > 8 {
                found.push(DetectedCredential {
                    kind: kind.to_string(),
                    label: var_name.to_lowercase().replace('_', "-"),
                    value: value.clone(),
                    source: format!("env:{var_name}"),
                    masked: mask_secret(&value),
                    auto_store,
                    credential_type: CredentialType::ApiKey,
                    metadata: serde_json::Value::Object(serde_json::Map::new()),
                });
            }
    }
    found
}

/// Full scan: environment variables + dotfiles + project files + SSH keys + TLS certs.
#[must_use] 
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
        if ssh_dir.is_dir()
            && let Ok(entries) = fs::read_dir(&ssh_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_private_key_file(&path)
                        && let Ok(content) = fs::read_to_string(&path)
                            && content.contains("PRIVATE KEY") {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                found.push(DetectedCredential {
                                    kind: "SSH Private Key".to_string(),
                                    label: format!("ssh-{name}"),
                                    value: content,
                                    source: format!("file:{}", path.display()),
                                    masked: format!("[SSH key: {name}]"),
                                    auto_store: false,
                                    credential_type: CredentialType::SshKey,
                                    metadata: serde_json::Value::Object(serde_json::Map::new()),
                                });
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
            if path.exists()
                && let Ok(content) = fs::read_to_string(&path) {
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
                                    credential_type: CredentialType::SecretText,
                                    metadata: serde_json::Value::Object(serde_json::Map::new()),
                                });
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
                        credential_type: CredentialType::SecretText,
                        metadata: serde_json::Value::Object(serde_json::Map::new()),
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
            if matches!(ext.as_ref(), "pem" | "key" | "crt" | "p12" | "pfx")
                && let Ok(content) = fs::read_to_string(&path) {
                    if content.contains("PRIVATE KEY") {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        found.push(DetectedCredential {
                            kind: "TLS Private Key".to_string(),
                            label: format!("tls-{name}"),
                            value: content,
                            source: format!("file:{}", path.display()),
                            masked: format!("[TLS key: {name}]"),
                            auto_store: false,
                            credential_type: CredentialType::TlsCert,
                            metadata: serde_json::Value::Object(serde_json::Map::new()),
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
                            credential_type: CredentialType::TlsCert,
                            metadata: serde_json::Value::Object(serde_json::Map::new()),
                        });
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

// ─── Sensitivity Classification for Memory Tiers ────────────────────────────

/// Classification level for content discovered during sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitivityLevel {
    /// Credential — auto-vault immediately (API keys, passwords, tokens, SSH keys, DB URLs)
    Credential,
    /// Infrastructure — encrypted private project memory (hostnames, IPs, ports, deploy paths)
    Infrastructure,
    /// Knowledge — safe for nomination → review → CLAUDE.md/QMD promotion
    Knowledge,
}

/// Known credential patterns in free text (not just env var names).
const TEXT_CREDENTIAL_PATTERNS: &[&str] = &[
    "sk-ant-",       // Anthropic
    "sk-proj-",      // OpenAI project
    "sk-svcacct-",   // OpenAI service account
    "sk-",           // Generic OpenAI
    "xai-",          // xAI
    "ghp_",          // GitHub PAT
    "gho_",          // GitHub OAuth
    "ghs_",          // GitHub App
    "github_pat_",   // GitHub fine-grained
    "glpat-",        // GitLab
    "AKIA",          // AWS access key
    "AIza",          // Google API key
    "Bearer ",       // Bearer tokens
    "Basic ",        // Basic auth
    "-----BEGIN",    // PEM-encoded keys/certs
];

/// Infrastructure patterns that should go to private project memory.
const INFRA_PATTERNS: &[&str] = &[
    ".internal",
    ".local",
    ".armored.",
    ".culpur.",
    "bastion",
    "jump",
    "/opt/projects/",
    "/opt/ems/",
    "/srv/",
    "/var/www/",
    "sudo docker exec",
    "pm2 restart",
    "systemctl",
];

/// Classify a piece of text content for the appropriate memory tier.
#[must_use]
pub fn classify_learning(content: &str) -> SensitivityLevel {
    // Check for credential patterns first (highest sensitivity)
    if detect_credential_in_text(content) {
        return SensitivityLevel::Credential;
    }
    // Check for infrastructure details
    if contains_infrastructure_details(content) {
        return SensitivityLevel::Infrastructure;
    }
    // Default: safe knowledge
    SensitivityLevel::Knowledge
}

/// Detect if free text contains what looks like a credential or secret.
#[must_use]
pub fn detect_credential_in_text(text: &str) -> bool {
    // Check for known API key prefixes
    for pattern in TEXT_CREDENTIAL_PATTERNS {
        if text.contains(pattern) {
            return true;
        }
    }
    // Check for password-like assignments
    let lower = text.to_lowercase();
    if (lower.contains("password") || lower.contains("passwd") || lower.contains("pwd"))
        && (lower.contains('=') || lower.contains(':'))
    {
        return true;
    }
    // Check for database URLs with credentials
    if text.contains("://") && text.contains('@') {
        let has_scheme = text.contains("postgres://")
            || text.contains("postgresql://")
            || text.contains("mysql://")
            || text.contains("mongodb://")
            || text.contains("redis://")
            || text.contains("amqp://");
        if has_scheme {
            return true;
        }
    }
    // Check for hex-encoded secrets (40+ char hex strings)
    let words: Vec<&str> = text.split_whitespace().collect();
    for word in &words {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
        if clean.len() >= 40 && clean.chars().all(|c| c.is_ascii_hexdigit()) {
            return true;
        }
    }
    false
}

/// Detect if text contains infrastructure details (IPs, hostnames, ports, deploy paths).
#[must_use]
pub fn contains_infrastructure_details(text: &str) -> bool {
    // Check known infrastructure patterns
    let lower = text.to_lowercase();
    for pattern in INFRA_PATTERNS {
        if lower.contains(&pattern.to_lowercase()) {
            return true;
        }
    }
    // Check for IPv4 addresses (not localhost, not example ranges)
    if contains_real_ipv4(text) {
        return true;
    }
    // Check for SSH connection strings (user@host)
    if text.contains('@') && text.contains("ssh ") {
        return true;
    }
    // Check for port patterns in context (host:port)
    if contains_host_port(text) {
        return true;
    }
    false
}

/// Check if text contains real (non-example, non-localhost) IPv4 addresses.
fn contains_real_ipv4(text: &str) -> bool {
    // Simple regex-free IPv4 detection
    for word in text.split(|c: char| !c.is_ascii_digit() && c != '.') {
        let parts: Vec<&str> = word.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
            let octets: Vec<u8> = parts.iter().filter_map(|p| p.parse().ok()).collect();
            if octets.len() == 4 {
                // Skip localhost, link-local, example ranges
                if octets[0] == 127 || octets[0] == 0 || (octets[0] == 169 && octets[1] == 254) {
                    continue;
                }
                // Skip documentation ranges (192.0.2.x, 198.51.100.x, 203.0.113.x)
                if (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                    || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                    || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                {
                    continue;
                }
                return true;
            }
        }
    }
    false
}

/// Check if text contains host:port patterns.
fn contains_host_port(text: &str) -> bool {
    for word in text.split_whitespace() {
        if let Some((host, port_str)) = word.rsplit_once(':') {
            if let Ok(port) = port_str.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u16>() {
                if port > 0 && port < 65535 && !host.is_empty() && host.contains('.') {
                    // Skip common safe patterns
                    if host == "localhost" || host == "127.0.0.1" || host == "0.0.0.0" {
                        continue;
                    }
                    return true;
                }
            }
        }
    }
    false
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_api_keys_as_credential() {
        assert_eq!(classify_learning("The key is sk-ant-api03-abc123"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("Set OPENAI_API_KEY=sk-proj-abc123"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("ghp_1234567890abcdef1234567890abcdef12345678"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("Use xai-abc123 for the API"), SensitivityLevel::Credential);
    }

    #[test]
    fn classifies_database_urls_as_credential() {
        assert_eq!(classify_learning("postgres://admin:secret@db.example.com:5432/prod"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("mongodb://user:pass@mongo.internal:27017/app"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("redis://default:password@cache.example.com:6379"), SensitivityLevel::Credential);
    }

    #[test]
    fn classifies_passwords_as_credential() {
        assert_eq!(classify_learning("The database password is: Xk9#mP2$vL"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("password=supersecret123"), SensitivityLevel::Credential);
    }

    #[test]
    fn classifies_pem_keys_as_credential() {
        assert_eq!(classify_learning("-----BEGIN RSA PRIVATE KEY-----"), SensitivityLevel::Credential);
        assert_eq!(classify_learning("-----BEGIN OPENSSH PRIVATE KEY-----"), SensitivityLevel::Credential);
    }

    #[test]
    fn classifies_ipv4_as_infrastructure() {
        assert_eq!(classify_learning("Deploy to 10.0.70.80"), SensitivityLevel::Infrastructure);
        assert_eq!(classify_learning("The server is at 88.198.71.105"), SensitivityLevel::Infrastructure);
    }

    #[test]
    fn classifies_internal_hostnames_as_infrastructure() {
        assert_eq!(classify_learning("SSH to bastion.example.internal"), SensitivityLevel::Infrastructure);
        assert_eq!(classify_learning("The bastion host handles SSH"), SensitivityLevel::Infrastructure);
    }

    #[test]
    fn classifies_deploy_commands_as_infrastructure() {
        assert_eq!(classify_learning("Run sudo docker exec wordpress-wordpress-1"), SensitivityLevel::Infrastructure);
        assert_eq!(classify_learning("pm2 restart anvilhub-web"), SensitivityLevel::Infrastructure);
    }

    #[test]
    fn classifies_safe_knowledge_correctly() {
        assert_eq!(classify_learning("This project uses Prisma with PostgreSQL"), SensitivityLevel::Knowledge);
        assert_eq!(classify_learning("Tests are in the __tests__/ directory"), SensitivityLevel::Knowledge);
        assert_eq!(classify_learning("The API follows RESTful conventions"), SensitivityLevel::Knowledge);
        assert_eq!(classify_learning("Use cargo test to run all tests"), SensitivityLevel::Knowledge);
    }

    #[test]
    fn localhost_is_not_infrastructure() {
        assert_eq!(classify_learning("Connect to localhost:3000"), SensitivityLevel::Knowledge);
        assert_eq!(classify_learning("The server runs on 127.0.0.1:8080"), SensitivityLevel::Knowledge);
    }

    #[test]
    fn documentation_ips_are_not_infrastructure() {
        assert_eq!(classify_learning("Example: 192.0.2.1 is a documentation IP"), SensitivityLevel::Knowledge);
        assert_eq!(classify_learning("Use 203.0.113.5 in your examples"), SensitivityLevel::Knowledge);
    }

    #[test]
    fn detect_credential_catches_hex_secrets() {
        // 40-char hex strings (like SHA1 hashes used as tokens)
        assert!(detect_credential_in_text("token: aabbccdd11223344556677889900aabbccddee11"));
    }

    #[test]
    fn detect_credential_skips_short_hex() {
        // Short hex strings are not credentials
        assert!(!detect_credential_in_text("commit abc123"));
    }
}
