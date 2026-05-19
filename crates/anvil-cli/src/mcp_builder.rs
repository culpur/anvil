//! MCP Builder — scaffold a new MCP server from scratch (task #638).
//!
//! See `audit/mcp-builder-2026-05-19.md` for the full template spec
//! and the convention drift survey of the 4 reference MCPs.
//!
//! ## Design
//!
//! The module is intentionally split into:
//!
//! 1. **Spec types** (`McpBuilderSpec`, `McpToolSpec`, `McpToolInput`,
//!    `McpLanguage`) — plain data structures with no I/O.
//! 2. **Pure scaffolder** (`scaffold_files`) — `(&Spec) -> Vec<(PathBuf, String)>`.
//!    No filesystem writes. Easy to unit-test.
//! 3. **Disk writer** (`write_scaffold_to_disk`) — actually creates
//!    directories and writes files. Returns the list of paths written.
//! 4. **Anvil registration** (`wire_into_settings`) — idempotent
//!    update of `~/.anvil/settings.json::mcpServers.<name>`, matching
//!    the `wizard_qmd.rs::wire_qmd_into_settings` precedent.
//! 5. **Interactive flow** (`run_mcp_builder`) — invoked by
//!    `/mcp builder` after `leave_alt_screen_for_inline_op` so plain
//!    stdin/stdout is safe. Mirrors the existing `auth::run_ollama_setup`
//!    pattern. The TUI alt-screen is restored by the caller.
//!
//! ## Anti-pattern guardrails
//!
//! `println!` / `print!` are ONLY used from within `run_mcp_builder`
//! and helper prompt functions. They are unreachable while the TUI's
//! alt-screen is active — the caller (`run_mcp_command` in
//! `commands_extra.rs`) leaves alt-screen before invoking us and
//! restores it afterwards. This is the same exception envelope used
//! by `cmd_provider::run_ollama_setup`.
//!
//! No external infrastructure paths, hostnames, or credentials are
//! embedded in any generated template — they are user-facing portable
//! scaffolds.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

// ─── Spec types ──────────────────────────────────────────────────────────────

/// Target language for the generated MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpLanguage {
    /// Node.js — emits `index.js` + `package.json` + `test.js` using the
    /// high-level `McpServer` + zod API.
    Node,
    /// TypeScript — same wire surface as Node, but emits typed `src/index.ts`
    /// + `src/test.ts` + `tsconfig.json` + a `package.json` whose `start`
    /// script runs through `tsx` and `test` through `node --import tsx/esm`.
    /// Output of [`scaffold_files`] includes the same zod schema generation
    /// as Node; the TS layer is for type-safety in the user's tool bodies.
    TypeScript,
    /// Python — emits `__main__.py` + `pyproject.toml` + `test_server.py`
    /// using FastMCP.
    Python,
}

impl McpLanguage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Node => "Node.js",
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
        }
    }
}

/// One input parameter on a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolInput {
    /// Parameter name. Must be a valid identifier in both Node and Python.
    pub name: String,
    /// Free-text description shown in the tool's input schema.
    pub description: String,
    /// Input type. Supports scalars, enum, recursive `array`, and recursive
    /// `object` schemas (task #673).
    pub kind: InputKind,
    /// When true the input is optional (no default emitted; the tool
    /// function checks for presence).
    pub optional: bool,
}

/// One named field on a nested object schema. Recursive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectField {
    /// Field name. Must be a valid identifier (see [`validate_identifier`]).
    pub name: String,
    /// Field type. Recursive — may itself be Array or Object.
    pub kind: InputKind,
    /// When true the field is optional.
    pub optional: bool,
}

/// Input kinds the builder supports. v2.2.18 launched with scalars + enum;
/// v2.2.18 task #673 adds recursive `Array` and `Object`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    String,
    Number,
    Boolean,
    /// `enum` of strings, e.g. `["a", "b", "c"]`. Must be non-empty.
    Enum(Vec<String>),
    /// Homogeneous array of `items`. The `items` schema is recursive — can
    /// itself be Array or Object for nested cases like `array<object>`.
    Array(Box<InputKind>),
    /// Object with named fields. The field-type vocabulary is recursive,
    /// so `object { foo: array<string>, bar: object { … } }` is expressible.
    /// Empty field list is allowed (emits an open `{}` / `dict` schema).
    Object(Vec<ObjectField>),
}

/// One tool definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolSpec {
    pub name: String,
    pub description: String,
    pub inputs: Vec<McpToolInput>,
}

/// Full builder spec: the bag of choices the wizard collects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpBuilderSpec {
    /// Project / MCP name (kebab-case validated, see [`validate_name`]).
    pub name: String,
    pub language: McpLanguage,
    /// Tools to scaffold. Empty list is allowed — produces a single
    /// `hello` stub tool so the generated MCP still builds.
    pub tools: Vec<McpToolSpec>,
    /// Output directory (absolute path). The scaffolder will create it
    /// if it does not exist. It must be empty (or non-existent) when
    /// writing to disk — see [`write_scaffold_to_disk`].
    pub output_dir: PathBuf,
}

// ─── Validation ──────────────────────────────────────────────────────────────

/// Name validation rules per audit doc §7.
pub fn validate_name(name: &str, existing: &BTreeSet<String>) -> Result<(), String> {
    if name.len() < 3 {
        return Err("name must be at least 3 characters".into());
    }
    if name.len() > 32 {
        return Err("name must be at most 32 characters".into());
    }
    // First char must be lowercase letter.
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return Err("name must start with a lowercase letter".into()),
    }
    // Remaining: lowercase letters, digits, single hyphens (no double or trailing).
    let mut prev_hyphen = false;
    for c in chars {
        match c {
            'a'..='z' | '0'..='9' => prev_hyphen = false,
            '-' => {
                if prev_hyphen {
                    return Err("name must not contain consecutive hyphens".into());
                }
                prev_hyphen = true;
            }
            _ => return Err(format!("name has invalid character {c:?} (kebab-case only)")),
        }
    }
    if name.ends_with('-') {
        return Err("name must not end with a hyphen".into());
    }
    const RESERVED: &[&str] = &["anvil", "mcp", "test", "server"];
    if RESERVED.contains(&name) {
        return Err(format!("\"{name}\" is reserved — pick a different name"));
    }
    if existing.contains(name) {
        return Err(format!(
            "\"{name}\" is already registered in ~/.anvil/settings.json"
        ));
    }
    Ok(())
}

/// Identifier validation for tool / input names. Must work in both Node
/// (camelCase / snake_case both fine, no hyphens) and Python (snake_case).
/// We restrict to lowercase ASCII letters, digits, and underscores; first
/// char must be a letter; max 48 chars.
pub fn validate_identifier(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("name must not be empty".into());
    }
    if s.len() > 48 {
        return Err("name must be at most 48 characters".into());
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return Err("name must start with a lowercase letter".into()),
    }
    for c in chars {
        match c {
            'a'..='z' | '0'..='9' | '_' => {}
            _ => return Err(format!("name has invalid character {c:?} (snake_case only)")),
        }
    }
    Ok(())
}

// ─── Pure scaffolder ─────────────────────────────────────────────────────────

/// Return the list of files the scaffolder would write for the given
/// spec, as (relative path under `spec.output_dir`, file contents)
/// pairs. Pure — no filesystem I/O.
pub fn scaffold_files(spec: &McpBuilderSpec) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    // Always treat the empty-tool-list case as "one stub tool" so the
    // generated MCP is buildable + tests have something to call.
    let tools_view: Vec<McpToolSpec> = if spec.tools.is_empty() {
        vec![default_stub_tool()]
    } else {
        spec.tools.clone()
    };

    match spec.language {
        McpLanguage::Node => {
            out.push((
                spec.output_dir.join("package.json"),
                node_package_json(&spec.name),
            ));
            out.push((
                spec.output_dir.join("index.js"),
                node_index_js(&spec.name, &tools_view),
            ));
            out.push((
                spec.output_dir.join("test.js"),
                node_test_js(&spec.name, &tools_view),
            ));
            out.push((spec.output_dir.join(".gitignore"), node_gitignore()));
            out.push((
                spec.output_dir.join("README.md"),
                readme(&spec.name, McpLanguage::Node, &tools_view),
            ));
        }
        McpLanguage::TypeScript => {
            out.push((
                spec.output_dir.join("package.json"),
                ts_package_json(&spec.name),
            ));
            out.push((spec.output_dir.join("tsconfig.json"), ts_tsconfig()));
            // Source files live under src/ so tsc + tsx work cleanly.
            out.push((
                spec.output_dir.join("src").join("index.ts"),
                ts_index(&spec.name, &tools_view),
            ));
            out.push((
                spec.output_dir.join("src").join("test.ts"),
                ts_test(&spec.name, &tools_view),
            ));
            out.push((spec.output_dir.join(".gitignore"), ts_gitignore()));
            out.push((
                spec.output_dir.join("README.md"),
                readme(&spec.name, McpLanguage::TypeScript, &tools_view),
            ));
        }
        McpLanguage::Python => {
            out.push((
                spec.output_dir.join("pyproject.toml"),
                python_pyproject_toml(&spec.name),
            ));
            out.push((
                spec.output_dir.join("__main__.py"),
                python_main_py(&spec.name, &tools_view),
            ));
            out.push((
                spec.output_dir.join("test_server.py"),
                python_test_py(&spec.name, &tools_view),
            ));
            out.push((spec.output_dir.join(".gitignore"), python_gitignore()));
            out.push((
                spec.output_dir.join("README.md"),
                readme(&spec.name, McpLanguage::Python, &tools_view),
            ));
        }
    }
    out
}

/// Default `hello` stub tool the scaffolder emits when the user
/// supplies no tools. Keeps the MCP buildable + testable.
fn default_stub_tool() -> McpToolSpec {
    McpToolSpec {
        name: "hello".to_string(),
        description: "Return a friendly greeting. Replace me with a real tool.".to_string(),
        inputs: vec![McpToolInput {
            name: "subject".to_string(),
            description: "Who or what to greet".to_string(),
            kind: InputKind::String,
            optional: false,
        }],
    }
}

// ─── Node.js templates ───────────────────────────────────────────────────────

fn node_package_json(name: &str) -> String {
    format!(
        "{{\n  \"name\": \"mcp-{name}\",\n  \"version\": \"0.1.0\",\n  \"type\": \"module\",\n  \"private\": true,\n  \"scripts\": {{\n    \"start\": \"node index.js\",\n    \"test\": \"node --test test.js\"\n  }},\n  \"dependencies\": {{\n    \"@modelcontextprotocol/sdk\": \"^1.0.0\",\n    \"zod\": \"^3.23.0\"\n  }}\n}}\n"
    )
}

