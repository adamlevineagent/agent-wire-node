# Handoff: Fleet Dispatch Failures (524 Timeouts)

**Date:** 2026-04-15
**Status:** Fleet dispatch IS working (34 successful returns) but some calls fail with HTTP 524 timeout after ~125 seconds.

## The Good News

Fleet dispatch is live and working. 34 of 38 dispatches returned successfully. The laptop is using BEHEM's 5090 for pyramid builds.

## The Problem

5 dispatches failed with Cloudflare error 524 (origin server timeout):
```
error: "Fleet dispatch to d07390ab... returned 524: error code: 524"
latency_ms: 125054  (~125 seconds)
```

524 means Cloudflare's tunnel kept the connection open but the origin (BEHEM's warp server) didn't respond within Cloudflare's timeout window. This happens when the LLM call takes too long — BEHEM's GPU is processing the prompt but the HTTP response doesn't come back before Cloudflare gives up.

## Current fleet dispatch timeout chain

1. **Fleet dispatch HTTP client:** `route.max_wait_secs` from dispatch policy escalation config (default 300s)
2. **Cloudflare tunnel:** ~100-120s timeout on proxied connections (not configurable per-request)
3. **BEHEM's warp server:** No explicit timeout on the fleet-dispatch handler — it awaits the queue result indefinitely

The bottleneck is Cloudflare's tunnel timeout (~100-120s). Long LLM calls (large prompts, slow models) exceed this.

## How to diagnose

**Check which calls failed vs succeeded:**
```bash
sqlite3 "/Users/adamlevine/Library/Application Support/wire-node/pyramid.db" \
  "SELECT event_type, json_extract(metadata, '$.latency_ms') as ms, json_extract(metadata, '$.tokens_prompt') as tok_in, json_extract(metadata, '$.tokens_completion') as tok_out FROM pyramid_compute_events WHERE event_type IN ('fleet_returned', 'fleet_dispatch_failed') AND timestamp > '2026-04-15T19:25:00' ORDER BY rowid DESC LIMIT 20;"
```

This shows latency for successful vs failed calls. If failed calls are all >100s and successful ones are <100s, Cloudflare timeout is confirmed.

**Check BEHEM's logs** (on the 5090):
Look for whether the fleet-dispatch handler received the request and whether the GPU completed the job:
```
# On BEHEM:
grep "fleet_dispatch\|fleet-dispatch\|Fleet" <behem-log-path>
```

If BEHEM completed the job AFTER the laptop got 524, the work was done but the result was lost.

## Possible fixes

1. **Cloudflare tunnel timeout:** Can be increased via `cloudflared` config (`--proxy-connect-timeout`, `--proxy-read-timeout`). Check if the tunnel provisioning code sets these.

2. **Chunked/streaming response:** Instead of waiting for the full LLM result, BEHEM could send a chunked HTTP response with keepalive pings to prevent Cloudflare from timing out.

3. **Webhook-based result delivery:** Instead of synchronous HTTP, BEHEM sends an ACK immediately, processes the job, then POSTs the result back to the laptop's tunnel. This is the Phase 3 architecture from the compute market plan.

4. **Shorter prompts / faster model:** The failed calls took >125s. If the prompts are large (evidence_answer with many nodes), splitting into smaller calls would keep each under the timeout.

## Context

The fleet dispatch handler on BEHEM (server.rs:1571-1624) receives the request, enqueues in the compute queue, and `awaits` the oneshot result. The HTTP response is held open until the GPU completes. For calls that take >100s, Cloudflare drops the connection before the response arrives.

The laptop sees 524 and falls through to local execution (the fleet_dispatch_failed event is recorded, dead peer removal fires, then the next call either retries fleet or goes local). The fallthrough is working correctly — no data loss, just wasted time on the timeout.
