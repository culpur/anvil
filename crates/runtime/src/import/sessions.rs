// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// Session import — Phase 6.3.
///
/// Discovers CC session JSONL files under `<profile_dir>/projects/*/`,
/// summarizes each via a configurable [`SessionSummarizer`], writes one
/// [`SessionSummary`] per session into the [`DailyStore`], and writes each
/// extracted lesson as a [`Nomination`] candidate for user review.
///
/// # Design notes
///
/// - **OFF by default** — gated behind `--include-sessions` because summarization
///   is expensive (~$5 / 1800 sessions at Haiku rates).
/// - **Read-only** on `~/.claude/` — we never write to CC's state.
/// - **Streaming JSONL** — sessions are read line-by-line; the entire 1 GB is
///   never loaded into memory at once.  The 50 MB cap per session prevents any
///   single transcript from blowing memory.
/// - **Resumable** — `<staging>/sessions/progress.json` records every processed
///   session by content_hash.  Re-runs skip already-processed entries.
/// - **Verbatim-with-flag** — every artifact carries `imported_from: claude_code`,
///   `imported_at`, `source_path`, `content_hash`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::daily::{DailyStore, SessionSummary, epoch_secs_to_date};
use crate::import::{now_rfc3339, sha256_hex, otel_import_discovered, otel_import_staged, otel_import_skipped};
use crate::import::staging::{anvil_config_home, StagingDir};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum session JSONL size.  Sessions larger than this are skipped.
const SESSION_MAX_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

/// Sessions smaller than this threshold are summarized in one call.
const SESSION_SMALL_THRESHOLD: u64 = 50 * 1024; // 50 KB

/// Target characters per chunk when splitting large sessions.
const CHUNK_CHARS: usize = 20_000;

// ── SummarizedSession ─────────────────────────────────────────────────────────

/// Structured output from the summarizer for a single session transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SummarizedSession {
    /// 1–3 paragraph narrative summary of what happened in this session.
    pub summary: String,
    /// Tasks that were completed (short phrases).
    pub tasks_completed: Vec<String>,
    /// Tasks that were started but not completed.
    pub tasks_open: Vec<String>,
    /// Files visibly modified in the session (paths).
    pub files_modified: Vec<String>,
    /// Lessons or patterns worth nominating to the knowledge store.
    pub lessons_learned: Vec<String>,
}

// ── SessionSummarizer trait ───────────────────────────────────────────────────

/// Summarize a raw JSONL transcript (or a pre-extracted excerpt) into a
/// [`SummarizedSession`].
///
/// This trait is the single seam between the import pipeline and the LLM
/// backend.  Tests inject a [`MockSummarizer`]; production code uses
/// [`ProviderSummarizer`].
pub trait SessionSummarizer: Send + Sync {
    /// Summarize `transcript` and return the structured result.
    ///
    /// `transcript` may be the full JSONL text or a concatenation of
    /// chunk summaries (in the hierarchical case).
    ///
    /// # Errors
    ///
    /// Returns `Err(reason)` when the provider call fails or returns
    /// unparseable output.  The caller records the error as a skip.
    fn summarize(&self, transcript: &str) -> Result<SummarizedSession, String>;
}

// ── MockSummarizer ────────────────────────────────────────────────────────────

/// Deterministic summarizer for tests.
///
/// Returns fixed output whose fields vary only by the length of the
/// transcript (to allow assertion on chunking behaviour).
pub struct MockSummarizer;

impl SessionSummarizer for MockSummarizer {
    fn summarize(&self, transcript: &str) -> Result<SummarizedSession, String> {
        let len = transcript.len();
        Ok(SummarizedSession {
            summary: format!("Mock summary of {len}-char transcript."),
            tasks_completed: vec![format!("mock-task-completed ({len})")],
            tasks_open: vec![],
            files_modified: vec!["mock/file.rs".to_string()],
            lessons_learned: vec![format!("mock-lesson from {len} chars")],
        })
    }
}

// ── ProviderSummarizer ────────────────────────────────────────────────────────

/// Production summarizer.  Uses `reqwest::blocking` to call the cheapest
/// configured provider: Ollama localhost first, then Anthropic Haiku, then
/// any remaining OpenAI-compat provider.
///
/// Provider detection order:
///  1. Ollama at `http://localhost:11434` — check `/api/tags` to confirm live.
///  2. `ANTHROPIC_API_KEY` in env — model `claude-haiku-4-5`.
///  3. `OPENAI_API_KEY` in env — model `gpt-4o-mini`.
///
/// Fails loudly (returns `Err`) when no provider is reachable.
pub struct ProviderSummarizer {
    /// The resolved provider variant to use.
    provider: SummaryProvider,
}

/// Resolved provider for summarization.
#[derive(Debug, Clone)]
enum SummaryProvider {
    Ollama { base_url: String, model: String },
    Anthropic { api_key: String, model: String },
    OpenAi { api_key: String, model: String },
}

impl ProviderSummarizer {
    /// Attempt to auto-detect the cheapest available provider.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no provider is configured or reachable.
    pub fn detect() -> Result<Self, String> {
        // 1. Ollama localhost — probe /api/tags
        let ollama_base = std::env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        if ollama_is_live(&ollama_base) {
            // Pick the first model that looks like a small/fast model,
            // or fall back to the first listed model.
            let model = pick_ollama_model(&ollama_base)
                .unwrap_or_else(|| "llama3.2:3b".to_string());
            return Ok(Self {
                provider: SummaryProvider::Ollama { base_url: ollama_base, model },
            });
        }

        // 2. Anthropic API key
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            if !key.is_empty() {
                return Ok(Self {
                    provider: SummaryProvider::Anthropic {
                        api_key: key,
                        model: "claude-haiku-4-5".to_string(),
                    },
                });
            }
        }

