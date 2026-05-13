# MEMORY LAYER 3 — Semantic

Anvil v2.2.14. Layer audit feeding `docs/research/SEVEN-LAYER-MEMORY.md`.

---

## 1. Layer definition

L3 Semantic memory is the **durable plaintext knowledge about *this project*** —
conventions, architecture facts, deploy quirks, library preferences, glossary
entries — that survives across sessions and is safe to inject into every system
prompt. It is hand-curated by humans, AI-suggested via nominations, and indexed
by QMD (BM25 + optional embeddings over the L3 markdown corpus). The on-disk
form is YAML-frontmatter `.md` files plus an `ANVIL.md` instruction file at
project root.

**Nominations is a pipeline feeding L3, not a peer tier.** The module
docstring at `crates/runtime/src/nominations.rs:1-9` states this explicitly:

> Nominations — the knowledge discovery pipeline for Anvil's memory tiers.
> When the AI discovers patterns, conventions, or facts during a session,
> they are classified by sensitivity and routed:
>   - Credentials → auto-vault (AES-256-GCM encrypted)
>   - Infrastructure → private project memory (encrypted)
>   - Knowledge → nomination (JSON, queued for review)
> Accepted nominations are promoted to ANVIL.md or QMD for future sessions.

That is: nominations is a *staging buffer*. The `Pending`/`Accepted`/`Rejected`
state machine at `crates/runtime/src/nominations.rs:61-70` exists only to gate
entry into L3. The terminal state for an accepted nomination is an L3 file
(ANVIL.md or `~/.anvil/projects/<hash>/memory/*.md`); the JSON sidecar should
then become archival metadata, not a search target. The current `/memory`
spec treats `nominations` as one of eight equal "tiers"
(`crates/commands/src/specs.rs:266`), which is the category error this
redesign fixes.

---

## 2. Current Anvil state

### Code modules

