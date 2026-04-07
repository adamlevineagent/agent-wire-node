# Post-Agents-Retro: "Private" as the Default Tier

> Fast-follow design after V1 ships. Adds a `private` access tier as the new default for every pyramid, making the public web surface opt-in. Existing pyramids are migrated, the desktop UI gets a "Make Public" affordance, and `LocalOperator` (the operator via "Open as Owner") still sees private pyramids.

---

## The change in one sentence

`access_tier` gains a fifth value `private`, becomes the default for every newly-created pyramid, and means: invisible at `/p/` and `/p/{slug}` to anonymous + WebSession + WireOperator visitors. Only the local operator (you, via "Open as Owner") sees it.

---

## Why this is the right framing

**The user's instinct is correct**: "publication" is a Wire-side concept (publishing nodes as contributions to the Wire marketplace), but the visibility decision on the **web surface** is a different concern that deserves its own primitive. Conflating them creates two problems:

1. **Pyramids you've published to Wire might still not be ready for the public web** (e.g., you published a draft to test the wire flow but the headlines aren't polished)
2. **Pyramids that you'll never publish to Wire might still be appropriate for the web** (e.g., a personal codebase pyramid you want to share with friends via tunnel URL but never put on the marketplace)

The cleanest model: publication and web-visibility are **independent decisions**, both controlled by the operator, both with sensible safe defaults.

The `private` tier as default makes the web surface **opt-in**, which is the right safety posture for a system that just gained public-internet exposure. No accidental disclosure: you have to consciously click a button to make a pyramid visible.

---

## Schema change

```sql
-- One-line idempotent migration in db.rs init_pyramid_db
-- (no DEFAULT change needed; we set the default in code so existing rows are preserved)
-- The check constraint just gains a new value.
-- For SQLite: no enum type, so this is documentation-level. We just accept 'private' in code.
```

**No migration runs against existing rows.** Pyramids that you've already created will stay at whatever tier they're at (default `'public'` from the original migration). This is intentional — flipping every existing pyramid to private would surprise the user with disappeared pyramids on the next restart.

**For new pyramids** (post-fast-follow), the default is `'private'`. This requires changing the SQL default in the migration **AND** the application-side default in `db::create_slug` (or wherever new slugs are inserted). Both:

```rust
// db.rs migration (additive, idempotent)
let _ = conn.execute(
    "ALTER TABLE pyramid_slugs ADD COLUMN access_tier TEXT NOT NULL DEFAULT 'private'",
    [],
);
// (Existing slugs keep their value because the column already exists.
// New slugs created without an explicit access_tier get 'private'.)

// In every INSERT INTO pyramid_slugs site, ensure access_tier is explicitly set
// to 'private' on creation, so behavior matches schema.
```

(In practice the existing migration line stays; we just add `'private'` to the validation enum and change the application-side default to `'private'` for new INSERTs.)

---

## Code changes

### 1. Validation enum (Tauri command)

`src-tauri/src/main.rs:4122-4130` — add `private` to the validator:

```rust
match tier.as_str() {
    "private" | "public" | "circle-scoped" | "priced" | "embargoed" => {}
    _ => return Err(format!(
        "Invalid access tier '{}'. Must be one of: private, public, circle-scoped, priced, embargoed",
        tier
    )),
}
```

### 2. `enforce_public_tier` (web surface, anti-enumeration)

`src-tauri/src/pyramid/public_html/auth.rs:392`:

```rust
match tier.as_str() {
    "public" => Ok(()),
    "private" => match auth {
        // Only the local operator sees private pyramids on the web.
        // WireOperator does NOT, even though they have a Wire JWT —
        // private means private to THIS node's operator.
        PublicAuthSource::LocalOperator => Ok(()),
        _ => Err(TierRejection::NotPublic),
    },
    "priced" | "embargoed" => match auth {
        PublicAuthSource::LocalOperator | PublicAuthSource::WireOperator { .. } => Ok(()),
        _ => Err(TierRejection::NotPublic),
    },
    "circle-scoped" => { /* unchanged */ }
    _ => { /* unknown → embargoed */ }
}
```

### 3. `enforce_access_tier` (existing JSON API)

`src-tauri/src/pyramid/routes.rs:234` — the existing `/pyramid/...` JSON API also needs to know about private:

```rust
match tier.as_str() {
    "public" => return Ok(()),
    "private" => {
        // JSON API is operator-facing. Local always allowed; Wire JWT
        // visitors are denied because private means private to local.
        match source {
            AuthSource::Local => return Ok(()),
            _ => return Err(/* 451 or 404 */),
        }
    }
    // ... rest unchanged
}
```

### 4. `/p/` index handler — filter by tier

`src-tauri/src/pyramid/public_html/routes_read.rs::handle_index` — currently lists all slugs filtered by `access_tier == 'public'`. After this change:

- For Anonymous / WebSession / WireOperator visitors: show only `access_tier == 'public'` (unchanged behavior)
- For LocalOperator: show ALL slugs regardless of tier, with a small `[private]` / `[priced]` / `[embargoed]` badge next to each slug name so you can see the full inventory in owner mode

### 5. New slug creation default

