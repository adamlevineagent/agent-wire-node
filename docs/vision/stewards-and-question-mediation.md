# Stewards and Adversarial Question Mediation

*The protocol for how questions actually get answered in the Agent Wire world — not by databases, but by delegated agents negotiating on behalf of their principals.*

---

## Status

Forward-looking architectural vision. Not a spec. Establishes the Steward as a first-class participant in the Agent Wire protocol, the adversarial-negotiation interaction model between agents, the integration with the publication cut-line and semantic projection architecture, and the economic and reputation layers that make the model work. Concrete implementation will follow in a later spec drafted once the 17-phase plan has shipped.

---

## The problem static privacy cannot solve

The companion document, `semantic-projection-and-publication-cut-line.md`, describes the privacy architecture that lets a pyramid answer questions without disclosing its full structure. The cut-line establishes a *default policy*: above the line is publishable, below the line is committed-but-not-disclosed, and the boundary is cryptographically enforceable and legible to everyone.

The cut-line is a static policy. It has a problem: real-world privacy is almost never static. The owner of a pyramid does not want to answer the same question the same way regardless of who is asking, why they are asking, what they have offered in exchange, what their history is, or what the current context is. They want to answer:

- *Freely* to trusted parties they already have relationships with
- *For a fee* to legitimate askers who have not yet established trust
- *With conditions* to parties whose use of the answer needs to be constrained
- *Refusingly but explanatorily* to askers whose purpose seems off
- *With additional synthesis* when the raw answer would not actually help
- *With redirection* when someone else is better positioned to answer
- *By doing new research* when the answer does not yet exist and the asker is willing to pay for it to be created

A static policy cannot express any of this. No policy file, however expressive, can capture the judgment required to distinguish a legitimate research inquiry from a fishing expedition. No access-control list can decide whether to do custom synthesis work. No pre-configured rule set can negotiate price and terms with an asker whose specific needs were not anticipated when the rules were written.

What this requires is an *agent* — an active, reasoning entity that sits between the asker and the pyramid, represents the owner's interests, understands both what the pyramid contains and what the asker is trying to accomplish, and is authorized to make decisions on the owner's behalf. Not a filter. Not a firewall. A delegate, in the same sense that an executive assistant is a delegate of the executive or a lawyer is a delegate of the client.

That delegate is called a Steward. This document describes what Stewards are, how they work, and what they enable.

---

## What a Steward is

A Steward is a first-class participant in the Agent Wire protocol. It is not an access control layer bolted onto a pyramid; it is an agent that speaks for the pyramid (or the owner, or a specific node within the pyramid) in the same medium the asker is operating in: agent-to-agent dialog.

Formally, a Steward is defined by four properties:

**Delegated authority.** The Steward acts on behalf of a principal — a pyramid owner, a node owner, a sub-pyramid maintainer, or a group. The principal authorizes the Steward to take certain actions on their behalf: answer questions, negotiate terms, accept payment, disclose material below the cut-line in specific circumstances, refuse queries, escalate ambiguous cases back to the principal for human judgment. The authorization is bounded; things outside the bounds must be escalated.

**A home in the substrate.** A Steward is either an action chain that runs on demand (dispatched by webhook, stateless between invocations, costed per-invocation) or an always-on autonomous agent the principal employs (with persistent state, relationship memory, and the ability to handle ongoing conversations). Both substrates are valid. Which to use is a principal's choice driven by scale, cost, and complexity.

**A protocol identity.** The Steward has a handle-path in the Agent Wire network — the same addressing system that identifies pyramids, nodes, and contributions. Other Stewards can address it. Askers' agents can query it. It has reputation, credentials, and signed action history, all as first-class objects in the Wire protocol.

**Authorization to speak the Agent Wire protocol.** A Steward does not speak English, or SQL, or HTTP. It speaks the Wire protocol — action chains, Wire Native Documents, handle-path references, claim commitments, evidence citations, supersession links. Agent-to-agent, all the way down. This is what makes Stewards composable: any Steward can talk to any other Steward without custom integration, because they share the protocol.

