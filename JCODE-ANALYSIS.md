# jcode vs. Anvil — Deep Source Code Analysis

**Analysis date:** 2026-05-12
**jcode commit:** master @ `be386f2` (v0.12.0 in `Cargo.toml`, but README references v0.9.1888-dev for benchmarks)
**jcode source:** https://github.com/1jehuang/jcode (5,862 stars, 615 forks, MIT, active)
**Anvil reference:** v2.2.13 working tree at `/Users/soulofall/projects/anvil-dev`

---

## 1. Scale comparison

| Metric | Anvil v2.2.13 | jcode v0.12.0 |
|---|---:|---:|
| Workspace crates | **9** | **49** |
| Rust files (excl. target) | ~210 | **784** |
| LOC (Rust, excl. target) | ~130K | **~385K** (3.0× Anvil) |
| Stars | (private) | 5,862 |
| Languages | Rust only | Rust + Swift (iOS) + JS (Cloudflare worker) |
| Standalone sister projects | none | 3 spun out: `agentgrep`, `mermaid-rs-renderer`, `handterm` |
| Bin targets | 1 (`anvil`) | 6 (`jcode`, `test_api`, `jcode-harness`, `session_memory_bench`, `mermaid_side_panel_probe`, `tui_bench`) |

jcode is roughly three times Anvil's size and decomposed into ~5× more crates. The decomposition is deliberate — `docs/CRATE_OWNERSHIP_BOUNDARIES.md` and `docs/MODULAR_ARCHITECTURE_RFC.md` define explicit ownership rules. This matters because compile-time iteration speed on a project this large is otherwise prohibitive, and the AGENTS.md flags it as a known pain point ("incremental debug cargo build with cache enabled takes about 1 minute… The goal is 5–20 seconds").

---

## 2. Shared baseline (parity exists)

Both products implement the same "table stakes" for a CLI coding agent. **Anvil is at parity** in these areas:

| Capability | Anvil | jcode |
|---|---|---|
| Tool registry + standard file/bash/web tools | `crates/tools/src/*` | `src/tool/*.rs` |
| MCP client over stdio (JSON-RPC 2.0) | `crates/runtime/src/mcp_stdio/` | `src/mcp/` |
| Anthropic + OpenAI + Ollama + OpenRouter + Gemini | `crates/api/src/providers/` | `crates/jcode-provider-*` + `src/provider/` |
| OAuth flows (Anthropic, OpenAI) | `crates/api/src/oauth.rs` | `src/auth/` (per-provider) |
| TUI with markdown rendering | `crates/anvil-cli/src/tui/` | `src/tui/` |
| Slash commands | `crates/commands/src/specs.rs` | `src/cli/commands.rs` |
| Hooks / event system | `crates/runtime/src/hooks.rs` | (lighter, via swarm bus) |
| Plugins / marketplace | `crates/plugins/` + AnvilHub | (no equivalent — jcode prefers in-binary skills) |
| Permission gating + safe modes | `crates/runtime/src/permissions/` | `src/safety.rs` |
| Session compaction at token limits | `crates/runtime/src/compact.rs` | `src/compaction.rs` + `crates/jcode-compaction-core` |
| Skill / agent registry | `crates/commands/src/agents.rs` | `src/skill.rs` |
| LSP integration | `crates/lsp/` | `src/tool/lsp.rs` |
| Routines / scheduled agents | `crates/runtime/src/routines/` (v2.2.13 foundation) | Ambient mode (production) |
| Memory tiers (User/Feedback/Project/Reference) | `crates/runtime/src/memory.rs` | (different model — see §3.1) |

These overlap. What follows is what doesn't.

---

## 3. Areas where jcode is materially ahead (adoption candidates)

Ordered roughly by leverage-per-engineering-effort.

### 3.1 Embedding-driven memory graph with cascade retrieval ⭐⭐⭐

**jcode:** `src/memory/` (5,548 LOC of memory subsystem alone), `src/memory_agent.rs` (1,696 LOC), `src/memory_graph.rs`, `crates/jcode-memory-types`, `src/embedding.rs`, `crates/jcode-embedding`, `docs/MEMORY_ARCHITECTURE.md`.

