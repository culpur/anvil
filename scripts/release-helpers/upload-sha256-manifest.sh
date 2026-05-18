#!/usr/bin/env bash
# upload-sha256-manifest.sh — publish the combined SHA256 checksum manifest
# to AnvilHub for the just-released Anvil version.
#
# Background:
#   The /api/version endpoint advertises a `sha256_url` that points at
#   anvilhub.culpur.net/sha256/<version>.txt. Prior to v2.2.17 this file was
#   never deployed automatically — v2.2.16 shipped with the URL returning 404,
#   which then broke users who tried `sha256sum -c` after downloading a
#   binary. (Task #620.) This helper closes the gap so every release
#   publishes the manifest as part of the pipeline.
#
# Usage:
#   upload_sha256_manifest <version> <output_dir>
#
# Inputs:
#   <version>     Semver string (e.g. 2.2.17). No leading "v".
#   <output_dir>  Directory containing the seven anvil-<target>.sha256
#                 sidecar files produced by release.sh Phase 1.5.
#
# Behaviour:
#   1. Builds a combined manifest in `sha256sum -c` format.
#   2. Audits the manifest for leaked infra strings (registry, anvil-source,
#      dist/builders, dev0001, …) per feedback-public-surface-infra-redaction.
#      Hard fail if any pattern hits.
#   3. base64-pipes the manifest to dev0001 via guard:30022, writes it to
#      /opt/projects/anvilhub/packages/web/public/sha256/<version>.txt.
#   4. git commit + git push to Gitea origin (the public/ dir is checked in
#      per the AnvilHub deploy pattern).
#   5. pm2 reload anvilhub so Next.js's public/ asset cache picks up the
#      new file. (Next.js 15 production mode bakes public/ at startup;
#      without the reload the URL stays 404.)
#   6. Re-verifies the URL with cache-busting headers.
#
# Exit codes:
#   0  → manifest deployed and URL serves 200
#   1  → bad arguments or missing sidecar files
#   2  → infra-redaction grep matched (manifest withheld; nothing pushed)
#   3  → ssh/deploy step failed
#   4  → post-deploy curl did not return 200
#
# Modular per feedback-anvil-main-rs-modularity: release.sh just calls this
# helper. The helper stays under 200 lines.

set -u

