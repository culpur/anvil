# MEMORY LAYER 1 — Working

Audit target: Anvil v2.2.14 (HEAD of `/home/user/repo`).
Synthesis parent: `docs/research/SEVEN-LAYER-MEMORY.md`.

---

## 1. Layer definition

L1 *Working* is everything the runtime hands the model for **this turn and this turn only**: the assembled `system_prompt: Vec<String>` plus the `messages: Vec<ConversationMessage>` buffer that ride together in every `ApiRequest` (see `crates/runtime/src/conversation/turn_executor.rs:55-58`). It is transient — recomputed on each iteration of the agentic loop — and is the **only** layer the model literally sees. All other memory layers (L2..L7) exist to feed *into* L1 each turn (some automatically, some lazily, some never). Working memory has no canonical on-disk form of its own; it is reconstructed from the session journal (`messages`) and the live prompt builder output. It owns two budgets: (a) the per-turn assembly budget (instruction-file truncation at `MAX_TOTAL_INSTRUCTION_CHARS = 12_000` chars, `prompt.rs:52`), and (b) the conversation-buffer budget enforced by `CompactionConfig` (`compact.rs:10-22`).

---

## 2. Current Anvil state

L1 lives in two cooperating concerns: **system-prompt assembly** (rebuilt per CLI startup / per skill-load / per hot-reload) and **per-turn message buffer** (mutated in place by `ConversationRuntime`).

| Concern | File:Line | Role |
|---|---|---|
| `SystemPromptBuilder` | `crates/runtime/src/prompt.rs:101-273` | Owns the per-turn system-prompt vector; builds 7–13 sections per call |
| `SystemPromptBuilder::build` | `crates/runtime/src/prompt.rs:242-273` | Concrete section order: intro → output style → system → tasks → actions → DYNAMIC_BOUNDARY → env → project → instruction-files → memory → qmd → config → append_sections |
| `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` marker | `crates/runtime/src/prompt.rs:43` | Sentinel placeholder for everything below the line being "dynamic"; never consumed by a splitter today |
| `with_known_files_from_cache` | `crates/runtime/src/prompt.rs:214-239` | Pulls the W11 `<known-files>` block from `~/.anvil/file-cache/` and appends it; sole automatic L7→L1 bridge |
| `with_qmd_context` / `with_qmd_results` | `crates/runtime/src/prompt.rs:196-209` | Public but **unused by the CLI** — see §3 |
| `MemoryManager::render_for_prompt` (called at `prompt.rs:258`) | (referenced; declared in `crates/runtime/src/memory.rs`) | Renders the ANVIL.md / `~/.anvil/memory/*.md` tier into a single section — sole automatic L3→L1 bridge |
| `load_system_prompt_with_identity` | `crates/runtime/src/prompt.rs:560-587` | Entry point used by CLI; wires `ProjectContext::discover_with_git`, `ConfigLoader`, and `with_known_files_from_cache` |
| `build_system_prompt_with_identity` (CLI) | `crates/anvil-cli/src/utils.rs:1455-1479` | Wraps the runtime call; also `insert(0, …)` an active-goal fragment (L4→L1) when `GoalManager::active_goal()` returns Some |
| Skill-load injection | `crates/anvil-cli/src/main.rs:5460-5466`, `5667-5672`, `6750-6754` | `/skill load <name>` does `self.system_prompt.insert(0, body)` — overwrites whatever was at index 0 (including a goal fragment) |
| Output-style → "fast" prefix | `crates/anvil-cli/src/main.rs:5783-5790` | `insert(0, "Be concise and direct.")` |
| Hot-reload replacement | `crates/runtime/src/conversation/mod.rs:312-314` (`replace_system_prompt`) | Used by `crates/anvil-cli/src/main.rs:2842`, `3384`, `4458` after ANVIL.md edits |
| `ConversationRuntime::system_prompt` field | `crates/runtime/src/conversation/mod.rs:133` | The live cached vector; cloned into every `ApiRequest` |
| Per-turn request build | `crates/runtime/src/conversation/turn_executor.rs:55-58` | `system_prompt: system_prompt.to_vec(), messages: session.messages.clone()` |
| `CompactionConfig` | `crates/runtime/src/compact.rs:10-22` | `preserve_recent_messages: 6`, `max_estimated_tokens: 10_000` (default); only knob bounding the buffer |
| `should_compact` | `crates/runtime/src/compact.rs:38-48` | Trigger predicate: count AND token estimate both crossed |
| `compact_session` | `crates/runtime/src/compact.rs:90-…` | Replaces the older prefix with a `System`-role summary message; preserves the last N messages verbatim |
| Auto-compact threshold | `crates/runtime/src/history.rs:107-116` (`compact_threshold_pct`, default 85%, `ANVIL_COMPACT_THRESHOLD` env) | Per-model trigger fired from `crates/anvil-cli/src/main.rs:8160-8182` (`maybe_auto_compact`) |
| Per-turn user-side reminder (QMD) | `crates/anvil-cli/src/main.rs:4619-4643` (`build_input_with_qmd_context`) | Wraps user input in a `<system-reminder>` block with rendered QMD + history context — this is *also* L1 working state but lives on the user message, not the system prompt |
| `/memory why` handler | `crates/commands/src/handlers.rs:899-914` | Static, hand-written explanation of injection order — **does not introspect the live `system_prompt`** |
| `/memory budget` handler | `crates/commands/src/handlers.rs:916-952` | Iterates on-disk tier directories — does **not** measure the in-flight system_prompt or message buffer |
| `MAX_INSTRUCTION_FILE_CHARS` / `MAX_TOTAL_INSTRUCTION_CHARS` | `crates/runtime/src/prompt.rs:51-52` (`4_000` / `12_000`) | Only working-memory byte budget enforced today |

