---
name: silent-cat
description: When asked what is in a file, check the known-files cache first; only read the file on a confirmed cache miss.
triggers: [cat, "show file", "what's in", "view file", "look at file"]
intensity_default: low
chains_to:
  - skill: file-fingerprint
    when: always
---

# silent-cat

When the user asks "what's in foo.json", "show me config.ts", or "look at the
Cargo.toml", do not immediately issue a file read. Check the cache first.

## Decision tree

```
User: "what's in <file>?"
  │
  ├─ Is <file> listed in the <known-files> system-prompt block?
  │    YES → answer from the summary. Done. No file read.
  │    NO  → cache miss. Proceed to step 2.
  │
  ├─ Run /file-cache get <file>
  │    HIT → answer from the cached entry. Done. No file read.
  │    MISS → proceed to step 3.
  │
  └─ Read the file. After reading, run file-fingerprint to cache it.
       /file-cache summarize <file> "<summary>"
```

## Answering from a cache hit

When the cache has the file, answer the question using the summary — do not
prefix with "according to the cache" or "the cache says". Just answer.

If the user's question requires detail the summary does not capture (e.g. they
want the full contents, not just the purpose), say so briefly and do the read:

```
The cache summary for auth.ts covers its exports but not the full token format
you asked about. Reading it now.
```

## Examples

### Cache hit — no read needed

```
User: what's in src/policy/egress.rs?

<known-files> contains:
  src/policy/egress.rs — exposes EgressPolicy + is_allowed; called by every tool that fetches

Response: src/policy/egress.rs exposes EgressPolicy with a single public method
is_allowed, which every outbound tool calls before making a network request.
```

### Cache miss — read + fingerprint

```
User: what's in db/schema.prisma?

<known-files>: not present
/file-cache get db/schema.prisma: MISS

→ Read the file.
→ After reading: /file-cache summarize db/schema.prisma "defines Asset, Module, User, AuditLog models; Asset has soft-delete deletedAt"
```

### Detail beyond summary

```
User: show me the full contents of .env.example

Summary in cache: lists required env vars for DB + auth.
User asked for full contents → cache is insufficient → read the file, do not fingerprint .env files.
```

## What counts as a "file inspection" prompt

Trigger this skill on prompts that match:
- "what's in X" / "what does X contain"
- "show me X" / "show file X"
- "look at X" / "view X"
- "cat X" / "print X"
- "open X" (when context is file inspection, not editor launch)

Do NOT trigger on "read X and then do Y" — that is a task prompt where the read
is a step, not the goal. In that case, check the cache as part of the task but
do not surface the cache-check to the user.