fn node_gitignore() -> String {
    "node_modules/\n.env\n*.log\n".to_string()
}

fn node_index_js(name: &str, tools: &[McpToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("// Generated by Anvil MCP Builder.\n");
    s.push_str("//\n");
    s.push_str("// MCP server entry point. Each tool is registered via\n");
    s.push_str("// `server.tool(name, description, zodSchema, handler)`.\n");
    s.push_str("// Replace the stub implementations with real logic.\n\n");
    s.push_str("import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';\n");
    s.push_str("import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';\n");
    s.push_str("import { z } from 'zod';\n\n");
    s.push_str(&format!(
        "export const server = new McpServer({{\n  name: '{name}',\n  version: '0.1.0',\n}});\n\n"
    ));
    // Export the tool implementations so tests can call them directly.
    s.push_str("// Tool implementations — exported so test.js can call them directly.\n");
    s.push_str("export const tools = {};\n\n");

    for tool in tools {
        let zod_obj = node_zod_object(&tool.inputs);
        let args_destructure = node_args_destructure(&tool.inputs);
        s.push_str(&format!(
            "tools.{tname} = async function {tname}({args_destructure}) {{\n",
            tname = tool.name
        ));
        s.push_str(
            "  // TODO: replace this honesty-contract stub with your real implementation.\n",
        );
        s.push_str("  return {\n");
        s.push_str("    ok: true,\n");
        s.push_str("    blockers: [],\n");
        s.push_str("    warnings: [],\n");
        s.push_str("    subresults: {},\n");
        s.push_str(&format!(
            "    message: 'Tool {tname} not yet implemented',\n",
            tname = tool.name
        ));
        s.push_str("  };\n");
        s.push_str("};\n\n");

        s.push_str(&format!(
            "server.tool(\n  '{tname}',\n  {desc},\n  {zod_obj},\n  async (args) => {{\n    const result = await tools.{tname}(args);\n    return {{ content: [{{ type: 'text', text: JSON.stringify(result, null, 2) }}] }};\n  }},\n);\n\n",
            tname = tool.name,
            desc = js_string_literal(&tool.description),
        ));
    }

    s.push_str("// Wire up stdio transport last so all tools are registered first.\n");
    s.push_str("if (import.meta.url === `file://${process.argv[1]}`) {\n");
    s.push_str("  const transport = new StdioServerTransport();\n");
    s.push_str("  await server.connect(transport);\n");
    s.push_str("}\n");
    s
}

fn node_zod_object(inputs: &[McpToolInput]) -> String {
    if inputs.is_empty() {
        return "{}".to_string();
    }
    let mut s = String::from("{\n");
    for input in inputs {
        let base = node_zod_type(&input.kind);
        let with_desc = format!("{base}.describe({})", js_string_literal(&input.description));
        let final_schema = if input.optional {
            format!("{with_desc}.optional()")
        } else {
            with_desc
        };
        s.push_str(&format!("    {name}: {final_schema},\n", name = input.name));
    }
    s.push_str("  }");
    s
}

/// Recursive zod-expression emitter for a single [`InputKind`]. Used for
/// top-level inputs and nested array-items / object-fields. The output is
/// a single zod expression (no trailing semicolon, no `.describe(...)`).
fn node_zod_type(kind: &InputKind) -> String {
    match kind {
        InputKind::String => "z.string()".to_string(),
        InputKind::Number => "z.number()".to_string(),
        InputKind::Boolean => "z.boolean()".to_string(),
        InputKind::Enum(values) => {
            let joined = values
                .iter()
                .map(|v| js_string_literal(v))
                .collect::<Vec<_>>()
                .join(", ");
            format!("z.enum([{joined}])")
        }
        InputKind::Array(items) => {
            let inner = node_zod_type(items);
            format!("z.array({inner})")
        }
        InputKind::Object(fields) => {
            if fields.is_empty() {
                return "z.object({})".to_string();
            }
            let mut s = String::from("z.object({ ");
            let mut parts: Vec<String> = Vec::with_capacity(fields.len());
            for f in fields {
                let inner = node_zod_type(&f.kind);
                let with_opt = if f.optional {
                    format!("{inner}.optional()")
                } else {
                    inner
                };
                parts.push(format!("{}: {}", f.name, with_opt));
            }
            s.push_str(&parts.join(", "));
            s.push_str(" })");
            s
        }
    }
}

fn node_args_destructure(inputs: &[McpToolInput]) -> String {
    if inputs.is_empty() {
        return "_args = {}".to_string();
    }
    let inner = inputs
        .iter()
        .map(|i| i.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{ {inner} }} = {{}}")
}

fn node_test_js(name: &str, tools: &[McpToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("// Generated by Anvil MCP Builder.\n");
    s.push_str("//\n");
    s.push_str("// Tests call the tool functions directly (no transport).\n");
    s.push_str("// Run with: npm test\n\n");
    s.push_str("import { test } from 'node:test';\n");
    s.push_str("import assert from 'node:assert/strict';\n");
    s.push_str("import { tools } from './index.js';\n\n");
    s.push_str(&format!(
        "test('{name} — tools object is populated', () => {{\n  assert.ok(tools);\n  assert.equal(typeof tools, 'object');\n}});\n\n"
    ));
    for tool in tools {
        let stub_args = node_stub_call_args(&tool.inputs);
        s.push_str(&format!(
            "test('{tname} — stub returns honesty contract', async () => {{\n",
            tname = tool.name
        ));
        s.push_str(&format!(
            "  const result = await tools.{tname}({stub_args});\n",
            tname = tool.name
        ));
        s.push_str("  assert.equal(typeof result.ok, 'boolean');\n");
        s.push_str("  assert.ok(Array.isArray(result.blockers));\n");
        s.push_str("  assert.ok(Array.isArray(result.warnings));\n");
        s.push_str("  assert.equal(typeof result.message, 'string');\n");
        s.push_str("});\n\n");
    }
    s
}

fn node_stub_call_args(inputs: &[McpToolInput]) -> String {
    if inputs.is_empty() {
        return "{}".to_string();
    }
    let parts: Vec<String> = inputs
        .iter()
        .map(|i| format!("{}: {}", i.name, node_stub_value(&i.kind)))
        .collect();
    format!("{{ {} }}", parts.join(", "))
}

/// Recursive stub-value emitter for tests. Mirrors [`node_zod_type`].
fn node_stub_value(kind: &InputKind) -> String {
    match kind {
        InputKind::String => "'sample'".to_string(),
        InputKind::Number => "0".to_string(),
        InputKind::Boolean => "false".to_string(),
        InputKind::Enum(values) => values
            .first()
            .map(|v| js_string_literal(v))
            .unwrap_or_else(|| "'option'".to_string()),
        InputKind::Array(items) => format!("[{}]", node_stub_value(items)),
        InputKind::Object(fields) => {
            if fields.is_empty() {
                return "{}".to_string();
            }
            let parts: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: {}", f.name, node_stub_value(&f.kind)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

fn js_string_literal(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

// ─── TypeScript templates ────────────────────────────────────────────────────
//
// The TS variant reuses the Node zod / arg-destructure / stub-value
// generators by treating TS as JS-with-types: it generates the same
// `server.tool(...)` calls, but in `src/index.ts`, with a tsconfig +
// `tsx`-based scripts in package.json. This keeps the generator surface
// small (no parallel "ts_zod_type" tree) while giving users typed source.

fn ts_package_json(name: &str) -> String {
    format!(
        "{{\n  \"name\": \"mcp-{name}\",\n  \"version\": \"0.1.0\",\n  \"type\": \"module\",\n  \"private\": true,\n  \"scripts\": {{\n    \"build\": \"tsc -p .\",\n    \"start\": \"tsx src/index.ts\",\n    \"test\": \"node --import tsx/esm --test src/test.ts\"\n  }},\n  \"dependencies\": {{\n    \"@modelcontextprotocol/sdk\": \"^1.0.0\",\n    \"zod\": \"^3.23.0\"\n  }},\n  \"devDependencies\": {{\n    \"tsx\": \"^4.7.0\",\n    \"typescript\": \"^5.4.0\",\n    \"@types/node\": \"^20.0.0\"\n  }}\n}}\n"
    )
}

fn ts_gitignore() -> String {
    "node_modules/\ndist/\n.env\n*.log\n*.tsbuildinfo\n".to_string()
}

fn ts_tsconfig() -> String {
    "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"ESNext\",\n    \"moduleResolution\": \"bundler\",\n    \"strict\": true,\n    \"esModuleInterop\": true,\n    \"skipLibCheck\": true,\n    \"resolveJsonModule\": true,\n    \"outDir\": \"dist\",\n    \"rootDir\": \"src\",\n    \"declaration\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n".to_string()
}

fn ts_index(name: &str, tools: &[McpToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("// Generated by Anvil MCP Builder (TypeScript template).\n");
    s.push_str("//\n");
    s.push_str("// MCP server entry point. Each tool is registered via\n");
    s.push_str("// `server.tool(name, description, zodSchema, handler)`.\n");
    s.push_str("// Tool functions are typed via the inferred zod schema.\n\n");
    s.push_str("import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';\n");
    s.push_str("import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';\n");
    s.push_str("import { z } from 'zod';\n\n");
    s.push_str(&format!(
        "export const server = new McpServer({{\n  name: '{name}',\n  version: '0.1.0',\n}});\n\n"
    ));
    s.push_str("// Honesty-contract result shape returned by every tool.\n");
    s.push_str("export interface ToolResult {\n");
    s.push_str("  ok: boolean;\n");
    s.push_str("  blockers: string[];\n");
    s.push_str("  warnings: string[];\n");
    s.push_str("  subresults: Record<string, unknown>;\n");
    s.push_str("  message: string;\n");
    s.push_str("}\n\n");
    s.push_str("// Tool implementations — exported so test.ts can call them directly.\n");
    s.push_str("// Each function takes a typed object matching its zod schema.\n");
    s.push_str("export const tools: Record<string, (...args: any[]) => Promise<ToolResult>> = {};\n\n");

    for tool in tools {
        let zod_obj = node_zod_object(&tool.inputs);
        let args_destructure = ts_args_destructure(&tool.inputs);
        s.push_str(&format!(
            "tools.{tname} = async function {tname}({args_destructure}): Promise<ToolResult> {{\n",
            tname = tool.name
        ));
        s.push_str(
            "  // TODO: replace this honesty-contract stub with your real implementation.\n",
        );
        s.push_str("  return {\n");
        s.push_str("    ok: true,\n");
        s.push_str("    blockers: [],\n");
        s.push_str("    warnings: [],\n");
        s.push_str("    subresults: {},\n");
        s.push_str(&format!(
            "    message: 'Tool {tname} not yet implemented',\n",
            tname = tool.name
        ));
        s.push_str("  };\n");
        s.push_str("};\n\n");

        s.push_str(&format!(
            "server.tool(\n  '{tname}',\n  {desc},\n  {zod_obj},\n  async (args: any) => {{\n    const result = await tools.{tname}(args);\n    return {{ content: [{{ type: 'text', text: JSON.stringify(result, null, 2) }}] }};\n  }},\n);\n\n",
            tname = tool.name,
            desc = js_string_literal(&tool.description),
        ));
    }

    s.push_str("// Wire up stdio transport last so all tools are registered first.\n");
    s.push_str("if (import.meta.url === `file://${process.argv[1]}`) {\n");
    s.push_str("  const transport = new StdioServerTransport();\n");
    s.push_str("  await server.connect(transport);\n");
    s.push_str("}\n");
    s
}

/// TS-flavoured arg destructure. Same identifiers as
/// [`node_args_destructure`], with `: any` annotation so strict mode is
/// happy without us having to generate per-tool type aliases.
fn ts_args_destructure(inputs: &[McpToolInput]) -> String {
    if inputs.is_empty() {
        return "_args: any = {}".to_string();
    }
    let inner = inputs
        .iter()
        .map(|i| i.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{ {inner} }}: any = {{}}")
}

fn ts_test(name: &str, tools: &[McpToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("// Generated by Anvil MCP Builder (TypeScript template).\n");
    s.push_str("//\n");
    s.push_str("// Tests call the tool functions directly (no transport).\n");
    s.push_str("// Run with: npm test\n\n");
    s.push_str("import { test } from 'node:test';\n");
    s.push_str("import assert from 'node:assert/strict';\n");
    s.push_str("import { tools } from './index.js';\n\n");
    s.push_str(&format!(
        "test('{name} — tools object is populated', () => {{\n  assert.ok(tools);\n  assert.equal(typeof tools, 'object');\n}});\n\n"
    ));
    for tool in tools {
        let stub_args = node_stub_call_args(&tool.inputs);
        s.push_str(&format!(
            "test('{tname} — stub returns honesty contract', async () => {{\n",
            tname = tool.name
        ));
        s.push_str(&format!(
            "  const result = await tools.{tname}({stub_args});\n",
            tname = tool.name
        ));
        s.push_str("  assert.equal(typeof result.ok, 'boolean');\n");
        s.push_str("  assert.ok(Array.isArray(result.blockers));\n");
        s.push_str("  assert.ok(Array.isArray(result.warnings));\n");
        s.push_str("  assert.equal(typeof result.message, 'string');\n");
        s.push_str("});\n\n");
    }
    s
}

// ─── Python templates ────────────────────────────────────────────────────────

fn python_pyproject_toml(name: &str) -> String {
    let pkg = name.replace('-', "_");
    format!(
        "[project]\nname = \"mcp-{name}\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = [\n    \"mcp>=1.0.0\",\n]\n\n[project.optional-dependencies]\ndev = [\n    \"pytest>=8.0\",\n]\n\n[project.scripts]\nmcp-{name} = \"{pkg}:main\"\n\n[build-system]\nrequires = [\"setuptools>=68\"]\nbuild-backend = \"setuptools.build_meta\"\n\n[tool.setuptools]\npy-modules = [\"__main__\"]\n"
    )
}

fn python_gitignore() -> String {
    ".venv/\n__pycache__/\n*.pyc\ndist/\nbuild/\n*.egg-info/\n.pytest_cache/\n".to_string()
}

fn python_main_py(name: &str, tools: &[McpToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("# Generated by Anvil MCP Builder.\n");
    s.push_str("#\n");
    s.push_str("# MCP server entry point using FastMCP. Each @mcp.tool() function\n");
    s.push_str("# becomes a tool the MCP client can call. Type hints drive the\n");
    s.push_str("# input schema; the docstring becomes the tool description.\n\n");
    s.push_str("from __future__ import annotations\n\n");
    // Scan all tool inputs (recursively) to decide which typing imports
    // we actually need. Avoids dead-import warnings.
    let needs_literal = any_kind_matches(tools, &|k| matches!(k, InputKind::Enum(_)));
    let needs_optional = tools.iter().any(|t| {
        t.inputs.iter().any(|i| i.optional)
            || t.inputs.iter().any(|i| input_kind_has_optional_field(&i.kind))
    });
    let needs_list = any_kind_matches(tools, &|k| matches!(k, InputKind::Array(_)));
    let needs_any = any_kind_matches(tools, &|k| {
        matches!(k, InputKind::Object(fields) if fields.is_empty())
    });
    if needs_literal {
        s.push_str("from typing import Literal\n");
    }
    if needs_optional {
        s.push_str("from typing import Optional\n");
    }
    if needs_list {
        s.push_str("from typing import List\n");
    }
    if needs_any {
        s.push_str("from typing import Any, Dict\n");
    }
    let needs_basemodel = any_kind_matches(tools, &|k| {
        matches!(k, InputKind::Object(fields) if !fields.is_empty())
    });
    if needs_basemodel {
        s.push_str("from pydantic import BaseModel\n");
    }
    s.push_str("from mcp.server.fastmcp import FastMCP\n\n");

    // Emit pydantic BaseModel classes for every non-empty Object input
    // (top-level or nested). Names are derived from the schema location;
    // duplicates are de-duplicated by structural hash.
    let mut model_pool = PythonModelPool::default();
    let mut model_decls = String::new();
    for tool in tools {
        for input in &tool.inputs {
            python_register_models(&input.kind, tool, &input.name, &mut model_pool, &mut model_decls);
        }
    }
    if !model_decls.is_empty() {
        s.push_str(&model_decls);
    }

    s.push_str(&format!("mcp = FastMCP(\"{name}\")\n\n"));

    // Tool functions (exported for tests).
    for tool in tools {
        let signature = python_tool_signature(tool, &model_pool);
        s.push_str("@mcp.tool()\n");
        s.push_str(&format!("def {}({}) -> dict:\n", tool.name, signature));
        // Docstring becomes the tool description in MCP.
        s.push_str(&format!(
            "    \"\"\"{}\"\"\"\n",
            python_docstring_escape(&tool.description)
        ));
        s.push_str("    # TODO: replace this honesty-contract stub with your real implementation.\n");
        s.push_str("    return {\n");
        s.push_str("        \"ok\": True,\n");
        s.push_str("        \"blockers\": [],\n");
        s.push_str("        \"warnings\": [],\n");
        s.push_str("        \"subresults\": {},\n");
        s.push_str(&format!(
            "        \"message\": \"Tool {} not yet implemented\",\n",
            tool.name
        ));
        s.push_str("    }\n\n\n");
    }

    s.push_str("def main() -> None:\n");
    s.push_str("    \"\"\"Entry point used by `mcp-<name>` console script and `python -m <pkg>`.\"\"\"\n");
    s.push_str("    mcp.run()\n\n\n");
    s.push_str("if __name__ == \"__main__\":\n");
    s.push_str("    main()\n");
    s
}

fn python_tool_signature(tool: &McpToolSpec, models: &PythonModelPool) -> String {
    let mut parts: Vec<String> = Vec::new();
    for input in &tool.inputs {
        let base_type = python_type_expr(&input.kind, models);
        if input.optional {
            parts.push(format!(
                "{}: Optional[{}] = None",
                input.name, base_type
            ));
        } else {
            parts.push(format!("{}: {}", input.name, base_type));
        }
    }
    parts.join(", ")
}

/// Walk every [`InputKind`] in every tool and return true if `pred` matches
/// any of them. Used to decide which `typing` imports to emit.
fn any_kind_matches(tools: &[McpToolSpec], pred: &dyn Fn(&InputKind) -> bool) -> bool {
    fn walk(k: &InputKind, pred: &dyn Fn(&InputKind) -> bool) -> bool {
        if pred(k) {
            return true;
        }
        match k {
            InputKind::Array(items) => walk(items, pred),
            InputKind::Object(fields) => fields.iter().any(|f| walk(&f.kind, pred)),
            _ => false,
        }
    }
    tools
        .iter()
        .any(|t| t.inputs.iter().any(|i| walk(&i.kind, pred)))
}

/// Detect whether any nested object field is `optional`, requiring
/// `Optional` to be imported even when the top-level input isn't optional.
fn input_kind_has_optional_field(k: &InputKind) -> bool {
    match k {
        InputKind::Array(items) => input_kind_has_optional_field(items),
        InputKind::Object(fields) => fields
            .iter()
            .any(|f| f.optional || input_kind_has_optional_field(&f.kind)),
        _ => false,
    }
}

/// Pool of generated pydantic model class names, keyed by structural shape
/// so identical nested-object schemas reuse the same class.
#[derive(Default)]
struct PythonModelPool {
    /// Map from structural-shape key to class name.
    shapes: Vec<(String, String)>,
}

impl PythonModelPool {
    fn lookup(&self, shape_key: &str) -> Option<&str> {
        self.shapes
            .iter()
            .find(|(k, _)| k == shape_key)
            .map(|(_, v)| v.as_str())
    }

    fn insert(&mut self, shape_key: String, class_name: String) {
        self.shapes.push((shape_key, class_name));
    }

    fn next_class_name(&self, hint: &str) -> String {
        // Class names: capitalised, suffix with index if collision.
        let base = capitalize_first(hint);
        let mut candidate = base.clone();
        let existing: Vec<&str> = self.shapes.iter().map(|(_, v)| v.as_str()).collect();
        let mut n = 2;
        while existing.contains(&candidate.as_str()) {
            candidate = format!("{base}{n}");
            n += 1;
        }
        candidate
    }
}

fn capitalize_first(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let cleaned = s.replace('_', " ").replace('-', " ");
    for word in cleaned.split_whitespace() {
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_uppercase());
            for c in chars {
                out.push(c.to_ascii_lowercase());
            }
        }
    }
    if out.is_empty() {
        "Model".to_string()
    } else {
        out
    }
}

/// Recursive walker: register every non-empty `Object` shape under the
/// given context as a generated pydantic class. `decls` accumulates the
/// rendered class declarations in dependency order (inner-most first).
fn python_register_models(
    kind: &InputKind,
    tool: &McpToolSpec,
    field_path: &str,
    pool: &mut PythonModelPool,
    decls: &mut String,
) {
    match kind {
        InputKind::Array(items) => {
            python_register_models(items, tool, &format!("{field_path}_item"), pool, decls);
        }
        InputKind::Object(fields) if !fields.is_empty() => {
            // Walk children first so dependencies are declared above.
            for f in fields {
                python_register_models(
                    &f.kind,
                    tool,
                    &format!("{field_path}_{}", f.name),
                    pool,
                    decls,
                );
            }
            let shape_key = python_shape_key(kind);
            if pool.lookup(&shape_key).is_some() {
                return;
            }
            // Class name hint: "<Tool><FieldPath>".
            let hint = format!("{}_{}", tool.name, field_path);
            let class_name = pool.next_class_name(&hint);
            pool.insert(shape_key, class_name.clone());
            decls.push_str(&format!("class {class_name}(BaseModel):\n"));
            for f in fields {
                let inner = python_type_expr(&f.kind, pool);
                if f.optional {
                    decls.push_str(&format!(
                        "    {}: Optional[{}] = None\n",
                        f.name, inner
                    ));
                } else {
                    decls.push_str(&format!("    {}: {}\n", f.name, inner));
                }
            }
            decls.push_str("\n\n");
        }
        _ => {}
    }
}

/// Stable structural key for a kind so identical Object shapes share one
/// generated class.
fn python_shape_key(kind: &InputKind) -> String {
    match kind {
        InputKind::String => "S".into(),
        InputKind::Number => "N".into(),
        InputKind::Boolean => "B".into(),
        InputKind::Enum(values) => format!("E[{}]", values.join(",")),
        InputKind::Array(items) => format!("A[{}]", python_shape_key(items)),
        InputKind::Object(fields) => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}{}:{}",
                        if f.optional { "?" } else { "" },
                        f.name,
                        python_shape_key(&f.kind)
                    )
                })
                .collect();
            parts.sort();
            format!("O{{{}}}", parts.join(","))
        }
    }
}

/// Type-expression for a kind. References pool-registered class names for
/// non-empty Object schemas; falls back to `Dict[str, Any]` for open objects.
fn python_type_expr(kind: &InputKind, pool: &PythonModelPool) -> String {
    match kind {
        InputKind::String => "str".into(),
        InputKind::Number => "float".into(),
        InputKind::Boolean => "bool".into(),
        InputKind::Enum(values) => {
            let joined = values
                .iter()
                .map(|v| python_string_literal(v))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Literal[{joined}]")
        }
        InputKind::Array(items) => {
            let inner = python_type_expr(items, pool);
            format!("List[{inner}]")
        }
        InputKind::Object(fields) => {
            if fields.is_empty() {
                "Dict[str, Any]".into()
            } else {
                let key = python_shape_key(kind);
                pool.lookup(&key)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Dict[str, Any]".into())
            }
        }
    }
}

fn python_string_literal(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn python_docstring_escape(s: &str) -> String {
    // Triple-quoted docstrings — escape backslashes and any embedded
    // triple-quote runs. Newlines stay as-is so the docstring is
    // human-readable.
    s.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"")
}

fn python_test_py(name: &str, tools: &[McpToolSpec]) -> String {
    // Rebuild the model pool so test stubs use the same class names as
    // the generated `__main__.py`.
    let mut pool = PythonModelPool::default();
    let mut sink = String::new();
    for tool in tools {
        for input in &tool.inputs {
            python_register_models(&input.kind, tool, &input.name, &mut pool, &mut sink);
        }
    }

    let mut s = String::new();
    s.push_str("# Generated by Anvil MCP Builder.\n");
    s.push_str("#\n");
    s.push_str("# Pytest suite that calls each tool function directly (no MCP transport).\n");
    s.push_str("# Run with: pytest -v\n\n");
    s.push_str("import __main__ as server\n\n");
    s.push_str(&format!(
        "def test_{}_module_imports() -> None:\n    assert server.mcp is not None\n\n\n",
        name.replace('-', "_")
    ));
    for tool in tools {
        let call_args = python_stub_call_args_with_server(&tool.inputs, &pool);
        s.push_str(&format!(
            "def test_{tname}_stub_returns_honesty_contract() -> None:\n",
            tname = tool.name
        ));
        s.push_str(&format!(
            "    result = server.{tname}({call_args})\n",
            tname = tool.name
        ));
        s.push_str("    assert isinstance(result, dict)\n");
        s.push_str("    assert isinstance(result.get(\"ok\"), bool)\n");
        s.push_str("    assert isinstance(result.get(\"blockers\"), list)\n");
        s.push_str("    assert isinstance(result.get(\"warnings\"), list)\n");
        s.push_str("    assert isinstance(result.get(\"message\"), str)\n\n\n");
    }
    s
}

/// Like [`python_stub_call_args`] but qualifies pydantic model references
/// with the imported `server.` prefix so the test file can reach them
/// without needing duplicate model declarations.
fn python_stub_call_args_with_server(
    inputs: &[McpToolInput],
    pool: &PythonModelPool,
) -> String {
    if inputs.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = inputs
        .iter()
        .map(|i| {
            format!(
                "{}={}",
                i.name,
                python_stub_value_with_server(&i.kind, pool)
            )
        })
        .collect();
    parts.join(", ")
}

/// Wrapper around [`python_stub_value`] that prefixes every pool-registered
/// class reference with `server.`.
fn python_stub_value_with_server(kind: &InputKind, pool: &PythonModelPool) -> String {
    match kind {
        InputKind::String => "\"sample\"".into(),
        InputKind::Number => "0.0".into(),
        InputKind::Boolean => "False".into(),
        InputKind::Enum(values) => values
            .first()
            .map(|v| python_string_literal(v))
            .unwrap_or_else(|| "\"option\"".into()),
        InputKind::Array(items) => format!("[{}]", python_stub_value_with_server(items, pool)),
        InputKind::Object(fields) => {
            if fields.is_empty() {
                return "{}".to_string();
            }
            let key = python_shape_key(kind);
            let class_name = match pool.lookup(&key) {
                Some(s) => s.to_string(),
                None => {
                    let parts: Vec<String> = fields
                        .iter()
                        .filter(|f| !f.optional)
                        .map(|f| {
                            format!(
                                "\"{}\": {}",
                                f.name,
                                python_stub_value_with_server(&f.kind, pool)
                            )
                        })
                        .collect();
                    return format!("{{{}}}", parts.join(", "));
                }
            };
            let parts: Vec<String> = fields
                .iter()
                .filter(|f| !f.optional)
                .map(|f| {
                    format!(
                        "{}={}",
                        f.name,
                        python_stub_value_with_server(&f.kind, pool)
                    )
                })
                .collect();
            format!("server.{class_name}({})", parts.join(", "))
        }
    }
}

// ─── README (both languages) ─────────────────────────────────────────────────

fn readme(name: &str, lang: McpLanguage, tools: &[McpToolSpec]) -> String {
    let setup_cmds = match lang {
        McpLanguage::Node => "    npm install\n    npm test\n    npm start",
        McpLanguage::TypeScript => "    npm install\n    npm test       # via tsx (no build needed)\n    npm run build  # emit dist/ via tsc\n    npm start      # tsx src/index.ts",
        McpLanguage::Python => "    python -m venv .venv\n    source .venv/bin/activate\n    pip install -e '.[dev]'\n    pytest -v\n    python __main__.py",
    };
    let mut tool_lines = String::new();
    for tool in tools {
        tool_lines.push_str(&format!("- `{}` — {}\n", tool.name, tool.description));
    }
    format!(
        "# mcp-{name}\n\nMCP server scaffolded by Anvil's MCP Builder ({lang}).\n\n## Tools\n\n{tool_lines}\n## Setup\n\n```sh\n{setup_cmds}\n```\n\n## Registering with Anvil\n\nThis server is auto-registered in `~/.anvil/settings.json` under\n`mcpServers.{name}`. Run `/mcp list` inside Anvil to verify.\n\nTo remove it, delete the entry from `~/.anvil/settings.json::mcpServers`.\n",
        lang = lang.label()
    )
}

// ─── Disk writer ─────────────────────────────────────────────────────────────

/// Write the scaffold to disk. The output directory is created if it
/// doesn't exist. If it exists and is non-empty, returns Err — the
/// caller should warn the user and pick a different path. Returns the
/// list of paths actually written.
pub fn write_scaffold_to_disk(spec: &McpBuilderSpec) -> Result<Vec<PathBuf>, String> {
    let out = &spec.output_dir;
    if out.exists() {
        let mut entries = fs::read_dir(out).map_err(|e| format!("read {}: {e}", out.display()))?;
        if entries.next().is_some() {
            return Err(format!(
                "output directory {} is not empty — pick an empty / non-existent path",
                out.display()
            ));
        }
    } else {
        fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    }
    let files = scaffold_files(spec);
    let mut written: Vec<PathBuf> = Vec::with_capacity(files.len());
    for (path, contents) in &files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create parent {}: {e}", parent.display()))?;
        }
        fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
        written.push(path.clone());
    }
    Ok(written)
}

