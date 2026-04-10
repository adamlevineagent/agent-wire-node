You are generating a tier routing YAML for a Wire Node knowledge pyramid.

Tier routing maps abstract "model tiers" (e.g. `fast_extract`, `synth_heavy`,
`stale_local`) to concrete (provider, model) pairs. The pyramid executor asks
for tiers by name; the routing table resolves them to actual API targets with
per-token pricing. Users typically care about cost vs quality trade-offs and
local vs cloud preferences.

Convert the user's natural-language intent into a valid `tier_routing` YAML
conforming to the JSON Schema below. The generated tier list should cover
every tier the user mentions PLUS the standard Wire Node tiers (fast_extract,
synth_heavy, stale_local) so the executor never hits a missing-tier error.

## Schema (JSON Schema)
{schema}

## User Intent
{intent}

{if current_yaml}
## Current Values (you are refining this existing routing)
{current_yaml}
{end}

{if notes}
## User Refinement Notes
{notes}

Apply these notes to the current values. Keep the tiers the user didn't
mention the same unless the notes imply otherwise.
{end}

## Output rules

- Output ONLY the YAML document, no prose before or after
- Start the document with `schema_type: tier_routing`
- `entries` is a list of tier entries; each entry has `tier_name`,
  `provider_id`, `model_id`, and optional `priority`, `context_limit`,
  `prompt_price_per_token`, `completion_price_per_token`
- When the user references a named provider (e.g. "openrouter", "ollama",
  "anthropic"), use that as `provider_id`. Users manage their providers
  separately; the router does not need to validate them here
- Pricing fields are per TOKEN, not per 1M tokens; encode in scientific
  notation if needed (e.g. `3.0e-6`)
- Include inline comments when a tier choice reflects a specific intent signal
  (e.g. "# user asked for local-only fallback when cloud is unavailable")
