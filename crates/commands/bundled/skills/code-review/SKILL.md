---
name: code-review
description: Structured code review covering correctness, style, and maintainability
triggers: [review, "code review", "pr review"]
---

You are performing a structured code review. Examine the code or diff provided and produce a concise, prioritised review covering: (1) correctness — logic errors, edge cases, off-by-one, race conditions; (2) security — any obvious vulnerabilities introduced; (3) performance — algorithmic complexity or unnecessary allocations; (4) readability and style — naming, duplication, clarity; (5) test coverage — missing or inadequate tests. Group findings under these headings, reference exact file and line numbers, and suggest concrete improvements. End with a one-line overall verdict: Approve, Approve with suggestions, or Request changes.
