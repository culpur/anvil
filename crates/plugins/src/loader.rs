use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::hooks::{sanitize_hook_command, HookKind, HookSpec};
use crate::diagnostics::PluginLoadDiagnostic;
use crate::manifest::{
    is_literal_command, load_plugin_from_directory_with_diagnostics, plugin_manifest_path,
    PluginHooks, PluginLifecycle, PluginToolDefinition, PluginToolManifest,
};
use crate::tools::PluginTool;
use crate::{
    BuiltinPlugin, BundledPlugin, ExternalPlugin, PluginDefinition, PluginError, PluginKind,
    PluginMetadata,
};

// ---------------------------------------------------------------------------
// Constants (shared with manager)
// ---------------------------------------------------------------------------

pub(crate) const BUILTIN_MARKETPLACE: &str = "builtin";
pub(crate) const BUNDLED_MARKETPLACE: &str = "bundled";
pub(crate) const EXTERNAL_MARKETPLACE: &str = "external";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[must_use]
pub fn builtin_plugins() -> Vec<PluginDefinition> {
    vec![PluginDefinition::Builtin(BuiltinPlugin {
        metadata: PluginMetadata {
            id: plugin_id("example-builtin", BUILTIN_MARKETPLACE),
            name: "example-builtin".to_string(),
            version: "0.1.0".to_string(),
            description: "Example built-in plugin scaffold for the Rust plugin system".to_string(),
            kind: PluginKind::Builtin,
            source: BUILTIN_MARKETPLACE.to_string(),
            default_enabled: false,
            root: None,
        },
        hooks: PluginHooks::default(),
        lifecycle: PluginLifecycle::default(),
        tools: Vec::new(),
    })]
}

/// Load a plugin definition from `root`, returning the definition and any
/// per-hook-entry diagnostics that were collected during manifest parsing.
///
/// Returns `Err` only for system-level failures (permission denied, OOM, etc.)
/// or for manifests that are structurally invalid JSON / violate the required
/// schema fields.  Unknown hook variants are captured as diagnostics instead.
pub(crate) fn load_plugin_definition_with_diagnostics(
    root: &Path,
    kind: PluginKind,
    source: String,
    marketplace: &str,
) -> Result<(PluginDefinition, Vec<PluginLoadDiagnostic>), PluginError> {
    let (manifest, diagnostics) = load_plugin_from_directory_with_diagnostics(root)?;
    let metadata = PluginMetadata {
        id: plugin_id(&manifest.name, marketplace),
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        kind,
        source,
        default_enabled: manifest.default_enabled,
        root: Some(root.to_path_buf()),
    };
    let hooks = resolve_hooks(root, &manifest.hooks);
    let lifecycle = resolve_lifecycle(root, &manifest.lifecycle);
    let tools = resolve_tools(root, &metadata.id, &metadata.name, &manifest.tools);
    let definition = match kind {
        PluginKind::Builtin => PluginDefinition::Builtin(BuiltinPlugin {
            metadata,
            hooks,
            lifecycle,
            tools,
        }),
        PluginKind::Bundled => PluginDefinition::Bundled(BundledPlugin {
            metadata,
            hooks,
            lifecycle,
            tools,
        }),
        PluginKind::External => PluginDefinition::External(ExternalPlugin {
            metadata,
            hooks,
            lifecycle,
            tools,
        }),
    };
    Ok((definition, diagnostics))
}

/// Convenience wrapper that drops per-hook diagnostics, for callers that do
/// not participate in the diagnostic pipeline (e.g. `install`, `update`).
#[allow(dead_code)]
pub(crate) fn load_plugin_definition(
    root: &Path,
    kind: PluginKind,
    source: String,
    marketplace: &str,
) -> Result<PluginDefinition, PluginError> {
    load_plugin_definition_with_diagnostics(root, kind, source, marketplace)
        .map(|(definition, _diagnostics)| definition)
}

