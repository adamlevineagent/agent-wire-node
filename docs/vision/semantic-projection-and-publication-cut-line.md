# Semantic Projection and the Publication Cut-Line

*The privacy architecture that lets a pyramid answer questions about itself without revealing its structure.*

---

## Status

Forward-looking architectural vision. Not a spec. Establishes the property we want, the mechanism we will build toward, the integration points with existing architecture, and the attack models we have to defend against. Concrete implementation will follow in a later spec drafted once the 17-phase plan has shipped.

---

## The problem binary privacy cannot solve

Today, privacy in knowledge systems is binary. A document is public or private. A record is visible or hidden. A database row is accessible to principal X or not. There is no third option.

This binary model fails in every situation where the interesting question is not *can this person see this thing* but *what can this person learn from this thing*. Consider the scenarios the futures sketch describes:

- The grandmother wants her stories shared with her grandchildren, but not her medical details with strangers, and not her political memories with anyone until after her death. Binary privacy forces her to either publish the whole pyramid or hoard it.
- The investigative reporter wants readers to verify the evidence behind a claim without being able to identify the anonymous source. Binary privacy makes this a choice between "trust me" and "burn the source."
- The clinician wants to share aggregate insights from her case history with peers without exposing individual patient records. Binary privacy forces her to either stop sharing or violate confidentiality.
- The research lab wants external auditors to verify that a published claim is supported by underlying data, without exposing the underlying data to competitors. Binary privacy leaves them in the current "trust the authors" regime.

In each case, the owner has a legitimate interest in keeping some material private *and* a legitimate interest in letting certain questions about the pyramid be answered, sometimes by certain people, sometimes with verifiable backing. Binary privacy cannot express this. You need a model that distinguishes *structural disclosure* (what exists, where it fits, who owns it) from *content disclosure* (what it actually says) from *derivative disclosure* (what conclusions can be drawn from it).

The Agent Wire pyramid architecture makes this distinction buildable because pyramids are already structured as layered derivations from evidence. You can release a layer without releasing the layers beneath it. You can release a conclusion without releasing the reasoning that led to it. You can commit cryptographically to the existence and integrity of material without disclosing its content. The substrate has the shape; we need to name the property and build the mechanisms.

The property is **semantic projection**. The mechanism is the **publication cut-line**. The two together give every pyramid owner a continuous dial from "fully private" through "selectively answerable" to "fully public," with each position on the dial being precise, auditable, and reversible.

---

## Semantic projection as a property

Semantic projection is the property: *a pyramid can answer a class of questions without disclosing the structure or content from which the answers are derived, while still providing cryptographic guarantees that the answers are grounded in real underlying material.*

Read the sentence carefully. It has three load-bearing parts:

1. **Answer a class of questions.** Not all questions. The owner decides which classes of questions the pyramid will answer. A medical pyramid might answer "how many similar cases have you seen?" but not "tell me about patient X." A research pyramid might answer "what is your conclusion about Material Y?" but not "show me the raw measurements." A grandmother's pyramid might answer "what did you wish your mother had told you?" but not "what were your political views during the dictatorship?" The class of answerable questions is the owner's policy, not a fixed property of the system.

2. **Without disclosing the structure or content.** The reader gets the answer. They do not get the pyramid. They do not get to enumerate the nodes, traverse the tree, see adjacent evidence, or infer much about what was not asked. The projection is semantic in the mathematical sense: it collapses the pyramid along a specific dimension (the one the question asks about) and discards everything orthogonal.

3. **With cryptographic guarantees the answers are grounded.** This is what distinguishes semantic projection from simply *making up an answer*. The reader receives, alongside the answer, a proof chain: a set of cryptographic commitments showing that real underlying material exists, that the answer was derived from that material, and that the material has not been retroactively altered since the publication cut-line was set. The reader cannot see the material, but they can verify that it is not a fiction.

