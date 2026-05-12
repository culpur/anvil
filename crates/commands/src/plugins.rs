use plugins::{PluginError, PluginHooks, PluginManager, PluginSummary, PluginTool};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginsCommandResult {
    pub message: String,
    pub reload_runtime: bool,
}

#[allow(clippy::too_many_lines)]
pub fn handle_plugins_slash_command(
    action: Option<&str>,
    target: Option<&str>,
    manager: &mut PluginManager,
) -> Result<PluginsCommandResult, PluginError> {
    match action {
        None | Some("list") => Ok(PluginsCommandResult {
            message: render_plugins_report(&manager.list_installed_plugins()?),
            reload_runtime: false,
        }),
        Some("install") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins install <path>".to_string(),
                    reload_runtime: false,
                });
            };
            let install = manager.install(target)?;
            let plugin = manager
                .list_installed_plugins()?
                .into_iter()
                .find(|plugin| plugin.metadata.id == install.plugin_id);
            Ok(PluginsCommandResult {
                message: render_plugin_install_report(&install.plugin_id, plugin.as_ref()),
                reload_runtime: true,
            })
        }
        Some("enable") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins enable <name>".to_string(),
                    reload_runtime: false,
                });
            };
            let plugin = resolve_plugin_target(manager, target)?;
            manager.enable(&plugin.metadata.id)?;
            Ok(PluginsCommandResult {
                message: format!(
                    "Plugins\n  Result           enabled {}\n  Name             {}\n  Version          {}\n  Status           enabled",
                    plugin.metadata.id, plugin.metadata.name, plugin.metadata.version
                ),
                reload_runtime: true,
            })
        }
        Some("disable") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins disable <name>".to_string(),
                    reload_runtime: false,
                });
            };
            let plugin = resolve_plugin_target(manager, target)?;
            manager.disable(&plugin.metadata.id)?;
            Ok(PluginsCommandResult {
                message: format!(
                    "Plugins\n  Result           disabled {}\n  Name             {}\n  Version          {}\n  Status           disabled",
                    plugin.metadata.id, plugin.metadata.name, plugin.metadata.version
                ),
                reload_runtime: true,
            })
        }
        Some("uninstall") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins uninstall <plugin-id>".to_string(),
                    reload_runtime: false,
                });
            };
            manager.uninstall(target)?;
            Ok(PluginsCommandResult {
                message: format!("Plugins\n  Result           uninstalled {target}"),
                reload_runtime: true,
            })
        }
        Some("update") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins update <plugin-id>".to_string(),
                    reload_runtime: false,
                });
            };
            let update = manager.update(target)?;
            let plugin = manager
                .list_installed_plugins()?
                .into_iter()
                .find(|plugin| plugin.metadata.id == update.plugin_id);
            Ok(PluginsCommandResult {
                message: format!(
                    "Plugins\n  Result           updated {}\n  Name             {}\n  Old version      {}\n  New version      {}\n  Status           {}",
                    update.plugin_id,
                    plugin
                        .as_ref()
                        .map_or_else(|| update.plugin_id.clone(), |plugin| plugin.metadata.name.clone()),
                    update.old_version,
                    update.new_version,
                    plugin
                        .as_ref()
                        .map_or("unknown", |plugin| if plugin.enabled { "enabled" } else { "disabled" }),
                ),
                reload_runtime: true,
            })
        }
        Some("details" | "show" | "info") => {
            // CC-139-F4 parity: `anvil plugin details <name>` (and slash
            // aliases /plugin details / show / info) print a multi-section
            // report with inventory + approximate token cost.
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugin details <name>".to_string(),
                    reload_runtime: false,
                });
            };
            let plugin = resolve_plugin_target(manager, target)?;
            let hooks = manager.aggregated_hooks().ok();
            let tools = manager.aggregated_tools().ok();
            Ok(PluginsCommandResult {
                message: render_plugin_details(&plugin, hooks.as_ref(), tools.as_deref()),
                reload_runtime: false,
            })
        }
        Some(other) => Ok(PluginsCommandResult {
            message: format!(
                "Unknown /plugins action '{other}'. Use list, install, enable, disable, uninstall, update, or details."
            ),
            reload_runtime: false,
        }),
    }
}