pub(crate) fn discover_plugin_dirs(root: &Path) -> Result<Vec<PathBuf>, PluginError> {
    match fs::read_dir(root) {
        Ok(entries) => {
            let mut paths = Vec::new();
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() && plugin_manifest_path(&path).is_ok() {
                    paths.push(path);
                }
            }
            paths.sort();
            Ok(paths)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(PluginError::Io(error)),
    }
}

// ---------------------------------------------------------------------------
// Path validation (runtime)
// ---------------------------------------------------------------------------

pub(crate) fn validate_hook_paths(root: Option<&Path>, hooks: &PluginHooks) -> Result<(), PluginError> {
    let Some(root) = root else {
        return Ok(());
    };
    for spec in hooks.pre_tool_use.iter().chain(hooks.post_tool_use.iter()) {
        // Prompt hooks have no filesystem path — only validate non-empty body.
        if spec.is_prompt() {
            spec.validate_non_empty().map_err(PluginError::InvalidManifest)?;
            continue;
        }
        validate_command_path(root, spec.body(), "hook")?;
    }
    Ok(())
}

pub(crate) fn validate_lifecycle_paths(
    root: Option<&Path>,
    lifecycle: &PluginLifecycle,
) -> Result<(), PluginError> {
    let Some(root) = root else {
        return Ok(());
    };
    for entry in lifecycle.init.iter().chain(lifecycle.shutdown.iter()) {
        validate_command_path(root, entry, "lifecycle command")?;
    }
    Ok(())
}

pub(crate) fn validate_tool_paths(root: Option<&Path>, tools: &[PluginTool]) -> Result<(), PluginError> {
    let Some(root) = root else {
        return Ok(());
    };
    for tool in tools {
        validate_command_path(root, &tool.command, "tool")?;
    }
    Ok(())
}