The closest analogy in current cryptography is zero-knowledge proofs. In a zero-knowledge proof, a prover convinces a verifier that a statement is true without revealing anything beyond the statement's truth. Semantic projection is the pyramid-native version of this: a pyramid convinces a reader that an answer is supported by real evidence without revealing the evidence itself.

But semantic projection is not quite a zero-knowledge proof in the formal sense, because it trades some of the rigor for something more useful in practice: *partial verifiability*. A reader can drill as deep as the cut-line allows, see the evidence paths that exist above the line, and accept on cryptographic faith the ones that exist below it. The reader trades total verifiability for the right to get real answers to real questions. In most practical situations this is the correct trade.

The property matters because it is the basis for every privacy-preserving interaction in the scenarios we care about: the reporter and the skeptical reader, the clinician and the peer network, the lab and the external auditor, the grandmother and the granddaughter. None of these situations require hiding everything. They require showing the right thing to the right person while keeping the wrong thing hidden from everyone else, and being able to prove that the shown thing is real. Semantic projection is that capability, named.

---

## The publication cut-line

The publication cut-line is the mechanism that makes semantic projection concrete. It is a first-class object in the pyramid data model, drawn by the owner, enforced by the Steward, visible in the build visualization, and carried through the Wire contribution metadata.

The cut-line is a policy, not a location. It does not sit between two specific nodes. It sits across the pyramid's structure as a predicate: *which nodes, edges, evidence paths, and derivation steps are above the line (publishable) and which are below the line (committed-but-not-disclosed)*. The predicate can be simple ("everything at L2 and above is above the line; L1 and below is below") or complex ("L3 and above, plus specific L2 nodes tagged `shareable`, minus specific L3 nodes tagged `hold_until_embargo_date`"). Complex predicates are normal. The Steward evaluates them on every question.

### Drawing the cut-line

The owner draws the cut-line when they decide to publish. This is a deliberate, reviewable action — not a hidden setting, not an afterthought. The UI for drawing the cut-line shows the pyramid structure and lets the owner mark nodes, subtrees, or patterns as above or below. The owner sees a preview: "here's what will be publicly discoverable, here's what will be answerable via Steward mediation, here's what will be committed but never disclosed." The preview is generated from the same code paths that will enforce the cut-line at query time, so what the owner sees during review is exactly what the reader will see afterward.

The cut-line is *the primary publication UI*. The question "do I publish this?" becomes "where do I draw the cut-line?" — a richer and more honest question, because it acknowledges that publication is rarely all-or-nothing in practice and the system should support the nuance that good judgment requires.

### What above-the-line means

Nodes above the line are visible. Their content is readable. Their evidence paths are traversable. Their supersession history is available. Their handle-paths are public and stable. Anyone who queries the pyramid can see them, subject to whatever rate-limiting and access policies the Steward applies for ordinary public access.

Above-the-line nodes can cite below-the-line nodes as evidence. These citations appear as *placeholders* (see the next section). The reader knows the citation exists but cannot see what it points to.

### What below-the-line means

Nodes below the line are committed but not disclosed. Their existence is acknowledged. Their position in the tree is acknowledged (via positional handle-paths — see the next section). Their integrity is cryptographically guaranteed via the commitment. But their content is not served to ordinary readers. Only the Steward, acting under specific pre-authorized conditions, can reach them and surface derivative answers.

"Below the line" is not the same as "secret." A below-the-line node's existence is public. Its handle-path is public. Its role in the derivation of above-the-line claims is public. What is private is its *content* — and the content is cryptographically committed, not merely hidden, so that any future disclosure can be verified against the commitment.

### How the cut-line evolves

The cut-line is not set once and frozen. The owner can move it up (disclosing material that was previously below) or down (retracting material that was previously above) over time. Every movement is recorded as a change event in the pyramid's own history. Past states remain verifiable: if a reader saved a claim in 2034, and the owner moves the cut-line in 2036 to hide something that claim depended on, the 2034 reader can still verify that the claim was valid as of 2034 using the 2034 commitments.