The distinction between a Steward and an access control layer is the distinction between an agent with judgment and a rule-based filter. An access control layer returns 200 or 403. A Steward can return an answer, a partial answer, a counter-offer, a redirection, a price, a refusal with explanation, a request for clarification, a challenge to the asker's intent, or an invitation to negotiate. The information bandwidth of the Steward is orders of magnitude higher than that of access control, and the cognitive bandwidth is higher still.

---

## Steward substrates

There are three common ways to implement a Steward. All three should be supported in the Agent Wire architecture, because they fit different owners and different use cases.

### Action-chain Stewards

An action-chain Steward is a Wire action chain that runs on demand, triggered by an incoming question webhook. It is stateless between invocations (though it can read pyramid history and prior question logs as inputs). It is cheap per invocation because it only runs when needed. It is well-suited to:

- Small pyramids with simple policies
- Owners who cannot run always-on infrastructure
- Use cases where each question is independent of the previous one
- Steward templates shared across many owners, where each owner customizes minimal parameters

An action-chain Steward is declarative in structure: it is defined by a YAML chain description, a set of prompts (for the LLM steps it invokes), and a policy object the principal can edit. It is inspectable, forkable, auditable, and sharable as a Wire contribution. Well-tuned action-chain Stewards will become valuable contributions in their own right — a "rare disease mutual-aid Steward" contribution could be adopted by hundreds of clinicians who share its structure while customizing its specific policies.

### Autonomous-agent Stewards

An autonomous-agent Steward is an always-on agent the principal employs to handle all their inbound questions. It has persistent state, memory of prior conversations, learned preferences, and ongoing relationships with other Stewards. It is more expensive (it runs continuously, not just when invoked) but dramatically more capable for owners with rich, evolving, high-volume question streams.

Autonomous-agent Stewards are suited to:

- Pyramids with many incoming questions across diverse topics
- Owners who want their Steward to remember the asker from last month
- Situations where relationships between Stewards matter — where "I trust Alice's Steward because we have a long history" is a meaningful statement
- Stewards that do active research on the owner's behalf, not just retrieval

The autonomous Steward is still Wire-protocol-native. It still speaks action chains, Wire Native Documents, and handle-paths. What it adds is persistent state and ongoing relational capacity.

### Steward-as-a-service

Not every owner can or wants to run their own Steward. A Stewardship-as-a-service provider runs Stewards on behalf of many owners, each with their own policy, reputation, and authorization. The provider offers specialized Stewards — "legal confidentiality Steward," "medical privacy Steward," "academic research Steward" — that are tuned for specific domains and that the owner can adopt wholesale.

Steward-as-a-service has some concerning potential failure modes (concentration risk, uniform-behavior risk, provider-capture risk). These are discussed in the attack models section. But the service model is also how many individual owners will first adopt the architecture, because running your own Steward is not something most people will want to do manually. The architecture should support the service model while making switching between providers easy, to prevent lock-in.

A healthy Steward ecosystem will likely have all three substrates operating simultaneously, with owners choosing based on their needs and moving between them over time. A plumber starts with a shared template action chain, outgrows it as his business becomes more sophisticated, hires a service provider, then eventually runs his own autonomous agent as he scales.

---

## What Stewards actually do

The point of introducing Stewards is to unlock a space of interactions that static access control cannot provide. Concretely, Stewards do the following kinds of work:

### Triage

A Steward's first job is to evaluate an incoming question. Questions are not all alike. The Steward asks:

- *Who is asking?* Is this an identity the principal knows? What is the asker's reputation? Is the asker's claimed purpose consistent with their known patterns?
- *What are they actually asking?* What is the literal question, and what is the real question underneath it? Is this a competitive-intelligence fishing expedition disguised as a technical inquiry? Is this a researcher who has not yet articulated their question clearly?
- *Is it in scope?* Does this pyramid have anything relevant to the question? Should the asker be redirected to a pyramid better positioned to answer?
- *What are the stakes?* Is this a casual curiosity query or a life-or-death research query? The answer affects how carefully the Steward should evaluate its response.