fn validate_command_path(root: &Path, entry: &str, kind: &str) -> Result<(), PluginError> {
    if is_literal_command(entry) {
        return Ok(());
    }
    let raw_path = if Path::new(entry).is_absolute() {
        PathBuf::from(entry)
    } else {
        root.join(entry)
    };
    if !raw_path.exists() {
        return Err(PluginError::InvalidManifest(format!(
            "{kind} path `{}` does not exist",
            raw_path.display()
        )));
    }
    // Resolve symlinks and any embedded `..` components, then verify the
    // canonical path still lives within the plugin root.  This prevents both
    // path-traversal attacks (hooks/../../sensitive) and symlink escapes.
    let canonical = raw_path.canonicalize().map_err(|e| {
        PluginError::InvalidManifest(format!(
            "could not canonicalize {kind} path `{}`: {e}",
            raw_path.display()
        ))
    })?;
    let canonical_root = root.canonicalize().map_err(|e| {
        PluginError::InvalidManifest(format!(
            "could not canonicalize plugin root `{}`: {e}",
            root.display()
        ))
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(PluginError::InvalidManifest(format!(
            "{kind} path `{}` escapes the plugin root directory",
            raw_path.display()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Lifecycle command execution
// ---------------------------------------------------------------------------

pub(crate) fn run_lifecycle_commands(
    metadata: &PluginMetadata,
    lifecycle: &PluginLifecycle,
    phase: &str,
    commands: &[String],
) -> Result<(), PluginError> {
    if lifecycle.is_empty() || commands.is_empty() {
        return Ok(());
    }

    for command in commands {
        let mut process = if Path::new(command).exists() {
            if cfg!(windows) {
                // Direct execution on Windows via cmd /C with a resolved path.
                let mut process = Command::new("cmd");
                process.arg("/C").arg(command);
                process
            } else {
                // Pass the path as a positional argument to `sh` — injection-safe
                // because the path is not shell-evaluated, and works for scripts
                // without the executable bit set.
                let mut process = Command::new("sh");
                process.arg(command);
                process
            }
        } else if cfg!(windows) {
            // Non-path shell command on Windows: validate before passing to cmd.
            sanitize_hook_command(command).map_err(|reason| {
                PluginError::CommandFailed(format!(
                    "plugin `{}` lifecycle command rejected: {reason}",
                    metadata.id
                ))
            })?;
            let mut process = Command::new("cmd");
            process.arg("/C").arg(command);
            process
        } else {
            // Non-path shell command on POSIX: validate before passing to sh -lc.
            sanitize_hook_command(command).map_err(|reason| {
                PluginError::CommandFailed(format!(
                    "plugin `{}` lifecycle command rejected: {reason}",
                    metadata.id
                ))
            })?;
            let mut process = Command::new("sh");
            process.arg("-lc").arg(command);
            process
        };
        if let Some(root) = &metadata.root {
            process.current_dir(root);
        }
        let output = process.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(PluginError::CommandFailed(format!(
                "plugin `{}` {} failed for `{}`: {}",
                metadata.id,
                phase,
                command,
                if stderr.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    stderr
                }
            )));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution helpers (manifest → runtime paths)
// ---------------------------------------------------------------------------

pub(crate) fn resolve_hooks(root: &Path, hooks: &PluginHooks) -> PluginHooks {
    PluginHooks {
        pre_tool_use: hooks
            .pre_tool_use
            .iter()
            .map(|spec| resolve_hook_spec(root, spec))
            .collect(),
        post_tool_use: hooks
            .post_tool_use
            .iter()
            .map(|spec| resolve_hook_spec(root, spec))
            .collect(),
    }
}

pub(crate) fn resolve_lifecycle(root: &Path, lifecycle: &PluginLifecycle) -> PluginLifecycle {
    PluginLifecycle {
        init: lifecycle
            .init
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
        shutdown: lifecycle
            .shutdown
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
    }
}

pub(crate) fn resolve_tools(
    root: &Path,
    plugin_id: &str,
    plugin_name: &str,
    tools: &[PluginToolManifest],
) -> Vec<PluginTool> {
    tools
        .iter()
        .map(|tool| {
            PluginTool::new(
                plugin_id,
                plugin_name,
                PluginToolDefinition {
                    name: tool.name.clone(),
                    description: Some(tool.description.clone()),
                    input_schema: tool.input_schema.clone(),
                },
                resolve_hook_entry(root, &tool.command),
                tool.args.clone(),
                tool.required_permission,
                Some(root.to_path_buf()),
            )
        })
        .collect()
}

/// Resolve a `HookSpec` against the plugin root directory.
///
/// For command specs, relative paths are expanded to absolute paths so the
/// runner can exec them without a working-directory assumption.  Prompt specs
/// carry no filesystem path and are returned unchanged.
fn resolve_hook_spec(root: &Path, spec: &HookSpec) -> HookSpec {
    match spec {
        HookSpec::Command(entry) => HookSpec::Command(resolve_hook_entry(root, entry)),
        HookSpec::Tagged { kind: HookKind::Prompt, body } => HookSpec::Tagged {
            kind: HookKind::Prompt,
            body: body.clone(),
        },
        HookSpec::Tagged { kind: HookKind::Command, body } => HookSpec::Tagged {
            kind: HookKind::Command,
            body: resolve_hook_entry(root, body),
        },
    }
}

fn resolve_hook_entry(root: &Path, entry: &str) -> String {
    if is_literal_command(entry) {
        entry.to_string()
    } else {
        root.join(entry).display().to_string()
    }
}

// ---------------------------------------------------------------------------
// ID helpers
// ---------------------------------------------------------------------------

pub(crate) fn plugin_id(name: &str, marketplace: &str) -> String {
    format!("{name}@{marketplace}")
}

pub(crate) fn sanitize_plugin_id(plugin_id: &str) -> String {
    plugin_id
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '@' | ':' => '-',
            other => other,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Filesystem utilities
// ---------------------------------------------------------------------------

pub(crate) fn copy_dir_all(source: &Path, destination: &Path) -> Result<(), PluginError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
