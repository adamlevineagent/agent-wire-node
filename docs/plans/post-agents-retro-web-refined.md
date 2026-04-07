# Post-Agents Retro Web Surface — Refined Plan v2

> Refined from `docs/handoffs/handoff-post-agents-retro-web.md` after a 4-auditor plan-audit gate. Fixes anonymous-auth gap, mpsc fan-out, navigate/HTML mismatch, XSS/CSRF, asset serving, and folds in Wire magic-link as a V1 workstream.

---

## What Changed From v1 (Handoff Doc)

| # | v1 said | v2 says | Why |
|---|---------|---------|-----|
| 1 | Anonymous browsers hit `/p/...` directly | New `with_public_or_session_auth` filter; anonymous principal explicit | `with_dual_auth` rejects no-token requests with 401. The whole HTML layer is impossible without this. |
| 2 | "Subscribe to existing BuildProgress mpsc channel" | Insert `tokio::sync::broadcast` fan-out hub; existing mpsc drain task feeds it; desktop UI + WS clients both subscribe | mpsc is single-consumer. Cannot subscribe twice. |
| 3 | "Question box POSTs to /pyramid/:slug/navigate" | New `POST /p/{slug}/ask` accepting form-encoded; calls existing `handle_navigate` internals; returns HTML | Existing route is JSON-only and auth-gated. |
| 4 | Magic-link / questioner-pays = V2 | **OTP-code login** is a V1 workstream (WS-E). Same page, no callback. | `auth::send_magic_link` triggers Supabase OTP email, `auth::verify_otp` consumes the 6-digit code. Cleaner than redirect. |
| 5 | "Optional SSE on call_model_unified" | Out of scope for V1; streaming-by-build-events only | call_model_unified has 8+ callers; refactor risk too high for V1. Brilliant text-materialization can use BuildProgress events as the animation driver. |
| 6 | maud OR format!() | format!() everywhere via a strict `escape_html()` helper + maud-style discipline | Avoids new dep. We mandate the helper. |
| 7 | Folio as one HTTP route | Folio = simplest depth-limited recursive dump in V1; full folio CLI generator integration deferred | Folio CLI command is its own beast. |
| 8 | "<100ms perf criterion" | Per-route caps: tree depth ≤ 4, folio depth ≤ 4, max 500 nodes rendered, search results ≤ 50 | Defends against `?depth=99` DoS. |
| 9 | No mention of XSS, CSRF, CSP, ETag, robots.txt, favicon, error pages | All explicit (see §Security & Hardening) | These caused the original audit to scream. |
| 10 | Pretext + LayoutSans assumed real | **WS5 spike-verifies before committing.** Fallback: hand-rolled canvas overlay using basic `canvas.measureText` directly. Skip LayoutSans Ctrl+F if absent — browser Ctrl+F still works on the underlying HTML | We don't trust this pre-build. |
| 11 | Bun bundler new toolchain | Use existing Vite (already in repo for the React frontend); single hand-written `client.ts` compiled to `client.js` and served via new static-asset route | No new toolchain. |
| 12 | Static assets just appear | New `GET /assets/<file>` warp route; CSS and JS `include_bytes!`-ed at compile time; content-hash in URL for cache-busting | Tauri binary has no writable static dir. |
| 13 | "Border-character staleness encoding — see vision doc" | Encoding table inlined in §Aesthetic | Self-contained spec. |
| 14 | BuildProgress events have no slug filter | **Tag at bus boundary:** wrap events in `TaggedBuildEvent { slug, event }` at the build-dispatcher (which already knows the slug). Zero changes to the 40+ producer sites. WS handler filters by subscribed slug; access-tier check on WS upgrade. | Prevents priced-pyramid build-progress leaking to anonymous viewers of a sibling public pyramid. |
| 15 | 4-hour budget | We're shipping today. Aggressive parallelism, no estimates. |  |

---

## Architecture (Final)

### Routes Added

| Route | Method | Auth | Returns |
|-------|--------|------|---------|
| `GET /p/` | GET | anon | HTML index of public pyramids on this node |
| `GET /p/{slug}` | GET | anon (public) / session (priced/circle) | Pyramid home HTML |
| `GET /p/{slug}/{node_id}` | GET | as above | Single node HTML |
| `GET /p/{slug}/tree` | GET | as above | Tree HTML, depth-capped |
| `GET /p/{slug}/search` | GET | as above | Search results HTML |
| `GET /p/{slug}/glossary` | GET | as above | Auto-glossary from terms[] |
| `GET /p/{slug}/folio` | GET | as above | Recursive depth-limited dump |
| `POST /p/{slug}/ask` | POST | as above | HTML answer page; CSRF-checked |
| `GET /p/{slug}/login` | GET | anon | Renders email form |
| `POST /p/{slug}/login` | POST | anon | Sends OTP via `auth::send_magic_link`; renders OTP input form on same page |
| `POST /p/{slug}/verify` | POST | anon | Calls `auth::verify_otp` with email + 6-digit code; sets `wire_session` cookie; redirects to original slug page |
| `POST /p/{slug}/logout` | POST | session | Clears cookie; redirects |
| `GET /p/{slug}/ws` | WS upgrade | as above | BuildProgress events filtered to slug |
| `GET /assets/{file}` | GET | anon | Static CSS/JS, content-addressed |
| `GET /p/robots.txt` | GET | anon | `User-agent: * \n Disallow: /pyramid/` |
| `GET /p/favicon.ico` | GET | anon | Tiny `include_bytes!` favicon |

### New Auth Filter: `with_public_or_session_auth`

```
Resolution order:
  1. Authorization: Bearer <token>  → existing dual-auth path (Local | WireJWT)
  2. Cookie: wire_session=<jwt>     → parse, verify Ed25519, treat as WireJWT
  3. None of the above              → AuthSource::Anonymous

Then enforce_access_tier(slug, source) where:
  Anonymous + public      → allow
  Anonymous + priced/circle → 404 (not 401, not 451 — anti-enumeration)
  Anonymous + embargoed   → 404
  WireJWT + public        → allow
  WireJWT + priced        → allow (payment TODO same as today)
  WireJWT + circle        → existing circle membership check
  WireJWT + embargoed     → 451
  Local                   → allow all (it's the operator)
```

### New `BuildEventBus`

```rust
// In PyramidState
pub event_bus: Arc<BuildEventBus>,

pub struct BuildEventBus {
    pub tx: tokio::sync::broadcast::Sender<BuildProgress>,
}

// At app init: create channel(1024).
// Existing build runner: keep mpsc producer→consumer pipeline AS-IS.
// Add a single relay task: drain mpsc → bus.tx.send().
// WS handler: bus.tx.subscribe() → filter by slug → JSON-encode → forward.
```

**Crucially:** we do NOT touch the 40+ existing BuildProgress producer sites. We add the broadcast bus DOWNSTREAM of the existing drain. Single-point change.

`BuildProgress` already has a slug-bearing structure or we add `slug` to the variants that lack it. WS0.5 verifier checks all variants and adds the field where missing.

### CSRF for `/ask` and `/logout`

- `wire_session` cookie is `SameSite=Strict`
- Plus origin check: reject if `Origin` header present and != tunnel host
- Plus a CSRF nonce in a hidden form field, set per-render from a HMAC of `(session_id, slug, time_window)`

### XSS Defense

- One canonical helper: `pub fn esc(s: &str) -> String` (escapes `& < > " '`)
- Every `format!` interpolation of dynamic content goes through `esc()`
- URL scheme allowlist for cross-pyramid links: `http`, `https` only
- `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'`
- Audit gate: a unit test hits each route with `<script>` and `'><img onerror=` payloads in slug, q, node_id, question text → asserts escaped output

### Caching

