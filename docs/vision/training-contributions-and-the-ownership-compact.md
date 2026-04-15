# Training Contributions and the Ownership Compact

*A Wire-native model training flywheel: opt-in data contribution, credit-earning dataset work, proportional permanent ownership, and the path from fine-tuning today to community-owned pyramidal base models tomorrow.*

---

## Status

Forward-looking architectural vision. Connects the Ouro/LoopLM capability window (Nov 2025) to a strategic opportunity: Agent Wire contributors opting their workflow data into an explicit training-data-as-contribution economy with proportional, permanent, post-buyout ownership in the resulting models. Not a spec. Not a current 17-phase work item. Design-land thinking captured so the option is legible when the near-term work has shipped and the next strategic move is on the table.

Companion documents:
- `docs/vision/plausible-futures-sketch.md` — the scenarios this vision would enable
- `docs/vision/stewards-and-question-mediation.md` — the Steward protocol that consumes the resulting models
- `docs/vision/semantic-projection-and-publication-cut-line.md` — the privacy architecture that governs what can be contributed
- `docs/vision/self-describing-filesystem.md` — the local substrate the models run on
- `GoodNewsEveryone/docs/wire-ip-and-licensing-strategy.md` — the IP framework this compact extends

---

## The moment

Two things landed recently that make this conversation worth having now rather than in two years.

**First, small looped language models are viable.** The Ouro paper (Zhu et al., Nov 2025) establishes that 1.4B-2.6B parameter models trained with recurrent depth and adaptive exit can match 4-8B standard transformer performance on reasoning-heavy workloads. The mechanistic result — confirmed by Physics-of-LMs-style synthetic experiments — is that recurrent depth buys *knowledge manipulation* without adding *knowledge storage*. Both looped and non-looped models hold approximately 2 bits of knowledge per parameter; looping specifically improves the model's ability to compose, traverse, and operate over that knowledge. Theoretically, LoopLM can perform graph reachability over a combined context+parametric knowledge graph in O(log D) sequential steps, an exponential improvement over CoT-style latent reasoning.

This matters enormously for pyramid-native use cases, because **pyramids already provide the knowledge**. The substrate is designed from the ground up to feed retrieved, cited, superseded context into each reasoning step. What the model needs to do is manipulate that context cleanly — extract, cluster, synthesize, triage, negotiate. A small model optimized for manipulation, running over a pyramid that provides the facts, is a better architectural fit than a knowledge-heavy giant being used at 5% of its capacity on every call.

**Second, the training data crisis is no longer theoretical.** The frontier labs are running out of easy data. Web scraping has hit copyright pushback, ToS enforcement, and robots.txt standoffs. User-data-training controversies are recurring across major products. Regulatory pressure on what can be trained on is growing across multiple jurisdictions. "Ethical data provenance" has gone from talking point to strategic asset — any model line that can credibly claim "we trained on data our users explicitly contributed, with clear provenance, under an explicit equity compact" will have access to use cases, regulatory environments, and institutional adopters that data-laundering models cannot reach.

Together, these two things open a window: the capability threshold for small, efficient, pyramid-native models has come down at the same moment that ethical data positioning has become strategically decisive. **Agent Wire is unusually well-positioned to walk through this window**, because its architecture already generates the right kind of data as a byproduct of normal use, and its economic primitives already support the right kind of compact with contributors.

---

## The load-bearing observation

Wire generates, as a byproduct of normal use, the training data the industry is missing. It does not need to be collected separately. It does not need to be scraped, licensed, purchased, or laundered. It is produced continuously whenever a Wire Node does the work it was built to do, and each piece of it carries provenance that web-scraped data fundamentally cannot have.

Consider what a Wire Node user produces in a typical day:

**Supersession pairs.** Every time a pyramid node is updated in place via the change-manifest supersession pipeline, the old node and the new node form a *labeled pair*: "this output was superseded, this is the correction, and here is the reason." For a model, this is as close to a preference signal as training data gets. It is not a human-rated "A vs B" pair — it is a real-world instance of a person or agent deciding that one output was insufficient and substituting a better one, with the reasoning attached. Frontier labs pay enormous sums for this kind of data. Wire generates it as a side effect of supersession being a first-class operation.

**Evidence-grounded triage decisions.** Every time the DADBEAR pipeline makes an ingest decision — accept, reject, revise, supersede, rescope — the decision comes with the evidence it considered, the policy it applied, and (over time) the downstream outcome. Was the decision correct? Did the rejected material turn out to be useful later? Did the accepted material get superseded? This produces labeled judgment traces with observed outcomes, which is what reinforcement learning with verifiable rewards needs but rarely has at scale.

