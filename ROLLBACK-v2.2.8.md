# Anvil v2.2.8 rollback runbook

If v2.2.8 misbehaves after install, this document gives you three rollback
paths in order of preference. Every path preserves your vault, sessions, and
project memory — none of them touch `~/.anvil/vault/` or
`~/.anvil/sessions/`.

The v2.2.7 release artifacts remain published on GitHub and Homebrew in
perpetuity — rollback is always available.

---

## Path 1 — `anvil upgrade --to 2.2.7` (quickest)

If the v2.2.8 binary still starts on your machine, use its own upgrade
tooling to downgrade:

```bash
anvil upgrade --to 2.2.7
```

This:

1. Downloads the v2.2.7 binary from
   `https://github.com/culpur/anvil/releases/download/v2.2.7/`
2. Verifies its SHA256 against `https://anvilhub.culpur.net/sha256/` (with
   GitHub as fallback)
3. Atomically replaces your current `anvil` binary

The installed v2.2.7 binary will refuse to load any v2.2.8-shape plugin
manifests — but thanks to v2.2.8's forward-compat fix, bad manifests are
skipped with a warning instead of crashing the binary. You will see
`[plugin warning]` lines on stderr for anything v2.2.7 can't parse; the
binary runs normally.

---

## Path 2 — reinstall v2.2.7 from Homebrew

If `anvil upgrade` fails (e.g., v2.2.8 crashes before dispatch), Homebrew
keeps older formula revisions available:

```bash
brew uninstall culpur/anvil/anvil
brew install culpur/anvil/anvil@2.2.7
```

Or pin to the specific commit that shipped v2.2.7 by editing the formula
URL in `$(brew --repository culpur/anvil)/Formula/anvil.rb` to point at the
v2.2.7 release assets.

---

## Path 3 — direct binary download

Bypass Homebrew entirely. Download the v2.2.7 binary for your platform:

```bash
# macOS Apple Silicon
curl -fsSL https://github.com/culpur/anvil/releases/download/v2.2.7/anvil-aarch64-apple-darwin -o anvil
curl -fsSL https://anvilhub.culpur.net/sha256/anvil-aarch64-apple-darwin.sha256 -o anvil.sha256
shasum -a 256 -c anvil.sha256
chmod +x anvil
sudo mv anvil /usr/local/bin/anvil

# macOS Intel
curl -fsSL https://github.com/culpur/anvil/releases/download/v2.2.7/anvil-x86_64-apple-darwin -o anvil
# ... same verify/install steps

# Linux x86_64
curl -fsSL https://github.com/culpur/anvil/releases/download/v2.2.7/anvil-x86_64-unknown-linux-gnu -o anvil

# Linux ARM64
curl -fsSL https://github.com/culpur/anvil/releases/download/v2.2.7/anvil-aarch64-unknown-linux-gnu -o anvil

# Windows x86_64
curl.exe -fsSL https://github.com/culpur/anvil/releases/download/v2.2.7/anvil-x86_64-pc-windows-gnu.exe -o anvil.exe
```

v2.2.7 SHA256 manifest (for verification without fetching `.sha256` files):

| Target | SHA256 |
|---|---|
| `anvil-aarch64-apple-darwin` | `110e76abf5408b0600e1134e589f8b0b39a6fcd4fcc376ccf9f1eed448df108d` |
| `anvil-x86_64-apple-darwin` | `6bf06d967ff8c9a20ae76c55963ec2d5afacd8a82c75b05ed6678bf1dda7c818` |
| `anvil-x86_64-unknown-linux-gnu` | `a57618881cd080617e3faba25b490c5ceb3f11d6b2fef9c695e7f4908ce2dd97` |
| `anvil-aarch64-unknown-linux-gnu` | `644d4e581d33eb61a58f4c32324dbd61ff3861b9ab83b4bba4b3828488d6b351` |
| `anvil-x86_64-pc-windows-gnu.exe` | `4750ce0dd4b99586f49a0f4f13c505ff3fc3a17544cd053098e18ae37d8334fb` |

---

## After rolling back

### Clean up v2.2.8-only artifacts

v2.2.8 creates some state that v2.2.7 can't read. Safe to remove:

```bash
# Bundled plugins materialized from the v2.2.8 binary (will be re-materialized
# by whichever binary is running now; safe to delete)
rm -rf ~/.anvil/plugins/bundled/

# Output style config (v2.2.7 doesn't know this key but will ignore it silently)
# Leave alone — harmless. Reset with: anvil /output-style precise when back on v2.2.8.

# /agent compose doesn't persist state; nothing to clean

# /skill-eval snapshots live in ./skill-evals/ under whatever directory you
# ran the command from — your call whether to keep them as historical data
```

### Skills / plugins you installed under v2.2.8

Skills and plugins you installed from AnvilHub under v2.2.8 continue to work
under v2.2.7 as long as their manifests are v2.2.7-compatible. Any plugin
with a v2.2.8-only feature (prompt-type hooks, for example) will be
**skipped with a warning** under v2.2.7 — not a crash. Check stderr on
startup.

### Vault and sessions

- Vault (`~/.anvil/vault/`) is forward-and-backward compatible — no
  migration needed.
- Sessions (`~/.anvil/sessions/`) saved under v2.2.8 are readable by
  v2.2.7 unless they include v2.2.8-specific features. A v2.2.7 binary
  that encounters an unknown feature in a session skips the unknown and
  loads the rest.
- Daily summaries (`~/.anvil/daily/`) are plain JSON and fully compatible.

---

## Reporting the issue

If you rolled back because v2.2.8 misbehaved, please file an issue at
https://github.com/culpur/anvil/issues with:

- The exact command that failed
- The full stderr output (especially anything starting with
  `[plugin warning]` or `Error: Problem`)
- Your `anvil --version` from BEFORE the rollback
- Whether `anvil --check` reports anything unusual

This runbook itself gets updated if new rollback edge cases surface.

---

**v2.2.7 remains supported.** The v2.2.8 release is additive — it does not
deprecate or remove any v2.2.7 functionality. If a v2.2.8 feature gets in
your way, rolling back is a supported and documented path, not an
escape hatch.
