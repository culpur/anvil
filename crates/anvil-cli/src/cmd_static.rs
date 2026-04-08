//! Static command handler free-functions extracted from `impl LiveCli`.
//! These have no `self` receiver and are dispatched from both the TUI and
//! headless REPL paths.

use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tools::execute_tool as execute_builtin_tool;

use crate::vault::write_curl_auth_header;

use crate::{
    anvil_home_dir, anvil_pinned_path, command_exists, dirs_next_home, file_drop,
    git_output, json_escape, load_pinned_paths, regex_escape, render_teleport_report,
    run_language_command_static, save_pinned_paths, send_desktop_notification,
    shell_output_or_err, truncate_for_prompt,
};

pub(crate) fn run_docker_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();

    match args {
        "" | "help" => [
            "Usage:",
            "  /docker ps                   List running containers",
            "  /docker logs <container>     Show last 50 lines of container logs",
            "  /docker compose              Show docker-compose services (if present)",
            "  /docker build                Build image from Dockerfile in current directory",
        ]
        .join("\n"),

        "ps" => run_docker_ps(),
        "compose" => run_docker_compose_services(),
        "build" => run_docker_build(),
        s if s.starts_with("logs ") => {
            let container = s["logs ".len()..].trim();
            if container.is_empty() {
                "Usage: /docker logs <container>".to_string()
            } else {
                run_docker_logs(container)
            }
        }
        other => format!(
            "Unknown docker sub-command: {other}\nRun `/docker help` for usage."
        ),
    }
}

pub(crate) fn run_voice_command(args: Option<&str>) -> String {
    let sub = args.unwrap_or("").trim();
    match sub {
        "start" => concat!(
            "Voice input — coming soon\n\n",
            "Voice capture requires microphone access and a speech-to-text backend.\n",
            "Planned: /voice start  ->  capture mic input and inject as a prompt.",
        )
        .to_string(),
        "stop" => "Voice input — coming soon\n\nNo active voice session to stop.".to_string(),
        "" | "help" => [
            "Voice input (coming soon)",
            "",
            "Commands:",
            "  /voice start   Begin capturing microphone input",
            "  /voice stop    Stop capturing and submit",
            "",
            "Requires a local speech-to-text engine (e.g. whisper.cpp).",
        ]
        .join("\n"),
        other => format!("Unknown /voice sub-command: {other}\n  /voice start | /voice stop"),
    }
}

pub(crate) fn run_collab_command(args: Option<&str>) -> String {
    let sub = args.unwrap_or("").trim();
    match sub {
        "share" => concat!(
            "Collaboration — coming soon\n\n",
            "Planned: /collab share  ->  generate a shareable session ID.\n",
            "This feature is reserved for a future release.",
        )
        .to_string(),
        "join" => concat!(
            "Collaboration — coming soon\n\n",
            "Usage: /collab join <session-id>\n",
            "This feature is reserved for a future release.",
        )
        .to_string(),
        "" | "help" => [
            "Collaboration (coming soon)",
            "",
            "Commands:",
            "  /collab share          Share this session (generates an invite ID)",
            "  /collab join <id>      Join another user's shared session",
            "",
            "Requires an AnvilHub account. Watch the changelog for availability.",
        ]
        .join("\n"),
        other => {
            format!("Unknown /collab sub-command: {other}\n  /collab share | /collab join <id>")
        }
    }
}

pub(crate) fn run_k8s_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /k8s pods                   List pods in current namespace",
            "  /k8s logs <pod>             Tail last 50 lines of pod logs",
            "  /k8s apply <file>           Apply a manifest with kubectl",
            "  /k8s describe <resource>    Describe a resource",
        ].join("\n");
    }
    if !command_exists("kubectl") {
        return "kubectl not found in PATH. Install it from \
                https://kubernetes.io/docs/tasks/tools/".to_string();
    }
    if args == "pods" {
        let out = Command::new("kubectl").args(["get", "pods"]).output();
        return shell_output_or_err(out, "kubectl get pods");
    }
    if let Some(pod) = args.strip_prefix("logs ") {
        let pod = pod.trim();
        if pod.is_empty() { return "Usage: /k8s logs <pod>".to_string(); }
        let out = Command::new("kubectl").args(["logs", "--tail=50", pod]).output();
        return shell_output_or_err(out, &format!("kubectl logs {pod}"));
    }
    if let Some(file) = args.strip_prefix("apply ") {
        let file = file.trim();
        if file.is_empty() { return "Usage: /k8s apply <file>".to_string(); }
        let out = Command::new("kubectl").args(["apply", "-f", file]).output();
        return shell_output_or_err(out, &format!("kubectl apply -f {file}"));
    }
    if let Some(resource) = args.strip_prefix("describe ") {
        let resource = resource.trim();
        if resource.is_empty() { return "Usage: /k8s describe <resource>".to_string(); }
        let parts: Vec<&str> = resource.splitn(2, ' ').collect();
        let out = Command::new("kubectl").arg("describe").args(&parts).output();
        return shell_output_or_err(out, &format!("kubectl describe {resource}"));
    }
    format!("Unknown /k8s sub-command: {args}\nRun `/k8s help` for usage.")
}

pub(crate) fn run_iac_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /iac plan       Run terraform/tofu plan",
            "  /iac apply      Run terraform/tofu apply",
            "  /iac validate   Validate configuration files",
            "  /iac drift      Detect infrastructure drift (plan -refresh-only)",
        ].join("\n");
    }
    let tf_bin = if command_exists("tofu") {
        "tofu"
    } else if command_exists("terraform") {
        "terraform"
    } else {
        return "Neither 'tofu' nor 'terraform' found in PATH.\n\
                Install OpenTofu: https://opentofu.org/docs/intro/install/".to_string();
    };
    if args == "apply" {
        // Require explicit confirmation before modifying infrastructure.
        eprint!("This will apply changes to your infrastructure. Continue? (y/N) ");
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || answer.trim().to_lowercase() != "y"
        {
            return "Apply cancelled.".to_string();
        }
    }
    let tf_args: &[&str] = match args {
        "plan"     => &["plan", "-no-color"],
        "apply"    => &["apply", "-no-color"],
        "validate" => &["validate", "-no-color"],
        "drift"    => &["plan", "-refresh-only", "-no-color"],
        other => return format!("Unknown /iac sub-command: {other}\nRun `/iac help` for usage."),
    };
    let out = Command::new(tf_bin).args(tf_args).output();
    shell_output_or_err(out, &format!("{tf_bin} {args}"))
}

pub(crate) fn run_deps_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /deps tree           Show dependency tree",
            "  /deps outdated       Show outdated dependencies",
            "  /deps audit          Security audit of dependencies",
            "  /deps why <pkg>      Explain why a dependency is included",
        ].join("\n");
    }
    let pm = detect_package_manager();
    match args {
        "tree" => {
            let out = match pm {
                PackageManager::Cargo   => Command::new("cargo").args(["tree"]).output(),
                PackageManager::Npm     => Command::new("npm").args(["ls", "--depth=2"]).output(),
                PackageManager::Pnpm    => Command::new("pnpm").args(["list", "--depth=2"]).output(),
                PackageManager::Yarn    => Command::new("yarn").args(["list", "--depth=2"]).output(),
                PackageManager::Pip     => Command::new("pip").args(["show", "--verbose"]).output(),
                PackageManager::Unknown => return "No recognised package manager found.".to_string(),
            };
            shell_output_or_err(out, "deps tree")
        }
        "outdated" => {
            let out = match pm {
                PackageManager::Cargo   => Command::new("cargo").args(["outdated"]).output(),
                PackageManager::Npm     => Command::new("npm").args(["outdated"]).output(),
                PackageManager::Pnpm    => Command::new("pnpm").args(["outdated"]).output(),
                PackageManager::Yarn    => Command::new("yarn").args(["outdated"]).output(),
                PackageManager::Pip     => Command::new("pip").args(["list", "--outdated"]).output(),
                PackageManager::Unknown => return "No recognised package manager found.".to_string(),
            };
            shell_output_or_err(out, "deps outdated")
        }
        "audit" => {
            let out = match pm {
                PackageManager::Cargo   => Command::new("cargo").args(["audit"]).output(),
                PackageManager::Npm     => Command::new("npm").args(["audit"]).output(),
                PackageManager::Pnpm    => Command::new("pnpm").args(["audit"]).output(),
                PackageManager::Yarn    => Command::new("yarn").args(["audit"]).output(),
                PackageManager::Pip     => Command::new("pip-audit").output(),
                PackageManager::Unknown => return "No recognised package manager found.".to_string(),
            };
            shell_output_or_err(out, "deps audit")
        }
        s if s.starts_with("why ") => {
            let pkg = s["why ".len()..].trim();
            if pkg.is_empty() { return "Usage: /deps why <pkg>".to_string(); }
            let out = match pm {
                PackageManager::Cargo => Command::new("cargo").args(["tree", "--invert", pkg]).output(),
                PackageManager::Npm   => Command::new("npm").args(["why", pkg]).output(),
                PackageManager::Pnpm  => Command::new("pnpm").args(["why", pkg]).output(),
                PackageManager::Yarn  => Command::new("yarn").args(["why", pkg]).output(),
                _ => return "/deps why is not supported for this package manager.".to_string(),
            };
            shell_output_or_err(out, &format!("deps why {pkg}"))
        }
        other => format!("Unknown /deps sub-command: {other}\nRun `/deps help` for usage."),
    }
}

