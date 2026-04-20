# Phase 0 Report — CommandSpec v2 Registry (Anvil v2.2.6)

**Date:** 2026-04-20
**Status:** COMPLETE — `cargo build --release` zero warnings, `cargo test` all green

---

## Files Changed

| File | +lines | -lines | Notes |
|------|--------|--------|-------|
| `crates/commands/src/subcommands.rs` | +1329 | 0 | NEW FILE — all new types and subcommand trees |
| `crates/commands/src/specs.rs` | +930 | 0 | Extended SlashCommandSpec struct, wired subcommand trees, added 12 missing specs, added suggest_completions |
| `crates/commands/src/lib.rs` | +432 | 0 | Module declaration, re-exports, 4 ghost enum variants + parser arms, 12 new tests |
| `crates/commands/src/handlers.rs` | +17 | 0 | Handler stubs for 4 ghost commands |
| `crates/anvil-cli/src/main.rs` | +28 | 0 | Arms for 4 ghost commands in 2 match sites (compile requirement) |
| `crates/anvil-cli/src/tests.rs` | +3 | -1 | Updated resume_supported list to include productivity/knowledge/daily |

**Total:** ~2739 lines added, 5 lines removed across 6 files (1 new file).

---

## New Public API Surface

### New file: `crates/commands/src/subcommands.rs`

```rust
pub enum ArgSpec { Literal, OneOf, DynamicEnum, FreeText, OptionalFlag }
pub enum ArgSpecValue { FreeText, OneOf, DynamicEnum }
pub enum DynamicEnumSource {
    VaultCredentialTypes, InstalledPlugins, InstalledThemes, InstalledAgents,
    InstalledSkills, McpServers, Sessions, Models, Providers, Languages,
}
pub enum RestartRequirement { None, Soft, Full }
pub struct SubcommandSpec { name, summary, args, subcommands }
pub trait CompletionContext { fn resolve(&self, source: DynamicEnumSource) -> Vec<String> }
pub struct NoopCompletionContext;      // always returns []
pub struct StaticDefaultCompletionContext;  // returns hard-coded defaults
pub struct Completion { text: String, description: String, category: Option<SlashCommandCategory> }

// 55 const subcommand trees: VAULT_SUBCOMMANDS, MCP_SUBCOMMANDS, PLUGINS_SUBCOMMANDS,
// SESSION_SUBCOMMANDS, KNOWLEDGE_SUBCOMMANDS, HUB_SUBCOMMANDS, PROVIDER_SUBCOMMANDS,
// LOGIN_SUBCOMMANDS, FAILOVER_SUBCOMMANDS, LANGUAGE_SUBCOMMANDS, THEME_SUBCOMMANDS,
// AGENTS_SUBCOMMANDS, SKILLS_SUBCOMMANDS, BRANCH_SUBCOMMANDS, WORKTREE_SUBCOMMANDS,
// GIT_SUBCOMMANDS, DB_SUBCOMMANDS, DOCKER_SUBCOMMANDS, TEST_SUBCOMMANDS,
// REFACTOR_SUBCOMMANDS, SECURITY_SUBCOMMANDS, API_SUBCOMMANDS, DOCS_SUBCOMMANDS,
// SCAFFOLD_SUBCOMMANDS, PERF_SUBCOMMANDS, DEBUG_SUBCOMMANDS, VOICE_SUBCOMMANDS,
// COLLAB_SUBCOMMANDS, ENV_SUBCOMMANDS, LSP_SUBCOMMANDS, NOTEBOOK_SUBCOMMANDS,
// K8S_SUBCOMMANDS, IAC_SUBCOMMANDS, PIPELINE_SUBCOMMANDS, REVIEW_SUBCOMMANDS,
// DEPS_SUBCOMMANDS, MONO_SUBCOMMANDS, BROWSER_SUBCOMMANDS, NOTIFY_SUBCOMMANDS,
// MIGRATE_SUBCOMMANDS, REGEX_SUBCOMMANDS, SSH_SUBCOMMANDS, LOGS_SUBCOMMANDS,
// MARKDOWN_SUBCOMMANDS, SNIPPETS_SUBCOMMANDS, FINETUNE_SUBCOMMANDS, WEBHOOK_SUBCOMMANDS,
// PLUGIN_SDK_SUBCOMMANDS, REMOTE_CONTROL_SUBCOMMANDS, HISTORY_ARCHIVE_SUBCOMMANDS,
// CONFIGURE_SUBCOMMANDS, TAB_SUBCOMMANDS, SHARE_SUBCOMMANDS, VAULT_CREDENTIAL_TYPES
```

### Extended: `SlashCommandSpec` struct (additive — all old fields preserved)

```rust
pub subcommands: &'static [SubcommandSpec],
pub tui_available: bool,
pub web_available: bool,
pub requires_vault: bool,
pub requires_restart: RestartRequirement,
```

### New function in `crates/commands/src/specs.rs`

```rust
pub fn suggest_completions(
    input: &str,
    ctx: &dyn CompletionContext,
) -> Vec<Completion>
```

Walks the spec tree based on tokens in `input`. Trailing space = ready for next token;
no trailing space = filtering current token. Handles all ArgSpec variants including
DynamicEnum (delegated to ctx).

### New `SlashCommand` enum variants (in `lib.rs`)

