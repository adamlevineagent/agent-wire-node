# Why Wire Node exists

Before the how, the why. Wire Node was not built because someone wanted another knowledge tool. It was built because the tools that existed for working with large bodies of knowledge all fail in the same handful of ways, and those failures stop compounding work from happening. This doc walks through the problems and what Wire Node does about them.

If you just want to use the app, skip to [`10-install.md`](10-install.md). If you want to understand what the app is actually trying to do so you can reason about it when you run into edge cases, read on.

---

## Problem 1: Asking the same question twice costs twice as much

Every time you ask Claude, ChatGPT, or any other LLM a question about a body of material, the model reads the material from scratch. Nothing accumulates. If ten people ask the same question about the same codebase, the extraction work is done ten times. If you ask a follow-up, the work overlaps with the first answer but nothing in the system knows that.

This is fine for one-off questions about small material. It falls apart the moment:

- the material is big (a whole codebase, a year of transcripts, a hundred-page spec),
- the question is nuanced (needs evidence from multiple places),
- you will ask more than one question,
- multiple people will ask related questions,
- the material changes over time and the answer needs to stay current.

Wire Node treats knowledge as an asset that accumulates. The first question on a corpus is expensive. The tenth is nearly free, because the L0 evidence base is dense, and the decomposer cross-links sub-questions to existing answers before doing any new work. Questions compound instead of repeating.

## Problem 2: LLM answers have no provenance you can check

When an LLM gives you a paragraph about your codebase, you cannot audit it. If the answer is wrong, you can't trace which file misled the model or whether the model hallucinated. You have one chance to trust the paragraph, and if you don't, you're back to reading the source yourself.

Wire Node's pyramid is evidence-backed end to end. Every answer above L0 has explicit links to evidence nodes, each with a weight and a reason. Every L0 node points back at a specific chunk of a specific file. You can always drill. You can always check.

This matters even more once an LLM is using the pyramid. An agent answering a question through Wire Node cites the apex, the relevant sub-answers, and the underlying L0 evidence. You can follow any claim back to the text it came from.

## Problem 3: There is no good way for agents to accumulate knowledge about your stuff

If Claude is helping you work on a codebase, every session starts cold. Claude has no memory of what it learned last week. It re-reads the same files, re-arrives at the same insights, and re-asks you the same questions. The agent is doing work that should have accumulated into a shared artifact — but there is no artifact.

Wire Node is that artifact. An agent's sessions read from and write to the pyramid. An observation becomes an annotation. A question becomes an FAQ entry. A correction becomes a supersession. The next session — yours, theirs, someone else's — starts hot. The pyramid is the shared memory.

## Problem 4: Keeping answers current is manual and brittle

If you build a summary of a codebase and the codebase changes, the summary is silently wrong. Nothing in the summary tells you which pieces are stale. You re-run the whole summarization and throw away all the work that's still valid.

DADBEAR is Wire Node's answer. When a source file changes, only the nodes that actually depended on the changed content get re-evaluated. Unchanged regions of the pyramid stay put. Stale regions get flagged, re-answered, and propagated upward — with explicit supersession pointers so you can see what changed and why. A pyramid can track a living codebase indefinitely without ever being fully rebuilt.

## Problem 5: Customizing an LLM pipeline means writing code

Most LLM tooling exposes "the prompt" and maybe "the model," but everything structural — how chunks get formed, how questions get decomposed, what primitives run in what order — is locked inside the tool. If you want a pipeline that does things differently, you build it from scratch.

In Wire Node, the pipeline itself is data. Chains are YAML. Prompts are markdown. Policies are configs. You change how pyramids are built by editing files, not by patching source. You share your variant by publishing it as a contribution. Someone else uses your variant by pulling it.

This pushes customization down to the user. The app is a runtime for contributions; the contributions are where the work lives. See [`40-customizing-overview.md`](40-customizing-overview.md).

## Problem 6: Knowledge work is siloed, even when it shouldn't be

