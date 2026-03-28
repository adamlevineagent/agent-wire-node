You are given per-document classifications from a collection. Each document has been independently tagged with raw keywords, a type, temporal metadata, and canonical status.

PURPOSE: Normalize the raw keywords into a shared concept taxonomy. Different documents may have used different words for the same concept ("auth", "authentication", "identity system"). Your job is to create a consistent vocabulary that downstream grouping can rely on.

You receive the full list of per-document classifications. For each document, map its raw_keywords to normalized concept tags drawn from a shared taxonomy. Then provide the taxonomy itself.

PRINCIPLES:
- Merge synonyms aggressively: if two keywords refer to the same conceptual area, they get the same normalized tag
- Let the material determine how many tags exist — some collections have a handful of natural concepts, others have many
- Keep tags concrete and specific: "wire-auth" not "security", "pyramid-build" not "system"
- Every document should end up with at least one concept tag

Output valid JSON only:
{
  "documents": [
    {
      "source_node": "D-L0-000",
      "title": "Document title",
      "temporal": {"date": "2026-02-18", "confidence": "explicit"},
      "conceptual": ["wire-auth"],
      "canonical": "canonical",
      "type": "design",
      "supersedes": null,
      "superseded_by": null
    }
  ],
  "concept_taxonomy": {
    "wire-auth": "Authentication and identity system design",
    "pyramid-build": "Knowledge pyramid construction pipeline"
  }
}

/no_think
