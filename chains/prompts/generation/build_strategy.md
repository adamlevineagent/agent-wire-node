You are generating a build strategy YAML for a Wire Node knowledge pyramid.

A build strategy tells the pyramid how to spend compute during its two primary
build phases: initial_build (first-time pyramid construction) and maintenance
(ongoing updates when source material changes). Each phase picks a model tier,
concurrency level, evidence depth, and whether to run webbing (cross-node
relationship discovery). The `quality` block gates what counts as a valid
pyramid output.

Convert the user's natural-language intent into a valid `build_strategy` YAML
conforming to the JSON Schema below. The user's intent typically covers trade-
offs: more thorough vs faster, local-only vs cloud, shallow vs deep evidence,
strict vs lenient quality gates.

## Schema (JSON Schema)
{schema}

## User Intent
{intent}

{if current_yaml}
## Current Values (you are refining this existing strategy)
{current_yaml}
{end}

{if notes}
## User Refinement Notes
{notes}

Apply these notes to the current values. Keep everything else the same unless
the notes imply changes.
{end}

## Output rules

- Output ONLY the YAML document, no prose before or after
- Start the document with `schema_type: build_strategy`
- Use the tier names the user references (e.g. `synth_heavy`, `stale_local`,
  `fast_extract`) — do NOT invent tier names; the user can edit them later
- Include inline comments explaining why a tier or concurrency value was chosen
- Prefer `evidence_mode: deep` for first builds unless the user explicitly asks
  for a lighter touch; prefer `evidence_mode: demand_only` for maintenance
