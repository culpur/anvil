use std::path::PathBuf;

// ─── Built-in style ───────────────────────────────────────────────────────────

/// The two hard-wired response styles shipped with Anvil.
///
/// Policy (2026-04-22):
/// - `Precise` is ALWAYS the default. A user who has never set `output_style`
///   gets `Precise`.
/// - `Condensed` is strictly opt-in; it is never auto-applied from trigger
///   keywords or heuristics.
/// - This axis is orthogonal to `/skill load terse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuiltInStyle {
    /// Natural model voice — no extra instructions prepended. (default)
    #[default]
    Precise,
    /// Token-economical mode — the bundled `terse` skill body is prepended to
    /// the system prompt for every turn. Auto-Clarity rules still apply.
    Condensed,
}

impl BuiltInStyle {
    /// The canonical lowercase string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Precise => "precise",
            Self::Condensed => "condensed",
        }
    }

    /// Parse from a string, case-insensitive. Returns `None` for unknown values.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "precise" => Some(Self::Precise),
            "condensed" => Some(Self::Condensed),
            _ => None,
        }
    }
}

// ─── Custom style ─────────────────────────────────────────────────────────────

/// A user-defined output style loaded from `~/.anvil/output-styles/<name>.md`.
///
/// File format:
/// ```text
/// ---
/// name: Tutor
/// description: Explanatory style with code commentary
/// ---
///
/// You are a patient teacher. After every code block, explain what changed in
/// 1-2 sentences. Never use jargon without first defining it.
/// ```
///
/// The YAML frontmatter `name` and `description` fields are required.  Files
/// missing either field are silently skipped (bad files must not crash the
/// registry build).  The body is the system-prompt fragment prepended before
/// the user turn when this style is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomStyle {
    /// The canonical identifier (taken from frontmatter, not the filename).
    pub name: String,
    /// One-line description shown in `/output-style list`.
    pub description: String,
    /// Raw system-prompt fragment; prepended to the model's system prompt.
    pub prompt_fragment: String,
}

// ─── Unified OutputStyle ──────────────────────────────────────────────────────

/// User-selectable response style — either a built-in variant or a custom
/// user-defined style loaded from `~/.anvil/output-styles/`.
///
/// When a custom style name matches a built-in name (e.g. the user creates
/// `~/.anvil/output-styles/precise.md`), the user file wins for the session
/// but the built-in semantic is replaced by the file's `prompt_fragment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputStyle {
    /// One of the two hard-wired styles.
    BuiltIn(BuiltInStyle),
    /// A user-defined style loaded from disk.
    Custom(CustomStyle),
}

impl Default for OutputStyle {
    fn default() -> Self {
        Self::BuiltIn(BuiltInStyle::default())
    }
}

impl OutputStyle {
    /// The name string used in config and `/output-style` feedback.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::BuiltIn(b) => b.as_str(),
            Self::Custom(c) => &c.name,
        }
    }

    /// Returns `true` for `BuiltIn(Precise)`.
    #[must_use]
    pub fn is_precise(&self) -> bool {
        matches!(self, Self::BuiltIn(BuiltInStyle::Precise))
    }

    /// Returns `true` for `BuiltIn(Condensed)`.
    #[must_use]
    pub fn is_condensed(&self) -> bool {
        matches!(self, Self::BuiltIn(BuiltInStyle::Condensed))
    }

    /// The system-prompt fragment to inject, or `None` for built-ins.
    ///
    /// Built-in styles handle their own injection via caller-side logic
    /// (Condensed injects the terse skill body; Precise injects nothing).
    #[must_use]
    pub fn prompt_fragment(&self) -> Option<&str> {
        match self {
            Self::BuiltIn(_) => None,
            Self::Custom(c) => Some(&c.prompt_fragment),
        }
    }
}

// ─── OutputStyleRegistry ──────────────────────────────────────────────────────