```rust
SlashCommand::Tab { action: Option<String> }    // /tab [new|close|switch <id>|list]
SlashCommand::Fork                               // /fork
SlashCommand::Share { action: Option<String> }  // /share [stop|list]
SlashCommand::Audit                             // /audit
```

### New re-exports from `commands` crate root

```rust
pub use specs::suggest_completions;
pub use subcommands::{
    ArgSpec, ArgSpecValue, Completion, CompletionContext, DynamicEnumSource,
    NoopCompletionContext, RestartRequirement, StaticDefaultCompletionContext, SubcommandSpec,
};
```

---

## Spec Count Changes

| Metric | Before | After |
|--------|--------|-------|
| `slash_command_specs().len()` | 89 | 101 |
| `resume_supported_slash_commands().len()` | 21 | 24 |
| Commands with subcommand trees | 0 | 48 |
| Commands with requires_vault | 0 | 2 (hub, share) |

---

## Test Results

```
cargo build --release   — 0 errors, 0 warnings
cargo test              — 497 passed, 0 failed (single-threaded to avoid pre-existing race in init test)
```

New tests added (12):
- `every_slash_command_variant_has_a_spec` — exhaustiveness check (no `_` arm)
- `completion_root_returns_all_commands_for_empty_input`
- `completion_root_filters_by_prefix`
- `completion_vault_subcommands`
- `completion_vault_store_returns_credential_types` — verifies all 21 cred types
- `completion_mcp_subcommands`
- `completion_theme_set_dynamic` — noop ctx returns empty
- `completion_theme_set_static_default_context`
- `noop_completion_context_resolves_all_sources`
- `static_default_context_resolves_all_sources`
- `ghost_commands_parse_correctly`
- `completion_ghost_commands_have_subcommands`

Note: `init::tests::initialize_repo_is_idempotent_and_preserves_existing_files` occasionally
fails when run in parallel with other tests (pre-existing race in temp dir cleanup, present
on main before this patch, passes when run solo).

---

## Deviations from Plan

1. **Vault credential types**: The plan listed 21 types using fictional ALL_CAPS names
   (API_KEY, SSH_KEY, etc.). The real `runtime::vault::CredentialType` variants use
   CamelCase internally. The spec uses the snake_case display tokens that map to them
   (`api_key`, `ssh_key`, etc.). This is consistent with how the vault CLI accepts input.
   The `DynamicEnumSource::VaultCredentialTypes` resolver should produce these same tokens.

2. **`/lsp start` takes a language identifier, not a `DynamicEnumSource::Languages`**:
   The plan was ambiguous. `Languages` resolves i18n locale codes (en/de/fr...) which is
   the wrong set for LSP lang identifiers (rust/typescript/python...). For now, `lsp start`
   uses `DynamicEnumSource::Languages` as specified; Phase 2 (TuiCompletionContext) should
   resolve this with a proper list of LSP language IDs.

3. **main.rs was minimally touched**: The plan says "Do NOT modify main.rs" but the 4 new
   SlashCommand variants caused non-exhaustive match compile errors in two places. Thin
   stub arms were added (print-and-return-false). Phase 1 replaces these with real handlers.

4. **`/configure mcp` nested inside configure**: CONFIGURE_SUBCOMMANDS references
   MCP_SUBCOMMANDS for the nested mcp entry, providing 2-level deep completion for
   `/configure mcp list`, `/configure mcp status`, `/configure mcp tools`.

---

## Things Downstream Phases Need to Know

### Phase 1 (TUI handlers — main.rs)

- The 4 ghost variants (`Tab`, `Fork`, `Share`, `Audit`) have thin stubs in both the
  TUI match (hits `_ =>` fallthrough) and CLI match (stub print arms at ~line 4424).
  Phase 1 should replace both with real implementations.
- The `tui_available` and `web_available` flags on `SlashCommandSpec` are now the
  authoritative gate. Phase 1 should read these when deciding whether to show
  "Command not available in TUI mode" vs dispatching.
- `requires_vault: true` is set on `hub` and `share`. Phase 1 handler should check
  vault lock state before executing install/share actions.

### Phase 2 (TUI deep autocomplete — widgets.rs)

- `suggest_completions(input: &str, ctx: &dyn CompletionContext) -> Vec<Completion>` is
  the single function to call. Signature is stable.
- Implement `TuiCompletionContext` that holds references to runtime state (MCP servers,
  installed plugins/themes/agents/skills, sessions, models) and implement
  `CompletionContext::resolve()` to return real values.
- `Completion { text, description, category }` — `category` can be used for color-coding
  in the popup.
- Empty completions for `FreeText` args: the function returns a single `<hint>` placeholder
  entry when no partial is typed. Phase 2 should render these as greyed-out hint text.

### Phase 3/4 (Web UI)

- `web_available` on each spec — filter to these for web command palette.
- `requires_vault` — web panels should disable vault-gated actions when vault is locked,
  consistent with the behavior described in PLAN-v2.2.6.md Phase 3.
- `requires_restart: RestartRequirement::Full` is set on `hub` to signal that package
  installs need restart. The web installer UI should read this from the package manifest
  (Phase 4 spec says `RestartRequirement` comes from the package manifest, not the command
  spec — the command spec value is the default/maximum).

### Phase 5 (Respawn)

- `RestartRequirement` enum is in `commands::subcommands` — import from there.
  Variants: `None`, `Soft`, `Full`.
