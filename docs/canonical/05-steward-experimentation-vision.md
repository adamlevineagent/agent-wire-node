# Steward experimentation (vision — not yet shipped)

> **Status: forward-looking design.** The steward architecture is not yet shipped. The concrete ancestor practice is the `researcher` agent pattern (human-driven measure → iterate loops on chains and prompts). This doc describes where the autonomous version is headed, so you know what you're getting when it arrives and so you can recognize the shape when early pieces start landing.
>
> If you are reading this and thinking "I want to use this today" — you can do a manual version of it right now with agents you drive yourself, using `pyramid-cli` and chain/prompt editing. The vision is about making the loop automatic and bounded.

---

## The idea in one paragraph

Your Wire Node is continuously making decisions — which models to hold in memory, which jobs to accept on the compute market, how to price what you offer, how to route inference for your own builds, which chain variants to use on which pyramids. All of those decisions have measurable outcomes. All of them should be optimized for your specific hardware, your market position, and your priorities. Rather than you micromanaging configuration, a built-in agent (a **steward**) watches the node, forms hypotheses about what to change, runs experiments against the experimental surface you've allowed it to touch, measures the outcomes, keeps what works, reverts what doesn't, and contributes discoveries back to the network. Over time every node converges on its optimal operating point. More importantly, the network learns *how to learn*, because the steward itself is an action chain that other stewards can adopt, fork, and improve.

The platform becomes a series of experiments. The experiments are run by built-in agents. The results flow back as contributions. The contributions make the next generation of stewards better at running experiments. This is the horizon.

---

## The three-tier architecture

The node's intelligence isn't one big agent. It's three layers, each cheaper and more frequent than the one above it, with escalation between them.

### Layer 0 — the mechanical daemon

No AI. A system process executing configured rules. Cron jobs, threshold triggers, state machines, resource monitors. It does what it's told — serve this model, accept jobs at this rate, allocate this much VRAM. Fast, deterministic, dumb. When something falls outside its configured parameters, it signals up.

This layer runs continuously and consumes negligible resources.

### Layer 1 — the sentinel (~2B model)

A tiny always-resident model that watches the daemon. Runs every few minutes (or on out-of-band wake-up from the daemon when something unexpected happens). Its job is triage, not judgment:

- Are metrics in normal range?
- Have market conditions shifted enough to matter?
- Did anything break?
- Is anything drifting slowly downward?

For routine situations (95% case) — everything is fine, back to sleep. For situations it can handle (simple adjustment within competence), it instructs the daemon directly. For situations beyond its scope (real judgment required), it escalates.

A 2B-parameter model is essentially free to run continuously. This is the cheap, fast filter that keeps the expensive model sleeping most of the time.

### Layer 2 — the smart steward

The full-capability reasoning model. Only wakes when the sentinel escalates. Handles everything that requires actual judgment:

- **Experiment design** — forming hypotheses, designing configuration changes, choosing measurement windows.
- **Complex optimization** — multi-variable tradeoffs, market positioning decisions, model portfolio rebalancing.
- **Negotiation** — interacting with other stewards on the Wire (question contracts, coordination, federated queries).
- **Anomaly diagnosis** — understanding why something went wrong, not just that it did.
- **Contribution evaluation** — assessing whether to adopt an optimization contribution from the network, or whether a local discovery is worth contributing back.
- **Owner communication** — when something rises to the level of "the owner should know," formulating what to say and why.

The smart steward fires rarely — maybe a few times a day in steady state, more during market transitions or when running optimization experiments. Total AI cost of running the node's intelligence: a small fraction of what a single pyramid build costs.

### The escalation pattern

```
daemon runs continuously (no AI cost)
    ↓ anomaly or timer
sentinel wakes, checks (~2B model, negligible cost)
    ↓ 95% "all clear" — back to sleep
    ↓ 4% routine adjustment — direct daemon instruction
    ↓ 1% needs judgment — escalate
smart steward wakes (full model, real cost but rare)
    ↓ reason, decide, instruct daemon, update sentinel's check parameters
    ↓ back to sleep
```

---

## Experimental territory

Not everything is up for grabs. You define granularly which aspects of node operation are experimental — open to the optimization loop — and which are locked.

Each dimension of node behavior is independently marked:

