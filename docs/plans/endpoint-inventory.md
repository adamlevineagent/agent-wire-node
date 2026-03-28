# HTTP Endpoint Inventory (2026-03-28)

For WS-ONLINE-S security hardening reference.

## After S1: What Stays on HTTP (read-only, Wire JWT or local auth)

### No Auth (must be auth-gated per S1)
- `GET /health` — server.rs:299
- `GET /stats` — server.rs:549 (leaks financial data)
- `GET /tunnel-status` — server.rs:562 (leaks tunnel URL)

### Wire JWT (existing)
- `GET /documents/:id` — server.rs:322

### Local Auth Token (read-only, safe for remote with Wire JWT added)
35 pyramid GET endpoints + 3 partner GET endpoints. See wire-online-push.md S1 "stays on HTTP" list.

## After S1: What Moves to Tauri IPC

28 mutation endpoints total:

### Pyramid Mutations (25)
- `POST /pyramid/slugs` — creates slugs (routes.rs:295)
- `POST /pyramid/:slug/build` — LLM build (routes.rs:324)
- `POST /pyramid/:slug/build/cancel` — cancel (routes.rs:314)
- `POST /pyramid/:slug/build/question` — question build (routes.rs:806)
- `POST /pyramid/:slug/build/preview` — LLM decomposition (routes.rs:818)
- `POST /pyramid/:slug/characterize` — LLM characterization (routes.rs:829)
- `POST /pyramid/:slug/ingest` — file ingestion (routes.rs:422)
- `POST /pyramid/config` — credential write (routes.rs:431)
- `POST /pyramid/:slug/annotate` — annotation write (routes.rs:449)
- `POST /pyramid/:slug/meta` — LLM meta passes (routes.rs:478)
- `POST /pyramid/:slug/auto-update/config` — config mutation (routes.rs:586)
- `POST /pyramid/:slug/auto-update/freeze` — freeze (routes.rs:597)
- `POST /pyramid/:slug/auto-update/unfreeze` — unfreeze (routes.rs:607)
- `POST /pyramid/:slug/auto-update/l0-sweep` — sweep trigger (routes.rs:617)
- `POST /pyramid/:slug/auto-update/breaker/resume` — breaker (routes.rs:627)
- `POST /pyramid/:slug/auto-update/breaker/build-new` — rebuild (routes.rs:638)
- `POST /pyramid/:slug/crystallize` — crystallization (routes.rs:683)
- `POST /pyramid/vine/build` — vine build (routes.rs:705)
- `POST /pyramid/:slug/vine/rebuild-upper` — vine rebuild (routes.rs:775)
- `POST /pyramid/:slug/vine/integrity` — integrity check (routes.rs:785)
- `POST /pyramid/:slug/publish` — Wire publish (routes.rs:839)
- `POST /pyramid/:slug/publish/question-set` — question set publish (routes.rs:848)
- `POST /pyramid/:slug/check-staleness` — staleness trigger (routes.rs:859)
- `POST /pyramid/chain/import` — chain import (routes.rs:896)
- `POST /pyramid/:slug/archive` — archive (routes.rs:547)
- `DELETE /pyramid/:slug/purge` — CASCADE DELETE (routes.rs:556)

### Partner Mutations (2)
- `POST /partner/message` — LLM call (partner/routes.rs:97)
- `POST /partner/session/new` — session creation (partner/routes.rs:78)

### Auth Mutations (1)
- `POST /auth/complete` — overwrites auth state (server.rs:498)

## New Endpoints to Add (Wire Online)
- `GET /pyramid/:slug/query-cost` — cost preview (read-only, Wire JWT)
- `GET /pyramid/:slug/export` — bulk export (read-only, Wire JWT, rate limited)
- `GET /pyramid/:slug/absorption-config` — read-only config
- `POST /pyramid/remote-query` — Vibesmithy proxy (local auth only)
- `POST /trace/openrouter` — webhook ingestion (signature auth)
