# Capability Cohesion — synthesis

**Status:** v2 — corrected against tree at HEAD `6870566` (post Phase 5.0.5 dispatch unification), 2026-05-14.
**Audit basis:** four parallel read-only audits + 1 re-verification pass against tree. Raw audits at `/tmp/capability-audit-{slash,tools,permissions,skills-agents-plugins}.md`. Re-verification at `/tmp/cohesion-reverify.md`.
**Companion to:** [`SEVEN-LAYER-MEMORY.md`](SEVEN-LAYER-MEMORY.md) (the memory arc).
**Scope:** slash commands, tools, permission/sandbox/hooks chain, skills+agents+plugins.

## Defect status after re-verification

Of the 23 originally synthesized defects:
- **13 CONFIRMED** still need fixing: #1, #4, #5, #6, #7, #8, #9, #10, #13, #15, #16, #20, #21, #22, #23 (with #16 being intentional-but-undocumented)
- **5 FALSE POSITIVES** — audit artifacts, no fix needed: #2 (AskUserQuestion wired at `providers.rs:1233`), #3 (MCP `ANVIL_PROJECT_DIR` wired at `transport.rs:43-44`), #12 (`goal` parser arm at `lib.rs:884`), #14 (memory tier validation at `handlers.rs:1027-1032`), #17 (hook fire sites verified at main.rs:2225/3640 + others)
- **1 PARTIAL:** #11 (`/model` and `/ollama` completion forks — both paths exist but read different sources)
- **3 NEEDS HUMAN CHECK:** #11 verification, #18 (hook `string[]` exec form), #19 (MCP hooks parser forward-compat)

Phase 5.0.5 dispatch unification fixed ZERO of the 23 cohesion defects (they predate it), but it shrank main.rs by 1,560 lines and eliminated 99 stub messages, removing a class of bugs not in this list.

**Real Phase 5.1/5.2/5.3 scope:** 14 defects (13 CONFIRMED + 1 PARTIAL), not 23.

---

## 1. How to read this doc

The seven-layer memory arc had one organizing principle: every memory tier should be coherent. This doc has the same shape for capabilities: every user-facing affordance should work end-to-end without registry drift.

The audits surfaced **23 concrete defects** and **6 structural causes**. Most are fixable without architectural change. Three (egress non-enforcement; plugin↔skill/agent bundling gap; subagent permission-mode inheritance) are user-visible product bugs and deserve a v2.2.15 release. The structural causes deserve a v2.3 milestone — "one registration per capability" — that prevents the next regression class.

The bucket structure mirrors the memory arc. Phase 1 fixes the bugs. Phase 2 unifies the registries. Phase 3 lands the structural prevention.

---

## 2. The cohesion model

A "capability" in Anvil today is any user-facing affordance: slash command, tool, permission/safety check, skill, agent, plugin. Each capability has up to **eight axes** that must stay in sync:

1. **Definition** — the source of truth (enum variant, JSON schema, YAML front-matter)
2. **Registration** — the lookup table the runtime queries (spec registry, tool_specs, skill index)
3. **Completion / discovery** — what the user sees when they hit TAB or `/`
4. **Handler** — the actual implementation
5. **Dispatch** — the runtime path that calls the handler
6. **Rendering** — TUI scrollback + viewer.html + tool-call card (three surfaces today, drift-prone)
7. **Permission gate** — does this capability flow through the safety chain?
8. **OTel + tests** — observability and regression coverage

**Cohesion failure modes:**
- **Axis drift** — one axis updated, another wasn't (the `/ollama` and `/file-cache` class).
- **Vocabulary fork** — same concept named two things across surfaces (skills' `triggers` vs traits' `dimensions`).
- **State fork** — same logical state stored in two places (`.anvil-agents/*.json` vs `~/.anvil/teams.json`).
- **Gate fork** — three permission systems with no unified registry (Anvil core grants, plugin `install_policy`, MCP auth).
- **Telemetry blind spot** — bash injects TRACEPARENT; plugin tool calls and skill invocations don't.

The memory arc's `WorkingMemorySnapshot` + `PromptSection` typed-storage pattern is the precedent: a single source of truth that downstream surfaces *render*, not duplicate.

---

## 3. Discovered defects

23 numbered defects, file:line cited. Severity per audit team. The **Critical** rows are user-visible product bugs that ship today on every Max-plan install.