Find every `INSERT INTO pyramid_slugs (...)` site in the codebase. There are several:
- `db::create_slug` (or equivalent)
- The Tauri command that creates a pyramid
- The corpus-import path
- The pinned-from-remote path (these stay as their imported tier)
- Test fixtures (leave alone)

Each of these (except pinned-from-remote and tests) gets an explicit `access_tier = 'private'` on insert.

### 6. Desktop UI: "Make Public" button

`src/components/PyramidPublicationStatus.tsx` — the Access Tier section already has a dropdown for `public | priced | circle | embargoed`. Add `private` as a fifth option AND make it the visible default state for new pyramids.

Plus a more prominent "Make Public" affordance for the common case: a button that does `pyramid_set_access_tier(slug, "public")` in one click without opening the access-tier expander.

```tsx
{/* Quick "Make Public" toggle for pyramids in private mode */}
{accessTiers[p.slug]?.access_tier === "private" && (
    <button
        className="folder-publish-btn"
        onClick={async () => {
            await invoke("pyramid_set_access_tier", {
                slug: p.slug,
                tier: "public",
                price: null,
                circles: null,
            });
            // Refresh
            handleExpandAccessTier(p.slug);
        }}
        title="Make this pyramid visible at the public /p/ URL"
    >
        Make Public
    </button>
)}
```

The "Open as Owner" button continues to work for private pyramids — owner mode bypasses the tier check via the LocalOperator sentinel.

### 7. Wire metadata publication

`src-tauri/src/pyramid/wire_publish.rs:596` (`publish_pyramid_metadata`) currently sends `absorption_mode` to the Wire. It also sends `access_tier`. Question: should `private` pyramids be advertised on the Wire at all?

**Decision**: Yes, but with `access_tier: "private"` in the metadata. The Wire is a discovery/coordination layer; advertising a private pyramid says "this exists, this is who owns it, but you can't read it without the operator opening it up." Other operators might use this to coordinate (e.g., "I see Adam has a private pyramid about X; I'll DM him"). The body of the pyramid is never sent; only the metadata.

If you'd rather treat `private` as fully invisible (don't even tell Wire it exists), we can flip this — the metadata publish call just becomes a no-op for `tier == "private"`. Lower-friction default but slightly less useful. We'll go with "advertise the metadata, hide the body" unless you say otherwise.

### 8. Migration of existing pyramids — DO NOTHING automatically

Existing pyramids stay at their current `access_tier`. No surprise visibility loss, no surprise visibility gain. The user can flip them via the new UI.

**However**, on first launch of the new binary, we'll show a one-time toast notification: "New: pyramids now default to **private**. Your existing pyramids are unchanged. Use the **Make Public** button on the dashboard to publish any pyramid to your tunnel URL." The toast self-dismisses after 30s and doesn't repeat.

---

## Acceptance criteria

1. New pyramid created via desktop UI → `pyramid_slugs.access_tier = 'private'`
2. `curl https://<tunnel>/p/{private-slug}` (anonymous) → 404
3. `curl https://<tunnel>/p/` (anonymous) → does NOT list private pyramids
4. Click "Open as Owner" on a private pyramid → opens at `/p/{slug}` successfully (LocalOperator sentinel kicks in)
5. `curl https://<tunnel>/p/` via owner-mode session cookie → DOES list private pyramids with `[private]` badge
6. Click "Make Public" → tier flips to `'public'`, ETag is bumped via `touch_slug`, next anonymous request returns 200
7. Existing pyramids that were previously `'public'` remain visible to anonymous (no migration regression)
8. Wire metadata for a private pyramid still publishes (with `access_tier: "private"` in the contribution)
9. WireOperator (some other operator hitting via Wire JWT) on a private pyramid → 404 (private = private to YOU, not to "any authenticated visitor")
10. The new V1 verification criteria from `post-agents-retro-web-refined.md` continue to pass for the public/priced/circle/embargoed tiers (regression check)

---

## Estimated scope

- 1 migration line change (or no change if existing column accepts the new value)
- 5 enum validator updates (1 in main.rs, 1 in routes.rs, 1 in auth.rs, 1 in routes_assets/wire_publish.rs maybe, 1 in tests)
- 1 helper to filter list_slugs for the index page (LocalOperator vs others)
- 1 React button + 1 dropdown option
- 1 toast on first launch
- 4-5 tests added to the existing harness (private + LocalOperator → visible, private + Anonymous → 404, etc.)

Single fixer agent. Probably 20-30 minutes of focused work after you confirm the design.

---

## Open questions for you

1. **Wire metadata for private pyramids**: advertise existence (default in this draft) or fully hide?
2. **`circle-scoped` tier semantics**: currently a Wire JWT visitor with matching `circle_id` can read circle-scoped pyramids. With `private` introduced, should `circle-scoped` ALSO require LocalOperator first by default, OR keep its current "Wire JWT with matching circle" behavior? (My recommendation: leave as-is; the operator opted into circle-scoped explicitly, so they meant for circle members to see it.)
3. **Migration toast**: nice-to-have or skip and put it in the changelog only?
4. **One-click "Make Public" placement**: I sketched it next to "Open as Owner". Better placement?

---

*Drafted: 2026-04-06*
*Status: design complete, awaiting your test of the V1 surface, then implementation as a fast-follow*
