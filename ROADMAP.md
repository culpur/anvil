# Anvil — Capability Backlog & Ecosystem Plan

**Last reframed:** 2026-05-11
**Prior approval:** 2026-04-20 (eleven-release sequencing, retired)

---

## How we plan releases now

The prior version of this doc treated each capability as a release. Eleven of them, v2.3 through v3.0. That framing was wrong for how this code actually moves.

Look at what's actually shipped:

- **v2.2.11** bundled token economy + skill chaining + memory + seven default skills in one release.
- **v2.2.12** is bundling `/ssh` + per-tab parallel inference + release pipeline hardening + RedrawScheduler + live-typing during turns + session friendly names + resume-by-name + `/fork` snapshots + hot-reload ANVIL.md + `/doctor release` + auto-show diff summary + interrupted-turn marking + `/clear --all`.

That's how patch releases work here: whatever is ready ships in the next cut. The release isn't a scope budget. It's a shipping moment.

**This doc is a backlog, not a schedule.** Items are ordered by *build dependency* (what unlocks what), not by version label. Version labels bump for reasons users can't ignore — on-disk format changes, CLI breaks, license shifts, architectural seams that require re-onboarding. Not feature accumulation.

The honest test for a minor bump:

1. Does on-disk state change in a way old Anvil can't read?
2. Does the CLI surface break compatibility?
3. Does a user *have* to learn something new to keep using Anvil the way they used it yesterday?

If all three are "no," it's a patch. Routines, self-building MCPs, marketplace work, multi-repo, team mode, privacy layer, eval harness, context workbench, IDE integration — by these rules, most of them are patch-shaped work that ships across many `v2.2.x` cuts.

A v3.0 only makes sense when there's an actual breaking change or a deliberate marketing moment. Not "we've accumulated enough features."

---

## Capability backlog

Ordered by build dependency. Items higher up unlock items lower down. Anything within a section can ship in any order, in any combination, in whatever `v2.2.x` is next.

### Tier 1 — Foundation already underway

Things in the tree today, partial or shippable, that other backlog items depend on.

| Capability | Surface in tree today |
|---|---|
| Cron grammar + daemon + LLM tool surface | `crates/runtime/src/cron.rs`, `crates/tools/src/automation_ops.rs` |
| Daily report generation | `crates/runtime/src/daily.rs` |
| Hooks system | `crates/runtime/src/hooks.rs` |
| Goals / nominations pipeline | `crates/runtime/src/goals.rs` |
| Worktree ops | `crates/tools/src/worktree_ops.rs` |
| Skills with `chains_to:` depth-3 traversal | `crates/commands/src/skill_chaining.rs` |
| AnvilHub install plumbing | `crates/runtime/src/hub.rs` |
| Vault (flat-label, all-or-nothing unlock) | `crates/runtime/src/vault/storage.rs` |
| File + command caches | `crates/runtime/src/{file_cache,command_cache}.rs` |
| Session friendly names + resume-by-name + `/fork` snapshots | landed in v2.2.12 |
| Per-tab parallel inference | landed in v2.2.12 (Bug 3-C3) |

### Tier 2 — Proactive execution (the routines backlog)

Slice-by-slice ordering from `docs/ROUTINES-ADOPTION-NOTES.md`. Each item ships when ready into the next `v2.2.x`. No item is "v2.4" — that label is retired.

**Cross-project research validation (2026-05-11):** `docs/research/RESEARCH-SUMMARY.md` synthesizes six deep audits comparing Anvil v2.2.12 against Archon, Hermes Agent, Superpowers, get-shit-done, gsd-2, and context-packet (`docs/research/COMPARE-vs-<project>.md`, ~2000 lines, ~355 file:line citations). The synthesis confirms the Tier 2 numbered ordering below and identifies one missing item: **verify-evidence DB + exponential-backoff retries** (gsd-2 reference) — slot in as a new item ~30 after the journal lands.

Build-order roughly smallest first, with dependencies noted:

