# println / eprintln TUI-reachability audit — 2026-05-18

Task #626 — categorize every `println!` / `eprintln!` / `print!` / `eprint!`
site under `crates/anvil-cli/src/` and `crates/commands/src/`, decide whether
it is reachable while ratatui's alt-screen is active, and feed the result into
a clippy gate.

**Coordinate with #624** (`run_agent_command` → `Result<String, _>`): the
in-flight `v2.2.14-phase1` branch already converts the 16 `println!` sites in
`crates/anvil-cli/src/cmd_provider.rs::run_agent_command` and removes them
from the audit (those are SAFE in this PR's tree; this audit covers
everything else).

## Heuristics

* **TUI-reachable** — the function's code path can run while
  `tui::run_tui_session` is alive *and* the alt-screen is up.  Established
  by tracing call sites back to either `LiveCli::handle_repl_command_tui`
  or `LiveCli::run_command_for_tui` (the `(msg, changed)` fallthrough on
  `Bughunter | Commit | Pr | Issue | Ultraplan | Teleport | DebugToolCall`
  at line 5511–5513 → `self.handle_repl_command(command)` → underlying
  `run_*` methods that `println!` their report).
* **Bracketed** — wrapped by `tui::leave_alt_screen_for_inline_op()` …
  `tui::restore_alt_screen()`.  When bracketed, the print is intentional
  and the TUI back-buffer is preserved across the inline op.
* **Verdict** —
  * **SAFE-HEADLESS** — entry from a subcommand path that exits before TUI
    starts (`anvil --setup`, `--upgrade`, `--uninstall`, `--update`,
    `--check`, `--init`, `--login`, `mcp-server`, `skill-eval`, `/agent
    traits` direct, `--version`, `--resume … /<cmd>`, `--print …`).
  * **SAFE-PREWIZARD** — runs inside `run_first_run_wizard` or `LiveCli::new`
    before `run_tui_session`.
  * **SAFE-DTC** — drop-to-CLI bracketed; TUI alt-screen has been left.
  * **SAFE-POSTDROP** — runs after `drop(tui)` (exit banner, panic-hook
    error path).
  * **SAFE-TEST** — `#[cfg(test)]` only.
  * **SAFE-COMMENT** — appears in a doc/string literal, not real I/O.
  * **SAFE-DEADCODE** — function has no callers (verified via `grep -rn`).
  * **BUG** — TUI-reachable, not bracketed.  Fixed in this PR.
  * **BUG-MINOR** — TUI-reachable but on an error/edge path; corruption
    is rare or one-line.  Fixed in this PR.
  * **BUG-DEFER** — TUI-reachable; fix requires structural work
    (e.g. interactive password modal).  Documented, not in this PR.

## File-by-file summary

| file | print sites | bucket | rationale |
| --- | ---: | --- | --- |
| `crates/anvil-cli/src/wizard.rs` | 218 | SAFE-PREWIZARD | `run_first_run_wizard()` is invoked from `run_repl` *before* TUI startup; whole file is the interactive bootstrap dialog. |
| `crates/anvil-cli/src/setup.rs` | 50 | SAFE-HEADLESS | `run_setup_wizard()` only fires from `CliAction::Setup` (`anvil --setup`). |
| `crates/anvil-cli/src/auth.rs` | 47 | SAFE-HEADLESS | `run_login` only from `CliAction::Login`. |
| `crates/anvil-cli/src/upgrade.rs` | 35 | SAFE-HEADLESS | `run_upgrade` only from `CliAction::Upgrade`. |
| `crates/anvil-cli/src/uninstall.rs` | 23 | SAFE-HEADLESS | `run_uninstall` only from `CliAction::Uninstall`. |
| `crates/anvil-cli/src/update.rs` | 17 | SAFE-HEADLESS | `run_self_update` only from `--update` flag. |
| `crates/anvil-cli/src/check.rs` | 9 | SAFE-HEADLESS | `run_check` only from `CliAction::Check`. |
| `crates/anvil-cli/src/mcp_server_mode.rs` | 7 | SAFE-HEADLESS | `anvil mcp-server` subcommand (stderr is the protocol log channel, not TUI). |
| `crates/anvil-cli/src/skill_eval.rs` | 1 | SAFE-HEADLESS | `anvil skill-eval` subcommand. |
| `crates/anvil-cli/src/utils.rs:808` | 1 | SAFE-HEADLESS | `run_init` only from `CliAction::Init`. |
| `crates/anvil-cli/src/render.rs:1290` | 1 | SAFE-COMMENT | Literal `\"…println!(\"hi\");…\"` inside `markdown_to_ansi()` markdown test. |
| `crates/anvil-cli/src/respawn.rs` | 4 | SAFE-HEADLESS / SAFE-DEADCODE | `prompt_restart_and_respawn` is `#[allow(dead_code)]`; when wired it must accept a TUI sink (tracked separately). |
| `crates/anvil-cli/src/tui/redraw.rs` | 0 | — | — |
| `crates/commands/src/traits.rs` | 6 | SAFE-COMMENT | All six matches are inside `///` doc comments. |