Each turn AND each response is embedded as a semantic vector. A petgraph DiGraph stores memories with **typed edges**: `HasTag`, `InCluster`, `RelatesTo`, `Supersedes`, `Contradicts`, `DerivedFrom`. Retrieval is **cascade** — vector similarity gives seed nodes, then BFS walks edges to pull related context, then a sidecar agent verifies relevance before injection. Decay half-lives differ by type (Correction 365d, Preference 90d, Fact 30d, Inferred 7d). A `Reinforcement` breadcrumb log gives traceability.

Critically, retrieval is **non-blocking**: a memory agent runs in parallel with the current turn, and the result is injected at the start of the *next* turn — so memory never adds latency. Falls back gracefully if not ready.

The README quote: "results in a human-like memory system which allows the agent to automatically recall relevant information to the conversation without actively calling memory tools or being a token burner."

**Anvil today:** Anvil's memory is **YAML-frontmattered markdown files in tiers** (`User`, `Feedback`, `Project`, `Reference`). All loaded into context up-front via the `MEMORY.md` index, capped by line count. No embeddings, no graph, no decay, no retrieval — it's "all of it, every turn."

**Gap:** This is the biggest functional gap. Anvil's approach is simple and reliable but ceiling-bounded. As the user's memory directory grows past ~10–20 entries the index becomes the only thing the model sees and the actual content gets truncated. jcode's approach scales.

**Adoption path:** Two-stage adoption is feasible:
1. **Phase 1 (low risk):** Add embeddings + cosine-similarity retrieval to existing tiered files. Inject top-K matches instead of full index. Keep file format. ~1 crate, reuses existing memory files.
2. **Phase 2 (bigger):** Layer typed edges, sidecar verifier, decay. This requires a memory side-agent and is closer to the full jcode design.

The ONNX-based embedder jcode uses is ~163 crates of dependency tail — feature-flagged in their Cargo.toml. The README confirms it's optional ("local embedding off" baseline is 27.8MB vs. 167MB with embeddings).

### 3.2 Soft interrupts (message injection without cancelling generation) ⭐⭐⭐

**jcode:** `src/soft_interrupt_store.rs`, agent loop checks in `src/agent/turn_streaming_mpsc.rs`, `docs/SOFT_INTERRUPT.md`.

When the user types during an in-flight turn, the message goes into a queue and is appended to history at one of three safe injection points:
- **Point B:** turn complete, no tools — clean append
- **Point C:** between tool executions (only for urgent aborts — must stub remaining `tool_result` blocks first to satisfy API constraints)
- **Point D:** all tools done, before next API call

No cancellation, no lost work, no re-sending the full prompt, cache stays warm. The doc walks through the API constraint precisely: "every `tool_use` must pair with a `tool_result`" — so urgent injection at C must fabricate stubs for skipped tools.

**Anvil today:** Hard interrupt — Ctrl-C cancels, message goes as a fresh user turn. Cache miss, lost in-flight work.

**Gap:** High-leverage UX. Especially valuable when a user is steering — "no wait, also check X" mid-tool-use shouldn't require an abort.

**Adoption path:** Anvil already has the turn-executor structure in `crates/runtime/src/conversation/turn_executor.rs`. The same three injection points exist; this is mostly a queue + a check at known points. Effort ~2–3 days. Worth doing.

### 3.3 Swarm / multi-agent coordination ⭐⭐

**jcode:** `crates/jcode-swarm-core/`, `src/tool/communicate.rs`, `src/bus.rs`, `src/channel.rs`, `docs/SWARM_ARCHITECTURE.md`.

A coordinator agent spawns workers, assigns scoped plans, and reviews plan-update proposals. Communication primitives: **DM** (agent-to-agent), **channel** (topic-based group), **broadcast** (swarm-wide), **shared context keys** (set/read/append). File-touch notifications detect read-then-modified-by-peer conflicts. Worktree managers own integration per group. Plans live in the server (not the repo) — "Plan distribution is out-of-band (not stored in the repo)."

Agent lifecycle: `spawned → ready → running → blocked/completed/failed/stopped/crashed`. Completion reports are **mandatory** for spawned agents — the server auto-forwards the final assistant response to the coordinator. README quote: "Agents are also able to spawn their own swarms autonomously."

**Anvil today:** Subagent dispatch via `crates/commands/src/agents.rs` exists, but it's single-shot fan-out (spawn → wait → return), not persistent coordination. No DM/channel primitive. No file-touch conflict detection. No worktree-manager role.

