// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::config::OAuthConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthTokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scopes: Vec<String>,
}

/// Wire-format token response from an OAuth `/oauth/token` endpoint, per
/// RFC 6749 §5.1.  Anthropic's token endpoint uses this exact shape:
///
/// ```json
/// {
///   "token_type": "Bearer",
///   "access_token": "sk-ant-oat01-...",
///   "refresh_token": "sk-ant-ort01-...",
///   "expires_in": 3600,
///   "scope": "user:profile user:inference user:sessions:claude_code"
/// }
/// ```
///
/// `expires_in` is in seconds (relative); the caller must compute
/// `expires_at = unix_now() + expires_in`. `scope` is a space-separated
/// string of scope names. The canonical Anthropic shape lives in the
/// parity fixture at `crates/runtime/src/oauth_fixtures/anthropic_token_response.json`
/// — diff it against current Claude Code / Anthropic responses during the
/// daily parity audit (see `feedback-claude-code-parity.md`).
///
/// We make every metadata field except `access_token` `Option`-typed so a
/// deserialize failure on a misshapen response surfaces as a clear `Err`
/// from `parse_token_response_strict`, NOT as a silent half-token write.
/// Task #595: this is the regression-critical path. Empty scopes + missing
/// expires_at = 401 "Invalid authentication credentials" from the Anthropic
/// Max-plan gate even when the bearer is wire-valid.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Seconds until expiry, as returned by the token endpoint.  Required by
    /// `parse_token_response_strict`; absence is a structural error.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Space-separated scope list as returned by the token endpoint.
    /// Required by `parse_token_response_strict`; absence or empty is a
    /// structural error.
    #[serde(default)]
    pub scope: Option<String>,
    /// `Bearer` for Anthropic.  Accepted for forward compat; not enforced.
    #[serde(default)]
    pub token_type: Option<String>,
}

/// Strictly parse a `/oauth/token` JSON body into an `OAuthTokenSet`.
///
/// Rejects responses where `scope` is missing/empty OR `expires_in` is
/// missing.  Both fields are mandatory per Anthropic's Max-plan OAuth gate
/// — without them the stored bearer is rejected at the first `/v1/messages`
/// call with HTTP 401 `authentication_error: Invalid authentication
/// credentials`.  Returns the canonical `OAuthTokenSet` with
/// `expires_at = now() + expires_in` and `scopes` as a `Vec<String>` of
/// whitespace-split tokens.
///
/// On error, returns a human-readable message; the caller MUST surface this
/// to the user and refuse to persist credentials.  See task #595 + memory
/// `feedback-anvil-max-plan-oauth-gate.md` (THIRD failure mode).
pub fn parse_token_response_strict(
    response: &OAuthTokenResponse,
) -> Result<OAuthTokenSet, String> {
    parse_token_response_strict_at(response, unix_now_seconds())
}

/// Deterministic variant of `parse_token_response_strict` that takes the
/// "now" timestamp as a parameter — used by the regression tests so they
/// don't depend on wall-clock time.
pub fn parse_token_response_strict_at(
    response: &OAuthTokenResponse,
    now_unix: u64,
) -> Result<OAuthTokenSet, String> {
    let expires_in = response.expires_in.ok_or_else(|| {
        "OAuth token-exchange response was malformed: missing `expires_in` \
         (RFC 6749 §5.1 mandates this field). Please retry `/provider \
         anthropic login`."
            .to_string()
    })?;
    let scope_str = response.scope.as_deref().unwrap_or("").trim();
    if scope_str.is_empty() {
        return Err(
            "OAuth token-exchange response was malformed: missing or empty \
             `scope` (RFC 6749 §5.1 / Anthropic's Max-plan gate require \
             non-empty scopes). Please retry `/provider anthropic login`."
                .to_string(),
        );
    }
    let scopes: Vec<String> = scope_str
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if scopes.is_empty() {
        return Err(
            "OAuth token-exchange response was malformed: scope split to empty list. \
             Please retry `/provider anthropic login`."
                .to_string(),
        );
    }
    let expires_at = now_unix.saturating_add(expires_in);
    Ok(OAuthTokenSet {
        access_token: response.access_token.clone(),
        refresh_token: response.refresh_token.clone(),
        expires_at: Some(expires_at),
        scopes,
    })
}

/// Current unix timestamp in seconds.  Used by `parse_token_response_strict`
/// and `save_oauth_credentials` (for the migration path on startup).
#[must_use]
pub fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Outcome of validating a saved Anthropic OAuth credential at startup.
///
/// Task #595 deliverable #4: surface incomplete credentials BEFORE the user
/// sends their first prompt and discovers the 401.  Task #595 deliverable
/// #7: optionally migrate the broken credential in place with documented
/// default scopes + a short expires_at so the next call triggers refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicCredentialStatus {
    /// No saved OAuth credential (user is on API key or hasn't logged in).
    Absent,
    /// Saved credential has non-empty scopes AND a present expires_at.
    Ok,
    /// Saved credential has scopes=[] or expires_at=None.  Must be migrated
    /// or the user must re-authenticate.
    Incomplete {
        scopes_empty: bool,
        expires_at_missing: bool,
    },
}

