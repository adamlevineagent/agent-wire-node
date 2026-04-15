You are generating a DADBEAR norms YAML for a Wire Node knowledge pyramid.

DADBEAR norms control the timing and threshold knobs for the background
auto-update loop — how often it scans, how long it debounces, when it promotes
sessions, how many files it batches, and when it considers a pyramid "runaway"
(too many stale nodes to rebuild safely). Norms are layered: a global default
applies to all pyramids, and per-pyramid overrides can tighten or loosen any
field.

Convert the user's natural-language intent into a valid `dadbear_norms` YAML
conforming to the JSON Schema below. Users typically describe trade-offs:
responsive vs quiet, aggressive vs lazy, frequent small batches vs large
batches, tight debounce vs generous debounce.

## Schema (JSON Schema)
{schema}

## User Intent
{intent}

{if current_yaml}
## Current Values (you are refining these existing norms)
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
- Start the document with `schema_type: dadbear_norms`
- Intervals are in seconds (`scan_interval_secs`, `debounce_secs`,
  `session_timeout_secs`). Pick values that match the user's stated urgency —
  e.g. "responsive" implies single-digit `scan_interval_secs`, "quiet" implies
  tens to hundreds of seconds
- `runaway_threshold` is a float between 0.0 and 1.0 — lower means more
  cautious (throttles earlier), higher means more aggressive (allows more
  staleness before throttling)
- `retention_window_days` is how long to keep old contribution versions —
  shorter saves disk, longer allows rollbacks
- `min_changed_files` controls the trigger threshold — 1 means every single
  file change triggers a rebuild, higher values batch changes together
- Include inline comments that explain why a specific value was chosen when the
  intent directly drove it