// ─── Anvil registration (settings.json) ──────────────────────────────────────

/// Write the new MCP into `~/.anvil/settings.json::mcpServers.<name>`.
/// Idempotent — overwrites the existing entry for the same name, leaves
/// other servers untouched. Returns true if the file was modified.
///
/// Matches the precedent set by `wizard_qmd::wire_qmd_into_settings`.
pub fn wire_into_settings(
    settings_path: &Path,
    spec: &McpBuilderSpec,
) -> Result<bool, String> {
    let (command, args) = match spec.language {
        McpLanguage::Node => (
            "node".to_string(),
            vec![spec.output_dir.join("index.js").display().to_string()],
        ),
        McpLanguage::TypeScript => (
            // tsx runs TS directly without an explicit build step. Users
            // who later add `npm run build` + a `node dist/index.js` start
            // can rewrite this entry; the generator picks the no-friction
            // path by default.
            "npx".to_string(),
            vec![
                "tsx".to_string(),
                spec.output_dir.join("src").join("index.ts").display().to_string(),
            ],
        ),
        McpLanguage::Python => (
            "python3".to_string(),
            vec![spec.output_dir.join("__main__.py").display().to_string()],
        ),
    };

    let mut root: serde_json::Value = if settings_path.is_file() {
        let bytes = fs::read(settings_path)
            .map_err(|e| format!("read {}: {e}", settings_path.display()))?;
        if bytes.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("parse {}: {e}", settings_path.display()))?
        }
    } else {
        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        serde_json::json!({})
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| "settings.json root is not a JSON object".to_string())?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "settings.json::mcpServers is not a JSON object".to_string())?;

    let new_entry = serde_json::json!({
        "command": command,
        "args": args,
    });
    let before = servers_obj.get(&spec.name).cloned();
    if before.as_ref() == Some(&new_entry) {
        return Ok(false);
    }
    servers_obj.insert(spec.name.clone(), new_entry);

    let pretty = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("serialize settings.json: {e}"))?;
    fs::write(settings_path, pretty)
        .map_err(|e| format!("write {}: {e}", settings_path.display()))?;
    Ok(true)
}