| File:line | Role |
| --- | --- |
| `crates/runtime/src/memory.rs:10-16` | `MemoryType` enum: `User`, `Feedback`, `Project`, `Reference` — sub-classes of L3 content |
| `crates/runtime/src/memory.rs:44-51` | `MemoryFile` struct: path, name, description, type, content |
| `crates/runtime/src/memory.rs:54-74` | `MemoryManager::new` — storage path keyed by SHA-256 of canonical project root |
| `crates/runtime/src/memory.rs:93-114` | `discover()` — scan `memory_dir` for `.md` files with YAML frontmatter (skips `MEMORY.md` index) |
| `crates/runtime/src/memory.rs:118-128` | `read_index()` — read `MEMORY.md` (max 200 lines) |
| `crates/runtime/src/memory.rs:131-150` | `save()` — write frontmatter file, then rebuild `MEMORY.md` |
| `crates/runtime/src/memory.rs:170-214` | `search_relevant_memories()` — QMD BM25 search across L3, falls back to `discover()` when QMD disabled or empty |
| `crates/runtime/src/memory.rs:218-240` | `render_for_prompt()` — emits `# Persistent memory` block for system prompt |
| `crates/runtime/src/memory.rs:242-271` | `rebuild_index()` — regenerates `MEMORY.md` table |
| `crates/runtime/src/memory.rs:309-349` | `parse_memory_file()` — YAML-frontmatter parser |
| `crates/runtime/src/prompt.rs:258` | L3 injection site — `MemoryManager::new(cwd).render_for_prompt()` |
| `crates/runtime/src/prompt.rs:335-356` | `discover_instruction_files()` — walks parents for `ANVIL.md`, `ANVIL.local.md`, `.anvil/ANVIL.md`, `.anvil/instructions.md` |
| `crates/runtime/src/prompt.rs:256` | `instruction_files` injection — separate from the `MemoryManager` render at line 258 |
| `crates/runtime/src/nominations.rs:22-40` | `Nomination` schema (id, category, content, confidence, status, promoted_to) |
| `crates/runtime/src/nominations.rs:43-58` | `NominationCategory` (Pattern/Convention/Architecture/Workflow/ToolPreference/Configuration) |
| `crates/runtime/src/nominations.rs:79-95` | `NominationStore::new` at `~/.anvil/nominations/` |
| `crates/runtime/src/nominations.rs:99-135` | `create()` — write `nom-{ts}-{seq}.json` |
| `crates/runtime/src/nominations.rs:165-194` | `accept()` / `reject()` — flip status only; **does not append to ANVIL.md** |
| `crates/runtime/src/auto_promote.rs:98-116` | `AutoPromoter` struct (thresholds 3/3/2) |
| `crates/runtime/src/auto_promote.rs:146-198` | `record()` — observe file reads, command runs, fact statements |
| `crates/runtime/src/auto_promote.rs:216-229` | `emit_nomination()` — writes a `Pending` nomination at confidence 0.7 |
| `crates/runtime/src/vault/scan.rs:294-356` | `SensitivityLevel`, `classify_learning()` — credential vs infrastructure vs knowledge routing |
| `crates/runtime/src/qmd.rs:65-292` | `QmdClient` — wraps the external `qmd` CLI; BM25 search + collections |
| `crates/runtime/src/qmd.rs:212-238` | `ensure_history_indexed()` — registers `anvil-history` collection (L2/L7 concern, not L3) |
| `crates/runtime/src/qmd.rs:427-450` | `render_qmd_context()` — `<qmd-context>` block injected at `prompt.rs:264` |
| `crates/commands/src/specs.rs:241-285` | `/memory` spec — lists `anvil-md` and `nominations` as peer tiers |
| `crates/commands/src/handlers.rs:685-714` | `memory_summary()` — counts at **`~/.anvil/memory/`** (mismatched path; see §3) |
| `crates/commands/src/handlers.rs:716-776` | `memory_show()` — `anvil-md` branch at 732-739, `nominations` branch at 741-745 |
| `crates/commands/src/handlers.rs:778-831` | `memory_inspect()` — searches L3 (`MemoryManager.discover`) and nominations together |
| `crates/commands/src/handlers.rs:833-845` | `memory_promote()` — calls `store.accept(id, "ANVIL.md")` (status flip only) |
| `crates/commands/src/handlers.rs:847-897` | `memory_forget()` — tries `MemoryManager::delete`, falls back to rejecting nominations |
| `crates/commands/src/handlers.rs:916-951` | `memory_budget()` — measures `nominations`, `daily`, `goals`, `file-cache`, `cmd-cache` (lumps `anvil-md` with cwd) |

### Data layout

| Path | Format | Retention |
| --- | --- | --- |
| `<cwd>/ANVIL.md`, `<cwd>/ANVIL.local.md`, `<cwd>/.anvil/ANVIL.md`, `<cwd>/.anvil/instructions.md` | Plain markdown (walked from cwd up) | Until user deletes |
| `~/.anvil/projects/<sha256[..16]>/memory/*.md` | YAML frontmatter (`name`, `description`, `type`) + body | Until `/memory forget` or manual delete |
| `~/.anvil/projects/<sha256[..16]>/memory/MEMORY.md` | Auto-generated index table | Rebuilt on save/delete |
| `~/.anvil/nominations/nom-<ts>-<seq>.json` | `Nomination` JSON (pipeline staging — not L3 itself) | Until `/memory prune` removes decided ones |
| QMD index (`qmd` CLI side-store, location external) | BM25 / vectors | Per QMD CLI lifecycle |

---

## 3. What's missing or miscategorized

### Category errors

1. **`nominations` is a peer tier in `/memory`.** Per `specs.rs:266`, the
   user-visible tier list has `anvil-md` and `nominations` side-by-side. They
   are not peers. Nominations is a *pending* sub-view of L3 — the
   pre-promotion staging area. Today `/memory inspect deploy` returns hits
   from both surfaces with separate prefixes (`handlers.rs:797-818`),
   reinforcing the false equivalence.
