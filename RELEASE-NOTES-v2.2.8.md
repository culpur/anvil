# Anvil v2.2.8 — PAI-inspired composition, learning, and robustness

Released: 2026-04-22

v2.2.8 brings the best-of-breed patterns from three reference projects — Daniel
Miessler's **Personal_AI_Infrastructure**, the **SuperClaude Framework**, and
**caveman** — into Anvil as idiomatic Rust rewrites. All adaptations are
MIT-compatible and attributed in their respective source files. The release
also closes four Claude-Code-parity bugs Anthropic fixed between v2.1.94 and
v2.1.117, fixes two infrastructure issues that could brick an installed
binary, and continues to honor the core rule: **Anvil defaults to precise
output — condensed is always opt-in**.

---

## Major new capabilities

### Trait-based agent composition — `/agent compose`
A 30-trait catalogue (expertise × personality × approach) across one bundled
YAML file yields thousands of agent variants without a file-per-variant
explosion. One command:

```
/agent compose security,skeptical,first-principles "audit crates/runtime/src/oauth.rs"
```

Composes a system prompt in a locked order (intro → expertise → personality
→ approach → task), spawns a subagent turn with it, and renders the response.
Dimension conflicts (two expertises, two personalities) hard-error by default
so the caller is forced to be intentional. Browse the catalogue with
`/agent traits`. Adapted from Miessler's `Packs/Agents/src/Data/Traits.yaml`
+ `ComposeAgent.ts`.

### Skill front-matter triggers with suggest-not-auto — `/skill suggest`
Skills can now declare YAML front-matter `triggers: [keyword, "phrase"]`.
Anvil watches your prompts, runs case-insensitive whole-word matching, and
**suggests** relevant skills instead of silently injecting them. Auto-inject
on user-prompt-keyword-match would be a prompt-injection vector — the user
confirms via `/skill load <name>`.

Three bundled skills ship as reference implementations:

- **`security-audit`** — OWASP-flavored code review (triggers: audit,
  security, vulnerability, owasp, pentest)
- **`code-review`** — pull-request code review (triggers: review, code review,
  pr review)
- **`terse`** — token-economy mode with Auto-Clarity rules that self-disable
  for security warnings, irreversible actions, multi-step procedures, consent
  confirmations, and non-expert error explanations (triggers: terse, brief,
  compress)

At turn end, Anvil emits a single-line hint when your prompt matches any
trigger. Tab completion resolves live against installed skills. Pattern
adapted from Miessler `SKILL.md` "USE WHEN" descriptions + SuperClaude
`commands/*.md` front-matter.

### Prompt-type hooks — inject context, don't just run commands
Plugin lifecycle hooks could previously only run shell commands. v2.2.8 adds
a prompt-type hook that injects an interpolated string into the next model
turn. Example `plugin.json` entry:

```json
{"hooks": {"PostToolUse": [
  {"type": "prompt", "body": "You just ran {tool_name}. If the result involves a code change, verify it still compiles and any tests still pass."}
]}}
```

Variables: `{tool_name}`, `{tool_input}`, `{cwd}`, `{date}`, `{model}`.
Backward-compatible — bare-string hooks still run as shell commands. Pattern
adapted from SuperClaude `hooks/hooks.json`.

### Three-arm skill evaluation harness — `anvil skill-eval`
Honest way to measure whether a skill helps:

```
anvil skill-eval --skill terse --prompts prompts.txt --model qwen3:8b
```

Runs three arms per prompt: `__baseline__` (no system prompt),
`__terse__` ("Answer concisely."), and `<skill>` (`"Answer concisely.\n\n" +
SKILL.md`). Writes JSON snapshots committed to git for diff-against.

Every report ships three **honest caveats** baked in:

- "Token counts are directional (±25%), not your provider's actual
  tokenizer."
- "Measures token count only. Does NOT measure fidelity, latency, cost, or
  quality."
- "A near-zero `skill_vs_terse_delta_pct` means the skill is doing nothing
  useful."

Pattern adapted from caveman `evals/`.

### Output style — `precise` (default) vs `condensed`
User-selectable global response style:

```
/output-style              # print current
/output-style precise      # default — full sentences, clarity over tokens
/output-style condensed    # activates the bundled terse skill
```

