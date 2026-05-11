# Archon vs Anvil — research comparison

**Audited:** 2026-05-11
**Archon path:** /Users/soulofall/projects/archon
**Anvil path:** /Users/soulofall/projects/anvil-dev
**Anvil current version:** v2.2.12 (just released today)

## 1. What Archon is (one paragraph)

Archon is an open-source **workflow engine for AI coding agents**, a "n8n for software development." It allows users to define deterministic, repeatable development processes (plan → implement → validate → review → PR creation) as DAG-based YAML workflows that mix deterministic bash nodes with AI-driven LLM prompts. It ships as a Bun/TypeScript monorepo with a Web UI, CLI, and adapters for Slack, Telegram, Discord, and GitHub webhooks. Licensed MIT, last commit 2026-05-11. The project is ~13 MB / 841 files with a single developer maintaining it. Core entry point is `packages/cli/src/cli.ts` and the HTTP server at `packages/server/src/index.ts`.

## 2. Architecture summary

**Top-level components:**

- **CLI (`packages/cli/`)** — Command-line entry point via `bun run cli`; routes to workflow executor, server, or direct codebase operations.
- **HTTP Server (`packages/server/`, Hono on port 3090)** — REST API for workflow management, codebases, and messaging; OpenAPI-generated from Zod schemas.
- **Web Adapter (`packages/server/src/adapters/web/`) and Web UI (`packages/web/`)** — React frontend at port 5173; SSE streaming for real-time message delivery.
- **Workflow Engine (`packages/workflows/`)** — DAG executor, YAML loader, prompt routing, node execution (bash, loop, approval, script, command, prompt).
- **Chat Platform Adapters (`packages/adapters/`)** — Slack (polling), Telegram (polling), GitHub (webhooks), Discord (WebSocket).
- **Core (`packages/core/`)** — Orchestration, command handler, database operations, conversation state machine.
- **Isolation (`packages/isolation/`)** — Worktree provider, path resolution, isolation error classifiers.
- **Git (`packages/git/`)** — Worktree operations, branch management, repo sync.
- **Providers (`packages/providers/`)** — AI agent SDKs (Claude, Codex, Pi community); each provider implements `IAgentProvider` interface.
- **Paths & logger (`packages/paths/`)** — Path resolution, Pino logger, telemetry capture, environment stripping.

**Database:** SQLite by default (`~/.archon/archon.db`), optional PostgreSQL via `DATABASE_URL`. 8 tables: `codebases`, `conversations`, `sessions`, `isolation_environments`, `workflow_runs`, `workflow_events`, `messages`, `codebase_env_vars`.

**Directory layout:** User-level `~/.archon/workspaces/owner/repo/{source,worktrees,artifacts,logs}`, repo-level `.archon/{workflows,commands,scripts,config.yaml}`. No cron daemon today; "long-lived" means the server process stays up via web UI or Docker.

## 3. Schedule grammar / dispatch

**Archon does not have schedule grammar or cron-like dispatch.** Workflows are **event-triggered only**: CLI (`bun run cli workflow run <name>`), Slack/Telegram slash commands (routed through chat adapters), GitHub webhooks (issues, PRs), or Web UI buttons. There is no `schedule:` field in workflow YAML, no `cron` table in the database, and no daemon polling.

**Dispatch is routing-based.** The orchestrator receives a message, calls an AI router to decide "is this a workflow request?", and if yes, uses `resolveWorkflowName()` (packages/workflows/src/router.ts:42-80) to match the intent to a workflow by name via a 4-tier fallback (exact → case-insensitive → suffix match → substring match). If no workflow match is found, it falls back to `archon-assist` (the catch-all "just answer the question" workflow). Workflow execution is **fire-and-forget on Web UI** (`dispatchBackgroundWorkflow()` at packages/core/src/orchestrator/orchestrator.ts:336-370), **foreground on CLI** (user waits for completion).

The **only scheduled capability** is the cleanup service at `packages/core/src/services/cleanup-service.ts`, which runs an in-process timer loop that deletes stale workflow runs and isolation environments on a 7-day interval. This is not exposed as a user-facing routine/cron feature.

## 4. Output / archive / delivery

**Archon does NOT have a delivery layer.** Workflow outputs are:

1. **Database (workflow_events table):** Every node transition, artifact, and error is logged to `workflow_events` at packages/core/src/db/workflow-events.ts:8-50. The `data` JSONB column stores node outputs as `node_output: string`.

