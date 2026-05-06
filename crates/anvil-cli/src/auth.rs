//! Provider authentication: login, logout, OAuth flow, and API-key setup.
//!
//! Handles all interactive credential acquisition for Anthropic (OAuth),
//! `OpenAI`, Ollama, and xAI.  The module is intentionally free of any TUI or
//! REPL state so it can be called at startup before `LiveCli` is constructed.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use api::AnvilApiClient;
use runtime::{
    clear_oauth_credentials, generate_pkce_pair, generate_state, loopback_redirect_uri,
    load_oauth_credentials, parse_oauth_callback_request_target, parse_pasted_oauth_code,
    save_oauth_credentials, ConfigLoader, OAuthAuthorizationRequest, OAuthCallbackParams,
    OAuthConfig, OAuthTokenExchangeRequest,
};

use crate::DEFAULT_OAUTH_CALLBACK_PORT;
use crate::vault::write_curl_auth_header;

/// Build the default Anthropic OAuth configuration.
pub(crate) fn default_oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: String::from("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        authorize_url: String::from("https://claude.ai/oauth/authorize"),
        token_url: String::from("https://platform.claude.com/v1/oauth/token"),
        callback_port: None,
        manual_redirect_url: Some(String::from(
            "https://platform.claude.com/oauth/code/callback",
        )),
        scopes: vec![
            String::from("user:profile"),
            String::from("user:inference"),
            String::from("user:sessions:claude_code"),
        ],
    }
}

/// Interactive provider login menu.
///
/// Dispatches to the correct per-provider setup function.  When `provider` is
/// `None` the user is presented with a numbered menu.
pub(crate) fn run_login(
    provider: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    #[allow(clippy::single_match_else)]
    let chosen = match provider.map(str::to_ascii_lowercase).as_deref() {
        Some(p) => p.to_string(),
        None => {
            println!("⚒ Anvil Login — Select a provider:\n");
            println!("  1) Anthropic  — Claude models (OAuth login via browser)");
            println!("  2) OpenAI     — GPT/o-series models (API key)");
            println!("  3) Ollama     — Local models (configure endpoint)");
            println!("  4) API Key    — Enter an Anthropic API key directly\n");
            print!("Choice [1-4]: ");
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            match choice.trim() {
                "1" | "anthropic" => "anthropic".to_string(),
                "2" | "openai" => "openai".to_string(),
                "3" | "ollama" => "ollama".to_string(),
                "4" | "apikey" | "api-key" | "key" => "apikey".to_string(),
                other => {
                    return Err(format!("Invalid choice: {other}").into());
                }
            }
        }
    };

    match chosen.as_str() {
        "anthropic" => run_anthropic_login(),
        "openai" => {
            run_openai_apikey_setup("OpenAI", "OPENAI_API_KEY", "openai_api_key", "sk-")
        }
        "ollama" => run_ollama_setup(),
        "apikey" => run_openai_apikey_setup(
            "Anthropic",
            "ANTHROPIC_API_KEY",
            "anthropic_api_key",
            "sk-ant-",
        ),
        other => Err(format!(
            "unknown provider '{other}'. Valid options: anthropic, openai, ollama, apikey"
        )
        .into()),
    }
}