**Data layout / retention:** L1 has no canonical persistence. The closest analogue is the session file (`Session::save_to_path`, `crates/runtime/src/session.rs:99`) which is JSON and stores the `messages` array only — the `system_prompt` vector is **not** serialised and is rebuilt fresh on resume (`anvil-cli/src/main.rs:1184-1198` `resume_session`). Retention is per-process; on `/quit` the prompt is dropped and on `/resume <path>` it is rebuilt from `ProjectContext::discover_with_git` + on-disk ANVIL.md + file-cache.

---

## 3. What's missing or miscategorized

| Gap | Where | Notes |
|---|---|---|
| `/memory why` is hardcoded, not introspective | `crates/commands/src/handlers.rs:899-914` | Lists 6 items but the live builder produces 7–13 sections (`prompt.rs:242-273`); items 5 (file-cache known-files) and 6 (daily task reconciliation) are aspirational |
| Daily reconciliation fragment is **never** injected | `crates/anvil-cli/src/main.rs:7573-7582` only `eprintln!`s open items at session end | `/memory why` line 908 claims this enters the prompt; grep confirms no injection site |
| QMD into system prompt is **never** called | `prompt.rs:196-209` defines `with_qmd_context` / `with_qmd_results`; no caller exists. QMD is only attached to the *user message* (`main.rs:4619-4643`) | This is a categorisation slip: QMD is currently a per-turn user-message wrapper (still L1 working) rather than a system-prompt section |
| Active-goal fragment, skill body, "fast" prefix all `insert(0, …)` | `utils.rs:1474-1475`, `main.rs:5466 / 5672 / 5790 / 6754` | They stomp each other and put L4 content *before* the assistant identity/intro, contradicting `/memory why`'s claimed order |
| No working-memory token budget | `compact.rs:18-19` defaults | `preserve_recent_messages: 6` (number, not tokens) and `max_estimated_tokens: 10_000` are *triggers* for compaction, not a budget on the system_prompt blocks |
| Permission-memory (L6) leak | `crates/runtime/src/permission_memory.rs` — none of it reaches the system prompt | Arguably belongs in L1 as a "what the user has already allowed" reminder; currently invisible to the model |
| Recent-message buffer length is hardcoded | `compact.rs:18` (`preserve_recent_messages: 6`) | No env var, no config key, no per-model tuning. Resume path uses `CompactionConfig { max_estimated_tokens: 0, ..default() }` (`main.rs:1585-1588`) to force compaction without honouring the live tuning |
| `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` marker is dead | `prompt.rs:43, 251` | Inserted but no consumer splits on it; intent was likely cache-stable head vs. dynamic tail (matters for prompt-caching providers) |
| `/memory budget` excludes working memory | `handlers.rs:920-927` | Tiers listed: anvil-md, nominations, daily, goals, file-cache, cmd-cache — nothing for the live system_prompt or message buffer |
| `agent_ctx.rs` / `agent_snapshot.rs` | not L1 | These are tracing / subagent process registry, **not** working memory. Mentioned in the brief but unrelated. |