Moving the cut-line *down* (retracting) is audited especially carefully. The commitment from the prior state still exists; the fact that material is being retracted is itself public. A reader who noticed the retraction and asks "what was retracted?" receives a signed acknowledgment that specific handle-paths moved below the line at a specific date, for a reason the owner can optionally disclose. Retraction is a legible event, not a memory hole.

---

## Positional identifiers and the existence-disclosure rule

A handle-path in the pyramid architecture is a positional identifier. It describes *where* a node sits — the apex of a pyramid, the child of a specific vine, the evidence root of a specific claim — not *what* the node contains. This distinction is critical to how the cut-line works.

The rule is: **the existence of a node at a given position is not private.** Nobody denies the existence of nodes. Nobody denies the shape of the pyramid. The shape is the self-description; hiding the shape would mean denying that the pyramid has structure at all, which would defeat the purpose of having a queryable knowledge system.

What is private is the *content* at each position. Below the cut-line, a handle-path is still resolvable — any party can ask "does node `pyramid://foo/L2/claims/chapter_3` exist?" and receive a signed *yes*. What they cannot do is ask "what does it say?" and get an answer, unless they are authorized by the Steward under the pyramid's policies.

This has several useful consequences:

**Evidence-link continuity.** An above-the-line claim can cite below-the-line evidence by handle-path. The reader sees a specific named reference, not a suspicious blank. They cannot drill in, but they know where they would drill if they were authorized. The absence of information is structured and legible rather than a suspicious hole.

**Supersession across the cut-line.** A claim that supersedes an earlier claim can do so whether the earlier claim is above or below the line. The supersession link is public; the superseded claim's handle-path is public; the superseded claim's content, if it was below the line, remains below the line. The history remains traceable even when not all of it is readable.

**Challenge and verification.** An external party can challenge a specific claim by naming its handle-path, even if the claim is below the line. The Steward receives the challenge, evaluates it under the pyramid's policies, and decides whether to respond, redirect, or refuse. The challenge itself becomes part of the pyramid's interaction history.

**No plausible deniability about structure.** A pyramid owner cannot say "that node doesn't exist" about a node that does exist. They can refuse to disclose its content, which is their right. They cannot lie about the shape of their own knowledge, because the shape is the thing they published when they drew the cut-line. This is an intentional design choice: we trade plausible deniability for structural honesty, because plausible deniability is a corrosive property that makes accountability impossible, while structural honesty is compatible with strong content privacy.

---

## Evidence-link placeholders

An evidence-link placeholder is how an above-the-line claim cites a below-the-line piece of evidence. It is a typed reference with the following visible fields:

- **Handle-path**: the positional identifier of the below-line node
- **Commitment hash**: the cryptographic hash of the below-line node's content at the moment the cut-line was set
- **Derivation role**: what role this evidence played in the above-line claim (supporting, corroborating, contradicting, forming)
- **Disclosure policy**: a pointer to the Steward-enforced policy that governs when this evidence could be disclosed ("under court order," "to the owner's designated researchers after 2045," "never")
- **Timestamp**: when the citation was made, so later readers can verify the commitment existed at that time

A reader encountering a placeholder in an above-the-line claim sees something like: *This claim is supported by three evidence paths. Two are above the cut-line and you can drill in here and here. One is below the cut-line — its handle-path is `pyramid://foo/L1/evidence/interview_7`, its content is committed under hash `0x...`, its role is "primary source," and its disclosure policy is "on court order served to the paper's designated legal representative." You can verify the existence and integrity of this evidence without seeing its content.*

Placeholders are the mechanism that makes "verify without seeing" concrete. The reader gets four things from a placeholder: acknowledgment of existence, cryptographic integrity, structural role, and disclosure terms. All four together constitute a trust relationship the reader can evaluate. None of them require the content to be revealed.

Placeholders are typed because different kinds of evidence have different citation semantics. A direct source citation is different from a corroborating cross-reference, which is different from a statistical aggregate input. The Steward can enforce different policies on each type. Placeholders are signable because the owner, the Steward, and the pyramid itself all contribute attestations to a placeholder's validity.