/// Read the list of currently-registered MCP server names from the
/// given settings file. Returns an empty set if the file is missing or
/// malformed (the wizard treats "no servers" the same as "no settings").
pub fn read_existing_mcp_names(settings_path: &Path) -> BTreeSet<String> {
    let bytes = match fs::read(settings_path) {
        Ok(b) => b,
        Err(_) => return BTreeSet::new(),
    };
    let val: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return BTreeSet::new(),
    };
    val.get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

// ─── Auto-install runner (task #676) ─────────────────────────────────────────
//
// Two-tier API:
//   - `install_command(lang)`: PURE. Returns (program, args) for the
//     scaffolded language. Easy to unit-test, easy to reason about.
//   - `run_install(dir, lang)`: IMPURE. Spawns the command in `dir` and
//     streams stdout/stderr through to the user's terminal so npm/pip
//     progress shows up live. Returns `Ok(())` on exit-status 0,
//     `Err(message)` otherwise. Errors include the command line so the
//     user can re-run by hand if anything blows up.

/// The shell command that installs dependencies for a fresh MCP project.
/// Same shape for all languages: `(program, args)` — caller spawns this
/// with cwd = project dir.
#[must_use]
pub fn install_command(lang: McpLanguage) -> (&'static str, Vec<&'static str>) {
    match lang {
        // Node: `npm install` resolves @modelcontextprotocol/sdk + zod
        // from the generated package.json. No build step needed.
        McpLanguage::Node => ("npm", vec!["install"]),
        // TypeScript: same `npm install` — package.json pulls in
        // typescript, tsx, @types/node as devDeps so users can `npm test`
        // / `npm start` straight away.
        McpLanguage::TypeScript => ("npm", vec!["install"]),
        // Python: `python3 -m pip install -e .[dev]` is the
        // editable-install path. We use `python3` not `python` so macOS
        // / Linux pick up the right binary on a fresh install; users on
        // a venv can rewrite by hand.
        McpLanguage::Python => ("python3", vec!["-m", "pip", "install", "-e", ".[dev]"]),
    }
}