**Gap:** Anvil's autonomous-execution culture (the `feedback-spawn-agents-for-large-work.md` memory says "Break >4hr builds into parallel agents") would benefit hugely from this. Right now spawning two agents on the same repo means they overwrite each other.

**Adoption path:** Heavy lift (~2–4 weeks). The bus + channel layer is small but the orchestration (coordinator logic, plan-update protocol, lifecycle FSM) is substantial. Worth it for production multi-agent work.

### 3.4 Ambient mode (autonomous background gardening + scout/work) ⭐⭐

**jcode:** `src/ambient.rs` + `src/ambient_runner.rs` + `src/ambient_scheduler.rs`, `crates/jcode-ambient-types`, `src/tool/ambient/`, `docs/AMBIENT_MODE.md`.

A scheduler picks resource-safe wake intervals based on user usage rate, rate-limit headroom, and remaining quota window: `user_rate * window_remaining → projected → ambient_budget = (remaining - user_projected) * 0.8 → interval`. When it wakes, the ambient agent does three passes:
1. **Garden** — dedup memories, resolve contradictions, decay confidences, retro-extract memories from past sessions
2. **Scout** — analyze recent sessions + git history for opportunities
3. **Work** — proactive tasks always via worktree + PR (Tier 2 safety, see §3.5)

Provider priority: OAuth first (free), pay-per-token last and opt-in. Must call `end_ambient_cycle` with summary + propose next wake.

**Anvil today:** Anvil's `routines/` work in v2.2.13 is the foundation for scheduled agents but is cron-driven, not adaptive, and not memory-aware. Anvil also has `/loop` and `ScheduleWakeup` but those are user-driven.

**Gap:** The autonomous gardening of memory is what makes jcode's memory system actually work over months — without it, semantic drift and contradiction accumulate.

**Adoption path:** Anvil already has the scheduling primitive in `routines/`. The missing pieces are: (a) memory-garden tasks, (b) adaptive interval calculator, (c) provider OAuth preference. Phase this after memory-graph adoption — gardening is moot without the graph.

### 3.5 Two-tier safety system with human-in-the-loop ⭐⭐

**jcode:** `src/safety.rs`, `src/tui/permissions.rs`, `docs/SAFETY_SYSTEM.md`.

Explicit policy split:
- **Tier 1 (auto-allowed):** file reads, test runs, memory ops, local branches
- **Tier 2 (requires permission):** emails, Slack posts, PR creation, deployments, system config, password changes

`request_permission` tool queues an action with an **urgency level**. Agent can either block-and-wait or move on. Notifications go via **email (SMTP/SendGrid/SES), SMS (Twilio), desktop (notify-send), webhook, or TUI review panel** — chosen by user config. A decision history feeds pattern learning to auto-promote/demote rules. Full JSON transcript of every action is emailed as a session summary.

**Anvil today:** 5 permission modes (`ReadOnly` / `WorkspaceWrite` / `DangerFullAccess` / `Prompt` / `Allow`) plus auto-mode hard-deny patterns (the `Bash(rm -rf*)` deny list). Strong tactical guards, but no out-of-band approval channels — if Anvil hits a permission prompt in headless mode, you're stuck.

**Gap:** For long-running autonomous work (Anvil routines, ambient mode, swarm) the out-of-band approval flows (SMS, email, desktop notify) are the missing piece. Anvil's existing model is fine when a human is at the keyboard.

**Adoption path:** Pluggable notification backends (SMTP / Twilio / webhook) and a `request_permission` tool that records to a queue. ~1 week. Especially valuable as a precondition for unattended routines.

### 3.6 Native iOS client over Tailscale + APNs ⭐⭐

**jcode:** `ios/` directory (Swift Package + SwiftUI app), `src/gateway.rs` (WebSocket on port 7643), `src/login_qr.rs`, `docs/IOS_CLIENT.md`.

Native Swift/SwiftUI app, not Electron. Two Swift modules: `JCodeKit` (SDK: `Connection`, `Protocol`, `Pairing`, `CredentialStore`, `JCodeClient`, `SessionManager`) and `JCodeMobile` (app: `ContentView`, `SpeechRecognizer`, `QRScannerView`, `AppModel`, `ImagePickerView`, `MarkdownText`, `Theme`). Pairing flow: server generates 6-digit code → QR rendered on TTY → phone scans → Tailscale-encrypted WebSocket → long-lived token in Keychain. APNs delivers tool-approval prompts (matches §3.5 — phone is the human-in-the-loop channel).

