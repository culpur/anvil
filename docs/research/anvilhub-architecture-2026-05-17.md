# AnvilHub Architecture — Research Brief for F2 (Skill-Chain Builder)

**Task:** #530 — Investigate AnvilHub user/auth + HubPackage schema + skill_chain runtime
**Status:** Internal architecture brief, not for any public surface
**Date:** 2026-05-17
**Unblocks:** #529 (F2: Skill-Chain Builder), informs #533 / #611
**Sibling work:** #528 (F1 source viewer) ships in parallel; this brief does not modify F1 surfaces

## TL;DR

Most of F2 is already half-built and unmerged-but-on-disk on dev0001.

- **Auth:** AnvilHub web uses NextAuth with Authentik as the sole OIDC provider (`Culpur SSO`). Passage hub routes validate the same Authentik access token via `/userinfo`. Both surfaces are wired today.
- **HubPackage schema:** The **canonical** schema lives in **Passage** (`prisma/schema.prisma`), not in the AnvilHub web package. The AnvilHub-web Prisma schema (`Package`, `User`, etc.) is **legacy / orphaned** — the running app reads from Passage via `/v1/hub/*` and never touches its own Prisma client for marketplace data.
- **Skill-chain on AnvilHub side:** Schema already extended with `SKILL_CHAIN` type, `HubUser`, `HubUserDraft`, `HubChain` models. `/v1/hub/me/drafts` (full CRUD + publish) and `/v1/hub/chains/:slug` (public read) routes are already implemented on Passage. **No UI exists yet** in `packages/web` (no `/chains` route, no React Flow canvas).
- **Skill-chain on Anvil side (`crates/commands/src/skill_chaining.rs`):** **Suggestion-only engine**, not an executor. Depth-3 traversal of `chains_to:` YAML frontmatter, suggests `/skill load <name>` to user, fully wired at three callsites in `crates/anvil-cli/src/main.rs`. Task #392 was correctly closed — but it shipped a **recommender**, not a **runner**. F2 needs a new executor.
- **React Flow already present:** `@xyflow/react ^12.10.2` is in `packages/web/package.json` — the canvas dependency F2 needs is already installed.

The doc below contains the sequence diagram, the verbatim Prisma blocks, the Anvil-side executor analysis, and the proposed F2 architecture mapped against the 8-axis capability contract.

---

## 1. User / Auth Flow

### Provider and identity surface

- **Provider:** Authentik at `https://login.culpur.net`, app slug `anvilhub`, client ID env var `AUTHENTIK_CLIENT_ID` (default literal `anvilhub`), client secret env var `AUTHENTIK_CLIENT_SECRET`.
- **OIDC discovery:** `https://login.culpur.net/application/o/anvilhub/.well-known/openid-configuration`
- **Userinfo (used by Passage):** `https://login.culpur.net/application/o/anvilhub/userinfo/`
- **End-session:** `https://login.culpur.net/application/o/anvilhub/end-session/`
- **Scopes:** `openid email profile`
- **NextAuth strategy:** JWT (no NextAuth database) — session is signed cookie containing `accessToken` + `idToken`.
- **Cookie names:** `next-auth.state`, `next-auth.callback-url`, `next-auth.csrf-token` (httpOnly, sameSite=lax, secure=true).

The web tier and the API tier do not share a session store — each request that hits Passage must send the Authentik access token as `Authorization: Bearer ...`. The web app pulls it from `session.accessToken` in the JWT.

### Sequence — anonymous browse → sign-in → publish

