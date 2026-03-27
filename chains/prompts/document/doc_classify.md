You are given the opening section (first ~20 lines) of every document in a collection.

PURPOSE: Create a shared vocabulary of concept tags so that downstream grouping uses consistent terminology. Also tag each document with basic metadata (type, approximate date) to help with temporal ordering.

For each document, provide:
- source_node: the exact D-L0-XXX ID
- title: from the header
- temporal: best-effort date (ISO if possible, or relative marker)
- conceptual: 1-2 normalized concept tags (use the SAME tag for the same subject across documents)
- canonical: "canonical" (latest on this topic), "superseded" (replaced by later doc), "foundational" (establishes baseline), or "partial"
- type: design | audit | implementation | strategy | report | reference | worksheet | meta

EFFICIENCY RULES:
- Normalize concept tags aggressively: aim for 5-12 distinct tags across the entire collection
- For temporal, if no date is visible, write "unknown" — don't waste effort guessing
- For canonical, default to "canonical" unless you see clear evidence of supersession
- Keep it fast: this step exists to CREATE VOCABULARY, not to deeply analyze content

Output valid JSON only:
{
  "documents": [
    {
      "source_node": "D-L0-000",
      "title": "Document title",
      "temporal": {"date": "2026-02-18", "confidence": "explicit"},
      "conceptual": ["wire-auth"],
      "canonical": "canonical",
      "type": "design"
    }
  ],
  "concept_taxonomy": {
    "wire-auth": "Authentication and identity system design",
    "pyramid-build": "Knowledge pyramid construction pipeline"
  }
}

/no_think