---

## 4. Inspector surface

What `/memory show working`, `/memory inspect`, `/memory budget` *should* return for L1, tied to current handler code:

| Subcommand | Today | What L1 should expose |
|---|---|---|
| `/memory show working` | tier name doesn't exist; `memory_show` (`handlers.rs:716-776`) returns "Unknown tier" | Dump the live `ConversationRuntime::system_prompt` vector, one section per heading, plus message-buffer length / token estimate |
| `/memory inspect <key>` | only scans anvil-md + nominations (`handlers.rs:778-831`) | Grep the assembled system-prompt sections AND the message buffer for `<key>`, surface origin (which builder method appended it) |
| `/memory why` | static text (`handlers.rs:899-914`) | Walk the live `system_prompt: Vec<String>`, label each entry by its source method (`get_simple_intro_section`, `environment_section`, `MemoryManager::render_for_prompt`, etc.), and call out the `insert(0, …)` stack from CLI overrides |
| `/memory budget` | on-disk byte totals only (`handlers.rs:916-952`) | Add a "working" row: `system_prompt` rendered length, `messages` count and estimated tokens via `estimate_session_tokens` (`compact.rs:33-35`), distance to `compact_threshold_pct * max_tokens_for_model` |

Touch points to extend: extend `MEMORY_SUBCOMMANDS` (referenced at `specs.rs:280` → `crates/commands/src/subcommands.rs`), add a `working` arm in `memory_show` and `memory_budget`. The handlers currently have no access to a live `ConversationRuntime`; they would need a new accessor or a snapshot passed in.

---

## 5. Migration moves

1. **Introduce `WorkingMemorySnapshot`** in a new file `crates/runtime/src/working.rs`. Struct: `{ sections: Vec<(SectionKind, String)>, message_count: usize, estimated_tokens: usize, recent_preserved: usize, compaction_distance_pct: u8 }`. Build it from `&ConversationRuntime` (read `system_prompt`, run `estimate_session_tokens(session)`, look up `compact_threshold_pct`). Effort: ~4h.
2. **Label sections at build time.** Replace `Vec<String>` in `SystemPromptBuilder` with `Vec<PromptSection { kind, body }>` (`crates/runtime/src/prompt.rs:102, 116, 242-273`). `SectionKind` enum: `Intro`, `OutputStyle`, `System`, `Tasks`, `Actions`, `Boundary`, `Environment`, `ProjectContext`, `InstructionFiles`, `MemoryAnvilMd`, `Qmd`, `Config`, `Append`, `Goal`, `Skill`, `FastPrefix`. Provide a `.render() -> Vec<String>` to keep the public surface stable. Effort: ~1d.
3. **Stop using `insert(0, …)`.** Replace the four CLI sites (`anvil-cli/src/main.rs:5466, 5672, 5790, 6754` and `anvil-cli/src/utils.rs:1474-1475`) with new builder methods `with_active_goal(fragment)`, `with_loaded_skill(body)`, `with_fast_prefix()`. They become real sections with a real ordering rule, defined inside `SystemPromptBuilder::build`. Effort: ~3h.
4. **Wire `/memory show working` and `/memory why`** to introspect a live snapshot. Add a parameter to `handle_slash_command` (or thread a `&ConversationRuntime` snapshot) so handlers in `crates/commands/src/handlers.rs:899-914` and `:716-776` can iterate `snapshot.sections` instead of printing hardcoded text. Effort: ~4h, including specs.rs tier-list update at `crates/commands/src/specs.rs:251-260`.
5. **Add a working-row to `/memory budget`** (`handlers.rs:916-952`): print `system_prompt` bytes + `~tokens` and `messages` bytes + `~tokens` + distance to auto-compact. Effort: ~2h.
6. **Move QMD off the user message and into the system prompt** by actually calling `SystemPromptBuilder::with_qmd_results` from the CLI (replacing `build_input_with_qmd_context` at `main.rs:4619-4643`). Or, if keeping per-turn user reminders, *re-categorise* QMD as L7 cache → L1 working-via-user-message and document it. Effort: ~1d depending on which way it lands.
7. **Wire the daily-reconciliation fragment**, which `/memory why` already advertises. Add `with_daily_reconciliation(open_items: &[String])` to `SystemPromptBuilder`, populate from `runtime::DailyStore::reconcile` at session start in `anvil-cli/src/main.rs:7575`. Effort: ~3h.
8. **Surface `preserve_recent_messages` and `max_estimated_tokens`** as config keys in `runtime/assets/config-schema.json` and `RuntimeConfig`. Today they are only set by callers; `CompactionConfig::default()` is the de facto policy. Effort: ~3h.
9. **Consume the `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` marker** in the provider layer: split into cache-stable head (everything before the boundary) and dynamic tail. Required for Anthropic prompt caching and the L7 cache mapping to land cleanly. Effort: ~1d (touches `crates/api/src/providers/`).
10. **Persist working-memory hashes** (not the prompt itself) at session save so resume can tell the user "your prompt has drifted by N sections since you last ran". Effort: ~4h.

