---
name: token-economy
description: Behavioral system prompt that teaches the agent to use W11 file-cache and W12 command-cache layers before doing I/O.
triggers: [token, tokens, context, economy, "save tokens", "reduce context"]
intensity_default: medium
chains_to:
  - skill: file-fingerprint
    when: always
  - skill: command-cache-aware
    when: always
  - skill: pattern-promote
    when: always
---

# token-economy

You are operating in token-economy mode. The goal is to minimize redundant
context consumption by consulting caches before performing I/O.

## Core principle

Every file read and every shell command consumes context window tokens. Many
reads and commands repeat the same information across a session. The cache
layers (W11 file-cache, W12 command-cache) exist so you pay for that information
once.

## Before reading a file

1. Check the `<known-files>` block in the current system prompt.
2. If the file appears there with a summary, answer from the summary.
3. Only fall back to a real file read on a confirmed cache miss.

Example:

```
# WRONG — burns tokens every call
read("src/policy/egress.rs")

# RIGHT — cache hit, no I/O
# system prompt contains: src/policy/egress.rs — exposes EgressPolicy + is_allowed; called by every tool that fetches
answer: "EgressPolicy is in src/policy/egress.rs; the entry point is is_allowed"
```

## Before running a read-only shell command

1. Check the command cache via `/cmd-cache get "<command>"`.
2. If a fresh result is present (within TTL), use it.
3. Run the command only on a cache miss or when freshness matters.

Cacheable: `ls`, `find`, `git log`, `git status`, `git diff`, `cat`, `grep`,
`cargo metadata`, `cargo tree`, `npm list`.

Never cached: `cargo build`, `cargo test`, `git push`, `git commit`, any command
that mutates state or whose output is time-sensitive (logs, metrics).

## After learning something durable

When you read a file and understand its role:

```
/file-cache summarize <path> "<one-line summary with key symbols>"
```

When you run a read-only command and get a stable result:

```
/cmd-cache set "<command>" "<result or key facts>"
```

## Chaining behavior

This skill auto-chains to:
- `file-fingerprint` — prompts you to fingerprint every file you read.
- `command-cache-aware` — reinforces which commands to cache and which to skip.
- `pattern-promote` — watches for repeated lookups and nominates them to durable
  memory when the threshold is crossed.

## What this skill does NOT do

- It does not prevent you from reading files. It prevents redundant reads.
- It does not auto-truncate responses. Use the `terse` skill for that.
- It does not manage the CLAUDE.md file. Use `claude-md-curator` for that.

## Session start checklist

At the start of a new session, before touching the filesystem:

1. Scan the `<known-files>` block to orient yourself.
2. Check `/cmd-cache stats` — if the cache is cold, the first few commands will
   be misses; that is expected.
3. Load `file-fingerprint` if you anticipate reading many files this session.

## Session end recommendation

Before closing the session, run `/file-cache stats` and `/cmd-cache stats` and
note any high-miss files or commands that should be pre-warmed next time.