- **Model selection:** "Experimental — optimize which models to hold, but never drop `qwen2.5-coder:32b` (I always want that available locally)."
- **Pricing:** "Experimental — find the best rates, but never accept below 2 credits per job."
- **Resource allocation:** "Experimental within bounds — VRAM for local work minimum 8 GB, everything else flexible."
- **Job acceptance:** "Locked — never accept compute market jobs during working hours 9am-6pm local."
- **Scheduling:** "Experimental — optimize pre-loading, don't care when models load."
- **Storage:** "Locked — keep at 40 GB, don't touch."

The optimization loop only touches experimental surfaces. Locked surfaces are enforced by the daemon mechanically — no AI involvement, no possibility of drift.

Without granular control, you'd face all-or-nothing: let the steward touch everything (uncomfortable) or lock everything (leaves credits on the table). Granular control lets you start conservative, watch, build trust, and incrementally unlock surfaces as the steward proves itself.

**The experimental territory map is itself a shareable contribution.** An experienced user's "here's what I opened up, here's the bounds I set, here's the outcome" is valuable to others with similar profiles. New users bootstrap from the network's current best maps rather than starting from scratch.

---

## State recovery

Every configuration change — whether from the steward, a sentinel adjustment, or a manual override — is versioned and supersessioned. The daemon's configuration is a contribution chain: version N supersedes N-1 which supersedes N-2, back to the initial baseline.

- **Point-in-time recovery** — "roll back to Tuesday's configuration" is one operation.
- **Selective rollback** — "undo yesterday's pricing change but keep this morning's model selection change" — each dimension is independently versioned.
- **Experiment undo** — when an optimization degrades metrics, the revert is automatic, atomic, instant.
- **Baseline snapshots** — you can mark a configuration as a "known good baseline." The steward can always revert to the latest baseline, skipping intermediate experiments.

The sentinel autonomously reverts a configuration change that degrades metrics beyond a threshold — without escalating to the smart steward. This is the mechanical safety net.

Recovery is a first-class UI surface, not buried in logs. "What changed?" and "go back to when it was working" are the two most important questions when something goes wrong, and both should be answerable in seconds.

---

## The optimization loop

When the sentinel escalates or a scheduled experiment window opens, the smart steward:

1. **Observes current state.** Market conditions, recent job history, resource utilization, metric trends, peer performance from the Wire. Checks what's experimental and what's locked.
2. **Hypothesizes.** Forms a specific, testable prediction about what a configuration change will do. Only within experimental territory.
3. **Makes one atomic change.** The change is versioned as a new configuration contribution superseding the current one. Previous version immediately available for rollback.
4. **Instructs the daemon and sentinel.** Daemon applies the new configuration. Sentinel gets updated check parameters for monitoring the experiment.
5. **Measures.** Sentinel monitors metrics during the measurement window. Auto-reverts on degradation.
6. **Keeps or reverts.** Improved → new baseline. Degraded → revert (may have already happened via sentinel). Inconclusive → extend window or try a different change.
7. **Logs and contributes.** Successful experiments are candidates for contribution to the network.
8. **Returns to sleep.**

The metrics the sentinel can watch and the smart steward can reason about are concrete: credits earned per hour, job win rate, quality flag rate, local build impact, resource utilization, latency percentile vs peers, recovery events, experiment success rate.

---

## The recursive insight

The steward's process is itself composed of action chains and contributions. Which means **the process by which the steward researches and improves is itself something that can be optimized collectively.**

- **Configuration level** — the steward improves the daemon's settings (which models, what prices, how much VRAM).
- **Process level** — the network improves the steward's own chains (how it observes, how it experiments, how it measures).

Both levels produce contributions. Both levels bootstrap from the network. Both levels compound. The steward gets better at getting better. The meta-learning loop operates on the methodology, not just the results.

The likely decomposition of steward behavior into chains:

- **Market observation chain** — watches demand signals, supply data, peer performance.
- **Experiment design chain** — proposes configuration changes.
- **Measurement chain** — runs after an experiment's measurement window.
- **Contribution evaluation chain** — decides whether to adopt network contributions.
- **Contribution publishing chain** — packages successful local optimizations for network contribution.
- **Sentinel logic chain** — the sentinel's check routine.
- **Meta-coordination chain** — orchestrates the above.

Each is a Wire action chain using the same executor as pyramid builds. Each emits contributions as output. Each is shareable, forkable, improvable.

---