Condensed mode prepends `terse` skill content to the system prompt for every
turn; precise leaves the model's natural voice alone. **Anvil never
auto-applies condensed** — it is always an explicit choice. Auto-Clarity
rules inside `terse` still fire even in condensed mode to keep security
warnings, irreversible actions, and consent confirmations readable.

---

## Robustness fixes

### Plugin loader is now forward-compatible
Previously, a single plugin manifest with an unknown hook variant (e.g. a
v2.2.8 tagged-hook in a v2.2.7 binary) could crash the entire `anvil` binary
at startup. v2.2.8 isolates per-plugin failures — bad manifests are skipped
with a `[plugin warning]` line on stderr, and discovery continues for the
rest. The new `PluginLoadDiagnostic` enum carries structured per-plugin
errors that surface to the user without taking the binary down.

### Bundled plugins are now embedded in the binary
Bundled plugins previously used `env!("CARGO_MANIFEST_DIR")` to locate their
content at runtime — which returns the developer's absolute build path,
baked in at compile time. Homebrew users' bundled plugins were invisible;
developers' installed binaries were wired to their live source tree and
would break whenever the tree changed. v2.2.8 embeds the bundled plugin tree
via `include_dir` and materializes it into `~/.anvil/plugins/bundled/` on
first run with SHA-based fingerprint for idempotent updates.

---

## Claude-Code-parity bug fixes

v2.2.8 mirrors four Anthropic fixes from v2.1.94 → v2.1.117:

- **429 Retry-After is now a minimum, not authoritative.** Previously a small
  `Retry-After` could burn the entire retry budget in ~13s. Now the wait is
  `max(server_hint, exponential_backoff)`.
- **5-minute stream dead-air timeout.** Streams that stall after a TCP
  connection stays up but data stops flowing now abort with a distinctive
  error instead of hanging indefinitely. Override via
  `ANVIL_STREAM_DEAD_AIR_MS`.
- **DangerFullAccess stability invariants.** Regression tests guard against
  approval flows silently downgrading permission modes (a class of bug
  Anthropic fixed upstream). Subagents inherit the parent's mode.
- **Request timeout is configurable.** Previously a hardcoded timeout could
  abort slow Ollama/local-LLM calls. Override via `ANVIL_API_TIMEOUT_MS`
  (default 600,000 ms / 10 min).

---

## Quality of life

- `/model` mid-conversation switch now warns about uncached re-read —
  mirrors Claude Code v2.1.108.
- `Ctrl+U` clears the input buffer and stashes it; `Ctrl+Y` restores —
  readline-style kill-yank. Doesn't interfere with vim Normal mode's
  native `Ctrl+U`.
- Ollama sessions now send `options.num_ctx` to the server so 128K-capable
  models actually get 128K of context (previously silently capped at
  Modelfile default — usually 2048). Override via `ANVIL_OLLAMA_NUM_CTX`
  or the shared `ANVIL_CONTEXT_SIZE`.
- Empty-after-tool-result streams no longer fail the turn. Ollama and some
  OpenAI-compatible backends answer a successful tool call with just a
  stop token; that is legitimate "done," not an error.
- Sandbox permission modes (`read-only` / `workspace-write` /
  `danger-full-access`) switch on the fly via `/permissions <mode>`. No
  runtime rebuild required.

---

## Tests

- **756 tests passing**, 0 failed.
- +198 tests since v2.2.7 (covering every new feature + all four CC-parity
  bug fixes).

## Upgrade path

```bash
anvil upgrade                  # preferred — SHA256 verified
brew upgrade culpur/anvil/anvil
```

## Rollback path

v2.2.8 preserves v2.2.7 artifacts. If anything misbehaves, see
`ROLLBACK-v2.2.8.md` for exact commands to return to v2.2.7.

## Attribution

PAI-inspired adaptations respect the source licenses:

- Miessler's `Personal_AI_Infrastructure`, MIT © 2025 Daniel Miessler
- `SuperClaude_Framework`, MIT © 2024 SuperClaude Framework Contributors
- `caveman`, MIT © 2026 Julius Brussee

See `PAI-SURVEY-v2.2.8.md` for the full adoption report and the decisions we
made on what to adopt, what to defer, and what to decline.