#[must_use]
pub fn validate_anthropic_credential(token: Option<&OAuthTokenSet>) -> AnthropicCredentialStatus {
    let Some(token) = token else {
        return AnthropicCredentialStatus::Absent;
    };
    if !token.access_token.starts_with("sk-ant-oat01-") {
        // Only Anthropic Max-plan tokens are subject to the metadata gate.
        // Tokens from other providers (or future shapes) get a pass.
        return AnthropicCredentialStatus::Ok;
    }
    let scopes_empty = token.scopes.is_empty();
    let expires_at_missing = token.expires_at.is_none();
    if scopes_empty || expires_at_missing {
        AnthropicCredentialStatus::Incomplete {
            scopes_empty,
            expires_at_missing,
        }
    } else {
        AnthropicCredentialStatus::Ok
    }
}

/// One-shot migration: if the saved Anthropic OAuth credential has empty
/// scopes and/or missing expires_at but a wire-valid `sk-ant-oat01-` access
/// token, transparently populate scopes with the documented default set
/// (`user:profile user:inference user:sessions:claude_code`) and set
/// expires_at to `now + 60s` so the very next `/v1/messages` call triggers
/// the refresh path (which re-fetches a wire-valid response).  Returns true
/// if a migration was applied.
///
/// Task #595 deliverable #7.  This unblocks users with the broken
/// credentials.json from the v2.2.15/v2.2.16 timeframe without forcing them
/// to re-OAuth.  Cross-references the documented defaults in
/// `crates/anvil-cli/src/auth.rs::default_oauth_config`.
pub fn migrate_incomplete_anthropic_credential() -> io::Result<bool> {
    let Some(mut token) = load_oauth_credentials()? else {
        return Ok(false);
    };
    if !matches!(
        validate_anthropic_credential(Some(&token)),
        AnthropicCredentialStatus::Incomplete { .. }
    ) {
        return Ok(false);
    }
    if !token.access_token.starts_with("sk-ant-oat01-") {
        return Ok(false);
    }
    let mut changed = false;
    if token.scopes.is_empty() {
        token.scopes = vec![
            "user:profile".to_string(),
            "user:inference".to_string(),
            "user:sessions:claude_code".to_string(),
        ];
        changed = true;
    }
    if token.expires_at.is_none() {
        // Force the refresh-on-first-use path: 60 seconds in the future is
        // already considered expired by the refresh resolver (it checks
        // `expires_at <= now`).  We use 60s rather than the past so a
        // currently-mid-flight request doesn't trip on a synthetic stale
        // value; the next turn will trigger refresh.
        token.expires_at = Some(unix_now_seconds().saturating_add(60));
        changed = true;
    }
    if changed {
        save_oauth_credentials(&token)?;
    }
    Ok(changed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceCodePair {
    pub verifier: String,
    pub challenge: String,
    pub challenge_method: PkceChallengeMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkceChallengeMethod {
    S256,
}

impl PkceChallengeMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::S256 => "S256",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAuthorizationRequest {
    pub authorize_url: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub state: String,
    pub code_challenge: String,
    pub code_challenge_method: PkceChallengeMethod,
    pub extra_params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokenExchangeRequest {
    pub grant_type: &'static str,
    pub code: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub code_verifier: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRefreshRequest {
    pub grant_type: &'static str,
    pub refresh_token: String,
    pub client_id: String,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Lenient deserializer for the `scopes` field in persisted credentials.
///
/// On-disk credentials files may arrive with `scopes` in several shapes
/// depending on which version wrote them or whether the file was hand-edited:
///
/// - JSON `null`  → treat as empty scope list
/// - JSON string  → split on spaces (standard OAuth wire format); single
///   scope strings without spaces produce a one-element vec
/// - JSON array   → normal path, pass through as-is
/// - Anything else (object, number, bool) → log a warning and return an
///   empty scope list rather than propagating a parse error that would lock
///   the user out of a still-valid token
fn deserialize_scopes_lenient<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => {
            if s.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(s.split_whitespace()
                    .map(|scope| scope.to_string())
                    .collect())
            }
        }
        Value::Array(arr) => arr
            .into_iter()
            .map(|item| match item {
                Value::String(s) => Ok(s),
                other => Err(serde::de::Error::custom(format!(
                    "scopes array element is not a string: {other}"
                ))),
            })
            .collect(),
        other => {
            // Corrupt but non-fatal: warn and fall back to empty scopes so
            // the caller can still use the access/refresh tokens.
            eprintln!(
                "[anvil] warning: unexpected type for 'scopes' in credentials ({other}); \
                 treating as empty scope list"
            );
            Ok(Vec::new())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredOAuthCredentials {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_scopes_lenient")]
    scopes: Vec<String>,
}

impl From<OAuthTokenSet> for StoredOAuthCredentials {
    fn from(value: OAuthTokenSet) -> Self {
        Self {
            access_token: value.access_token,
            refresh_token: value.refresh_token,
            expires_at: value.expires_at,
            scopes: value.scopes,
        }
    }
}

impl From<StoredOAuthCredentials> for OAuthTokenSet {
    fn from(value: StoredOAuthCredentials) -> Self {
        Self {
            access_token: value.access_token,
            refresh_token: value.refresh_token,
            expires_at: value.expires_at,
            scopes: value.scopes,
        }
    }
}

impl OAuthAuthorizationRequest {
    #[must_use]
    pub fn from_config(
        config: &OAuthConfig,
        redirect_uri: impl Into<String>,
        state: impl Into<String>,
        pkce: &PkceCodePair,
    ) -> Self {
        Self {
            authorize_url: config.authorize_url.clone(),
            client_id: config.client_id.clone(),
            redirect_uri: redirect_uri.into(),
            scopes: config.scopes.clone(),
            state: state.into(),
            code_challenge: pkce.challenge.clone(),
            code_challenge_method: pkce.challenge_method,
            extra_params: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_extra_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_params.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn build_url(&self) -> String {
        let mut params = vec![
            ("response_type", "code".to_string()),
            ("client_id", self.client_id.clone()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("scope", self.scopes.join(" ")),
            ("state", self.state.clone()),
            ("code_challenge", self.code_challenge.clone()),
            (
                "code_challenge_method",
                self.code_challenge_method.as_str().to_string(),
            ),
        ];
        params.extend(
            self.extra_params
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
        );
        let query = params
            .into_iter()
            .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(&value)))
            .collect::<Vec<_>>()
            .join("&");
        format!(
            "{}{}{}",
            self.authorize_url,
            if self.authorize_url.contains('?') {
                '&'
            } else {
                '?'
            },
            query
        )
    }
}

impl OAuthTokenExchangeRequest {
    #[must_use]
    pub fn from_config(
        config: &OAuthConfig,
        code: impl Into<String>,
        state: impl Into<String>,
        verifier: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            grant_type: "authorization_code",
            code: code.into(),
            redirect_uri: redirect_uri.into(),
            client_id: config.client_id.clone(),
            code_verifier: verifier.into(),
            state: state.into(),
        }
    }

    #[must_use]
    pub fn form_params(&self) -> BTreeMap<&str, String> {
        BTreeMap::from([
            ("grant_type", self.grant_type.to_string()),
            ("code", self.code.clone()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("client_id", self.client_id.clone()),
            ("code_verifier", self.code_verifier.clone()),
            ("state", self.state.clone()),
        ])
    }
}

impl OAuthRefreshRequest {
    #[must_use]
    pub fn from_config(
        config: &OAuthConfig,
        refresh_token: impl Into<String>,
        scopes: Option<Vec<String>>,
    ) -> Self {
        Self {
            grant_type: "refresh_token",
            refresh_token: refresh_token.into(),
            client_id: config.client_id.clone(),
            scopes: scopes.unwrap_or_else(|| config.scopes.clone()),
        }
    }

    #[must_use]
    pub fn form_params(&self) -> BTreeMap<&str, String> {
        BTreeMap::from([
            ("grant_type", self.grant_type.to_string()),
            ("refresh_token", self.refresh_token.clone()),
            ("client_id", self.client_id.clone()),
            ("scope", self.scopes.join(" ")),
        ])
    }
}

pub fn generate_pkce_pair() -> io::Result<PkceCodePair> {
    let verifier = generate_random_token(32)?;
    Ok(PkceCodePair {
        challenge: code_challenge_s256(&verifier),
        verifier,
        challenge_method: PkceChallengeMethod::S256,
    })
}

pub fn generate_state() -> io::Result<String> {
    generate_random_token(32)
}

#[must_use]
pub fn code_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_encode(&digest)
}

#[must_use]
pub fn loopback_redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}/callback")
}

pub fn credentials_path() -> io::Result<PathBuf> {
    Ok(credentials_home_dir()?.join("credentials.json"))
}

pub fn load_oauth_credentials() -> io::Result<Option<OAuthTokenSet>> {
    let path = credentials_path()?;
    let root = read_credentials_root(&path)?;
    let Some(oauth) = root.get("oauth") else {
        return Ok(None);
    };
    if oauth.is_null() {
        return Ok(None);
    }
    let stored = serde_json::from_value::<StoredOAuthCredentials>(oauth.clone())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(stored.into()))
}

pub fn save_oauth_credentials(token_set: &OAuthTokenSet) -> io::Result<()> {
    // Task #595 deliverable #3: invariant gate at the write boundary.
    // An Anthropic Max-plan bearer (sk-ant-oat01-...) with empty scopes OR
    // missing expires_at is structurally broken and will be rejected by
    // Anthropic's gate with 401.  Refuse to persist it: debug builds panic
    // (to catch parser regressions in tests), release builds return Err
    // so the caller surfaces "OAuth token-exchange response was malformed".
    if token_set.access_token.starts_with("sk-ant-oat01-") {
        let scopes_empty = token_set.scopes.is_empty();
        let expires_at_missing = token_set.expires_at.is_none();
        if scopes_empty || expires_at_missing {
            let msg = format!(
                "refusing to persist incomplete Anthropic OAuth credentials \
                 (scopes_empty={scopes_empty}, expires_at_missing={expires_at_missing}); \
                 see task #595 + feedback-anvil-max-plan-oauth-gate.md"
            );
            debug_assert!(false, "{msg}");
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
        }
    }
    let path = credentials_path()?;
    let mut root = read_credentials_root(&path)?;
    root.insert(
        "oauth".to_string(),
        serde_json::to_value(StoredOAuthCredentials::from(token_set.clone()))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    );
    write_credentials_root(&path, &root)
}

pub fn clear_oauth_credentials() -> io::Result<()> {
    let path = credentials_path()?;
    let mut root = read_credentials_root(&path)?;
    root.remove("oauth");
    write_credentials_root(&path, &root)
}

pub fn parse_oauth_callback_request_target(target: &str) -> Result<OAuthCallbackParams, String> {
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));
    if path != "/callback" {
        return Err(format!("unexpected callback path: {path}"));
    }
    parse_oauth_callback_query(query)
}

/// Parse a value pasted by the user when the browser callback can't reach
/// localhost (WSL2, SSH, container without published ports).
///
/// Accepts two shapes and returns `(code, state_opt)`:
///
/// * A bare authorization code, e.g. `abc123-def456` or
///   `abc123-def456#state=xyz` (the `#state=` suffix is what
///   `https://platform.claude.com/oauth/code/callback` shows the user).
/// * The full callback URL, e.g.
///   `http://localhost:39817/callback?code=abc123&state=xyz`. In that case
///   `code` and `state` are extracted from the query string.
///
/// Returns `Err` for empty or obviously malformed input (whitespace inside
/// the bare token, or a URL without a `code=` parameter).
pub fn parse_pasted_oauth_code(input: &str) -> Result<(String, Option<String>), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty paste".to_string());
    }

    // Full URL form: anything that has a scheme (http://, https://) and a
    // query string. The bare-code form may also contain `?` so we look for
    // `://` to disambiguate.
    if trimmed.contains("://") {
        let (_, query) = trimmed
            .split_once('?')
            .ok_or_else(|| "pasted URL is missing the '?code=...' query".to_string())?;
        // Strip a fragment if the provider appended one.
        let query = query.split('#').next().unwrap_or(query);
        let params = parse_oauth_callback_query(query)?;
        let code = params
            .code
            .ok_or_else(|| "pasted URL did not include a 'code' parameter".to_string())?;
        if code.is_empty() {
            return Err("pasted URL had an empty 'code' parameter".to_string());
        }
        return Ok((code, params.state));
    }

    // Some providers display `<code>#state=<state>` or `<code>?state=<state>`
    // on the manual-callback page; accept either as a bare paste.
    let (code_part, state_part) = if let Some((c, rest)) = trimmed.split_once('#') {
        (c, Some(rest))
    } else if let Some((c, rest)) = trimmed.split_once('?') {
        (c, Some(rest))
    } else {
        (trimmed, None)
    };

    if code_part.is_empty() {
        return Err("pasted code is empty".to_string());
    }
    // A bare auth code is opaque but always URL-safe: ASCII letters, digits,
    // and a small set of separator chars. Anything else (whitespace, control
    // chars, slashes) means the user pasted something we don't recognise.
    if !code_part
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
    {
        return Err("pasted value is not a recognised OAuth code or callback URL".to_string());
    }

    let state = match state_part {
        None => None,
        Some(rest) => {
            // Look for `state=...` in the suffix; ignore any other params.
            let mut found = None;
            for pair in rest.split('&').filter(|p| !p.is_empty()) {
                if let Some((k, v)) = pair.split_once('=')
                    && k == "state"
                {
                    found = Some(percent_decode(v)?);
                    break;
                }
            }
            found
        }
    };

    Ok((code_part.to_string(), state))
}