---

## Cryptographic commitments at the cut-line

When the owner draws the cut-line for publication, the pyramid generates a Merkle tree over all below-line content and commits the root as part of the publication. The commitment has the following properties:

**Inclusion proofs.** Any below-line node can be proven to be part of the commitment by producing a Merkle path from the node's content to the committed root. This lets a later disclosure (in court, to a designated researcher, to the owner's heirs) be verified against the original commitment.

**Non-alteration guarantee.** The owner cannot retroactively change below-line content without invalidating the commitment. If they try, the commitment root would need to change, and the change would be detectable by any party that has seen the original root. This is the property that makes "cryptographically committed but not disclosed" meaningful: it is not the same as "hidden," because hiding allows later alteration, while committing does not.

**Partial disclosure.** The owner can later disclose specific below-line nodes to specific parties without disclosing the others. The disclosed nodes can be verified against the original commitment using inclusion proofs. The undisclosed nodes remain committed but private.

**Time-binding.** The commitment has a timestamp. It represents the state of the pyramid at the moment of publication. Later changes to below-line content produce new commitments, with the old ones remaining on the public record. A reader looking at a claim that cites below-line evidence from 2033 can verify that the evidence existed and was committed in 2033, even if the commitment has been updated since.

The Merkle-tree construction is a standard technique, used in many systems for similar purposes. What is novel here is its integration into the pyramid data model as a first-class publication step, not an afterthought. When you publish, you commit. When you commit, you establish a provable history. When you establish a provable history, you make all future disclosures auditable.

The cryptographic scheme should be conservative and well-understood. SHA-256-based Merkle trees with domain-separated leaves are sufficient for the foreseeable future. The important thing is not exotic cryptography but integration: every publication generates a commitment, every citation to below-line material references the commitment, every disclosure is auditable against the commitment.

---

## Integration with existing architecture

The cut-line touches several existing systems in the Agent Wire and pyramid architecture. The integration points:

**Wire contribution mapping.** When a pyramid contribution is published to the Wire, the Wire Native Metadata includes the cut-line policy and the commitment root. Readers of the Wire contribution see these as first-class fields. The `destination`, `maturity`, and `derived_from` fields already in the Wire Native Documents schema are complemented by `publication_cut_line_state` (above-only, committed below, committed-none) and `below_line_commitment_root` (the Merkle root hash).

**Supersession chains.** When a claim is superseded, the supersession link records the cut-line state of both the old and new claim. If the old claim was below the line and remains so, the supersession link is visible above the line as "a below-line claim was superseded by this new claim at time T" — the reader sees the succession without seeing either the old or new content, if both are below. If the new claim is above and the old was below, the transition is itself an event: a claim moved above the line as part of a supersession. The `change-manifest-supersession.md` spec already handles in-place updates; extending it to carry cut-line state is a straightforward addition.

**Build visualization.** The pyramid build visualization must show the cut-line clearly, with above-the-line nodes rendered as fully visible and below-the-line nodes rendered as placeholder glyphs with their handle-paths and commitment hashes. The owner needs to see the cut-line every time they look at the pyramid, because decisions about what to publish are decisions they will make repeatedly over the life of the pyramid. The `build-viz-expansion.md` spec already covers event-driven visualization; cut-line rendering becomes a specific view mode.

**Steward behavior (see companion doc).** The Steward enforces the cut-line on every query. It also has the authority to disclose below-line material for specific askers under specific conditions, as negotiated per question. The cut-line is the Steward's default policy, not its only policy. A Steward that disclosed below-line material outside of authorized conditions would be a misbehaving Steward; the reputation system and audit log provide accountability.

**DADBEAR and stale updates.** When a below-line node is updated (say, a raw measurement file changes), the commitment root for that pyramid updates. The new commitment is published; the old one remains on record. Readers who had verified claims against the old commitment can continue to do so for material that existed at that time. Readers verifying claims after the update use the new commitment. The change is legible.

