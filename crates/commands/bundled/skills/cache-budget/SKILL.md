---
name: cache-budget
description: Audit the file and command caches periodically; prune stale or oversized entries.
triggers: [budget, "cache size", "cache stats"]
intensity_default: low
---

# cache-budget

The W11 file-cache and W12 command-cache have finite budgets. Left unchecked,
they accumulate stale entries that waste context and mislead lookups.

## Check cache health

Run these at the start of a long session or whenever cache hits seem stale:

```
/file-cache stats
/cmd-cache stats
```

Expected output shape:

```
file-cache: 47 entries, 12 KB, oldest 3d ago, 4 stale
cmd-cache:  23 entries,  2 KB, oldest 1h ago, 0 stale
```

## Prune thresholds

Trigger a prune when:

| Condition | Action |
|---|---|
| Entry count > 1000 | `/file-cache prune --stale` |
| Entry count > 500 for cmd-cache | `/cmd-cache prune --stale` |
| Oldest entry > 7 days | `/file-cache prune --older-than 7d` |
| Oldest cmd entry > 24 hours | `/cmd-cache prune --older-than 24h` |
| You know a directory was deleted or renamed | `/file-cache prune --missing` |

## Manual eviction

When you know a specific entry is wrong (e.g., you refactored a file):

```
/file-cache evict <path>
/cmd-cache evict "<command>"
```

## Full wipe (rare)

Only when the cache is badly corrupted or you are starting a fundamentally
different task in the same session:

```
/file-cache clear
/cmd-cache clear
```

This is the nuclear option — it forces cold-start I/O for the next several
commands. Prefer targeted eviction.

## Routine cadence

- **Start of session**: run stats, prune stale if count > 100.
- **After a large refactor**: evict affected files, do not wipe.
- **End of session**: no action needed — the caches persist to the next session
  automatically.

## Reading the stats output

`stale` = entries whose source file/command has changed since the cache entry
was written. These are safe to prune; they will not give correct answers.

`missing` = entries pointing to files that no longer exist. Prune these to
reclaim budget.

`fresh` = entries within TTL with no detected source change. Keep these.