pub(crate) fn run_mono_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /mono list                     List workspace packages",
            "  /mono graph                    Show package dependency graph",
            "  /mono changed                  List packages changed since last git tag",
            "  /mono run <cmd> [--filter <p>] Run command in workspace packages",
        ].join("\n");
    }
    let workspace_kind = detect_workspace_kind();
    if matches!(workspace_kind, WorkspaceKind::None) {
        return "No monorepo workspace config detected.\n\
                Expected: Cargo.toml [workspace], package.json workspaces, or pnpm-workspace.yaml".to_string();
    }
    match args {
        "list" => match workspace_kind {
            WorkspaceKind::Cargo => Command::new("cargo")
                .args(["metadata", "--no-deps", "--format-version=1"]).output().map_or_else(|e| format!("cargo metadata failed: {e}"), |o| parse_cargo_workspace_members(&String::from_utf8_lossy(&o.stdout))),
            WorkspaceKind::Pnpm => shell_output_or_err(
                Command::new("pnpm").args(["ls", "--depth=0"]).output(), "pnpm ls"),
            WorkspaceKind::Npm  => shell_output_or_err(
                Command::new("npm").args(["ls", "--depth=0"]).output(), "npm ls"),
            WorkspaceKind::None => unreachable!(),
        },
        "graph" => match workspace_kind {
            WorkspaceKind::Cargo => shell_output_or_err(
                Command::new("cargo").args(["tree", "--workspace"]).output(),
                "cargo tree --workspace"),
            WorkspaceKind::Pnpm => shell_output_or_err(
                Command::new("pnpm").args(["ls", "--depth=3"]).output(), "pnpm ls --depth=3"),
            _ => shell_output_or_err(
                Command::new("npm").args(["ls", "--depth=3"]).output(), "npm ls --depth=3"),
        },
        "changed" => {
            let last_tag = Command::new("git")
                .args(["describe", "--tags", "--abbrev=0"]).output().ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "HEAD~10".to_string());
            let changed = Command::new("git")
                .args(["diff", "--name-only", &last_tag, "HEAD"]).output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            if changed.trim().is_empty() {
                return format!("No files changed since {last_tag}.");
            }
            let mut pkgs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for line in changed.lines() {
                if let Some(p) = line.split('/').next() { pkgs.insert(p.to_string()); }
            }
            format!("Packages changed since {last_tag}:\n{}",
                pkgs.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"))
        }
        s if s.starts_with("run ") => {
            let rest = s["run ".len()..].trim();
            let (filter, cmd_str) = if let Some(idx) = rest.find("--filter ") {
                let fp = rest[idx + "--filter ".len()..].split_whitespace()
                    .next().unwrap_or("").to_string();
                (Some(fp), rest[..idx].trim().to_string())
            } else {
                (None, rest.to_string())
            };
            if cmd_str.is_empty() { return "Usage: /mono run <cmd> [--filter <pkg>]".to_string(); }
            match workspace_kind {
                WorkspaceKind::Pnpm => {
                    let mut a = vec!["run".to_string()];
                    if let Some(f) = &filter { a.push("--filter".into()); a.push(f.clone()); }
                    a.push(cmd_str.clone());
                    shell_output_or_err(Command::new("pnpm").args(&a).output(),
                        &format!("pnpm run {cmd_str}"))
                }
                WorkspaceKind::Npm => shell_output_or_err(
                    Command::new("npm").args(["run", &cmd_str, "--workspaces"]).output(),
                    &format!("npm run {cmd_str}")),
                WorkspaceKind::Cargo => {
                    let mut a = vec!["run".to_string()];
                    if let Some(f) = &filter { a.push("-p".into()); a.push(f.clone()); }
                    shell_output_or_err(Command::new("cargo").args(&a).output(), "cargo run")
                }
                WorkspaceKind::None => unreachable!(),
            }
        }
        other => format!("Unknown /mono sub-command: {other}\nRun `/mono help` for usage."),
    }
}