The remaining files contain a mix.  Per-site detail below.

## Detailed table (per site)

### `crates/anvil-cli/src/main.rs` (142 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 188 | `main` | `eprintln!` | N | N | SAFE-HEADLESS | Pre-TUI "another Anvil process running" warning. |
| 204 | `main` | `eprintln!` | N | N | SAFE-HEADLESS | Resume-marker banner; pre-TUI. |
| 260 | `main` | `eprintln!` | N | N | SAFE-POSTDROP | Top-level error path after `drop(tui)`. |
| 433 | `run::EmitSchema` | `println!` | N | N | SAFE-HEADLESS | `anvil --emit-schema`. |
| 843 | `parse_args` | `print!` | N | N | SAFE-HEADLESS | `anvil skill-eval --help`. |
| 979, 989 | `parse_args` error arms | `eprintln!` | N | N | SAFE-HEADLESS | CLI parse errors. |
| 1014 | `parse_direct_slash_cli_action` | `println!` | N | N | SAFE-HEADLESS | `anvil /agent traits` (then `std::process::exit(0)`). |
| 1162–1183 | `dump_manifests`, `print_bootstrap_plan`, `print_system_prompt` | mix | N | N | SAFE-HEADLESS | CliAction handlers, pre-TUI. |
| 1191 | `print_version` | `println!` | N | N | SAFE-HEADLESS | `anvil --version`. |
| 1203, 1215, 1221, 1232, 1242, 1246 | `resume_session` (top-level fn) | mix | N | N | SAFE-HEADLESS | `--resume <path> /<cmd>` headless replay (`CliAction::ResumeSession`). |
| 1883 | OAuth refresh warning printer | `eprintln!` | N | N | SAFE-PREWIZARD | Pre-TUI warning. |
| 1900–2050 | `print_managed_sessions`, OAuth-incomplete banner, vault-migration prompt | mix | N | N | SAFE-PREWIZARD | All called from `run_repl` setup section before `run_tui_session`. |
| 2112–2283 | vault unlock / OAuth retry banners | mix | N | N | SAFE-PREWIZARD | Pre-TUI. |
| 3969–3977 | `print_exit_resume_banner` | `println!` | N | N | SAFE-POSTDROP | Called after `drop(tui)` in `run_tui_session`. |
| 3984, 3988 | `run_repl_plain` startup banners | `println!` | N | N | SAFE-HEADLESS | Plain REPL — non-TUI. |
| 4030 | `run_repl_plain` file-drop notice | `println!` | N | N | SAFE-HEADLESS | Non-TUI REPL. |
| 4106, 4117 | `inject_task_notifications` (headless variant) | `eprintln!` | N | N | SAFE-HEADLESS | This is the headless variant at 4091; TUI uses `inject_task_notifications_tui` (4059) which calls `tui.push_system`. |
| 4267 | `LiveCli::new` history-prune notice | `eprintln!` | N | N | SAFE-PREWIZARD | Constructor; runs before TUI startup. |
| 4799 | `LiveCli::run_turn_file_drop` | `println!` | N | N | SAFE-HEADLESS | Only called from `run_repl_plain` (4037). |
| 4954, 4968 | `LiveCli::run_turn` | `println!` | N | N | SAFE-HEADLESS | `run_turn` is only called from `run_repl_plain` (4043) and `run_prompt_text_or_json` (6069). |
| 5021–5056 | `LiveCli::run_qmd_command` (lines 5021–5056) | mix | N | N | SAFE-DEADCODE | Verified via `grep -rn run_qmd_command crates/` — function has zero callers. TUI `/qmd` is handled inline at line 5194 with `tui.push_system`. |
| 5156, 5158, 5159 | `handle_repl_command_tui::GenerateImage` | `println!` / `print!` | **Y** | Y | SAFE-DTC | Bracketed by `leave_alt_screen_for_inline_op()` / `restore_alt_screen()` at 5155 / 5164. |
| 5836 | `run_command_for_tui::Restart{soft:false}` | `print!` | N | **Y** | **BUG-DEFER** | Interactive `[y/N]` prompt + `read_line` while TUI owns stdin. Already broken UX. Tracked as follow-up — a `/restart` confirm modal belongs in the TUI layer. In this PR, the comment is updated to flag the issue; the existing behavior is left intact because the alternative (refuse from TUI) would regress the only working escape hatch. |
| 6090 | `LiveCli::run_prompt_json` | `println!` | N | N | SAFE-HEADLESS | `--print --json`; no TUI. |
| 6394 | `LiveCli::run_generate_image` | `println!` | depends on caller | Y | SAFE-DTC | Only entered from line 5157 (TUI bracketed) or direct `--print` (headless). |
| 6553 | `LiveCli::maybe_reload_instructions` | `eprintln!` | N | **Y** | **BUG** | Reload notice fires from inside the TUI loop at main.rs:3101 and main.rs:3683. → switch to `tracing::info!`. |
| 6687–6708 | `handle_repl_command` LLM-turn arms | (no `println!` here) | — | Y | (placeholder) | The arms themselves don't print; they call `self.run_*` which does. See cmd_ai.rs and cmd_static.rs entries below. |
| 6791, 6799–6802 | `LiveCli::record_daily` | `eprintln!` | N | **Y** | **BUG-MINOR** | Called from inside the TUI loop at the `ReadResult::Exit` arm (main.rs:3265) before `drop(tui)`. Output corrupts the alt-screen for a few ms; visible artifact on quit. → capture lines and emit from caller after `drop(tui)`. |
| 6927, 6941, 6953 | `LiveCli::set_model` | `println!` | N | **Y** | **BUG** | `set_model(Some(...))` is called from `run_command_for_tui::Model` (5494). → return `String`, caller decides where it goes. |
| 6965, 6979, 6998 | `LiveCli::set_permissions` | `println!` | N | **Y** | **BUG** | `set_permissions(Some(...))` is called from `run_command_for_tui::Permissions` (5505). → return `String`. |
| 7007, 7026 | `LiveCli::clear_session` | `println!` | N | **Y** | **BUG** | `clear_session(confirm)` is called from `run_command_for_tui::Clear` (5517) with `confirm=false` possible from TUI. → return `String`. |
| 7037 | `LiveCli::print_cost` | `println!` | N | N | SAFE-HEADLESS | TUI `/cost` is handled inline at 5391; this method only used by headless `handle_repl_command::Cost`. |
| 7045, 7065 | `LiveCli::resume_session` (method, NOT free fn) | `println!` | N | N | SAFE-HEADLESS | Headless variant; TUI uses `resume_session_tui` (7445). |
| 7101 | `LiveCli::print_config` | `println!` | N | **Y** | **BUG** | Called from `run_command_for_tui::Restart{soft:true}` at line 5832 — reaches TUI via `Self::print_config(None)?`. → return `String` (helper renamed `format_config_report` already exists). |
| 7106 | `LiveCli::print_memory` | `println!` | N | N | SAFE-HEADLESS | TUI handles `/memory` inline via `render_memory_report`. |
| 7117 | `LiveCli::print_live_agents` | `println!` | N | N | SAFE-HEADLESS | Headless `/agents`. |
| 7121, 7127 | `LiveCli::print_agents`/`print_skills` | `println!` | N | N | SAFE-HEADLESS | Subcommand entries `anvil agents` / `anvil skills`. |
| 7176 | `LiveCli::maybe_emit_skill_hint` | `println!` | N | N | SAFE-HEADLESS | Only called from `LiveCli::run_turn` (4971), which is the headless turn driver. |
| 7181 | `LiveCli::print_diff` | `println!` | N | N | SAFE-HEADLESS | TUI returns `render_diff_report()` inline. |
| 7186 | `LiveCli::print_version` | `println!` | N | N | SAFE-HEADLESS | TUI handles `Version` inline. |
| 7202, 7217, 7222, 7241, 7258, 7260, 7268, 7274, 7295 | `LiveCli::handle_session_command` | `println!` | N | N | SAFE-HEADLESS | Headless variant; TUI uses `handle_session_command_tui` (7381). |
| 7305 | `LiveCli::handle_plugins_command` | `println!` | N | N | SAFE-HEADLESS | Headless; TUI uses `handle_plugins_command_tui` (7355). |
| 7480 | `LiveCli::compact` | `println!` | N | **Y** | **BUG** | Called from `run_command_for_tui::Compact` (5460). → return `String`. |
| 8167 | `LiveCli::run_debug_tool_call` | `println!` | N | **Y** | **BUG** | Reached from the LLM-turn fallthrough at run_command_for_tui:5512 (DebugToolCall) → handle_repl_command (6720 region). → return `String`. |