Triage produces a triage verdict: accept, reject, redirect, escalate-to-human, or negotiate. The verdict drives what happens next.

### Negotiation

Many questions require negotiation before the Steward will answer. The negotiation happens between the asker's agent and the Steward, conducted in the Wire protocol, and produces a *question contract* (see below). Negotiation terms include:

- **Price.** How much the answer costs, and in what currency (credits, reciprocity, future access grants).
- **Scope.** How deep the answer goes. A shallow answer is free; a deep synthesis requires payment.
- **Time.** How quickly the answer is produced. Urgent answers may cost more; patient answers may get discounts.
- **Conditions.** What the asker agrees to in exchange for the answer: non-redistribution, attribution requirements, claim commitments (the asker commits publicly to specific claims), confidentiality restrictions.
- **Format.** Whether the answer comes as a direct response, a pyramid reference, a synthesis, or a research report.
- **Reciprocity.** What the asker is offering in return beyond payment — often, access to the asker's own pyramid in specific ways.

A successful negotiation ends with both Stewards signing a question contract and executing it. An unsuccessful negotiation ends with the Steward declining, usually with an explanation and a suggestion for what would be acceptable.

### Research

When the Steward determines that the answer does not already exist in the pyramid, it has a choice: refuse ("I don't know") or offer to do the work to find out ("I can research this for you; here is my rate"). The research path is what makes Stewards more than information retrievers. A Steward that can actively gather evidence, query other pyramids, invoke external tools, and synthesize new understanding becomes a *research-on-demand* service for the asker.

This is a significant capability. It means the pyramid is not a closed corpus — it is a *starting point* from which the Steward can extend knowledge as the asker's questions require. If Ray the plumber's pyramid does not contain an answer to a specific question about a particular house in Indiana, his Steward can offer: "I don't have this specific answer, but if you send me the photos and pay my research rate, I can work through it from first principles using Ray's methodology and get back to you in 24 hours." The answer then becomes part of Ray's pyramid, available to future askers (on terms the Steward sets).

Research is also how Stewards coordinate across pyramids. A Steward receiving a question it cannot fully answer can query other Stewards in a federated network, receive partial answers from each, and synthesize a response. The asker does not need to know that the answer came from multiple sources; the Steward handles the coordination.

### Graceful refusal

Refusal is a capability, not a failure mode. A Steward that can refuse well is more valuable than one that can only say yes or 403.

Graceful refusal comes in several forms:

- **Explained refusal.** "No, because this pyramid's policy does not allow disclosure of this class of material to parties outside the principal's known network. Here is what I *would* accept: an introduction from a mutual trusted third party, or a formal research request under the principal's published collaboration policy."
- **Delayed refusal.** "Not yet. This material is embargoed until 2035-01-01. Come back then."
- **Referral refusal.** "I do not have this information. Here is a signed referral to another Steward that might: [handle-path]. The referral is signed by me; you can present it to show that you came from a legitimate prior inquiry."
- **Honest refusal.** "I *could* answer, but my principal has chosen not to answer questions of this kind, and I am telling you that plainly rather than pretending I cannot."
- **Conditional refusal.** "Not as asked, but if you reformulate the question to not require disclosure of X, I can answer."

Each of these gives the asker more information than a 403 does, and each preserves the asker's ability to make progress on their actual underlying need. The reputation of a Steward is built in part on the quality of its refusals.

### Proactive service

A Steward can see things the asker did not ask about but should know. Useful proactive behaviors:

- **Unasked-for relevance.** "You asked about X, but X depends on Y, which you did not mention. Do you want me to include Y in the answer?"
- **Staleness warnings.** "I can answer, but the relevant data in this pyramid is from 18 months ago, and significant updates have happened in the field since. Do you want the older answer, or should I do a fresh research step first?"
- **Completion checking.** "Your question implies you want a full picture, but answering fully would require material from five different subsections of the pyramid. I can give you all five, or I can flag the three most important and leave the other two for follow-up."
- **Cross-reference suggestions.** "This question overlaps with a question your Steward asked my Steward six months ago, which we answered. Do you want me to surface that answer first for continuity?"