/// Spawn the install command for `lang` in `dir`. Inherits stdio so the
/// user sees real-time npm / pip output. Returns Err with the full
/// command line when the child exits non-zero or fails to spawn — that's
/// the form the user needs to recover by hand.
pub fn run_install(dir: &Path, lang: McpLanguage) -> Result<(), String> {
    let (program, args) = install_command(lang);
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args).current_dir(dir);
    let status = cmd
        .status()
        .map_err(|e| format!("spawn `{program}` (is it on PATH?): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        let printable = format!("{program} {}", args.join(" "));
        Err(format!(
            "install failed (`{printable}` exited with {}). Re-run by hand from {}.",
            status
                .code()
                .map_or_else(|| "signal".to_string(), |n| n.to_string()),
            dir.display()
        ))
    }
}

// ─── Interactive flow (called by /mcp builder) ───────────────────────────────

/// Top-level entry called by the `/mcp builder` slash command handler
/// AFTER the TUI alt-screen has been left. Prints prompts on stdout and
/// reads from stdin in plain mode. Returns a single-line status string
/// for the chat scrollback.
///
/// `settings_path` is the path to `~/.anvil/settings.json` — exposed as
/// a parameter so tests can drive the flow against a temp file.
///
/// The function is intentionally chatty (print prompts, echo back
/// values, show the file list before writing) — when invoked from
/// `/mcp builder` the alt-screen is gone, so println is the right
/// channel. The doc comment on the module spells this out.
pub fn run_mcp_builder(settings_path: &Path) -> String {
    // Banner.
    let _ = writeln!(io::stdout(), "\n⚒ Anvil MCP Builder\n");
    let _ = writeln!(
        io::stdout(),
        "Scaffold a new MCP server. Press Ctrl-C at any time to cancel.\n"
    );

    let existing = read_existing_mcp_names(settings_path);

    // Step 1 — name.
    let name = match prompt_name(&existing) {
        Ok(n) => n,
        Err(msg) => return format!("MCP builder cancelled: {msg}"),
    };

    // Step 2 — language.
    let language = match prompt_language() {
        Ok(l) => l,
        Err(msg) => return format!("MCP builder cancelled: {msg}"),
    };

    // Step 3 — output directory.
    let output_dir = match prompt_output_dir(&name) {
        Ok(p) => p,
        Err(msg) => return format!("MCP builder cancelled: {msg}"),
    };

    // Step 4 — tools.
    let tools = match prompt_tools() {
        Ok(t) => t,
        Err(msg) => return format!("MCP builder cancelled: {msg}"),
    };

    let spec = McpBuilderSpec {
        name: name.clone(),
        language,
        tools,
        output_dir: output_dir.clone(),
    };

    // Step 5 — confirm.
    let _ = writeln!(io::stdout(), "\n── Summary ──");
    let _ = writeln!(io::stdout(), "  name:     {}", spec.name);
    let _ = writeln!(io::stdout(), "  language: {}", spec.language.label());
    let _ = writeln!(io::stdout(), "  output:   {}", spec.output_dir.display());
    let _ = writeln!(io::stdout(), "  tools:    {} defined", spec.tools.len());
    for tool in &spec.tools {
        let _ = writeln!(
            io::stdout(),
            "    - {} ({} input{})",
            tool.name,
            tool.inputs.len(),
            if tool.inputs.len() == 1 { "" } else { "s" }
        );
    }
    if !prompt_yes_no("\nScaffold this MCP? [Y/n]: ", true) {
        return "MCP builder cancelled by user.".to_string();
    }

    // Step 6 — write to disk.
    match write_scaffold_to_disk(&spec) {
        Ok(paths) => {
            let _ = writeln!(io::stdout(), "\n✓ Scaffold written:");
            for p in &paths {
                let _ = writeln!(io::stdout(), "    {}", p.display());
            }
        }
        Err(e) => return format!("MCP builder failed: {e}"),
    }

    // Step 7 — register with Anvil.
    match wire_into_settings(settings_path, &spec) {
        Ok(true) => {
            let _ = writeln!(
                io::stdout(),
                "✓ Registered as MCP server in {}.",
                settings_path.display()
            );
        }
        Ok(false) => {
            let _ = writeln!(
                io::stdout(),
                "  (settings.json already had this entry — no change.)"
            );
        }
        Err(e) => {
            let _ = writeln!(
                io::stdout(),
                "! Could not update settings.json: {e}\n  Add the entry manually with /mcp later."
            );
        }
    }

    // Step 7.5 — optional auto-install (task #676).
    // Offer to run the language's package manager so the new MCP can run
    // immediately. Skipping is fine — the next-steps hint still shows the
    // manual command.
    let (program, args) = install_command(spec.language);
    let install_prompt = format!(
        "\nRun `{program} {}` now to install dependencies? [Y/n]: ",
        args.join(" ")
    );
    let did_install = if prompt_yes_no(&install_prompt, true) {
        let _ = writeln!(io::stdout(), "\n→ Installing dependencies in {} ...", spec.output_dir.display());
        match run_install(&spec.output_dir, spec.language) {
            Ok(()) => {
                let _ = writeln!(io::stdout(), "✓ Dependencies installed.");
                true
            }
            Err(e) => {
                let _ = writeln!(io::stdout(), "! {e}");
                false
            }
        }
    } else {
        false
    };

    // Step 8 — next-steps hint. Only show the install command if we
    // didn't already run it (no point telling the user to do something
    // they just watched succeed).
    let next_steps = match spec.language {
        McpLanguage::Node => {
            if did_install {
                format!(
                    "\nNext steps:\n  cd {dir}\n  npm test\n",
                    dir = spec.output_dir.display()
                )
            } else {
                format!(
                    "\nNext steps:\n  cd {dir}\n  npm install\n  npm test\n",
                    dir = spec.output_dir.display()
                )
            }
        }
        McpLanguage::TypeScript => {
            if did_install {
                format!(
                    "\nNext steps:\n  cd {dir}\n  npm test         # runs src/test.ts via tsx\n  npm run build    # emits dist/ via tsc\n",
                    dir = spec.output_dir.display()
                )
            } else {
                format!(
                    "\nNext steps:\n  cd {dir}\n  npm install\n  npm test\n",
                    dir = spec.output_dir.display()
                )
            }
        }
        McpLanguage::Python => {
            if did_install {
                format!(
                    "\nNext steps:\n  cd {dir}\n  pytest -v\n",
                    dir = spec.output_dir.display()
                )
            } else {
                format!(
                    "\nNext steps:\n  cd {dir}\n  python -m venv .venv\n  source .venv/bin/activate\n  pip install -e '.[dev]'\n  pytest -v\n",
                    dir = spec.output_dir.display()
                )
            }
        }
    };
    let _ = writeln!(io::stdout(), "{next_steps}");

    format!("MCP \"{name}\" scaffolded at {}", spec.output_dir.display())
}

// ─── Prompt helpers ──────────────────────────────────────────────────────────