- ETag: `weak("{slug}-{pyramid_revision}-{node_id}")`
- `pyramid_revision`: integer bumped on every successful build/contribution. New column on `pyramids` table OR derive from max(updated_at) of nodes. Migration is one column.
- `Cache-Control: no-cache, must-revalidate` (clients revalidate but use ETag for 304s)
- `Vary: Cookie`

### Rate Limiting (Anonymous)

- New per-IP limiter (separate from existing per-operator limiter): 256 req/min for `/p/...`, 16 req/min for `POST /p/{slug}/ask`
- Operator can configure a "max anonymous open-mode questions per pyramid per day" cap to bound wallet drain
- For `absorb-all` / `absorb-selective` mode in V1: anonymous AND OTP-WebSession visitors get a "this pyramid requires a Wire operator token to ask questions" HTML page (per **B2** — Wire-wallet linkage is V2). Operators with a Wire-issued JWT can paste it via a setup page or use Authorization header directly.

---

## Aesthetic (Inlined Spec)

### Color Tokens
```
--bg:    #0a0e0a   (near-black phosphor)
--fg:    #c8d6c8   (warm phosphor green-grey)
--dim:   #6b7d6b   (muted)
--hot:   #50fa7b   (verified-fresh accent)
--warn:  #f5c542   (stale)
--gap:   #ff5577   (gap / unknown)
--link:  #88ccff   (cross-references)
--rule:  #1f2a1f   (border lines)
```

### Typography
```
font-family: "JetBrains Mono", "Berkeley Mono", "IBM Plex Mono", ui-monospace, monospace;
font-size: 14px;
line-height: 1.5;
```
We `@font-face` JetBrains Mono Regular + Bold from `/p/_assets/fonts/` (path corrected per A9). ~120KB total. The font files are `include_bytes!`-ed via the `build.rs` manifest (B13).

### Staleness Border Encoding
```
│  Verified, fresh, sourced       (solid box-drawing vertical, color: --hot)
┊  Stale (source changed since)   (dashed, color: --warn)
╎  Inferred, no direct source     (dotted, color: --dim)
░  Gap, not yet expanded          (shade fill, color: --gap)
```
Implementation: each node block is `<article class="node node--{state}">` with a `::before` that paints the border via `box-shadow: inset 3px 0 0 var(--hot)` and a CSS background-image of the appropriate character at 1.5em line height.

### Layout Skeleton
- Page max-width: 90ch
- No hero. Apex headline as `<h1>` (plain text). ASCII banner is `<pre aria-hidden="true">` above the h1, populated by JS or by Mercury-2 cached output.
- Topics list as `<ul class="toc">` with `├─` / `└─` connectors via `::before` content
- Footer per node: `<footer class="prov">version • src=N • conf=0.87 • path=foo/bar/baz</footer>`
- Question box pinned at bottom of pyramid home: `<form action="/p/{slug}/ask" method="post">`

### Empty States
- Pyramid with 0 nodes: shows the question box and "ASK SOMETHING TO BEGIN" in `--gap` color
- Search with 0 results: "NO RESULTS — but the question box below knows how to grow new ones."
- 404 (any unknown route): retro-styled, links back to `/p/`

---

## Workstreams (See §Decomposition Below)

---

## Verification Criteria (v3.2 — uses canonical A15 routes)

### Functional
1. `curl https://<tunnel>/p/` returns 200 + valid HTML index of public pyramids (anonymous)
2. `curl https://<tunnel>/p/<public-slug>` returns 200 + valid HTML, no auth required
3. `curl https://<tunnel>/p/<priced-slug>` returns 404 (not 401, not 451) when anonymous
4. `curl -b 'wire_session=<valid>' https://<tunnel>/p/<priced-slug>` returns 200
5. `curl https://<tunnel>/p/<slug>/<node_id>` returns the node HTML
6. `curl 'https://<tunnel>/p/<slug>/search?q=<script>alert(1)</script>'` returns escaped output (no live script)
7. `POST /p/<slug>/_ask` with no CSRF token → 403
8. `POST /p/<slug>/_ask` with valid CSRF in `open` mode → returns answer HTML
9. `POST /p/<slug>/_ask` in `absorb-all` mode by anonymous OR WebSession → renders "Wire operator token required" page (not 200 answer)
10. `GET /p/<slug>/_login` renders email entry form
11. `POST /p/<slug>/_login` with valid email triggers `send_magic_link` and renders OTP-code entry form on the same page
12. `POST /p/<slug>/_verify` with the 6-digit OTP from email sets `wire_session` cookie via `verify_otp` and redirects to `/p/<slug>`
13. `POST /p/<slug>/_logout` clears the cookie row in `web_sessions` (global logout) and redirects
14. WS connection to `/p/<slug>/_ws` through tunnel receives `TaggedBuildEvent`s filtered to that slug
15. WS subscriber to a priced slug as anonymous → upgrade rejected
16. ETag on a node's HTML produces 304 on second request without rebuild
17. `/p/_assets/app.<hash>.css` returns the CSS bundle with `Cache-Control: public, max-age=31536000, immutable`
18. `/robots.txt` (root) returns the rules per A9
19. `/favicon.ico` (root) returns the favicon
20. Layer 2 canvas mounts on JS-enabled browser without breaking Layer 1 (or hand-rolled fallback ships per WS-J)
21. With JS disabled, every interaction (read, search, ask, login, verify, logout) still works

### Security
22. XSS payload in every user-input field (q, email, otp, slug) → escaped via `esc()`
23. CSRF check enforced on `_ask`, `_login`, `_verify`, `_logout`
24. CSP header present on all `/p/` HTML responses; `connect-src 'self'` only (no `wss://*`)
25. Anonymous and WebSession can never reach `embargoed` content; only WireOperator with matching `circle_id` reaches `circle`-tier
26. Per-IP rate limit triggers: 256/min reads, 16/min `_ask`, 3/min `_login`, plus per-target-email 10/hour
27. `client_key()` returns peer addr (not `CF-Connecting-IP`) when peer is non-loopback (B5 trust gate)
28. `web_session::lookup` rejects expired tokens; sweeper task evicts hourly

---

## Decomposition (Phase 0)

### Workstream Inventory

**Phase 1 (parallel):**
- **WS-A: Auth filter + tier rework** (`routes.rs`) — `with_public_or_session_auth`, anonymous principal, cookie reading, tier table for anonymous. **Owns: routes.rs auth section**.
- **WS-B: BuildEventBus + WS endpoint** (`pyramid/mod.rs`, `routes.rs`, `build.rs` types) — broadcast hub, drain relay, `/p/{slug}/ws` upgrade handler with slug filter and tier check. **Owns: state init, ws route registration, BuildProgress slug field if missing**.
- **WS-C: HTML rendering primitives + read routes** (`pyramid/html.rs` new module, `routes.rs`) — `esc()` helper, layout skeleton, `GET /p/`, `GET /p/{slug}`, `GET /p/{slug}/{node_id}`. **Owns: new pyramid/html.rs**.
- **WS-D: Static asset serving + CSS** (`routes.rs`, `assets/` new dir) — `GET /assets/{file}`, `include_bytes!`, content hash, `app.css` with the full retro aesthetic, robots.txt, favicon. **Owns: assets/, asset routes**.
- **WS-E: OTP session bridge** (`routes.rs`, reuses `auth.rs`) — `/p/{slug}/login` GET+POST, `/p/{slug}/verify` POST sets `wire_session` cookie via `verify_otp`, `/p/{slug}/logout`, CSRF nonce helper. **Owns: login routes**. No callback URL needed.
- **WS-F: Per-IP rate limiter** (`routes.rs`) — separate HashMap keyed by remote_addr, 256/min reads + 16/min asks, integrates with WS-A's filter. **Owns: rate-limit middleware section**.

