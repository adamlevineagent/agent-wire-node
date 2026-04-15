You are given all the document extractions from a single THREAD — a concept area from a document collection. These documents were grouped because they cover the same subject.

Each document has been classified with:
- **type**: design, audit, implementation, strategy, report, worksheet, etc.
- **date**: when it was written (documents are ordered earliest to latest)
- **canonical**: whether this is the authoritative source (canonical > partial > foundational > superseded)

PURPOSE: Synthesize this thread into a SINGLE AUTHORITATIVE REFERENCE NODE that tells the COMPLETE STORY of this concept area — from initial design through current state. A reader should come away understanding what this concept is, how it evolved, and what the current truth is.

TEMPORAL AUTHORITY:
- Documents are ordered chronologically. LATER documents are MORE AUTHORITATIVE.
- When a later document contradicts an earlier one, the later one is current truth. Record the change as a correction (wrong → right, with source doc).
- Superseded documents provide HISTORICAL CONTEXT, not current truth.
- Track the EVOLUTION: "Initially designed as X (design-doc, Feb 10). Audit found issues (Feb 25). Redesigned as Y (Mar 5). Current state: Y with modifications."

TYPE-AWARE SYNTHESIS:
- **Design docs** → decisions made, alternatives rejected, rationale
- **Audits** → findings with severity, status (fixed/open/deferred)
- **Implementation plans** → what was built, dependencies, current status
- **Strategy docs** → goals, positioning, success metrics
- **Reports/worksheets** → specific data points, test results, measurements

ORIENTATION — write a comprehensive briefing:
- What is this concept area?
- What's the timeline?
- What's the current state?
- What changed over time?
- What's still open?
- What should someone know before working in this area?

Then organize into sub-topics. Each should be a distinct aspect of this concept area. Let the material determine how many — a simple concept might need 2, a complex one might need 8.

For each topic:
- name: clear aspect of this concept area
- current: The CURRENT TRUTH incorporating temporal evolution. Include specific findings, decisions, metrics, dates.
- entities: every specific name, system, person, document, decision, metric
- corrections: what changed (wrong → right, with source docs)
- decisions: what was decided, when, why

Output valid JSON only:
{
  "headline": "2-6 word concept label",
  "orientation": "Comprehensive temporal story of this concept area. Current state, how it got here, what's open.",
  "source_nodes": ["D-L0-000", "D-L0-005"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "Current truth with full temporal context. Specific decisions, metrics, dates, status.",
      "entities": ["person: Alice", "system: Supabase Auth", "decision: switch to magic-link (Feb 15)"],
      "corrections": [{"wrong": "password-based auth", "right": "magic-link + OTP", "who": "design-v2, Feb 15"}],
      "decisions": [{"decided": "use Supabase magic-link", "why": "credential storage concerns from audit-01"}]
    }
  ]
}

/no_think