/// Holds all available output styles: built-ins plus any user-defined styles
/// discovered from `~/.anvil/output-styles/`.
///
/// Construction is cheap.  User files are loaded lazily via
/// [`OutputStyleRegistry::ensure_loaded`]; the result is cached for the session.
#[derive(Debug, Default)]
pub struct OutputStyleRegistry {
    user_styles: Vec<CustomStyle>,
    loaded: bool,
}

impl OutputStyleRegistry {
    /// Create a new, empty registry.  User styles are not yet loaded.
    #[must_use]
    pub fn new() -> Self {
        Self {
            user_styles: Vec::new(),
            loaded: false,
        }
    }

    /// Load (or reload) custom styles from `styles_dir`
    /// (typically `~/.anvil/output-styles/`).
    ///
    /// Files with missing `name` or `description` frontmatter are logged to
    /// stderr and skipped.  A name collision with a built-in is allowed — the
    /// user file takes precedence when that name is activated.
    pub fn load_user_styles(&mut self, styles_dir: &std::path::Path) {
        self.user_styles.clear();
        self.loaded = true;

        let entries = match std::fs::read_dir(styles_dir) {
            Ok(e) => e,
            Err(_) => return, // directory absent — no custom styles
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match parse_style_file(&path) {
                Ok(style) => self.user_styles.push(style),
                Err(e) => {
                    eprintln!(
                        "anvil: skipping output style file {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }

    /// Ensure user styles have been loaded from `styles_dir`.
    /// Idempotent — only loads once per instance.
    pub fn ensure_loaded(&mut self, styles_dir: &std::path::Path) {
        if !self.loaded {
            self.load_user_styles(styles_dir);
        }
    }

    /// All available style names (built-ins first, then user styles).
    /// Also includes control tokens `list` and `reset`.
    #[must_use]
    pub fn all_names(&self) -> Vec<String> {
        let user_names: std::collections::HashSet<&str> =
            self.user_styles.iter().map(|s| s.name.as_str()).collect();

        let mut names: Vec<String> = Vec::new();

        for builtin in [BuiltInStyle::Precise, BuiltInStyle::Condensed] {
            if !user_names.contains(builtin.as_str()) {
                names.push(builtin.as_str().to_string());
            }
        }
        for style in &self.user_styles {
            names.push(style.name.clone());
        }
        names.push("list".to_string());
        names.push("reset".to_string());
        names
    }

    /// Resolve a style name to an `OutputStyle`.
    ///
    /// Lookup order:
    ///   1. User styles (case-insensitive match)
    ///   2. Built-in styles
    ///   3. `None` — unknown name
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<OutputStyle> {
        let lower = name.to_ascii_lowercase();
        if let Some(user) = self
            .user_styles
            .iter()
            .find(|s| s.name.to_ascii_lowercase() == lower)
        {
            return Some(OutputStyle::Custom(user.clone()));
        }
        BuiltInStyle::from_str(name).map(OutputStyle::BuiltIn)
    }

    /// Format a human-readable list of all styles for `/output-style list`.
    #[must_use]
    pub fn list_display(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push("Built-in styles:".to_string());
        for builtin in [BuiltInStyle::Precise, BuiltInStyle::Condensed] {
            let shadowed = self
                .user_styles
                .iter()
                .any(|s| s.name.to_ascii_lowercase() == builtin.as_str());
            if shadowed {
                lines.push(format!("  {} (shadowed by user style)", builtin.as_str()));
            } else {
                let desc = match builtin {
                    BuiltInStyle::Precise => "natural model voice, no extra instructions (default)",
                    BuiltInStyle::Condensed => "token-economical terse rules, Auto-Clarity active",
                };
                lines.push(format!("  {}  —  {}", builtin.as_str(), desc));
            }
        }
        if !self.user_styles.is_empty() {
            lines.push(String::new());
            lines.push("User styles (~/.anvil/output-styles/):".to_string());
            for style in &self.user_styles {
                lines.push(format!("  {}  —  {}", style.name, style.description));
            }
        }
        lines.join("\n")
    }

    /// Immutable access to loaded user styles.
    #[must_use]
    pub fn user_styles(&self) -> &[CustomStyle] {
        &self.user_styles
    }
}

// ─── File parser ──────────────────────────────────────────────────────────────

/// Parse a `~/.anvil/output-styles/<name>.md` file into a [`CustomStyle`].
///
/// Returns `Err` if frontmatter is absent or `name`/`description` are missing.
fn parse_style_file(path: &std::path::Path) -> Result<CustomStyle, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read error: {e}"))?;

    let content_trimmed = content.trim_start();

    let after_open = content_trimmed
        .strip_prefix("---")
        .ok_or("missing opening '---' frontmatter delimiter")?;
    let after_open = after_open.trim_start_matches('\n').trim_start_matches('\r');

    let close_pos = after_open
        .find("\n---")
        .ok_or("missing closing '---' frontmatter delimiter")?;

    let frontmatter = &after_open[..close_pos];
    let body_raw = &after_open[close_pos + 4..]; // skip "\n---"

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = Some(rest.trim().to_string());
        }
    }

    let name = name
        .filter(|s| !s.is_empty())
        .ok_or("frontmatter missing required 'name' field")?;

    let description = description
        .filter(|s| !s.is_empty())
        .ok_or("frontmatter missing required 'description' field")?;

    let prompt_fragment = body_raw
        .trim_start_matches('\n')
        .trim_start_matches('\r')
        .to_string();

    Ok(CustomStyle { name, description, prompt_fragment })
}

// ─── Public helpers ───────────────────────────────────────────────────────────

/// Return the default output-styles directory: `~/.anvil/output-styles/`.
#[must_use]
pub fn default_output_styles_dir() -> PathBuf {
    super::default_config_home().join("output-styles")
}

/// Parse an `OutputStyle` name from `config.json`.
///
/// Only built-in names are resolved here (no disk I/O).  Custom style names
/// stored in config.json fall back to `Precise` at startup; the registry
/// resolves them at activation time.
#[must_use]
pub fn output_style_from_str_builtin_only(s: &str) -> OutputStyle {
    BuiltInStyle::from_str(s)
        .map(OutputStyle::BuiltIn)
        .unwrap_or_default()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = CTR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("output-style-test-{ns}-{id}"))
    }

