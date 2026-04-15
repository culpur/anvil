//! Network egress control — configurable domain allowlist for tool network access.
//!
//! Default policy: only AI provider APIs are allowed.
//! Configure via `~/.anvil/config.json` under `security.egress_allowlist`.

use std::collections::HashSet;

/// Default allowed domains (AI provider APIs + localhost).
const DEFAULT_ALLOWLIST: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "api.x.ai",
    "localhost",
    "127.0.0.1",
    "::1",
    // AnvilHub API
    "api.culpur.net",
    "anvilhub.culpur.net",
    "passage.culpur.net",
];

/// Egress policy for controlling outbound network access from tools.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    /// Allowed domains. Tools cannot access URLs outside this list.
    pub allowlist: HashSet<String>,
    /// Whether egress control is enabled. When false, all domains are allowed.
    pub enabled: bool,
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self {
            allowlist: DEFAULT_ALLOWLIST.iter().map(|s| (*s).to_string()).collect(),
            enabled: false, // disabled by default until user opts in
        }
    }
}

impl EgressPolicy {
    /// Check if a URL is allowed by the egress policy.
    #[must_use] 
    pub fn is_allowed(&self, url: &str) -> bool {
        if !self.enabled {
            return true;
        }
        // Extract domain from URL
        let domain = extract_domain(url);
        match domain {
            Some(d) => self.allowlist.contains(d) || self.allowlist.contains(&d.to_lowercase()),
            None => false,
        }
    }

    /// Add a domain to the allowlist.
    pub fn add_domain(&mut self, domain: &str) {
        self.allowlist.insert(domain.to_lowercase());
    }

    /// Remove a domain from the allowlist.
    pub fn remove_domain(&mut self, domain: &str) -> bool {
        self.allowlist.remove(&domain.to_lowercase())
    }

    /// Render the current policy as a human-readable string.
    #[must_use] 
    pub fn render_status(&self) -> String {
        let status = if self.enabled { "ENABLED" } else { "DISABLED" };
        let mut domains: Vec<_> = self.allowlist.iter().cloned().collect();
        domains.sort();
        let list = domains.join("\n    ");
        format!(
            "Network egress control: {status}\n  Allowed domains ({}):\n    {list}",
            domains.len()
        )
    }

    /// Load from config values (list of domain strings).
    #[must_use] 
    pub fn from_config(domains: &[String], enabled: bool) -> Self {
        let mut allowlist: HashSet<String> =
            DEFAULT_ALLOWLIST.iter().map(|s| (*s).to_string()).collect();
        allowlist.extend(domains.iter().cloned());
        Self { allowlist, enabled }
    }
}

/// Extract the domain (hostname) from a URL string.
fn extract_domain(url: &str) -> Option<&str> {
    let after_scheme = url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("wss://"))
        .or_else(|| url.strip_prefix("ws://"))
        .unwrap_or(url);

    let domain_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let domain_port = &after_scheme[..domain_end];
    // Strip port if present
    let domain = domain_port.split(':').next().unwrap_or(domain_port);
    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allows_providers() {
        let policy = EgressPolicy { enabled: true, ..Default::default() };
        assert!(policy.is_allowed("https://api.anthropic.com/v1/messages"));
        assert!(policy.is_allowed("https://api.openai.com/v1/chat/completions"));
        assert!(policy.is_allowed("http://localhost:11434/api/chat"));
    }

    #[test]
    fn blocks_unknown_domains() {
        let policy = EgressPolicy { enabled: true, ..Default::default() };
        assert!(!policy.is_allowed("https://evil.com/exfiltrate"));
        assert!(!policy.is_allowed("https://attacker.io/callback"));
    }

    #[test]
    fn disabled_allows_everything() {
        let policy = EgressPolicy::default();
        assert!(policy.is_allowed("https://anything.com/any/path"));
    }

    #[test]
    fn add_remove_domain() {
        let mut policy = EgressPolicy { enabled: true, ..Default::default() };
        assert!(!policy.is_allowed("https://custom.example.com/api"));
        policy.add_domain("custom.example.com");
        assert!(policy.is_allowed("https://custom.example.com/api"));
        policy.remove_domain("custom.example.com");
        assert!(!policy.is_allowed("https://custom.example.com/api"));
    }

    #[test]
    fn extract_domain_works() {
        assert_eq!(extract_domain("https://api.anthropic.com/v1/messages"), Some("api.anthropic.com"));
        assert_eq!(extract_domain("http://localhost:11434/api/chat"), Some("localhost"));
        assert_eq!(extract_domain("wss://api.culpur.net/v1/relay"), Some("api.culpur.net"));
    }
}
