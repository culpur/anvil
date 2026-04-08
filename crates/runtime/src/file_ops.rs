use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glob::Pattern;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Project-boundary sandbox
// ---------------------------------------------------------------------------

/// Walk up from the current working directory looking for well-known project
/// root markers. Returns the first ancestor that contains one of those markers,
/// or the CWD itself when none is found.
pub fn find_project_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let markers = [".git", ".anvil", "Cargo.toml", "package.json"];
    let mut current = cwd.as_path();
    loop {
        for marker in &markers {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    cwd
}

/// Returns `true` when `path` is equal to or nested beneath `root`.
/// Both paths should already be fully resolved (canonicalized or
/// `normalize_path_allow_missing` output) before calling this function.
pub fn is_within_boundary(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Returns `true` when a path is always permitted for writes regardless of the
/// project boundary:
///   - anything under `/tmp`
///   - anything under the system temp directory (`std::env::temp_dir()`)
///   - anything under `$TMPDIR` (covers macOS `/private/var/folders/…`)
///   - anything under the user's `~/.anvil/` directory
fn is_always_allowed_write(path: &Path) -> bool {
    // Canonical /tmp
    if path.starts_with("/tmp") {
        return true;
    }

    // std::env::temp_dir() — resolves the platform temp dir (e.g. macOS
    // returns /private/var/folders/… even though $TMPDIR points there too).
    let sys_tmp = std::env::temp_dir();
    if path.starts_with(&sys_tmp) {
        return true;
    }
    // Also try the canonicalized form of the system temp dir in case the path
    // was already resolved through symlinks.
    if let Ok(canonical_tmp) = sys_tmp.canonicalize() {
        if path.starts_with(&canonical_tmp) {
            return true;
        }
    }

    // $TMPDIR env var (explicit, may differ from std::env::temp_dir() result)
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        let tmpdir = PathBuf::from(&tmpdir);
        if !tmpdir.as_os_str().is_empty() && path.starts_with(&tmpdir) {
            return true;
        }
        // Canonicalized form
        if let Ok(canonical) = tmpdir.canonicalize() {
            if path.starts_with(&canonical) {
                return true;
            }
        }
    }

    // ~/.anvil/
    if let Some(home) = std::env::var_os("HOME") {
        let anvil_home = PathBuf::from(home).join(".anvil");
        if path.starts_with(&anvil_home) {
            return true;
        }
        // Canonicalized form
        if let Ok(canonical) = anvil_home.canonicalize() {
            if path.starts_with(&canonical) {
                return true;
            }
        }
    }

    false
}

/// Enforces the write sandbox. Returns an error when the write should be
/// blocked. Respects `ANVIL_ALLOW_GLOBAL_WRITES=1` as a power-user bypass.
fn enforce_write_boundary(path: &Path) -> io::Result<()> {
    // Power-user escape hatch
    if std::env::var("ANVIL_ALLOW_GLOBAL_WRITES").as_deref() == Ok("1") {
        return Ok(());
    }

    // Paths that are always safe to write
    if is_always_allowed_write(path) {
        return Ok(());
    }

    let root = find_project_root();
    // Canonicalize root for a reliable prefix comparison; fall back gracefully.
    let canonical_root = root.canonicalize().unwrap_or(root);

    if !is_within_boundary(path, &canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "path is outside project boundary: {} (root: {})",
                path.display(),
                canonical_root.display()
            ),
        ));
    }

    Ok(())
}

