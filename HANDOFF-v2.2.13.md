# Handoff: Anvil v2.2.13 Release

**Audience:** A fresh Claude Code session, no memory of the prior planning conversations.
**Working dir:** `/Users/soulofall/projects/anvil-dev`
**Identity:** You are Maverick, the team lead for Culpur Defense engineering. Lead the team — don't stop for approval on known tasks. Execute autonomously, verify your work, and report results.

---

## 1. Read these first

Read in this order, no skipping:

1. `CLAUDE.md` — global project instructions (identity, core rules, deploy patterns).
2. `ROADMAP.md` — capability backlog. **The roadmap is no longer a release schedule.** Items ship into whichever `v2.2.x` patch is next when they're ready. Version bumps require a real user-visible reason (on-disk break, CLI break, mandatory re-learning). Routines work is **Tier 2**.
3. `docs/ROUTINES-ADOPTION-NOTES.md` — the full design doc for the routines work. The body still says "v2.4" in many places because of when it was written; the calibration note at the top explains how to read those references. Treat them as **build-order anchors, not release labels**.
4. `RELEASE-NOTES-v2.2.11.md` and `RELEASE-NOTES-v2.2.12.md` (if the latter exists yet) — these set the prose style. Match it.
5. `scripts/release.sh` — the release pipeline. Don't bypass it.

Also load the user's `MEMORY.md` rules. The ones most relevant to this work:

- **Commit, push, and deploy after completing work** — never ask, just do it.
- **Release notes must be written, never auto-generated from commit subject** (`feedback-release-notes.md`).
- **On every release, update EVERY page** — both READMEs, /anvil, AnvilHub, Homebrew — with new-feature narrative (`feedback-every-surface-on-release.md`). Never report gaps as "recommendations."
- **Anvil docs/code/changelogs say "CC", never "Claude" or "Claude Code"** (`feedback-cc-only-naming.md`).
- **Historical changelog entries are byte-immutable on every release** — splice new entries on top of live README, never rewrite old ones (`feedback-changelog-preserve-historical.md`).
- **Never call in-place fixes "shipped"** — only actual releases get that language (`feedback-no-shipping-language.md`).
- **MCP hardening principles** — for any generated MCP work: honesty contract, input validation, no global replaces, no shell sed, dry-run default, no auto-generated notes (`feedback-mcp-hardening-principles.md`).

---

## 2. Where v2.2.12 stands

Check `git log --oneline` and `Cargo.toml` for current state. v2.2.12 is in flight as of 2026-05-11 with these clusters of work landed:

- **T1**: release pipeline hardening (responding to v2.2.11 incidents — empty release notes body, build.rs rerun, no global README rewrites)
- **T2**: RedrawScheduler, TUI accepts live typing while a turn is in flight
- **T3**: session friendly names, resume-by-name, `/fork` Arc-shared snapshots, exit banner
- **T4**: `/doctor release`, auto-show diff summary, interrupted-turn marking, `/clear --all`, hot-reload ANVIL.md/MEMORY.md
- **T5**: `/ssh` embedded client (A–F, in-process russh, vault HostCredential, vt100 tab UI)
- **Bug 3-C3**: per-tab parallel inference via JoinHandle spawn

**Before starting v2.2.13:** confirm v2.2.12 has been tagged and released. If it hasn't, that's your immediate predecessor work — write `RELEASE-NOTES-v2.2.12.md` (real prose, never auto-generated), match the v2.2.11 style, run the release pipeline, ship it. Do not start v2.2.13 with v2.2.12 untagged.

---

## 3. Scope for v2.2.13

The roadmap's Tier 2 routines backlog is the source list. Items in build-order:

1. `[SILENT]` marker in delivery layer + system prompt
2. Schedule grammar expansion (duration / interval / cron / ISO into one parser, normalized to `next_run`)
3. Output archive at `~/.anvil/routines/output/{id}/{ts}.md`
4. Routine packet schema (`{summary, body, input_hash}` JSON alongside `.md`)
5. TOML authoring + loader

**v2.2.13 scope: ship items 1–4. Item 5 is a stretch goal if time allows.**

Why this slice:

- Items 1–4 are independent enough to land cleanly together.
- They establish the on-disk archive shape (which everything downstream depends on) without committing to the file format for routine definitions yet.
- None of them require the `anvil routined` daemon binary, so the consent-UX work (items 20–24) stays out of scope.
- They're all small enough that one well-organized release can include them.

