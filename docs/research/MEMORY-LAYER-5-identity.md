# MEMORY LAYER 5 — Identity / Secrets

> One of seven parallel layer audits. Synthesized at `docs/research/SEVEN-LAYER-MEMORY.md`.
> Anvil version under audit: **v2.2.14** (HEAD).

---

## 1. Layer definition

**L5 — Identity / Secrets** is the encrypted, gated-by-vault-unlock state of Anvil.
It holds two adjacent kinds of data: (a) **credentials** — API keys, passwords,
SSH keys, TOTP seeds, OAuth tokens, database URLs — and (b) **infrastructure
facts** — hostnames, IPs, deploy paths, port numbers — that are too sensitive
for plaintext `ANVIL.md` but are not credentials per se.

**Gating model.** All L5 reads require an unlocked vault:

1. On first use, the user runs `/vault setup`, choosing a master password.
2. The master password is fed through **Argon2id**
   (`m_cost=65536` KiB, `t_cost=3`, `p_cost=4`, `Some(32)` output)
   to derive a 32-byte **KEK** (`crates/runtime/src/vault/mod.rs:257-259`).
3. The KEK is held *only* in process memory, in a process-global `OnceLock`
   (`crates/runtime/src/vault_session.rs:15-20`), and is zeroed on `Drop`
   (`crates/runtime/src/vault_session.rs:8-11`, doc claim).
