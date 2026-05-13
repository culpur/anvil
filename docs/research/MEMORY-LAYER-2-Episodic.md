# MEMORY LAYER 2 — Episodic

**Audit target:** Anvil v2.2.14 (HEAD of `/home/user/repo`)
**Layer function:** Per-session events — what happened, what was decided, what was deferred. Replayable.
**Currently mapped from:** `daily` tier (`~/.anvil/daily/YYYY-MM-DD.json`) + `~/.anvil/history/` conversation archive + per-workspace `.anvil/sessions/<id>.json` transcripts.

---

## 1. Layer definition

L2 Episodic memory records **what happened in past sessions**: which user requests came in, what tasks were completed, what tasks were deferred, which files were touched, how many tokens were spent, and the verbatim conversation transcript for replay or search. It is distinct from L1 (the live working buffer for the current turn), from L3 (consolidated semantic facts like ANVIL.md), and from L4 (procedural skills). The shape is *event-stream / replay-log*, not normalized knowledge — episodes are appended once, never edited, and either summarized into L3 (via nominations) or pruned. Anvil today splits this layer across three independently designed stores that do not know about each other.

---

## 2. Current Anvil state

### Stores composing L2 today

| Concern | Type | On-disk path | Format | Writer | Reader | Retention |
|---|---|---|---|---|---|---|
| Per-day rollup of completed sessions (with task extraction + cost) | `DailySummary` containing `Vec<SessionSummary>` | `~/.anvil/daily/YYYY-MM-DD.json` | JSON, pretty-printed, one file per UTC calendar day | `DailyStore::record_session` called at session exit | `/daily`, `/memory show daily`, `memory_why()` (claims an injection that doesn't actually happen) | `/memory prune` removes files older than **30 days** |
| Full conversation archive (markdown, with YAML front-matter for QMD) | `ArchiveEntry` per file | `~/.anvil/history/<session-id>-<unix-ts>.md` | Markdown + YAML front-matter | `HistoryArchiver::archive_session` on `/compact` and on auto-compact | `/history-archive` list/search/view (search uses QMD `anvil-history` collection) | **None** — files accumulate indefinitely |
| Per-workspace live session transcript (resume target for `--continue`) | `Session` JSON | `<cwd>/.anvil/sessions/<session-id>.json` | JSON | `LiveCli::persist_session` after every turn | `--resume`, `--continue`, `/sessions` | **None** — workspace-local, gitignored |

### Source citations

| Code surface | Path | Lines |
|---|---|---|
| `SessionSummary`, `DailySummary` types | `crates/runtime/src/daily.rs` | 18–51 |
| `DailyStore::new` / `with_dir` | `crates/runtime/src/daily.rs` | 60–71 |
| `DailyStore::record_session` (writes + reconciles) | `crates/runtime/src/daily.rs` | 94–110 |
| `DailyStore::reconcile` (task carry-forward) | `crates/runtime/src/daily.rs` | 185–206 |
| `DailyStore::recent` (last N days) | `crates/runtime/src/daily.rs` | 210–238 |
| `extract_tasks` (verb-based task detection) | `crates/runtime/src/daily.rs` | 276–315 |
| `TASK_VERBS` / `COMPLETION_INDICATORS` heuristics | `crates/runtime/src/daily.rs` | 250–267 |
| `HistoryArchiver` struct + `archive_session` | `crates/runtime/src/history.rs` | 33–82 |
| `HistoryArchiver::list_archives` | `crates/runtime/src/history.rs` | 86–105 |
| `MIN_ARCHIVE_MESSAGES = 10` threshold | `crates/runtime/src/history.rs` | 12 |
| Markdown archive builder w/ YAML front-matter | `crates/runtime/src/history.rs` | 129–193 |
| Auto-compact threshold env override (`ANVIL_COMPACT_THRESHOLD`) | `crates/runtime/src/history.rs` | 14–19, 110–116 |
| QMD `ensure_history_indexed` (registers `anvil-history` collection) | `crates/runtime/src/qmd.rs` | 207–238 |
| `LiveCli::record_daily` (session-exit hook) | `crates/anvil-cli/src/main.rs` | 7504–7585 |
| `LiveCli::compact` (archives, then compacts, then re-indexes) | `crates/anvil-cli/src/main.rs` | 8121–8153 |
| `LiveCli::maybe_auto_compact` (archives at threshold) | `crates/anvil-cli/src/main.rs` | 8160–8202 |
| `collect_modified_files` (heuristic file-path scrape from tool results) | `crates/anvil-cli/src/main.rs` | 1474–1504 |
| `/history-archive` handler (list/search/view/stats) | `crates/anvil-cli/src/commands_extra.rs` | 110–206 |
| `/daily` handler | `crates/anvil-cli/src/commands_extra.rs` | 276+ |
| `sessions_dir()` (per-workspace `.anvil/sessions/`) | `crates/anvil-cli/src/session.rs` | 29–36 |
| `.anvil/sessions/` gitignore | `crates/anvil-cli/src/init.rs` | 12 |
| `/memory show daily` branch | `crates/commands/src/handlers.rs` | 746–754 |
| `/memory` summary counts daily files | `crates/commands/src/handlers.rs` | 692–693 |
| `/memory budget` includes daily | `crates/commands/src/handlers.rs` | 923 |
| `/memory prune` 30-day cutoff for daily | `crates/commands/src/handlers.rs` | 957–960, 999–1023 |
| `/memory why` claim of daily task injection (item 6) | `crates/commands/src/handlers.rs` | 908 |
| `CompactionConfig::preserve_recent_messages = 6` (L1 boundary) | `crates/runtime/src/compact.rs` | 11, 18 |

---

## 3. What's missing or miscategorized

### Gaps (things L2 should own but currently doesn't)

1. **`~/.anvil/history/` is invisible to `/memory`.** `memory_summary` (`handlers.rs:685-714`), `memory_show` (`handlers.rs:716-776`), `memory_budget` (`handlers.rs:916-952`), and `memory_prune` (`handlers.rs:954-966`) never touch `home.join("history")`. Users see archived transcripts only via the separate `/history-archive` command. The seven-layer story has no single answer to "show me episodic memory."
2. **`.anvil/sessions/<id>.json` is also invisible to `/memory`.** Live transcripts are managed by `/sessions` and `--resume` only; there is no episodic accounting of them.
3. **No retention on `~/.anvil/history/`.** `HistoryArchiver` writes forever; `memory_prune` skips the history dir entirely (`handlers.rs:954-966`). On a busy machine this is the largest disk consumer of any memory tier.
4. **The "Daily task reconciliation fragment" promised by `/memory why` (handlers.rs:908) is not actually injected.** `crates/runtime/src/prompt.rs` only calls `MemoryManager::new(&cwd).render_for_prompt()` (line 258) — there is **no** `DailyStore::today()`/`reconcile()` call in the prompt build path. The system-prompt-injection contract is silently broken; this is the single highest-value bug in the layer.
5. **No cross-store linking.** A `SessionSummary` (daily) has a `session_id`, and an archive file is named `<session_id>-<ts>.md`, and a workspace transcript is `<session_id>.json` — but no helper resolves a session_id to its three artifacts. `/memory inspect` cannot search any of them by session_id.
6. **No event-level granularity.** Episodic memory is currently rolled up at *session-exit* only. Mid-session events (a tool call, a permission decision, a goal advance) are not captured as discrete episodes; they exist only embedded in the live `Session` JSON and disappear at compact.
7. **`SessionSummary.credentials_auto_vaulted` is hardcoded to `0`** (main.rs:7563) — the field exists but no producer fills it; L6 Policy is not feeding L2.
8. **`/memory inspect <key>` does not search daily or history** (`handlers.rs:778-831`) — it only searches anvil-md and nominations. Episodic content is unreachable by key lookup.

### Miscategorizations (things in this layer that arguably belong elsewhere)

| Currently in L2 | Arguably belongs in | Reason |
|---|---|---|
| `extract_tasks` heuristic in `daily.rs:276-315` | L4 Procedural or its own "task extractor" module | This is a content-mining transform, not episodic storage. The verb table couples L2 to a specific NLP heuristic. |
| `collect_modified_files` in `main.rs:1474-1504` | L7 Cache / file-cache | File-modification tracking is what `file-cache` (W11) already does; duplicating heuristic scrapes here is brittle. |
| Auto-compact threshold (`HistoryArchiver::compact_threshold_pct`, `history.rs:110-116`) | L1 Working / `CompactionConfig` | This is a working-buffer policy that happens to live on the archiver because the archiver is the thing that fires at the threshold. |
| Per-workspace `.anvil/sessions/` (resume target) | Arguably its own "transient L1 spillover" — it is a live cursor, not an episode | Today it is treated as session state; once a session exits, it's stale but never deleted. |

---

## 4. Inspector surface

What the unified `/memory` command **should** return for L2 after the migration, and what it returns today.

### `/memory show episodic`

Today: **does not exist** — closest is `/memory show daily` (handlers.rs:746-754), which shows only the JSON rollup for today's calendar day. Nothing surfaces history archives or workspace transcripts.

Proposed contents:

```
Episodic memory (L2)

Daily rollups (~/.anvil/daily/)
  2026-05-12.json   3 sessions  12,400 tok  $0.04   2 open items
  2026-05-11.json   1 session   ...
  (showing 7 most recent of 14)

Session archives (~/.anvil/history/)
  session-1747000000-1747010000.md   42 msgs  claude-sonnet-4-6  2h ago
  ...
  (12 archives, 4.2 MB; QMD-indexed as 'anvil-history')

Workspace transcripts (./.anvil/sessions/)
  session-1747010000.json   18 msgs  active
  session-1746500000.json   62 msgs  3d ago
  ...
```

### `/memory inspect <key>`

Today: searches only anvil-md + nominations (handlers.rs:792-817). Episodic content is unreachable.

Proposed: also grep daily `tasks_completed` / `tasks_open` / `files_modified` strings, and grep archive markdown bodies (QMD-backed when available, plain substring fallback). Same handler, three more `for` loops over `DailyStore::recent(N)` and `HistoryArchiver::list_archives()`.

### `/memory budget`

Today (handlers.rs:920-927): daily is one of six tiers tracked; history is **omitted**. Daily disk usage is reported but history disk usage (typically the largest) is not.

Proposed: a single `episodic` row that sums daily + history + workspace-sessions byte counts. Token count is approximate (history bodies are full transcripts, not consumed at prompt time — they are search-indexed only). Suggest splitting the row into `episodic.daily`, `episodic.history`, `episodic.sessions` for actionability.

### `/memory prune` (episodic)

Today (handlers.rs:954-966): only daily JSON > 30d is pruned; history and workspace-sessions are never touched. After migration: identical cutoff (default 30d) plus a separate history retention (default 90d for archive markdown; `ANVIL_HISTORY_RETENTION_DAYS` env override). Workspace `.anvil/sessions/` should be cleaned when the .json mtime is older than 14d AND there is a matching archived file.

---

## 5. Migration moves

1. **Expose history as a first-class episodic tier in `/memory`.** Edit `memory_summary` (`crates/commands/src/handlers.rs:685-714`) to count `home.join("history")` markdown files; edit `memory_show` (`crates/commands/src/handlers.rs:716-776`) to add an `"episodic"` branch that calls `runtime::HistoryArchiver::new().list_archives()` and `runtime::DailyStore::new().recent(7)`; edit `memory_budget` (`crates/commands/src/handlers.rs:916-952`) to include `home.join("history")`. Edit the `OneOf` arg list in `crates/commands/src/subcommands.rs:1490-1499` to add `"episodic"` (and keep `"daily"` as an alias for back-compat). **~4 hours.**

2. **Fix the broken `/memory why` claim** — either inject open items into the system prompt or remove the lie. The honest move is to add a `DailyStore::today()` + `reconcile()` call to `SystemPromptBuilder::build` in `crates/runtime/src/prompt.rs` around the existing `MemoryManager::render_for_prompt()` call (line 258), gated behind a config flag so it can be turned off cheaply. Then update `/memory why` text at `crates/commands/src/handlers.rs:899-913` to match reality. **~3 hours including a regression test in `prompt.rs`'s tests module.**

3. **Wire `/memory inspect` to episodic stores.** Extend `memory_inspect` (`crates/commands/src/handlers.rs:778-831`) with two more search loops: (a) over `DailyStore::recent(30)` searching `tasks_completed`, `tasks_open`, `files_modified` for the key; (b) over `HistoryArchiver::list_archives()` calling `fs::read_to_string` on each path and doing a case-insensitive substring match, with a hard cap (e.g. 50 archives) and a per-file size cap (e.g. 1 MB) to bound latency. Prefer QMD path when `qmd.is_enabled()`. **~6 hours.**

4. **Add history retention to `/memory prune`.** Edit `memory_prune` (`crates/commands/src/handlers.rs:954-966`) to also call `prune_old_files(&home.join("history"), 90)` (the existing helper at `handlers.rs:999-1023` already handles arbitrary dirs; today it skips files whose extension is not `.json` — line 1013 — so generalize it to take an extension parameter or add a sibling helper for `.md`). Add `ANVIL_HISTORY_RETENTION_DAYS` env override mirroring `ANVIL_COMPACT_THRESHOLD` style at `crates/runtime/src/history.rs:14-19`. **~2 hours.**

5. **Cross-store session_id resolver.** Add `fn resolve_episode(session_id: &str) -> Episode` (new module `crates/runtime/src/episode.rs`) that bundles `Option<SessionSummary>` from `DailyStore::recent(30)`, `Option<ArchiveEntry>` from `HistoryArchiver::list_archives()`, and `Option<PathBuf>` from the workspace `.anvil/sessions/<id>.json`. Re-export from `crates/runtime/src/lib.rs:158` alongside the existing history exports. Plumb through to `/memory inspect`. **~1 day.**

6. **Move `extract_tasks` out of `daily.rs`** into a sibling `crates/runtime/src/task_extract.rs` and have `DailyStore::record_session` import from there. Pure refactor — the function is already pure and tested at `daily.rs:619-667`. This separates the L2 storage concern from the L4-adjacent NLP heuristic. **~1 hour.**

7. **(Optional, larger) Event-level episodic log.** Add `crates/runtime/src/episode_log.rs` that appends `EpisodeEvent` records (tool-call, permission-decision, goal-advance, file-write) to `~/.anvil/history/events-YYYY-MM.jsonl`. Hooked from existing emitters: `runtime::otel::session_end` (`main.rs:7545`), `permission_memory.rs`, `goals.rs`. This is the real fix for L2's current rollup-only granularity; defer if not feeding L7 cache. **~3-5 days.**

8. **Delete or rename `/history-archive`** once `/memory show episodic` is canonical. The double-surface (one command for episodic, another for the same data) is the root cause of `~/.anvil/history/` being invisible to `/memory`. Leave a deprecation alias in `crates/commands/src/specs.rs:768-782` for one release. **~1 hour.**

**Total core work (items 1–6): ~3 days. With item 7: ~1 week.**

---

## 6. Risks and reversibility

| Move | Risk | Rollback |
|---|---|---|
| 1 (expose history in `/memory`) | Larger byte counts in `/memory budget` may alarm users used to today's understated total. Mitigate: split the row so users see the breakdown. | Pure additive edit to `handlers.rs`; revert the file. |
| 2 (inject open-items into prompt) | Token-budget regression — adds bytes to every system prompt. If `open_items` is large (long pause between sessions), this is unbounded. Mitigate: cap to top-3 by recency, hard-cap byte length, gate behind `ANVIL_INJECT_DAILY_RECONCILIATION=1` for two releases before defaulting on. | Revert `prompt.rs` edit; the rest is text-only. |
| 3 (`/memory inspect` over history) | I/O cost on machines with many archives. Mitigate: bounded archive count + size cap as described. | Revert handler; no on-disk change. |
| 4 (history retention) | **Data loss.** If retention default is wrong, irreplaceable transcripts vanish. Mitigate: default 90 days (vs daily's 30); add `--dry-run` flag to `/memory prune`; require the env var to be set before *any* history pruning in the first release. | Restoring from disk is impossible — but the pruner only deletes `.md` files; trash-bin pattern (move to `~/.anvil/.trash/` first) is a one-line change. |
| 5 (episode resolver) | Pure-additive new module; risk is wasted effort if downstream consumers (web UI, MCP) don't materialize. | Delete the module. |
| 6 (move `extract_tasks`) | Public API churn: `crates/runtime/src/lib.rs:158` re-exports `extract_tasks`. Keep the re-export pointing at the new location. | Trivial revert. |
| 7 (event log) | Disk growth, IPC contention if writers fight for the same JSONL file. Mitigate: month-rotated files (path includes `YYYY-MM`), one writer per session, best-effort fsync. | Delete the writer; existing rollup path is unaffected. |
| 8 (rename `/history-archive`) | UI/script breakage for anyone calling `/history-archive` from automation. Mitigate: alias for two releases, then a hard error with a pointer. | Restore the spec entry. |

**General reversibility:** every move above is local to a small set of files. The only **irreversible** move is item 4 (deletion); guard it with a trash-bin + dry-run flag before first release.

---

## 7. Cross-layer dependencies

| Other layer | Touch point | What L2 migration changes for them |
|---|---|---|
| **L1 Working** | `CompactionConfig::preserve_recent_messages = 6` at `crates/runtime/src/compact.rs:11,18`; compaction in `LiveCli::compact` (`main.rs:8121-8153`) is the *transition* from L1 → L2 (archive happens *before* messages are discarded). | The auto-compact threshold currently lives on `HistoryArchiver` (`history.rs:110-116`) — moving it to `CompactionConfig` (move 7's optional sibling) clarifies which layer owns the policy. The MIN_ARCHIVE_MESSAGES=10 floor (`history.rs:12`) is an L2 admission policy that filters what L1 hands over. |
| **L3 Semantic** | Nominations (`crates/runtime/src/nominations.rs`) are the L2→L3 promotion path: a session generates a nomination (`SessionSummary.nominations_generated` at `daily.rs:48`); `/memory promote` (handlers.rs:833-845) consolidates an episode into ANVIL.md. | Move 5 (cross-store resolver) lets nominations link back to their originating episode. Move 2 (injecting reconciliation) potentially competes with ANVIL.md for prompt budget — coordinate caps. |
| **L4 Procedural** | `goals.rs` and `routines/` are referenced by `SessionSummary` only implicitly (tasks_completed/open). Move 6 separates the task-extraction NLP from L2 storage so L4 can reuse it. | If move 7 lands, goal-advance events feed L2's event log; L4 doesn't need its own log. |
| **L5 Identity** | `vault.bin` is excluded from `/memory budget` (handlers.rs:949). `SessionSummary.credentials_auto_vaulted` (`daily.rs:49`) is the documented L5→L2 signal; today it is always 0 (main.rs:7563). | Wiring this field is part of L5's audit, not L2's, but L2 must keep accepting and displaying the count. |
| **L6 Policy** | `permission_memory.rs` decisions are episodic by nature ("user allowed bash:* on 2026-05-10"). Today they live in their own store and are *not* surfaced in `/daily` or `/history-archive`. | Move 7's event log is the natural home for permission decisions; until then L6 and L2 remain disjoint. |
| **L7 Cache** | `QmdClient::ensure_history_indexed` (`qmd.rs:207-238`) registers the `anvil-history` collection — QMD **is** L7 over L2. `/history-archive search` (`commands_extra.rs:142-166`) is the user-facing L7 query over episodic data. | Move 4 (history retention) must trigger `qmd.invalidate_cache()` / `qmd update` after pruning to keep the index consistent. Move 1 should drop the duplicate `/history-archive search` surface in favor of `/memory inspect`, which goes through L7 the same way. |

---

*End of L2 audit. Sibling docs: L1 Working, L3 Semantic, L4 Procedural, L5 Identity, L6 Policy, L7 Cache. Synthesis at `docs/research/SEVEN-LAYER-MEMORY.md`.*