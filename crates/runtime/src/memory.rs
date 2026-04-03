use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::default_config_home;

const MAX_INDEX_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }

    fn from_str(s: &str) -> Self {
        match s.trim() {
            "user" => Self::User,
            "feedback" => Self::Feedback,
            "reference" => Self::Reference,
            _ => Self::Project,
        }
    }
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    pub memory_type: MemoryType,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct MemoryManager {
    memory_dir: PathBuf,
}

impl MemoryManager {
    /// Create a `MemoryManager` whose storage directory is derived from the
    /// SHA-256 of the canonical project path.
    ///
    /// Storage path: `~/.anvil/projects/<first-16-hex-chars-of-sha256>/memory/`
    #[must_use]
    pub fn new(project_dir: &Path) -> Self {
        let canonical = project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf());
        let hash = project_path_hash(&canonical);
        let memory_dir = default_config_home()
            .join("projects")
            .join(&hash)
            .join("memory");
        Self { memory_dir }
    }

    /// Create a `MemoryManager` rooted at a specific directory (useful for
    /// tests and alternative config homes).
    #[must_use]
    pub fn with_dir(memory_dir: PathBuf) -> Self {
        Self { memory_dir }
    }

    /// The resolved memory directory path.
    #[must_use]
    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }

    /// Scan the memory directory for `.md` files, parse their YAML frontmatter,
    /// and return all valid memory files.  Files with missing or malformed
    /// frontmatter are silently skipped.
    #[must_use]
    pub fn discover(&self) -> Vec<MemoryFile> {
        let entries = match fs::read_dir(&self.memory_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut files: Vec<MemoryFile> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map_or(false, |ext| ext == "md")
                    && entry.path().file_name().map_or(true, |n| n != "MEMORY.md")
            })
            .filter_map(|entry| {
                let path = entry.path();
                let raw = fs::read_to_string(&path).ok()?;
                parse_memory_file(path, &raw)
            })
            .collect();

        files.sort_by(|a, b| a.name.cmp(&b.name));
        files
    }

    /// Read the MEMORY.md index file (up to `MAX_INDEX_LINES` lines).
    #[must_use]
    pub fn read_index(&self) -> String {
        let path = self.memory_dir.join("MEMORY.md");
        match fs::read_to_string(&path) {
            Ok(content) => content
                .lines()
                .take(MAX_INDEX_LINES)
                .collect::<Vec<_>>()
                .join("\n"),
            Err(_) => String::new(),
        }
    }

    /// Write a memory file with YAML frontmatter and update MEMORY.md.
    pub fn save(
        &self,
        name: &str,
        description: &str,
        memory_type: MemoryType,
        content: &str,
    ) -> std::io::Result<PathBuf> {
        fs::create_dir_all(&self.memory_dir)?;

        let filename = sanitize_filename(name);
        let path = self.memory_dir.join(format!("{filename}.md"));

        let file_content = format!(
            "---\nname: {name}\ndescription: {description}\ntype: {memory_type}\n---\n\n{content}",
        );
        fs::write(&path, file_content)?;

        self.rebuild_index()?;
        Ok(path)
    }

    /// Delete a memory file by name and update MEMORY.md.
    pub fn delete(&self, name: &str) -> std::io::Result<()> {
        let filename = sanitize_filename(name);
        let path = self.memory_dir.join(format!("{filename}.md"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.rebuild_index()
    }

    /// Search memory files by relevance using QMD.
    ///
    /// Queries the QMD index for files under this manager's memory directory
    /// that match `query`.  Falls back to returning all memories when QMD is
    /// unavailable or finds no results above the score threshold.
    #[must_use]
    pub fn search_relevant_memories(
        &self,
        query: &str,
        qmd: &crate::qmd::QmdClient,
    ) -> Vec<MemoryFile> {
        if !qmd.is_enabled() {
            return self.discover();
        }

        let results = qmd.search(query, 10, 0.4);
        if results.is_empty() {
            return self.discover();
        }

        // Build a set of filenames that QMD matched so we can cross-reference
        // against the memory files on disk.
        let matched_names: std::collections::HashSet<String> = results
            .iter()
            .map(|r| {
                // QMD paths look like "some/path/memory/foo.md" — extract the
                // filename stem (without extension) to match against MemoryFile.
                std::path::Path::new(&r.file)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();

        let all = self.discover();
        let relevant: Vec<MemoryFile> = all
            .iter()
            .filter(|m| {
                let stem = sanitize_filename(&m.name);
                matched_names.contains(&stem)
            })
            .cloned()
            .collect();

        // If none of the QMD matches map to known memory files, return all.
        if relevant.is_empty() {
            all
        } else {
            relevant
        }
    }

    /// Format all discovered memory files for injection into the system prompt.
    #[must_use]
    pub fn render_for_prompt(&self) -> String {
        let files = self.discover();
        if files.is_empty() {
            return String::new();
        }

        let mut sections = vec!["# Persistent memory".to_string()];

        let index = self.read_index();
        if !index.is_empty() {
            sections.push(format!("## Memory index\n{index}"));
        }

        for file in &files {
            let type_label = file.memory_type.as_str();
            sections.push(format!(
                "## {} (type: {type_label})\n{}\n\n{}",
                file.name, file.description, file.content
            ));
        }

        sections.join("\n\n")
    }

    /// Rebuild the MEMORY.md index from the current files on disk.
    fn rebuild_index(&self) -> std::io::Result<()> {
        let files = self.discover();
        if files.is_empty() {
            // Remove the index if no memory files remain.
            let index_path = self.memory_dir.join("MEMORY.md");
            match fs::remove_file(&index_path) {
                Ok(()) | Err(_) => {}
            }
            return Ok(());
        }

        let mut lines = vec![
            "# Memory index".to_string(),
            String::new(),
            "| Name | Type | Description |".to_string(),
            "| ---- | ---- | ----------- |".to_string(),
        ];
        for file in &files {
            lines.push(format!(
                "| {} | {} | {} |",
                file.name,
                file.memory_type.as_str(),
                file.description
            ));
        }

        let index_path = self.memory_dir.join("MEMORY.md");
        fs::write(index_path, lines.join("\n"))
    }
}

/// Compute the first 16 hex characters of the SHA-256 of the canonical path.
#[must_use]
pub fn project_path_hash(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    hex_encode(&result[..8])
}

/// Return the memory directory for a project, given its directory, without
/// constructing a full `MemoryManager`.  Useful in tools and other crates that
/// only need the path.
#[must_use]
pub fn memory_dir_for_project(project_dir: &Path) -> PathBuf {
    MemoryManager::new(project_dir).memory_dir().to_path_buf()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Parse a memory `.md` file.  Returns `None` if the file lacks valid
/// frontmatter.
fn parse_memory_file(path: PathBuf, raw: &str) -> Option<MemoryFile> {
    let rest = raw.trim_start();

    if !rest.starts_with("---") {
        return None;
    }

    // Find the closing `---` marker.
    let after_open = rest.trim_start_matches('-').trim_start_matches('\n');
    // Re-locate so we skip exactly the first `---\n` line.
    let open_end = rest.find('\n')?;
    let after_open_line = &rest[open_end + 1..];
    let close_pos = after_open_line.find("\n---")?;
    let frontmatter = &after_open_line[..close_pos];
    let body_start = close_pos + "\n---".len();
    let body = after_open_line[body_start..].trim_start_matches('\n');

    let _ = after_open; // suppress unused warning

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut memory_type = MemoryType::Project;

    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("type:") {
            memory_type = MemoryType::from_str(value.trim());
        }
    }

    Some(MemoryFile {
        path,
        name: name?,
        description: description.unwrap_or_default(),
        memory_type,
        content: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_memory_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time is after epoch")
            .as_nanos();
        let seq = TEST_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
        let dir = std::env::temp_dir().join(format!("anvil-memory-test-{nanos}-{seq}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn project_path_hash_is_16_hex_chars() {
        let hash = project_path_hash(Path::new("/tmp/myproject"));
        assert_eq!(hash.len(), 16, "hash should be 16 hex chars: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn project_path_hash_is_stable() {
        let h1 = project_path_hash(Path::new("/tmp/myproject"));
        let h2 = project_path_hash(Path::new("/tmp/myproject"));
        assert_eq!(h1, h2);
    }

    #[test]
    fn project_path_hash_differs_for_different_paths() {
        let h1 = project_path_hash(Path::new("/tmp/project-a"));
        let h2 = project_path_hash(Path::new("/tmp/project-b"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn saves_and_discovers_memory_file() {
        let dir = temp_memory_dir();
        let mgr = MemoryManager::with_dir(dir.clone());

        let path = mgr
            .save("test-note", "A test note", MemoryType::Project, "Some content here.")
            .expect("save should succeed");

        assert!(path.exists(), "written file should exist");

        let files = mgr.discover();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "test-note");
        assert_eq!(files[0].description, "A test note");
        assert_eq!(files[0].memory_type, MemoryType::Project);
        assert!(files[0].content.contains("Some content here."));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saves_and_deletes_memory_file() {
        let dir = temp_memory_dir();
        let mgr = MemoryManager::with_dir(dir.clone());

        mgr.save("to-delete", "Ephemeral", MemoryType::Feedback, "gone soon")
            .expect("save should succeed");
        assert_eq!(mgr.discover().len(), 1);

        mgr.delete("to-delete").expect("delete should succeed");
        assert_eq!(mgr.discover().len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebuilds_memory_index_after_save() {
        let dir = temp_memory_dir();
        let mgr = MemoryManager::with_dir(dir.clone());

        mgr.save("alpha", "Alpha note", MemoryType::User, "alpha content")
            .expect("save alpha");
        mgr.save("beta", "Beta note", MemoryType::Reference, "beta content")
            .expect("save beta");

        let index = mgr.read_index();
        assert!(index.contains("alpha"), "index should contain alpha: {index}");
        assert!(index.contains("beta"), "index should contain beta: {index}");
        assert!(index.contains("user"), "index should contain type user: {index}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_for_prompt_is_empty_with_no_files() {
        let dir = temp_memory_dir();
        let mgr = MemoryManager::with_dir(dir.clone());
        assert!(mgr.render_for_prompt().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_for_prompt_includes_name_type_and_content() {
        let dir = temp_memory_dir();
        let mgr = MemoryManager::with_dir(dir.clone());

        mgr.save(
            "workflow-notes",
            "Dev workflow reminders",
            MemoryType::Feedback,
            "Always run tests before commit.",
        )
        .expect("save");

        let rendered = mgr.render_for_prompt();
        assert!(rendered.contains("# Persistent memory"));
        assert!(rendered.contains("workflow-notes"));
        assert!(rendered.contains("feedback"));
        assert!(rendered.contains("Always run tests before commit."));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_memory_file_rejects_missing_frontmatter() {
        let path = PathBuf::from("/fake/no-frontmatter.md");
        let raw = "Just plain content, no frontmatter.";
        assert!(parse_memory_file(path, raw).is_none());
    }

    #[test]
    fn parse_memory_file_rejects_missing_name() {
        let path = PathBuf::from("/fake/no-name.md");
        let raw = "---\ndescription: some desc\ntype: project\n---\n\ncontent here";
        assert!(parse_memory_file(path, raw).is_none());
    }

    #[test]
    fn parse_memory_file_parses_all_types() {
        for (type_str, expected) in [
            ("user", MemoryType::User),
            ("feedback", MemoryType::Feedback),
            ("project", MemoryType::Project),
            ("reference", MemoryType::Reference),
            ("unknown", MemoryType::Project),
        ] {
            let raw = format!("---\nname: test\ndescription: d\ntype: {type_str}\n---\n\nbody");
            let file = parse_memory_file(PathBuf::from("/fake/test.md"), &raw)
                .expect("should parse");
            assert_eq!(file.memory_type, expected, "type_str={type_str}");
        }
    }

    #[test]
    fn memory_dir_for_project_returns_path_under_config_home() {
        let dir = memory_dir_for_project(Path::new("/tmp/some-project"));
        let config_home = default_config_home();
        assert!(
            dir.starts_with(&config_home),
            "memory dir {dir:?} should be under config home {config_home:?}"
        );
        assert!(dir.ends_with("memory"));
    }

    #[test]
    fn sanitize_filename_replaces_special_chars() {
        assert_eq!(sanitize_filename("My Note!"), "my-note-");
        assert_eq!(sanitize_filename("valid-name_123"), "valid-name_123");
        assert_eq!(sanitize_filename("spaces here"), "spaces-here");
    }
}