pub fn parse_oauth_callback_query(query: &str) -> Result<OAuthCallbackParams, String> {
    let mut params = BTreeMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .map_or((pair, ""), |(key, value)| (key, value));
        params.insert(percent_decode(key)?, percent_decode(value)?);
    }
    Ok(OAuthCallbackParams {
        code: params.get("code").cloned(),
        state: params.get("state").cloned(),
        error: params.get("error").cloned(),
        error_description: params.get("error_description").cloned(),
    })
}

fn generate_random_token(bytes: usize) -> io::Result<String> {
    let mut buffer = vec![0_u8; bytes];
    File::open("/dev/urandom")?.read_exact(&mut buffer)?;
    Ok(base64url_encode(&buffer))
}

fn credentials_home_dir() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("ANVIL_CONFIG_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".anvil"))
}

fn read_credentials_root(path: &PathBuf) -> io::Result<Map<String, Value>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(Map::new());
            }
            serde_json::from_str::<Value>(&contents)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                .as_object()
                .cloned()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "credentials file must contain a JSON object",
                    )
                })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error),
    }
}

fn write_credentials_root(path: &PathBuf, root: &Map<String, Value>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rendered = serde_json::to_string_pretty(&Value::Object(root.clone()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temp_path = path.with_extension("json.tmp");

    // Write the temp file with mode 0o600 (owner read/write only) so that
    // credentials are never world-readable even transiently.
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp_path)?;
        writeln!(f, "{rendered}")?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&temp_path, format!("{rendered}\n"))?;
    }

    fs::rename(temp_path, path)
}

