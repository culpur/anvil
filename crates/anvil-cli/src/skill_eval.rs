// skill_eval.rs — `anvil skill-eval` subcommand implementation.
//
// Wires the three-arm eval engine from `compat_harness::skill_evals` into the
// CLI.  Does not touch the engine itself — only parses arguments, loads prompts,
// resolves skill paths, formats output, and calls `run_evals_with_caller`.

use std::path::{Path, PathBuf};

use compat_harness::skill_evals::{
    ArmCaller, EvalConfig, EvalReport, AnvilProviderCaller, run_evals_with_caller,
};

// ─── Argument types ───────────────────────────────────────────────────────────

/// Parsed arguments for `anvil skill-eval`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillEvalArgs {
    /// Path to a SKILL.md file, or a bundled skill name like `terse`.
    pub skill: String,
    /// Path to a prompts file (one per line) or a directory of `.txt` files.
    pub prompts: String,
    /// Model identifier, e.g. `claude-sonnet-4-6` or `qwen3:8b`.
    pub model: String,
    /// Provider override. When `None`, resolved automatically from the model name.
    pub provider: Option<String>,
    /// Directory where JSON snapshots land. Defaults to `./skill-evals`.
    pub snapshot_dir: PathBuf,
    /// Summary output format. Defaults to `EvalOutputFormat::Markdown` (precise).
    pub output: EvalOutputFormat,
}

/// Summary output style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvalOutputFormat {
    /// Full multi-line table (default).
    Markdown,
    /// Single-line condensed summary.
    Json,
}

impl EvalOutputFormat {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "markdown" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "unsupported value for --output: {other} (expected markdown or json)"
            )),
        }
    }
}

// ─── Argument parser ──────────────────────────────────────────────────────────

pub(crate) const USAGE: &str = "\
Usage: anvil skill-eval --skill <path|name> --prompts <file|dir> --model <name> [OPTIONS]

Options:
  --skill <path|name>      Path to SKILL.md, or a bundled skill name (e.g. terse)
  --prompts <file|dir>     File with one prompt per line, or directory of .txt files
  --model <name>           Model to evaluate (e.g. claude-sonnet-4-6, qwen3:8b)
  --provider <name>        anthropic / openai / ollama / xai / google (optional; auto-detected)
  --snapshot-dir <dir>     Directory for JSON snapshots (default: ./skill-evals)
  --output <format>        markdown (default) or json
";

/// Parse `skill-eval` sub-arguments (the slice after `skill-eval` is consumed).
pub(crate) fn parse_skill_eval_args(args: &[String]) -> Result<SkillEvalArgs, String> {
    let mut skill: Option<String> = None;
    let mut prompts: Option<String> = None;
    let mut model: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut snapshot_dir: Option<PathBuf> = None;
    let mut output = EvalOutputFormat::Markdown;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--help" | "-h" => {
                return Err(USAGE.to_string());
            }
            "--skill" => {
                skill = Some(
                    args.get(idx + 1)
                        .ok_or("--skill requires a value")?
                        .clone(),
                );
                idx += 2;
            }
            flag if flag.starts_with("--skill=") => {
                skill = Some(flag[8..].to_string());
                idx += 1;
            }
            "--prompts" => {
                prompts = Some(
                    args.get(idx + 1)
                        .ok_or("--prompts requires a value")?
                        .clone(),
                );
                idx += 2;
            }
            flag if flag.starts_with("--prompts=") => {
                prompts = Some(flag[10..].to_string());
                idx += 1;
            }
            "--model" => {
                model = Some(
                    args.get(idx + 1)
                        .ok_or("--model requires a value")?
                        .clone(),
                );
                idx += 2;
            }
            flag if flag.starts_with("--model=") => {
                model = Some(flag[8..].to_string());
                idx += 1;
            }
            "--provider" => {
                provider = Some(
                    args.get(idx + 1)
                        .ok_or("--provider requires a value")?
                        .clone(),
                );
                idx += 2;
            }
            flag if flag.starts_with("--provider=") => {
                provider = Some(flag[11..].to_string());
                idx += 1;
            }
            "--snapshot-dir" => {
                snapshot_dir = Some(PathBuf::from(
                    args.get(idx + 1)
                        .ok_or("--snapshot-dir requires a value")?,
                ));
                idx += 2;
            }
            flag if flag.starts_with("--snapshot-dir=") => {
                snapshot_dir = Some(PathBuf::from(&flag[15..]));
                idx += 1;
            }
            "--output" => {
                let val = args.get(idx + 1).ok_or("--output requires a value")?;
                output = EvalOutputFormat::parse(val)?;
                idx += 2;
            }
            flag if flag.starts_with("--output=") => {
                output = EvalOutputFormat::parse(&flag[9..])?;
                idx += 1;
            }
            other => {
                return Err(format!(
                    "unknown skill-eval flag: {other}\n\n{USAGE}"
                ));
            }
        }
    }

    let skill = skill.ok_or_else(|| {
        format!("--skill is required\n\n{USAGE}")
    })?;
    let prompts = prompts.ok_or_else(|| {
        format!("--prompts is required\n\n{USAGE}")
    })?;
    let model = model.ok_or_else(|| {
        format!("--model is required\n\n{USAGE}")
    })?;

    Ok(SkillEvalArgs {
        skill,
        prompts,
        model,
        provider,
        snapshot_dir: snapshot_dir.unwrap_or_else(|| PathBuf::from("./skill-evals")),
        output,
    })
}

