# Handle-Paths: Publish-Time Only (Kills the Allocator)

**Date:** 2026-04-11
**From:** Wire backend investigation session (Adam + Claude)
**For:** Whoever's working the `.understanding/` / Self-Describing Filesystem pivot on agent-wire-node
**Supersedes:** `GoodNewsEveryone/docs/handoffs/2026-04-11-wire-handle-allocator-api.md`
**Status:** Decision locked. Node-side pivot needs to be adjusted before any more code is written against the old model.

---

## TL;DR

The previous handoff proposed a `POST /v1/handles/allocate` endpoint so `.understanding/` files could carry canonical Wire handle-paths from the moment they were created on disk. **Scrap that.** We looked at the canonical spec and the existing Wire backend, realized pre-allocation is solving a problem that doesn't exist, and found that the canonical reference model already supports what we actually need.

**The decision:**

- **No new `POST /handles/allocate` endpoint.** Don't build it. Delete the spec.
- **No new `wire_handle_allocations` table.** No TTLs, no reservation bookkeeping, no GC questions.
- **Wire's existing `insert_contribution_atomic()` stays the sole handle-path allocator.** Handle-paths are assigned atomically at publication time, exactly as the canonical `wire-handle-paths.md` spec says. That code already exists, ships in production, and has been audit-hardened.
- **Local `.understanding/` docs do NOT have handle-paths.** They live by file-path identity inside the `.understanding/` directory, which is a legal Wire Native Document reference form (`{ doc: relative/path.md }`) already defined in the canonical spec.
- **Operator onboarding requires handle registration** before any publish can happen. That's the only new thing we need and it belongs in the node's onboarding flow, not the Wire backend.

The net effect is a **simpler** pivot, not a more complex one. The original handoff invented a new endpoint, a new table, a rate-limiting scheme, a garbage-collection policy, and a client cache. All of that just vanishes.

---

## What we went through to get here

### 1. The original handoff asked the wrong question

The original handoff framed the pivot as "local docs need Wire handle-paths at creation time because publication is just attaching content to an already-allocated address." That framing made pre-allocation feel inevitable: if every doc in `.understanding/` has to look structurally identical to a published Wire doc from the moment it's written, you need the handle before the content exists.

The framing was load-bearing. Once it cracks, the whole allocator falls with it.

### 2. I checked whether the endpoint already existed

Hit `goodnewseveryone-definitive` pyramid (the Wire backend codebase, 847 nodes), `all-docs-definitive` (docs, 744 nodes), and the live source tree. Findings:

- **There is no `POST /handles/allocate` route.** Not in the codebase, not in the docs, nothing.
- **There is a `generate_daily_seq(p_agent_id UUID, p_epoch_day INTEGER) RETURNS INTEGER` SQL RPC** in `supabase/migrations/20260320100000_ux_pass_foundation.sql`. It returns `MAX(daily_seq) + 1` from `wire_contributions`, serialized by `pg_advisory_xact_lock(737, hashtext(agent_id || ':' || epoch_day))`. It was later hardened with `SECURITY DEFINER` + `search_path` in `20260321500000_security_definer_and_notices.sql`.
- **`generate_daily_seq` is called exclusively from inside `insert_contribution_atomic()`** — the atomic insert path. It is NOT exposed as a standalone endpoint. It does not exist without a contribution row being written in the same transaction.
- **The TypeScript-side `generateHandlePath()`** in `src/lib/server/wire-handle-paths.ts:59` is explicitly marked `@deprecated` with a warning: *"Do NOT call this function for new contributions. Using both this function and the SQL path creates a race condition on daily_seq."* The fallback path (line 151) throws loudly: *"generate_daily_seq RPC not found — handle-path generation requires this database function. Run the migration..."* There is no silent degradation path.

So the backend has exactly one handle-path allocator, and it is welded to the contribution-insert transaction by design.

### 3. Wire Time is documented (I was wrong to ask)

The canonical spec `GoodNewsEveryone/docs/wire-handle-paths.md` and the live code in `src/lib/server/wire-handle-paths.ts` agree exactly on Wire Time semantics:

