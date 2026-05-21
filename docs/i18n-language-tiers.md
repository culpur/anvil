# i18n Language Tiers — task #645 (planning phase)

**Date:** 2026-05-20
**Scope:** Anvil + viewer.html
**Per user (2026-05-20):** NO Arabic, NO Hindi this cycle. All Tier-1 languages are LTR.

---

## Sizing assumption

Audit (see `i18n-stack-recommendation.md` §1):

- **Anvil unique English strings, post-extraction estimate:** ~600-800 keys, ~3 500-4 500 English words. (Drivers: `wizard.rs` 4 141 LOC + 337 density, `main.rs` 10 223 LOC + 305 density, `cmd_static.rs` + `cmd_ai.rs` + `configure.rs` + TUI rendering.) Currently `locales/en.yml` has only 80 keys — the gap is what the implementer agent must close in v2.2.19 Phase 2.
- **viewer.html unique English strings, post-extraction estimate:** ~120-180 keys, ~600-900 English words. (Driver: 62 visible text nodes + 40 JS-set labels + 3 prompts/alerts + ~50 tooltips, error toasts, status labels.)
- **Combined English source word count:** ~4 100-5 400.

Per-language word count = source words × ratio in the table below.

---

## Tier 1 — must ship in v2.2.19 (8 languages including en)

| Code | Native name | Direction | Plural rules (CLDR) | Word-ratio vs en | Est. words (Tier-1 total) | Notes |
|---|---|---|---|---|---|---|
| `en` | English | LTR | 2 (one, other) | 1.00 | ~4 100-5 400 (base) | Source of truth. `locales/en.yml` + `viewer.locales.js` `en` block. |
| `es` | Español | LTR | 2 (one, other) | 1.15-1.25 | ~5 000 | Universal Spanish; avoid Latin America vs. Castilian split. Audience: largest non-en CLI base. |
| `zh-CN` | 简体中文 | LTR | 1 (other) | 0.55-0.65 | ~2 800 | Simplified Chinese. NO plural suffix variants needed. |
| `fr` | Français | LTR | 2 (one, other) | 1.15-1.25 | ~5 000 | Note French plural rule: 0 takes singular ("0 message" not "0 messages"). |
| `pt-BR` | Português (Brasil) | LTR | 2 (one, other) | 1.10-1.20 | ~4 800 | Brazil-flavored Portuguese, not pt-PT. |
| `ru` | Русский | LTR | 4 (one, few, many, other) | 1.05-1.15 | ~4 600 | Implementer MUST wire `_few` / `_many` suffixes; plural rules are the most complex in Tier 1. |
| `ja` | 日本語 | LTR | 1 (other) | 0.50-0.60 | ~2 500 | No plural variants. Watch for keyboard-input edge cases in TUI input bar. |
| `de` | Deutsch | LTR | 2 (one, other) | 1.20-1.30 | ~5 200 | Famously long compound words; verify TUI label widths don't overflow rail/footer. |

**Tier 1 total translated work:** ~30 000 words across 7 non-en languages.

---

## Tier 2 — stretch goals for v2.2.19, otherwise v2.2.20 (10 more)

| Code | Native name | Direction | Plural rules | Word-ratio |
|---|---|---|---|---|
| `ko` | 한국어 | LTR | 1 (other) | 0.55-0.65 |
| `it` | Italiano | LTR | 2 | 1.10-1.20 |
| `tr` | Türkçe | LTR | 2 | 1.05-1.15 |
| `vi` | Tiếng Việt | LTR | 1 (other) | 1.00-1.10 |
| `pl` | Polski | LTR | 4 (one, few, many, other) | 1.10-1.20 |
| `id` | Bahasa Indonesia | LTR | 1 (other) | 1.00-1.10 |
| `nl` | Nederlands | LTR | 2 | 1.10-1.20 |
| `sv` | Svenska | LTR | 2 | 1.05-1.15 |
| `nb` | Norsk bokmål | LTR | 2 | 1.05-1.15 |
| `uk` | Українська | LTR | 4 (one, few, many, other) | 1.05-1.15 |

**Tier 2 total translated work:** ~45 000 words across 10 languages. (`pl` and `uk` are the plural-rule complexity outliers; everything else is simple.)

---

## Tier 3 — v2.2.20 and beyond (30-50 long tail, deferred)

Candidates drawn from Aegis's 78-locale set, minus excluded Arabic / Hindi / Hebrew / Persian / Urdu / Pashto (per RTL exclusion this cycle):

European: `bg cs da el et fi ga hr hu is lb lt lv mk mt ro sk sl sq sr ca be bs ka hy`
Asian: `zh-TW th ms tl km my lo bn ta te si ne kk uz mn az ky tg tk`
African: `sw am ha yo ig zu af so`
Americas: `ht`

**Approximate Tier-3 scope:** 35-45 languages × ~4 800 average words = ~180 000-220 000 translated words. Will need batching across multiple LLM-translator runs.

---

## Pluralization complexity — flag for the dev wiring rules

When the implementer agent wires `rust-i18n` plural keys (and the analog in `viewer.locales.js`), these locales need attention:

| Locale | Categories | Suffix pattern |
|---|---|---|
| `en es fr de pt-BR it tr nl sv nb` | 2 | `_one`, `_other` |
| `ru uk pl` | 4 | `_one`, `_few`, `_many`, `_other` |
| `zh-CN ja ko vi id` | 1 | `_other` only (or use base key) |

For the viewer.html custom helper, plural selection is done in app code (`n === 1` branch) — keep plural-sensitive strings to the bare minimum (likely just `msg.count`, `tab.count`, `error.retry_in` — maybe 5-8 keys total).

For `rust-i18n` v4, the macro `t!("key", count = n)` auto-selects suffix based on CLDR category for the active locale, so the dev work is **just authoring the suffixed YAML entries**, not writing selection logic.

---

## Native names — picker MUST show native, not English

When the language selector renders (`/language` slash command in Anvil; dropdown in viewer.html status bar), show the native name. Aegis convention. Example dropdown rendering:

```
English          Español          中文          Français
Português        Русский          日本語        Deutsch
```

Both surfaces share the same native-name table — implementer agent should put it in `crates/anvil-cli/src/i18n_meta.rs` and `public/viewer.locales.js` (duplicated, ~30 LOC each, acceptable).

---

## Summary

- **Ship in v2.2.19:** 8 languages (`en es zh-CN fr pt-BR ru ja de`), no RTL.
- **Stretch for v2.2.19:** 10 more (`ko it tr vi pl id nl sv nb uk`).
- **Defer to v2.2.20+:** 30-50 long-tail.
- **Estimated translator-agent work for Tier 1:** ~30 000 non-en words across 7 languages, broken into 14 batches (one per locale per surface). Each batch is a ~150-key JSON/YAML diff suitable for a single Sonnet 4.6 call.
- **Plural-rule complexity flags:** `ru pl uk` need 4-form suffixes; `zh-CN ja ko` need single-form; everything else is standard 2-form.
