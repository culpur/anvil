# MEMORY-LAYER-6 — Policy

**Layer:** L6 — Policy
**Audit version:** Anvil v2.2.13 (workspace `Cargo.toml:6`; brief notes the audit target as v2.2.14, but `HEAD` reads `2.2.13`).
**Sibling docs (not yet written):** L1 Working, L2 Episodic, L3 Semantic, L4 Procedural, L5 Identity, L7 Cache.
**Synthesis target:** `docs/research/SEVEN-LAYER-MEMORY.md`.

---

## 1. Layer definition

L6 is the **decisions** layer: a durable record of *what the agent is allowed to do* in this project. It sits between L5 *secrets* (what you know that's sensitive — credentials, vault contents) and L7 *cache* (what you've already computed — file/command outputs). Where L5 answers "what do I know?" and L7 answers "what did I compute?", L6 answers **"what am I permitted to do?"** It is policy-shaped, not knowledge-shaped: every entry is a *grant*, *deny*, or *escalation rule* scoped to a tool/input pattern, not a fact about the world. Today this layer is operative inside the runtime (a tool call cannot run without consulting it) but is **entirely invisible to `/memory`** — promoting it to L6 is mostly an exposure move, not a new abstraction.

---

## 2. Current Anvil state

### 2.1 Owning files

| Concern | File | Notes |
|---|---|---|
| Persisted grants (the L6 store) | `crates/runtime/src/permission_memory.rs:1-191` | Primary owner. Defines `PermissionScope`, `PermissionMemoryEntry`, `PermissionMemory`. |
| Static policy types & gate | `crates/runtime/src/permissions/mod.rs:1-144` | `PermissionMode`, `PermissionPolicy`, `PermissionPrompter`, `PermissionOutcome`. The runtime *type* of L6 decisions. |
| Sync reviewer (deny-on-pattern) | `crates/runtime/src/permissions/reviewer.rs:1-80` | `ReviewerConfig`, `ReviewerMode`, `BlockAction`. Config-driven deterministic regex gate. Policy-shaped: rules about what to refuse. |
| Auto-mode hard-deny list | `crates/runtime/src/auto_mode.rs:1-64` | `AutoModeConfig.hard_deny: Vec<String>`. Per-project deny patterns loaded from `settings.json#/autoMode`. |
| Egress allowlist | `crates/runtime/src/egress.rs:1-80` | `EgressPolicy { allowlist, enabled }`. Domain-scoped network policy. |
| Content filter rules | `crates/runtime/src/content_filter.rs:1-80` | `ContentFilterConfig`. Injection-pattern and secret-pattern deny rules. |
| Consumption / decision point | `crates/runtime/src/conversation/permission_gate.rs:25-154` | `evaluate_and_execute()` — the unified gate that orders auto-mode-hard-deny → hooks → reviewer → policy → prompter. |
| Module export | `crates/runtime/src/lib.rs:48`, `lib.rs:122` | `mod permission_memory;` and `pub use permission_memory::{PermissionMemory, PermissionMemoryEntry, PermissionScope};` |

### 2.2 `PermissionScope` enum (`permission_memory.rs:18-27`)

Exactly the three variants asked about:

| Variant | Lifetime | Persisted? |
|---|---|---|
| `Session` | Current CLI process only | No (in-memory) |
| `Project` | This project directory | Yes — project file |
| `Global` | All projects on this machine | Yes — global file |

### 2.3 `PermissionMemoryEntry` shape (`permission_memory.rs:29-39`)

```
tool_name: String              // e.g. "bash", "write_file"
input_pattern: Option<String>  // None = wildcard for this tool
scope: PermissionScope
granted_at: u64                // unix seconds
```

Matching rule (`permission_memory.rs:113-121`): a grant matches when `tool_name` equals the request AND (`input_pattern` is `None` OR `input.contains(pattern)`). Substring, not glob.

### 2.4 On-disk layout

| Scope | Path | Format | Created when |
|---|---|---|---|
| Project | `~/.anvil/projects/<project_path_hash>/permissions.json` | JSON, `{ "entries": [PermissionMemoryEntry...] }` (`permission_memory.rs:58-61`) | `save()` called with any non-`Session` entry. |
| Global | `~/.anvil/permissions.json` | Same JSON shape | Same. |
| Session | (in-memory only) | — | Never persisted (`permission_memory.rs:163` filters out `Session` before write). |