#[must_use]
pub fn render_plugins_report(plugins: &[PluginSummary]) -> String {
    let mut lines = vec!["Plugins".to_string()];
    if plugins.is_empty() {
        lines.push("  No plugins installed.".to_string());
        return lines.join("\n");
    }
    for plugin in plugins {
        let enabled = if plugin.enabled {
            "enabled"
        } else {
            "disabled"
        };
        lines.push(format!(
            "  {name:<20} v{version:<10} {enabled}",
            name = plugin.metadata.name,
            version = plugin.metadata.version,
        ));
    }
    lines.join("\n")
}

fn render_plugin_install_report(plugin_id: &str, plugin: Option<&PluginSummary>) -> String {
    let name = plugin.map_or(plugin_id, |plugin| plugin.metadata.name.as_str());
    let version = plugin.map_or("unknown", |plugin| plugin.metadata.version.as_str());
    let enabled = plugin.is_some_and(|plugin| plugin.enabled);
    format!(
        "Plugins\n  Result           installed {plugin_id}\n  Name             {name}\n  Version          {version}\n  Status           {}",
        if enabled { "enabled" } else { "disabled" }
    )
}

/// CC-139-F4 parity: render a detailed inventory + token-cost estimate.
///
/// Note: `hooks` and `tools` here are the *aggregated* (workspace-wide)
/// counts, since `PluginSummary` does not carry per-plugin hooks. That
/// matches the spirit of CC's output — "what does enabling this
/// surface cost across the runtime" — without needing a per-plugin
/// registry walk we don't otherwise expose.
#[must_use]
pub fn render_plugin_details(
    plugin: &PluginSummary,
    hooks: Option<&PluginHooks>,
    tools: Option<&[PluginTool]>,
) -> String {
    let m = &plugin.metadata;
    let status = if plugin.enabled { "enabled" } else { "disabled" };
    let root = m
        .root
        .as_ref()
        .map_or_else(|| "—".to_string(), |p| p.display().to_string());

    let (pre_hooks, post_hooks, hook_bytes) = hooks.map_or((0, 0, 0usize), |h| {
        let pre = h.pre_tool_use.len();
        let post = h.post_tool_use.len();
        // Rough byte size: serialise the spec list to JSON.
        let bytes = serde_json::to_string(h).map(|s| s.len()).unwrap_or(0);
        (pre, post, bytes)
    });
    let (tool_count, tool_bytes) = tools.map_or((0, 0usize), |list| {
        let count = list.len();
        let bytes = list
            .iter()
            .map(|t| {
                let def = t.definition();
                def.name.len() + def.description.as_deref().map_or(0, str::len)
            })
            .sum::<usize>();
        (count, bytes)
    });
    // Token cost: ~4 chars per token is the conventional approximation.
    let approx_tokens = ((hook_bytes + tool_bytes) + 3) / 4;

    let mut lines = vec![
        format!("Plugin: {}", m.name),
        format!("  ID               {}", m.id),
        format!("  Version          {}", m.version),
        format!("  Status           {status}"),
        format!("  Kind             {:?}", m.kind),
        format!("  Source           {}", m.source),
        format!("  Root             {root}"),
    ];
    if !m.description.is_empty() {
        lines.push(format!("  Description      {}", m.description));
    }
    lines.push(String::new());
    lines.push("Inventory (aggregated, workspace-wide):".to_string());
    lines.push(format!("  Pre-tool hooks   {pre_hooks}"));
    lines.push(format!("  Post-tool hooks  {post_hooks}"));
    lines.push(format!("  Tools            {tool_count}"));
    lines.push(String::new());
    lines.push("Approx. system-prompt cost:".to_string());
    lines.push(format!("  Bytes            {}", hook_bytes + tool_bytes));
    lines.push(format!("  Tokens (~)       {approx_tokens}"));
    lines.join("\n")
}

fn resolve_plugin_target(
    manager: &mut PluginManager,
    target: &str,
) -> Result<PluginSummary, PluginError> {
    let mut matches = manager
        .list_installed_plugins()?
        .into_iter()
        .filter(|plugin| plugin.metadata.id == target || plugin.metadata.name == target)
        .collect::<Vec<_>>();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(PluginError::NotFound(format!(
            "plugin `{target}` is not installed or discoverable"
        ))),
        _ => Err(PluginError::InvalidManifest(format!(
            "plugin name `{target}` is ambiguous; use the full plugin id"
        ))),
    }
}