// ─── Skill path resolver ──────────────────────────────────────────────────────

/// Resolve the skill argument to an absolute `PathBuf`.
///
/// If `skill` is an absolute path, use it directly.
/// Otherwise treat it as a bundled skill name and look for
/// `<workspace>/crates/commands/bundled/skills/<name>/SKILL.md`.
pub(crate) fn resolve_skill_path(skill: &str) -> Result<PathBuf, String> {
    let p = Path::new(skill);

    // Absolute path — use as-is.
    if p.is_absolute() {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(format!("skill file not found: {skill}"));
    }

    // Relative path that actually exists in the CWD.
    if p.exists() {
        return Ok(p.to_path_buf());
    }

    // Bundled skill resolution.
    // Walk up from the current binary location (or CWD) looking for the workspace.
    let bundled = find_bundled_skill(skill)?;
    if bundled.exists() {
        return Ok(bundled);
    }
    Err(format!(
        "bundled skill '{skill}' not found (tried {}) — use an absolute path or check the skill name",
        bundled.display()
    ))
}

/// Find the workspace root by looking for `Cargo.toml` with `[workspace]` content,
/// then return `<root>/crates/commands/bundled/skills/<name>/SKILL.md`.
fn find_bundled_skill(name: &str) -> Result<PathBuf, String> {
    // Try CARGO_MANIFEST_DIR env var first (set during `cargo run` / tests).
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let root = PathBuf::from(&manifest_dir);
        // CARGO_MANIFEST_DIR is the crate dir; workspace is two levels up
        // (crates/anvil-cli → workspace root).
        let candidate = root
            .join("../..")
            .join("crates/commands/bundled/skills")
            .join(name)
            .join("SKILL.md");
        let canonical = candidate
            .canonicalize()
            .unwrap_or(candidate.clone());
        return Ok(canonical);
    }

    // Walk from CWD upward until we find a Cargo.toml containing "[workspace]".
    let cwd = std::env::current_dir()
        .map_err(|e| format!("cannot determine CWD: {e}"))?;
    for ancestor in cwd.ancestors() {
        let toml = ancestor.join("Cargo.toml");
        if toml.is_file() {
            if let Ok(content) = std::fs::read_to_string(&toml) {
                if content.contains("[workspace]") {
                    let candidate = ancestor
                        .join("crates/commands/bundled/skills")
                        .join(name)
                        .join("SKILL.md");
                    return Ok(candidate);
                }
            }
        }
    }

    // Last resort: return a relative path for a reasonable error message.
    Ok(PathBuf::from(format!(
        "crates/commands/bundled/skills/{name}/SKILL.md"
    )))
}

// ─── Prompt loader ────────────────────────────────────────────────────────────

/// Load prompts from a file (one per line) or a directory of `.txt` files.
///
/// Trailing blank lines are trimmed. Empty lines within the file are skipped.
/// Directory `.txt` files are sorted alphabetically; each file contributes one
/// prompt (its trimmed full contents).
pub(crate) fn load_prompts(path_str: &str) -> Result<Vec<String>, String> {
    let path = Path::new(path_str);

    if !path.exists() {
        return Err(format!("prompts path not found: {path_str}"));
    }

    if path.is_dir() {
        return load_prompts_from_dir(path);
    }

    load_prompts_from_file(path)
}