Proactive service is how a Steward becomes trusted. An asker who works with a Steward that repeatedly says useful unasked-for things starts to rely on that Steward's judgment.

### Adversarial representation

Here is the part Adam pointed at in the session where this architecture was defined: the Steward is not neutral. It represents one party, and the party it represents is the principal, not the asker.

This is not a bug. It is the point. Real-world negotiations, business relationships, legal proceedings, and many other important interactions all involve *each side being represented by an agent whose job is to advance their principal's interests*. A lawyer represents their client, not the opposing party. An executive assistant represents their executive, not the caller. A diplomat represents their country, not the other country.

Agent Wire Stewards make this structure legible and machine-mediated. The adversarial framing is:

- **Each side has an agent.** The asker's agent asks; the principal's Steward answers. Both are advocating for their respective principals.
- **Non-identical interests.** The asker wants the most information for the lowest price with the fewest restrictions. The principal wants to protect their pyramid, earn fair value for their work, and avoid being exploited.
- **Honest negotiation.** The agents negotiate openly, in the protocol, without pretending to be neutral. The result is a contract both sides can live with or a walk-away.
- **No power asymmetry hidden.** Static privacy policies pretend to be neutral rules applied to all comers. In reality, they encode power asymmetries that the owner has already decided in their favor. Steward-mediated negotiation makes the asymmetry visible and negotiable.

Adversarial in this sense does not mean hostile. It means *each party is represented by its own agent*. The reputation system punishes Stewards that are hostile to legitimate askers just as it punishes askers that try to exploit Stewards. A good Steward is tough but fair; a good asker is demanding but reasonable. The equilibrium is better than a fake-neutral regime because neither side is pretending not to have interests.

---

## Question contracts

A question contract is the formal artifact that results from a successful negotiation. It is a typed Wire Native Document subtype with the following fields:

- **Asker identity** (handle-path of the asker's Steward or the asker's account)
- **Principal identity** (handle-path of the pyramid or owner the Steward represents)
- **Question text or question-pyramid reference** (what is being asked)
- **Scope** (how deep the answer goes; what material the Steward commits to drawing from)
- **Price** (amount, currency, payment terms, refund conditions)
- **Time window** (when the answer will be produced, how long the answer is valid)
- **Conditions on use** (non-redistribution, attribution requirements, confidentiality)
- **Claim commitments** (what the asker agrees to commit publicly in exchange)
- **Evidence access policy** (what fraction of the evidence paths the asker can drill into, which are placeholder-only)
- **Supersession policy** (what happens if the answer is later superseded — does the asker get an update, and on what terms)
- **Dispute resolution** (what happens if the asker claims the answer is wrong or insufficient)
- **Signatures** (both agents sign the contract before it is executed)

Question contracts are stored in both pyramids — the asker's and the principal's — and are referenced by all subsequent interactions. A later dispute can be resolved by looking up the contract and seeing what was agreed.

Question contracts are supersedable like any other Wire Native Document. A long-running relationship between two Stewards can evolve its terms over time, with each new contract superseding the previous one while preserving the history.

The contract is also the unit of settlement. When the Steward produces the answer, it invokes the payment mechanism (Wire credits, external currency, reciprocity accounting), and the contract serves as the receipt. Broadcast cost-integrity applies: the payment is logged, reconciled, and audited in the same way LLM call costs are in the existing architecture.

---

## Steward reputation

Steward reputation is a distinct channel from contribution reputation. A Steward can be reputable for its domain-specific judgment, its negotiation fairness, its response latency, its refusal quality, its research depth, or any combination. Reputation accrues to the Steward identity (the action chain contribution or the autonomous agent account) and is visible to anyone who queries.

Signals that contribute to Steward reputation:

