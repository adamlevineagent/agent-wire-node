# Handback: HTTP Mutations Are Live — You Can Use Them

## Status
All pyramid HTTP mutation routes are **working right now** on the running app (port 8765). The 410 stubs were removed and the app was rebuilt and redeployed to `/Applications/Wire Node.app`.

## What was fixed
25 pyramid POST/DELETE routes in `src-tauri/src/pyramid/routes.rs` were restored from 410 stubs back to their real handlers. The regression was introduced in commit `f4320f5` ("Phase 0 security hardening").

## How to use them
All mutation routes require `Authorization: Bearer <token>` header. Token is in `~/Library/Application Support/wire-node/pyramid_config.json` → `auth_token` field.

Current token: `vibesmithy-test-token`

### Examples that work right now:
```bash
TOKEN="vibesmithy-test-token"

# Create a slug
curl -X POST http://localhost:8765/pyramid/slugs \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"slug":"my-slug","content_type":"document","source_path":"/path/to/docs"}'

# Trigger ingest
curl -X POST http://localhost:8765/pyramid/my-slug/ingest \
  -H "Authorization: Bearer $TOKEN"

# Trigger build
curl -X POST http://localhost:8765/pyramid/my-slug/build \
  -H "Authorization: Bearer $TOKEN"

# Check build status (GET, always worked)
curl http://localhost:8765/pyramid/my-slug/build/status \
  -H "Authorization: Bearer $TOKEN"

# Cancel build
curl -X POST http://localhost:8765/pyramid/my-slug/build/cancel \
  -H "Authorization: Bearer $TOKEN"
```

### Via MCP server
The MCP server at `mcp-server/` already uses these HTTP routes correctly. Tools like `pyramid_build`, `pyramid_ingest`, `pyramid_create_slug`, `pyramid_build_cancel` all work through HTTP POST with Bearer auth. No changes needed to the MCP layer.

### Via CLI
```bash
PYRAMID_AUTH_TOKEN=vibesmithy-test-token node mcp-server/dist/cli.js build my-slug
PYRAMID_AUTH_TOKEN=vibesmithy-test-token node mcp-server/dist/cli.js build-status my-slug
```

## What was also fixed in this session
The chain validator in `chain_engine.rs` was requiring `instruction` on `container` primitives, which broke `document-default` and `code-default` chains. Orchestration primitives (`container`, `loop`, `gate`, `split`) are now exempt from the instruction requirement. This means fresh builds should get past validation.

## What's NOT fixed
Partner routes (`partner_send_message`, `partner_session_new`) in `src-tauri/src/partner/routes.rs` still return 410. These are Dennis/Partner chat routes, not pyramid build routes. They were not in scope for this fix.

## Verified at
2026-04-03, tested against running app PID 21146 on port 8765.
Confirmed: zero pyramid 410 stubs in `/Applications/Wire Node.app` binary.