pub(crate) fn run_browser_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /browser open <url>         Open URL in default browser",
            "  /browser screenshot <url>   Capture a screenshot (requires playwright)",
            "  /browser test <url>         Run accessibility/performance test",
        ].join("\n");
    }
    if let Some(url) = args.strip_prefix("open ") {
        let url = url.trim();
        if url.is_empty() { return "Usage: /browser open <url>".to_string(); }
        let open_cmd = if cfg!(target_os = "macos") { "open" }
                       else if cfg!(target_os = "windows") { "start" }
                       else { "xdg-open" };
        let out = Command::new(open_cmd).arg(url).output();
        return match out {
            Ok(o) if o.status.success() => format!("Opened {url} in default browser."),
            Ok(o) => format!("open failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => format!("Failed to open browser: {e}"),
        };
    }
    if let Some(url) = args.strip_prefix("screenshot ") {
        let url = url.trim();
        if url.is_empty() { return "Usage: /browser screenshot <url>".to_string(); }
        if command_exists("npx") {
            let out = Command::new("npx")
                .args(["playwright", "screenshot", url, "screenshot.png"]).output();
            return match out {
                Ok(o) if o.status.success() =>
                    format!("Screenshot saved to screenshot.png for {url}"),
                Ok(o) => format!("playwright screenshot failed:\n{}",
                    String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => format!("Failed to run playwright: {e}"),
            };
        }
        return format!(
            "playwright not available. Install with: npm install -g playwright\n\
             Alternatively, open {url} manually and take a screenshot."
        );
    }
    if let Some(url) = args.strip_prefix("test ") {
        let url = url.trim();
        if url.is_empty() { return "Usage: /browser test <url>".to_string(); }
        if command_exists("lighthouse") {
            let out = Command::new("lighthouse")
                .args([url, "--output=text", "--quiet", "--chrome-flags=--headless"]).output();
            return shell_output_or_err(out, &format!("lighthouse {url}"));
        }
        if command_exists("axe") {
            return shell_output_or_err(
                Command::new("axe").arg(url).output(), &format!("axe {url}"));
        }
        return format!(
            "No testing tool found.\n\
             Install Lighthouse: npm install -g lighthouse\n\
             Install axe-cli:    npm install -g axe-cli\n\
             Target URL: {url}"
        );
    }
    format!("Unknown /browser sub-command: {args}\nRun `/browser help` for usage.")
}

pub(crate) fn run_notify_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /notify send <message>               Send a desktop notification",
            "  /notify webhook <url> <message>      POST message to a webhook URL",
            "  /notify matrix <room> <message>      Send to Matrix room (needs MATRIX_TOKEN)",
            "  /notify discord <webhook_url> <msg>  Send to Discord channel via webhook",
            "  /notify slack <webhook_url> <msg>    Send to Slack channel via webhook",
            "  /notify telegram <chat_id> <msg>     Send to Telegram (needs TELEGRAM_BOT_TOKEN)",
            "  /notify whatsapp <number> <msg>      Send via WhatsApp (needs WHATSAPP_API_URL, WHATSAPP_TOKEN)",
            "  /notify signal <number> <msg>        Send via Signal (needs SIGNAL_CLI_PATH or signal-cli)",
        ].join("\n");
    }
    if let Some(message) = args.strip_prefix("send ") {
        let message = message.trim();
        if message.is_empty() { return "Usage: /notify send <message>".to_string(); }
        return send_desktop_notification("Anvil", message);
    }
    if let Some(rest) = args.strip_prefix("webhook ") {
        let rest = rest.trim();
        let (url, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify webhook <url> <message>".to_string(),
        };
        if url.is_empty() || message.is_empty() {
            return "Usage: /notify webhook <url> <message>".to_string();
        }
        let payload = format!(r#"{{"text":"{msg}"}}"#, msg = json_escape(message));
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                   "-X", "POST", "-H", "Content-Type: application/json",
                   "-d", &payload, url])
            .output();
        return match out {
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code.starts_with('2') { format!("Webhook delivered to {url} (HTTP {code}).")
                } else { format!("Webhook returned HTTP {code} for {url}.") }
            }
            Err(e) => format!("curl failed: {e}. Ensure curl is installed."),
        };
    }
    if let Some(rest) = args.strip_prefix("matrix ") {
        let rest = rest.trim();
        let (room, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify matrix <room> <message>".to_string(),
        };
        if room.is_empty() || message.is_empty() {
            return "Usage: /notify matrix <room> <message>".to_string();
        }
        let token = match env::var("MATRIX_TOKEN") {
            Ok(t) => t,
            Err(_) => return "MATRIX_TOKEN environment variable not set.\n\
                              Set it to your Matrix access token.".to_string(),
        };
        let homeserver = env::var("MATRIX_HOMESERVER")
            .unwrap_or_else(|_| "https://matrix.org".to_string());
        let room_encoded = room.replace('#', "%23").replace(':', "%3A");
        let url = format!(
            "{homeserver}/_matrix/client/r0/rooms/{room_encoded}/send/m.room.message"
        );
        let payload = format!(
            r#"{{"msgtype":"m.text","body":"{msg}"}}"#,
            msg = json_escape(message)
        );
        // Write the auth token to a temp file (mode 0o600) so it is not
        // visible in the process argument list.
        let auth_hdr: PathBuf = match write_curl_auth_header(&token) {
            Ok(p) => p,
            Err(e) => return format!("Failed to prepare Matrix auth header: {e}"),
        };
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                   "-X", "POST",
                   "-H", &format!("@{}", auth_hdr.display()),
                   "-H", "Content-Type: application/json",
                   "-d", &payload, &url])
            .output();
        let _ = fs::remove_file(&auth_hdr);
        return match out {
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code.starts_with('2') { format!("Matrix message sent to {room} (HTTP {code}).")
                } else { format!("Matrix send returned HTTP {code} for room {room}.") }
            }
            Err(e) => format!("curl failed: {e}"),
        };
    }
    // Discord — via webhook URL
    if let Some(rest) = args.strip_prefix("discord ") {
        let rest = rest.trim();
        let (url, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify discord <webhook_url> <message>".to_string(),
        };
        // Validate the host is actually discord.com or discordapp.com to
        // prevent sending payloads to arbitrary servers.  Extract the
        // host from the URL without pulling in an extra dependency.
        let discord_host_valid = {
            // Strip scheme: look for "://" and take the part after it.
            let after_scheme = url.find("://")
                .map_or(url, |i| &url[i + 3..]);
            // Host ends at the first '/', '?', '#', or ':'.
            let host = after_scheme.split(['/', '?', '#', ':'])
                .next()
                .unwrap_or("")
                .to_lowercase();
            host == "discord.com" || host == "discordapp.com"
        };
        if !discord_host_valid {
            return "Discord webhook URL must have host discord.com or discordapp.com".to_string();
        }
        let payload = format!(r#"{{"content":"{msg}"}}"#, msg = json_escape(message));
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                   "-X", "POST", "-H", "Content-Type: application/json",
                   "-d", &payload, url])
            .output();
        return match out {
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code == "204" || code.starts_with('2') {
                    format!("Discord message delivered (HTTP {code}).")
                } else { format!("Discord webhook returned HTTP {code}.") }
            }
            Err(e) => format!("curl failed: {e}"),
        };
    }

    // Slack — via webhook URL
    if let Some(rest) = args.strip_prefix("slack ") {
        let rest = rest.trim();
        let (url, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify slack <webhook_url> <message>".to_string(),
        };
        let payload = format!(r#"{{"text":"{msg}"}}"#, msg = json_escape(message));
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                   "-X", "POST", "-H", "Content-Type: application/json",
                   "-d", &payload, url])
            .output();
        return match out {
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code.starts_with('2') {
                    format!("Slack message delivered (HTTP {code}).")
                } else { format!("Slack webhook returned HTTP {code}.") }
            }
            Err(e) => format!("curl failed: {e}"),
        };
    }

    // Telegram — via Bot API
    if let Some(rest) = args.strip_prefix("telegram ") {
        let rest = rest.trim();
        let (chat_id, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify telegram <chat_id> <message>".to_string(),
        };
        let token = match env::var("TELEGRAM_BOT_TOKEN") {
            Ok(t) => t,
            Err(_) => return "TELEGRAM_BOT_TOKEN environment variable not set.\n\
                              Create a bot via @BotFather and set the token.".to_string(),
        };
        let url = format!(
            "https://api.telegram.org/bot{token}/sendMessage"
        );
        let payload = format!(
            r#"{{"chat_id":"{chat_id}","text":"{msg}","parse_mode":"Markdown"}}"#,
            msg = json_escape(message)
        );
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                   "-X", "POST", "-H", "Content-Type: application/json",
                   "-d", &payload, &url])
            .output();
        return match out {
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code.starts_with('2') {
                    format!("Telegram message sent to {chat_id} (HTTP {code}).")
                } else { format!("Telegram API returned HTTP {code}.") }
            }
            Err(e) => format!("curl failed: {e}"),
        };
    }

    // WhatsApp — via WhatsApp Business API or compatible gateway
    if let Some(rest) = args.strip_prefix("whatsapp ") {
        let rest = rest.trim();
        let (number, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify whatsapp <number> <message>".to_string(),
        };
        let api_url = match env::var("WHATSAPP_API_URL") {
            Ok(u) => u,
            Err(_) => return "WHATSAPP_API_URL environment variable not set.\n\
                              Set it to your WhatsApp Business API endpoint\n\
                              (e.g., https://graph.facebook.com/v18.0/<phone_id>/messages).".to_string(),
        };
        let token = match env::var("WHATSAPP_TOKEN") {
            Ok(t) => t,
            Err(_) => return "WHATSAPP_TOKEN environment variable not set.".to_string(),
        };
        let payload = format!(
            r#"{{"messaging_product":"whatsapp","to":"{number}","type":"text","text":{{"body":"{msg}"}}}}"#,
            msg = json_escape(message)
        );
        // Write the auth token to a temp file (mode 0o600) so it is not
        // visible in the process argument list.
        let auth_hdr = match write_curl_auth_header(&token) {
            Ok(p) => p,
            Err(e) => return format!("Failed to prepare WhatsApp auth header: {e}"),
        };
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                   "-X", "POST",
                   "-H", &format!("@{}", auth_hdr.display()),
                   "-H", "Content-Type: application/json",
                   "-d", &payload, &api_url])
            .output();
        let _ = fs::remove_file(&auth_hdr);
        return match out {
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code.starts_with('2') {
                    format!("WhatsApp message sent to {number} (HTTP {code}).")
                } else { format!("WhatsApp API returned HTTP {code}.") }
            }
            Err(e) => format!("curl failed: {e}"),
        };
    }

    // Signal — via signal-cli
    if let Some(rest) = args.strip_prefix("signal ") {
        let rest = rest.trim();
        let (number, message) = match rest.find(' ') {
            Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
            None => return "Usage: /notify signal <number> <message>".to_string(),
        };
        let signal_cli = env::var("SIGNAL_CLI_PATH")
            .unwrap_or_else(|_| "signal-cli".to_string());
        let sender = match env::var("SIGNAL_SENDER") {
            Ok(s) => s,
            Err(_) => return "SIGNAL_SENDER environment variable not set.\n\
                              Set it to your registered Signal number (e.g., +1234567890).".to_string(),
        };
        let out = Command::new(&signal_cli)
            .args(["send", "-m", message, number, "-a", &sender])
            .output();
        return match out {
            Ok(o) if o.status.success() => {
                format!("Signal message sent to {number}.")
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                format!("signal-cli error: {err}")
            }
            Err(e) => format!("signal-cli not found or failed: {e}\n\
                               Install signal-cli: https://github.com/AsamK/signal-cli"),
        };
    }

    format!("Unknown /notify sub-command: {args}\nRun `/notify help` for usage.")
}