- **Wire Time** = UTC-7, fixed, no DST (`Etc/GMT+7` in POSIX, where the sign convention is flipped)
- **Wire epoch** = `2026-01-01 00:00:00 WT` = `2026-01-01T07:00:00Z` in UTC
- **TypeScript constant:** `WIRE_EPOCH_UTC_MS = Date.UTC(2026, 0, 1, 7, 0, 0, 0)`
- **Formula:** `epoch_day = floor((now_utc_ms - WIRE_EPOCH_UTC_MS) / 86_400_000)`
- **SQL equivalent:** `epoch_day = (created_at AT TIME ZONE 'Etc/GMT+7')::date - '2026-01-01'::date`
- Today (2026-04-11) = Wire epoch_day 100. Adam's `playful/100/42` example in the original handoff is correctly dated.

Any client that needs to compute epoch_day can use either formula. Neither the node nor the Wire backend should invent a new one.

### 4. Adam clarified the three sub-questions

- **Q5 (email-form handles):** Don't accept them. Require handle registration at operator onboarding. No email fallback on either side. This is simpler than anything in the original handoff.
- **Q6 (response shape):** If there were an allocate endpoint, it would return epoch_day explicitly (Form A) rather than forcing the client to re-derive from server_time. Keeps client dialects (Rust vs TS) from drifting. **Moot under the new decision — there is no response shape because there is no endpoint.**
- **Q8 ("only good for the hour"):** Adam had been thinking of hour-level tracking but the real model is day-scoped: `daily_seq` is the user's contribution number *that day*, and it is permanent and monotonic. No TTL, no reclamation, no ghosts. This is what the canonical spec already says.

### 5. The realization that broke pre-allocation

Mid-answer on Q8, Adam noticed: "our plan grabs handles at creation not publishing, which seems like it'll be confusing." That instinct was right. Here is exactly what's confusing about pre-allocation, laid out:

1. **Sequence number stops meaning what the spec says it means.** Per `wire-handle-paths.md:37-42`, sequence is "nth contribution by this agent on this epoch-day" and conveys "relative volume — how busy the agent was that day." Under pre-allocation, it becomes "nth reservation," which might be the 2nd thing ever published, or nothing. The volume signal breaks. The spec has to be rewritten.
2. **404 becomes ambiguous.** `playful/100/7` could be "no such path" OR "reserved draft, not yet published" — two different states that need different UX. Right now there's only one 404 state.
3. **Permanent gaps in public history.** Draft 10 things, publish 2 of them at seqs 7 and 9, you now have 8 visible holes in your public timeline. Forever.
4. **Privacy leak.** The server has to track your drafts to guarantee non-reuse of reserved sequences. "playful allocated 10 handles at 3pm" is metadata you just published to the database.
5. **A whole new failure mode class.** The original handoff spent sections on rate limits, garbage collection (the "open question"), idempotency, and a client-side cache with refill logic. Every one of those is a knob that can break. None exist in the publish-time model.

### 6. The key thing I almost missed — canonical spec already solves this

The canonical `wire-handle-paths.md:60-68` shows three legal reference forms for `derived_from`:

```yaml
derived_from:
  - { ref: "nightingale/77/3", weight: 0.5 }             # handle-path
  - { doc: wire-actions.md, weight: 0.3 }                 # file path (local corpus doc)
  - { corpus: "wire-docs/wire-actions.md", weight: 0.2 }  # corpus path (remote corpus doc)
```

**Local docs can already cite each other by file path.** This is not a hack, not a workaround, not a compromise. It's in the canonical spec. A `.understanding/foo.md` that cites `.understanding/bar.md` via `{ doc: bar.md }` is a valid Wire Native Document — the file is self-describing, carries full YAML rear-matter, is indistinguishable from a publishable Wire doc except that its global handle-path hasn't been minted yet.

The original handoff was treating "doesn't have a handle-path" as if it disqualified a file from being a Wire Native Document. It doesn't. The spec already allows for the two-world state (local + published) and tells you how to reference across it.

---

## Why publish-time wins on every axis

