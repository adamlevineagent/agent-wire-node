# Handoff: Stop BEHEM from Fleet-Dispatching to Laptop

**Date:** 2026-04-15
**Problem:** BEHEM's dispatch policy has `provider_id: fleet` first, so its stale engine/DADBEAR work gets fleet-dispatched to the laptop. The laptop's GPU is busy running BEHEM's background work instead of being free for its own tasks.
**Fix:** Remove `fleet` from BEHEM's dispatch policy. BEHEM serves fleet requests (incoming from laptop) but never sends them outward.

## What to do

**1. Find BEHEM's pyramid.db**

On Windows, it's typically at:
```
%APPDATA%\wire-node\pyramid.db
```
Or check the Wire Node data directory.

**2. Read the current dispatch policy**

```bash
sqlite3 pyramid.db "SELECT yaml_content FROM pyramid_config_contributions WHERE schema_type = 'dispatch_policy' AND status = 'active' ORDER BY rowid DESC LIMIT 1;"
```

You'll see something like:
```yaml
schema_type: dispatch_policy
version: 1
provider_pools:
  ollama-local:
    concurrency: 1
routing_rules:
  - name: ollama-catchall
    match_config: {}
    route_to:
      - provider_id: fleet        # ← REMOVE THIS LINE
      - provider_id: ollama-local
        is_local: true
build_coordination:
  defer_maintenance_during_build: true
```

**3. Create a new contribution that supersedes it**

The cleanest way: toggle local mode off then on in the Wire Node UI (Settings). This regenerates the dispatch policy. Then immediately edit the DB to remove fleet:

```bash
sqlite3 pyramid.db "UPDATE pyramid_config_contributions SET yaml_content = REPLACE(yaml_content, '      - provider_id: fleet
', '') WHERE schema_type = 'dispatch_policy' AND status = 'active';"
```

**OR** do it manually — copy the YAML, remove the `provider_id: fleet` line, and update:

```bash
sqlite3 pyramid.db "UPDATE pyramid_config_contributions SET yaml_content = 'schema_type: dispatch_policy
version: 1
provider_pools:
  ollama-local:
    concurrency: 1
routing_rules:
  - name: ollama-catchall
    match_config: {}
    route_to:
      - provider_id: ollama-local
        is_local: true
build_coordination:
  defer_maintenance_during_build: true' WHERE schema_type = 'dispatch_policy' AND status = 'active';"
```

**4. Restart Wire Node on BEHEM**

The dispatch policy is loaded at startup and hot-reloaded on ConfigSynced. A restart ensures the new policy is active.

## What this does

- BEHEM's routing becomes: `[ollama-local]` only — uses its own GPU, never dispatches outward
- BEHEM still ACCEPTS incoming fleet dispatch requests from the laptop (the `/v1/compute/fleet-dispatch` endpoint is unaffected by the dispatch policy)
- The laptop's routing stays: `[fleet, ollama-local]` — tries BEHEM first, falls back to own GPU
- BEHEM's stale engine / DADBEAR work runs on BEHEM's own GPU instead of being sent to the laptop

## Result

- Laptop dispatches build work → BEHEM's 5090 (via fleet)
- BEHEM's background work stays on BEHEM's GPU (no fleet dispatch)
- Laptop's GPU is free (fan stops spinning on BEHEM's work)

## Long-term fix

The proper solution is per-node dispatch policy configuration via the generative config system. "Server" nodes (BEHEM) serve fleet but don't dispatch. "Client" nodes (laptop) dispatch to fleet. This should be a toggle in the UI, not a manual DB edit. TODO for a future session.