/// Set up an OpenAI-compatible API key (also used for Anthropic direct key).
pub(crate) fn run_openai_apikey_setup(
    provider_name: &str,
    env_var: &str,
    cred_key: &str,
    key_prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n⚒ {provider_name} API Key Setup\n");

    if let Ok(existing) = std::env::var(env_var)
        && !existing.is_empty() {
            let masked = if existing.len() > 12 {
                format!("{}...{}", &existing[..8], &existing[existing.len() - 4..])
            } else {
                "****".to_string()
            };
            println!("{env_var} is already set: {masked}");
            print!("Replace it? [y/N]: ");
            io::stdout().flush()?;
            let mut confirm = String::new();
            io::stdin().read_line(&mut confirm)?;
            if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
                println!("Keeping existing key.");
                return Ok(());
            }
        }

    println!("Get your API key from:");
    if provider_name == "OpenAI" {
        println!("  https://platform.openai.com/api-keys\n");
    } else {
        println!("  https://console.anthropic.com/settings/keys\n");
    }

    print!("Paste your {provider_name} API key: ");
    io::stdout().flush()?;
    let mut key = String::new();
    io::stdin().read_line(&mut key)?;
    let key = key.trim();
    if key.is_empty() {
        return Err("No key provided.".into());
    }
    if !key_prefix.is_empty() && !key.starts_with(key_prefix) {
        println!(
            "⚠ Warning: key doesn't start with '{key_prefix}' — are you sure this is a {provider_name} key?"
        );
    }

    // Prefer the encrypted vault when it is unlocked for this session.
    let vault_stored = runtime::vault_is_session_unlocked()
        && runtime::vault_session_upsert(cred_key, key).is_ok();

    if !vault_stored {
        // Fallback: plaintext credentials.json (backward compat).
        let creds_path = runtime::credentials_path()?;
        let mut root = if creds_path.exists() {
            let data = fs::read_to_string(&creds_path)?;
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };
        root.insert(
            cred_key.to_string(),
            serde_json::Value::String(key.to_string()),
        );
        if let Some(parent) = creds_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&creds_path, serde_json::to_string_pretty(&root)?)?;
    }

    println!("\n✓ {provider_name} API key saved.");
    println!("\nAlternatively, set in your shell: export {env_var}=<key>");
    println!(
        "Use with: anvil --model {}",
        if provider_name == "OpenAI" {
            "gpt-5.4-mini"
        } else {
            "claude-sonnet-4-6"
        }
    );
    Ok(())
}

/// Query the Anthropic `/v1/models` API for the live model list.
/// Returns `Vec<(model_id, display_name)>`. Returns empty on failure.
pub(crate) fn query_anthropic_models() -> Vec<(String, String)> {
    let token = load_oauth_credentials()
        .ok()
        .flatten()
        .map(|t| format!("Authorization: Bearer {}", t.access_token));
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .map(|k| format!("x-api-key: {k}"));

    let auth_header = token.or(api_key);
    let Some(auth) = auth_header else {
        return Vec::new();
    };

    let mut args = vec![
        "-s".to_string(),
        "--connect-timeout".to_string(),
        "5".to_string(),
        "-H".to_string(),
        auth,
        "-H".to_string(),
        "anthropic-version: 2023-06-01".to_string(),
    ];
    if args[4].starts_with("Authorization") {
        args.push("-H".to_string());
        args.push("anthropic-beta: oauth-2025-04-20".to_string());
    }
    args.push("https://api.anthropic.com/v1/models".to_string());

    let output = std::process::Command::new("curl").args(&args).output();
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }

    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let Some(data) = val.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };

    data.iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            let name = m
                .get("display_name")
                .and_then(|n| n.as_str())
                .unwrap_or(id);
            Some((id.to_string(), name.to_string()))
        })
        .collect()
}