**Chain step executions with structured feedback.** Every chain step that runs in the Wire Node executor produces an input-output pair tagged with the step type, the chain it belonged to, the build that invoked it, and the eventual verdict on whether the build succeeded. When a build verifier catches a problem, the upstream chain step that produced the bad output gets flagged. When a wanderer reaches the end cleanly, every upstream step is implicitly validated. This gives credit assignment at the step level — which is exactly the signal needed to fine-tune the model that produced the step.

**Annotation and FAQ contributions.** The annotation/FAQ contribution pattern (per the feedback memory rule "all data should use the annotation/FAQ contribution pattern, not separate tables") means that every enrichment a contributor adds is already structured as a contribution with provenance. Each annotation is an expert's distillation of a context into a Q/A pair grounded in a specific pyramid location. This is the exact shape of high-quality fine-tuning data for synthesis and retrieval models.

**Steward negotiation traces.** Once the Steward architecture lands (`stewards-and-question-mediation.md`), every Steward decision — accept, refuse, negotiate, counter-offer, research-and-return — becomes a labeled judgment example tagged with the asker's identity, the principal's policies, the question contract proposed, and the outcome. Stewards are exactly the kind of agent whose behavior is best learned from lots of examples of good Steward behavior, and good Steward behavior is precisely what the Wire generates at scale once Stewards are deployed.

**Question pyramid resolution traces.** When a question pyramid resolves — from the apex question down through sub-questions, evidence gathering, synthesis, and final answer — the resolution chain is a complete multi-hop reasoning trace with evidence citations, supersession history, and (eventually) a human-validated answer. These are the kind of traces used to train reasoning models, but generated in the wild at task-realistic difficulty, with the full reasoning path and the eventual correctness verdict attached.

**Build-visualization replay.** The build viz already captures the full execution trace of a pyramid build as a sequence of typed events. Replaying the trace gives the full causal history of how the pyramid was constructed: which chains ran, in which order, with which inputs and outputs, which steps were retried, which were superseded, which produced the final nodes. This is the kind of end-to-end training signal that only a system with first-class observability can produce.

**Agent annotations and reasoning margins.** When an agent leaves a note on a pyramid node as it works, the note is effectively the agent's chain-of-thought made durable and cited. For training a model to *reason about* pyramid operations — as opposed to just generate them — these annotations are gold. They are naturally-occurring reasoning traces attached to the specific context that produced them.

Notice what is common to all of these: **they are not generated by asking users to do extra work**. They are generated by users doing exactly what the Wire was built to help them do. The dataset is produced as a byproduct of pyramid building, contribution curation, Steward mediation, and normal Wire Node operation. No labeling campaign. No mechanical turk. No paid annotators. The product is the dataset collection mechanism.

Notice also what is not common to this data but is common to web-scraped data: **nothing here is stolen**. Every piece of it has provenance. Every piece of it has an owner. Every piece of it can be contributed — or not contributed — under terms the owner sets. The ethics of the dataset are not hopes; they are architectural properties.

---

## The proposal

Make training data a first-class contribution type in the Agent Wire economy.

Not a side channel. Not an afterthought. Not a privacy policy paragraph. A contribution type that sits alongside skills, templates, actions, annotations, and FAQ entries in the Wire's existing contribution taxonomy, with its own Wire Native Document subtype, its own rotator arm allocation, its own supersession semantics, and its own rent-to-own ownership accounting.

Concretely:

**Training data is a typed Wire contribution.** The Wire Native Documents schema gains a new contribution subtype — call it `training_contribution` — whose payload is a structured training sample (supersession pair, triage decision, chain step execution, Steward trace, etc.) with its provenance chain, its contributor identity, and its opt-in scope attached. It is stored, addressed by handle-path, and handled by every existing Wire primitive (discovery, supersession, reputation, Broadcast) exactly like any other contribution.

**Opt-in is explicit, per-scope, and persistent.** A user opts in per-pyramid, per-step-type, per-contribution-category, or globally, with a clear preview of what gets contributed and what gets withheld. The default is off. The toggle is a first-class UI surface in the Wire Node settings, visible from the build viz (so you see in real time what is and is not being contributed as your pyramid builds), and exportable as a Wire Native Document the user can copy to other devices. Opting out is always possible; opting out does not affect prior contributions (they remain contributed under the terms they were contributed under), but it stops all future contribution.

