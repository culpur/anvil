---
name: pattern-promote
description: Detect repeated lookups and nominate them to durable memory via the nominations queue.
triggers: [pattern, repeated, promote, nominate, frequently]
intensity_default: low
---

# pattern-promote

When you notice a file, command, or snippet that you have consulted three or
more times in the same session, that information belongs in durable memory —
not in the shrinking context window.

## The threshold rule

> **3+ consultations this session → nominate it.**

"Consultation" means:
- Reading the same file more than twice.
- Running the same shell command more than twice.
- Writing or copying the same boilerplate snippet more than twice.
- Looking up the same definition, constant, or config value more than twice.

## How to nominate

Use the nominations queue (W13 skill-chaining engine, backed by
`crates/runtime/src/nominations.rs`):

```
/knowledge nominate "<fact>" [--context "<why this is durable>"]
```

Examples:

```
# File you keep re-reading
/knowledge nominate "src/policy/egress.rs exposes EgressPolicy + is_allowed; called by every tool that fetches"

# Command result that keeps being needed
/knowledge nominate "cargo workspace members: anvil-cli, commands, runtime, tui"

# Architectural constant
/knowledge nominate "All API responses wrapped in {data, pagination} envelope — see src/api/response.rs"
```

## What qualifies as durable

Nominate when the fact is:
- Architectural (will be true next week, not just today).
- Cross-cutting (relevant to multiple files or tasks, not one isolated function).
- Frequently needed (the threshold was crossed for a reason).

Do NOT nominate:
- Current task progress ("I'm editing foo.rs right now").
- Temporary debug values or log output.
- Facts about external systems that change often (API rate limits, current queue
  depth).

## Relationship to CLAUDE.md

Nominations are reviewed by the team lead before landing in CLAUDE.md or
ANVIL.md. You propose; a human confirms. This prevents drift and keeps project
memory lean.

If a fact is immediately obvious as permanent (a project-wide invariant, the
primary entry point of a binary), you may suggest adding it to CLAUDE.md
directly — but use the `claude-md-curator` skill for that path.

## Session summary

At the end of a session, do a single pass: which files did you read three or
more times? Which commands did you run three or more times? Batch-nominate them
rather than nominating one-by-one mid-task (less disruptive to flow).