        // 3. OpenAI API key
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            if !key.is_empty() {
                return Ok(Self {
                    provider: SummaryProvider::OpenAi {
                        api_key: key,
                        model: "gpt-4o-mini".to_string(),
                    },
                });
            }
        }

        Err(
            "No summarization provider available. \
             Start Ollama (`ollama serve`) or set ANTHROPIC_API_KEY / OPENAI_API_KEY."
                .to_string(),
        )
    }

    /// Build the prompt for summarization.
    fn build_prompt(transcript: &str) -> String {
        format!(
            "You are summarizing an AI coding session transcript. \
             Extract the following fields from the conversation. \
             Respond with a JSON object containing these exact keys: \
             summary (string: 2–4 sentences describing what happened), \
             tasks_completed (array of short strings), \
             tasks_open (array of short strings), \
             files_modified (array of file paths mentioned as changed), \
             lessons_learned (array of patterns/conventions discovered). \
             Keep each lesson to one sentence max. \
             Return ONLY the JSON object with no markdown code fences or preamble.\n\n\
             TRANSCRIPT:\n{transcript}"
        )
    }

    /// Call Ollama's `/api/generate` endpoint (blocking).
    fn call_ollama(base_url: &str, model: &str, prompt: &str) -> Result<SummarizedSession, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": false
        });

        let resp = client
            .post(format!("{base_url}/api/generate"))
            .json(&body)
            .send()
            .map_err(|e| format!("ollama request: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("ollama returned HTTP {}", resp.status()));
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("parse ollama response: {e}"))?;
        let text = json
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or("ollama response missing 'response' field")?;

        parse_summary_json(text)
    }

    /// Call the Anthropic Messages API (blocking).
    fn call_anthropic(api_key: &str, model: &str, prompt: &str) -> Result<SummarizedSession, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": prompt }]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| format!("anthropic request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("anthropic returned HTTP {status}: {body}"));
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("parse anthropic response: {e}"))?;
        let text = json
            .pointer("/content/0/text")
            .and_then(|v| v.as_str())
            .ok_or("anthropic response missing content[0].text")?;

        parse_summary_json(text)
    }

    /// Call the OpenAI Chat Completions API (blocking).
    fn call_openai(api_key: &str, model: &str, prompt: &str) -> Result<SummarizedSession, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": prompt }]
        });

        let resp = client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .map_err(|e| format!("openai request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("openai returned HTTP {status}: {body}"));
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("parse openai response: {e}"))?;
        let text = json
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .ok_or("openai response missing choices[0].message.content")?;

        parse_summary_json(text)
    }
}

impl SessionSummarizer for ProviderSummarizer {
    fn summarize(&self, transcript: &str) -> Result<SummarizedSession, String> {
        let prompt = Self::build_prompt(transcript);
        match &self.provider {
            SummaryProvider::Ollama { base_url, model } => {
                Self::call_ollama(base_url, model, &prompt)
            }
            SummaryProvider::Anthropic { api_key, model } => {
                Self::call_anthropic(api_key, model, &prompt)
            }
            SummaryProvider::OpenAi { api_key, model } => {
                Self::call_openai(api_key, model, &prompt)
            }
        }
    }
}

// ── Provider helpers ──────────────────────────────────────────────────────────

/// Return `true` if Ollama is reachable at `base_url`.
fn ollama_is_live(base_url: &str) -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{base_url}/api/tags"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Pick a small/fast model from Ollama's installed models.
///
/// Prefers models containing "3b", "7b", "mini", "small", "qwen", "phi",
/// "gemma", or "llama3" (in that order).  Falls back to the first listed
/// model.
fn pick_ollama_model(base_url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let resp = client
        .get(format!("{base_url}/api/tags"))
        .send()
        .ok()?;

    let json: serde_json::Value = resp.json().ok()?;
    let models = json.get("models")?.as_array()?;

    let names: Vec<String> = models
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();

    // Prefer small/fast indicators.
    for keyword in &["3b", "mini", "small", "7b", "phi", "gemma", "qwen", "llama3"] {
        if let Some(n) = names.iter().find(|n| n.to_ascii_lowercase().contains(keyword)) {
            return Some(n.clone());
        }
    }

    names.into_iter().next()
}

/// Parse the LLM's JSON response into a [`SummarizedSession`].
///
/// Strips common markdown code fence wrappers before parsing.
fn parse_summary_json(text: &str) -> Result<SummarizedSession, String> {
    // Strip ```json ... ``` or ``` ... ``` wrappers.
    let stripped = text.trim();
    let stripped = stripped
        .strip_prefix("```json")
        .or_else(|| stripped.strip_prefix("```"))
        .unwrap_or(stripped)
        .trim_start();
    let stripped = stripped
        .strip_suffix("```")
        .unwrap_or(stripped)
        .trim_end();

    let v: serde_json::Value =
        serde_json::from_str(stripped).map_err(|e| format!("JSON parse: {e} — text: {stripped}"))?;

    let summary = v
        .get("summary")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let tasks_completed = string_array(&v, "tasks_completed");
    let tasks_open = string_array(&v, "tasks_open");
    let files_modified = string_array(&v, "files_modified");
    let lessons_learned = string_array(&v, "lessons_learned");

    Ok(SummarizedSession {
        summary,
        tasks_completed,
        tasks_open,
        files_modified,
        lessons_learned,
    })
}

/// Extract a JSON array of strings from a value by key.
fn string_array(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ── SessionImportOpts ─────────────────────────────────────────────────────────

/// Options for `run_sessions_import`.
pub struct SessionImportOpts {
    /// When `true`, actually run summarization; `false` is a no-op.
    pub include_sessions: bool,
    /// The summarizer implementation to use.
    pub summarizer: Box<dyn SessionSummarizer>,
}

impl SessionImportOpts {
    /// Construct with the production [`ProviderSummarizer`] auto-detected.
    ///
    /// # Errors
    ///
    /// Propagates `ProviderSummarizer::detect` errors.
    pub fn production() -> Result<Self, String> {
        Ok(Self {
            include_sessions: true,
            summarizer: Box::new(ProviderSummarizer::detect()?),
        })
    }

    /// Construct for dry-run / disabled mode (no summarization is performed).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            include_sessions: false,
            summarizer: Box::new(MockSummarizer),
        }
    }
}

// ── SessionImportRecord ───────────────────────────────────────────────────────