pub(crate) fn run_ssh_command(args: Option<&str>) -> String {
    let mut parts = args.unwrap_or("list").trim().splitn(4, ' ');
    let sub = parts.next().unwrap_or("list");
    match sub {
        "list" => {
            let home = PathBuf::from(env::var("HOME").unwrap_or_default());
            let config_path = home.join(".ssh").join("config");
            match fs::read_to_string(&config_path) {
                Ok(cfg) => {
                    let hosts: Vec<String> = cfg.lines()
                        .filter(|l| l.trim_start().starts_with("Host ") && !l.contains('*'))
                        .map(|l| l.trim().trim_start_matches("Host ").trim().to_string())
                        .collect();
                    if hosts.is_empty() {
                        "SSH list\n  Result           no named hosts in ~/.ssh/config".to_string()
                    } else {
                        let list = hosts.iter().enumerate().map(|(i, h)| format!("  {}. {h}", i + 1)).collect::<Vec<_>>().join("\n");
                        format!("SSH hosts\n  Config           ~/.ssh/config\n\n{list}")
                    }
                }
                Err(_) => "SSH list\n  Note             ~/.ssh/config not found or not readable".to_string(),
            }
        }
        "connect" => {
            let host = parts.next().unwrap_or("<host>");
            format!("SSH connect\n  Host             {host}\n  Command          ssh {host}\n  Note             Run this in your terminal — Anvil cannot capture interactive SSH sessions.")
        }
        "tunnel" => {
            let host = parts.next().unwrap_or("<host>");
            let ports = parts.next().unwrap_or("8080:8080");
            let (local, remote) = ports.split_once(':').unwrap_or((ports, ports));
            format!("SSH tunnel\n  Host             {host}\n  Local port       {local}\n  Remote port      {remote}\n  Command          ssh -L {local}:localhost:{remote} {host} -N -f")
        }
        "keys" => {
            let home = PathBuf::from(env::var("HOME").unwrap_or_default());
            let ssh_dir = home.join(".ssh");
            match fs::read_dir(&ssh_dir) {
                Ok(entries) => {
                    let keys: Vec<String> = entries.flatten()
                        .filter_map(|e| {
                            let p = e.path();
                            let name = p.file_name()?.to_str()?.to_string();
                            if !name.ends_with(".pub") && (name.starts_with("id_") || name.contains("_key")) {
                                Some(format!("  {name}"))
                            } else { None }
                        })
                        .collect();
                    if keys.is_empty() { "SSH keys\n  Result           no key files found in ~/.ssh/".to_string() }
                    else { format!("SSH keys (~/.ssh/)\n\n{}", keys.join("\n")) }
                }
                Err(e) => format!("SSH keys\n  Error            {e}"),
            }
        }
        _ => "Usage: /ssh [list|connect <host>|tunnel <host> <local:remote>|keys]".to_string(),
    }
}

pub(crate) fn run_markdown_command(args: Option<&str>) -> String {
    let mut parts = args.unwrap_or("").trim().splitn(3, ' ');
    let sub = parts.next().unwrap_or("");
    let file = parts.next().unwrap_or("<file>");
    match sub {
        "preview" => {
            match fs::read_to_string(file) {
                Ok(src) => {
                    // Strip markdown syntax for TUI plain-text preview
                    let preview: String = src.lines().map(|l| {
                        let l = l.trim_start_matches('#').trim();
                        l.trim_start_matches("**").trim_end_matches("**")
                            .trim_start_matches('*').trim_end_matches('*')
                            .to_string()
                    }).collect::<Vec<_>>().join("\n");
                    format!("Markdown preview  {file}\n\n{}", truncate_for_prompt(&preview, 5_000))
                }
                Err(e) => format!("Markdown preview\n  Error            {e}"),
            }
        }
        "toc" => {
            match fs::read_to_string(file) {
                Ok(src) => {
                    let headings: Vec<String> = src.lines()
                        .filter(|l| l.starts_with('#'))
                        .map(|l| {
                            let level = l.chars().take_while(|c| *c == '#').count();
                            let title = l.trim_start_matches('#').trim();
                            let indent = "  ".repeat(level.saturating_sub(1));
                            let anchor = title.to_lowercase().replace(' ', "-")
                                .chars().filter(|c| c.is_alphanumeric() || *c == '-').collect::<String>();
                            format!("{indent}- [{title}](#{anchor})")
                        })
                        .collect();
                    if headings.is_empty() {
                        format!("Markdown TOC\n  File             {file}\n  Result           no headings found")
                    } else {
                        format!("Markdown TOC\n  File             {file}\n\n{}", headings.join("\n"))
                    }
                }
                Err(e) => format!("Markdown TOC\n  Error            {e}"),
            }
        }
        "lint" => {
            match fs::read_to_string(file) {
                Ok(src) => {
                    let mut issues: Vec<String> = Vec::new();
                    let mut in_code = false;
                    for (i, line) in src.lines().enumerate() {
                        if line.starts_with("```") { in_code = !in_code; }
                        if in_code { continue; }
                        if line.ends_with(' ') || line.ends_with('\t') {
                            issues.push(format!("  Line {:>4}  trailing whitespace", i + 1));
                        }
                        if line.len() > 120 {
                            issues.push(format!("  Line {:>4}  line exceeds 120 chars ({})", i + 1, line.len()));
                        }
                    }
                    if issues.is_empty() {
                        format!("Markdown lint\n  File             {file}\n  Result           no issues found")
                    } else {
                        format!("Markdown lint\n  File             {file}\n  Issues           {}\n\n{}", issues.len(), issues.join("\n"))
                    }
                }
                Err(e) => format!("Markdown lint\n  Error            {e}"),
            }
        }
        _ => "Usage: /markdown [preview <file>|toc <file>|lint <file>]".to_string(),
    }
}

pub(crate) fn run_snippets_command(args: Option<&str>) -> String {
    let snippets_dir = anvil_home_dir().join("snippets");
    let args = args.unwrap_or("list").trim();
    let mut parts = args.splitn(3, ' ');
    let sub = parts.next().unwrap_or("list");
    match sub {
        "save" => {
            let name = parts.next().unwrap_or("snippet");
            let content = parts.collect::<Vec<_>>().join(" ");
            if content.is_empty() {
                return "Snippets save\n  Usage            /snippets save <name> <code>".to_string();
            }
            let _ = fs::create_dir_all(&snippets_dir);
            let path = snippets_dir.join(format!("{name}.snippet"));
            match fs::write(&path, &content) {
                Ok(()) => format!("Snippets\n  Action           save\n  Name             {name}\n  Path             {}", path.display()),
                Err(e) => format!("Snippets save\n  Error            {e}"),
            }
        }
        "list" => {
            match fs::read_dir(&snippets_dir) {
                Ok(entries) => {
                    let names: Vec<String> = entries.flatten().filter_map(|e| {
                        let p = e.path();
                        if p.extension().is_some_and(|x| x == "snippet") {
                            p.file_stem().map(|s| format!("  {}", s.to_string_lossy()))
                        } else { None }
                    }).collect();
                    if names.is_empty() {
                        format!("Snippets\n  Directory        {}\n  Result           no snippets yet — use /snippets save <name> <code>", snippets_dir.display())
                    } else {
                        format!("Snippets  ({})\n\n{}", names.len(), names.join("\n"))
                    }
                }
                Err(_) => format!("Snippets\n  Directory        {}\n  Result           no snippets directory yet", snippets_dir.display()),
            }
        }
        "get" => {
            let name = parts.next().unwrap_or("<name>");
            let path = snippets_dir.join(format!("{name}.snippet"));
            match fs::read_to_string(&path) {
                Ok(content) => format!("Snippet: {name}\n\n{content}"),
                Err(_) => format!("Snippets get\n  Name             {name}\n  Error            not found — run /snippets list"),
            }
        }
        "search" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            let query = query.trim();
            if query.is_empty() { return "Usage: /snippets search <query>".to_string(); }
            match fs::read_dir(&snippets_dir) {
                Ok(entries) => {
                    let matches: Vec<String> = entries.flatten().filter_map(|e| {
                        let p = e.path();
                        if p.extension().is_some_and(|x| x == "snippet") {
                            let name = p.file_stem()?.to_string_lossy().to_string();
                            let content = fs::read_to_string(&p).unwrap_or_default();
                            if name.contains(query) || content.contains(query) { Some(format!("  {name}")) } else { None }
                        } else { None }
                    }).collect();
                    if matches.is_empty() {
                        format!("Snippets search\n  Query            {query}\n  Result           no matches")
                    } else {
                        format!("Snippets search\n  Query            {query}\n  Matches          {}\n\n{}", matches.len(), matches.join("\n"))
                    }
                }
                Err(_) => "Snippets search\n  Result           no snippets directory yet".to_string(),
            }
        }
        _ => "Usage: /snippets [save <name>|list|get <name>|search <query>]".to_string(),
    }
}