Each item is described in detail in `docs/ROUTINES-ADOPTION-NOTES.md` — sections 4 (`[SILENT]`), 2 (schedule grammar), 6 (output archive), 7.5 (packet schema). Read those sections in full before writing code.

**Cross-project research validation (added 2026-05-11):** `docs/research/RESEARCH-SUMMARY.md` confirms this v2.2.13 scope is correct, with these per-item references:

- **Item 1 (`[SILENT]`)** — Hermes implements at `cron/scheduler.py:129, 1741-1790`. See `docs/research/COMPARE-vs-hermes-agent.md` §11 Gap 8.
- **Item 2 (schedule grammar)** — Hermes implements at `cron/jobs.py:184-263`. See `docs/research/COMPARE-vs-hermes-agent.md` §11 Gap 1.
- **Item 3 (output archive)** — Hermes implements at `cron/jobs.py:975+`. See `docs/research/COMPARE-vs-hermes-agent.md` §11 Gap 5.
- **Item 4 (packet schema)** — context-packet implements at `src/types.ts:3-12`, hasher at `src/hasher.ts:25-28`. See `docs/research/COMPARE-vs-context-packet.md` §11 Gap 2.
- **One refinement:** fold context-packet's **anti-injection delimiters** (`src/sanitize.ts:5-11`, ~1 hour add) into item 4 work. See `docs/research/COMPARE-vs-context-packet.md` §11 Gap 3.

Two adoption candidates exist that are **not** in this v2.2.13 scope and don't need to be — they're cheap parallel adds if convenient or wait for v2.2.14:
- **`red_flags` frontmatter** (Superpowers-style anti-pattern injection) — ~4 hours, see `docs/research/COMPARE-vs-superpowers.md` §12 Gap 1.
- **"REQUIRED SUB-SKILL" prose convention** — documentation only, ~1-2 hours, see `docs/research/COMPARE-vs-superpowers.md` §12 Gap 2.

**Out of scope for v2.2.13 (explicitly):**

- The `anvil routined` daemon binary or any launchd/systemd unit work (Tier 2 items 19–24).
- The consent UX (§17 of adoption notes).
- Delivery adapters beyond `local` (items 10–11 — defer to v2.2.14).
- Pre-agent script + wake gate (item 12 — defer to v2.2.14).
- `context_from` injection (item 6 in roadmap, defer).
- Any v2.5 self-built-MCP plumbing (Tier 4).

If during the work you discover something in 1–4 depends on a deferred item, stop and reason about whether to add the dependency to this release or split it differently. Don't silently expand scope.

---

## 4. Implementation details per item

### Item 1: `[SILENT]` marker

**File touches:**
- `crates/runtime/src/cron.rs` (or wherever routine dispatch happens once items 3–4 land — see below)
- The system-prompt builder for routine runs (find via `grep -rn "system_prompt\|SYSTEM_PROMPT" crates/runtime/src/`)

**Behavior:**
- Add literal-string constant: `pub const SILENT_MARKER: &str = "[SILENT]";`
- In the delivery path, after the LLM completes and **after the archive write has completed**, check whether `output.contains(SILENT_MARKER)`. If yes, log "silent" to the run record and skip delivery. Archive still written.
- In the system prompt for routine runs, add the line: `"If there is genuinely nothing new to report, respond with exactly [SILENT] (nothing else) to suppress delivery."`
- Case-sensitive match. Do not lowercase. Do not strip.

**Tests required:**
- Unit test: output containing `[SILENT]` → delivery skipped, archive written
- Unit test: output without marker → delivery proceeds normally
- Unit test: output with `[silent]` lowercase → marker NOT triggered (case-sensitive)

### Item 2: Schedule grammar expansion

**File touches:**
- `crates/runtime/src/cron.rs:184` area (around `CronManager::create`)
- New parser function: `pub fn parse_schedule(s: &str) -> Result<Schedule, String>`

**Behavior:**

Define an enum:

```rust
pub enum Schedule {
    Cron(String),           // "0 9 * * *"
    Interval(Duration),     // "every 30m"
    OnceAfter(Duration),    // "30m"
    OnceAt(SystemTime),     // "2026-02-03T14:00:00Z"
}
```

