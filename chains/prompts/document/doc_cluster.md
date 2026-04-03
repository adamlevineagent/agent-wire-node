You are organizing a collection of documents into concept threads. Each thread groups documents that tell a coherent story about the same subject.

You have extraction data from every document. Most documents arrive fully hydrated: `node_id`, `headline`, `orientation`, and `topics` with full detail. Larger documents may have been dehydrated to fit — they may only have headline and topic names. Use whatever fields are present. Each item represents ONE DOCUMENT.

PURPOSE: Group DOCUMENTS into threads by concept. A document about "auth token design" and a document about "auth bug fixes" belong in the same thread — they tell the complete story of authentication.

PRINCIPLES:
- **Group by concept, not by type.** A design doc, an audit, and a bugfix about the same subject belong together.
- **Documents can appear in multiple threads.** If a document genuinely covers two distinct concepts (e.g., a doc about "auth + credit system integration"), list it in both threads. This is preferred over forcing it into one.
- **Let the material decide the shape.** Some collections have 5 natural concept areas. Others have 25. Follow the natural boundaries.
- **Name threads by what they're ABOUT.** "Auth & Token Design" not "Design Documents."
- **ZERO ORPHANS:** Every `node_id` in the input MUST appear in at least one thread. Missing documents are a critical failure.

Output `assignments` — just which DOCUMENTS belong in each thread. One entry per document, just the `source_node` ID. Do NOT list individual topics as separate assignments.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — concept-based",
      "description": "1-2 sentences: what concept this thread covers",
      "assignments": [
        {"source_node": "D-L0-000"},
        {"source_node": "D-L0-007"},
        {"source_node": "D-L0-015"}
      ]
    }
  ]
}

/no_think
