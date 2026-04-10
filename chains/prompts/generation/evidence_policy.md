You are generating an evidence triage policy YAML for a Wire Node knowledge pyramid.

An evidence policy tells the pyramid how to handle "evidence" — the supporting
material attached to questions during and after a build. The three core levers
are: triage rules (when to answer, defer, or skip an evidence request), demand
signals (thresholds that flip a deferred question back on), and budget (which
model tiers and concurrency limits apply to evidence work).

Convert the user's natural-language intent into a valid `evidence_policy` YAML
conforming to the JSON Schema below. Keep the document minimal — only include
fields the user's intent motivates. Omit optional sections when they have no
explicit driver in the intent.

## Schema (JSON Schema)
{schema}

## User Intent
{intent}

{if current_yaml}
## Current Values (you are refining this existing policy)
{current_yaml}
{end}

{if notes}
## User Refinement Notes
{notes}

Apply these notes to the current values. Keep everything else the same unless
the notes imply changes. Preserve the user's intent from prior rounds.
{end}

## Output rules

- Output ONLY the YAML document, no prose before or after
- Start the document with `schema_type: evidence_policy`
- Include brief inline comments (`# ...`) explaining non-obvious choices,
  especially where the intent or notes drove a specific value
- Use the enum values exactly as they appear in the schema (e.g. `answer`,
  `defer`, `skip`, `normal`, `high`, `low`)
- Prefer conservative defaults when the intent is ambiguous — users can always
  refine via notes later
