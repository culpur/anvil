# Anvil v2.2.13 — Windows is back, BSD joins, routines on disk

Released: 2026-05-11

v2.2.12 deferred Windows by one release while the new SSH agent code
got a cross-platform fix. v2.2.13 closes that gap — Windows x86_64 is
back, FreeBSD x86_64 and NetBSD x86_64 join the platform list, and a
quiet on-disk routines foundation has landed in the runtime ready for
the v2.2.14 daemon work. Single-binary upgrade — no config migration,
no new dependencies, no behavior change for existing sessions. Update
via `anvil upgrade` or `brew reinstall anvil`.

This release ships on **seven platforms**: macOS ARM64, macOS Intel,
Linux x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, and NetBSD
x86_64. Every binary is SHA256-verified by the release pipeline and
published with a paired `.sha256` manifest at
[anvilhub.culpur.net/sha256/](https://anvilhub.culpur.net/sha256/).

---

## Headline: Windows x86_64 is back

The Windows hold in v2.2.12 had a specific cause. The new SSH agent
authentication code called a Unix-domain-socket function that doesn't
exist on Windows, so the cross-compile failed at the type-check step.
The fix is structural: the `auth_agent` function is now
`#[cfg(unix)]`-gated, and a `#[cfg(not(unix))]` stub takes its place
on Windows. The stub emits a clear `AuthFailure` event explaining
that ssh-agent auth isn't supported on this platform and that the
user should use a key file or password instead.

The rest of the SSH auth chain — key-file, password,
keyboard-interactive — works on Windows exactly as it does on Unix.
Only the ssh-agent path is platform-conditional. Windows users get
the SSH tab feature introduced in v2.2.12 with the same modal
connection form, the same vt100 rendering, and the same Ctrl+B
prefix keys; they just need to point Anvil at a key file rather than
rely on Pageant or ssh-agent.

Windows users on v2.2.11 can finally upgrade.

---

## Headline: FreeBSD x86_64 and NetBSD x86_64

Two new binaries land in v2.2.13:

- **FreeBSD x86_64** (`anvil-x86_64-unknown-freebsd`)
- **NetBSD x86_64** (`anvil-x86_64-unknown-netbsd`)

Both are SHA256-verified and signed by the release pipeline. Drop the
binary on a FreeBSD or NetBSD host, `chmod +x`, and run. No
prerequisites beyond the OS itself.

The `install/install.sh` script now detects FreeBSD, OpenBSD, and
NetBSD from `uname` output and resolves the correct binary
automatically. A `dist/freebsd/Makefile` skeleton ships in the repo
for users who want to drop Anvil into their local FreeBSD ports tree.

**FreeBSD ARM64 and OpenBSD x86_64 are not shipping in this
release.** The Rust toolchain does not publish a precompiled standard
library for either target, which makes a clean cross-compile path
substantially more involved. Both are queued for v2.2.14.

---

## Headline: release pipeline hardening

The v2.2.12 release shipped four binaries instead of five because
the Windows build failed silently — a subprocess output filter in
the release script was masking the exit code, so the pipeline kept
going and copied a stale v2.2.11 artifact into the v2.2.12 release.
This class of bug shouldn't be possible.

v2.2.13 removes the mask. A failed cross-compile now aborts the
release before any artifact gets uploaded. Combined with the
v2.2.12 release-pipeline batch (tag-vs-HEAD pre-flight,
build-from-tagged-commit, php-lint guard, render-time changelog
injection), the pipeline is significantly less able to ship a
partial or stale build without anyone noticing.

---

## Quiet headline: routines foundation on disk

A new module landed in the runtime: `crates/runtime/src/routines/`.
It contains three components — schedule grammar, output archive,
packet schema — plus a top-level `[SILENT]` marker constant. No
existing code consumes it. No user-facing command surfaces it.
Nothing in this release runs a routine. The module is foundation
for the v2.2.14 daemon work and is described here so the on-disk
shape is known.

**Schedule grammar** (`schedule.rs`, 25 tests). A `Schedule` enum
accepts four forms:

- Duration: `30m`, `2h`, `90s`
- Interval: `every 2h`
- Cron: classical 5-field syntax (`0 9 * * *`)
- ISO 8601 UTC timestamp: `2026-05-15T09:30:00Z`

`next_fire(&Schedule, after: u64) -> Option<u64>` returns the next
unix-timestamp the routine should run. Classical 5-field cron is
powerful but has friction for one-shot timers and human-readable
intervals — the duration / interval / ISO forms cover the common
cases without forcing every user to memorize cron syntax.

**Output archive** (`archive.rs`, 16 tests). Each routine run writes
a markdown file to `~/.anvil/routines/output/<routine_id>/<ISO>.md`
with YAML frontmatter containing the run record. Status is `Clean`
(output present, no marker), `Silent` (output contained the
`[SILENT]` marker — meaning no signal to surface this run), or
`Failed`. All writes are atomic; the output directory uses 0o700
perms.

**Packet schema** (`packet.rs`, 18 tests). When a routine needs to
inject its result into a conversation, it builds a `RoutinePacket`
with an `input_hash` (SHA-256 over system_prompt + user_prompt +
script_output) and a 280-char summary extracted from the first
non-empty paragraph of output. The packet wraps in anti-injection
delimiters; any pre-existing markers in the script output are
stripped before wrapping so an upstream script can't smuggle a fake
packet through.

**The `[SILENT]` marker** is the early-stop primitive. A routine
that polls `every 5m` for "is the build broken?" can emit `[SILENT]`
when the build is green and no model invocation happens at all —
zero tokens until the script detects a real signal. This shifts the
cost model for long-running, low-information observers from "pay
every fire" to "pay only on change."

Routines, daemon, and `/routine` slash command land in v2.2.14.

---

## Quality

- **1,146 workspace tests pass** — 583 lib tests in the runtime
  crate plus per-tab inference, SSH, and tool-card integration
  tests carried over from v2.2.12. Zero failures. Zero warnings.
- **63 new tests for the routines module** — 25 schedule, 16
  archive, 18 packet, plus the smoke test on `is_silent_output`.
- **Seven binaries this release.** macOS ARM64, macOS Intel, Linux
  x86_64, Linux ARM64, Windows x86_64, FreeBSD x86_64, NetBSD
  x86_64.
- **22–27 MB single binary** — no runtime dependencies, no install
  prerequisites beyond the binary itself.

---

## Install

```bash
# Homebrew (macOS & Linux)
brew upgrade culpur/anvil/anvil
# or fresh install
brew install culpur/anvil/anvil

# curl installer (macOS / Linux / FreeBSD / NetBSD)
curl -fsSL https://anvilhub.culpur.net/install.sh | bash

# PowerShell (Windows)
irm https://anvilhub.culpur.net/install.ps1 | iex

# Already installed
anvil upgrade
```

FreeBSD users with a local ports tree can drop the binary in via
the skeleton `dist/freebsd/Makefile` in the repo.

---

## Full changelog

https://github.com/culpur/anvil/compare/v2.2.12...v2.2.13