fn load_prompts_from_file(path: &Path) -> Result<Vec<String>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read prompts file {}: {e}", path.display()))?;

    let prompts: Vec<String> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    if prompts.is_empty() {
        return Err(format!(
            "prompts file {} contains no non-empty lines",
            path.display()
        ));
    }
    Ok(prompts)
}

fn load_prompts_from_dir(dir: &Path) -> Result<Vec<String>, String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read prompts directory {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map_or(false, |ext| ext.eq_ignore_ascii_case("txt"))
        })
        .collect();

    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        return Err(format!(
            "prompts directory {} contains no .txt files",
            dir.display()
        ));
    }

    let mut prompts = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            prompts.push(trimmed);
        }
    }

    if prompts.is_empty() {
        return Err(format!(
            "prompts directory {} contains only empty .txt files",
            dir.display()
        ));
    }
    Ok(prompts)
}

// ─── Provider resolution ──────────────────────────────────────────────────────

/// Resolve the provider string from a model name using the same logic as the
/// `api` crate's `detect_provider_kind`.
pub(crate) fn resolve_provider_from_model(model: &str) -> String {
    use api::{detect_provider_kind, ProviderKind};
    match detect_provider_kind(model) {
        ProviderKind::AnvilApi => "anthropic".to_string(),
        ProviderKind::OpenAi => "openai".to_string(),
        ProviderKind::Xai => "xai".to_string(),
        ProviderKind::Gemini => "google".to_string(),
        ProviderKind::Ollama => "ollama".to_string(),
        other => {
            // For all extended providers, use the display name lowercased as slug
            api::provider_display_name(other).to_lowercase().replace(' ', "-")
        }
    }
}

// ─── Summary formatters ───────────────────────────────────────────────────────

/// Format a full multi-line (precise) summary.
pub(crate) fn format_precise_summary(report: &EvalReport, snapshot_dir: &Path) -> String {
    let s = &report.summary;
    let skill_vs_terse = s.skill_vs_terse_delta_pct;
    let baseline_vs_terse_pct = if s.baseline_avg_tokens == 0.0 {
        0.0
    } else {
        (s.terse_avg_tokens - s.baseline_avg_tokens) / s.baseline_avg_tokens * 100.0
    };
    let skill_vs_baseline_pct = if s.baseline_avg_tokens == 0.0 {
        0.0
    } else {
        (s.skill_avg_tokens - s.baseline_avg_tokens) / s.baseline_avg_tokens * 100.0
    };

    let snap_path = snapshot_dir
        .join(&report.model)
        .join(&report.skill_name);

    let mut out = String::new();

    out.push_str(&format!(
        "Skill evaluation — {} × {}\n",
        report.skill_name, report.model
    ));
    out.push_str(&format!(
        "  baseline avg  : {:>4} tokens\n",
        s.baseline_avg_tokens.round() as u64
    ));
    out.push_str(&format!(
        "  terse avg     : {:>4} tokens ({:+.1}% vs baseline)\n",
        s.terse_avg_tokens.round() as u64,
        baseline_vs_terse_pct
    ));
    out.push_str(&format!(
        "  skill avg     : {:>4} tokens ({:+.1}% vs terse, {:+.1}% vs baseline)\n",
        s.skill_avg_tokens.round() as u64,
        skill_vs_terse,
        skill_vs_baseline_pct
    ));
    out.push('\n');
    out.push_str("  Honest caveats:\n");
    for caveat in &s.honest_caveats {
        // Wrap each caveat at ~80 columns for readability, indented 4 spaces.
        for line in wrap_caveat(caveat, 76) {
            out.push_str(&format!("    * {line}\n"));
        }
    }
    out.push('\n');
    out.push_str(&format!("  Snapshots: {}/*.json\n", snap_path.display()));
    out
}

/// Format a single-line condensed summary.
pub(crate) fn format_condensed_summary(report: &EvalReport) -> String {
    let s = &report.summary;
    let skill_vs_terse = s.skill_vs_terse_delta_pct;

    // Condensed line
    let main_line = format!(
        "eval: {} × {} — baseline {} / terse {} / skill {} tokens ({:+.1}% vs terse)",
        report.skill_name,
        report.model,
        s.baseline_avg_tokens.round() as u64,
        s.terse_avg_tokens.round() as u64,
        s.skill_avg_tokens.round() as u64,
        skill_vs_terse,
    );

    // Caveats are always appended, even in condensed mode.
    let mut out = main_line;
    out.push('\n');
    out.push_str("  Honest caveats:\n");
    for caveat in &s.honest_caveats {
        for line in wrap_caveat(caveat, 76) {
            out.push_str(&format!("    * {line}\n"));
        }
    }
    out
}