upload_sha256_manifest() {
    local version="${1:-}"
    local output_dir="${2:-}"

    if [ -z "$version" ] || [ -z "$output_dir" ]; then
        echo "upload-sha256-manifest: usage: upload_sha256_manifest <version> <output_dir>" >&2
        return 1
    fi
    if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9a-zA-Z.-]+)?$'; then
        echo "upload-sha256-manifest: '$version' is not a valid semver" >&2
        return 1
    fi
    if [ ! -d "$output_dir" ]; then
        echo "upload-sha256-manifest: output_dir '$output_dir' does not exist" >&2
        return 1
    fi

    local targets=(
        "anvil-aarch64-apple-darwin"
        "anvil-x86_64-apple-darwin"
        "anvil-aarch64-unknown-linux-gnu"
        "anvil-x86_64-unknown-linux-gnu"
        "anvil-x86_64-pc-windows-gnu.exe"
        "anvil-x86_64-unknown-freebsd"
        "anvil-x86_64-unknown-netbsd"
    )

    # ─── 1. Assemble manifest from per-binary sidecars ─────────────────────
    local tmp_manifest
    tmp_manifest=$(mktemp -t anvil-sha256-XXXXXX) || return 1
    {
        printf '# Anvil v%s — SHA256 checksums\n' "$version"
        printf '# Generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '# Source: https://github.com/culpur/anvil/releases/tag/v%s\n' "$version"
        printf '#\n'
        printf '# Verify a downloaded binary:\n'
        printf '#   curl -fLO https://anvilhub.culpur.net/sha256/%s.txt\n' "$version"
        printf '#   sha256sum -c %s.txt --ignore-missing\n' "$version"
    } > "$tmp_manifest"

    local missing=0
    local t sidecar line
    for t in "${targets[@]}"; do
        sidecar="$output_dir/${t}.sha256"
        if [ ! -f "$sidecar" ]; then
            echo "  ⚠ missing sidecar: $sidecar — skipping (NetBSD Tier-3 build may have failed)" >&2
            missing=$((missing + 1))
            continue
        fi
        # Sidecar format is `<hash>  <filename>`. Take the first non-empty line.
        line=$(awk 'NF { print; exit }' "$sidecar")
        if [ -z "$line" ]; then
            echo "  ⚠ empty sidecar: $sidecar — skipping" >&2
            missing=$((missing + 1))
            continue
        fi
        printf '%s\n' "$line" >> "$tmp_manifest"
    done

    # If more than 1 target is missing (NetBSD is Tier-3 and may legitimately
    # be absent), refuse to publish a degraded manifest.
    if [ "$missing" -gt 1 ]; then
        echo "upload-sha256-manifest: $missing sidecars missing — refusing to publish a partial manifest" >&2
        rm -f "$tmp_manifest"
        return 1
    fi

    # ─── 2. Infra-redaction audit ─────────────────────────────────────────
    # Per feedback-public-surface-infra-redaction: every public surface must
    # be sanitized BEFORE publish.
    local infra_pattern='registry\.culpur|anvil-source|dist/builders|dev0001|node6f|cross-rs|rust:[0-9]\.[0-9]+-bookworm|sysroot|guard\.armored|10\.0\.|pm2'
    if grep -iE "$infra_pattern" "$tmp_manifest" >/dev/null 2>&1; then
        echo "upload-sha256-manifest: REFUSING to publish — leaked infra strings detected:" >&2
        grep -inE "$infra_pattern" "$tmp_manifest" >&2
        rm -f "$tmp_manifest"
        return 2
    fi

    echo "  ✓ Manifest assembled ($(wc -l < "$tmp_manifest" | tr -d ' ') lines, $(wc -c < "$tmp_manifest" | tr -d ' ') bytes), infra-redaction clean"

    # ─── 3. Push to dev0001 via guard bastion ─────────────────────────────
    # Two-hop SSH: local → guard.armored.ninja:30022 → dev0001.
    # Write via base64 to avoid quote-escape headaches.
    local payload
    payload=$(base64 -i "$tmp_manifest" | tr -d '\n')
    rm -f "$tmp_manifest"

    local remote_path="/opt/projects/anvilhub/packages/web/public/sha256/${version}.txt"
    local ssh_outer='ssh -o ConnectTimeout=10 -p 30022 -i '"$HOME"'/.ssh/id_ed25519_guard soulofall@guard.armored.ninja'

    # 3a. mkdir + write + chmod
    if ! $ssh_outer "ssh -o ConnectTimeout=10 dev0001 \"mkdir -p /opt/projects/anvilhub/packages/web/public/sha256 && echo $payload | base64 -d > $remote_path && chmod 644 $remote_path\"" >/dev/null 2>&1; then
        echo "upload-sha256-manifest: failed to write $remote_path on dev0001" >&2
        return 3
    fi
    echo "  ✓ Wrote $remote_path"

    # 3b. git commit + push (file is tracked per AnvilHub deploy pattern)
    local commit_cmd="cd /opt/projects/anvilhub && \
        git add packages/web/public/sha256/${version}.txt && \
        git diff --cached --quiet && echo NO_CHANGES || \
        (git -c user.email=releases@culpur.net -c user.name=anvil-release \
            commit -m 'sha256: publish v${version} checksum manifest' && \
         git push origin HEAD)"
    if ! $ssh_outer "ssh -o ConnectTimeout=10 dev0001 \"$commit_cmd\"" >/dev/null 2>&1; then
        echo "  ⚠ git commit/push on dev0001 reported non-zero — file is on disk, but not committed" >&2
        # Non-fatal; the file IS deployed and serving. Operator can reconcile.
    else
        echo "  ✓ git commit + push to Gitea origin"
    fi

    # 3c. pm2 reload — Next.js 15 production mode bakes public/ at startup.
    # Without this reload the URL keeps returning 404 even though the file
    # is on disk. (Discovered #620.)
    if ! $ssh_outer "ssh -o ConnectTimeout=10 dev0001 'sudo -i pm2 reload anvilhub'" >/dev/null 2>&1; then
        echo "upload-sha256-manifest: pm2 reload anvilhub failed — URL will remain 404" >&2
        return 3
    fi
    echo "  ✓ pm2 reload anvilhub"

    # ─── 4. Verify URL serves 200 ─────────────────────────────────────────
    local url="https://anvilhub.culpur.net/sha256/${version}.txt"
    local http_code
    http_code=$(curl -sL --max-time 15 -H 'Cache-Control: no-cache' -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || echo "000")
    if [ "$http_code" != "200" ]; then
        echo "upload-sha256-manifest: URL $url returned HTTP $http_code (expected 200)" >&2
        return 4
    fi
    echo "  ✓ $url → 200"
    return 0
}

# Allow direct CLI invocation for ad-hoc backfill (e.g. v2.2.16 #620 itself).
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    if [ "$#" -lt 2 ]; then
        cat <<USAGE >&2
Usage: $0 <version> <output_dir>

Publishes <output_dir>/anvil-*.sha256 sidecars as a combined manifest at
https://anvilhub.culpur.net/sha256/<version>.txt.

Example:
  $0 2.2.16 /tmp/v2.2.16-shas
USAGE
        exit 1
    fi
    upload_sha256_manifest "$1" "$2"
    exit $?
fi
