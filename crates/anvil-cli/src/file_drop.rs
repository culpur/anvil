/// File drag-and-drop and image paste support.
///
/// When a user drags a file into the terminal on macOS/Linux the terminal
/// emits the file path as text, optionally wrapped in single quotes.  This
/// module detects those paths, reads the file, and converts it into an
/// appropriate set of content blocks that can be injected into the
/// conversation before the next model turn.
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use runtime::ContentBlock;

// ---------------------------------------------------------------------------
// Size limits
// ---------------------------------------------------------------------------

/// Text files larger than this are truncated before injection.
const TEXT_SIZE_LIMIT: usize = 100 * 1024; // 100 KB

/// Image files larger than this are rejected outright.
const IMAGE_SIZE_LIMIT: usize = 20 * 1024 * 1024; // 20 MB

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// What to do with a detected file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAction {
    /// Read as UTF-8 text and wrap in `<file>` tags.
    Text,
    /// Read as binary, base64-encode, send as an image block.
    Image,
    /// File type we don't handle yet — show info only.
    Unknown,
}

/// Result of processing a dropped / pasted file.
pub struct FileDropResult {
    /// Human-readable status line for TUI / stdout.
    pub notice: String,
    /// Content blocks to inject into the conversation (may be empty on error).
    pub blocks: Vec<ContentBlock>,
}

// ---------------------------------------------------------------------------
// Path detection
// ---------------------------------------------------------------------------

/// If `input` looks like a single file path (or a newline-separated list of
/// paths), return the `PathBuf` values that actually exist on disk.
///
/// Recognises:
/// - Absolute paths  (`/foo/bar`)
/// - Home-relative   (`~/foo/bar`)
/// - Current-dir     (`./foo/bar`)
/// - Single or double-quoted variants produced by some terminals
pub fn detect_file_paths(input: &str) -> Vec<PathBuf> {
    // Try the whole input as one path first, then fall back to per-line.
    let single = try_resolve_path(input.trim());
    if let Some(p) = single {
        return vec![p];
    }

    // Multi-line: each line is a separate candidate.
    let mut found = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(p) = try_resolve_path(trimmed) {
            found.push(p);
        }
    }
    found
}

fn try_resolve_path(raw: &str) -> Option<PathBuf> {
    // Strip surrounding quotes added by some terminals.
    let s = raw
        .trim_matches('\'')
        .trim_matches('"')
        .trim();

    if s.is_empty() {
        return None;
    }

    // Must look like a path prefix we recognise.
    if !s.starts_with('/')
        && !s.starts_with("~/")
        && !s.starts_with("./")
        && !s.starts_with("../")
    {
        return None;
    }

    let expanded = if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(s)
    };

    if expanded.exists() && expanded.is_file() {
        Some(expanded)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// File type classification
// ---------------------------------------------------------------------------

/// Decide how to handle a file based on its extension.
pub fn classify(path: &Path) -> FileAction {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        // Images — send as vision content block.
        "png" | "jpg" | "jpeg" | "gif" | "webp" => FileAction::Image,

        // Text / source / config — inject verbatim (up to size limit).
        "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "md" | "txt" | "json" | "toml"
        | "yml" | "yaml" | "sh" | "bash" | "zsh" | "fish" | "css" | "html" | "xml"
        | "sql" | "go" | "java" | "c" | "cpp" | "h" | "hpp" | "rb" | "php" | "swift"
        | "kt" | "scala" | "r" | "lua" | "vim" | "conf" | "cfg" | "ini" | "env"
        | "dockerfile" | "makefile" | "cmake" | "tf" | "hcl" | "nix" | "proto"
        | "graphql" | "gql" | "diff" | "patch" | "log" | "csv" | "tsv" => FileAction::Text,

        // Special case: files without an extension that look like known names.
        "" => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            match name.as_str() {
                "makefile" | "dockerfile" | "jenkinsfile" | "rakefile"
                | "gemfile" | "procfile" | "vagrantfile" => FileAction::Text,
                _ => FileAction::Unknown,
            }
        }

        _ => FileAction::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------

/// Process a single detected file path into content blocks.
pub fn process_file(path: &Path) -> FileDropResult {
    let display = path.display().to_string();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&display)
        .to_string();

    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) => {
            return FileDropResult {
                notice: format!("Could not read {filename}: {err}"),
                blocks: vec![],
            };
        }
    };
    #[allow(clippy::cast_possible_truncation)]
    let size = metadata.len() as usize;

    match classify(path) {
        FileAction::Image => process_image(path, &filename, size),
        FileAction::Text => process_text(path, &filename, &display, size),
        FileAction::Unknown => FileDropResult {
            notice: format!(
                "File attached: {filename} ({}) — unknown type, showing as text",
                human_size(size)
            ),
            blocks: vec![ContentBlock::Text {
                text: format!(
                    "The user dropped a file: {display}\nSize: {}\nType: unknown\n\
                     (Anvil cannot parse this file type; ask the user what to do with it.)",
                    human_size(size)
                ),
            }],
        },
    }
}

fn process_image(path: &Path, filename: &str, size: usize) -> FileDropResult {
    if size > IMAGE_SIZE_LIMIT {
        return FileDropResult {
            notice: format!(
                "Image {filename} is too large ({} > 20 MB limit) — skipped",
                human_size(size)
            ),
            blocks: vec![],
        };
    }

    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) => {
            return FileDropResult {
                notice: format!("Could not read image {filename}: {err}"),
                blocks: vec![],
            };
        }
    };

    let media_type = image_media_type(path);
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let text_block = ContentBlock::Text {
        text: format!(
            "The user has shared an image file: {filename} ({}). \
             What can you see in this image?",
            human_size(size)
        ),
    };
    let image_block = ContentBlock::Image { media_type, data };

    FileDropResult {
        notice: format!("Image loaded: {filename} ({})", human_size(size)),
        blocks: vec![text_block, image_block],
    }
}

fn process_text(path: &Path, filename: &str, display: &str, size: usize) -> FileDropResult {
    let Ok(raw) = fs::read_to_string(path) else {
        // Not valid UTF-8 — treat as unknown binary.
        return FileDropResult {
            notice: format!(
                "File {filename} ({}) does not appear to be UTF-8 text — skipped",
                human_size(size)
            ),
            blocks: vec![],
        };
    };

    let (content, truncated) = if raw.len() > TEXT_SIZE_LIMIT {
        (
            format!(
                "{}\n\n[... truncated — file is {}; showing first 100 KB ...]",
                &raw[..TEXT_SIZE_LIMIT],
                human_size(size)
            ),
            true,
        )
    } else {
        (raw, false)
    };

    let notice = if truncated {
        format!(
            "File loaded (truncated): {filename} ({} shown of {})",
            human_size(TEXT_SIZE_LIMIT),
            human_size(size)
        )
    } else {
        format!("File loaded: {filename} ({})", human_size(size))
    };

    let text = format!(
        "I'm sharing a file: {display}\n\n\
         <file path=\"{display}\">\n\
         {content}\n\
         </file>\n\n\
         What would you like to do with this file?",
    );

    FileDropResult {
        notice,
        blocks: vec![ContentBlock::Text { text }],
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(clippy::match_same_arms)]
fn image_media_type(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
    .to_string()
}

#[allow(clippy::cast_precision_loss)]
fn human_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
