# Parking Lot

- Compare current code prompts against document/conversation chain patterns only after establishing a fresh baseline
- Consider whether `thread_clustering` and `upper_layer_synthesis` need different architectural framing for code than documents
- Check whether file-level extraction is overproducing implementation detail vs architectural role

### Known Issues & Gotchas
- **Mercury-2 (OpenRouter/Mid Tier) Structured Output Bug**: Mercury-2 crashes/truncates when passed a large context (e.g., >15-17k tokens like the `compact_inputs` codebase topics payload) via strict `response_format: json_schema`. Wait for the provider bug to be patched, or keep `response_schema` removed from macro-synthesis steps like `code_concept_areas` so it falls back to native text generation + regex healing.