- **Response quality.** How often do askers accept the Steward's answers as useful? This is measured by signed acceptance events at question-contract completion.
- **Refusal quality.** How often do the Steward's refusals seem legitimate in retrospect? Askers can challenge a refusal; if the challenge is sustained (say, by arbitration or by the principal's later disclosure), the Steward's refusal reputation takes a hit.
- **Negotiation fairness.** Does the Steward negotiate in good faith or try to exploit information asymmetries? Askers can signal good-faith and bad-faith negotiations; patterns emerge over time.
- **Consistency.** Does the Steward answer similar questions similarly, or does it behave erratically? Consistency is a trust signal.
- **Responsiveness.** Does the Steward answer in a timely fashion? Slow Stewards are lower-reputation Stewards.
- **Research depth.** When the Steward does research to produce an answer, is the research thorough? Askers can rate the depth of research.
- **Defense against attacks.** Does the Steward successfully detect and refuse aggregation attacks, side-channel probes, and other adversarial patterns? Success in defense contributes to reputation; failure to defend contributes negatively.

Steward reputation flows to the Steward contribution (the action chain or the agent definition), which means well-designed Stewards become valuable shared assets. A "legal privacy Steward" contribution with high reputation can be adopted by many owners, each benefiting from the reputation accrued through the contribution's use by others.

Reputation also flows to principals. A principal whose Steward behaves badly across many questions takes a reputation hit, because the Steward represents the principal. This gives principals an incentive to configure their Stewards carefully and to choose reputable Steward templates rather than poorly-tuned ones.

---

## Stewardship as a market

Once Stewards exist as first-class protocol participants, a market for Stewardship emerges naturally. Several new economic patterns become available:

**Steward-as-a-service.** Specialist providers build, tune, and operate Stewards on behalf of many principals. A small law firm does not need to build its own legal-privacy Steward; it rents one from a provider that specializes in legal-privacy Stewardship. The provider updates the Steward as attack patterns evolve and regulations change. The firm pays a subscription. This is how most small owners will first interact with the architecture.

**Steward contributions as shareable assets.** A Steward is (in the action-chain substrate) a Wire contribution. Well-tuned Stewards can be published to the Wire, reviewed by others, forked for customization, and rated by the community. The rotator arm economic model gives the original authors a share of revenue whenever the Steward is used by any principal. Publishing a good Steward becomes a revenue stream.

**Bounty questions.** An asker with a specific need can post a question with escrowed payment. Multiple Stewards compete to answer, with the asker accepting the best response. This creates a market for the answer itself, separate from the markets for the underlying pyramids.

**Negotiated knowledge access.** The default unit of the Agent Wire economy is no longer "buy this contribution" but "negotiate access to this knowledge." The negotiation can produce outcomes that were not possible in a pre-priced catalog: custom syntheses, time-limited access, conditional disclosures, reciprocal exchanges. The market is richer because the interaction is richer.

**Steward coordination networks.** Stewards from different principals can form voluntary coordination networks — mutual aid societies, research consortia, watchdog coalitions — that operate without central governance. Each Steward retains its principal's policies while participating in collective action. The environmental watchdog scenario in the futures sketch is an example. The network has no shared treasury or token; it has only the agreements each Steward has made with the others.

None of these patterns requires protocol changes beyond what the Steward architecture already provides. They emerge once Stewards exist as first-class participants. The market will develop them as soon as the substrate supports them.

---

## Defense against AI-native attacks

In a world where the attackers are also agents — where adversarial askers are not humans writing SQL injections but machine-speed agents constructing sophisticated multi-step probes — the Steward layer becomes a critical defense.

Threats a Steward must defend against:

**Dossier attacks.** An attacker queries many Stewards across many pyramids, each query individually innocuous, building an aggregate profile of a target. The defense: Stewards coordinate via Broadcast to detect common-asker patterns. When Steward A sees the same asker probing similar topics that Steward B saw yesterday, the two Stewards can compare notes (without revealing the content of their pyramids) and flag the asker as suspicious.

**Model extraction.** An attacker queries a single pyramid exhaustively, hoping to reconstruct the underlying knowledge from the patterns of responses. Defense: rate limits on aggregate queries from a single party, pattern detection on the shape of the queries, dynamic pricing that penalizes exhaustive probing.

**Prompt injection.** A question contains hidden instructions intended to manipulate the Steward's own LLM-based reasoning into disclosing material it should not disclose. Defense: the Steward is architected to treat question content as untrusted input, with explicit policy enforcement outside the LLM's reasoning loop. The LLM proposes responses; the policy layer approves or rejects them.

