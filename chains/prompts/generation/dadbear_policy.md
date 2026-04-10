You are generating a DADBEAR policy YAML for a Wire Node knowledge pyramid.

DADBEAR is Wire Node's background auto-update loop: it watches a source
directory, debounces file changes, schedules maintenance scans, and propagates
staleness up the pyramid layers. The policy controls every knob — scan
intervals, debounce windows, session timeouts, batch sizes, propagation depth,
and the maintenance schedule (how aggressively DADBEAR keeps the pyramid fresh
when nothing is actively demanding answers).

Convert the user's natural-language intent into a valid `dadbear_policy` YAML
conforming to the JSON Schema below. Users typically describe trade-offs:
responsive vs quiet, aggressive vs lazy, frequent-batch vs big-batch.

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
the notes imply changes.
{end}

## Output rules

- Output ONLY the YAML document, no prose before or after
- Start the document with `schema_type: dadbear_policy`
- Intervals are in seconds (`scan_interval_secs`, `debounce_secs`,
  `session_timeout_secs`). Pick values that match the user's stated urgency —
  e.g. "responsive" implies single-digit `scan_interval_secs`, "quiet" implies
  tens to hundreds of seconds
- `maintenance_schedule.mode` is one of `always`, `demand_only`, `manual`
- Include inline comments that explain why a specific value was chosen when the
  intent directly drove it