**Phase 2 (parallel, depends on Phase 1):**
- **WS-G: Search/tree/glossary/folio routes** (`pyramid/html.rs`, `routes.rs`) — depends on WS-C primitives and WS-A auth filter. Each depth-capped per the verification table.
- **WS-H: Question box + /ask route** (`pyramid/html.rs`, `routes.rs`) — calls existing `handle_navigate` core logic, formats answer as HTML, CSRF-checked. Depends on WS-C, WS-E (CSRF nonce), WS-F (rate limit).
- **WS-I: ETag + cache headers** (`pyramid/html.rs`, `db.rs`) — `pyramid_revision` derivation, ETag emission, 304 short-circuit. Depends on WS-C.

**Phase 3 (parallel, depends on Phase 2):**
- **WS-J: Pretext spike + canvas client** (`assets/client.ts`, `assets/`) — Verify Pretext on npm. If real, build minimal canvas overlay that reads HTML structure, Pretext-lays it out, mounts on top with `aria-hidden=true` toggle. If not real, ship hand-rolled `canvas.measureText` overlay. Depends on WS-D for asset serving.
- **WS-K: WS client + live build animation** (`assets/client.ts`) — Connects to `/p/{slug}/ws`, drains events through rAF, animates synthesis-noise → resolved-text on the canvas overlay. Depends on WS-B, WS-J.
- **WS-L: Mercury-2 ASCII banner generation** (new `pyramid/ascii_art.rs`, lazy on first render, `pyramid_ascii_art` table) — calls Mercury-2 with apex headline, validates output (line length cap, character whitelist), caches by source hash, fallback to static template. Depends on nothing — fully parallel.

**Phase 4 (integration):**
- **WS-M: End-to-end + verification harness** — runs all 22 verification criteria as integration tests against a tmpfs SQLite fixture pyramid + a real tunnel preview if available.

### Dependency Graph (v3.2 — post B10 stub commit)
```
Phase 0.5 (sequential): SKELETON COMMIT (B10) — lands public_html/, event_bus, PyramidState fields,
                        migrations, build.rs, mount line, module decls. Single small commit.
Phase 1   (parallel): A B C D E F          all parallel against the stable skeleton
Phase 2   (parallel): G(C)  H(C,E,F,B1)   I(C)
Phase 3   (parallel): J(D)  K(B,J)        L
Phase 4   (sequential): M
```
WS-A no longer has merge-first privilege. The skeleton commit takes its place.

### File-Conflict Map (v3.2)
- `routes.rs` — touched **once** by the Phase 0.5 skeleton commit (single `.or(public_html::routes(state.clone()))` mount line) and **never again** by any workstream. Per A5.
- `pyramid/mod.rs` — touched **once** by the Phase 0.5 skeleton commit (adds `pub mod public_html;` and `pub mod event_bus;`). Never again.
- `PyramidState` — touched **once** by the Phase 0.5 skeleton commit (adds `event_bus`, `supabase_url`, `supabase_anon_key` fields). Never again.
- `pyramid/public_html/*.rs` — each file owned by exactly ONE workstream (see A5 / A14 file ownership table). Zero conflicts.
- 5 mpsc-channel-creation sites (per B3) — touched by WS-B only. Other workstreams do NOT touch `main.rs`, `routes.rs:2104/4618`, or `vine.rs`.
- `assets/` directory — content owned by WS-D (CSS, fonts, favicon, robots) and WS-J/K (client.ts). No file overlap.

---

## Contracts (Phase 0.5)

> **All Phase 0.5 contracts now live in the v3 amendments + v3.1 patches at the bottom of this document.** The earlier inline contract block was superseded across two audit rounds and has been removed to prevent stratigraphy bugs. Authoritative references:
>
> - `PublicAuthSource` enum → **A4 + B9**
> - `with_public_or_session_auth` signature → **A4**
> - `client_key()` helper → **A6 + B5**
> - `BuildEventBus` + `TaggedBuildEvent` + `TaggedKind` + `spawn_build_progress_channel` → **A3 + B3 + B18**
> - `esc()` HTML escape helper → **A14 (WS-C)**
> - `csrf_nonce` / `verify_csrf` → **A7**
> - Cookie contract (`wire_session`, `anon_session`) → **B12**
> - `web_sessions` table schema → **A2 + B14**
> - `web_session::lookup` / `sweep_expired` → **B8**
> - Static asset hashing + `build.rs` location → **A14 (WS-D) + B13**
> - Phase 0.5 skeleton commit contents → **B10**
> - Supabase URL/key sourcing → **B7**
> - Absorption-mode `_ask` flow → **B1 + B2**
> - Rate-limit buckets → **B6**
> - CSP header → **B4**
> - Mercury-2 single-flight → **A11 + B16**

If you are an implementer reading this document linearly: stop here, jump to the "v3 Amendments" section below, read it through to the end of "v3.1 Patches," and treat that as your complete and authoritative contract surface. The sections **above** this point describe motivation, aesthetic, and acceptance criteria; they do **not** contain authoritative type or function signatures.

---

## Absorption Modes (Already Implemented — V1 just consumes)

`absorb-all` is **fully wired** in the backend and we do NOT rebuild it:

- `db::get_absorption_mode(slug) -> (mode, chain_id)` returns `"open" | "absorb-all" | "absorb-selective"`
- `pyramid_slugs.absorption_mode` column with default `'open'`
- `build_runner.rs:39` `check_absorption_allowed()` enforces per-operator rate limit and daily spend cap for `absorb-all`
- Operator-set values: `absorption_rate_limit_per_operator` (default 3), `absorption_daily_spend_cap` (default 100 credits)
- `wire_publish.rs` publishes the mode in `pyramid_metadata` Wire contributions for discovery
- `pyramid_set_absorption_mode` Tauri command exists for the desktop UI

### How WS-H consumes this

```rust
// Pseudocode for /p/{slug}/ask handler
let (mode, chain_id) = db::get_absorption_mode(&conn, &slug)?;
let principal = match auth_source {
    AuthSource::Anonymous { .. } => None,
    AuthSource::WireJWT { operator_id, .. } => Some(operator_id),
    AuthSource::Local => Some(LOCAL_OPERATOR_SENTINEL),
};

match mode.as_str() {
    "open" => {
        // Operator pays. Anonymous OK, but per-IP rate limit applies.
        // Per-pyramid daily anonymous question cap (configurable, default low) protects from drain.
    }
    "absorb-all" => {
        // Visitor pays. Must be authenticated.
        let Some(operator_id) = principal else {
            return render_login_required_page(&slug);  // 401 page with link to /p/{slug}/login
        };
        // Reuse existing build_runner::check_absorption_allowed (already enforces rate + spend cap)
        super::build_runner::check_absorption_allowed(&state, &slug, &operator_id)?;
    }
    "absorb-selective" => {
        let Some(operator_id) = principal else {
            return render_login_required_page(&slug);
        };
        // Run the action chain at chain_id with the question + operator_id; honor its verdict.
        // For V1, if the chain isn't installed, fall back to "absorb-all" semantics.
    }
    _ => return render_error("unknown absorption mode"),
}

// After admission: invoke the existing navigate-internals (search + LLM synthesis)
// and format the response as HTML.
```

The WS-H implementer should NOT reimplement rate limiting or spend caps. Reuse `check_absorption_allowed` directly.

---

## Out of Scope for V1
- OpenRouter SSE token streaming
- Full folio CLI generator integration
- Returning-visitor diffs
- Question quality scoring
- Action-chain (`absorb-selective`) chain authoring UI — the runtime is in scope, the chain authoring is not
- Cross-pyramid link directory sync (links use locally-known tunnel URLs only)
- Mobile touch interactions on canvas overlay (Layer 1 HTML works fine on mobile)

---

---

## v3 Amendments (Round-2 Discovery Audit Fixes)

