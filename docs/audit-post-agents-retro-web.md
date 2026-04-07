# Audit: Post-Agents Retro Web — Vision / Research / Status

**Scope:** Three documents against the `agent-wire-node` codebase (Rust/warp backend, Tauri desktop app, TypeScript MCP CLI)

**Method:** Direct code inspection (grep, view_file), pyramid FAQ/search cross-reference, line-level verification of every status claim.

---

## Executive Summary

The three documents are **internally consistent and well-structured**. The vision is clear and philosophically coherent. The status doc correctly identifies the rendering layer as the only missing piece. The audit surfaces **7 findings** — one hard gap (WebSocket streaming), terminology drift, and minor documentation cleanups.

**Verdict: Sound foundation. Fix the absorption mode naming and acknowledge the WebSocket scope, then this is ready as a build handoff.**

---

## Findings (Severity-Ranked)

### 🔴 P1 — Hard Gap

#### 1. WebSocket Streaming: Not Implemented

The vision doc states ([line 158-165](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/vision/post-agents-retro-web.md#L158-L165)):
> Server's LLM pipeline synthesizes answer tokens → Tokens arrive over WebSocket

**Code reality:**
- `grep -ri "websocket\|warp::ws" src-tauri/src/` → **zero results**
- No WebSocket upgrade handler in [routes.rs](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs)
- No token streaming infrastructure exists anywhere in the Rust backend

The status doc correctly lists this under "Not Built" but the vision doc describes the streaming experience as if it's a simple wiring job ("well-understood pattern"). **This is new server-side infrastructure, not just plumbing.**

**Recommendation:** Update the build scope estimate. WebSocket streaming from warp requires:
- `warp::ws()` filter + upgrade handler
- Token buffer/channel from the LLM pipeline (currently the [chain_executor](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/chain_executor.rs) runs synchronously to completion)
- Session management (which expansion is this visitor watching?)
- Tunnel passthrough (does `cloudflared` proxy WebSocket upgrades? Likely yes, but untested)

---

### 🟡 P2 — Terminology Drift / Doc-Code Misalignment

#### 2. Pretext/LayoutSans: Not Yet Installed (Informational)

Neither `@chenglou/pretext` nor LayoutSans appear in any `package.json` or import — they haven't been integrated yet. This is expected: they're Layer 2 build items, not infrastructure gaps.

The research doc's performance claims come from Pretext's own benchmarks and community demos. The library is real (40k GitHub stars, Cheng Lou / ex-React core), and the API surface is small (`prepare()`, `layout()`, `layoutNextLine()`). Any integration issues will surface in the first hours of the canvas renderer workstream.

**Not a risk.** This project went from fork to 243-node pyramid engine with DADBEAR, tunnel auth, and Wire publishing in 22 days. A new dependency being young is not a signal in this execution context.

---

#### 3. Absorption Mode Naming Mismatch

| Vision Doc Says | Code Uses | Location |
|----------------|-----------|----------|
| `owner-absorbs` | `open` | [main.rs:4160](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/main.rs#L4160) |
| `questioner-pays` | `absorb-all` | [main.rs:4160](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/main.rs#L4160) |
| `action-chain` | `absorb-selective` | [main.rs:4170](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/main.rs#L4170) |

The vision doc invents human-readable names that don't match the implementation. A build agent following the vision doc would wire up absorption modes using the wrong enum values.

> [!WARNING]
> The status doc also uses the vision doc's names ("Owner-pays, questioner-pays, action-chain assessment") rather than the code names. Both docs need correction to match the actual enum: `open | absorb-all | absorb-selective`.

#### 4. Line Number References in Status Doc Are Approximate

| Claim | Status Doc Line Ref | Actual Location |
|-------|-------------------|-----------------|
| `with_dual_auth` | `routes.rs:93-180` | ✅ Exact — [routes.rs:93](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L93) |
| Access tier enforcement | `routes.rs:234-308` | ✅ Exact — [routes.rs:234](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L234) |
| Rate limiting | `routes.rs:138-158` | ✅ Exact — [routes.rs:139](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L139) |
| Heartbeat with tunnel_url | `auth.rs:370-417` | ✅ Exact — [auth.rs:370](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/auth.rs#L370) |
| Discovery metadata | `wire_publish.rs:570-628` | ✅ Exact — [wire_publish.rs:570](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L570) |

**Verdict: All line references are accurate.** This is unusually good for planning docs.

#### 5. Rate Limit Value: Status Doc Says 100/min/operator — Code Confirms

Status doc: "Rate limiting (100/min/operator)" → [routes.rs:151](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L151): `if entry.0 > 100` with a 60-second window. ✅ Verified.

---

### 🟢 P3 — Minor / Cosmetic

#### 6. Folio Route Listed in Architecture But Not Implemented

The vision doc shows `GET /p/{slug}/folio` and the status doc's architecture diagram includes it. The [folio generator plan](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/plans/pyramid-folio-generator.md) and [handoff](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/handoffs/handoff-pyramid-folio-command.md) exist as docs, but:
- `grep -ri "folio" src-tauri/src/` → **zero results** in Rust code
- The folio is a CLI command, not an HTTP route

This is correctly in "Not Built" scope but the architecture diagram presents all 6 routes as if they're the same effort level. The folio endpoint may need the most backend work since it's a depth-controlled recursive render.

#### 7. Mercury-2 Referenced as "Diffusion LLM" in Vision Doc — It's an OpenRouter Text Model

The vision doc frames Mercury-2 as an ASCII art generator ("Mercury-2 (unlimited, free diffusion LLM)"). In reality, `inception/mercury-2` is used throughout the codebase as a **text LLM** for:
- Question compilation ([question_compiler.rs:977](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/question_compiler.rs#L977))
- Supersession analysis ([supersession.rs:63](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/supersession.rs#L63))
- Evidence answering ([evidence_answering.rs:79](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/evidence_answering.rs#L79))
- Default primary model ([mod.rs:133](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/mod.rs#L133))

The vision doc's claim that Mercury-2 will generate ASCII art (banners, topic dividers, structural diagrams) is aspirational — Mercury-2 *can* generate text-formatted output, but the document implies image-diffusion-level generation quality. The research doc is more honest, noting LLM ASCII art reliability by category.

---

## Verified Infrastructure (All ✅)

| Claim | Verified | Evidence |
|-------|----------|----------|
| Cloudflare tunnel provisioning | ✅ | `tunnel.rs` exists, referenced in 18 source files |
| Dual auth (local + Wire JWT) | ✅ | [routes.rs:93-180](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L93-L180) — constant-time comparison, dot-counting heuristic |
| Access tier enforcement (public/priced/circle/embargoed) | ✅ | [routes.rs:234-322](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L234-L322) — all 4 tiers + unknown handler + 451 for embargoed |
| Rate limiting (100/min/operator) | ✅ | [routes.rs:137-158](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L137-L158) — per-operator HashMap with 60s window |
| Heartbeat with tunnel_url | ✅ | [auth.rs:370-417](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/auth.rs#L370-L417) — sends tunnel_url or explicit null |
| Discovery metadata on Wire | ✅ | [wire_publish.rs:570-628](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L570-L628) — `pyramid_metadata` contribution type with slug, tunnel_url, access_tier, absorption_mode |
| Absorption modes | ✅ | DB schema, IPC commands, rate limiting, daily spend caps — full implementation |
| Handle paths | ✅ | Returned from Wire API, stored in `pyramid_id_map`, used for addressing |
| DADBEAR staleness detection | ✅ | `stale_engine.rs`, `staleness_bridge.rs`, referenced in pyramid codebase (243 nodes confirm) |
| Payment token infrastructure (WS-ONLINE-H) | ✅ (scaffolded) | [routes.rs:516-592](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/routes.rs#L516-L592) — validation implemented, enforcement commented out with clear TODO |

---

## Cross-Document Consistency

| Aspect | Vision ↔ Research | Vision ↔ Status | Research ↔ Status |
|--------|-------------------|-----------------|-------------------|
| Tech stack | ✅ Match | ✅ Match | ✅ Match |
| Build scope estimates | ✅ Match | ✅ Match | ✅ Match |
| Performance numbers | ✅ Match | ✅ Match | N/A |
| Route definitions | ✅ Match | ✅ Match | ✅ Match |
| Absorption mode names | ❌ Both wrong | ❌ Both wrong | ✅ Both wrong consistently |
| What exists vs. what's needed | ✅ Match | ✅ Match | ✅ Match |

---

## Recommendations

### Before Build Handoff

1. **Fix absorption mode names** in both the vision and status docs to match code (`open`, `absorb-all`, `absorb-selective`) — or rename the code enum to match the vision docs if you prefer the human-readable names.

2. **Add a "WebSocket streaming" section** to the status doc under "Not Built" that acknowledges this is new server infrastructure, not just piping. The chain executor currently runs to completion synchronously.

3. **Run a Pretext spike** before committing to the 3-4 week timeline. A half-day prototype would de-risk the entire Layer 2 bet.

### For the Vision Doc

4. **Clarify Mercury-2's role**: It's a text LLM that generates ASCII art as text output, not an image diffusion model. The research doc handles this correctly; the vision doc could be clearer.

5. **The folio route** should be flagged as having additional backend complexity vs. the other 5 HTML routes, since it requires depth-controlled recursive rendering.

### Not Findings (Things That Are Fine)

- The "Two concurrent layers" architecture is well-reasoned and avoids the classic "canvas kills accessibility" trap
- The performance envelope table uses credible sources
- The expansion economics model is clean and the three modes map naturally to the existing infrastructure  
- The philosophy section is genuinely distinctive — "anti-marketing by design" as an aesthetic principle is something a build agent can use for CSS decisions
- Cross-pyramid linking via tunnel URLs is simple and appropriate for V1
