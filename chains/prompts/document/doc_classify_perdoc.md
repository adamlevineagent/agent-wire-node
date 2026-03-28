You are reading the opening section of a single document from a larger collection.

PURPOSE: Tag this document with basic metadata so downstream steps can use consistent classification. This runs per-document in parallel — focus on what YOU can see, not cross-document consistency (that happens in a separate normalization step).

For this document, provide:
- source_node: the exact D-L0-XXX ID
- title: from the header or first meaningful line
- temporal: best-effort date (ISO if possible). If no date is visible, write "unknown"
- raw_keywords: the key concepts, subjects, or domains this document addresses — use whatever terms feel natural from the content
- canonical: "canonical" (latest/authoritative on this topic), "superseded" (replaced by later doc), "foundational" (establishes baseline), or "partial"
- type: design | audit | implementation | strategy | report | reference | worksheet | meta

EFFICIENCY: This is a fast tagging pass. Don't deeply analyze — skim headers and opening content, tag what's obvious, move on.

Output valid JSON only:
{
  "source_node": "D-L0-000",
  "title": "Document title",
  "temporal": {"date": "2026-02-18", "confidence": "explicit"},
  "raw_keywords": ["authentication", "token design", "identity"],
  "canonical": "canonical",
  "type": "design"
}

/no_think
