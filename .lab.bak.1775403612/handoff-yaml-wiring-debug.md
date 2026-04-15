# Urgent: Three YAML Wiring Bugs in question.yaml

You built the recipe-as-contribution refactor. These are integration bugs between question.yaml and the 4 primitives you wrote. You know exactly how the primitives consume their inputs — these should be fast fixes.

## Bug 1: extraction_schema receives no questions

**Symptom:** extraction_schema LLM call produces "No specific questions were provided. Unable to generate a question-shaped extraction prompt."

**Cause:** The `extraction_schema` step's input block doesn't wire the decomposed question tree into the step. The LLM gets the system prompt (extraction_schema.md) but no user prompt content containing the questions.

**What I tried:** Added `input: { question_tree: "$decompose" }` to the extraction_schema step in question.yaml. This hit Bug 2.

**What you need to determine:** How does the `extract` primitive pass `step.input` content to the LLM? Is it serialized as the user prompt? Appended to the instruction? The extraction_schema prompt expects to receive a list of questions — however your primitive delivers input content, that's how the questions need to arrive.

## Bug 2: $decompose_delta unresolved on fresh builds

**Symptom:** `Chain aborted at step 'extraction_schema': Unresolved reference: $decompose_delta`

**Cause:** On a fresh build (no existing overlay), the `decompose_delta` step is skipped by its `when` condition. But if extraction_schema's input references `$decompose_delta`, the ref is unresolved and the executor aborts.

**The real question:** How should the extraction_schema step receive the question tree on BOTH fresh and delta paths? Options:
- Both decompose and decompose_delta write to the same output key (so downstream steps always find it at one ref)
- The executor treats unresolved refs in input blocks as null/empty instead of aborting
- Two extraction_schema steps with `when` conditions matching the decompose path that ran

You know which option fits your primitive design.

## Bug 3: Generated extraction prompt produces markdown, not JSON

**Symptom:** `Step 'l0_extract': JSON parse failed: No JSON found in: Here's a quick rundown of what the next-env.d.ts file...`

**Cause:** The extraction_schema step generates an `extraction_prompt` field. The `l0_extract` step uses it via `instruction_from: "$extraction_schema.extraction_prompt"`. Mercury receives this generated prompt and responds with conversational markdown because the generated prompt doesn't include JSON output format instructions.

**What I tried:** Edited extraction_schema.md to tell it the generated prompt must include JSON format instructions and `/no_think`. Haven't confirmed it works because Bug 1 and Bug 2 blocked reaching this point with valid questions.

**What you need to determine:** Should the JSON output format be injected by the executor (the `extract` primitive always appends "Output valid JSON only" to the instruction), or should the generated prompt carry its own JSON format instructions? The latter puts format control in the contribution (prompt), the former puts it in the equipment (Rust). Given Pillar 2, the prompt should carry it — but the executor may already append format instructions that the generated prompt is duplicating or contradicting.

## How to test

```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
# All slugs are archived — create fresh
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/slugs -d '{"slug":"wiring-test","content_type":"code","source_path":"/Users/adamlevine/AI Project Files/vibesmithy"}'
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/wiring-test/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/wiring-test/build/question -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'
# Poll
curl -s -H "$AUTH" localhost:8765/pyramid/wiring-test/build/status
```

Success = build completes with status "complete", nodes at depth > 0 exist, apex is reachable via drill.