| Axis | Pre-allocation (old plan) | Publish-time (new plan) |
|---|---|---|
| Wire backend changes | New route + new table + TTL + rate limit + GC + idempotency | **Zero** |
| Client-side bookkeeping | Cache, refill threshold, local state file, migration sweeper | **None** |
| Canonical spec changes | Sequence semantics rewritten, add "reserved" state, change 404 meaning | **None** |
| Monotonic sequence invariant | Broken (reservations are not publications) | **Preserved** |
| Privacy of drafts | Server must track them | **Not server's problem** |
| Permanent gaps in public history | Yes | **No** |
| Multi-device draft collision | Solved via allocator serialization | **Solved via file-path identity** (already how filesystems work) |
| Local doc ↔ published doc identity | Identical address from day zero (the supposed benefit) | **Different**: local uses path, published uses handle-path. Rewritten at publish. |

The only thing we give up is "a local draft has a stable Wire address before publication." We traded that for everything in the rest of the table. I cannot construct a user-facing scenario where the loss matters — drafts are private, and private drafts don't need globally-resolvable addresses. Across devices, file paths are a perfectly good identity if you're syncing the files themselves.

If a use case surfaces later that genuinely needs pre-allocation (agent-to-agent draft sharing? pre-declared question slots? some Vibesmithy spatial UX thing?), we build the allocator *then*, designed around that actual use case instead of a speculative one.

---

## What this means for the node pivot — concrete work

The `.understanding/` Self-Describing Filesystem pivot was already in motion. Here's what changes and what doesn't:

### Delete
- **Drop `WireHandleAllocator` trait and both impls.** There is no allocator to wrap. The `HandleAllocator` abstraction in the original handoff was serving a role that doesn't exist.
- **Drop `LocalHandleAllocator`.** No local synthetic handles either. Local docs don't have handle-paths, period.
- **Drop the "migrate local-sourced handles to Wire handles" one-shot sweeper.** Nothing to migrate.
- **Drop the Wire backend handoff** (`GoodNewsEveryone/docs/handoffs/2026-04-11-wire-handle-allocator-api.md`). This document supersedes it. I recommend marking the original file as superseded rather than deleting it, so future agents understand the decision history.

### Change
- **Wire Native Document rear-matter for local docs:** no `handle_path` field. Or: explicit `handle_path: null` + `state: local` if the rear-matter schema insists on the key. The canonical `wire-native-documents.md` spec needs to answer whether that field is required or optional — read it carefully before you commit to one or the other.
- **Citations between local docs:** use `{ doc: relative-path }` form, per `wire-handle-paths.md:60-68`. This is the canonical spec's own example. Citations to already-published Wire contributions stay as `{ ref: "handle/day/seq" }`.
- **Publish pipeline:** when a local doc is published, the existing `insert_contribution_atomic()` call assigns its handle-path server-side. No change to the Wire API call. What *does* need to change: before the publish, walk the local doc's `derived_from` and rewrite any `{ doc: relative-path }` citation that points to another doc **being published in the same batch** into a `{ ref: "handle/day/seq" }` using the handle-path that the cited doc received from its own `insert_contribution_atomic()` call. Citations to local-only docs (not in the publish batch) stay as doc-refs and resolve to corpus documents on the Wire side — also legal per the spec.
- **Publish ordering matters.** If `bar.md` cites `foo.md` and both are being published together, `foo.md` must publish first so `bar.md`'s citation can be rewritten with `foo.md`'s freshly-minted handle-path. Build a dependency-ordered publish pass. This is simpler than it sounds: topological sort on the `derived_from` graph restricted to docs in the publish batch.
- **Operator onboarding:** add a handle-registration step. The node cannot publish anything until the operator has an active handle on the Wire backend. The backend currently throws *"Agent has no active handle — register a handle first"* if the insert runs without one, so this is a user-experience requirement, not a new constraint. Get it in front of the user *before* they try to publish, not after the first attempt fails.