2. **`ANVIL.md` (project root) and `~/.anvil/projects/<hash>/memory/*.md`
   are two parallel L3 surfaces with two parallel injection paths.** The
   root `ANVIL.md` flows through `discover_instruction_files` →
   `instruction_files` block (`prompt.rs:256`). The hashed per-project
   memory dir flows through `MemoryManager::render_for_prompt`
   (`prompt.rs:258`). They share no schema, no index, no QMD coverage path,
   and no consistent name.

### Gaps

3. **`/memory promote` doesn't actually promote.** `memory_promote`
   (`handlers.rs:833-845`) only calls `store.accept(id, "ANVIL.md")`, which
   updates the JSON status field. There is no code path that reads the
   nomination's `content` and *appends or saves* it to ANVIL.md or
   `MemoryManager`. `MemoryManager::save` is never called in production
   (verified by grep over `crates/runtime/src` and `crates/commands/src`
   non-test code).
4. **Path mismatch in `/memory` summary.** `memory_summary` counts files at
   `home.join("memory")` (`handlers.rs:688-689`), i.e.
   `~/.anvil/memory/`, but the actual `MemoryManager` writes to
   `~/.anvil/projects/<hash>/memory/` (`memory.rs:69-72`). The summary
   counter is reading a directory that the writer never populates.
5. **`MemoryType` is a meaningful sub-typing of L3 that the UI ignores.**
   `User`/`Feedback`/`Project`/`Reference` (`memory.rs:10-16`) are written
   into frontmatter, surfaced in `render_for_prompt`, but
   `/memory show anvil-md` does not group or filter by type, and `/memory
   inspect` doesn't expose it.
6. **No L3 read-back of accepted nominations.** Even if a nomination is
   `Accepted` (`promoted_to: "ANVIL.md"`), nothing in the prompt builder
   distinguishes "this came from a nomination" from raw L3 content. The
   provenance chain breaks at the status flip.
7. **QMD indexes `anvil-history` (L2/L7) but not the project memory
   directory.** `qmd.rs:212-238` only registers `anvil-history`. The
   per-project memory dir at `~/.anvil/projects/<hash>/memory/` and the
   project-root `ANVIL.md` files are not added as QMD collections, so
   `search_relevant_memories` at `memory.rs:170-214` only finds matches if
   some other process happens to have indexed those paths.
8. **`ANVIL.md` vs `MEMORY.md` split is non-obvious.** README §6 confirms
   `ANVIL.md` is the project instruction file; `MEMORY.md` (memory.rs:118,
   247, 269) is the *auto-generated index* for the hashed per-project
   memory dir. Same word, two roles.

### Things currently in L3 that arguably belong elsewhere

- The `Feedback` `MemoryType` (memory.rs:13, 22) is per-user behavioral
  preference, not project-semantic knowledge. It belongs in **L5 Identity**,
  not L3.
- The `User` `MemoryType` (memory.rs:12, 21) is similarly user-scoped, not
  project-scoped → also L5.

---

## 4. Inspector surface

What the new `/memory show semantic`, `/memory inspect`, `/memory budget` should
return for L3 — tied to current handler code.

| Command | Current behavior | Proposed L3 behavior |
| --- | --- | --- |
| `/memory show semantic` | n/a (today: `/memory show anvil-md` at `handlers.rs:732-739` dumps `MemoryManager::render_for_prompt`) | Dump (a) all project-root `ANVIL.md` files via `discover_instruction_files`, (b) all `~/.anvil/projects/<hash>/memory/*.md` via `MemoryManager::discover`, (c) accepted-but-not-yet-injected nominations, grouped by source. |
| `/memory show semantic --pending` | n/a (today: `/memory show nominations` at `handlers.rs:741-745` calls `NominationStore::format_pending`) | Sub-view of L3 — the pending nominations queue, presented as "draft L3 entries awaiting promotion", not a separate tier. |
| `/memory inspect <key>` | `handlers.rs:778-831` — scans L3 files (via `MemoryManager::discover`) and nominations (via `NominationStore::list`); two separate result groups | Single result list with provenance: `[L3 active]` / `[L3 pending]` / `[L3 project-root]`. Add QMD-ranked results from `MemoryManager::search_relevant_memories` (`memory.rs:170-214`) when QMD is enabled. |
| `/memory budget` | `handlers.rs:916-951` — `anvil-md` row uses `cwd` for path so it double-counts repo content; `nominations` listed separately | Single `semantic` row summing project-root instructions + hashed memory dir + pending nominations (with sub-breakdown). |
| `/memory why` | `handlers.rs:899-913` — describes injection order ("ANVIL.md files (project root, then ~/.anvil/memory/*.md)") — the stated path is wrong (real path is `~/.anvil/projects/<hash>/memory/`) | Should report both the project-root files actually loaded (from `discover_instruction_files`) and the hashed per-project dir, byte-counted. |