/// Interactive Ollama endpoint configuration.
pub(crate) fn run_ollama_setup() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n⚒ Ollama Configuration\n");

    let default_host = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    print!("Ollama endpoint [{default_host}]: ");
    io::stdout().flush()?;
    let mut host_input = String::new();
    io::stdin().read_line(&mut host_input)?;
    let host = host_input.trim();
    let host = if host.is_empty() {
        default_host.clone()
    } else {
        host.to_string()
    };

    print!("API key (press Enter for none): ");
    io::stdout().flush()?;
    let mut key_input = String::new();
    io::stdin().read_line(&mut key_input)?;
    let api_key = key_input.trim().to_string();

    print!("Testing connection to {host}... ");
    io::stdout().flush()?;

    let mut curl_args = vec![
        "-s".to_string(),
        "--connect-timeout".to_string(),
        "5".to_string(),
    ];
    let auth_header_path_opt: Option<PathBuf> = if api_key.is_empty() {
        None
    } else {
        match write_curl_auth_header(&api_key) {
            Ok(p) => {
                curl_args.push("-H".to_string());
                curl_args.push(format!("@{}", p.display()));
                Some(p)
            }
            Err(_) => None,
        }
    };
    curl_args.push(format!("{host}/api/tags"));

    let curl_result = std::process::Command::new("curl").args(&curl_args).output();
    if let Some(ref p) = auth_header_path_opt {
        let _ = fs::remove_file(p);
    }
    match curl_result {
        Ok(out) if out.status.success() => {
            println!("✓ Connected\n");

            let mut model_names: Vec<String> = Vec::new();
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
                && let Some(models) = val.get("models").and_then(|m| m.as_array()) {
                    println!("Available models:");
                    for (i, m) in models.iter().enumerate() {
                        let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        let size = m
                            .get("size")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0);
                        println!("  {}) {:<30} {:.1}GB", i + 1, name, size / 1e9);
                        model_names.push(name.to_string());
                    }
                }

            if !model_names.is_empty() {
                println!();
                print!(
                    "Select a model [1-{}] or press Enter to skip: ",
                    model_names.len()
                );
                io::stdout().flush()?;
                let mut choice = String::new();
                io::stdin().read_line(&mut choice)?;
                let choice = choice.trim();

                if let Ok(n) = choice.parse::<usize>()
                    && n >= 1 && n <= model_names.len() {
                        let selected = &model_names[n - 1];
                        println!("\n✓ Selected: {selected}");
                        println!("\nStart Anvil with: anvil model {selected}");
                    }
            }
        }
        _ => {
            println!("✗ Could not connect");
            println!("Make sure Ollama is running: ollama serve");
        }
    }

    if host != default_host || !api_key.is_empty() {
        // Prefer vault when unlocked for this session.
        let vault_ok = if runtime::vault_is_session_unlocked() {
            let h = runtime::vault_session_upsert("ollama_host", &host).is_ok();
            let k = if api_key.is_empty() {
                true
            } else {
                runtime::vault_session_upsert("ollama_api_key", &api_key).is_ok()
            };
            h && k
        } else {
            false
        };

        if vault_ok {
            println!("\n✓ Ollama configuration saved to encrypted vault.");
        } else if let Ok(creds_path) = runtime::credentials_path() {
            let mut root = if creds_path.exists() {
                let data = fs::read_to_string(&creds_path).unwrap_or_default();
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
                    .unwrap_or_default()
            } else {
                serde_json::Map::new()
            };
            root.insert(
                "ollama_host".to_string(),
                serde_json::Value::String(host.clone()),
            );
            if !api_key.is_empty() {
                root.insert(
                    "ollama_api_key".to_string(),
                    serde_json::Value::String(api_key),
                );
            }
            if let Some(parent) = creds_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(
                &creds_path,
                serde_json::to_string_pretty(&root).unwrap_or_default(),
            );
            println!("\n✓ Configuration saved to {}", creds_path.display());
        }

        if host != default_host {
            println!("To persist the endpoint, add to your shell profile:");
            println!("  export OLLAMA_HOST={host}");
        }
    }

    Ok(())
}