### Keep
- **Everything else about the `.understanding/` pivot.** Files are canonical, SQLite is derived cache, every file is a Wire Native Document with YAML rear-matter. All of that is untouched.
- **Multi-device draft sync by file syncing.** Use whatever sync mechanism you were already planning (git, rsync, CRDT, whatever). File paths are stable identities across devices as long as the files themselves are stable.
- **Wire backend.** Truly nothing changes on the Wire side. `insert_contribution_atomic()` already does exactly what we need. `generate_daily_seq()` is already hardened. The resolve endpoint `/api/v1/wire/contributions/resolve/{handlePath}` stays as-is.

---

## Things I did not resolve and you need to decide

1. **`handle_path` field in local rear-matter — absent, null, or `state: local`?** Depends on how strict `wire-native-documents.md` is about the field. Read that spec's YAML schema section before choosing. Whichever you pick, use the same flag everywhere so the local/published state is a single bit.
2. **What happens when a local doc cites a local doc that is *never* published?** Under the new model, that citation stays as `{ doc: relative-path }` forever, which means when the parent doc publishes, its `derived_from` still contains a local file-path reference. The Wire backend will store it as a doc-ref citation. Confirm with the Wire spec (`wire-native-documents.md` + `wire-handle-paths.md` + whatever handles `derived_from` validation) that this is legal. If the backend rejects doc-ref citations for published docs, you need a pre-publish validator that surfaces "these local-only citations will be dropped or must be published alongside." My read of the spec is that it's legal, but verify.
3. **Cross-device publish race.** Two devices each publish a local doc to Wire simultaneously. `generate_daily_seq` serializes the sequence allocation so they don't collide — that's already handled server-side. But if both devices draft locally and assign overlapping *file paths*, you have a local conflict to resolve before anything touches Wire. This is filesystem sync territory, not Wire territory. Whatever sync mechanism the pivot chooses handles this.
4. **Onboarding UX for handle registration.** Needs frontend design. What does the user see, what does the flow look like, what happens if they refuse? Flag this as a UX workstream in the pivot plan — not as an afterthought attached to the backend spec.

---

## Annotations to push back to the pyramid

Before this work gets lost to session end, add these annotations to `goodnewseveryone-definitive`:

1. **On `L0-396` (Programmable Intelligence Build):** annotation explaining that `insert_contribution_atomic()` being the sole handle-path allocator is a *feature*, not a gap — and that pre-allocation schemes proposed by client-side pivots should be rejected at spec-review time. Generalized understanding: handle-path monotonicity is an invariant, not an implementation detail, and any client-side request for pre-allocation is a signal that the client is conflating "needs Wire-native rear-matter" with "needs a globally-resolvable address."
2. **On `Q-L0-568` (Handle-Path Identity):** annotation reinforcing that local-only Wire Native Documents are legal and cite other local docs via file-path `derived_from` entries. Generalized understanding: "Wire Native Document" and "published contribution" are not the same state — the rear-matter schema is the same, the identity scheme is different, and the spec supports both simultaneously.
3. **On `L0-807` (20260321500000 Security Definer And Notices):** annotation that `generate_daily_seq` has been audit-hardened and the client side should never attempt to replicate its logic locally. The `@deprecated` TypeScript `generateHandlePath()` exists only as a failure-mode reminder, not an alternative path.

I'll do this in follow-up unless you want me to push them now.

---

## Related reading

- Canonical handle-path spec: `GoodNewsEveryone/docs/wire-handle-paths.md`
- Wire Native Document format: `GoodNewsEveryone/docs/wire-native-documents.md` (needs a re-read to confirm `handle_path` field requiredness)
- Current live allocator code: `GoodNewsEveryone/src/lib/server/wire-handle-paths.ts`
- Live SQL migrations: `GoodNewsEveryone/supabase/migrations/20260320100000_ux_pass_foundation.sql` and `20260321500000_security_definer_and_notices.sql`
- SDFS vision (why `.understanding/` exists): `agent-wire-node/docs/vision/self-describing-filesystem.md`
- The now-superseded allocator handoff: `GoodNewsEveryone/docs/handoffs/2026-04-11-wire-handle-allocator-api.md`
- Today's folder-nodes pivot (the adjacent in-flight work this interacts with): `agent-wire-node/docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`
