use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::SearchResult;

pub struct SearchResultCache {
    entries: HashMap<String, (Instant, Vec<SearchResult>)>,
}

impl SearchResultCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Returns cached results if they exist and are within the TTL.
    pub fn get(&self, key: &str, ttl: Duration) -> Option<Vec<SearchResult>> {
        self.entries.get(key).and_then(|(ts, results)| {
            if ts.elapsed() < ttl {
                Some(results.clone())
            } else {
                None
            }
        })
    }

    pub fn insert(&mut self, key: String, results: Vec<SearchResult>) {
        self.entries.insert(key, (Instant::now(), results));
    }
}