| # | Surface | Defect | File:line | Severity |
|---|---|---|---|---|
| 1 | Tools/Perm | `EgressPolicy::is_allowed` is never consulted by `tools/web_ops.rs::execute_web_fetch`. The allowlist landed in settings.json (commit `6e5f638`) but enforcement is unwired. Users setting `security.egress_allowlist` get no isolation. | `crates/tools/src/web_ops.rs:72` (no `is_allowed` call) | **Critical** |
| 2 | Tools | `AskUserQuestion` tool has a spec but `execute_tool` has no match arm — the tool never executes when the LLM emits it. | `crates/tools/src/lib.rs::execute_tool` (no AskUserQuestion arm) | **Critical** |
| 3 | Agents | Subagent permission-mode inheritance (CC-BUG-3/4, task #83) is **declared but not wired**. `ConversationRuntime::spawn_subagent` does not pass parent's `permission_mode` to the child. DangerFullAccess parent's subagents land in default mode. | `crates/runtime/src/agent_ctx.rs:spawn_subagent` (NEEDS HUMAN CHECK on exact line) | **Critical** |
| 4 | Plugins | Plugin manifest has no `skills` or `agents` fields. Plugins shipping a SKILL.md or agent JSON are silently ignored at load. No cross-surface bundling. | `crates/plugins/src/manifest.rs:PluginManifest` (entire struct) | **High** |
| 5 | Skills | Plugin-installed skill roots are not in `discover_skill_roots`. A plugin's `~/.anvil/plugins/X/skills/` directory is invisible to the skill loader. | `crates/commands/src/agents.rs:discover_skill_roots:73-97` | **High** |
| 6 | Tools | Tool-result rendering forks across **three** surfaces with no shared formatter: `format_tool.rs` (TUI scrollback), `viewer.html`, and the pretty card. Only **7 of 38 tools** have explicit branches; 31 fall through to generic key-extraction. | `crates/tools/src/format_tool.rs` (count branches) | **High** |
| 7 | Tools | MCP/LSP tools (ListMcpResourcesTool, ReadMcpResourceTool, LSPTool, SendMessage) have no fallback formatter — their output renders as raw JSON. | `crates/tools/src/format_tool.rs` (no MCP arm) | **High** |
| 8 | Tools | `Glob` and `Grep` have no `FileCache` integration despite the cache shipping. Same call repeats full-scan every turn. | `crates/tools/src/file_tools.rs` (no `FileCacheManager::lookup`) | **High** |
| 9 | Tools | `WebFetch`, `WebSearch`, `RemoteTrigger` are never cached via `CommandCache`. | `crates/tools/src/web_ops.rs` (no `CommandCacheManager` callsite) | **High** |
| 10 | Slash | Subcommand vocabulary lives in 3+ places per command: parser match, spec hint string, handler dispatch. `/memory`, `/skills`, `/config`, `/agent` all drift independently. | `crates/commands/src/lib.rs:573-575`, `specs.rs:304`, `handlers.rs:173-220` | **High** |
| 11 | Slash | Completion logic forks: `/model` uses live provider APIs (commit `186de12`); `/ollama` uses stale static arrays. No shared interface. | `crates/anvil-cli/src/tui/completion.rs:374` vs `crates/commands/src/subcommands.rs` | **High** |
| 12 | Slash | Orphan spec entry: `goal` exists in `specs.rs:489` with no matching parser arm in `lib.rs`. The bidirectional drift test catches enum→spec but missed this spec→enum direction. (NEEDS HUMAN CHECK — confirm the drift test actually checks both directions; the audit found one orphan still present.) | `crates/commands/src/specs.rs:489` | **Medium** |
| 13 | Slash | Specs advertise `[allow\|deny\|...]` subcommand hints but ship empty `subcommands: &[]` arrays. `/permissions`, `/export`, `/knowledge` are user-visible offenders. | `crates/commands/src/specs.rs:349, 394, 504` | **Medium** |
| 14 | Slash | Handler dispatch lacks pre-validation. `/memory show random-tier` silently no-ops instead of erroring. Handlers receive raw strings; the spec's subcommand list isn't enforced. | `crates/commands/src/handlers.rs::memory_show` (no validation) | **Medium** |
| 15 | Slash | TUI thread safety is not principled. Handlers are `fn` not `async fn`. The `/changelog` synchronous-LLM-call bug (already fixed) is symptomatic — no guard prevents the next handler from doing the same. | `crates/commands/src/handlers.rs` (all handlers) | **Medium** |
| 16 | Perm | Hooks fire **before** the permission gate, giving hooks override authority. Intentional but undocumented — a hook can `allow` an operation the gate would otherwise prompt for. | `crates/runtime/src/conversation/permission_gate.rs:25-154` (order) | Medium |
| 17 | Perm | SessionStart, SessionEnd, CwdChanged, PostToolBatch, Notification hook events have no traceable fire site in the runtime. The events are documented but unverified-as-emitted. (NEEDS HUMAN CHECK) | `crates/runtime/src/hooks.rs::HookEvent` variants vs callsites | Medium |
| 18 | Perm | `string[]` exec form (CC-139-F6, task #453) lands in the schema but the runtime executor still expects shell_command. Hooks that ship as `args: ["bash", "-c", "..."]` may fall back to shell anyway. | `crates/runtime/src/config/hooks.rs` (parse path) | Medium |
| 19 | Perm | MCP hook invocation (FEAT-30, task #272) is implemented in the runtime but the config parser still expects `plugins::HookSpec`, not `RuntimeHookSpec`. Plugins that ship MCP hooks may not register. | `crates/runtime/src/config/hooks.rs` (parser) | Medium |
| 20 | Agents | `anvil agents live` (CC-139-F1, task #462) reads `~/.anvil/agents/*.json` but does NOT include subagents spawned via TeamDelegate (which store in `~/.anvil/teams.json`). User's "live monitor" is incomplete during active delegations. | `crates/runtime/src/agent_snapshot.rs:list_live_snapshots` | Medium |
| 21 | Plugins | Plugin tool calls have no OTel instrumentation. Bash injects TRACEPARENT (task #477); plugin tool dispatch doesn't. | `crates/plugins/src/manager.rs::execute_tool` | Medium |
| 22 | Teams | TeamDelegate subagent OTel headers (CC-139-F16, task #461) are partially wired. `TeamManager::delegate_task` returns `DelegationRecord` with no agent-context fields — parent-agent-id never reaches the subagent's HTTP layer. | `crates/runtime/src/team.rs:delegate_task` | Medium |
| 23 | Teams | TeamDelegate `shared_context` is visible to all members, but member `system_prompt` is set once at add-member time and reused — if the parent modifies context mid-delegation, the subagent's prompt is **stale**. | `crates/runtime/src/team.rs:TeamMember` | Medium |

**Critical defects #1–3 are user-visible product bugs.** They should ship as a v2.2.15 patch independent of the structural arc.

---

## 4. Structural causes

Six structural causes underlie the 23 defects. Each enables a class of drift; fixing one closes multiple defects.

### C-1 — One capability, N registries (no shared registration)
Slash commands have 5+ registries (parser, spec, subcommands, completion, handler, dispatch). Tools have 12+ axes (spec, handler, gate, egress, sandbox, TUI, viewer, OTel, hooks, cache, MCP, errors, tests). Skills, agents, plugins each have their own discovery + loader + index. No single registration unifies a capability's axes. Drift is structurally inevitable.

**Closes defects:** #10, #11, #12, #13, #14, #15 (slash drift class); also enables a unified package format that closes #4, #5.

### C-2 — Vocabulary fork across surfaces
- Skill front-matter says `triggers`; traits say `dimensions`; eval harness says `category`.
- `/memory show nominations` vs `/memory show semantic --pending` (memory arc already fixed this with the alias deprecation).
- `subcommands` listed inline in specs hint strings vs in `MEMORY_SUBCOMMANDS` arrays.
- Tool result rendering uses different shape keys per surface (TUI vs viewer.html vs pretty card).

**Closes defects:** #6, #7, #10, S4 (skill vocabulary drift).

### C-3 — State fork: same logical thing in two stores
- Agents in `.anvil-agents/*.json`; team members in `~/.anvil/teams.json`; cross-session view queries only the first.
- Skills in `~/.anvil/skills/` + bundled; plugin-shipped skills in `~/.anvil/plugins/X/skills/`; loader doesn't merge.
- Slash command parser parses what the spec doesn't advertise (or vice versa).

**Closes defects:** #5, #20, #12 (orphan).

### C-4 — Three permission models, no unified registry
- Anvil core: `permission_gate.rs` (auto-mode → hook → memory → reviewer → policy → prompter).
- Plugin install: `requirements.toml::check_install_policy`.
- MCP server auth: each MCP server has its own.

A plugin install that triggers a tool call goes through plugin gate, then the tool gate; a memory grant doesn't apply to plugin installs and vice versa. The three systems don't observe each other's decisions.

**Closes defects:** P3 (plugin install gate independence). Bigger architectural prize: one decision log, one OTel feed, one user-facing `/permissions` surface.

### C-5 — Egress is loaded but not enforced
The egress allowlist parses cleanly (`6e5f638`), surfaces in `/memory show policy`, but **no tool consults it**. WebFetch, WebSearch, RemoteTrigger, plugin tool egress, bash subshell network calls, hook command exec — all unconstrained. This is the **product gap** that's most user-visible: a security feature that doesn't secure anything.

**Closes defects:** #1, plus implicit gaps in #9, #21.

### C-6 — OTel blind spots
- Bash tool calls: TRACEPARENT injected (`477`) ✓
- MCP server stdio calls: TRACEPARENT injected ✓
- Plugin tool dispatch: **missing**
- Skill invocations: **missing** (are they tool calls? unclear)
- Subagent spawns: `parent-agent-id` **not wired** (task #461 incomplete)

A user can't reconstruct what their AI did in a session — coverage forks by surface.

**Closes defects:** #21, #22.

---

## 5. Reconciled migration sequence

**Per user direction 2026-05-14:** the entire arc lands as **v2.2.14 Phase 5 — Capability Cohesion**, not a multi-release schedule. v2.2.14 does not tag until every bucket is 8/8 across every capability. The 8-axis contract (see `feedback-anvil-capability-contract.md`) is enforced at task-completion time, not at release-prep time.

Five buckets, ordered by dependency, each revert-safe on `v2.2.14-phase1`. Total scope: **roughly 15–20 engineer-days** of work; **~3–5 days wall-clock** with parallel agents (Bucket 0 first, Buckets 1+2+3 in parallel, Bucket 4 last, plus a parallel CC-parity reverification stream).

```mermaid
graph TD
  subgraph B0["Bucket 0 (v2.2.14+) — Cheap gates"]
    b0a[Bidirectional drift tests: spec→enum, completion→handler]
    b0b[Smoke test: every command in menu has a real handler]
    b0c[Lint: no synchronous LLM calls in slash handlers]
  end

  subgraph B1["Bucket 1 (v2.2.15) — Critical product fixes"]
    b1a["#1 Wire EgressPolicy::is_allowed into web_ops.rs + automation_ops.rs"]
    b1b["#2 Add AskUserQuestion handler arm"]
    b1c["#3 Wire permission_mode inheritance into spawn_subagent"]
    b1d["#5 Add plugin roots to discover_skill_roots"]
    b1e["#20 Merge teams.json into anvil agents live view"]
    b1f["#22 Wire parent-agent-id into subagent HTTP layer"]
  end

  subgraph B2["Bucket 2 (v2.2.16) — Vocabulary + state unification"]
    b2a["#6 Shared tool-result formatter (Vec<ResultBlock>)"]
    b2b["#7 MCP/LSP fallback formatter"]
    b2c["#8 #9 Wire Glob/Grep/Web tools to FileCache + CommandCache"]
    b2d["#10 Unify subcommand vocabulary (single source per command)"]
    b2e["#11 Shared LiveCompletionProvider trait"]
    b2f["#14 Spec-driven dispatch validation"]
    b2g[Skill vocabulary normalization (triggers/dimensions/category)]
  end

  subgraph B3["Bucket 3 (v2.2.17) — Structural prevention"]
    b3a["C-1 Single capability_registration! macro"]
    b3b["C-3 PluginManifest gains skills + agents fields"]
    b3c["C-4 Unified permission registry across core + plugin + MCP"]
    b3d["C-6 OTel parity audit + close blind spots"]
    b3e[Hook event coverage verification + missing-event tests]
  end

  subgraph B4["Bucket 4 (v2.3.0) — Polish"]
    b4a[Hook execution form: string[] vs shell, document precedence]
    b4b[Sandbox + permission_mode interaction docs + tests]
    b4c[Subagent context re-evaluation per delegation]
    b4d["Cross-skill chaining surface (#23, depth-3, TUI visibility)"]
  end

  B0 --> B1
  B1 --> B2
  B2 --> B3
  B3 --> B4
```

### Bucket 0 — Phase 5.0 — Drift gates (~0.5 days, lands first, single agent)

Ship now. No new affordances; just regression-class prevention.

1. **Extend the bidirectional drift test** to a third axis: every spec entry must have a parser arm AND a handler match. The current `every_slash_command_variant_has_a_spec` only checks enum↔spec. Add `every_spec_has_a_handler` and `every_handler_has_a_spec`. (~2 hours)
2. **Smoke test: menu↔handler reachability**. For every command in `slash_command_specs()`, dispatch it with an empty arg list; assert the handler doesn't print "(stub)" or "usage:" boilerplate. (~2 hours)
3. **Lint: no `run_internal_prompt_text` in slash handlers**. The `/changelog` bug class. A `#[deny]` lint rule or a build-time grep gate. (~1 hour)

### Bucket 1 — Phase 5.1 — Critical product fixes (~3 days, parallel with 5.2 + 5.3)

Closes the three user-visible critical bugs + four high-severity gaps. All on `v2.2.14-phase1`.

1. **#1 Egress enforcement** — wire `EgressPolicy::is_allowed` into `web_ops.rs::execute_web_fetch` + `web_ops.rs::execute_web_search` + `tools/automation_ops.rs::reqwest` calls. Return structured error when denied; emit OTel `egress.denied`. (~1 day)
2. **#2 AskUserQuestion handler** — add the match arm; route to TUI modal. (~3 hours)
3. **#3 Permission mode inheritance** — `spawn_subagent` accepts and propagates parent's `permission_mode`. Add explicit test: parent in DangerFullAccess → subagent inherits. (~4 hours)
4. **#5 Plugin skill roots** — `discover_skill_roots` queries the active plugin manager for installed-plugin skill directories. (~3 hours)
5. **#20 Cross-session agent view** — `anvil agents live` queries BOTH `agents/*.json` AND `teams.json::active_delegations`. (~3 hours)
6. **#22 Parent-agent-id wiring** — `TeamManager::delegate_task` passes parent agent context to the subagent HTTP layer; OTel header lands on outgoing requests. (~4 hours)

### Bucket 2 — v2.2.16 — Vocabulary + state unification (~5 days)

The hard work of un-forking. No new behavior — just collapses the drift surfaces.

1. **#6 Shared tool-result formatter** — replace per-site formatting in `format_tool.rs`/`viewer.html`/pretty card with a typed `ResultBlock` schema. Each tool returns blocks; all three surfaces render the same blocks. (~1.5 days)
2. **#7 MCP/LSP fallback formatter** — covers tools without explicit format branches. (~3 hours)
3. **#8 / #9 Cache integration** — Glob/Grep results consult `FileCacheManager::lookup` first; WebFetch/WebSearch/RemoteTrigger consult `CommandCacheManager`. Respect `is_l5_path` sentinel from memory arc Phase 3.5. (~1 day)
4. **#10 Subcommand vocabulary unification** — single `SUBCOMMANDS` const per command, consumed by parser + spec hint + handler validation. (~6 hours)
5. **#11 LiveCompletionProvider trait** — completion handlers implement a common interface; `/model` and `/ollama` share the live-fetch path. (~6 hours)
6. **#14 Spec-driven dispatch validation** — handler entry point validates argv against the spec's subcommand list before dispatching. Unknown subcommand → user-facing error. (~3 hours)
7. **Skill vocabulary normalization** — `triggers`/`dimensions`/`category` collapse to a single term in user-facing surfaces. (~3 hours)

### Bucket 3 — v2.2.17 — Structural prevention (~6 days)

The architecture work. Lands the registration discipline that prevents the next cohesion arc.

1. **C-1 capability_registration! macro** — one macro per capability that registers all axes atomically (parser, spec, completion, handler, dispatch, MCP exposure, tests). Migrate 5–10 slash commands as the proof. (~2.5 days)
2. **C-3 PluginManifest gains skills + agents** — manifest schema accepts `skills: [...]`, `agents: [...]`. Plugin loader registers them with the respective indices. (~1.5 days)
3. **C-4 Unified permission registry** — one decision log, one `/permissions` view across core grants + plugin install gates + MCP auth. (~1 day spec, deferrable impl)
4. **C-6 OTel parity audit** — plugin tool dispatch + skill invocations get OTel hookpoints. Subagent `parent-agent-id` reaches every HTTP call. (~6 hours)
5. **Hook event coverage** — verify SessionStart/SessionEnd/CwdChanged/PostToolBatch/Notification fire from the runtime. Add tests for each. (~6 hours)

### Bucket 4 — v2.3.0 — Polish (~2 days)

Lowest urgency.

1. Hook execution form documentation (`string[]` vs shell precedence). (~2 hours)
2. Sandbox + permission_mode interaction matrix in docs + tests. (~4 hours)
3. **#23** subagent context re-evaluation per delegation. (~3 hours)
4. Cross-skill chaining surface — make the depth-3 chain visible in TUI as one workflow, not N separate invocations. (~4 hours)

---

## 6. Cross-bucket dependency notes

- **Bucket 0 unblocks confidence in Bucket 1.** Without the drift gates, every Bucket 1 fix risks reintroducing a different axis drift on landing.
- **Bucket 2's `ResultBlock` schema unblocks Bucket 3's macro work** — the macro registers a tool with a typed output block, which the shared formatter renders.
- **Bucket 1 #1 (egress enforcement) is the gating release blocker.** Currently the egress allowlist is a security feature that doesn't secure anything; v2.2.15 should not ship without it.
- **Bucket 3's plugin-manifest expansion is mutually exclusive with the v2.2.14 plugin manifest contract.** It's an additive schema change (new optional fields), but ANY existing plugin distribution will need to opt in to use the new affordances. Plan a one-cycle alias / migration window.
- **The macro-based registration (C-1) is intentionally not in Bucket 1.** Bucket 1 fixes bugs the macro would have prevented; lay the macro in Bucket 3 once the per-axis pain is fresh and the abstraction's seams are obvious.

---

## 7. Strict SKIP list

What this arc explicitly does NOT do, even though it surfaced during audit:

- **No new tools.** Tools added in Bucket 2 are wiring (cache, formatter); no new tool functions.
- **No MCP server changes.** MCP auth remains its own thing; unified permission registry is Bucket 3 spec only.
- **No skill content changes.** Skill front-matter schema doesn't change beyond vocabulary normalization (the user-facing name).
- **No new permission modes.** DangerFullAccess / acceptEdits / read-only remain the three modes.
- **No deprecation of slash commands.** Aliases stay one cycle (memory arc precedent).
- **No release surfaces.** This arc ships as code changes on `v2.2.14-phase1` → `v2.2.15`, etc. No tag movement until you authorize.

---

## 8. Verification

For each bucket, the acceptance test:

- **Bucket 0:** All three new drift tests pass on green main; intentionally breaking the spec/parser/handler axis fails the test deterministically.
- **Bucket 1:** Curl probe with egress disabled denies the WebFetch call; `AskUserQuestion` from an LLM completes via TUI modal; a DangerFullAccess parent's subagent runs in DangerFullAccess; `anvil agents live` lists a delegated subagent.
- **Bucket 2:** All tool results render with the same structure across TUI + viewer + pretty card (snapshot test); `/model` and `/ollama` completion both come from the same provider-API path; `Glob foo.rs` twice in a session shows a cache hit on the second.
- **Bucket 3:** `capability_registration!` migration converts 5 slash commands; total spec/parser/handler/test LOC drops; a malformed plugin manifest with a typo'd `skills:` field falls back to default behavior (forward-compat from F5 extends to new fields).

---

## 9. What this synthesis did NOT do

- **Did not run the smoke tests.** The audits are read-only static analysis. Defects flagged "NEEDS HUMAN CHECK" need TUI exercise to confirm.
- **Did not estimate the macro (C-1) work bottom-up.** 2.5 days is a top-down estimate from the memory arc's prompt-section refactor. Could be more.
- **Did not consult the user-visible side of the four surfaces** (no actual TUI runs). The defects are evidence-based from code paths; some may not manifest in a live session.
- **Did not pre-decide which bucket goes in which release.** The release-vehicle decision (v2.2.15 = Bucket 1, etc.) is a recommendation; user holds release authorization.
- **Did not name an owner.** All buckets are sequenced for one engineer + one agent stream; if more capacity is available, Buckets 1, 2, 3 can overlap on parallel tracks once Bucket 0 lands.

---

## 10. See also

- [`SEVEN-LAYER-MEMORY.md`](SEVEN-LAYER-MEMORY.md) — the memory arc this synthesis mirrors.
- [`V3-ARC-PLAN.md`](V3-ARC-PLAN.md) — the four-phase plan that became Phases 1–4 of v2.2.14.
- `/tmp/capability-audit-slash.md` — raw slash command audit
- `/tmp/capability-audit-tools.md` — raw tools audit
- `/tmp/capability-audit-permissions.md` — raw permission/sandbox/hooks audit
- `/tmp/capability-audit-skills-agents-plugins.md` — raw skills/agents/plugins audit (also embedded in this turn's agent reply)