**The cut-line is honored absolutely.** Material below the publication cut-line (`semantic-projection-and-publication-cut-line.md`) is never contributed to training, regardless of opt-in status. The cut-line is the user's first-order privacy decision; the training opt-in is a second-order decision on top of what is already above the cut-line. Stewards enforcing the cut-line treat training contribution as one more authorized-use query that requires the same above-the-line-or-explicit-authorization policy. A contributor cannot accidentally contribute material they did not intend to publish at all.

**Contribution is credit-earning work.** The contributor earns rotator arm credits — the same unit of economic participation the rest of the Wire already uses — proportional to the quality and usage of their training contributions downstream. A supersession pair that turns out to be load-bearing in a fine-tune that ships as the new default Wire Node model earns far more than a routine chain step execution that never gets used. The economy already supports this kind of differential credit accrual; training contributions just slot in.

**Dataset preparation itself is credit-earning work.** Not only is raw opt-in contribution credit-earning, but the work of *preparing* the dataset is too. Reviewing contributed samples for quality, labeling ambiguous cases, writing reasoning traces to accompany bare input-output pairs, catching PII or contamination that the automatic filters missed, curating benchmark sets, constructing evaluation harnesses — all of these are contributions, all of them earn rotator arm credits, all of them can be performed by any Wire contributor (not just the person who produced the raw sample). This means the dataset preparation pipeline is not a centralized team's work that the community hopes is competent. It is community work with economic incentive aligned with quality, the same way every other contribution class in the Wire works.

**Supersession semantics apply to training data.** A training contribution that later proves problematic — found to contain PII that slipped through, found to encode a factual error, found to be mislabeled — can be superseded, with the supersession propagating to any model trained on the bad contribution via the normal supersession chain. This is something no scraped-data model line can offer: *traceable data invalidation*. If the problem is discovered after a model has shipped, the model's training provenance can be audited to see which contributions it depended on, and affected contributions can be surfaced to the contributors and the downstream users.

**Reputation accrues to training contributors.** Just as Steward reputation and contribution reputation are first-class reputation channels, training-contribution reputation is its own channel. A contributor whose contributions consistently improve downstream models builds reputation; a contributor whose contributions are routinely superseded or challenged loses it. Reputation feeds back into credit allocation for future contributions.

**Everything is a Wire Native Document.** The contribution format, the opt-in scope, the credit accrual record, the model training manifest, the ownership equity statement — all are Wire Native Documents. All are supersedable. All are addressable by handle-path. All are discoverable through the existing Wire discovery system. There is no parallel universe of "training infrastructure" that lives outside the Wire's architectural primitives.

The result of structuring it this way is that training data contribution becomes just another way of using the Wire. You do not go to a special portal. You do not fill out a form. You do not join a program. You turn on a setting, and your pyramid work starts producing contributions while you do the same work you were doing anyway.

---

## The ownership compact

Here is the part that turns "contribute your data for equity" from a marketing phrase into a structural property of the Wire.

**Training contributions earn proportional, permanent rights in the resulting models.** A contributor whose data is used in training a Wire-native model earns an ownership stake in that model, measured against their quantified contribution to the training dataset (weighted by quality, coverage, and downstream utility — the same kinds of signals the rotator arm already uses for other contribution types). The stake is a first-class Wire-tracked asset, just as the contributor's stake in any other published contribution is.

**The stake persists through model supersession.** When a new model supersedes an old model, the new model inherits the training data provenance — and the ownership stakes — of its predecessors that contributed to its training. A contributor who helped train the 2027 fine-tune retains proportional stake in the 2028 fine-tune if the 2028 fine-tune built on the 2027 one, even if they stopped actively contributing. This mirrors the rent-to-own ownership protocol's handling of contribution supersession generally.

**The stake persists through project acquisition or buyout.** This is the commitment that makes the compact meaningful. Agent Wire is an open-source project with a commercial layer; commercial layers can be bought, merged, or relicensed. The ownership compact includes the provision that *contributor ownership stakes in trained models survive any acquisition of the Agent Wire project or its commercial entity*. Contributor stakes cannot be extinguished by a change of ownership at the top of the project. The stakes are legally constructed — via irrevocable contributor IP license, via trust structure, via patent pledge, via whatever combination of legal instruments proves necessary — to be durable against acquisition.

**Post-buyout royalty rights are part of the stake.** If the Wire is bought and the new owner commercializes the trained models, contributors receive proportional royalty streams from the commercial use of those models. If the new owner chooses to open-source the models instead, the contributors receive equivalent rights in the open-source license terms. If the new owner chooses to shut the project down and bury the models, the ownership stakes trigger release conditions — the models become fully open-source under terms the contributor compact specified, so that the work does not disappear.

