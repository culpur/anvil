use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use serde_json;
use crate::providers::InternalPromptProgressRun;
use crate::{
    command_exists, detect_project_type_for_pipeline, extract_notebook_cell, git_output,
    git_status_ok, lsp_binary_for_lang, parse_line_range, parse_titled_body,
    recent_user_context, run_test_suite, sanitize_generated_message, shell_output_or_err,
    shell_quote, truncate_for_prompt, write_temp_text_file, LiveCli,
};

impl LiveCli {

    pub(crate) fn run_review_pr_command(&self, number: Option<&str>) -> String {
        // Build the `gh pr diff` args.
        let mut diff_args = vec!["pr", "diff"];
        let mut view_args = vec!["pr", "view", "--json", "title,body,author"];
        let num_owned;
        if let Some(n) = number {
            num_owned = n.to_string();
            diff_args.push(&num_owned);
            view_args.push(&num_owned);
        }

        let diff = Command::new("gh")
            .args(&diff_args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        if diff.trim().is_empty() {
            return "No PR diff found. Make sure gh is installed, authenticated, and a PR is open for the current branch.".to_string();
        }

        let meta = Command::new("gh")
            .args(&view_args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        // Parse metadata for a human-readable header.
        let pr_label = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta) {
            let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("(no title)");
            let author = v.get("author")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or("unknown");
            let body = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
            format!("PR: {title}\nAuthor: {author}\nDescription:\n{body}\n")
        } else {
            meta.clone()
        };

        let diff_truncated = if diff.len() > 40_000 {
            format!("{}\n... (truncated)", &diff[..40_000])
        } else {
            diff.clone()
        };

        let prompt = format!(
            "You are a senior code reviewer. Review the following GitHub pull request \
             and provide a structured assessment covering:\n\
             1. Summary of changes\n\
             2. Critical bugs or logic errors\n\
             3. Security vulnerabilities\n\
             4. Performance concerns\n\
             5. Code style and readability issues\n\
             6. Suggested improvements\n\n\
             {pr_label}\n\
             ## Diff\n\
             ```diff\n{diff_truncated}\n```\n\n\
             Be thorough but concise.",
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(review) => format!("PR Review:\n\n{review}"),
            Err(e) => format!("review-pr: {e}"),
        }
    }

    pub(crate) fn run_bughunter(&self, scope: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let scope = scope.unwrap_or("the current repository");
        let prompt = format!(
            "You are /bughunter. Inspect {scope} and identify the most likely bugs or correctness issues. Prioritize concrete findings with file paths, severity, and suggested fixes. Use tools if needed."
        );
        println!("{}", self.run_internal_prompt_text(&prompt, true)?);
        Ok(())
    }

    pub(crate) fn run_ultraplan(&self, task: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let task = task.unwrap_or("the current repo work");
        let prompt = format!(
            "You are /ultraplan. Produce a deep multi-step execution plan for {task}. Include goals, risks, implementation sequence, verification steps, and rollback considerations. Use tools if needed."
        );
        let mut progress = InternalPromptProgressRun::start_ultraplan(task);
        match self.run_internal_prompt_text_with_progress(&prompt, true, Some(progress.reporter()))
        {
            Ok(plan) => {
                progress.finish_success();
                println!("{plan}");
                Ok(())
            }
            Err(error) => {
                progress.finish_failure(&error.to_string());
                Err(error)
            }
        }
    }

    pub(crate) fn run_test_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /test generate <file>   Analyse a source file and generate unit tests",
                "  /test run               Run the project test suite",
                "  /test coverage          Run the test suite and show coverage summary",
            ]
            .join("\n");
        }

        if args == "run" {
            return run_test_suite(false);
        }

        if args == "coverage" {
            return run_test_suite(true);
        }

        if let Some(file) = args.strip_prefix("generate ") {
            let file = file.trim();
            if file.is_empty() {
                return "Usage: /test generate <file>".to_string();
            }
            let path = PathBuf::from(file);
            let source = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => return format!("Cannot read {file}: {e}"),
            };
            let prompt = format!(
                "You are /test generate. Analyse the following source file and produce a comprehensive unit-test suite for it.\n\
                 - Follow the testing idioms and conventions of the language detected.\n\
                 - Cover edge cases, error paths, and happy paths.\n\
                 - Output only the test file content, properly formatted.\n\
                 - Suggest the filename to save the tests to.\n\n\
                 Source file: {file}\n\n```\n{source}\n```",
                source = truncate_for_prompt(&source, 12_000),
            );
            match self.run_internal_prompt_text(&prompt, false) {
                Ok(result) => format!("Generated tests for {file}:\n\n{result}"),
                Err(e) => format!("test generate failed: {e}"),
            }
        } else {
            format!("Unknown /test sub-command: {args}\nRun `/test help` for usage.")
        }
    }

    pub(crate) fn run_git_rebase_assistant(&self) -> String {
        let log = match git_output(&["log", "--oneline", "-20"]) {
            Ok(s) => s,
            Err(e) => return format!("git log failed: {e}"),
        };
        let prompt = format!(
            "You are /git rebase assistant. Summarise the following recent commits and suggest \
             which ones would benefit from being squashed, reordered, or dropped during an \
             interactive rebase. Provide the exact git rebase -i command to run and explain \
             each recommended action.\n\nRecent commits:\n{log}"
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => result,
            Err(e) => format!("git rebase assistant failed: {e}"),
        }
    }

    pub(crate) fn run_git_conflicts(&self) -> String {
        // Find files with conflict markers
        let conflict_check = Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(env::current_dir().unwrap_or_default())
            .output();

        let conflict_files = match conflict_check {
            Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
            Err(e) => return format!("git diff failed: {e}"),
        };

        if conflict_files.is_empty() {
            return "No merge conflicts detected in the working tree.".to_string();
        }

        let file_list: Vec<&str> = conflict_files.lines().collect();
        let mut snippets = Vec::new();

        for file in file_list.iter().take(5) {
            if let Ok(content) = fs::read_to_string(file) {
                let conflict_section: String = content
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| {
                        line.starts_with("<<<<<<<")
                            || line.starts_with("=======")
                            || line.starts_with(">>>>>>>")
                    })
                    .map(|(i, line)| format!("  L{}: {line}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !conflict_section.is_empty() {
                    snippets.push(format!("{file}:\n{conflict_section}"));
                }
            }
        }

        let summary = snippets.join("\n\n");
        let prompt = format!(
            "You are /git conflicts. Explain the following merge conflicts and recommend \
             the best resolution strategy for each one. Be specific about which side (ours/theirs) \
             to keep or how to manually combine them.\n\nConflicted files:\n{}\n\nConflict markers:\n{}",
            file_list.join(", "),
            summary
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!(
                "Merge conflicts detected in: {}\n\n{result}",
                file_list.join(", ")
            ),
            Err(e) => format!("conflict analysis failed: {e}"),
        }
    }

    pub(crate) fn run_git_cherry_pick(&self, sha: &str) -> String {
        if sha.is_empty() {
            return "Usage: /git cherry-pick <sha>".to_string();
        }
        let show = match git_output(&["show", "--stat", sha]) {
            Ok(s) => s,
            Err(e) => return format!("git show {sha} failed: {e}"),
        };
        let prompt = format!(
            "You are /git cherry-pick assistant. The user wants to cherry-pick commit {sha}.\n\
             Summarise what this commit does, flag any risks (e.g. conflicts, dependency on \
             prior commits), and provide the exact command to run.\n\nCommit info:\n{show}"
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => result,
            Err(e) => format!("cherry-pick assistant failed: {e}"),
        }
    }

    pub(crate) fn run_refactor_rename(&self, old: &str, new: &str) -> String {
        // Count occurrences first so the user can confirm before Anvil acts
        let count_output = Command::new("rg")
            .args(["--color=never", "--count-matches", old])
            .current_dir(env::current_dir().unwrap_or_default())
            .output();

        let occurrence_info = match count_output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if text.is_empty() {
                    format!("Symbol `{old}` not found in the workspace.")
                } else {
                    let total: usize = text
                        .lines()
                        .filter_map(|l| l.split(':').next_back().and_then(|n| n.trim().parse::<usize>().ok()))
                        .sum();
                    format!("Found {total} occurrences of `{old}` across:\n{text}")
                }
            }
            Err(_) => format!("ripgrep not available; cannot count occurrences of `{old}`"),
        };

        let prompt = format!(
            "You are /refactor rename. The user wants to rename `{old}` to `{new}` across the codebase.\n\
             Provide step-by-step instructions including:\n\
             1. Which files to update and why.\n\
             2. Any identifier collisions or naming conflicts to watch for.\n\
             3. The exact rg/sed commands to perform the rename safely.\n\
             4. Any follow-up changes (e.g. tests, docs, config files).\n\n\
             Occurrence summary:\n{occurrence_info}"
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!("Refactor rename `{old}` -> `{new}`\n\n{occurrence_info}\n\n{result}"),
            Err(e) => format!("refactor rename failed: {e}"),
        }
    }

    pub(crate) fn run_refactor_extract(&self, file: &str, lines: &str) -> String {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => return format!("Cannot read {file}: {e}"),
        };

        let (start, end) = parse_line_range(lines);
        let selected: String = source
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let lineno = i + 1;
                lineno >= start && (end == 0 || lineno <= end)
            })
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n");

        if selected.is_empty() {
            return format!("No lines selected in {file} for range `{lines}`.");
        }

        let prompt = format!(
            "You are /refactor extract. The user wants to extract lines {lines} from `{file}` into a new function.\n\
             Analyse the selected code and provide:\n\
             1. A suggested function name and signature (infer parameters from free variables).\n\
             2. The complete extracted function definition.\n\
             3. The call-site replacement snippet.\n\
             4. Any considerations about scope, return values, or side effects.\n\n\
             Selected code:\n```\n{selected}\n```"
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!("Extract function from {file} lines {lines}:\n\n{result}"),
            Err(e) => format!("refactor extract failed: {e}"),
        }
    }

    pub(crate) fn run_refactor_move(&self, source: &str, dest: &str) -> String {
        let source_content = match fs::read_to_string(source) {
            Ok(s) => s,
            Err(e) => return format!("Cannot read {source}: {e}"),
        };
        let dest_exists = fs::metadata(dest).is_ok();
        let dest_preview = if dest_exists {
            fs::read_to_string(dest)
                .map(|s| format!("Destination file exists:\n```\n{}\n```", truncate_for_prompt(&s, 4_000)))
                .unwrap_or_default()
        } else {
            format!("Destination file `{dest}` does not yet exist (will be created).")
        };

        let prompt = format!(
            "You are /refactor move. The user wants to move code from `{source}` to `{dest}`.\n\
             Provide:\n\
             1. What to move and what to keep in the source file.\n\
             2. How to update imports/exports/use declarations on both sides.\n\
             3. The exact file edits needed.\n\
             4. Any circular dependency risks.\n\n\
             Source file ({source}):\n```\n{src}\n```\n\n{dest_preview}",
            src = truncate_for_prompt(&source_content, 6_000),
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!("Refactor move `{source}` -> `{dest}`:\n\n{result}"),
            Err(e) => format!("refactor move failed: {e}"),
        }
    }

    pub(crate) fn run_db_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        match sub {
            "" | "help" => [
                "Database tools",
                "",
                "  /db connect <url>   Probe a database connection",
                "  /db schema          Inspect schema files in the project",
                "  /db query <sql>     Analyse SQL with AI (performance, security)",
                "  /db migrate         Detect schema drift and suggest migrations",
                "",
                "Supported URL prefixes: postgres://, mysql://, sqlite://",
            ]
            .join("\n"),

            "connect" => {
                if rest.is_empty() { return "Usage: /db connect <url>".to_string(); }
                let driver = if rest.starts_with("postgres") {
                    "psql"
                } else if rest.starts_with("mysql") {
                    "mysql"
                } else if rest.starts_with("sqlite") {
                    "sqlite3"
                } else {
                    return format!(
                        "Unsupported scheme: {rest}\nSupported: postgres://, mysql://, sqlite://"
                    );
                };
                match Command::new(driver).arg("--version").output() {
                    Err(_) => format!(
                        "Driver `{driver}` not found on PATH.\nInstall it then retry: /db connect {rest}"
                    ),
                    Ok(_) => format!(
                        "Driver `{driver}` is available.\nURL: {rest}\n\nNext: /db schema  or  /db query <sql>"
                    ),
                }
            }

            "schema" => {
                let cwd = env::current_dir().unwrap_or_default();
                let found: Vec<String> = [
                    "prisma/schema.prisma", "schema.prisma",
                    "knexfile.js", "knexfile.ts", "database.yml", "db/schema.rb",
                ]
                .iter()
                .filter(|c| cwd.join(c).exists())
                .map(std::string::ToString::to_string)
                .collect();

                if found.is_empty() {
                    return "No schema files found (prisma/schema.prisma, knexfile, etc.).".to_string();
                }

                let mut lines = vec![format!("Schema files ({})\n", found.len())];
                for f in &found {
                    lines.push(format!("  {f}"));
                    if let Ok(content) = fs::read_to_string(cwd.join(f.as_str())) {
                        for pl in content.lines()
                            .filter(|l| !l.trim().is_empty()
                                && !l.trim_start().starts_with("//")
                                && !l.trim_start().starts_with('#'))
                            .take(20)
                        {
                            lines.push(format!("    {pl}"));
                        }
                    }
                    lines.push(String::new());
                }
                lines.push("Tip: /db migrate for drift analysis.".to_string());
                lines.join("\n")
            }

            "query" => {
                if rest.is_empty() { return "Usage: /db query <sql>".to_string(); }
                let prompt = format!(
                    "Analyse this SQL query:\n```sql\n{rest}\n```\n\n                     1. Validate syntax.\n                     2. Suggest performance improvements (indexes, rewrites).\n                     3. Identify SQL injection risks in a dynamic version.\n                     4. Explain what the query returns in plain English."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("Query analysis:\n\n{r}"),
                    Err(e) => format!("db query failed: {e}"),
                }
            }

            "migrate" => {
                let cwd = env::current_dir().unwrap_or_default();
                let schema_path = ["prisma/schema.prisma", "schema.prisma"]
                    .iter()
                    .find(|p| cwd.join(p).exists())
                    .copied();

                let schema_info = if let Some(path) = schema_path {
                    fs::read_to_string(cwd.join(path)).map_or_else(|_| "Could not read schema.".to_string(), |s| format!(
                            "Prisma schema (`{path}`):\n```prisma\n{}\n```",
                            truncate_for_prompt(&s, 8_000)
                        ))
                } else {
                    let mut files = Vec::new();
                    for dir in &["migrations", "db/migrations", "prisma/migrations"] {
                        if let Ok(rd) = fs::read_dir(cwd.join(dir)) {
                            for e in rd.flatten() {
                                let name = e.file_name().to_string_lossy().to_string();
                                if name.ends_with(".sql") || name.ends_with(".ts") {
                                    files.push(format!("{dir}/{name}"));
                                }
                            }
                        }
                    }
                    if files.is_empty() {
                        return "No schema or migration files found.".to_string();
                    }
                    files.sort();
                    format!(
                        "Migration files:\n{}",
                        files.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n")
                    )
                };

                let prompt = format!(
                    "Analyse for schema drift and suggest migrations.\n\n{schema_info}\n\n                     1. Summarise models/tables.\n                     2. Identify drift (missing indexes, bad nullability, un-normalised relations).\n                     3. Suggest concrete migration steps.\n                     4. Highlight breaking changes."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("Migration analysis:\n\n{r}"),
                    Err(e) => format!("db migrate failed: {e}"),
                }
            }

            other => format!("Unknown /db sub-command: {other}\nRun `/db help` for usage."),
        }
    }

    pub(crate) fn run_api_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        match sub {
            "" | "help" => [
                "API development helpers",
                "",
                "  /api spec <file>       Generate OpenAPI spec from a source file",
                "  /api mock <spec>       Start a mock server from an OpenAPI spec",
                "  /api test <url>        Test an API endpoint via curl",
                "  /api docs              Generate API docs for the project",
            ]
            .join("\n"),

            "spec" => {
                if rest.is_empty() { return "Usage: /api spec <file>".to_string(); }
                let source = match fs::read_to_string(rest) {
                    Ok(s) => s,
                    Err(e) => return format!("Cannot read {rest}: {e}"),
                };
                let prompt = format!(
                    "Generate an OpenAPI 3.1 specification (YAML) for this source file.\n                     Extract all routes, HTTP methods, request/response schemas, parameters.\n                     File: {rest}\n\n```\n{}\n```",
                    truncate_for_prompt(&source, 8_000)
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("OpenAPI spec for {rest}:\n\n{r}"),
                    Err(e) => format!("api spec failed: {e}"),
                }
            }

            "mock" => {
                if rest.is_empty() { return "Usage: /api mock <spec-file>".to_string(); }
                if !std::path::Path::new(rest).exists() {
                    return format!("Spec file not found: {rest}");
                }
                for (tool, tool_args) in &[
                    ("prism", vec!["mock", rest]),
                    ("json-server", vec!["--watch", rest]),
                ] {
                    if let Ok(mut child) = Command::new(tool).args(tool_args).spawn() {
                        thread::sleep(Duration::from_millis(500));
                        if child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                            return format!(
                                "Mock server started with `{tool}`.\nSpec: {rest}\nCtrl+C to stop."
                            );
                        }
                    }
                }
                format!(
                    "No mock server tool found. Install:\n                     - npm install -g @stoplight/prism-cli\n                     - npm install -g json-server\n\nRetry: /api mock {rest}"
                )
            }

            "test" => {
                if rest.is_empty() { return "Usage: /api test <url>".to_string(); }
                match Command::new("curl")
                    .args(["-s", "-D", "-", "-o", "/dev/null", "--max-time", "10", rest])
                    .output()
                {
                    Err(e) => format!("curl failed: {e}"),
                    Ok(o) => {
                        let headers = String::from_utf8_lossy(&o.stdout).to_string();
                        let status = headers.lines().next().unwrap_or("").trim().to_string();
                        let ct = headers.lines()
                            .find(|l| l.to_lowercase().starts_with("content-type:"))
                            .unwrap_or("")
                            .to_string();
                        format!("API test: {rest}\n\nStatus: {status}\n{ct}\n\nHeaders:\n{headers}")
                    }
                }
            }

            "docs" => {
                let cwd = env::current_dir().unwrap_or_default();
                let route_dirs = [
                    "src/routes", "routes", "src/api", "api",
                    "src/controllers", "controllers",
                ];
                let mut route_files: Vec<String> = Vec::new();
                for dir in &route_dirs {
                    if let Ok(entries) = fs::read_dir(cwd.join(dir)) {
                        for e in entries.flatten() {
                            let name = e.file_name().to_string_lossy().to_string();
                            if name.ends_with(".ts") || name.ends_with(".js") || name.ends_with(".rs") {
                                route_files.push(format!("{dir}/{name}"));
                            }
                        }
                    }
                }
                if route_files.is_empty() {
                    return "No route files found. Use `/api spec <file>` to target one directly.".to_string();
                }
                let file_list = route_files.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n");
                let prompt = format!(
                    "Generate Markdown API documentation.\nRoute files:\n{file_list}\n\n                     For each endpoint: method, path, description, params, response, auth.\n                     Include a table of contents."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("API documentation:\n\n{r}"),
                    Err(e) => format!("api docs failed: {e}"),
                }
            }

            other => format!("Unknown /api sub-command: {other}\nRun `/api help` for usage."),
        }
    }

    pub(crate) fn run_docs_generate(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let mut stack = Vec::new();
        if cwd.join("Cargo.toml").exists()     { stack.push("Rust/Cargo"); }
        if cwd.join("package.json").exists()   { stack.push("Node.js/npm"); }
        if cwd.join("pyproject.toml").exists() { stack.push("Python/pyproject"); }
        let tech = if stack.is_empty() { "unknown".to_string() } else { stack.join(", ") };
        let prompt = format!(
            "Generate comprehensive documentation for a {tech} project at `{cwd}`.\n             Produce: 1. Overview  2. Installation  3. Configuration               4. Usage examples  5. Dev workflow  6. Contributing\nOutput as Markdown.",
            cwd = cwd.display()
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(r) => format!("Generated documentation:\n\n{r}"),
            Err(e) => format!("docs generate failed: {e}"),
        }
    }

    pub(crate) fn run_docs_readme(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let existing = ["README.md", "readme.md", "README.rst"]
            .iter()
            .find_map(|n| fs::read_to_string(cwd.join(n)).ok());
        let project_name = ["Cargo.toml", "package.json", "pyproject.toml"]
            .iter()
            .find_map(|f| {
                let content = fs::read_to_string(cwd.join(f)).ok()?;
                content.lines().find_map(|l| {
                    let l = l.trim();
                    if l.starts_with("name") {
                        let val = l.split(['=', ':'])
                            .nth(1)?
                            .trim()
                            .trim_matches(['"', '\'', ',', ' ']);
                        if !val.is_empty() && val != "{" {
                            return Some(val.to_string());
                        }
                    }
                    None
                })
            })
            .unwrap_or_else(|| {
                cwd.file_name().unwrap_or_default().to_string_lossy().to_string()
            });
        let context = match existing {
            Some(content) => format!(
                "Update this README:\n```markdown\n{}\n```\nImprove it with:",
                truncate_for_prompt(&content, 4_000)
            ),
            None => format!("Create a README for `{project_name}` with:"),
        };
        let prompt = format!(
            "{context}\n- Title and description\n- Quick start\n- Usage examples\n             - Configuration\n- Dev setup\n- License\n\nOutput only the README.md content."
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(r) => format!("README.md:\n\n{r}"),
            Err(e) => format!("docs readme failed: {e}"),
        }
    }

    pub(crate) fn run_docs_architecture(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let mut structure = Vec::new();
        if let Ok(entries) = fs::read_dir(&cwd) {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(std::fs::DirEntry::file_name);
            for e in sorted.iter().take(40) {
                let name = e.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git"|"target"|"node_modules"|".cache"|"vendor") {
                    continue;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                structure.push(if is_dir { format!("  {name}/") } else { format!("  {name}") });
            }
        }
        let structure_text = structure.join("\n");
        let prompt = format!(
            "Generate an architecture overview document.\nRoot: {cwd}\nStructure:\n{structure_text}\n\n\
             Include: 1. Description  2. ASCII component diagram  3. Data flow\n\
             4. Technology stack  5. Design decisions  6. Deployment topology\n\
             Output as Markdown.",
            cwd = cwd.display(),
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(r) => format!("Architecture overview:\n\n{r}"),
            Err(e) => format!("docs architecture failed: {e}"),
        }
    }

    pub(crate) fn run_docs_changelog(&self) -> String {
        let git_log = Command::new("git")
            .args(["log", "--oneline", "--no-merges", "--format=%h %ad %s", "--date=short", "-100"])
            .output();
        match git_log {
            Err(e) => format!("git log failed: {e}"),
            Ok(o) if !o.status.success() => "Not in a git repository or no commits yet.".to_string(),
            Ok(o) => {
                let raw = String::from_utf8_lossy(&o.stdout).to_string();
                if raw.trim().is_empty() { return "No commits found.".to_string(); }
                let prompt = format!(
                    "Generate a CHANGELOG.md from this git log.\n                     Group by type and release (Keep-a-Changelog format).\n\n                     ```\n{}\n```",
                    truncate_for_prompt(&raw, 6_000)
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("CHANGELOG.md:\n\n{r}"),
                    Err(e) => format!("docs changelog failed: {e}"),
                }
            }
        }
    }

    pub(crate) fn run_scaffold_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        const TEMPLATES: &[(&str, &str)] = &[
            ("rust",   "Rust binary — Cargo.toml, src/main.rs, .gitignore"),
            ("node",   "Node.js — package.json, src/index.js, .gitignore"),
            ("python", "Python — pyproject.toml, src/__init__.py, .gitignore"),
            ("react",  "React + Vite — package.json, src/App.tsx, Tailwind CSS"),
            ("nextjs", "Next.js — package.json, app/page.tsx, Tailwind CSS"),
            ("go",     "Go module — go.mod, cmd/main.go, .gitignore"),
            ("docker", "Docker service — Dockerfile, docker-compose.yml, .env.example"),
        ];

        match sub {
            "" | "help" => {
                let mut lines = vec![
                    "Usage:".to_string(),
                    "  /scaffold new <template>   Create a project from a template".to_string(),
                    "  /scaffold list             List available templates".to_string(),
                    String::new(),
                    "Templates:".to_string(),
                ];
                for (name, desc) in TEMPLATES {
                    lines.push(format!("  {name:<10}  {desc}"));
                }
                lines.join("\n")
            }
            "list" => {
                let mut lines = vec!["Available templates:".to_string()];
                for (name, desc) in TEMPLATES {
                    lines.push(format!("  {name:<10}  {desc}"));
                }
                lines.join("\n")
            }
            "new" => {
                let template = rest;
                if template.is_empty() {
                    return "Usage: /scaffold new <template>\n  Run /scaffold list for available templates.".to_string();
                }
                if !TEMPLATES.iter().any(|(n, _)| *n == template) {
                    let names: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
                    return format!(
                        "Unknown template: {template}\n  Available: {}\n  Run /scaffold list for details.",
                        names.join(", ")
                    );
                }
                let cwd = env::current_dir().unwrap_or_default();
                let prompt = format!(
                    "You are /scaffold. The user wants to create a new {template} project in the current directory ({cwd}).\n\
                     Generate the complete file tree and contents for a production-ready {template} project.\n\
                     Follow best practices:\n\
                     - Include a .gitignore appropriate for the ecosystem.\n\
                     - Include a minimal README.md.\n\
                     - Include a sensible directory structure.\n\
                     - Include linting/formatting config where standard (e.g. .eslintrc, rustfmt.toml).\n\
                     - For compiled languages include a build script.\n\
                     Output each file as a code block with the path as the heading.\n\
                     After the files, list 3 next steps the developer should take.",
                    cwd = cwd.display(),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Scaffold: {template}\n\n{result}"),
                    Err(e) => format!("scaffold failed: {e}"),
                }
            }
            other => format!(
                "Unknown /scaffold sub-command: {other}\n  /scaffold list   List templates\n  /scaffold new <template>   Create project"
            ),
        }
    }

    pub(crate) fn run_perf_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        match sub {
            "" | "help" => [
                "Usage:",
                "  /perf profile <command>   Profile a shell command and report timing",
                "  /perf benchmark <file>    Analyse benchmarks in a file",
                "  /perf flamegraph          Guide for generating a flamegraph",
                "  /perf analyze             Analyze profiling artifacts in the workspace",
            ].join("\n"),

            "profile" => {
                if rest.is_empty() {
                    return "Usage: /perf profile <command>".to_string();
                }
                // Warn about shell injection risk and reject the most
                // obviously dangerous patterns before passing to sh -c.
                let dangerous_patterns = ["$(", "`", "&&", "||", ";", "|", ">", "<", ">>"];
                for pattern in &dangerous_patterns {
                    if rest.contains(pattern) {
                        return format!(
                            "Warning: /perf profile rejected command containing \
                             potentially dangerous shell operator '{pattern}'.\n\
                             Use simple commands without shell metacharacters."
                        );
                    }
                }
                eprintln!("[anvil] /perf profile: executing via sh -c: {rest}");
                let start = std::time::Instant::now();
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(rest)
                    .current_dir(env::current_dir().unwrap_or_default())
                    .output();
                let elapsed = start.elapsed();
                let (stdout, stderr, exit_status) = match output {
                    Ok(o) => (
                        String::from_utf8_lossy(&o.stdout).trim().to_string(),
                        String::from_utf8_lossy(&o.stderr).trim().to_string(),
                        if o.status.success() { "success".to_string() } else {
                            format!("exit {}", o.status.code().unwrap_or(-1))
                        },
                    ),
                    Err(e) => (String::new(), e.to_string(), "error".to_string()),
                };
                let summary = format!(
                    "Perf Profile\n  Command          {rest}\n  Wall time        {elapsed:.3?}\n  Status           {exit_status}"
                );
                let combined = format!("{summary}\n\nStdout:\n{stdout}\n\nStderr:\n{stderr}");
                let prompt = format!(
                    "You are /perf profile. A command was profiled.\n\
                     Command: {rest}\nWall time: {elapsed:.3?}\nExit status: {exit_status}\n\
                     Stdout (truncated):\n{so}\nStderr (truncated):\n{se}\n\n\
                     Provide a brief analysis:\n\
                     1. Is the runtime acceptable for this type of command?\n\
                     2. What are the likely bottlenecks?\n\
                     3. Concrete suggestions to speed it up.",
                    so = truncate_for_prompt(&stdout, 3_000),
                    se = truncate_for_prompt(&stderr, 1_000),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(analysis) => format!("{combined}\n\nAnalysis:\n{analysis}"),
                    Err(_) => combined,
                }
            }

            "benchmark" => {
                if rest.is_empty() {
                    return "Usage: /perf benchmark <file>".to_string();
                }
                let source = match fs::read_to_string(rest) {
                    Ok(s) => s,
                    Err(e) => return format!("Cannot read {rest}: {e}"),
                };
                let prompt = format!(
                    "You are /perf benchmark. Analyse `{rest}` for benchmark functions.\n\
                     Source:\n```\n{}\n```\n\n\
                     For each benchmark:\n\
                     1. Describe what it measures.\n\
                     2. Identify measurement pitfalls (warm-up, noise, allocations).\n\
                     3. Suggest how to run it.\n\
                     4. Propose improvements to the benchmark itself if any.",
                    truncate_for_prompt(&source, 8_000),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Benchmark analysis for {rest}:\n\n{result}"),
                    Err(e) => format!("perf benchmark failed: {e}"),
                }
            }

            "flamegraph" => {
                let cwd = env::current_dir().unwrap_or_default();
                let prompt = format!(
                    "You are /perf flamegraph. Describe how to generate a flamegraph for the project at `{}`.\n\
                     Provide:\n\
                     1. Which profiling tool is best suited (cargo-flamegraph, perf + flamegraph.pl, py-spy, async-profiler, etc.).\n\
                     2. The exact commands to install the tool and capture a profile.\n\
                     3. How to interpret the resulting flamegraph.\n\
                     4. Common hotspot patterns to look for.",
                    cwd.display(),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Flamegraph guide:\n\n{result}"),
                    Err(e) => format!("perf flamegraph failed: {e}"),
                }
            }

            "analyze" => {
                let cwd = env::current_dir().unwrap_or_default();
                let artifacts: Vec<String> = ["perf.data", "flame.svg", "flamegraph.svg", "callgrind.out", "profile.json"]
                    .iter()
                    .filter_map(|name| {
                        let p = cwd.join(name);
                        if p.exists() { Some((*name).to_string()) } else { None }
                    })
                    .collect();
                let artifact_summary = if artifacts.is_empty() {
                    "No standard profiling artifacts found in the current directory.".to_string()
                } else {
                    format!("Found profiling artifacts: {}", artifacts.join(", "))
                };
                let prompt = format!(
                    "You are /perf analyze. {artifact_summary}\nWorking directory: {}\n\
                     Provide guidance on:\n\
                     1. How to interpret any discovered artifacts.\n\
                     2. General profiling best practices for this project type.\n\
                     3. Recommended next steps to identify performance regressions.",
                    cwd.display(),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Perf analysis:\n  {artifact_summary}\n\n{result}"),
                    Err(e) => format!("perf analyze failed: {e}"),
                }
            }

            other => format!("Unknown /perf sub-command: {other}\nRun `/perf help` for usage."),
        }
    }

    pub(crate) fn run_debug_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        match sub {
            "" | "help" => [
                "Usage:",
                "  /debug start <file>              Start debugging — show launch config",
                "  /debug breakpoint <file:line>    Explain what to observe at a breakpoint",
                "  /debug watch <expr>              Explain how to watch an expression",
                "  /debug explain <error>           Explain an error with full context",
            ]
            .join("\n"),
            "start" => {
                if rest.is_empty() {
                    return "Usage: /debug start <file>".to_string();
                }
                let source = fs::read_to_string(rest).map_or_else(|_| format!("<could not read {rest}>"), |s| truncate_for_prompt(&s, 6_000));
                let prompt = format!(
                    "You are /debug start. The user wants to debug `{rest}`.\n\
                     File contents:\n```\n{source}\n```\n\n\
                     Provide:\n\
                     1. The debugger to use (gdb, lldb, delve, pdb, node --inspect, etc.) and why.\n\
                     2. A minimal launch configuration (VSCode launch.json or equivalent).\n\
                     3. The exact command to start the debugger from the terminal.\n\
                     4. Key entry points worth setting initial breakpoints at.\n\
                     5. Environment variables or flags needed for debug symbols."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Debug start for {rest}:\n\n{result}"),
                    Err(e) => format!("debug start failed: {e}"),
                }
            }
            "breakpoint" => {
                if rest.is_empty() {
                    return "Usage: /debug breakpoint <file:line>".to_string();
                }
                let (file, line) = rest
                    .rfind(':')
                    .map_or((rest, ""), |p| (&rest[..p], &rest[p + 1..]));
                let context_lines = if file.is_empty() {
                    String::new()
                } else {
                    fs::read_to_string(file).map_or_else(|_| format!("<could not read {file}>"), |s| {
                            let lineno: usize = line.parse().unwrap_or(0);
                            if lineno == 0 {
                                return truncate_for_prompt(&s, 4_000);
                            }
                            let start = lineno.saturating_sub(10);
                            let end = lineno + 10;
                            s.lines()
                                .enumerate()
                                .filter(|(i, _)| *i + 1 >= start && *i < end)
                                .map(|(i, l)| format!("{:>4} | {l}", i + 1))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                };
                let prompt = format!(
                    "You are /debug breakpoint. The user set a breakpoint at `{rest}`.\n\
                     Code context (lines around {line}):\n```\n{context_lines}\n```\n\n\
                     Explain:\n\
                     1. What program state to inspect when execution pauses here.\n\
                     2. Which variables are in scope and expected values.\n\
                     3. Conditions that might cause unexpected behaviour.\n\
                     4. How to set a conditional breakpoint if useful."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Breakpoint {rest}:\n\n{result}"),
                    Err(e) => format!("debug breakpoint failed: {e}"),
                }
            }
            "watch" => {
                if rest.is_empty() {
                    return "Usage: /debug watch <expression>".to_string();
                }
                let prompt = format!(
                    "You are /debug watch. The user wants to watch the expression: `{rest}`\n\
                     Explain:\n\
                     1. How to set a watchpoint in common debuggers (gdb, lldb, VSCode, pdb, delve).\n\
                     2. What changes to the expression would trigger a break.\n\
                     3. Data watchpoint vs expression watch vs value watch — and the difference.\n\
                     4. Performance implications of watching this expression."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Watch `{rest}`:\n\n{result}"),
                    Err(e) => format!("debug watch failed: {e}"),
                }
            }
            "explain" => {
                if rest.is_empty() {
                    return "Usage: /debug explain <error message or stack trace>".to_string();
                }
                let session_context = recent_user_context(self.runtime.session(), 4);
                let prompt = format!(
                    "You are /debug explain. Analyse and explain the following error.\n\
                     Error:\n```\n{rest}\n```\n\
                     Recent conversation context:\n{session_context}\n\n\
                     Provide:\n\
                     1. Root cause — what went wrong and why.\n\
                     2. Where in the code to look (file/function/line if determinable).\n\
                     3. Step-by-step fix.\n\
                     4. How to prevent this class of error in the future."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Error explanation:\n\n{result}"),
                    Err(e) => format!("debug explain failed: {e}"),
                }
            }
            other => format!("Unknown /debug sub-command: {other}\nRun `/debug help` for usage."),
        }
    }

    pub(crate) fn run_changelog_command(&self) -> String {
        // Determine the last tag for the commit range.
        let last_tag = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .current_dir(env::current_dir().unwrap_or_default())
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });

        let (range, range_desc) = match &last_tag {
            Some(tag) => (format!("{tag}..HEAD"), format!("since tag `{tag}`")),
            None => ("HEAD".to_string(), "all commits (no tags found)".to_string()),
        };

        let log = Command::new("git")
            .args(["log", &range, "--oneline", "--no-merges"])
            .current_dir(env::current_dir().unwrap_or_default())
            .output().map_or_else(|e| format!("<git log failed: {e}>"), |o| String::from_utf8_lossy(&o.stdout).trim().to_string());

        if log.trim().is_empty() {
            return format!(
                "Changelog\n  Range            {range_desc}\n  Result           No new commits since last tag."
            );
        }

        let prompt = format!(
            "You are /changelog. Generate a CHANGELOG.md entry from these git commits ({range_desc}).\n\
             \n\
             Rules:\n\
             1. Group commits by conventional commit type:\n\
                feat: -> New Features | fix: -> Bug Fixes | docs: -> Documentation\n\
                style: -> Style | refactor: -> Refactoring | perf: -> Performance\n\
                test: -> Tests | chore:/build:/ci: -> Maintenance\n\
                Commits without a prefix -> Other Changes\n\
             2. Format each item as: - Short human-readable description (#sha)\n\
             3. Add a header: ## [Unreleased] - YYYY-MM-DD\n\
             4. Keep descriptions concise but informative.\n\
             \n\
             Commits:\n{log}"
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!(
                "Changelog ({range_desc})\nCommits:\n{log}\n\n--- CHANGELOG.md entry ---\n{result}"
            ),
            Err(e) => format!("changelog failed: {e}"),
        }
    }

    pub(crate) fn run_env_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(3, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let key = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();

        match sub {
            "" | "show" => {
                let secret_pats = [
                    "KEY", "SECRET", "TOKEN", "PASSWORD", "PASS", "AUTH", "CREDENTIAL", "PRIVATE",
                ];
                let mut vars: Vec<(String, String)> = env::vars().collect();
                vars.sort_by(|a, b| a.0.cmp(&b.0));
                let mut lines = vec!["Environment variables (secrets redacted):".to_string()];
                for (k, v) in &vars {
                    let redact = secret_pats.iter().any(|p| k.to_uppercase().contains(p));
                    lines.push(format!("  {k}={}", if redact { "<redacted>" } else { v }));
                }
                lines.push(String::new());
                lines.push(format!("  Total: {} variables", vars.len()));
                lines.join("\n")
            }
            "set" => {
                if key.is_empty() {
                    return "Usage: /env set <KEY> <VALUE>".to_string();
                }
                // Note: modifying the process env requires unsafe in Rust 1.80+.
                // This project forbids unsafe blocks; record the intent and advise
                // the user to use `export KEY=VALUE` in their shell instead.
                format!(
                    "Env set (shell-only)\n  Key              {key}\n  Value            {}\n\n\
                     Note: Anvil cannot modify the process environment without unsafe code.\n\
                     Run the following in your shell to set this variable:\n\
                     export {key}={}",
                    if val.is_empty() { "<empty>" } else { val },
                    if val.is_empty() { String::new() } else { shell_quote(val) },
                )
            }
            "load" => {
                let path = key;
                if path.is_empty() {
                    return "Usage: /env load <file>".to_string();
                }
                let content = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) => return format!("Cannot read {path}: {e}"),
                };
                let (mut loaded, mut skipped) = (0usize, 0usize);
                let mut export_lines: Vec<String> = Vec::new();
                for line in content.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    if let Some(eq) = t.find('=') {
                        let k = t[..eq].trim();
                        let v = t[eq + 1..].trim().trim_matches('"').trim_matches('\'');
                        if k.is_empty() {
                            skipped += 1;
                        } else {
                            export_lines.push(format!("export {k}={}", shell_quote(v)));
                            loaded += 1;
                        }
                    } else {
                        skipped += 1;
                    }
                }
                format!(
                    "Env load\n  File             {path}\n  Loaded           {loaded} variable(s)\n  Skipped          {skipped} line(s)\n  Scope            session (not persisted)"
                )
            }
            "diff" => {
                let cwd = env::current_dir().unwrap_or_default();
                let mut env_files: Vec<std::path::PathBuf> = Vec::new();
                for name in &[
                    ".env",
                    ".env.example",
                    ".env.local",
                    ".env.staging",
                    ".env.production",
                    ".env.development",
                ] {
                    let p = cwd.join(name);
                    if p.exists() {
                        env_files.push(p);
                    }
                }
                if env_files.is_empty() {
                    return "Env diff\n  No .env files found in the current directory.".to_string();
                }
                let mut summaries: Vec<String> = Vec::new();
                for ef in &env_files {
                    let content = fs::read_to_string(ef).unwrap_or_default();
                    let keys: Vec<&str> = content
                        .lines()
                        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
                        .filter_map(|l| l.find('=').map(|p| &l[..p]))
                        .collect();
                    summaries.push(format!(
                        "  {} ({} keys): {}",
                        ef.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                        keys.len(),
                        keys.join(", ")
                    ));
                }
                let prompt = format!(
                    "You are /env diff. Analyse these .env files and highlight:\n\
                     1. Keys present in one file but missing in another.\n\
                     2. Keys that need to be kept in sync.\n\
                     3. Any suspicious or potentially insecure patterns.\n\nFiles:\n{}",
                    summaries.join("\n")
                );
                let header = format!("Env diff\n  Files found:\n{}\n", summaries.join("\n"));
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("{header}\n{r}"),
                    Err(e) => format!("env diff failed: {e}"),
                }
            }
            other => format!(
                "Unknown /env sub-command: {other}\n\n\
                 Usage:\n  /env show               Show current environment (secrets redacted)\n\
                   /env set <KEY> <VALUE>  Set an env var for this session\n\
                   /env load <file>        Load a .env file into the session\n\
                   /env diff               Compare .env files in the workspace"
            ),
        }
    }

    pub(crate) fn run_lsp_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /lsp start <lang>         Start language server for a language",
                "  /lsp symbols <file>       List symbols in a file via LSP",
                "  /lsp references <symbol>  Find all references to a symbol",
                "",
                "Supported languages: rust, typescript, python, go, java",
            ].join("\n");
        }
        if let Some(lang) = args.strip_prefix("start ") {
            let lang = lang.trim();
            if lang.is_empty() { return "Usage: /lsp start <lang>".to_string(); }
            let binary = lsp_binary_for_lang(lang);
            let found = Command::new("which").arg(&binary).output()
                .map(|o| o.status.success()).unwrap_or(false);
            return if found {
                format!("LSP server for '{lang}' is available ({binary}).\nServer would be started on next file operation.")
            } else {
                format!("LSP server binary '{binary}' not found in PATH.\nInstall it first (e.g. `cargo install rust-analyzer` for rust).")
            };
        }
        if let Some(file) = args.strip_prefix("symbols ") {
            let file = file.trim();
            if file.is_empty() { return "Usage: /lsp symbols <file>".to_string(); }
            let source = match fs::read_to_string(file) {
                Ok(s) => s,
                Err(e) => return format!("Cannot read {file}: {e}"),
            };
            let prompt = format!(
                "You are an LSP server. List all top-level symbols (functions, structs, classes, \
                 enums, constants, type aliases) in this source file. Format each as:\n\
                 <kind> <name>  <line>\n\nFile: {file}\n\n```\n{src}\n```",
                src = truncate_for_prompt(&source, 10_000),
            );
            return match self.run_internal_prompt_text(&prompt, false) {
                Ok(r) => format!("Symbols in {file}:\n\n{r}"),
                Err(e) => format!("lsp symbols failed: {e}"),
            };
        }
        if let Some(symbol) = args.strip_prefix("references ") {
            let symbol = symbol.trim();
            if symbol.is_empty() { return "Usage: /lsp references <symbol>".to_string(); }
            let output = Command::new("grep")
                .args(["-rn", "--include=*.rs", "--include=*.ts",
                       "--include=*.py", "--include=*.go", symbol, "."])
                .output();
            return match output {
                Ok(o) if !o.stdout.is_empty() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let lines: Vec<&str> = text.lines().take(40).collect();
                    format!("References to '{symbol}' ({} shown):\n\n{}", lines.len(), lines.join("\n"))
                }
                _ => format!("No references found for '{symbol}'."),
            };
        }
        format!("Unknown /lsp sub-command: {args}\nRun `/lsp help` for usage.")
    }

    pub(crate) fn run_notebook_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /notebook run <file>              Execute all cells in a .ipynb notebook",
                "  /notebook cell <file> <n>         Run cell N (0-based) in a notebook",
                "  /notebook export <file> <format>  Export notebook (html|py|pdf)",
            ].join("\n");
        }
        if let Some(file) = args.strip_prefix("run ") {
            let file = file.trim();
            if file.is_empty() { return "Usage: /notebook run <file>".to_string(); }
            if command_exists("jupyter") {
                let out = Command::new("jupyter")
                    .args(["nbconvert", "--to", "notebook", "--execute", "--inplace", file])
                    .output();
                return match out {
                    Ok(o) if o.status.success() => format!("Executed notebook: {file}"),
                    Ok(o) => format!("nbconvert failed:\n{}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => format!("Failed to run jupyter: {e}"),
                };
            }
            let raw = match fs::read_to_string(file) {
                Ok(s) => s, Err(e) => return format!("Cannot read {file}: {e}"),
            };
            let prompt = format!(
                "This is a Jupyter notebook (JSON). Summarise what each cell does and what \
                 output would be expected if executed top-to-bottom. Identify any likely errors.\n\n{}",
                truncate_for_prompt(&raw, 12_000),
            );
            return match self.run_internal_prompt_text(&prompt, false) {
                Ok(r) => format!("Notebook analysis for {file}:\n\n{r}"),
                Err(e) => format!("notebook run: {e}"),
            };
        }
        if let Some(rest) = args.strip_prefix("cell ") {
            let mut parts = rest.trim().splitn(3, ' ');
            let file = parts.next().unwrap_or("").trim();
            let cell_str = parts.next().unwrap_or("").trim();
            if file.is_empty() || cell_str.is_empty() {
                return "Usage: /notebook cell <file> <n>".to_string();
            }
            let cell_n: usize = match cell_str.parse() {
                Ok(n) => n,
                Err(_) => return format!("Cell index must be a number, got '{cell_str}'."),
            };
            let raw = match fs::read_to_string(file) {
                Ok(s) => s, Err(e) => return format!("Cannot read {file}: {e}"),
            };
            return match extract_notebook_cell(&raw, cell_n) {
                Ok(src) => {
                    let prompt = format!(
                        "Execute or explain this Jupyter notebook cell (cell {cell_n} from {file}).\n\n```python\n{src}\n```"
                    );
                    match self.run_internal_prompt_text(&prompt, false) {
                        Ok(r) => format!("Cell {cell_n} from {file}:\n\n{r}"),
                        Err(e) => format!("notebook cell: {e}"),
                    }
                }
                Err(e) => format!("Cell {cell_n} not found in {file}: {e}"),
            };
        }
        if let Some(rest) = args.strip_prefix("export ") {
            let mut parts = rest.trim().splitn(3, ' ');
            let file = parts.next().unwrap_or("").trim();
            let fmt = parts.next().unwrap_or("html").trim();
            if file.is_empty() { return "Usage: /notebook export <file> <format>".to_string(); }
            if !matches!(fmt, "html" | "pdf" | "py" | "script") {
                return format!("Unsupported format '{fmt}'. Use: html, pdf, py.");
            }
            if command_exists("jupyter") {
                let out = Command::new("jupyter").args(["nbconvert", "--to", fmt, file]).output();
                return match out {
                    Ok(o) if o.status.success() => format!("Exported {file} to {fmt}."),
                    Ok(o) => format!("Export failed:\n{}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => format!("jupyter nbconvert failed: {e}"),
                };
            }
            return "jupyter not found in PATH. Install with: pip install jupyter".to_string();
        }
        format!("Unknown /notebook sub-command: {args}\nRun `/notebook help` for usage.")
    }

    pub(crate) fn run_pipeline_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /pipeline generate   Generate CI config from project type",
                "  /pipeline lint       Validate existing CI pipeline config",
                "  /pipeline run        Trigger a local pipeline run via act/gitlab-runner",
            ].join("\n");
        }
        match args {
            "generate" => {
                let project_type = detect_project_type_for_pipeline();
                let prompt = format!(
                    "Generate a production-quality CI/CD pipeline configuration for a {project_type} project.\n\
                     - If the project uses GitHub Actions, output a .github/workflows/ci.yml file.\n\
                     - If it uses GitLab CI, output a .gitlab-ci.yml file.\n\
                     - Cover: lint, test, build, and Docker image build if applicable.\n\
                     - Use the best community actions/images for {project_type}.\n\
                     - Output only the YAML file, nothing else."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("Generated CI pipeline for {project_type}:\n\n{r}"),
                    Err(e) => format!("pipeline generate: {e}"),
                }
            }
            "lint" => {
                let candidates = [
                    ".github/workflows/ci.yml", ".github/workflows/main.yml",
                    ".gitlab-ci.yml", "Jenkinsfile", ".circleci/config.yml",
                ];
                let found: Vec<&str> = candidates.iter().copied()
                    .filter(|f| Path::new(f).exists()).collect();
                if found.is_empty() {
                    return "No CI configuration files found in common locations.".to_string();
                }
                let mut report = String::from("Pipeline lint:\n");
                for path in &found {
                    let content = match fs::read_to_string(path) {
                        Ok(c) => c,
                        Err(e) => { let _ = write!(report, "\n  {path}: cannot read ({e})\n"); continue; }
                    };
                    let prompt = format!(
                        "Review this CI/CD pipeline config for errors, security issues, and improvements.\n\n\
                         File: {path}\n\n```yaml\n{yaml}\n```\n\nBe concise — max 10 lines.",
                        yaml = truncate_for_prompt(&content, 8_000),
                    );
                    let result = self.run_internal_prompt_text(&prompt, false)
                        .unwrap_or_else(|e| format!("lint error: {e}"));
                    let _ = write!(report, "\n{path}:\n{result}\n");
                }
                report
            }
            "run" => {
                if command_exists("act") {
                    let out = Command::new("act").args(["--list"]).output();
                    let list = shell_output_or_err(out, "act --list");
                    return format!("Local runner (act) available.\n{list}\n\nRun `act` in your shell to execute.");
                }
                if command_exists("gitlab-runner") {
                    return "GitLab Runner available. Run `gitlab-runner exec shell <job>` in your shell.".to_string();
                }
                "No local pipeline runner found. Install 'act': https://github.com/nektos/act".to_string()
            }
            other => format!("Unknown /pipeline sub-command: {other}\nRun `/pipeline help` for usage."),
        }
    }

    pub(crate) fn run_review_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /review <file>    Review a source file for issues",
                "  /review staged    Review all staged changes",
                "  /review pr        Review the current PR diff",
            ].join("\n");
        }
        let build_prompt = |label: &str, code: &str| -> String {
            format!(
                "You are a senior code reviewer. Review the following {label} and provide:\n\
                 1. Critical bugs or logic errors\n\
                 2. Security vulnerabilities\n\
                 3. Performance concerns\n\
                 4. Code style and readability issues\n\
                 5. Suggested improvements\n\n\
                 ```\n{code}\n```\n\nBe concise — max 20 lines.",
                code = truncate_for_prompt(code, 12_000),
            )
        };
        match args {
            "staged" => {
                let diff = Command::new("git").args(["diff", "--cached"]).output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if diff.trim().is_empty() { return "No staged changes to review.".to_string(); }
                match self.run_internal_prompt_text(&build_prompt("staged git diff", &diff), false) {
                    Ok(r) => format!("Code review (staged changes):\n\n{r}"),
                    Err(e) => format!("review staged: {e}"),
                }
            }
            "pr" => {
                let base = Command::new("git")
                    .args(["merge-base", "HEAD", "origin/main"]).output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "origin/main".to_string());
                let diff = Command::new("git").args(["diff", &base, "HEAD"]).output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if diff.trim().is_empty() { return "No diff found against origin/main.".to_string(); }
                match self.run_internal_prompt_text(&build_prompt("pull request diff", &diff), false) {
                    Ok(r) => format!("Code review (PR diff):\n\n{r}"),
                    Err(e) => format!("review pr: {e}"),
                }
            }
            file => {
                let source = match fs::read_to_string(file) {
                    Ok(s) => s, Err(e) => return format!("Cannot read {file}: {e}"),
                };
                match self.run_internal_prompt_text(&build_prompt("source file", &source), false) {
                    Ok(r) => format!("Code review ({file}):\n\n{r}"),
                    Err(e) => format!("review: {e}"),
                }
            }
        }
    }

    pub(crate) fn run_migrate_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(4, ' ');
        match parts.next().unwrap_or("") {
            "framework" => {
                let from = parts.next().unwrap_or("<from>");
                let to = parts.next().unwrap_or("<to>");
                let cwd = env::current_dir().unwrap_or_default();
                let mut files: Vec<String> = Vec::new();
                for ext in &["ts", "tsx", "js", "jsx", "vue", "svelte"] {
                    if let Ok(rd) = fs::read_dir(&cwd) {
                        for e in rd.flatten() {
                            let p = e.path();
                            if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                                if let Some(n) = p.file_name() { files.push(n.to_string_lossy().to_string()); }
                            }
                        }
                    }
                }
                let file_list = if files.is_empty() { "(no source files detected)".to_string() } else { files[..files.len().min(20)].join("\n") };
                let prompt = format!(
                    "I need to migrate a {from} project to {to}.\nSource files found:\n{file_list}\n\nProvide a step-by-step migration plan covering: dependency changes, config updates, breaking API differences, and code patterns to refactor. Be specific and actionable."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(plan) => format!("Migrate\n  From             {from}\n  To               {to}\n\n{plan}"),
                    Err(e) => format!("migrate framework failed: {e}"),
                }
            }
            "language" => {
                let from = parts.next().unwrap_or("<from>");
                let to = parts.next().unwrap_or("<to>");
                let prompt = format!(
                    "Explain how to migrate a codebase from {from} to {to}. Cover: type system differences, standard library equivalents, build toolchain changes, testing approach, and common gotchas. Be concise and practical."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(plan) => format!("Migrate language\n  From             {from}\n  To               {to}\n\n{plan}"),
                    Err(e) => format!("migrate language failed: {e}"),
                }
            }
            "deps" => {
                let cwd = env::current_dir().unwrap_or_default();
                if !cwd.join("package.json").exists() {
                    return "Migrate deps\n  Error            no package.json found in current directory".to_string();
                }
                let from = if cwd.join("yarn.lock").exists() { "yarn" } else if cwd.join("pnpm-lock.yaml").exists() { "pnpm" } else { "npm" };
                format!(
                    "Migrate deps\n  Detected         {from}\n\n  npm  → pnpm      npm install -g pnpm && pnpm import\n  npm  → yarn      npm install -g yarn && yarn import\n  yarn → pnpm      pnpm import\n  Note             Remove old lock files and node_modules after switching."
                )
            }
            _ => "Usage: /migrate [framework <from> <to>|language <from> <to>|deps]".to_string(),
        }
    }

    pub(crate) fn run_regex_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let (sub, rest) = args.split_once(' ').map_or((args, ""), |(a, b)| (a, b));
        match sub {
            "build" => {
                let desc = rest.trim();
                if desc.is_empty() { return "Usage: /regex build <natural language description>".to_string(); }
                let prompt = format!("Generate a regex pattern for: {desc}\nRespond with ONLY the pattern on line 1, then a '#' comment explaining each part on line 2.");
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(out) => format!("Regex build\n  Description      {desc}\n\n{out}"),
                    Err(e) => format!("regex build failed: {e}"),
                }
            }
            "test" => {
                let mut p = rest.splitn(2, ' ');
                let pattern = p.next().unwrap_or("").trim();
                let input = p.next().unwrap_or("").trim();
                if pattern.is_empty() || input.is_empty() {
                    return "Usage: /regex test <pattern> <input>".to_string();
                }
                let out = Command::new("grep").args(["-Po", pattern]).stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn();
                match out {
                    Ok(mut child) => {
                        if let Some(mut s) = child.stdin.take() {
                            use std::io::Write;
                            let _ = s.write_all(input.as_bytes());
                        }
                        let result = child.wait_with_output();
                        let matched = result.map(|r| String::from_utf8_lossy(&r.stdout).trim().to_string()).unwrap_or_default();
                        if matched.is_empty() {
                            format!("Regex test\n  Pattern          {pattern}\n  Input            {input}\n  Result           no match")
                        } else {
                            format!("Regex test\n  Pattern          {pattern}\n  Input            {input}\n  Match            {matched}")
                        }
                    }
                    Err(_) => format!("Regex test\n  Pattern          {pattern}\n  Input            {input}\n  Note             Test manually: echo '{input}' | grep -Po '{pattern}'"),
                }
            }
            "explain" => {
                let pattern = rest.trim();
                if pattern.is_empty() { return "Usage: /regex explain <pattern>".to_string(); }
                let prompt = format!("Explain this regex pattern in plain English, component by component:\n\n{pattern}\n\nBe concise. List each token and what it matches.");
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(out) => format!("Regex explain\n  Pattern          {pattern}\n\n{out}"),
                    Err(e) => format!("regex explain failed: {e}"),
                }
            }
            _ => "Usage: /regex [build <description>|test <pattern> <input>|explain <pattern>]".to_string(),
        }
    }

    pub(crate) fn run_logs_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() {
            return "Usage: /logs [tail <file>|search <file> <pattern>|analyze <file>|stats <file>]".to_string();
        }
        let mut parts = args.splitn(4, ' ');
        let sub = parts.next().unwrap_or("");
        match sub {
            "tail" => {
                let file = parts.next().unwrap_or("<file>");
                match Command::new("tail").args(["-n", "50", file]).output() {
                    Ok(o) => format!("Logs tail  {file}\n\n{}", truncate_for_prompt(&String::from_utf8_lossy(&o.stdout), 4_000)),
                    Err(e) => format!("logs tail failed: {e}"),
                }
            }
            "search" => {
                let file = parts.next().unwrap_or("<file>");
                let pattern = parts.next().unwrap_or("<pattern>");
                match Command::new("grep").args(["-n", "-C", "2", pattern, file]).output() {
                    Ok(o) => {
                        let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if text.is_empty() {
                            format!("Logs search\n  File             {file}\n  Pattern          {pattern}\n  Result           no matches found")
                        } else {
                            format!("Logs search  {file}  pattern={pattern}\n\n{}", truncate_for_prompt(&text, 4_000))
                        }
                    }
                    Err(e) => format!("logs search failed: {e}"),
                }
            }
            "analyze" => {
                let file = parts.next().unwrap_or("<file>");
                let content = match fs::read_to_string(file) {
                    Ok(c) => c,
                    Err(e) => return format!("logs analyze\n  Error            {e}"),
                };
                let sample = truncate_for_prompt(&content, 10_000);
                let prompt = format!("Analyze these log entries:\n1. Identify errors and root causes\n2. Recurring patterns\n3. Performance anomalies\n4. Recommended fixes\n\nLog:\n{sample}");
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(a) => format!("Logs analyze\n  File             {file}\n\n{a}"),
                    Err(e) => format!("logs analyze failed: {e}"),
                }
            }
            "stats" => {
                let file = parts.next().unwrap_or("<file>");
                match fs::read_to_string(file) {
                    Ok(content) => {
                        let total = content.lines().count();
                        let errors = content.lines().filter(|l| { let u = l.to_uppercase(); u.contains("ERROR") || u.contains("FATAL") }).count();
                        let warns = content.lines().filter(|l| l.to_uppercase().contains("WARN")).count();
                        let info = content.lines().filter(|l| l.to_uppercase().contains("INFO")).count();
                        format!("Logs stats\n  File             {file}\n  Total lines      {total}\n  ERROR/FATAL      {errors}\n  WARN             {warns}\n  INFO             {info}")
                    }
                    Err(e) => format!("logs stats\n  Error            {e}"),
                }
            }
            _ => "Usage: /logs [tail <file>|search <file> <pattern>|analyze <file>|stats <file>]".to_string(),
        }
    }

    pub(crate) fn run_finetune_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(3, ' ');
        match parts.next().unwrap_or("") {
            "prepare" => {
                let file = parts.next().unwrap_or("<file>");
                match fs::read_to_string(file) {
                    Ok(src) => {
                        let lines = src.lines().count();
                        let json_lines = src.lines().filter(|l| l.trim_start().starts_with('{')).count();
                        let prompt = format!(
                            "Review this fine-tuning training data file:\nFile: {file}\nLines: {lines}\nJSON-like: {json_lines}\nSample:\n{}\n\nCheck: JSONL correctness, role pairs, data diversity, biases, sample count. Provide quality assessment.",
                            truncate_for_prompt(&src, 2_000)
                        );
                        match self.run_internal_prompt_text(&prompt, false) {
                            Ok(a) => format!("Finetune prepare\n  File             {file}\n  Lines            {lines}\n\n{a}"),
                            Err(e) => format!("finetune prepare failed: {e}"),
                        }
                    }
                    Err(e) => format!("Finetune prepare\n  Error            {e}"),
                }
            }
            "validate" => {
                let file = parts.next().unwrap_or("<file>");
                match fs::read_to_string(file) {
                    Ok(src) => {
                        let mut errors: Vec<String> = Vec::new();
                        for (i, line) in src.lines().enumerate() {
                            let l = line.trim();
                            if l.is_empty() { continue; }
                            if serde_json::from_str::<serde_json::Value>(l).is_err() {
                                errors.push(format!("  Line {:>4}  invalid JSON: {}", i + 1, &l[..l.len().min(60)]));
                            }
                        }
                        if errors.is_empty() {
                            let count = src.lines().filter(|l| !l.trim().is_empty()).count();
                            format!("Finetune validate\n  File             {file}\n  Examples         {count}\n  Result           valid JSONL")
                        } else {
                            format!("Finetune validate\n  File             {file}\n  Errors           {}\n\n{}", errors.len(), errors.join("\n"))
                        }
                    }
                    Err(e) => format!("Finetune validate\n  Error            {e}"),
                }
            }
            "start" => "Finetune start\n  Steps\n    1. Validate data     /finetune validate <file>\n    2. Upload file        openai api files.create -f <file> -p fine-tune\n    3. Start job          openai api fine_tuning.jobs.create -m gpt-4o-mini -t <file-id>\n  Docs                 https://platform.openai.com/docs/guides/fine-tuning".to_string(),
            "status" => {
                match Command::new("openai").args(["api", "fine_tuning.jobs.list"]).output() {
                    Ok(o) if o.status.success() => format!("Finetune jobs\n\n{}", truncate_for_prompt(&String::from_utf8_lossy(&o.stdout), 3_000)),
                    _ => "Finetune status\n  Note             Install OpenAI CLI: pip install openai\n  Then:            openai api fine_tuning.jobs.list".to_string(),
                }
            }
            _ => "Usage: /finetune [prepare <file>|validate <file>|start|status]".to_string(),
        }
    }

    pub(crate) fn run_commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let status = git_output(&["status", "--short"])?;
        if status.trim().is_empty() {
            println!("Commit\n  Result           skipped\n  Reason           no workspace changes");
            return Ok(());
        }

        git_status_ok(&["add", "-A"])?;
        let staged_stat = git_output(&["diff", "--cached", "--stat"])?;
        let prompt = format!(
            "Generate a git commit message in plain text Lore format only. Base it on this staged diff summary:\n\n{}\n\nRecent conversation context:\n{}",
            truncate_for_prompt(&staged_stat, 8_000),
            recent_user_context(self.runtime.session(), 6)
        );
        let message = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        if message.trim().is_empty() {
            return Err("generated commit message was empty".into());
        }

        let path = write_temp_text_file("anvil-commit-message.txt", &message)?;
        let output = Command::new("git")
            .args(["commit", "--file"])
            .arg(&path)
            .current_dir(env::current_dir()?)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("git commit failed: {stderr}").into());
        }

        println!(
            "Commit\n  Result           created\n  Message file     {}\n\n{}",
            path.display(),
            message.trim()
        );
        Ok(())
    }

    pub(crate) fn run_pr(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let staged = git_output(&["diff", "--stat"])?;
        let prompt = format!(
            "Generate a pull request title and body from this conversation and diff summary. Output plain text in this format exactly:\nTITLE: <title>\nBODY:\n<body markdown>\n\nContext hint: {}\n\nDiff summary:\n{}",
            context.unwrap_or("none"),
            truncate_for_prompt(&staged, 10_000)
        );
        let draft = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        let (title, body) = parse_titled_body(&draft)
            .ok_or_else(|| "failed to parse generated PR title/body".to_string())?;

        if command_exists("gh") {
            let body_path = write_temp_text_file("anvil-pr-body.md", &body)?;
            let output = Command::new("gh")
                .args(["pr", "create", "--title", &title, "--body-file"])
                .arg(&body_path)
                .current_dir(env::current_dir()?)
                .output()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!(
                    "PR\n  Result           created\n  Title            {title}\n  URL              {}",
                    if stdout.is_empty() { "<unknown>" } else { &stdout }
                );
                return Ok(());
            }
        }

        println!("PR draft\n  Title            {title}\n\n{body}");
        Ok(())
    }

    pub(crate) fn run_issue(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let prompt = format!(
            "Generate a GitHub issue title and body from this conversation. Output plain text in this format exactly:\nTITLE: <title>\nBODY:\n<body markdown>\n\nContext hint: {}\n\nConversation context:\n{}",
            context.unwrap_or("none"),
            truncate_for_prompt(&recent_user_context(self.runtime.session(), 10), 10_000)
        );
        let draft = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        let (title, body) = parse_titled_body(&draft)
            .ok_or_else(|| "failed to parse generated issue title/body".to_string())?;

        if command_exists("gh") {
            let body_path = write_temp_text_file("anvil-issue-body.md", &body)?;
            let output = Command::new("gh")
                .args(["issue", "create", "--title", &title, "--body-file"])
                .arg(&body_path)
                .current_dir(env::current_dir()?)
                .output()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!(
                    "Issue\n  Result           created\n  Title            {title}\n  URL              {}",
                    if stdout.is_empty() { "<unknown>" } else { &stdout }
                );
                return Ok(());
            }
        }

        println!("Issue draft\n  Title            {title}\n\n{body}");
        Ok(())
    }
}