```
┌──────────┐       ┌──────────────────┐       ┌──────────────┐       ┌──────────────┐
│ Browser  │       │ anvilhub.culpur  │       │ login.culpur │       │ api.culpur   │
│          │       │ (Next.js web)    │       │ (Authentik)  │       │ (Passage)    │
└────┬─────┘       └────────┬─────────┘       └──────┬───────┘       └──────┬───────┘
     │                      │                        │                      │
     │  GET /  (anon)       │                        │                      │
     │─────────────────────►│                        │                      │
     │                      │  SSR fetch /v1/hub/    │                      │
     │                      │  packages (no auth)    │                      │
     │                      │───────────────────────────────────────────────►│
     │  HTML + cards        │◄──────────── public packages ──────────────────│
     │◄─────────────────────│                        │                      │
     │                      │                        │                      │
     │  click "Sign In"     │                        │                      │
     │─────────────────────►│                        │                      │
     │  302 → Authentik /authorize?response_type=code&...                    │
     │◄─────────────────────│                        │                      │
     │  follow redirect → Authentik consent          │                      │
     │──────────────────────────────────────────────►│                      │
     │                      │                        │                      │
     │  302 callback?code=  │                        │                      │
     │◄──────────────────────────────────────────────│                      │
     │  GET /api/auth/      │                        │                      │
     │  callback/authentik  │                        │                      │
     │─────────────────────►│  POST /token (code)    │                      │
     │                      │───────────────────────►│                      │
     │                      │◄────── id_token + access_token ───────────────│
     │                      │  mint NextAuth JWT cookie (httpOnly)          │
     │  Set-Cookie + 302    │                        │                      │
     │◄─────────────────────│                        │                      │
     │                      │                        │                      │
     │  GET /publish        │                        │                      │
     │─────────────────────►│                        │                      │
     │                      │  read JWT, render form │                      │
     │  HTML form           │                        │                      │
     │◄─────────────────────│                        │                      │
     │                      │                        │                      │
     │  submit POST /publish (client-side)           │                      │
     │  with session.accessToken                     │                      │
     │─────────────────────►│  POST /v1/hub/packages │                      │
     │                      │  Authorization: Bearer <access_token>         │
     │                      │───────────────────────────────────────────────►│
     │                      │                        │ requireHubAuth →    │
     │                      │                        │ GET /userinfo       │
     │                      │                        │◄─────────── profile  │
     │                      │                        │                      │
     │                      │                        │ create HubPackage   │
     │                      │                        │ + scan + upsert     │
     │  redirect /success   │◄──────────── 201 + warnings ─────────────────│
     │◄─────────────────────│                        │                      │
```

### Auth gates by route (Passage)

| Route                                | Method     | Middleware          | Visibility            |
|--------------------------------------|------------|---------------------|-----------------------|
| `/v1/hub/packages` (list)            | GET        | none                | Public (APPROVED/FLAGGED only) |
| `/v1/hub/packages/:slug`             | GET        | `optionalHubAuth`   | Public; author/admin sees own QUARANTINED |
| `/v1/hub/packages`                   | POST       | `requireHubAuth`    | Authenticated         |
| `/v1/hub/packages/:slug`             | PATCH      | `requireHubAuth` + ownership | Author only  |
| `/v1/hub/packages/:slug/install`     | POST       | rate-limit only     | Public (telemetry)    |
| `/v1/hub/packages/:slug/download`    | POST       | none                | Public                |
| `/v1/hub/packages/:slug/source[/*]`  | GET        | rate-limit only     | Public (F1)           |
| `/v1/hub/me/drafts*`                 | GET/POST/PATCH/DELETE | `requireHubAuth` | Owner only (F2) |
| `/v1/hub/me/drafts/:id/publish`      | POST       | `requireHubAuth`    | Owner only (F2)       |
| `/v1/hub/chains/:slug`               | GET        | none                | Public (F2 read)      |
| `/v1/hub/publishers/:slug`           | GET        | none                | Public                |
| `/v1/hub/admin/publishers/*`         | (various)  | admin-only          | Staff (`@culpur.net`) |

