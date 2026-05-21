use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use plugins::{Plugin, PluginManager, PluginManagerConfig};

use super::normalize_optional_args;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DefinitionSource {
    ProjectCodex,
    ProjectAnvil,
    UserCodexHome,
    UserCodex,
    UserAnvil,
    Bundled,
}

impl DefinitionSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProjectCodex => "Project (.codex)",
            Self::ProjectAnvil => "Project (.anvil)",
            Self::UserCodexHome => "User ($CODEX_HOME)",
            Self::UserCodex => "User (~/.codex)",
            Self::UserAnvil => "User (~/.anvil)",
            Self::Bundled => "Bundled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSummary {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) source: DefinitionSource,
    pub(crate) shadowed_by: Option<DefinitionSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: Option<String>,
    /// Keywords declared in the skill's YAML front-matter `triggers:` list.
    /// Skills without a `triggers:` key default to an empty vec and behave
    /// exactly as before this feature was added (explicit-invoke only).
    pub triggers: Vec<String>,
    pub source: DefinitionSource,
    pub shadowed_by: Option<DefinitionSource>,
    pub origin: SkillOrigin,
    /// Skills this skill chains to on completion (W13 skill-chaining engine).
    pub chains_to: Vec<crate::skill_chaining::ChainEntry>,
    /// Body size in bytes, for token-budget accounting in the chaining engine.
    pub body_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    SkillsDir,
    LegacyCommandsDir,
}

