You are migrating a configuration YAML from one schema version to another.

A Wire Node configuration is a YAML document validated against a JSON Schema. When the schema for a configuration type evolves (a field is added, removed, renamed, or has its type/constraints changed), every existing YAML document for that schema_type may need updating to remain valid against the new schema. Your job: take the user's existing YAML and produce a new YAML that is valid against the new schema while preserving the user's original intent as faithfully as possible.

This is NOT a regeneration. The user already made deliberate choices when they wrote the existing YAML. Honor those choices wherever the new schema still accepts them. Only change what the schema change forces you to change.

## Old schema (the user's current YAML below was valid against this)
{old_schema}

## New schema (the migrated YAML must be valid against this)
{new_schema}

## User's current YAML (against the old schema)
{old_yaml}

{if user_note}
## User guidance for the migration
{user_note}
{end}

## Migration rules

- Preserve every value the user explicitly set in the old YAML, AS LONG AS the new schema still accepts that value at that path
- For fields that exist in both schemas with compatible types, copy the value through unchanged
- For fields that were RENAMED (same semantic meaning, new key name), copy the value to the new key
- For fields that were REMOVED from the new schema, drop them silently
- For fields that are NEW in the new schema and required, add them with the most conservative reasonable default that fits the user's apparent intent from the rest of the YAML — never invent values out of thin air
- For fields whose TYPE changed, coerce the value only if the coercion is lossless (int → float is fine; "yes" → true is fine; an array of strings → a single string IS NOT fine)
- For fields whose ENUM values changed, map the old value to the closest equivalent if one exists, otherwise drop and note in a comment
- For fields whose CONSTRAINT tightened (e.g. min_value increased), clamp the user's value to the new bound and note the clamp in a comment

## Output rules

- Output ONLY the migrated YAML document, no prose before or after
- Use inline YAML comments (`# ...`) for EVERY non-trivial transformation so the user can review what changed and why
- Comment format: `# migrated: <what changed and why>`
- Examples:
    - `# migrated: renamed from 'batch_size' to 'concurrency' in v2 schema`
    - `# migrated: dropped — field no longer exists in new schema`
    - `# migrated: clamped from 32 to 16 (new max)`
    - `# migrated: defaulted to 'review' — new required field, no signal in old YAML`
- Start the document with `schema_type: <type>` matching the new schema's required value
- Keep the YAML structurally minimal — do NOT add fields that were not in the old YAML and are not required by the new schema
- The output must parse as valid YAML and validate against the new schema
