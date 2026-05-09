---
name: file-fingerprint
description: After reading any file, summarize it into the W11 file-cache with durable, symbol-rich one-liners.
triggers: [fingerprint, "file cache", "remember file", "file summary"]
intensity_default: low
---

# file-fingerprint

When you read a file this turn, fingerprint it immediately after you understand
its role. Fingerprints go into the W11 file-cache via:

```
/file-cache summarize <path> "<summary>"
```

## What makes a good summary

A summary should capture durable architectural facts — things that are true
session after session unless someone refactors the file. Avoid describing
transient state or implementation details that change frequently.

Good summaries name the exported symbols and say who calls them:

```
# GOOD
src/policy/egress.rs — exposes EgressPolicy + is_allowed; called by every tool that fetches

# GOOD
db/migrations/0042_add_indexes.sql — adds idx_assets_module_id, idx_assets_status; run once

# BAD — implementation detail, not identity
src/utils/retry.rs — uses async/await with exponential backoff

# BAD — too vague to be useful on a cache hit
src/main.rs — main entry point
```

## Summary format rules

- One sentence, ≤ 120 characters.
- Lead with the primary export or concept: `exposes X`, `defines Y`, `implements Z`.
- Include callers if known and stable: `called by A and B`.
- Include the module or crate if the path alone is ambiguous.
- Avoid "this file" — the path is already the key.

## When NOT to fingerprint

- Config files you modified — the summary would be stale until next read.
- Temp files, build artifacts, generated code.
- Files you read solely to answer an ephemeral question and will never consult
  again.

## Freshness

The W11 file-cache tracks a content hash. If you run
`/file-cache summarize` and the file has changed since the last fingerprint,
the cache entry is updated automatically. You do not need to manually expire
entries — but do re-read and re-summarize if you know a file has been edited
this session.

## Cascade effect

One well-placed fingerprint pays off every time another skill (or a future
session) checks `<known-files>` before doing a file read. The token savings
compound across multi-file tasks.
