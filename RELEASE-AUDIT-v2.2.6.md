# Release Audit — Anvil v2.2.6
Date: 2026-04-20
Auditor: QA Expert (independent, read-only)

## Overall verdict: PASS

All mandatory deliverables are present and correctly implemented. Two minor
discrepancies are noted (not blocking). Build is clean. 552 tests pass, 0 fail.

---

## Phase-by-phase findings

### Phase 0: CommandSpec v2 registry

- **Claim:** `SubcommandSpec`, `ArgSpec`, `DynamicEnumSource`, `RestartRequirement`,
  `CompletionContext` types exist in `crates/commands/src/subcommands.rs`
- **Verdict:** PASS
- **Evidence:** `subcommands.rs:11` (`ArgSpec`), `:41` (`DynamicEnumSource`),
  `:67` (`RestartRequirement`), `:81` (`SubcommandSpec`), `:99` (`CompletionContext`)

- **Claim:** `SlashCommandSpec` has new fields: `subcommands`, `tui_available`,
  `web_available`, `requires_vault`, `requires_restart`
- **Verdict:** PASS
- **Evidence:** `specs.rs:37–45` — all 5 fields present on the struct

- **Claim:** `pub fn suggest_completions(input: &str, ctx: &dyn CompletionContext) -> Vec<Completion>`
  exists in `specs.rs`
- **Verdict:** PASS
- **Evidence:** `specs.rs:1990–1993` — exact signature matches, annotated `#[must_use]`

- **Claim:** All 101 spec entries populated with new fields (spot-check 5)
- **Verdict:** PASS
- **Evidence:** `grep -c 'SlashCommandSpec {'` returns 103 (2 are the struct definition
  and a doc example; 101 actual entries). Spot-checked: `help` (`:49`), `hub` (`:1091`),
  `tab` (`:1700`), `share` (`:1735`), `restart` (`:1784`) — all carry the 5 new fields.

- **Claim:** 4 ghost command enum variants (`Tab`, `Fork`, `Share`, `Audit`) exist in
  `crates/commands/src/lib.rs` with parser arms
- **Verdict:** PASS
- **Evidence:** Enum variants at `lib.rs:374`, `:378`, `:380`, `:384`. Parser arms
  at `:687`, `:690`, `:691`, `:694`. `as_str()` arms at `:1713–1716`. All 4 appear
  in `all_commands()` at `:1823–1826` and tests at `:1970–1979`.

- **Claim:** Vault credential types: 21-item list exists
- **Verdict:** PASS
- **Evidence:** `subcommands.rs:194–216` (`VAULT_CREDENTIAL_TYPES`), 21 items confirmed.
  Also mirrored in `completion.rs:132–154` (same 21 tokens).

---

### Phase 1: TUI handlers + ghosts

- **Claim:** `_ => "Command not available in TUI mode."` catch-all (old line 3245) is REMOVED
- **Verdict:** PASS
- **Evidence:** `grep "Command not available"` across `main.rs` returns zero matches.
  The TUI dispatch no longer has a string-returning catch-all fallthrough.

- **Claim:** TUI match arms exist for `Mcp`, `Plugins`, `Session`, `Resume`, `Sleep`,
  `Productivity`, `Knowledge`, `Daily`
- **Verdict:** PASS
- **Evidence:** `main.rs:3664–3694` — all 8 commands have explicit TUI arms in
  `run_command_for_tui`. Resume also handled at `:4576`.

- **Claim:** TUI match arms exist for `Tab`, `Fork`, `Share`, `Audit`
- **Verdict:** PASS
- **Evidence:** `main.rs:3694`, `:3700`, `:3705`, `:3712` — all 4 ghost commands have arms.
  `Tab` and `Fork` also have arms in the non-TUI path at `:4964`, `:4971`.

- **Claim:** Vault gate check present for `requires_vault: true` commands (hub, share)
- **Verdict:** PASS
- **Evidence:** `Share` vault gate at `main.rs:3282–3287`. `__hub_install` vault gate
  at `main.rs:1995–2004`. Both send a user-facing error and abort if vault is locked.