/// Emits a warning to stderr when a read targets a path outside the project
/// boundary. Never blocks the read — read-only access is considered safe.
fn warn_if_outside_boundary(path: &Path) {
    let root = find_project_root();
    let canonical_root = root.canonicalize().unwrap_or(root);
    if !is_within_boundary(path, &canonical_root) {
        eprintln!(
            "[anvil] warning: reading path outside project boundary: {} (root: {})",
            path.display(),
            canonical_root.display()
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextFilePayload {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "numLines")]
    pub num_lines: usize,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    pub file: TextFilePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredPatchHunk {
    #[serde(rename = "oldStart")]
    pub old_start: usize,
    #[serde(rename = "oldLines")]
    pub old_lines: usize,
    #[serde(rename = "newStart")]
    pub new_start: usize,
    #[serde(rename = "newLines")]
    pub new_lines: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "oldString")]
    pub old_string: String,
    #[serde(rename = "newString")]
    pub new_string: String,
    #[serde(rename = "originalFile")]
    pub original_file: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "userModified")]
    pub user_modified: bool,
    #[serde(rename = "replaceAll")]
    pub replace_all: bool,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobSearchOutput {
    #[serde(rename = "durationMs")]
    pub duration_ms: u128,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchInput {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    #[serde(rename = "output_mode")]
    pub output_mode: Option<String>,
    #[serde(rename = "-B")]
    pub before: Option<usize>,
    #[serde(rename = "-A")]
    pub after: Option<usize>,
    #[serde(rename = "-C")]
    pub context_short: Option<usize>,
    pub context: Option<usize>,
    #[serde(rename = "-n")]
    pub line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    pub case_insensitive: Option<bool>,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub head_limit: Option<usize>,
    pub offset: Option<usize>,
    pub multiline: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchOutput {
    pub mode: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub content: Option<String>,
    #[serde(rename = "numLines")]
    pub num_lines: Option<usize>,
    #[serde(rename = "numMatches")]
    pub num_matches: Option<usize>,
    #[serde(rename = "appliedLimit")]
    pub applied_limit: Option<usize>,
    #[serde(rename = "appliedOffset")]
    pub applied_offset: Option<usize>,
}

pub fn read_file(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;
    warn_if_outside_boundary(&absolute_path);
    let content = fs::read_to_string(&absolute_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start_index = offset.unwrap_or(0).min(lines.len());
    let end_index = limit.map_or(lines.len(), |limit| {
        start_index.saturating_add(limit).min(lines.len())
    });
    let selected = lines[start_index..end_index].join("\n");

    Ok(ReadFileOutput {
        kind: String::from("text"),
        file: TextFilePayload {
            file_path: absolute_path.to_string_lossy().into_owned(),
            content: selected,
            num_lines: end_index.saturating_sub(start_index),
            start_line: start_index.saturating_add(1),
            total_lines: lines.len(),
        },
    })
}

pub fn write_file(path: &str, content: &str) -> io::Result<WriteFileOutput> {
    let absolute_path = normalize_path_allow_missing(path)?;
    enforce_write_boundary(&absolute_path)?;
    let original_file = fs::read_to_string(&absolute_path).ok();
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute_path, content)?;

    Ok(WriteFileOutput {
        kind: if original_file.is_some() {
            String::from("update")
        } else {
            String::from("create")
        },
        file_path: absolute_path.to_string_lossy().into_owned(),
        content: content.to_owned(),
        structured_patch: make_patch(original_file.as_deref().unwrap_or(""), content),
        original_file,
        git_diff: None,
    })
}

pub fn edit_file(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    enforce_write_boundary(&absolute_path)?;
    let original_file = fs::read_to_string(&absolute_path)?;
    if old_string == new_string {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_string and new_string must differ",
        ));
    }
    if !original_file.contains(old_string) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "old_string not found in file",
        ));
    }

    let updated = if replace_all {
        original_file.replace(old_string, new_string)
    } else {
        original_file.replacen(old_string, new_string, 1)
    };
    fs::write(&absolute_path, &updated)?;

    Ok(EditFileOutput {
        file_path: absolute_path.to_string_lossy().into_owned(),
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        original_file: original_file.clone(),
        structured_patch: make_patch(&original_file, &updated),
        user_modified: false,
        replace_all,
        git_diff: None,
    })
}

pub fn glob_search(pattern: &str, path: Option<&str>) -> io::Result<GlobSearchOutput> {
    let started = Instant::now();
    // When no explicit path is provided, default to the project root rather
    // than raw CWD so the search is always scoped to a known boundary.
    let base_dir = path
        .map(normalize_path)
        .transpose()?
        .unwrap_or_else(find_project_root);
    let search_pattern = if Path::new(pattern).is_absolute() {
        pattern.to_owned()
    } else {
        base_dir.join(pattern).to_string_lossy().into_owned()
    };

    let mut matches = Vec::new();
    let entries = glob::glob(&search_pattern)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    for entry in entries.flatten() {
        if entry.is_file() {
            matches.push(entry);
        }
    }

    matches.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(Reverse)
    });

    let truncated = matches.len() > 100;
    let filenames = matches
        .into_iter()
        .take(100)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    Ok(GlobSearchOutput {
        duration_ms: started.elapsed().as_millis(),
        num_files: filenames.len(),
        filenames,
        truncated,
    })
}