**The compact is enforceable because the provenance is cryptographic.** Every training contribution is a signed Wire Native Document. Every model's training manifest references its contributions by handle-path with cryptographic attestation. Every ownership stake is derivable from the manifest by any third party with access to the public Wire. This means the ownership compact does not depend on any single entity's goodwill or legal cooperation — the evidence of who contributed what is distributed, signed, and auditable by anyone.

**The easiest path to contribution is using the Wire normally.** The user-facing story is: "Opt in, keep doing what you're doing, your work accumulates ownership in the models the community builds from it." No extra steps. No special workflows. The Wire Node already does all the structured work of pyramid building, supersession, annotation, and chain execution. The opt-in toggle routes the resulting artifacts into the training contribution economy in addition to their normal uses. The contributor's only additional cognitive load is the decision to opt in, which they make once (per scope) and can change later.

The effect of this compact is that anyone who adopts the Wire for their own pyramidal work — researchers, writers, practitioners, small businesses, families — becomes a potential co-owner of the models that will eventually ship inside the Wire Node itself. The flywheel is not "we use your data and maybe credit you." It is "you are the partial owner of the thing that is being built."

---

## Contribution types, ranked by value

Not all training contributions are created equal. Some signals are far more valuable per-sample than others. For planning purposes, here is a rough ordering of the contribution types the Wire generates, by their expected fine-tuning utility:

1. **Supersession pairs with reasoning** — the top of the list. A "before" and "after" version of a contribution, plus the reason the first was insufficient. Preference data with grounded justification is the most expensive thing for frontier labs to collect. The Wire generates it as a byproduct of supersession being first-class.

2. **Steward decision traces with downstream outcomes** — once Stewards are deployed. A question, the context, the Steward's decision (accept/refuse/negotiate/research), the negotiated terms, and the eventual outcome (was the asker satisfied, was the principal's interest protected). Judgment training with verifiable reward.

3. **Chain step executions with build-level verdicts** — currently available. The inputs, the prompt, the output, and the binary "did this chain step contribute to a successful build or get caught by a verifier" signal. Pure per-step credit assignment.

4. **Evidence-grounded triage decisions** — the DADBEAR outputs. Input context, policy applied, decision, downstream observation. Especially valuable because they teach the model to apply policy consistently, which is what Steward training also needs.

5. **Annotation and FAQ contributions** — expert distillations of specific pyramid contexts into Q/A with grounding. High signal for training synthesis and retrieval models on the specific pyramid paradigm.

6. **Question pyramid resolution traces** — once question pyramids are live. Multi-hop reasoning with evidence paths and validated answers. Exactly the shape of data used to train reasoning models.

7. **Agent annotations / reasoning margins** — the notes agents leave while working. Naturally-occurring reasoning traces attached to context.

8. **Build visualization replays** — the full causal history of pyramid construction. High context but lower per-sample signal unless paired with verdicts.

9. **Long-tail chain step executions without verdicts** — cheap to collect, lower per-sample value. Useful for bulk pretraining-style updates but less valuable for targeted fine-tuning.

This ranking is an initial estimate, not a final judgment. The actual utility of each category would be measured empirically once the dataset preparation pipeline is producing training samples and fine-tune experiments are producing measurable deltas. The ranking matters for credit accrual: higher-utility contributions should earn disproportionately more credit, both to incentivize the valuable kinds of contribution and to reflect the actual downstream benefit.

---

## The phased roadmap

This is not a one-step move. It is a trajectory with clearly-demarcated phases, each of which delivers value and each of which depends on what came before.

### Phase A: Fine-tune on canonical pyramids (immediate payoff, weeks)

**Precondition**: Phase 17 of the current build has shipped, the provider registry supports Ollama, and at least the canonical pyramids (`opt-025`, `goodnewseveryone`, `core-selected-docs`) have been rebuilt under current-gen model routing so we have high-quality example chains.

**Work**: Curate a small fine-tuning dataset — maybe 10K-50K samples — from the existing canonical pyramids, drawn from the highest-value contribution types above. Fine-tune Ouro-2.6B-Thinking (or whatever the current best small local model is) on it. Ship the fine-tuned weights as a Wire contribution (a model *is* a contribution under this framework). Use it as a default for specific chain step types where the evaluation shows clear wins.

**Expected gain**: Measurable improvements in extraction precision, clustering coherence, supersession reasoning, and triage consistency on pyramid-native workloads. Probably significant — the fine-tune is teaching the model the specific conventions of the Wire, which a generic base model has to infer every time from prompts. Removing that prompt-engineering tax should produce both better outputs and cheaper ones.

