# Memory Layer 4 — Procedural

Audit target: Anvil v2.2.14 (HEAD of `/home/user/repo`).
Sibling docs: `MEMORY-LAYER-{1..7}-*.md`. Synthesis: `SEVEN-LAYER-MEMORY.md`.

---

## 1. Layer definition

**L4 Procedural** is *how-to* memory — programs that **act** on the world rather than facts that **describe** it. An entry is L4 when its purpose is to drive behavior: a skill body that prepends to the system prompt to bias generation; a slash command spec that names a workflow the user can invoke; a routine (cron/event-triggered prompt) that fires unattended; a long-running goal that survives session boundaries and steers what the agent works on. Contrast with **L3 Semantic** (ANVIL.md, nominations) which holds *facts the agent reads about the world* — "OLLAMA_HOST is on port 11434", "deploys use SSH key X". L3 declares; L4 executes. A skill body like `silent-cat/SKILL.md` is L4 even though it is a markdown file, because it changes what the agent *does* on the next turn; a sentence in ANVIL.md saying "we use Postgres" is L3 because it changes what the agent *knows*. Goals are L4 because they encode an *intent to act*, not a fact; they have status (`Active|Paused|Done|Archived`) and milestones, not assertions.

---

## 2. Current Anvil state

L4 is **the most fragmented layer in Anvil today.** Three sub-stores exist, none of them addressed uniformly:

### 2a. Goals — wired into `/memory`

| Concern | Citation | Notes |
|---|---|---|
| Type definitions | `crates/runtime/src/goals.rs:32-75` | `GoalStatus`, `Goal` struct |
| Manager | `crates/runtime/src/goals.rs:140-167` | `GoalManager::new()` derives goals_dir from project hash |
| On-disk path | `crates/runtime/src/goals.rs:3-6,165` | `~/.anvil/goals/<project-path-hash>/<goal-id>.json` |
| Atomic writes | `crates/runtime/src/goals.rs:194-203` | `.tmp` + rename |
| ID format | `crates/runtime/src/goals.rs:57,509-514` | `g-<unix_secs>-<6char_hex>` |
| Description cap | `crates/runtime/src/goals.rs:21` | 4096 chars |
| Prompt fragment | `crates/runtime/src/goals.rs:494-497` | `<active-goal id="…">…</active-goal>`, truncated to 200 chars (const `GOAL_PROMPT_INJECT_TRUNCATE`, `goals.rs:27`) |
| List formatter | `crates/runtime/src/goals.rs:444` | `format_goal_list` |
| Re-export | `crates/runtime/src/lib.rs:135` | (not the line; symbol re-exported from `goals`) |
| `/goal` spec | `crates/commands/src/specs.rs:1918-1939` | new/list/resume/pause/done/show, `Workspace` category, **web_available: false** |
| `/memory show goals` | `crates/commands/src/handlers.rs:755-762` | dispatches to `GoalManager::list()` |
| `/memory` summary | `crates/commands/src/handlers.rs:694-695` | counts files in `~/.anvil/goals` |
| `/memory budget` row | `crates/commands/src/handlers.rs:924` | "goals" tier in budget table |
| `/memory why` line 3 | `crates/commands/src/handlers.rs:906` | injection ordering documents active-goal fragment |

### 2b. Bundled skills — invisible to `/memory`

10 SKILL.md bodies are embedded at compile time via `include_str!`:

| Skill name | Citation |
|---|---|
| `security-audit` | `crates/commands/src/agents.rs:725-728` |
| `code-review` | `crates/commands/src/agents.rs:729-732` |
| `terse` | `crates/commands/src/agents.rs:733-736` |
| `token-economy` | `crates/commands/src/agents.rs:738-741` |
| `file-fingerprint` | `crates/commands/src/agents.rs:742-745` |
| `command-cache-aware` | `crates/commands/src/agents.rs:746-749` |
| `pattern-promote` | `crates/commands/src/agents.rs:750-753` |
| `cache-budget` | `crates/commands/src/agents.rs:754-757` |
| `anvil-md-curator` | `crates/commands/src/agents.rs:758-761` |
| `silent-cat` | `crates/commands/src/agents.rs:762-765` |

Plus a second registry of name+description for the host's skill-summary view (used by `/skills`): `crates/commands/src/agents.rs:328-355` — `commit`, `review-pr`, `simplify`, `loop`, `schedule`, `claude-api` (6 entries, descriptions only — no embedded body).

