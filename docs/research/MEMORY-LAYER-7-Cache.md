# MEMORY LAYER 7 — Cache (derived)

Anvil version under audit: **v2.2.14** (HEAD of `/home/user/repo`).
Layer role in the seven-layer model: **L7 — derived / recomputable performance state.**

---

## 1. Layer definition

L7 is the *derived* layer: every byte in L7 can be regenerated from L1–L6 plus
the filesystem. Its defining property is **nukability without information
loss** — wipe the whole layer and the agent simply rebuilds it on the next
turn at a temporary performance cost. This contrasts L3 Semantic (durable
ground truth that humans curate) and L5 Identity (sensitive ground truth in
the vault); both of those would be permanently lost if deleted. L7 contents
exist solely to skip recomputation: a sha256 you already calculated, a `git
log` you already ran, an embedding you already indexed. Anything that *only*
caches a derivation belongs here; anything that records a decision or a fact
the system could not reconstruct does not.

Today three sub-stores satisfy this property in Anvil:

1. **File-fingerprint cache** — sha256 + size + mtime + summary + symbols per
   file (`file_cache.rs`). Derivable from disk.
2. **Command-output cache** — stdout/stderr/exit for read-only commands
   (`command_cache.rs`). Derivable by re-running the command.
3. **QMD on-disk index files** — BM25/vector index over `~/.anvil/history/`
   and (when configured) other Anvil-managed markdown corpora. Owned by the
   external `qmd` binary; derivable by re-indexing the source corpus.

The L7 invariant: **no L7 byte may contain unencrypted L5 (vault) content.**
Caches that key off paths inside the vault, or hash a vault file's contents,
violate the layer's "safe to nuke and re-derive" guarantee in the wrong
direction — they leak ground truth that L5 is supposed to keep encrypted at
rest.

---

## 2. Current Anvil state

### Inventory

| Sub-tier        | Owner module | On-disk path | Format | Eviction |
|---|---|---|---|---|
| file-cache | `crates/runtime/src/file_cache.rs:121` (`FileCacheManager`) | `~/.anvil/projects/<project-hash>/file-cache/<sha-prefix2>/<sha256>.json` (`file_cache.rs:5`, `file_cache.rs:131-145`) | One JSON file per entry, sharded by 2-char sha prefix | Lazy: stale entries auto-pruned on `lookup` (`file_cache.rs:7-8, 155-200`); manual `prune()` rehashes every entry against disk (`file_cache.rs:324-380`). **No TTL, no size cap, no LRU.** |
| cmd-cache | `crates/runtime/src/command_cache.rs:562` (`CommandCacheManager`) | `~/.anvil/projects/<project-hash>/cmd-cache/<command-hash>.json` (`command_cache.rs:9, 578-582`) | One JSON file per entry; key = first 16 hex of `sha256(command + cwd)` (`command_cache.rs:11, 532-541`) | Per-entry TTL (60–1800s, varies by command class — `command_cache.rs:422-453`) AND mtime-watch on inferred `touched_files` (`command_cache.rs:14-17, 609-630`). `prune_stale()` evicts expired entries (`command_cache.rs:737-758`). **No size cap, no LRU.** |
| qmd-history-index | external `qmd` binary, configured by `crates/runtime/src/qmd.rs:212` (`QmdClient::ensure_history_indexed`) | **Outside `~/.anvil/`** — owned by the `qmd` CLI's own data dir (test fixture at `qmd.rs:517` shows `Index: /tmp/index.sqlite`). Anvil only knows the source corpus: `~/.anvil/history/` (`history.rs:31, 34, 53-54`). | SQLite index produced by `qmd update` (`qmd.rs:234-235`). Anvil never reads it directly. | None visible to Anvil; the QMD CLI manages its own index lifecycle. `qmd update` is triggered on every `ensure_history_indexed` call (anvil-cli/src/main.rs:3947, 8149, 8202). |

### Code shape

- `FileCacheEntry`: `file_cache.rs:84-107` (path, sha256, size, mtime,
  last_seen, line_count, language, summary, key_symbols, access_count).