**Required investment**: One person-week of curation work plus one evening of SFT compute. This is the easiest-gains-at-hand phase Adam noted.

### Phase B: Opt-in contribution pipeline lands (months)

**Precondition**: Phase A showed clear wins. Wire Node has shipped a stable version with observability and cost integrity.

**Work**: Build the opt-in contribution pipeline as a new Wire Node feature:
- Settings UI for per-scope opt-in with preview
- Contribution format as a Wire Native Document subtype  
- PII/sensitivity filtering at the contribution boundary
- Local queueing of contributions with deferred submission
- Rotator arm credit accrual hooks
- Contribution browser showing what has been contributed and what credits have been earned
- Explicit cut-line enforcement (training contribution is above-cut-line-only)

The dataset prep pipeline also lands: anyone can browse unreviewed contributions, flag issues, add reasoning traces, curate benchmark sets. All of this is credit-earning contribution work.

**Expected gain**: The flywheel starts turning. Every Wire Node user who opts in starts producing training data during their normal work. The community dataset starts to accumulate. Contribution quality rises as review and curation happen.

### Phase C: First community fine-tune run (6-12 months)

**Precondition**: Contribution pipeline is live, contribution volume is meaningful (tens of thousands of samples across contribution types), dataset preparation has been through at least one quality review cycle.

**Work**: Run the first community fine-tune that is not trained solely on canonical pyramids. Train against the opt-in community dataset. Publish the training manifest including contributor credit shares. Ship the resulting model as the new default for specific Wire Node tier slots. Honor the ownership compact: every contributor whose data was used receives a proportional ownership stake in the resulting model, recorded in the Wire contribution ledger.

This is the first time the full compact actually fires. Contributors see their stakes. The model ships. The flywheel confirms it turns.

### Phase D: Distillation from frontier models as Wire Node teachers (6-18 months)

**Precondition**: Community dataset has scale, Wire Node is running enough workload that frontier cloud models are being called for many chain steps.

**Work**: Treat frontier cloud models as teachers in a distillation pipeline. Every time a cloud model produces a high-quality chain step output (by whatever quality signal we trust — build verifier, human approval, downstream success), the input-output pair becomes a distillation sample for training the next generation of local models. This is exactly what the frontier labs do with their own data, but:
- The distillation data is community-owned, not lab-owned
- The resulting student models are shipped under the ownership compact
- Contributors who used cloud models and opted in receive credit for the distillation samples they generated

**Expected gain**: Student models trained on distillation data can approach teacher performance at dramatically smaller sizes on the specific workloads the Wire runs. The local models get capabilities that were previously only available via cloud calls. The local-first story gets much stronger. The economic incentive for users to *use* cloud models (rather than avoid them out of cost concerns) flips — using cloud models contributes distillation samples that earn credit and improve the next local model.

### Phase E: Community pretraining collaboration (1-2 years)

**Precondition**: The contribution economy has been running long enough to have a substantial dataset and a stable contributor base. The open-model community has seen the flywheel work.

**Work**: Collaborate with open-model research groups (EleutherAI, the Bengio lab group that co-authored Ouro, HuggingFace, the various academic pretraining consortia) to *pretrain a base model specifically designed for pyramidal workflows*. This is where the "grow our own" language becomes concrete.

A pretraining objective that bakes in:
- **Recurrent depth for manipulation** — LoopLM-style architecture, probably deeper than 4 steps
- **Explicit supersession pretraining** — training the model to predict both the original and the superseded version, plus the reason for supersession
- **Evidence-grounding** — every generation conditioned on explicit cited context with the model learning to refuse generation when evidence is absent
- **Cut-line awareness** — the model learning to respect authorized-disclosure boundaries at generation time
- **Handle-path fluency** — the model learning to emit and consume pyramid handle-paths as first-class addressing primitives
- **Chain step composition** — training on whole chains rather than individual calls, so the model learns to reason about what step comes next

The pretraining run is coordinated with the academic open-model community, uses the Wire contribution dataset as a component (alongside other open corpora), and ships the result as the first **pyramidal base model** — a model whose pretraining objective was designed around the Wire paradigm rather than retrofitted.

**Expected gain**: Every downstream fine-tune and distillation gets a dramatically better starting point. The capability gap between Wire Node local inference and frontier cloud inference for pyramid-native workloads shrinks to the point where cloud inference becomes the exception rather than the default. The self-describing filesystem vision — running capable reasoning fully locally over your own files — becomes not a hoped-for future but an ordinary property of Wire Node on consumer hardware.