    #[test]
    fn default_is_precise() {
        assert_eq!(
            OutputStyle::default(),
            OutputStyle::BuiltIn(BuiltInStyle::Precise)
        );
    }

    #[test]
    fn builtin_styles_present() {
        assert_eq!(BuiltInStyle::from_str("precise"), Some(BuiltInStyle::Precise));
        assert_eq!(BuiltInStyle::from_str("condensed"), Some(BuiltInStyle::Condensed));
        assert_eq!(BuiltInStyle::from_str("PRECISE"), Some(BuiltInStyle::Precise));
        assert_eq!(BuiltInStyle::from_str("unknown"), None);
    }

    #[test]
    fn is_precise_and_is_condensed_helpers() {
        let p = OutputStyle::BuiltIn(BuiltInStyle::Precise);
        let c = OutputStyle::BuiltIn(BuiltInStyle::Condensed);
        assert!(p.is_precise());
        assert!(!p.is_condensed());
        assert!(c.is_condensed());
        assert!(!c.is_precise());
    }

    #[test]
    fn precise_has_no_prompt_fragment() {
        let p = OutputStyle::BuiltIn(BuiltInStyle::Precise);
        assert_eq!(p.prompt_fragment(), None);
    }

    #[test]
    fn condensed_has_no_prompt_fragment() {
        // Condensed fragment is injected by caller via TERSE_SKILL_BODY path.
        let c = OutputStyle::BuiltIn(BuiltInStyle::Condensed);
        assert_eq!(c.prompt_fragment(), None);
    }

    #[test]
    fn user_styles_load_from_dir() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("tutor.md"),
            "---\nname: Tutor\ndescription: Explanatory teaching style\n---\n\nYou are a patient teacher.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.load_user_styles(&dir);