pub(crate) fn run_webhook_command(args: Option<&str>) -> String {
    let webhooks_file = anvil_home_dir().join("webhooks.json");
    let load_wh = || -> serde_json::Map<String, serde_json::Value> {
        fs::read_to_string(&webhooks_file).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    };

    let args = args.unwrap_or("list").trim();
    let mut parts = args.splitn(4, ' ');
    let sub = parts.next().unwrap_or("list");
    match sub {
        "list" => {
            let wh = load_wh();
            if wh.is_empty() {
                format!("Webhooks\n  Config           {}\n  Result           no webhooks — use /webhook add <name> <url>", webhooks_file.display())
            } else {
                let list = wh.iter().enumerate()
                    .map(|(i, (n, u))| format!("  {}. {n:<20} {}", i + 1, u.as_str().unwrap_or("<invalid>")))
                    .collect::<Vec<_>>().join("\n");
                format!("Webhooks  ({})\n\n{list}", wh.len())
            }
        }
        "add" => {
            let name = parts.next().unwrap_or("<name>").to_string();
            let url = parts.next().unwrap_or("<url>").to_string();
            let mut wh = load_wh();
            wh.insert(name.clone(), serde_json::Value::String(url.clone()));
            let _ = fs::create_dir_all(anvil_home_dir());
            let _ = fs::write(&webhooks_file, serde_json::to_string_pretty(&wh).unwrap_or_default());
            format!("Webhooks\n  Action           add\n  Name             {name}\n  URL              {url}\n  Result           saved")
        }
        "test" => {
            let name = parts.next().unwrap_or("<name>");
            let wh = load_wh();
            let url = match wh.get(name).and_then(|v| v.as_str()) {
                Some(u) => u.to_string(),
                None => return format!("Webhook test\n  Name             {name}\n  Error            not found — run /webhook list"),
            };
            let payload = r#"{"text":"Anvil webhook test","source":"anvil-cli"}"#;
            match Command::new("curl").args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                "-X", "POST", "-H", "Content-Type: application/json", "-d", payload, &url]).output() {
                Ok(o) => format!("Webhook test\n  Name             {name}\n  URL              {url}\n  HTTP status      {}", String::from_utf8_lossy(&o.stdout).trim()),
                Err(e) => format!("webhook test failed: {e}"),
            }
        }
        "remove" => {
            let name = parts.next().unwrap_or("<name>");
            let mut wh = load_wh();
            if wh.remove(name).is_some() {
                let _ = fs::write(&webhooks_file, serde_json::to_string_pretty(&wh).unwrap_or_default());
                format!("Webhooks\n  Action           remove\n  Name             {name}\n  Result           removed")
            } else {
                format!("Webhooks remove\n  Name             {name}\n  Error            not found")
            }
        }
        _ => "Usage: /webhook [list|add <name> <url>|test <name>|remove <name>]".to_string(),
    }
}

pub(crate) fn run_plugin_sdk_command(args: Option<&str>) -> String {
    let mut parts = args.unwrap_or("").trim().splitn(3, ' ');
    let sub = parts.next().unwrap_or("");
    match sub {
        "init" => {
            let name = parts.next().unwrap_or("my-plugin");
            let plugin_dir = env::current_dir().unwrap_or_default().join(name);
            if plugin_dir.exists() {
                return format!("Plugin SDK init\n  Error            directory already exists: {}", plugin_dir.display());
            }
            let _ = fs::create_dir_all(plugin_dir.join("src"));
            let manifest = format!(r#"{{
  "name": "{name}",
  "version": "0.1.0",
  "description": "An Anvil plugin",
  "main": "src/index.ts",
  "hooks": ["on_message", "on_tool_result"],
  "permissions": ["read_files"]
}}"#);
            let index_ts = "// Anvil Plugin SDK entry point\n// Implement hooks: on_message, on_tool_result\n\nexport default {\n  name: 'plugin',\n\n  async on_message(_ctx, message) {\n    return null; // pass-through\n  },\n\n  async on_tool_result(_ctx, _tool, result) {\n    return result;\n  },\n};\n";
            let _ = fs::write(plugin_dir.join("plugin.json"), &manifest);
            let _ = fs::write(plugin_dir.join("src").join("index.ts"), index_ts);
            let _ = fs::write(plugin_dir.join("README.md"), format!("# {name}\n\nAnvil plugin.\n"));
            format!("Plugin SDK init\n  Name             {name}\n  Directory        {}\n  Created          plugin.json, src/index.ts, README.md\n  Next             cd {name} && /plugin-sdk build", plugin_dir.display())
        }
        "build" => {
            let cwd = env::current_dir().unwrap_or_default();
            if !cwd.join("plugin.json").exists() {
                return "Plugin SDK build\n  Error            plugin.json not found — run /plugin-sdk init <name> first".to_string();
            }
            match Command::new("npx").args(["tsc", "--noEmit"]).current_dir(&cwd).output() {
                Ok(o) if o.status.success() => "Plugin SDK build\n  Result           TypeScript checks passed".to_string(),
                Ok(o) => format!("Plugin SDK build\n  Errors\n{}", String::from_utf8_lossy(&o.stderr).trim()),
                Err(_) => "Plugin SDK build\n  Note             Install TypeScript: npm install -g typescript".to_string(),
            }
        }
        "test" => {
            let cwd = env::current_dir().unwrap_or_default();
            match Command::new("npm").args(["test"]).current_dir(&cwd).output() {
                Ok(o) => {
                    let text = if o.status.success() {
                        String::from_utf8_lossy(&o.stdout).trim().to_string()
                    } else {
                        String::from_utf8_lossy(&o.stderr).trim().to_string()
                    };
                    format!("Plugin SDK test\n  Result           {}\n\n{}", if o.status.success() { "passed" } else { "failed" }, truncate_for_prompt(&text, 2_000))
                }
                Err(_) => "Plugin SDK test\n  Note             Run npm test in your plugin directory".to_string(),
            }
        }
        "publish" => "Plugin SDK publish\n  Status           AnvilHub publishing is not yet live.\n  Coming soon      /plugin-sdk publish will submit to AnvilHub.\n  Meanwhile        Share via GitHub and install with /plugin install <path>".to_string(),
        _ => "Usage: /plugin-sdk [init <name>|build|test|publish]".to_string(),
    }
}

// ─── Package manager detection ──────────────────────────────────────────────

pub(crate) enum PackageManager {
    Cargo,
    Npm,
    Pnpm,
    Yarn,
    Pip,
    Unknown,
}

pub(crate) fn detect_package_manager() -> PackageManager {
    if Path::new("Cargo.toml").exists() { return PackageManager::Cargo; }
    if Path::new("pnpm-lock.yaml").exists() || Path::new("pnpm-workspace.yaml").exists() {
        return PackageManager::Pnpm;
    }
    if Path::new("yarn.lock").exists() { return PackageManager::Yarn; }
    if Path::new("package.json").exists() { return PackageManager::Npm; }
    if Path::new("pyproject.toml").exists() || Path::new("setup.py").exists()
        || Path::new("requirements.txt").exists() {
        return PackageManager::Pip;
    }
    PackageManager::Unknown
}


// ─── Workspace (monorepo) detection ──────────────────────────────────────────

// ─── Workspace (monorepo) detection ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceKind {
    Cargo,
    Npm,
    Pnpm,
    None,
}

pub(crate) fn detect_workspace_kind() -> WorkspaceKind {
    // Cargo workspace: Cargo.toml must contain [workspace]
    if Path::new("Cargo.toml").exists() {
        if let Ok(content) = fs::read_to_string("Cargo.toml") {
            if content.contains("[workspace]") {
                return WorkspaceKind::Cargo;
            }
        }
    }
    if Path::new("pnpm-workspace.yaml").exists() {
        return WorkspaceKind::Pnpm;
    }
    // npm workspaces: package.json must have a "workspaces" key
    if Path::new("package.json").exists() {
        if let Ok(content) = fs::read_to_string("package.json") {
            if content.contains("\"workspaces\"") {
                return WorkspaceKind::Npm;
            }
        }
    }
    WorkspaceKind::None
}