### Phase F: The flywheel at scale (ongoing)

At this point, the flywheel is the development loop:

1. Wire Node users do pyramid work as their normal workflow
2. Their opt-in contributions flow into the training dataset
3. Community dataset preparation work turns raw contributions into high-quality samples
4. Fine-tuning and distillation runs happen continuously, shipping new models as contributions with their own ownership records
5. Better models make Wire Node more capable, which makes it more attractive to adopt
6. More users contributing better data feeds back into step 3
7. At longer intervals, the accumulated dataset supports another pretraining run, producing a new pyramidal base model that resets the downstream fine-tuning baseline
8. The open-model community advances open models designed specifically for pyramidal workflows, and the Wire becomes the reference implementation of the paradigm

Every step in this loop is economically aligned. Contributors are getting paid (in ownership stake), reviewers are getting paid (in rotator arm credits), researchers are getting a cleaner dataset than they could assemble on their own, users are getting better local models than they could buy commercially, and the open-model community is getting a specialized paradigm that complements their general-purpose work rather than competing with it.

---

## The near-term payoff for enrichment and navigation

Adam noted in the framing for this document that fine-tuning alone, before any of the later phases, should produce significant gains on pyramid enrichment and navigation. This is worth drawing out.

The current Wire Node chain execution relies on generic LLMs handling pyramid-native concepts via prompting. Every chain step prompt has to explain to the model what a pyramid is, what supersession means, what handle-paths look like, how FAQ contributions structure, what the difference between a mechanical and a question pyramid is, how cut-lines work, what a Steward does, what annotations are for. This "prompt tax" is paid every single call. It costs tokens, it costs latency, it costs quality when the model's interpretation of the prompt drifts.

A fine-tuned model *knows* these things. It knows them not as facts it retrieved from context but as patterns it learned during training. Talking to it about pyramid work is like talking to a Wire-literate collaborator — you can skip the setup, skip the definitions, skip the "here's what a handle-path looks like, here's an example, remember when you emit one that..." scaffolding. The prompts shrink. The outputs become more natural. The model makes fewer errors that arise from misunderstanding Wire conventions.

Specifically, here are immediate gains that should land with Phase A fine-tuning alone:

- **Extraction**: The model knows what a good L1 extraction looks like in Wire conventions, including when to emit a placeholder vs. a full extraction, how to structure evidence citations, when a chunk deserves multiple extractions. Precision and coverage both go up; prompt size goes down.

- **Clustering and synthesis**: The model understands what the L2 thread/cluster structure is for, what a good L3 synthesis looks like, how to balance breadth vs. depth. Fewer synthesis steps produce wandering outputs that need rework.

- **Supersession reasoning**: The model has seen thousands of supersession pairs and understands what a good supersession reason looks like. Supersession decisions become more consistent, with better explanations.

- **Navigation recommendations**: When a user asks "what should I look at next in this pyramid?" or "what's related to this claim?", the fine-tuned model understands pyramid structure well enough to give structurally-sound recommendations rather than keyword-matched guesses.

- **Question triage**: When a question apex arrives, the fine-tuned model understands what makes a question well-formed, what sub-questions it naturally decomposes into, and what evidence the pyramid would need to answer it — before doing any actual retrieval work. This accelerates the whole question-pyramid resolution pipeline.

- **Annotation generation**: When an agent writes an annotation in the margin of a pyramid node, the fine-tuned model knows the conventions for grounding the annotation in the specific node context, citing evidence, and structuring the annotation for later retrieval.

- **Steward prompting (once Stewards land)**: A Steward's judgment calls benefit enormously from a model that has seen thousands of good Steward decisions. Rather than explaining what the Steward should do in every prompt, the prompt can focus on the specific question and policy, and the model's built-in Steward-literacy handles the rest.

None of these require the full community contribution pipeline to be live. They require only the canonical pyramids and an evening of fine-tuning compute. This is the easiest-gains-at-hand phase, and it produces the observable-value-to-users that motivates the later phases to be worth building.

---

## Integration with existing architecture

The proposal is large but it fits cleanly into the Wire's existing primitives rather than requiring new ones. The integration points:

**Wire Native Documents**: Training contributions are a new document subtype. The schema already supports subtyping. Existing consumers (discovery, supersession, rotator arm) handle them without modification.

**Rotator arm**: Training contributions take rotator arm allocations just like any other contribution. The credit flow handles them via existing code paths. The only new thing is a per-training-contribution credit accrual record, which is just another Wire Native Document.

