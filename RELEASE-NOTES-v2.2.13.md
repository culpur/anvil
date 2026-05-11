# Anvil v2.2.13 — Windows is back, BSD joins, routines on disk

Released: 2026-05-11

v2.2.12 deferred Windows by one release while the new SSH agent code
got a cross-platform fix. v2.2.13 closes that gap — Windows x86_64 is
back in the build matrix, the release pipeline gains FreeBSD x86_64
and NetBSD x86_64 binaries on top, and a quiet on-disk routines
foundation has landed in the runtime ready for the v2.2.14 daemon
work. Single-binary upgrade — no config migration, no new
dependencies, no behavior change for existing sessions. Update via
`anvil upgrade` or `brew reinstall anvil`.

This release ships on **seven platforms**: macOS ARM64, macOS Intel,
Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, and NetBSD
x86_64. All binaries are SHA256-verified and signed by the release
pipeline. FreeBSD ARM64 and OpenBSD x86_64 are source-only this
release — the Rust toolchain does not ship a precompiled standard
library for either target, and a clean cross-compile path that builds
the stdlib from source is queued for v2.2.14.

---

## Headline: Windows x86_64 is back

The Windows hold in v2.2.12 had a specific cause. The new SSH agent
authentication code in `crates/runtime/src/ssh/driver.rs` called
`russh::AgentClient::connect_uds()` — a function that only exists on
Unix. Windows has no Unix-domain sockets, so the cross-compile failed
at the type-check step. The fix is structural, not a workaround: the
`auth_agent` function is now `#[cfg(unix)]`-gated, and a
`#[cfg(not(unix))]` stub takes its place on Windows. The stub emits a
clear `AuthFailure` event explaining that ssh-agent auth isn't
supported on this platform and that the user should use a key file or
password instead.

The rest of the SSH auth chain — key-file, password, keyboard-interactive —
works on Windows exactly as it does on Unix. Only the ssh-agent path
is platform-conditional. Windows users get the SSH tab feature
introduced in v2.2.12 with the same modal connection form, same
russh-backed vt100 rendering, same Ctrl+B prefix keys; they just need
to point Anvil at a key file rather than rely on Pageant or
ssh-agent.

This is the v2.2.13 unblocker. Windows users on v2.2.11 can finally
upgrade.

---

## Headline: BSD cross-compile in the release pipeline

The build matrix gains two BSD targets:

- **FreeBSD x86_64** (`x86_64-unknown-freebsd`) — Rust Tier 2, hard-fail in the pipeline
- **NetBSD x86_64** (`x86_64-unknown-netbsd`) — Rust Tier 3, soft-fail with source-build fallback

Cross-compile runs through Culpur-owned builder images at
`registry.culpur.net/culpur/anvil-builder-<target>:rust-<version>`.
The images are built from `rust:1.94-bookworm` plus an extracted
FreeBSD 14.3 or NetBSD 9.3 sysroot, with the C cross-toolchain
(clang + lld + llvm-ar) and target-specific CFLAGS / linker flags
preconfigured. Dockerfiles live at `dist/builders/` and are rebuilt +
pushed to the registry whenever the Rust toolchain version bumps.

The pre-flight that produced this design caught real problems early:
the upstream cross-rs images turned out to have no `cargo` or `rustc`
on PATH (they ship only the C toolchain), their `:0.2.5` tag is
missing for FreeBSD ARM64, and OpenBSD has no published image at all.
Building our own images removes all three failure modes.

The install script (`install/install.sh`) detects FreeBSD, OpenBSD,
and NetBSD from `uname` output. FreeBSD x86_64 and NetBSD x86_64 fall
through to the prebuilt binary download path. FreeBSD ARM64, OpenBSD
x86_64, and any non-x86_64 NetBSD variant exit with a clear message
pointing at the source build:

```bash
cargo install --git https://github.com/culpur/anvil-source
```

A `dist/freebsd/Makefile` skeleton ships in the repo for users who
want to drop Anvil into their local FreeBSD ports tree.

---

## Headline: release pipeline hardening

The v2.2.12 release shipped four binaries instead of five because the
Windows build failed silently — the cross-compile errored, but
`scripts/release.sh` had `cargo build … 2>&1 | tail -1` masking the
exit code, so the pipeline kept going and copied a stale v2.2.11
artifact into the v2.2.12 release. This is a class of bug that
shouldn't be possible.

v2.2.13 removes the `| tail -1` mask from every cargo invocation in
the release script. `set -euo pipefail` at the top of the script now
actually catches a build failure. A failed cross-compile aborts the
release before any artifact gets uploaded.

Combined with the v2.2.12 T1 release-pipeline batch (tag-vs-HEAD
pre-flight, build-from-tagged-commit, php-lint guard, `changelog.json`
render-time injection), the release pipeline is now significantly
less able to ship a partial or stale build without anyone noticing.

---

## Quiet headline: routines foundation on disk

A new module landed in the runtime: `crates/runtime/src/routines/`.
It contains three components — `schedule`, `archive`, `packet` — plus
a top-level `[SILENT]` marker constant. No existing code consumes it.
No user-facing command surfaces it. Nothing in this release runs a
routine. The module is foundation for the v2.2.14 daemon work and is
described here so the on-disk shape is known.

**Schedule grammar** (`schedule.rs`, 421 lines, 25 tests). A
`Schedule` enum accepts four forms:

- Duration: `30m`, `2h`, `90s`
- Interval: `every 2h`
- Cron: classical 5-field syntax (`0 9 * * *`)
- ISO 8601 UTC timestamp: `2026-05-15T09:30:00Z`