- Discovery roots (on-disk skills): `crates/commands/src/agents.rs:178-253` — walks ancestors of `cwd` and `$HOME` for `.codex/skills`, `.anvil/skills`, `.codex/commands`, `.anvil/commands`.
- Loader: `crates/commands/src/agents.rs:775-799` (`load_skill_body`) — bundled first, then disk.
- Parser: `crates/commands/src/agents.rs:357-439` (`load_skills_from_roots`) — frontmatter, `chains_to`, `triggers`, `body_bytes`, `shadowed_by`.
- Chain evaluator: `crates/commands/src/skill_chaining.rs:32` documents depth ≤ 3, `skill_chaining.rs:121-130` (`max_depth=3`).
- `/skill` spec: `crates/commands/src/specs.rs:1858-1879` (`Core` category) — `list/load/suggest/chains`.
- `/skills` spec: `crates/commands/src/specs.rs:543-554` (`Automation` category) — list-only.

**Crucially, `/memory show` does not have a `skills` tier**: `crates/commands/src/handlers.rs:716-776` enumerates `anvil-md, vault, private, nominations, daily, file-cache, cmd-cache, goals` and nothing else. The skills body bytes are loaded into the system prompt (per `/memory why` line 4, `handlers.rs:906`) but `/memory` cannot report them.

### 2c. Cron / routines — designed, partly built, completely absent from `/memory`

| Concern | Citation | Notes |
|---|---|---|
| Cron entry type | `crates/runtime/src/cron.rs:15-26` | `CronEntry { id, name, cron_expression, prompt, enabled, last_run, next_run, target_url, created_at }` |
| Cron store path | `crates/runtime/src/cron.rs:51` (doc), `runtime::cron::default_store_path` | `~/.anvil/cron.json` — single JSON, not a directory |
| Cron daemon | `crates/runtime/src/lib.rs:135` re-exports `CronDaemon`, `CronManager`, `CronEntry`, `next_run_time` |
| Routines module | `crates/runtime/src/lib.rs:22` | `pub mod routines;` |
| Routines entry point | `crates/runtime/src/routines/mod.rs:1-26` | exports `archive`, `packet`, `schedule`; `SILENT_MARKER = "[SILENT]"` |
| Schedule grammar | `crates/runtime/src/routines/schedule.rs:1-32` | `Cron`, `Interval`, `OnceAfter`, `OnceAt` |
| Archive layout | `crates/runtime/src/routines/archive.rs:1-9` | `~/.anvil/routines/output/{routine_id}/{YYYYMMDDTHHMMSSZ}.md` |
| Packet schema | `crates/runtime/src/routines/packet.rs:1-50` | `{summary, body, input_hash}`, SHA-256 hash, `PACKET_OPEN`/`PACKET_CLOSE` delimiters |

There is **no `routines` directory inside `~/.anvil/` populated by anything user-facing**: the TOML loader (Tier 2 item 5), dispatcher, delivery layer, `anvil routine` CLI, and reconciliation module are all still in the ROADMAP backlog (`ROADMAP.md:65-91`). What *exists* is the foundation: schedule parsing, archive writer, packet schema, silent marker. `automation_ops.rs:2,126,143,151` uses `CronManager::global()` — the cron precursor is reachable, but no slash command in `specs.rs` exposes it (no `/cron`, no `/routine`, no `/schedule` slash command spec, verified by grep).

### 2d. Slash command catalog — the procedural surface itself

The slash command spec list at `crates/commands/src/specs.rs:50…` enumerates **111 commands** across 5 categories (`SlashCommandCategory` at `specs.rs:4-10`):

| Category | Count | Sample commands relevant to L4 |
|---|---|---|
| Core | 28 | `/skill`, `/output-style`, `/model`, `/help` |
| Workspace | 26 | `/memory`, `/init`, `/goal`, `/daily` |
| Session | 10 | `/resume`, `/session`, `/export` |
| Git | 10 | `/commit`, `/pr`, `/branch` |
| Automation | 37 | `/skills`, `/loop`, `/bughunter`, `/hub` |

The catalog itself is L4 content (each entry is a named procedure with an argument-hint, optional subcommands, and permission gates such as `requires_vault`). Today `/memory` does not enumerate it. The closest surface is `/help` (handled outside the memory inspector).

---

## 3. What's missing or miscategorized

