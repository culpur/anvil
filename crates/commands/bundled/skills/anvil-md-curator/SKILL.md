---
name: anvil-md-curator
description: Know what belongs in ANVIL.md versus session state versus the nominations queue, and write entries that stay useful.
triggers: ["anvil md", "project memory", "curate", "long-term"]
intensity_default: medium
---

# anvil-md-curator

ANVIL.md is project long-term memory. Every token in it is loaded into
every session. Bloated or stale entries cost tokens; missing entries cost
repeated rediscovery.

## The three tiers of memory

| Tier | Home | Lifetime | Examples |
|---|---|---|---|
| Long-term | ANVIL.md | Permanent (until someone edits) | Architecture, invariants, deploy patterns |
| Nominated | nominations queue | Until reviewed and promoted or rejected | Candidates surfaced by `pattern-promote` |
| Ephemeral | Session context only | This session | Current task, files open, command output |

Never put ephemeral facts in ANVIL.md. Never leave durable facts only in
session context.

## What belongs in ANVIL.md

Add an entry when the fact is:

1. **Architectural** — defines how the system is structured, not what it
   contains right now. "All API responses use the `{data, pagination}` envelope"
   is architectural. "The assets table currently has 47 rows" is not.

2. **Cross-cutting** — relevant to tasks across the whole project, not to one
   file or one feature. "EgressPolicy is the entry point for all outbound calls"
   is cross-cutting. "The retry logic in egress.rs uses 3 attempts" is not.

3. **Stable** — will still be true next month unless intentional refactoring
   changes it. Invariants, naming conventions, deploy procedures.

4. **Invisible from the code alone** — things a new contributor would not find
   by reading files. "Never use rsync to deploy — git pull only" is invisible.
   "The function is called `is_allowed`" is not (just read the file).

## What does NOT belong in ANVIL.md

- Current task progress, branch names, "I'm working on X".
- File contents (use the file-cache instead).
- Command output (use the command-cache instead).
- Temporary workarounds ("this test is skipped because the CI is broken").
- Anything that will be wrong in a week.

## How to write a good ANVIL.md entry

Write it as a statement a future agent (or human) can act on directly:

```
# GOOD — actionable, stable, cross-cutting
Deploy pattern: push to main → git pull on dev0001 → npm run build → pm2 restart

# GOOD — architectural invariant
All outbound network calls go through EgressPolicy.is_allowed. Never call
external APIs directly.

# BAD — ephemeral
Currently editing src/api/assets.rs to add pagination.

# BAD — too fine-grained
src/policy/egress.rs has a function called `is_allowed` that takes a URL.
```

## Update procedure

When you learn a durable fact this session:

1. Check ANVIL.md — is it already there? Is the existing entry still accurate?
2. If missing: add a concise entry under the relevant section.
3. If stale: update the entry in place; do not leave both old and new.
4. If you are unsure whether it is durable: use `/knowledge nominate` instead
   and let a human promote it.

Do not add more than 3 new entries to ANVIL.md in a single session without
a human review. Quality over quantity.