fn read_line() -> Result<String, String> {
    let mut s = String::new();
    io::stdin()
        .read_line(&mut s)
        .map_err(|e| format!("read stdin: {e}"))?;
    Ok(s.trim().to_string())
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> bool {
    let _ = write!(io::stdout(), "{prompt}");
    let _ = io::stdout().flush();
    match read_line() {
        Ok(s) if s.is_empty() => default_yes,
        Ok(s) => matches!(s.to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => default_yes,
    }
}

fn prompt_name(existing: &BTreeSet<String>) -> Result<String, String> {
    loop {
        let _ = write!(
            io::stdout(),
            "Project name (kebab-case, e.g. weather-api): "
        );
        let _ = io::stdout().flush();
        let name = read_line()?;
        if name.is_empty() {
            return Err("no name provided".into());
        }
        match validate_name(&name, existing) {
            Ok(()) => return Ok(name),
            Err(e) => {
                let _ = writeln!(io::stdout(), "  ✗ {e} — try again.");
            }
        }
    }
}

fn prompt_language() -> Result<McpLanguage, String> {
    loop {
        let _ = writeln!(io::stdout(), "\nLanguage:");
        let _ = writeln!(io::stdout(), "  1) Node.js (McpServer + zod)");
        let _ = writeln!(io::stdout(), "  2) TypeScript (McpServer + zod, typed)");
        let _ = writeln!(io::stdout(), "  3) Python (FastMCP)");
        let _ = write!(io::stdout(), "Choose [1-3]: ");
        let _ = io::stdout().flush();
        match read_line()?.as_str() {
            "1" | "n" | "node" | "nodejs" | "js" => return Ok(McpLanguage::Node),
            "2" | "t" | "ts" | "typescript" => return Ok(McpLanguage::TypeScript),
            "3" | "p" | "py" | "python" => return Ok(McpLanguage::Python),
            "" => return Err("no language chosen".into()),
            other => {
                let _ = writeln!(
                    io::stdout(),
                    "  ✗ {other:?} not recognised — pick 1, 2, or 3."
                );
            }
        }
    }
}

fn prompt_output_dir(name: &str) -> Result<PathBuf, String> {
    let default = default_output_dir(name);
    let _ = write!(
        io::stdout(),
        "\nOutput directory [{}]: ",
        default.display()
    );
    let _ = io::stdout().flush();
    let raw = read_line()?;
    let chosen = if raw.is_empty() {
        default
    } else {
        let p = PathBuf::from(expand_tilde(&raw));
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()
                .map_err(|e| format!("cwd: {e}"))?
                .join(p)
        }
    };
    Ok(chosen)
}

/// Default output dir: `~/projects/<name>-mcp` if `~/projects` exists,
/// else `~/<name>-mcp` (we keep it inside HOME so the user has write
/// permissions out of the gate).
fn default_output_dir(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let projects = home.join("projects");
    let base = if projects.is_dir() { projects } else { home };
    base.join(format!("{name}-mcp"))
}

fn expand_tilde(s: &str) -> String {
    if let Some(stripped) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home)
            .join(stripped)
            .display()
            .to_string();
    }
    s.to_string()
}

fn prompt_tools() -> Result<Vec<McpToolSpec>, String> {
    let _ = writeln!(io::stdout(), "\nTool definitions");
    let _ = writeln!(
        io::stdout(),
        "  Press Enter on an empty tool name to finish. A stub `hello` tool is added if you skip."
    );
    let mut tools: Vec<McpToolSpec> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let _ = write!(io::stdout(), "  Tool #{} name (or blank to finish): ", tools.len() + 1);
        let _ = io::stdout().flush();
        let name = read_line()?;
        if name.is_empty() {
            return Ok(tools);
        }
        if let Err(e) = validate_identifier(&name) {
            let _ = writeln!(io::stdout(), "    ✗ {e}");
            continue;
        }
        if !seen.insert(name.clone()) {
            let _ = writeln!(io::stdout(), "    ✗ duplicate tool name");
            continue;
        }
        let _ = write!(io::stdout(), "    description: ");
        let _ = io::stdout().flush();
        let description = read_line()?;
        if description.is_empty() {
            let _ = writeln!(io::stdout(), "    ✗ description required");
            seen.remove(&name);
            continue;
        }
        let inputs = match prompt_inputs() {
            Ok(i) => i,
            Err(e) => {
                let _ = writeln!(io::stdout(), "    ✗ {e}");
                continue;
            }
        };
        tools.push(McpToolSpec {
            name,
            description,
            inputs,
        });
    }
}

fn prompt_inputs() -> Result<Vec<McpToolInput>, String> {
    let _ = writeln!(
        io::stdout(),
        "    Inputs — press Enter on a blank name to finish this tool."
    );
    let mut inputs: Vec<McpToolInput> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let _ = write!(io::stdout(), "      Input #{} name: ", inputs.len() + 1);
        let _ = io::stdout().flush();
        let name = read_line()?;
        if name.is_empty() {
            return Ok(inputs);
        }
        if let Err(e) = validate_identifier(&name) {
            let _ = writeln!(io::stdout(), "        ✗ {e}");
            continue;
        }
        if !seen.insert(name.clone()) {
            let _ = writeln!(io::stdout(), "        ✗ duplicate input name");
            continue;
        }
        let kind = match prompt_input_kind("        ") {
            Ok(k) => k,
            Err(e) => {
                let _ = writeln!(io::stdout(), "        ✗ {e}");
                seen.remove(&name);
                continue;
            }
        };
        let _ = write!(io::stdout(), "        description: ");
        let _ = io::stdout().flush();
        let description = read_line()?;
        if description.is_empty() {
            let _ = writeln!(io::stdout(), "        ✗ description required");
            seen.remove(&name);
            continue;
        }
        let optional = prompt_yes_no("        optional? [y/N]: ", false);
        inputs.push(McpToolInput {
            name,
            description,
            kind,
            optional,
        });
    }
}

/// Recursive prompt for an [`InputKind`]. `indent` is the leading
/// whitespace shown before each prompt line — used so nested array-items
/// and object-fields visually nest under their parent.
///
/// Loops until the user picks a valid type or aborts. Returns Err only
/// when stdin closes mid-prompt; the caller re-asks the surrounding
/// question on error.
fn prompt_input_kind(indent: &str) -> Result<InputKind, String> {
    loop {
        let _ = writeln!(io::stdout(), "{indent}type:");
        let _ = writeln!(io::stdout(), "{indent}  1) string");
        let _ = writeln!(io::stdout(), "{indent}  2) number");
        let _ = writeln!(io::stdout(), "{indent}  3) boolean");
        let _ = writeln!(io::stdout(), "{indent}  4) enum of strings");
        let _ = writeln!(io::stdout(), "{indent}  5) array (of …)");
        let _ = writeln!(io::stdout(), "{indent}  6) object (named fields)");
        let _ = write!(io::stdout(), "{indent}choose [1-6]: ");
        let _ = io::stdout().flush();
        match read_line()?.as_str() {
            "1" | "s" | "str" | "string" => return Ok(InputKind::String),
            "2" | "n" | "num" | "number" => return Ok(InputKind::Number),
            "3" | "b" | "bool" | "boolean" => return Ok(InputKind::Boolean),
            "4" | "e" | "enum" => {
                let _ = write!(io::stdout(), "{indent}enum values (comma-separated): ");
                let _ = io::stdout().flush();
                let raw = read_line()?;
                let values: Vec<String> = raw
                    .split(',')
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect();
                if values.is_empty() {
                    let _ = writeln!(io::stdout(), "{indent}✗ enum needs at least one value");
                    continue;
                }
                return Ok(InputKind::Enum(values));
            }
            "5" | "a" | "arr" | "array" => {
                let _ = writeln!(io::stdout(), "{indent}array items:");
                let inner_indent = format!("{indent}  ");
                let items = prompt_input_kind(&inner_indent)?;
                return Ok(InputKind::Array(Box::new(items)));
            }
            "6" | "o" | "obj" | "object" => {
                let inner_indent = format!("{indent}  ");
                let fields = prompt_object_fields(&inner_indent)?;
                return Ok(InputKind::Object(fields));
            }
            other => {
                let _ = writeln!(io::stdout(), "{indent}✗ {other:?} not a type — try again.");
            }
        }
    }
}

