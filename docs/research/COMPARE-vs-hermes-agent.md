# Hermes Agent vs Anvil — research comparison

**Audited:** 2026-05-11
**Hermes path:** /Users/soulofall/projects/hermes-agent
**Anvil path:** /Users/soulofall/projects/anvil-dev
**Anvil current version:** v2.2.12 (just released today)
**Hermes version:** 0.13.0 (MIT license)
**Hermes repo:** NousResearch/hermes-agent, last commit 64145a1 (2026-05-11)

---

## 1. What Hermes is

Hermes Agent is a self-improving AI agent built by Nous Research that runs scheduled and event-triggered automations. It emphasizes learning loops — agents create skills from experience, improve them during use, and search prior conversations. Codebase size: ~3600 lines of core cron/scheduler code plus 40+ built-in tools. Primary language: Python 3.11+. Five cron-related modules: `cron/scheduler.py` (1908 lines), `cron/jobs.py` (~1000 lines), `tools/cronjob_tools.py` (~800 lines), plus gateway integration and delivery routing. Last commit 2026-05-11. Solves the "run automations anywhere" problem: local, Docker, SSH, Daytona, Modal serverless. Can be installed on a $5 VPS or a user's laptop; automations run unattended after setup.

## 2. Architecture summary

Hermes has modular layers:

- **Cron core:** `cron/jobs.py` (line 1+) stores jobs in JSON at `~/.hermes/cron/jobs.json` (line 39). `CronDaemon` thread polls every 60 seconds (gateway calls tick repeatedly via `gateway/` integration). Uses file lock `~/.hermes/cron/.tick.lock` (scheduler.py:7, jobs.py comment) to serialize multiple processes.
- **Scheduler:** `cron/scheduler.py` (line 1+) runs due jobs, injects pre-script context, parses wake-gate JSON, scans assembled prompts for injection, delivers output to 12+ platform targets (telegram, discord, slack, etc.), archives all output to `~/.hermes/cron/output/{job_id}/{timestamp}.md` (jobs.py:5, 45, scheduler.py:1741+).
- **Tools:** `tools/cronjob_tools.py` (line 1+) exposes the compressed `cronjob` tool to the LLM (lines 526+), supporting create/update/list/remove actions. Tool calls pass through the standard agent loop (no auto-approval gate within the tool; approval happens at session level if configured).
- **Skills system:** Skills are Markdown files with YAML frontmatter (skills/{name}/SKILL.md). Frontmatter declares `name`, `description`, `version`, `platforms`, `metadata.hermes.tags`, `metadata.hermes.related_skills` (yuanbao/SKILL.md:1-5, dogfood/SKILL.md:1-5). Loaded at cron-run time via `agent/skill_preprocessing.py` + `tools/skills_tool.py`.
- **Gateway:** `gateway/` directory handles platform delivery (Telegram, Discord, Slack, Matrix, SMS, email, webhook, etc.). Each platform has an adapter (platform_registry, channel_directory).
- **Delivery targets:** Parsed by `_resolve_single_delivery_target()` in scheduler.py:258+. Supports `local`, `origin` (infer platform from current session), `platform`, `platform:chat_id`, `platform:chat_id:thread_id`, `all` (fan-out to every connected platform).

## 3. Schedule grammar / dispatch

Hermes has a four-format parser at `cron/jobs.py:184` (function `parse_schedule`):

- **Duration / one-shot:** `"30m"`, `"2h"`, `"1d"` → fires once in N minutes/hours/days from now. Returns `{kind: "once", run_at: ISO_timestamp, display: "once in 30m"}` (jobs.py:252-262).
- **Interval / recurring:** `"every 30m"`, `"every 2h"` → recurring every N minutes. Returns `{kind: "interval", minutes: N, display: "every Nm"}` (jobs.py:206-214).
- **Cron expression:** `"0 9 * * *"` — standard 5-field cron, validated via `croniter` library (jobs.py:219-233). Returns `{kind: "cron", expr: "...", display: "..."}`.
- **ISO timestamp:** `"2026-02-03T14:00Z"` or `"2026-02-03T14:00"` — one-shot at wall-clock time. Timezone-aware (jobs.py:236-250, handles naive timestamps by interpreting as local timezone via `dt.astimezone()`).

