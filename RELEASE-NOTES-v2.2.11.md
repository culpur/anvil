# Anvil v2.2.11 — Token economy, skill chaining, memory tier inspector

Released: 2026-05-09

The biggest Anvil release since v2.0. v2.2.11 adds a full token-economy
layer (file fingerprint cache, command output cache, skill chaining,
seven default cache-aware skills), a brand-new `/memory` command tree
that surfaces every memory tier the agent can read or write, an
auto-promoter that watches for repeated lookups and suggests promoting
them to durable memory, and ten workstreams of correctness work
covering hooks, effort, goals, profiles, schema, telemetry, output
styles, the reviewer-agent gate, MCP server mode, and admin policy
floors.

This is a single-binary upgrade — no config migration, no new
dependencies your machine doesn't already have. Update via
`anvil upgrade` or `brew reinstall anvil`.

---

## Headline: token economy

Most agent sessions read the same five files four times, run
`git status` six times, and re-explain the same project layout to the
model on every turn. v2.2.11 stops paying for that.

### File-fingerprint cache (`/memory show file-cache`)

Every file the agent reads is fingerprinted (sha256 + mtime + size +
first 200 lines + detected language) and stored under
`~/.anvil/projects/<project-hash>/file-cache/`. A `<known-files>` block
is built from the cache and injected into the system prompt, so the
model knows what files exist and their summaries before it ever calls
`read_file`. Cache invalidates automatically when mtime or sha256
changes; concurrent writers are safe (atomic rename + per-call unique
tmp filenames after a race fix in this release).

Tests: 18 unit tests. Race fix verified 5/5 under concurrent stress.

### Command-output cache (`/memory show cmd-cache`)

`bash` calls go through a TTL-bounded cache keyed on `(command, cwd)`.
A static `is_cacheable` allowlist gates which commands cache at all
(read-only `git status`, `ls`, `cat`, `pwd`, etc. — never `rm`,
`apt-get`, `curl`, anything network). Cached entries record their
`touched_files`; when any of those files change, the entry is invalidated
on lookup. Hot-path detection counts session invocations so frequently
re-run commands surface in `/memory budget`.

Tests: 27 unit tests. Wired into `crates/runtime/src/bash.rs` via the
single canonical `is_cacheable` entry point.

### Skill chaining (`chains_to:` frontmatter)

Skills can now declare follow-up skills in their frontmatter:

```yaml
---
name: code-review
triggers: [review, "code review", "pr review"]
chains_to:
  - skill: security-audit
    when: { mentions: ["auth", "crypto", "permission"] }
  - skill: terse
    when: { user_pattern: "^/quick" }
---
```

The `ChainEvaluator` walks the chain graph at turn-start, suggesting
(never auto-running) up to depth 3 and 25KB of additional skill text.
Cycle detection via HashSet, byte budget enforcement, suggest-not-auto
pattern preserved end-to-end.

The audit caught and fixed a real bug here mid-release: parsed
`chains_to:` was being discarded at both filesystem-load call sites in
`agents.rs` (`fm.chains_to` → `vec![]`). Skill chaining was effectively
dead for every non-bundled skill until v2.2.11. It works now.

Tests: ChainEvaluator + ChainEntry + cycle + depth + budget cases.

### Seven default token-economy skills

Bundled at compile time (`include_str!`), discoverable via `/skill load`
without disk I/O:

| Skill | What it teaches |
|-------|-----------------|
| `token-economy` | Meta-skill, chains to the three cache skills |
| `file-fingerprint` | Fingerprint files into the W11 cache after reading |
| `command-cache-aware` | Check the W12 cache before `ls`/`git`/`cat`/`grep` |
| `pattern-promote` | Nominate repeated lookups to durable memory |
| `cache-budget` | Audit and prune file/command caches periodically |
| `anvil-md-curator` | Rules for what belongs in ANVIL.md vs ephemeral |
| `silent-cat` | Answer "what's in X" from cache before reading the file |

Bundle count is now 10 (previously 3: `code-review`, `security-audit`,
`terse`). Each W14 skill is ≤200 lines, has a valid frontmatter
(name + description + ≥1 trigger), and `token-economy`'s chain
references all resolve to real bundled skills. Regression-guarded.

Tests: 10 new tests covering presence, frontmatter validity, chain
resolution, count.

---

## Headline: `/memory` command tree

A new top-level slash command surfaces every memory tier the agent can
read or write:

```
/memory                   summary across all tiers
/memory show <tier>       dump one tier
/memory inspect <key>     search every tier for a key
/memory promote <nom-id>  accept a pending nomination → ANVIL.md
/memory forget <key>      remove an entry / reject a nomination
/memory why               explain what's in the system prompt right now
/memory budget            byte + estimated-token usage per tier
/memory prune             evict stale entries
```

Tiers covered: `anvil-md`, `vault`, `private`, `nominations`, `daily`,
`file-cache`, `cmd-cache`, `goals`. Tab-completion is grammar-driven
(`/memory show <Tab>` enumerates the 8 valid tier names). Detailed help
is built into `/help memory`.

The implementation is **pure formatting and dispatch** on top of
existing runtime APIs — no new storage, no new indices, no new
privileged code. If a tier already had a public reader, `/memory show
<tier>` calls it; if not, it explains why.