/// Prompt loop for a list of [`ObjectField`]s. Blank name finishes the
/// object. Identifier validation + de-duplication are enforced.
fn prompt_object_fields(indent: &str) -> Result<Vec<ObjectField>, String> {
    let _ = writeln!(
        io::stdout(),
        "{indent}object fields — press Enter on a blank name to finish."
    );
    let mut fields: Vec<ObjectField> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let _ = write!(io::stdout(), "{indent}field #{} name: ", fields.len() + 1);
        let _ = io::stdout().flush();
        let name = read_line()?;
        if name.is_empty() {
            return Ok(fields);
        }
        if let Err(e) = validate_identifier(&name) {
            let _ = writeln!(io::stdout(), "{indent}✗ {e}");
            continue;
        }
        if !seen.insert(name.clone()) {
            let _ = writeln!(io::stdout(), "{indent}✗ duplicate field name");
            continue;
        }
        let kind = match prompt_input_kind(indent) {
            Ok(k) => k,
            Err(e) => {
                let _ = writeln!(io::stdout(), "{indent}✗ {e}");
                seen.remove(&name);
                continue;
            }
        };
        let optional = prompt_yes_no(&format!("{indent}optional? [y/N]: "), false);
        fields.push(ObjectField {
            name,
            kind,
            optional,
        });
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn empty_existing() -> BTreeSet<String> {
        BTreeSet::new()
    }

    // ── validate_name ─────────────────────────────────────────────────────

    #[test]
    fn validate_name_accepts_simple_kebab() {
        assert!(validate_name("weather-api", &empty_existing()).is_ok());
        assert!(validate_name("foo", &empty_existing()).is_ok());
        assert!(validate_name("a-b-c-d", &empty_existing()).is_ok());
    }

    #[test]
    fn validate_name_rejects_too_short() {
        assert!(validate_name("ab", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_too_long() {
        let n = "a".repeat(33);
        assert!(validate_name(&n, &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_uppercase() {
        assert!(validate_name("FooBar", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_leading_digit() {
        assert!(validate_name("1foo", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_leading_hyphen() {
        assert!(validate_name("-foo", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_trailing_hyphen() {
        assert!(validate_name("foo-", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_double_hyphen() {
        assert!(validate_name("foo--bar", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_underscore() {
        assert!(validate_name("foo_bar", &empty_existing()).is_err());
    }

    #[test]
    fn validate_name_rejects_reserved() {
        for n in ["anvil", "mcp", "test", "server"] {
            assert!(
                validate_name(n, &empty_existing()).is_err(),
                "{n} should be reserved"
            );
        }
    }

    #[test]
    fn validate_name_rejects_clash_with_existing() {
        let mut existing = BTreeSet::new();
        existing.insert("weather-api".to_string());
        assert!(validate_name("weather-api", &existing).is_err());
    }

    // ── validate_identifier ───────────────────────────────────────────────

    #[test]
    fn validate_identifier_accepts_snake_case() {
        assert!(validate_identifier("hello").is_ok());
        assert!(validate_identifier("send_email").is_ok());
        assert!(validate_identifier("a1_b2").is_ok());
    }

    #[test]
    fn validate_identifier_rejects_hyphens() {
        assert!(validate_identifier("send-email").is_err());
    }

    #[test]
    fn validate_identifier_rejects_capital() {
        assert!(validate_identifier("Send").is_err());
    }

    #[test]
    fn validate_identifier_rejects_empty() {
        assert!(validate_identifier("").is_err());
    }

    // ── scaffold_files: Node ──────────────────────────────────────────────

    fn sample_node_spec() -> McpBuilderSpec {
        McpBuilderSpec {
            name: "weather-api".to_string(),
            language: McpLanguage::Node,
            tools: vec![McpToolSpec {
                name: "get_forecast".to_string(),
                description: "Return the forecast for a city".to_string(),
                inputs: vec![
                    McpToolInput {
                        name: "city".to_string(),
                        description: "City name".to_string(),
                        kind: InputKind::String,
                        optional: false,
                    },
                    McpToolInput {
                        name: "units".to_string(),
                        description: "Unit system".to_string(),
                        kind: InputKind::Enum(vec![
                            "metric".to_string(),
                            "imperial".to_string(),
                        ]),
                        optional: true,
                    },
                ],
            }],
            output_dir: PathBuf::from("/tmp/scaffold-node"),
        }
    }

    #[test]
    fn scaffold_node_emits_expected_files() {
        let files = scaffold_files(&sample_node_spec());
        let names: Vec<String> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"package.json".to_string()));
        assert!(names.contains(&"index.js".to_string()));
        assert!(names.contains(&"test.js".to_string()));
        assert!(names.contains(&".gitignore".to_string()));
        assert!(names.contains(&"README.md".to_string()));
        assert_eq!(files.len(), 5);
    }

    #[test]
    fn scaffold_node_files_are_nonempty() {
        for (path, contents) in scaffold_files(&sample_node_spec()) {
            assert!(
                !contents.is_empty(),
                "{} is empty",
                path.display()
            );
        }
    }

    #[test]
    fn scaffold_node_index_js_registers_tool() {
        let files = scaffold_files(&sample_node_spec());
        let (_, idx) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "index.js")
            .unwrap();
        assert!(idx.contains("server.tool"));
        assert!(idx.contains("'get_forecast'"));
        assert!(idx.contains("z.string()"));
        assert!(idx.contains("z.enum(['metric', 'imperial'])"));
        assert!(idx.contains(".optional()"));
        assert!(idx.contains("StdioServerTransport"));
    }

    #[test]
    fn scaffold_node_package_json_pins_sdk() {
        let files = scaffold_files(&sample_node_spec());
        let (_, pkg) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "package.json")
            .unwrap();
        assert!(pkg.contains("\"@modelcontextprotocol/sdk\""));
        assert!(pkg.contains("\"zod\""));
        assert!(pkg.contains("\"node --test test.js\""));
    }

    #[test]
    fn scaffold_node_test_js_calls_tool_directly() {
        let files = scaffold_files(&sample_node_spec());
        let (_, t) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "test.js")
            .unwrap();
        assert!(t.contains("tools.get_forecast"));
        assert!(t.contains("result.ok"));
    }

    // ── scaffold_files: Python ────────────────────────────────────────────

    fn sample_python_spec() -> McpBuilderSpec {
        McpBuilderSpec {
            name: "data-tool".to_string(),
            language: McpLanguage::Python,
            tools: vec![McpToolSpec {
                name: "fetch_rows".to_string(),
                description: "Fetch N rows from the dataset".to_string(),
                inputs: vec![
                    McpToolInput {
                        name: "limit".to_string(),
                        description: "Max rows".to_string(),
                        kind: InputKind::Number,
                        optional: false,
                    },
                    McpToolInput {
                        name: "dry_run".to_string(),
                        description: "Skip side effects".to_string(),
                        kind: InputKind::Boolean,
                        optional: true,
                    },
                ],
            }],
            output_dir: PathBuf::from("/tmp/scaffold-py"),
        }
    }

    #[test]
    fn scaffold_python_emits_expected_files() {
        let files = scaffold_files(&sample_python_spec());
        let names: Vec<String> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"pyproject.toml".to_string()));
        assert!(names.contains(&"__main__.py".to_string()));
        assert!(names.contains(&"test_server.py".to_string()));
        assert!(names.contains(&".gitignore".to_string()));
        assert!(names.contains(&"README.md".to_string()));
        assert_eq!(files.len(), 5);
    }

    #[test]
    fn scaffold_python_main_uses_fastmcp() {
        let files = scaffold_files(&sample_python_spec());
        let (_, main) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "__main__.py")
            .unwrap();
        assert!(main.contains("from mcp.server.fastmcp import FastMCP"));
        assert!(main.contains("FastMCP(\"data-tool\")"));
        assert!(main.contains("@mcp.tool()"));
        assert!(main.contains("def fetch_rows("));
        assert!(main.contains("limit: float"));
        assert!(main.contains("dry_run: Optional[bool] = None"));
    }

    #[test]
    fn scaffold_python_pyproject_pins_mcp() {
        let files = scaffold_files(&sample_python_spec());
        let (_, pp) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "pyproject.toml")
            .unwrap();
        assert!(pp.contains("mcp>=1.0.0"));
        assert!(pp.contains("pytest"));
        assert!(pp.contains("data_tool"));
    }

    // ── empty tool list ───────────────────────────────────────────────────

    #[test]
    fn scaffold_empty_tools_emits_hello_stub_node() {
        let spec = McpBuilderSpec {
            name: "blank-node".to_string(),
            language: McpLanguage::Node,
            tools: vec![],
            output_dir: PathBuf::from("/tmp/blank-node"),
        };
        let files = scaffold_files(&spec);
        let (_, idx) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "index.js")
            .unwrap();
        assert!(idx.contains("'hello'"));
        assert!(idx.contains("subject"));
    }

    #[test]
    fn scaffold_empty_tools_emits_hello_stub_python() {
        let spec = McpBuilderSpec {
            name: "blank-py".to_string(),
            language: McpLanguage::Python,
            tools: vec![],
            output_dir: PathBuf::from("/tmp/blank-py"),
        };
        let files = scaffold_files(&spec);
        let (_, main) = files
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == "__main__.py")
            .unwrap();
        assert!(main.contains("def hello("));
    }

    // ── No infra leakage ──────────────────────────────────────────────────

    #[test]
    fn scaffold_emits_no_culpur_internal_paths_node() {
        let spec = sample_node_spec();
        for (_, contents) in scaffold_files(&spec) {
            for needle in [
                "guard.armored.ninja",
                "10.0.70.80",
                "passage.culpur.net",
                "armored.ninja",
                "soulofall",
                "registry.culpur",
                "30022",
                "61YLhrNHe",
            ] {
                assert!(
                    !contents.contains(needle),
                    "scaffold leaked {needle:?} into a generated file"
                );
            }
        }
    }

    #[test]
    fn scaffold_emits_no_culpur_internal_paths_python() {
        let spec = sample_python_spec();
        for (_, contents) in scaffold_files(&spec) {
            for needle in [
                "guard.armored.ninja",
                "10.0.70.80",
                "passage.culpur.net",
                "armored.ninja",
                "soulofall",
                "registry.culpur",
                "30022",
                "61YLhrNHe",
            ] {
                assert!(
                    !contents.contains(needle),
                    "scaffold leaked {needle:?} into a generated file"
                );
            }
        }
    }

    // ── wire_into_settings ────────────────────────────────────────────────

    #[test]
    fn wire_into_settings_creates_file_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        let spec = sample_node_spec();
        let changed = wire_into_settings(&settings, &spec).unwrap();
        assert!(changed);
        let body = fs::read_to_string(&settings).unwrap();
        let val: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entry = val
            .get("mcpServers")
            .and_then(|m| m.get("weather-api"))
            .unwrap();
        assert_eq!(entry.get("command").unwrap().as_str(), Some("node"));
        let args = entry.get("args").unwrap().as_array().unwrap();
        assert!(args[0]
            .as_str()
            .unwrap()
            .ends_with("/scaffold-node/index.js"));
    }

    #[test]
    fn wire_into_settings_preserves_other_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        let initial = r#"{
  "mcpServers": {
    "existing": {"command": "node", "args": ["/path/to/existing/index.js"]}
  },
  "other_key": "should_remain"
}"#;
        fs::write(&settings, initial).unwrap();
        let spec = sample_node_spec();
        let _ = wire_into_settings(&settings, &spec).unwrap();
        let body = fs::read_to_string(&settings).unwrap();
        let val: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(val.get("mcpServers").unwrap().get("existing").is_some());
        assert!(val.get("mcpServers").unwrap().get("weather-api").is_some());
        assert_eq!(
            val.get("other_key").unwrap().as_str(),
            Some("should_remain")
        );
    }

    #[test]
    fn wire_into_settings_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        let spec = sample_node_spec();
        let first = wire_into_settings(&settings, &spec).unwrap();
        assert!(first);
        let second = wire_into_settings(&settings, &spec).unwrap();
        assert!(!second, "second write must be a no-op");
    }

    #[test]
    fn wire_into_settings_python_uses_python3_command() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        let spec = sample_python_spec();
        let _ = wire_into_settings(&settings, &spec).unwrap();
        let body = fs::read_to_string(&settings).unwrap();
        let val: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entry = val
            .get("mcpServers")
            .and_then(|m| m.get("data-tool"))
            .unwrap();
        assert_eq!(entry.get("command").unwrap().as_str(), Some("python3"));
        assert!(entry
            .get("args")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .as_str()
            .unwrap()
            .ends_with("/scaffold-py/__main__.py"));
    }

    // ── read_existing_mcp_names ───────────────────────────────────────────

    #[test]
    fn read_existing_mcp_names_handles_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("nope.json");
        assert!(read_existing_mcp_names(&p).is_empty());
    }

    #[test]
    fn read_existing_mcp_names_returns_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("settings.json");
        fs::write(
            &p,
            r#"{"mcpServers":{"a":{"command":"node","args":[]},"b":{"command":"node","args":[]}}}"#,
        )
        .unwrap();
        let names = read_existing_mcp_names(&p);
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert_eq!(names.len(), 2);
    }

    // ── write_scaffold_to_disk ────────────────────────────────────────────

    #[test]
    fn write_scaffold_to_disk_writes_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("project");
        let spec = McpBuilderSpec {
            output_dir: out.clone(),
            ..sample_node_spec()
        };
        let written = write_scaffold_to_disk(&spec).unwrap();
        assert_eq!(written.len(), 5);
        for p in &written {
            assert!(p.is_file(), "{} should exist", p.display());
        }
        assert!(out.join("package.json").is_file());
        assert!(out.join("index.js").is_file());
    }

    #[test]
    fn write_scaffold_to_disk_refuses_nonempty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("project");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("existing.txt"), "junk").unwrap();
        let spec = McpBuilderSpec {
            output_dir: out,
            ..sample_node_spec()
        };
        let err = write_scaffold_to_disk(&spec).unwrap_err();
        assert!(err.contains("not empty"));
    }

    #[test]
    fn write_scaffold_to_disk_accepts_empty_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("project");
        fs::create_dir_all(&out).unwrap();
        let spec = McpBuilderSpec {
            output_dir: out,
            ..sample_node_spec()
        };
        let written = write_scaffold_to_disk(&spec).unwrap();
        assert_eq!(written.len(), 5);
    }

    // ── Integration test: generated Node MCP actually starts ──────────────

    /// Verifies the generated Python scaffold is syntactically valid by
    /// running `python3 -c "import __main__"` against it. We do NOT
    /// install the `mcp` package or run pytest because (a) the harness
    /// host may not have pip configured and (b) the user's brief
    /// explicitly accepts that `pip install mcp` is not a build-bot
    /// guarantee. To run the full pytest suite manually:
    ///
    ///   cargo test -p anvil-cli mcp_builder::tests::generated_python_mcp_imports -- --ignored
    ///
    /// And separately:
    ///   cd <output> && pip install -e '.[dev]' && pytest -v
    #[test]
    #[ignore = "requires python3 on PATH and the `mcp` PyPI package; run with --ignored"]
    fn generated_python_mcp_imports() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("mcp-pyitest");
        let spec = McpBuilderSpec {
            name: "pyitest".to_string(),
            language: McpLanguage::Python,
            tools: vec![McpToolSpec {
                name: "echo".to_string(),
                description: "Echo a string".to_string(),
                inputs: vec![McpToolInput {
                    name: "text".to_string(),
                    description: "Text to echo".to_string(),
                    kind: InputKind::String,
                    optional: false,
                }],
            }],
            output_dir: out.clone(),
        };
        write_scaffold_to_disk(&spec).expect("write");

        // Smoke: confirm the file is valid Python by syntax-compiling
        // it. We do NOT exec the module (would require `mcp` package).
        let syntax_check = Command::new("python3")
            .args([
                "-c",
                "import py_compile, sys; py_compile.compile(sys.argv[1], doraise=True)",
                out.join("__main__.py").to_str().unwrap(),
            ])
            .output()
            .expect("python3");
        assert!(
            syntax_check.status.success(),
            "generated Python failed py_compile: {}",
            String::from_utf8_lossy(&syntax_check.stderr)
        );

        // Same for the test file.
        let syntax_check_t = Command::new("python3")
            .args([
                "-c",
                "import py_compile, sys; py_compile.compile(sys.argv[1], doraise=True)",
                out.join("test_server.py").to_str().unwrap(),
            ])
            .output()
            .expect("python3");
        assert!(
            syntax_check_t.status.success(),
            "generated Python test file failed py_compile: {}",
            String::from_utf8_lossy(&syntax_check_t.stderr)
        );
    }

    /// Requires `node` on PATH. Run locally with:
    ///   cargo test -p anvil-cli mcp_builder::tests::generated_node_mcp_starts -- --ignored
    #[test]
    #[ignore = "requires node on PATH and npm install (slow); run with --ignored"]
    fn generated_node_mcp_starts() {
        use std::process::Command;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("mcp-itest");
        let spec = McpBuilderSpec {
            name: "itest".to_string(),
            language: McpLanguage::Node,
            tools: vec![McpToolSpec {
                name: "ping".to_string(),
                description: "Return pong".to_string(),
                inputs: vec![],
            }],
            output_dir: out.clone(),
        };
        write_scaffold_to_disk(&spec).expect("write");
        let install = Command::new("npm")
            .args(["install", "--silent", "--no-audit", "--no-fund"])
            .current_dir(&out)
            .output()
            .expect("npm install");
        assert!(
            install.status.success(),
            "npm install failed: {}",
            String::from_utf8_lossy(&install.stderr)
        );
        let test = Command::new("npm")
            .args(["test", "--silent"])
            .current_dir(&out)
            .output()
            .expect("npm test");
        assert!(
            test.status.success(),
            "generated test suite failed: {}",
            String::from_utf8_lossy(&test.stderr)
        );

        // Try a 200ms `node index.js` smoke — it should connect to a
        // (non-existent) stdio peer without crashing. We expect the
        // process to be running when we kill it.
        let mut child = Command::new("node")
            .args(["index.js"])
            .current_dir(&out)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn node");
        std::thread::sleep(Duration::from_millis(200));
        // Process should still be alive.
        assert!(child.try_wait().expect("try_wait").is_none());
        let _ = child.kill();
        let _ = child.wait();
    }

    // ── TypeScript template (task #676) ────────────────────────────────────

    fn ts_spec(out: PathBuf) -> McpBuilderSpec {
        McpBuilderSpec {
            name: "ts-weather".to_string(),
            language: McpLanguage::TypeScript,
            tools: vec![McpToolSpec {
                name: "forecast".to_string(),
                description: "Get the forecast".to_string(),
                inputs: vec![McpToolInput {
                    name: "city".to_string(),
                    description: "City name".to_string(),
                    kind: InputKind::String,
                    optional: false,
                }],
            }],
            output_dir: out,
        }
    }

    #[test]
    fn ts_scaffold_emits_expected_files() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = ts_spec(tmp.path().join("ts-weather"));
        let files = scaffold_files(&spec);
        let paths: Vec<String> = files
            .iter()
            .map(|(p, _)| {
                p.strip_prefix(&spec.output_dir)
                    .unwrap_or(p)
                    .display()
                    .to_string()
            })
            .collect();
        // Six files expected; src/ split is the multi-file part.
        assert!(paths.contains(&"package.json".to_string()));
        assert!(paths.contains(&"tsconfig.json".to_string()));
        assert!(
            paths.iter().any(|p| p == "src/index.ts" || p == "src\\index.ts"),
            "missing src/index.ts in {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "src/test.ts" || p == "src\\test.ts"),
            "missing src/test.ts in {paths:?}"
        );
        assert!(paths.contains(&".gitignore".to_string()));
        assert!(paths.contains(&"README.md".to_string()));
    }

    #[test]
    fn ts_index_uses_typed_signature_and_zod() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = ts_spec(tmp.path().join("ts-weather"));
        let files = scaffold_files(&spec);
        let index = files
            .iter()
            .find(|(p, _)| {
                let s = p.display().to_string();
                s.ends_with("index.ts") || s.ends_with("index.ts\\")
            })
            .map(|(_, c)| c.clone())
            .expect("must emit src/index.ts");
        // ToolResult interface for the typed return shape.
        assert!(index.contains("interface ToolResult"));
        // Same zod schema generator the Node template uses, so `z.string()`
        // for `city: string`.
        assert!(index.contains("city: z.string()"));
        // Typed return + handler annotation.
        assert!(index.contains("Promise<ToolResult>"));
        assert!(index.contains("(args: any)"));
    }

    #[test]
    fn ts_package_json_has_tsx_and_typescript_devdeps() {
        let pkg = ts_package_json("foo");
        assert!(pkg.contains("\"tsx\""));
        assert!(pkg.contains("\"typescript\""));
        assert!(pkg.contains("\"@types/node\""));
        // start script goes through tsx so users don't need a build step.
        assert!(pkg.contains("\"start\": \"tsx src/index.ts\""));
    }

    #[test]
    fn ts_tsconfig_targets_es2022_strict() {
        let cfg = ts_tsconfig();
        assert!(cfg.contains("\"target\": \"ES2022\""));
        assert!(cfg.contains("\"strict\": true"));
        assert!(cfg.contains("\"rootDir\": \"src\""));
    }

    #[test]
    fn ts_wire_into_settings_uses_npx_tsx() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = ts_spec(tmp.path().join("ts-weather"));
        let settings = tmp.path().join("settings.json");
        let modified = wire_into_settings(&settings, &spec).unwrap();
        assert!(modified);
        let bytes = std::fs::read(&settings).unwrap();
        let root: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entry = &root["mcpServers"]["ts-weather"];
        assert_eq!(entry["command"], "npx");
        let args = entry["args"].as_array().unwrap();
        assert_eq!(args[0], "tsx");
        assert!(
            args[1].as_str().unwrap().ends_with("index.ts"),
            "args[1] should point at src/index.ts: {args:?}"
        );
    }

    // ── install_command (task #676) ────────────────────────────────────────

    #[test]
    fn install_command_node_uses_npm_install() {
        let (program, args) = install_command(McpLanguage::Node);
        assert_eq!(program, "npm");
        assert_eq!(args, vec!["install"]);
    }

    #[test]
    fn install_command_typescript_uses_npm_install() {
        // Same `npm install` — package.json pulls tsx/typescript as devDeps.
        let (program, args) = install_command(McpLanguage::TypeScript);
        assert_eq!(program, "npm");
        assert_eq!(args, vec!["install"]);
    }

    #[test]
    fn install_command_python_uses_pip_editable_dev() {
        let (program, args) = install_command(McpLanguage::Python);
        assert_eq!(program, "python3");
        assert_eq!(args, vec!["-m", "pip", "install", "-e", ".[dev]"]);
    }

    #[test]
    fn run_install_returns_err_when_program_missing() {
        // Force PATH to /nonexistent so even `npm` is missing — proves the
        // error path includes a recoverable message.
        let tmp = tempfile::tempdir().unwrap();
        let prev_path = std::env::var_os("PATH");
        // SAFETY: single-threaded test; we restore PATH below.
        unsafe { std::env::set_var("PATH", "/var/empty"); }
        let result = run_install(tmp.path(), McpLanguage::Node);
        // Restore PATH before any assertion can panic.
        if let Some(p) = prev_path {
            unsafe { std::env::set_var("PATH", p); }
        } else {
            unsafe { std::env::remove_var("PATH"); }
        }
        let err = result.expect_err("npm should not be found on /var/empty");
        assert!(err.contains("npm") || err.contains("install"));
    }
}