Total scope: ~5 engineer-days for moves 1–7; moves 8–10 are stretch.

---

## 6. Risks and reversibility

| Risk | Blast radius | Rollback |
|---|---|---|
| Re-typing `system_prompt: Vec<String>` to `Vec<PromptSection>` breaks every caller of `replace_system_prompt` (`conversation/mod.rs:312`) and the test fixtures in `conversation/mod.rs:463-472, 478, 538, 583, 654, 729, 756` | Compile-time only; nothing runtime | Keep `Vec<String>` shim via `From<Vec<PromptSection>>` and roll back with a single revert |
| Re-ordering sections changes model behaviour (the goal fragment moves from index 0 to a labelled slot) | Behavioural drift, hard to detect | Land behind a feature flag `ANVIL_PROMPT_ORDER_V2` and keep both orderings selectable for one minor version |
| Wiring `with_qmd_results` into the system prompt changes prompt-cache hit rate (QMD varies per user message) | Token cost regression | Keep the user-message variant as the default; gate system-prompt injection behind config |
| Adding daily reconciliation could leak yesterday's task list into a fresh project | Privacy / signal quality | Scope to `cwd`-bound DailyStore filter; default off |
| Touching `CompactionConfig` defaults changes when sessions auto-summarise | Could surprise long-running sessions | Read from config with current defaults; existing behaviour preserved unless user opts in |
| Moving `insert(0, …)` callers to builder methods misses one (4 sites; easy to miss) | Goal fragment vanishes or duplicates | Add a `#[deprecated]` shim on `insert(0, …)` patterns; CI grep for direct `self.system_prompt.insert` |
| `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` consumer touches provider code | Breaks streaming if mis-split | Land split as no-op (boundary stripped, no caching change) first; add caching second |

Reversibility is high for moves 1, 3, 4, 5, 7, 10 (pure code reorg). Moves 2, 6, 8, 9 ship behaviour change and want a flag.

---

## 7. Cross-layer dependencies

| Other layer | How L1 depends on it today | What changes after migration |
|---|---|---|
| **L2 Episodic** (history / daily) | `HistoryArchiver` is invoked *before* compaction (`main.rs:8123-8128`); daily summaries are written on quit but never re-injected | After move 7, daily-reconciliation fragment becomes a labelled L1 section sourced from L2 |
| **L3 Semantic** (anvil-md) | `MemoryManager::render_for_prompt` called inline at `prompt.rs:258` — fully automatic L3→L1 bridge today | No change; just labelled as `SectionKind::MemoryAnvilMd` |
| **L4 Procedural** (goals / skills) | Goal fragment `insert(0, …)` at `utils.rs:1475`; skill body `insert(0, …)` at `main.rs:5466, 5672, 6754`; "terse" skill at `5667` | Move 3 turns these into real sections; section order becomes documented |
| **L5 Identity** (vault / private) | **Never injected** — `/memory why` line 910 explicitly says so | No change; remains an opt-in `/vault read`-style flow |
| **L6 Policy** (permission_memory) | Not injected today (gap noted in §3) | Could be added as a `SectionKind::PolicyReminder` post-migration; not in scope of moves 1–10 |
| **L7 Cache** (file-cache / cmd-cache / QMD) | `with_known_files_from_cache` at `prompt.rs:214-239` is the only automatic L7→L1 bridge; QMD goes via user message (`main.rs:4619-4643`); cmd-cache never reaches L1 | Moves 6 and 9 unify these: QMD as a real L1 section, dynamic-boundary makes file-cache cache-stable separately |