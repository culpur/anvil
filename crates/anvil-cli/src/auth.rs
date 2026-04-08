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

use api::AnvilApiClient;
use runtime::{
    clear_oauth_credentials, generate_pkce_pair, generate_state, loopback_redirect_uri,
    load_oauth_credentials, parse_oauth_callback_request_target, save_oauth_credentials,
    ConfigLoader, OAuthAuthorizationRequest, OAuthConfig, OAuthTokenExchangeRequest,
};

use crate::DEFAULT_OAUTH_CALLBACK_PORT;
use crate::write_curl_auth_header;

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

    if let Ok(existing) = std::env::var(env_var) {
        if !existing.is_empty() {
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

/// Query the Anthropic /v1/models API for the live model list.
/// Returns Vec<(model_id, display_name)>. Returns empty on failure.
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
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(models) = val.get("models").and_then(|m| m.as_array()) {
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

                if let Ok(n) = choice.parse::<usize>() {
                    if n >= 1 && n <= model_names.len() {
                        let selected = &model_names[n - 1];
                        println!("\n✓ Selected: {selected}");
                        println!("\nStart Anvil with: anvil model {selected}");
                    }
                }
            }
        }
        _ => {
            println!("✗ Could not connect");
            println!("Make sure Ollama is running: ollama serve");
        }
    }

    if host != default_host || !api_key.is_empty() {
        if let Ok(creds_path) = runtime::credentials_path() {
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
pub(crate) fn run_anthropic_login() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let config = ConfigLoader::default_for(&cwd).load()?;
    let default_oauth = default_oauth_config();
    let oauth = config.oauth().unwrap_or(&default_oauth);
    let callback_port = oauth.callback_port.unwrap_or(DEFAULT_OAUTH_CALLBACK_PORT);
    let redirect_uri = loopback_redirect_uri(callback_port);
    let pkce = generate_pkce_pair()?;
    let state = generate_state()?;
    let authorize_url =
        OAuthAuthorizationRequest::from_config(oauth, redirect_uri.clone(), state.clone(), &pkce)
            .build_url();

    println!("Starting Anvil OAuth login (Anthropic)...");
    println!("Listening for callback on {redirect_uri}");
    if let Err(error) = open_browser(&authorize_url) {
        eprintln!("warning: failed to open browser automatically: {error}");
        println!("Open this URL manually:\n{authorize_url}");
    }

    let callback = wait_for_oauth_callback(callback_port)?;
    if let Some(error) = callback.error {
        let description = callback
            .error_description
            .unwrap_or_else(|| "authorization failed".to_string());
        return Err(io::Error::other(format!("{error}: {description}")).into());
    }
    let code = callback.code.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "callback did not include code")
    })?;
    let returned_state = callback.state.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "callback did not include state")
    })?;
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

/// Listen on the OAuth callback port and parse the redirect parameters.
pub(crate) fn wait_for_oauth_callback(
    port: u16,
) -> Result<runtime::OAuthCallbackParams, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
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