/// Outcome for a single session file.
#[derive(Debug, Clone)]
pub struct SessionImportRecord {
    /// Absolute path of the `.jsonl` file.
    pub source_path: PathBuf,
    /// SHA-256 of the file contents.
    pub content_hash: String,
    /// Calendar date derived from session timestamps (`YYYY-MM-DD`).
    pub session_date: String,
    /// Session ID parsed from the JSONL (or derived from the filename).
    pub session_id: String,
    /// What happened to this session.
    pub status: SessionImportStatus,
    /// Human-readable reason (populated for `Skipped` and `Failed`).
    pub reason: Option<String>,
    /// Number of lessons written as nominations.
    pub nominations_written: usize,
}

/// Status of a single session import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionImportStatus {
    /// Successfully summarized and written to `DailyStore`.
    Summarized,
    /// Already processed on a previous run (idempotency).
    AlreadyProcessed,
    /// Skipped because the file exceeds `SESSION_MAX_BYTES`.
    Skipped,
    /// Summarization or I/O failed.
    Failed,
}

// ── Progress tracking ─────────────────────────────────────────────────────────

/// Resumable progress state persisted to `<staging>/sessions/progress.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionProgress {
    /// Content hashes of sessions that were fully processed (summarized or
    /// intentionally skipped — not failed).
    pub processed: Vec<String>,
    /// Content hashes of sessions skipped due to oversize.
    pub skipped_oversized: Vec<String>,
    /// RFC 3339 timestamp when the current (or previous) run started.
    pub started_at: String,
    /// Source path of the last successfully processed session.
    pub last_session_path: Option<String>,
}

impl SessionProgress {
    fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("create dir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        // Atomic write via temp file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
        Ok(())
    }

    fn is_processed(&self, hash: &str) -> bool {
        self.processed.iter().any(|h| h == hash)
            || self.skipped_oversized.iter().any(|h| h == hash)
    }

    fn mark_processed(&mut self, hash: String) {
        if !self.processed.contains(&hash) {
            self.processed.push(hash);
        }
    }

    fn mark_skipped_oversized(&mut self, hash: String) {
        if !self.skipped_oversized.contains(&hash) {
            self.skipped_oversized.push(hash);
        }
    }
}

// ── JSONL extraction ──────────────────────────────────────────────────────────

/// Extracted metadata from a session JSONL file.
struct SessionMeta {
    /// First user message text (extracted but reserved for future use).
    #[allow(dead_code)]
    first_user_message: String,
    /// Model identifier from the first assistant turn.
    model: String,
    /// Approximate wall-clock duration derived from message timestamps.
    duration_secs: u64,
    /// Session date `YYYY-MM-DD` derived from the first message timestamp.
    session_date: String,
    /// Session UUID (from `sessionId` fields in the JSONL).
    session_id: String,
}

/// Extract session metadata and transcript text from a JSONL file.
///
/// Streams the file line-by-line; never loads the whole content at once.
/// Returns `(meta, transcript)` where `transcript` is the user+assistant text
/// suitable for summarization.
fn extract_session_data(path: &Path) -> Result<(SessionMeta, String), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let reader = BufReader::new(file);

    let mut first_user_message = String::new();
    let mut model = String::from("unknown");
    let mut first_ts: Option<u64> = None;
    let mut last_ts: Option<u64> = None;
    let mut session_id = String::new();
    let mut transcript_parts: Vec<String> = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("read line: {e}"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract session ID from any line that has it.
        if session_id.is_empty() {
            if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) {
                session_id = sid.to_string();
            }
        }

        // Extract timestamp (Unix ms or seconds).
        if let Some(ts) = extract_timestamp(&v) {
            if first_ts.is_none() {
                first_ts = Some(ts);
            }
            last_ts = Some(ts);
        }

        // Extract model name.
        if model == "unknown" {
            if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
                model = m.to_string();
            }
        }

        // Extract message content.
        let role = v.get("type").and_then(|t| t.as_str());
        let msg_role = v
            .pointer("/message/role")
            .or_else(|| v.get("role"))
            .and_then(|r| r.as_str());

        let content = extract_message_content(&v);

        match (role, msg_role) {
            // CC JSONL format: {"type":"user","message":{...}} or {"role":"user",...}
            (Some("human") | Some("user"), _) | (_, Some("human") | Some("user")) => {
                if first_user_message.is_empty() && !content.is_empty() {
                    first_user_message = content.chars().take(200).collect();
                }
                if !content.is_empty() {
                    transcript_parts.push(format!("User: {content}"));
                }
            }
            (Some("assistant") | Some("ai"), _) | (_, Some("assistant")) => {
                if !content.is_empty() {
                    transcript_parts.push(format!("Assistant: {content}"));
                }
            }
            _ => {}
        }
    }

    // Derive duration from timestamps.
    let (duration_secs, session_date) = match (first_ts, last_ts) {
        (Some(first), Some(last)) => {
            let dur = last.saturating_sub(first);
            let date = epoch_secs_to_date(first);
            (dur, date)
        }
        (Some(first), None) => (0, epoch_secs_to_date(first)),
        _ => (0, crate::daily::today_date()),
    };

    // Fall back session_id to filename stem.
    if session_id.is_empty() {
        session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
    }

    let meta = SessionMeta {
        first_user_message,
        model,
        duration_secs,
        session_date,
        session_id,
    };

    let transcript = transcript_parts.join("\n");
    Ok((meta, transcript))
}

/// Extract a Unix-seconds timestamp from various CC JSONL line shapes.
fn extract_timestamp(v: &serde_json::Value) -> Option<u64> {
    // Try "timestamp" (seconds or millis), "ts", "created_at".
    for key in &["timestamp", "ts", "created_at"] {
        if let Some(ts_val) = v.get(key) {
            if let Some(n) = ts_val.as_u64() {
                // Heuristic: if > 10^10 assume milliseconds.
                let secs = if n > 10_000_000_000 { n / 1000 } else { n };
                return Some(secs);
            }
            if let Some(s) = ts_val.as_str() {
                // Try ISO 8601 via minimal parse.
                if let Some(secs) = parse_iso_to_secs(s) {
                    return Some(secs);
                }
            }
        }
    }
    None
}