1. `[SILENT]` marker in delivery layer + system prompt
2. Schedule grammar expansion (duration / interval / cron / ISO into one parser, normalized to `next_run`)
3. Output archive at `~/.anvil/routines/output/{id}/{ts}.md`
4. Routine packet schema (`{summary, body, input_hash}` JSON alongside `.md`)
5. TOML authoring + loader (file format for routine definitions; also extends skill frontmatter)
6. `context_from` linear chaining (pull last packet of N upstream routines into prompt)
7. Token-budgeted three-phase resolver (summaries → bodies → truncate-from-most-distant)
8. Anti-injection delimiters around injected packets
9. Idempotency-hash skip (same `input_hash` → reuse prior packet)
10. Delivery layer adapter trait + `local` / `email` / `webhook` adapters + target grammar
11. Per-adapter truncation policy + `archive_url` in every truncated payload
12. Pre-agent script + wake gate (`{"wakeAgent": false}` on last stdout line skips LLM)
13. Vault-ref resolution (`vault:<label>` flat-prefix, resolves via existing `VaultManager`)
14. Vault-ref linter (regex catches raw token shapes in TOMLs)
15. Pre-dispatch reconciliation module (vault refs, script path, sources, locks, archive)
16. `red_flags` block in skill / routine frontmatter (anti-pattern table injected into system prompt)
17. Imperative "REQUIRED SUB-SKILL" prose support (complement to declarative `chains_to`)
18. Temporary JSONL run log at `~/.anvil/routines/runs/{YYYY-MM}.jsonl`
19. Tick file lock at `~/.anvil/cron/.tick.lock`
20. `anvil routined` daemon binary (the existing `cron_daemon_loop` foreground, with logging + pid file)
21. launchd plist generator + systemd-user unit generator + install/uninstall subcommands
22. Consent UX (`anvil routine install-daemon` prompt — see §17 in adoption notes for full wording)
23. `enabled: false` default for new routine TOMLs + `anvil routine enable / disable`
24. `anvil routine status` rich output (daemon state, per-routine fire counts, blocked reasons)
25. Reference routines: `daily-release-check`, `silent-changelog-watcher`, `pattern-analyzer-stub`, `morning-summary`
26. Watch trigger (filesystem `notify` crate)
27. Webhook trigger (inbound HMAC endpoint, secret from vault)
28. Event trigger (`session.end`, `git.push`, `goal.nominated` from existing hooks)
29. Skill-as-routine bridge (skill loader detects `[routine]` frontmatter)

**The daemon work (20–24) is the most user-visible item in this tier.** It changes when Anvil can act on the user's behalf — from "while interactive" to "always, after explicit opt-in." It's still patch-shaped: opt-in, additive, reversible, zero on-disk format change for users who don't install it.

### Tier 3 — Durable session journal

Single durable journal that captures every tool invocation + args + timing + result. Originally roadmapped as "v2.3 Session Persistence." It's a real piece of work but it doesn't have to land as a single release — the journal can phase in:

1. JSONL append-only log (already in Tier 2 item 18 as a temporary form)
2. Per-session journal file with structured schema
3. Migration of cron/routines/daily outputs into the journal
4. Resume-by-name + `/fork` rebased on journal state (partial: friendly names already in v2.2.12)
5. Cross-session query surface (the v2.11 "Context Workbench" idea — but as journal-query commands, not a separate workbench)

### Tier 4 — Self-improving tools

Originally roadmapped as "v2.5 Self-Building MCP Servers." Stays as the most strategically valuable capability — *"the AI assistant that writes its own tools"* — and still depends on Tiers 2 and 3 being in place. Pipeline stays the same; the slicing changes:

1. Pattern analyzer ships as a routine (Tier 2 item 25 `pattern-analyzer-stub` is its scaffold)
2. New `mcp_opportunity` category in `goals.rs` nominations pipeline
3. MCP generator from Node templates, vault-scoped, sandbox + tool allowlist + resource budget
4. Test runner + shadow-run validation
5. Lifecycle commands: `/mcp self list | pause | disable | delete | diff`
6. Opt-in publish to AnvilHub review queue
7. Rust templates (later)
8. LLM-assisted pattern detection (later)

Each numbered item can ship as a patch when it's done. The full capability isn't gated on a release label.