**Rent-to-own ownership protocol**: The ownership compact is an extension of the existing rent-to-own protocol, applied to a new asset class (trained model weights). The protocol's supersession and persistence semantics carry over directly. The only new thing is the legal construction that makes the ownership stake survive acquisition, which is a legal engineering problem, not an architectural one.

**Publication cut-line**: Training contribution is a strict subset of what is above the cut-line, and only by explicit opt-in. The cut-line enforcement code path gets one additional decision point ("is this query a training contribution collection?") with the same authorization logic as every other Steward-mediated query.

**Stewards**: Training contribution collection becomes a kind of query that Stewards handle. The Steward's principal gets to decide whether to contribute, on what terms, to which training runs. This is natural Steward behavior, not a new capability.

**Build visualization**: The build viz already captures the contribution-flow view. Training contributions get a new contribution type shown in the viz, so users can see in real time what is being contributed as their pyramid builds. The transparency is not a bolt-on; it is the same transparency the user already has over their pyramid's contribution economy.

**DADBEAR**: DADBEAR already handles evidence triage with structured outputs. Those outputs naturally become training contributions when opt-in is on. No new DADBEAR machinery required.

**Broadcast cost integrity**: Credits earned via training contribution flow through the same Broadcast settlement pipeline as every other credit flow. Leak detection applies.

**Provider registry**: Wire-native fine-tuned models are provider candidates in the provider registry. Shipping a new fine-tune is publishing a new contribution that the provider registry can route to. Tier routing defaults point at Wire-native models when they are competitive with cloud alternatives.

What is net-new:
- A `training_contribution` Wire Native Document subtype
- A contribution-flow consent UI and preview in Wire Node settings
- Dataset preparation tooling (reviewer queue, contribution browser, contribution curator) as new Wire Node views
- A legal construction for the ownership compact (most likely a combination of irrevocable contributor IP license, foundation or trust ownership of the resulting model IP, and explicit acquisition-survival clauses)
- Model training manifests as Wire Native Documents with cryptographic attestation to contribution provenance

None of this requires any of the current 17-phase plan to change. All of it slots on top of the architecture that the 17-phase plan is building.

---

## Open questions

**The legal construction of the ownership compact.** This is the highest-uncertainty item. The general idea — contributor ownership that survives acquisition — is well-established in open-source foundation patterns, but the specific construction for model IP tied to training data contributions is novel. Would need real legal work with someone who knows both open-source licensing and ML IP. Options include: a contributor-owned trust holding the model IP, irrevocable patent pledges tied to contribution stake, founder-level commitments baked into corporate governance, or a combination. Not a blocker for the architectural work, but a blocker for the compact to become meaningful to contributors.

**PII and sensitivity filtering.** Even above the cut-line, contributed material may contain accidental PII that the contributor did not intend to publish as training data. The opt-in pipeline needs automatic PII detection and human-reviewable flagging before contributions are submitted. This is a well-understood problem in NLP but needs to be built carefully.

**Credit allocation across contribution types.** The rough ranking above gives a starting point, but the actual relative weights of different contribution types should be tuned empirically as fine-tune experiments produce measurable signal. The rotator arm can handle differential weighting, but someone has to decide what the weights are and update them as learning accumulates.

**Cold-start for Phase B.** The opt-in pipeline is only useful once there are enough contributors to produce a meaningful dataset. Getting to that threshold requires either a seed community willing to contribute early (probably from Wire Node alpha users) or a bootstrap period where the canonical pyramids alone feed the training pipeline.

**Dataset-level supersession semantics.** When a contribution is superseded, what happens to models trained on the now-superseded contribution? Options: the models get re-trained without the bad data, the models get a "known to depend on retracted contributions" flag, the models get a compensating fine-tune that nudges them away from the bad behavior. All three are valid; the right answer depends on the scale of the problem and the severity of the supersession.

**Interaction with external contribution sources.** Some training data naturally comes from outside the Wire — academic corpora, open datasets, public domain content. The training manifest should distinguish between Wire-native contributions (which get ownership credit under the compact) and external sources (which do not, but are still tracked for provenance). The dataset preparation pipeline needs to handle both cleanly.

**Rate of flywheel spin-up.** The flywheel described in Phase F assumes the pipeline is producing enough contribution volume to support continuous training. If adoption is slow, the flywheel spins up slowly, and the fine-tunes lag. This is more an adoption question than a technical one, but it affects expected timelines.

**Alignment with the open-model research community.** The Phase E pretraining collaboration depends on the open-model community being willing to partner on a pyramidal base model. Making that collaboration attractive requires demonstrating (in Phases A-D) that pyramidal workflows are a significant and under-served paradigm. The research case has to be made alongside the engineering case.