- `FileCacheStats`: `file_cache.rs:111-114` (entry_count, total_bytes_cached).
- `FileCacheError`: `file_cache.rs:50-54` (`Io`, `Json`).
- `CommandCacheEntry`: `command_cache.rs:31-52` (command, cwd, stdout,
  stderr, exit_code, duration_ms, captured_at, touched_files,
  stale_after_secs, hits).
- `CommandCacheStats`: `command_cache.rs:56-61` (total_entries, stale_entries,
  total_hits, total_size_bytes).
- `CommandCacheError`: `command_cache.rs:65-70` (`Io`, `Json`, `DirCreate`).
- `QmdClient`: `qmd.rs:65-292`, with in-process `Mutex<HashMap<String,
  Vec<QmdResult>>>` query cache at `qmd.rs:68` (per-session, **also L7** —
  derived from the on-disk index).

### Callers (proof L7 is actually exercised)

- `prompt.rs:224-233` builds the `<known-files>` system-prompt block from
  `FileCacheManager::list()` via `build_known_files_block`
  (`file_cache.rs:480`).
- `bash.rs:177, 252` consults `CommandCacheManager` on every read-only bash
  invocation.
- `anvil-cli/src/main.rs:3944-3947, 4634, 4638, 8149, 8202` plumbs QMD
  search+index into the per-turn system reminder.

### Discrepancy worth recording

`memory_summary` in `handlers.rs:707-710` counts files at
`~/.anvil/file-cache/` and `~/.anvil/cmd-cache/`. The real on-disk paths are
`~/.anvil/projects/<project-hash>/file-cache/...` and
`~/.anvil/projects/<project-hash>/cmd-cache/...`. The current
`/memory` count is therefore **always zero in normal operation** — a latent
bug exposed by this audit. Same issue in `memory_budget` (`handlers.rs:925-926`).

`/file-cache` handler is **a stub** (`handlers.rs:556-559`): it merely prints
usage text and never instantiates `FileCacheManager`. Only `/cmd-cache`
(`handlers.rs:560-617`) actually wires through to its manager.

---

## 3. What's missing or miscategorized

| Issue | Evidence | Severity |
|---|---|---|
| **file-cache and cmd-cache are sibling top-level `/memory` tiers** rather than two sub-stores of a single L7 cache tier. | `specs.rs:267-268`, `handlers.rs:723`, `handlers.rs:773`. They're listed peer-to-peer with `anvil-md`, `vault`, `daily`, etc. — a category error: those peers are *durable*, these two are *derived*. | High (the redesign premise) |
| **QMD on-disk index is not surfaced as a `/memory` tier at all**, despite being L7 derived state Anvil triggers (`qmd.rs:212-238`). | Absent from `specs.rs:262-269` tier list and from `memory_summary`/`memory_budget` (`handlers.rs:685-714, 916-952`). | Medium |
| **In-process QMD query cache** (`qmd.rs:68`) is L7 but invisible — there is no `invalidate_cache` exposure under `/memory`, only an internal call at `qmd.rs:242`. | Manual code inspection. | Low (rebuilds in seconds) |
| **`/memory prune` ignores L7 entirely** — it only prunes `daily/` and decided nominations (`handlers.rs:954-966`). | Direct read of handler. | High (no unified prune means users must memorise `/cmd-cache prune` + a not-yet-implemented `/file-cache prune`). |
| **`/file-cache` slash command is a stub** — never calls `FileCacheManager` (`handlers.rs:556-559`). | Direct read of handler. | High (documented in `specs.rs:267` and `handlers.rs:769` as a working command). |
| **`memory_summary` counts wrong paths.** `~/.anvil/file-cache` and `~/.anvil/cmd-cache` don't exist — the real layout is `~/.anvil/projects/<hash>/...`. | `handlers.rs:707-710` vs `file_cache.rs:136-140` and `command_cache.rs:578-581`. | High (cosmetic but misleading) |
| **L5 invariant unenforced.** Nothing prevents a future cmd-cache entry from storing decrypted vault output. `is_cacheable` (`command_cache.rs:125-272`) gates on syntactic shell features, not on whether the command reads `~/.anvil/vault.bin` or a decrypted vault path. | No enforcement code located. | High (design gap, not a current bug) |
| **No size cap or LRU on either cache.** `file_cache.rs` has byte-budgets only for the *system-prompt block* (`MAX_PROMPT_BYTES = 4 KB`, `MAX_PROMPT_ENTRIES = 50` at `file_cache.rs:41-44`), not for on-disk growth. `command_cache.rs` has TTL only. | Direct read. | Medium (unbounded growth in long-lived projects) |
| **`/memory inspect` does not search L7.** It only looks at `anvil-md` and `nominations` (`handlers.rs:778-831`). Finding a cached file or a cached command requires the dedicated commands. | Direct read. | Medium |