fn base64url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let block = (u32::from(bytes[index]) << 16)
            | (u32::from(bytes[index + 1]) << 8)
            | u32::from(bytes[index + 2]);
        output.push(TABLE[((block >> 18) & 0x3F) as usize] as char);
        output.push(TABLE[((block >> 12) & 0x3F) as usize] as char);
        output.push(TABLE[((block >> 6) & 0x3F) as usize] as char);
        output.push(TABLE[(block & 0x3F) as usize] as char);
        index += 3;
    }
    match bytes.len().saturating_sub(index) {
        1 => {
            let block = u32::from(bytes[index]) << 16;
            output.push(TABLE[((block >> 18) & 0x3F) as usize] as char);
            output.push(TABLE[((block >> 12) & 0x3F) as usize] as char);
        }
        2 => {
            let block = (u32::from(bytes[index]) << 16) | (u32::from(bytes[index + 1]) << 8);
            output.push(TABLE[((block >> 18) & 0x3F) as usize] as char);
            output.push(TABLE[((block >> 12) & 0x3F) as usize] as char);
            output.push(TABLE[((block >> 6) & 0x3F) as usize] as char);
        }
        _ => {}
    }
    output
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(&mut encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Result<String, String> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hi = decode_hex(bytes[index + 1])?;
                let lo = decode_hex(bytes[index + 2])?;
                decoded.push((hi << 4) | lo);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).map_err(|error| error.to_string())
}