| Gap | Why it matters | Where to land |
|---|---|---|
| **Bundled skills not in `/memory`** | 10 compile-time skill bodies inject into the system prompt (per `handlers.rs:906`) but no tier reports them. The user cannot answer "what procedures might fire?" from `/memory show`. | Add `procedural` (or `skills`) tier to `memory_show` and `memory_summary` |
| **On-disk skills not in `/memory`** | Same as above but worse: `.codex/skills`, `.anvil/skills`, and the legacy `.codex/commands` / `.anvil/commands` directories (`agents.rs:181-205`) are discovered for `/skills` but invisible to `/memory budget` and `/memory inspect` | Same handler addition; reuse `discover_skill_roots` |
| **Slash command catalog not in `/memory`** | The 111-entry `specs.rs` table is the durable surface of "things the user can ask the agent to do"; today it is only reachable via `/help` | Optional subcategory: `procedural commands` |
| **Goals are in `/memory` but flagged `web_available: false`** | `specs.rs:1939` excludes `/goal` from web sessions. The procedural layer is half-blind from the web. | Either flip the flag (requires audit of TUI-only state writes) or surface `goals` read-only via `/memory show goals` on web |
| **Cron / routines exist but no slash command exposes them** | `cron.rs` and `routines/*` ship in `runtime` (`lib.rs:22,135`); there is no `/cron`, `/routine`, or `/schedule` spec in `specs.rs` (verified by grep). Anvil can store a cron entry but a user has no in-band way to see one. | Stub `/memory show procedural --routines` to read `~/.anvil/cron.json` and the `~/.anvil/routines/output/` archive; populate when ROADMAP Tier 2 items 5/10/20 land |
| **Skill bodies counted as bytes nowhere** | `/memory budget` (`handlers.rs:916-951`) walks `anvil-md, nominations, daily, goals, file-cache, cmd-cache` — bundled skill bytes (compile-time) and disk-skill bytes (variable) are not tallied | Add a `procedural` row that sums `body_bytes` from `load_skills_from_roots` + `BUNDLED_SKILL_BODIES` lengths |
| **`/memory why` lists "skill body" but does not say *which* skill** | `handlers.rs:906` says "Skill body (if a skill was loaded via /skill load)" but provides no introspection of the *currently loaded* skill state | Surface the loaded skill name(s) via `memory_why` once skill-load state is reachable from the handler |
| **No reconciliation surface for routines** | ROUTINES-ADOPTION-NOTES §10.5 specifies pre-dispatch drift detectors; today nothing reports drift because nothing dispatches | Reserve a `procedural reconcile` sub-view; populate when reconcile module lands |

---

## 4. Inspector surface

What `/memory show procedural` (proposed) **should** return when L4 lands as a single unified tier. Tie-points to existing handler code:

| Inspector call | Behavior | Wires to |
|---|---|---|
| `/memory` (no args) | Add one summary row: `procedural   N goal(s), K skill(s), R routine(s)` | extend `memory_summary` at `handlers.rs:685-714` between current `goals` (line 694) and `vault` (696) — or replace the `goals` row |
| `/memory show procedural` | Three subsections: **Active goal** (truncated description), **Skills** (one row per skill: source — bundled / project / user, body_bytes, chains_to, shadowed_by), **Routines** (one row per cron+routine entry: name, schedule, enabled, last_run) | new `"procedural"` branch in `memory_show` at `handlers.rs:731-775`; calls `GoalManager::list()`, `agents::load_skills_from_roots(agents::discover_skill_roots(cwd))`, `CronManager::global().lock().entries()` |
| `/memory show goals` | Keep working unchanged (back-compat); new alias `/memory show procedural --goals` | already implemented at `handlers.rs:755-762` |
| `/memory show skills` | New: list skills (bundled + on-disk) | reuse `load_skills_from_roots` + `BUNDLED_SKILL_BODIES` (`agents.rs:724-766`) |
| `/memory show routines` | New stub: lists cron entries from `~/.anvil/cron.json`; pre-populates "no routines configured" until Tier 2 ships TOML loader | reads `CronManager::global()` (`cron.rs:51-59`) and walks `~/.anvil/routines/output/` if it exists |
| `/memory inspect <key>` | Extend search to include skill names, skill triggers, goal descriptions, cron entry names/prompts | add new loops to `memory_inspect` at `handlers.rs:778-831` after the nominations loop at lines 806-818 |
| `/memory budget` | Add row `procedural` that sums skill body bytes (bundled + on-disk) + goal file bytes + cron.json bytes + routines/output bytes | extend `tiers` array at `handlers.rs:920-927` (currently has 6 rows: anvil-md, nominations, daily, goals, file-cache, cmd-cache) |
| `/memory why` | Replace lines 3 and 4 of `memory_why` (`handlers.rs:906-907`) with an "L4 procedural" section that lists *which* skill is loaded and *which* goal is active by ID | edit string literal at `handlers.rs:899-913` |
| `/memory prune` | Add: prune `Done`+`Archived` goals older than N days, prune `~/.anvil/routines/output/*` older than N days | extend `memory_prune` at `handlers.rs:954-966` |
| Spec doc-comment | Update `/memory` spec at `specs.rs:248-278` to document a `procedural` tier, list it in TIERS at `specs.rs:269` | edit `specs.rs:261-270` |