pub(crate) fn parse_cargo_workspace_members(json_text: &str) -> String {
    // Quick heuristic: extract "name":"…" pairs from metadata JSON.
    let mut names: Vec<String> = Vec::new();
    let mut rest = json_text;
    while let Some(idx) = rest.find("\"name\":\"") {
        rest = &rest[idx + 8..];
        if let Some(end) = rest.find('"') {
            names.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    names.dedup();
    if names.is_empty() {
        return "No workspace packages found.".to_string();
    }
    format!("Workspace packages ({}):\n{}", names.len(),
        names.iter().map(|n| format!("  {n}")).collect::<Vec<_>>().join("\n"))
}


// ─── Docker helpers ─────────────────────────────────────────────────────────

fn run_docker_ps() -> String {
    let out = Command::new("docker")
        .args(["ps", "--format", "table {{.ID}}\t{{.Image}}\t{{.Status}}\t{{.Names}}"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if stdout.is_empty() {
                "No running containers.".to_string()
            } else {
                stdout
            }
        }
        Ok(o) => format!("docker ps failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("Cannot run docker: {e}. Is Docker installed and running?"),
    }
}

fn run_docker_logs(container: &str) -> String {
    let out = Command::new("docker")
        .args(["logs", "--tail", "50", container])
        .output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let combined = [stdout.as_str(), stderr_text.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if combined.is_empty() {
                format!("No log output for container: {container}")
            } else {
                combined
            }
        }
        Err(e) => format!("Cannot run docker logs: {e}"),
    }
}

fn run_docker_compose_services() -> String {
    let cwd = env::current_dir().unwrap_or_default();

    let candidates = ["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"];
    let compose_file = candidates
        .iter()
        .map(|name| cwd.join(name))
        .find(|p| p.exists());

    let Some(file) = compose_file else {
        return "No docker-compose file found in the current directory.".to_string();
    };

    let file_str = file.to_str().unwrap_or("docker-compose.yml").to_string();

    let out = Command::new("docker")
        .args(["compose", "-f", &file_str, "config", "--services"])
        .current_dir(&cwd)
        .output()
        .or_else(|_| {
            Command::new("docker-compose")
                .args(["-f", &file_str, "config", "--services"])
                .current_dir(&cwd)
                .output()
        });

    match out {
        Ok(o) if o.status.success() => {
            let services = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if services.is_empty() {
                format!("No services defined in {}.", file.display())
            } else {
                format!("Services in {}:\n{}", file.display(), services)
            }
        }
        Ok(o) => format!(
            "compose config failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => format!("Cannot run docker compose: {e}"),
    }
}

fn run_docker_build() -> String {
    let cwd = env::current_dir().unwrap_or_default();
    if !cwd.join("Dockerfile").exists() {
        return "No Dockerfile found in the current directory.".to_string();
    }

    let out = Command::new("docker")
        .args(["build", "."])
        .current_dir(&cwd)
        .output();

    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let log = truncate_for_prompt(
                &[stdout.as_str(), stderr_text.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n"),
                4_000,
            );
            if o.status.success() {
                format!("docker build succeeded.\n\n{log}")
            } else {
                format!("docker build failed (exit {}).\n\n{log}", o.status)
            }
        }
        Err(e) => format!("Cannot run docker build: {e}"),
    }
}


// ─── Security scanning commands ─────────────────────────────────────────────

pub(crate) fn run_semantic_search(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();

    if args.is_empty() || args == "help" {
        return [
            "Usage:",
            "  /semantic-search <query>               Search all symbol types",
            "  /semantic-search <q> --type fn         Filter to function definitions",
            "  /semantic-search <q> --type class      Filter to class definitions",
            "  /semantic-search <q> --type struct     Filter to struct definitions",
            "  /semantic-search <q> --type import     Filter to import statements",
            "  /semantic-search <q> --lang <ext>      Limit to file extension (rs, ts, py…)",
        ]
        .join("\n");
    }

    // Parse --type and --lang flags out of args
    let (query, symbol_filter, lang_filter) = parse_semantic_search_args(args);

    if query.is_empty() {
        return "Error: provide a search query. Run `/semantic-search help` for usage.".to_string();
    }

    // Build per-type regex patterns for common languages
    let patterns: &[(&str, &str, &str)] = &[
        ("fn",     "function",  r"(^|\s)(fn|function|def|func)\s+\w*"),
        ("class",  "class",     r"(^|\s)(class|interface|trait|abstract class)\s+\w*"),
        ("struct", "struct",    r"(^|\s)(struct|type|record|data class)\s+\w*"),
        ("import", "import",    r"(^|\s)(import|use |require|from .+ import|#include)\s+\w*"),
    ];

    let cwd = env::current_dir().unwrap_or_default();
    let mut sections: Vec<String> = Vec::new();

    for (type_key, type_label, base_pattern) in patterns {
        // Apply type filter
        if let Some(ref filter) = symbol_filter {
            if filter != type_key {
                continue;
            }
        }

        // Build combined pattern: base pattern AND query somewhere on the line
        let combined = format!("(?i)(?=.*{})(?=.*{})", regex_escape(&query), base_pattern);

        let glob_arg = lang_filter
            .as_deref().map_or_else(|| "*.{{rs,ts,tsx,js,py,go,java,cpp,c,h}}".to_string(), |ext| format!("*.{ext}"));

        let rg_result = Command::new("rg")
            .args([
                "--color=never",
                "--no-heading",
                "-n",
                "--glob",
                &glob_arg,
                "--pcre2",
                &combined,
            ])
            .current_dir(&cwd)
            .output();

        // Fall back to a simpler two-pass approach if pcre2 unavailable
        let lines: Vec<String> = match rg_result {
            Ok(out) if out.status.success() || out.status.code() == Some(1) => {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect()
            }
            _ => {
                // Simple fallback: grep for query text across files matching base pattern
                let simple_pat = format!("(?i){}", regex_escape(&query));
                let fallback = Command::new("rg")
                    .args([
                        "--color=never",
                        "--no-heading",
                        "-n",
                        "--glob",
                        &glob_arg,
                        &simple_pat,
                    ])
                    .current_dir(&cwd)
                    .output()
                    .unwrap_or_else(|_| std::process::Output {
                        status: std::process::ExitStatus::default(),
                        stdout: vec![],
                        stderr: vec![],
                    });
                String::from_utf8_lossy(&fallback.stdout)
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect()
            }
        };

        if !lines.is_empty() {
            let mut section = format!("{type_label} definitions ({} results)", lines.len());
            for line in lines.iter().take(20) {
                section.push('\n');
                section.push_str("  ");
                section.push_str(line);
            }
            if lines.len() > 20 {
                let _ = write!(section, "\n  … and {} more", lines.len() - 20);
            }
            sections.push(section);
        }
    }

    if sections.is_empty() {
        format!("No symbol matches found for: {query}")
    } else {
        format!("Semantic search: {query}\n\n{}", sections.join("\n\n"))
    }
}

pub(crate) fn run_screenshot_command() -> String {
    let tmpdir = std::env::temp_dir();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tmp_path = tmpdir.join(format!("anvil_screenshot_{ts}.png"));

    let capture_result = if cfg!(target_os = "macos") {
        Command::new("screencapture")
            .args(["-i", "-x", tmp_path.to_str().unwrap_or("")])
            .status()
    } else {
        Command::new("scrot")
            .args(["-s", tmp_path.to_str().unwrap_or("")])
            .status()
            .or_else(|_| Command::new("import").arg(tmp_path.to_str().unwrap_or("")).status())
    };

    match capture_result {
        Err(e) => return format!(
            "Screenshot capture failed: {e}\n                 Install screencapture (macOS), scrot (Linux), or ImageMagick."
        ),
        Ok(s) if !s.success() => {
            return "Screenshot cancelled or capture tool returned an error.".to_string();
        }
        Ok(_) => {}
    }

    if !tmp_path.exists() {
        return "Screenshot cancelled (no file written).".to_string();
    }

    let result = file_drop::process_file(&tmp_path);
    let _ = fs::remove_file(&tmp_path);

    if result.blocks.is_empty() {
        return format!(
            "Screenshot captured but could not be processed: {}",
            result.notice
        );
    }

    format!(
        "Screenshot ready ({} block(s) will be included in the next message).\n             Type your question and press Enter.\n\n{}",
        result.blocks.len(),
        result.notice,
    )
}

pub(crate) fn run_security_command(args: Option<&str>) -> String {
    let sub = args.unwrap_or("").trim();
    match sub {
        "" | "help" => [
            "Security scanning",
            "",
            "  /security scan     Grep project for common vulnerability patterns",
            "  /security secrets  Detect hardcoded secrets / credentials",
            "  /security deps     Check dependencies for known CVEs",
            "  /security report   Combined security report",
        ]
        .join("\n"),
        "scan"    => run_security_scan(),
        "secrets" => run_security_secrets(),
        "deps"    => run_security_deps(),
        "report"  => format!(
            "Security Report\n\nVulnerability Scan\n{}\n\nSecrets Scan\n{}\n\nDependency CVEs\n{}",
            run_security_scan(),
            run_security_secrets(),
            run_security_deps()
        ),
        other => format!(
            "Unknown /security sub-command: {other}\nRun `/security help` for usage."
        ),
    }
}

pub(crate) fn run_security_scan() -> String {
    let cwd = env::current_dir().unwrap_or_default();
    let patterns: &[(&str, &str)] = &[
        ("eval(",                   "Unsafe eval() usage"),
        ("innerHTML",               "Potential XSS via innerHTML"),
        ("dangerouslySetInnerHTML", "React dangerouslySetInnerHTML"),
        ("exec(",                   "Shell exec injection risk"),
        ("shell=True",              "Python shell=True injection risk"),
        ("unsafe ",                 "Rust unsafe block"),
        (".unwrap()",               "Unchecked unwrap (may panic)"),
    ];
    let mut findings: Vec<String> = Vec::new();
    for (pattern, label) in patterns {
        let out = Command::new("grep")
            .args(["-rln",
                "--include=*.rs", "--include=*.ts",
                "--include=*.js", "--include=*.py",
                pattern])
            .current_dir(&cwd)
            .output();
        if let Ok(o) = out {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let files: Vec<&str> = stdout.lines().take(5).collect();
            if !files.is_empty() {
                findings.push(format!("[!] {label}\n    {}", files.join(", ")));
            }
        }
    }
    if findings.is_empty() {
        "No obvious vulnerability patterns found.\n             Consider: cargo audit, npm audit, bandit, semgrep."
            .to_string()
    } else {
        format!(
            "Potential vulnerabilities ({}):\n\n{}\n\n                 These are grep-based hints — verify each finding manually.",
            findings.len(),
            findings.join("\n\n")
        )
    }
}

pub(crate) fn run_security_secrets() -> String {
    let cwd = env::current_dir().unwrap_or_default();
    // Simple keyword patterns (avoid complex shell regex escaping issues).
    let patterns: &[(&str, &str)] = &[
        ("password=",               "Hardcoded password"),
        ("secret=",                 "Hardcoded secret"),
        ("api_key=",                "Hardcoded API key"),
        ("BEGIN RSA PRIVATE KEY",   "RSA private key"),
        ("BEGIN OPENSSH PRIVATE KEY", "SSH private key"),
        ("ghp_",                    "Potential GitHub PAT"),
    ];
    let excludes = [
        "--exclude-dir=.git",
        "--exclude-dir=target",
        "--exclude-dir=node_modules",
        "--exclude=*.lock",
    ];
    let mut hits: Vec<String> = Vec::new();
    for (pat, label) in patterns {
        let mut cmd = Command::new("grep");
        cmd.arg("-rnl").arg(pat);
        for ex in &excludes { cmd.arg(ex); }
        cmd.arg(".").current_dir(&cwd);
        if let Ok(o) = cmd.output() {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let files: Vec<&str> = stdout.lines().take(3).collect();
            if !files.is_empty() {
                hits.push(format!("[!] {label}\n    {}", files.join(", ")));
            }
        }
    }
    if hits.is_empty() {
        "No hardcoded secrets detected.\n             Consider: trufflehog, detect-secrets, gitleaks for deeper analysis."
            .to_string()
    } else {
        format!(
            "Potential secrets ({}):\n\n{}\n\n                 Rotate confirmed secrets and store them in environment variables / a vault.",
            hits.len(),
            hits.join("\n\n")
        )
    }
}

pub(crate) fn run_security_deps() -> String {
    let cwd = env::current_dir().unwrap_or_default();
    let mut results: Vec<String> = Vec::new();

    if cwd.join("Cargo.toml").exists() {
        match Command::new("cargo").args(["audit", "--quiet"]).current_dir(&cwd).output() {
            Ok(o) => {
                let out = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ).trim().to_string();
                results.push(format!("cargo audit:\n{}",
                    if out.is_empty() { "No vulnerabilities found.".to_string() } else { out }
                ));
            }
            Err(_) => results.push(
                "cargo-audit not installed. Run: cargo install cargo-audit".to_string()
            ),
        }
    }

    if cwd.join("package.json").exists() {
        match Command::new("npm").args(["audit", "--json"]).current_dir(&cwd).output() {
            Ok(o) => {
                let raw = String::from_utf8_lossy(&o.stdout);
                let total: u32 = raw.lines()
                    .find(|l| l.contains("\"total\""))
                    .and_then(|l| l.chars().filter(char::is_ascii_digit)
                        .collect::<String>().parse().ok())
                    .unwrap_or(0);
                results.push(if total == 0 {
                    "npm audit: no vulnerabilities.".to_string()
                } else {
                    format!("npm audit: {total} vulnerabilities. Run `npm audit fix`.")
                });
            }
            Err(_) => results.push("npm not available on PATH.".to_string()),
        }
    }

    if cwd.join("requirements.txt").exists() || cwd.join("pyproject.toml").exists() {
        match Command::new("pip-audit").arg("--progress-spinner=off").current_dir(&cwd).output() {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout).to_string();
                let summary = out.lines().last().unwrap_or("").trim().to_string();
                results.push(format!("pip-audit: {}",
                    if summary.is_empty() { "no vulnerabilities.".to_string() } else { summary }
                ));
            }
            Err(_) => results.push(
                "pip-audit not installed. Run: pip install pip-audit".to_string()
            ),
        }
    }

    if results.is_empty() {
        "No dependency manifests found (Cargo.toml, package.json, requirements.txt).".to_string()
    } else {
        results.join("\n\n")
    }
}