**Anvil today:** No mobile client. BEMA mobile exists but is unrelated to Anvil.

**Gap:** This is product-strategy-level, not a code gap. The CLAUDE.md identifies BEMA as a separate product. Adopting a jcode-style mobile client for Anvil would be a multi-month effort — but the **wire protocol** alone is adoptable (gateway WebSocket + Identify/ServerInfo/PermissionResponse messages) without writing a Swift app, and it would unblock approval-from-phone for Anvil routines.

**Adoption path:** Two slices:
- **Cheap slice (~1 week):** Add WebSocket gateway + pairing flow + token auth to Anvil. Lets any client talk to it. Pair with §3.5.
- **Full slice (months):** Swift app. Probably not worth it given BEMA exists.

### 3.7 Session resume from foreign harnesses ⭐

**jcode:** README claims "Session resume is supported for codex, claude code, opencode, and pi." Implementation lives in `src/import.rs` + `crates/jcode-import-core`.

The agent can pick up a session that was started in Claude Code or Codex and continue. This is sticky — once users try jcode and discover their existing Claude Code sessions work, they're more likely to stay.

**Anvil today:** No equivalent. Anvil sessions are Anvil-only.

**Gap:** Strategic positioning. If Anvil is competing for share against Claude Code users, "import your Claude Code session" is a strong wedge.

**Adoption path:** Claude Code session storage is well-understood (`~/.claude/projects/<hash>/`). An importer that translates Claude Code session JSON → Anvil session is a focused project (~1 week). Codex format is also documented in jcode's `src/import.rs`.

### 3.8 Side panel + info widgets + side-channel rendering ⭐

**jcode:** `src/side_panel.rs`, `src/tool/side_panel.rs`, `crates/jcode-side-panel-types`, `src/tui/info_widget*.rs` (a dozen files), `src/tui/mermaid.rs`, custom mermaid-rs-renderer.

The TUI has a persistent right-side panel where the agent can stream auxiliary content (diff, file view, mermaid diagrams) without taking conversation real estate. **Info widgets** are even more interesting — they're rendered in negative space only, never displacing chat. They show memory state, swarm graph, token usage, tips, todos, ambient status.

README: "I created a new mermaid rendering library to render diagrams 1800× faster. It has no browser or Typescript dependency."

**Anvil today:** Anvil's TUI is conversation-only. No side panel. No structured info widgets. (Anvil has a status line and a scrollback, but not a parallel rendering surface.)

**Gap:** UX. Especially compelling for long-running ambient or swarm work where you want at-a-glance state.

**Adoption path:** Medium effort. ratatui supports split layouts. The info-widget abstraction (only render in negative space) is the clever bit and would require a layout pass.

### 3.9 Performance / time-to-first-frame ⭐

**jcode:** README benchmarks claim **14ms time-to-first-frame** vs. Claude Code's 3,437ms. Achieved via: lazy module loading (`src/startup_profile.rs`), feature-gated PDF/embeddings, optional jemalloc, aggressive `cargo` profile tuning (`opt-level = 1, codegen-units = 256, incremental = true` in release).

`scripts/dev_cargo.sh` uses sccache + clang/lld for faster builds. Multiple `[[bin]]` benchmark targets gated by `dev-bins` feature.

**Anvil today:** Anvil doesn't have published benchmarks. Time-to-first-frame is not a tracked metric.

**Gap:** Performance discipline. Not a feature gap — a process gap.

**Adoption path:** Free wins to take: jemalloc behind a feature, `tui_bench` style harness, opt-level tuning in profile.

### 3.10 Multi-account + per-account model switching ⭐

**jcode:** `src/auth/account_store.rs`, `src/auth/lifecycle*.rs`. README: "Ran out of tokens on your first ChatGPT Pro subscription? `/account` and quickly switch to your second."

Multiple OAuth identities per provider, picker UI, lifecycle tracking (token refresh, expiry).

**Anvil today:** Single account per provider. Re-auth required to switch.

**Gap:** Operational. Matters more for individuals juggling personal/work subscriptions than for an enterprise Culpur deployment.

**Adoption path:** Refactor `crates/api/src/oauth.rs` to key by `(provider, account_id)` instead of `provider`. ~3–5 days.

### 3.11 agentgrep — grep with file structure context ⭐

