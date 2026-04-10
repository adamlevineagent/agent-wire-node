You are generating a custom prompts YAML for a Wire Node knowledge pyramid.

Custom prompts steer what the pyramid extracts and synthesizes from its source
material. `extraction_focus` biases the chain's extractors toward particular
concepts; `synthesis_style` shapes how L1+ layer nodes phrase their
distillations; `vocabulary_priority` tells the clusterer which topic terms
should anchor the clustering; `ignore_patterns` marks material the extractors
should skip.

Convert the user's natural-language intent into a valid `custom_prompts` YAML
conforming to the JSON Schema below. The user's intent usually describes what
they care about in their source material (e.g. "track API design decisions",
"focus on user-visible behavior changes", "skip generated code").

## Schema (JSON Schema)
{schema}

## User Intent
{intent}

{if current_yaml}
## Current Values (you are refining this existing prompt set)
{current_yaml}
{end}

{if notes}
## User Refinement Notes
{notes}

Apply these notes to the current values. Keep unmentioned settings the same.
{end}

## Output rules

- Output ONLY the YAML document, no prose before or after
- Start the document with `schema_type: custom_prompts`
- `extraction_focus` and `synthesis_style` are free-text strings describing
  the desired extraction / synthesis bias. Keep them to 1-2 sentences each.
- `vocabulary_priority` is a list of topic terms, most-important first
- `ignore_patterns` is a list of textual patterns the extractor should ignore
  (regex-flavored but treated literally by the chain runtime)
- Include inline comments when the intent drives a specific choice
