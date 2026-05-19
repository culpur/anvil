//! Default-collection-path detector.
//!
//! Picks a smart default for "Path to index for cross-session memory"
//! so the wizard's TextInputModal can pre-fill the most likely answer.

use std::path::{Path, PathBuf};

pub const MIN_MARKDOWN_FILES: usize = 5;
pub const MAX_WALK_DEPTH: usize = 4;

#[must_use]
pub fn pick_default() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    for candidate in default_candidates(&home) {
        if candidate.exists() && count_markdown(&candidate) >= MIN_MARKDOWN_FILES {
            return Some(candidate);
        }
    }
    None
}

#[must_use]
pub fn default_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Documents"),
        home.join("Notes"),
        home.join("projects"),
        home.join("work"),
        home.to_path_buf(),
    ]
}

#[must_use]
pub fn count_markdown(root: &Path) -> usize {
    let mut count = 0usize;
    walk(root, 0, &mut count);
    count
}

fn walk(dir: &Path, depth: usize, count: &mut usize) {
    if *count >= MIN_MARKDOWN_FILES || depth > MAX_WALK_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *count >= MIN_MARKDOWN_FILES {
            return;
        }
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == ".git"
            {
                continue;
            }
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk(&path, depth + 1, count),
            Ok(ft) if ft.is_file() => {
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                {
                    *count += 1;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn default_candidates_starts_with_documents() {
        let home = Path::new("/home/user");
        let c = default_candidates(home);
        assert_eq!(c[0], home.join("Documents"));
        assert_eq!(c[4], home.to_path_buf());
    }

    #[test]
    fn count_markdown_threshold_5_in_flat_dir() {
        let tmp = TempDir::new().unwrap();
        for i in 0..6 {
            fs::write(tmp.path().join(format!("a{i}.md")), b"# x").unwrap();
        }
        assert!(count_markdown(tmp.path()) >= MIN_MARKDOWN_FILES);
    }

    #[test]
    fn count_markdown_below_threshold_under_5() {
        let tmp = TempDir::new().unwrap();
        for i in 0..3 {
            fs::write(tmp.path().join(format!("a{i}.md")), b"# x").unwrap();
        }
        assert!(count_markdown(tmp.path()) < MIN_MARKDOWN_FILES);
    }

    #[test]
    fn count_markdown_recurses_into_subfolders() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("docs").join("inner");
        fs::create_dir_all(&sub).unwrap();
        for i in 0..6 {
            fs::write(sub.join(format!("note{i}.md")), b"# x").unwrap();
        }
        assert!(count_markdown(tmp.path()) >= MIN_MARKDOWN_FILES);
    }

    #[test]
    fn count_markdown_ignores_target_and_dotfiles_and_node_modules() {
        let tmp = TempDir::new().unwrap();
        let blacklist = ["target", "node_modules", ".git", ".obsidian-cache"];
        for hidden in blacklist {
            let d = tmp.path().join(hidden);
            fs::create_dir_all(&d).unwrap();
            for i in 0..6 {
                fs::write(d.join(format!("h{i}.md")), b"# x").unwrap();
            }
        }
        assert!(count_markdown(tmp.path()) < MIN_MARKDOWN_FILES);
    }

    #[test]
    fn count_markdown_extension_is_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        for (i, ext) in ["MD", "Md", "md", "MD", "md", "md"].iter().enumerate() {
            fs::write(tmp.path().join(format!("x{i}.{ext}")), b"# x").unwrap();
        }
        assert!(count_markdown(tmp.path()) >= MIN_MARKDOWN_FILES);
    }
}