The admin-only gate is currently `user?.email?.endsWith('@culpur.net')` — not a group check. (Open question #3 below.)

---

## 2. HubPackage Schema (Passage — canonical)

The schema below is **the source of truth**. The AnvilHub-web Prisma file exists but is orphaned (no migration applied in production, web layer talks to Passage via REST). F2 must extend the Passage schema, not the web one.

### Enums

```prisma
enum HubPackageType {
  SKILL
  PLUGIN
  AGENT
  THEME
  SKILL_CHAIN     // already declared, F2-ready
}

enum HubTrustLevel {
  UNVERIFIED      // default
  VERIFIED        // manually granted by staff
  CULPUR_OFFICIAL // auto-applied to AnvilHub-Official publisher
  REVOKED         // delisted
}

enum HubPackageStatus {
  PENDING_REVIEW  // just uploaded
  APPROVED        // passed scan
  QUARANTINED     // failed scan, hidden
  FLAGGED         // passed with warnings, visible with badge
}
```

### Core HubPackage

```prisma
model HubPackage {
  id                  String           @id @default(uuid())
  name                String           @unique          // marketplace name (== slug)
  slug                String           @unique          // URL slug, [a-z0-9-]
  type                HubPackageType                    // SKILL | PLUGIN | AGENT | THEME | SKILL_CHAIN
  status              HubPackageStatus @default(PENDING_REVIEW)
  version             String                            // semver
  description         String                            // short, ≤500 chars
  longDescription     String?          @db.Text         // marketing copy
  readme              String?          @db.Text         // markdown
  authorName          String
  authorEmail         String?                           // matched against Authentik email for ownership
  repository          String?                           // git URL
  homepage            String?
  license             String           @default("MIT")
  tags                String[]         @default([])
  categories          String[]         @default([])
  manifest            Json?                             // type-specific manifest blob
  installCmd          String                            // computed server-side, e.g. "/skill install foo"
  compatibility       String           @default(">=0.1.0")
  downloads           Int              @default(0)
  rating              Float            @default(0)
  ratingCount         Int              @default(0)
  verified            Boolean          @default(false)  // legacy single-tier flag
  featured            Boolean          @default(false)
  sourceArchiveUrl    String?                           // MinIO URL (F1)
  sourceArchiveSize   Int?
  sourceArchiveSha256 String?
  lastScanResult      Json?
  publisherId         String?
  publisher           HubPublisher?    @relation(...)
  verifiedPublisher   Boolean          @default(false)  // F3 two-tier
  highestVerifiedVersion String?                        // F3 fast-path for card
  versions            HubPackageVersion[]
  reviews             HubPackageReview[]
  chain               HubChain?                         // 1:1 — present iff type=SKILL_CHAIN (F2)
  createdAt           DateTime         @default(now())
  updatedAt           DateTime         @updatedAt
  @@map("HubPackage")
}
```

### Related tables (existing)

- `HubPackageVersion` — semver row per release; F3 verified-build fields (`verifiedBuild`, `signature`, `signatureAlgo`, `scanResult`, `verifiedAt`).
- `HubPackageReview` — 1-5 star + comment, unique by (packageId, authorEmail).
- `HubInstallEvent` — anonymous telemetry, IP hashed (SHA-256 hex, 64 chars).
- `HubPublisher` — F3 trust-level profile, slug-addressable at `/v1/hub/publishers/:slug`.

### F2-specific tables (already present on disk)

```prisma
model HubUser {
  id           String         @id @default(uuid())
  authentikSub String         @unique     // mirrors profile.sub from /userinfo
  email        String
  displayName  String
  createdAt    DateTime       @default(now())
  updatedAt    DateTime       @updatedAt
  drafts       HubUserDraft[]
  @@map("HubUser")
}

model HubUserDraft {
  id            String   @id @default(uuid())
  userId        String
  user          HubUser  @relation(...)
  name          String   @default("Untitled Chain")
  chainManifest Json                              // full DAG (see ChainManifest schema)
  createdAt     DateTime @default(now())
  updatedAt    DateTime @updatedAt
  @@map("HubUserDraft")
}

model HubChain {
  id            String     @id @default(uuid())
  packageId     String     @unique
  package       HubPackage @relation(...)
  nodes         Json       // ChainNode[]      — { id, skillSlug, position:{x,y}, data? }
  edges         Json       // ChainEdge[]      — { id, source, target, sourceHandle?, targetHandle?, condition? }
  slotsManifest Json       // SlotBindings     — { slotName: { fromNode, fromOutput } }
  createdAt     DateTime   @default(now())
  updatedAt     DateTime   @updatedAt
  @@map("HubChain")
}
```

### Annotated field roles for F2

| Field                          | Role                                                                       |
|--------------------------------|----------------------------------------------------------------------------|
| `HubPackage.type = SKILL_CHAIN`| Discriminator; UI routes off this; Anvil `/chain install <slug>` keys off it |
| `HubPackage.manifest`          | Mirror of `HubChain.{nodes,edges,slotsManifest}` for install-time atomicity |
| `HubPackage.installCmd`        | Computed by `computeInstallCmd({type:'SKILL_CHAIN', slug})` — currently returns what? (see open Q #1) |
| `HubChain.nodes`               | React Flow node array, each references a `HubPackage(type=SKILL).slug` via `skillSlug` |
| `HubChain.edges`               | React Flow edge array; `condition` field carries chain trigger semantics (Always / Expr) |
| `HubChain.slotsManifest`       | Maps chain-level inputs/outputs to specific node ports — enables composition |
| `HubUserDraft.chainManifest`   | Same shape as HubChain but pre-publish, private to one user                |

### What's missing for F2 completeness

- **Skill dependency join table:** today `HubChain.nodes[*].skillSlug` is a denormalized string — no FK, no cascade, no integrity. A `HubChainDependency(chainId, skillSlug, requiredVersion)` table would let us cascade-warn on dependency revocation. See open Q #4.
- **Per-node validation status:** when a skill in a published chain gets QUARANTINED, the chain has no marker that it's now partially-broken. Suggest adding `HubChain.dependencyStatus: Json` (computed) or a worker that flips chain status to FLAGGED.
- **Anvil execution semantics declaration:** the chain manifest doesn't declare whether nodes run sequentially, in parallel, or with branching. The edge `condition` field is a freeform string. F2 must lock down a small grammar (Always / IfOutput / IfSkillLoaded) and write it into the schema as an enum or validation rule.
- **Chain version + immutability:** `HubPackageVersion` covers it, but `HubChain` itself only carries the latest DAG. Need either a `HubChainVersion` table or a guarantee that re-publish creates a new HubPackage row.

---

## 3. Skill-Chain on the Anvil Side

### Where it lives

- Engine: `crates/commands/src/skill_chaining.rs` (951 lines, last touched for v2.2.13).
- Re-exports: `crates/commands/src/lib.rs:16-19` exports `ChainCandidate`, `ChainEvaluator`, `ChainEntry`, `ChainWhen`, `format_chain_candidates`, `format_chain_hint`, `render_chains_graph`.
- Frontmatter parsing: `crates/commands/src/agents.rs:540` (`parse_chains_to`) populates `SkillSummary.chains_to: Vec<ChainEntry>` at skill-load time.
- Slash dispatch: `/skill chains` renders the graph at `crates/anvil-cli/src/main.rs:6002-6011`.
- Auto-hint at turn-start: two callsites in `crates/anvil-cli/src/main.rs` (lines 3836-3857 and 4888-4911) — these invoke `ChainEvaluator::new().evaluate(...)` and print `format_chain_hint(...)`.

### Data model — verbatim

```rust
pub enum ChainWhen { Always, IfKeyword(String), IfSkillLoaded(String) }
pub struct ChainEntry { pub skill: String, pub when: ChainWhen }
pub struct ChainCandidate {
    pub skill_name: String,
    pub triggered_by: String,
    pub reason: String,
    pub depth: usize,
}
pub struct ChainEvaluator {
    pub max_depth: usize,         // default 3
    pub max_total_bytes: usize,   // default 25_000
    pub max_chain_per_skill: usize, // default 5
}
```

The unit of declaration is a YAML frontmatter block on a `SKILL.md` file:

```yaml
---
name: code-review
chains_to:
  - skill: security-audit
    when: always
  - skill: file-fingerprint
    when: "if-keyword: cat"
---
```

Or string shorthand: `chains_to: [skill-a, skill-b]` (everything is `Always`).

### What the engine does today

1. User loads a skill with `/skill load code-review`.
2. On the **next turn-start**, the evaluator runs over all loaded skills.
3. For each loaded skill, it walks the `chains_to:` tree up to `max_depth=3`, applying `when` clauses and a 25 KB accumulated body-bytes budget.
4. Cycle, dedup, and already-loaded guards prevent infinite recursion.
5. The output is a `Vec<ChainCandidate>` formatted by `format_chain_hint` and printed inline as:
   `💡 You loaded code-review; it chains to security-audit (always). /skill load <name> to add.`
6. The user **manually decides** whether to run `/skill load <name>`.

### What it explicitly does NOT do

- **No execution.** Skills are SKILL.md context files; "loading" injects their body into the system prompt. There is no concept of running a skill — they are not callable.
- **No chain "run."** No code in the repo currently invokes a chain end-to-end, passes outputs between steps, or treats a chain as an executable artifact.
- **No remote chain discovery.** Chains live entirely in the local skill discovery tree (`crates/commands/bundled/skills/` + plugin contributions). There is no Hub fetch of remote chains today.
- **No depth-N where N>3.** Hard cap, all tests verify it. F2 can configure but should keep the default.
- **No error / cancel semantics.** Nothing to cancel — it's a one-shot recommender.

### Distance from F2's "skill_chain runtime engine"

F2's HubChain manifest (`nodes[]`, `edges[]`, `slotsManifest`) is a **DAG with explicit ports**. The current Anvil engine is a **tree of references with `when:` clauses**, no ports.

To execute a HubChain manifest, Anvil needs a **new module**, not an extension of `skill_chaining.rs`. The new module needs:

1. `ChainManifest` deserializer compatible with Passage's `me.ts` Zod schema.
2. Topological-sort planner that detects cycles + unreachable nodes.
3. A per-node "step" abstraction. Open Q #5: what is a step? A skill-load? An LLM turn with the skill loaded? A tool invocation?
4. Slot-binding resolver that wires `slotsManifest[outputSlot] = { fromNode, fromOutput }` to actual values.
5. Cancellation via the existing tokio cancel-token plumbing used by Ctrl+C.
6. OTel spans, hook integration, permission gate routing (all 8 axes per `feedback-anvil-capability-contract.md`).

### Verification of #392's depth-3 claim

Confirmed. Test `evaluator_walks_three_levels_deep_caps_at_three` at `skill_chaining.rs:910-931` asserts:
- A→B→C→D→E: with default `max_depth=3`, B is at depth 1, C at depth 2, D at depth 3, E excluded.
- The test passes (visible in v2.2.13 CI history; not re-run for this brief).

#392 is closed correctly. The engine is wired, tested, and reachable. It just isn't an executor — and was never claimed to be.

---

## 4. Proposed F2 Architecture

### Schema additions

Already on disk (no migration needed): `HubUser`, `HubUserDraft`, `HubChain`, `HubPackageType.SKILL_CHAIN`.

Recommended *new* additions (require migration):

```prisma
model HubChainDependency {
  id           String     @id @default(uuid())
  chainId      String
  chain        HubChain   @relation(fields: [chainId], references: [id], onDelete: Cascade)
  skillSlug    String                                 // FK by slug, not id (skill may be re-published)
  requiredCompatibility String  @default(">=0.0.0")
  createdAt    DateTime   @default(now())
  @@unique([chainId, skillSlug])
  @@index([skillSlug])                                // reverse lookup: "which chains use this skill?"
  @@map("HubChainDependency")
}

// Optional: edge semantics enum, replacing freeform `condition: string`
enum HubChainEdgeCondition {
  ALWAYS
  IF_OUTPUT_TRUTHY
  IF_SKILL_LOADED
  IF_PREVIOUS_SUCCEEDED
}
```

### API endpoints (already exist except where noted)

| Endpoint                                    | Status        | Notes |
|---------------------------------------------|---------------|-------|
| `GET /v1/hub/me/drafts`                     | DONE          | Lists current user's drafts |
| `POST /v1/hub/me/drafts`                    | DONE          | Creates draft |
| `PATCH /v1/hub/me/drafts/:id`               | DONE          | Owner-only update |
| `DELETE /v1/hub/me/drafts/:id`              | DONE          | Owner-only delete |
| `POST /v1/hub/me/drafts/:id/publish`        | DONE          | Promote draft → HubPackage(SKILL_CHAIN) + HubChain in single Tx |
| `GET /v1/hub/chains/:slug`                  | DONE          | Public read; returns nodes/edges/slotsManifest + skillDeps + `anvil://chain/run/<slug>` deep link |
| `GET /v1/hub/chains/:slug/dependencies`     | **TO BUILD**  | Resolved skill deps with current status (uses HubChainDependency) |
| `POST /v1/hub/chains/:slug/install`         | **TO BUILD**  | Telemetry counterpart to `/v1/hub/packages/:slug/install` |

### UI component tree (anvilhub-web — TO BUILD)

```
src/app/chains/                            (new route)
  page.tsx                                 List view (SKILL_CHAIN type filter)
  [slug]/
    page.tsx                               Public chain detail + read-only canvas
src/app/me/                                (new route, behind useSession())
  drafts/
    page.tsx                               List user's drafts
    [id]/
      page.tsx                             Builder canvas (React Flow)
src/components/chain-builder/              (new)
  ChainCanvas.tsx                          React Flow wrapper (@xyflow/react already installed)
  ChainNode.tsx                            Custom node for a skill reference
  ChainEdge.tsx                            Custom edge with condition label
  ChainSidebar.tsx                         Skill picker (calls /v1/hub/packages?type=SKILL)
  ChainPublishModal.tsx                    Maps to POST /me/drafts/:id/publish
  ChainViewer.tsx                          Read-only variant for /chains/[slug]
src/lib/chainSchema.ts                     ChainManifest Zod schema mirroring Passage's
src/lib/chainDraft.ts                      Draft CRUD client (fetch wrappers)
```

### Anvil-side executor (TO BUILD)

New crate module: `crates/runtime/src/skill_chain_run/` (separate from `crates/commands/src/skill_chaining.rs`, which stays the suggest-engine).

```
crates/runtime/src/skill_chain_run/
  mod.rs                Public API: run_chain(slug, args) -> Result<ChainResult, ChainError>
  manifest.rs           ChainManifest + ChainNode + ChainEdge structs (serde, matches Passage Zod)
  fetch.rs              Pulls manifest from api.culpur.net/v1/hub/chains/:slug; caches per-version
  plan.rs               Topological sort + cycle detection + slot resolution
  exec.rs               Per-node executor; one step = "load skill + run one turn"
  cancel.rs             Hook into existing tokio cancel-token plumbing
  otel.rs               Spans: chain.run, chain.step, chain.step.skill_load, chain.step.turn
```

New slash command surface (per the 8-axis contract):

1. **Definition:** `SlashCommand::Chain { action: ChainAction }` enum variant.
2. **Registration:** `slash_command_specs` entry with subcommands `install`, `list`, `info`, `run`, `cancel`.
3. **Completion:** TAB-complete via existing spec mechanism.
4. **Handler:** `crates/commands/src/handlers.rs::handle_chain_command` (analogous to `handle_hub_command`).
5. **Dispatch:** unified through `commands/dispatch.rs::dispatch_slash_command` per `feedback-anvil-dispatch-unified.md`.
6. **Rendering:** chain-step progress renders identically in TUI scrollback + viewer.html (use the shared formatter).
7. **Permission gate:** chain.run is a mutating-ish op (loads skills, runs turns); flow through the safety chain identical to a multi-tool turn.
8. **OTel + tests:** spans listed above; ≥1 end-to-end test that fetches a fixture chain and runs it.

Deep-link handling: macOS/Linux URL handler `anvil://chain/run/<slug>` → `anvil --chain <slug>` (a new flag). Wire in `crates/anvil-cli/src/main.rs` arg parsing.

### Drift test (per `feedback-slash-spec-drift.md`)

Extend `every_slash_command_variant_has_a_spec` to also assert that every `SlashCommand::Chain` action is dispatched in `commands/dispatch.rs`. No new test needed if the existing bidirectional drift test already covers it — verify before merge.

---

## 5. Open Questions for the User

These cannot be determined from code alone. Per `feedback-no-silent-deferral.md` they belong here, not buried as TODOs.

1. **`installCmd` for SKILL_CHAIN — what string does `computeInstallCmd({type:'SKILL_CHAIN', slug})` return?** I didn't read `utils/hubInstallCmd.ts` directly. Hypotheses: `/chain install <slug>` (new namespace) vs. `/hub install chain:<slug>` (reuse `/hub`) vs. `anvil chain install <slug>` (CLI subcommand). Pick one before F2-Anvil work starts; it locks the slash-command shape and the deep-link behavior.

2. **Chain visibility model — public, private, or both?** Today Passage exposes `GET /v1/hub/chains/:slug` publicly for `APPROVED`/`FLAGGED` SKILL_CHAIN packages, and `/v1/hub/me/drafts` for owner-only drafts. Is there a middle tier (unlisted but accessible by deep-link)? If yes, `HubPackageStatus` needs a `PRIVATE` value and listing has to exclude it.

3. **Admin / staff gate — should it be the current `email.endsWith('@culpur.net')` heuristic or an Authentik group check?** Cross-references `reference-authentik-api.md` (Authentik PK 23 has groups; we can call `/api/v3/core/groups/?members__pk=<sub>` from `hubAuth.ts`). If you want a real group check before F2 ships, that's a 1-2h hardening item that should be bundled.

4. **Skill-dependency model — slug or ID?** I'm recommending `HubChainDependency.skillSlug: String` (FK by slug, not id) so dependencies survive re-publish of a referenced skill. Counter-argument: if a skill author transfers ownership, the slug might be re-issued. Want a versioned FK (`packageId + version`) instead? Locks down chain-deps to specific versions.

5. **Per-node execution semantics — what IS a chain step?**
   - **Option A:** A step = "load the referenced skill, then run one LLM turn." Slot inputs become a prompt-prefix; slot outputs are parsed from the LLM response by a delimiter. Easiest to ship, weakest contract.
   - **Option B:** A step = "load skill, run one turn with a strict JSON output schema declared in the skill's frontmatter." Stronger but requires schema-discipline in skills.
   - **Option C:** A step = "run one tool, using the skill only for system-prompt augmentation." Most explicit. Worst UX if the chain author wanted LLM reasoning between steps.

   This is the highest-leverage decision in F2. The shape of `slotsManifest` and the executor's complexity scale with it.

6. **Chain depth cap.** The suggest engine caps at 3. Should the **executor** also cap at 3, or are we letting users build chains-of-chains 10 deep? Recommended cap: 5 for the executor, with `max_depth` configurable in `~/.anvil/config.toml`.

7. **Fresh-session vs same-session execution.** When a chain runs, does it run in the user's current `anvil` session (sharing memory, hooks, file-cache), or does each step run in an isolated sub-session (like `/subagent`)? Trade-off: isolation buys safety but loses context; sharing buys context but a chain step can pollute parent state.

8. **AnvilHub `/chains` nav link timing.** Should we surface a "Chains" tab in `src/components/Header.tsx` immediately (empty state) or only when ≥1 SKILL_CHAIN package is APPROVED? UX preference question.

9. **Anvil-side "discovered chains" UX.** Once `/chain install <slug>` lands a manifest in `~/.anvil/chains/<slug>.json`, should `/skill chains` (the current graph viewer) also surface installed remote chains? Or should chains have their own `/chain list` viewer? Today `/skill chains` only knows about `chains_to:` declarations from local SKILL.md files.

10. **MinIO / source archive for chains.** SKILL_CHAIN packages don't really have a "tarball" the way a SKILL/PLUGIN does — the chain manifest IS the artifact. Does `sourceArchiveUrl` apply (we'd tar up a one-file manifest)? Or is the JSON manifest from `/v1/hub/chains/:slug` the canonical install payload?

---

## Appendix A — File index for #529 (F2 implementer)

**Read-only references:**
- `/opt/projects/passage-culpur.net/prisma/schema.prisma:2027-2235` (Hub models)
- `/opt/projects/passage-culpur.net/src/routes/hub/me.ts` (drafts CRUD + publish, already done)
- `/opt/projects/passage-culpur.net/src/routes/hub/chains.ts` (public chain detail, already done)
- `/opt/projects/passage-culpur.net/src/routes/hub/packages.ts` (POST mirror this for chains if needed)
- `/opt/projects/passage-culpur.net/src/middleware/hubAuth.ts` (auth pattern)
- `/Users/soulofall/projects/anvil-dev/crates/commands/src/skill_chaining.rs` (suggest engine — do NOT modify for F2)
- `/Users/soulofall/projects/anvil-dev/crates/commands/src/lib.rs:9-19` (re-exports)

**To-build files:**
- `/opt/projects/anvilhub/packages/web/src/app/chains/page.tsx`
- `/opt/projects/anvilhub/packages/web/src/app/chains/[slug]/page.tsx`
- `/opt/projects/anvilhub/packages/web/src/app/me/drafts/[id]/page.tsx`
- `/opt/projects/anvilhub/packages/web/src/components/chain-builder/*`
- `/Users/soulofall/projects/anvil-dev/crates/runtime/src/skill_chain_run/*` (new module)
- `SlashCommand::Chain` enum variant + spec + handler + dispatch + drift-test extension

**Do NOT touch (owned by #528):**
- `/opt/projects/anvilhub/packages/web/src/app/[type]/[slug]/page.tsx`
- `/opt/projects/anvilhub/packages/web/src/components/source-viewer/`

---

## Appendix B — Surfaces inspected (for next-release audit)

Per `feedback-public-surface-infra-redaction.md`: this brief is internal. If any of it becomes user-facing later, the following references would need to be scrubbed/redacted:

- `dev0001`, `/opt/projects/anvilhub`, `/opt/projects/passage-culpur.net` paths (internal hostnames/paths)
- `registry.culpur.net/git/culpur/anvil.git` (internal Gitea)
- The Authentik client ID literal (`anvilhub`) and userinfo URL — already in public auth flow but worth flagging
- Authentik admin staff gate heuristic (`@culpur.net` email suffix)
- The `culpur-admins` / `culpur-staff` group naming if we move to a real group check

No AnvilHub public pages were scraped for this brief; all data came from source code on dev0001 + local clone. No HTTP requests to anvilhub.culpur.net were made.