---

## 5. Migration moves

Numbered concrete moves. Each cites file(s) to add/edit and an effort estimate.

1. **Re-cast `nominations` as the `pending` sub-view of L3.** Edit
   `crates/commands/src/specs.rs:241-285` to remove `nominations` from the
   `TIERS` block; replace with a single `semantic` tier and document `/memory
   show semantic [--pending|--accepted|--all]`. Edit
   `crates/commands/src/handlers.rs:716-776` to dispatch a unified
   `memory_show_semantic(filter)` that combines `MemoryManager::discover`
   (L3 active) with `NominationStore::list(Some(Pending))` (L3 pending).
   **Effort: 4 hours.**

2. **Make `/memory promote` actually write to L3.** Edit
   `crates/commands/src/handlers.rs:833-845` to (a) load the nomination via
   `NominationStore::get`, (b) call `MemoryManager::save(name,
   description, MemoryType::Project, &nom.content)` for the project memory
   dir *or* append to project-root `ANVIL.md`, (c) then call
   `store.accept(id, <actual_target>)`. Add a `MemoryManager::append_to_anvil_md`
   helper in `crates/runtime/src/memory.rs`. **Effort: 6 hours including
   tests.**

3. **Unify the L3 storage path.** Fix `memory_summary` at
   `crates/commands/src/handlers.rs:688-689` to read from
   `MemoryManager::new(cwd).memory_dir()` instead of `home.join("memory")`.
   Fix `memory_why` at `crates/commands/src/handlers.rs:899-913` to report
   the correct path (`~/.anvil/projects/<hash>/memory/`). **Effort: 1 hour.**

4. **Add `MemoryManager::project_root_files()`** in
   `crates/runtime/src/memory.rs` that wraps `discover_instruction_files`
   (currently private in `crates/runtime/src/prompt.rs:335-356` — make
   `pub(crate)` and re-export). Use it from `/memory show semantic` so
   project-root `ANVIL.md` is visible from the inspector. **Effort: 2
   hours.**

5. **Add a `qmd collection add anvil-semantic` step** in
   `crates/runtime/src/qmd.rs` (mirror `ensure_history_indexed` at lines
   212-238). Index `~/.anvil/projects/<hash>/memory/**/*.md` and the
   project-root `ANVIL.md`. Trigger from a new `MemoryManager` hook on
   `save()`/`delete()` so QMD stays current. **Effort: 4 hours.**

6. **Route `User` and `Feedback` `MemoryType` variants to L5 Identity.**
   Edit `crates/runtime/src/memory.rs:10-16` to drop those variants (or
   mark them deprecated). Migration helper: scan existing `*.md` for
   `type: user|feedback` and move them to L5 Identity storage. **Effort:
   half a day; depends on L5 design landing first.**

7. **Add provenance metadata to promoted nominations.** Extend the
   frontmatter schema in `crates/runtime/src/memory.rs:131-150` to carry a
   `nominated_from: nom-<id>` field. Surface it in `/memory show semantic`.
   **Effort: 2 hours.**