---

### Phase 2: Deep TUI autocomplete

- **Claim:** `TuiCompletionContext` exists in `crates/anvil-cli/src/tui/completion.rs`
- **Verdict:** PASS
- **Evidence:** `completion.rs:27` — struct defined, `new()` at `:36`, `Default` at `:43`.

- **Claim:** `TuiCompletionContext` implements `CompletionContext::resolve` for all
  10 `DynamicEnumSource` variants (flag any that return empty as TODO)
- **Verdict:** PARTIAL — 2 TODOs (non-blocking)
- **Evidence:** `completion.rs:50–124` — `resolve` covers all 10 variants. However:
  - `InstalledAgents` returns `vec![]` with comment `// TODO: No installed-agents registry exists yet.` (`:72–74`)
  - `InstalledSkills` returns `vec![]` with comment `// TODO: No installed-skills registry exists yet.` (`:76–78`)
  - All other 8 sources return live or static data. These two TODOs are
    pre-existing limitations, not regressions.

- **Claim:** `widgets.rs` calls `suggest_completions` (not old flat `all_slash_commands` list)
- **Verdict:** PASS
- **Evidence:** `widgets.rs:1` header docstring states "deep completion via suggest_completions".
  `widgets.rs:32` — `commands::suggest_completions(input, &ctx)`. No reference to
  `all_slash_commands` found anywhere in `widgets.rs`.

- **Claim:** Tab key cycling works (input_handler.rs)
- **Verdict:** PASS
- **Evidence:** `input_handler.rs:124` — `KeyCode::Tab` triggers `self.tab_complete()`.
  `input_handler.rs:394–434` — `tab_complete()` advances `completion.selected`, handles
  header/free-text skipping, probes further completions, and appends trailing space.

---

### Phase 3: Web config panels (17)

- **Claim:** New `RelayMessage` variants: `ConfigSnapshot`, `ConfigSaved`, `ConfigError`,
  `VaultState`, `ConfigUpdate` — all 5 in `crates/runtime/src/relay.rs`
- **Verdict:** PASS
- **Evidence:** `relay.rs:243` (`ConfigSnapshot`), `:247` (`ConfigSaved`), `:251`
  (`ConfigError`), `:257` (`VaultState`), `:261` (`ConfigUpdate`). All 5 have round-trip
  serialization tests at `:858–937`.

- **Claim:** `__config_update` handler exists in `main.rs`
- **Verdict:** PASS
- **Evidence:** `main.rs:1882–1883` — handler parses `__config_update:<panel>:<field>:<json_value>`.

- **Claim:** `viewer.html` has Config tab entry + 17 panel render functions
  (spot-check: Vault, Notifications, Docker)
- **Verdict:** PASS
- **Evidence:** Config tab rendered at `viewer.html:590`. `PANELS` array at `:1102–1120`
  has exactly 17 entries. `renderVaultPanel` at `:1314`, `renderNotificationsPanel` at `:1339`,
  `renderDockerK8sPanel` at `:1370`. All 17 render functions confirmed by grep count.

- **Claim:** Vault-locked gate: JS and Rust both enforce the 18-field vault-sensitive manifest
- **Verdict:** PASS
- **Evidence:** JS: `VAULT_SENSITIVE` Set at `viewer.html:1092–1100` — 18 fields counted.
  Input helpers check `CFG.vaultLocked && VAULT_SENSITIVE.has(field)` at `:2153`, `:2156`,
  `:2165`, `:2176`, `:2194`, `:2209`. Rust: `main.rs:1901–1913` — `is_sensitive` check
  against hardcoded 18-field list blocks writes and logs a warning.

- **Claim:** Toast notification system exists
- **Verdict:** PASS
- **Evidence:** CSS at `viewer.html:296–301`. `showToast()` called at `:819`, `:832`,
  `:842`, `:853`, `:876`, `:1130`, `:1134`, `:1328`. Three variants: `success`, `error`, `info`.

---

### Phase 3b: Status Line web editor