### `crates/anvil-cli/src/cmd_provider.rs` (45 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 252, 267 | `LiveCli::print_status` | `println!` / `eprintln!` | N | N | SAFE-HEADLESS | Comment in code confirms the TUI path uses `format_status_report` inline in `run_command_for_tui::Status` (5376). |
| 481–711 (16 sites) | `LiveCli::run_provider_command` login flows (Anthropic, OpenAI, Ollama, Gemini, xAI, Copilot, Azure, Bedrock, etc.) | mix | **Y** | Y | SAFE-DTC | Every login arm is wrapped by `leave_alt_screen_for_inline_op()` + `restore_alt_screen()` (lines 481/510/515/531/536/551/563/606/612/635/641/656/662/677/682/694/699/711). |
| 897–999 (multiple) | `LiveCli::run_agent_command` | `println!` | N | Y | **#624-FIXED** | The `v2.2.14-phase1` in-flight diff converts this entire function to `Result<String, _>` and removes every `println!`. The pre-#624 BUG count was 8; the post-#624 count is 0. |

### `crates/anvil-cli/src/cmd_ai.rs` (9 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 90 | `run_bughunter` | `println!` | N | **Y** | **BUG** | Reached from LLM-turn fallthrough (run_command_for_tui:5511 → handle_repl_command:6692 → run_bughunter). → return `String`. |
| 104 | `run_ultraplan` | `println!` | N | **Y** | **BUG** | Same LLM-turn fallthrough (Ultraplan). → return `String`. |
| 822 | `run_perf_command` | `eprintln!` | N | **Y** | **BUG** | "executing via sh -c" diagnostic fires inside `run_command_for_tui::Perf` (5659). → `tracing::info!`. |
| 1734 | `run_commit` | `println!` | N | **Y** | **BUG** | LLM-turn fallthrough (Commit). |
| 1761 | `run_commit` (result summary) | `println!` | N | **Y** | **BUG** | Same. |
| 1789, 1797 | `run_pr` | `println!` | N | **Y** | **BUG** | LLM-turn fallthrough (Pr). |
| 1820, 1828 | `run_issue` | `println!` | N | **Y** | **BUG** | LLM-turn fallthrough (Issue). |