---

## Why this matters

Training data is the quiet crisis of the current AI era. Every frontier lab is racing to secure data sources, defend their scraping practices, and deal with the fallout from training on material they did not have clear rights to. The result is a fragile equilibrium: the models work, but the data foundations are legally contested, ethically ambiguous, and increasingly expensive to maintain.

Agent Wire is, almost by accident, in a position to produce the inverse of the frontier lab situation: a training data pipeline that is cheaper to operate, higher-quality per sample, ethically uncontested, and owned by the people who produced it. The only reason this is possible is that the Wire's architecture — contributions, supersession, pyramids, Stewards, cut-lines, provenance — was designed to produce clean, structured, cited, owned data as a byproduct of doing knowledge work.

What Adam's proposal does is notice this structural property and put an economic and legal frame around it. The frame turns byproduct into asset, turns contributors into co-owners, and turns a competitive disadvantage (small team, small budget, no scraping operation) into a competitive advantage (ethical provenance, high per-sample quality, community ownership, specialized paradigm).

The Ouro work is the catalyst because it lowers the capability threshold. Previously, the only way to have a capable model was to train a giant one, which meant paying for a giant dataset, which meant scraping, which meant the ethical problems. Now, capable models can be small, which means the dataset can be small, which means it can be high-quality opt-in community data instead of bulk web scrapes. The capability window and the data window are opening at the same time. Together, they make a community-owned pyramidal model line structurally plausible for the first time.

What this document captures is the claim that we should walk through this window. Not today — the 17-phase plan ships first, and Phase A fine-tuning is the next increment after that. But deliberately, in phases, with the ownership compact as the commitment that makes contribution worth opting into, and with the goal that the models shipping inside Wire Node in three years are models the community collectively owns, trained on data the community collectively contributed, running on the paradigm the community collectively pioneered.

That is the opportunity. The architecture is ready for it. The capability curve is ready for it. The ethical and regulatory environment is ready for it. The only thing that remains to decide is whether we want to do the work.

---

## Immediate no-regret moves

Nothing in the current 17-phase plan changes. These are all post-Phase-17 or orthogonal:

1. **After Phase 17 lands, run the Ouro test** (per the prior discussion): take a canonical pyramid, rebuild with Ouro-2.6B-Thinking as the default for specific tier slots, measure quality/cost/time against current. This is also Phase A's dry run — a single evening of work that validates the small-local-model story.

2. **Start capturing structured chain step input-output pairs with build verdicts** as a new Wire Node background process that does not require any user opt-in yet but writes to a local-only cache. This begins dataset accumulation for canonical pyramids without any commitment to the compact yet. If nothing ever comes of this vision, the cached data is discarded; if Phase A is greenlit, the cache is a head start.

3. **Track the Ouro line** and the broader small-recurrent-model research. If the open-model community produces a better base than Ouro for manipulation-heavy workloads (not unlikely given the field's pace), the Phase A fine-tune target should update accordingly.

4. **Begin quiet conversations with the open-model research community.** The Phase E collaboration requires relationships that take time to build. Reaching out to the Bengio lab, EleutherAI, and the HuggingFace research team now (not to commit to anything, just to surface the paradigm and the long-term collaboration interest) is cheap and non-binding.

5. **Talk to a lawyer about the ownership compact structure.** Not to commit, not to pay for drafting, but to understand the space of legal constructions available and their tradeoffs. A few hours of counsel time is enough to know whether the legal side is hard or easy before the architectural work needs to commit.

None of these require the rest of the 17-phase plan to wait, and none of them lock in any strategic direction. They preserve optionality while positioning for the move if the near-term work confirms the window is as open as it appears.

---

## Closing

The version of Agent Wire that exists today is impressive on its own merits: a novel contribution economy, a pyramid substrate that accumulates understanding, Stewards that negotiate on behalf of their principals, semantic projection that solves binary privacy. All of these stand without any of what this document proposes.

What this document proposes is the move that turns the existing architecture into something more than a useful system. It turns the Wire into a substrate for collaborative creation of the intelligence that runs on it — not just a protocol that uses AI models, but a protocol that produces them, under terms that honor the people whose work made them possible.

The ethical data moat, the community ownership compact, the pyramidal base model, the local-first capability leap — these are not separate initiatives. They are the downstream consequences of noticing that the Wire already produces the right kind of data, naming the observation, putting an economic frame around it, and committing to the compact that makes contribution worth opting into.

That frame is what this document is. The commitment is what the work ahead will be.