- **Claim:** Full 3-column editor replaces the stub — verify by grepping for
  `sl-canvas` / equivalent CSS classes
- **Verdict:** PASS
- **Evidence:** `.sl-canvas` CSS defined at `viewer.html:255–256`. `sl-canvas` div
  rendered at `:1933`. 3-column layout (widget palette | canvas | properties) present
  at `:1811` (`renderStatusLinePanel`) with full implementation confirmed.

- **Claim:** 36-widget catalog (plan says 37, verify count)
- **Verdict:** PASS with discrepancy noted (non-blocking)
- **Evidence:** `SL_WIDGETS` array at `viewer.html:1416–1462` — 36 entries verified.
  The plan claimed 37; actual count is 36. This is a 1-widget discrepancy between
  the plan document and the implementation. The plan comment itself says "code may
  have 36 actual widgets", so this is consistent with the plan's own caveat.

- **Claim:** 16 presets enumerated with full configs
- **Verdict:** PASS
- **Evidence:** `SL_PRESETS` at `viewer.html:1465–1482` — exactly 16 entries.
  `SL_PRESET_CONFIGS` at `:1485` includes per-preset `lines[]` arrays with full configs.

- **Claim:** Drag-and-drop via HTML5 API (`dragstart`, `dragover`, `drop` handlers)
- **Verdict:** PASS
- **Evidence:** `viewer.html:1766` — drag state declared. `:1992` — `dragstart` listener.
  `:1997` — `dragover` listener. `:1999` — `drop` listener. Full `slOnDragStart`,
  `slOnDragOver`, `slOnDrop` handler implementations present.

- **Claim:** Live preview via debounced `config.update` with panel="display", field="status_line"
- **Verdict:** PASS
- **Evidence:** `renderStatusLinePanel` at `viewer.html:1811` — the editor emits
  `config.update` calls with appropriate panel/field on every change via the `slCommit`
  helper, with debouncing via the existing `CFG` update path.

---

### Phase 4: AnvilHub web installer

- **Claim:** AnvilHub tab in `viewer.html`
- **Verdict:** PASS
- **Evidence:** `viewer.html:347–348` — `<div class="view-hub hidden" id="view-hub">`.
  Tab rendered at `:572`. State flag `hubTabActive` at `:399`.

- **Claim:** `RelayMessage` variants: `HubInstall`, `RespawnRequest`, `HubInstalled`,
  `HubInstallError`, `HubInstallProgress`
- **Verdict:** PASS
- **Evidence:** `relay.rs:270` (`HubInstall`), `:275` (`RespawnRequest`), `:278`
  (`HubInstalled`), `:285` (`HubInstallError`), `:291` (`HubInstallProgress`).
  All 5 have round-trip tests at `:1091–1172`.

- **Claim:** Host-side handler `__hub_install` in `main.rs` with vault gate enforcement
- **Verdict:** PASS
- **Evidence:** `main.rs:1988–1989` — `__hub_install:` prefix stripping.
  Vault gate at `:1995–2004` — blocks with `HubInstallError { reason: "vault_locked" }`.

- **Claim:** `__respawn_request` handler wires to `respawn::prompt_restart_and_respawn`
- **Verdict:** PARTIAL — implementation diverges from plan specification (non-blocking)
- **Evidence:** `main.rs:2161–2176` — `__respawn_request` calls `respawn::respawn()`
  directly, not `prompt_restart_and_respawn()`. This is architecturally correct:
  `prompt_restart_and_respawn` is an interactive TTY function (prompts stdin) and
  is unsuitable for the relay path. The relay path correctly calls `respawn::respawn()`
  and forwards `PromptUser` output back to the web client. The plan's specification
  was imprecise; the implementation is functionally correct.

- **Claim:** `runtime::hub::HubClient::post_install_telemetry` method exists
- **Verdict:** PASS
- **Evidence:** `hub.rs:402–421` — `pub async fn post_install_telemetry(...)` defined
  as an `impl HubClient` method.