---

## 4. Inspector surface

| Verb | Today | Proposed unified behaviour |
|---|---|---|
| `/memory show cache` | Says `"File-cache details are managed via /file-cache list."` and a sibling line for cmd-cache (`handlers.rs:769-770`). | Print a one-screen table: per sub-tier (file / cmd / qmd-index) → entries, bytes, hit-count, stale-count, last-touched. Delegate sub-tier deep dives back to `/file-cache list` and `/cmd-cache list`. |
| `/memory show cache file` | n/a | Equivalent to today's `/file-cache list` (currently a stub at `handlers.rs:556-559`, so this is also the implementation hook). |
| `/memory show cache cmd` | n/a | Equivalent to today's `/cmd-cache list` (`handlers.rs:565-581`). |
| `/memory show cache qmd` | n/a | Run `qmd status --json` via `QmdClient::status` (`qmd.rs:133-158`), print `total_docs/total_vectors/size_mb`. |
| `/memory inspect <key>` | Skips L7 (`handlers.rs:778-831`). | After scanning durable tiers, also scan: file-cache by path substring; cmd-cache by command substring; QMD by `QmdClient::search` (`qmd.rs:99`). Print as `[file-cache] /path/to/file.rs sha:abc123 …` etc. |
| `/memory budget` | Counts wrong dirs (`handlers.rs:925-926`). | Use the *actual* manager APIs: `FileCacheManager::stats()` (`file_cache.rs:313-320`), `CommandCacheManager::stats()` (`command_cache.rs:715-735`), `QmdClient::status()`. |
| `/memory prune` | No L7 pruning (`handlers.rs:954-966`). | Run, in order: `FileCacheManager::prune()` (`file_cache.rs:324-380`), `CommandCacheManager::prune_stale()` (`command_cache.rs:737-758`), `qmd update` (which Anvil already invokes via `ensure_history_indexed` at `qmd.rs:234-235`). Print N pruned per sub-tier. |
| `/memory prune cache` | n/a | Same as above but skips daily/nominations. |
| `/file-cache *`, `/cmd-cache *` | Today: stub + working manager (`handlers.rs:556, 560`). | Keep as **backwards-compatible aliases**. New `/file-cache list` should be implemented (it isn't today) by routing to the same code as `/memory show cache file`. |

Design constraint to preserve: the dedicated `/file-cache` and `/cmd-cache`
sub-command shapes (`list|stats|prune|forget <key>`) are stable surface area
and must continue to work unchanged.

---

## 5. Migration moves

Numbered, in dependency order. Each item names file(s) to touch and an
effort estimate.

1. **Fix the `/file-cache` stub.** Replace `handlers.rs:556-559` with a real
   match that mirrors the `CmdCache` handler (`handlers.rs:560-617`),
   delegating to `FileCacheManager::list`/`stats`/`prune`/`forget`
   (`file_cache.rs:286, 313, 324, 277`). **Effort: 2–3 h.** Unblocks
   everything else.

2. **Fix the `memory_summary` / `memory_budget` paths.** Edit
   `handlers.rs:707-710` and `handlers.rs:920-927` to compute the project
   hash via `runtime::memory::project_path_hash` and read
   `~/.anvil/projects/<hash>/file-cache/` and `~/.anvil/projects/<hash>/cmd-cache/`.
   Even simpler: call `FileCacheManager::stats()` and
   `CommandCacheManager::stats()` directly. **Effort: 1 h.**

3. **Introduce a unified `cache` tier.** In `handlers.rs`, add a `memory_show`
   arm `"cache" => …` *above* the existing `"file-cache"` and `"cmd-cache"`
   arms (`handlers.rs:769-770`); add a parallel arm in `specs.rs:261-269`
   tier list and `handlers.rs:723` / `handlers.rs:773` usage strings. Three
   sub-cases: `cache`, `cache file`, `cache cmd`, `cache qmd`. **Effort: 3 h.**

4. **Implement `/memory prune cache`.** Add a helper `prune_cache_tier()` next
   to `prune_old_files` (`handlers.rs:999-1023`) that sequentially calls
   `FileCacheManager::prune()`, `CommandCacheManager::prune_stale()`, and
   (if `QmdClient::is_enabled`) re-invokes `ensure_history_indexed` to nudge
   `qmd update`. Wire into the `memory_prune` dispatch at `handlers.rs:954`,
   and into `memory_show`'s subcommand router (so `/memory prune cache`
   parses correctly via `MEMORY_SUBCOMMANDS` in
   `crates/commands/src/subcommands.rs`). **Effort: 4 h.**

