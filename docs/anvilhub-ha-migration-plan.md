# AnvilHub HA Migration Plan
# Recon date: 2026-05-20 | Author: Maverick (DevOps)
# Status: PLAN ONLY — no infrastructure changes made

---

## 1. Executive Summary

- **The move**: AnvilHub production is currently a single Next.js process running on dev0001 (10.0.70.80:3100), fronted by a bare ProxyPass vhost on guard. The goal is to replicate the passage-prod pattern: deploy anvilhub onto f0+f1+f2, expose it behind an Apache balancer at anvilhub.culpur.net, and sever dev0001 from prod traffic.
- **DB is a clean-slate migration**: The prod AnvilHub database (`anvilhub` on dev0001 localhost Postgres) has **zero rows in all 5 tables** and is only 8 MB. No logical replication needed — create fresh DB on Patroni, run `prisma migrate deploy`, done.
- **CT 113 discrepancy**: The `.anvil-release.toml` and MCP describe pm2 running inside CT 113 (LXC on Proxmox 188.40.211.105), but the Apache vhost currently points to dev0001:3100 and dev0001 is actually serving the process (`next-server v15.2.4` running as root PID, not via pm2). CT 113's actual role is unclear from this recon — it may be orphaned or serve a different role. **Requires investigation before Phase A.**
- **What is NOT changing**: prod Passage on f0+f1+f2 is untouched. dev0001 continues to serve dev AnvilHub and all other dev services. DNS (Cloudflare → guard) does not change.
- **Static files (install.sh, sha256/*.txt)**: Served by Next.js from `/public` on each node — no NFS or MinIO required for these. MinIO `anvilhub-source` bucket is for F1/F2 source archives, not for the web server static files.

---

## 2. Current State Inventory

### 2.1 Source tree

- **Location on dev0001**: `/opt/projects/anvilhub/` (monorepo root)
- **Next.js app**: `/opt/projects/anvilhub/packages/web/` (the only workspace)
- **Gitea remote** (primary): `https://registry.culpur.net/git/culpur/anvilhub-web.git`
- **GitHub mirror**: `https://github.com/culpur/anvilhub-web.git`
- **Git HEAD**: `fatal: Failed to resolve HEAD as a valid ref` — the dev0001 checkout has no commits (files were likely dropped in without a git init, or the working tree is ahead of an empty repo). **The source of truth is the gitea remote, not the dev0001 checkout.**

### 2.2 Process / runtime

- **pm2 list on dev0001**: EMPTY. No pm2-registered process.
- **Actual process**: `next-server v15.2.4` running as root (not via pm2), listening on `*:3100`. Started manually or by a non-pm2 mechanism.
- **ecosystem.config.cjs** (`/opt/projects/anvilhub/ecosystem.config.cjs`):
  - `name: "anvilhub"`, `script: ".../node_modules/.bin/next"`, `args: "start -p 3100"`
  - `cwd: "/opt/projects/anvilhub/packages/web"`
  - `instances: 1`, `exec_mode: fork` (default — not cluster)
  - `max_memory_restart: "512M"`

### 2.3 Apache / routing

- **Guard vhost** (`/etc/apache2/sites-enabled/anvilhub.culpur.net.conf`):
  - HTTP → HTTPS redirect
  - `ProxyPass / http://10.0.70.80:3100/` (dev0001 direct, no balancer)
  - No `ProxyPreserveHost` in current conf means Host header may pass through
  - Missing: `RequestHeader set X-Forwarded-For`, ModSecurity, security headers, h2 protocol, per-route SecRule exclusions (all present in passage-prod.conf)
- **Guard2**: Vhost not confirmed reachable, but single-guard setup is a pre-existing risk not introduced by this migration.
- **DNS**: `anvilhub.culpur.net` resolves to Cloudflare IPs `188.114.96.3` / `188.114.97.3` (proxied). Cloudflare → guard. DNS does not need to change for the migration.
- **Guard local /etc/hosts**: `anvilhub.culpur.net` → `10.0.70.5` (guard itself). This is the hairpin for inter-service calls originating from guard.
- **passage-prod.conf**: Already staged and enabled at `/etc/apache2/sites-enabled/passage-prod.conf` with f0:3010 / f1:3010 / f2:3010 balancer, `lbmethod=byrequests`, `failonstatus=500,502,503`. This is the exact template to clone for anvilhub.

### 2.4 Database

- **Engine**: PostgreSQL on dev0001 localhost (no Patroni, no HAProxy)
- **Database name**: `anvilhub`
- **Role**: `anvilhub` (owner of all tables)
- **Schema**: 5 tables — `User`, `Package`, `PackageVersion`, `Review`, `ApiKey` (Prisma-managed, camelCase table names = Prisma default)
- **Data**: **0 rows in all tables**, 8 MB total. Clean-slate migration — no pg_dump needed.
- **Migrations**: Prisma schema present at `packages/web/prisma/schema.prisma`. Seed scripts: `seed.ts` + `seed-marketplace.ts`. No Prisma migration history folder was found (`prisma/migrations/` absent), meaning schema was applied via `prisma db push` not `prisma migrate`. Need to confirm before Phase A.
- **Patroni on f0/f1/f2**: Running, HAProxy at `127.0.0.1:5000` on each node (rw leader routing). No `anvilhub` database exists on the cluster yet.

### 2.5 Environment / secrets

- **`.env` location**: `/opt/projects/anvilhub/packages/web/.env`
- **Keys present** (values redacted, NOT in this document):
  - `DATABASE_URL` — currently `postgresql://anvilhub:<pass>@localhost/<db>`
  - `NEXT_PUBLIC_APP_URL`
  - `NEXT_PUBLIC_API_URL`
  - `AUTHENTIK_CLIENT_ID`
  - `AUTHENTIK_CLIENT_SECRET`
  - `NEXTAUTH_URL` — currently set to `https://anvilhub.culpur.net` (inferred from context)
  - `NEXTAUTH_SECRET`
- **Vault status**: Secrets appear to be in `.env` only. No vault entry confirmed for anvilhub secrets. **Must be vaulted before Phase A.**

### 2.6 Next.js config

- **`next.config.ts`**: `experimental.serverActions.allowedOrigins: ["localhost:3100", "anvilhub.culpur.net"]`
  - `localhost:3100` is a **hardcoded localhost reference** that must be updated when deploying to f0/f1/f2. The correct value for HA deployment is just `["anvilhub.culpur.net"]` — localhost is irrelevant when the server is not the origin.
- **`middleware.ts`**: Sets Cloudflare cache headers (`public, s-maxage=60, stale-while-revalidate=300`) for DB-backed paths. No localhost dependency. Compatible with multi-node deployment.
- **Next.js standalone build**: NOT configured — no `output: "standalone"` in next.config.ts. Each node needs `npm ci` + `next build` during deploy.

### 2.7 Static assets and install infrastructure

- **Served by**: Next.js from `/public` on the running node (standard Next.js static file serving)
- **Files in `/public`**: `install.sh`, `install.ps1`, `install.cmd`, `favicon.ico`, `logo.png`, hero images, `/sha256/` folder (contains per-version `.txt` files), `/releases/` folder
- **MinIO `anvilhub-source` bucket**: Per memory `anvilhub-source-minio.md`, this is for Anvil release source archives (F1/F2 format), accessed via `anvilhub-source-writer` IAM. It is NOT used for serving static web assets. No MinIO dependency for this migration.
- **Cloudflare caching**: The middleware sets `s-maxage=60` on DB-backed routes. Static assets under `/_next/static/` get long-lived cache headers from Next.js automatically. Post-cutover: trigger a Cloudflare cache purge to flush stale origin references.

### 2.8 Authentication / Authentik

- **Provider**: Authentik OIDC at `https://login.culpur.net/application/o/anvilhub/`
- **NextAuth**: Using `next-auth` v4 with a custom OAuth provider pointing at Authentik well-known URL
- **NEXTAUTH_URL**: Must be set to `https://anvilhub.culpur.net` on f0/f1/f2 (no change to value, but each node needs it in `.env`)
- **Authentik application**: App slug is `anvilhub`. No redirect URI change needed since the public hostname stays `anvilhub.culpur.net`. **Verify Authentik app's allowed redirect URIs include `https://anvilhub.culpur.net/api/auth/callback/authentik` before Phase A.**
- **NextAuth session cookies**: Shared via `NEXTAUTH_SECRET`. In cluster mode, all nodes must use the **same** `NEXTAUTH_SECRET` value. Deploy `.env` with identical `NEXTAUTH_SECRET` to all three nodes.

### 2.9 Background jobs / scheduled tasks

- **Cron**: No anvilhub cron entries found (checked `crontab -l` and `sudo crontab -l`).
- **BullMQ/queues**: No BullMQ, Bull, or queue imports found in source. False positives on grep were page content text.
- **Conclusion**: No stateful background workers. Safe for multi-replica deployment without a job deduplication layer.

### 2.10 CT 113 / architecture discrepancy

- **`.anvil-release.toml`** says: `anvilhub_pm2_proxmox_host = "188.40.211.105"`, `anvilhub_pm2_container_id = "113"`. PM2 ops for releases go via `bastion → root@188.40.211.105 → pct exec 113 → pm2`.
- **`/etc/apache2/sites-enabled/anvilhub.culpur.net.conf`** says: `ProxyPass / http://10.0.70.80:3100/` (dev0001, not CT 113).
- **dev0001 port 3100**: IS open and serving (`next-server v15.2.4` running as root).
- **Interpretation**: The Apache vhost was never updated to point at CT 113. Either (a) CT 113 was provisioned but traffic was never cut over and dev0001 remains the actual prod, or (b) CT 113 holds a copy that gets pm2-restarted on release but traffic never reaches it. **CT 113 is not receiving any production traffic at this time.** Dev0001 is the actual prod origin.
- **CT 113 does NOT appear in the LAN hosts file** (10.0.70.x range). CT 113 may be on a separate Proxmox bridge or NAT. Cannot confirm without Proxmox console access.
- **Proxmox host** `188.40.211.105` is `node0001.culpur.net` per guard's hosts file. SSH to it is not possible via guard's current key setup (connection refused during recon).

### 2.11 f0 / f1 capacity

| Node | IP | Free Disk | Free RAM | Current Services |
|------|-----|-----------|----------|-----------------|
| f0 | 10.0.70.6 | 256 GB / 300 GB | 7.8 GB / 19 GB | passage (4-cluster), passage-ws (fork), Patroni/PG, HAProxy |
| f1 | 10.0.70.7 | 820 GB / 850 GB | 16 GB / 19 GB | passage (4-cluster), passage-ws, Patroni, HAProxy, thehive, babel-v2 |
| f2 | 10.0.70.8 | not reached | not reached | Patroni, passage (assumed per balancer config) |

- f0 is tighter on disk (256 GB free) but has 7.8 GB free RAM — adequate for a 512 MB Next.js process.
- f1 is spacious. f2 unreachable via guard SSH (task #693 noted port drift).

### 2.12 Anvil release pipeline references to CT 113

Files that reference CT 113 and must be updated after migration:

| File | Reference |
|------|-----------|
| `/Users/soulofall/projects/anvil-dev/.anvil-release.toml` | `anvilhub_pm2_proxmox_host`, `anvilhub_pm2_container_id`, `anvilhub_pm2_home` |
| `/Users/soulofall/projects/mcp-servers/anvil-release/lib/helpers.js` | `ANVILHUB_PM2_PROXMOX_HOST = '188.40.211.105'`, `ANVILHUB_PM2_CONTAINER_ID = '113'`, `ANVILHUB_PM2_HOME = '/root/.pm2'` (lines 30-32) |
| `/Users/soulofall/projects/mcp-servers/anvil-release/lib/tools/update-pages.js` | All `pm2ProxmoxHost` / `pm2ContainerId` / `pm2Home` usage (lines 379-383, 665, 699, 704) |

After migration, pm2 ops will SSH directly to `soulofall@f0.armored.ninja` (or `node6f0`) port 22 and run `pm2 restart anvilhub` — no more `pct exec`. The `.anvil-release.toml` `[targets]` section will need new keys (or removal of the proxmox keys in favor of a direct SSH host).

### 2.13 Dev subdomain

- **`dev-api.culpur.net`** already exists, routing to dev0001:3099 (dev Passage). No `dev-anvilhub.culpur.net` vhost exists on guard.
- Dev AnvilHub currently has no separate subdomain — it shares `anvilhub.culpur.net` with prod. **Creating `dev-anvilhub.culpur.net` is a Phase A prereq** to enable dev/prod separation before cutover.

---

## 3. Target Architecture (ASCII)

```
Internet
    │
    ▼
Cloudflare  (anvilhub.culpur.net → proxied)
    │
    ▼
guard / guard2  (10.0.70.5 / 10.0.70.4)
│
│   /etc/apache2/sites-enabled/anvilhub-prod.conf
│   <Proxy balancer://anvilhub-prod>
│     BalancerMember http://10.0.70.6:3200   route=f0   status=+H (Phase B, drain)
│     BalancerMember http://10.0.70.7:3200   route=f1   status=+H
│     BalancerMember http://10.0.70.8:3200   route=f2   status=+H (optional)
│   </Proxy>
│
├─── f0 (10.0.70.6) :3200  ──→ pm2 "anvilhub" (fork, 1 instance)
│                                 │
├─── f1 (10.0.70.7) :3200  ──→  pm2 "anvilhub" (fork, 1 instance)
│                                 │
└─── f2 (10.0.70.8) :3200  ──→  pm2 "anvilhub" (fork, 1 instance) [optional]
                                  │
                        Patroni HA Postgres
                        127.0.0.1:5000 (HAProxy, RW leader)
                        DB: anvilhub_prod
                        Role: anvilhub_prod

dev0001 (10.0.70.80) :3100  ─── dev-anvilhub.culpur.net
                                 DB: anvilhub (local PG)
                                 [source tree: /opt/projects/anvilhub]
                                 [git pull + npx next build on deploy]

CT 113 on node0001 (188.40.211.105)
  → DECOM or repurpose after migration
  → Release MCP no longer routes through pct exec

MinIO (s3.culpur.net)
  anvilhub-source bucket  → F1/F2 source archives (unchanged, not web-served)
```

**Port selection**: Use **:3200** for AnvilHub on f0/f1/f2 to avoid conflict with Passage :3010 and relay :8081.

---

## 4. Phased Migration Plan

### Phase A — Prerequisites (1–2 days)

**A.1 — Resolve CT 113 discrepancy**
- SSH to node0001 (188.40.211.105) as root and run `pct exec 113 -- bash -c "pm2 list; ss -tlnp | grep 3100"` to determine CT 113's actual state.
- If CT 113 has a running anvilhub process: document what pm2 release steps currently push to vs what Apache actually serves.
- Decision: If CT 113 is orphaned (no traffic), mark DECOM-pending. If it IS serving prod somehow (via a different path not visible in Apache config), update the migration plan before proceeding.

**A.2 — Vault anvilhub secrets**
- Add all `.env` values from `/opt/projects/anvilhub/packages/web/.env` to vault with key prefix `anvilhub/prod/`.
- Values needed on f0/f1/f2 `.env`: `DATABASE_URL` (new Patroni URL), `NEXT_PUBLIC_APP_URL`, `NEXT_PUBLIC_API_URL`, `AUTHENTIK_CLIENT_ID`, `AUTHENTIK_CLIENT_SECRET`, `NEXTAUTH_URL`, `NEXTAUTH_SECRET`.

**A.3 — Provision `anvilhub_prod` database on Patroni**
- Connect to Patroni leader (HAProxy :5000 on any fX node):
  ```sql
  CREATE ROLE anvilhub_prod WITH LOGIN PASSWORD '<<generate>>';
  CREATE DATABASE anvilhub_prod OWNER anvilhub_prod;
  ```
- Run `prisma migrate deploy` (or `prisma db push` if no migrations folder) from dev0001 pointing at new DB URL to apply schema.
- Confirm all 5 tables created with correct ownership.

**A.4 — Verify Authentik redirect URIs**
- In Authentik admin at `login.culpur.net`: open application `anvilhub`, confirm allowed redirect URIs include `https://anvilhub.culpur.net/api/auth/callback/authentik`. No change needed if already present.
- Add `https://dev-anvilhub.culpur.net/api/auth/callback/authentik` for the dev instance.

**A.5 — Create `dev-anvilhub.culpur.net` vhost and DNS**
- Add Cloudflare DNS A record `dev-anvilhub.culpur.net` → guard IP (proxied), or use existing guard catch-all.
- Create `/etc/apache2/sites-enabled/dev-anvilhub.culpur.net.conf` on guard mirroring the existing `anvilhub.culpur.net.conf` but pointing to `http://10.0.70.80:3100/`.
- Current `anvilhub.culpur.net.conf` will temporarily remain pointing at dev0001 until Phase C cutover.

**A.6 — Write anvilhub_app Puppet module (optional but recommended)**
- Clone `passage_app/manifests/init.pp` structure as `anvilhub_app/manifests/init.pp`.
- Key diffs from passage_app: port 3200 (not 3010), `cwd: /opt/anvilhub/packages/web`, `script: .../node_modules/.bin/next`, `args: start -p 3200`, single fork instance (not 4-cluster), no relay-server app, env file at `/opt/anvilhub/packages/web/.env`.
- If Puppet is not yet reachable, do manual deploy in Phase B and codify Puppet post-migration.

---

### Phase B — Parallel Deploy to f0+f1+f2 (1 day)

All steps with balancer `status=D` (drain/disabled) — zero traffic reaches the new instances during this phase.

**B.1 — Clone source tree to f0/f1/f2**
```bash
ssh f0 "git clone https://registry.culpur.net/git/culpur/anvilhub-web.git /opt/anvilhub"
ssh f1 "git clone https://registry.culpur.net/git/culpur/anvilhub-web.git /opt/anvilhub"
ssh f2 "git clone https://registry.culpur.net/git/culpur/anvilhub-web.git /opt/anvilhub"
```

**B.2 — Install deps and build on each node**
```bash
ssh fX "cd /opt/anvilhub && npm ci && npm run build"
```
Build must succeed on all three nodes before proceeding.

**B.3 — Write `.env` on each node** (from vault, not copy from dev0001)
- `DATABASE_URL` → `postgresql://anvilhub_prod:<pass>@127.0.0.1:5000/anvilhub_prod`
- `NEXTAUTH_URL` → `https://anvilhub.culpur.net`
- All other vars same values, same NEXTAUTH_SECRET across all nodes.
- Update `next.config.ts` `allowedOrigins` to remove `localhost:3100` before building: value should be `["anvilhub.culpur.net"]` only.

**B.4 — Start pm2 on each node**
```bash
ssh fX "cd /opt/anvilhub && pm2 start ecosystem.config.cjs && pm2 save"
```
Verify `ss -tlnp | grep 3200` is LISTEN on each node.

**B.5 — Smoke test each node directly**
```bash
# From guard:
curl -sk http://10.0.70.6:3200/api/version | jq .latest_version
curl -sk http://10.0.70.7:3200/api/version | jq .latest_version
curl -sk http://10.0.70.8:3200/api/version | jq .latest_version
```
Expected: `"2.2.17"` from all three.

**B.6 — Add anvilhub-prod balancer vhost (disabled, no traffic yet)**
Create `/etc/apache2/sites-enabled/anvilhub-prod.conf` on guard modelled on `passage-prod.conf`:
- Port 3200 instead of 3010
- ServerName `anvilhub.culpur.net`
- `status=D` on all BalancerMembers (disabled) initially
- Include full security header set and ModSecurity rules from passage-prod template
- Do NOT enable the vhost or reload Apache yet.

---

### Phase C — Cutover (30 minutes, reversible)

**C.1 — Pre-cutover checklist**
- [ ] All three fX nodes serving correct `/api/version` on :3200
- [ ] Patroni DB healthy (`SHOW synchronous_standby_names;` returns expected)
- [ ] Authentik redirect URIs confirmed
- [ ] Cloudflare "Under Attack" mode OFF (would cache stale responses)
- [ ] Bastion→dev0001 SSH tunnel confirmed live (rollback path)

**C.2 — Enable balancer members one at a time**
On guard, edit `anvilhub-prod.conf`, remove `status=D` from f0 only:
```apache
BalancerMember http://10.0.70.6:3200 route=f0 retry=5 timeout=90 ...
```
Reload Apache: `sudo apache2ctl graceful`.
Test `https://anvilhub.culpur.net` — confirm it loads, DB-backed pages work, login flow works.

**C.3 — Enable f1, then f2**
Same as C.2 for f1 and f2. Monitor error logs: `sudo tail -f /var/log/apache2/error.log`.

**C.4 — Disable dev0001 member in old vhost**
Edit `anvilhub.culpur.net.conf` to redirect to the balancer instead, or simply disable the old vhost:
```bash
sudo a2dissite anvilhub.culpur.net.conf && sudo apache2ctl graceful
```

**C.5 — Purge Cloudflare cache**
Purge everything for `anvilhub.culpur.net` via CF dashboard or API. Verify CDN edge serves fresh content from new origin.

**C.6 — Monitor for 1 hour**
- Watch Apache access logs for 5xx rate
- Watch pm2 logs on all fX nodes: `pm2 logs anvilhub --lines 100`
- Watch Patroni DB: `SELECT count(*), now() FROM "User";` every few minutes

**Rollback at any point during Phase C**:
Re-enable `anvilhub.culpur.net.conf` pointing to dev0001:3100 and `a2dissite anvilhub-prod.conf`. Takes < 60 seconds.

---

### Phase D — Decom dev0001 AnvilHub and CT 113 (1 day, after 48-hour soak)

**D.1 — Stop dev0001 next-server process**
```bash
# After 48h soak with no traffic to dev0001:3100
kill $(ss -tlnp sport 3100 | grep -oP 'pid=\K[0-9]+')
```

**D.2 — Decom CT 113**
- SSH to node0001 Proxmox host: `pct stop 113 && pct destroy 113` (requires Proxmox console access or direct SSH).
- Verify no other services depended on CT 113 (recon could not confirm — check before destroying).

**D.3 — Update release pipeline**
Update `.anvil-release.toml` and anvil-release MCP:
- Remove `anvilhub_pm2_proxmox_host`, `anvilhub_pm2_container_id`, `anvilhub_pm2_home` from `.anvil-release.toml`.
- Add `anvilhub_pm2_host = "node6f0"` (or `10.0.70.6`).
- Update `lib/helpers.js`: replace `runPm2InContainer()` with `runPm2Direct()` — direct SSH to `soulofall@node6f0` port 22.
- Update `lib/tools/update-pages.js`: remove all `pm2ProxmoxHost` / `pm2ContainerId` / `pct exec` paths.
- Run `npm test` in anvil-release MCP to verify suite passes.

**D.4 — Update ecosystem.config.cjs for fX nodes**
Change port from 3100 → 3200 (already done in B.3, but confirm the committed ecosystem file is updated in git).

---

### Phase E — dev0001 Reconfigured as Dev-Only

**E.1 — Rename/update dev0001 AnvilHub process**
- dev0001's anvilhub process continues on :3100 as dev.
- `dev-anvilhub.culpur.net` now routes to dev0001:3100 (A.5 already done this).

**E.2 — Update CI/CD deploy targets** (when pipeline is built)
- Dev deploys: push to `main` → dev0001 `git pull && npm run build && pm2 restart anvilhub`.
- Prod deploys: tag `vX.Y.Z` → manual gate → f0/f1/f2 `git pull && npm run build && pm2 restart anvilhub`.

**E.3 — Commit updated deployment docs**
- Update any references to `10.0.70.80:3100` as prod in runbooks.

---

## 5. Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|-----------|
| CT 113 discrepancy — unknown state | High | High | Resolve in Phase A.1 before any work begins. If CT 113 is somehow serving prod traffic via an undiscovered path, cutover plan is wrong. |
| Next.js `allowedOrigins` misconfiguration causes Server Actions to fail | Medium | High | Update `next.config.ts` before build in B.3. Test Server Actions (publish, login) on each node in B.5. |
| Patroni leader election during cutover causes DB writes to fail | Low | High | Cutover during low-traffic window. Patroni HAProxy at :5000 handles failover transparently. Test :5000 connectivity from fX before Phase C. |
| NEXTAUTH_SECRET mismatch between nodes invalidates sessions | Medium | Medium | All nodes MUST share identical `NEXTAUTH_SECRET`. Deploy from vault, not copy-paste. Verify by logging in through balancer and testing session persistence. |
| Cloudflare caches stale API responses post-cutover | Medium | Medium | Purge CF cache (C.5) immediately after cutover. Middleware already sets `s-maxage=60` so worst case is 60s staleness. |
| f2 (10.0.70.8) unreachable via SSH from guard | Confirmed | Low (optional node) | Phase B/C can proceed with f0+f1 only. f2 added after task #693 port restoration is confirmed. |
| Prisma schema applied via `db push` (not migrations) — no migration history | High | Medium | Run `prisma migrate dev --name init` on dev0001 to generate migration history before Phase A.3, then use `prisma migrate deploy` on Patroni. |
| Anvil CLI update_check hits `anvilhub.culpur.net/api/version` during cutover window | Low | Low | API is stateless and served from Next.js. 60s CF cache means clients get slightly stale version during drain — acceptable. |
| Source archive install scripts (`install.sh`) served from `/public` need to work on all nodes | Low | High | Files are in git, built into each node's `public/` — no shared filesystem needed. Verify sha256 txt files are present post-build on each node. |
| Authentik OIDC callback fails if redirect URI not added | Medium | High | Verify in Phase A.4. Authentik app slug is `anvilhub`; admin at login.culpur.net. |

---

## 6. Open Questions (max 5 — must answer before Phase A)

**Q1 (BLOCKER): What is CT 113's actual current role?**
The `.anvil-release.toml` and MCP route pm2 restarts via `pct exec 113`, but Apache on guard sends traffic to dev0001:3100, which IS actively serving. Are both running, and which is prod? Can you SSH to Proxmox host 188.40.211.105 as root from your terminal and run `pct exec 113 -- bash -c "pm2 list; ss -tlnp"`?

**Q2 (BLOCKER): Prisma migration vs db push — does a `prisma/migrations/` folder exist?**
The recon found `schema.prisma` and seed scripts but no `migrations/` subfolder in `/opt/projects/anvilhub/packages/web/prisma/`. If schema was applied with `db push`, there is no migration history to replay on the Patroni DB — we'd need to run `prisma migrate dev --name init` first. Confirm which command was used to set up the schema.

**Q3: Should f2 (10.0.70.8) be included in the initial deployment?**
f2 is the third Patroni node and is in the passage-prod balancer, but SSH to it from guard is broken (task #693). If task #693 is resolved before Phase B, include f2. If not, deploy f0+f1 first and add f2 in a follow-up. What is the current status of task #693?

**Q4: What port should AnvilHub use on f0/f1/f2?**
This plan proposes **:3200** to avoid conflicts with Passage (:3010) and the relay (:8081). Confirm there are no other services on f0/f1 that use :3200 (recon showed only :3010 and :5000 in use).

**Q5: Standalone build or full monorepo deploy?**
Current setup uses a full monorepo with `node_modules` on dev0001. For f0/f1/f2, we can either (a) clone the monorepo and run `npm ci + next build` on each node per deploy, or (b) configure `output: "standalone"` in next.config.ts and copy only the standalone artifact. Option (a) is simpler and mirrors the dev0001 pattern. Option (b) is faster per-deploy and reduces the artifact on each node. Which approach do you prefer?

---

## 7. Time Estimates

| Phase | Elapsed Time | Notes |
|-------|-------------|-------|
| Phase A — Prerequisites | 1–2 days | Vault work + DB provisioning + DNS. Blocked on Q1 and Q2. |
| Phase B — Parallel Deploy | 4–6 hours | Clone + build + pm2 + smoke test. Depends on f0/f1 `npm ci` speed. |
| Phase C — Cutover | 30 minutes | Rolling balancer weight flip. Low risk if B fully validated. |
| Phase D — Decom CT 113 + pipeline update | 2–4 hours | Requires Proxmox console for CT destroy. MCP update is mechanical. |
| Phase E — Dev-only cleanup | 1 hour | Vhost + DNS + docs. |
| **Total** | **2–4 days** | Assumes no blockers beyond Q1/Q2. |

---

*Recon performed: 2026-05-20. No infrastructure changes were made. All findings are read-only observations.*
*Next step: answer Open Questions Q1 and Q2 before Phase A begins.*