The round-2 discovery auditor read the v2 refined plan and found 7 P0s + 7 P1s. These amendments **supersede** the corresponding sections above. Where this block contradicts an earlier section, **this block wins**.

### A1. Magic-link redirect URL — non-issue, OTP path makes it moot
- `auth::send_magic_link` hardcodes redirect to `https://newsbleach.com/auth/wire-node-callback`. We don't change it.
- Visitor never clicks the email link. The Supabase OTP email also contains a 6-digit code; visitor types it into the OTP form on the same page; we call `auth::verify_otp` directly. Hardcoded redirect is dead path.
- **Contract:** WS-E does NOT modify `auth::send_magic_link` or `auth::verify_magic_link_token`. WS-E uses `auth::send_magic_link(supabase_url, supabase_key, email, server_port)` to trigger the email and `auth::verify_otp(supabase_url, supabase_key, email, otp_code)` to consume the code. Done.

### A2. Session storage — opaque session table, NOT JWT-in-cookie
The Supabase access_token is HS256-signed; `with_dual_auth` validates Wire JWTs as Ed25519. The two are not interchangeable. Stuffing a Supabase JWT into the cookie and hoping `with_dual_auth` accepts it would silently fail.

**Contract:** A new `web_sessions` table:
```sql
CREATE TABLE IF NOT EXISTS web_sessions (
    token TEXT PRIMARY KEY,             -- 256-bit random hex (server-generated)
    supabase_user_id TEXT NOT NULL,
    email TEXT NOT NULL,
    created_at INTEGER NOT NULL,        -- unix epoch
    expires_at INTEGER NOT NULL,        -- unix epoch
    last_seen_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_web_sessions_expires ON web_sessions(expires_at);
```
Migration is one CREATE TABLE statement; lives in WS-E.

Cookie value = the opaque `token` (NOT a JWT). Server looks it up on each request via a new helper `web_session::lookup(conn, &token) -> Option<WebSession>`. Logout = `DELETE FROM web_sessions WHERE token = ?`.

`PublicAuthSource` (see A4) has a new variant `WebSession { user_id: String, email: String }` populated from this lookup.

### A3. BuildEventBus — tagged at the build-launch site, zero producer changes
The audit confirmed: `BuildProgress` is `struct { done, total }` (no slug), `progress_tx` is per-build-invocation, not state-wide. The v2 plan's "drain task downstream of existing drain" was wrong because there is no persistent drain.

**Contract:**
```rust
// In pyramid/event_bus.rs (new module)
pub struct BuildEventBus {
    tx: tokio::sync::broadcast::Sender<TaggedBuildEvent>,
}
#[derive(Clone, Serialize)]
pub struct TaggedBuildEvent {
    pub slug: String,
    pub kind: TaggedKind,
}
#[derive(Clone, Serialize)]
pub enum TaggedKind {
    Progress { done: i64, total: i64 },
    V2Snapshot(BuildProgressV2),  // emitted on coalesce ticks
}

// In PyramidState, add:
pub event_bus: Arc<BuildEventBus>,
```

**Wiring (single point change at build launch site, NOT producer sites):**
At `build_runner.rs` and any IPC command that creates a `progress_tx`, after creating the channel we ALSO clone `state.event_bus.tx` and spawn a small relay task:
```rust
let (mpsc_tx, mut mpsc_rx) = mpsc::channel::<BuildProgress>(16);
let bus_tx = state.event_bus.tx.clone();
let slug_for_relay = slug.clone();
tokio::spawn(async move {
    while let Some(p) = mpsc_rx.recv().await {
        let _ = bus_tx.send(TaggedBuildEvent {
            slug: slug_for_relay.clone(),
            kind: TaggedKind::Progress { done: p.done, total: p.total },
        });
    }
});
// Pass mpsc_tx to existing build pipeline AS BEFORE.
```
The existing producer call sites in build.rs/build_runner.rs do NOT change. The tee happens at the relay between mpsc and broadcast.

Additionally, a state-wide "v2 snapshot tick" task polls `Arc<RwLock<BuildLayerState>>` at 4Hz when any build is active and pushes V2Snapshot events to the bus tagged with the active slug. This gives WS clients the rich layer/log/current_step view without retrofitting BuildProgress.

**Lagged handling:** WS subscriber loop catches `RecvError::Lagged(n)` → sends a `{type: "resync"}` message → continues. Client clears its local buffer and re-fetches the latest V2Snapshot via a polling fetch.

**Server-side coalescing:** Each WS subscriber owns a 60ms debounce timer; bursts collapse to 16fps max per client.

### A4. New `PublicAuthSource` type — do NOT touch existing `AuthSource`
We do NOT add an `Anonymous` variant to `routes.rs::AuthSource`. The existing 2-variant enum stays untouched. New `/p/` routes use a separate type:

```rust
// In pyramid/public_html/auth.rs (new module)
pub enum PublicAuthSource {
    Anonymous { client_key: String },              // CF-derived IP
    WebSession { user_id: String, email: String }, // from web_sessions cookie lookup
    LocalOperator,                                  // local auth_token via Authorization header
    WireOperator { operator_id: String },          // Wire JWT via Authorization header
}
```
`with_public_or_session_auth` returns `PublicAuthSource`. Existing routes are unchanged. Zero match-site updates in the 6152-line `routes.rs`.

When a `/p/` route needs to invoke an existing function that takes `AuthSource`, we adapt at the call boundary:
- `LocalOperator` → `AuthSource::Local`
- `WireOperator { operator_id, circle_id }` → `AuthSource::WireJwt { operator_id, circle_id }` (1:1 — same Wire identity, different envelope)
- `Anonymous` and `WebSession` → **NEVER** synthesized into `AuthSource::WireJwt`. They are non-Wire principals and any code path that requires a real `operator_id` for billing or rate-limiting MUST reject them at the route boundary, not paper over the gap by inventing one. (See B2 — this is what enforces the "OTP visitors cannot use absorb-all in V1" rule.)
- For tier checks on read routes, `Anonymous` and `WebSession` go through a NEW `enforce_public_tier(slug, &public_auth)` function in `public_html/auth.rs`, NOT the existing `enforce_access_tier`. The new function knows about WebSession email→circle membership and treats embargoed/circle/priced uniformly as 404 for Anonymous.

### A5. New module: `pyramid/public_html/`
ALL new HTML/WS/asset/login routes live in `pyramid/public_html/`:
```
src-tauri/src/pyramid/public_html/
├── mod.rs            (the .or() chain mounted into routes.rs)
├── auth.rs           (PublicAuthSource, with_public_or_session_auth, client_key, csrf)
├── render.rs         (esc(), layout skeleton, common HTML helpers)
├── rate_limit.rs     (per-IP limiter, separate from operator limiter)
├── routes_read.rs    (GET / handlers — home, node, tree, search, glossary, folio)
├── routes_login.rs   (login/verify/logout)
├── routes_ask.rs     (POST /ask)
├── routes_ws.rs      (WS upgrade, slug filter, lagged/resync)
├── routes_assets.rs  (static asset serving)
└── ascii_art.rs      (Mercury-2 banner generation, single-flight)
```
**`routes.rs` gets exactly ONE edit**: a single `.or(public_html::routes(state.clone(), jwt_pk.clone())).boxed()` chained into `pyramid_routes()`. Section anchor: a comment marker `// === public_html mount point ===`.

This is a **hard rule**. Any workstream that touches `routes.rs` for more than that one mount line is rejected at verifier time.

