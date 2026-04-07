# Remote Pyramid Access — Infrastructure Status

> Assessment of what's needed to serve pyramids as the post-agents-retro web surface
> via Cloudflare tunnel. Updated 2026-04-06 after code inspection.

---

## Status: Ready to Build the Rendering Layer

All infrastructure for remote pyramid access is operational. The only missing piece
is the web rendering layer — HTML routes, the Pretext/LayoutSans canvas client, and
a WebSocket handler to forward existing internal progress events.

### Fully Operational Infrastructure

| Capability | Status | Location |
|-----------|--------|----------|
| Cloudflare tunnel provisioning | ✅ | `tunnel.rs` — download, provision, start, monitor, persist |
| Dual auth (local + Wire JWT) | ✅ | `routes.rs:93-180` — `with_dual_auth` filter |
| Access tier enforcement | ✅ | `routes.rs:234-308` — public/priced/circle/embargoed |
| Rate limiting (100/min/operator) | ✅ | `routes.rs:138-158` — per-operator, per-tunnel |
| Heartbeat with tunnel_url | ✅ | `auth.rs:370-417` — reports tunnel_url to Wire server |
| Discovery metadata on Wire | ✅ | `wire_publish.rs:570-628` — pyramid_metadata contributions |
| Question → expansion pipeline | ✅ | Fully wired and working locally |
| Absorption modes | ✅ | `open`, `absorb-all`, `absorb-selective` — DB schema, IPC commands, rate limiting |
| Handle paths for URLs | ✅ | Universal addressing, deep-linkable |
| DADBEAR staleness detection | ✅ | Running, maintains pyramid freshness |
| Build progress event stream | ✅ | `mpsc::channel<BuildProgress>` — 40+ event types across build pipeline |
| Warp WebSocket support | ✅ | `warp = "0.3"` includes `warp::ws()` natively |
| OpenRouter SSE parsing | ✅ | `parse_openrouter_response_body` handles `data:` prefix stripping |

### Not Built (V1 Scope)

| Component | Effort | Notes |
|-----------|--------|-------|
| Warp HTML routes | Days | 6-8 endpoints returning semantic HTML |
| CSS retro aesthetic | Days | The creative feel |
| Pretext + LayoutSans client | Days | Install, wire to pyramid data |
| Canvas renderer with ASCII aesthetic | 1-2 weeks | Creative/engineering core |
| WebSocket handler | 1 day | Forward existing `BuildProgress` events via `warp::ws()` |
| WebSocket token streaming | 1 day | Add `"stream": true` to `call_model_unified`, parse SSE chunks |
| Mercury-2 ASCII art generation | 1 week | Per-pyramid banners, topic dividers |

### V2 (After Identity Ships)

- Email/magic-link visitor identity on web surface
- Questioner-pays (`absorb-all`) absorption mode for web visitors
- Action chain (`absorb-selective`) question assessment
- Returning-visitor diffs
- Question quality scoring
- Overlay lenses

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│  YOUR MACHINE                                               │
│                                                             │
│  Wire Node (Tauri app, port 8765)                           │
│   ├── Layer 1: Semantic HTML routes (warp, server-rendered) │
│   │    ├── /p/{slug}                                        │
│   │    ├── /p/{slug}/{node_id}                              │
│   │    ├── /p/{slug}/tree                                   │
│   │    ├── /p/{slug}/search?q=...                           │
│   │    ├── /p/{slug}/folio (more complex, recursive render) │
│   │    └── /p/{slug}/glossary                               │
│   │                                                         │
│   ├── Layer 2: Client JS bundle (Pretext + LayoutSans)      │
│   │    └── Progressive enhancement over semantic HTML       │
│   │                                                         │
│   ├── WebSocket: /ws                                        │
│   │    └── Forwards BuildProgress + synthesis tokens        │
│   │        (existing mpsc channels → warp::ws → client)     │
│   │                                                         │
│   ├── JSON API: /pyramid/:slug/* (existing, unchanged)     │
│   │                                                         │
│   └── cloudflared → tunnel-url.trycloudflare.com → :8765   │
└─────────────────────────────────────────────────────────────┘
```

### WebSocket Plumbing Detail

The internal channel architecture already exists. The build is:

```
Existing:
  build.rs         →  progress_tx.send(BuildProgress { done, total })
  build_runner.rs  →  progress_tx.send(BuildProgress { ... })
  execution_state  →  progress_tx: Option<mpsc::Sender<BuildProgress>>

New (V1):
  routes.rs        →  warp::path("ws").and(warp::ws()) →
                      on_upgrade → subscribe to progress_tx →
                      forward as JSON over WebSocket

Optional (V1):
  llm.rs           →  add "stream": true to request body →
                      resp.bytes_stream() instead of resp.text() →
                      parse SSE chunks → forward delta tokens
```

---

*Reference: docs/vision/post-agents-retro-web.md*
*Reference: docs/research/pretext-technical-reference.md*