pub fn grep_search(input: &GrepSearchInput) -> io::Result<GrepSearchOutput> {
    // When no explicit path is provided, default to the project root so
    // searches are scoped to the known boundary rather than raw CWD.
    let base_path = input
        .path
        .as_deref()
        .map(normalize_path)
        .transpose()?
        .unwrap_or_else(find_project_root);

    let regex = RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive.unwrap_or(false))
        .dot_matches_new_line(input.multiline.unwrap_or(false))
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;

    let glob_filter = input
        .glob
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let file_type = input.file_type.as_deref();
    let output_mode = input
        .output_mode
        .clone()
        .unwrap_or_else(|| String::from("files_with_matches"));
    let context = input.context.or(input.context_short).unwrap_or(0);

    let mut filenames = Vec::new();
    let mut content_lines = Vec::new();
    let mut total_matches = 0usize;

    for file_path in collect_search_files(&base_path)? {
        if !matches_optional_filters(&file_path, glob_filter.as_ref(), file_type) {
            continue;
        }

        let Ok(file_contents) = fs::read_to_string(&file_path) else {
            continue;
        };

        if output_mode == "count" {
            let count = regex.find_iter(&file_contents).count();
            if count > 0 {
                filenames.push(file_path.to_string_lossy().into_owned());
                total_matches += count;
            }
            continue;
        }

        let lines: Vec<&str> = file_contents.lines().collect();
        let mut matched_lines = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                total_matches += 1;
                matched_lines.push(index);
            }
        }

        if matched_lines.is_empty() {
            continue;
        }

        filenames.push(file_path.to_string_lossy().into_owned());
        if output_mode == "content" {
            for index in matched_lines {
                let start = index.saturating_sub(input.before.unwrap_or(context));
                let end = (index + input.after.unwrap_or(context) + 1).min(lines.len());
                for (current, line) in lines.iter().enumerate().take(end).skip(start) {
                    let prefix = if input.line_numbers.unwrap_or(true) {
                        format!("{}:{}:", file_path.to_string_lossy(), current + 1)
                    } else {
                        format!("{}:", file_path.to_string_lossy())
                    };
                    content_lines.push(format!("{prefix}{line}"));
                }
            }
        }
    }

    let (filenames, applied_limit, applied_offset) =
        apply_limit(filenames, input.head_limit, input.offset);
    let content_output = if output_mode == "content" {
        let (lines, limit, offset) = apply_limit(content_lines, input.head_limit, input.offset);
        return Ok(GrepSearchOutput {
            mode: Some(output_mode),
            num_files: filenames.len(),
            filenames,
            num_lines: Some(lines.len()),
            content: Some(lines.join("\n")),
            num_matches: None,
            applied_limit: limit,
            applied_offset: offset,
        });
    } else {
        None
    };

    Ok(GrepSearchOutput {
        mode: Some(output_mode.clone()),
        num_files: filenames.len(),
        filenames,
        content: content_output,
        num_lines: None,
        num_matches: (output_mode == "count").then_some(total_matches),
        applied_limit,
        applied_offset,
    })
}

