You are given all the document extractions from a single THREAD — a concept area from a document collection. These documents were grouped because they cover the same subject.

Each document has been classified with:
- **type**: design, audit, implementation, strategy, report, worksheet, etc.
- **date**: when it was written (documents are ordered earliest → latest)
- **canonical**: whether this is the authoritative source (canonical > partial > foundational > superseded)

Your job: synthesize this thread into a SINGLE AUTHORITATIVE REFERENCE NODE that tells the COMPLETE STORY of this concept area — from initial design through current state.

TEMPORAL AUTHORITY RULES:
- Documents are ordered chronologically. LATER documents are MORE AUTHORITATIVE.
- When a later document contradicts an earlier one, the later one is current truth. Record the change as a correction (wrong → right, with source doc).
- Superseded documents provide HISTORICAL CONTEXT, not current truth.
- A canonical audit finding overrides a foundational design assumption.
- Track the EVOLUTION: "Initially designed as X (design-doc, Feb 10). Audit found issues A, B, C (audit, Feb 25). Redesigned as Y (implementation, Mar 5). Current state: Y with modifications."

TYPE-AWARE SYNTHESIS:
- **Design docs** → capture the decisions made, alternatives rejected, and rationale
- **Audits** → capture findings with severity, status (fixed/open/deferred), and what was tested
- **Implementation plans** → capture what was built, dependencies, and current status
- **Strategy docs** → capture goals, positioning, and success metrics
- **Reports/worksheets** → capture specific data points, test results, measurements

ORIENTATION — write a COMPREHENSIVE briefing (8-15 sentences):
- What is this concept area? Define it in one sentence.
- What's the TIMELINE? "First discussed Feb 10 (design spec), audited Feb 25, implemented Mar 5."
- What's the CURRENT STATE? What was the final decision/implementation?
- What CHANGED over time? Key pivots, reversals, refinements.
- What's STILL OPEN? Unresolved questions, deferred items, known gaps.
- What should someone KNOW before working in this area?

Then organize into 3-8 sub-topics. For each:
- name: a clear aspect of this concept area
- current: 4-8 sentences. The CURRENT TRUTH incorporating all temporal evolution. Include specific findings, decisions, metrics, dates. "Auth was redesigned from password-based to magic-link (design-v2, Feb 15) after the security audit found credential storage issues (audit-01, Feb 12). Implementation completed Feb 28. Current state: Supabase magic-link + OTP, no password flow. Open: token refresh strategy not finalized (noted in implementation handoff)."
- entities: every specific name, system, person, document, decision, metric
- corrections: what changed (early assumption → later revision, with source docs)
- decisions: what was decided, when, why, by whom

Output valid JSON only:
{
  "headline": "2-6 word concept label",
  "orientation": "8-15 sentences: complete temporal story of this concept area. Current state, how it got here, what's open.",
  "source_nodes": ["D-L0-000", "D-L0-005"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "4-8 sentences. Current truth with full temporal context. Specific decisions, metrics, dates, status.",
      "entities": ["person: Alice", "system: Supabase Auth", "decision: switch to magic-link (Feb 15)", "audit: finding #3 (severity: high, status: fixed)"],
      "corrections": [{"wrong": "password-based auth", "right": "magic-link + OTP", "who": "design-v2, Feb 15"}],
      "decisions": [{"decided": "use Supabase magic-link", "why": "credential storage concerns from audit-01"}]
    }
  ]
}

/no_think
