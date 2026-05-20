# AnvilHub Production HA Migration Plan
**Author:** Maverick | **Date:** 2026-05-20 | **Status:** PLAN — supersedes `anvilhub-ha-migration-plan.md`

This is the hardened **production-grade HA** plan. The earlier recon doc (`anvilhub-ha-migration-plan.md`) describes the topology move. THIS doc adds the HA discipline: load-balancing, health-checks, self-healing, zero-downtime deploys, observability, and disaster recovery.

---

## 0. Goal (north star)

| Surface | Before | After |
|---|---|---|
| **Prod URL** | anvilhub.culpur.net → dev0001 (single Next.js fork, no pm2) | anvilhub.culpur.net → guard+guard2 ↔ f0+f1+f2 balanced, health-checked, self-healing |
| **DB** | local Postgres on dev0001 | Patroni HA cluster (f0/f1/f2) via HAProxy :5000 (rw) |
| **Failure mode** | dev0001 down = full outage | Single-node failure = automatic balancer drain, no user impact |
| **Deploy** | manual `git pull && pm2 restart` | rolling deploy: 1 node at a time, balancer drains, health-gate, next |
| **Observability** | none | health endpoint + apache balancer-manager + Wazuh alerts + Grafana |
| **Recovery** | manual restart | systemd auto-restart, pm2 cluster respawn, Puppet drift correction |
| **Dev** | shared `anvilhub.culpur.net` with prod | separate `dev-anvilhub.culpur.net` → dev0001 |

---

## 1. HA Architecture — full picture

```
                    Internet
                       │
                       ▼
              Cloudflare (CDN + WAF)
                       │
                       ▼
        ┌──────────────┴──────────────┐
        │                             │
   guard (10.0.70.5)            guard2 (10.0.70.4)
   Apache + ModSecurity         Apache + ModSecurity (replica)
   keepalived VIP (active)      keepalived VIP (standby)
        │                             │
        └──────────────┬──────────────┘
                       │
        Apache mod_proxy_balancer
        balancer://anvilhub-prod
         lbmethod=byrequests
         failonstatus=500,502,503,504
         retry=5
         disablereuse=Off
                       │
        ┌──────────────┼──────────────┐
        │              │              │
        ▼              ▼              ▼
   f0:3200         f1:3200        f2:3200
   route=f0        route=f1       route=f2
   status=+H       status=+H      status=+H
   ping-test ✓    ping-test ✓    ping-test ✓

  Each node:
   ┌──────────────────────────────────────┐
   │ systemd: pm2-anvilhub.service        │
   │  └─ pm2 god daemon (passage user)    │
   │      └─ ecosystem: 2 cluster workers │
   │          └─ Next.js standalone build │
   │              └─ port 3200            │
   │                                       │
   │ /health endpoint (HTTP 200 = ready)  │
   │ /api/ready endpoint (DB reachable?)  │
   │                                       │
   │ patroni-haproxy 127.0.0.1:5000 (rw)  │
   │ patroni-haproxy 127.0.0.1:5001 (ro)  │
   │  └─ Postgres cluster: anvilhub_prod  │
   └──────────────────────────────────────┘

  Self-healing layers:
    L1: systemd Restart=on-failure  (process crash → auto-restart)
    L2: pm2 cluster mode             (worker crash → another spawns)
    L3: Apache failonstatus drain    (bad responses → out of pool)
    L4: Puppet apply (every 30 min)  (drift → reconciled)
    L5: Wazuh + Matrix alert         (sustained failure → page operator)
```

---

## 2. What's NEW vs the earlier plan

The earlier plan got the topology right. This plan **adds**:

### 2.1 Real HA, not just load-balancing
- **pm2 cluster mode (2 workers)** per node, not single-fork. A worker crash respawns instantly without taking the node out of the pool.
- **Next.js standalone build** (`output: "standalone"` in next.config.ts) → smaller per-node artifact, faster deploy, no `node_modules` on each node.
- **systemd unit** wraps pm2 god daemon (not pm2-startup) → kernel-level restart guarantee, survives OOM.