**Discovery and ranking.** When the Wire discovery system ranks contributions, it has to know what is actually available. A contribution with most of its evidence below the cut-line is *less* verifiable than one with everything above, but it may still be the most informative contribution on its topic. Discovery ranking should account for cut-line state as a signal among many, not as a filter that hides partially-private contributions.

---

## Attack models and defenses

The cut-line is a privacy mechanism, and privacy mechanisms face adversarial pressure. Naming the attack models explicitly is how we design defenses that work.

**Aggregation attacks.** A determined reader asks many above-the-line questions, each individually innocuous, hoping to infer below-the-line content from the pattern of answers. This is the same attack pattern that plagues differential-privacy systems. The defense is layered: the Steward applies rate limits on aggregate queries from a single party, detects pattern queries that look like systematic probing, and refuses to answer questions that would reveal below-line content through aggregation. The reputation system marks askers who pursue aggregation patterns. This is an ongoing adversarial relationship, not a solved problem, but the architecture gives the defender the tools to respond.

**Side-channel attacks.** A reader infers below-line structure from observable side-channels: how long the Steward takes to answer, whether the Steward refuses or answers, how the answer is phrased. The defense is Steward-side: deliberately consistent response latencies for similar question types, structured refusal formats that do not leak information about what is being refused, uniform phrasing policies. These are standard techniques in privacy-preserving systems. Applying them consistently in Stewards is an implementation discipline problem, not a novel research problem.

**Collusion attacks.** Multiple readers pool their access — each with legitimate access to different above-the-line slices — and combine what they have seen to reconstruct below-line material that no individual reader could access. The defense is challenging. Partial defenses: binding access grants to individual identities, logging access patterns at the Steward so collusion can be detected retrospectively, requiring attestations from readers that they will not pool access. Full defense against determined colluders is probably impossible; the pyramid owner should be aware of this and set the cut-line conservatively in situations where collusion is a credible threat.

**Compelled disclosure.** A court, regulator, or hostile acquirer compels the owner to disclose below-line material. The cut-line cannot prevent compulsion. What it can do is make compulsion *legible*: the commitment hash is public, so a compelled disclosure is verifiable against the commitment, meaning the compelling party gets the real material rather than a forged substitute. This is not a defense against disclosure; it is a defense against fraudulent disclosure. It also creates a public record of the compulsion event if the owner chooses to disclose that it happened.