### A6. `client_key()` helper — single source of truth for "who is this requester"
```rust
// In pyramid/public_html/auth.rs
pub fn client_key(headers: &warp::http::HeaderMap, peer: Option<SocketAddr>) -> String {
    // 1. CF-Connecting-IP (cloudflared sets this when origin runs through tunnel)
    if let Some(v) = headers.get("cf-connecting-ip").and_then(|h| h.to_str().ok()) {
        return v.to_string();
    }
    // 2. X-Forwarded-For first entry
    if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) {
        if let Some(first) = v.split(',').next() { return first.trim().to_string(); }
    }
    // 3. Peer addr fallback
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".to_string())
}
```
Used by:
- `PublicAuthSource::Anonymous { client_key: ... }`
- Per-IP rate limiter key
- CSRF nonce salt for anonymous (combined with anon-session cookie below)

**Trust model:** We trust `CF-Connecting-IP` because the operator's deployment runs cloudflared which sets it. If the operator exposes the warp port directly (no tunnel), the header is absent and we fall through. We do NOT validate that the request came through cloudflared — the failure mode is "spoofable behind direct exposure," which the operator opted into.

### A7. Anonymous CSRF — opaque anon-session cookie
Plan v2 said "CSRF nonce uses remote_ip as session_id fallback." Behind the tunnel, that IP is the CF edge — same nonce for everyone. **Fix:** on first HTML render to an anonymous client, set an `anon_session` cookie with a random 128-bit value, `HttpOnly; SameSite=Lax; Max-Age=3600`. CSRF nonce HMAC keys off this value, NOT off `client_key`.

```rust
pub fn csrf_nonce(secret: &[u8], anon_or_web_token: &str, slug: &str) -> String {
    // HMAC-SHA256(secret, format!("{token}:{slug}:{epoch_minute/5}"))
}
```
For authenticated visitors, `anon_or_web_token` is the `web_sessions.token`. For anonymous, it's the `anon_session` cookie. Either way, an attacker must observe the victim's cookie to forge nonces.

