You are identifying the natural conceptual groupings in a document collection. You have document headlines and normalized concept tags from the taxonomy step.

PURPOSE: A reader will explore this collection one thread at a time. Each thread becomes a synthesis that traces how understanding of a concept evolved across documents. Your groupings determine what stories get told.

PRINCIPLES:
- **Group by concept, not by type.** A design doc, an audit, and a bugfix about the same subject belong together — they tell the complete story of that subject.
- **Let the material decide the shape.** Follow the natural boundaries in the material. Don't force documents into artificial groups.
- **Name threads by what they're ABOUT.** "Auth & Token Design" not "Design Documents." Be specific enough that a reader can scan thread names and know exactly where to look.
- **Each thread covers a coherent concept area.** A reader exploring that thread should come away understanding one complete area of knowledge.

You are identifying the THREAD DEFINITIONS only. Document assignment to threads happens in a separate per-document step. Define threads that cover all the concept tags present in the collection.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — concept-based",
      "description": "1-2 sentences: what concept this thread covers",
      "concept_tags": ["wire-auth", "identity"]
    }
  ]
}

/no_think