5. **Extend `/memory inspect` to L7.** In `memory_inspect`
   (`handlers.rs:778-831`), after the existing nominations loop, scan
   `FileCacheManager::list()` by path substring and
   `CommandCacheManager::list()` (`command_cache.rs:696`) by command
   substring. Optionally probe `QmdClient::search` (`qmd.rs:99`) when QMD is
   enabled. **Effort: 3 h.**

6. **Keep dedicated commands as aliases.** No code move required — leave
   `SlashCommand::FileCache` and `SlashCommand::CmdCache`
   (`crates/commands/src/lib.rs:465, 885`) intact. Add a doc line to
   `specs.rs` for `/file-cache` and `/cmd-cache` pointing users to `/memory
   show cache`. **Effort: 30 min.**

7. **Surface QMD as a first-class L7 sub-tier.** Add `qmd-index` to the tier
   list in `specs.rs:262-269` and to the usage string at `handlers.rs:723,
   773`. In `memory_show`, gate on `QmdClient::is_enabled`
   (`qmd.rs:90-92`) and print "QMD not installed (optional enhancement)" if
   absent. **Effort: 2 h.**

8. **Encode the L5 invariant.** Add a documented sentinel function
   `runtime::cache::is_l5_path(p: &Path) -> bool` next to `file_cache.rs`
   that returns true for `~/.anvil/vault.bin`, `~/.anvil/private/*.enc`, and
   any path under a configured vault-mount. Gate
   `FileCacheManager::store`/`lookup` and
   `CommandCacheManager::store`/`lookup` on this check. Add a debug-assert
   that stored entries don't include vault-derived `touched_files`.
   **Effort: 1 day.** (Higher because it needs test coverage and a fuzz
   pass on shell command parsing.)

9. **Add a size cap / LRU.** Introduce `ANVIL_FILE_CACHE_MAX_MB` and
   `ANVIL_CMD_CACHE_MAX_MB` env vars (default e.g. 50 MB each); when
   `stats()` exceeds the cap, evict by `last_seen` / `captured_at`. Touch
   `file_cache.rs:313-320` and `command_cache.rs:715-735` + a new
   `evict_lru` method on each manager. **Effort: 1 day.**

**Total: ≈3.5 days for items 1–7 (functional unification); +2 days for items
8–9 (invariant + LRU).** Items 1–2 alone are worth shipping as a v2.2.15
fast-follow because they fix user-visible bugs.

---

## 6. Risks and reversibility