**The six MCP hardening rules** from `feedback-mcp-hardening-principles.md` are non-negotiable for every generated MCP: honesty contract, input validation, no global replaces, no shell sed, dry-run default, no auto-generated notes.

### Tier 5 — Distribution and marketplace

Originally roadmapped as "v2.6 Agent Marketplace & Composition."

1. AnvilHub manifest extension for routine TOMLs (today it installs skills; extend the same `hub.rs` install path)
2. AnvilHub manifest extension for self-built MCPs
3. Agent publish flow on AnvilHub
4. Per-agent / per-MCP vault scope grants (depends on vault gaining a scope concept — currently flat)
5. Marketplace review queue (already part of Tier 4 item 6)

### Tier 6 — Workspace expansion

Originally roadmapped as "v2.7 Multi-Repo Workspaces." Breaks single-repo assumptions in the session layer, but doesn't have to land as one release:

1. Session model accepts multiple workspace roots
2. Tool calls scope cleanly across workspaces
3. Routines can span multiple repos
4. Worktree management per workspace

### Tier 7 — Collaboration

Originally roadmapped as "v2.8 Team Mode." Most of this is delivery-surface and access-grant work:

1. Telegram delivery adapter
2. Slack delivery adapter
3. GitHub-comment delivery adapter
4. Per-user vault access grants (depends on vault scopes)
5. `/share` evolution into multi-user shared sessions (relay already in tree from v2.2.x)

### Tier 8 — Enterprise trust surface

Originally split across "v2.9 Local Model Privacy Layer," "v2.10 Evaluation Harness," "v2.11 Context Engineering Workbench," "v2.12 Native IDE Integration." All independent capabilities that can ship as patches:

- Privacy layer extends the vault scanner; ship one detector at a time
- Evaluation harness — 5-provider foundation is already in the tree; harness commands ship one at a time
- Context workbench — fold into Tier 3 journal query surface; no separate "workbench" needed
- IDE integration — LSP crate exists today (`crates/lsp/`); integration with VS Code / JetBrains / Zed ships per-IDE

### Tier 8.5 — Platform reach (cross-compile + remote-host parity)

Anvil ships as a single binary on every platform we claim to support. v2.2.12 shipped 4 of 5 advertised targets because Windows cross-compile broke on the new SSH agent code (`russh::AgentClient::connect_uds()` does not exist on Windows — no Unix-domain sockets). BSD was never on the matrix at all, which is a separate gap that needs to land before we make claims about "every platform, no install prereqs."

This tier is the platform-reach correction. Two work streams, neither of which justifies a version bump on its own; both fold into the next `v2.2.x` cuts.

#### Stream A — Windows SSH parity (v2.2.13 commitment)

The Windows binary missing from v2.2.12 is the immediate gap. Tracked as task #441. Three things have to happen, in order:

1. **`#[cfg(unix)]` gate on the SSH agent auth path.** `crates/runtime/src/ssh/driver.rs:141-186` (the `auth_agent` function) and its single call site at `:128` get a `cfg(unix)` attribute. The `SshAuthMethod::Agent` variant stays in the public enum on Windows — only the auth attempt is suppressed, with a `SshError::Auth("SSH agent auth is not supported on Windows (no Unix-domain sockets in russh::keys::agent::client)")` returned when invoked. The auth chain falls through to `KeyFile` → `Password` → `KeyboardInteractive` exactly as it does on Unix when the agent isn't present.

   This is the minimum-viable Windows fix. It does not give Windows users ssh-agent support; it stops the cross-compile failure that prevented any Windows binary from shipping in v2.2.12.

2. **Resolve the 5 `E0282` type-inference errors in `crates/runtime/src/ssh/driver.rs`.** These are pre-existing in the v2.2.12 SSH code and only surface on Windows because the Windows target inference path is stricter than Unix in the russh trait surface. Add explicit type annotations at each site; do not change runtime behavior. Identified during the v2.2.12 release pipeline; details in `/tmp/anvil-win-build.log` from that session and in the task #441 description.