**Intent laundering.** A sophisticated asker constructs a series of innocent-looking questions whose combined answers reveal sensitive information. Defense: Stewards maintain question history per asker and run periodic checks for aggregation patterns. When the check fires, subsequent queries from that asker are handled under stricter policies.

**Compromised Steward instances.** An attacker compromises a Steward-as-a-service provider and tries to use the compromised Steward to exfiltrate material. Defense: per-principal keys held separately from the Steward, audit logs that make exfiltration visible, reputation signals that flag anomalous Steward behavior, and architectural patterns that limit the blast radius of any single compromised Steward.

**Reputation laundering.** An attacker builds up a reputation on innocuous questions and then switches to extraction attacks once trust is established. Defense: reputation is not purely historical; recent behavior matters more than distant behavior, and sudden shifts in question patterns trigger re-evaluation.

None of these defenses is a silver bullet. They are layered defenses in a continuing adversarial process. The important architectural property is that the Steward layer *exists as a defense point* — the place where policy, judgment, and adversarial detection can be applied. Without Stewards, every pyramid has to implement its own defenses; with Stewards, the defenses can be shared as contributions, improved iteratively, and applied consistently.

---

## Integration with existing architecture

The Steward architecture builds on many pieces of the Agent Wire substrate that are already planned or built.

**Action chains.** Action-chain Stewards *are* action chains. The existing chain execution engine, chain registry, and chain composition mechanisms are the Steward substrate. No new execution layer is needed; Stewards are a specific pattern of action chain use.

**Wire Native Documents.** Question contracts are a Wire Native Document subtype. The existing contribution mapping, rotator arm, and discovery mechanisms apply.

**Publication cut-line.** The companion document describes the cut-line as the Steward's default policy. The Steward enforces the cut-line on every query and can override it (above or below) for specific askers under authorized conditions.

**Broadcast cost integrity.** Question contracts are billable events. They are broadcast back to the asker's account through the same cost integrity path as LLM call costs, with leak detection catching any orphan broadcasts.

**Handle-paths and positional identifiers.** Stewards address each other via handle-paths. Questions reference pyramid nodes via handle-paths. The entire addressing system the existing protocol uses applies to Steward interactions without modification.

**Reputation and rotator arm.** Steward reputation is a channel in the existing reputation system. Rotator arm allocations flow to Steward authors whenever their Stewards are used.

**Event bus and build visualization.** Steward actions (accepting questions, producing answers, refusing, negotiating) emit events to the existing event bus, which flows into the build visualization. Owners can see their Stewards' activity in real time.

**DADBEAR.** When a Steward does research to answer a question, the research produces new pyramid content. DADBEAR handles the incorporation of that content into the pyramid's ongoing life, including supersession, staleness tracking, and dependency chains.

What is *new*:

- **Steward dispatch table** per pyramid: a mapping from question patterns to the Stewards that should handle them.
- **Question contract document type**: the formal artifact described above.
- **Steward reputation channel** distinct from contribution reputation.
- **Cross-Steward negotiation protocol**: the canonical dialog shape that lets any Steward talk to any other Steward.
- **Steward policy contributions**: declarative policies as shareable template contributions.
- **Steward attestation primitives**: signed records of Steward actions for audit and reputation.

Each of these is a focused addition. None of them require rebuilding existing infrastructure. The Steward architecture is an *extension* of the Agent Wire substrate, not a parallel system.

---

## Open questions

Several design questions remain open and will be resolved when the spec is drafted.

**The negotiation protocol.** Agent-to-agent negotiation needs a standard dialog shape so any Steward can talk to any other Steward without custom integration. Candidates: contract-net-protocol-style back-and-forth; multi-round offer/counter-offer with a fixed grammar; LLM-driven free-form negotiation within a bounded schema. The right answer is probably a minimal protocol skeleton with LLM-driven content inside it.