/// Run the Anthropic OAuth browser login flow.
///
/// Tries to bind a localhost callback listener and, in parallel, prompts the
/// user to paste the auth code. The first path to deliver a valid `(code,
/// state)` pair wins. If the listener can't bind (port in use, no permission,
/// WSL2/SSH/container without published ports), the flow falls back to
/// paste-only mode and the prompt is the only path.
pub(crate) fn run_anthropic_login() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let config = ConfigLoader::default_for(&cwd).load()?;
    let default_oauth = default_oauth_config();
    let oauth = config.oauth().unwrap_or(&default_oauth);
    let callback_port = oauth.callback_port.unwrap_or(DEFAULT_OAUTH_CALLBACK_PORT);

    // Try binding the loopback listener first. If it fails we go paste-only,
    // which still works for users whose browser callback can't reach the host
    // running Anvil (SSH, WSL2 without port-forward, container without
    // published ports).
    let listener = TcpListener::bind(("127.0.0.1", callback_port)).ok();
    let redirect_uri = if listener.is_some() {
        loopback_redirect_uri(callback_port)
    } else {
        // Fall back to the provider's manual-redirect endpoint when we have
        // one configured; otherwise reuse the loopback URI for state-shape
        // compatibility (the user will paste the code regardless).
        oauth
            .manual_redirect_url
            .clone()
            .unwrap_or_else(|| loopback_redirect_uri(callback_port))
    };
    let pkce = generate_pkce_pair()?;
    let state = generate_state()?;
    let authorize_url =
        OAuthAuthorizationRequest::from_config(oauth, redirect_uri.clone(), state.clone(), &pkce)
            .build_url();

    println!("Starting Anvil OAuth login (Anthropic)...");
    if listener.is_some() {
        println!("Listening for callback on {redirect_uri}");
    } else {
        println!(
            "Could not bind localhost:{callback_port} for the OAuth callback."
        );
        println!("Falling back to paste-the-code mode (this is normal under SSH, WSL2, or containers).");
    }
    if let Err(error) = open_browser(&authorize_url) {
        eprintln!("warning: failed to open browser automatically: {error}");
    }
    println!("\nIf the browser doesn't open or the redirect can't reach this machine,");
    println!("open this URL manually:\n{authorize_url}");
    println!(
        "\nThen paste the code or full callback URL from the browser's address bar here."
    );
    println!("(Press Ctrl+C to abort.)\n");

    let (code, returned_state) = await_oauth_response(listener, &state)?;
    if returned_state != state {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oauth state mismatch").into());
    }

    let client = AnvilApiClient::from_auth(api::AuthSource::None).with_base_url(api::read_base_url());
    let exchange_request =
        OAuthTokenExchangeRequest::from_config(oauth, code, state, pkce.verifier, redirect_uri);
    let runtime = tokio::runtime::Runtime::new()?;
    let token_set = runtime.block_on(client.exchange_oauth_code(oauth, &exchange_request))?;
    save_oauth_credentials(&runtime::OAuthTokenSet {
        access_token: token_set.access_token,
        refresh_token: token_set.refresh_token,
        expires_at: token_set.expires_at,
        scopes: token_set.scopes,
    })?;
    println!("Anvil OAuth login complete.");
    Ok(())
}

/// One side of the race in `await_oauth_response`: either a successful
/// `(code, state)` pair or an error message.
enum OAuthRaceOutcome {
    Ok(String, String),
    Err(String),
}

