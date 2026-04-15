# Mini Web Pyramid: The "Evidence-Apex" Strategy

## Problem Statement
The monolithic `l0_webbing` step historically triggers LLM syntax loops ("runaways") because it attempts to evaluate graph edges across all extracted L0 nodes simultaneously. A 34-node space produces $O(N^2)$ structural possibilities, which exceeds the cognitive bounds of Mercury 2 and runs up to the 48k token limit.

Furthermore, we must resolve this scale issue **without** breaking the "direct evidence hop" requirement. If we build generic intermediate nodes that don't natively point back to the source evidence, we defeat the purpose of the web structure.

## Solution Architecture
We will replace the single `l0_webbing` `web` primitive with a 3-step `container` sub-chain that builds a proxy graph.

### 1. Domain Clustering (`l0_web_cluster`)
- **Primitive**: `classify`
- **Input**: Batches of `$source_extract` L0 nodes (dehydrated to just `headline` and `orientation/summary`).
- **Function**: Groups L0s into 3–5 high-level thematic "Domains" (e.g., UI, Core Logic, Database).
- **Scale benefit**: Reduces $N=34$ raw nodes down to ~$N=4$ domains.

### 2. Domain Apex Synthesis (`l0_domain_apex`)
- **Primitive**: `synthesize`
- **Input**: The L0 nodes assigned to each Domain.
- **Function**: Creates a synthetic intermediate node (Domain Node) representing that cluster.
- **The "Evidence-Apex" Trick**: We explicitly prompt the `synthesize` step to output an `evidence_map` or require its `children` array to explicitly contain the `Q-L0-XXX` node IDs it is summarizing. This ensures the Domain Apex isn't just a generic abstraction — it physically houses the direct pointers to the lowest-level evidence.

### 3. Master Edge Graph (`l0_master_web`)
- **Primitive**: `web`
- **Input**: The 3–5 outputted Domain Apex nodes.
- **Function**: Draws the structural edges between the Domains.
- **Resolution**: Because the Domain nodes natively contain the `Q-L0-XXX` pointers, mapping the relationships between Domain A and Domain B inherently maps the raw source evidence of A to the raw source evidence of B, satisfying the constraint without an $O(N^2)$ hallucination penalty.

## Implementation Requirements

1. **Update `chains/defaults/question.yaml`:**
   Remove the `primitive: web` block for `l0_webbing` and replace it with a `primitive: container` block holding the three steps.

2. **Author new Prompts:**
   - `$prompts/question/web_cluster.md` (Domain grouping based on L0s)
   - `$prompts/question/web_cluster_merge.md` (If batches overflow)
   - `$prompts/question/web_domain_apex.md` (Synthesize the Domain node and embed the explicit L0 pointer paths)
   - `$prompts/question/web_master.md` (Draw edges between the domains)

3. **Config Tuning:**
   - Set `batch_size: 50` on `l0_web_cluster` to ensure we never overwhelm the classifer.
   - Set `model_tier: mid` (Mercury 2) for all steps since the token space will be carefully bounded.