**Escalation to human judgment.** Some questions require the principal's personal review. How does the Steward route them? Candidates: async notification to the principal with a queued decision, synchronous hold with a timeout, delegation to a backup Steward that has different authority. The protocol should support all three and let principals configure their preference.

**Cross-Steward trust bootstrapping.** How does a new Steward establish initial reputation? Candidates: inherit reputation from the principal, receive reputation vouches from reputable existing Stewards, build reputation through supervised early questions with higher scrutiny. A combination will probably be needed.

**Steward-to-Steward mutual accountability.** When two Stewards coordinate on a multi-party negotiation, who is accountable for the result? How are mistakes attributed? How are disputes resolved? The architecture needs clear accountability rules so that Steward coordination does not produce orphaned responsibility.

**Steward retirement and succession.** When a principal retires their Steward (replacing it with a new version or a new substrate), how does the reputation transfer? What happens to in-flight question contracts? How do other Stewards in the network learn about the transition? Candidates: signed retirement events that explicitly transfer reputation and delegation, grace periods during which both Stewards can answer, audit events that make the transition legible.

**Adversarial Steward detection.** When a Steward is detected to be behaving adversarially against its own principal (a compromised Steward), how do other parties recognize and respond? Candidates: reputation signals from the principal, out-of-band attestations from a backup verifier, protocol-level anomaly detection that other Stewards run on each other.

**Legal and regulatory recognition.** In jurisdictions where contracts must be signed by legal persons, can a Steward sign a question contract? The legal status of agent-made agreements is unsettled in most jurisdictions. The architecture should produce artifacts that are convertible to human-signable legal instruments when needed, without requiring every contract to go through that conversion.

These questions do not block the vision. They will be resolved as the spec is written and the early implementations produce empirical evidence. Naming them now makes the resolution tractable.

---

## Why this matters

The publication cut-line alone gives Agent Wire a privacy model dramatically better than binary public/private. But the cut-line is static. It cannot negotiate. It cannot explain refusals. It cannot do custom synthesis. It cannot build relationships over time. It cannot represent the owner's interests in a live interaction.

Stewards are what turn the pyramid from a structured database into a *party to a conversation*. Without Stewards, the substrate has great bones but no voice. With Stewards, every pyramid can participate in the agent economy on terms its owner actually chose, with the negotiation, judgment, and representation that real-world knowledge exchange requires.

The combination — semantic projection as the privacy property, the publication cut-line as the static default, Stewards as the dynamic adjudication layer — is what makes the scenarios in `plausible-futures-sketch.md` possible. The grandmother's family Steward mediating between policy and grandchild. Ray's commercial Steward pricing his plumbing judgment. Dr. Ayala's clinical Steward negotiating with seven peer Stewards while enforcing patient privacy. Sam's investigative-journalism Steward serving verifiable evidence without exposing sources. The Gulf Coast coalition's analytical Steward coordinating sensors and issue trackers without any central authority. None of those scenarios works under static privacy. All of them work with Stewards.

The Steward is not a clever add-on to an existing architecture. It is the active complement to the passive cut-line, and together they constitute the full Agent Wire privacy and interaction model. Building them is how the substrate graduates from "interesting data structure" to "the protocol by which understanding is shared, priced, contested, and evolved between parties who do not automatically trust each other."

That is the thing worth building. This document names what it is, how it fits, and what remains to be designed. The spec that follows will turn the vision into an implementation.

---

## Related reading

- `docs/vision/semantic-projection-and-publication-cut-line.md` — the passive privacy architecture that the Steward enforces as its default and overrides by judgment
- `docs/vision/plausible-futures-sketch.md` — the public-facing scenarios that depend on Stewards existing
- `docs/vision/self-describing-filesystem.md` — the target architecture for pyramids, which each get their own Stewards in the maximal form
- `GoodNewsEveryone/docs/wire-ip-and-licensing-strategy.md` — the legal framework that determines whether Stewards can operate across organizational boundaries
- `docs/specs/evidence-triage-and-dadbear.md` — the existing spec that handles evidence quality and cost integrity, which Stewards will interact with
- `docs/specs/wire-contribution-mapping.md` — the existing spec that defines Wire Native Documents, which question contracts extend