/// Race the loopback listener (if any) against a stdin paste prompt and
/// return the first valid `(code, state)` pair. Both paths validate that the
/// returned state matches `expected_state` so a mismatched paste fails fast.
fn await_oauth_response(
    listener: Option<TcpListener>,
    expected_state: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel::<OAuthRaceOutcome>();

    // Listener thread (only if we managed to bind).
    if let Some(listener) = listener {
        let tx_listener = tx.clone();
        let expected = expected_state.to_string();
        thread::spawn(move || {
            let outcome = match accept_oauth_callback(&listener) {
                Ok(callback) => listener_callback_to_outcome(callback, &expected),
                Err(e) => OAuthRaceOutcome::Err(format!("listener error: {e}")),
            };
            // It's fine if the receiver is gone — the paste path won.
            let _ = tx_listener.send(outcome);
        });
    }

    // Paste prompt thread. Reads lines from stdin until one parses; an empty
    // line just re-prompts so a typo doesn't kill the listener path.
    let tx_paste = tx.clone();
    let expected = expected_state.to_string();
    thread::spawn(move || {
        let stdin = io::stdin();
        loop {
            let mut prompt = io::stdout();
            let _ = write!(prompt, "Paste code or callback URL: ");
            let _ = prompt.flush();
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) => {
                    // EOF (Ctrl+D): give up on the paste path silently so
                    // the listener can still win.
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = tx_paste.send(OAuthRaceOutcome::Err(format!(
                        "stdin read error: {e}"
                    )));
                    return;
                }
            }
            if line.trim().is_empty() {
                continue;
            }
            match parse_pasted_oauth_code(&line) {
                Ok((code, Some(state))) => {
                    if state == expected {
                        let _ = tx_paste.send(OAuthRaceOutcome::Ok(code, state));
                    } else {
                        let _ = tx_paste.send(OAuthRaceOutcome::Err(
                            "pasted state did not match the value Anvil generated. Aborting."
                                .to_string(),
                        ));
                    }
                    return;
                }
                Ok((code, None)) => {
                    // Bare code with no state: trust it but keep the
                    // expected state so the token exchange round-trips.
                    let _ = tx_paste.send(OAuthRaceOutcome::Ok(code, expected.clone()));
                    return;
                }
                Err(e) => {
                    eprintln!("Could not parse pasted value: {e}. Try again or press Ctrl+C.");
                }
            }
        }
    });

    // Drop our own sender so the channel disconnects when both worker
    // threads have returned.
    drop(tx);

    // Block until somebody reports an outcome. recv_timeout lets us notice
    // disconnection promptly even if both paths bailed out.
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(OAuthRaceOutcome::Ok(code, state)) => return Ok((code, state)),
            Ok(OAuthRaceOutcome::Err(message)) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, message).into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No outcome yet; loop and wait again.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("OAuth login aborted (no callback received)").into());
            }
        }
    }
}

fn listener_callback_to_outcome(
    callback: OAuthCallbackParams,
    expected_state: &str,
) -> OAuthRaceOutcome {
    if let Some(error) = callback.error {
        let description = callback
            .error_description
            .unwrap_or_else(|| "authorization failed".to_string());
        return OAuthRaceOutcome::Err(format!("{error}: {description}"));
    }
    let Some(code) = callback.code else {
        return OAuthRaceOutcome::Err("callback did not include code".to_string());
    };
    let Some(returned_state) = callback.state else {
        return OAuthRaceOutcome::Err("callback did not include state".to_string());
    };
    if returned_state != expected_state {
        return OAuthRaceOutcome::Err("oauth state mismatch".to_string());
    }
    OAuthRaceOutcome::Ok(code, returned_state)
}

/// Clear saved OAuth credentials (logout).
pub(crate) fn run_logout() -> Result<(), Box<dyn std::error::Error>> {
    clear_oauth_credentials()?;
    println!("Anvil OAuth credentials cleared.");
    Ok(())
}

/// Open the given URL in the system default browser.
pub(crate) fn open_browser(url: &str) -> io::Result<()> {
    let commands = if cfg!(target_os = "macos") {
        vec![("open", vec![url])]
    } else if cfg!(target_os = "windows") {
        vec![("cmd", vec!["/C", "start", "", url])]
    } else {
        vec![("xdg-open", vec![url])]
    };
    for (program, args) in commands {
        match Command::new(program).args(args).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no supported browser opener command found",
    ))
}

/// Accept a single OAuth callback request on an already-bound listener.
///
/// The race-with-paste flow binds the listener up front (so it can decide
/// whether to fall back to paste-only mode) and then hands the listener here
/// to wait for the redirected request.
fn accept_oauth_callback(
    listener: &TcpListener,
) -> Result<runtime::OAuthCallbackParams, Box<dyn std::error::Error>> {
    let (mut stream, _) = listener.accept()?;
    let mut buffer = [0_u8; 4096];
    let bytes_read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request.lines().next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing callback request line")
    })?;
    let target = request_line.split_whitespace().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "missing callback request target",
        )
    })?;
    let callback = parse_oauth_callback_request_target(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let body = if callback.error.is_some() {
        "Anvil OAuth login failed. You can close this window."
    } else {
        "Anvil OAuth login succeeded. You can close this window."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    Ok(callback)
}