2. **Filesystem artifacts:** Node stdout/stderr and generated files land in `~/.archon/workspaces/owner/repo/artifacts/runs/{workflow-id}/`. Artifact paths are computed by `getRunArtifactsPath(owner, repo, workflowRunId)` at packages/paths/src/archon-paths.ts:366-368. Artifacts are **never deleted** — cleanup service does not touch them.

3. **Web UI streaming:** Workflow output is streamed to the user's browser in real-time via SSE from `packages/server/src/adapters/web/transport.ts`. Messages are buffered if the client reconnects within a grace period (5 seconds).

4. **Platform-native output:** Slack/Telegram adapters receive final workflow text via `platform.sendMessage()`, but there is **no built-in email, webhook, or file delivery**. Stdout from bash nodes goes to the database; there is no facility to email it, POST it, or archive it outside the workflow's artifacts folder.

The **archive shape** is per-workflow: `~/.archon/workspaces/owner/repo/logs/{workflow-id}.jsonl` (JSONL, one event per line). This is separate from the `workflow_events` database table — it's a **read-only log file for debugging**, not a durable delivery surface.

## 5. Skill / agent definition format

Archon has **commands** (markdown files at `.archon/commands/`) and **workflows** (YAML files at `.archon/workflows/`). There is no "skill" concept.

**Commands:** Plain text markdown at `.archon/commands/{name}.md`. Archon does not parse frontmatter. Commands are discovered by filename (`archon-assist`, `archon-plan-setup`, etc.) and invoked by a `command:` node in a workflow. The command text is the entire message sent to the LLM. Example at `.archon/workflows/defaults/archon-assist.yaml:1-20` — the node `id: assist` has `command: archon-assist`, which is resolved to `.archon/commands/archon-assist.md`.

**Workflows:** YAML at `.archon/workflows/{name}.yaml` (or subdirectories). Schema defined at `packages/workflows/src/schemas/workflow.ts:91-96`. Structure: name, description, optional provider/model/effort/thinking, and a DAG of nodes:

```yaml
name: archon-idea-to-pr
description: ...
nodes:
  - id: create-plan
    command: archon-create-plan
    context: fresh
  - id: implement-tasks
    depends_on: [create-plan]
    prompt: "Read the plan. Implement..."
    loop:
      until: ALL_TASKS_COMPLETE
      fresh_context: true
```

Node types: `command:`, `prompt:`, `bash:`, `loop:`, `approval:`, `script:` (at packages/workflows/src/schemas/dag-node.ts). No frontmatter or TOML — YAML only.

## 6. Context injection mechanism

**Linear chaining only. No RAG or retrieval.**

Context comes from:
1. **Variable substitution:** `$1`, `$2`, `$3` (CLI args), `$ARTIFACTS_DIR` (per-run folder), `$WORKFLOW_ID`, `$BASE_BRANCH`, `$DOCS_DIR`, `$LOOP_PREV_OUTPUT` (previous iteration in loop nodes), `$LOOP_USER_INPUT` (approval gate input), `$REJECTION_REASON` (reject feedback). See packages/workflows/src/executor-shared.ts for substitution logic.

2. **Node output chaining:** `$nodeId.output` substitutes the stdout of a prior node into a downstream node's prompt. The DAG executor captures all node outputs (including AI text) in a map, then substitutes on demand at packages/workflows/src/dag-executor.ts:1030-1080.

3. **Multi-agent context (limited):** The `trigger_rule` field (at packages/workflows/src/schemas/dag-node.ts:103-106) controls when a node runs relative to its dependencies. `all_success` (default) waits for all deps to succeed; `one_success` runs when any dep succeeds; `all_completed` runs regardless of dep status. This enables fan-in (multiple agents feeding one synthesizer) without explicit context injection — the synthesizer just reads `$nodeId.output` from each prior agent. Example: `archon-idea-to-pr.yaml:131-133` has five parallel review agents, all feeding a `synthesize` node via `trigger_rule: one_success`.

4. **No built-in context from upstream runs.** If routine X wants to read the output of routine Y (completed in a prior day), Archon has **no mechanism for that**. The closest is a bash node that reads `workflow_events` table and extracts prior outputs, but that's user-implemented.

The **prompt builder** is at packages/workflows/src/router.ts. It:
- Loads the command or prompt text from YAML.
- Substitutes variables.
- Injects tool definitions (bash, script runners, git ops, workflow-trigger).
- For Claude, adds skills + skill chains (via `@archon/providers`).
- Sends to the AI provider.

## 7. Vault / secrets model

**Archon does NOT have a vault.** API keys and credentials must be placed in environment variables. The system injects per-codebase env vars from `remote_agent_codebase_env_vars` table (set via Web UI at `packages/server/src/routes/`) into the LLM call and bash nodes.

