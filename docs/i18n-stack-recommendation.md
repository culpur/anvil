# i18n Stack Recommendation — task #645 (planning phase)

**Date:** 2026-05-20
**Scope:** Anvil TUI/CLI (Rust) + Passage `viewer.html` remote-control web UI
**Authors:** Maverick (auditor/planner agent)
**Status:** PROPOSAL — implementer agent will execute against this in v2.2.19

---

## 1. Audit summary (what's already there)

### Anvil (Rust)
- **Crate present:** `rust-i18n = "4"` declared in `workspace.dependencies` and consumed by `anvil-cli`.
- **Macro wiring:** `crates/anvil-cli/src/main.rs:115` declares `rust_i18n::i18n!("../../locales", fallback = "en");` at the binary root.
- **Locale switcher present:** `crates/anvil-cli/src/utils.rs:323` calls `rust_i18n::set_locale(lang)` from `run_language_command_static`.
- **Locales seeded:** 7 files in `locales/` — `en.yml de.yml es.yml fr.yml ja.yml ru.yml zh-CN.yml` (~98 lines / ~80 keys each).
- **Call sites today:** **ZERO `t!()` invocations across the codebase.** The wiring is a *stub*: keys exist, locale switching works, but no source code reads from the bundle. All on-screen text is still hardcoded English.

### Passage viewer.html
- **i18n today:** None. No `data-i18n`, no `i18next`, no `t(...)` helper, no locale switcher.
- **String separation:** Strings are inline in DOM-construction code and template literals. No `STRINGS` const.
- **Size:** 206 KB, 3 597 LOC, single self-contained HTML/CSS/JS file.

### Aegis reference (cross-check)
- **Library:** `i18next` + `react-i18next` + `expo-localization` + `AsyncStorage` for persistence.
- **File format:** One **directory per locale**, one JSON file **per namespace**: `common.json`, `auth.json`, `chat.json`, `calls.json`, `contacts.json`, `settings.json`. So 6 namespaces × 78 locales = 468 files.
- **Total LOC:** ~75 000 across `aegis-culpur.net/locales/` (excluding `node_modules/`).
- **Namespacing pattern:** **By feature, not by screen.** A button used in both chat and contacts lives in `common.json`. A reusable shape we'll mirror in Anvil.
- **Pluralization:** Per-locale, via i18next's `key_one` / `key_other` / `key_few` / `key_many` suffix convention. No hand-coded rules — i18next ships them.

Decision implication: **Aegis-style namespaced JSON-per-feature is the right shape**, but the storage technology differs per surface (YAML for Rust, JSON for JS) because that's what each ecosystem reads natively without a build step.

---

## 2. Anvil (Rust) — three options compared

| | `rust-i18n` v4 | `fluent-bundle` / `fluent` | `i18n-embed` |
|---|---|---|---|
| File format | YAML/JSON, one per locale | FTL (Fluent) per locale | FTL via `i18n-embed-fl` |
| Compile-time embed | Yes (`include_str!` at macro expansion) | Manual + `include_dir!` | Yes via `RustEmbed` |
| Runtime locale switch | `rust_i18n::set_locale("es")` — process-global | `FluentBundle` per locale, swap | Hot-reload optional, swap loader |
| Plural rules | Limited (suffix `_one`/`_other`) | First-class CLDR plural categories + selectors | Same as Fluent |
| Gender / variant selectors | None | First-class | First-class |
| Macro ergonomics | `t!("key", name = "x")` — terse | `fl!(LOADER, "key", name = "x")` — needs loader handle | Same as Fluent |
| Binary-size cost | Small (~30 KB per locale at this scale) | Larger (~200 KB Fluent runtime) | Larger (Fluent + RustEmbed) |
| Already in workspace | **YES** | No | No |
| Existing call-site cost to migrate | **Zero** — already wired | Re-wire `i18n!` → `static_loader!` macros | Same as Fluent |

### Recommendation for Anvil: **stay on `rust-i18n` v4**

Concrete code shape for the example string:

```rust
// crates/anvil-cli/src/wizard.rs
let title = t!("wizard.welcome.title");
// Renders: "Welcome to Anvil — your AI coding assistant"

// With interpolation:
let greeting = t!("wizard.welcome.greeting", name = user_name);

// With plural (rust-i18n v4 suffix convention):
let count_msg = t!("status.messages_count", count = n);
// en.yml:
//   status.messages_count_one: "%{count} message"
//   status.messages_count_other: "%{count} messages"
```

**Why keep it:** (1) Already in `Cargo.toml`, already declared in `main.rs`, already loading the 7 stub locales. Switching to Fluent now means re-doing this plumbing for marginal gain. (2) Anvil strings are mostly imperative UI labels and short error sentences — Fluent's selector grammar is overkill. (3) Binary size matters: Anvil ships 7 platform binaries; each Fluent runtime addition is multiplied. (4) The plural-suffix convention covers our needed cases (en/es/fr/de/pt-BR have 2 forms; ru/pl have 3-4; zh-CN/ja have 1). For ru/pl rust-i18n supports `_few` / `_many` suffixes that map to CLDR categories.