/// Very minimal ISO 8601 parser: `YYYY-MM-DDTHH:MM:SS` → Unix seconds.
/// Returns `None` for anything it can't parse.
fn parse_iso_to_secs(s: &str) -> Option<u64> {
    // Expect at least "YYYY-MM-DD" (10 chars).
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let year: u64 = std::str::from_utf8(&b[..4]).ok()?.parse().ok()?;
    let month: u64 = std::str::from_utf8(&b[5..7]).ok()?.parse().ok()?;
    let day: u64 = std::str::from_utf8(&b[8..10]).ok()?.parse().ok()?;

    let (hour, min, sec) = if b.len() >= 19 {
        let h: u64 = std::str::from_utf8(&b[11..13]).ok()?.parse().ok()?;
        let m: u64 = std::str::from_utf8(&b[14..16]).ok()?.parse().ok()?;
        let s: u64 = std::str::from_utf8(&b[17..19]).ok()?.parse().ok()?;
        (h, m, s)
    } else {
        (0, 0, 0)
    };

    // Days since epoch via Gregorian civil algorithm (same as epoch_secs_to_date in reverse).
    let days = days_since_epoch(year, month, day)?;
    Some(days * 86_400 + hour * 3_600 + min * 60 + sec)
}

/// Days from 1970-01-01 to `year`-`month`-`day`.
fn days_since_epoch(year: u64, month: u64, day: u64) -> Option<u64> {
    if year < 1970 || month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }
    // Count days for full years since 1970.
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    // Days in months of current year.
    let month_days: [u64; 12] = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    for m in 1..month {
        days += month_days[(m - 1) as usize];
    }
    days += day - 1;
    Some(days)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Extract readable text from a JSONL message value.
fn extract_message_content(v: &serde_json::Value) -> String {
    // Try message.content (array or string).
    if let Some(mc) = v.pointer("/message/content") {
        return extract_content_value(mc);
    }
    // Try top-level content.
    if let Some(c) = v.get("content") {
        return extract_content_value(c);
    }
    String::new()
}

fn extract_content_value(v: &serde_json::Value) -> String {
    if let Some(s) = v.as_str() {
        return s.chars().take(1000).collect();
    }
    if let Some(arr) = v.as_array() {
        // Content blocks array — join text blocks.
        let parts: Vec<&str> = arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        return parts.join(" ").chars().take(1000).collect();
    }
    String::new()
}

// ── Chunked summarization ─────────────────────────────────────────────────────

/// Split `transcript` on user-turn boundaries into chunks of at most
/// `CHUNK_CHARS` characters, then summarize each chunk with `summarizer`.
/// Finally, summarize the concatenated chunk-summaries.
///
/// Exposed as `pub` for integration testing (`import_phase63.rs`).
pub fn summarize_chunked_pub(
    transcript: &str,
    summarizer: &dyn SessionSummarizer,
) -> Result<SummarizedSession, String> {
    summarize_chunked(transcript, summarizer)
}

fn summarize_chunked(
    transcript: &str,
    summarizer: &dyn SessionSummarizer,
) -> Result<SummarizedSession, String> {
    let chunks = split_on_user_turns(transcript, CHUNK_CHARS);

    if chunks.is_empty() {
        return Err("transcript produced no chunks".to_string());
    }

    if chunks.len() == 1 {
        return summarizer.summarize(&chunks[0]);
    }

    // Summarize each chunk.
    let mut chunk_summaries: Vec<String> = Vec::new();
    for chunk in &chunks {
        match summarizer.summarize(chunk) {
            Ok(s) => chunk_summaries.push(format!(
                "CHUNK SUMMARY:\ntasks_completed: {}\ntasks_open: {}\nlessons: {}\nsummary: {}",
                s.tasks_completed.join("; "),
                s.tasks_open.join("; "),
                s.lessons_learned.join("; "),
                s.summary
            )),
            Err(e) => {
                // On chunk failure, include a placeholder and continue.
                chunk_summaries.push(format!("(chunk summarization failed: {e})"));
            }
        }
    }

    let combined = chunk_summaries.join("\n\n");
    summarizer.summarize(&combined)
}