impl SkillOrigin {
    pub const fn detail_label(self) -> Option<&'static str> {
        match self {
            Self::SkillsDir => None,
            Self::LegacyCommandsDir => Some("legacy /commands"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    pub(crate) source: DefinitionSource,
    pub(crate) path: PathBuf,
    pub(crate) origin: SkillOrigin,
}

pub fn handle_agents_slash_command(args: Option<&str>, cwd: &Path) -> std::io::Result<String> {
    match normalize_optional_args(args) {
        None | Some("list") => {
            let roots = discover_definition_roots(cwd, "agents");
            let agents = load_agents_from_roots(&roots)?;
            Ok(render_agents_report(&agents))
        }
        Some("-h" | "--help" | "help") => Ok(render_agents_usage(None)),
        Some(args) => Ok(render_agents_usage(Some(args))),
    }
}

pub fn handle_skills_slash_command(args: Option<&str>, cwd: &Path) -> std::io::Result<String> {
    match normalize_optional_args(args) {
        None | Some("list") => {
            let roots = discover_skill_roots(cwd);
            let mut skills = load_skills_from_roots(&roots)?;
            let active_names: std::collections::HashSet<String> = skills
                .iter()
                .filter(|s| s.shadowed_by.is_none())
                .map(|s| s.name.to_ascii_lowercase())
                .collect();
            for def in bundled_skill_defs() {
                let shadowed_by = if active_names.contains(&def.name.to_ascii_lowercase()) {
                    // Find the source that owns this name
                    skills
                        .iter()
                        .find(|s| {
                            s.name.eq_ignore_ascii_case(def.name) && s.shadowed_by.is_none()
                        })
                        .map(|s| s.source)
                } else {
                    None
                };
                skills.push(SkillSummary {
                    name: def.name.to_string(),
                    description: Some(def.description.to_string()),
                    // Static bundled skill definitions listed here have no
                    // trigger keywords — they are explicit-invoke only.
                    triggers: vec![],
                    source: DefinitionSource::Bundled,
                    shadowed_by,
                    origin: SkillOrigin::SkillsDir,
                    chains_to: vec![],
                    body_bytes: None,
                });
            }
            Ok(render_skills_report(&skills))
        }
        Some("-h" | "--help" | "help") => Ok(render_skills_usage(None)),
        Some(args) => Ok(render_skills_usage(Some(args))),
    }
}

pub(crate) fn discover_definition_roots(
    cwd: &Path,
    leaf: &str,
) -> Vec<(DefinitionSource, PathBuf)> {
    let mut roots = Vec::new();

    for ancestor in cwd.ancestors() {
        push_unique_root(
            &mut roots,
            DefinitionSource::ProjectCodex,
            ancestor.join(".codex").join(leaf),
        );
        push_unique_root(
            &mut roots,
            DefinitionSource::ProjectAnvil,
            ancestor.join(".anvil").join(leaf),
        );
    }

    if let Ok(codex_home) = env::var("CODEX_HOME") {
        push_unique_root(
            &mut roots,
            DefinitionSource::UserCodexHome,
            PathBuf::from(codex_home).join(leaf),
        );
    }

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_root(
            &mut roots,
            DefinitionSource::UserCodex,
            home.join(".codex").join(leaf),
        );
        push_unique_root(
            &mut roots,
            DefinitionSource::UserAnvil,
            home.join(".anvil").join(leaf),
        );
    }

    roots
}

pub fn discover_skill_roots(cwd: &Path) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    for ancestor in cwd.ancestors() {
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectCodex,
            ancestor.join(".codex").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectAnvil,
            ancestor.join(".anvil").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectCodex,
            ancestor.join(".codex").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectAnvil,
            ancestor.join(".anvil").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    if let Ok(codex_home) = env::var("CODEX_HOME") {
        let codex_home = PathBuf::from(codex_home);
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodexHome,
            codex_home.join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodexHome,
            codex_home.join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodex,
            home.join(".codex").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodex,
            home.join(".codex").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserAnvil,
            home.join(".anvil").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserAnvil,
            home.join(".anvil").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    // Defect #5: append skill roots from installed plugins.
    // Each installed plugin whose manifest declares skills has a `root/skills/`
    // directory (or per-skill paths) that the skill loader would otherwise miss.
    append_plugin_skill_roots(&mut roots);

    roots
}

/// Query the active `PluginManager` for installed-plugin skill directories and
/// append them to `roots`.  Best-effort: errors are silently swallowed so a
/// broken plugin registry never prevents the rest of skill discovery.
fn append_plugin_skill_roots(roots: &mut Vec<SkillRoot>) {
    let config_home = if let Ok(h) = env::var("ANVIL_CONFIG_HOME") {
        PathBuf::from(h)
    } else if let Some(h) = env::var_os("HOME") {
        PathBuf::from(h).join(".anvil")
    } else {
        return;
    };

    let config = PluginManagerConfig::new(&config_home);
    let mut mgr = PluginManager::new(config);

    let plugins = match mgr.discover_plugins() {
        Ok(p) => p,
        Err(_) => return,
    };

    for plugin in plugins {
        let Some(root) = plugin.metadata().root.as_ref() else { continue };

        // Primary convention: <plugin-root>/skills/
        let skills_dir = root.join("skills");
        push_unique_skill_root(roots, DefinitionSource::UserAnvil, skills_dir, SkillOrigin::SkillsDir);

        // Per-manifest entries: extract parent dirs of declared skill paths.
        // This handles plugins that put skills in subdirectories other than `skills/`.
        // (We can't directly read the manifest here without re-loading it, so we
        //  rely on the conventional `skills/` directory for now.)
    }
}

fn push_unique_root(
    roots: &mut Vec<(DefinitionSource, PathBuf)>,
    source: DefinitionSource,
    path: PathBuf,
) {
    if path.is_dir() && !roots.iter().any(|(_, existing)| existing == &path) {
        roots.push((source, path));
    }
}

fn push_unique_skill_root(
    roots: &mut Vec<SkillRoot>,
    source: DefinitionSource,
    path: PathBuf,
    origin: SkillOrigin,
) {
    if path.is_dir() && !roots.iter().any(|existing| existing.path == path) {
        roots.push(SkillRoot {
            source,
            path,
            origin,
        });
    }
}

pub(crate) fn load_agents_from_roots(
    roots: &[(DefinitionSource, PathBuf)],
) -> std::io::Result<Vec<AgentSummary>> {
    let mut agents = Vec::new();
    let mut active_sources = BTreeMap::<String, DefinitionSource>::new();

    for (source, root) in roots {
        let mut root_agents = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.path().extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let contents = fs::read_to_string(entry.path())?;
            let fallback_name = entry.path().file_stem().map_or_else(
                || entry.file_name().to_string_lossy().to_string(),
                |stem| stem.to_string_lossy().to_string(),
            );
            root_agents.push(AgentSummary {
                name: parse_toml_string(&contents, "name").unwrap_or(fallback_name),
                description: parse_toml_string(&contents, "description"),
                model: parse_toml_string(&contents, "model"),
                reasoning_effort: parse_toml_string(&contents, "model_reasoning_effort"),
                source: *source,
                shadowed_by: None,
            });
        }
        root_agents.sort_by(|left, right| left.name.cmp(&right.name));

        for mut agent in root_agents {
            let key = agent.name.to_ascii_lowercase();
            if let Some(existing) = active_sources.get(&key) {
                agent.shadowed_by = Some(*existing);
            } else {
                active_sources.insert(key, agent.source);
            }
            agents.push(agent);
        }
    }

    Ok(agents)
}

struct BundledSkillDef {
    name: &'static str,
    description: &'static str,
}

const fn bundled_skill_defs() -> &'static [BundledSkillDef] {
    &[
        BundledSkillDef {
            name: "commit",
            description: "Create a git commit with a well-crafted message based on staged changes",
        },
        BundledSkillDef {
            name: "review-pr",
            description: "Review a pull request, analyzing changes for issues, style, and correctness",
        },
        BundledSkillDef {
            name: "simplify",
            description: "Review changed code for reuse, quality, and efficiency, then fix any issues found",
        },
        BundledSkillDef {
            name: "loop",
            description: "Run a prompt or slash command on a recurring interval",
        },
        BundledSkillDef {
            name: "schedule",
            description: "Create, update, list, or run scheduled remote agents on a cron schedule",
        },
        BundledSkillDef {
            name: "claude-api",
            description: "Help build apps with the Claude API or Anthropic SDK",
        },
    ]
}

