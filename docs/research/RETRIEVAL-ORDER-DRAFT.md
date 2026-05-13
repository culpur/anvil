# Candidate `<retrieval-order>` Prompt Block — Draft

**Status:** Working draft. Pre-empts the seven-layer doc's retrieval-order section so we can compare and converge.
**Date:** 2026-05-12
**Target:** added to system prompt by `crates/runtime/src/prompt.rs::environment_section()` (or as a separate block immediately after it)
**Validates:** the "what version of anvil am I" → web search failure

---

## Why this is needed

The audit + verification work established:

1. The REPL at `main.rs:3921` correctly threads model + provider into the system prompt.
2. The environment block at `prompt.rs:280-327` correctly emits *"You are Anvil v2.2.11, Currently loaded model: claude-opus-4-7..."*
3. The model **has** the answer in its prompt and **ignores it anyway**, reaching for web search.

The model is not wired-wrong. It's policy-uninstructed. Without an explicit retrieval-order directive, the model defaults to "search the world" for any question it isn't 100% confident about — even when the answer is sitting in its own context window. This block fixes that with one prompt-content change.

---

## Design constraints

- **Layer-name agnostic.** The seven-layer doc may settle on slightly different layer names than what I'm using here. The block's structure must be easy to retitle without restructuring.
- **Imperative, not advisory.** "Consult X first" — not "you may want to consult X."
- **Specific question-type cues.** The model needs to recognize *which questions* hit this policy, not apply it to every question.
- **Web search demoted, not banned.** Web is still the fallback for genuinely external information. The block makes it the *last* resort, not a forbidden one.
- **Token cost low.** Block should be ≤300 tokens. The environment block today is ~150 tokens; doubling that is tolerable.

---

## Draft v1 — minimal block

```text
# Retrieval order

Before reaching for external tools, consult Anvil's local context in this order:

1. **This prompt** — your identity, version, model, working directory, date, and active goal are stated above in the environment block. Trust this content as authoritative.
2. **Loaded files** — when a `<known-files>` block is present, it lists every file Anvil has fingerprinted with summaries. Check there before calling `read_file`.
3. **Project memory** — when `# Persistent memory` or `# Anvil instructions` blocks appear above, they contain user-stated facts, project conventions, and ANVIL.md content. Consult before assuming.
4. **Project tree** — for code, configuration, or release information about *this* project, use `read_file`, `glob_search`, or `grep_search` on the working directory.
5. **QMD knowledge base** — for documented project history, infrastructure, or prior decisions, use the `mcp__qmd__query` tool (semantic search over the user's local markdown corpus).
6. **Web search** — only when the question is genuinely about external information (third-party libraries, recent events, public documentation) that local sources can't answer.

For questions about Anvil itself ("what version are you?", "what model is loaded?", "what can you do?") — the answer is in the environment block above. Do not web search.

For questions about *this project* ("what does X do?", "where is Y defined?", "what was the last release?") — use `read_file`, `glob_search`, `grep_search`, or `mcp__qmd__query` on the local tree before web search.
```

**Token estimate:** ~240 tokens (gpt-4 tokenizer rough count).

---

## Draft v2 — layer-aware (depends on seven-layer doc landing first)

When the seven-layer doc commits to layer names, this version replaces v1. Placeholder names below using Jared's stated layer titles:

```text
# Retrieval order

Anvil organizes context as seven memory layers. Consult them in priority order before reaching for external tools.

1. **Sensory / Working** — Your current turn input and conversation history. The environment block above states your identity, version, model, working directory, date, and active goal. Trust this content as authoritative.
2. **Episodic** — Timestamped past interactions, when present in `# Recent activity` or `<recent-routines>` blocks. Use for "what happened" questions.
3. **Semantic** — User-stated facts, project conventions, vault inventory, goals, nominations. Appears as `# Persistent memory`, `# Anvil instructions`, `<vault-inventory>`, `<active-goals>` blocks above.
4. **Procedural** — Loaded skills and learned workflows. Appears as `# Skills` blocks or via the `Skill` tool.
5. **Long-term storage (QMD)** — Documented project history, infrastructure, prior decisions. Use `mcp__qmd__query` for semantic search.
6. **Project tree** — Files, config, code in the working directory. Use `read_file`, `glob_search`, `grep_search`.
7. **External (web)** — Only for genuinely external information not answerable from layers 1–6.

For questions about Anvil itself ("what version are you?", "what model is loaded?", "what can you do?") — the answer is in the environment block (Layer 1). Do not web search.

For questions about *this project* ("what does X do?", "where is Y defined?", "what was the last release?") — Layers 1–6 before Layer 7.
```

**Token estimate:** ~290 tokens.

---

## Implementation sketch

Single function added to `crates/runtime/src/prompt.rs`:

```rust
fn retrieval_order_section() -> String {
    // ~30 lines of the v1 or v2 block above, as a raw string
}
```

Called from `SystemPromptBuilder::build()` immediately after `environment_section()`:

```rust
sections.push(self.environment_section());
sections.push(retrieval_order_section());  // NEW
```

That's it. No new struct, no new field, no new dependency. The block is static (doesn't depend on session state) so it can be a `const &str` if we want zero-cost.

Estimated effort: **20 minutes** to add the function + call site + a single integration test that asserts the block appears in the prompt output.

---

## Acceptance test

Run Anvil v2.2.x with this patch. Ask: **"what version of anvil are you?"**

**Expected:** model answers `"Anvil v2.2.X running on claude-opus-4-7 served by Anthropic"` directly from the environment block. **No** `WebSearch` tool call. **No** `read_file` on Cargo.toml.

**If still web-searches:** the model is overriding the instruction. That'd indicate we need stronger language ("**Do not** invoke `WebSearch` for self-identity questions") or that the block needs to land *before* the system role, not after.

---

## Open questions for review (when seven-layer doc lands)

1. **Block position.** v1 puts retrieval-order *after* the environment section. v2 might want it *before* the boundary marker (`SYSTEM_PROMPT_DYNAMIC_BOUNDARY`) so it's part of the cacheable static prefix. Which is right?

2. **Layer names.** v1 uses Anvil-existing block names (`<known-files>`, `# Persistent memory`). v2 uses the cognitive layer names (Sensory, Working, etc.). The seven-layer doc decides which vocabulary the model should think in.

3. **Tool-name explicitness.** v1 mentions `mcp__qmd__query`, `read_file`, `glob_search`, `grep_search` by name. Pro: removes ambiguity. Con: brittle to tool renaming and adds noise.

4. **Conditional layers.** Some blocks only appear if data exists (`<vault-inventory>` only if vault is unlocked and non-empty). Should retrieval-order list them unconditionally (so the model knows to look for them) or only when they're present? v2 lists unconditionally.

5. **Negative cases.** Should the block enumerate cases where web search IS the right call ("for third-party libraries, public documentation, recent events")? v1 implies it; explicit might be safer.

---

## Why this is the smallest cohesion fix

Original audit said "smallest fix is wiring the prompt-builder identity threading." Verification proved that was already wired. The actual smallest fix is **content, not code**: tell the model how to think about the layers it already sees. This block does that in one function and one call site.

When the seven-layer doc lands, we replace v1 with v2 and the block becomes the canonical surface where the layer model meets the agent.