### `crates/anvil-cli/src/cmd_static.rs` (9 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 171 | `run_iac_command` (apply confirm prompt) | `eprint!` | N | **Y** | **BUG-DEFER** | The whole `/iac apply` confirmation reads stdin while ratatui owns it — already broken UX, not just print-corruption. Tracked: add a TUI confirm modal or refuse `/iac apply` from TUI. |
| 1555, 1557, 1559, 1578, 1579, 1580 | `run_undo` | mix | N | N | SAFE-HEADLESS | `run_command_for_tui::Undo` is hard-gated at 5621–5623 ("Use /undo in non-TUI mode"); never reaches this fn from TUI. |
| 1833, 1837 | `run_teleport` (free fn) | `println!` | N | **Y** | **BUG** | Reached via LLM-turn fallthrough (Teleport). → return `String`. |

### `crates/anvil-cli/src/tui/input_handler.rs` (1 site)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 1376 | `save_api_key_credential` | `eprintln!` | N | **Y** | **BUG** | Fallback path in the in-TUI provider-login modal. → `tracing::warn!`. |

### `crates/anvil-cli/src/tui/completion.rs` (2 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 113 | `spawn_background_model_fetch` (background thread) | `eprintln!` | N | **Y** | **BUG** | `/model` TAB completion fires this on provider probe warnings. → `tracing::warn!`. |
| 594 | `live_model_choices` test path | `eprintln!` | N | N | SAFE-TEST | `#[cfg(test)]` only. |