- `<project_path_hash>` comes from `crate::memory::project_path_hash(&canonical)` (`permission_memory.rs:14`, `permission_memory.rs:76-80`) — the **same hashing scheme used by private_memory**. L6 is therefore **tied to a project hash exactly like L5 private memory**.
- Load merges global *first*, then project, so project entries shadow global on conflict (`permission_memory.rs:86-98`).

### 2.5 Retention

No TTL, no prune. Once written, `permissions.json` lives until the file is deleted manually. `save()` only persists the file if at least one non-`Session` entry exists (`permission_memory.rs:167-170`) — empty stores never create a file.

### 2.6 Observability gap

`PermissionMemory` is `pub use`-d from `crates/runtime/src/lib.rs:122`, but **a workspace grep for `PermissionMemory` outside its own file and `lib.rs` returns zero hits**:

```
$ grep -rn "PermissionMemory" crates/ --include="*.rs" \
    | grep -v 'permission_memory.rs\|lib.rs'
# (no output)
```

So the *struct exists, is tested, and is exported* — but **no production code in this tree constructs, loads, or queries it**. The actual gate at turn time uses `PermissionPolicy` (`crates/runtime/src/permissions/mod.rs`) and `PermissionPromptDecision::AllowAlways` (`mod.rs:42`) to remember a session grant via the prompter, never reaching `permission_memory`. **The persistent L6 store is dead code in v2.2.13 — wired but not used.** That changes the migration scope considerably (see §5).

---

## 3. What's missing or miscategorized