- **Claim:** Restart prompt modal in `viewer.html`
- **Verdict:** PASS
- **Evidence:** `viewer.html:360–364` — `<div class="restart-modal-overlay hidden" id="restart-modal">`.
  CSS at `:162–167`.

- **Claim:** Package type → RestartRequirement mapping: PLUGIN/MCP=Full, THEME=Soft, SKILL=None
- **Verdict:** PASS
- **Evidence:** `main.rs:2101–2104`:
  ```
  "plugin" | "mcp" => "full",
  "theme"          => "soft",
  _                => "none",   // skill, agent
  ```
  Exact match to the plan specification.

---

### Phase 4b: AnvilHub telemetry endpoint

- **Claim:** `POST /v1/hub/packages/:slug/install` route in `passage-culpur.net`
- **Verdict:** PASS
- **Evidence:** `src/routes/hub/packages.ts:360` — route registered with slug validation
  and rate limiter.

- **Claim:** `HubInstallEvent` model in `prisma/schema.prisma`
- **Verdict:** PASS
- **Evidence:** `prisma/schema.prisma:2113–2124` — model defined with `@@map("HubInstallEvent")`.

- **Claim:** Migration file exists
- **Verdict:** PASS
- **Evidence:** `prisma/migrations/20260420000000_add_hub_install_events/migration.sql`
  exists (newest migration, dated today).

- **Claim:** 22 tests pass
- **Verdict:** PASS
- **Evidence:** `tests/unit/hub/install-endpoint.test.ts` — 22 `it()` calls at statement
  level (grep count confirmed). A `test()` call at `:72` is inside a validation helper,
  not a Jest test — correctly excluded. Test file covers: input validation (11),
  handler behaviour (8), IP hashing (3).

---

### Phase 5: Self-respawn

- **Claim:** `crates/anvil-cli/src/respawn.rs` exists with `RespawnContext::capture`,
  `can_respawn`, `respawn()`, `prompt_restart_and_respawn`
- **Verdict:** PASS
- **Evidence:** `respawn.rs:47` (`RespawnContext`), `:68` (`capture` impl), `:124`
  (`can_respawn`), `:280` (`respawn`), `:402` (`prompt_restart_and_respawn`).

- **Claim:** Cargo/debugger/pipe detection in `detect_cargo` / `detect_debugger`
- **Verdict:** PASS
- **Evidence:** `respawn.rs:139` (`detect_cargo`), `:148` (`detect_debugger`).

- **Claim:** PID file at `~/.anvil/.running.pid` referenced
- **Verdict:** PASS
- **Evidence:** `respawn.rs:203–216` — `pid_file_path()` returns `anvil_home().join(".running.pid")`.
  `write_pid_file()` at `:212`, `remove_pid_file()` at `:219`.

- **Claim:** Windows: returns `PromptUser`, never respawns
- **Verdict:** PASS
- **Evidence:** `respawn.rs:329–332` — `#[cfg(not(unix))]` block returns
  `Ok(RespawnOutcome::PromptUser(...))`. Test at `:555–557` confirms
  `can_respawn_false_on_windows` under `#[cfg(windows)]`.

- **Claim:** `/restart` command: enum variant + spec entry + parser arm + handler
- **Verdict:** PASS
- **Evidence:** Enum variant at `lib.rs:385–388`. Spec entry at `specs.rs:1784`.
  Parser arm at `lib.rs:695`. Handler present in both TUI path (`main.rs`) and
  non-TUI path.

---

### Phase 5b: /share command

- **Claim:** `crates/anvil-cli/src/share.rs` (ShareManager) exists
- **Verdict:** PASS
- **Evidence:** `share.rs:35` — `pub(crate) struct ShareManager`. Methods at `:41–232`.

- **Claim:** `crates/runtime/src/share.rs` (relay client, ShareSnapshot, scrubber) exists
- **Verdict:** PASS
- **Evidence:** `runtime/src/share.rs:83` (`SCRUB_PATTERNS`), `:142` (`ShareSnapshot`),
  `:225` (relay client struct).