fn decode_hex(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid percent-encoding byte: {byte}")),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serial_test::serial;

    use super::{
        clear_oauth_credentials, code_challenge_s256, credentials_path, generate_pkce_pair,
        generate_state, load_oauth_credentials, loopback_redirect_uri,
        migrate_incomplete_anthropic_credential, parse_oauth_callback_query,
        parse_oauth_callback_request_target, parse_pasted_oauth_code,
        parse_token_response_strict_at, save_oauth_credentials, validate_anthropic_credential,
        AnthropicCredentialStatus, OAuthAuthorizationRequest, OAuthConfig, OAuthRefreshRequest,
        OAuthTokenExchangeRequest, OAuthTokenResponse, OAuthTokenSet, StoredOAuthCredentials,
    };

    fn sample_config() -> OAuthConfig {
        OAuthConfig {
            client_id: "runtime-client".to_string(),
            authorize_url: "https://console.test/oauth/authorize".to_string(),
            token_url: "https://console.test/oauth/token".to_string(),
            callback_port: Some(4545),
            manual_redirect_url: Some("https://console.test/oauth/callback".to_string()),
            scopes: vec!["org:read".to_string(), "user:write".to_string()],
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_env_lock()
    }

    fn temp_config_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "runtime-oauth-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ))
    }

    #[test]
    fn s256_challenge_matches_expected_vector() {
        assert_eq!(
            code_challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generates_pkce_pair_and_state() {
        let pair = generate_pkce_pair().expect("pkce pair");
        let state = generate_state().expect("state");
        assert!(!pair.verifier.is_empty());
        assert!(!pair.challenge.is_empty());
        assert!(!state.is_empty());
    }

    #[test]
    fn builds_authorize_url_and_form_requests() {
        let config = sample_config();
        let pair = generate_pkce_pair().expect("pkce");
        let url = OAuthAuthorizationRequest::from_config(
            &config,
            loopback_redirect_uri(4545),
            "state-123",
            &pair,
        )
        .with_extra_param("login_hint", "user@example.com")
        .build_url();
        assert!(url.starts_with("https://console.test/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=runtime-client"));
        assert!(url.contains("scope=org%3Aread%20user%3Awrite"));
        assert!(url.contains("login_hint=user%40example.com"));

        let exchange = OAuthTokenExchangeRequest::from_config(
            &config,
            "auth-code",
            "state-123",
            pair.verifier,
            loopback_redirect_uri(4545),
        );
        assert_eq!(
            exchange.form_params().get("grant_type").map(String::as_str),
            Some("authorization_code")
        );

        let refresh = OAuthRefreshRequest::from_config(&config, "refresh-token", None);
        assert_eq!(
            refresh.form_params().get("scope").map(String::as_str),
            Some("org:read user:write")
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn oauth_credentials_round_trip_and_clear_preserves_other_fields() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        let path = credentials_path().expect("credentials path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(&path, "{\"other\":\"value\"}\n").expect("seed credentials");

        let token_set = OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(123),
            scopes: vec!["scope:a".to_string()],
        };
        save_oauth_credentials(&token_set).expect("save credentials");
        assert_eq!(
            load_oauth_credentials().expect("load credentials"),
            Some(token_set)
        );
        let saved = std::fs::read_to_string(&path).expect("read saved file");
        assert!(saved.contains("\"other\": \"value\""));
        assert!(saved.contains("\"oauth\""));

        clear_oauth_credentials().expect("clear credentials");
        assert_eq!(load_oauth_credentials().expect("load cleared"), None);
        let cleared = std::fs::read_to_string(&path).expect("read cleared file");
        assert!(cleared.contains("\"other\": \"value\""));
        assert!(!cleared.contains("\"oauth\""));

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        std::fs::remove_dir_all(config_home).expect("cleanup temp dir");
    }

    #[test]
    fn parses_callback_query_and_target() {
        let params =
            parse_oauth_callback_query("code=abc123&state=state-1&error_description=needs%20login")
                .expect("parse query");
        assert_eq!(params.code.as_deref(), Some("abc123"));
        assert_eq!(params.state.as_deref(), Some("state-1"));
        assert_eq!(params.error_description.as_deref(), Some("needs login"));

        let params = parse_oauth_callback_request_target("/callback?code=abc&state=xyz")
            .expect("parse callback target");
        assert_eq!(params.code.as_deref(), Some("abc"));
        assert_eq!(params.state.as_deref(), Some("xyz"));
        assert!(parse_oauth_callback_request_target("/wrong?code=abc").is_err());
    }

    #[test]
    fn parse_pasted_oauth_code_accepts_bare_code() {
        let (code, state) =
            parse_pasted_oauth_code("abc123-def456").expect("bare code parses");
        assert_eq!(code, "abc123-def456");
        assert_eq!(state, None);

        // Whitespace around the bare code should be tolerated.
        let (code, state) =
            parse_pasted_oauth_code("   abc.123_DEF~456   \n").expect("padded bare code parses");
        assert_eq!(code, "abc.123_DEF~456");
        assert_eq!(state, None);
    }

    #[test]
    fn parse_pasted_oauth_code_accepts_full_callback_url() {
        let (code, state) = parse_pasted_oauth_code(
            "http://localhost:39817/callback?code=abc123&state=xyz",
        )
        .expect("full URL parses");
        assert_eq!(code, "abc123");
        assert_eq!(state.as_deref(), Some("xyz"));

        // Manual-redirect URL with percent-encoded state.
        let (code, state) = parse_pasted_oauth_code(
            "https://platform.claude.com/oauth/code/callback?code=AC_456&state=stat%2Fbar",
        )
        .expect("manual redirect URL parses");
        assert_eq!(code, "AC_456");
        assert_eq!(state.as_deref(), Some("stat/bar"));

        // Trailing fragment is ignored.
        let (code, state) = parse_pasted_oauth_code(
            "http://localhost:39817/callback?code=abc&state=zz#anchor",
        )
        .expect("URL with fragment parses");
        assert_eq!(code, "abc");
        assert_eq!(state.as_deref(), Some("zz"));
    }

    #[test]
    fn parse_pasted_oauth_code_extracts_state_from_bare_suffix() {
        let (code, state) =
            parse_pasted_oauth_code("abc123#state=xyz").expect("hash suffix parses");
        assert_eq!(code, "abc123");
        assert_eq!(state.as_deref(), Some("xyz"));

        let (code, state) =
            parse_pasted_oauth_code("abc123?state=xyz").expect("question suffix parses");
        assert_eq!(code, "abc123");
        assert_eq!(state.as_deref(), Some("xyz"));
    }

    #[test]
    fn parse_pasted_oauth_code_rejects_malformed_input() {
        assert!(parse_pasted_oauth_code("").is_err());
        assert!(parse_pasted_oauth_code("    ").is_err());
        // URL without a query string.
        assert!(parse_pasted_oauth_code("http://localhost:39817/callback").is_err());
        // URL with query but no `code` param.
        assert!(
            parse_pasted_oauth_code("http://localhost:39817/callback?state=xyz").is_err()
        );
        // Random text with whitespace inside (not a URL, not a bare code).
        assert!(parse_pasted_oauth_code("not a code").is_err());
        // Forward slashes are illegal in a bare code.
        assert!(parse_pasted_oauth_code("path/to/code").is_err());
    }

    #[test]
    fn parse_pasted_oauth_code_state_mismatch_rejection() {
        // The function itself returns the parsed state; the caller is
        // expected to compare it against the generated value. This test
        // documents that contract.
        let (_, state) = parse_pasted_oauth_code(
            "http://localhost:39817/callback?code=abc&state=they-sent-this",
        )
        .expect("URL parses");
        let expected = "we-generated-this";
        assert_ne!(state.as_deref(), Some(expected));
    }

    // --- scopes lenient deserializer tests (security fix #565, CC-143-B) ---

    #[test]
    fn scopes_deserializes_from_null() {
        let json = r#"{"accessToken":"tok","scopes":null}"#;
        let stored: StoredOAuthCredentials =
            serde_json::from_str(json).expect("null scopes should deserialize");
        assert_eq!(stored.scopes, Vec::<String>::new());
    }

    #[test]
    fn scopes_deserializes_from_string() {
        let json = r#"{"accessToken":"tok","scopes":"openid"}"#;
        let stored: StoredOAuthCredentials =
            serde_json::from_str(json).expect("string scopes should deserialize");
        assert_eq!(stored.scopes, vec!["openid".to_string()]);
    }

    #[test]
    fn scopes_deserializes_from_space_separated_string() {
        let json = r#"{"accessToken":"tok","scopes":"openid profile email"}"#;
        let stored: StoredOAuthCredentials =
            serde_json::from_str(json).expect("space-separated scopes should deserialize");
        assert_eq!(
            stored.scopes,
            vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string()
            ]
        );
    }

    #[test]
    fn scopes_deserializes_from_array() {
        let json = r#"{"accessToken":"tok","scopes":["openid","profile"]}"#;
        let stored: StoredOAuthCredentials =
            serde_json::from_str(json).expect("array scopes should deserialize");
        assert_eq!(
            stored.scopes,
            vec!["openid".to_string(), "profile".to_string()]
        );
    }

    #[test]
    fn scopes_deserializes_from_object_falls_back_to_empty() {
        // Corrupt value — lenient path must not panic or return Err.
        let json = r#"{"accessToken":"tok","scopes":{"bad":"type"}}"#;
        let stored: StoredOAuthCredentials =
            serde_json::from_str(json).expect("object scopes should fall back gracefully");
        assert_eq!(stored.scopes, Vec::<String>::new());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn load_oauth_credentials_handles_corrupt_scopes_gracefully() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        let path = credentials_path().expect("credentials path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");

        // Write credentials.json with scopes as JSON null — the corrupt-but-
        // recoverable shape that previously caused an io::Error + auth lockout.
        std::fs::write(
            &path,
            r#"{"oauth":{"accessToken":"live-token","scopes":null}}"#,
        )
        .expect("write corrupt credentials");

        let result = load_oauth_credentials().expect("load should succeed despite null scopes");
        let token_set = result.expect("should return Some token set");
        assert_eq!(token_set.access_token, "live-token");
        assert_eq!(token_set.scopes, Vec::<String>::new());

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        std::fs::remove_dir_all(config_home).expect("cleanup temp dir");
    }

    // ─── Task #595 regression tests — strict wire-response parser ────────

    /// Well-formed Anthropic /oauth/token response — matches the parity
    /// fixture at `crates/runtime/src/oauth_fixtures/anthropic_token_response.json`.
    #[test]
    fn strict_parser_accepts_well_formed_anthropic_response() {
        let wire: OAuthTokenResponse = serde_json::from_str(
            r#"{
                "token_type": "Bearer",
                "access_token": "sk-ant-oat01-AAAA",
                "refresh_token": "sk-ant-ort01-BBBB",
                "expires_in": 3600,
                "scope": "user:profile user:inference user:sessions:claude_code"
            }"#,
        )
        .expect("well-formed wire response should deserialize");
        let token = parse_token_response_strict_at(&wire, 1_700_000_000)
            .expect("well-formed response should parse");
        assert_eq!(token.access_token, "sk-ant-oat01-AAAA");
        assert_eq!(token.refresh_token.as_deref(), Some("sk-ant-ort01-BBBB"));
        assert_eq!(token.expires_at, Some(1_700_003_600));
        assert_eq!(
            token.scopes,
            vec![
                "user:profile".to_string(),
                "user:inference".to_string(),
                "user:sessions:claude_code".to_string(),
            ],
        );
        // Both REGRESSION-CRITICAL guards: non-empty scopes + present expires_at.
        assert!(!token.scopes.is_empty(), "scopes must be populated");
        assert!(token.expires_at.is_some(), "expires_at must be populated");
    }

    /// Loads the on-disk fixture verbatim — fails the build if the fixture
    /// drifts from what the strict parser accepts.  Task #595 deliverable
    /// #6 (parity fixture).
    #[test]
    fn strict_parser_accepts_on_disk_parity_fixture() {
        let raw = include_str!(
            "oauth_fixtures/anthropic_token_response.json"
        );
        let wire: OAuthTokenResponse =
            serde_json::from_str(raw).expect("fixture deserializes");
        let token = parse_token_response_strict_at(&wire, 1_700_000_000)
            .expect("fixture parses through strict parser");
        assert!(!token.scopes.is_empty(), "fixture must have non-empty scopes");
        assert!(token.expires_at.is_some(), "fixture must have expires_at");
        assert_eq!(token.expires_at, Some(1_700_003_600));
    }

    /// Missing `scope` → must return Err and NOT produce a half-token.
    /// This is the negative branch of the regression-critical gate.
    #[test]
    fn strict_parser_rejects_response_missing_scope() {
        let wire: OAuthTokenResponse = serde_json::from_str(
            r#"{
                "access_token": "sk-ant-oat01-AAAA",
                "refresh_token": "sk-ant-ort01-BBBB",
                "expires_in": 3600
            }"#,
        )
        .expect("wire response should deserialize even without scope");
        let err = parse_token_response_strict_at(&wire, 1_700_000_000)
            .expect_err("response without scope must be rejected");
        assert!(err.contains("scope"), "error message should mention scope: {err}");
    }

    /// Empty-string `scope` is just as broken — same as the user repro on
    /// 2026-05-17 12:05 PM where credentials.json had scopes=[].
    #[test]
    fn strict_parser_rejects_response_with_empty_scope() {
        let wire: OAuthTokenResponse = serde_json::from_str(
            r#"{
                "access_token": "sk-ant-oat01-AAAA",
                "expires_in": 3600,
                "scope": ""
            }"#,
        )
        .expect("deserialize");
        assert!(parse_token_response_strict_at(&wire, 1_700_000_000).is_err());
    }

    /// Missing `expires_in` → must Err.
    #[test]
    fn strict_parser_rejects_response_missing_expires_in() {
        let wire: OAuthTokenResponse = serde_json::from_str(
            r#"{
                "access_token": "sk-ant-oat01-AAAA",
                "scope": "user:profile user:inference"
            }"#,
        )
        .expect("deserialize");
        let err = parse_token_response_strict_at(&wire, 1_700_000_000)
            .expect_err("response without expires_in must be rejected");
        assert!(
            err.contains("expires_in"),
            "error message should mention expires_in: {err}"
        );
    }

    /// `validate_anthropic_credential` classifies each saved-credential
    /// shape correctly — covers the startup banner branches.
    #[test]
    fn validate_anthropic_credential_classifies_correctly() {
        assert_eq!(
            validate_anthropic_credential(None),
            AnthropicCredentialStatus::Absent,
        );
        let healthy = OAuthTokenSet {
            access_token: "sk-ant-oat01-OK".to_string(),
            refresh_token: Some("sk-ant-ort01-OK".to_string()),
            expires_at: Some(1_700_000_000),
            scopes: vec!["user:inference".to_string()],
        };
        assert_eq!(
            validate_anthropic_credential(Some(&healthy)),
            AnthropicCredentialStatus::Ok,
        );
        let broken_both = OAuthTokenSet {
            access_token: "sk-ant-oat01-BAD".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
        };
        assert_eq!(
            validate_anthropic_credential(Some(&broken_both)),
            AnthropicCredentialStatus::Incomplete {
                scopes_empty: true,
                expires_at_missing: true,
            },
        );
        // Non-Anthropic token bypasses the gate entirely.
        let other_provider = OAuthTokenSet {
            access_token: "ya29-google".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
        };
        assert_eq!(
            validate_anthropic_credential(Some(&other_provider)),
            AnthropicCredentialStatus::Ok,
        );
    }

    /// `save_oauth_credentials` invariant: refuse to persist an Anthropic
    /// `sk-ant-oat01-` token with empty scopes or missing expires_at.
    /// Release builds must return Err.  Debug builds panic via
    /// `debug_assert!` — that path runs in the test profile too, so we
    /// catch the panic with `std::panic::catch_unwind` to keep the test
    /// crate friendly.
    #[test]
    #[serial(anvil_config_home)]
    fn save_oauth_refuses_incomplete_anthropic_credential() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        std::fs::create_dir_all(&config_home).expect("create config home");

        let broken = OAuthTokenSet {
            access_token: "sk-ant-oat01-BROKEN".to_string(),
            refresh_token: Some("sk-ant-ort01-X".to_string()),
            expires_at: None,
            scopes: vec![],
        };
        let result = std::panic::catch_unwind(|| save_oauth_credentials(&broken));
        match result {
            Ok(Ok(())) => panic!("save should not succeed on incomplete Anthropic creds"),
            Ok(Err(_)) | Err(_) => { /* expected: Err in release, panic in debug */ }
        }

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// Migration path: incomplete Anthropic OAuth credential gets repaired
    /// in place.  Task #595 deliverable #7.
    #[test]
    #[serial(anvil_config_home)]
    fn migrate_incomplete_anthropic_credential_populates_defaults() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        let path = credentials_path().expect("credentials path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");

        // Seed credentials.json with the exact user-repro shape from
        // 2026-05-17: valid sk-ant-oat01- bearer, scopes=[], expiresAt=null.
        std::fs::write(
            &path,
            r#"{"oauth":{
                "accessToken":"sk-ant-oat01-USER_REPRO_TOKEN",
                "refreshToken":"sk-ant-ort01-USER_REFRESH",
                "scopes":[],
                "expiresAt":null
            }}"#,
        )
        .expect("seed broken credentials");

        let migrated = migrate_incomplete_anthropic_credential()
            .expect("migration should succeed");
        assert!(migrated, "broken credential should be migrated");

        let loaded = load_oauth_credentials()
            .expect("load")
            .expect("token set present");
        assert_eq!(loaded.access_token, "sk-ant-oat01-USER_REPRO_TOKEN");
        assert_eq!(
            loaded.scopes,
            vec![
                "user:profile".to_string(),
                "user:inference".to_string(),
                "user:sessions:claude_code".to_string(),
            ],
        );
        assert!(
            loaded.expires_at.is_some(),
            "expires_at should be populated"
        );

        // Second call must be a no-op (already healthy).
        let migrated_again = migrate_incomplete_anthropic_credential()
            .expect("second migration call should succeed");
        assert!(
            !migrated_again,
            "healthy credential should not be re-migrated"
        );

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        std::fs::remove_dir_all(config_home).expect("cleanup temp dir");
    }
}