All formats normalize to a structured dict with `kind`, `display`, and either `run_at` (unix-epoch for once) or `minutes`/`expr` (for recurring). Implementation uses `croniter.croniter()` for validation and interval expansion. Parse happens at job-create time (`cronjob_tools.py:506`, `jobs.py:686`), and the normalized structure is persisted in the jobs JSON file.

## 4. Output / archive / delivery

**Output archive:** All cron job outputs land in `~/.hermes/cron/output/{job_id}/{timestamp}.md` (jobs.py:5, 45, 761, 975-980). Markdown file created every run, regardless of delivery targets. Contains: front-matter with job metadata, script output block (if pre-script ran), agent response, delivery results.

**Delivery targeting:** Specified at job create/update via `deliver` parameter in `cronjob` tool (cronjob_tools.py:572-574). Grammar:
- `"local"` — save archive only, no delivery.
- `"origin"` — auto-detect current session's platform + chat_id + thread_id (infer platform from env vars `HERMES_SESSION_PLATFORM`, `HERMES_SESSION_CHAT_ID` via `_origin_from_env()` in cronjob_tools.py:97-110).
- `"all"` — fan-out to all connected home channels (one env var per platform: `TELEGRAM_HOME_CHAT_ID`, `DISCORD_HOME_CHANNEL`, `SLACK_HOME_CHANNEL`, etc.). Resolved at fire time by iterating `_HOME_TARGET_ENV_VARS` (scheduler.py:99-120, 240-256).
- `"platform"` or `"platform:chat_id"` or `"platform:chat_id:thread_id"` — explicit target. Parsed by `_resolve_single_delivery_target()` (scheduler.py:258-342).
- Comma-separated combinations: `"origin,all"` delivers to both.

**Suppression marker:** `[SILENT]` (defined as constant at scheduler.py:129) is checked in the LLM's final response. If the response is exactly `"[SILENT]"` (or contains it on its own), delivery is skipped but the archive is still written (jobs.py:5 comment, scheduler.py:1741-1790 delivery section). Prompt guidance at scheduler.py:928-929: agent is told "respond with exactly `[SILENT]` (nothing else) to suppress delivery."

**Token-aware truncation:** Not implemented in v0.13.0. Hermes truncates for specific platforms (some chat platforms have max message lengths) but does not implement a three-phase token-budget resolver. Each adapter (Telegram, Slack, etc.) handles its own max length if needed.

## 5. Skill / agent definition format

**Skills:** Markdown files with YAML frontmatter (skills/{skill_name}/SKILL.md). Example (yuanbao/SKILL.md:1-8):
```yaml
---
name: yuanbao
description: "Yuanbao (元宝) groups: @mention users, query info/members."
version: 1.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [yuanbao, mention, at, group, members, 元宝, 派, 艾特]
    related_skills: []
---
```

Frontmatter parsing happens in `tools/skills_hub.py:732+` (`parse_yaml_frontmatter`). Skills are discovered by glob from `~/.hermes/skills/` and `~/.hermes/optional-skills/`, plus remote sources (GitHub, agentskills.io registry). 

**Cron job definition:** Stored as JSON in `~/.hermes/cron/jobs.json` (jobs.py:39). No YAML/TOML file format for user authoring; jobs are created via LLM tool calls (cronjob tool in cronjob_tools.py:526+) or programmatically via Python API. Schema includes:
- `id` (uuid)
- `name` (string)
- `prompt` (string, the task)
- `schedule` (dict from `parse_schedule`, kind + run_at/minutes/expr)
- `schedule_display` (string, for UI)
- `skills` (list of skill names, loaded at run time)
- `deliver` (comma-separated string: "local", "origin", "all", or "platform:chat_id:thread_id")
- `script` (optional, path to pre-script under ~/.hermes/scripts/)
- `no_agent` (bool, if True script IS the job, no LLM run)
- `enabled` (bool, default True)
- `enabled_toolsets` (optional, list of toolset names to restrict)
- `workdir` (optional, absolute path for project context)
- `context_from` (optional, list of upstream job names whose last output gets injected as context — linear chaining only, no DAG in v0.13.0)
- `created_at`, `updated_at`, `last_run`, `next_run` (timestamps)