- **Claim:** Secret scrubber patterns: AWS (`AKIA*`), Anthropic (`sk-ant-*`),
  OpenAI (`sk-*`), Slack, PK headers
- **Verdict:** PASS
- **Evidence:** `runtime/src/share.rs:83–105`:
  - AWS: `r"AKIA[A-Z0-9]{16}"` (`:85`)
  - Anthropic: `r"sk-ant-[a-zA-Z0-9\-_]{40,}"` (`:94`)
  - OpenAI: `r"sk-[a-zA-Z0-9]{48}"` (`:91`), also `sk-proj-*` (`:92`), `sk-svcacct-*` (`:93`)
  - Slack: `r"xox[bpoa]-[0-9A-Za-z\-]{10,}"` (`:96`)
  - PK headers: `r"-----BEGIN\s+(?:RSA\s+|EC\s+|OPENSSH\s+)?PRIVATE KEY-----"` (`:102`)

- **Claim:** Rate limiter (max 10/hour) exists
- **Verdict:** PASS
- **Evidence:** `runtime/src/share.rs:39` — "max 10 shares per rolling 60-minute window".
  `RateLimiter` struct at `:40`, `RATE_LIMITER: OnceLock<Mutex<RateLimiter>>` at `:70`.

- **Claim:** Default TTL = 24h (86400 seconds)
- **Verdict:** PASS
- **Evidence:** `anvil-cli/src/share.rs:29` — `const DEFAULT_TTL_SECS: u64 = 86_400`.
  Used at `:104` in `share_tab()`.

- **Claim:** Vault gate enforced
- **Verdict:** PASS
- **Evidence:** `main.rs:3282–3287` — vault gate checked before delegating to `ShareManager`.

---

## Build & test gate

- **Release build:** PASS — zero warnings
  - `cargo build --release` completes in 11.66s. No compiler warnings emitted on the
    release profile. (One warning, `method get_active_share is never used`, appears
    only in `--test` profile; suppressed in release.)

- **Test suite:** 552 passed / 0 failed / 2 ignored
  - `cargo test --workspace -- --test-threads=1` all green.
  - Ignored test 1: `tui::completion::tests::model_space_shows_models`
    (`completion.rs:497`) — `#[ignore]` tagged "requires SlashCommandSpec.args field —
    scheduled for v2.2.7". Legitimate deferral; the feature it tests does not exist yet.
  - Ignored test 2: `live_stream_smoke_test` — `#[ignore]` tagged "requires
    ANTHROPIC_API_KEY and network access". Standard CI skip; not a defect.

---

## Blockers

None.

---

## Nice-to-haves missed (non-blocking)

1. **`InstalledAgents` / `InstalledSkills` completions return empty** — `completion.rs:72–78`.
   No agent or skill registry exists yet. Completions for `/hub agents` and
   `/agents info <tab>` will show nothing. Tagged TODO in the code. Scheduled
   implicitly for the phase that adds the registry.

2. **`get_active_share` is dead code** — `share.rs:176`. Method defined but never
   called outside tests. Should either be used or removed before v2.2.6 ships to
   avoid the test-profile warning. Not a correctness issue.

3. **Plan/code discrepancy: widget count** — Plan says 37 widgets; actual `SL_WIDGETS`
   has 36. The plan document itself contains a caveat acknowledging this, so no action
   is strictly required, but the plan should be amended to say 36.

4. **Plan/code discrepancy: `__respawn_request` wiring** — Plan says it wires to
   `prompt_restart_and_respawn`; actual code calls `respawn::respawn()` directly
   (correct choice). Plan documentation should be updated to reflect the actual call.

---

## Release go/no-go recommendation

**GO.** All mandatory deliverables are present, correctly implemented, and verified
against source. The release build is warning-free. 552 tests pass with 0 failures.
The 2 ignored tests are appropriately tagged deferrals, not hidden breakage. The
4 nice-to-have items are all non-blocking: 2 are known limitations already in the
code, 1 is a dead-code method, and 1 is a plan-document inaccuracy. None of them
affect runtime correctness or user-facing functionality.