4. The KEK is the **same** secret used for both credential encryption (envelope
   over a per-credential random DEK; `vault/mod.rs:9-11`) and for the encrypted
   per-project private memory store (`private_memory.rs:8-11` —
   *"The KEK used is the same vault master key already in memory — no
   separate password required."*).
5. L5 contents are **never auto-injected** into the system prompt. The only
   "injection-shaped" function on the type (`PrivateProjectMemory::format_for_context`,
   `private_memory.rs:148`) exists but has **zero production callers** —
   `grep` finds it only in tests. The user-facing guarantee is asserted in
   `crates/commands/src/handlers.rs:910`.

In layer terms: L5 is the only tier whose read path requires an explicit
session-level secret. All other layers (L1-L4, L6-L7) read freely from
plaintext or app-keyed state. Crossing into L5 is a permission-and-decryption
boundary, not a categorisation boundary.

---

## 2. Current Anvil state

### Code inventory

| Concern | File | Lines |
|---|---|---|
| Vault module root, `VaultManager`, KDF setup | `crates/runtime/src/vault/mod.rs` | 1-14, 220-270 |
| Argon2id KDF + AES-GCM primitives | `crates/runtime/src/vault/crypto.rs` | 1-30 |
| On-disk layout (`vault.meta`, `cred_<label>.enc`, `totp_<label>.enc`) | `crates/runtime/src/vault/storage.rs` | 14-105 |
| Credential scanner & sensitivity classifier | `crates/runtime/src/vault/scan.rs` | 1-50, 290-356 |
| Session-scoped KEK cache (`OnceLock<Mutex<VaultManager>>`) | `crates/runtime/src/vault_session.rs` | 15-20, 33-99 |
| Convenience read/write helpers | `crates/runtime/src/vault_session.rs` | 101-133 |
| Encrypted private project memory (`PrivateProjectMemory`) | `crates/runtime/src/private_memory.rs` | 1-15, 30-178 |
| Crypto helpers for private memory (`encrypt_blob`, `decrypt_blob`) | `crates/runtime/src/private_memory.rs` | 180-213 |
| Project-hash derivation (SHA-256) | `crates/runtime/src/private_memory.rs` | 235-257 |
| `/memory` summary, show, inspect, budget for vault/private | `crates/commands/src/handlers.rs` | 696-706, 763-768, 824-826, 949 |
| `/memory why` — explicit "never injected" statement | `crates/commands/src/handlers.rs` | 899-913 |
| Module-level reexports | `crates/runtime/src/lib.rs` | 20, 157, 176 |
| SSH alias / credential bridge | `crates/runtime/src/ssh/vault_alias.rs` | 28-251 |
| Project-archive sweep (deletes `~/.anvil/private/<hash>.enc`) | `crates/anvil-cli/src/project.rs` | 19, 114-118, 145 |
| Spec entry for `/memory` tiers | `crates/commands/src/specs.rs` | 261-269 |
| Spec entry for `/vault` command | `crates/commands/src/specs.rs` | 1363+ |

### Data layout

| Tier | Path | Format | Encryption | Retention |
|---|---|---|---|---|
| Vault metadata | `~/.anvil/vault/vault.meta` | JSON, **plaintext** | none (holds Argon2id salt, KDF params, verify token) | persistent until `/vault destroy` |
| Vault credential | `~/.anvil/vault/cred_<sanitized-label>.enc` | JSON envelope, base64 ciphertext | AES-256-GCM, per-credential random DEK, DEK itself wrapped by KEK | persistent |
| Vault TOTP | `~/.anvil/vault/totp_<sanitized-label>.enc` | JSON envelope | as above | persistent |
| Private project memory | `~/.anvil/private/<sha256(canonical-root)>.enc` | `[12-byte nonce][AES-256-GCM ciphertext]`, plaintext is `{"key":"value", ...}` JSON | AES-256-GCM, KEK used directly (no per-file DEK) | persistent; removed by `anvil project archive` |
| File mode (Unix) | n/a | n/a | files created with mode `0o600` (`private_memory.rs:216-227`) | n/a |

### Verified vs claimed

| README claim (line) | Code reality | Verdict |
|---|---|---|
| AES-256-GCM envelope, random DEK per credential (`README.md:499`) | `vault/mod.rs:9-11`, `vault/crypto.rs` | confirmed |
| Argon2id `65MB, 3 iterations, 4 parallelism` (`README.md:500`) | `vault/mod.rs:257` — `Params::new(65536, 3, 4, Some(32))` | confirmed (65536 KiB = 64 MiB; README rounds up) |
| "Master password prompted once per session, KEK held in memory only" (`README.md:501`) | `vault_session.rs:15-20` `OnceLock` + `vault_session.rs:8-11` docstring | confirmed |
| RFC 6238 TOTP (`README.md:502`) | `vault/mod.rs:13` plus `crypto::generate_totp_code` | confirmed |
| macOS Keychain integration | `grep -ri keychain crates/ README.md` returns **no matches** | **not implemented**. The README does not actually claim Keychain; the audit prompt asked us to verify. Denied. |

### Bugs / oddities found while reading

- `handlers.rs:696` checks `home.join("vault.bin")` to decide if the vault is
  initialised, but the real init marker is `~/.anvil/vault/vault.meta`
  (`vault/storage.rs:50`, `vault/mod.rs:238`). The `/memory` one-line summary
  for the vault tier will therefore always say "not initialized" regardless
  of actual state. Cosmetic, but a real defect.
- `vault/scan.rs:343-356` defines `classify_learning`, the auto-promote bridge
  into L5. **It has no production callers** — grep shows only the in-module
  tests. The taxonomy `Credential / Infrastructure / Knowledge` is declared
  but never used to route real content. This is the largest miscategorisation
  in the layer.
- `PrivateProjectMemory::format_for_context` (`private_memory.rs:148-176`)
  emits a fully-formed system-prompt block. It is **also** only called from
  tests. So the "never injected" claim (`handlers.rs:910`) is true today,
  but the loaded gun sits on the table.

---

## 3. What's missing or miscategorised

1. **Vault and private memory are one layer, not two.** They share the same KEK
   (`private_memory.rs:9-11`), the same unlock event, the same session lifetime,
   and the same "never injected" rule. The current split is accidental — it
   reflects two different implementation phases, not a principled distinction.
   Recommendation: collapse into **L5** with sub-categories
   `credentials` (vault) and `infra-facts` (private). Keep the on-disk paths
   distinct (different formats), but unify the user-facing tier name.
2. **The auto-promote bridge is dead.** `classify_learning`
   (`vault/scan.rs:345`) is supposed to route detected credentials and infra
   strings into L5 instead of L3 nominations. Nothing calls it. This means
   in practice today an LLM-suggested learning like
   `bastion=10.0.70.80` becomes a `nominations/*.json` plaintext file, not
   a `private/<hash>.enc` blob. **Plaintext leak of infra facts into L2/L3.**
3. **`/memory inspect` cannot search L5 even by key.** `handlers.rs:825-826`
   tells the user "Vault and encrypted private memory are not searched for
   security reasons." But searching *key names* (not values) is safe and
   useful — `/vault list` does it for credentials. Private memory has no
   equivalent.
4. **No unified `/memory show identity`.** Today the user must know whether
   to type `vault` or `private`. There is no single L5 view.
5. **`/memory budget`** explicitly excludes L5 (`handlers.rs:949`). It could
   safely show **count and total bytes** without revealing content.
6. **The SSH alias module (`ssh/vault_alias.rs`)** stores SSH host descriptors
   in the vault as a `HostCredential` (`vault/mod.rs:95`). This means infra-host
   metadata sometimes lives in the credential vault (because it's bundled with
   a private key) and sometimes in `PrivateProjectMemory` (for free-text infra
   notes). The split is determined by whether the user used `/vault ssh add` or
   typed prose. Another accidental boundary.
7. **`vault.meta` is plaintext.** Acceptable (it only holds the Argon2id salt
   and a verify token), but the layer doc should make it explicit so L7 cache
   layers know the salt file is not sensitive.
8. **The `handlers.rs:696` `vault.bin` bug** (see §2) reveals that no one
   actually exercises the `/memory` vault summary path. Adding the unified L5
   inspector in §5 will force that path live and catch the regression.

---

## 4. Inspector surface

### Today

| `/memory` invocation | Output for L5 |
|---|---|
| `/memory` (no args, `handlers.rs:685-714`) | One line for vault ("initialized (encrypted)" iff `~/.anvil/vault.bin` exists — see §2 bug) and one line for private ("N encrypted file(s)" or "no files") |
| `/memory show vault` (`handlers.rs:763`) | Literal string: *"Vault contents are not shown in plain text for security reasons. Use /vault list to see stored credential names."* |
| `/memory show private` (`handlers.rs:766`) | *"Private memory is AES-256-GCM encrypted and vault-locked. Unlock the vault first, then use the private memory API."* (note: there is no public CLI for browsing private memory) |
| `/memory inspect <key>` (`handlers.rs:778-831`) | Scans only `anvil-md` and `nominations`. Returns: *"Vault and encrypted private memory are not searched for security reasons."* |
| `/memory budget` (`handlers.rs:916-951`) | L5 not listed in the table. Trailer: *"Note: vault and private memory are excluded (encrypted, not injected)."* |
| `/memory why` (`handlers.rs:899-913`) | The seven-step injection order — explicitly: |

> *"The vault, private memory, and encrypted tiers are NEVER injected automatically."*
> — `crates/commands/src/handlers.rs:910`

> *"Nominations are SUGGESTED only -- they only enter the prompt after /memory promote."*
> — `crates/commands/src/handlers.rs:911`

This is the load-bearing user-facing safety invariant for L5. Every change in
§5 must preserve it verbatim.

### What L5 inspector can safely show

| Field | Safe to show? | Why |
|---|---|---|
| Tier name, file count | yes | already shown |
| Total bytes on disk | yes | reveals no plaintext |
| Sorted list of vault credential labels | yes | `/vault list` already does this |
| Sorted list of private-memory keys | **yes** if vault is unlocked | key names are not values |
| Decryption status (unlocked / locked) | yes | already a public concept |
| Last-modified time per entry | yes | metadata, not content |
| Any value | **never** | this is the entire safety claim |

The new `/memory show identity` should show the top of this table when the
vault is locked, and the full table when unlocked.

---

## 5. Migration moves

Numbered, with file targets and effort estimates. Effort is for one engineer,
includes tests, excludes review.

1. **Add `identity` as an alias tier covering vault + private** (~2h).
   Edit `crates/commands/src/specs.rs:262-269` to add a `identity` row to the
   TIERS list with summary "Unified view of vault credentials + private infra
   facts (L5)". Add `identity` to the `Tiers:` usage strings at
   `crates/commands/src/handlers.rs:723` and the unknown-tier message at
   `handlers.rs:773`.

2. **Implement `memory_show_identity`** (~4h).
   New private fn in `crates/commands/src/handlers.rs` near line 763. If vault
   is locked (`vault_session::vault_is_session_unlocked` is false), print a
   locked banner only. If unlocked: list vault labels via
   `with_session_vault(|vm| vm.list_credentials())` and private-memory keys
   via `PrivateProjectMemory::for_project(&cwd).load(kek).map(keys)`.
   **Never** dereference values. Add unit test in `handlers.rs` tests module.

3. **Extend `/memory inspect` to search L5 key-names only** (~2h).
   Edit `memory_inspect` (`handlers.rs:778-831`) to, when the vault is
   unlocked, scan credential labels and private-memory keys for substring
   matches. Emit results as `[identity:vault] LABEL` and
   `[identity:private] KEY` lines. Update the "not searched" message so it
   only appears when the vault is **locked**. Update the explanatory string
   at `handlers.rs:825-826`.

4. **Extend `/memory budget` to include L5 counts** (~1h).
   Edit `memory_budget` (`handlers.rs:916-951`) to add an `identity` row
   showing (a) bytes on disk for `~/.anvil/vault/` + `~/.anvil/private/`,
   (b) a literal `~Tokens` column of `0` (we never inject them), and (c)
   an `(encrypted, not injected)` annotation. Move the trailer note up into
   the table row so the user sees it inline.

5. **Fix the `vault.bin` bug in `memory_summary`** (~15min).
   `handlers.rs:696` currently checks `home.join("vault.bin")`. Replace with
   `runtime::vault_is_initialized()` (already exported from `vault_session`).

6. **Wire `classify_learning` into the nominations pipeline** (~6h, the most
   subtle move). Locate the nomination-emit site (whatever today writes
   `~/.anvil/nominations/*.json` from learning candidates). Before persisting,
   call `vault::scan::classify_learning` on the candidate's content. Branch:
   - `SensitivityLevel::Credential` → reject the nomination, log "use /vault
     store instead"
   - `SensitivityLevel::Infrastructure` → if vault is unlocked, write to
     `PrivateProjectMemory` via `upsert`; if locked, drop the nomination and
     surface a banner ("infrastructure learning rejected — unlock vault to
     persist")
   - `SensitivityLevel::Knowledge` → existing nomination path
   This closes the **L3-to-L5 plaintext leak** identified in §3.2.

7. **Document `format_for_context` as off-by-default** (~30min).
   Edit `crates/runtime/src/private_memory.rs:141-176` to add a `#[cfg(test)]`
   guard *or* a `#[doc(hidden)]` plus an `// LOAD-BEARING: this function is
   intentionally dead code — see SEVEN-LAYER-MEMORY.md L5 §1` comment. Goal:
   prevent a future PR from wiring it into the prompt builder by accident.
   Do NOT delete the function — keeping it makes the temptation explicit and
   testable.

8. **Add a `/memory why` paragraph for L5 routing** (~30min).
   Edit `handlers.rs:899-913` to append: *"Detected credentials and
   infrastructure strings are routed at learning-classification time to L5
   (identity). They never become nominations."* This documents the §5.6 change.

9. **Migration test: zero-injection assertion** (~2h).
   New integration test under `crates/runtime/tests/` that builds a full
   system prompt with the vault unlocked and a populated `PrivateProjectMemory`,
   then asserts neither label/key/value appears in the rendered prompt bytes.
   This is the regression net for moves 1-8.

**Total effort estimate: ~18 hours (~2.5 dev-days).**

---

## 6. Risks and reversibility

This is the **highest-risk layer**. A mistake leaks credentials.

### Safety invariants (must hold across every move)

1. **No L5 value ever appears in any L1 prompt block.** Enforced by move 9
   (zero-injection test).
2. **No L5 value ever appears in any L2 episodic file.** Daily summary
   (`crates/runtime/src/daily.rs`) currently has a `credentials_auto_vaulted`
   counter (line 50) but no path that would log a value — verify this still
   holds after move 6.
3. **No L5 value ever reaches L7 cache.** Today the cache layers (file-cache,
   cmd-cache) are unaware of L5; the content filter (`content_filter.rs:87-105`)
   documents the rule. Move 6 must not introduce a cache write.
4. **Vault unlock is single-use per session.** Don't double-prompt; don't
   re-derive the KEK on each access. Already enforced via `OnceLock`
   (`vault_session.rs:15-20`).
5. **KEK never crosses a process boundary.** No serialization to disk, no IPC,
   no shell env export. Today's code respects this — move 2's `with_session_vault`
   closure pattern preserves it.

### Failure modes

| If move… | Risk | Detection | Roll-back |
|---|---|---|---|
| 2 prints values | Direct credential leak in TUI/web | Manual review + move 9 test | Revert the function; the locked-banner path is the conservative default |
| 3 substring-searches values instead of keys | Same as above | Manual review | Revert; keep "not searched" message |
| 6 mis-classifies a credential as `Knowledge` | Plaintext credential lands in `nominations/*.json` | Add a final `content_filter::scan_for_secrets` pass on every nomination write as belt-and-braces | The fallback secret-scan blocks the write at the filter, not at the classifier |
| 6 mis-classifies plain text as `Credential` and the vault is locked | Genuine learning silently dropped | Log every classification at INFO; surface a one-time banner | User can retry after unlock |
| 7 `format_for_context` deletion forgotten | Future PR re-enables injection | Static check: a `cargo test` assertion that the function is `#[cfg(test)]`-gated or `doc(hidden)` | Revert the offending PR |
| 5 `vault.bin` fix wrong path | `/memory` summary stays buggy | Manual smoke | Revert one line |

### Reversibility

All moves are **inspector-side** (handlers, specs, classifier wiring). None
change the on-disk format, none change the KEK derivation, none touch the
session unlock flow. To roll back any single move: revert the commit. There
is no data migration to undo, because L5 data formats are unchanged.

The single move that touches *write* paths is **§5.6** (classifier wiring).
For belt-and-braces, gate it behind a feature flag
(`ANVIL_L5_AUTOROUTE=1`) for the first release; default off; flip default to
on in the next release once the test suite + manual testing have caught any
mis-classifications.

---

## 7. Cross-layer dependencies

| Layer | Touch-point | What to verify or change |
|---|---|---|
| **L1 Working** | Prompt builder + `format_for_context` (`private_memory.rs:148`) | Verify `format_for_context` is never called from the prompt-build path. Add the move-9 integration test. A future `vault:<label>` substitution syntax (proposed elsewhere) belongs here — keep the substitution at *render time*, never at *memorise time*, so the substituted plaintext never lands in any persisted block. |
| **L2 Episodic** | `daily.rs:50` `credentials_auto_vaulted` counter; nomination writer | Verify the daily summary records only **counts**, never labels or values. After move 6, the nomination writer must reject anything `classify_learning` flags as `Credential` or `Infrastructure`. |
| **L3 Semantic** | `vault/scan.rs:343-356` `classify_learning` is the bridge | Move 6 wires it in. Sensitive facts auto-route to L5 instead of L3 nominations. Today this routing is dead. |
| **L4 Procedural** | Skills referencing vault | A skill *body* must not embed a vault value. Skill *parameters* may reference `vault:<label>` and resolve at execution time inside a vault-unlocked context (use `vault_session::with_session_vault`). The skill loader code (search `crates/runtime/src/skills/` if it exists; not surveyed in this audit) should reject any skill that captures a resolved secret into a stored variable. |
| **L6 Policy** | `vault_session::init_session_vault` | Vault unlock is a permission decision: the master password prompt is the only place where Anvil asks the user for a secret. Permission memory (`crates/runtime/src/permission_memory.rs`, L6, not L5) governs *whether a tool can ask for a credential*; L5 governs *where the credential is stored*. The two must not be merged. |
| **L7 Cache** | `content_filter.rs:87-105` doc note | Cache layers (file-cache, cmd-cache, response cache) MUST NEVER receive L5 plaintext. Today the boundary is enforced by *not calling* L5 from any cache path. After move 6, audit every new code path that touches `PrivateProjectMemory` to ensure none writes to a cache. The move-9 zero-injection test should be extended with a cache-scan assertion. |

---

*End of L5 audit. Synthesis writer: see §3 for the vault+private collapse
proposal, §5 for the ordered migration, §6 for the safety invariants the
final design must preserve.*