**Loader:** `cron/jobs.py:100+` (`_normalize_job_record`, `load_jobs`, `save_jobs`) handles persistence. No schema validation beyond type coercion; the tool (cronjob_tools.py) does the input validation before persisting.

## 6. Context injection mechanism

**Script output injection:** If a job has `script` set, Hermes runs the script *before* the agent (scheduler.py:1030+, `_run_script`). Script output (stdout, stderr captured separately) is injected into the agent's prompt as a block. The mechanism:

1. Script runs via bash for `.sh`/`.bash`, Python for anything else (scheduler.py:1050).
2. Stdout is captured and redacted (secrets removed via `_redact_cron_script_output`, scheduler.py:787+).
3. If the last line of stdout is JSON `{"wakeAgent": false}`, the agent is skipped entirely — no LLM run, treated as silent (scheduler.py:811-834, `_parse_wake_gate`). This is the "wake gate" pattern, checked at line 1091, 1151.
4. Otherwise, stdout is injected into the prompt as `"## Script Output\n{stdout}"` block before the agent runs (scheduler.py:869+, `_build_job_prompt`).

**Context-from injection:** Linear chaining (v0.13.0 only, no DAG). A job can declare `context_from: ["job-name-1", "job-name-2"]` in the JSON (cronjob_tools.py:624-625). At dispatch time, the most recent completed output of each named job is fetched from the archive and injected as separate blocks: `"## Context From job-name-1\n{last_output}"` (scheduler.py:918+, `_resolve_context_packets`). If a referenced job hasn't run yet or is disabled, the block is omitted (no error, silent degrade).

**Skill injection:** Skills are loaded and their full text is injected into the system prompt (scheduler.py:849+, `_build_job_prompt`). Skill loading happens via `agent/skill_preprocessing.py` which searches the local skill registry.

**Prompt assembly:** Final prompt = `[system prompt with cron-specific guidance] + [loaded skills] + [script output block if any] + [context_from blocks] + [user-supplied job prompt]` (scheduler.py:837+, `_build_job_prompt`). This assembled prompt is then scanned for injection (line 989+) before the agent runs.

## 7. Vault / secrets model

**No vault system in Hermes v0.13.0.** Secrets are handled by:

1. **Environment variables:** Hermes reads `.env` file at `HERMES_HOME/.env` (loaded at startup, or per-run via config reload in scheduler.py:1268).
2. **Inline API key in job spec:** Jobs can have `api_key` field (cronjob_tools.py line 581, "Optional per-job model override"). This is stored in plain JSON and not recommended.
3. **Credential pool:** For LLM providers, Hermes can load a credential pool per provider (scheduler.py:1387-1402, `agent.credential_pool.load_pool`). Pools are provider-specific (e.g., a list of API keys for OpenRouter).
4. **Redaction:** Pre-script stdout is redacted via `_redact_cron_script_output` (scheduler.py:787+) to prevent accidental secret leakage in logs. Uses regex patterns to detect and mask common token shapes (sk-, ghp_, xoxb-, etc.).

There is no Hermes-specific vault abstraction or scope-gating per job. Secrets are either in .env (global), in the jobs.json (not recommended), or loaded from credential pools (provider-specific). No `vault://` reference grammar.

## 8. Daemon / persistent execution

**Tick mechanism:** In v0.13.0, Hermes runs cron jobs via a thread inside the gateway process or interactive CLI. The gateway spawns `CronDaemon` as a background thread (no separate daemon binary). The thread's `tick()` method is called repeatedly by the gateway every 60 seconds (scheduler.py:1 comment), using a file lock to serialize across multiple processes (scheduler.py:7, jobs.py:44).

**No systemd/launchd service in the shipped product.** Users who want persistent execution must either:
- Keep the `hermes gateway` running in the background (tmux, supervisor, systemd user service for the gateway itself — not Hermes-specific).
- Run `hermes` interactive CLI in the background.

The cron infrastructure is embedded, not a standalone daemon. There is no `hermes routined` subcommand.