/// Split `text` into chunks no larger than `max_chars`, preferring to break
/// on `"User: "` boundaries.
fn split_on_user_turns(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        let is_user_turn = line.starts_with("User: ");
        // If adding this line would overflow and we're at a user-turn boundary,
        // flush the current chunk.
        if is_user_turn && !current.is_empty() && current.len() + line.len() > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

// ── Nomination writer ─────────────────────────────────────────────────────────

/// Write lesson nominations into `<staging>/nominations/sess-{id}-{idx}.json`.
///
/// Nominations in staging are NOT committed to the live `~/.anvil/nominations/`
/// directory automatically.  The commit phase handles that.  During dry-run,
/// they remain in staging only.
fn write_lesson_nominations(
    session_id: &str,
    session_date: &str,
    lessons: &[String],
    imported_at: &str,
    source_path: &Path,
    staging: &StagingDir,
) -> usize {
    let mut count = 0;

    for (idx, lesson) in lessons.iter().enumerate() {
        // Build a nomination JSON manually so we can inject import metadata.
        let nom_id = format!("nom-sess-{session_id}-{idx:04}");

        // Build a Nomination struct and add import metadata.
        let nom = serde_json::json!({
            "id": nom_id,
            "created_at": imported_at,
            "session_id": session_id,
            "category": "workflow",
            "content": lesson,
            "confidence": 0.6,
            "status": "pending",
            "promoted_to": null,
            // Import metadata stamps.
            "imported_from": "claude_code",
            "imported_at": imported_at,
            "source_path": source_path.display().to_string(),
            "session_date": session_date,
        });

        let json = match serde_json::to_string_pretty(&nom) {
            Ok(j) => j,
            Err(_) => continue,
        };

        let rel = format!("nominations/sess-{session_id}-{idx:04}.json");
        if staging.stage_bytes(&rel, json.as_bytes()).is_ok() {
            count += 1;
        }
    }

    count
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Run the sessions import pipeline.
///
/// # Arguments
///
/// - `profile_dir`: CC profile directory (read-only)
/// - `staging`: staging directory handle
/// - `opts`: import options (summarizer + include flag)
///
/// # Returns
///
/// A `Vec<SessionImportRecord>` with one entry per discovered `.jsonl` file.
/// Caller is responsible for committing nominated lessons and updating the
/// DailyStore from staging (in the non-dry-run path).
///
/// # OTel events
///
/// - `import.discovered{kind:"session", n}` — emitted once with total count
/// - `import.translated{kind:"session"}` — per successfully summarized session
/// - `import.staged{kind:"session"}` — per lesson nomination staged
/// - `import.skipped{kind:"session", reason}` — per oversized or failed session
pub fn run_sessions_import(
    profile_dir: &Path,
    staging: &StagingDir,
    opts: &mut SessionImportOpts,
) -> Vec<SessionImportRecord> {
    if !opts.include_sessions {
        return Vec::new();
    }

    let progress_path = staging.path("sessions/progress.json");
    let mut progress = SessionProgress::load(&progress_path);
    if progress.started_at.is_empty() {
        progress.started_at = now_rfc3339();
    }

    // Discover all .jsonl files under <profile_dir>/projects/*/
    let jsonl_paths = discover_session_files(profile_dir);
    let total = jsonl_paths.len();
    otel_import_discovered("session", total);

    let _daily_dir = anvil_config_home().join("daily");
    let imported_at = now_rfc3339();

    let mut records: Vec<SessionImportRecord> = Vec::new();

    for path in jsonl_paths {
        // Check file size.
        let file_size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Compute content hash for idempotency.
        let content_hash = crate::import::sha256_file(&path)
            .unwrap_or_else(|| sha256_hex(path.to_string_lossy().as_bytes()));

        // Idempotency gate.
        if progress.is_processed(&content_hash) {
            records.push(SessionImportRecord {
                source_path: path.clone(),
                content_hash,
                session_date: String::new(),
                session_id: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                status: SessionImportStatus::AlreadyProcessed,
                reason: Some("already processed".to_string()),
                nominations_written: 0,
            });
            continue;
        }

        // Oversize check.
        if file_size > SESSION_MAX_BYTES {
            otel_import_skipped("session", 1, "oversized");
            progress.mark_skipped_oversized(content_hash.clone());
            let _ = progress.save(&progress_path);
            records.push(SessionImportRecord {
                source_path: path.clone(),
                content_hash,
                session_date: String::new(),
                session_id: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                status: SessionImportStatus::Skipped,
                reason: Some("oversized".to_string()),
                nominations_written: 0,
            });
            continue;
        }

        // Extract metadata and transcript (streaming).
        let (meta, transcript) = match extract_session_data(&path) {
            Ok(r) => r,
            Err(e) => {
                records.push(SessionImportRecord {
                    source_path: path.clone(),
                    content_hash,
                    session_date: crate::daily::today_date(),
                    session_id: path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    status: SessionImportStatus::Failed,
                    reason: Some(format!("extract failed: {e}")),
                    nominations_written: 0,
                });
                continue;
            }
        };

        // Summarize.
        let summarized = if file_size < SESSION_SMALL_THRESHOLD {
            opts.summarizer.summarize(&transcript)
        } else {
            summarize_chunked(&transcript, opts.summarizer.as_ref())
        };

        let summarized = match summarized {
            Ok(s) => s,
            Err(e) => {
                records.push(SessionImportRecord {
                    source_path: path.clone(),
                    content_hash,
                    session_date: meta.session_date.clone(),
                    session_id: meta.session_id.clone(),
                    status: SessionImportStatus::Failed,
                    reason: Some(format!("summarize failed: {e}")),
                    nominations_written: 0,
                });
                continue;
            }
        };

        // Build the SessionSummary and write to DailyStore.
        let session_summary = SessionSummary {
            session_id: meta.session_id.clone(),
            model: meta.model.clone(),
            duration_secs: meta.duration_secs,
            tokens_used: 0, // unknown from transcript
            messages_count: 0,
            tasks_completed: summarized.tasks_completed.clone(),
            tasks_open: summarized.tasks_open.clone(),
            files_modified: summarized.files_modified.clone(),
            nominations_generated: summarized.lessons_learned.len(),
            credentials_auto_vaulted: 0,
        };

        // Write to a staging-specific DailyStore path so dry-run doesn't
        // pollute ~/.anvil/daily/ directly.
        // In the headless pipeline, run_import_pipeline_headless decides whether
        // to copy staging/daily/ → ~/.anvil/daily/ during the commit phase.
        let staged_daily_dir = staging.path("sessions/daily");
        let staged_store = DailyStore::with_dir(staged_daily_dir);

        // For the staged store we need to get the right date record.
        let date_for_record = meta.session_date.clone();
        let mut daily_record = staged_store.get(&date_for_record).unwrap_or_else(|| {
            crate::daily::DailySummary {
                date: date_for_record.clone(),
                ..Default::default()
            }
        });

        daily_record.total_tokens = daily_record
            .total_tokens
            .saturating_add(session_summary.tokens_used);
        daily_record.sessions.push(session_summary);
        daily_record.open_items = staged_store.reconcile(&daily_record);

        // Write staged daily record.
        let staged_daily_path = staging.path(&format!("sessions/daily/{date_for_record}.json"));
        if let Some(parent) = staged_daily_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&daily_record) {
            let _ = std::fs::write(&staged_daily_path, json);
        }

        // Write lesson nominations to staging.
        let nom_count = write_lesson_nominations(
            &meta.session_id,
            &meta.session_date,
            &summarized.lessons_learned,
            &imported_at,
            &path,
            staging,
        );

        otel_import_staged("session", 1);
        if nom_count > 0 {
            otel_import_staged("nomination", nom_count);
        }

        progress.mark_processed(content_hash.clone());
        progress.last_session_path = Some(path.display().to_string());
        let _ = progress.save(&progress_path);

        records.push(SessionImportRecord {
            source_path: path.clone(),
            content_hash,
            session_date: meta.session_date,
            session_id: meta.session_id,
            status: SessionImportStatus::Summarized,
            reason: None,
            nominations_written: nom_count,
        });
    }

    records
}

/// Discover all `.jsonl` files under `<profile_dir>/projects/*/`.
///
/// Walks one level deep (project dirs), then reads any `.jsonl` directly in
/// each project dir.  Does NOT recurse further.
fn discover_session_files(profile_dir: &Path) -> Vec<PathBuf> {
    let projects_dir = profile_dir.join("projects");
    if !projects_dir.exists() {
        return Vec::new();
    }

    let mut result: Vec<PathBuf> = Vec::new();

    let project_entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    for project_entry in project_entries.flatten() {
        let project_dir = project_entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        let dir_entries = match std::fs::read_dir(&project_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in dir_entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                result.push(path);
            }
        }
    }

    // Sort for deterministic ordering.
    result.sort();
    result
}

