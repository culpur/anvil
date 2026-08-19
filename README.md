<div align="center">

<br>

# &#9874; Anvil

### One AI. Every model. Yours.

[![Version](https://img.shields.io/badge/version-2.2.30-0FBCFF?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![Platform](https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows%20%7C%20BSD-lightgrey?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![35 AI Providers](https://img.shields.io/badge/35%20AI%20Providers-00D084?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![138 Commands](https://img.shields.io/badge/138%20Commands-c77dff?style=for-the-badge&labelColor=0a0f1e)](https://github.com/culpur/anvil/releases/latest)
[![License](https://img.shields.io/badge/proprietary-1e293b?style=for-the-badge&labelColor=0a0f1e)](LICENSE)

**Ask more than one model. Watch what it spends. Let it use secrets it never sees.**<br>
**Your models, your credentials, your machine — one binary, no account, no telemetry.**

[**Download**](https://github.com/culpur/anvil/releases/latest) &bull; [**AnvilHub**](https://anvilhub.culpur.net) &bull; [**Changelog**](#changelog) &bull; [**Product Page**](https://culpur.net/anvil/)

<br>
</div>

---

## What Anvil is

Anvil is a terminal-native AI workbench that treats **models as interchangeable parts you own**, not as a service you rent.

Most assistants give you one model behind one vendor's pipe. Anvil gives you **35 providers**, a **138-command** surface, and the machinery to make several models argue with each other, to price every turn before you spend it, and to hand a credential to a tool without ever showing it to the model.

It runs as a single binary on **seven platforms**, needs no account, and sends no telemetry. Point it at cloud APIs with your own keys, or at a local Ollama and never leave the machine.

---

## Why it exists

Three problems the single-vendor model can't solve:

**One model's answer looks the same whether it's right or wrong.** There's no second opinion, no visible disagreement, nothing to tell you the confident paragraph you just read is the one case it gets wrong. Anvil's answer is `/consult` — ask a panel, then look at where they *diverge*.

**Spend is invisible until the invoice.** Per-token pricing drifts, providers differ by 50×, and most tools show you a number after the money is gone — or quietly show `$0.00` because they couldn't price the call. Anvil prices each turn against a live registry, refuses *before* the provider call when a cap would break, and says **"unknown"** rather than lying with a zero.

**Secrets get pasted into prompts.** Every key you type into a chat box is a key that model saw, that vendor logged, and that transcript now contains. Anvil detects credentials at the boundary, vaults them, and replaces them with a handle it resolves only when a tool actually runs.

---

## What Anvil does

| | |
|---|---|
| &#129504; **Ask more than one** | `/consult` puts one question to a panel of models simultaneously — each through its own client and its own key — and reports agreement, divergence, and what the minority saw that the others missed. `/relay` hands one problem down a chain in order, each model seeing the real transcript before it, redacted at every vendor boundary. Neither will fake it: **a consult of one is not a consult.** |
| &#128176; **See the money first** | Every turn priced against a live pricing registry and booked per seat. A budget cap **refuses before the provider call**, not after. An unpriced turn reports its total as *unknown* — never as `$0`. `/cost` aggregates daily, weekly, monthly, yearly, by provider. |
| &#128272; **Use a secret without seeing it** | Typed credential vault — **AES-256-GCM** envelope encryption, **Argon2id** key derivation. Pasted credentials are detected at ingest, moved into the vault, and replaced with a `{{vault:label}}` handle the host resolves only at tool-run time. A loan to another seat supplies the **label**, never the value. |
| &#128737; **Refuse at the point of effect** | Per-principal capabilities, workspace scoping, and a permission gate enforced where the action happens — not where it's requested. Sandbox policy floor (`strict`/`preferred`/`off`) with real macOS seatbelt enforcement, an egress allowlist, and a tamper-evident audit log verifiable with `anvil audit verify`. |
| &#128100; **Know who did what** | Actions are attributed to the **seat** that took them, not to "the session". A peer's message is labelled as the peer's. Roles carry real scope, and a viewer-role seat gets a disabled composer rather than a hidden button. |
| &#127760; **Run it anywhere you look** | The same session in the terminal, a browser, or a phone — full slash-command surface on every one, not a read-only mirror. `/local-control` serves the whole console over loopback with no cloud round-trip. |
| &#129517; **Give it a memory and a map** | A seven-layer memory system you can browse, edit, and re-scope, with `[[links]]` between entries. `/mindmap` renders your thinking as a force-directed graph you can drive from the terminal or the browser. |
| &#129302; **Put a team on it** | `/team` delegates to parallel sub-agents; `/chain` runs multi-step chains end-to-end; `/fleet` fans one task out across every live host you own and collects the results. Agents are filed into **12 built-in squads** (security, infra, compliance, quality, delivery, build, legal, finance, healthcare, education, busops, research) — and an unstaffed squad **refuses** rather than quietly running a general-purpose agent instead. |
| &#128268; **Extend it** | MCP client with local **and** remote transports (Streamable HTTP/SSE, browser OAuth). Six built-in sub-agent types plus anything you install. Skills, plugins, themes, and agents — build locally, sync as a private draft, publish to the AnvilHub marketplace under your own identity when you're ready. |

---

## What it looks like

**Ask a panel instead of an oracle:**

```
> /consult which of these two migration plans is safer?

  CONSENSUS      3 of 4 seats agree plan B is safer
  DIVERGENCE     1 seat dissents on the rollback window
  THE DISSENT    asked gpt-5.6 directly — it flagged a 40-minute
                 window where both schemas are live, which the
                 majority did not model
```

`/consult` will refuse a panel of one — that isn't a consult, and it says so.

**Hand one problem down a chain:**

```
> /relay draft it, then review it, then harden it
  claude-opus-5  →  gpt-5.6  →  grok-4.5
```

Each model sees the real transcript of the work before it, redacted at every vendor boundary.

**Know the price before you pay it:**

```
> /budget set 5.00/day
> /cost

  today          $1.84   anthropic $1.20 · openai $0.61 · ollama $0.00
  1 turn unpriced — total is a LOWER BOUND, not $0
```

The cap refuses the next call **before** it goes out, not after the money is gone.

**Use a key you never expose:**

```
> deploy with the staging token
  [screened] credential detected and vaulted as {{vault:staging_token}}
  [tool] curl -H "Authorization: Bearer {{vault:staging_token}}" …
         resolved at run time — the model never saw the value
```

---

## Who it's for

- **Developers who don't trust one model** — and want a second and third opinion on the answer that matters, with the disagreement shown rather than averaged away
- **Anyone paying for tokens** — who wants the cap enforced *before* the call and an honest "unknown" instead of a fake `$0`
- **Consultants and contractors** juggling client credentials, who need isolation between projects and a vault the assistant can use but never read
- **Privacy-conscious and air-gapped users** — no account, no telemetry, local Ollama, and a binary that works with the network off
- **Teams** who need actions attributed to a person, roles that actually restrict, and a shared session that doesn't mean shared credentials
- **Regulated environments** — a separate FIPS build exists with its own, deliberately narrower capability set

---

## Live remote control

**Start a session in the terminal. Finish it on your phone.**

```
you@workstation:~$ anvil
> /remote-control
  Remote control active — open the pairing URL on any device
  Pairing code: 847291
```

Open the URL, enter the six-digit code, and you're driving the same live session — not a read-only mirror.

- **Full bidirectional control** — type, run commands, manage tabs from any device
- **The whole command surface** — every slash command, with deep autocomplete, on web and mobile
- **Real-time streaming** — responses token-by-token in the browser
- **Configure from the browser** — swap providers, change models, manage credentials
- **PIN-paired and encrypted**, with automatic reconnection

Or run `/local-control` and get the identical console bound to loopback — no relay, no cloud, nothing leaves the machine.


---

## What's new in v2.2.30 &mdash; Ask More Than One

**v2.2.30 stops treating one model's answer as the answer &mdash; and stops treating "the session" as the thing that acted.**

**Ask more than one.** `/consult` puts the same question to a panel of models at once, each through its own client and its own key, then reports where they agree, where they diverge, and asks the minority what it saw that the others missed. It never picks a winner for you. `/relay` takes the other shape: one problem handed down a chain of models in order, each seeing the real transcript of the work before it, redacted at every vendor boundary. Neither will fake it &mdash; a consult of one is not a consult, and a single model working alone is a normal turn, not a relay.

**Somebody did that, and Anvil knows who.** Every action now carries a verified identity: a subject, a display name, an org, a role, an explicit capability set, a workspace scope, and a credential scope. Actions are attributed to the **principal** that took them rather than to the session &mdash; a peer's message is labelled as the peer's, and tool calls name the principal that caused them on the durable record, not just in the live view. Permissions are enforced at the point of effect, so a role that cannot write cannot write, whichever surface it is sitting at. Nothing is unrestricted by omission: an empty scope grants nothing, and whole-machine reach is a flag somebody had to set on purpose.

**Say no, then offer a door.** When a scoped principal is refused, it can ask for a one-time or a permanent increase instead of simply failing; the request reaches AnvilHub and the decision comes back to the running session. `/scope` shows what is pending.

**Work as an organization.** Organizations, membership, invitations and roles live on AnvilHub. Roles are authored, not chosen from a fixed list &mdash; an admin can define one with its own capabilities and workspace scope, while the seeded roles stay immutable. A seat limit is checked when an invitation is *accepted*, not only when it is sent, because an org four-fifths full can otherwise issue twenty invitations that were each legal the moment they were raised. Joining an org grants other people access; it never transfers ownership, which stays with one person. Credentials can be lent between people: a loan carries the vault **label**, never the value, names the lender in the audit line, and expires in minutes &mdash; and the borrower can use it without reading it and cannot lend it onward.

**Run every Anvil from one place.** Claim a deployment from the web and manage it there &mdash; give it a name and a persona, choose its primary agent, watch its live status. Install and remove skills, plugins and agents on a claimed Anvil, applied live when it is online and queued when it is asleep. Build a capability locally, sync it up as a private draft, review it, then publish it to the marketplace under your own identity &mdash; nothing leaves your machine until you say so. `/fleet` sends one task to every live host you own and collects the answers.

**Spend you can see before you pay it.** Every turn is priced against a live pricing registry and booked per principal, and a budget cap now refuses *before* the provider call rather than reporting the overspend afterwards. When a turn cannot be priced, Anvil says the total is unknown instead of quietly calling it zero.

**Use a secret without seeing it.** Credentials pasted into a session are detected, moved into the vault, and replaced with a handle the host resolves only when a tool actually runs &mdash; so Anvil can use a secret it never reads.

The command registry is now **138 commands**, all discoverable and runnable from the web and mobile viewers, not just the terminal.

## What's new in v2.2.28 &mdash; The Vivid Viewer

**v2.2.28 makes the whole web viewer worth looking at.** The force-directed mind map from 2.2.27 keeps its layout and behavior, but now it sits on a lit stage &mdash; nodes glow in their own color, curved/dashed/dotted links pop, and a sparse ambient starfield (with the occasional shooting star) drifts behind the graph. The conversation is no longer a flat terminal dump: your turns and Anvil's render as distinct message cards, fenced code becomes its own glowing panel with a language chip, inline `code` and **bold** render properly, and long context collapses to a single chip you can expand. The seven-layer memory readout in the rail is now interactive &mdash; click any layer to open the **memory browser**, every stored memory a card with a colored type badge. Settings moved out of the scroll into their own floating **window**. And the atmosphere isn't web-only: a new **`/atmosphere on|off`** command carries the same starfield behind the terminal conversation, mind map, and memory (on by default). Everything here lives in the viewer's presentation layer &mdash; no engine, protocol, or crypto changes &mdash; so it's identical in commercial and FIPS builds.

## What's new in v2.2.27 &mdash; The Mind Map Release

**v2.2.27 gives Anvil a living, visual map of your thinking.** Run `/mindmap` (alias `/mm`) to open a daily-thoughts map &mdash; a collapsible outline you drive from the terminal, and a full interactive graph you drive from the web viewer (local-control and remote-control alike). The web canvas is force-directed: nodes are aware of each other and repel so they never overlap, you drag any node and its neighbors move out of the way, zoom has no lower limit so the whole map stays in view (and the wheel zooms about the cursor), and a click shows a node's full notes. Colorize nodes, pick link styles, attach images, and view a day/week/month/year of interconnections. Opt in and the map learns what Anvil learns: today's memories mirror into a tidy collapsible branch (tagged by type, tasks marked open/closed), a separate **Memory Graph** view projects your whole memory system as a color-coded graph with its `[[links]]`, and thought suggestions reach into QMD + memories. Also new: an opt-in **live-session overlay** that shows a calm "live session in browser" screen on the terminal while a web session drives it, with the full conversation intact when you wake it. Everything above is off by default and toggled in `/configure`.

## What's new in v2.2.25 &mdash; The Security Hardening Release

**v2.2.25 is a focused security patch** that closes fifteen findings from a deep, adversarially-verified audit of the codebase (no criticals). It strengthens the relay pairing model (the daemon now re-verifies pairing at the Rust layer before acting on any privileged message &mdash; CWE-306/862 &mdash; and the pairing-code attempt limit can no longer be reset by reconnecting, CWE-307), locks down Windows credential storage (owner-only ACLs on secret files + the daemon capability token, and a client-identity check on the named pipe &mdash; CWE-732/284), and hardens the sandbox (deny-by-default read access to credential directories + secret-env scrubbing, network-deny under `sandbox.require = strict`, and a consolidated macOS seatbelt profile &mdash; CWE-522/668/1059). The remote-MCP transport is now bounded (response and line size caps, CWE-400) and SSRF-guarded (URLs checked against their resolved IP, refusing private/link-local/metadata ranges by default, CWE-918). Rounding it out: owner-only secret and run directories (CWE-732/367), a normalization-hardened command reviewer (CWE-697), and an allowlisted `/perf` argument (CWE-77). No feature changes, no config migration; behavior with `sandbox.require = off` (the default) is byte-for-byte unchanged. **Recommended upgrade for every user.**

## What's new in v2.2.24 &mdash; The Stability &amp; xAI Release

**v2.2.24 makes sessions connect reliably and brings xAI up to date.** A daemon crash that dropped sessions the instant you connected (`early eof`) is fixed &mdash; the provider runtime is now shut down off the async worker, and an older daemon can no longer shadow a newer build. Text selection copies from **exactly** where your cursor is, even when lines above it wrap. xAI/Grok is refreshed: `grok` targets **grok-4.3**, retired ids are gone, **grok-build-0.1** is available, and Grok's **Live Search** is supported through an isolated Responses-API path. A new **sandbox policy gate** (`sandbox.require = strict | preferred | off`) with real **macOS seatbelt enforcement** lets you set an isolation floor. `AskUserQuestion` is answerable over the daemon (with an opt-in idle timeout), a sub-agent's **partial output is preserved** when a stream fails, and the terminal is restored cleanly on every exit. Underneath: a **daemon-leak fix** (atomic singleton, orphan/idle self-exit, and a new `anvil daemon reap`) that stops runaway background processes.

## What's new in v2.2.23 &mdash; The Voice &amp; Reach Release

**v2.2.23 lets you talk to Anvil and reaches remote MCP servers.** Start voice entry with `/voice` or `Ctrl+E` and your words are transcribed into the input box **live**, in short chunks, as you speak &mdash; toggle on/off, `Ctrl+C` interrupts, and you can keep typing while recording. Transcription runs through **your own** OpenAI-compatible key, or a **local whisper.cpp** backend (offline, no key) that the setup wizard can install for you. Anvil's MCP client gained a full **remote transport** &mdash; Streamable HTTP/SSE, not just local stdio &mdash; with `anvil mcp login <server>` browser OAuth (PKCE, RFC 8414 discovery) and per-server refreshable tokens. It also closes a wide **Claude-Code parity** gap: inline `!`-bash, `/config key=value`, `/permissions` denial reasons, `autoMode.classifyAllShell`, org model allow/deny, a MEMORY.md compaction reminder, and a **shared action spinner** that labels every action (Thinking, Recording, Transcribing, Running command, Searching, Authenticating). Security hardened with `sandbox.credentials` (blocks commands from reading credential files or dumping secret env) and destructive-command guards; `anvil login` runs the OAuth flow then launches straight into Anvil; and routines gained a self-contained **email delivery** path with a machine-bound system-secret store.

## What's new in v2.2.22 &mdash; The Memory Release

**v2.2.22 makes your persistent memory browsable, manageable, and relevant.** A new in-TUI memory browser (`/memory browse`) and a web memory console &mdash; with an SVG graph of the `[[link]]` web &mdash; let you filter, read, and now **create, edit, forget, and re-scope** memories directly. `/local-control` serves that same console locally over loopback, no cloud round-trip. Memory injection became smart: identity and learned-process tiers stay pinned while project/reference memories are surfaced **per-turn, filtered to your message**, so the prompt stays lean as the store grows. The web viewer reached full parity with the TUI (cost, conversation-on-pair, input history, every rail field, live context + block countdown). Underneath: a 10-minute streaming timeout, OS-proxy auto-detect off by default, a daemon version handshake, a workflow/process self-improvement reviewer, and a security-hardening pass.

## What's new in v2.2.21 &mdash; Durable Daemon Sessions, the Relay Bridge, and the Big Modularization

**v2.2.21 is the largest single-release reshape since v2.0.** Session execution moves out of the TUI and into the `anvild` daemon &mdash; close your terminal and the session keeps running; reopen anywhere and reattach. A browser can pair to a live session over an encrypted relay and drive it. Underneath, the biggest modularization pass in project history split the codebase into **31 focused crates**, three of the planned performance wins landed, and a cluster of P0 daemon bugs were found and fixed under live verification. FreeBSD x86_64 and NetBSD x86_64 now ship as prebuilt binaries.

### Durable daemon sessions (D.1&ndash;D.4)

Session execution now lives in `anvild`, not the TUI process. The session protocol contract (`SessionRequest` / `SessionEvent`) defines the on-wire types; a `SessionActor` hosts the conversation inside the daemon; the TUI is repointed as a thin viewer over a Unix socket. Close the TUI &mdash; the session keeps running. Reopen it, or attach from elsewhere, and you reattach to the same live session. Crash the daemon, restart it, and the session replays from the journal. Multi-client attach means more than one viewer can watch the same session, with peer-attach / peer-detach events.

### Named, multi-tab sessions (#911)

Daemon sessions became first-class and human-addressable. The session manifest gained a `name` and `last_active_at_ns`; a `Rename` IPC verb carries it on the wire. The TUI drives the full lifecycle &mdash; `/session list|rename|kill` over one-shot IPC, then `/session new|open|load|close` mapped onto TUI tabs with per-tab daemon routing. The capstone is **same-cwd auto-resume**: reopen Anvil in a directory that already has a live daemon session and it reattaches instead of starting cold.

### Relay bridge &mdash; attach from the browser (#914, #917, #919&ndash;#921)

`anvil routined relay start` pairs a browser viewer to a live daemon session over a passage WebSocket and auto-opens it; pairing returns a full PIN. #917 (P0) fixed a daemon-side tool-dispatch name-drift bug where every tool fell through to a stub &mdash; daemon-hosted sessions now execute real tools. #919 adds viewer UX (input echo, thinking spinner, live context). #920 wires `/permissions` end-to-end over the relay. #921 adds a ring recorder so resume/replay carries the actual conversation &mdash; a viewer pairing mid-session sees history, not a blank pane.

### Performance &mdash; pooling, parallel tools, faster search (#898, #899, #900)

A shared `reqwest::Client` across the workspace ends the per-call TLS handshake + DNS lookup and keeps HTTP/2 connections warm (#898). Read-only tools now run in parallel: when the model requests two or more read-only tools in one batch (`read_file`, `grep_search`, `glob_search`, web fetches), they fan out across worker threads and merge back in original order; mutating tools still run sequentially through the permission gate (#899). `grep_search` is now gitignore-aware and streams &mdash; it walks and searches simultaneously, respects `.gitignore`, and skips binaries, so it no longer drowns in `node_modules/` or `target/` (#900).

### The big modularization &mdash; 31 crates

`anvil-cli` was split into `anvil-ollama`, `anvil-mcp-builder`, `anvil-tui`, and `anvil-wizard`. Thirteen crates were extracted from `runtime` across two rounds &mdash; `anvil-vault`, `anvil-journal`, `anvil-permissions`, `anvil-oauth`, `anvil-hooks`, `anvil-mcp`, `anvil-memory`, `anvil-curator`, `anvil-search`, `anvil-relay`, `anvil-reflection`, `anvil-skill-chain-exec`, `anvil-routines` &mdash; with intermediate crates breaking the dependency cycles. `runtime` shrank from roughly 70K to about 57.7K lines; the workspace now has **31 crates**. Supporting work: an `anvil-e2e` integration-test crate and compile-time locale-key drift detection.

### Daemon hardening &mdash; P0 fixes (#902&ndash;#917)

The daemon execution path was load-bearing for the first time, and live verification surfaced a cluster of P0s &mdash; every one fixed: wiring the real provider runtime into the daemon (#905), connecting the attach forwarder task (#906, with an e2e gate in #910), binding REPL session I/O to the long-lived runtime (#903), reading the correct PID file (#904), and a `/heal` probe that checks actual process liveness (#902). Auto-compaction now actually compacts (#913). And the security P0: anvild binary downgrade-prevention (#827).

### Quality

**3,926 tests passing, zero failures.** Several latent shared-state test races (process-global locale, env-var, and atomic flags that only flaked under specific parallel scheduling) were tracked down and serialized. IPC gained a dual-transport abstraction (Unix sockets + Windows named pipes) so the daemon runs natively on all seven platforms.

### Compatibility

v2.2.21 is a drop-in upgrade from v2.2.20. Config, vault, and session formats are forward-compatible &mdash; no migration steps required. The daemon-default execution path is opt-in for now: sessions run in-process unless you pass `anvil --daemon`. If you have stale `anvild` processes from binaries you've moved or trashed, kill them before upgrading &mdash; `anvil --update` refuses to run while stale daemons are alive and prints the exact PIDs.

---

### Install

Seven platforms, SHA256-verified, single binary, no runtime required.

```bash
# Homebrew (macOS & Linux)
brew install culpur/anvil/anvil

# Or download directly
curl -fsSL https://anvilhub.culpur.net/install.sh | bash
```

| Platform | Download |
|----------|----------|
| **macOS ARM** (M1/M2/M3/M4) | [`anvil-aarch64-apple-darwin`](https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-apple-darwin) |
| **macOS Intel** | [`anvil-x86_64-apple-darwin`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-apple-darwin) |
| **Linux x86_64** | [`anvil-x86_64-unknown-linux-gnu`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-linux-gnu) |
| **Linux ARM64** | [`anvil-aarch64-unknown-linux-gnu`](https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-unknown-linux-gnu) |
| **Windows x86_64** | [`anvil-x86_64-pc-windows-gnu.exe`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-pc-windows-gnu.exe) |
| **FreeBSD x86_64** | [`anvil-x86_64-unknown-freebsd`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-freebsd) |
| **NetBSD x86_64** | [`anvil-x86_64-unknown-netbsd`](https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-netbsd) |

No account. No sign-in. Download, run, configure your providers.

---

## 35 providers, one terminal

| Provider | Models | Auth |
|----------|--------|------|
| **Anthropic** | claude-opus-4-7, claude-sonnet-4-6, claude-haiku-4-5 | OAuth (Max plan supported) or API Key |
| **OpenAI** | GPT-5, o3, o4-mini | API Key |
| **OpenAI Codex** | codex-mini | API Key |
| **Google Gemini OAuth + Antigravity** | Gemini 2.5 Pro, Gemini 2.5 Flash, Gemini 2.0 Flash Thinking | Code Assist OAuth (PKCE) |
| **AWS Bedrock** | Anthropic Claude family, Llama, Mistral, Titan | manual SigV4 (no AWS SDK) |
| **Cursor Cloud Agents** | claude-4-sonnet-thinking, sonnet-4, sonnet-3-7-thinking | API Key + GitHub repo binding |
| **GitHub Copilot** | gpt-5, gpt-5-mini, gpt-4.1, gpt-4o, sonnet-4, opus-4.5 | Device flow |
| **Azure OpenAI** | (deployment-name based) | API Key + `api-version` |
| **xAI** | Grok-4, Grok-4-mini, Grok-3 | API Key |
| **Ollama** *(recommended)* | Llama, Qwen, Mistral, DeepSeek, Gemma, GPT-OSS | Local &mdash; no key needed |
| **Ollama Cloud** | kimi-k2.6:cloud, gpt-oss:120b-cloud | ed25519 device key (via local daemon) |
| **Groq** | Llama 3.3 70B, Mixtral, DeepSeek R1 | API Key |
| **Fireworks AI** | Llama 3.1/3.2 family, Mixtral, DeepSeek | API Key |
| **Mistral** | Mistral Large, Codestral, Mixtral | API Key |
| **Perplexity** | sonar, sonar-pro, sonar-reasoning | API Key |
| **DeepSeek** | deepseek-chat, deepseek-coder, deepseek-r1 | API Key |
| **Together AI** | Llama, Qwen, Mistral, Mixtral, DeepSeek | API Key |
| **DeepInfra** | Llama, Qwen, DeepSeek, Mistral | API Key |
| **Cerebras** | Llama 3.1/3.3, Qwen | API Key |
| **NVIDIA NIM** | Llama 3.x, Nemotron family | API Key |
| **HuggingFace** | Inference-API hosted models | API Token |
| **Moonshot AI** | Kimi K2, moonshot-v1 | API Key |
| **Nebius** | Llama, Qwen, DeepSeek | API Key |
| **Scaleway** | Llama, Mistral | API Key |
| **STACKIT** | Llama 3.1 | API Key |
| **Baseten** | Llama, Qwen, DeepSeek | API Key |
| **Cortecs** | Llama, Qwen, Mistral | API Key |
| **302.AI** | OpenAI-compatible aggregator | API Key |
| **ZAI** | OpenAI-compatible aggregator | API Key |
| **OpenRouter** | 200+ models from every major provider | API Key |
| **LMStudio** | local OpenAI-compatible server | Local &mdash; no key needed |
| **Chutes** | OpenAI-compatible aggregator | API Key |
| **MiniMax** | minimax-text, abab models | API Key |

Configure priority chains. Automatic failover when one hits a rate limit. Per-provider budgets. Cost tracking per session. Zero-cost local inference with Ollama or LMStudio. **No IDE spoofing, no scraped credentials.** Every provider implementation either uses a documented public API or identifies as Anvil honestly in headers.

---

## Quick Start

```bash
anvil                               # Start interactive session
/remote-control                     # Share via browser
/model claude-opus-4-7              # Switch model
/provider anthropic                 # Switch provider
/vault add                          # Store a credential
/ssh myserver                       # Open SSH tab
/productivity                       # Session stats
/mcp list                           # MCP server status
/fork experiment                    # Branch the conversation
/focus                              # Distraction-free mode
/export md                          # Export as Markdown
```

---

## Also in the box

**138 slash commands** (including the `/cursor` command tree and `/memory clean` / `/cursor stream` / `anvil agents` cross-session monitor). **35 AI providers.** 45 built-in tools. MCP integration. Per-tab parallel inference. SSH tabs. Tool-call cards with Ctrl+O expand. Multi-tab sessions. Git integration. Code productivity dashboard. Session history search. 37-widget configurable status line with 16 presets. Vim keybindings. Focus view. File sandbox with permission modes. 7-language i18n. AnvilHub marketplace for skills, plugins, agents, and themes. Web UI with full configuration parity. First-run setup wizard. CC&rarr;Anvil migration (`anvil import claude-code`). anvil(1) manpage. All of it optional. None of it required.

Feature list is in [the changelog below](#changelog) and [anvilhub.culpur.net/about](https://anvilhub.culpur.net/about). The feature list isn't the story. The freedom is.

---

## Links

| | |
|---|---|
| &#127968; **Product Page** | [culpur.net/anvil](https://culpur.net/anvil/) |
| &#128230; **Marketplace** | [anvilhub.culpur.net](https://anvilhub.culpur.net) |
| &#128214; **Full Changelog** | [anvilhub.culpur.net/about](https://anvilhub.culpur.net/about) |
| &#128172; **Issues** | [github.com/culpur/anvil/issues](https://github.com/culpur/anvil/issues) |

---

## License

Copyright (c) 2024-2026 Culpur Defense Inc. All rights reserved.

---

## Changelog




### v2.2.30 &mdash; August 19, 2026

**Ask More Than One.**

- &#10003; **`/consult`** &mdash; ask a panel of models the same question at once, each through its own client and key; reports agreement and divergence instead of picking a winner, and asks the minority what it saw
- &#10003; **`/relay`** &mdash; hand one problem down a chain of models in order, each seeing the real transcript of the work before it, redacted at every vendor boundary
- &#10003; **Honest refusals** &mdash; a consult of one is not a consult; a single model working alone is a normal turn, not a relay
- &#10003; **Priced turns** &mdash; every turn priced against a live pricing registry and booked per seat; an unpriced turn reports its total as unknown, never as $0
- &#10003; **Budget cap that refuses first** &mdash; the cap is enforced *before* the provider call, not reported after the spend
- &#10003; **Use a secret without seeing it** &mdash; pasted credentials are detected, vaulted, and replaced with a handle the host resolves only at tool-run time; a loan supplies the label, never the value
- &#10003; **138 commands** &mdash; the full registry is discoverable and runnable from the web and mobile viewers, not only the terminal

### v2.2.29 &mdash; July 25, 2026

**Manage Your Anvil.**

- &#10003; **AnvilHub is now mission control** &mdash; claim a deployment and manage it from the web: give it a name and persona, pick its primary agent, and see its live status.
- &#10003; **Install & remove capabilities from the marketplace** &mdash; add or remove skills, plugins, and agents on a claimed Anvil straight from AnvilHub &mdash; applied live when it's online, queued when it's asleep.
- &#10003; **Build &rarr; sync &rarr; publish** &mdash; build a capability locally, sync it up as a *private draft*, review it, then publish it to the marketplace in one click. Your Anvil packs and uploads it under your identity; nothing leaves until you say so.
- &#10003; **Starter chain templates** &mdash; the chain builder ships curated templates wired from real store skills, so you compose from a working chain instead of a blank canvas.
- &#10003; **Latest models, out of the box** &mdash; refreshed defaults and aliases to the current flagships: Claude Opus 5, plus GPT-5.6, Grok 4.5, and Gemini 3.6 Flash. The `/model` picker already live-fetches every provider, so you always see the newest.
- &#10003; **Leaner first run** &mdash; the setup wizard no longer installs external dependencies; Anvil's native memory, voice, and media subsystems are the default, and QMD detection is silent. Prefer a guided setup? `anvil setup --web`.


### v2.2.28 &mdash; July 18, 2026

**The Vivid Viewer.**

- &#10003; **Mind map, lit up** &mdash; nodes glow in their own color, curved/dashed/dotted links pop, and a sparse ambient starfield (with occasional shooting stars) drifts behind the graph.
- &#10003; **A conversation worth reading** &mdash; distinct message cards, fenced code as glowing panels with a language chip, real inline `code` and **bold**, and long context collapsed to an expandable chip.
- &#10003; **Browse memory by layer** &mdash; the seven-layer rail readout is interactive; click a layer to open the memory browser, every memory a card with a colored type badge.
- &#10003; **Settings in a window** &mdash; Configuration is a floating window (pinned header, clean scroll, aligned fields) instead of scrolling inline.
- &#10003; **`/atmosphere on|off`** &mdash; the same starfield behind the terminal conversation, mind map, and memory; on by default, toggled by command.
- &#10003; **One visual language** &mdash; rail, header, bars, composer, and top bar all share the glass-and-glow look; nothing moved.
- &#10003; **Presentation-layer only** &mdash; no engine, protocol, or crypto changes, so the viewer is identical in commercial and FIPS builds.

### v2.2.27 &mdash; July 16, 2026

**The Mind Map Release.**

- &#10003; **`/mindmap` (alias `/mm`)** &mdash; a daily-thoughts mind map, rendered as a collapsible outline in the TUI and a full interactive graph in the web viewer. One shared document; edits and captures sync live between the terminal and the web.
- &#10003; **Force-directed web canvas** &mdash; nodes repel so they never overlap; free-drag a node and neighbors move aside; +/&minus;/fit zoom with no lower limit (whole map always in view) and cursor-anchored wheel zoom; click a node for its full notes.
- &#10003; **Style &amp; time** &mdash; colorize nodes, choose link line-types, attach images, and view a day / week / month / year of interconnections.
- &#10003; **Learns what Anvil learns (opt-in)** &mdash; today's memories mirror into a collapsible "memories" branch (tagged by type, tasks open/closed); a separate read-only **Memory Graph** view projects the whole memory system, grouped by kind with its `[[links]]`; thought suggestions reach into QMD + memories.
- &#10003; **Live-session overlay (opt-in)** &mdash; when a web client drives the session, the terminal can show a calm "live session in browser" screen with live context/token counts; the full conversation is intact when you wake it.
- &#10003; **Remote &amp; local viewers unified** &mdash; the AnvilHub remote-control viewer serves the same mind map and features as local-control.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64 (MSVC), FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.26 &mdash; July 14, 2026

**The Convergence Release.**

- &#10003; **Learns your recurring workflows** &mdash; when you repeat the same multi-step workflow across sessions, Anvil offers to capture it as a reusable skill; run `/skills capture` to draft one from your exemplar (or `--never` to dismiss).
- &#10003; **Corporate TLS proxies out of the box** &mdash; `ANVIL_TLS_TRUST=system` trusts your OS system trust store, so TLS-inspecting proxies (Zscaler, Palo Alto, etc.) with a locally-installed MITM CA just work.
- &#10003; **Audit log (opt-in, on by default)** &mdash; a tamper-evident log of security-relevant actions, verifiable with `anvil audit verify`; disable with `ANVIL_AUDIT=off`.
- &#10003; **Build &amp; supply-chain hardening** &mdash; every binary built with explicit anti-exploitation flags, each release ships a CycloneDX **SBOM**, and a `cargo deny` policy gates licenses, sources, and advisories &mdash; converged from the regulated edition into every build.
- &#10003; **Windows OAuth fixed** &mdash; browser sign-in on Windows now completes reliably (PKCE via `OsRng`, single browser launch, `dirs_next` home resolution); the Windows asset is the MSVC wizard-fix build.

### v2.2.25 &mdash; July 8, 2026

**The Security Hardening Release.**

- &#10003; **Fifteen audit findings closed** &mdash; a deep, adversarially-verified security review (2 High, 6 Medium, 6 Low, 1 Info; no criticals). Recommended upgrade for all users; no feature changes, no config migration.
- &#10003; **Relay pairing enforced locally** &mdash; the daemon re-verifies pairing at the Rust layer before acting on any privileged message (CWE-306/862); the pairing-code attempt limit can no longer be reset by reconnecting (CWE-307).
- &#10003; **Windows credential storage locked down** &mdash; secret files and the daemon capability token get an owner-only ACL bound at creation, and the daemon named pipe verifies the connecting client's identity (CWE-732 / CWE-284).
- &#10003; **Sandbox deny-by-default** &mdash; an active sandbox denies read access to credential directories and scrubs secret env; `sandbox.require = strict` denies outbound network; the macOS seatbelt profile is consolidated to one canonical implementation (CWE-522 / 668 / 1059).
- &#10003; **Remote MCP bounded &amp; SSRF-guarded** &mdash; HTTP/SSE body and stdio line sizes are capped (CWE-400), and MCP URLs are checked against their resolved IP, refusing private/link-local/metadata ranges by default with an `ssrfAllowlist` opt-in (CWE-918).
- &#10003; **Additional hardening** &mdash; owner-only secret and run directories (CWE-732/367), a normalization-hardened command reviewer (CWE-697), and an allowlisted `/perf` argument (CWE-77). Behavior with `sandbox.require = off` (the default) is byte-for-byte unchanged.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.24 &mdash; July 6, 2026

**The Stability &amp; xAI Release.**

- &#10003; **Sessions connect reliably** &mdash; fixed a daemon crash that dropped sessions on connect (`early eof`); the provider runtime is now shut down off the async worker so sessions start, run, and tear down cleanly. An older daemon can no longer shadow a newer build (build-timestamp recency guard).
- &#10003; **Text selection copies from the right place** &mdash; drag-to-copy now starts exactly at your cursor even when lines above it wrap, with correct wide/emoji-character handling.
- &#10003; **xAI / Grok refresh + Live Search** &mdash; `grok` targets grok-4.3, retired ids removed, grok-build-0.1 added, and Grok Live Search via the `x_search` Responses-API tool (`ANVIL_XAI_LIVE_SEARCH=1`).
- &#10003; **Sandbox policy gate** &mdash; `sandbox.require = strict | preferred | off` with real macOS `sandbox-exec` seatbelt enforcement; the active backend shows in `anvil daemon status`.
- &#10003; **AskUserQuestion over the daemon** &mdash; questions fan out over the relay so a daemon-attached or web viewer can answer, with an opt-in idle timeout. Sub-agent partial output is preserved with an `[incomplete response]` marker when a stream fails.
- &#10003; **Terminal hygiene, paste, and the daemon-leak fix** &mdash; the terminal is restored on every exit (quit, Ctrl+C, panic); large pastes land once. Autostart is suppressed under test/headless, the singleton PID lock is atomic, an idle orphan self-exits, and `anvil daemon reap` clears runaway daemons.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.23 &mdash; June 29, 2026

**The Voice &amp; Reach Release.**

- &#10003; **Voice entry** &mdash; `/voice` or `Ctrl+E` for live in-terminal dictation; words transcribe into the input box as you speak. Toggle on/off, `Ctrl+C` interrupts, type while recording. Cloud (your key) by default, or a local whisper.cpp backend the wizard can install.
- &#10003; **Remote MCP servers** &mdash; Streamable HTTP/SSE transport (not just local stdio) with `anvil mcp login <server>` browser OAuth (PKCE, RFC 8414 discovery) + per-server refreshable tokens.
- &#10003; **Claude-Code parity** &mdash; inline `!`-bash, `/config key=value`, `/permissions` denial reasons, `autoMode.classifyAllShell`, org model allow/deny, MEMORY.md compaction reminder, and a shared action spinner that labels every action.
- &#10003; **Security &amp; login** &mdash; `sandbox.credentials` blocks commands from reading credential files or dumping secret env; destructive-command guards flag hard resets and force-cleans; `anvil login` runs the OAuth flow then launches straight into Anvil.
- &#10003; **Routine email delivery** &mdash; a self-contained email path with a machine-bound system-secret store.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.22 &mdash; June 17, 2026

**The Memory Release.**

- &#10003; **Memory browser + web console** &mdash; a new in-TUI memory browser (`/memory browse`) and a web memory console with an SVG graph of the `[[link]]` web let you filter, read, and **create, edit, forget, and re-scope** memories directly. `/local-control` serves that console locally over loopback.
- &#10003; **Smart memory injection** &mdash; identity and learned-process tiers stay pinned while project/reference memories are surfaced per-turn, filtered to your message, so the prompt stays lean as the store grows.
- &#10003; **Web viewer parity** &mdash; the browser viewer reached full parity with the TUI (cost, conversation-on-pair, input history, every rail field, live context + block countdown).
- &#10003; **Underneath** &mdash; a 10-minute streaming timeout, OS-proxy auto-detect off by default, a daemon version handshake, a workflow/process self-improvement reviewer, and a security-hardening pass.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.21 &mdash; June 15, 2026

**Durable Daemon Sessions, the Relay Bridge, and the Big Modularization.**

- &#10003; **Durable daemon sessions (D.1&ndash;D.4, #841&ndash;#844)** &mdash; session execution moves out of the TUI into `anvild`. `SessionRequest` / `SessionEvent` define the on-wire contract; a `SessionActor` hosts the conversation in the daemon; the TUI is a thin viewer over a Unix socket. Close the TUI and the session keeps running; reopen or attach from elsewhere and reattach to the same live session; crash and restart the daemon and the session replays from the journal. Multi-client attach with peer-attach / peer-detach events.
- &#10003; **Named, multi-tab sessions + same-cwd auto-resume (#911)** &mdash; the session manifest gains `name` + `last_active_at_ns`; a `Rename` IPC verb carries it. `/session list|rename|kill` over one-shot IPC; `/session new|open|load|close` mapped onto TUI tabs with per-tab daemon routing. Reopen Anvil in a directory with a live daemon session and it reattaches instead of starting cold.
- &#10003; **Relay bridge &mdash; attach from the browser (#914, #917, #919&ndash;#921)** &mdash; `anvil routined relay start` pairs a browser viewer to a live session over a passage WebSocket and auto-opens it. #917 (P0) fixed a daemon tool-dispatch name-drift bug where every tool hit a stub. #919 viewer UX (input echo, thinking spinner, live context). #920 `/permissions` end-to-end over the relay. #921 ring recorder so resume/replay carries the actual conversation.
- &#10003; **Performance: shared HTTP client + parallel tools + gitignore search (#898, #899, #900)** &mdash; one shared `reqwest::Client` ends per-call TLS handshakes (#898); read-only tools in a batch fan out across worker threads and merge in original order, mutating tools still gated sequentially (#899); `grep_search` walks-and-searches simultaneously, respects `.gitignore`, and skips binaries (#900).
- &#10003; **The big modularization &mdash; 31 crates (#867, #873&ndash;#896)** &mdash; `anvil-cli` split into `anvil-ollama` / `anvil-mcp-builder` / `anvil-tui` / `anvil-wizard`; thirteen crates extracted from `runtime` (`anvil-vault`, `anvil-journal`, `anvil-permissions`, `anvil-oauth`, `anvil-hooks`, `anvil-mcp`, `anvil-memory`, `anvil-curator`, `anvil-search`, `anvil-relay`, `anvil-reflection`, `anvil-skill-chain-exec`, `anvil-routines`) with intermediate crates breaking cycles. `runtime` shrank ~70K &rarr; ~57.7K lines; workspace now 31 crates. Plus an `anvil-e2e` test crate and compile-time locale-key drift detection.
- &#10003; **Daemon hardening &mdash; P0 fixes (#902&ndash;#917, #827)** &mdash; wire the real provider runtime into the daemon (#905); connect the attach forwarder task (#906, e2e gate #910); bind REPL session I/O to the long-lived runtime (#903); read the correct PID file (#904); `/heal` checks actual process liveness (#902); auto-compaction now actually compacts (#913); anvild binary downgrade-prevention (#827, P0 security).
- &#10003; **3,926 tests passing, zero failures** &mdash; latent shared-state test races (process-global locale, env-var, atomic flags) tracked down and serialized. IPC gained a dual-transport abstraction (Unix sockets + Windows named pipes) so the daemon runs natively on all seven platforms.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.20 &mdash; May 31, 2026

**Signed Provenance, Anti-Skills, Event Routines, A/B Curator, Memory Federation + Forge Screensaver.**

- &#10003; **F1 &mdash; Signed skill provenance** &mdash; every skill carries a signed `provenance.jsonl` ledger keyed by an ed25519 keypair at `~/.anvil/keys/skill_signing.ed25519` (0600). The verifier walks the ledger from genesis on every load; a broken chain surfaces as a P0 in `/skill why <name>`. Trust-on-first-use pins an imported skill's pubkey. New `anvil skill why|pubkey|verify`.
- &#10003; **F4 &mdash; Anti-skills (negative learning)** &mdash; the curator records failure modes as `MemoryType::AntiPattern` entries with their own retrieval-order block. Anti-skills annotate rather than block; the proposal flow shows a "you tried something similar on YYYY-MM-DD and it scored N% worse" footnote.
- &#10003; **F6 &mdash; Event-triggered routines** &mdash; anvild routines accept FileWatch / Webhook / Process / Log triggers in addition to cron. Webhook listener is axum 0.8, localhost-only by default. `tick_routines()` pulled out as a pure function; four integration tests cover fire/GC/Ask/re-arm.
- &#10003; **F3 &mdash; Curator A/B evaluation** &mdash; new skills run an A/B pass against a held-out batch; winners promote, losers go to the anti-skill pool. Additive, escalate-only: a worse candidate cannot replace a better incumbent. Surfaced in `REPORT.md` + `run.json` (`ab_decisions`).
- &#10003; **F2 &mdash; Cross-machine memory federation** &mdash; x25519 key agreement, HKDF-SHA256 derivation, AES-256-GCM (fresh nonce per call), ed25519 signatures, trust-on-first-use peer pinning. `anvil memory sync|peer add|peer list`.
- &#10003; **Forge screensaver** &mdash; video-driven two-phase animation; the source mp4 is baked once into a 665 KB gzipped cell-grid bundle `include_bytes!`-d into the binary, no ffmpeg at build or run time. Falls back to overlay-only if the asset is missing.
- &#10003; **3,061 tests passing, 0 failing** &mdash; up from 2,922 at the start of the arc (+139). Three `crates/commands/` parallel-test flakes pinned to a `serial_test::serial` token.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.19 &mdash; May 22, 2026

**18 Languages. Memory Cohesion Complete. The Web-Based MCP Builder.**

- &#10003; **Internationalization &mdash; 18 locales** &mdash; the TUI, wizard, slash-command output, and remote-control viewer all flow through `rust-i18n` v4 in Rust and the new `viewer.locales.js` runtime in the browser. Tier-1 ships English, Spanish, Simplified Chinese, French, Brazilian Portuguese, Russian, Japanese, German &mdash; 264 keys each. Tier-2 adds Korean, Italian, Turkish, Vietnamese, Polish, Indonesian, Dutch, Swedish, Norwegian Bokm&aring;l, Ukrainian. Locale selection persists to `~/.anvil/config.json`, falls back to `$LANG` on first launch, applies immediately to every wizard step. `/configure` menu has a Language Picker submenu rendering native scripts (&#54620;&#44397;&#50612;, &#1056;&#1091;&#1089;&#1089;&#1082;&#1080;&#1081;, &#20013;&#25991;). Viewer ships 176 fully-wired keys covering chrome plus `vault.*` and `config.*` panels; live re-render walker on locale switch with no page reload.
- &#10003; **Seven-layer memory &mdash; all GREEN** &mdash; the 2026-05-21 cohesion audit found Layer 3 (Semantic), Layer 6 (Reflective), and Layer 7 (Cache) still partial. v2.2.19 closes all three. **Layer 3:** `/memory promote <nomination-id>` now actually persists nominated facts to disk &mdash; the v2.2.14 stub flipped a status flag without ever calling `MemoryManager::save()`. Full chain writes the fact and appends provenance comments before marking the nomination accepted. New `--target <file>` flag. **Layer 7:** file-cache path-discovery bug fixed; `memory_budget` no longer checks a path that doesn&rsquo;t exist in the project-scoped layout. `/memory show cache` enumerates file-cache, command-cache, and QMD-cache stats. `/memory prune cache --dry-run` walks both `FileCacheManager` and `CommandCacheManager`. **Layers 1, 2, 4:** `/memory layer 1` renders a live snapshot via `PromptSectionsExt::iter_by_kind()`. `/memory show episodic` unifies daily summaries, history archives, workspace sessions. `/memory prune episodic` adds TTL retention with a trash-bin safety net. `/memory show procedural` consolidates GoalManager + skills + CronManager.
- &#10003; **AnvilHub `/build` page + `anvil-mcp-builder` micro-service** &mdash; the v2.2.18 `/mcp builder` TUI wizard now has a web counterpart. Three endpoints: `POST /api/builder/spec` (LLM-generated spec from free-text, SSE-streamed), `POST /api/builder/generate` (turns spec into a base64 tarball &mdash; Node.js, TypeScript, or Python templates), `POST /api/builder/sandbox` (extracts the tarball and runs `anvil-sandbox-runner` network-cut). Operator OAuth token loaded from the Anvil vault at startup, never from `.env`. Sandbox endpoint gated on publisher standing &mdash; user must be in the `anvilhub-publishers` Authentik group OR have at least one HubPackage published. 5-minute Map cache; falls closed on backend error.
- &#10003; **MCP pagination (CC v2.1.144-B6 / v2.1.146-B2)** &mdash; MCP client now consumes the full `nextCursor` / `has_more` pagination chain for `tools/list`, `resources/list`, `resources/templates/list`, `prompts/list`. Previously MCP servers with paginated responses had everything beyond page 1 silently dropped.
- &#10003; **Spinner/elapsed-time freeze fix (CC v2.1.145-B3)** &mdash; TUI render queue wakes from a wall-clock timer in addition to input events. After terminal refocus or resize, the spinner and elapsed-time display no longer freeze until next keypress. New `RedrawReason::TerminalStructural` routes Resize/FocusGained/FocusLost through the soft draw path (no ANSI clear flash, per the photosensitivity rule from v2.2.17).
- &#10003; **MCP `permissions.allow` honored (CC community #61077 SECURITY)** &mdash; `permissions.allow` rules with patterns like `mcp__server__tool` or `mcp__server__*` are now consulted at MCP tool dispatch time. Previously the allowlist was loaded but the MCP dispatch path bypassed it and always prompted.
- &#10003; **Bash env-var permission bypass (CC v2.1.145-B1 SECURITY)** &mdash; Bash patterns of the form `KEY=VALUE command` are decomposed and the command portion checked against the allowlist.
- &#10003; **Skill fork-context recursion guard (CC v2.1.145-B4)** &mdash; a skill cannot transitively invoke itself; recursion check uses the full skill ancestry chain.
- &#10003; **Resume session model preservation (CC community #61068)** &mdash; `anvil --resume` restores the model that was active when the session was saved, not the global default.
- &#10003; **CC parity P2 sweep** &mdash; API startup 15s timeout for side-channel calls (#722), mime-type magic-number fallback in `Read` (#723), `/branch` history recovery post-EnterWorktree via `worktree_ops::original_sessions_dir()` (#724), MCP image fallback for unsupported MIME &mdash; saves to `~/.anvil/mcp-images/<sha256>.<ext>` (#725), skill watcher FD exhaustion prevention &mdash; excludes `target/`, `node_modules/`, `.git/`, `dist/`, `build/`, `.cache/`, `.next/`, `__pycache__/` (#726), theme color reset on first `/session rename` fixed (#727), EnterWorktree MCP config preservation via snapshot before `set_current_dir` (#728).
- &#10003; **`anvild` separate process name across 7 platforms (#766)** &mdash; the background OAuth-refresh + routines daemon now runs as `anvild`, not `anvil daemon foreground`. New `anvild_path_from(anvil_binary)` helper rewrites the binary path used by every supervisor unit &mdash; macOS LaunchAgent, Linux user-systemd, FreeBSD/NetBSD rc.d, Windows Task Scheduler &mdash; plus the in-TUI `daemon::spawn_detached` fallback. `ps -ef | grep daemon` now shows `anvild`. `install.sh` and `install.ps1` create the `anvild` symlink (Unix) or hardlink (Windows NTFS) alongside the main binary.
- &#10003; **Wizard P0 bundle &mdash; Ollama modal clipping, daemon prompt alt-screen, vertical-split hint (#767, #768, #769)** &mdash; `ConfirmModal` height now derived from body wrap so Yes/No buttons stay visible regardless of body length; new `Ctrl+B` Back keybind. The &ldquo;Install anvild as background service?&rdquo; prompt moved into the alt-screen as Step 8 of 9 (no more drop-to-CLI mid-wizard). New 1-line hint above BUILD section in the rail bottom group: `&#8997;+drag deck only` on macOS, `Alt+drag deck only` on Linux/Windows/BSD. i18n keys added across all 18 locales.
- &#10003; **Release-pipeline step-gates (#714, #730)** &mdash; `scripts/release.sh` now wraps every phase in `step "PN: <description>"` + `ok "PN"` / `fail "PN"` markers. New `scripts/release-helpers/step-gates.sh` provides primitives + JSON status persistence + an EXIT-trap silent-exit detector. Closes the v2.2.18 Phase 6 silent-exit class. `scripts/test-release-gates.sh` runs release.sh in `--dry-run` and asserts every expected phase fires START + terminal marker exactly once.
- &#10003; **Net +50 tests across the workspace** &mdash; i18n drift gate + picker invariant + 8 locale-load; +9 episodic / +6 promote / +6 cache / +5 working / +3 procedural for memory cohesion; +35 across all 15 CC-parity fixes; +7 publisher-standing tests in the new micro-service.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.18 &mdash; May 20, 2026

**The Web Viewer Lands. Autocompact Gets Honest. Mouse Capture Off by Default.**

- &#10003; **Web viewer &mdash; full TUI parity (#680, #681, #683, #692, #695, #696)** &mdash; `passage` relay and AnvilHub viewer get a tab-routing rewrite matching the TUI&rsquo;s per-tab architecture. `/tab new`, `/tab rename`, `/tab switch`, `Ctrl+T`. Per-tab `user_message` and `slash_result` routing (no more cross-tab leakage). Default layout `vertical_split + tabs`. Cost-type chip (OAuth / local / cloud) instead of fabricated dollar figures. Cached + broadcast `MemorySnapshot` so the memory rail populates on reconnect. `SessionMeta` carries `context_max` and `build_sha`. Default-allow forwarding for unhandled viewer messages. Collapsible tool cards. Always-visible slash bar with `Cmd+K` palette.
- &#10003; **Autocompact threshold fix (#697 CRITICAL)** &mdash; `maybe_auto_compact` was measuring against `max_output_tokens` (8K&ndash;16K) instead of `context_window` (64K&ndash;200K+). Sessions on long-context models were compacting at roughly 6K input tokens. Now reads `session.context_window` and ignores `max_output_tokens` entirely. New OTel span `anvil.autocompact.threshold` emits `context_window`, `used_tokens`, `threshold_pct`, `triggered`. `/compact why` prints the full threshold calculation.
- &#10003; **Mouse capture default OFF (#696 P4)** &mdash; mouse capture disabled by default on all platforms, restoring terminal copy-paste (Cmd+C / Ctrl+Shift+C / Ctrl+C) for users who hadn&rsquo;t opted in. One-time first-run toast shows `/config mouse_capture true` and `--mouse`. `mouse_capture_default_off_regression` test asserts the default at the type level.
- &#10003; **Bracketed paste in textarea modals (#685)** &mdash; multi-line paste now works inside `/mcp builder` and other wizard textareas. Wires `tui::paste::handle_paste` into the textarea event loop with `
` &rarr; `
` normalization.
- &#10003; **Per-tab relay routing (#696)** &mdash; `relay::user_message` and `relay::slash_result` route to active `Tab.id` rather than broadcasting. Concurrent inference in two tabs no longer leaks between them.
- &#10003; **OAuth / local / cloud cost label (#696 P1)** &mdash; TUI status footer shows a semantic cost-type chip instead of a fabricated dollar amount for providers where per-token cost is not knowable.
- &#10003; **`MemorySnapshot` rail parity (#695)** &mdash; vertical-split rail uses `layouts::common` helpers instead of a hand-rolled draw path. Same fidelity as the classic inline view.
- &#10003; **Alt-screen raw mode restore (#688)** &mdash; `restore_alt_screen` re-enables raw mode on return from inline operations. Was the root cause of &ldquo;keyboard stops working after `/mcp builder` cancel&rdquo; in v2.2.17.
- &#10003; **`FORCE_FULL_REDRAW` consumption (#688)** &mdash; consumed inside `handle_repl_command_tui` so the blank-screen-after-cancel regression cannot recur.
- &#10003; **Mouse capture + alt-screen pairing (#688)** &mdash; mouse capture state is paired with alt-screen state. Enabling mouse capture outside the alt-screen no longer leaves the terminal inconsistent after exit.
- &#10003; **Force full redraw after inline-op restore (#687)** &mdash; any inline operation that restores the alt-screen forces a full redraw. Eliminates partial-frame artifacts.
- &#10003; **Textarea keybinds corrected (#686)** &mdash; `Enter` submits, `Ctrl+N` inserts a newline. Previously inverted.
- &#10003; **`/mcp builder` long-description textarea (#684)** &mdash; long-description field is now a multi-line textarea modal instead of a single-line input.
- &#10003; **PermissionPrompt round-trip regression test (#677)** &mdash; end-to-end test fires a tool call that requires a permission prompt, verifies the prompt renders, sends the approval, asserts the turn completes. Guards permission-prompt state from desyncing with the turn loop.
- &#10003; **Release-pipeline Phase 6 silent-exit guard (#654)** &mdash; `scripts/release.sh` Phase 6 wraps every SSH hop in an explicit `|| { echo "Phase 6 SSH failed"; exit 1; }` guard. Previously a failed remote call could terminate the script with exit 0, leaving subsequent surface updates unrun.
- &#10003; **anvil-release MCP host targeting fix (#698 CRITICAL)** &mdash; `anvilhub_pm2_host` reverted to `dev0001` after `#655` incorrectly routed pm2 ops to CT 113 (which is dead). Apache vhost has always proxied `anvilhub.culpur.net` to `dev0001:3100`.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.17 &mdash; May 18, 2026

**The Setup Wizard, Reflection, Sandboxing, and the Source Viewer.**

- &#10003; **New first-run wizard** &mdash; welcome card &rarr; nine modal steps &rarr; vault unlock &rarr; main TUI, all in one alt-screen. Per-step descriptions explain why each question is asked. OAuth waits poll on 100&nbsp;ms and stream the elapsed counter live. Step 9 is CC migration in-modal. Brighter grey font for modal text.
- &#10003; **Autonomous reflection loop** &mdash; stuck-detector switches strategy and writes a multi-attempt scratchpad when a turn loops without progress.
- &#10003; **`anvil-sandbox-runner` companion binary** &mdash; hub-install detonation runs in an isolated binary, shipped alongside `anvil` on all seven platforms.
- &#10003; **AnvilHub source viewer** &mdash; every one of the 558 HubPackages has a viewable source archive; 547 packages got synthesized `Documentation` tabs from DB columns.
- &#10003; **Vertical-split rail-stays-painted fix (#648)** &mdash; ratatui `swap_buffers` contract violation in the `#574` region-gated repaint surfaced as blank/garbage rails after wizard exit. All three layouts now always paint every region every frame.
- &#10003; **TUI flash eliminated on Gnome Terminal/alacritty (#622, #629)** &mdash; full-screen `Clear()` is now gated on `DirtyRegions::ALL` and `commit_pending_redraw` no longer routes `TextDelta` through `terminal.clear()`. Photosensitivity hazard during streaming output resolved.
- &#10003; **Wizard mouse-capture default OFF (#623)** &mdash; native text selection now works cross-platform. Banner is no longer Mac-only.
- &#10003; **`/agent compose` + `/agent traits` rewired (#624)** &mdash; no more `println!` corrupting the alt-screen. 23 BUG sites fixed in the broader println audit (#626), with `#![deny(clippy::print_stdout, clippy::print_stderr)]` on every TUI-touching file and a regression test to block future drift.
- &#10003; **In-TUI ConfirmModal + PasswordModal** &mdash; vault unlock for returning users is now a modal in the existing alt-screen, not a CLI prompt; ConfirmModal supports two-button destructive-action confirmation.
- &#10003; **Vertical-split Shift+drag deck-only selection (#625)** &mdash; rail no longer comes along when you select conversation text.
- &#10003; **Vertical-split rail keybinds wired (#634)** &mdash; g / d / s / a / Ctrl+R now work in the rail, with a drift gate to keep them wired.
- &#10003; **Reactive compaction sizes from actual overflow (#564)** &mdash; summary-size budget is now seeded from the real overflow delta, not a fixed guess.
- &#10003; **`ANVIL_STOP_HOOK_BLOCK_CAP` (#566)** &mdash; caps Stop-hook blocking to prevent infinite-loop runaway if a hook mis-fires.
- &#10003; **Session auto-titling wired (#580)** &mdash; `derive_title_from_first_message()` now actually drives the trigger.
- &#10003; **Hook PWD refresh after worktree switch (#561)** &mdash; PWD-relative hooks no longer go stale when `EnterWorktree` runs.
- &#10003; **Welcome banner names active provider for 3P users (#562)** &mdash; no more hardcoded Anthropic when you're on Groq/Bedrock/etc.
- &#10003; **`release-surfaces.yaml` enforcement gate (#614)** &mdash; one manifest is the single source of truth for every public surface; `scripts/verify-release-surfaces.sh` is the gate.
- &#10003; **AnvilHub `/sha256/2.2.17.txt` published** &mdash; out-of-band checksum manifest for primary-source SHA256 verification.
- &#10003; **`install.sh` + `install.ps1` rebuilt** &mdash; live versions on `anvilhub.culpur.net` updated to the proper `/api/version`-aware variants. Fixes the `tag_name` regex breakage (#619) and the hardcoded `windows-msvc` Windows target (#612).
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.16 &mdash; May 17, 2026

**The TUI Layout System. Eight live-switchable layout variants on a per-tab `TuiLayoutConfig`. New default: Vertical Split + Tabs.**

- &#10003; **TUI Layout System** &mdash; four layout architectures (Vertical Split / Classic / Three-Pane / Journal) &times; tabs/no-tabs = eight variants, every one a real renderer. Per-tab `TuiLayoutConfig`. `/layout list`, `/layout <alias>`, `/layout <alias> --global` (writes to `~/.anvil/config.json`), `/layout reset`. State-machine contract enforced by integration tests; shared session state (log, input, model) survives the switch; terminal clear on switch so previous cells don't bleed.
- &#10003; **New default: Vertical Split + Tabs** &mdash; persistent left rail (sessions, agents, tools, MEMORY, gate state) next to a swappable right deck. Rail owns all chrome (banner, status, model, cost); deck has input only. Mouse-draggable split anchor. Migration-safe: users with an explicit `tui_layout` setting keep their value; only upgraders without the key see the change, plus a one-time intro toast.
- &#10003; **Wizard step 7 highlight** &mdash; first-run setup wizard now defaults to Vertical Split (option `[1]`); Classic moves to `[2]`. New installs that just press Enter land on the rail+deck view.
- &#10003; **Slash-completion popup wired into all renderers** &mdash; completion works regardless of which layout is active. Every render path also redraws on every keystroke so input is live.
- &#10003; **Three-pane Insert discoverability** &mdash; framed hint + ghost input makes the always-on input legible; CONTEXT band uses `Constraint::Fill` so it actually fills available height. Vim modal removed &mdash; typing always edits the input.
- &#10003; **Paste handling rebuild** &mdash; consolidated paste handler routes terminal bracketed paste, OSC52, and drag-and-drop file paths. Mouse capture is off by default so native terminal copy works. `ContentBlock::Document` is wired end-to-end for PDF and Office docs &mdash; dropping a `.pdf` attaches it as a document block to the next request. Long-paste placeholder collapses multi-KB pastes to a `[Pasted N chars]` token in history. Keystroke-burst detection converts drag-and-drop on terminals without OSC52 into a single paste.
- &#10003; **Ctrl+C mid-stream cancel across all 7 providers** &mdash; `DefaultRuntimeClient` honors the cancel token in every provider implementation. `tokio::select!` wraps the blocking HTTP read so the connection actually closes when the token fires. New wiremock integration test covers the cancel path end-to-end.
- &#10003; **Vertical-split rail polish** &mdash; uppercase section headers, cross-tab status aggregates, tool-call boxes close cleanly, markdown styled, cost rendered to 2 decimals, QMD folded into MEMORY, agent tab-binding, split-anchor draggable.
- &#10003; **AnvilHub verification gate** &mdash; `/hub status` ships all 8 axes, `HubPackage` carries verified-badge structs, `require_verified` config gate, `/plugin update REVOKED` guard, update probe prefers anvilhub `/api/version` and falls back to GitHub Releases.
- &#10003; **TUI correctness long tail** &mdash; `/vault unlock` retries up to 3 times and pre-fills the prompt on failure; welcome banner names the active provider, not hardcoded Anthropic; session-title heuristic skips a bare URL as first message; 5xx errors name the configured provider/gateway; spinner color warms warm-green&rarr;amber&rarr;red on elapsed time; `Read` offset accepts string forms with whitespace/`+` prefix; `ANVIL_MCP_TOOL_TIMEOUT` env override per request; async `fetch_all_configured_models` with timeout + Ctrl+C cancel; strict RFC 6749 token-exchange parser + startup validator; lenient scopes deserializer prevents OAuth lockout; `ProviderLoginModal` for in-TUI OAuth/API-key flows; layout-switch terminal clear prevents stale-cell ghosting.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.15 &mdash; May 15, 2026

**The largest release in Anvil's history. 121 commits over v2.2.13. Supersedes the corrupted v2.2.14 tag (never published).**

- &#10003; **Provider catalog 5 &rarr; 31** &mdash; AWS Bedrock (manual SigV4, no AWS SDK), Google Gemini OAuth + Antigravity (Code Assist OAuth with honest Anvil identification &mdash; not VS Code spoofing), GitHub Copilot (device flow), Azure OpenAI (deployment-name + api-version), Cursor Cloud Agents (repo-bound), plus 24 OpenAI-compatible providers: Groq, Fireworks, Mistral, Perplexity, DeepSeek, Together AI, DeepInfra, Cerebras, NVIDIA NIM, HuggingFace, Moonshot, Nebius, Scaleway, STACKIT, Baseten, Cortecs, 302.AI, ZAI, OpenRouter, LMStudio, Chutes, MiniMax, OpenCode, OpenCode-Go.
- &#10003; **`/cursor` first-class command tree** &mdash; six TAB-completable subcommands (`launch`, `list`, `get`, `cancel`, `artifacts`, `stream`) for Cursor's public Cloud Agents API. Repo-bound by design &mdash; `git remote get-url origin` is mandatory.
- &#10003; **`/model` cross-provider unified picker** &mdash; one TAB-completable list across every configured provider, provider-prefixed (`anthropic/claude-4.5-sonnet`, `groq/llama-3.3-70b`, `cursor/claude-4-sonnet-thinking`). Atomic switch updates routing + system prompt identity + TUI chrome together. Live-fetched from each provider's `/models` endpoint; static registry is fallback only.
- &#10003; **Memory Cohesion Arc** &mdash; the seven-layer memory system from v2.2.5 (Sensory / Working / Episodic / Semantic / Procedural / Reflective / Long-term) cohesion-tested end-to-end. Retrieval-order block in system prompt, `WorkingMemorySnapshot` as `Vec<PromptSection>`, `PermissionMemory` wired into permission gate, `/file-cache` real handler, egress allowlist in settings + `/policy view`, `/memory why` injecting daily summaries.
- &#10003; **Capability Cohesion Arc** &mdash; every Anvil capability now meets an 8-axis contract (definition / registration / completion / handler / dispatch / rendering / permission gate / OTel + tests). Build-time drift gate enforces it. Slash dispatch unified at one site. Stub messages banned.
- &#10003; **CC&rarr;Anvil migration** &mdash; `anvil import claude-code` brings prior assistant work forward verbatim with provenance frontmatter. Memory entries get `imported_from` stamps. Project instruction files merged without clobbering. Settings get conflict detection. Skills get collision handling. Past sessions (up to ~1,800 JSONL, ~1&nbsp;GB) optionally summarized into daily records. The day-2 command `anvil memory clean` rewrites entries through a configurable LLM and detects duplicate meanings via Jaccard similarity. First-run wizard offers migration as opt-in.
- &#10003; **CC parity catch-up v2.1.132 &rarr; v2.1.139** &mdash; 17 features + 7 security/verify items: `ANVIL_PROJECT_DIR` / `ANVIL_SESSION_ID` / `ANVIL_DISABLE_ALTERNATE_SCREEN` / `ANVIL_EFFORT` env propagation, transcript view nav, cross-session `anvil agents` live monitor, `worktree.baseRef`, `autoMode.hard_deny` short-circuit, hook `continueOnBlock`, hook args `string[]` exec form, `/scroll-speed` with live preview, `anvil plugin details`, subagent OTel headers with parent-agent-id, plus security gates on `--resume`/`--continue` underscore paths, plan-mode + Edit allow rule write blocking, MCP content-block result visibility, `Skill(name *)` wildcard delimiter check, settings hot-reload on symlinks, stream-idle false-fire elimination, multi-image paste correctness.
- &#10003; **Token economy + reliability** &mdash; file-fingerprint cache wired into `read_file`/`write_file`/`edit_file`/system prompt (shipped dormant in v2.2.11). Command-output cache wired into `glob_search` and `grep_search`. WebFetch + WebSearch get 5-min and 1-hour TTL caches. Skill-chaining engine depth-3 wired (suggestion engine; executor lands in v2.2.16). Auto-promote engine for `/memory` notes active.
- &#10003; **Honesty contract codified** &mdash; test-suite-enforced contracts: no silent deferral, 8-axis capability contract, CC-only naming, changelog preservation (historical entries byte-immutable), migration instinct (bring everything verbatim-with-flag), live-model-list (not registry), atomic provider/model switch.
- &#10003; **Seven platforms** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. Every binary SHA256-verified.

### v2.2.14 &mdash; (internal-only, never publicly released)

v2.2.14 was tagged internally but never published as binaries due to a release-pipeline incident. All v2.2.14 work is included in v2.2.15 above (Memory Cohesion Arc, Capability Cohesion Arc, CC parity v2.1.132-139, per-tab parallel inference fixes, file-fingerprint cache wiring, auto-promote engine).

### v2.2.13 &mdash; May 11, 2026

**Windows is back, BSD joins, routines on disk** &mdash; seven platforms now.

- &#10003; **FreeBSD x86_64 + NetBSD x86_64 binaries** &mdash; first-ever BSD support. Every binary SHA256-verified and signed by the release pipeline, with paired `.sha256` manifests at [anvilhub.culpur.net/sha256/](https://anvilhub.culpur.net/sha256/).
- &#10003; **Windows x86_64 is back** &mdash; the v2.2.12 hold is fixed. ssh-agent auth is now `#[cfg(unix)]`-gated with a clean Windows stub. The rest of the SSH driver (key-file, password, kbd-interactive) works on Windows exactly as on Unix.
- &#10003; **Seven platforms total** &mdash; macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64.
- &#10003; **Release pipeline hardening** &mdash; build errors now hard-fail instead of silently producing partial releases (the v2.2.12 incident where the Windows build failed silently and a stale artifact was published).
- &#10003; **Routines foundation on disk** &mdash; schedule grammar (duration, interval, cron, ISO timestamp), output archive with `[SILENT]` early-stop, and SHA-256 input-hash packet schema. 63 new tests. The v2.2.14 daemon ships on top.
- &#10003; FreeBSD ARM64 and OpenBSD x86_64 are not in this release &mdash; the Rust toolchain does not publish a precompiled standard library for either target. Both are queued for v2.2.14.
- &#10003; 1,146 workspace tests passing across every crate. Zero failures. Zero warnings.

### v2.2.12 &mdash; May 11, 2026

**Parallel and Transparent** &mdash; the app gets out of your way.

- &#10003; Per-tab parallel inference &mdash; each tab owns its own `Arc<Mutex<ConversationRuntime>>` and runs turns on dedicated worker threads; fire prompts in multiple tabs simultaneously
- &#10003; Mid-turn TUI responsiveness &mdash; Ctrl+T, F2/F3, `/ssh`, and cross-tab submit all respond immediately during streaming; the app is interactive throughout a turn
- &#10003; Tool-call cards &mdash; every Glob/Grep/Read/Write/Edit/Bash/WebSearch/MCP call renders a bordered card with actual input (pattern, path, command) the moment it fires; Ctrl+O expands to full JSON + result
- &#10003; SSH tabs &mdash; `/ssh host` opens a modal connection form with russh backend, vt100 rendering, Ctrl+B prefix keys; connections saved as vault `HostCredential` aliases
- &#10003; Tab bar markers &mdash; `*` (unread), `&#9888;` (pending permission), `&#215;` (clickable close); terminal-friendly navigation (F2/F3/Ctrl+arrow/Alt+digit/click)
- &#10003; Session continuity &mdash; `anvil --continue` honors saved model from `.meta.json` sidecar; Ollama sessions reconnect without credential errors; exit prints resume commands
- &#10003; Scrollback fix &mdash; HISTORICAL VIEW was showing only 1&ndash;4 chars per assistant line; pending text growth now invalidates cached scrollback line vectors
- &#10003; `/quit` no longer deadlocks &mdash; self-recursive mutex in `record_daily` fixed
- &#10003; First-run setup wizard &mdash; mouse capture, theme, permission mode opt-ins on first launch; `anvil setup` / `anvil --first-run` to reconfigure
- &#10003; anvil(1) manpage ships with Homebrew installs
- &#10003; `/clear` clears workspace context across all tabs, not just the active one
- &#10003; Release pipeline hardening &mdash; tag-vs-HEAD pre-flight, build-from-tag, php-lint guard, changelog.json render-time injection on AnvilHub
- &#10003; 318 tests passing, ~22 MB binary

### v2.2.11 &mdash; May 9, 2026

**Outweigh-them-all release** &mdash; self-awareness plus ten core surfaces in one cut.

- &#10003; System prompt now leads with "You are Anvil v2.2.11" and references the currently loaded model + provider in every turn &mdash; no more hallucinating a different identity
- &#10003; **W1 hook events:** PreToolUse, PostToolUse, UserPromptSubmit, SessionStart, SessionEnd, PreCompact, Notification &mdash; full CC parity
- &#10003; **W2 effort slider:** `/effort low|medium|high` &mdash; tune reasoning depth per turn, persisted per session
- &#10003; **W3 goal persistence:** per-session goals survive `/clear` and reconnect, surfaced in status line
- &#10003; **W4 named profiles:** save and switch (provider, model, effort, output style) tuples by name
- &#10003; **W5 published JSON schema:** `settings.json` fully typed, IDE-completable, served at `anvilhub.culpur.net/schema/settings.json`
- &#10003; **W6 OpenTelemetry events:** `OTEL_EXPORTER_OTLP_ENDPOINT` support, permission_decision + tool_call + token_usage spans
- &#10003; **W7 custom output styles:** define your own `/output-style` names in settings, ship them as plugins
- &#10003; **W8 reviewer-agent approval gate:** optional second-agent review before file writes, configurable threshold
- &#10003; **W9 anvil mcp-server mode:** run Anvil itself as an MCP server &mdash; expose agents and tools to any MCP client
- &#10003; **W10 requirements.toml admin policy floor:** enforce minimum versions, required plugins, denied domains org-wide
- &#10003; Rename `CLAUDE.md` &rarr; `ANVIL.md` across user-facing strings + the anvil-md-curator skill
- &#10003; Build-time fix: `cargo:rerun-if-changed` now watches the actual ref file, not just `.git/HEAD` &mdash; GIT_SHA stays current across rebuilds

### v2.2.10 &mdash; May 6, 2026

**TUI usability patch.**

- &#10003; TUI: long lines wrap instead of right-truncating
- &#10003; TUI: native terminal selection restored (Shift-drag works again)
- &#10003; TUI: tool-result summaries now actually summarize
- &#10003; Release pipeline: regenerate sha256 manifests every build + verify-before-release
- &#10003; Release pipeline: fix repo target on `gh release` calls

### v2.2.9 &mdash; May 6, 2026

**Claude Code parity catch-up.**

- &#10003; Claude Code parity: `--print`/`--agent` honor frontmatter, plugin prune, scroll snap
- &#10003; Subagent summaries, `/mcp` tool count, API 400 error surfacing
- &#10003; OTEL env vars, MCP reconnect summary, worktree HEAD detection
- &#10003; Spinner red on errors, theme refresh, env vars (`DISABLE_UPDATES`, `HIDE_CWD`, `EFFORT`, `AI_AGENT`)
- &#10003; Long URL clickability, `/clear` tab title cleanup, editor handoff hardening

### v2.2.8 &mdash; April 22, 2026

**PAI-inspired composition, learning, and robustness.**

- &#10003; `/agent compose <traits...> "<task>"` &mdash; trait-based agent composition engine, 30-trait catalogue (expertise &times; personality &times; approach), dimension-conflicts hard-error by default. Adapted from Miessler's `Personal_AI_Infrastructure`.
- &#10003; Skill front-matter `triggers` with suggest-not-auto UX &mdash; three bundled reference skills (`security-audit`, `code-review`, `terse`). Never auto-inject (prompt-injection vector); user confirms via `/skill load <name>`.
- &#10003; Prompt-type hooks &mdash; plugin lifecycle hooks can now inject a string into the next model turn with `{tool_name}` / `{cwd}` / `{date}` / `{model}` interpolation. Backward-compatible with bare-string command hooks.
- &#10003; `anvil skill-eval` &mdash; three-arm evaluation harness (`__baseline__` / `__terse__` / `<skill>`) with honest caveats baked into every report. Adapted from `caveman`.
- &#10003; `/output-style precise|condensed` &mdash; user-selectable global response style. Precise (default) preserves full sentences; condensed activates the bundled `terse` skill. **Never auto-applies condensed** &mdash; Auto-Clarity rules still fire for security / irreversible / multi-step / consent contexts even when condensed.
- &#10003; Plugin loader is forward-compatible &mdash; a single bad manifest no longer crashes the entire binary. `PluginLoadDiagnostic` surfaces per-plugin warnings on stderr.
- &#10003; Bundled plugins are now embedded in the binary via `include_dir` &mdash; Homebrew users' bundled plugins are visible; developers' installed binaries no longer depend on their live source tree.
- &#10003; Claude-Code-parity bug fixes: 429 `Retry-After` minimum; 5-min stream dead-air timeout; configurable request timeout (`ANVIL_API_TIMEOUT_MS`); `/model` warns on mid-conversation switch; DangerFullAccess stability invariants.
- &#10003; 756 tests passing.

### v2.2.7 &mdash; April 21, 2026

**Cross-OS installers, `anvil upgrade`, shell completions, curated Ollama menu, Windows fixes, release-pipeline hardening.**

- &#10003; `install.sh` (macOS/Linux) and `install.ps1` (Windows) with SHA256 verification from anvilhub.culpur.net with GitHub fallback &mdash; aborts on dual failure, no unverified binary ever lands
- &#10003; `anvil upgrade`, `anvil --check`, `anvil --setup`, `anvil --uninstall` &mdash; full lifecycle from the binary itself
- &#10003; Shell completions for bash, zsh, fish, and PowerShell &mdash; all 138 slash commands, subcommands, flags, provider and model names
- &#10003; First-run wizard: curated Ollama model menu &mdash; Llama 3.x, Qwen 3 / 2.5-Coder, Mistral Nemo, Gemma 3, Phi 4, Code Llama, Codestral, per-model confirmation
- &#10003; TUI scrollback + text selection via Shift-drag pass-through to the terminal emulator
- &#10003; Windows: correct `HOME` / `PATH` / `PATHEXT` handling, `.exe` on respawn, cmd.exe-aware install detection
- &#10003; QMD cross-platform discovery &mdash; no more hard-coded Unix socket paths
- &#10003; Ollama tool-use: multi-format parser (Anthropic, OpenAI, XML, JSON-fence, natural language) with fail-loud on ambiguity
- &#10003; Remote-control 503 fixed &mdash; relay WebSocket process declaration corrected
- &#10003; Release pipeline: per-binary embedded-version audit gate &mdash; makes the v2.2.6 Windows-exe-labeled-as-2.2.1 class of bug impossible
- &#10003; 618 tests passing, zero warnings

### v2.2.6 &mdash; April 20, 2026

**Command Parity, Deep Autocomplete, Web Config, AnvilHub Installer.**

- &#10003; 17 web config panels &mdash; vault, notifications, SSH, Docker/K8s, memory, and more
- &#10003; Full Status Line editor in browser &mdash; 36 widgets, 16 presets, drag-and-drop, live preview
- &#10003; AnvilHub installer &mdash; search, install, restart prompt &mdash; vault-gated, telemetry-tracked
- &#10003; Deep hierarchical autocomplete &mdash; `/vault store <Tab>` &rarr; 21 credential types
- &#10003; 8 previously-broken TUI handlers now working &mdash; `/mcp`, `/plugins`, `/session`, `/daily`, and more
- &#10003; New commands &mdash; `/tab`, `/fork`, `/share`, `/audit`, `/restart`
- &#10003; Self-respawn on macOS/Linux after plugin installs

### v2.2.5 &mdash; April 19, 2026

**Intelligent Memory System &mdash; six major features.**

- &#10003; Interactive Status Line Editor &mdash; full TUI editor with 6 sub-screens + WebUI drag-and-drop visual editor
- &#10003; 37 widgets, 16 presets (8 emoji-rich themes), per-widget category colors
- &#10003; Code Productivity Dashboard &mdash; live git diff tracking, `/productivity` command
- &#10003; MCP Server Manager &mdash; `/mcp` command, live McpStatus widget
- &#10003; Session History Browser &mdash; `/history-archive stats` with model breakdown
- &#10003; Plugin System UI &mdash; web viewer management panel with config toggles
- &#10003; Agent Panel Expansion &mdash; web viewer agent management buttons

### v2.2.4 &mdash; April 16, 2026

**Security Hardening + Optimization** &mdash; 17 audit findings fixed.

- &#10003; Constant-time HMAC verification, plugin command injection prevention
- &#10003; Path traversal protection, cryptographic session IDs
- &#10003; 110 functions made const fn, zero compiler warnings
- &#10003; RC widget: live client count with connect/disconnect signals

### v2.2.3 &mdash; April 15, 2026

**Six Major Features** &mdash; interactive editor, productivity, MCP, history, plugins, agents.

- &#10003; Interactive Status Line Editor &mdash; 37 widgets, 16 presets, visual editor
- &#10003; Code Productivity Dashboard &mdash; live git diff tracking
- &#10003; MCP Server Manager, Session History Browser, Plugin UI, Agent Panel

### v2.2.2 &mdash; April 14, 2026

**Customizable Widget-Based Status Line** &mdash; 8 presets for different workflows.

- &#10003; Widget-based status line system &mdash; 28 widget types, dynamic rendering
- &#10003; 8 presets: default, minimal, developer, token-heavy, git-heavy, compact, cost-focused, streamer
- &#10003; `/configure statusline` command with full tab completion
- &#10003; Web viewer config panel gains Status Line preset selector
- &#10003; Dynamic footer height &mdash; 2-line presets maximize content area

### v2.2.1 &mdash; April 14, 2026

**URL rendering fix, context-aware vault, CI/CD automation.**

- &#10003; URL rendering fix &mdash; terminal hyperlinks render correctly across all providers
- &#10003; Context-aware vault &mdash; vault auto-selects credentials based on active project context
- &#10003; CI/CD automation &mdash; `/cicd` command scaffolds pipelines for GitHub Actions and GitLab CI

### v2.2.0 &mdash; April 14, 2026

**Typed Credential Vault** &mdash; the vault is now the single source of truth for ALL sensitive data.

- &#10003; Typed credential entries &mdash; `name`, `type`, `value`, `tags`, `created_at`, `rotated_at`
- &#10003; Vault covers API keys, SSH keys, TLS certs, tokens, and environment secrets
- &#10003; `/vault add` &mdash; interactive typed credential entry with category selection
- &#10003; `/vault rotate` &mdash; rotate any credential in-place, preserving audit history
- &#10003; `/vault export` &mdash; encrypted vault export for backup and migration
- &#10003; `/vault inject` &mdash; load vault secrets into shell env for any subprocess
- &#10003; Audit trail v2 &mdash; every vault access logged with timestamp, operation, and credential type

### v2.1.4 &mdash; April 14, 2026

**Browser configuration panel, Gemini provider, slash command execution in web viewer.**

- &#10003; Browser-based visual configuration panel
- &#10003; Google Gemini as 5th provider
- &#10003; Slash commands execute from web viewer
- &#10003; 30+ commands with subcommand completions

### v2.1.3 &mdash; April 14, 2026

**Edition 2024, dependency modernization, Claude Code parity.**

- &#10003; Focus view &mdash; `/focus` hides sidebars and agent panels for distraction-free mode
- &#10003; Context-low warning &mdash; proactive alert before auto-compaction fires
- &#10003; Stalled stream handling &mdash; detects and recovers from stuck token streams
- &#10003; `/loop` and `/proactive` &mdash; recurring prompt loops and proactive agent nudges

### v2.1.2 &mdash; April 14, 2026

**Credential scanner, egress control, conversation branching &mdash; 16 new features.**

- &#10003; Credential auto-detection &mdash; scans env vars, dotfiles, SSH keys, TLS certs
- &#10003; Network egress control &mdash; configurable domain allowlist
- &#10003; Signed session transcripts &mdash; HMAC-SHA256 audit trail
- &#10003; Conversation branching &mdash; `/fork` to snapshot and branch
- &#10003; Markdown session export &mdash; `/export md` with code blocks
- &#10003; Remote control browser auto-open
- &#10003; Expanded cost tracking &mdash; OpenAI, xAI, Ollama pricing
- &#10003; Smart context compaction &mdash; preserves recent messages and code blocks

### v2.1.1 &mdash; April 13, 2026

**Live streaming responses, thinking status indicator, remote control.**

- &#10003; Live streaming responses &mdash; real-time token-by-token rendering
- &#10003; Remote control &mdash; `/remote-control` to share sessions via browser
- &#10003; Thinking mode &mdash; `/think` enables extended reasoning

### v2.1.0 &mdash; April 8, 2026

**Encrypted vault, file sandbox, modular architecture** &mdash; security-first release.

### v2.0.0 &mdash; April 8, 2026

**Full Claude Code Parity** &mdash; multi-agent system, TUI tabs, context management.

### v1.0.4 &mdash; April 7, 2026

Multi-agent system &mdash; 7 agent types with task orchestration.

### v1.0.3 &mdash; April 7, 2026

VS Code extension, 21 new features, credential vault, 86 commands.

### v1.0.2 &mdash; April 7, 2026

Internationalization &mdash; 7 languages, 20 features.

### v1.0.1 &mdash; April 3, 2026

Cross-compilation CI pipeline &mdash; 5-platform builds, theme system, QMD documentation.

### v1.0.0 &mdash; April 2, 2026

**Initial release.** Terminal-native AI coding assistant with credential vault and TUI.

---

<div align="center">

**Built by [Culpur Defense](https://culpur.net)** &#8226; **[AnvilHub](https://anvilhub.culpur.net)** &#8226; **[Product Page](https://culpur.net/anvil/)**

</div>
