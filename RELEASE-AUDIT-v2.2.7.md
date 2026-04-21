# Release Audit — Anvil v2.2.7

**Date:** 2026-04-20  
**Auditor:** QA Expert (independent, read actual source)  
**HEAD:** 97758e9 (docs: add v2.2.7 release notes)

---

## Overall Verdict: NO-GO

One blocker found in source. All other buckets pass.

---

## Per-Bucket Findings

### Bucket 1 — Release pipeline version embed audit: PASS

- `extractBinaryVersion` at helpers.js:210, `auditBinaryVersions` at helpers.js:250 — confirmed present.
- github-release.js:121 calls `auditBinaryVersions` at label "D6: Binary version audit (hard gate — must run before upload)" — confirmed before upload loop.
- T16a–T16f all pass: `node --test test/suite.test.js` → 34 pass, 0 fail.
- T16f correctly simulates the v2.2.6 scenario (4 of 5 stale), asserts `ok: false` and `mismatches.length === 4`.

### Bucket 2+4 — Cross-platform env: PASS (with note)

- `env::var("HOME")` appears at tools/src/lib.rs:2324 and runtime/src/prompt.rs:731 — both are inside `#[cfg(test)]` / `#[test]` functions. Not a production code path.
- `.split("\\:")` — zero hits across crates.
- runtime/src/qmd.rs uses `which::which()` exclusively (lines 256, 531, 534, 573).
- `dirs-next = "2.0.0"`: anvil-cli, api, runtime, tools crates all declare it.
- `which = "6"`: runtime/Cargo.toml:33 confirmed.

### Bucket 3 — TUI scrollback: PASS

- `crates/anvil-cli/src/tui/scrollback.rs` exists.
- `Tab` struct has `scrollback: ScrollbackBuffer` and `scrollback_state: ScrollbackState` (state.rs:242-244).
- `PageUp` (scroll_up 10), `PageDown` (scroll_down 10), `End` (line 114) handled in input_handler.rs.
- Shift+Drag comment at input_handler.rs:41-47 confirms pass-through, not consumed.
- Ring buffer cap: `DEFAULT_CAPACITY: usize = 10_000` at scrollback.rs:21.

### Bucket 5 — Ollama tool-use parser: PASS

- `crates/api/src/providers/ollama_tool_parser.rs` exists.
- All 4 formats documented and implemented: Format 1 (OpenAI tool_calls, primary path in openai_compat.rs), Format 2 (XML tool_use tags, line 122), Format 3 (JSON code fences, line 145), Format 4 (natural language heuristic, line 105).
- Fail-loud warning at ollama_tool_parser.rs:358: `"WARNING: The model described writing a file but no tool call was executed."` — exact text confirmed.
- openai_compat.rs:822 calls `parse_ollama_text_for_tool_calls` inside `normalize_response`.
- api crate test result: 67 passed, 0 failed.

### Bucket 6 — Remote-control 503: PASS

- `/Users/soulofall/projects/passage-culpur.net/ecosystem.config.cjs` declares `passage` (line 12) and `anvil-relay-ws` (line 58) — two apps confirmed.
- Live endpoint check: HTTP 200 returned (not 503).

### Bucket 7 — Cross-compile verification: PASS

All 5 artifacts in `target/release-artifacts/` embed "Anvil v2.2.7":

| Target | SHA256 |
|--------|--------|
| anvil-aarch64-apple-darwin | 383351c1... |
| anvil-aarch64-unknown-linux-gnu | dba77f8a... |
| anvil-x86_64-apple-darwin | 5fcb2e7d... |
| anvil-x86_64-pc-windows-gnu.exe | 19c8f132... |
| anvil-x86_64-unknown-linux-gnu | 4759802... |

### Bucket 8 — Cross-OS installers: PASS

- `install/install.sh` exists, `set -euo pipefail` at line 13, SHA256 verification at lines 161-188 (shasum + sha256sum fallback).
- `install/install.ps1` exists, `Get-FileHash -Algorithm SHA256` at line 132.
- Completions: `install/completions/anvil.{bash,zsh,fish,ps1}` — all four present.
- `check.rs`, `setup.rs`, `upgrade.rs`, `uninstall.rs` all exist.
- `main.rs` wires all 4: `run_check`, `run_setup_wizard`, `run_upgrade`, `run_uninstall` (lines 89-93, 247-253).
- Ollama model menu in setup.rs: Llama 3.1, Qwen 3, Mistral Nemo, Gemma 3, Phi 4 plus coding variants present. Comment at line 305 and println at line 386 both explicitly confirm DeepSeek and Kimi are excluded. CONFIRMED absent.

### Version consistency: FAIL (blocker)

- `[workspace.package] version = "2.2.7"` — correct.
- **BLOCKER:** `crates/runtime/src/hub.rs:411` contains hardcoded `"client": "anvil/2.2.6"` in `post_install_telemetry()`. This is production code (not test code). Every AnvilHub plugin install triggered via v2.2.7 will report client version 2.2.6 in telemetry.
- `crates/anvil-cli/src/upgrade.rs:376` contains `"2.2.6"` as a test assertion value (`assert!(!is_newer("2.2.6", "2.2.7"))`) — this is legitimate test data, not a stale version string. Not a blocker.

---

## Build & Test Results

```
cargo build --release   →  Finished in 12.81s, ZERO warnings, ZERO errors
cargo test --workspace  →  618 passed, 0 failed, 2 ignored
```

Test breakdown by crate: anvil-cli 286, runtime 154, api 67, tools 29, upgrade 28, check 28, others 26.

---

## Blockers

**1 blocker — must fix before release:**

File: `crates/runtime/src/hub.rs`, line 411

```rust
"client": "anvil/2.2.6",   // <-- stale, must be 2.2.7 or dynamic
```

Fix: replace with `format!("anvil/{}", env!("CARGO_PKG_VERSION"))` so it tracks the workspace version automatically and this cannot regress again.

Reproduce:
```
grep -n '"client": "anvil/' crates/runtime/src/hub.rs
```

---

## Nice-to-Haves Missed (non-blocking)

- `install/install.sh:195` warns "No checksum file found — skipping verification" rather than hard-failing. Acceptable for now but a future hardening target.
- No SHA256 `.sha256` sidecar files present alongside `target/release-artifacts/` binaries — they appear to be generated at upload time by the release pipeline, which is fine provided the pipeline generates them before the installer fetches them.

---

## Signoff

**NO-GO.** One stale hardcoded version string (`"anvil/2.2.6"`) in production telemetry code at `hub.rs:411` will misreport the client version on every plugin install; fix to `env!("CARGO_PKG_VERSION")` and re-run the build gate before shipping.
