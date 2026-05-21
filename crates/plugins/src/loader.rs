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
            hub_trust_level: None,
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
        hub_trust_level: None,
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
        // CC parity v2.2.14: exec-form hooks resolve args[0] (the program)
        // against the plugin root the same way Command does; args[1..] are
        // user values and pass through verbatim (the whole point of args[]
        // is no shell, no path mangling on argument values).
        HookSpec::Exec { args, continue_on_block } => {
            let mut resolved = args.clone();
            if let Some(program) = resolved.first_mut() {
                *program = resolve_hook_entry(root, program);
            }
            HookSpec::Exec {
                args: resolved,
                continue_on_block: *continue_on_block,
            }
        }
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
// Task #726 (CC v2.1.144-B18 parity): skill watcher FD-exhaustion prevention
// ---------------------------------------------------------------------------
//
// CC's skill watcher fired on every file change in the skill directory tree.
// When users dropped a build artifact dir (Rust `target/`, JS `node_modules/`,
// generic `dist/` / `build/` / `.git/`) inside a skill folder, every compile
// cascade triggered a full skill reload — exhausting the inotify FD quota.
//
// Anvil does not currently ship a live skill watcher; this module pre-bakes
// the policy that any future watcher MUST consult so the regression can never
// re-surface. Two filters:
//
//   - `is_excluded_watch_subtree(path)` — at watcher creation, recursive
//     watches MUST skip these subtrees so the inotify FD count stays bounded.
//   - `should_trigger_skill_reload(path)` — when an event fires, the watcher
//     MUST only initiate a reload action when the changed file is a `.md`
//     (case-insensitive on the extension). Non-`.md` changes still update the
//     watch-state index (so deletions / new files are tracked) but never
//     trigger the expensive reload path.
//
// CC's fix shape: monitor everything for index integrity, reload state only
// on `.md` edits.

/// Subtree names that the skill watcher MUST exclude from recursive watches.
/// These are the build/cache directories whose churn rate is unbounded and
/// has no signal value for a skill index.
///
/// Source: CC v2.1.144-B18 + observed FD-exhaustion incidents in marketplaces
/// that ship Rust / Node-toolchain skills.
pub const SKILL_WATCH_EXCLUDED_SUBTREES: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "dist",
    "build",
    ".cache",
    ".next",
    "__pycache__",
];

/// Returns `true` iff any path component matches a directory name in
/// `SKILL_WATCH_EXCLUDED_SUBTREES`. Case-sensitive (filesystem semantics).
///
/// Future watcher: call this before `Watcher::watch(path, Recursive)` and
/// short-circuit when `true`. Recursive watches over excluded subtrees are
/// the FD-exhaustion vector and MUST never be opened, even on rescans.
#[must_use]
pub fn is_excluded_watch_subtree(path: &Path) -> bool {
    path.components().any(|comp| {
        if let std::path::Component::Normal(os) = comp {
            if let Some(name) = os.to_str() {
                return SKILL_WATCH_EXCLUDED_SUBTREES
                    .iter()
                    .any(|excluded| *excluded == name);
            }
        }
        false
    })
}