3. **Honest Windows ssh-agent support (deferred follow-up, not a v2.2.13 blocker).** Once #1 and #2 unblock the Windows build, the real Windows ssh-agent story is named-pipe-based — `\\.\pipe\openssh-ssh-agent` on Win32-OpenSSH installs, or Pageant's window-message protocol for PuTTY. Neither lives in russh today. The right move is a follow-up that wires `russh-keys` agent client to a Windows named-pipe transport, gated `#[cfg(windows)]`. Files a separate task when v2.2.13 ships; not a hard blocker for v2.2.13 because Windows users can still auth via `KeyFile` (which is what most do anyway).

4. **`scripts/release.sh` tail-truncation bug.** Independent of the SSH work but caught during the same v2.2.12 cycle: every cross-compile invocation in `release.sh` ends with `2>&1 | tail -1`, which swallows compiler errors AND replaces the cargo exit code with `tail`'s exit code (always 0). The Windows build failed compilation but the docker wrapper reported exit 0 and the `cp` step on the next line silently copied a stale artifact from a prior build. Fix: drop the `| tail -1` and rely on `set -e` (already set at the top of release.sh) plus the existing audit step at Phase 1.5 that strings-checks the embedded version in every output binary. The version audit caught the stale-binary symptom; the build wrapper should have caught the root cause first.

   This bug class is also why task #441 description was expanded to bundle these into v2.2.13 work.

**v2.2.13 ships when 1, 2, and 4 are done.** Item 3 is a v2.2.14 (or later) candidate; do not gate v2.2.13 on it.

#### Stream B — BSD cross-compile (FreeBSD first, OpenBSD + NetBSD next)

BSD was missed in the original 5-platform commitment. This is a real distribution gap: FreeBSD has an active sysadmin community that overlaps heavily with our "consultants / privacy-conscious developers / small teams" audience, and we've been claiming "every platform" while shipping for 2 OSes (4 target triples). Correcting this is a credibility item, not a feature item.

Build order, smallest first:

1. **FreeBSD x86_64-unknown-freebsd target.** Rust has Tier-2 support for this triple. The cross-compile path is `cross` (the `cross-rs/cross` tool, not generic Docker — `cross` has a maintained FreeBSD sysroot image). Add to `scripts/release.sh`:
   ```bash
   docker run --rm -v "$PROJECT_DIR:/build" -w /build \
       ghcr.io/cross-rs/x86_64-unknown-freebsd:latest \
       cargo build --release --target x86_64-unknown-freebsd
   ```
   The `runtime/ssh/driver.rs` `#[cfg(unix)]` gate from Stream A item 1 already covers FreeBSD (BSD is unix per `target_family`). The only BSD-specific work we expect is in the daemon path (Tier 2 items 20-22) when we get there — launchd plist and systemd-user unit generators have no BSD analog; FreeBSD uses `rc.d` shell scripts and OpenBSD/NetBSD use a similar pattern. The daemon work generates whichever is appropriate based on `cfg(target_os)`.

2. **FreeBSD aarch64-unknown-freebsd target.** Same approach as #1, separate target triple. Apple Silicon developers running FreeBSD VMs and the rising population of BSD-on-ARM workstations are the audience here.

3. **OpenBSD x86_64-unknown-openbsd target.** OpenBSD has Tier-3 Rust support (no official binaries from upstream Rust), so we either (a) cross-compile from a build host running OpenBSD or (b) accept a build-from-source story for OpenBSD users (documented in install docs, with `cargo install --git` instructions). Decision deferred to whoever picks up the work; do not block FreeBSD on this.

4. **NetBSD x86_64-unknown-netbsd target.** Tier-3 like OpenBSD. Same decision posture: ship if it's straightforward, document the source-build path if not.

5. **Homebrew formula extension.** The current `culpur/homebrew-anvil` formula targets `darwin` + `linux` URLs. Homebrew on FreeBSD exists but is rare; the FreeBSD distribution path is `pkg`/ports, not brew. Provide a FreeBSD port skeleton in `dist/freebsd/Makefile` and document the manual install path (`fetch` the binary + verify sha256 + drop in `/usr/local/bin`) until somebody asks for the ports submission.

6. **AnvilHub install scripts.** `install.sh` already does OS detection via `uname -s`; extend the case statement to add `FreeBSD` → use the freebsd binary, error out cleanly on OpenBSD/NetBSD if those targets are not yet shipping with the current release ("OpenBSD support coming in v2.2.x — build from source with: …").