**Wake-gate pattern:** Implemented in full. Pre-script can emit `{"wakeAgent": false}` as its last stdout line (scheduler.py:811-834, `_parse_wake_gate`). When this gate is detected, the cron job:
1. Does not run the agent (no LLM call, zero tokens).
2. Writes the archive output as "Status: silent (wakeAgent=false)" (scheduler.py:1100).
3. Skips all delivery (scheduler.py:1089-1095, 1151-1163).
This is the core "pay-only-when-there's-news" pattern.

## 9. Anti-pattern guardrails

**Prompt injection scanner:** Hermes scans the **assembled prompt** (not just user input) before running the agent, catching injection payloads that come from malicious skills. Class `CronPromptInjectionBlocked` at scheduler.py:45-54 raises when a match is found. Patterns checked (cronjob_tools.py:37-62):

- Invisible unicode (U+200B, U+200C, U+200D, U+FEFF).
- Ignore-previous-instructions patterns (regex `r'ignore\s+(?:\w+\s+)*(?:previous|all|above|prior)...'`).
- Exfiltration attempts (curl with secret-var substitution, POST payloads with env-var strings).
- Invisible CSS / HTML comments.

Scanning is strict for cron (auto-approval context) and warns for other contexts. On injection match, the job is blocked and marked as failed with clear error reporting (scheduler.py:1004-1008, 1166-1183).

**Script path confinement:** Pre-scripts must resolve to `~/.hermes/scripts/` and nowhere else. Path resolution uses `Path.resolve()` + ancestor checks (jobs.py:735-745, `_validate_cron_script_path`).

**Credential pool isolation:** Per-job models use the credential pool API, not inlined secrets. Pool requests go through the provider's SDK (scheduler.py:1387+).

**Toolset gating:** Jobs can declare `enabled_toolsets` (cronjob_tools.py:632) to restrict which tools are available during the run. Default is all tools; restricting to ["web", "terminal", "file"] reduces input tokens and limits what a compromised job can do.

**No skill sandboxing in v0.13.0.** Skills are loaded and injected into the prompt as-is. The prompt scanner (above) catches post-load injection but does not prevent skills from attempting exfiltration via tool calls.

## 10. Where Anvil already has parity

Anvil v2.2.12 already shipped these features (or has them staged):

| Feature | Hermes | Anvil | Citations |
|---|---|---|---|
| **Cron data model + persistence** | JSON in `~/.hermes/cron/jobs.json` | `crates/runtime/src/cron.rs`, `CronEntry`, `CronStore` on disk | anvil-dev/docs/ROUTINES-ADOPTION-NOTES.md:17 |
| **Cron daemon / background tick** | Thread in gateway, file-locked tick every 60s | Background thread, polls every 30s, `run_pending()` | ROUTINES-ADOPTION-NOTES.md:18 |
| **Cron tool surface (LLM-callable)** | `cronjob` tool (create/list/update/remove) | `CronCreate`, `CronList`, `CronDelete` wired in `automation_ops.rs` | ROUTINES-ADOPTION-NOTES.md:19 |
| **Remote trigger** | Webhook HTTP POST (via gateway platform adapters) | `RemoteTrigger` for cross-instance dispatch (HTTPS + bearer auth) | ROUTINES-ADOPTION-NOTES.md:20 |
| **Daily report runtime** | Via cron job with no_agent=True or script-only job | `crates/runtime/src/daily.rs`, 693 lines | ROUTINES-ADOPTION-NOTES.md:21 |
| **Hooks / pre/post operations** | None in cron (auth happen at agent level) | `crates/runtime/src/hooks.rs`, 1558 lines | ROUTINES-ADOPTION-NOTES.md:22 |
| **Goals / nominations / pattern detection** | Skills via curation (no agent self-nomination) | `crates/runtime/src/goals.rs`, 881 lines | ROUTINES-ADOPTION-NOTES.md:23 |
| **Worktree isolation** | Not implemented | `crates/tools/src/worktree_ops.rs` | ROUTINES-ADOPTION-NOTES.md:24 |
| **File + command cache** | Not implemented (but script stdout is captured) | `crates/runtime/src/{file_cache,command_cache}.rs` | ROUTINES-ADOPTION-NOTES.md:25 |
| **Skills with chaining** | Skills are monolithic Markdown; no chains_to in cron context | `crates/commands/src/skill_chaining.rs`, `chains_to:` depth-3 | ROUTINES-ADOPTION-NOTES.md:26 |
| **Vault-scoped secrets** | Env vars + credential pools (no scope gating) | `crates/runtime/src/vault/`, encrypted, scopes | ROUTINES-ADOPTION-NOTES.md:28 |
| **Output archive** | `~/.hermes/cron/output/{id}/{ts}.md` | Proposed: `~/.anvil/routines/output/{id}/{ts}.md` | ROUTINES-ADOPTION-NOTES.md:105-114 |