### 2.2 Active health-checks
- **`/health`** endpoint (HTTP 200, no DB) — fast liveness for Apache `ping=`/`ttl=`
- **`/api/ready`** endpoint (HTTP 200 iff DB SELECT 1 succeeds) — readiness for orchestrator-style checks
- **Apache `mod_proxy_hcheck`** does active probes every 5s — auto-drain on consecutive failures

### 2.3 Apache balancer hardening (beyond passage-prod template)
```apache
<Proxy "balancer://anvilhub-prod">
    BalancerMember "http://10.0.70.6:3200" route=f0 retry=5 timeout=90 \
        ping=200ms hcmethod=GET hcuri=/health hcexpr=ok_status hcinterval=5 hcpasses=2 hcfails=3
    BalancerMember "http://10.0.70.7:3200" route=f1 retry=5 timeout=90 \
        ping=200ms hcmethod=GET hcuri=/health hcexpr=ok_status hcinterval=5 hcpasses=2 hcfails=3
    BalancerMember "http://10.0.70.8:3200" route=f2 retry=5 timeout=90 \
        ping=200ms hcmethod=GET hcuri=/health hcexpr=ok_status hcinterval=5 hcpasses=2 hcfails=3
    ProxySet lbmethod=byrequests stickysession=ROUTEID
    ProxySet failonstatus=500,502,503,504
</Proxy>

<Macro hc-expr>
    Define ok_status %{REQUEST_STATUS} = 200
</Macro>
```

Key differences from passage-prod:
- **`hcmethod=GET hcuri=/health`** — active probe (passage just uses ProxyPass)
- **`hcinterval=5 hcfails=3`** — drain after 15s of failures
- **`stickysession=ROUTEID`** — Server Actions + NextAuth sessions stick to one node (avoids re-encrypting JWE per node)
- **`lbmethod=byrequests`** — even distribution under healthy state

### 2.4 Rolling deploy (zero downtime)
The release pipeline must:
1. Pick first node (e.g. f0)
2. Mark `status=+D` (drain) in Apache, graceful reload — existing requests finish, new ones go to f1/f2
3. Wait 10s for inflight to drain
4. SSH f0: `git pull && npm ci && npm run build && pm2 reload anvilhub`
5. Probe `http://10.0.70.6:3200/health` until 200 OK
6. Remove `status=+D` in Apache, graceful reload — f0 back in pool
7. Repeat for f1, then f2
8. **Never more than 1 node down at a time.** Capacity: 2/3 nodes always serving.

The `scripts/release.sh` Phase 6 (anvilhub deploy) needs this loop. Right now it's `pm2 restart anvilhub` on dev0001 — single bang, would cause outage.

### 2.5 Database resilience
- **PgBouncer** in front of HAProxy :5000 OPTIONAL — would handle prisma connection-pool churn under burst. Defer to v2: start with prisma's built-in pool (5 connections per worker × 2 workers × 3 nodes = 30 connections, well under Postgres default 100).
- **Patroni read-only routing**: AnvilHub is mostly read-heavy (Package list/search). Use HAProxy :5001 (read replicas) for read queries via Prisma's `$queryRaw` or a read-only DATABASE_URL split. **Defer to v2** — single rw URL is fine for now.
- **Migrations on deploy** — `prisma migrate deploy` runs ONCE from f0 only (gated by a deploy-lock flag). Other nodes wait. Otherwise three concurrent migrators race.

### 2.6 Static asset HA
- Each node has its own `public/` from the build — **no shared filesystem required**.
- `install.sh`, `install.ps1`, `/sha256/*.txt`, `/releases/*` — built into each node's build artifact.
- **Critical for sha256 verification**: all three nodes MUST serve byte-identical artifacts. Solved by all deploying from the same git tag + reproducible `next build`. Verified post-deploy by `sha256sum public/install.sh` across nodes — must match.