---

## 5. Migration moves

Concrete, numbered, citation-anchored. Estimates are wall-time for one engineer.

1. **Define the `procedural` tier name and rename `goals` to a sub-view** — edit `crates/commands/src/specs.rs:269` (TIERS doc-comment) to replace `goals` with `procedural`, retain `goals` as a documented alias. Update `crates/commands/src/handlers.rs:723` and `:773` usage strings. (1 hour)

2. **Add a `procedural` branch in `memory_show`** — new arm in the `match tier` at `crates/commands/src/handlers.rs:731-775`. Composition: call `GoalManager::list()` (already at line 757), call `agents::load_skills_from_roots(agents::discover_skill_roots(cwd))` (existing at `agents.rs:357,178`), enumerate `BUNDLED_SKILL_BODIES` (`agents.rs:724`), enumerate `CronManager::global().entries()` (re-exported via `lib.rs:135`). Format as three subsections separated by `---`. (3-4 hours)

3. **Add per-name aliases** — accept `"goals"`, `"skills"`, `"routines"` as filtered views of `procedural`. Each dispatches to the relevant slice of step 2. Edit `memory_show` arms; keep `goals` arm at `handlers.rs:755-762` unchanged for back-compat. (1 hour)

4. **Extend `memory_summary`** — at `crates/commands/src/handlers.rs:685-714`, after the existing `goals_count` line (694-695) add: skills count from `load_skills_from_roots` plus 10 bundled, routines count from `CronManager::global().entries().len()` plus a count of `~/.anvil/routines/output/*/` directories. (1 hour)

5. **Extend `memory_budget`** — at `crates/commands/src/handlers.rs:920-927`, replace the `("goals", home.join("goals"))` row with `("procedural", ...)` that sums: goal files bytes + `cron.json` bytes + `~/.anvil/routines/output/` bytes + on-disk skill body bytes (walk `discover_skill_roots`) + a constant for bundled skills (sum of `BUNDLED_SKILL_BODIES` body string lengths, computed at startup or inlined). Helper: extend `dir_total_bytes` at `handlers.rs:985-997` to handle nested dirs, or add `dir_total_bytes_recursive`. (3 hours)

6. **Extend `memory_inspect`** — at `crates/commands/src/handlers.rs:778-831`, after the nominations loop (lines 806-818) add a loop over `load_skills_from_roots` results matching `key_lower` against `name`, `description`, and `triggers`. Add a loop over `GoalManager::list()` matching against `description` and `tags`. Add a loop over `CronManager::global().entries()` matching against `name` and `prompt`. (2 hours)

7. **Rewrite `memory_why`** — at `crates/commands/src/handlers.rs:899-913`, expand line 4 ("Skill body ...") into a list of *currently loaded* skills (requires plumbing through whatever session state tracks the most recent `/skill load`). If session plumbing is hard, start with: "Skill bodies prepended by `/skill load`: <name> (<bytes>B)". Cite `agents::load_skill_body` at `agents.rs:775-799`. (2-3 hours; depends on session-state reachability)

8. **Stub `/memory show routines` for the empty case** — when `~/.anvil/cron.json` is empty and `~/.anvil/routines/output/` does not exist, return `"No routines configured. Routine support: Tier 2 in ROADMAP.md (items 5, 10, 20). Cron precursor available via runtime::CronManager."` This makes the sub-view discoverable now, populates when ROADMAP Tier 2 item 5 (TOML loader) lands. (30 min)

9. **Extend `memory_prune`** — at `handlers.rs:954-966`, after the nominations prune (line 961) add: prune `Goal` records where `status == Done` and `last_resumed_at` (or `created_at`) is older than N days. Add: prune files in `~/.anvil/routines/output/*/` older than N days. Reuse `prune_old_files` at `handlers.rs:999-1023`. (2 hours)