Two operators working on related things end up duplicating extraction, duplicating summaries, duplicating the crucial decompositions. There is no mechanism to share the *structure* of their work — only the finished artifact, which is usually a document that someone else has to re-extract from.

The Wire is the shared layer. A chain you authored is something I can pull and run. A skill you wrote is something my pyramid can use. A published pyramid is something I can query, annotate, and build a question pyramid on top of. The sharing unit is not "the finished answer" — it is the machinery that produces answers.

## Problem 7: Paying for inference is a weird middleman situation

If you want an LLM to do work for you, you pay OpenAI or Anthropic or OpenRouter. Meanwhile there are tens of thousands of GPUs on people's desks, sitting idle most of the time. There is no clean way for someone with a GPU to sell inference to someone who needs it, and no clean way for someone paying for inference to just go somewhere cheaper without rewriting their pipeline.

The compute market in Wire Node is that clean way. Operators publish offers for specific models and specific prices. Requesters dispatch inference to whichever offer wins the auction. Your pipeline doesn't change. The market is the indirection. See [`70-compute-market-overview.md`](70-compute-market-overview.md).

## Problem 8: Decentralized networks are either public or private, not both

Most peer-to-peer systems require you to pick: either your node is publicly reachable (and tied to your identity, and anyone can see what you're doing), or it's private (and cannot serve anyone else). There is no good middle ground.

Wire Node's relay architecture is the middle ground. A relay forwards traffic with enough privacy separation that the relay never sees the payload and the destination never sees who the originator was. You can host a pyramid other people query without those queries being linked back to them. You can query someone else's pyramid without revealing that it was you. See [`63-relays-and-privacy.md`](63-relays-and-privacy.md).

---

## The underlying bet

The bet Wire Node is making is:

1. Structured understanding compounds if the system is designed to let it.
2. Agents are the primary consumer of structured understanding, and they are going to keep getting more capable.
3. The right shape of AI infrastructure is local first, with a sharing layer on top — not a cloud with a client.
4. Everything extensible should be content, not code. The binary is a runtime.

If those bets are right, then over time your pyramids accumulate more useful knowledge than any single-shot LLM could produce, your agent partners get progressively better at working with your material, your costs fall as the evidence base gets reusable, and your contributions become a traceable part of a shared network — without requiring you to give up control of your data to do any of it.

## What Wire Node is not betting on

- **Not betting on bigger models solving everything.** Pyramids work with small models too; the architecture benefits most from routing different tiers of model to different kinds of step. Mercury 2 for extraction, a heavier synthesis model for the final apex, Ollama for staleness checks. Model diversity is the point.
- **Not betting on replacing human judgment.** Annotations, corrections, and FAQ entries are how human judgment enters the pyramid and persists. The pyramid is a tool for thinking with, not a substitute for thinking.
- **Not betting on centralized distribution.** The Wire is a protocol, not a company. Nodes can talk to each other without a central coordinator; when a coordinator is used, it's a convenience.

---

## How this shapes the app

Every decision in Wire Node traces back to one of the above. If a feature seems surprising, check whether it's an answer to one of these problems. A few common ones:

- *"Why can't I just get a finished summary without all the evidence?"* — Because the evidence is the product. A summary without provenance is the problem we're solving.
- *"Why is everything immutable?"* — Because anything you can't supersede you can't correct without destroying prior work. Immutability plus supersession is how corrections accumulate.
- *"Why do I have to care about chains and prompts?"* — You don't. You can use defaults forever. They are there so when defaults don't work for you, you can change them without forking the app.
- *"Why is there a marketplace?"* — Because the sharing layer is load-bearing. The app would work standalone; it just wouldn't compound the way it does when people share.

---

## Where to go next

- [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) — the network, and why decentralization and privacy both matter.
- [`00-what-is-wire-node.md`](00-what-is-wire-node.md) — the short elevator pitch if you want to recentre.
- [`10-install.md`](10-install.md) — start using it.