| Gap | Where | Severity |
|---|---|---|
| `permission_memory` not surfaced in `/memory` at all | `crates/commands/src/handlers.rs:716-776` (`memory_show`'s `match tier`), `crates/commands/src/subcommands.rs:1486-1538` (`MEMORY_SUBCOMMANDS`) | High — primary L6 store is invisible. |
| `permission_memory` not connected to the gate | `crates/runtime/src/conversation/permission_gate.rs:25-154` | High — the persistent store is dead code; only session-only `AllowAlways` exists via `PermissionPromptDecision` (`permissions/mod.rs:42`). |
| Auto-mode hard-deny list not exposed as policy | `crates/runtime/src/auto_mode.rs:29-32` | Medium — user-authored deny patterns are policy and should be visible. |
| Reviewer config not exposed as policy | `crates/runtime/src/permissions/reviewer.rs:52-72` | Medium — user-extended `extra_destructive_patterns`/`extra_credential_patterns` are *project policy* not runtime config. |
| Egress allowlist not exposed as policy | `crates/runtime/src/egress.rs:22-37` | Medium — `EgressPolicy` is loaded from `~/.anvil/config.json#/security.egress_allowlist` and is decisions about what URLs tools may reach. Clearly L6. |
| Content-filter custom patterns | `crates/runtime/src/content_filter.rs:8-19` | Low — `extra_injection_patterns` / `extra_secret_patterns` are user-authored rules. Arguably policy *about untrusted content* rather than policy *about agent actions* — defensible either way (see §3.1). |
| No deny-list or "prompt-each-time" entries in `PermissionMemoryEntry` | `permission_memory.rs:29-39` | High — entries are allow-only; there is no way to persistently *deny* a tool/pattern (auto-mode hard-deny is the closest, but lives in `settings.json`, not the memory store). |
| Vault unlock state ambiguous between L5 and L6 | `crates/runtime/src/lib.rs:178` exposes `vault_is_session_unlocked` | Low — see §7. |

### 3.1 What is policy vs runtime config?

The defensible split:

- **Policy (L6):** rules the user (or the agent on the user's behalf) *decides* and that subsequent runs should remember. Includes `permission_memory.json`, `autoMode.hard_deny`, reviewer extra patterns, egress allowlist edits. These are *promotion-eligible* — they survive sessions and define what the agent may do.
- **Runtime config:** boolean toggles / mode selectors set once at startup (`PermissionMode::ReadOnly` vs `WorkspaceWrite` vs `DangerFullAccess` in `permissions/mod.rs:8-14`; `enabled` flags on egress/reviewer). These are *configuration* — they shape behavior but are not "decisions about specific actions."

`auto_mode.rs` is policy. The doc-comment at `auto_mode.rs:5-10` even describes it as "the safety override … never want certain operations attempted" — pure decision content.

`content_filter.rs` is the grayest: the *enabled flag* is config, but the user-supplied patterns are policy. Recommend treating the *patterns* as L6 visible-state and the *flag* as runtime config.

---

## 4. Inspector surface

After migration, `/memory show policy` and friends should expose the following.

### 4.1 `/memory show policy`

```
=== policy contents ===

Permission grants (PermissionMemory):
  Scope     Tool             Input pattern         Granted (UTC)
  -------   -------------    -------------------   ---------------------
  global    read_file        (any)                 2025-11-04 14:02
  project   bash             cargo test            2025-11-12 09:18
  project   bash             cargo build           2025-11-12 09:21
  session   write_file       docs/                 2026-05-12 10:44
  (4 entries: 1 global, 2 project, 1 session)

Auto-mode hard-deny (autoMode.hard_deny):
  Bash(rm -rf *)
  Bash(git push --force*)
  write_file(/etc/*)

Reviewer extra patterns:
  destructive: ["DROP\\s+TABLE", "truncate\\s+--no-warnings"]
  credential : ["sk-ant-[A-Za-z0-9]{32,}"]

Egress allowlist (enabled=false):
  api.anthropic.com
  api.openai.com
  api.x.ai
  ...
```

Three sub-tables. Each row has a *scope*, a *tool/pattern*, and a *source file*. Source attribution matters: a user inspecting the policy must be able to tell whether a deny came from `permissions.json`, `settings.json`, or a default.

### 4.2 `/memory inspect <key>` extension

Currently `memory_inspect()` only searches anvil-md and nominations (`handlers.rs:792-818`). It should also walk `PermissionMemory::all_entries()` (`permission_memory.rs:188-190`) and match on `tool_name` or `input_pattern`. Same for hard-deny entries.

### 4.3 `/memory budget`

Add a `policy` row:

```
  policy           <bytes>      <~tokens>
```

`policy` is comparable in size to `nominations` — a few hundred bytes typical, KB-scale worst case. No injection-prompt cost (policy is not injected into the system prompt; see §7 cross-layer notes).

### 4.4 New entry types proposed for `PermissionMemoryEntry`

Today: implicit allow. To support **deny** and **prompt-each-time** rows visibly:

```rust
pub enum PermissionEffect { Allow, Deny, Prompt }   // new
pub struct PermissionMemoryEntry {
    tool_name: String,
    input_pattern: Option<String>,
    scope: PermissionScope,
    effect: PermissionEffect,                       // new
    granted_at: u64,
}
```

This is a *forward-compatible* schema bump: old files (which have no `effect` field) deserialize as `Allow` via `#[serde(default)]`.

---

## 5. Migration moves

Numbered list, in dependency order.

1. **Wire `PermissionMemory` into the gate.** Today the gate at `permission_gate.rs:25-154` consults policy/reviewer/auto-mode/prompter but never reads from disk. Add a `PermissionMemory` load on session start, query `mem.is_allowed(tool, input)` *before* invoking the prompter, and call `mem.grant(...) + save()` on `PermissionPromptDecision::AllowAlways` (`permissions/mod.rs:42`). Without this, L6 is decorative. Effort: ~1 day. Files: `crates/runtime/src/conversation/permission_gate.rs` (edit), `crates/runtime/src/conversation/mod.rs` (thread a `&mut PermissionMemory` into `evaluate_and_execute`), session initializer (call site).

2. **Add a `policy` tier to the `/memory show` handler.** Mirror the existing `anvil-md` / `daily` / `nominations` branches at `handlers.rs:732-770`. The branch should render the three-section table from §4.1. Effort: ~3 hours. Files: `crates/commands/src/handlers.rs` (edit — add `"policy" => ...` arm to `match tier` at line 731), no new modules.

3. **Add `policy` to the subcommand registry.** Append `"policy"` to the `OneOf` list at `crates/commands/src/subcommands.rs:1490-1499` so completion offers it. Effort: ~5 minutes.

4. **Update `/memory` spec docs.** Add a `policy` row to the `TIERS` block at `crates/commands/src/specs.rs:262-269` and bump the example list at `:272-277`. Effort: ~5 minutes.

5. **Extend `memory_summary()` and `memory_budget()`.** Add a row that reports the count of project + global permission entries and the byte size of the on-disk files (`~/.anvil/projects/<hash>/permissions.json` and `~/.anvil/permissions.json`). Use `PermissionMemory::all_entries().count()` and the existing `dir_total_bytes` / explicit `metadata().len()` patterns at `handlers.rs:707-714` and `:920-927`. Effort: ~30 minutes.

6. **Extend `memory_inspect()` to search policy.** Walk `PermissionMemory::all_entries()` (`permission_memory.rs:188`) and emit `[policy] <scope> <tool> ...` rows when `tool_name` or `input_pattern` substring-match the key. Same for `auto_mode.hard_deny`. Effort: ~30 minutes. File: `crates/commands/src/handlers.rs:778-831`.

7. **Schema bump: add `PermissionEffect` (allow/deny/prompt).** Backward-compatible via `serde(default)`. Required to make persistent *denies* representable rather than relying on the runtime-config-resident `autoMode.hard_deny` list. Effort: ~2 hours including tests. File: `crates/runtime/src/permission_memory.rs` (edit).

8. **Surface `auto_mode.hard_deny` and reviewer/egress configs in `policy` view.** Read-only display — these stay in `settings.json`/`config.json` as their source of truth. Effort: ~2 hours total. Files: `crates/commands/src/handlers.rs` (read `AutoModeConfig`, `ReviewerConfig`, `EgressPolicy` and format).

9. **(Optional) `/memory forget --policy <tool>[:pattern]`.** Mirror existing `memory_forget` (`handlers.rs:847-897`) for removing a permission entry. Out of scope for first pass; nominate for follow-up. Effort: ~3 hours.

10. **Tests.** Add handler-level tests for the new `"policy"` branch (mirroring `memory_summary_contains_all_tiers` at `handlers.rs:1054+`). Add a runtime-level test that `evaluate_and_execute` consults `PermissionMemory` before prompting. Effort: ~4 hours.

**Rough total:** ~2.5–3 days of focused work for steps 1–8 + tests. Step 1 is the bulk; steps 2–8 are mostly mechanical exposure.

---

## 6. Risks and reversibility

| Risk | Likelihood | Blast radius | Mitigation |
|---|---|---|---|
| Step 1 changes prompt frequency. Once `PermissionMemory` is consulted, users who already clicked "always allow X" stop being prompted. Mostly a feature, but the first run after upgrade may surprise users who didn't realize they had stale grants. | Medium | Low (UX surprise) | Ship behind a one-version-cycle settings flag `permissions.use_permission_memory` defaulting on for new installs and off for upgrades. |
| Listing all grants in `/memory show policy` reveals patterns like `"bash"  "git push origin main"` that effectively disclose which branches/projects the user works on. | Low | Privacy (local-only data) | Same redaction posture as anvil-md is fine; this is a local CLI. But: when a session is shared via `/share`, the policy tier must be excluded by default. Cite `crates/runtime/src/share.rs` for the exclusion list when implementing. |
| Step 7's schema bump is on-disk. Old binaries reading new files would see unknown fields. | Low | Forward compat | `serde(default)` on `effect`; on-disk format remains a flat array of records. Old binaries silently drop the `effect` field, defaulting to `Allow` — same as today's behavior. |
| Step 2's `/memory show policy` becomes a vector for an indirect-prompt-injection attack if the agent reads its own policy and a malicious entry contains injection text. | Very low | Containment | Policy entries are user-authored and don't pass through the model unless the user explicitly types `/memory show policy`. Same risk model as `/memory show anvil-md` today. |
| Coupling `permission_gate` to `PermissionMemory` could regress on hard-deny precedence (auto-mode hard-deny must still win over a stale persistent allow). | Medium | Functional | Order check in `evaluate_and_execute` must remain: (1) auto-mode hard-deny, (2) hook decision, (3) reviewer, (4) `PermissionMemory::is_allowed`, (5) `PermissionPolicy::authorize` / prompter. Add a test that a persisted allow for `Bash` *does not* bypass `auto_mode.hard_deny`. |
| **Roll back.** | — | — | All changes except step 1 are read-only views. Reverting step 1 means swapping the gate back to its old branch on `evaluate_and_execute`; the on-disk `permissions.json` simply stops being consulted and degrades to ignored data. No on-disk data migration needed in either direction. |

---

## 7. Cross-layer dependencies

| Layer | Touchpoint | Detail |
|---|---|---|
| **L1 Working** | Permission gate runs once per tool call | `crates/runtime/src/conversation/permission_gate.rs:25-154` is exactly the point where L6 turns into a runtime decision. The working-context layer must respect L6 hard-denies even when the model "really wants to." |
| **L2 Episodic** | Daily summary should log denies | `crates/runtime/src/daily.rs` (separate file, not read here) — recommend writing a one-line `permission_denied: tool=X reason=Y` event when `PermissionDeniedPayload` fires (`permission_gate.rs:218-224`). Today only OTEL gets it (`otel::permission_decision`); episodic memory does not. |
| **L3 Semantic** | No direct overlap | Policy doesn't store *facts*; anvil-md doesn't store *grants*. Cleanly disjoint. |
| **L4 Procedural** | Skills/commands run *subject to* L6 | Loading a skill (`/skill load`) does not grant additional permissions. A skill that wants `bash` still gets gated by `PermissionPolicy + PermissionMemory`. This is the right design; document it in the synthesis. |
| **L5 Identity / secrets** | Vault unlock state — L5 or L6? | `crates/runtime/src/lib.rs:178` exposes `vault_is_session_unlocked`. Argued both ways: it's *knowledge* (L5) that the user proved possession of a passphrase, **and** it's a *decision* (L6) that secret-reads are permitted this session. Recommend: **the unlock event is L5** (it gates *knowledge access*), but **per-secret access grants are L6** (they decide which *tools* may dereference vault keys). Today the unlock state is session-only and not persisted, which means there is no L6 entry for it — that's correct and should stay. |
| **L7 Cache** | Cache decisions hinge on permission scope | `crates/runtime/src/command_cache.rs` (separate file) caches command outputs; whether a cached entry is *reusable* depends on whether the cached command was allowed by L6 at write time. Recommend: cache entries include the `PermissionScope` that was active when they were created, and `cmd-cache` invalidates global-scope entries when the global `permissions.json` changes. Out of scope for this migration but worth flagging for the L7 audit. |

---

## Appendix A — quick verification commands

```
# All references to the L6 store:
grep -rn "PermissionMemory" crates/ --include="*.rs"

# Scope enum:
sed -n '18,27p' crates/runtime/src/permission_memory.rs

# Current /memory tier branches:
sed -n '731,775p' crates/commands/src/handlers.rs

# Memory subcommand registry:
sed -n '1486,1538p' crates/commands/src/subcommands.rs

# Gate ordering:
sed -n '25,154p' crates/runtime/src/conversation/permission_gate.rs
```

## Appendix B — files-to-touch checklist for steps 1–8

- [ ] `crates/runtime/src/conversation/permission_gate.rs` — wire `PermissionMemory` query into `evaluate_and_execute`
- [ ] `crates/runtime/src/conversation/mod.rs` — thread `&mut PermissionMemory` through
- [ ] `crates/runtime/src/permission_memory.rs` — add `PermissionEffect` enum (step 7)
- [ ] `crates/commands/src/handlers.rs` — add `"policy"` arm; extend summary/budget/inspect
- [ ] `crates/commands/src/subcommands.rs` — add `"policy"` to `MEMORY_SUBCOMMANDS` `OneOf`
- [ ] `crates/commands/src/specs.rs` — add `policy` row to the `/memory` `TIERS` block (line ~262)
- [ ] Tests in `handlers.rs#memory_tests` and `permission_gate.rs#tests`