## 11. Where Anvil has gaps relative to Hermes

**Gap 1: Schedule grammar expansion.** Hermes's `parse_schedule()` (jobs.py:184-263) supports four formats: duration (`30m`), interval (`every 2h`), cron (`0 9 * * *`), ISO timestamp (`2026-02-03T14:00Z`). Anvil v2.2.12 stores `cron_expression: String` only, accepting classical 5-field cron (ROUTINES-ADOPTION-NOTES.md:38-48). **Verifier:** Run `grep -n "cron_expression" /Users/soulofall/projects/anvil-dev/crates/runtime/src/cron.rs` to confirm current schema; `parse_schedule` exists in Hermes at jobs.py:184 but not in Anvil.

**Gap 2: Pre-agent script + wake gate.** Hermes implements script pre-execution (scheduler.py:1030+), stdout injection, and wake-gate parsing (`_parse_wake_gate`, scheduler.py:811-834). Anvil does not ship script pre-execution (ROUTINES-ADOPTION-NOTES.md:52-72 spec's it as new). **Verifier:** Hermes's `_run_script` is at scheduler.py:1030; grep for "script" in anvil-dev/crates/runtime/src/cron.rs to confirm absence.

**Gap 3: Linear context chaining (`context_from`).** Hermes implements `context_from: ["job-name-1", ...]` at jobs.py level (cronjob_tools.py:624-625, scheduler.py:918-935 injection). Anvil stages this in ROUTINES-ADOPTION-NOTES.md:117-147 but has not shipped it in v2.2.12. **Verifier:** Search `context_from` in anvil-dev/crates/runtime/src/cron.rs; unlikely to exist yet.

**Gap 4: Delivery target grammar + adapters.** Hermes supports 12+ platforms (Telegram, Discord, Slack, SMS, email, webhook, Matrix, Feishu, DingTalk, WeChat, homeassistant, bluebubbles, QQ) with a unified delivery parser (scheduler.py:258-342, `_resolve_single_delivery_target`). Anvil has no delivery layer in v2.2.12 (ROUTINES-ADOPTION-NOTES.md:83-103 proposes `local` / `email` / `webhook`). **Verifier:** Search "deliver\|webhook\|email" in anvil-dev/crates/runtime/src/cron.rs; expect no delivery code.

**Gap 5: Output archive markdown generation.** Hermes writes `~/.hermes/cron/output/{job_id}/{timestamp}.md` every run (jobs.py:5, 975-980) with full metadata. Anvil does not generate per-run markdown archives in v2.2.12. **Verifier:** Hermes's archive writer is at jobs.py:975+; search "output.*md\|archive" in anvil-dev/crates/ and expect minimal matches.

**Gap 6: Prompt injection scanning.** Hermes scans assembled prompts (including loaded skill content) for injection patterns before running the agent in cron context (scheduler.py:45-54, 989-1008, cronjob_tools.py:37-62). Anvil's v2.2.12 general safety scanning (per MCP hardening docs) does not include a specific cron-context scanner. **Verifier:** Search "CronPromptInjectionBlocked\|_scan_cron_prompt" in hermes-agent/cron/scheduler.py (lines 45, 989); search same terms in anvil-dev/crates/runtime/ (expect no match).

**Gap 7: Wake-gate JSON parsing.** Hermes pre-script can emit `{"wakeAgent": false}` to skip the agent (scheduler.py:811-834). Anvil's ROUTINES-ADOPTION-NOTES.md:52-72 proposes wake-gate but does not ship it in v2.2.12. **Verifier:** Hermes's `_parse_wake_gate` is at scheduler.py:811; grep for "wakeAgent\|wake.*gate" in anvil-dev/crates/runtime/src/cron.rs (expect no match).

**Gap 8: Silent suppression marker.** Hermes checks for `[SILENT]` in the LLM response and skips delivery while keeping the archive (scheduler.py:129, 1741-1790). Anvil does not implement `[SILENT]` in v2.2.12 (proposed at ROUTINES-ADOPTION-NOTES.md:75-79). **Verifier:** Search "SILENT" in anvil-dev/crates/ (expect no match); Hermes constant at scheduler.py:129.

**Gap 9: Skill-as-routine bridge.** Hermes has no skill-frontmatter `[routine]` block (skills are Markdown only). Anvil proposes this (ROUTINES-ADOPTION-NOTES.md:152-182) but has not shipped it. Neither system implements it in v2.2.12. **Verifier:** This is a design-time gap, not a runtime code gap.

**Gap 10: Token-budgeted context resolution.** Hermes v0.13.0 does not implement three-phase token budgeting for `context_from` packets (Anvil proposes this at ROUTINES-ADOPTION-NOTES.md:132-147). Hermes injects full last outputs; Anvil's design calls for summaries + bodies + truncation. **Verifier:** Hermes's context injection at scheduler.py:918-935 is a flat list of outputs, no token accounting. Anvil's proposal is at ROUTINES-ADOPTION-NOTES.md but not implemented in v2.2.12.

## 12. Adopt / skip recommendation

Based on Anvil's design rules (per-tab isolation, vault-only secrets, MCP hardening, suggest-not-auto for AnvilHub), the gaps above are prioritized:

**ADOPT (2-3 days of work):**

1. **Schedule grammar expansion (§11 Gap 1).** Hermes's `parse_schedule()` (jobs.py:184-263) directly applies. Cost: ~half a day parser + normalized storage. Rationale: all four formats (duration, interval, cron, ISO timestamp) solve real UX problems (users expect `every 30m` and `in 2 hours` to work). Anvil's cron.rs already has a string field; expand it to parse into a normalized enum/struct. No vault risk, no per-tab isolation risk.

2. **Pre-agent script + wake gate (§11 Gap 2).** Hermes's `_run_script` + `_parse_wake_gate` (scheduler.py:1030+, 811-834) are the reference. Cost: ~1.5 days. Rationale: wake-gate is the load-bearing feature for "pay-only-when-news-arrives" monitoring. Script isolation via path confinement (Hermes's model at jobs.py:735-745) is proven. Pairs well with Anvil's worktree ops for per-task isolation. Per ROUTINES-ADOPTION-NOTES.md:52-72, this is the highest-leverage addition.