Tests: 14 new memory handler tests covering parse, dispatch, unknown
subcommand fallback, empty-arg vs missing-arg distinction.

---

## Headline: AutoPromoter (suggest-not-auto)

`crates/runtime/src/auto_promote.rs` adds a session-scoped engine that
counts file reads, command runs, and stated facts. When a threshold is
crossed (5 reads of the same file, 3 runs of the same command, 2
re-statements of the same fact), it emits a nomination JSON file under
`~/.anvil/nominations/` for the user to review via `/memory promote`.

**Pure suggest-not-auto.** Nothing is automatically written to
ANVIL.md, the vault, or any persistent store. The user is the only
source of promotion authority.

Path canonicalization (`/var` → `/private/var` on macOS), whitespace
normalization (`git    status` ≡ `git status`), and per-session reset
semantics are unit-tested.

Tests: 9 unit tests covering threshold logic, normalisation, reset,
stats accuracy.

---

## v2.2.11 correctness work (W1–W10)

Ten workstreams of catch-up correctness work landed alongside the
headline features. None are flashy individually; together they close
the door on a class of subtle bugs and parity gaps.

| W | What |
|---|------|
| W1 | Hook events catch-up — SessionStart/End, FileChanged, CwdChanged, PermissionRequest/Denied, PostToolBatch, Notification all reach hook handlers; missing tests added |
| W2 | Effort/reasoning slider — per-provider mapping (Anthropic thinking budget, OpenAI reasoning.effort, Gemini thinkingBudget) with `max_effort` interaction |
| W3 | Goal persistence — file-backed JSON at `~/.anvil/goals/<project-hash>/<id>.json` with atomic writes (after the first attempt shipped a stub by accident, redone in this release) |
| W4 | Named profiles — `[profiles.<name>]` config sections with active-profile selection |
| W5 | Published JSON Schema — `anvil --emit-schema` outputs draft 2020-12, all settings keys covered |
| W6 | OpenTelemetry events — opt-in, redact-by-default, feature-flagged, `permission_decision` event now wired (was orphaned in v2.2.10) |
| W7 | Custom output styles — `~/.anvil/output-styles/<name>.md` discovered and selectable via `/output-style` |
| W8 | Reviewer-agent approval gate — deterministic regex scanner (not an LLM call) for high-risk operations |
| W9 | `anvil mcp-server` mode — Codex parity, MCP stdio transport, exposed every Anvil tool over MCP |
| W10 | `requirements.toml` — admin policy floor parsed at startup; `bypassPermissions` string now parses to `DangerFullAccess` |

Plus three follow-up correctness commits caught by the round-2 audit:
W3 stub-replace, W1 dead-wired hooks, W6 permission_decision wiring + W5
schema tightening.

---

## Audit & cleanup work

A deep audit pass after the W1–W10 merge uncovered (and fixed):

* The `chains_to` propagation bug above (real correctness issue, not cosmetic)
* A W11 atomic-write race exposed by the new concurrent-store test
  (`subsec_nanos` collisions between threads on a fast machine —
  fixed with a process-local `AtomicU64` counter in tmp filenames)
* Three cosmetic dupes: a redundant `CommandCacheManager::is_cacheable`
  delegate, a dangling W13 doc-comment fragment, audit reports
  noting "Phase 4 doesn't exist" (it does, the auditor was wrong; left
  as-is)

---

## Test coverage

| Crate | Tests | Δ from v2.2.10 |
|-------|-------|----------------|
| anvil-cli | 225 | +18 |
| commands | 111 | +24 (W14: 10, W15: 14) |
| runtime | 506 | +27 (W11: 18, W12: 27 net of cleanup) + 9 W15 = 506 |
| api | 108 | unchanged |
| (others) | 127 | unchanged |
| **Total workspace** | **1077** | **+67** |

All green on `cargo test --workspace --release -- --test-threads=1`. The
serial-test verification caught the W11 atomic-write race that parallel
runs masked.

---

## Surfaces touched

* `runtime/src/{file_cache,command_cache,auto_promote,goals,requirements,...}.rs` — major
* `runtime/src/{hooks,otel,permissions/reviewer,effort,...}.rs` — wiring
* `commands/src/{skill_chaining,handlers,agents,specs,subcommands}.rs` — major
* `commands/bundled/skills/*` — 7 new skills
* `anvil-cli/src/{mcp_server_mode,mcp_server_tools}.rs` — new
* `runtime/assets/config-schema.json` — `requirements`, `effort`, `profiles`, `outputStyles`, `otel` keys

---

## Upgrade

```bash
anvil upgrade
# or
brew reinstall anvil
```

No config migration required. The token-economy caches are created on
first use. Existing ANVIL.md, vault, and goals files are read as-is.
The seven new bundled skills are available immediately via
`/skill load token-economy` (or any of the others).

If you want the agent to start using the cache layers right away,
`/skill load token-economy` chains in `file-fingerprint`,
`command-cache-aware`, and `pattern-promote`. From that point the
session burns fewer tokens on repeat lookups.

---

## What's next

`/memory` plus AutoPromoter plus skill chaining is a foundation, not a
finish line. v2.3 is scoped around session persistence + reconnect
(picking sessions back up after sleep/crash/transit), hosted Ollama via
Passage, and the cloud-run sessions that were deferred from v2.2.11.
None of that is in this release.
