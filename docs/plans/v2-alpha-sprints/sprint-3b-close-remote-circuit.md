# Sprint 3b — Close the Remote Pyramid Circuit

## Context

Sprint 3 built the auth/payment infrastructure for remote pyramids but the circuit isn't closed. A user can't yet publish a pyramid and have someone else discover and query it through the Wire. This sprint closes the three remaining gaps.

## What Already Works (verified)

- Wire server: `pyramid-query-token`, `payment-intent`, `payment-redeem` endpoints
- Wire server: `wire/query` supports `?type=pyramid_metadata` filter
- Wire server: `contribute` accepts `type: "pyramid_metadata"` with tunnel_url in structured_data
- Node: `pyramid_publish` command publishes nodes + metadata (including tunnel_url) to Wire
- Node: `handle_remote_query` proxy acquires real Wire JWT from pyramid-query-token
- Node: Dual-auth on all read-only pyramid routes (Wire JWT accepted)
- Node: `pyramid_remote_query` Tauri command exists
- Node: `query_remote_pyramid` vocabulary command exists
- Node: Cloudflare tunnel provisioned and running

## What's Missing (3 items)

### 1. Tunnel Ingress Verification

The tunnel is provisioned via cloudflared but we need to verify it actually exposes the pyramid HTTP server port. The pyramid server binds to `127.0.0.1:{port}` and cloudflared should route the tunnel URL to that port. Check `tunnel.rs` — does `start_tunnel()` pass the correct local port? If not, fix it.

### 2. Search Pyramids Tab — Real Discovery

The Pyramids tab in SearchMode.tsx is a placeholder. Replace it with:
1. On tab select, query Wire: `wireApiCall('GET', '/api/v1/wire/query?type=pyramid_metadata&limit=20')`
2. Parse results — each has `structured_data` containing: `pyramid_slug`, `node_count`, `content_type`, `tunnel_url`, `access_tier`, `apex_headline`, `topics`
3. Display as cards: title (apex_headline), slug, node count, content type, access tier badge (public/priced)
4. Each card has "Query" button that triggers a remote query

### 3. One-Click Remote Query from Discovery

When user clicks "Query" on a discovered pyramid:
1. Call `invoke('pyramid_remote_query', { tunnel_url, slug, action: 'apex' })` to get the apex
2. Display the result in a slide-over panel or navigate to Understanding with remote badge
3. Error handling: tunnel unreachable → show error, payment required → show cost, auth failure → re-auth

## Files

| File | Change |
|------|--------|
| `src-tauri/src/tunnel.rs` | Verify ingress routes local pyramid port |
| `src/components/modes/SearchMode.tsx` | Replace Pyramids tab placeholder with Wire query |
| `src/components/IntentBar.tsx` | No changes needed |

## Verification

1. Publish a local pyramid → it appears in the Search > Pyramids tab
2. Click "Query" on a discovered pyramid → apex loads and displays
3. Tunnel URL in published metadata resolves to the serving node's pyramid API