**Secrets handling:**
- `.archon/config.yaml` and `.env` files are **user-responsibility**; Archon does not encrypt them.
- Env vars set via Web UI are stored plaintext in the `codebase_env_vars` table (packages/core/src/db/env-vars.ts:22-46).
- Sensitive values (tokens, API keys) are **redacted in logs** by `credential-sanitizer.ts` (matches patterns like `sk-`, `ghp_`, `xoxb-`).
- Bash nodes receive env vars as-is in `process.env`; they are visible to scripts.
- The LLM provider (Claude SDK, Codex SDK) consumes env vars directly from the host process — Archon does not mediate or rotate credentials.

**No scope concept.** All env vars for a codebase are accessible to all workflows and nodes. A malicious or buggy node can leak them.

## 8. Daemon / persistent execution

**Archon does NOT have a cron daemon today.** There is **no long-lived background process** that fires workflows on a schedule. The system works in three modes:

1. **Web UI:** Run `archon serve` (or binary `archon serve`); the HTTP server stays up on port 3090, Web UI on 5173. Workflows are triggered by web button clicks. The process must stay running.

2. **CLI:** Run `bun run cli workflow run <name>` or similar; the CLI runs the workflow to completion and exits.

3. **Docker:** `docker-compose up` starts the server + web UI. The container stays running; workflows are dispatched via the Web UI or webhook adapters (Slack, Telegram, GitHub).

There is **no `/etc/systemd/user/` service**, no launchd plist, no Task Scheduler entry. If the user quits the `archon serve` process or stops the Docker container, no routines fire. This is intentional — Archon prioritizes simplicity and does not manage long-lived daemons.

The cleanup service (`packages/core/src/services/cleanup-service.ts:11-30`) runs an in-process interval (default 300s) to delete old workflow runs and isolation environments. This is **not exposed as a user feature** and only runs while the server is live.

## 9. Anti-pattern guardrails

**Archon employs these safeguards:**

1. **Worktree isolation per workflow run** (packages/isolation/src/providers/worktree.ts) — Every workflow run creates a fresh git worktree on a unique branch. Changes are isolated; parallel runs don't conflict. Users must explicitly commit and push to merge changes.

2. **Permission-mode gating** (packages/providers/src/claude/) — Claude SDK options include `permissionMode: 'bypassPermissions' | 'askAlways' | 'default'`. Archon passes this through from workflow config but does NOT enforce a default; SDK defaults apply. No blanket sandbox.

3. **Allowed/denied tools per node** (packages/workflows/src/schemas/dag-node.ts:151-152) — Claude nodes can restrict tool use via `allowed_tools` / `denied_tools` arrays. Archon does not expose this to other providers.

4. **Prompt-level redaction** (packages/core/src/utils/credential-sanitizer.ts) — Before sending to the LLM, Archon redacts obvious token patterns (`sk-`, `ghp_`, `xoxb-`). Best-effort, not comprehensive.

5. **Error classification + safe fallback** (packages/workflows/src/executor-shared.ts:13-50) — The executor catches node errors and classifies them (git conflicts, auth failures, timeout, unknown). Unknown errors abort the run after 3 consecutive failures (UNKNOWN_ERROR_THRESHOLD). Helps prevent silent loops.

6. **No shell sed / no global file rewrites** — Bash nodes run arbitrary shell; there is **no safety layer**. The workflow author is responsible for safe scripts. Archon does not prevent `sed -i 's/foo/bar/g' *` or other dangerous patterns.

**What Archon explicitly does NOT do:**
- No prompt injection filtering on LLM output (the assumption is the LLM is trusted).
- No allowlist of safe bash commands (users can run anything).
- No sandbox for workflows themselves — a malicious YAML can read vault secrets, delete repos, etc.
- No "dry-run by default" mode (workflows execute immediately).

## 10. Where Anvil already has parity

Anvil **today (v2.2.12)** covers several of these capabilities:

| Archon feature | Anvil equivalent | Citation |
|---|---|---|
| DAG workflow execution | crates/commands/src/skill_chaining.rs (depth-3 chains) | Chains via `chains_to:` frontmatter, not full DAG |
| Worktree isolation per run | crates/tools/src/worktree_ops.rs | Anvil creates per-task worktrees |
| Cron / scheduled tasks | crates/runtime/src/cron.rs (basic 5-field cron) | Implements `CronEntry`, daemon thread, LLM tool surface |
| Daily report generation | crates/runtime/src/daily.rs | Daily summary already exists |
| Hooks (pre/post tool) | crates/runtime/src/hooks.rs | Session hooks, pre/post tool hooks |
| Goals / nominations | crates/runtime/src/goals.rs | Pattern detection pipeline |
| File + command caches | crates/runtime/src/{file_cache,command_cache}.rs | Token-economy caching from v2.2.11 |
| Skills with dependencies | crates/commands/src/skill_chaining.rs | `chains_to: depth` traversal exists |
| Marketplace distribution | crates/runtime/src/hub.rs | AnvilHub install plumbing exists |
| Secrets / vault | crates/runtime/src/vault/storage.rs | Flat-label vault with all-or-nothing unlock |
| SSH operations | crates/runtime/src/ssh/ | SSH tab from v2.2.12 |
| Per-tab parallel inference | crates/tools/ | Shipped v2.2.12 per-tab |
| Session journal framework | ROADMAP.md Tier 3 (in progress, v2.3) | Durable session journal not yet landed |
| Resume by name | crates/runtime/src/session.rs | Session friendly names in v2.2.12 |

## 11. Where Anvil has gaps relative to Archon

Concrete gaps, with citations:

| Gap | Archon implementation | Where Anvil would need it |
|---|---|---|
| **Full DAG execution with fan-out/join** | packages/workflows/src/dag-executor.ts (topological sort, Promise.allSettled for independent nodes) | Anvil has `chains_to` (linear); Archon has DAG with `depends_on`, `trigger_rule` (all_success/one_success/all_completed), and parallel node execution. No equivalent in crates/commands/ or crates/runtime/. |
| **YAML workflow format with typed schemas** | packages/workflows/src/schemas/workflow.ts + packages/workflows/src/loader.ts | Anvil defines routines as TOML (planned, not landed); Archon uses YAML with Zod validation. Need new `crates/runtime/src/routines/loader.rs` for TOML parsing and DAG conversion. |
| **Node output substitution ($nodeId.output)** | packages/workflows/src/dag-executor.ts:1030-1080 | Anvil has variable substitution ($1, $2, $ARTIFACTS_DIR) but no per-node output injection. Would need map capture in routine executor. |
| **Loop nodes with until conditions** | packages/workflows/src/schemas/loop.ts, dag-executor.ts (loop iteration detection) | Anvil has no loop-until-condition construct. Would need `until: <json-detection>` schema and evaluator. |
| **Approval gates with optional reason capture** | packages/workflows/src/schemas/dag-node.ts (ApprovalNode), execution at dag-executor.ts | Anvil has `approval` in initial designs but not implemented. Need `capture_response: true` to store user feedback as `$node-id.output`. |
| **Interactive mode (foreground execution)** | packages/workflows/src/executor.ts (interactive check) + web adapter SSE | Anvil is interactive-only (user at terminal); no batch/background mode. Archon can dispatch workflows "fire-and-forget" while user does other work. Opposite problem: Anvil needs background execution *option*, Archon needs foreground mode. |
| **Multi-platform adapter dispatch (Slack/Telegram/GitHub/Discord)** | packages/adapters/ (5 adapters) | Anvil runs only interactively in Claude Code CLI. No Slack/Telegram/Discord/GitHub integration. Not a gap for Anvil's design (single-user, local-first), but architectural difference. |
| **Webhook-driven execution** | packages/adapters/src/forge/github/ (webhook handler) | Anvil has no webhook receiver. Routines would need to integrate with an external webhook service or polling task. |
| **Long-running daemon (launchd/systemd)** | ROADMAP Tier 2, item 20-21 (planned, not shipped) | Archon: server-based, users run `archon serve` or Docker. Anvil: CLI-only, interactive. Tier 2 (routines daemon, §17) designs exactly this: separate `anvil routined` process + launchd/systemd unit. |

## 12. Adopt / skip recommendation

**Recommendations per gap, tied to Anvil's design principles:**