        let styles = registry.user_styles();
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0].name, "Tutor");
        assert_eq!(styles[0].description, "Explanatory teaching style");
        assert_eq!(styles[0].prompt_fragment, "You are a patient teacher.\n");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn user_styles_load_from_dir_skips_non_md() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("style.txt"), "not a style file").unwrap();
        fs::write(
            dir.join("real.md"),
            "---\nname: Real\ndescription: A real style\n---\n\nBody here.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.load_user_styles(&dir);

        assert_eq!(registry.user_styles().len(), 1);
        assert_eq!(registry.user_styles()[0].name, "Real");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn name_collision_user_wins() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("precise.md"),
            "---\nname: precise\ndescription: Custom precise override\n---\n\nCustom precise body.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.load_user_styles(&dir);

        let resolved = registry.resolve("precise");
        assert!(resolved.is_some());
        match resolved.unwrap() {
            OutputStyle::Custom(c) => {
                assert_eq!(c.name, "precise");
                assert_eq!(c.description, "Custom precise override");
                assert_eq!(c.prompt_fragment, "Custom precise body.\n");
            }
            OutputStyle::BuiltIn(_) => panic!("user style should win over built-in"),
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_style_falls_back_to_default() {
        let registry = OutputStyleRegistry::new();
        assert!(registry.resolve("nonexistent").is_none());
        assert_eq!(OutputStyle::default(), OutputStyle::BuiltIn(BuiltInStyle::Precise));
    }

    #[test]
    fn invalid_frontmatter_missing_name_skipped() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("bad.md"),
            "---\ndescription: No name here\n---\n\nBody.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.load_user_styles(&dir);

        assert_eq!(registry.user_styles().len(), 0, "bad file should be skipped");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_frontmatter_missing_description_skipped() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("bad.md"),
            "---\nname: NoDesc\n---\n\nBody.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.load_user_styles(&dir);

        assert_eq!(registry.user_styles().len(), 0, "bad file should be skipped");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn all_names_includes_builtins_and_user() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("verbose.md"),
            "---\nname: Verbose\ndescription: Very detailed\n---\n\nBe verbose.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.load_user_styles(&dir);

        let names = registry.all_names();
        assert!(names.contains(&"precise".to_string()));
        assert!(names.contains(&"condensed".to_string()));
        assert!(names.contains(&"Verbose".to_string()));
        assert!(names.contains(&"list".to_string()));
        assert!(names.contains(&"reset".to_string()));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prompt_fragment_appears_in_custom_style() {
        let style = OutputStyle::Custom(CustomStyle {
            name: "Teacher".to_string(),
            description: "Teaching style".to_string(),
            prompt_fragment: "Always explain with examples.".to_string(),
        });
        assert_eq!(
            style.prompt_fragment(),
            Some("Always explain with examples.")
        );
        assert!(!style.is_precise());
        assert!(!style.is_condensed());
    }

    #[test]
    fn ensure_loaded_is_idempotent() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("a.md"),
            "---\nname: Alpha\ndescription: First\n---\n\nContent.\n",
        ).unwrap();

        let mut registry = OutputStyleRegistry::new();
        registry.ensure_loaded(&dir);
        let count_first = registry.user_styles().len();

        // Write another file after first load — ensure_loaded should NOT reload.
        fs::write(
            dir.join("b.md"),
            "---\nname: Beta\ndescription: Second\n---\n\nContent2.\n",
        ).unwrap();

        registry.ensure_loaded(&dir);
        let count_second = registry.user_styles().len();

        assert_eq!(count_first, count_second, "ensure_loaded must be idempotent");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_style_from_str_builtin_only_roundtrip() {
        let p = output_style_from_str_builtin_only("precise");
        assert_eq!(p, OutputStyle::BuiltIn(BuiltInStyle::Precise));

        let c = output_style_from_str_builtin_only("condensed");
        assert_eq!(c, OutputStyle::BuiltIn(BuiltInStyle::Condensed));

        // Unknown name → default (Precise)
        let u = output_style_from_str_builtin_only("unknown-custom");
        assert_eq!(u, OutputStyle::default());
    }
}