**jcode:** External crate at `github.com/1jehuang/agentgrep`, tagged `v0.1.2`. README: "Agent grep is a grep tool I made for the jcode agent. It adds file structure information (ie the list of functions, their displacement, etc) to the grep return, so that the agent can infer more of what the file does without actually reading the file. It also implements a harness-level integration that adaptively truncates returns based on what the agent has already seen."

**Anvil today:** `tools/src/file_ops.rs` has plain `grep_search`. No structural context.

**Gap:** Token efficiency for code search.

**Adoption path:** agentgrep is open-source. Anvil could depend on it directly via git tag (it's already MIT-compatible).

### 3.12 Other adoptable details

| Feature | jcode location | Anvil status | Notes |
|---|---|---|---|
| Mermaid rendering in TUI | `src/tui/mermaid.rs` + spun-out `mermaid-rs-renderer` | Missing | High polish; uses their own lib |
| Image rendering in TUI | `src/tui/image.rs`, `src/tui/generated_image.rs` | Missing | Useful for diagrams, screenshots |
| Voice dictation | `src/dictation.rs`, `jcode dictate` | Missing | Calls user's configured STT command |
| Restart snapshot | `src/restart_snapshot.rs` | Missing | Resume mid-turn after crash |
| Soft-shutdown idle timeout | Server idles 5 min then shuts | `serve` mode minimal | Better resource posture |
| Telemetry via Cloudflare Worker | `telemetry-worker/` (D1 + JS) | Missing | Cheap, scalable backend pattern |
| Server name + session name (memorable) | "blazing fox" combo, `~/.jcode/servers.json` | Anvil has session IDs | UX win |
| 6-digit pairing code | `src/login_qr.rs` | N/A (no remote client) | Required for §3.6 |
| Browser tool with normalized protocol | `src/tool/browser.rs` + `docs/BROWSER_PROVIDER_PROTOCOL.md` | Anvil uses MCP claude-in-chrome | jcode's is built-in + protocol-typed |
| AWS Bedrock provider | `src/provider/bedrock.rs` + aws-sdk-* deps | Missing | Enterprise checkbox |
| Cursor / Antigravity / Copilot providers | `src/provider/cursor.rs`, `antigravity.rs`, `copilot.rs` | Missing | Long tail |
| Conversation/session search tool | `src/tool/conversation_search.rs`, `src/tool/session_search.rs` | Missing | RAG over your own history |
| Batch tool | `src/tool/batch.rs` | Missing | Parallel command execution |
| Background tool | `src/tool/bg.rs` | Partial (Anvil has Bash run_in_background) | Different model |

---

## 4. Areas where Anvil is ahead (jcode gaps Anvil shouldn't lose)

It's not all one-way. Anvil has things jcode doesn't.

### 4.1 Plugin / marketplace architecture
Anvil has a real plugin system (`crates/plugins/`) with manifests, lifecycle hooks, marketplace (AnvilHub), session-scoped plugin installation. jcode has none of this — its extension story is "tell the agent to enter self-dev mode and modify the source." Anvil's model is more user-friendly for non-developers.

### 4.2 AnvilHub marketplace + product ecosystem
The hub at `anvilhub.culpur.net` for skills/plugins/agents/themes is a distribution surface jcode doesn't have. jcode is one binary, take it or leave it.

### 4.3 Output styles (customizable rendering templates)
`crates/runtime/src/config/output_style.rs` — user-defined response rendering. jcode is opinionated about its TUI; less customization.

### 4.4 Profile system
`crates/runtime/src/config/profile.rs` — multiple named config profiles via `--profile` / `ANVIL_PROFILE`. jcode has provider profiles but not full config profiles.

### 4.5 Vault + SSH credential management
`crates/runtime/src/vault/`, `crates/anvil-cli/src/tui/ssh_*` — encrypted local vault, multi-tab SSH, vault-injected env. jcode has no equivalent.

### 4.6 Reviewer (W8 content gate)
`crates/runtime/src/permissions/reviewer.rs` — pluggable content reviewer for sensitive operations. jcode's safety system is rule-based; Anvil's allows LLM-as-judge.

### 4.7 QMD knowledge integration
`crates/runtime/src/qmd.rs` — workspace-knowledge injection from a local markdown index. Different from memory; serves as documentation lookup. jcode has memory but no separate documentation/knowledge surface.

### 4.8 Compat-harness crate
`crates/compat-harness/` for upstream parity testing. jcode has session-import from foreign harnesses, but no equivalent testbed.

### 4.9 7-platform release matrix
Anvil ships macOS×2 + Linux×2 + Windows + FreeBSD + NetBSD. jcode is "Linux/macOS/Windows" — narrower.

### 4.10 Locale support
Anvil has `/locales` with `rust-i18n`. jcode is English-only.

---

## 5. Strategic differences in shape

Beyond features, the two products have **structurally different** architectures:

**jcode is server-centric.** One daemon owns sessions, tools, MCP pool, provider state. Multiple clients (TUI, iOS, future desktop) connect over Unix socket or WebSocket. Sessions persist independently of any client.

**Anvil is process-centric.** One `anvil` process per session. Sessions stored on disk but the process owns the state. `server` crate exists but is minimal (an asset viewer).

This is the single biggest architectural difference. It enables almost everything in §3.3 (swarm), §3.6 (iOS), §3.5 (out-of-band approvals), §3.4 (ambient daemon) for jcode. **If Anvil wants to adopt multiple capabilities from §3, it should adopt the server architecture first, then layer features on top.** Otherwise each feature requires its own out-of-band mechanism.

The jcode server architecture: `src/server.rs` (main daemon), `src/session.rs` (session lifecycle in-server), `src/gateway.rs` (WebSocket for remote clients), `src/sidecar.rs` (lightweight LLM for memory + compaction). Listens on `/run/user/$UID/jcode.sock` and port 7643. Hot-reloadable via `/reload` (execs new binary, clients reconnect).

---

## 6. Prioritized adoption recommendations

If we adopt only the highest-leverage items in order:

1. **Soft interrupts (§3.2)** — biggest UX win, smallest scope. 2–3 days.
2. **agentgrep dependency (§3.11)** — token-efficiency win. 1 day.
3. **Performance discipline (§3.9)** — startup_profile + jemalloc + benchmarks. 1 week.
4. **Embedding-driven memory retrieval, phase 1 (§3.1)** — embed existing tier files, top-K injection. 2 weeks.
5. **Server architecture pivot (§5)** — enables 6/7/8 below. 3–4 weeks.
6. **Out-of-band approval channels (§3.5)** — SMTP/webhook/Twilio backends for permission prompts. 1 week (after server).
7. **WebSocket gateway + pairing (§3.6 cheap slice)** — opens door to mobile/remote clients. 1 week (after server).
8. **Swarm coordination (§3.3)** — DM/channel + coordinator. 3–4 weeks (after server).
9. **Foreign-session import (§3.7)** — Claude Code session importer. 1 week.
10. **Ambient mode (§3.4)** — gardening of memory + autonomous work. 3–4 weeks (after memory phase 2 + server).

**Strategic recommendation:** the first three are free wins regardless of direction. After that, the question is whether Anvil commits to the **server-centric** model. If yes, sequence 4 → 5 → 6/7/8/10. If no, take 4 and skip 5–10 entirely.

---

## 7. Things to deliberately NOT adopt

- **Self-dev mode** — jcode lets the agent modify its own source code as a feature. This is a research bet that fits jcode's "skill ceiling" thesis. For Anvil it conflicts with stability, the public-surface infra-redaction principle, and your release discipline. Skip.
- **49-crate decomposition** — Anvil at 9 crates is roughly right for its size. Going to 49 to mirror jcode would be premature scaling. Decompose only as compile times demand.
- **Custom mermaid renderer / custom terminal (handterm)** — these are jcode-author hobby projects. Anvil shouldn't replicate; if mermaid is needed, depend on a published library or call out to the existing one.
- **Their telemetry posture** — TELEMETRY.md is a 16KB defense of their telemetry collection. Anvil's Culpur-internal posture differs; don't import this.
- **Multi-account / per-account model switching** for the Culpur deployment specifically — Culpur uses Authentik SSO, not personal subscriptions. The feature matters for jcode's individual-developer audience, not Culpur Defense's internal Anvil users.

---

## 8. One-line summary

> **jcode is what Anvil could be if it pivoted to a server-centric, embedding-memory-first, multi-client architecture — at the cost of 3× the codebase. The highest-leverage isolated adoptions for Anvil today are soft interrupts, embedding-based memory retrieval, and a WebSocket gateway with out-of-band approval channels.**