`parse_schedule()` returns a `Schedule`; `next_fire(&Schedule, after: u64) -> Option<u64>`
returns the next unix-timestamp the routine should run. Cron handling
reuses the existing `cron::unix_to_parts` plumbing; ISO parsing accepts
RFC3339 with explicit UTC offset only. Classical 5-field cron is
powerful but has friction for one-shot timers and human-readable
intervals — the duration / interval / ISO forms cover the common cases
without forcing every user to memorize cron syntax.

**Output archive** (`archive.rs`, 431 lines, 16 tests). Each routine
run writes a markdown file to
`~/.anvil/routines/output/<routine_id>/<ISO>.md` with YAML frontmatter
containing the run record (routine_id, run_id, timestamps, duration,
status, schedule_display, model, tokens, output, error). Filenames use
the lexicographically-sortable `YYYYMMDDTHHMMSSZ` form; frontmatter
uses the human-readable `YYYY-MM-DDTHH:MM:SSZ` form. Status is
`Clean` (output present, no marker), `Silent` (output contained the
`[SILENT]` marker — meaning no signal to surface this run), or
`Failed`. Path-traversal validation rejects `..`, `/`, and null bytes
in routine IDs. All writes are atomic via tmp+rename; the output
directory is created with 0o700 perms.

**Packet schema** (`packet.rs`, 381 lines, 18 tests). When a routine
needs to inject its result into a conversation, it builds a
`RoutinePacket` with an `input_hash` (SHA-256 over system_prompt +
user_prompt + script_output, NUL-separated) and a 280-char summary
extracted from the first non-empty paragraph of output. The packet
wraps in anti-injection delimiters
(`<<<ROUTINE-PACKET-START>>>` / `<<<ROUTINE-PACKET-END>>>`); any
pre-existing markers in the script output are stripped before wrapping
so an upstream script can't smuggle a fake packet through. The
`input_hash` lets a future cache layer recognize when a script
re-emits identical output and skip the round-trip.

**The `[SILENT]` marker** is the early-stop primitive. A routine
that polls `every 5m` for "is the build broken?" can emit
`[SILENT]` when the build is green and no model invocation happens at
all — zero tokens until the script detects a real signal. This shifts
the cost model for long-running, low-information observers from
"pay every fire" to "pay only on change."

`is_silent_output(output: &str) -> bool` is the public API. Routines
work, daemon, and `/routine` slash command land in v2.2.14.

---

## Under the hood

- **Workspace version bump.** `Cargo.toml` workspace version moves
  from `2.2.12` to `2.2.13`. Pure metadata change; no source impact.
- **`#[cfg(unix)]` discipline.** `ssh/driver.rs` `auth_agent` is now
  conditionally compiled with a parallel Windows stub. The rest of
  the SSH driver — `auth_password`, `auth_publickey`, the russh
  client handler, the keystroke encoder, the event channel — compiles
  identically on both platforms.
- **Build matrix expanded from 5 to 7.** `scripts/release.sh`
  `TARGETS` array grows; per-target build steps use either the host
  toolchain (macOS), an Ubuntu-based Docker container (Linux + Windows
  mingw), or a Culpur-owned builder image at registry.culpur.net (BSD).
- **Builder images are first-class artifacts.** `dist/builders/`
  contains the Dockerfiles + the `build-and-push.sh` script that
  bakes the images and pushes them to `registry.culpur.net/culpur/`.
  They are rebuilt on Rust toolchain bumps, not on every release.
- **Tier-3 soft-fail.** The NetBSD build step continues past a
  toolchain-bump error with a clear warning rather than aborting the
  release. Its absence from a release is logged but does not block
  macOS / Linux / Windows / FreeBSD shipment.
- **install.sh BSD detection.** Resolves `freebsd`, `openbsd`,
  `netbsd` from `uname -s`. Falls back to a source-build hint for
  unsupported combinations (FreeBSD ARM64, OpenBSD any arch, NetBSD
  ARM64).
- **`dist/freebsd/Makefile`.** Skeleton FreeBSD port file with the
  release URL, distfile pattern, install rule, and license metadata.
  Not submitted to the FreeBSD ports tree as part of this release.

---

## Quality

- **All workspace tests pass** — 583 lib tests in the runtime crate
  plus per-tab inference, SSH, and tool-card integration tests carried
  over from v2.2.12. Zero failures. Zero warnings.
- **63 new tests for the routines module** — 25 schedule, 16 archive,
  18 packet, plus the smoke test on `is_silent_output`. None of these
  tests gate the v2.2.13 release; they validate the foundation that
  v2.2.14 will build on.
- **Seven binaries this release.** macOS ARM64, macOS Intel, Linux
  x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64
  (Tier-3, may be source-only on some toolchain bumps). FreeBSD ARM64
  and OpenBSD x86_64 are source-only and queued for v2.2.14 once a
  build-std path is in place.
- **~22 MB single binary** — no runtime dependencies, no install
  prerequisites beyond the binary itself.

---

## Install

```bash
# Homebrew (macOS & Linux)
brew upgrade culpur/anvil/anvil
# or fresh install
brew install culpur/anvil/anvil

# curl installer (macOS / Linux / FreeBSD / OpenBSD / NetBSD)
curl -fsSL https://anvilhub.culpur.net/install.sh | bash

# PowerShell (Windows)
irm https://anvilhub.culpur.net/install.ps1 | iex

# Already installed
anvil upgrade
```

FreeBSD x86_64 users with a local ports tree can drop the binary in
via the skeleton `dist/freebsd/Makefile` in the repo. FreeBSD ARM64,
OpenBSD x86_64, and any non-x86_64 NetBSD users build from source
with `cargo install --git`; the install script detects this case and
prints the exact command to run.

---

## Full changelog

https://github.com/culpur/anvil/compare/v2.2.12...v2.2.13