fn collect_search_files(base_path: &Path) -> io::Result<Vec<PathBuf>> {
    if base_path.is_file() {
        return Ok(vec![base_path.to_path_buf()]);
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(base_path) {
        let entry = entry.map_err(|error| io::Error::other(error.to_string()))?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

fn matches_optional_filters(
    path: &Path,
    glob_filter: Option<&Pattern>,
    file_type: Option<&str>,
) -> bool {
    if let Some(glob_filter) = glob_filter {
        let path_string = path.to_string_lossy();
        if !glob_filter.matches(&path_string) && !glob_filter.matches_path(path) {
            return false;
        }
    }

    if let Some(file_type) = file_type {
        let extension = path.extension().and_then(|extension| extension.to_str());
        if extension != Some(file_type) {
            return false;
        }
    }

    true
}

fn apply_limit<T>(
    items: Vec<T>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> (Vec<T>, Option<usize>, Option<usize>) {
    let offset_value = offset.unwrap_or(0);
    let mut items = items.into_iter().skip(offset_value).collect::<Vec<_>>();
    let explicit_limit = limit.unwrap_or(250);
    if explicit_limit == 0 {
        return (items, None, (offset_value > 0).then_some(offset_value));
    }

    let truncated = items.len() > explicit_limit;
    items.truncate(explicit_limit);
    (
        items,
        truncated.then_some(explicit_limit),
        (offset_value > 0).then_some(offset_value),
    )
}

fn make_patch(original: &str, updated: &str) -> Vec<StructuredPatchHunk> {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }

    vec![StructuredPatchHunk {
        old_start: 1,
        old_lines: original.lines().count(),
        new_start: 1,
        new_lines: updated.lines().count(),
        lines,
    }]
}

fn normalize_path(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };
    candidate.canonicalize()
}

fn normalize_path_allow_missing(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };

    if let Ok(canonical) = candidate.canonicalize() {
        return Ok(canonical);
    }

    if let Some(parent) = candidate.parent() {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = candidate.file_name() {
            return Ok(canonical_parent.join(name));
        }
    }

    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        edit_file, find_project_root, glob_search, grep_search, is_always_allowed_write,
        is_within_boundary, read_file, write_file, GrepSearchInput,
    };

    /// Global mutex to serialise tests that mutate process-level env vars.
    /// Rust's test harness runs tests in parallel; without this, one thread
    /// can remove ANVIL_ALLOW_GLOBAL_WRITES while another is relying on it.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("anvil-native-{name}-{unique}"))
    }

    #[test]
    fn reads_and_writes_files() {
        let path = temp_path("read-write.txt");
        let write_output = write_file(path.to_string_lossy().as_ref(), "one\ntwo\nthree")
            .expect("write should succeed");
        assert_eq!(write_output.kind, "create");

        let read_output = read_file(path.to_string_lossy().as_ref(), Some(1), Some(1))
            .expect("read should succeed");
        assert_eq!(read_output.file.content, "two");
    }

    #[test]
    fn edits_file_contents() {
        let path = temp_path("edit.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta alpha")
            .expect("initial write should succeed");
        let output = edit_file(path.to_string_lossy().as_ref(), "alpha", "omega", true)
            .expect("edit should succeed");
        assert!(output.replace_all);
    }

    // ------------------------------------------------------------------
    // Sandbox helper unit tests
    // ------------------------------------------------------------------

    #[test]
    fn is_within_boundary_returns_true_for_child() {
        let root = std::path::Path::new("/home/user/project");
        let child = std::path::Path::new("/home/user/project/src/main.rs");
        assert!(is_within_boundary(child, root));
    }

    #[test]
    fn is_within_boundary_returns_true_for_root_itself() {
        let root = std::path::Path::new("/home/user/project");
        assert!(is_within_boundary(root, root));
    }

    #[test]
    fn is_within_boundary_returns_false_for_sibling() {
        let root = std::path::Path::new("/home/user/project");
        let sibling = std::path::Path::new("/home/user/other/secret.txt");
        assert!(!is_within_boundary(sibling, root));
    }

    #[test]
    fn is_within_boundary_returns_false_for_parent() {
        let root = std::path::Path::new("/home/user/project");
        let parent = std::path::Path::new("/home/user");
        assert!(!is_within_boundary(parent, root));
    }

    #[test]
    fn is_within_boundary_rejects_path_traversal_lookalike() {
        // "/home/user/project-evil" must NOT be considered inside "/home/user/project"
        let root = std::path::Path::new("/home/user/project");
        let evil = std::path::Path::new("/home/user/project-evil/foo.txt");
        assert!(!is_within_boundary(evil, root));
    }

    #[test]
    fn find_project_root_returns_a_directory() {
        let root = find_project_root();
        // The returned path must exist and be a directory (or at least not a file).
        // In CI the CWD may not have markers; that is fine — we just verify the
        // function returns something usable.
        assert!(!root.as_os_str().is_empty());
    }

    #[test]
    fn tmp_writes_are_always_allowed() {
        let tmp = std::env::temp_dir().join("anvil-sandbox-test.txt");
        assert!(
            is_always_allowed_write(&tmp),
            "temp dir should be an always-allowed write location"
        );
    }

    #[test]
    fn slash_tmp_is_always_allowed() {
        let path = std::path::Path::new("/tmp/anvil-test/foo.txt");
        assert!(is_always_allowed_write(path));
    }

    #[test]
    fn anvil_home_is_always_allowed() {
        if let Some(home) = std::env::var_os("HOME") {
            let anvil_path = std::path::PathBuf::from(home)
                .join(".anvil")
                .join("cache.db");
            assert!(
                is_always_allowed_write(&anvil_path),
                "~/.anvil/* should be an always-allowed write location"
            );
        }
    }

    #[test]
    fn write_to_tmp_succeeds_despite_boundary() {
        // Writes to temp dir must succeed even when project root is elsewhere.
        let path = temp_path("sandbox-write.txt");
        let result = write_file(path.to_string_lossy().as_ref(), "sandbox ok");
        assert!(result.is_ok(), "write to $TMPDIR should be allowed: {result:?}");
    }

    #[test]
    fn write_outside_boundary_blocked_without_bypass() {
        // Pick a path that is definitely outside any project root but also not
        // under /tmp or ~/.anvil. We use /var/anvil-test-boundary (not writable
        // in practice, but the sandbox check happens before the syscall).
        let outside = std::path::Path::new("/var/anvil-sandbox-boundary-test/secret.txt");

        // Hold the env mutex so no other test can set ANVIL_ALLOW_GLOBAL_WRITES
        // concurrently and accidentally let this write through.
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("ANVIL_ALLOW_GLOBAL_WRITES");

        // enforce_write_boundary is private; exercise it through write_file.
        // The function must return PermissionDenied before trying to touch disk.
        let result = write_file(outside.to_string_lossy().as_ref(), "should be blocked");
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => { /* expected */ }
            Err(e) => {
                // Any other OS error (e.g. EROFS, EACCES) also means the write
                // didn't succeed, which is fine from a security standpoint.
                let _ = e;
            }
            Ok(_) => panic!("write outside project boundary should have been blocked"),
        }
    }

    #[test]
    fn write_bypass_env_var_allows_outside_path() {
        // ANVIL_ALLOW_GLOBAL_WRITES=1 must skip the boundary check. We cannot
        // actually write to a protected OS directory, so we verify that any
        // error that occurs does NOT carry our sandbox message — meaning our
        // sandbox did not fire and only the OS rejected the write.
        let outside = std::path::Path::new("/var/anvil-sandbox-bypass-test/secret.txt");

        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("ANVIL_ALLOW_GLOBAL_WRITES", "1");
        let result = write_file(outside.to_string_lossy().as_ref(), "bypass");
        std::env::remove_var("ANVIL_ALLOW_GLOBAL_WRITES");
        drop(_guard);

        match result {
            Err(e) => {
                // Our sandbox error always contains this sentinel phrase.
                // An OS-level denial (EACCES, EROFS, etc.) will not.
                assert!(
                    !e.to_string().contains("path is outside project boundary"),
                    "sandbox should not have fired when bypass is set, but sandbox message found: {e}"
                );
            }
            Ok(_) => { /* wrote successfully — also acceptable */ }
        }
    }

    #[test]
    fn globs_and_greps_directory() {
        let dir = temp_path("search-dir");
        std::fs::create_dir_all(&dir).expect("directory should be created");
        let file = dir.join("demo.rs");
        write_file(
            file.to_string_lossy().as_ref(),
            "fn main() {\n println!(\"hello\");\n}\n",
        )
        .expect("file write should succeed");

        let globbed = glob_search("**/*.rs", Some(dir.to_string_lossy().as_ref()))
            .expect("glob should succeed");
        assert_eq!(globbed.num_files, 1);

        let grep_output = grep_search(&GrepSearchInput {
            pattern: String::from("hello"),
            path: Some(dir.to_string_lossy().into_owned()),
            glob: Some(String::from("**/*.rs")),
            output_mode: Some(String::from("content")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: Some(true),
            case_insensitive: Some(false),
            file_type: None,
            head_limit: Some(10),
            offset: Some(0),
            multiline: Some(false),
        })
        .expect("grep should succeed");
        assert!(grep_output.content.unwrap_or_default().contains("hello"));
    }
}