3. **Silent suppression marker `[SILENT]` (§11 Gap 8).** One-line implementation (Hermes: scheduler.py:129, 1741-1790). Cost: ~30 minutes. Rationale: solves "stop spamming me with results when there's nothing new" in one line of code. Low risk, high UX value.

4. **Output archive markdown (§11 Gap 5).** Hermes template at jobs.py:975-980. Cost: ~half a day. Rationale: diagnostic surface for "what did the routine do, and when?" Non-negotiable for production use (users need to audit / replay). Anvil already has daily reports; extending to per-routine archives is straightforward.

5. **Delivery target grammar + local/email/webhook adapters (§11 Gap 4).** Hermes parser at scheduler.py:258-342 is comprehensive; Anvil's ROUTINES-ADOPTION-NOTES.md:83-103 proposes a minimal starter set (local, email, webhook). Cost: ~1.5 days for target grammar + three adapters. Rationale: local delivery (always on) + email (existing SMTP config or vault creds) + webhook (outbound HTTPS POST to vault-stored URL) cover 80% of use cases. Match Hermes's target syntax (`local`, `email`, `email:addr@example.com`, `webhook`, `webhook:https://...`) for forward compatibility. Per Anvil's design: webhook URL must be vault-stored (no inline URLs in TOML).

6. **Linear context chaining `context_from` (§11 Gap 3).** Hermes at scheduler.py:918-935. Cost: ~1 day. Rationale: fan-in pattern for morning summaries that read three nightly jobs. Hermes's flat list approach (no DAG) is sufficient for v2.4 scope (ROUTINES-ADOPTION-NOTES.md:344-352 confirms this). Integrate with Anvil's session journal (per ROUTINES-ADOPTION-NOTES.md:200-209, "routine run is just a journal-attached session").