8. **Rename the auto-generated `MEMORY.md` index** to something
   unambiguous (`INDEX.md` or `_index.md`) in `crates/runtime/src/memory.rs`
   (constants at lines 119, 247, 269). Keeps `MEMORY.md` from colliding
   conceptually with `ANVIL.md`. **Effort: 1 hour.**

9. **Stop including pending nominations in `memory_budget`.** Edit
   `crates/commands/src/handlers.rs:916-951` so pending nominations are a
   sub-line under `semantic`, not a peer entry. **Effort: 1 hour.**

Total effort estimate: **roughly 3-4 days** of focused work, dominated by
moves 2 and 5.

---

## 6. Risks and reversibility

| Risk | Probability | Mitigation |
| --- | --- | --- |
| Move 2 (real promote) destroys content if frontmatter is malformed | Medium | Wrap append in atomic write (write `.tmp`, rename). Add `--dry-run`. |
| Move 1 breaks existing `/memory show nominations` muscle memory | High | Keep `nominations` as an alias for `semantic --pending` for one minor version. |
| Move 5 (QMD collection registration) blocks if `qmd` CLI is absent | Low | `qmd.rs` already treats QMD as best-effort (`qmd.rs:65-292` — disabled state returns empty). Match that pattern. |
| Path migration (move 3) reveals that existing users have orphaned `~/.anvil/memory/*.md` | Medium | Add a one-shot migration on startup: copy `~/.anvil/memory/*.md` → `~/.anvil/projects/<current-hash>/memory/`. |
| `/memory promote` semantics change breaks a downstream script | Low | Document in release notes; keep return-text format stable. |
| Move 6 deletes user feedback notes mid-session | Medium | Migration writes to L5 first, only then removes from L3. Reversible via `/memory forget`. |

**Rollback plan:** every move is a pure code change with no schema-breaking
on-disk format change (move 7 is additive frontmatter — old files still
parse via `parse_memory_file` at `memory.rs:309`). Reverting a move means
reverting the commit; on-disk state remains compatible.

The single highest-risk dependency is the **`/memory promote` flow**: today
the nomination store is the source of truth for "what was accepted",
because the actual write never happens. After move 2, ANVIL.md becomes the
source of truth and the nomination JSON becomes audit metadata. If move 2
ships broken, accepted nominations could be lost. Land move 2 with a strong
test suite around `accept()` + `MemoryManager::save()` integration.

---

## 7. Cross-layer dependencies

| Layer | Touch point |
| --- | --- |
| **L1 Working** | System-prompt injection. L3 contents flow through `crates/runtime/src/prompt.rs:253-262` (`render_instruction_files` at 256 + `MemoryManager::render_for_prompt` at 258). Any change to L3 layout changes the prompt budget. |
| **L2 Episodic** | Daily summaries (`crates/runtime/src/daily.rs`) can feed nominations: repeated facts across daily sessions are observed by `AutoPromoter::record` with `AccessKind::FactStated` (`auto_promote.rs:181-195`) and emitted as `NominationCategory::Convention`. L2 → nominations pipeline → L3. |
| **L4 Procedural** | Skills (`anvil-md-curator`, `pattern-promote`) reference L3 — see README:101 and README:99. A migrated L3 means skill bodies that mention "ANVIL.md" must stay valid. The skill chain in README:104 is unchanged but should be re-audited. |
| **L5 Identity** | `User` and `Feedback` `MemoryType` variants (`memory.rs:10-16`) belong here, not in L3. Sensitive facts caught by `classify_learning` as `Infrastructure` (`vault/scan.rs:298-305`) route to `private_memory.rs`, not L3. The classifier is the routing point between L3 and L5/private. |
| **L6 Policy** | `content_filter.rs:96` is aware of "ANVIL.md write operations" — any policy changes to who can write L3 (e.g. require vault unlock) live there. |
| **L7 Cache** | QMD index files belong to L7 but their *content* is L3. `MemoryManager::search_relevant_memories` (`memory.rs:170-214`) and `QmdClient::search` (`qmd.rs:99-127`) are the read path from L7 into L3. Move 5 (above) makes the L3 → L7 indexing relationship explicit. |