pub fn load_skills_from_roots(roots: &[SkillRoot]) -> std::io::Result<Vec<SkillSummary>> {
    let mut skills = Vec::new();
    let mut active_sources = BTreeMap::<String, DefinitionSource>::new();

    for root in roots {
        let mut root_skills = Vec::new();
        for entry in fs::read_dir(&root.path)? {
            let entry = entry?;
            match root.origin {
                SkillOrigin::SkillsDir => {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let skill_path = entry.path().join("SKILL.md");
                    if !skill_path.is_file() {
                        continue;
                    }
                    let contents = fs::read_to_string(skill_path)?;
                    let body_bytes = contents.len();
                    let fm = parse_skill_frontmatter(&contents);
                    root_skills.push(SkillSummary {
                        name: fm.name
                            .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string()),
                        description: fm.description,
                        triggers: fm.triggers,
                        source: root.source,
                        shadowed_by: None,
                        origin: root.origin,
                        chains_to: fm.chains_to,
                        body_bytes: Some(body_bytes),
                    });
                }
                SkillOrigin::LegacyCommandsDir => {
                    let path = entry.path();
                    let markdown_path = if path.is_dir() {
                        let skill_path = path.join("SKILL.md");
                        if !skill_path.is_file() {
                            continue;
                        }
                        skill_path
                    } else if path
                        .extension()
                        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("md"))
                    {
                        path
                    } else {
                        continue;
                    };

                    let contents = fs::read_to_string(&markdown_path)?;
                    let body_bytes = contents.len();
                    let fallback_name = markdown_path.file_stem().map_or_else(
                        || entry.file_name().to_string_lossy().to_string(),
                        |stem| stem.to_string_lossy().to_string(),
                    );
                    let fm = parse_skill_frontmatter(&contents);
                    root_skills.push(SkillSummary {
                        name: fm.name.unwrap_or(fallback_name),
                        description: fm.description,
                        triggers: fm.triggers,
                        source: root.source,
                        shadowed_by: None,
                        origin: root.origin,
                        chains_to: fm.chains_to,
                        body_bytes: Some(body_bytes),
                    });
                }
            }
        }
        root_skills.sort_by(|left, right| left.name.cmp(&right.name));

        for mut skill in root_skills {
            let key = skill.name.to_ascii_lowercase();
            if let Some(existing) = active_sources.get(&key) {
                skill.shadowed_by = Some(*existing);
            } else {
                active_sources.insert(key, skill.source);
            }
            skills.push(skill);
        }
    }

    Ok(skills)
}

fn parse_toml_string(contents: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =");
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(value) = trimmed.strip_prefix(&prefix) else {
            continue;
        };
        let value = value.trim();
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            continue;
        };
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Parsed output of a skill's YAML front-matter block.
pub(crate) struct SkillFrontmatter {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    /// Trigger keywords declared with `triggers: [word, "phrase"]`.
    /// Empty when the key is absent, preserving backwards compatibility.
    pub(crate) triggers: Vec<String>,
    /// Chain entries declared with `chains_to:`.
    pub(crate) chains_to: Vec<crate::skill_chaining::ChainEntry>,
    /// Input slots declared with the `inputs:` block list.  Empty when the
    /// key is absent — existing skills keep working unchanged.
    ///
    /// Consumed by the v2.2.17 skill-chain builder (React Flow canvas,
    /// sub-track D) which renders valid input handles for each node. Until
    /// that wiring lands the field is parsed but unread, hence the explicit
    /// allow.
    #[allow(dead_code)]
    pub(crate) inputs: Vec<SkillSlot>,
    /// Output slots declared with the `outputs:` block list.  Empty when the
    /// key is absent.
    #[allow(dead_code)]
    pub(crate) outputs: Vec<SkillSlot>,
}

/// A single input or output slot on a skill.
///
/// Surfaced by `GET /v1/hub/packages/:slug` so the AnvilHub builder UI can
/// render handles for each side of a skill node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSlot {
    /// Slot identifier, e.g. `user_query`.  Must match `^[a-z][a-z0-9_]*$`
    /// and be at most 32 characters.  Duplicate names within the same list
    /// (inputs or outputs) cause the slot to be skipped.
    pub name: String,
    /// Slot data kind — defaults to `Text` when omitted.
    pub kind: SkillSlotKind,
    /// Optional human-readable description shown in the builder UI.
    pub description: Option<String>,
    /// Whether the slot must be wired before the chain can execute.
    /// Defaults to `true` when omitted.
    pub required: bool,
}

/// The data kind carried by a skill slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSlotKind {
    /// Free-form text (default when `kind:` is omitted).
    Text,
    /// A file path or file content.
    File,
    /// Structured JSON payload.
    Json,
    /// An image (path or inline data).
    Image,
    /// Boolean flag.
    Boolean,
}

impl SkillSlotKind {
    /// Lowercase tag used in YAML and JSON serialisation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::File => "file",
            Self::Json => "json",
            Self::Image => "image",
            Self::Boolean => "boolean",
        }
    }

    /// Parse a `kind:` value.  Unknown / empty values are treated as a
    /// "kind missing" signal (caller substitutes the `Text` default).
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" | "string" | "str" => Some(Self::Text),
            "file" | "path" => Some(Self::File),
            "json" | "object" => Some(Self::Json),
            "image" | "img" => Some(Self::Image),
            "boolean" | "bool" => Some(Self::Boolean),
            _ => None,
        }
    }
}