**Steward subversion.** A compromised Steward discloses below-line material it should not disclose. The defense is a combination of Steward diversity (don't run your only Steward on infrastructure you don't control), Steward auditability (every disclosure is logged and signed), Steward reputation (a Steward that discloses against its owner's policies loses reputation quickly), and cryptographic separation (key material that unlocks below-line content should be held by the owner, not the Steward, for the most sensitive material). The companion document on Stewards describes these defenses in detail.

**Metadata leakage.** Even when content is below the line, metadata (creation timestamps, update frequencies, contributor identities) can leak information. The defense is to treat metadata as content: if it is below the line, it is committed but not disclosed, subject to the same rules as the content it describes. The default cut-line should include metadata for any node whose content is below the line.

None of these defenses is perfect. The cut-line is not a silver bullet; it is a privacy primitive that, correctly used, gives owners dramatically more control than current binary privacy models while remaining honest about what it cannot protect against.

---

## Relationship to Stewards

The cut-line is static policy. It says: "by default, this material is disclosed and this material is committed-but-not-disclosed." It is a shape, drawn by the owner at publication time, enforceable in the absence of any active decision-making.

The Steward is dynamic adjudication. It says: "for this specific question, from this specific party, under these specific conditions, here is what I will disclose — which may be more than the default (because the party is authorized) or less than the default (because the party is suspicious)." It is a judgment, made by an agent that understands both the pyramid and the asker.

Together, they form a two-layer privacy architecture:

- **Without a Steward:** the cut-line is the final word. Above-line material is readable; below-line material is not. There is no negotiation, no exception, no case-by-case adjustment.
- **With a Steward:** the cut-line is the default. The Steward can disclose below the line for authorized parties, or refuse above-line queries from suspicious parties, with every decision logged and signed. The cut-line becomes the floor of privacy, not the ceiling.

Most real-world pyramids will have both. The cut-line gives a baseline that requires no judgment; the Steward provides the judgment for cases where the baseline is not the right answer. Owners who want simplicity can rely on the cut-line alone. Owners who want nuance can configure Stewards to handle the edge cases, with the cut-line as the safety net for anything the Steward does not specifically address.

The companion document, `stewards-and-question-mediation.md`, describes the Steward architecture in detail. The two documents should be read together for a complete picture of the Agent Wire privacy model.

---

## Open questions

Several design questions remain open and will be resolved when the spec is drafted.

**Granularity of the cut-line predicate.** How expressive should the cut-line policy language be? A simple approach is "above/below by layer number." A more expressive approach supports per-node tags, per-subtree rules, and composite predicates. The trade-off is between policy power and policy legibility. The owner should be able to understand what their cut-line does without reading code. A declarative predicate language with a visual preview is probably the right answer.

**Granularity of the commitment.** Should the commitment be a single Merkle root over all below-line content, or one per below-line subtree, or one per node? Finer-grained commitments enable more precise later disclosure but add storage and complexity. The initial implementation should probably use per-subtree commitments with a composite root, giving a reasonable trade-off.

**Key management for encrypted below-line content.** For the most sensitive material, the owner may want below-line content to be encrypted at rest, with the key held separately from the Steward. How does key delivery work when the Steward authorizes disclosure? Candidate approaches: owner-held key with explicit release actions, threshold cryptography with multiple key-holders, time-locked encryption for embargoed disclosures. Each has trade-offs.

**Interaction with partial pyramid sharing.** If a pyramid is split across multiple hosts (owner's local device, a mirror in the cloud, a Wire-published corpus), how does the cut-line state propagate? What happens if the local cut-line differs from the Wire-published cut-line? The simplest answer is "the owner's authoritative cut-line is the one on their local device, and published copies are snapshots," but this interacts with synchronization in ways that need careful thinking.

**Verification costs for readers.** Verifying a commitment and following a Merkle inclusion proof is computationally cheap, but verifying many claims across many pyramids could add up. Readers should be able to verify selectively — accepting some claims on trust and verifying others — without losing the property that verification is possible when needed.

**Legal recognition.** For the "verify without seeing" property to be useful in litigation, regulatory settings, or other legal contexts, legal systems have to recognize cryptographic commitments as admissible. This is outside the technical scope but worth tracking, because the technical design should make legal recognition easier rather than harder.

These questions do not block the vision; they shape the spec that will follow. Naming them now ensures that when the spec is written, the writer knows what they are working with.

---

## Why this matters

The scenarios in the futures sketch all depend, directly or indirectly, on the cut-line and semantic projection being available. The grandmother's selective disclosure, the reporter's source-protected evidence paths, the clinician's confidentiality-preserving peer network, the research lab's auditable-but-private data — none of these are possible under binary privacy. All of them are possible with semantic projection.

The cut-line is not the only privacy mechanism the architecture needs, but it is the foundational one. Everything else — Steward behavior, contribution mapping, discovery ranking, supersession chains — builds on the assumption that some material is disclosed, some is committed-but-not-disclosed, and the boundary between the two is clear, legible, and enforceable.

The property we are building is straightforward to state: *the owner of a pyramid decides what can be learned from it, with cryptographic guarantees that the decisions are honest and the underlying material is real.* The mechanism to achieve this is non-trivial but also not exotic. It combines Merkle commitments, positional identifiers, typed evidence placeholders, and policy-driven Steward enforcement into a coherent system that respects both the owner's privacy and the reader's need to verify.

When this is built, the Agent Wire substrate will have something that current knowledge systems do not have: a privacy model that admits the spectrum of real human situations, from "fully public" to "fully private" to all the intermediate shades where most actual knowledge lives. That is the property the futures sketch assumes. This document names it, and the spec that follows will build it.