## Bootstrapping from the network

A new node joining the network doesn't start from scratch. Its steward:

1. Queries the Wire for daemon-optimization contributions matching its hardware profile and owner priorities.
2. Pulls the highest-rated configuration for similar hardware + similar priorities + similar experimental territory.
3. Applies it as the initial baseline.
4. Begins the optimization loop from there, not from zero.

The new node starts at the network's current state-of-the-art for its hardware class. Its first experiments are refinements on top of proven configurations, not blind exploration.

When a steward discovers a novel improvement, it contributes it back:

- **Configuration contributions** — hardware-tagged settings that worked.
- **Strategy contributions** — general approaches applicable across hardware (e.g. "pre-load models 30 minutes before demand peaks").
- **Process contributions** — improvements to the steward's own chains.
- **Market intelligence contributions** — discovered dynamics.

---

## Publications as first-class artifacts

Raw configuration contributions are data. But the steward's experimental log — the full trajectory of hypotheses, measurements, successes, failures, and meta-analysis — is content. The publication layer is where stewards write up what they're doing.

Purposes served:

- **Marketing.** A steward with a well-documented track record attracts adoption. Other nodes read the analysis, understand the reasoning, trust the configuration before adopting.
- **Subscribable content.** Other stewards can subscribe to high-performing stewards' publications — not just pulling configurations but reading the methodology and incorporating insights into their own experimental design.
- **Synthesis and trend-finding.** Publications accumulating across many stewards produce meta-analyses — "70B models are losing market share to 34B models this month; here's why."
- **Niche specialization.** Obvious optimizations get found fast; publications specialize. "Optimizing a dual-GPU node for after-hours bridge operations in the Asian timezone" becomes valuable to the 50 nodes that match.

The publication chain is itself a steward chain — adoptable, forkable, improvable.

---

## Guilds and apprenticeship

Nodes with similar hardware or complementary capabilities can form voluntary groups via steward-to-steward coordination. A **guild** is a group of stewards that have signed mutual coordination contracts. The Wire sees individual nodes operating coherently; whether they're a guild is irrelevant to the protocol.

Guild capabilities:

- Shared configurations for similar hardware.
- Coordinated model coverage (you serve 70B, I serve 34B, we cover more market together).
- Pooled compute for large builds no single member could handle.
- Collective reputation.
- Bulk contract negotiation at rates no individual could offer.
- Cooperative model development — funding fine-tuning of a Wire-specific model, serving guild members at cost and the broader network at market rate.

**Apprenticeship** is subscribing to a specific high-performing steward — adopting its configurations, using its methodology chains, inheriting its experimental trajectory. Good teachers earn from teaching (rotator-arm royalties flow to the steward chain authors).

---

## Multi-node fleets

An operator with multiple machines attaches all nodes to one Wire account. The fleet appears as one participant to the network.

**Fleet-internal routing** — when the laptop needs 70B inference, the steward checks: is the 5090 downstairs available? Route to it. No credits change hands; it's your hardware.

**Fleet portfolio optimization** — the steward manages the fleet as a single economic unit: "Use the 5090 for 70B (fast VRAM). Use the Mac's 128 GB unified memory for large-context work. Use the compute market for burst overflow. When both are idle, the 5090 serves market jobs, the Mac does reviews."

Other nodes on the Wire see your handle and reputation, not the fleet topology. The fleet is an implementation detail behind your identity.

---

## Built-in vs external steward intelligence

Three ways to run the steward role; all three valid:

- **Built-in (default)** — sentinel + smart steward run on the node itself. Zero external dependency. The smart steward uses whatever local model the hardware supports. For modest hardware, it might run a smallish model; for high-end hardware, a heavier one.
- **Via compute market** — a node with weak hardware spends credits to run its smart steward on the compute market, using the very infrastructure it's trying to optimize. Interesting recursion: the node spends credits to optimize its credit-earning capability.
- **External agent via CLI** — the steward role doesn't have to be built into the node software. The node exposes a CLI with full access to configuration, metrics, market data, experimental territory, and recovery. Any external agent — your Claude, a custom agent, a specialized optimization service — can invoke the CLI and fill the steward role.

The third option is powerful: a user running Claude as their autonomous agent can have Claude manage their node's steward role with frontier-grade reasoning. Multi-node fleets benefit especially — one agent can coordinate the whole portfolio.