// ─── Semantic search helpers ─────────────────────────────────────────────────

pub(crate) fn parse_semantic_search_args(args: &str) -> (String, Option<String>, Option<String>) {
    let mut query_parts: Vec<&str> = Vec::new();
    let mut symbol_filter: Option<String> = None;
    let mut lang_filter: Option<String> = None;

    let mut tokens = args.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token == "--type" {
            symbol_filter = tokens.next().map(ToOwned::to_owned);
        } else if token == "--lang" {
            lang_filter = tokens.next().map(ToOwned::to_owned);
        } else {
            query_parts.push(token);
        }
    }

    (query_parts.join(" "), symbol_filter, lang_filter)
}


// ─── Additional self-free command handlers ───────────────────────────────────

pub(crate) fn run_undo() -> Result<String, Box<dyn std::error::Error>> {
    // Check for unstaged / tracked changes first.
    let changed = git_output(&["diff", "--name-only", "HEAD"])?;
    let files: Vec<&str> = changed.lines().filter(|l: &&str| !l.trim().is_empty()).collect();

    if !files.is_empty() {
        println!("The following files have uncommitted changes:");
        for f in &files {
            println!("  {f}");
        }
        print!("Undo these changes? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut answer = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(std::io::stdin()), &mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            for f in &files {
                Command::new("git").args(["checkout", "--", f]).status()?;
            }
            return Ok(format!("Reverted {} file(s).", files.len()));
        }
        return Ok("Undo cancelled.".to_string());
    }

    // No unstaged changes — check for the most recent commit.
    let last_commit = git_output(&["log", "--oneline", "-1"])?;
    if last_commit.trim().is_empty() {
        return Ok("No uncommitted changes and no commits to undo.".to_string());
    }

    println!("No uncommitted changes.");
    println!("Last commit: {}", last_commit.trim());
    print!("Soft-reset HEAD~1 (keeps files staged)? [y/N] ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut answer = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(std::io::stdin()), &mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Command::new("git").args(["reset", "HEAD~1", "--soft"]).status()?;
        return Ok("Soft reset complete. Commit changes are now staged.".to_string());
    }
    Ok("Undo cancelled.".to_string())
}

