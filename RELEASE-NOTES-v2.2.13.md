# Anvil v2.2.13 — Windows is back, BSD joins, routines on disk

Released: 2026-05-11

v2.2.12 deferred Windows by one release while the new SSH agent code
got a cross-platform fix. v2.2.13 closes that gap — Windows x86_64 is
back in the build matrix, the release pipeline gained four BSD targets
on top, and a quiet on-disk routines foundation has landed in the
runtime ready for the v2.2.14 daemon work. Single-binary upgrade — no
config migration, no new dependencies, no behavior change for existing
sessions. Update via `anvil upgrade` or `brew reinstall anvil`.

This release ships on **nine platforms**, up from four in v2.2.12 and
five in v2.2.11: macOS ARM64, macOS Intel, Linux x86_64, Linux ARM64,
Windows x86_64, FreeBSD x86_64, FreeBSD ARM64, OpenBSD x86_64, and
NetBSD x86_64. All binaries are SHA256-verified and signed by the
release pipeline. Tier-2 targets (FreeBSD, both arches) are hard-fail
in the build; Tier-3 targets (OpenBSD, NetBSD) are soft-fail with a
build-from-source fallback when the cross-rs sysroot is unavailable.

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

The build matrix now includes four BSD targets:

- **FreeBSD x86_64** (`x86_64-unknown-freebsd`) — Rust Tier 2, hard-fail
- **FreeBSD ARM64** (`aarch64-unknown-freebsd`) — Rust Tier 2, hard-fail
- **OpenBSD x86_64** (`x86_64-unknown-openbsd`) — Rust Tier 3, soft-fail
- **NetBSD x86_64** (`x86_64-unknown-netbsd`) — Rust Tier 3, soft-fail

Cross-compile runs through cross-rs Docker images
(`ghcr.io/cross-rs/<target>:0.2.5`) alongside the existing Linux and
Windows mingw containers. The install script (`install/install.sh`)
detects FreeBSD, OpenBSD, and NetBSD from `uname` output and resolves
the correct binary name. A `dist/freebsd/Makefile` skeleton ships in
the repo so a FreeBSD user can drop the binary into their local ports
tree.

Tier-3 BSD users (OpenBSD, NetBSD) whose cross-rs image is missing or
broken on the day of a release get a clear message in install.sh
pointing them at a source build with `cargo install --git`. The Tier-2
FreeBSD targets are treated as first-class platforms — a missing
FreeBSD binary fails the release.

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
- **Build matrix expanded from 5 to 9.** `scripts/release.sh`
  `TARGETS` array grows; per-target build steps use either the host
  toolchain (macOS), an Ubuntu-based Docker container (Linux + Windows
  mingw), or a cross-rs container (BSD).
- **Tier-3 soft-fail.** OpenBSD and NetBSD build steps continue past a
  missing-sysroot error with a clear warning rather than aborting the
  release. Their absence from a release is logged in the release notes
  but does not block macOS/Linux/Windows/FreeBSD shipment.
- **install.sh BSD detection.** Resolves `freebsd`, `openbsd`,
  `netbsd` from `uname -s`. Falls back to a source-build hint for
  unsupported architecture combinations (OpenBSD ARM64, NetBSD ARM64).
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
- **Nine binaries this release.** macOS ARM64, macOS Intel, Linux
  x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, FreeBSD ARM64,
  OpenBSD x86_64 (Tier-3, may be source-only), NetBSD x86_64 (Tier-3,
  may be source-only).
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

FreeBSD users with a local ports tree can drop the binary in via the
skeleton `dist/freebsd/Makefile` in the repo. OpenBSD and NetBSD
users may need to build from source if the cross-compile sysroot was
unavailable on the day of release; the install script will detect
this and point them at the source build.

---

## Full changelog

https://github.com/culpur/anvil/compare/v2.2.12...v2.2.13
