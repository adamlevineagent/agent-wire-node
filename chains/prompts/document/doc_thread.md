You are given all the document extractions from a single THREAD — a coherent topic area from a document collection. These documents were grouped together because they cover related subjects.

Your job: synthesize this thread into a SINGLE AUTHORITATIVE REFERENCE NODE so rich that a reader can understand this entire topic area WITHOUT reading the original documents. The node should CONTAIN the knowledge, not just point to it.

TEMPORAL RULE: Documents have an implicit order. Later documents (higher index) may supersede earlier ones. When a later document contradicts an earlier one, the later document is the current truth. Record what changed as a correction.

ORIENTATION — write a COMPREHENSIVE briefing (8-15 sentences). Cover ALL of these:
- What is this topic area about? What project/system does it relate to?
- What are the KEY CONCLUSIONS across all documents? Not "several bugs were found" but "12 bugs found: 8 fixed, 3 deferred, 1 wontfix. Critical: FK constraint bug in pyramid_nodes blocks layered rebuild."
- What decisions were made? Quote the specific decision and rationale.
- What changed over time? Did early assumptions get revised?
- What remains open/unresolved?
- What should someone reading this thread KNOW before taking action?

Then organize into 3-8 sub-topics. For each:
- name: a clear aspect of this thread
- current: 4-8 sentences with SPECIFIC findings, decisions, numbers, names. Don't say "improvements were made" — say "response time dropped from 2.3s to 0.4s after switching from REST to IPC (decided in design-doc-v3, implemented in sprint 12)"
- entities: every specific name, system, person, document, decision, bug, metric mentioned

Output valid JSON only:
{
  "headline": "2-6 word thread label",
  "orientation": "8-15 sentences: comprehensive briefing covering conclusions, decisions, changes over time, and open items. Dense enough that a reader understands this topic without reading originals.",
  "source_nodes": ["D-L0-000", "D-L0-005"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "4-8 sentences. Specific findings, decisions, metrics, status. Full operational detail.",
      "entities": ["person: Alice", "system: Pyramid Engine", "metric: 2.3s → 0.4s", "decision: switch to IPC", "bug: #142 FK constraint"],
      "corrections": [{"wrong": "early assumption", "right": "revised understanding", "who": "doc-v3"}],
      "decisions": [{"decided": "what was decided", "why": "rationale"}]
    }
  ]
}

/no_think