### 2.7 NextAuth + session HA
- **`NEXTAUTH_SECRET`** identical across all 3 nodes (deploy from vault, never copy-paste). One typo = silent session invalidation.
- **JWT strategy** (default in next-auth v4) — stateless, no Redis needed. Each node validates the JWE independently.
- **Sticky session via `ROUTEID` cookie** — ensures one user lands on same node for the session. Reduces re-auth on hot path.
- If we ever switch to **database session strategy**, must add Redis/PgBouncer for the session table — premature today.

### 2.8 Authentik HA
Authentik itself is single-host (login.culpur.net is its own infra). Not in scope for this migration. AnvilHub HA tolerates Authentik downtime (existing JWT sessions keep working for their TTL).

### 2.9 Cloudflare ↔ origin TLS
- Origin TLS termination is at **guard Apache**, not on f0/f1/f2.
- guard→fX traffic is plain HTTP on private LAN (10.0.70.0/24). This is fine because the LAN is not externally reachable per `network-traffic-architecture.md`.
- Origin pull from Cloudflare: ensure Cloudflare Origin CA cert on guard is valid + auto-renewed (existing certbot setup).

### 2.10 Self-healing layers (5 levels)

| Level | Mechanism | Recovers from | Latency |
|---|---|---|---|
| **L1** | systemd `Restart=on-failure` | process crash, OOM, SIGSEGV | <5s |
| **L2** | pm2 cluster (2 workers) | single-worker crash | instant (other worker handles) |
| **L3** | Apache `hcfails=3 hcinterval=5` | bad node (5xx, unreachable) | 15s drain |
| **L4** | Puppet `30min` agent run | config drift, missing dep, wrong perms | 30 min |
| **L5** | Wazuh alert + Matrix room ping | sustained outage (>5 min) | 5 min to operator |

L1+L2 = "machine recovers itself". L3 = "fleet recovers itself". L4 = "config drift heals". L5 = "humans get paged when self-heal fails".

---

## 3. Phase-by-phase plan (extending the earlier doc)

This plan reuses Phase A/B/C/D/E from `anvilhub-ha-migration-plan.md` and **layers in** the HA hardening.

### Phase A — Prerequisites (1-2 days)

Same as before, PLUS:

**A.7 — Add `/health` and `/api/ready` endpoints to AnvilHub source**
- `/health` → static `{"ok": true, "version": process.env.ANVIL_VERSION, "node": process.env.NODE_HOSTNAME}` — no DB
- `/api/ready` → `await prisma.$queryRaw\`SELECT 1\`` → 200 if OK, 503 if DB down
- These two routes MUST exist before Phase B (Apache hcheck depends on them)
- Commit + push to `culpur/anvilhub-web` BEFORE Phase B's clone step

**A.8 — Switch Next.js to standalone build**
- Add `output: "standalone"` to `next.config.ts`
- Update build script: `next build && cp -r .next/static .next/standalone/.next/static && cp -r public .next/standalone/public`
- Verify on dev0001 build first; size should drop 70-80% (no `node_modules` in artifact)
- Defer if standalone breaks any feature — use full monorepo build as fallback

