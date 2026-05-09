---
name: command-cache-aware
description: Check the W12 command-cache before running ls, git status, cat, or grep — and never cache mutating commands.
triggers: ["command cache", "cache shell", "stop running", "already ran"]
intensity_default: low
---

# command-cache-aware

Before running any read-only shell command, check whether a cached result
exists:

```
/cmd-cache get "<command>"
```

If the result is present and within TTL, use it. Skip the shell call.

## Cacheable commands (safe to cache)

These commands are read-only and their output is stable within a typical
session. Cache them freely:

| Command pattern | Default TTL | Notes |
|---|---|---|
| `ls <dir>` | 5 min | Re-run after file creates/deletes |
| `find <dir> -name <pat>` | 5 min | Re-run after file creates |
| `git status` | 2 min | Re-run after any git operation |
| `git log --oneline -N` | 10 min | Stable unless commits land |
| `git diff [ref]` | 5 min | Re-run after edits |
| `cat <file>` | session | Use file-cache instead; /cmd-cache is a fallback |
| `grep -r <pattern> <dir>` | 5 min | Re-run after edits |
| `cargo metadata` | session | Changes only on Cargo.toml edits |
| `cargo tree` | session | Same |
| `npm list --depth=0` | session | Changes only on package.json edits |

"Session" TTL means: valid until you know the underlying source changed.

## Never-cached commands

These commands either mutate state or produce time-sensitive output. Do not
cache them, and do not trust a cached result if one somehow exists:

- `cargo build`, `cargo test`, `cargo check`
- `git commit`, `git push`, `git pull`, `git rebase`, `git merge`
- `npm install`, `npm run`, `yarn`, `pnpm`
- Any command that writes to the filesystem or network
- `curl`, `wget`, `ssh`, `rsync`
- Log tailing: `tail -f`, `journalctl -f`
- Time-sensitive queries: `date`, `uptime`, `ps`, `top`

## Staleness heuristic

If you know you ran a mutating command since the last cache entry for a given
read-only command, treat that cache entry as stale. Examples:

- You ran `git commit` → treat cached `git status` and `git log` as stale.
- You created a file → treat cached `ls` and `find` results as stale.
- You edited a file → treat cached `cat` and `grep` results for that file as
  stale.

When in doubt, prefer a fresh run over a stale cache hit.

## Storing results

After running a cacheable command and getting a useful result:

```
/cmd-cache set "<command>" "<result or key facts>"
```

For large output, store the key facts rather than the full output:
```
# Full output is 300 lines — store what matters
/cmd-cache set "find src -name '*.rs'" "42 files; key: src/main.rs, src/lib.rs, src/policy/"
```