**Single concession:** `rust-i18n` v4's macro requires the locales path at compile time. The existing `i18n!("../../locales", fallback = "en")` is correct; we keep it.

---

## 3. Passage viewer.html — three options compared

| | `i18next` + `i18next-browser-languagedetector` | Tiny custom `t()` helper | `polyglot.js` |
|---|---|---|---|
| Size | ~30 KB min+gz combined | ~20 LOC, ~1 KB | ~7 KB min+gz |
| Build step required | No (ESM via CDN) but pollutes single-file ethos | No | No |
| Namespacing | Yes, native | Yes, `STRINGS.section.key` dot path | Yes |
| Plural rules | All CLDR plural categories shipped | Hand-roll per-locale (yuck for ru/pl) | English-style only by default |
| Interpolation | `t('greet', { name })` → `Hello {{name}}` | `t('greet', { name })` → `Hello {name}` | `t('greet', { name })` |
| Auto-detect browser language | Yes (`languagedetector` plugin) | Read `navigator.language` ourselves (3 LOC) | Manual |
| LocalStorage persistence | Plugin handles it | Hand-roll (4 LOC) | Manual |
| Hits single-file viewer.html ethos | No (CDN imports) | **Yes** | Mostly |

### Recommendation for viewer.html: **tiny custom `t()` helper + sidecar `viewer.locales.js`**

Concrete code shape:

```html
<!-- end of viewer.html, before main <script> block -->
<script src="/viewer.locales.js"></script>
<script>
  // ~25-LOC i18n helper, lives at top of main script block
  const VIEWER_LOCALE = (() => {
    const stored = localStorage.getItem('anvil_viewer_locale');
    if (stored && LOCALES[stored]) return stored;
    const browser = (navigator.language || 'en').toLowerCase();
    // exact match, then language-only fallback
    if (LOCALES[browser]) return browser;
    const short = browser.split('-')[0];
    if (LOCALES[short]) return short;
    return 'en';
  })();

  function t(key, vars) {
    const bundle = LOCALES[VIEWER_LOCALE] || LOCALES.en;
    let s = bundle[key];
    if (s == null) s = LOCALES.en[key] || key;  // graceful fallback
    if (vars) {
      for (const k in vars) s = s.replace(new RegExp(`{${k}}`, 'g'), vars[k]);
    }
    return s;
  }
  function setViewerLocale(code) {
    if (!LOCALES[code]) return false;
    localStorage.setItem('anvil_viewer_locale', code);
    location.reload();  // simplest: full re-render
    return true;
  }
</script>
```

`viewer.locales.js` ships as a sibling file in `public/` and defines:

```js
const LOCALES = {
  en: { 'header.status.connected': 'Connected', 'btn.send': 'Send', ... },
  es: { ... },
  // ... 7 Tier-1 locales
};
```

**Why this and not i18next:** (1) viewer.html is **intentionally monolithic** (per task #680 — single-file deploy). Adding i18next means a CDN dep or a 30 KB inline blob; the tiny helper is 25 LOC. (2) We don't need CLDR plural categories on the web side — the viewer has very few plural-sensitive strings; we'll handle the two or three with explicit `t('msgs.one')` / `t('msgs.other')` keys keyed off `n === 1`. (3) Sidecar file means translators edit JSON-ish JS without touching viewer.html — clean diff, easy to hand to the translator agent.

**Why not polyglot.js:** It's smaller than i18next but still adds a dep and its plural rules are weakest of the three.

---

## 4. Combined recommendation table

| Surface | Library | File location | Format | Locale-switch trigger |
|---|---|---|---|---|
| Anvil (Rust) | `rust-i18n` v4 (already in tree) | `locales/<code>.yml` | YAML, flat dotted keys | `/language <code>` slash command, persisted to `~/.anvil/settings.json` |
| viewer.html (JS) | Tiny custom `t()` + sidecar | `public/viewer.locales.js` | JS const, namespaced object | Dropdown in viewer status bar, persisted to `localStorage["anvil_viewer_locale"]` |

Both surfaces use the **same key naming convention** — dotted paths grouped by feature area (e.g. `wizard.welcome.title`, `tui.status.connected`, `viewer.btn.send`) — so a translator working both sides has a consistent mental model.

---

## 5. Open questions for the implementer agent

1. Should `~/.anvil/settings.json`'s `locale` field auto-propagate to the viewer when a remote-control session boots (so a `/language es` in Anvil makes the webui Spanish too)? **Stretch goal for v2.2.19; baseline is independent settings.**
2. Should the Anvil binary embed all 7 locale YAMLs at compile time (current `rust_i18n::i18n!` macro does this) or load from `~/.anvil/locales/` at runtime for user overrides? **Stay embedded for v2.2.19; user-override is a v2.3 idea.**
3. Do we want a `--lang` CLI flag override that beats `~/.anvil/settings.json`? **Yes, low cost; add to migration plan.**

---

**Sign-off:** Use `rust-i18n` v4 for Anvil, custom 25-LOC helper for viewer.html. Both surfaces ship 7 Tier-1 locales in v2.2.19 (en base + 6 translated, with one stretch). See `i18n-language-tiers.md` and `i18n-v2.2.19-migration-plan.md` for execution detail.