**A.9 — Write `anvilhub_app` Puppet module**
- Path: `puppet-control/site/profile/manifests/anvilhub_app.pp` (mirrors `passage_app.pp`)
- Parameters: `port = 3200`, `cluster_instances = 2`, `db_url_eyaml`, `nextauth_secret_eyaml`
- Resources: clone repo, npm ci, npm run build, write .env, install pm2 systemd unit, ensure healthy
- Commit but DO NOT apply yet (apply gated by Phase B's manual smoke test)

**A.10 — eyaml-encrypt anvilhub prod secrets**
- DATABASE_URL (Patroni leader), NEXTAUTH_SECRET, AUTHENTIK_CLIENT_SECRET, GIPHY_API_KEY (if used)
- Store under `passwords.yaml::anvilhub::prod::*` on puppet master (CT 103)
- Commit eyaml file (encrypted form is safe to commit)

**A.11 — Add anvilhub_prod DB role + DB on Patroni**
- Via leader (HAProxy :5000 on any fX): `CREATE ROLE anvilhub_prod LOGIN PASSWORD '<<from vault>>'; CREATE DATABASE anvilhub_prod OWNER anvilhub_prod;`
- Migration history fix first: on dev0001 run `prisma migrate dev --name init` to generate `migrations/` folder from current schema (this is Q2 from the earlier plan), commit, push
- Then `DATABASE_URL=postgresql://anvilhub_prod:...@<f0-ip>:5000/anvilhub_prod prisma migrate deploy` from dev0001
- Verify 5 tables created with correct ownership

**A.12 — DNS: dev-anvilhub.culpur.net**
- Cloudflare A record → guard IP (proxied)
- guard vhost `/etc/apache2/sites-enabled/dev-anvilhub.culpur.net.conf` → `ProxyPass / http://10.0.70.80:3100/`
- This goes live BEFORE Phase C cutover so dev users have a stable URL once prod cuts over

### Phase B — Parallel deploy to f0+f1+f2 (1 day)

All steps with `status=D` in balancer — zero prod traffic.

**B.1 — Clone source to /opt/anvilhub on each node**
```bash
for h in node6f0 node6f1 node6f2; do
  ssh $h 'sudo git clone https://registry.culpur.net/git/culpur/anvilhub-web.git /opt/anvilhub && sudo chown -R passage:passage /opt/anvilhub'
done
```
(Uses passage user since passage_app puppet pattern sets up sshusers + sudoers for it. Confirm `passage` user exists on f0/f1/f2 — it does per passage deploy.)

**B.2 — Install + build on each node (parallel where possible)**
```bash
for h in node6f0 node6f1 node6f2; do
  ssh $h 'cd /opt/anvilhub && sudo -u passage npm ci && sudo -u passage npm run build'
done
```
Standalone build output at `/opt/anvilhub/.next/standalone/`. Verify each node's build before proceeding.

**B.3 — Deploy .env to each node FROM VAULT (not copy)**
- On each node, write `/opt/anvilhub/packages/web/.env` with:
  - `DATABASE_URL=postgresql://anvilhub_prod:<from-vault>@127.0.0.1:5000/anvilhub_prod`
  - `NEXTAUTH_URL=https://anvilhub.culpur.net`
  - `NEXTAUTH_SECRET=<from-vault>` (identical on all 3)
  - `AUTHENTIK_CLIENT_ID=<from-vault>`, `AUTHENTIK_CLIENT_SECRET=<from-vault>`
  - `NEXT_PUBLIC_APP_URL=https://anvilhub.culpur.net`
  - `NODE_HOSTNAME=$(hostname)` (for /health debug)
- chmod 0600, chown passage:passage

**B.4 — Install systemd unit for pm2 god daemon**
On each node:
```
[Unit]
Description=AnvilHub pm2 daemon
After=network-online.target patroni.service
Wants=network-online.target

[Service]
Type=forking
User=passage
LimitNOFILE=infinity
LimitNPROC=infinity
Environment=PM2_HOME=/home/passage/.pm2
PIDFile=/home/passage/.pm2/pm2.pid
ExecStart=/usr/bin/pm2 resurrect
ExecReload=/usr/bin/pm2 reload all
ExecStop=/usr/bin/pm2 kill
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```
`systemctl enable --now pm2-anvilhub.service`

**B.5 — pm2 cluster start**
```bash
cd /opt/anvilhub/packages/web
pm2 start npm --name anvilhub -i 2 -- start
pm2 save
```
`-i 2` = 2 workers, cluster mode (not fork). Restart-on-crash automatic.

**B.6 — Smoke test each node directly**
```bash
for ip in 10.0.70.6 10.0.70.7 10.0.70.8; do
  echo === $ip ===
  ssh node6fX "curl -sf http://$ip:3200/health | jq" || echo "FAIL"
  ssh node6fX "curl -sf http://$ip:3200/api/ready" || echo "FAIL READY"
done
```
Expect all 6 checks to pass. If any node fails, debug there — don't proceed.

**B.7 — Write anvilhub-prod balancer vhost (DISABLED)**
On guard: `/etc/apache2/sites-available/anvilhub-prod.conf` — full template from §2.3 above. **Do not enable yet.**
- Confirm `apache2ctl configtest` passes
- Copy to guard2 (same path)
- Both vhosts disabled (not in `sites-enabled/`)

### Phase C — Cutover (30 minutes, fully reversible)

**C.1 — Pre-flight checklist**
- [ ] All 3 fX nodes serving 200 on /health
- [ ] Patroni cluster healthy (`patronictl list` shows leader + replicas in sync)
- [ ] Authentik redirect URI added: `https://anvilhub.culpur.net/api/auth/callback/authentik` (was already present, just re-verify)
- [ ] Vault has anvilhub prod creds (test fetch one)
- [ ] dev0001 ssh tunnel from bastion confirmed (rollback path)
- [ ] Cloudflare cache-everything rule for `anvilhub.culpur.net/_next/static/*` confirmed
- [ ] guard2 has been verified as standby for guard (`keepalived` or VIP check)

**C.2 — Enable the new vhost (still on disabled BalancerMembers)**
```bash
sudo a2ensite anvilhub-prod.conf  # on both guard and guard2
sudo apache2ctl configtest && sudo apache2ctl graceful
```
At this point, anvilhub-prod is configured but every BalancerMember has `status=+D` — no traffic reaches new nodes. Old vhost (`anvilhub.culpur.net.conf`) still serves dev0001:3100.

**Wait, hold on** — both can't be active for same hostname. Apache uses first-match. We need to either:
- (a) Disable old vhost atomically when new vhost goes live (preferred — clean cut)
- (b) Use balancer-only with old dev0001 added as a 4th member, gradually drain

Pick **(a)** for simplicity. The actual sequence is:

**C.3 — Switch traffic to balancer (zero-downtime via atomic swap)**
```bash
sudo a2dissite anvilhub.culpur.net.conf
sudo a2ensite anvilhub-prod.conf
sudo apache2ctl configtest && sudo apache2ctl graceful
```
Wait — but balancer members are still `status=+D`. This would 503. So actually:

**C.3 (corrected) — Enable balancer members BEFORE old vhost swap**
```bash
# Step 1: edit anvilhub-prod.conf, remove status=+D from f0 only
sudo sed -i 's/status=+D route=f0/route=f0/' /etc/apache2/sites-enabled/anvilhub-prod.conf
sudo apache2ctl graceful

# Step 2: confirm balancer-manager shows f0 as OK
curl -k https://localhost/balancer-manager  # behind auth on guard

# Step 3: enable f1
sudo sed -i 's/status=+D route=f1/route=f1/' /etc/apache2/sites-enabled/anvilhub-prod.conf
sudo apache2ctl graceful

# Step 4: enable f2 (if task #693 confirmed)
sudo sed -i 's/status=+D route=f2/route=f2/' /etc/apache2/sites-enabled/anvilhub-prod.conf
sudo apache2ctl graceful

# Step 5: ATOMIC SWAP — disable old, enable new (already enabled, but currently lower priority)
# Actually if anvilhub-prod.conf has the same ServerName, Apache will pick alphabetical first.
# Simplest: disable old vhost, then graceful — anvilhub-prod becomes the only match.
sudo a2dissite anvilhub.culpur.net.conf
sudo apache2ctl graceful
```

**C.4 — Validation (5 min)**
```bash
# From off-network:
curl -sf https://anvilhub.culpur.net/health
curl -sf https://anvilhub.culpur.net/api/ready
curl -sfL https://anvilhub.culpur.net/  # full home page render

# Run login flow manually in browser:
#   1. open anvilhub.culpur.net
#   2. click Login
#   3. Authentik flow completes
#   4. /api/auth/session returns user object
```

**C.5 — Purge Cloudflare cache**
```bash
# Via CF API (token in vault):
curl -X POST "https://api.cloudflare.com/client/v4/zones/<zone-id>/purge_cache" \
  -H "Authorization: Bearer $CF_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{"hosts":["anvilhub.culpur.net"]}'
```

**C.6 — Watch for 1 hour**
- Apache `tail -f /var/log/apache2/error.log /var/log/apache2/access.log | grep -v 200`
- pm2 on each fX: `pm2 logs anvilhub`
- Patroni: `psql -h 127.0.0.1 -p 5000 -U anvilhub_prod -c "SELECT count(*) FROM \"User\";"` periodically
- Cloudflare analytics dashboard

**Rollback (anytime in C, <60 seconds):**
```bash
sudo a2ensite anvilhub.culpur.net.conf
sudo a2dissite anvilhub-prod.conf
sudo apache2ctl graceful
```

### Phase D — Decom + harden (after 48-hour soak)

**D.1 — Kill dev0001 prod next-server**
```bash
ssh dev0001 'pkill -f "next-server.*3100" && sleep 2 && pgrep -f next-server'
# Verify process is gone
```

**D.2 — Re-purpose dev0001:3100 for dev only**
- Update dev0001's anvilhub ecosystem.config.cjs to bind only on 127.0.0.1:3100 (not 0.0.0.0)
- Set `NEXTAUTH_URL=https://dev-anvilhub.culpur.net`
- pm2 start under `soulofall` user, not root
- Add to `dev-anvilhub.culpur.net.conf` (already created in A.12)

**D.3 — Update release pipeline (anvil-release MCP + .anvil-release.toml)**
- Remove `anvilhub_pm2_proxmox_host`, `anvilhub_pm2_container_id`, `anvilhub_pm2_home`
- Add `anvilhub_deploy_hosts = ["node6f0", "node6f1", "node6f2"]`
- Add `anvilhub_balancer_host = "guard.armored.ninja"` + balancer-manager URL
- Update `lib/tools/update-pages.js` to do **rolling deploy**: for each host in sequence (a) drain in balancer (b) git pull + build (c) pm2 reload (d) wait for /health 200 (e) un-drain
- Add `--canary` flag: deploy to f0 only, hold, manual confirm, then f1+f2

**D.4 — Decom CT 113**
- IF Q1 from earlier plan confirmed CT 113 is orphaned: `pct stop 113 && pct destroy 113` on Proxmox host
- Document in MEMORY.md: anvilhub no longer uses CT 113
- Delete `feedback-anvilhub-deployment.md` memory entry (it's now wrong)

**D.5 — Install Wazuh agent rules**
- Wazuh rule: `level=10` if `anvilhub.*5(00|02|03)` appears in `/var/log/apache2/error.log` on guard for >5 occurrences in 60s
- Matrix alert routing to ops room (per `soc-integration.md`)
- Test: temporarily mark all 3 BalancerMembers `status=+D` for 30s → confirm alert fires

**D.6 — Add Grafana dashboard**
- Panel 1: Apache balancer-manager scrape → per-node request rate + 5xx rate
- Panel 2: pm2 memory + CPU per worker (Wazuh agent collects)
- Panel 3: Patroni leader + replica lag
- Panel 4: Cloudflare zone analytics (requests/cached/uncached)

### Phase E — Dev/prod separation finalization

**E.1 — Dev CI/CD**
- Push to `main` on culpur/anvilhub-web → webhook → dev0001 `git pull && npm run build && pm2 reload anvilhub` (no rolling, single node)

**E.2 — Prod CI/CD (gated)**
- Tag `vX.Y.Z` → manual gate in Gitea → triggers Phase D.3's rolling deploy across f0/f1/f2
- Verify all 3 nodes serve the new version before pipeline reports success
- If any node fails: pipeline halts, balancer auto-drains failed node (L3), operator alerted (L5)

**E.3 — Document all of this**
- Write `~/.claude/projects/-Users-soulofall-projects/memory/anvilhub-ha-deployment.md` (replaces obsolete `anvilhub-deployment.md`)
- Include: rolling-deploy procedure, balancer-manager URL, health-check URLs, vault keys, Cloudflare zone ID, Wazuh rule IDs

---

## 4. Self-healing matrix (the meat)

| Failure | Detected by | Recovery action | Time to recover | Operator paged? |
|---|---|---|---|---|
| Worker SIGSEGV | pm2 cluster | pm2 spawns replacement worker | <1s | No |
| All workers on f0 crash | systemd `Restart=on-failure` | systemd restarts pm2 | 5s | No |
| f0 returns 503 for /health | Apache `hcfails=3` | balancer drains f0 | 15s | No |
| f0 returns 503 for 60s | Wazuh rule 10X | Matrix alert → ops room | 60s | Yes |
| f0 OOM (process killed) | systemd OOMScoreAdjust + Restart=on-oom | systemd restarts pm2 | 5s | No (logged) |
| Patroni leader fails | HAProxy :5000 reroutes | new leader elected (Patroni) | 30s | No |
| guard Apache down | guard2 keepalived takes VIP | guard2 serves all traffic | 5s | Yes (warn) |
| All 3 fX nodes down | Apache returns 503 on all | NO automatic recovery | — | Yes (page) |
| Cloudflare zone down | external | NO automatic recovery (CF SLA 99.99%) | — | Yes (page) |
| Bad config landed (puppet apply broke node) | puppet `--noop` pre-deploy + balancer drain | Puppet reverts, drain holds traffic | 30 min | Yes (alert) |

**The system survives any single-node failure with zero user impact.** Two simultaneous failures cause partial degradation but not full outage as long as 1 node remains healthy.

---

## 5. Disaster recovery

### 5.1 What if Patroni cluster splits?
- Apache balancer points all nodes at local HAProxy :5000. If Patroni elects wrong leader on a partition, write requests fail with `read-only transaction`.
- Mitigation: Patroni has `synchronous_mode=on` + `synchronous_node_count=1` (verify on CT 103 etcd config). One sync replica required for commits. Split-brain protected by etcd quorum.

### 5.2 What if all 3 nodes lose `.env`?
- Puppet reapplies from eyaml every 30 min.
- Vault is the source of truth — `.env` is regeneratable.
- Document the manual recovery: `puppet agent -t` on each node.

### 5.3 What if Cloudflare blocks us?
- Origin (guard) is on a static IP. Bypass CF by adding host override or hitting origin IP directly.
- Run `dig +short anvilhub.culpur.net @8.8.8.8` to confirm CF DNS record — if missing, restore from Cloudflare's history.

### 5.4 Backup posture
- Patroni cluster: nightly pg_dump via vzdump (see `weekly-cifs-backup-window.md` for storage box)
- AnvilHub source: git history on registry.culpur.net + GitHub mirror
- MinIO source archives: tied to S3 lifecycle, in scope for `minio-s3-cluster.md`
- **What we DON'T back up**: Built artifacts (regeneratable from source + tag). That's fine.

---

## 6. Open questions remaining

These extend the earlier plan's Q1-Q5. **NEW questions from this HA hardening pass:**

**Q6 (HA-specific): keepalived between guard and guard2?**
Today, `guard.armored.ninja` is a single A record. If guard goes down, guard2 doesn't auto-take traffic. The HA plan assumes a VIP. Confirm:
- Is keepalived running on guard + guard2?
- If NO: add it as a Phase A prereq. Configure VIP `10.0.70.4` (or similar) as the active address shared between guard and guard2.
- If YES: confirm both nodes have anvilhub-prod.conf in `/etc/apache2/sites-enabled/`.

**Q7: Standalone build vs full monorepo?**
Standalone is faster + smaller, but if any feature breaks under standalone (rare but possible with custom server middleware), we fall back. Test on dev0001 first.

**Q8: pm2 cluster vs single fork?**
2 workers per node × 3 nodes = 6 total workers. Memory: 6 × 512 MB = 3 GB total cluster memory. Acceptable. Worth it for crash-tolerance.

**Q9: Cloudflare cache rules for /api/* routes?**
Currently middleware sets `s-maxage=60` on DB-backed. Confirm CF respects this and doesn't cache /api/auth/* (auth flows must NOT be cached). Add explicit cache bypass rule for `/api/auth/*` in CF page rules.

**Q10: Should we add PgBouncer in front of Patroni :5000?**
Defer. With 30-connection ceiling and prisma's pool, we're well under Postgres's 100. Revisit at 1000 concurrent users.

**Q11: Wazuh rule + Matrix room — does the channel exist?**
We have `soc-integration.md` for general SIEM ops. Need explicit anvilhub-ops Matrix room. Probably reuse #culpur-ops.

---

## 7. Time estimate (extended)

| Phase | Sub-tasks | Time |
|---|---|---|
| **A** Prereqs | DB + DNS + Puppet + secrets + new endpoints + standalone build | 2 days |
| **B** Parallel deploy | clone + build + pm2 cluster + systemd + smoke test | 1 day |
| **C** Cutover | balancer enable + atomic swap + 1hr soak | 4 hours |
| **D** Decom + harden | dev0001 kill + CT 113 destroy + rolling-deploy pipeline + Wazuh + Grafana | 2 days |
| **E** Dev/prod separation | finalize CI/CD + docs | 0.5 day |
| **Total** | | **~5.5 days** |

vs the earlier plan's 2-4 days — the difference is everything in §2 (real HA, self-healing, observability).

---

## 8. What to verify before signing off Phase A

```bash
# 1. /health endpoint shipped to source tree
grep -r "/health\|/api/ready" /opt/projects/anvilhub/packages/web/app/

# 2. Standalone build works
ssh dev0001 'cd /opt/projects/anvilhub && npm run build && ls .next/standalone/server.js'

# 3. Patroni cluster healthy
ssh node6f0 'patronictl -c /etc/patroni.yml list'

# 4. anvilhub_prod DB + role exists
ssh node6f0 'psql -h 127.0.0.1 -p 5000 -U anvilhub_prod -d anvilhub_prod -c "\dt"'

# 5. Vault has anvilhub/prod/* keys
vault kv list secret/anvilhub/prod/

# 6. Authentik redirect URIs updated
# (manual check in login.culpur.net admin)

# 7. keepalived on guard + guard2 (if Q6 confirmed)
ssh guard 'systemctl status keepalived'
ssh guard2 'systemctl status keepalived'

# 8. Puppet module compiles
ssh puppet 'cd /etc/puppetlabs/code/environments/production && puppet parser validate site/profile/manifests/anvilhub_app.pp'

# 9. anvilhub-prod.conf passes apache2ctl configtest (DISABLED still)
ssh guard 'sudo apache2ctl -t -D DUMP_VHOSTS | grep anvilhub-prod'

# 10. Cloudflare API token has cache:purge scope
# (manual check in CF dashboard)
```

10/10 must pass before Phase B starts.

---

## 9. Sign-off requirements

Before any production change:

- [ ] User confirms Q6 (keepalived status)
- [ ] User decides on standalone build (Q7)
- [ ] User confirms 5.5-day timeline acceptable
- [ ] User authorizes Phase A.7 (adding /health endpoints to anvilhub-web — this is a code commit + push to anvilhub-web repo)
- [ ] User confirms Wazuh alert routing target (Q11)

After Phase B (before C):

- [ ] All 10 verification checks in §8 pass
- [ ] User runs the full login flow manually on one of the f0/f1/f2 nodes (bypass guard, hit fX:3200 directly via SSH tunnel) and confirms it works
- [ ] User confirms cutover window (low-traffic time)

After Phase C (before D):

- [ ] 48-hour soak with no production incidents
- [ ] User reviews Apache logs, pm2 logs, Patroni metrics
- [ ] User confirms CT 113 can be destroyed

---

*Authored 2026-05-20. Ready for sign-off.*