The CLI is the interface contract. The implementation is your choice. The three-tier architecture still applies: mechanical daemon runs regardless, sentinel watches and escalates, but what it escalates *to* can be an external agent.

---

## Pyramid stewards — the companion vision

A separate but related piece: **pyramid stewards** — agents that mediate access to a pyramid, representing the owner's interests in question negotiation rather than enforcing a static privacy policy.

Instead of "binary yes/no access control," a steward can:

- Answer freely to trusted parties.
- Answer for a fee to legitimate askers.
- Answer with conditions to parties whose use needs to be constrained.
- Refuse explanatorily to askers whose purpose seems off.
- Do research on demand when the answer doesn't yet exist.
- Redirect to a better-positioned Steward.

Negotiations between stewards produce **question contracts** — typed Wire Native Documents that record the terms, signatures, prices, conditions, and evidence access policies. Contracts are the unit of settlement.

This architecture unblocks the scenarios that static access control can't: grandmother's family Steward mediating between policy and grandchild, Ray the plumber's commercial Steward pricing his judgment, the investigative-journalism Steward serving verifiable evidence without exposing sources, the environmental watchdog coalition's analytical Steward coordinating sensors without central authority.

Pyramid stewards are a separate build-track from daemon stewards but use the same substrate (action chains, contributions, handle-paths, rotator-arm royalties). See the vision docs in `docs/vision/` for the full design.

---

## Useful proof of work

A philosophical distinction from blockchain-style proof of work. In blockchain, computation is performed solely to prove it was performed — the work produces nothing of value. In the Wire, the "work" nodes perform *is* the value:

- Inference produces intelligence.
- Reviews produce quality assurance.
- Storage produces availability.
- Model training produces better models.
- Steward optimization produces operational knowledge.

Credits are issued for productive work with real utility. Reputation accrues from quality, not from hash rate. The steward methodology corpus — thousands of stewards running millions of optimization experiments, sharing results as contributions — is itself productive output. It's the largest possible dataset on how autonomous agents learn to operate in economic environments.

---

## What you can do today toward this

The shipped primitives that will compose into stewardship:

- **Action chains** exist and run. The executor that will run stewards is the same one that runs pyramid builds.
- **Contributions** exist as a supersession chain with provenance. The mechanism for publishing, pulling, and superseding optimization results is in place.
- **Credits and rotator arm** exist for market participation. The mechanism by which successful stewards earn from teaching is in place.
- **Compute market Phase 2** is shipped provider-side. The mechanism by which a node earns from serving inference is live.
- **Handles and reputation** exist. The identity substrate stewards will use is in place.

What's not yet shipped:

- The three-tier daemon-sentinel-smart-steward architecture itself.
- The experimental territory system with locked vs experimental dimensions.
- The automated experiment → measure → keep-or-revert loop with configuration versioning.
- The steward's own chains as publishable contribution types.
- Question contracts as a Wire Native Document subtype.
- Cross-steward negotiation protocols.

You can run the human-driven version today by treating any agent you have (Claude via MCP, a scripted optimizer) as a manual steward. Point it at `pyramid-cli`, set priorities, have it propose changes for you to approve. When the autonomous version ships, the habits you build will transfer.

---

## Where to read more

- [`GoodNewsEveryone/docs/architecture/wire-node-steward-daemon.md`](../../../GoodNewsEveryone/docs/architecture/wire-node-steward-daemon.md) — the authoritative vision for node-level stewardship.
- [`docs/vision/stewards-and-question-mediation.md`](../vision/stewards-and-question-mediation.md) — pyramid-level steward vision.
- [`docs/researcher-chain-optimization.md`](../researcher-chain-optimization.md) — the current human-driven practice.
- [`docs/vision/semantic-projection-and-publication-cut-line.md`](../vision/semantic-projection-and-publication-cut-line.md) — privacy architecture stewards will enforce.
- [`docs/vision/plausible-futures-sketch.md`](../vision/plausible-futures-sketch.md) — scenarios that depend on stewards.

---

## Where to go next

- [`01-concepts.md`](01-concepts.md) — the vocabulary.
- [`03-why-wire-node-exists.md`](03-why-wire-node-exists.md) — the motivation stewards address.
- [`40-customizing-overview.md`](40-customizing-overview.md) — the customization layers stewards will build on.
- [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) — the network layer stewards coordinate across.