`parse_schedule` accepts:
- `"30m"`, `"2h"`, `"1d"` → `OnceAfter`
- `"every 30m"`, `"every 2h"` → `Interval`
- `"0 9 * * *"` (5-field cron) → `Cron` (validate via existing croniter/cron dependency — find what's already in `Cargo.toml`)
- `"2026-02-03T14:00:00Z"` → `OnceAt` (parse with chrono or std `SystemTime::FromStr`)

Add field to `CronEntry`:

```rust
pub kind: ScheduleKind,  // serde-tagged enum
```

`CronManager::run_pending` already computes `next_run` — update the `next_run` computation to dispatch on `kind` instead of always treating `cron_expression` as classic cron.

**Compatibility:** Existing `cron.json` files have entries with `cron_expression` only. Add a `serde(default)` migration so old entries default to `kind: "cron"`. Do not break existing user data.

**Tests required:**
- Parser tests for all four formats (positive + a few malformed inputs)
- `next_run` computation tests for each `ScheduleKind`
- Migration test: load a v2.2.12-shaped `cron.json` and confirm it parses correctly

### Item 3: Output archive at `~/.anvil/routines/output/{id}/{ts}.md`

**File touches:**
- New module: `crates/runtime/src/routines/archive.rs` (create the `routines/` subdirectory if not present)
- Wire from `cron.rs::fire_local` (the dispatch site, around line 491)

**Behavior:**
- After every routine fire (success, failure, or `[SILENT]`), write a markdown file at `~/.anvil/routines/output/{routine_id}/{ISO-8601-timestamp}.md`.
- Use `std::fs::create_dir_all` for the per-routine directory.
- File contents:

```markdown
---
routine_name: <name>
routine_id: <id>
run_id: <uuid>
started_at: <RFC3339>
ended_at: <RFC3339>
duration_ms: <n>
status: clean | silent | failed
schedule: <display string from §2 work>
model: <model identifier>
tokens_in: <n>
tokens_out: <n>
---

## Agent Output

<the full LLM response, verbatim>

## Delivery

- local: ok
- <future adapters here>
```

- Atomic write: write to `{ts}.md.tmp` then `rename` (see `feedback-pm2-logs-ownership.md` rationale for atomic patterns).
- Directory perms: 0700 (owner-read/write/exec only). The vault module does this already — grep for the pattern.

**Tests required:**
- Archive write for a synthetic successful run
- Archive write for a `[SILENT]` run (status field reflects it)
- Archive write for a failed run (error captured)
- Path traversal: routine_id with `../` is rejected at archive write time

### Item 4: Routine packet schema

**File touches:**
- Extend the archive module from item 3
- New struct `RoutinePacket` somewhere in `crates/runtime/src/routines/`

**Behavior:**
- Alongside every `{ts}.md`, write `{ts}.json` with:

```rust
pub struct RoutinePacket {
    pub routine_id: String,
    pub run_id: String,
    pub started_at: String,       // RFC3339
    pub ended_at: String,
    pub status: PacketStatus,     // Clean | Silent | Failed
    pub summary: String,          // first paragraph of agent output, or empty for Silent/Failed
    pub body: String,             // full agent output, verbatim
    pub input_hash: String,       // SHA-256 of the inputs that produced this packet
}
```

- `input_hash` computation: SHA-256 over the concatenation of (system prompt + user prompt + script output if any + chained-from packet bodies if any). For v2.2.13 with only items 1–4 in scope, this is just `(system_prompt + user_prompt).as_bytes()`.
- `summary` extraction: first paragraph of `body`. If `body` is empty or `[SILENT]`-only, `summary` is empty.
- Same atomic-write discipline as item 3.

**Tests required:**
- Packet schema round-trips through serde JSON
- `input_hash` is stable across identical inputs
- `input_hash` differs when the system prompt changes
- `summary` extraction handles edge cases (no paragraph breaks, empty body, only-marker body)

---

## 5. Release process

Follow the pipeline. Do not bypass it.

1. **Bump version** in `Cargo.toml` workspace package: `2.2.12` → `2.2.13`. Do not edit any child crate version separately; they inherit from workspace.
2. **Write release notes** at `RELEASE-NOTES-v2.2.13.md`. Match the v2.2.11 style — opens with the headline, walks each feature in order with concrete examples, closes with a forward-look paragraph that names what's *not* in this release. Real prose. No auto-generation from commit subjects. Never use "shipped" language for things that haven't shipped yet — at write time, this release hasn't shipped.
3. **Update READMEs** per `feedback-every-surface-on-release.md`:
   - `README.md` (the local Anvil-source one)
   - The public anvil README (find via the repo layout — `feedback-anvil-repos.md` says private repo is `anvil-source`, public is `anvil`; the release pipeline handles cross-repo sync)
   - `/anvil` product page on culpur.net (HTML, wpdb PHP per `feedback-wp-page-update.md`)
   - AnvilHub page
   - Homebrew formula
   - Splice new changelog entry on top; never rewrite historical entries (per `feedback-changelog-preserve-historical.md`)
4. **Run pre-flight**: `cargo test --workspace` must pass. `cargo clippy --workspace -- -D warnings` must pass. `cargo fmt --check` must pass. There is also an `/doctor release` slash command in the in-repo Anvil binary (T4-M) — use it.
5. **Run the release pipeline**: `./scripts/release.sh`. Read `scripts/release.sh` head-to-tail before invoking — it has pre-flight gates that catch real bugs (the v2.2.11 build.rs/tag mismatch is documented in the comments).
6. **Verify the published release**: download the macOS arm64 binary from GitHub Releases, run `anvil --version`, confirm it reports `2.2.13` and the SHA matches the tag.
7. **Cross-publish**: per `CLAUDE.md` deploy pattern, the Anvil release pipeline updates 8 pages. Confirm all 8 are updated. Don't report gaps as recommendations — fix them.

---

## 6. Definition of done

All of these must be true before reporting v2.2.13 complete:

- [ ] v2.2.12 was tagged and released before v2.2.13 work started (or you wrote and shipped v2.2.12 first as part of this handoff).
- [ ] `Cargo.toml` workspace version is `2.2.13`.
- [ ] All four items (1–4) are implemented with tests, all tests pass, clippy + fmt clean.
- [ ] `RELEASE-NOTES-v2.2.13.md` exists, is real prose, matches v2.2.11 style, follows CC-only naming.
- [ ] Release-pipeline pre-flight passed.
- [ ] `git tag v2.2.13` exists, points at the right commit (verify the commit SHA matches what built into the binary).
- [ ] GitHub release published with all platform binaries.
- [ ] All 8 deploy surfaces updated with new-feature narrative.
- [ ] `anvil --version` on the released macOS binary reports `2.2.13`.
- [ ] No "shipped" language used for anything that hasn't actually shipped.

---

## 7. If you get stuck

- **Tests failing:** read the failure, fix the root cause, don't comment out. Per `feedback-never-disable-services.md` — fix root causes, don't mask.
- **Release pipeline fails pre-flight:** read what it caught. The pipeline was deliberately hardened in v2.2.12 (T1 cluster) specifically to catch the v2.2.11-class incidents. Trust the gates.
- **Scope drift:** items 5+ are not in this release. If you find yourself reaching for the TOML loader, stop and reconsider. The next session will pick that up for v2.2.14.
- **Architectural ambiguity:** the adoption-notes doc has §16 (six resolved questions) and §17 (consent design). They cover most edge cases. If the answer isn't there, write down the question and ship without the ambiguous code path — defer it to the next release.

---

## 8. Hand-back

When v2.2.13 is shipped, write a short `HANDOFF-v2.2.14.md` for the next session, modeled on this doc. Include:

- What actually landed in v2.2.13 (may differ slightly from this scope — that's fine, report reality)
- Scope for v2.2.14 per `docs/research/RESEARCH-SUMMARY.md` build order:
  - Tier 2 item 5 (TOML authoring + loader, ~1 day)
  - Tier 2 item 6 (linear `context_from` chaining — Hermes `scheduler.py:918-935` ref, ~1 day)
  - Tier 2 item 7 (three-phase token-budgeted resolver — context-packet `src/resolve.ts:16-141` ref, ~1.5 days)
  - Cheap parallel adds if not done in v2.2.13: `red_flags` frontmatter + "REQUIRED SUB-SKILL" prose convention (~5 hours combined)
- Flag for v2.2.16 horizon: **pre-dispatch reconciliation module** (Tier 2 item 15, gsd-2 ADR-017 reference). This is the load-bearing safety net for routines that have been running for weeks; do not let it slip past the routines daemon work (Tier 2 items 20-24).
- Any decisions you made during v2.2.13 that affect downstream work
- Any known issues or rough edges in v2.2.13 that need follow-up

Lead the team. Don't stop for approval on known tasks. Ship it.

---

## 9. Research artifacts (added 2026-05-11)

For deep verification of any architectural claim about routines work, six per-project comparison docs exist at `docs/research/COMPARE-vs-<project>.md` covering Archon, Hermes Agent, Superpowers, get-shit-done, gsd-2, and context-packet. The cross-project synthesis is at `docs/research/RESEARCH-SUMMARY.md`. ~2000 lines total, ~355 file:line citations. Any claim in this handoff doc can be re-verified by grepping the cited line in the relevant external project at `/Users/soulofall/projects/<project>/`.
