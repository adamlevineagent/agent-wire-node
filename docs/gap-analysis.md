# Gap Analysis: `code.yaml` vs `document-v4-classified.yaml`

The `document-v4-classified.yaml` pipeline and the V4 `code.yaml` pipeline share the same architectural skeleton (extract -> concept synthesis -> assign -> weave). 

However, looking at the structural gap between the pipelines reveals that the **Document pipeline has a "Four-Axis Semantic Grouping" model** that explicitly executes a multi-pass taxonomy normalization phase *prior to extraction*, which `code.yaml` currently skips.

Here are the specific missing architectural components in `code.yaml` that are present in the Documents V4 pipeline:

## 1. Pre-Extraction Taxonomy & Normalization (The "L-0.5" Pass)
The `document` pipeline does not just immediately extract the contents of the files. It runs a pre-processing loop:
1. **`doc_classify_perdoc`**: Does a shallow read of the first 20 lines of every document to tag it temporally (date), extract raw keywords, establish canonical nature, and classify the overall type.
2. **`doc_taxonomy`**: Performs a single global LLM call that consumes the raw keywords from the classification step and builds a normalized, unified global taxonomy (merging synonyms, standardizing domain names).

**Gap:** `code.yaml` jumps straight into `l0_code_extract` with no preceding taxonomy synthesis or architectural map. Each code file evaluates its topics in a vacuum, which leads to synonymous/fragmented topics (e.g. "auth_system", "Authentication", "Firebase_login") being propagated out to the synthesis phase. 

## 2. Type-Aware / Taxonomy-Contextual Extraction
In `document-v4-classified.yaml`, the normalized taxonomy (`$doc_taxonomy`) is passed into `l0_doc_extract` as a `context:` block. 
This means the extraction LLM knows *what type* of document it's reading and the global taxonomy it should align with while distilling topics, significantly tightening the downstream token load.

**Gap:** `code.yaml` extracts without global context, meaning the output size blows up because the LLM includes everything it thinks might be relevant, rather than aligning to a known architectural taxonomy.

## 3. Two-Dimensional Context in Concept Identification
When `document-v4-classified.yaml` defines the macro-threads (`doc_concept_areas`), it passes BOTH the extracted topics (`$l0_doc_extract`) AND the formalized taxonomy (`$doc_taxonomy`). This stabilizes the model's structural definitions because the LLM natively aligns the thread concepts to the explicitly requested taxonomical boundaries.

**Gap:** `code_concept_areas` relies *exclusively* on the raw `$l0_code_extract` arrays. It acts blind and has to infer the global architecture entirely from the bottom-up nodes, which leads to bloated and unpredictable prompts.

## 4. Temporal Synthesis & Supersession
`doc_thread.md` explicitly supports temporal evolution. Documents within a thread are ordered chronologically; older assertions are superseded by newer assertions, maintaining an accurate source of truth for changing systems.

**Gap:** `code_thread` treats all code as functionally flat right now, simply merging assignments structurally. 

---

**Conclusions for Code Pipeline Expansion:**
To fully align `code.yaml` with the robust standard of `documents-v4.yaml`:
1. We need a fast, parallel **L-0.25 Code Taxonomy extraction pass**, reading just paths + syntax headers to build a shallow project map.
2. Compile that into a global `code_taxonomy` mapping out architectural domains.
3. Pass `code_taxonomy` into `l0_code_extract` so files self-align to the project domains rather than generating ad-hoc topic names.
