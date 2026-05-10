#!/usr/bin/env node
//
// scripts/release-helpers/changelog-prepend.mjs
//
// Prepends a new entry to a structured changelog.json file. Used by
// release.sh (T1-D) to update the AnvilHub /about page changelog without
// risking the find-replace mangling that took down our public README in
// incident #399.
//
// Schema (changelog.json):
//   [
//     { version: "2.2.12", date: "2026-05-12", headline: "...",
//       items: ["✓ feature 1", "✓ feature 2"] },
//     { version: "2.2.11", date: "2026-05-09", ... },
//     ...
//   ]
//
// Newest entries first. The file is the single source of truth — the
// AnvilHub /about page should import it at render time, not parse the
// rendered HTML.
//
// Usage:
//   node changelog-prepend.mjs --file changelog.json --version 2.2.12 \
//        --date 2026-05-12 --headline "..." --items "feat: x" --items "fix: y"
//
//   # Idempotent: refuses to add a duplicate version. Use --force to
//   # overwrite an existing entry (e.g. revising an unreleased changelog).
//
// Exit codes:
//   0  — prepended (or, with --force, replaced)
//   1  — invalid args
//   2  — duplicate version (without --force)
//   3  — file read/write or JSON parse error

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { argv, exit, stderr } from 'node:process';

function parseArgs() {
  const args = { items: [] };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--file') args.file = argv[++i];
    else if (a === '--version') args.version = argv[++i];
    else if (a === '--date') args.date = argv[++i];
    else if (a === '--headline') args.headline = argv[++i];
    else if (a === '--items') args.items.push(argv[++i]);
    else if (a === '--force') args.force = true;
    else if (a === '--help' || a === '-h') {
      console.log('usage: changelog-prepend.mjs --file F --version V --date YYYY-MM-DD --headline H [--items I...] [--force]');
      exit(0);
    } else {
      stderr.write(`unknown arg: ${a}\n`);
      exit(1);
    }
  }
  for (const k of ['file', 'version', 'date', 'headline']) {
    if (!args[k]) {
      stderr.write(`missing required arg: --${k}\n`);
      exit(1);
    }
  }
  if (!/^\d{4}-\d{2}-\d{2}$/.test(args.date)) {
    stderr.write(`--date must be YYYY-MM-DD (got ${JSON.stringify(args.date)})\n`);
    exit(1);
  }
  if (!/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(args.version)) {
    stderr.write(`--version must be semver-ish (got ${JSON.stringify(args.version)})\n`);
    exit(1);
  }
  return args;
}

function loadChangelog(path) {
  if (!existsSync(path)) return [];
  let raw;
  try { raw = readFileSync(path, 'utf8'); } catch (e) {
    stderr.write(`could not read ${path}: ${e.message}\n`);
    exit(3);
  }
  if (!raw.trim()) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      stderr.write(`${path} is not a JSON array\n`);
      exit(3);
    }
    return parsed;
  } catch (e) {
    stderr.write(`could not parse ${path} as JSON: ${e.message}\n`);
    exit(3);
  }
}

function main() {
  const args = parseArgs();
  const log = loadChangelog(args.file);

  const idx = log.findIndex(e => e.version === args.version);
  if (idx !== -1 && !args.force) {
    stderr.write(`version ${args.version} already exists (use --force to overwrite)\n`);
    exit(2);
  }

  const entry = {
    version: args.version,
    date: args.date,
    headline: args.headline,
    items: args.items,
  };

  let newLog;
  if (idx === -1) {
    // Prepend
    newLog = [entry, ...log];
  } else {
    // Replace in place
    newLog = [...log];
    newLog[idx] = entry;
  }

  // Pretty-print 2-space indent so diffs are reviewable
  const out = JSON.stringify(newLog, null, 2) + '\n';
  try {
    writeFileSync(args.file, out, 'utf8');
  } catch (e) {
    stderr.write(`could not write ${args.file}: ${e.message}\n`);
    exit(3);
  }
  console.log(idx === -1 ? `✓ prepended ${args.version}` : `✓ replaced ${args.version} (--force)`);
}

main();