7. **CI coverage.** Add FreeBSD to the release-pipeline audit gate (the strings-grep version check from Stream A item 4). Once a FreeBSD binary exists, it is subject to the same v-string audit every other binary is.

**No v2.2.x commits to BSD work yet.** This is the backlog framing — BSD lands when somebody is ready to push it through, ideally bundled with whichever v2.2.x patch is next after v2.2.13. Item 1 (FreeBSD x86_64) is the highest-leverage entry point because it's the closest to the existing cross-compile infrastructure and unblocks audience credibility immediately.

#### Stream C — Platform-conditional code audit (continuous)

The v2.2.12 Windows incident was caused by code that compiled fine on macOS + Linux but referenced a Unix-only API on a target that doesn't have it. The release-pipeline audit gate (binary version strings check, php-lint guard) catches *outcomes* of broken cross-compiles but not the cause. Two things help going forward:

- **Run `cargo check --target x86_64-pc-windows-gnu` as part of `cargo test` on the developer machine before tag.** Adds ~30s to the test cycle; catches the next russh-style mismatch before the release pipeline does. Folded into the `anvil doctor --release` pre-flight from v2.2.12 (task #415).
- **When BSD lands, the same applies for `--target x86_64-unknown-freebsd`.** Each cross-compile target gets a `cargo check` in pre-flight.

The principle: **every advertised platform must compile in CI before the release tag is cut.** v2.2.12 violated this. v2.2.13 must not.

---

### Tier 9 — The actual v3.0 candidate

A v3.0 only makes sense when there's a real reason. Candidates:

- On-disk format change that requires a migration users can't ignore
- A licensing or business-model shift
- Hosted-Anvil ("Passage Model 4 — Enterprise Managed") going live and changing the install story
- An architectural break (e.g., the routines daemon becoming the primary entry point, with `anvil` interactive becoming a client of `anvil routined`)

Until one of these happens, stay on `v2.2.x`. The version number is cheap; user trust is not.

---

## Cross-cutting principles (preserved)

- **Per-tab isolation stays the unit of billing and policy.** Every new feature must preserve it. Routines, self-built MCPs, Team Mode users, and workspaces all bill per-tab.
- **Nominations is the delivery vehicle for all "Anvil suggests X" UX.** New categories (`mcp_opportunity`, future ones) extend the existing system rather than growing parallel UIs.
- **AnvilHub is the distribution surface for all shareable artifacts.** Agents, skills, plugins, themes, self-built MCPs, routine TOMLs — one marketplace, not several.
- **Passage stays focused on routing + metering + billing.** Infrastructure cost accounting, lifecycle management, and provider licensing are separate services feeding Passage. Resist monolith drift.
- **Version bumps mean something to the user.** Not "we shipped a lot." On-disk break, CLI break, or architectural seam, or no bump.

---

## Passage monetization mapping

Unchanged from the prior version. Reframed in patch-release terms.

### Model 1 — Self-Contained (shipped)

Blocker is distribution, not code. Routines and the durable journal both land under Model 1 and strengthen the free tier. README rewrite + HN launch + targeted subreddits are the near-term go-to-market actions, independent of backlog work.

### Model 2 — Hosted Ollama (next revenue milestone)

Depends on the routines daemon (Tier 2 items 20–22) because cloud-executed routines are the first thing users will pay compute for.

- Build infrastructure cost-tracking as a **separate service** feeding raw cost data into Passage — do not bloat Passage itself
- Pick one hosting provider (Linode / OVH / Phoenix NAP), run a single pilot instance, prove the metering loop end to end before adding a second

### Model 3 — BYOK Cloud

Unlocks once Model 2 ops are stable. Passage gains logic to charge the orchestration fee per tab while attributing inference cost to the user's own provider keys. Pairs well with Tier 6 (multi-repo) because consultants juggling client credentials are the natural BYOK audience.

### Model 4 — Enterprise Managed

Lands with whatever justifies v3.0. Requires Tier 8 (privacy + eval) and Tier 7 (collaboration) maturity — enterprises will not sign without privacy guarantees and team controls.

### Not on the backlog, by design

**FIPS / federal track.** Documented as intent. Keep algorithm choices FIPS-friendly opportunistically (don't introduce non-FIPS crypto), but no dedicated work until traction and funding exist. Revisit after Models 2 and 3 are generating revenue.

---

## Critical files / surfaces likely touched

| Tier / capability | Surfaces |
|---|---|
| Tier 2 routines daemon | `crates/runtime/src/cron.rs`, new `crates/anvil-cli/src/routined.rs` |
| Tier 2 delivery | new `crates/runtime/src/routines/delivery/` |
| Tier 2 reconciliation | new `crates/runtime/src/routines/reconcile.rs` |
| Tier 3 journal | `crates/runtime/src/session.rs` (becomes durable) |
| Tier 4 self-built MCPs | `crates/runtime/src/goals.rs` (`mcp_opportunity`), `crates/runtime/src/vault/` (per-MCP scopes — needs vault scope work first) |
| Tier 4 publish | `anvilhub/` — self-built-MCP publish flow |
| Tier 5 marketplace | `crates/runtime/src/hub.rs`, `anvilhub/` — manifest extensions |
| Tier 6 multi-repo | `crates/runtime/src/session.rs` — relax single-repo assumptions |
| Tier 7 team | `crates/runtime/src/vault/` — per-user access grants |
| Model 2 metering | `passage/` — metering hooks for hosted Ollama |
| Model 3 BYOK | `passage/` — platform-fee logic |
| Model 4 / v3.0 | `passage/` — managed-pool billing |
| MCP hardening (forever) | `feedback-mcp-hardening-principles.md` — template generator must enforce all six rules |
| Tier 8.5 Windows SSH (v2.2.13) | `crates/runtime/src/ssh/driver.rs` — `#[cfg(unix)]` gate + E0282 fixes |
| Tier 8.5 BSD cross-compile | `scripts/release.sh` — add freebsd targets via `cross-rs` images; `dist/freebsd/Makefile` for ports skeleton; `install.sh` OS detection extension |
| Tier 8.5 platform pre-flight | `crates/anvil-cli/src/doctor.rs` — extend `anvil doctor --release` with `cargo check` per target |

---

## Known risks (preserved)

1. **Distribution is the #1 risk.** Product is real; market awareness isn't. Phase 2 cannot succeed without fixing Model 1 distribution first.
2. **Infrastructure cost accounting is non-trivial.** Needs to be built right before Model 2 launches; wrong cost math eats margin.
3. **Complexity creep.** Four billing models, multi-provider orchestration, cloud lifecycle management — keep Passage focused, resist feature sprawl.
4. **Solo founder bandwidth.** Currently employed full-time elsewhere. Realistic pace matters more than ambitious backlog.
5. **Scope inflation via release-shaped thinking.** The prior eleven-release roadmap encouraged exactly this. The backlog framing is the corrective; do not regress.
6. **Platform-reach credibility.** v2.2.12 advertised 5 platforms and shipped 4. Until v2.2.13 lands the Windows fix and BSD support is on the matrix, the README "every platform, no install prereqs" line is overpromised. Tier 8.5 exists to close this gap.

---

## Near-term non-code actions

These are distribution / positioning work that must happen alongside Tier 2 routines work, not after:

1. Rewrite Anvil GitHub README and culpur.net/anvil product page with freedom-first positioning
2. Draft a Hacker News launch post for Model 1 (rebrand the story, don't re-launch the code)
3. Scope Model 2: infrastructure cost tracking service, Ollama hosting plan, per-user metering exposure
4. Pick one provider (Linode / OVH / Phoenix NAP) and price out a hosted-Ollama pilot instance
5. Don't think about FIPS for at least another quarter

---

## How to use this doc

When picking what to work on next:

1. Look at Tier 1 — is anything still partial?
2. Move to Tier 2 — what's the smallest unfinished item that unlocks the most downstream work?
3. Don't ask "what does this release ship?" Ask "what's ready when I cut the next patch?"
4. When in doubt, the build-order numbering in each tier is the dependency order.
5. Version label bumps only when a user-visible reason justifies it.