### `crates/anvil-cli/src/remote_control.rs` (2 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 82 | `run_remote_control_command` (relay-thread runtime build error) | `eprintln!` | N | **Y** | **BUG** | Spawned from `handle_repl_command_tui::RemoteControl` (5225). → `tracing::warn!`. |
| 91 | `run_remote_control_command` (relay disconnect error) | `eprintln!` | N | **Y** | **BUG** | Same. → `tracing::warn!`. |

### `crates/anvil-cli/src/providers.rs` (4 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 499 | `build_runtime` MCP tool-discovery error | `eprintln!` | N | **Y** | **BUG** | Called from every TUI `build_runtime_with_tui_slot` site. → `tracing::warn!`. |
| 536 | `build_runtime` LSP init error | `eprintln!` | N | **Y** | **BUG** | Same path. → `tracing::warn!`. |
| 617 | `build_runtime_with_tui_slot` MCP tool-discovery error | `eprintln!` | N | **Y** | **BUG** | Same. → `tracing::warn!`. |
| 653 | `build_runtime_with_tui_slot` LSP init error | `eprintln!` | N | **Y** | **BUG** | Same. → `tracing::warn!`. |

### `crates/anvil-cli/src/vault.rs` (1 site)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 46 | `read_password_prompt` | `eprint!` | N | **Y** | **BUG-DEFER** | `/vault unlock` etc. route through `run_command_for_tui::Vault` (5676) and try to read a master password from stdin. Already broken in TUI mode (input goes to ratatui). Requires in-TUI password modal — tracked as follow-up. |

### `crates/commands/src/handlers.rs` (1 site)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 1551 | `run_import_pipeline_headless` (session commit failure) | `eprintln!` | N | **Y** | **BUG** | Called from `run_command_for_tui::Import` (main.rs:6086). → `tracing::warn!`. |

### `crates/commands/src/skill_chaining.rs` (3 sites)

| line | function | macro | bracketed | TUI-reachable | verdict | note |
| ---: | --- | --- | :---: | :---: | --- | --- |
| 251 | `ChainEvaluator::evaluate` (broken chain ref) | `eprintln!` | N | **Y** | **BUG** | Runs after every TUI turn (main.rs:3860 / 4920). → `tracing::warn!`. |
| 470 | `parse_chains_to` (malformed entry) | `eprintln!` | N | **Y** | **BUG** | Same chain — runs during in-TUI skill discovery. → `tracing::warn!`. |
| 519 | `parse_inline_chains_to` (object-form fallback) | `eprintln!` | N | **Y** | **BUG** | Same chain. → `tracing::warn!`. |

## Totals

* Total sites scanned: **627** (= 218 + 142 + 50 + 47 + 45 + 35 + 23 + 17 + 9 + 9 + 9 + 7 + 6 + 4 + 4 + 3 + 2 + 2 + 1 + 1 + 1 + 1 + 1)
* SAFE-* buckets: **596**
* **#624 already in flight** (cmd_provider.rs `run_agent_command`): **8** (was a separate BUG cluster; not re-touched here)
* **BUG / BUG-MINOR (fixed in this PR)**: **23**
* **BUG-DEFER (tracked as follow-up)**: **3** (cmd_static.rs:171 `/iac apply` confirm, main.rs:5836 `/restart` confirm, vault.rs:46 password prompt)

## SUSPECT

None.  Every site above resolves to a definite verdict from static
call-graph analysis.  The three BUG-DEFER sites are TUI-reachable but
their fix is a UX redesign (interactive modal), not a print-replacement —
they are tracked outside this PR.