/// Commit staged sessions (daily records + nominations) from staging into live dirs.
///
/// Called from the headless pipeline after the user has confirmed.
/// In dry-run mode this is a no-op.
pub fn commit_sessions_from_staging(staging: &StagingDir) -> Result<usize, String> {
    let staged_daily = staging.path("sessions/daily");
    if !staged_daily.exists() {
        return Ok(0);
    }

    let live_daily = anvil_config_home().join("daily");
    std::fs::create_dir_all(&live_daily)
        .map_err(|e| format!("create daily dir: {e}"))?;

    let mut committed = 0;

    let entries = std::fs::read_dir(&staged_daily)
        .map_err(|e| format!("read staged daily: {e}"))?;

    for entry in entries.flatten() {
        let src = entry.path();
        if src.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let filename = src.file_name().unwrap_or_default().to_string_lossy().to_string();
        let dest = live_daily.join(&filename);

        // Merge with existing daily record if present.
        if dest.exists() {
            if let (Ok(src_text), Ok(dst_text)) = (
                std::fs::read_to_string(&src),
                std::fs::read_to_string(&dest),
            ) {
                if let (Ok(mut dst_rec), Ok(src_rec)) = (
                    serde_json::from_str::<crate::daily::DailySummary>(&dst_text),
                    serde_json::from_str::<crate::daily::DailySummary>(&src_text),
                ) {
                    // Append new sessions that don't already exist.
                    for sess in src_rec.sessions {
                        if !dst_rec.sessions.iter().any(|s| s.session_id == sess.session_id) {
                            dst_rec.sessions.push(sess);
                        }
                    }
                    if let Ok(merged_json) = serde_json::to_string_pretty(&dst_rec) {
                        let _ = std::fs::write(&dest, merged_json);
                        committed += 1;
                        continue;
                    }
                }
            }
        }

        // No existing record — copy directly.
        std::fs::copy(&src, &dest)
            .map_err(|e| format!("copy {filename}: {e}"))?;
        committed += 1;
    }

    // Commit staged nominations to live nominations dir.
    let staged_noms = staging.path("nominations");
    if staged_noms.exists() {
        let live_noms = anvil_config_home().join("nominations");
        std::fs::create_dir_all(&live_noms)
            .map_err(|e| format!("create nominations dir: {e}"))?;

        if let Ok(entries) = std::fs::read_dir(&staged_noms) {
            for entry in entries.flatten() {
                let src = entry.path();
                if src.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let fname = src.file_name().unwrap_or_default().to_string_lossy().to_string();
                let dest = live_noms.join(&fname);
                if !dest.exists() {
                    let _ = std::fs::copy(&src, &dest);
                }
            }
        }
    }

    Ok(committed)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn set_anvil_home(path: &Path) {
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
    }

    fn clear_anvil_home() {
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
    }

    /// Write a minimal JSONL session file at `path`.
    fn write_session_jsonl(path: &Path, session_id: &str, n_turns: usize) {
        let mut lines = Vec::new();
        for i in 0..n_turns {
            let ts = 1_700_000_000u64 + (i as u64 * 60);
            // User turn.
            lines.push(format!(
                r#"{{"type":"human","sessionId":"{session_id}","timestamp":{ts},"message":{{"role":"user","content":"Fix the bug #{i}"}}}}"#
            ));
            // Assistant turn.
            lines.push(format!(
                r#"{{"type":"assistant","sessionId":"{session_id}","timestamp":{ts}+10,"message":{{"role":"assistant","content":"Done — fixed #{i}."}}}}"#
            ));
        }
        std::fs::write(path, lines.join("\n")).expect("write session jsonl");
    }

    /// Build a CC profile fixture with `n_projects` projects, each having one
    /// session JSONL file with `turns_per_session` turns.
    fn build_cc_sessions_fixture(root: &Path, n_projects: usize, turns_per_session: usize) -> PathBuf {
        let profile_dir = root.join(".claude");
        for i in 0..n_projects {
            let proj_dir = profile_dir.join("projects").join(format!("proj-{i:04}"));
            std::fs::create_dir_all(&proj_dir).expect("create project dir");
            let session_id = format!("sess-{i:04}");
            let session_path = proj_dir.join(format!("{session_id}.jsonl"));
            write_session_jsonl(&session_path, &session_id, turns_per_session);
        }
        profile_dir
    }

    // ── discovery finds all jsonl files ──────────────────────────────────────

    #[test]
    fn discovery_finds_all_jsonl_files() {
        let dir = TempDir::new().expect("tmpdir");
        let profile = build_cc_sessions_fixture(dir.path(), 3, 2);
        let found = discover_session_files(&profile);
        assert_eq!(found.len(), 3, "should find 3 session files");
    }

    // ── mock summarizer returns deterministic output ──────────────────────────

    #[test]
    fn mock_summarizer_is_deterministic() {
        let s = MockSummarizer;
        let a = s.summarize("hello world").expect("summarize");
        let b = s.summarize("hello world").expect("summarize again");
        assert_eq!(a, b, "mock summarizer must be deterministic");
    }

    #[test]
    fn mock_summarizer_varies_by_length() {
        let s = MockSummarizer;
        let short = s.summarize("hi").expect("short");
        let long = s.summarize("a".repeat(500).as_str()).expect("long");
        assert_ne!(short.summary, long.summary, "mock must vary by length");
    }

    // ── run_sessions_import disabled returns empty vec ────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn disabled_opts_returns_empty() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());
        let profile = build_cc_sessions_fixture(dir.path(), 2, 2);
        let staging = StagingDir::create_clean().expect("staging");
        let mut opts = SessionImportOpts::disabled();
        let records = run_sessions_import(&profile, &staging, &mut opts);
        clear_anvil_home();
        assert!(records.is_empty(), "disabled opts must return empty records");
    }

    // ── run_sessions_import: oversized files are skipped ─────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn oversized_sessions_are_skipped() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());
        let profile_dir = dir.path().join(".claude");
        let proj_dir = profile_dir.join("projects").join("proj-big");
        std::fs::create_dir_all(&proj_dir).expect("mkdir");

        // Write a file that's exactly SESSION_MAX_BYTES + 1 bytes.
        let big_path = proj_dir.join("big-session.jsonl");
        let oversize = SESSION_MAX_BYTES + 1;
        let content = vec![b'x'; oversize as usize];
        std::fs::write(&big_path, &content).expect("write big file");

        let staging = StagingDir::create_clean().expect("staging");
        let mut opts = SessionImportOpts {
            include_sessions: true,
            summarizer: Box::new(MockSummarizer),
        };
        let records = run_sessions_import(&profile_dir, &staging, &mut opts);
        clear_anvil_home();

        assert_eq!(records.len(), 1, "one record expected");
        assert_eq!(
            records[0].status,
            SessionImportStatus::Skipped,
            "oversized session must be Skipped"
        );
        assert_eq!(
            records[0].reason.as_deref(),
            Some("oversized"),
            "reason must be 'oversized'"
        );
    }

    // ── run_sessions_import: idempotency (re-run skips processed) ────────────

    #[test]
    #[serial(anvil_config_home)]
    fn idempotency_second_run_skips_already_processed() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());
        let profile = build_cc_sessions_fixture(dir.path(), 2, 3);
        let staging = StagingDir::create_clean().expect("staging");

        let mut opts1 = SessionImportOpts {
            include_sessions: true,
            summarizer: Box::new(MockSummarizer),
        };
        let first_records = run_sessions_import(&profile, &staging, &mut opts1);

        let summarized_first: Vec<_> = first_records
            .iter()
            .filter(|r| r.status == SessionImportStatus::Summarized)
            .collect();
        assert_eq!(summarized_first.len(), 2, "first run should summarize 2 sessions");

        // Second run with the same staging dir (progress.json is preserved).
        let mut opts2 = SessionImportOpts {
            include_sessions: true,
            summarizer: Box::new(MockSummarizer),
        };
        let second_records = run_sessions_import(&profile, &staging, &mut opts2);
        clear_anvil_home();

        let already_processed: Vec<_> = second_records
            .iter()
            .filter(|r| r.status == SessionImportStatus::AlreadyProcessed)
            .collect();
        assert_eq!(
            already_processed.len(),
            2,
            "second run must report all sessions as AlreadyProcessed; got {:?}",
            second_records.iter().map(|r| &r.status).collect::<Vec<_>>()
        );
    }

    // ── run_sessions_import: lessons become nominations in staging ────────────

    #[test]
    #[serial(anvil_config_home)]
    fn lessons_are_written_to_staging_nominations() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());
        let profile = build_cc_sessions_fixture(dir.path(), 1, 2);
        let staging = StagingDir::create_clean().expect("staging");
        let mut opts = SessionImportOpts {
            include_sessions: true,
            summarizer: Box::new(MockSummarizer),
        };
        let records = run_sessions_import(&profile, &staging, &mut opts);
        clear_anvil_home();

        let summarized: Vec<_> = records
            .iter()
            .filter(|r| r.status == SessionImportStatus::Summarized)
            .collect();
        assert_eq!(summarized.len(), 1, "one session should be summarized");

        let nominations_written = summarized[0].nominations_written;
        assert!(
            nominations_written > 0,
            "at least one nomination should be written"
        );

        // Verify the nomination file exists in staging.
        let sess_id = &summarized[0].session_id;
        let nom_path = staging.path(&format!("nominations/sess-{sess_id}-0000.json"));
        assert!(
            nom_path.exists(),
            "nomination file must exist at {nom_path:?}"
        );

        // Verify import metadata stamps.
        let content = std::fs::read_to_string(&nom_path).expect("read nomination");
        let v: serde_json::Value = serde_json::from_str(&content).expect("parse nomination json");
        assert_eq!(
            v.get("imported_from").and_then(|v| v.as_str()),
            Some("claude_code"),
            "nomination must have imported_from: claude_code"
        );
        assert!(
            v.get("imported_at").is_some(),
            "nomination must have imported_at"
        );
        assert!(
            v.get("source_path").is_some(),
            "nomination must have source_path"
        );
    }

    // ── chunked summarization test ────────────────────────────────────────────

    #[test]
    fn chunked_summarization_merges_chunks() {
        let s = MockSummarizer;
        // Build a transcript large enough to produce multiple chunks.
        let turn = "User: Fix something\nAssistant: Done.\n";
        let big = turn.repeat(CHUNK_CHARS / turn.len() + 5);
        let result = summarize_chunked(&big, &s);
        assert!(result.is_ok(), "chunked summarization should succeed");
    }

    // ── split_on_user_turns respects boundary ─────────────────────────────────

    #[test]
    fn split_on_user_turns_respects_max_chars() {
        let turn = "User: hello\nAssistant: hi\n";
        let text = turn.repeat(100);
        let chunks = split_on_user_turns(&text, 200);
        for chunk in &chunks {
            assert!(
                chunk.len() <= 200 + turn.len(), // allow one turn overflow
                "chunk too large: {} chars",
                chunk.len()
            );
        }
        // The full content must be covered.
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert!(total > 0, "chunks must have content");
    }

    // ── parse_summary_json handles code fences ────────────────────────────────

    #[test]
    fn parse_summary_json_strips_code_fences() {
        let json = r#"```json
{
  "summary": "Did stuff.",
  "tasks_completed": ["task a"],
  "tasks_open": [],
  "files_modified": [],
  "lessons_learned": ["lesson 1"]
}
```"#;
        let result = parse_summary_json(json).expect("parse with code fence");
        assert_eq!(result.summary, "Did stuff.");
        assert_eq!(result.tasks_completed, vec!["task a"]);
        assert_eq!(result.lessons_learned, vec!["lesson 1"]);
    }

    #[test]
    fn parse_summary_json_plain_json() {
        let json = r#"{"summary":"Plain.","tasks_completed":[],"tasks_open":["open 1"],"files_modified":[],"lessons_learned":[]}"#;
        let result = parse_summary_json(json).expect("plain json");
        assert_eq!(result.summary, "Plain.");
        assert_eq!(result.tasks_open, vec!["open 1"]);
    }

    // ── progress save/load round-trip ─────────────────────────────────────────

    #[test]
    fn progress_save_load_round_trip() {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join("progress.json");
        let mut p = SessionProgress {
            started_at: "2026-05-15T00:00:00Z".to_string(),
            ..Default::default()
        };
        p.mark_processed("hash-abc".to_string());
        p.mark_skipped_oversized("hash-big".to_string());
        p.save(&path).expect("save");

        let loaded = SessionProgress::load(&path);
        assert_eq!(loaded.processed, vec!["hash-abc"]);
        assert_eq!(loaded.skipped_oversized, vec!["hash-big"]);
        assert_eq!(loaded.started_at, "2026-05-15T00:00:00Z");
    }

    // ── cancellation and resume ───────────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn resumable_after_partial_run() {
        // Simulate cancellation after first session.
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());
        let profile = build_cc_sessions_fixture(dir.path(), 3, 2);
        let staging = StagingDir::create_clean().expect("staging");

        // Write a progress file that marks session 0 as processed.
        let progress_path = staging.path("sessions/progress.json");
        let profile_dir = dir.path().join(".claude");
        let proj0_dir = profile_dir.join("projects").join("proj-0000");
        let sess0_path = proj0_dir.join("sess-0000.jsonl");

        let hash0 = crate::import::sha256_file(&sess0_path)
            .unwrap_or_else(|| "fallback".to_string());
        let mut p = SessionProgress {
            started_at: "2026-05-15T00:00:00Z".to_string(),
            ..Default::default()
        };
        p.mark_processed(hash0);
        p.save(&progress_path).expect("save progress");

        let mut opts = SessionImportOpts {
            include_sessions: true,
            summarizer: Box::new(MockSummarizer),
        };
        let records = run_sessions_import(&profile, &staging, &mut opts);
        clear_anvil_home();

        let already: usize = records
            .iter()
            .filter(|r| r.status == SessionImportStatus::AlreadyProcessed)
            .count();
        let summarized: usize = records
            .iter()
            .filter(|r| r.status == SessionImportStatus::Summarized)
            .count();

        assert_eq!(already, 1, "1 session should be AlreadyProcessed (simulated resume)");
        assert_eq!(summarized, 2, "2 remaining sessions should be summarized");
    }

    // ── parse_iso_to_secs ─────────────────────────────────────────────────────

    #[test]
    fn parse_iso_known_date() {
        // 2024-01-15T00:00:00 = 1705276800
        let secs = parse_iso_to_secs("2024-01-15T00:00:00Z");
        assert_eq!(secs, Some(1_705_276_800), "known timestamp must parse");
    }

    // ── commit_sessions_from_staging: merges into existing daily ──────────────

    #[test]
    #[serial(anvil_config_home)]
    fn commit_merges_sessions_into_existing_daily() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // Create a pre-existing daily record.
        let live_daily = dir.path().join("daily");
        std::fs::create_dir_all(&live_daily).expect("mkdir daily");
        let existing = crate::daily::DailySummary {
            date: "2024-01-15".to_string(),
            sessions: vec![crate::daily::SessionSummary {
                session_id: "existing-session".to_string(),
                model: "claude-sonnet-4-6".to_string(),
                duration_secs: 100,
                tokens_used: 500,
                messages_count: 2,
                tasks_completed: vec![],
                tasks_open: vec![],
                files_modified: vec![],
                nominations_generated: 0,
                credentials_auto_vaulted: 0,
            }],
            open_items: vec![],
            total_tokens: 500,
            total_cost_usd: 0.0,
        };
        std::fs::write(
            live_daily.join("2024-01-15.json"),
            serde_json::to_string_pretty(&existing).expect("serialize"),
        )
        .expect("write existing daily");

        // Create staging with an additional session for the same date.
        let staging = StagingDir::create_clean().expect("staging");
        let staged_daily = staging.path("sessions/daily");
        std::fs::create_dir_all(&staged_daily).expect("mkdir staged daily");

        let new_summary = crate::daily::DailySummary {
            date: "2024-01-15".to_string(),
            sessions: vec![crate::daily::SessionSummary {
                session_id: "new-imported-session".to_string(),
                model: "claude-haiku-4-5".to_string(),
                duration_secs: 300,
                tokens_used: 1_000,
                messages_count: 4,
                tasks_completed: vec!["Imported from CC".to_string()],
                tasks_open: vec![],
                files_modified: vec![],
                nominations_generated: 1,
                credentials_auto_vaulted: 0,
            }],
            open_items: vec![],
            total_tokens: 1_000,
            total_cost_usd: 0.0,
        };
        std::fs::write(
            staged_daily.join("2024-01-15.json"),
            serde_json::to_string_pretty(&new_summary).expect("serialize new"),
        )
        .expect("write staged daily");

        let committed = commit_sessions_from_staging(&staging).expect("commit");
        clear_anvil_home();

        assert_eq!(committed, 1, "one daily record should be committed");

        let merged_path = dir.path().join("daily").join("2024-01-15.json");
        assert!(merged_path.exists(), "merged daily file must exist");
        let merged_text = std::fs::read_to_string(&merged_path).expect("read");
        let merged: crate::daily::DailySummary =
            serde_json::from_str(&merged_text).expect("parse merged");
        assert_eq!(
            merged.sessions.len(),
            2,
            "merged record must have 2 sessions; got {}",
            merged.sessions.len()
        );
    }
}
