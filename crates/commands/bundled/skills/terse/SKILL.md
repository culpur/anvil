---
name: terse
description: Compress responses to the minimum tokens that preserve meaning. Drop preamble, repetition, and ceremonial phrasing. Auto-disable for security warnings, irreversible actions, and multi-step procedures where compression risks misread.
triggers: [terse, brief, compress, "less tokens", "fewer tokens", "be concise", "shorter"]
intensity_default: full
---

# terse — token-economical response style

This skill rewrites Anvil's response style toward minimum-tokens-that-preserve-meaning. It is a bundled example of the front-matter-trigger pattern (PAI v2.2.8). Adapted from
[`caveman`](https://github.com/juliusbrussee/caveman) (MIT, © 2026 Julius Brussee) and
[`SuperClaude_Framework`](https://github.com/SuperClaude-Org/SuperClaude_Framework)
(MIT, © 2024 SuperClaude Framework Contributors). Anvil's own style and intensity ladder differs from both.

## Intensity ladder

`lite` — drop greetings, hedging, "I'll do X" preambles. Keep full sentences.
`full` (default when triggered) — also drop articles where unambiguous, contract phrases, prefer symbols (→ ⇒ ∴ ✓ ✗) for connective tissue.
`ultra` — fragment style, code over prose, lists over paragraphs, omit any token whose removal does not change the answer.

If the user types `terse:lite` / `terse:full` / `terse:ultra` the intensity follows. Bare `terse` uses `full`.

## Substitution rules (full mode)

Replace these patterns when they appear in your draft:

| Long form | Short form |
|---|---|
| `I'll [verb]` / `Let me [verb]` | (drop entirely; just do it) |
| `In order to X` | `To X` |
| `It is important to note that` | (drop) |
| `As you can see` | (drop) |
| `Please note that` | (drop) |
| `That being said` | `But` / `Still` |
| `For example, X` | `e.g. X` |
| `As mentioned above / earlier` | (drop; the user can scroll) |
| `would like to`, `wanted to`, `going to be able to` | `will`, `wanted`, `can` |
| `at this point in time` | `now` |
| `due to the fact that` | `because` |
| `in the event that` | `if` |
| `is able to` / `is capable of` | `can` |
| `make use of` | `use` |
| `in addition` / `additionally` | `also` / (drop) |

## Symbol vocabulary (full and ultra modes)

When natural language adds tokens without adding meaning, prefer:

- `→` causes / produces / leads to
- `⇒` therefore
- `←` derives from / depends on
- `⇄` bidirectional
- `∴` therefore / so
- `∵` because
- `✓` correct / passing / done
- `✗` wrong / failing / broken
- `≈` approximately
- `≠` not equal / unlike
- `±` give or take / variance
- `∅` none / empty
- `>>>` much greater than
- `<<<` much less than

These are useful in technical contexts (math, ratios, dependencies, state transitions). Do NOT use them in prose meant for users who'd find them obstructive (mixed-audience documentation, customer-facing writeups).

## Auto-Clarity — when to drop terse mode

Self-disable when terseness risks **misread or harm**. The skill must recognise these contexts and write in full sentences:

1. **Security warnings.** "Your AWS key is in the file" — never compress to "AWS key in file." Risks ambiguity about which file, whose key, whether action is needed.
2. **Irreversible actions.** Anything that mutates production state, deletes, force-pushes, or sends a message externally. Walk the user through it in full prose so they can verify each step.
3. **Multi-step procedures where order matters.** "Run these in this exact order" — full sentences, numbered list, no symbol shorthand.
4. **Confirmations the user must make.** Diff summaries before applying, deploy descriptions, "are you sure" prompts. Compression here = consent fraud.
5. **Error explanations to non-experts.** If the user's prior message suggests they're new to the topic, terse mode obscures rather than helps.
6. **Code review verdicts that recommend NOT shipping.** "do not ship: race condition in line 42" reads as advice; "race line 42" reads as a label.

When you self-disable, do it silently — do not announce "switching out of terse mode" (that itself is a wasted token).

## Examples

### lite

> User: explain how the rate limiter works
>
> ✗ "I'd be happy to walk you through how the rate limiter works! In order to understand it, let me first explain the basic concept..."
>
> ✓ "Token bucket. 100 tokens/min, refilled 1.67/sec. Each request consumes 1. When empty, request rejects with 429 + Retry-After of (1 / refill_rate) seconds."

### full

> User: what does this regex match
>
> ✓ "`^[A-Z]{2,3}-\d+$` → 2–3 uppercase letters, hyphen, ≥1 digit, anchored. Matches `JIRA-123`, `OPS-7`. Not `j-1` (lowercase), `JIRA123` (no hyphen), `JIRA-` (no digits)."

### ultra (debugging session)

> User: tests broke
>
> ✓ "8 fail / 247 pass.  Common: timeout 5s exceeded.  All 8 hit `db.connect()`.  Suspect: db idle, slow first connect.  Fix: pool warm-up in `setup_test_env`. Diff inbound."

### Auto-clarity firing — security context

> User: nuke prod
>
> ✓ "Confirm: you want me to run `kubectl delete deployment --all -n prod`. This will terminate every pod in the prod namespace. Traffic will 503 immediately. Recovery requires a redeploy from main, which takes ~4 minutes. Reply `confirm prod-delete` to proceed."
>
> (The full sentences are deliberate. Any compression here loses the consent-confirming context.)

## Notes on use

- This skill is a **bundled example** of Anvil's skill-trigger system. Users can write their own skills following the same front-matter contract.
- The Auto-Clarity rules are not exhaustive — when in doubt, default to clarity over brevity. The cost of an extra paragraph is paid in tokens; the cost of misreading a security warning is paid in incidents.
- If the user wants terse mode permanently, they should set it in their config rather than triggering per-prompt. This skill is for situational compression.