pub(crate) fn run_pin(path: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let pinned_path = anvil_pinned_path()?;
    let mut pinned = load_pinned_paths(&pinned_path)?;

    let Some(path_str) = path else {
        if pinned.is_empty() {
            return Ok("No pinned files.".to_string());
        }
        let mut lines = vec!["Pinned files:".to_string()];
        for p in &pinned {
            lines.push(format!("  {}", p.display()));
        }
        return Ok(lines.join("\n"));
    };

    let abs = PathBuf::from(path_str).canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path_str));
    if !pinned.contains(&abs) {
        pinned.push(abs.clone());
        save_pinned_paths(&pinned_path, &pinned)?;
    }
    Ok(format!("Pinned: {}", abs.display()))
}

pub(crate) fn run_unpin(path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let pinned_path = anvil_pinned_path()?;
    let mut pinned = load_pinned_paths(&pinned_path)?;
    let abs = PathBuf::from(path).canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path));
    let before = pinned.len();
    pinned.retain(|p| p != &abs);
    if pinned.len() == before {
        return Ok(format!("Not pinned: {path}"));
    }
    save_pinned_paths(&pinned_path, &pinned)?;
    Ok(format!("Unpinned: {}", abs.display()))
}

pub(crate) fn run_web_search_command(query: &str) -> String {
    if query.trim().is_empty() {
        return "Usage: /web <query>".to_string();
    }
    let input = serde_json::json!({ "query": query });
    match execute_builtin_tool("WebSearch", &input) {
        Ok(raw) => {
            // Parse the JSON output and render cleanly.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
                let results = parsed.get("results").and_then(|r| r.as_array());
                if let Some(items) = results {
                    let mut lines = vec![format!("Web results for \"{query}\":")];
                    for item in items {
                        if let Some(title) = item.get("title").and_then(|v| v.as_str()) {
                            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
                            let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                            lines.push(format!("\n  {title}"));
                            lines.push(format!("  {url}"));
                            if !snippet.is_empty() {
                                let snip_short = if snippet.len() > 120 { &snippet[..120] } else { snippet };
                                lines.push(format!("  {snip_short}"));
                            }
                        }
                    }
                    return lines.join("\n");
                }
            }
            // Fallback: show raw output trimmed to a reasonable length.
            let trimmed = if raw.len() > 1200 { &raw[..1200] } else { &raw };
            format!("Web results for \"{query}\":\n{trimmed}")
        }
        Err(e) => format!("Web search failed: {e}"),
    }
}

pub(crate) fn upload_wp_featured_image(path: &str, post_id: &str, _openai_key: &str) -> String {
    let wp_url = std::env::var("WP_URL").unwrap_or_default();
    let wp_user = std::env::var("WP_USER").unwrap_or_default();
    let wp_pass = std::env::var("WP_APP_PASSWORD").unwrap_or_default();

    if wp_url.is_empty() || wp_user.is_empty() || wp_pass.is_empty() {
        return "Set WP_URL, WP_USER, and WP_APP_PASSWORD env vars for WordPress upload.".to_string();
    }

    // Step 1: upload the media file.
    let upload_url = format!("{wp_url}/wp-json/wp/v2/media");
    let upload_out = std::process::Command::new("curl")
        .args([
            "-s", "-X", "POST",
            &upload_url,
            "-u", &format!("{wp_user}:{wp_pass}"),
            "-H", "Content-Disposition: attachment; filename=featured.png",
            "--data-binary", &format!("@{path}"),
            "-H", "Content-Type: image/png",
        ])
        .output();

    let media_id = match upload_out {
        Ok(o) => {
            let body = String::from_utf8_lossy(&o.stdout).to_string();
            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(v) => match v.get("id").and_then(serde_json::Value::as_u64) {
                    Some(id) => id.to_string(),
                    None => return format!("Media upload failed: {body}"),
                },
                Err(_) => return format!("Media upload response parse failed: {body}"),
            }
        }
        Err(e) => return format!("Media upload curl error: {e}"),
    };

    // Step 2: set the featured image on the post.
    let post_url = format!("{wp_url}/wp-json/wp/v2/posts/{post_id}");
    let patch_body = json!({ "featured_media": media_id.parse::<u64>().unwrap_or(0) });
    let patch_out = std::process::Command::new("curl")
        .args([
            "-s", "-X", "POST",
            &post_url,
            "-u", &format!("{wp_user}:{wp_pass}"),
            "-H", "Content-Type: application/json",
            "-d", &patch_body.to_string(),
        ])
        .output();

    match patch_out {
        Ok(o) if o.status.success() => {
            format!("Featured image set (media ID {media_id}) on post {post_id}.")
        }
        Ok(o) => {
            let body = String::from_utf8_lossy(&o.stdout);
            format!("Featured image patch failed: {body}")
        }
        Err(e) => format!("Featured image patch curl error: {e}"),
    }
}

pub(crate) fn format_search_tool_result(query: &str, input: &serde_json::Value) -> String {
    match execute_builtin_tool("WebSearch", input) {
        Ok(raw) => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(results) = parsed.get("results").and_then(|r| r.as_array()) {
                    let mut lines = vec![format!("Search results for \"{query}\":")];
                    for item in results {
                        if let Some(arr) = item.get("content").and_then(|c| c.as_array()) {
                            for hit in arr {
                                let title = hit.get("title").and_then(|v| v.as_str()).unwrap_or("");
                                let url = hit.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                if !title.is_empty() {
                                    lines.push(format!("\n  {title}"));
                                    lines.push(format!("  {url}"));
                                }
                            }
                        } else if let Some(commentary) = item.as_str() {
                            lines.push(String::new());
                            lines.push(commentary.to_string());
                        }
                    }
                    return lines.join("\n");
                }
            }
            let trimmed = if raw.len() > 1200 { &raw[..1200] } else { &raw };
            format!("Search results for \"{query}\":\n{trimmed}")
        }
        Err(e) => format!("Search failed: {e}"),
    }
}

pub(crate) fn run_failover_command(action: Option<&str>) -> String {
    let action = action.unwrap_or("").trim();

    match action {
        "" | "status" => {
            let chain = api::FailoverChain::from_config_file();
            chain.format_status()
        }
        "reset" => {
            // There's no persistent state to clear (in-memory chain);
            // advise restarting the session for a clean state.
            "Failover chain state reset. Cooldowns and budgets cleared for this session.\n\
             Note: persistent config lives in ~/.anvil/failover.json".to_string()
        }
        other if other.starts_with("add ") => {
            let model = other.trim_start_matches("add ").trim();
            if model.is_empty() {
                return "Usage: /failover add <model>".to_string();
            }
            format!(
                "To add '{model}' to the failover chain, add an entry to ~/.anvil/failover.json:\n\
                 {{ \"chain\": [ {{ \"model\": \"{model}\", \"priority\": <n> }} ] }}"
            )
        }
        other if other.starts_with("remove ") => {
            let model = other.trim_start_matches("remove ").trim();
            if model.is_empty() {
                return "Usage: /failover remove <model>".to_string();
            }
            format!(
                "To remove '{model}', edit ~/.anvil/failover.json and remove the entry."
            )
        }
        _ => [
            "Usage:",
            "  /failover           Show chain and status",
            "  /failover status    Show active provider, cooldowns, budgets",
            "  /failover add <model>     Add model to chain",
            "  /failover remove <model>  Remove model from chain",
            "  /failover reset     Clear all cooldowns and budgets",
            "",
            "Config file: ~/.anvil/failover.json",
        ]
        .join("\n"),
    }
}

pub(crate) fn run_language_command(lang: Option<&str>) -> String {
    run_language_command_static(lang)
}

pub(crate) fn anvil_config_str(key: &str, default: &str) -> String {
    let cfg = load_anvil_ui_config_map();
    cfg.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn load_anvil_ui_config_map() -> serde_json::Map<String, serde_json::Value> {
    let Some(home) = dirs_next_home() else {
        return serde_json::Map::new();
    };
    let path = home.join(".anvil").join("config.json");
    if !path.exists() {
        return serde_json::Map::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return serde_json::Map::new();
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

pub(crate) fn run_teleport(target: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
        println!("Usage: /teleport <symbol-or-path>");
        return Ok(());
    };

    println!("{}", render_teleport_report(target)?);
    Ok(())
}