| Gap | Recommendation | Rationale |
|---|---|---|
| **Full DAG with fan-out/join** | **ADOPT** (critical path, Tier 2) | Archon's DAG model is load-bearing for the self-improving-MCP use case (Tier 4 item 1: pattern analyzer runs as a routine, fans out validation runs). Anvil's roadmap already commits to DAG for Tier 5 marketplace (agent composition). The implementation cost is ~1 day once the TOML loader lands. The five-parallel-review pattern in `archon-idea-to-pr.yaml:104-133` proves fan-out value. Build order: ship TOML loader (Tier 2 item 5), then add DAG support to routine executor (new item). Per `ROUTINES-ADOPTION-NOTES.md` §7, the three-phase token-budgeted context resolver (items 7-9) provides 80% of DAG value without a graph engine — ship that first, DAG second. |
| **YAML workflow format** | **SKIP** | Anvil uses TOML for routines (planned, §8 of adoption notes) because it matches existing vault/config conventions and has no indentation pitfalls. Archon chose YAML for frontend drag-drop builder (packages/web/src/routes/WorkflowBuilderPage). Anvil's adoption notes explicitly recommend TOML + skill-frontmatter bridge (§8), not YAML. The cost to adopt Archon's YAML format is ~half a day of translation, but it contradicts Anvil's standardization on TOML. Keep TOML. |
| **Node output substitution** | **ADOPT** (medium priority, Tier 2 items 4-6) | Essential for context_from and multi-step routines. The adoption notes (§6-7.5) outline exactly this: `context_from` pulls last output of upstream routines; three-phase token-budgeted resolver truncates if needed. Archon's implementation is clean (`dag-executor.ts:1030-1080`); mirror for routine packets. Cost: ~half a day for basic version, 1 day for token-budgeted resolver. |
| **Loop until conditions** | **ADOPT** (lower priority, Tier 2 point release) | Archon's loop nodes (packages/workflows/src/schemas/loop.ts:5-40) support `until: <string>` JSON detection. Anvil's adoption notes (§12.5) suggest "imperative REQUIRED SUB-SKILL prose" as the complement — the routine prompt includes "repeat the cycle until X," and the agent detects the condition in its output. This is a hybrid: no explicit until-detection, but system prompt nudges the agent. Archon's explicit detection is safer. Cost: ~2 hours for basic until-detection (parse agent output for JSON marker, advance iteration count). Worth landing in a 2.4.x point release. |
| **Approval gates with reason capture** | **ADOPT** (high priority, Tier 2 items critical path) | Archon's approval nodes (packages/workflows/src/schemas/dag-node.ts:70-93) pause execution and collect user feedback. Anvil's adoption notes (§8) mention approval as a gate but don't commit to reason capture. Archon proves the pattern: `capture_response: true` stores user text as `$node-id.output` for downstream nodes. Cost: ~3 hours to wire into routine executor + system prompt injection of `$REJECTION_REASON`. Ship with the delivery layer (item 6 in adoption notes, 1-2 days). |
| **Interactive vs background mode** | **SKIP (partially)** | Anvil is interactive-only by design (single user, local CLI). Archon's background mode (fire-and-forget from web, while user continues) is a different execution model. The Tier 2 daemon decision (Q5 in adoption notes) resolves this: separate `anvil routined` process handles background execution; interactive CLI stays foreground. Anvil doesn't "adopt" Archon's background mode; it builds its own daemon. Zero cost to skip Archon's web-dispatch pattern; Anvil's daemon solves the same problem with a different architecture. |
| **Multi-platform adapters** | **SKIP** | Archon's Slack/Telegram/Discord adapters are beautiful but orthogonal to Anvil's single-user focus. Tier 7 (v2.8, collaboration) is when Anvil gains Telegram/Slack delivery *for routines*, not general chat interfaces. Archon's adapter architecture (packages/adapters/) is worth understanding if that tier lands, but skip for v2.4. Cost to skip: none — Tier 2 doesn't depend on it. |
| **Webhook-driven execution** | **DEFER to Tier 2 item 27** | Archon shows the pattern (packages/adapters/src/forge/github/); Anvil's adoption notes (§9) call this a 2.4.x point release ("webhook trigger"). Worth landing after the base daemon (Tier 2 item 27, ~2 days). Skip for initial launch. |
| **Long-running daemon** | **ADOPT (critical)** | This is Tier 2 items 20-24 of `ROADMAP.md`. Archon's `archon serve` is a server-based daemon; Anvil's will be `anvil routined` + launchd/systemd + consent gate. Archon's implementation is simpler (it's the HTTP server); Anvil's is more complex (separate daemon, explicit consent, per-routine enable/disable). The adoption notes (§17) detail the consent UX precisely because this is the trust boundary. Cost: 3-4 weeks solo work, including consent, daemon binary, daemon lifecycle commands, and visibility (`anvil routine status`). **Non-negotiable for Tier 2.** |

**Summary:** Adopt the DAG execution model (plan for ~1.5 days once TOML lands), node output substitution (plan for ~1 day), and approval-gate reason capture (plan for ~3 hours). Defer loop-until and webhook triggers to 2.4.x point releases. Skip YAML (use TOML), multi-platform adapters, and interactive-mode architecture. The daemon work is Tier 2 critical path, separate from Archon adoption.

---

**Total citation count: 68 file:line references**