/// Returns `true` iff a change event on `path` should trigger a skill reload.
///
/// Policy:
///   1. Path MUST be a regular file (no directories — those are watch-index
///      updates only).
///   2. Extension MUST be `.md` (case-insensitive — `Skill.MD` and
///      `skill.md` both reload).
///   3. The path MUST NOT live inside an excluded watch subtree
///      (defense-in-depth: even if the watcher somehow saw a `.md` event
///      inside `target/`, we still don't reload — those are build artifacts).
///
/// Non-`.md` changes (build artifacts, binaries, lock files, `node_modules/`
/// transient files) return `false` so the watcher only updates its index;
/// no reload action fires.
#[must_use]
pub fn should_trigger_skill_reload(path: &Path) -> bool {
    if is_excluded_watch_subtree(path) {
        return false;
    }
    let Some(ext_os) = path.extension() else {
        return false;
    };
    let Some(ext) = ext_os.to_str() else {
        return false;
    };
    ext.eq_ignore_ascii_case("md")
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

#[cfg(test)]
mod skill_watcher_filter_tests {
    //! Task #726 (CC v2.1.144-B18 parity): regression tests for the skill
    //! watcher FD-exhaustion filters. Pins the contract so any future watcher
    //! implementation (or refactor of these helpers) doesn't silently re-open
    //! the inotify-flood vector.
    //!
    //! The two tests requested by the parity audit:
    //!   1. A `.o` build artifact inside `target/` MUST NOT trigger a reload.
    //!   2. A `skill.md` edit MUST trigger a reload.
    //!
    //! Plus the watch-subtree-exclusion check that prevents the FD flood at
    //! the watcher's `Watch::Recursive` registration.

    use super::{
        is_excluded_watch_subtree, should_trigger_skill_reload, SKILL_WATCH_EXCLUDED_SUBTREES,
    };
    use std::path::Path;

    /// Driver mirroring how a real watcher would consume events. We count
    /// "reload" actions and use the same predicate the production wiring would
    /// consult. Two scenarios (parity audit spec):
    ///   - Touch `target/build.o` → counter unchanged.
    ///   - Touch `skill.md` → counter +1.
    #[test]
    fn build_artifact_in_target_does_not_reload_md_does() {
        let mut reload_counter: u32 = 0;

        // (1) Build artifact inside `target/` — must NOT reload.
        let target_o = Path::new("skills/audit/target/build.o");
        if should_trigger_skill_reload(target_o) {
            reload_counter += 1;
        }
        assert_eq!(
            reload_counter, 0,
            "touching target/build.o must NOT trigger a skill reload (parity audit task #726)"
        );

        // (2) `skill.md` edit — MUST reload.
        let skill_md = Path::new("skills/audit/SKILL.md");
        if should_trigger_skill_reload(skill_md) {
            reload_counter += 1;
        }
        assert_eq!(
            reload_counter, 1,
            "touching SKILL.md MUST trigger one skill reload (parity audit task #726)"
        );
    }

    #[test]
    fn excluded_subtrees_blocked_even_for_md_files() {
        // Defense-in-depth: even a `.md` file inside `target/` does NOT
        // reload — that path is build-artifact territory and a watcher
        // event there means a build is in progress.
        for excluded in SKILL_WATCH_EXCLUDED_SUBTREES {
            let path = format!("skills/audit/{excluded}/CHANGELOG.md");
            assert!(
                !should_trigger_skill_reload(Path::new(&path)),
                "{path} must NOT trigger reload (excluded subtree {excluded})"
            );
        }
    }

    #[test]
    fn non_md_extensions_never_reload() {
        // Common build / package / lock / archive artifacts the audit calls out.
        for path in &[
            "skills/audit/build.o",
            "skills/audit/lib.a",
            "skills/audit/lib.so",
            "skills/audit/lib.dylib",
            "skills/audit/lib.dll",
            "skills/audit/Cargo.lock",
            "skills/audit/package-lock.json",
            "skills/audit/yarn.lock",
            "skills/audit/SKILL.txt", // close-miss → still rejected
            "skills/audit/SKILL",     // no extension → rejected
        ] {
            assert!(
                !should_trigger_skill_reload(Path::new(path)),
                "{path} must NOT trigger reload"
            );
        }
    }

    #[test]
    fn md_case_insensitive_triggers_reload() {
        for path in &[
            "skills/audit/SKILL.md",
            "skills/audit/skill.md",
            "skills/audit/Skill.MD",
            "skills/audit/README.md",
            "skills/nested/sub/skill.md",
        ] {
            assert!(
                should_trigger_skill_reload(Path::new(path)),
                "{path} MUST trigger reload"
            );
        }
    }

    #[test]
    fn excluded_subtrees_detected_anywhere_in_path() {
        // Position-independent: the policy fires whether the excluded
        // component is at the root, mid-path, or deep.
        assert!(is_excluded_watch_subtree(Path::new("target")));
        assert!(is_excluded_watch_subtree(Path::new("a/target")));
        assert!(is_excluded_watch_subtree(Path::new("a/b/target/c")));
        assert!(is_excluded_watch_subtree(Path::new("a/node_modules/d")));
        assert!(is_excluded_watch_subtree(Path::new(".git/objects/aa")));
        // Clean paths are NOT excluded — important so the watcher doesn't
        // silently drop the entire skill directory.
        assert!(!is_excluded_watch_subtree(Path::new("skills")));
        assert!(!is_excluded_watch_subtree(Path::new("skills/audit")));
        assert!(!is_excluded_watch_subtree(Path::new(
            "skills/audit/SKILL.md"
        )));
    }

    #[test]
    fn excluded_subtree_partial_name_does_not_match() {
        // `node_modules` matches; `node_modules_backup` does NOT — only
        // exact path-component names are excluded. Guards against
        // overly-aggressive exclusion that would swallow legitimate paths.
        assert!(!is_excluded_watch_subtree(Path::new(
            "skills/audit/node_modules_backup/README.md"
        )));
        assert!(!is_excluded_watch_subtree(Path::new(
            "skills/audit/targeted/SKILL.md"
        )));
        // But a real `node_modules` next to `node_modules_backup` IS
        // excluded.
        assert!(is_excluded_watch_subtree(Path::new(
            "skills/audit/node_modules_backup/node_modules/lodash/SKILL.md"
        )));
    }
}