| Risk | Likelihood | Blast radius | Reversal |
|---|---|---|---|
| `/memory prune cache` deletes an entry the agent re-needs same-turn. | Medium (warm caches help mid-task). | Slow turn while caches rebuild — no data loss. | None needed; next bash/file lookup re-fills. |
| Unified handler breaks `/file-cache` / `/cmd-cache` for scripts that scrape exact output. | Low (no documented stable-format contract). | External tooling parsing slash-command output breaks. | Keep aliases (move 6) byte-identical. |
| L5 invariant check (move 8) wrongly excludes a legitimate non-vault file because it lives under `~/.anvil/`. | Low. | Cache miss on that file → slower, but correct. | Adjust the `is_l5_path` predicate; pure cache miss has no permanent effect. |
| LRU eviction during a long session evicts an entry the next prompt-builder pass wants for the `<known-files>` block. | Medium. | Block shrinks by one entry — no semantic change. | Increase cap; cache rebuilds. |
| `memory_summary` path fix (move 2) suddenly reports much higher cache sizes than users are used to (because counts were 0 before). | High. | User confusion. | Document in release notes; no rollback needed. |
| `qmd update` invoked from `/memory prune cache` blocks for seconds on a large history corpus. | Medium. | UI freeze. | Run async, like the existing `ensure_history_indexed` callers (`anvil-cli/src/main.rs:3947`). |

Overall: **L7 is the safest layer to migrate.** Everything is derivable; the
worst outcome of a wrong move is a slow turn while caches re-warm. The only
high-severity risk is the L5 invariant — which is *strengthened* by this
migration, not weakened.

---

## 7. Cross-layer dependencies

| Layer | Where this migration touches it |
|---|---|
| **L1 Working memory** | Cache hits short-circuit bash and file-read steps that would otherwise occur during turn execution. The known-files block (`file_cache.rs:480, prompt.rs:233`) is L7-derived text injected into the L1 working context. Migration items 4 and 5 must not block during turn execution. |
| **L2 Episodic** | QMD indexes `~/.anvil/history/` (`qmd.rs:212, history.rs:31`) — the L7 `qmd-index` is *derived from* L2. Item 7 must treat QMD as ephemeral re-derivation, never as authoritative. |
| **L3 Semantic** | Anvil could (and likely should) register `anvil-md` as a second QMD collection alongside `anvil-history`. Currently only `anvil-history` is registered (`qmd.rs:230`). When the L3 audit doc proposes this, the L7 sub-tier `qmd-index` must add a second sub-corpus and a second `qmd update` trigger. |
| **L4 Procedural** | `cmd-cache` memoises the procedural layer's outputs (which shell commands have been run). `is_cacheable` (`command_cache.rs:125-272`) is effectively a *procedural policy* table — it lives here in L7 but should be reviewed by the L4 audit so the policy isn't duplicated. |
| **L5 Identity** | **Hard invariant: L7 MUST NOT contain decrypted L5 contents.** Move 8 implements this. The current code does *not* enforce it (no grep hit for vault-path exclusion in `file_cache.rs` or `command_cache.rs`). The L5 audit doc should flag this as a cross-layer follow-up. |
| **L6 Policy** | A cmd-cache hit must respect the *current* permission scope, not the scope that was in effect when the entry was stored. Today's lookup at `command_cache.rs:593-620` does not consult `runtime::permissions`. The L6 audit should propose adding a permission re-check on hit so a tightened policy isn't bypassed by a stale-but-valid cache entry. |

---

## Appendix: files cited

- `crates/commands/src/specs.rs:241-285`
- `crates/commands/src/handlers.rs:540-617, 648-1023` (especially 556, 707-714, 769-770, 916-966)
- `crates/commands/src/lib.rs:71, 465, 885, 2039, 2160`
- `crates/runtime/src/file_cache.rs:1-380, 449-516, 480` and constants at 32-44
- `crates/runtime/src/command_cache.rs:1-272, 422-453, 560-758`
- `crates/runtime/src/qmd.rs:65-292, 395-449`
- `crates/runtime/src/history.rs:31-54`
- `crates/runtime/src/memory.rs:276` (`project_path_hash`)
- `crates/runtime/src/config/mod.rs:617` (`default_config_home`)
- `crates/runtime/src/prompt.rs:11, 196, 224-264`
- `crates/runtime/src/bash.rs:177, 252`
- `crates/anvil-cli/src/main.rs:3874, 3944-3947, 4634, 4638, 8149, 8202`
- `crates/anvil-cli/src/commands_extra.rs:150`