fn wrap_caveat(text: &str, max_width: usize) -> Vec<String> {
    if text.len() <= max_width {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= max_width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

// ─── Runner ───────────────────────────────────────────────────────────────────

/// Entry point called from `main.rs` for `CliAction::SkillEval`.
///
/// Uses `AnvilProviderCaller` for production runs.
pub(crate) fn run_skill_eval(args: SkillEvalArgs) -> Result<(), Box<dyn std::error::Error>> {
    run_skill_eval_with_caller(args, &AnvilProviderCaller)
}

/// Inner runner that accepts an `ArmCaller` for testability.
pub(crate) fn run_skill_eval_with_caller(
    args: SkillEvalArgs,
    caller: &dyn ArmCaller,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Resolve skill path.
    let skill_path = resolve_skill_path(&args.skill)?;

    // 2. Load prompts.
    let prompts = load_prompts(&args.prompts)?;

    // 3. Resolve provider.
    let provider = args
        .provider
        .unwrap_or_else(|| resolve_provider_from_model(&args.model));

    // 4. Build EvalConfig.
    let cfg = EvalConfig {
        skill_path,
        prompts,
        model: args.model,
        provider,
        snapshot_dir: args.snapshot_dir.clone(),
    };

    // 5. Run with tokio.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime error: {e}"))?;

    let report = rt.block_on(run_evals_with_caller(&cfg, caller))?;

    // 6. Print summary.
    let summary = match args.output {
        EvalOutputFormat::Markdown => {
            format_precise_summary(&report, &args.snapshot_dir)
        }
        EvalOutputFormat::Json => {
            format_condensed_summary(&report)
        }
    };
    print!("{summary}");

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── arg parser tests ─────────────────────────────────────────────────────

    #[test]
    fn parses_minimum_required_args() {
        let a = parse_skill_eval_args(&args(&[
            "--skill", "terse",
            "--prompts", "prompts.txt",
            "--model", "qwen3:8b",
        ]))
        .expect("should parse");
        assert_eq!(a.skill, "terse");
        assert_eq!(a.prompts, "prompts.txt");
        assert_eq!(a.model, "qwen3:8b");
        assert_eq!(a.provider, None);
        assert_eq!(a.snapshot_dir, PathBuf::from("./skill-evals"));
        assert_eq!(a.output, EvalOutputFormat::Markdown);
    }

    #[test]
    fn parses_all_optional_args() {
        let a = parse_skill_eval_args(&args(&[
            "--skill", "terse",
            "--prompts", "prompts.txt",
            "--model", "claude-sonnet-4-6",
            "--provider", "anthropic",
            "--snapshot-dir", "/tmp/snaps",
            "--output", "json",
        ]))
        .expect("should parse");
        assert_eq!(a.provider, Some(s("anthropic")));
        assert_eq!(a.snapshot_dir, PathBuf::from("/tmp/snaps"));
        assert_eq!(a.output, EvalOutputFormat::Json);
    }

    #[test]
    fn missing_skill_returns_error() {
        let err = parse_skill_eval_args(&args(&[
            "--prompts", "prompts.txt",
            "--model", "claude-sonnet-4-6",
        ]))
        .unwrap_err();
        assert!(
            err.contains("--skill is required"),
            "expected '--skill is required' in: {err}"
        );
    }

    #[test]
    fn missing_prompts_returns_error() {
        let err = parse_skill_eval_args(&args(&[
            "--skill", "terse",
            "--model", "claude-sonnet-4-6",
        ]))
        .unwrap_err();
        assert!(
            err.contains("--prompts is required"),
            "expected '--prompts is required' in: {err}"
        );
    }

    #[test]
    fn missing_model_returns_error() {
        let err = parse_skill_eval_args(&args(&[
            "--skill", "terse",
            "--prompts", "prompts.txt",
        ]))
        .unwrap_err();
        assert!(
            err.contains("--model is required"),
            "expected '--model is required' in: {err}"
        );
    }

    #[test]
    fn unknown_output_value_returns_error() {
        let err = parse_skill_eval_args(&args(&[
            "--skill", "terse",
            "--prompts", "prompts.txt",
            "--model", "claude-sonnet-4-6",
            "--output", "foo",
        ]))
        .unwrap_err();
        assert!(
            err.contains("unsupported value for --output"),
            "expected unsupported value error in: {err}"
        );
    }

    #[test]
    fn unknown_flag_returns_error() {
        let err = parse_skill_eval_args(&args(&[
            "--skill", "terse",
            "--prompts", "prompts.txt",
            "--model", "claude-sonnet-4-6",
            "--unknown-flag", "val",
        ]))
        .unwrap_err();
        assert!(
            err.contains("unknown skill-eval flag"),
            "expected unknown flag error in: {err}"
        );
    }

    // ── prompt loader tests ───────────────────────────────────────────────────

    #[test]
    fn load_prompts_from_file_reads_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("prompts.txt");
        std::fs::write(&file, "first prompt\nsecond prompt\n\n").expect("write");

        let prompts = load_prompts(file.to_str().expect("path")).expect("load");
        assert_eq!(prompts, vec!["first prompt", "second prompt"]);
    }

    #[test]
    fn load_prompts_from_file_trims_trailing_blank_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("prompts.txt");
        std::fs::write(&file, "a\nb\n\n\n").expect("write");

        let prompts = load_prompts(file.to_str().expect("path")).expect("load");
        assert_eq!(prompts, vec!["a", "b"]);
    }

    #[test]
    fn load_prompts_from_dir_reads_txt_files_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("b.txt"), "prompt B").expect("write b");
        std::fs::write(dir.path().join("a.txt"), "prompt A").expect("write a");
        std::fs::write(dir.path().join("c.txt"), "prompt C").expect("write c");
        std::fs::write(dir.path().join("notes.md"), "ignored").expect("write md");

        let prompts = load_prompts(dir.path().to_str().expect("path")).expect("load");
        assert_eq!(prompts, vec!["prompt A", "prompt B", "prompt C"]);
    }

    #[test]
    fn load_prompts_from_dir_ignores_non_txt_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("prompt.txt"), "only txt").expect("write");
        std::fs::write(dir.path().join("readme.md"), "ignored").expect("write md");

        let prompts = load_prompts(dir.path().to_str().expect("path")).expect("load");
        assert_eq!(prompts, vec!["only txt"]);
    }

    // ── skill resolver tests ──────────────────────────────────────────────────

    #[test]
    fn resolve_skill_path_uses_absolute_path_as_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_file = dir.path().join("SKILL.md");
        std::fs::write(&skill_file, "# Skill").expect("write");

        let resolved = resolve_skill_path(skill_file.to_str().expect("path"))
            .expect("should resolve");
        assert_eq!(resolved, skill_file);
    }

    #[test]
    fn resolve_skill_path_absolute_missing_returns_error() {
        let err = resolve_skill_path("/nonexistent/absolute/SKILL.md").unwrap_err();
        assert!(err.contains("not found"), "expected 'not found' in: {err}");
    }

    #[test]
    fn resolve_bundled_skill_terse_resolves_to_expected_path() {
        // CARGO_MANIFEST_DIR is set during cargo test — use it to find workspace root.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set during cargo test");
        let expected = PathBuf::from(&manifest_dir)
            .join("../../crates/commands/bundled/skills/terse/SKILL.md")
            .canonicalize()
            .expect("terse SKILL.md must exist");

        let resolved = resolve_skill_path("terse").expect("should resolve terse");
        assert_eq!(resolved, expected);
    }

    // ── summary formatter tests ───────────────────────────────────────────────

    fn mock_report() -> EvalReport {
        use compat_harness::skill_evals::{ArmResult, EvalSummary, honest_caveats};
        let make_arm = |arm: &str, tokens: usize| ArmResult {
            arm: arm.to_string(),
            prompt: "test".to_string(),
            response: "x".repeat(tokens * 4),
            estimated_tokens: tokens,
            byte_len: tokens * 4,
        };
        let results_per_prompt = vec![[
            make_arm("__baseline__", 142),
            make_arm("__terse__", 87),
            make_arm("terse", 72),
        ]];
        let summary = EvalSummary {
            baseline_avg_tokens: 142.0,
            terse_avg_tokens: 87.0,
            skill_avg_tokens: 72.0,
            skill_vs_terse_delta_pct: (72.0 - 87.0) / 87.0 * 100.0,
            honest_caveats: honest_caveats(),
        };
        EvalReport {
            model: "claude-sonnet-4-6".to_string(),
            skill_name: "terse".to_string(),
            results_per_prompt,
            summary,
        }
    }

    #[test]
    fn precise_summary_contains_expected_shape() {
        let report = mock_report();
        let out = format_precise_summary(&report, &PathBuf::from("./skill-evals"));

        assert!(out.contains("Skill evaluation"), "missing header: {out}");
        assert!(out.contains("terse"), "missing skill name: {out}");
        assert!(out.contains("claude-sonnet-4-6"), "missing model: {out}");
        assert!(out.contains("baseline avg"), "missing baseline: {out}");
        assert!(out.contains("terse avg"), "missing terse: {out}");
        assert!(out.contains("skill avg"), "missing skill: {out}");
        assert!(out.contains("Snapshots:"), "missing snapshots line: {out}");
    }

    #[test]
    fn condensed_summary_contains_expected_shape() {
        let report = mock_report();
        let out = format_condensed_summary(&report);

        assert!(out.contains("eval:"), "missing eval: prefix: {out}");
        assert!(out.contains("terse"), "missing skill name: {out}");
        assert!(out.contains("claude-sonnet-4-6"), "missing model: {out}");
        assert!(out.contains("baseline"), "missing baseline: {out}");
        assert!(out.contains("vs terse"), "missing vs terse: {out}");
    }

    #[test]
    fn precise_summary_always_has_caveats() {
        let report = mock_report();
        let out = format_precise_summary(&report, &PathBuf::from("./skill-evals"));
        assert!(out.contains("Honest caveats"), "caveats missing from precise: {out}");
        // All 3 caveats must appear.
        assert!(
            out.contains("byte_len") || out.contains("byte-length") || out.contains("heuristic"),
            "caveat 1 missing: {out}"
        );
        assert!(
            out.contains("fidelity") || out.contains("does NOT measure") || out.contains("latency"),
            "caveat 2 missing: {out}"
        );
        assert!(
            out.contains("terse_delta") || out.contains("terse_vs") || out.contains("useful"),
            "caveat 3 missing: {out}"
        );
    }

    #[test]
    fn condensed_summary_always_has_caveats() {
        let report = mock_report();
        let out = format_condensed_summary(&report);
        assert!(out.contains("Honest caveats"), "caveats missing from condensed: {out}");
    }

    // ── mock end-to-end runner ────────────────────────────────────────────────

    struct MockCaller {
        call_count: Arc<AtomicUsize>,
    }

    impl ArmCaller for MockCaller {
        fn call<'life0, 'life1, 'life2, 'life3, 'async_trait>(
            &'life0 self,
            _model: &'life1 str,
            system_prompt: Option<&'life2 str>,
            _user_prompt: &'life3 str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'async_trait>,
        >
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            'life2: 'async_trait,
            'life3: 'async_trait,
        {
            let count = Arc::clone(&self.call_count);
            let reduction = system_prompt.map_or(0, |s| s.len() / 2);
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                let base = 400usize;
                Ok("x".repeat(base.saturating_sub(reduction)))
            })
        }
    }

    #[test]
    fn run_skill_eval_with_caller_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Write SKILL.md
        let skill_path = dir.path().join("SKILL.md");
        std::fs::write(&skill_path, "# Test\nBe terse.").expect("write skill");

        // Write prompts file
        let prompts_path = dir.path().join("prompts.txt");
        std::fs::write(&prompts_path, "What is Rust?\nExplain async.").expect("write prompts");

        let call_count = Arc::new(AtomicUsize::new(0));
        let caller = MockCaller { call_count: Arc::clone(&call_count) };

        let skill_args = SkillEvalArgs {
            skill: skill_path.to_str().expect("path").to_string(),
            prompts: prompts_path.to_str().expect("path").to_string(),
            model: "claude-sonnet-4-6".to_string(),
            provider: Some("anthropic".to_string()),
            snapshot_dir: dir.path().join("snaps"),
            output: EvalOutputFormat::Markdown,
        };

        run_skill_eval_with_caller(skill_args, &caller)
            .expect("run should succeed");

        // 2 prompts × 3 arms = 6 calls
        assert_eq!(call_count.load(Ordering::SeqCst), 6);

        // Snapshots directory created
        assert!(dir.path().join("snaps").is_dir());
    }
}