10. **Update spec doc-comment** — `crates/commands/src/specs.rs:248-278`. Replace the `goals` TIERS row at `:269` with `procedural` and document its sub-views. Add subcommand entries to `MEMORY_SUBCOMMANDS` at `crates/commands/src/subcommands.rs` (consult that file for the existing pattern). (1 hour)

11. **Flip or guard `/goal web_available`** — `crates/commands/src/specs.rs:1939` is `web_available: false`. Audit `/goal` handlers for TUI-only state writes; if read paths are web-safe, expose `/goal list`, `/goal show` on web; keep mutate paths TUI-only. Decision point with web team — defer if non-trivial. (variable, 2-8 hours)

12. **Document routines-as-L4 in the spec** — once Tier 2 item 5 (TOML loader, `ROADMAP.md:67`) lands, add a `routines` sub-view to step 2 populated by the new loader. Until then keep the stub from step 8. (no work now; recorded for future)

**Total near-term effort: ~16-20 hours (2-3 dev-days).** Steps 1-6 are the minimum viable migration; steps 7-11 round out the inspector; step 12 is the future-routine landing pad.

---

## 6. Risks and reversibility

| Risk | Mitigation | Reversibility |
|---|---|---|
| Renaming `goals` tier to `procedural` breaks existing user habits (`/memory show goals`) | Keep `goals` as a documented alias arm in `memory_show` (step 3) | Trivial — revert the doc-comment edit; aliases stay |
| `memory_budget` recursive walk on `~/.anvil/routines/output/` is slow if archive grows large | Cap recursion depth, or add `--no-archive` flag; budget calls are user-initiated so latency is bounded | Drop the recursive call; restore single-dir `dir_total_bytes` |
| `load_skill_body` lookup on `memory_inspect` adds I/O to every search | Cache the discover_skill_roots result for the lifetime of the handler call; current `memory_inspect` already does I/O for nominations | Remove the new loop |
| Exposing bundled-skill bytes in `memory_show` leaks the existence of compile-time triggers to the user (info disclosure) | Skill names + descriptions are already user-visible via `/skills`; bodies are not dumped in `/memory show` (only counts) | n/a — same posture as `/skills` |
| Goal description prompt-injection via `/memory inspect` (a malicious goal description containing `</active-goal>`) | `build_active_goal_prompt_fragment` (`goals.rs:494-497`) does not escape; pre-existing risk, not introduced here. Note for separate hardening pass. | Pre-existing |
| Routine-stub return value confuses users into thinking routines exist | Stub message names ROADMAP Tier 2 explicitly; copy the same phrasing into `/help` for `/memory` | Edit the stub string |
| `/goal web_available = true` exposes mutation paths to web | Gate write paths separately; step 11 explicitly recommends read-only first | Flip the flag back |

**Roll-back path:** every change is in two files (`handlers.rs`, `specs.rs`) plus the optional `subcommands.rs` entries. `git revert` of the L4 commit restores prior behavior; no on-disk format changes; goal/cron/routine data layouts are untouched.

---

## 7. Cross-layer dependencies

| Other layer | Touch point | Direction | Citation |
|---|---|---|---|
| **L1 Working** | Skills and slash commands inject into the working-set prompt (skill body, active-goal fragment); `/memory why` documents the inject order | L4 → L1 | `handlers.rs:899-913`, `goals.rs:494-497`, `agents.rs:775-799` |
| **L2 Episodic** | Goals carry `session_ids: Vec<String>` (`goals.rs:65-66`) and `last_resumed_at` (`:67-68`) — a goal is the join key between procedural intent and episodic history. Daily summaries (`handlers.rs:746-753`) reconcile pending tasks that may map to goal open_items | L4 ↔ L2 | `goals.rs:65-68`, `handlers.rs:746-753` |
| **L3 Semantic** | Skill bodies may reference ANVIL.md content (ex: `anvil-md-curator` skill); the `anvil-md` tier in `memory_show` (`handlers.rs:732-740`) is what skills like `pattern-promote` operate on. Procedural and semantic are pipeline-coupled (semantic *informs* procedural) | L4 ← L3 | `agents.rs:758-761` (anvil-md-curator), `handlers.rs:732-740` |
| **L5 Identity** | Skills that need credentials route via vault refs (ROUTINES-ADOPTION-NOTES §11; routines must use `vault://` per ROADMAP Tier 2 item 13). Today `requires_vault: bool` is set per slash command in `specs.rs` (false for `/memory`, true for command specs that need it) | L4 → L5 | `specs.rs:283`, `ROADMAP.md` Tier 2 item 13 |
| **L6 Policy** | Every L4 invocation is permission-gated. `SlashCommandSpec` carries `requires_vault`, `requires_restart`, `tui_available`, `web_available` (`specs.rs:25-49`). Skill triggers fire only when `permission_memory` allows; `crates/runtime/src/permission_memory.rs` plus `permissions/` enforce | L4 → L6 | `specs.rs:25-49`, `runtime/src/permission_memory.rs`, `runtime/src/permissions/` |
| **L7 Cache** | `cmd-cache` (`handlers.rs:709-710`) memoizes outputs of command-style procedural calls per W12 token economy; `command-cache-aware` skill (`agents.rs:746-749`) teaches the agent to use it; `file-cache` similarly under `file-fingerprint` skill (`agents.rs:742-745`) | L4 → L7 | `handlers.rs:707-710`, `agents.rs:742-749`, `runtime/src/command_cache.rs`, `runtime/src/file_cache.rs` |

