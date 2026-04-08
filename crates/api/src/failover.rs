use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::providers::ProviderKind;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FailoverEntry {
    pub model: String,
    pub provider_kind: ProviderKind,
    pub priority: u32,
    pub max_daily_tokens: Option<u64>,
    pub max_daily_cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct UsageBudget {
    pub tokens_used_today: u64,
    pub cost_today_usd: f64,
    /// When daily counters were last reset (seconds since UNIX epoch).
    pub last_reset_secs: u64,
}

impl UsageBudget {
    fn new() -> Self {
        Self {
            tokens_used_today: 0,
            cost_today_usd: 0.0,
            last_reset_secs: unix_now_secs(),
        }
    }

    /// Reset if the calendar day has changed since last reset.
    fn maybe_reset(&mut self) {
        let now = unix_now_secs();
        let day_secs = 86_400_u64;
        if now.saturating_sub(self.last_reset_secs) >= day_secs {
            self.tokens_used_today = 0;
            self.cost_today_usd = 0.0;
            self.last_reset_secs = now;
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailoverConfig {
    pub chain: Vec<FailoverEntry>,
    pub auto_failover: bool,
    /// Default cooldown after a rate-limit hit if no `Retry-After` header.
    pub cooldown_seconds: u64,
    pub notify_on_failover: bool,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            chain: Vec::new(),
            auto_failover: true,
            cooldown_seconds: 60,
            notify_on_failover: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailoverEvent {
    pub kind: FailoverEventKind,
    pub from_model: String,
    pub to_model: Option<String>,
    pub retry_in_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverEventKind {
    RateLimited,
    BudgetExceeded,
    Recovered,
}

// ---------------------------------------------------------------------------
// FailoverChain — manages the ordered provider chain with cooldowns & budgets
// ---------------------------------------------------------------------------

pub struct FailoverChain {
    config: FailoverConfig,
    /// Sorted entries (lowest priority number first = highest priority).
    entries: Vec<FailoverEntry>,
    /// Index of the currently active provider in `entries`.
    active_index: usize,
    /// entry-index → when the rate-limit cooldown expires.
    rate_limit_cooldowns: HashMap<usize, Instant>,
    /// entry-index → daily usage budget tracking.
    usage_budgets: HashMap<usize, UsageBudget>,
}

impl FailoverChain {
    #[must_use]
    pub fn new(config: FailoverConfig) -> Self {
        let mut entries = config.chain.clone();
        entries.sort_by_key(|e| e.priority);
        Self {
            config,
            entries,
            active_index: 0,
            rate_limit_cooldowns: HashMap::new(),
            usage_budgets: HashMap::new(),
        }
    }

    /// Load from `~/.anvil/failover.json` and fall back to an empty chain.
    #[must_use]
    pub fn from_config_file() -> Self {
        let config = load_failover_config().unwrap_or_default();
        Self::new(config)
    }

    // ------------------------------------------------------------------
    // Querying
    // ------------------------------------------------------------------

    /// Return the model string for the currently active provider.
    #[must_use]
    pub fn active_model(&self) -> Option<&str> {
        self.entries.get(self.active_index).map(|e| e.model.as_str())
    }

    /// Decide which provider to use. Skips rate-limited and over-budget
    /// entries starting from the preferred primary.  Returns the index of
    /// the selected entry, or `None` if all are unavailable.
    #[must_use]
    pub fn select_provider(&mut self) -> Option<usize> {
        // Try to return to the primary if its cooldown has expired.
        self.maybe_recover_primary();

        // Find the first available entry from priority order.
        for idx in 0..self.entries.len() {
            if self.entry_is_available(idx) {
                self.active_index = idx;
                return Some(idx);
            }
        }
        None
    }

    // ------------------------------------------------------------------
    // Failure signalling
    // ------------------------------------------------------------------

    /// Signal that the provider at `index` returned a 429 rate-limit response.
    /// Returns a `FailoverEvent` to display if notification is enabled.
    pub fn on_rate_limited(
        &mut self,
        index: usize,
        retry_after_secs: Option<u64>,
    ) -> Option<FailoverEvent> {
        let cooldown = retry_after_secs.unwrap_or(self.config.cooldown_seconds);
        self.rate_limit_cooldowns
            .insert(index, Instant::now() + Duration::from_secs(cooldown));

        let from_model = self.entries.get(index)?.model.clone();

        // Advance to next available.
        let next_idx = self.next_available_after(index)?;
        self.active_index = next_idx;
        let to_model = self.entries.get(next_idx).map(|e| e.model.clone());

        if self.config.notify_on_failover {
            Some(FailoverEvent {
                kind: FailoverEventKind::RateLimited,
                from_model,
                to_model,
                retry_in_secs: Some(cooldown),
            })
        } else {
            None
        }
    }

    /// Signal that the provider at `index` has exceeded its budget.
    pub fn on_budget_exceeded(&mut self, index: usize) -> Option<FailoverEvent> {
        let from_model = self.entries.get(index)?.model.clone();
        let next_idx = self.next_available_after(index)?;
        self.active_index = next_idx;
        let to_model = self.entries.get(next_idx).map(|e| e.model.clone());

        if self.config.notify_on_failover {
            Some(FailoverEvent {
                kind: FailoverEventKind::BudgetExceeded,
                from_model,
                to_model,
                retry_in_secs: None,
            })
        } else {
            None
        }
    }

    // ------------------------------------------------------------------
    // Success accounting
    // ------------------------------------------------------------------

    /// Record token usage for the provider at `index`.
    pub fn record_usage(&mut self, index: usize, tokens: u64, cost_usd: f64) {
        let budget = self
            .usage_budgets
            .entry(index)
            .or_insert_with(UsageBudget::new);
        budget.maybe_reset();
        budget.tokens_used_today += tokens;
        budget.cost_today_usd += cost_usd;
    }

    // ------------------------------------------------------------------
    // Slash-command helpers
    // ------------------------------------------------------------------

    /// Clear all cooldowns and reset daily budgets.
    pub fn reset(&mut self) {
        self.rate_limit_cooldowns.clear();
        self.usage_budgets.clear();
        self.active_index = 0;
    }

    /// Add a new entry to the chain (or update if model already exists).
    pub fn add_entry(&mut self, entry: FailoverEntry) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.model == entry.model) {
            *existing = entry;
        } else {
            self.entries.push(entry);
            self.entries.sort_by_key(|e| e.priority);
        }
        // Sync config chain.
        self.config.chain = self.entries.clone();
    }

    /// Remove an entry by model name.
    pub fn remove_entry(&mut self, model: &str) {
        self.entries.retain(|e| e.model != model);
        self.config.chain = self.entries.clone();
        self.active_index = self.active_index.min(self.entries.len().saturating_sub(1));
    }

    /// Return the model string at the given priority index (0 = highest priority).
    #[must_use]
    pub fn model_at(&self, idx: usize) -> Option<&str> {
        self.entries.get(idx).map(|e| e.model.as_str())
    }

    /// Format a status report for the `/failover status` command.
    #[must_use]
    pub fn format_status(&self) -> String {
        if self.entries.is_empty() {
            return "Failover chain is empty. Use /failover add <model> to build a chain.".to_string();
        }

        let mut lines = vec!["Failover chain:".to_string(), String::new()];
        let now = Instant::now();

        for (idx, entry) in self.entries.iter().enumerate() {
            let active_marker = if idx == self.active_index { " [active]" } else { "" };

            let cooldown_str = if let Some(&cooldown_until) = self.rate_limit_cooldowns.get(&idx) {
                if cooldown_until > now {
                    let remaining = (cooldown_until - now).as_secs();
                    format!("  rate-limited ({remaining}s remaining)")
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            let budget_str = if let Some(budget) = self.usage_budgets.get(&idx) {
                let mut parts = Vec::new();
                if let Some(max_tok) = entry.max_daily_tokens {
                    parts.push(format!(
                        "tokens {}/{}",
                        budget.tokens_used_today, max_tok
                    ));
                }
                if let Some(max_cost) = entry.max_daily_cost_usd {
                    parts.push(format!(
                        "cost ${:.4}/${:.2}",
                        budget.cost_today_usd, max_cost
                    ));
                }
                if parts.is_empty() {
                    String::new()
                } else {
                    format!("  budget: {}", parts.join(", "))
                }
            } else {
                String::new()
            };

            lines.push(format!(
                "  {}. {:<36} priority {}{}{}{}",
                idx + 1,
                entry.model,
                entry.priority,
                active_marker,
                cooldown_str,
                budget_str,
            ));
        }

        lines.push(String::new());
        lines.push(format!(
            "  auto_failover: {}   cooldown: {}s   notify: {}",
            self.config.auto_failover,
            self.config.cooldown_seconds,
            self.config.notify_on_failover,
        ));

        lines.join("\n")
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn entry_is_available(&mut self, idx: usize) -> bool {
        let now = Instant::now();

        // Check rate-limit cooldown.
        if let Some(&cooldown_until) = self.rate_limit_cooldowns.get(&idx) {
            if cooldown_until > now {
                return false;
            }
        }

        // Check daily budgets.
        let entry = match self.entries.get(idx) {
            Some(e) => e.clone(),
            None => return false,
        };

        if let Some(budget) = self.usage_budgets.get_mut(&idx) {
            budget.maybe_reset();
            if let Some(max_tok) = entry.max_daily_tokens {
                if budget.tokens_used_today >= max_tok {
                    return false;
                }
            }
            if let Some(max_cost) = entry.max_daily_cost_usd {
                if budget.cost_today_usd >= max_cost {
                    return false;
                }
            }
        }

        true
    }

    fn next_available_after(&mut self, current: usize) -> Option<usize> {
        ((current + 1)..self.entries.len()).find(|&idx| self.entry_is_available(idx))
    }

    fn maybe_recover_primary(&mut self) {
        if self.active_index == 0 {
            return;
        }
        let now = Instant::now();
        // Check if index 0 is now available.
        if let Some(&cooldown_until) = self.rate_limit_cooldowns.get(&0) {
            if cooldown_until > now {
                return;
            }
        }
        // Primary is no longer rate-limited — reset to it.
        self.active_index = 0;
        // We don't emit a recovery event here; callers inspect active_model().
    }
}

// ---------------------------------------------------------------------------
// Format a failover event as a TUI notification string
// ---------------------------------------------------------------------------

#[must_use]
pub fn format_failover_event(event: &FailoverEvent) -> String {
    match event.kind {
        FailoverEventKind::RateLimited => {
            let retry = event
                .retry_in_secs
                .map(|s| format!(" (retry in {s}s)"))
                .unwrap_or_default();
            let to = event
                .to_model
                .as_deref()
                .map_or_else(|| " No fallback available.".to_string(), |m| format!(" Failing over to {m}."));
            format!(
                "Warning: Rate limited on {}{retry}.{to}",
                event.from_model
            )
        }
        FailoverEventKind::BudgetExceeded => {
            let to = event
                .to_model
                .as_deref()
                .map_or_else(|| " No fallback available.".to_string(), |m| format!(" Using {m}."));
            format!(
                "Warning: Daily budget reached for {}.{to}",
                event.from_model
            )
        }
        FailoverEventKind::Recovered => {
            format!("Info: {} rate limit cleared. Resuming primary provider.", event.from_model)
        }
    }
}

// ---------------------------------------------------------------------------
// Config file loading
// ---------------------------------------------------------------------------

fn load_failover_config() -> Option<FailoverConfig> {
    let home = dirs_home()?;
    let path = home.join(".anvil").join("failover.json");
    let text = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&text).ok()?;

    let auto_failover = val.get("auto_failover").and_then(serde_json::Value::as_bool).unwrap_or(true);
    let cooldown_seconds = val.get("cooldown_seconds").and_then(serde_json::Value::as_u64).unwrap_or(60);
    let notify_on_failover = val.get("notify_on_failover").and_then(serde_json::Value::as_bool).unwrap_or(true);

    let chain = val
        .get("chain")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let model = item.get("model")?.as_str()?.to_string();
                    #[allow(clippy::cast_possible_truncation)]
                    let priority = item
                        .get("priority")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(i as u64 + 1) as u32;
                    let max_daily_tokens = item
                        .get("max_daily_tokens")
                        .and_then(serde_json::Value::as_u64);
                    let max_daily_cost_usd = item
                        .get("max_daily_cost_usd")
                        .and_then(serde_json::Value::as_f64);
                    let provider_kind =
                        crate::providers::detect_provider_kind(&model);
                    Some(FailoverEntry {
                        model,
                        provider_kind,
                        priority,
                        max_daily_tokens,
                        max_daily_cost_usd,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(FailoverConfig {
        chain,
        auto_failover,
        cooldown_seconds,
        notify_on_failover,
    })
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chain(models: &[(&str, u32)]) -> FailoverChain {
        let entries = models
            .iter()
            .map(|(model, priority)| FailoverEntry {
                model: model.to_string(),
                provider_kind: ProviderKind::AnvilApi,
                priority: *priority,
                max_daily_tokens: None,
                max_daily_cost_usd: None,
            })
            .collect();
        FailoverChain::new(FailoverConfig {
            chain: entries,
            auto_failover: true,
            cooldown_seconds: 60,
            notify_on_failover: true,
        })
    }

    #[test]
    fn select_provider_returns_highest_priority() {
        let mut chain = make_chain(&[("claude-sonnet-4-6", 2), ("claude-opus-4-6", 1)]);
        let idx = chain.select_provider().unwrap();
        assert_eq!(chain.entries[idx].model, "claude-opus-4-6");
    }

    #[test]
    fn rate_limit_skips_to_next() {
        let mut chain = make_chain(&[("model-a", 1), ("model-b", 2)]);
        // Pre-select so active is 0.
        chain.select_provider();
        let event = chain.on_rate_limited(0, Some(10)).unwrap();
        assert_eq!(event.kind, FailoverEventKind::RateLimited);
        assert_eq!(event.from_model, "model-a");
        assert_eq!(event.to_model.as_deref(), Some("model-b"));
        assert_eq!(chain.active_index, 1);
    }

    #[test]
    fn budget_exceeded_skips_to_next() {
        let mut chain = make_chain(&[("model-a", 1), ("model-b", 2)]);
        chain.select_provider();
        let event = chain.on_budget_exceeded(0).unwrap();
        assert_eq!(event.kind, FailoverEventKind::BudgetExceeded);
        assert_eq!(chain.active_index, 1);
    }

    #[test]
    fn reset_clears_cooldowns_and_budgets() {
        let mut chain = make_chain(&[("model-a", 1), ("model-b", 2)]);
        chain.on_rate_limited(0, Some(3600));
        chain.reset();
        assert!(chain.rate_limit_cooldowns.is_empty());
        assert_eq!(chain.active_index, 0);
    }

    #[test]
    fn add_and_remove_entries() {
        let mut chain = make_chain(&[("model-a", 1)]);
        chain.add_entry(FailoverEntry {
            model: "model-b".to_string(),
            provider_kind: ProviderKind::Ollama,
            priority: 2,
            max_daily_tokens: None,
            max_daily_cost_usd: None,
        });
        assert_eq!(chain.entries.len(), 2);
        chain.remove_entry("model-b");
        assert_eq!(chain.entries.len(), 1);
    }

    #[test]
    fn format_failover_event_rate_limited() {
        let event = FailoverEvent {
            kind: FailoverEventKind::RateLimited,
            from_model: "claude-opus-4-6".to_string(),
            to_model: Some("claude-sonnet-4-6".to_string()),
            retry_in_secs: Some(45),
        };
        let msg = format_failover_event(&event);
        assert!(msg.contains("Rate limited on claude-opus-4-6"));
        assert!(msg.contains("45s"));
        assert!(msg.contains("claude-sonnet-4-6"));
    }

    #[test]
    fn format_failover_event_recovered() {
        let event = FailoverEvent {
            kind: FailoverEventKind::Recovered,
            from_model: "claude-opus-4-6".to_string(),
            to_model: None,
            retry_in_secs: None,
        };
        let msg = format_failover_event(&event);
        assert!(msg.contains("rate limit cleared"));
    }
}
