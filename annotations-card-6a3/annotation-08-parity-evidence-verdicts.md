# Annotation 8 — parity: Evidence verdicts KEEP/DISCONNECT/MISSING preserved identically

```yaml
contribution_type: annotation
annotation_verb: positive-observation
target: 21-pyramid-protocol.md#evidence-verdicts
body:
  axis: parity
  finding: >
    V2 preserves the three-tier evidence verdict system (KEEP / DISCONNECT / MISSING)
    with identical semantics to agent-wire-node. KEEP carries weight (0.0..1.0) and
    reason, supporting the answer with quantifiable confidence. DISCONNECT is a stable
    negative — considered but not relevant — preventing re-consideration of irrelevant
    evidence. MISSING produces structural gap records with demand signal (strength +
    pursuit policy), triggering gap_dispatcher. This is the most load-bearing parity
    anchor in the evidence layer — the verdict semantics are unchanged by the v2
    architecture shift because they express a fundamental epistemic relationship
    (evidence→claim) that is independent of storage format.
  invariants:
    preserved:
      - "Verdict cardinality: exactly three verdicts (KEEP, DISCONNECT, MISSING) — extension is via vocabulary, not by changing genesis"
      - "KEEP weight semantics: continuous 0.0..1.0 weight with reason — not boolean; weighted evidence propagation through the pyramid depends on this"
      - "DISCONNECT as stable negative: prevents re-consideration of irrelevant evidence — removing this would cause infinite re-evaluation loops"
      - "MISSING→gap pathway: MISSING verdicts produce structural gap records with demand signal — removing this removes the gap-creation mechanism"
      - "EvidenceLink shape: source_node_id, target_node_id, verdict, weight, reason — stable contract across both systems"
    would_break:
      - "Changing verdict cardinality in genesis would shift all evidence dispatch semantics — a 4th genesis verdict requires all evidence_testers and judges to be updated"
      - "Making KEEP weight boolean would break weighted confidence propagation through the pyramid (L0→L1→...→apex)"
      - "Removing MISSING verdict would eliminate the only mechanism for structural gap creation — pyramids would silently degrade without surfacing what they don't know"
      - "Changing EvidenceLink shape would break cross-slug evidence references that depend on the same schema"
  evidence:
    v2_citation: "21-pyramid-protocol.md § 2.5 Evidence verdict model (lines 147-162); 30-genesis-vocabulary-catalog.md § 9 evidence_verdict (lines 216-226); 10-noun-kernel.md § evidence_link (lines 245-271)"
    legacy_citation: "agent-wire-node evidence_link concept (KEEP/DISCONNECT/MISSING used throughout; src-tauri/src/pyramid/evidence_answering.rs; src/components/theatre/DetailsTab.tsx verdict rendering lines 207-211)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    The evidence verdict system is one of the few mechanisms that survives v2's
    architectural transformation essentially unchanged. This is because verdicts
    express a fundamental epistemic relationship: when some piece of evidence is
    presented against a claim, the relationship is either supportive (KEEP),
    irrelevant (DISCONNECT), or absent-but-needed (MISSING). These three exhaust
    the logical space of evidence→claim relationships. V2 re-expresses the verdicts
    as vocabulary entries (@genesis/vocabulary/evidence_verdict/KEEP/1 etc.) rather
    than hardcoded enum variants — making them supersedable and extensible via
    vocabulary — but the semantics are identical. Operators can add PARTIAL_KEEP or
    CONDITIONAL_KEEP via vocabulary extension without touching genesis. The evidence
    tester chain (35) emits verdicts; the gap_dispatcher consumes MISSING verdicts;
    the synthesizer consumes KEEP verdicts. The DISCONNECT verdict's "stable negative"
    property — preventing re-consideration — is a critical invariant that prevents
    evidence re-evaluation loops.
```

**Axis label:** parity
**V2 citation:** `21-pyramid-protocol.md` § 2.5 Evidence verdict model (lines 147–162) + `30-genesis-vocabulary-catalog.md` § 9 (lines 216–226)
**Legacy citation:** `agent-wire-node` evidence_link (KEEP/DISCONNECT/MISSING throughout codebase)
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