Procedural is the **most cross-cutting** of the seven layers: it reads L3 (knowledge), executes against L5 (credentials) and L6 (permissions), writes L2 (episodic side-effects), is itself injected into L1 (working prompt), and offloads results to L7 (cache).

---

## Appendix A — File:line citation index

Primary citations used in this document:

- `crates/commands/src/specs.rs:4-10` — `SlashCommandCategory` enum
- `crates/commands/src/specs.rs:25-49` — `SlashCommandSpec` struct
- `crates/commands/src/specs.rs:241-285` — `/memory` spec
- `crates/commands/src/specs.rs:543-554` — `/skills` spec
- `crates/commands/src/specs.rs:1700-1713` — `/loop` spec
- `crates/commands/src/specs.rs:1858-1879` — `/skill` spec
- `crates/commands/src/specs.rs:1918-1939` — `/goal` spec
- `crates/commands/src/handlers.rs:685-714` — `memory_summary`
- `crates/commands/src/handlers.rs:716-776` — `memory_show`
- `crates/commands/src/handlers.rs:755-762` — goals branch
- `crates/commands/src/handlers.rs:778-831` — `memory_inspect`
- `crates/commands/src/handlers.rs:899-913` — `memory_why`
- `crates/commands/src/handlers.rs:916-951` — `memory_budget`
- `crates/commands/src/handlers.rs:954-966` — `memory_prune`
- `crates/commands/src/agents.rs:178-253` — `discover_skill_roots`
- `crates/commands/src/agents.rs:328-355` — `bundled_skill_defs` (name+description registry)
- `crates/commands/src/agents.rs:357-439` — `load_skills_from_roots`
- `crates/commands/src/agents.rs:724-766` — `BUNDLED_SKILL_BODIES`
- `crates/commands/src/agents.rs:775-799` — `load_skill_body`
- `crates/commands/src/skill_chaining.rs:32,121-142` — chain depth ≤ 3
- `crates/runtime/src/goals.rs:1-6` — storage layout doc
- `crates/runtime/src/goals.rs:32-75` — `GoalStatus`, `Goal`
- `crates/runtime/src/goals.rs:140-167` — `GoalManager`
- `crates/runtime/src/goals.rs:444` — `format_goal_list`
- `crates/runtime/src/goals.rs:494-497` — `build_active_goal_prompt_fragment`
- `crates/runtime/src/cron.rs:15-26` — `CronEntry`
- `crates/runtime/src/cron.rs:51-59` — global singleton at `~/.anvil/cron.json`
- `crates/runtime/src/routines/mod.rs:1-26` — submodule index + `SILENT_MARKER`
- `crates/runtime/src/routines/archive.rs:1-9` — archive path layout
- `crates/runtime/src/routines/packet.rs:1-50` — packet schema
- `crates/runtime/src/routines/schedule.rs:1-32` — schedule grammar
- `crates/runtime/src/lib.rs:22,135` — module + re-exports
- `crates/commands/bundled/skills/{security-audit,code-review,terse,token-economy,file-fingerprint,command-cache-aware,pattern-promote,cache-budget,anvil-md-curator,silent-cat}/SKILL.md` — 10 bundled skill bodies
- `docs/ROUTINES-ADOPTION-NOTES.md` §7.5 (token-budgeted injection), §10.5 (pre-dispatch reconciliation), §12.5 (skill discipline)
- `ROADMAP.md:55-91` — Tier 2 routines backlog (items 1-29)