/// Maximum allowed length of a slot `name:`.
pub(crate) const SKILL_SLOT_NAME_MAX_LEN: usize = 32;

/// Validate that a slot name matches `^[a-z][a-z0-9_]*$` and is within the
/// length budget.  Returns `true` when the name is valid.
pub(crate) fn is_valid_skill_slot_name(name: &str) -> bool {
    if name.is_empty() || name.len() > SKILL_SLOT_NAME_MAX_LEN {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    true
}

/// Parse the YAML front-matter block at the top of a skill markdown file.
///
/// Only the `name:`, `description:`, and `triggers:` keys are extracted;
/// all other keys are ignored.  Skills without a leading `---` fence or
/// without specific keys return empty/`None` defaults — they are valid and
/// continue to behave exactly as before (explicit `/skill-name` only).
pub(crate) fn parse_skill_frontmatter(contents: &str) -> SkillFrontmatter {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return SkillFrontmatter {
            name: None,
            description: None,
            triggers: vec![],
            chains_to: vec![],
            inputs: vec![],
            outputs: vec![],
        };
    }

    // Collect frontmatter body so we can do a second pass for the
    // multi-line inputs/outputs block lists.
    let mut fm_lines: Vec<String> = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        fm_lines.push(line.to_string());
    }

    let mut name = None;
    let mut description = None;
    let mut triggers: Vec<String> = vec![];
    // When true we are inside a `triggers:` block list.
    let mut in_triggers = false;
    let chains_to = crate::skill_chaining::parse_chains_to(contents);

    for line in &fm_lines {
        let trimmed = line.trim();

        // Detect whether this line starts a new top-level key (not indented).
        let is_new_key = !line.starts_with(' ') && !line.starts_with('\t');
        if is_new_key {
            in_triggers = false;
        }

        if let Some(value) = trimmed.strip_prefix("name:") {
            let value = unquote_frontmatter_value(value.trim());
            if !value.is_empty() {
                name = Some(value);
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("description:") {
            let value = unquote_frontmatter_value(value.trim());
            if !value.is_empty() {
                description = Some(value);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("triggers:") {
            // Inline list form: `triggers: [audit, "security review"]`
            let rest = rest.trim();
            if rest.starts_with('[') {
                let inner = rest
                    .trim_start_matches('[')
                    .trim_end_matches(']');
                for item in inner.split(',') {
                    let kw = unquote_frontmatter_value(item.trim());
                    if !kw.is_empty() {
                        triggers.push(kw.to_ascii_lowercase());
                    }
                }
            } else if rest.is_empty() {
                // Block list form: subsequent indented `- item` lines
                in_triggers = true;
            }
            continue;
        }
        if in_triggers {
            if let Some(item) = trimmed.strip_prefix('-') {
                let kw = unquote_frontmatter_value(item.trim());
                if !kw.is_empty() {
                    triggers.push(kw.to_ascii_lowercase());
                }
            }
        }
    }

    let inputs = parse_skill_slot_block(&fm_lines, "inputs");
    let outputs = parse_skill_slot_block(&fm_lines, "outputs");

    SkillFrontmatter {
        name,
        description,
        triggers,
        chains_to,
        inputs,
        outputs,
    }
}

/// Parse a single `inputs:` / `outputs:` block-list section out of the
/// frontmatter line buffer.
///
/// Recognised YAML shape (the only one we support — flow-style `[…]` lists
/// for inputs/outputs would land in a future revision):
///
/// ```yaml
/// inputs:
///   - name: target_url
///     kind: text
///     description: "API endpoint to scan"
///     required: true
///   - name: auth_token
///     required: false
/// ```
///
/// Defaults applied per slot:
///   * `kind:` absent or unrecognised → `Text`
///   * `required:` absent → `true`
///   * `description:` absent → `None`
///
/// Validation:
///   * `name:` must match `^[a-z][a-z0-9_]*$` and be ≤32 chars; invalid
///     names cause the slot to be silently dropped.
///   * Duplicate `name:` values inside the same list cause the later
///     occurrence to be dropped.  Cross-list duplicates (input/output sharing
///     a name) are permitted.
fn parse_skill_slot_block(fm_lines: &[String], key: &str) -> Vec<SkillSlot> {
    // Locate the `key:` line at top-level indentation.
    let header_index = fm_lines.iter().position(|line| {
        !line.starts_with(' ')
            && !line.starts_with('\t')
            && line.trim() == format!("{key}:")
    });
    let Some(start) = header_index else {
        return Vec::new();
    };

    // Walk subsequent indented lines, splitting into per-slot groups on
    // each `- ` marker.
    let mut groups: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in fm_lines.iter().skip(start + 1) {
        // A non-indented line ends the block.
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        if is_top_level && !line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim_start();
        if let Some(after_dash) = trimmed.strip_prefix("- ") {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            current.push(after_dash);
        } else if trimmed == "-" {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            // Empty slot opener — first key/value lands on the next line.
        } else if !trimmed.is_empty() {
            current.push(trimmed);
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }

    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut slots: Vec<SkillSlot> = Vec::new();
    for group in &groups {
        let mut slot_name: Option<String> = None;
        let mut slot_kind: Option<SkillSlotKind> = None;
        let mut slot_desc: Option<String> = None;
        let mut slot_required: Option<bool> = None;

        for raw in group {
            let line = raw.trim();
            if let Some(value) = line.strip_prefix("name:") {
                let value = unquote_frontmatter_value(value.trim());
                if !value.is_empty() {
                    slot_name = Some(value);
                }
            } else if let Some(value) = line.strip_prefix("kind:") {
                let value = unquote_frontmatter_value(value.trim());
                if !value.is_empty() {
                    slot_kind = SkillSlotKind::parse(&value);
                }
            } else if let Some(value) = line.strip_prefix("description:") {
                let value = unquote_frontmatter_value(value.trim());
                if !value.is_empty() {
                    slot_desc = Some(value);
                }
            } else if let Some(value) = line.strip_prefix("required:") {
                let value = unquote_frontmatter_value(value.trim()).to_ascii_lowercase();
                slot_required = match value.as_str() {
                    "true" | "yes" => Some(true),
                    "false" | "no" => Some(false),
                    _ => None,
                };
            }
        }

        let Some(name) = slot_name else {
            // Slot without a name — skip silently; the chain builder UI
            // surfaces a "malformed slot" indicator at lint time.
            continue;
        };
        if !is_valid_skill_slot_name(&name) {
            continue;
        }
        if !seen_names.insert(name.clone()) {
            // Duplicate inside the same list — keep the first occurrence.
            continue;
        }

        slots.push(SkillSlot {
            name,
            kind: slot_kind.unwrap_or(SkillSlotKind::Text),
            description: slot_desc,
            required: slot_required.unwrap_or(true),
        });
    }

    slots
}

/// Public wrapper for `unquote_frontmatter_value` — used by `skill_chaining.rs` (W13).
pub fn unquote_frontmatter_value_pub(value: &str) -> String {
    unquote_frontmatter_value(value)
}

fn unquote_frontmatter_value(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|trimmed| trimmed.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|trimmed| trimmed.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim()
        .to_string()
}

pub(crate) fn render_agents_report(agents: &[AgentSummary]) -> String {
    if agents.is_empty() {
        return "No agents found.".to_string();
    }

    let total_active = agents
        .iter()
        .filter(|agent| agent.shadowed_by.is_none())
        .count();
    let mut lines = vec![
        "Agents".to_string(),
        format!("  {total_active} active agents"),
        String::new(),
    ];

    for source in [
        DefinitionSource::ProjectCodex,
        DefinitionSource::ProjectAnvil,
        DefinitionSource::UserCodexHome,
        DefinitionSource::UserCodex,
        DefinitionSource::UserAnvil,
    ] {
        let group = agents
            .iter()
            .filter(|agent| agent.source == source)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }

        lines.push(format!("{}:", source.label()));
        for agent in group {
            let detail = agent_detail(agent);
            match agent.shadowed_by {
                Some(winner) => lines.push(format!("  (shadowed by {}) {detail}", winner.label())),
                None => lines.push(format!("  {detail}")),
            }
        }
        lines.push(String::new());
    }

    lines.join("\n").trim_end().to_string()
}

fn agent_detail(agent: &AgentSummary) -> String {
    let mut parts = vec![agent.name.clone()];
    if let Some(description) = &agent.description {
        parts.push(description.clone());
    }
    if let Some(model) = &agent.model {
        parts.push(model.clone());
    }
    if let Some(reasoning) = &agent.reasoning_effort {
        parts.push(reasoning.clone());
    }
    parts.join(" · ")
}

pub(crate) fn render_skills_report(skills: &[SkillSummary]) -> String {
    if skills.is_empty() {
        return "No skills found.".to_string();
    }

    let total_active = skills
        .iter()
        .filter(|skill| skill.shadowed_by.is_none())
        .count();
    let mut lines = vec![
        "Skills".to_string(),
        format!("  {total_active} available skills"),
        String::new(),
    ];

    for source in [
        DefinitionSource::ProjectCodex,
        DefinitionSource::ProjectAnvil,
        DefinitionSource::UserCodexHome,
        DefinitionSource::UserCodex,
        DefinitionSource::UserAnvil,
        DefinitionSource::Bundled,
    ] {
        let group = skills
            .iter()
            .filter(|skill| skill.source == source)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }

        lines.push(format!("{}:", source.label()));
        for skill in group {
            let mut parts = vec![skill.name.clone()];
            if let Some(description) = &skill.description {
                parts.push(description.clone());
            }
            if let Some(detail) = skill.origin.detail_label() {
                parts.push(detail.to_string());
            }
            let detail = parts.join(" · ");
            match skill.shadowed_by {
                Some(winner) => lines.push(format!("  (shadowed by {}) {detail}", winner.label())),
                None => lines.push(format!("  {detail}")),
            }
        }
        lines.push(String::new());
    }

    lines.join("\n").trim_end().to_string()
}

fn render_agents_usage(unexpected: Option<&str>) -> String {
    let mut lines = vec![
        "Agents".to_string(),
        "  Usage            /agents".to_string(),
        "  Direct CLI       anvil agents".to_string(),
        "  Sources          .codex/agents, .anvil/agents, $CODEX_HOME/agents".to_string(),
    ];
    if let Some(args) = unexpected {
        lines.push(format!("  Unexpected       {args}"));
    }
    lines.join("\n")
}

fn render_skills_usage(unexpected: Option<&str>) -> String {
    let mut lines = vec![
        "Skills".to_string(),
        "  Usage            /skills".to_string(),
        "  Direct CLI       anvil skills".to_string(),
        "  Sources          .codex/skills, .anvil/skills, legacy /commands".to_string(),
    ];
    if let Some(args) = unexpected {
        lines.push(format!("  Unexpected       {args}"));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Skill body resolution
// ---------------------------------------------------------------------------

/// Embedded bodies for the trigger-matched bundled skills.
const BUNDLED_SKILL_BODIES: &[(&str, &str)] = &[
    (
        "security-audit",
        include_str!("../bundled/skills/security-audit/SKILL.md"),
    ),
    (
        "code-review",
        include_str!("../bundled/skills/code-review/SKILL.md"),
    ),
    (
        "terse",
        include_str!("../bundled/skills/terse/SKILL.md"),
    ),
    // W14 token-economy skills
    (
        "token-economy",
        include_str!("../bundled/skills/token-economy/SKILL.md"),
    ),
    (
        "file-fingerprint",
        include_str!("../bundled/skills/file-fingerprint/SKILL.md"),
    ),
    (
        "command-cache-aware",
        include_str!("../bundled/skills/command-cache-aware/SKILL.md"),
    ),
    (
        "pattern-promote",
        include_str!("../bundled/skills/pattern-promote/SKILL.md"),
    ),
    (
        "cache-budget",
        include_str!("../bundled/skills/cache-budget/SKILL.md"),
    ),
    (
        "anvil-md-curator",
        include_str!("../bundled/skills/anvil-md-curator/SKILL.md"),
    ),
    (
        "silent-cat",
        include_str!("../bundled/skills/silent-cat/SKILL.md"),
    ),
];

/// Task #734 — Memory cohesion I.5 (Procedural tier consolidation).
///
/// Return the `(name, body_len_bytes)` pair for every bundled skill so
/// the `/memory show procedural` handler can enumerate them without
/// exposing the include_str!'d bodies. Crate-private — bundled bodies
/// are still resolved via `load_skill_body` for execution.
#[must_use]
pub(crate) fn bundled_skill_inventory() -> Vec<(&'static str, usize)> {
    BUNDLED_SKILL_BODIES
        .iter()
        .map(|(name, body)| (*name, body.len()))
        .collect()
}

/// Resolve the full body of a named skill.
///
/// Resolution order:
/// 1. Bundled skills embedded at compile time (security-audit, code-review, terse).
/// 2. Skills found on disk via `discover_skill_roots(cwd)`.
///
/// Returns `Ok(body)` on success, `Err(message)` when no such skill is found.
pub fn load_skill_body(name: &str, cwd: &Path) -> Result<String, String> {
    // 1. Bundled skills — fast, no I/O.
    for (bundled_name, body) in BUNDLED_SKILL_BODIES {
        if bundled_name.eq_ignore_ascii_case(name) {
            return Ok((*body).to_string());
        }
    }

    // 2. Disk-discovered skills.
    let roots = discover_skill_roots(cwd);
    for root in &roots {
        let skill_dir = root.path.join(name);
        let skill_md = skill_dir.join("SKILL.md");
        if skill_md.is_file() {
            return fs::read_to_string(&skill_md)
                .map_err(|e| format!("could not read skill file {}: {e}", skill_md.display()));
        }
        // Also check legacy flat .md files.
        let flat_md = root.path.join(format!("{name}.md"));
        if flat_md.is_file() {
            return fs::read_to_string(&flat_md)
                .map_err(|e| format!("could not read skill file {}: {e}", flat_md.display()));
        }
    }

    Err(format!(
        "No such skill '{name}'. Use /skill list to browse installed skills."
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled_body(name: &str) -> Option<&'static str> {
        BUNDLED_SKILL_BODIES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, body)| *body)
    }

    #[test]
    fn bundled_skill_token_economy_present() {
        assert!(
            bundled_body("token-economy").is_some(),
            "token-economy must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    #[test]
    fn bundled_skill_file_fingerprint_present() {
        assert!(
            bundled_body("file-fingerprint").is_some(),
            "file-fingerprint must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    #[test]
    fn bundled_skill_command_cache_aware_present() {
        assert!(
            bundled_body("command-cache-aware").is_some(),
            "command-cache-aware must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    #[test]
    fn bundled_skill_pattern_promote_present() {
        assert!(
            bundled_body("pattern-promote").is_some(),
            "pattern-promote must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    #[test]
    fn bundled_skill_cache_budget_present() {
        assert!(
            bundled_body("cache-budget").is_some(),
            "cache-budget must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    #[test]
    fn bundled_skill_anvil_md_curator_present() {
        assert!(
            bundled_body("anvil-md-curator").is_some(),
            "anvil-md-curator must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    #[test]
    fn bundled_skill_silent_cat_present() {
        assert!(
            bundled_body("silent-cat").is_some(),
            "silent-cat must be registered in BUNDLED_SKILL_BODIES"
        );
    }

    fn assert_valid_frontmatter(name: &str) {
        let body = bundled_body(name)
            .unwrap_or_else(|| panic!("{name} not in BUNDLED_SKILL_BODIES"));
        let fm = parse_skill_frontmatter(body);
        assert!(
            fm.name.is_some(),
            "{name}: frontmatter must have a `name:` field"
        );
        assert!(
            fm.description.is_some(),
            "{name}: frontmatter must have a `description:` field"
        );
        assert!(
            !fm.triggers.is_empty(),
            "{name}: frontmatter must declare at least one trigger"
        );
        let line_count = body.lines().count();
        assert!(
            line_count <= 200,
            "{name}: skill body is {line_count} lines, exceeds 200-line budget"
        );
    }

    #[test]
    fn all_default_skills_have_valid_frontmatter() {
        for name in &[
            "token-economy",
            "file-fingerprint",
            "command-cache-aware",
            "pattern-promote",
            "cache-budget",
            "anvil-md-curator",
            "silent-cat",
        ] {
            assert_valid_frontmatter(name);
        }
    }

    #[test]
    fn chain_references_resolve_token_economy_to_real_skills() {
        let body = bundled_body("token-economy").expect("token-economy must be bundled");
        for chain_target in &["file-fingerprint", "command-cache-aware", "pattern-promote"] {
            assert!(
                body.contains(chain_target),
                "token-economy body must reference chain target '{chain_target}'"
            );
            assert!(
                bundled_body(chain_target).is_some(),
                "chain target '{chain_target}' referenced by token-economy must also be bundled"
            );
        }
    }

    #[test]
    fn bundled_skill_count_is_ten() {
        assert_eq!(
            BUNDLED_SKILL_BODIES.len(),
            10,
            "expected exactly 10 bundled skills, got {}",
            BUNDLED_SKILL_BODIES.len()
        );
    }

    // Defect #5 — plugin skill roots appear in discover_skill_roots.
    //
    // We create a minimal valid plugin in a temp directory, point
    // ANVIL_CONFIG_HOME at it, and verify that the plugin's `skills/`
    // subdirectory appears in the roots returned by `discover_skill_roots`.
    //
    // `append_plugin_skill_roots` is best-effort (errors are swallowed), so we
    // set up enough structure to ensure the plugin is discovered rather than
    // silently skipped.
    #[test]
    fn plugin_skills_dir_appears_in_discover_skill_roots() {
        use std::fs;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_home = tmp.path();

        // Layout: <config_home>/plugins/installed/test-plugin/
        //   plugin.json
        //   skills/my-skill/SKILL.md   <-- must be a directory + SKILL.md
        let install_root = config_home
            .join("plugins")
            .join("installed")
            .join("test-plugin");
        let skills_dir = install_root.join("skills");
        let skill_dir = skills_dir.join("my-skill");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(skill_dir.join("SKILL.md"), "---\nname: my-skill\ndescription: test\ntriggers: [test]\n---\nBody.\n")
            .expect("write SKILL.md");

        // Write minimal plugin manifest (no tools/hooks/commands).
        let manifest_json = r#"{
            "name": "test-plugin",
            "version": "0.1.0",
            "description": "Integration test plugin for defect #5"
        }"#;
        fs::write(install_root.join("plugin.json"), manifest_json)
            .expect("write plugin.json");

        // Also create the bundled root override so sync_bundled_plugins does
        // not try to materialize the embedded binary tree into the temp dir.
        // We point it at an empty directory: no bundled plugins to sync.
        let empty_bundled = config_home.join("plugins").join("bundled");
        fs::create_dir_all(&empty_bundled).expect("create empty bundled root");

        // Override ANVIL_CONFIG_HOME for this test invocation.
        // Note: env::set_var is not test-parallel-safe, but `discover_skill_roots`
        // is fast and this is the established pattern in this test module.
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        // SAFETY: single-threaded test — no other thread reads ANVIL_CONFIG_HOME.
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", config_home) };

        // Use a cwd that has no .anvil/skills of its own so we only get plugin-sourced roots.
        let cwd = tmp.path().join("workspace");
        fs::create_dir_all(&cwd).expect("create workspace dir");

        let roots = discover_skill_roots(&cwd);

        // Restore env before any assertion (so failures don't leave it set).
        match prev {
            Some(v) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") },
        }

        let skills_path = install_root.join("skills");
        assert!(
            roots.iter().any(|r| r.path == skills_path),
            "plugin skills dir {skills_path:?} must appear in discover_skill_roots output; got: {roots:?}",
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Sub-track C — inputs/outputs frontmatter (task #529, v2.2.17 F2-C)
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_skill_with_inputs_outputs() {
        let body = "---\n\
name: vulnapi-scanner\n\
inputs:\n  \
- name: target_url\n    \
kind: text\n    \
description: \"API endpoint to scan\"\n    \
required: true\n  \
- name: auth_token\n    \
kind: text\n    \
required: false\n\
outputs:\n  \
- name: report\n    \
kind: json\n    \
description: \"OWASP findings\"\n\
---\n\
\n\
Body.\n";
        let fm = parse_skill_frontmatter(body);

        assert_eq!(fm.inputs.len(), 2);
        assert_eq!(fm.inputs[0].name, "target_url");
        assert_eq!(fm.inputs[0].kind, SkillSlotKind::Text);
        assert_eq!(
            fm.inputs[0].description.as_deref(),
            Some("API endpoint to scan")
        );
        assert!(fm.inputs[0].required);

        assert_eq!(fm.inputs[1].name, "auth_token");
        assert!(!fm.inputs[1].required);

        assert_eq!(fm.outputs.len(), 1);
        assert_eq!(fm.outputs[0].name, "report");
        assert_eq!(fm.outputs[0].kind, SkillSlotKind::Json);
        assert!(fm.outputs[0].required, "required defaults to true");
    }

    #[test]
    fn parse_skill_missing_inputs_outputs_defaults_empty() {
        let body = "---\nname: legacy\ndescription: existing skill\n---\n\nBody.\n";
        let fm = parse_skill_frontmatter(body);
        assert!(
            fm.inputs.is_empty(),
            "absent inputs: must yield empty vec for backwards-compat"
        );
        assert!(
            fm.outputs.is_empty(),
            "absent outputs: must yield empty vec for backwards-compat"
        );
        assert_eq!(fm.name.as_deref(), Some("legacy"));
    }

    #[test]
    fn parse_skill_rejects_invalid_slot_name() {
        // Names violating `^[a-z][a-z0-9_]*$` or the 32-char budget must
        // be dropped from the parsed slot list.
        let body = "---\n\
name: bad-names\n\
inputs:\n  \
- name: 1starts_with_digit\n    \
kind: text\n  \
- name: Capitalised\n    \
kind: text\n  \
- name: has-hyphen\n    \
kind: text\n  \
- name: way_too_long_a_name_that_exceeds_the_thirty_two_char_budget\n    \
kind: text\n  \
- name: valid_one\n    \
kind: text\n\
---\n";
        let fm = parse_skill_frontmatter(body);
        assert_eq!(
            fm.inputs.len(),
            1,
            "only the single valid slot should survive; got {:?}",
            fm.inputs
        );
        assert_eq!(fm.inputs[0].name, "valid_one");

        // Direct unit-level validation of the helper.
        assert!(is_valid_skill_slot_name("user_query"));
        assert!(is_valid_skill_slot_name("q"));
        assert!(!is_valid_skill_slot_name(""));
        assert!(!is_valid_skill_slot_name("1bad"));
        assert!(!is_valid_skill_slot_name("Bad"));
        assert!(!is_valid_skill_slot_name("has-hyphen"));
        assert!(!is_valid_skill_slot_name("has space"));
    }

    #[test]
    fn parse_skill_rejects_duplicate_input_names() {
        // Two inputs share the name `query`; the second must be dropped.
        // A separate output named `query` is permitted — cross-list
        // duplicates do not collide.
        let body = "---\n\
name: dup-test\n\
inputs:\n  \
- name: query\n    \
kind: text\n  \
- name: query\n    \
kind: file\n  \
- name: other\n    \
kind: text\n\
outputs:\n  \
- name: query\n    \
kind: json\n\
---\n";
        let fm = parse_skill_frontmatter(body);
        assert_eq!(fm.inputs.len(), 2, "duplicate input must be dropped");
        assert_eq!(fm.inputs[0].name, "query");
        assert_eq!(
            fm.inputs[0].kind,
            SkillSlotKind::Text,
            "first occurrence wins"
        );
        assert_eq!(fm.inputs[1].name, "other");

        assert_eq!(fm.outputs.len(), 1, "output `query` is allowed alongside input");
        assert_eq!(fm.outputs[0].kind, SkillSlotKind::Json);
    }

    #[test]
    fn parse_skill_kind_defaults_to_text() {
        let body = "---\n\
name: defaults\n\
inputs:\n  \
- name: q\n    \
description: \"no kind specified\"\n  \
- name: bogus\n    \
kind: not_a_real_kind\n\
---\n";
        let fm = parse_skill_frontmatter(body);
        assert_eq!(fm.inputs.len(), 2);
        assert_eq!(
            fm.inputs[0].kind,
            SkillSlotKind::Text,
            "absent kind: must default to Text"
        );
        assert_eq!(
            fm.inputs[1].kind,
            SkillSlotKind::Text,
            "unknown kind: value must default to Text"
        );
    }

    #[test]
    fn parse_skill_required_defaults_to_true() {
        let body = "---\n\
name: required-defaults\n\
inputs:\n  \
- name: must_have\n    \
kind: text\n  \
- name: optional_flag\n    \
kind: boolean\n    \
required: false\n  \
- name: weird_value\n    \
kind: text\n    \
required: maybe\n\
---\n";
        let fm = parse_skill_frontmatter(body);
        assert_eq!(fm.inputs.len(), 3);
        assert!(
            fm.inputs[0].required,
            "absent required: must default to true"
        );
        assert!(
            !fm.inputs[1].required,
            "explicit required: false must be honoured"
        );
        assert!(
            fm.inputs[2].required,
            "unparseable required: value must fall back to the true default"
        );
    }
}