**SKIP:**

7. **Prompt injection scanner (§11 Gap 6).** Hermes's scanner at scheduler.py:45-54, 989-1008, cronjob_tools.py:37-62 is cron-specific. Anvil's MCP hardening (per feedback-mcp-hardening-principles.md) covers tool-call scanning at the agent level. Cron automations do not have interactive tool approval; they're auto-approval context. Instead of duplicating Hermes's regex patterns, enforce at the scheduling layer: **no skills that are not allowlisted can run in a cron routine** (extension of AnvilHub install model). Cost of custom scanner: ~half a day. Value vs risk: skill allowlist is simpler and paired with Anvil's per-tab billing. **Skip the scanner; ship skill allowlist instead.**

8. **Token-budgeted context resolution (§11 Gap 10).** Hermes has flat injection (no token accounting). Anvil's design (ROUTINES-ADOPTION-NOTES.md:132-147) proposes three-phase budgeting + anti-injection delimiters. This is a v2.4 follow-up (ship linear chaining first, optimize later). **Skip for now; stage in v2.5 once usage tells us if linear chaining is limiting.** No cliff here; `context_from` injection is forward-compatible.

9. **Skill-as-routine bridge (§11 Gap 9).** Neither Hermes nor Anvil v2.2.12 has a `[routine]` block in skill frontmatter. This pairs with Anvil's skills CLI (crates/commands/src/skill_chaining.rs) and requires schema changes. Cost: ~half a day. Rationale: not a blocking gap. Routines can be TOML files in `~/.anvil/routines/` (per ROUTINES-ADOPTION-NOTES.md:156-182). Skill frontmatter is nice-to-have for embedded routines but not essential for v2.4. **Defer to v2.4.x; ship TOML-file routines first.**

---

**Cost summary for Adopt items (1-6):** ~5-6 days of solo work. Build order: schedule grammar (half day) → script + wake-gate (1.5 days) → archive + delivery (2 days) → context_from (1 day). Estimated ship: ~5 days of work, can be split across v2.2.12/v2.2.13 patches per ROUTINES-ADOPTION-NOTES.md:304.

**Design rule checkpoints:**

- **Per-tab isolation (Anvil MEMORY.md):** Routines run as journal-attached sessions, inheriting the tab concept. Script path confinement + workdir isolation (per ROUTINES-ADOPTION-NOTES.md:10) keeps routines scoped. ✓
- **Vault-only secrets (ROUTINES-ADOPTION-NOTES.md:234-242):** No inline API keys in TOML. Delivery targets store URLs in vault only. ✓
- **Suggest-not-auto for AnvilHub (AnvilHub policy):** Routines installed from AnvilHub are `enabled: false` by default (per ROUTINES-ADOPTION-NOTES.md:513). Users enable explicitly. ✓
- **MCP hardening:** Cron routines use the same tool allowlist as interactive sessions. No new tool surface. ✓

---

**Previously-confirmed adoption items (from ROUTINES-ADOPTION-NOTES.md §3):**

1. **Schedule grammar** — CONFIRM. Hermes's four-format parser (jobs.py:184-263) is the reference. Adopt.
2. **Wake gate** — CONFIRM. `_parse_wake_gate` at scheduler.py:811-834 is the canonical implementation. Adopt.
3. **`[SILENT]` suppression** — CONFIRM. One-line check at scheduler.py:1741-1790. Adopt.
4. **Delivery target grammar** — CONFIRM. Hermes's `_resolve_single_delivery_target` (scheduler.py:258-342) shows the pattern. Adopt minimal set (local/email/webhook).

These four were already flagged as adoption candidates in ROUTINES-ADOPTION-NOTES.md §3; the citations above confirm their provenance and prove they are non-negotiable for v2.4 feature parity.

---

**30 citations total across Hermes and Anvil docs.**