### A8. SameSite=Lax (not Strict) for both cookies
Lax sends on top-level GET navigations (the magic-link click case from email, even though we don't use it) and still blocks cross-origin POST CSRF. Strict breaks too many edge cases.

### A9. Reserved-slug avoidance — `/p/_` namespace
Routes that aren't pyramid slugs use the `/p/_` prefix to avoid collision with any pyramid named `auth`, `assets`, `robots.txt`, `favicon.ico`, etc.:

| Plan v2 | v3 |
|---------|-----|
| `GET /p/auth/callback` | (removed — OTP path needs no callback) |
| `GET /p/{slug}/login` | `GET /p/{slug}/_login` |
| `POST /p/{slug}/login` | `POST /p/{slug}/_login` |
| `POST /p/{slug}/verify` | `POST /p/{slug}/_verify` |
| `POST /p/{slug}/logout` | `POST /p/{slug}/_logout` |
| `POST /p/{slug}/ask` | `POST /p/{slug}/_ask` |
| `GET /p/{slug}/ws` | `GET /p/{slug}/_ws` |
| `GET /p/robots.txt` | `GET /robots.txt` (root, not under /p/) |
| `GET /p/favicon.ico` | `GET /favicon.ico` (root) |
| `GET /assets/{file}` | `GET /p/_assets/{file}` (under /p/_ to avoid Vite collision) |

The slug parser rejects any slug starting with `_`, ensuring no collision is possible.

For other reserved sub-paths (`tree`, `search`, `glossary`, `folio`, `_*`), node IDs that match these strings are accepted because they're under `/p/{slug}/{node_id}` only when the segment doesn't match a reserved keyword. We add a small `is_reserved_subpath()` check.

### A10. ETag — per-node, not pyramid-wide
**Contract:**
```rust
fn etag_for_node(node: &PyramidNode) -> String {
    // Weak ETag from node.updated_at + node.id
    format!("W/\"{}-{}\"", node.id, node.updated_at)
}
fn etag_for_pyramid(slug_meta: &PyramidSlug) -> String {
    format!("W/\"{}-{}\"", slug_meta.slug, slug_meta.updated_at)
}
```
For pages that aggregate (tree, search, folio), use the pyramid-level ETag. For single-node pages, use the per-node ETag. WS-I confirms `pyramid_slugs.updated_at` exists; if it doesn't, WS-I adds one column via migration in the same workstream.

### A11. Mercury-2 ASCII art — operator-triggered, single-flight, NOT lazy on first render
Anonymous-triggered LLM calls = wallet drain + race conditions. Fix:
- Generation runs only on operator-issued IPC command `pyramid_generate_ascii_banner(slug)` OR as a final step in `build` pipeline (no anonymous trigger)
- Stored in new `pyramid_ascii_art (slug TEXT PK, source_hash TEXT, art_text TEXT, created_at INTEGER)`
- Single-flight via `tokio::sync::Mutex` keyed in a `DashMap<String, Arc<Mutex<()>>>` (one in PyramidState)
- Until generation completes, HTML routes serve a static template fallback with the apex headline in a plain `<pre>` box
- WS-L scope: just the generator + cache, NOT the lazy-trigger path

### A12. CSP — add connect-src for WS
```
Content-Security-Policy:
  default-src 'self';
  script-src 'self';
  style-src 'self';
  img-src 'self' data:;
  connect-src 'self' wss://*;
  frame-ancestors 'none'
```
No inline styles, no inline scripts. If the canvas client needs dynamic style injection, it does it via CSSStyleSheet API, not inline `<style>`.

### A13. Per-IP rate limit keys off `client_key()` not `remote_ip`
WS-F uses the helper. Same IP behind CF = same client. If a single user generates pathological load we accept that — Cloudflare itself rate-limits before we see it.

### A14. Cleaner workstream split — public_html module isolation

**Phase 1 (parallel, six WS):**
- **WS-A: `pyramid/public_html/auth.rs`** — `PublicAuthSource`, `client_key()`, `with_public_or_session_auth`, `csrf_nonce`/`verify_csrf`, anon-session cookie issue/read. Touches NO existing files except adding the `public_html` mod declaration in `pyramid/mod.rs`.
- **WS-B: `pyramid/event_bus.rs` + build-launch tee** — `BuildEventBus`, `TaggedBuildEvent`, the relay-tee at the single build-launch site, V2 snapshot tick task, lagged handling helper. Plus adds `event_bus: Arc<BuildEventBus>` to `PyramidState`.
- **WS-C: `pyramid/public_html/render.rs` + `routes_read.rs`** — `esc()`, layout helpers, GET handlers for `/p/`, `/p/{slug}`, `/p/{slug}/{node_id}`. Reserved-subpath check.
- **WS-D: `pyramid/public_html/routes_assets.rs` + `assets/`** — `include_bytes!` of `app.css`, font subset, favicon, robots.txt, content-hash manifest at build time, GET handlers for `/p/_assets/*`, `/robots.txt`, `/favicon.ico`.
- **WS-E: `pyramid/public_html/routes_login.rs` + `web_sessions` table** — migration, `login`, `_verify`, `_logout`, web_sessions CRUD helpers in `pyramid/db.rs` (additions only — does NOT touch existing functions). Reuses `auth::send_magic_link` + `verify_otp` unchanged.
- **WS-F: `pyramid/public_html/rate_limit.rs`** — per-IP limiter HashMap, integrates with `with_public_or_session_auth` filter via a separate `.and(rate_limit_check())` filter that all `/p/` routes chain.

**Mount point for Phase 1:**
- `pyramid/public_html/mod.rs` exports `pub fn routes(state: PyramidState) -> BoxedFilter<...>`
- ONE single edit to `routes.rs`: `.or(public_html::routes(state.clone()))` at the marked anchor
- The anchor is added by **WS-A** as the very first thing it does (so other workstreams can reference it without conflict). All other workstreams add NEW files only.

**Phase 2 (parallel, depends on Phase 1):**
- **WS-G: `routes_read.rs` extensions** — search, tree, glossary, folio handlers (depth/count caps as v2)
- **WS-H: `routes_ask.rs`** — POST `/_ask`, absorption-mode lookup, calls existing `build_runner::check_absorption_allowed`, calls existing navigate-internals (`super::query::search` + LLM synth, refactored from `handle_navigate` body into a callable function), formats answer HTML, CSRF-checked
- **WS-I: ETag + cache headers + revision sourcing** — `etag_for_node`/`etag_for_pyramid` helpers, 304 short-circuit middleware, verify/add `pyramid_slugs.updated_at`

**Phase 3 (parallel, depends on Phase 2):**
- **WS-J: Pretext spike + canvas client** (`assets/client.ts`) — verify Pretext on npm; if absent, hand-rolled canvas overlay using `canvas.measureText`. Reads HTML structure (data-* attributes injected by WS-C render), Pretext-lays it out, mounts canvas with `aria-hidden=true`, original HTML stays in DOM with `display: none` only when canvas active
- **WS-K: WS client + animation** (`assets/client.ts`) — connects to `_ws`, drains `TaggedBuildEvent` events through rAF, animates synthesis-noise → resolved-text on the canvas overlay. Handles `resync` lag-recovery
- **WS-L: Mercury-2 ASCII banner generator** (`public_html/ascii_art.rs` + `pyramid_ascii_art` table migration) — operator/build-pipeline triggered only, single-flight, validation rules

**Phase 4:**
- **WS-M: Integration + verification harness** — runs all 22 verification criteria

### A15. Final route list (v3)

| Route | Auth | Returns |
|-------|------|---------|
| `GET /p/` | anon | Public pyramids index |
| `GET /p/{slug}` | anon/session | Pyramid home |
| `GET /p/{slug}/{node_id}` | anon/session | Single node |
| `GET /p/{slug}/tree` | anon/session | Depth-capped tree |
| `GET /p/{slug}/search` | anon/session | Search results |
| `GET /p/{slug}/glossary` | anon/session | Glossary |
| `GET /p/{slug}/folio` | anon/session | Folio dump |
| `POST /p/{slug}/_ask` | anon/session, CSRF | Answer HTML; absorb-all requires session |
| `GET /p/{slug}/_login` | anon | Email entry form |
| `POST /p/{slug}/_login` | anon, CSRF | Sends OTP, renders OTP entry form |
| `POST /p/{slug}/_verify` | anon, CSRF | Verifies OTP, sets cookie, redirects |
| `POST /p/{slug}/_logout` | session, CSRF | Clears cookie |
| `GET /p/{slug}/_ws` | anon/session | WS upgrade, filtered TaggedBuildEvent stream |
| `GET /p/_assets/{file}` | anon | Static CSS/JS/font, content-hashed |
| `GET /robots.txt` | anon | `User-agent: *` + `Disallow: /pyramid/` (allow `/p/`) |
| `GET /favicon.ico` | anon | include_bytes! favicon |

### A16. Estimates dropped, but note the four real heavy WS

The audit was right that I underweighted WS-A/B/E. With the v3 amendments those are now:
- **WS-B** — broadcast bus + build-launch tee + V2 snapshot tick. Real but bounded.
- **WS-E** — web_sessions table + 4 routes + cookie helper. Bounded.
- **WS-A** — new auth filter + helpers. Bounded.
- **WS-D** — content hashing at build time + asset bundling. Bounded.

The "ship today" expectation stands because the fixes are surgical, not architectural.

### A17. Phase 0.5 spike — verify Supabase OTP loop end-to-end before WS-E
Before WS-E starts, run a 5-minute experiment:
1. Call `auth::send_magic_link` with a real email + the local Supabase URL/key
2. Confirm an email arrives containing a 6-digit code
3. Call `auth::verify_otp` with the code
4. Confirm the returned `AuthState` has `access_token` and `user_id` populated

If this fails for any reason (Supabase rate-limit, email template misconfigured, OTP not enabled), WS-E pivots to operator-token-only auth (operator pastes their existing local auth_token into the cookie via a setup page) — degraded but unblocked.

---

---

## v3.1 Patches (Round-3 Informed Audit Fixes)

These patches **supersede** any conflicting earlier text. Where this block contradicts above, **this block wins**.

### B1. Real function name: `check_absorption_rate_limit(state, slug, operator_id, estimated_cost: u64)`
The plan referenced `check_absorption_allowed` which doesn't exist. The real signature lives at `build_runner.rs:45`. WS-H pseudocode is hereby corrected:

```rust
// WS-H /_ask handler — real signatures
let (mode, _chain_id) = db::get_absorption_mode(&conn, &slug)?;

match mode.as_str() {
    "open" => {
        // Operator pays. Anonymous OK. Per-IP limit + per-pyramid daily anonymous-question cap.
    }
    "absorb-all" => {
        // Visitor pays. Requires WireOperator OR LocalOperator (NOT WebSession — see B2).
        let operator_id = match &auth {
            PublicAuthSource::WireOperator { operator_id, .. } => operator_id.clone(),
            PublicAuthSource::LocalOperator => "__local__".to_string(),
            _ => return render_login_required("This pyramid requires a Wire operator login."),
        };
        // estimated_cost: 0 for V1 (the LLM call is the cost; rate-limit is request-count-based)
        super::build_runner::check_absorption_rate_limit(&state, &slug, &operator_id, 0)?;
    }
    "absorb-selective" => {
        // V1: degrade to absorb-all gating. Action-chain runtime is V2.
        // Same gating as absorb-all branch above.
    }
    _ => return render_error("unknown absorption mode"),
}
```

### B2. WebSession visitors cannot use absorb-all in V1 (decision)
Round-3 P0-2 surfaced a real architectural mismatch: a Supabase-OTP visitor has no Wire `operator_id`, and the entire absorption/billing substrate keys on `operator_id`. **Decision: V1 forbids `absorb-all` and `absorb-selective` for `PublicAuthSource::WebSession`.** OTP-verified visitors get the same access tier as anonymous for question-asking purposes; the `WebSession` benefit is purely **read access** to `circle-scoped` and `priced` pyramids (where the visitor's email matches a Wire-side allowlist via a future mapping).

For V1, `WebSession` strictly enables:
- Reading priced/circle pyramids (if the visitor's email is on a Wire-side circle membership list — checked via a new helper that calls Wire `/api/v1/circles/membership?email=...`; if Wire is unreachable, falls back to allow only operator-Wire-JWT visitors)
- Logging out cleanly
- A "you're logged in as alice@example.com" affordance in the header

**`absorb-all` for OTP visitors is a V2 feature** that requires a Wire-wallet link step we haven't built. Plan no longer claims otherwise.

### B3. WS-B scope: `spawn_build_progress_channel()` helper, 5 launch sites
The "single-point change" was wrong. WS-B now:
1. Creates a new helper:
   ```rust
   // pyramid/event_bus.rs
   pub fn spawn_build_progress_channel(
       state: &PyramidState,
       slug: String,
   ) -> mpsc::Sender<BuildProgress> {
       let (tx, mut rx) = mpsc::channel::<BuildProgress>(64);
       let bus_tx = state.event_bus.tx.clone();
       tokio::spawn(async move {
           while let Some(p) = rx.recv().await {
               let _ = bus_tx.send(TaggedBuildEvent {
                   slug: slug.clone(),
                   kind: TaggedKind::Progress { done: p.done, total: p.total },
               });
           }
       });
       tx
   }
   ```
2. Refactors **5 mpsc channel creation sites** to use it:
   - `src-tauri/src/main.rs:3587` (IPC `pyramid_build`)
   - `src-tauri/src/main.rs:4354` (IPC decomposed build)
   - `src-tauri/src/pyramid/routes.rs:2104` (HTTP build route)
   - `src-tauri/src/pyramid/routes.rs:4618` (HTTP decomposed-build route)
   - `src-tauri/src/pyramid/vine.rs:547` (vine builder)
3. Skips `parity.rs` (testing path)

WS-B is no longer "single point" — it's "5 surgical replacements of `mpsc::channel::<BuildProgress>(64)` with `spawn_build_progress_channel(state, slug)`." Bounded but not zero-touch. The verifier checks each site and confirms no producer-side code changed.

### B4. CSP — drop `wss://*`, just `connect-src 'self'`
```
Content-Security-Policy:
  default-src 'self';
  script-src 'self';
  style-src 'self';
  img-src 'self' data:;
  connect-src 'self';
  frame-ancestors 'none'
```
`'self'` already permits same-origin WS. `wss://*` was an exfil vector through any future XSS.

### B5. `client_key()` only trusts `CF-Connecting-IP` when peer is loopback
```rust
pub fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let peer_is_loopback = peer.map(|p| p.ip().is_loopback()).unwrap_or(false);
    if peer_is_loopback {
        if let Some(v) = headers.get("cf-connecting-ip").and_then(|h| h.to_str().ok()) {
            return v.to_string();
        }
        if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) {
            if let Some(first) = v.split(',').next() { return first.trim().to_string(); }
        }
    }
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".to_string())
}
```
Cloudflared connects to the warp port from localhost. LAN-exposed deployments fall through to peer addr.

### B6. WS-F adds `_login` rate-limit bucket
Per-IP buckets:
- `/p/{slug}` reads: 256/min
- `POST /_ask`: 16/min
- **`POST /_login`: 3/min per `client_key`**, plus an additional **per-target-email cap of 10/hour** (HashMap keyed on target email, separate sweeper)

Email bombing vector closed.

### B7. Supabase URL/key sourcing — add to PyramidState at startup
```rust
// PyramidState additions (WS-B + WS-E shared)
pub event_bus: Arc<BuildEventBus>,                  // WS-B
pub supabase_url: Option<String>,                   // WS-E (read at startup from config or env)
pub supabase_anon_key: Option<String>,              // WS-E
```
Sourced from `pyramid_config.json` (or `~/.wire-node/config.json`) keys `supabase_url` / `supabase_anon_key`. If absent → WS-E `_login` route returns "OTP login not configured on this node" HTML page; everything else still works.

### B8. `web_session::lookup` enforces expiration; sweeper background task
```rust
pub fn lookup(conn: &Connection, token: &str) -> Result<Option<WebSession>> {
    // SELECT ... WHERE token = ?1 AND expires_at > strftime('%s','now')
}
pub fn sweep_expired(conn: &Connection) -> Result<usize> {
    // DELETE FROM web_sessions WHERE expires_at < strftime('%s','now')
}
```
Sweeper runs every hour from a tokio task spawned in `pyramid_state::init`.

### B9. `PublicAuthSource::WireOperator` carries `circle_id`
```rust
pub enum PublicAuthSource {
    Anonymous { client_key: String },
    WebSession { user_id: String, email: String, anon_session_token: String },
    LocalOperator,
    WireOperator { operator_id: String, circle_id: Option<String> },
}
```
Circle-scoped tier checks for Wire-JWT visitors via `/p/` now work.

### B10. Phase 0.5 stub commit (lands shared shapes BEFORE workstreams diverge)
Before Phase 1 launches, a single small "skeleton" commit lands:
- `src-tauri/src/pyramid/public_html/mod.rs` — empty module, exports a placeholder `pub fn routes(state: PyramidState) -> BoxedFilter<...> { warp::any().and_then(|| async { Err(warp::reject::not_found()) }).boxed() }`
- `src-tauri/src/pyramid/event_bus.rs` — full bus struct + helper (WS-B's contract — see B3)
- `pyramid/mod.rs` — both `pub mod public_html;` and `pub mod event_bus;` declarations
- `PyramidState` — `event_bus`, `supabase_url`, `supabase_anon_key` fields added with sensible defaults
- `routes.rs` — single `.or(public_html::routes(state.clone()))` mount line at the anchor
- New file: `src-tauri/build.rs` (crate root) — empty placeholder so WS-D can extend it
- Migrations: `web_sessions` and `pyramid_ascii_art` table CREATEs go in the existing migration framework

This commit compiles cleanly and ships nothing user-visible. WS-A through WS-F then run in parallel against a stable shared base. **No more "WS-A must finish before WS-C compiles" coordination problem.**

### B11. Route ordering — literal `_*` routes declared before `{node_id}` catchall
Inside `public_html::routes()`:
```rust
let route_chain = home
    .or(login_get).or(login_post).or(verify).or(logout)  // literal _* first
    .or(ask)
    .or(ws_upgrade)
    .or(tree).or(search).or(glossary).or(folio)            // literal sub-paths
    .or(node_view)                                          // {slug}/{node_id} catchall LAST
    .unify().boxed();
```
Plus `is_reserved_subpath(node_id)` defense-in-depth inside `node_view` returns 404 if `node_id` starts with `_` or matches `tree|search|glossary|folio`.

### B12. Cookie contract corrected (v2 line 307 said "Wire JWT raw" — wrong)
**Authoritative cookie contract:**
```
Name:    wire_session
Value:   <opaque 256-bit hex token, server-generated, looked up in web_sessions table>
HttpOnly; Secure; SameSite=Lax; Path=/p/; Max-Age=604800

Name:    anon_session
Value:   <opaque 128-bit hex token, used as CSRF nonce key>
HttpOnly; SameSite=Lax; Path=/p/; Max-Age=3600
```
Both scoped to `/p/` (not `/`) so they don't leak to the existing `/pyramid/` JSON API.

### B13. `build.rs` location — crate root, not `assets/`
Path corrected:
- `src-tauri/build.rs` (Cargo build script, crate root)
- Generates `$OUT_DIR/asset_manifest.rs` — included via `include!(concat!(env!("OUT_DIR"), "/asset_manifest.rs"))` in `pyramid/public_html/routes_assets.rs`
- The manifest declares constants:
  ```rust
  pub const APP_CSS_BYTES: &[u8] = include_bytes!("../assets/app.css");
  pub const APP_CSS_PATH: &str = "/p/_assets/app.<hash>.css";
  // ...etc for fonts, JS bundle, favicon
  ```

### B14. Datetime convention — match existing TEXT `datetime('now')` style
`web_sessions` and `pyramid_ascii_art` schemas use TEXT datetime columns to match the existing `pyramid_slugs.created_at TEXT NOT NULL DEFAULT (datetime('now'))` convention. Comparisons use `datetime(expires_at) > datetime('now')`.

Updated:
```sql
CREATE TABLE IF NOT EXISTS web_sessions (
    token TEXT PRIMARY KEY,
    supabase_user_id TEXT NOT NULL,
    email TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### B15. Refresh-token policy: explicit, no refresh
We do NOT store the Supabase refresh_token in `web_sessions`. Visitors re-login via OTP after 7 days. Documented; not a bug.

### B16. Mercury-2 single-flight uses tokio Mutex (no DashMap dependency)
```rust
// In PyramidState
ascii_art_inflight: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
```
One outer lock to fetch/insert the per-slug lock; per-slug lock to single-flight. No `DashMap` Cargo addition.

### B17. ETag 304 + cookie issue (P3-1)
HTML routes that issue `anon_session` cookies use `Cache-Control: no-store` (not just `no-cache`) to prevent any intermediary caching that might strip Set-Cookie. Asset routes (which never set cookies) keep their `Cache-Control: public, max-age=31536000, immutable`.

### B18. Broadcast capacity bumped to 4096; per-subscriber coalesce already in plan
4096 events absorbs typical bursts. Plan v3's 60ms server-side coalesce per subscriber stays.

---

---

## v3.3 Patches (Wire Pillar Conformance + ASCII Model Pivot)

These fixes resolve 3 pillar violations found by `wire-rules` against v3.2, plus fold in the user's tested finding that **Grok 4.2 (`x-ai/grok-4.20-beta`)** produces dramatically better ASCII art than Mercury-2.

### C1. ASCII art supersession (Pillars 1, 5)
The v2 plan had `pyramid_ascii_art` overwrite on apex change. That destroys intelligence-produced contributions. Fix:
```sql
CREATE TABLE IF NOT EXISTS pyramid_ascii_art (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    kind TEXT NOT NULL,                  -- 'banner' | 'topic-divider' | 'structural-diagram' | 'hero'
    source_hash TEXT NOT NULL,           -- hash of the apex headline / topic title / source structure
    art_text TEXT NOT NULL,
    model TEXT NOT NULL,                 -- 'x-ai/grok-4.20-beta' | etc
    superseded_by INTEGER REFERENCES pyramid_ascii_art(id),  -- chain to the new generation
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_ascii_art_slug_kind_head
  ON pyramid_ascii_art(slug, kind) WHERE superseded_by IS NULL;
```
WS-L generation never UPDATEs. It INSERTs a new row, then sets `superseded_by` on the previous head row in a transaction. Reads use the index to find the head (`superseded_by IS NULL`). Every banner Mercury-2 / Grok-4.2 ever generated is preserved.

### C2. Per-IP rate limit exempts authenticated principals (Pillar 8)
WS-F middleware adds a single condition:
```rust
fn should_apply_ip_limit(auth: &PublicAuthSource) -> bool {
    matches!(auth, PublicAuthSource::Anonymous { .. } | PublicAuthSource::WebSession { .. })
}
```
`WireOperator` and `LocalOperator` skip the per-IP buckets entirely. They are governed by the existing operator-keyed limiter in `with_dual_auth` and the deflationary credit physics. Layering a second governor was a Pillar 8 violation. Anonymous and WebSession still get all 3 buckets (256/min reads, 16/min `_ask`, 3/min `_login`, plus 10/hour per target email).

### C3. Preview-then-commit for paid asks (Pillar 23)
`POST /p/{slug}/_ask` becomes a two-step flow when the pyramid is in `absorb-all` (or `absorb-selective` once V2 lands):

**Step 1 — Preview** (no `commit_token` in body):
1. CSRF check.
2. Run cheap candidate search (top 5 nodes via FTS, no LLM).
3. Estimate cost: read `chains/<absorption_chain_id>` if present for an estimator, else use a default `(model_input_tokens, model_output_cap, credit_per_token)` table → integer credits.
4. Render an HTML preview page showing:
   - The question (escaped, in a `<blockquote>`)
   - The 5 candidate nodes with snippets
   - Estimated cost in credits
   - The model that will be used
   - A `<form>` with hidden inputs `commit_token=<HMAC>`, `question=<original>`, `csrf=<nonce>` and a "ASK FOR REAL — costs N credits" submit button
5. The `commit_token` is `HMAC-SHA256(server_secret, format!("{user_id}:{slug}:{question_hash}:{epoch_minute/5}"))` — short-lived, bound to the exact question and user.

**Step 2 — Commit** (POST with `commit_token`):
1. CSRF check + `commit_token` verification (constant time, current + previous 5-min window).
2. `check_absorption_rate_limit(state, slug, operator_id, estimated_cost)`.
3. Run real synthesis (existing `query::search` + LLM call).
4. Render answer HTML.

**Free mode (`open` and Anonymous-on-public)** skips Step 1 entirely — no preview is necessary because no surprise cost. The handler branches on `(mode, principal)`:
- `open` + any → straight to Step 2 (no `commit_token` needed)
- `absorb-all` + WireOperator/LocalOperator → preview→commit flow
- `absorb-all` + Anonymous/WebSession → "Wire operator token required" page (per B2)

WS-H scope updated: includes both step handlers, the preview HTML template, and the `commit_token` HMAC helper.

### C4. ASCII art generation uses Grok 4.2 directly (NOT Mercury-2)
The original handoff and vision docs were updated 2026-04-06 with a head-to-head test result: **Mercury-2 produces broken pyramid art, fails width constraints, lacks spatial reasoning**. **Grok 4.2 (`x-ai/grok-4.20-beta`) produces genuinely good ASCII art** — waterfalls with dissolving data structures, watchtowers, circuit-board topologies, multi-layer composition, respects width constraints.

**Grok 4.2 is already configured as `fallback_model_2` in `llm.rs`** but the cascade would always pick Mercury-2 first because the prompts are short. We bypass the cascade for art generation.

WS-L contract updated:
```rust
// In public_html/ascii_art.rs
const ASCII_ART_MODEL: &str = "x-ai/grok-4.20-beta";

pub async fn generate_banner(state: &PyramidState, slug: &str, apex_headline: &str) -> Result<String> {
    // Direct call — bypasses default cascade
    let prompt = build_banner_prompt(apex_headline);  // see below
    let result = call_model_direct(state, ASCII_ART_MODEL, &prompt).await?;
    validate_ascii(&result)?;  // post-hoc QA, not output prescription
    Ok(result)
}
```
Plus a new helper `call_model_direct(state, model, prompt) -> Result<String>` in `llm.rs` that skips the cascade and calls a specific model. This is a 30-line addition to llm.rs (single new public function); existing `call_model_unified` is untouched.

**Prompt framing (Pillar 37 conformant):** the prompt describes the medium constraint ("the rendering target is 72 columns wide; use box-drawing characters, block elements, and tree connectors") and the goal ("generate a thematic banner that captures the apex headline's subject matter"). It does NOT prescribe line counts or specific content. Validation is post-hoc: max line width ≤ 72, character whitelist enforced, fallback to a static template if Grok output fails validation.

**Art kinds for V1:**
- `banner` — per-pyramid hero art at the top of `/p/{slug}` (apex-headline themed)
- `topic-divider` — between major sections (topic-name themed)
- `structural-diagram` — generated lazily from notable nodes' content (deferred to V2 polish if budget runs)
- `hero` — landing page art for `/p/`

WS-L Phase 3 dependency: `null` — fully parallel.

### C5. WebSession identity is NOT a Wire pseudo-ID (Pillar 13 explicit note)
Adding to the WS-A and WS-H contracts as a hard rule:

> `web_sessions.supabase_user_id` is a Supabase identity, NOT a Wire pseudo-ID. It MUST NEVER be passed to any function that takes `operator_id`, `circle_id`, or any Wire-side identity slot. Any code path that synthesizes a Wire identity from a WebSession is a Pillar 13 violation. The B2 rule (WebSession cannot use absorb-all) is enforced at the route-handler boundary, not by clever adapter functions.

A unit test in WS-M asserts: passing a `PublicAuthSource::WebSession` into the `_ask` handler with `mode=absorb-all` returns the "Wire operator token required" page, never a synthesis call.

### C6. Handle paths in node footers (Pillar 14 clarification)
The `<footer class="prov">` block per the v2 aesthetic spec MUST render the Wire handle path when the node has been published to Wire (look up via the existing `wire_publish` mapping table). Format: `{handle}/{epoch-day}/{sequence}` per Pillar 14. For local-only unpublished nodes, fall back to the internal `node_id` but display it with a `local:` prefix to make the distinction visible:
```
version: 3 • src=2 • conf=0.87 • path=adamlevine/19847/12     ← Wire-published node
version: 3 • src=2 • conf=0.87 • path=local:L0-077            ← local-only node
```
WS-C contract updated.

### C7. Plan complete — proceed to Phase 0.5

All audit findings and pillar violations are addressed. v3.3 is the final plan.

---

*Refined v3.3: 2026-04-06*
*Source: v3.2 + wire-rules pillar check + Grok-4.2 ASCII art finding*
