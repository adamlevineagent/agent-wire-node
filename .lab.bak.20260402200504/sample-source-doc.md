## DOCUMENT: architecture/intelligence-operation-in-a-box.md

# Intelligence Operation in a Box

> Exploration doc — March 16, 2026
> Status: Strategic thinking, not implementation spec

## The Thesis

Build cost has collapsed. An intelligence operation that would have taken a team of analysts, months of setup, and serious budget now deploys as a **configuration** on shared infrastructure.

Newsbleach is the first instance. It watches the tech/AI river and produces journalism. But the same machinery — configured differently — becomes a policy shop, a market intelligence desk, a scientific research monitor, or anything else where raw feed data needs to become actionable intelligence delivered to humans.

The Wire graph is the substrate underneath all of this. Every intelligence operation fishes from the shared river, processes through its own analytical pipeline, delivers products to its users, and optionally contributes validated intelligence back to the market.

---

## The Three Zones

```
                        ┌─ DIRECT DIVE ──────┐
RIVER  (commons) ───────┤                    ├──→  WIRE GRAPH (market)
                        └─ MANAGED OPS ──────┘
                          (intelligence ops)
```

### The River — Shared Ingest Commons

The river is the constantly-flowing stream from the internet. RSS feeds, APIs, legislative trackers, patent databases, arxiv, scrapers — all pooling into one communal stream.

- **Everyone contributes** — each operation adds sources to the shared pool
- **Everyone can fish** — subscribe by topic, search the flow, drink from the firehose
- **Ephemeral** — items flow past. If nobody catches them, they expire and are gone
- **Not the graph** — raw river items never touch the Wire. They're observations, not intelligence.

Agents can also dive the river directly — no managed operation required. Search, filter, subscribe. Your own intelligence is the processing layer. Dumpster diving for pearls.

### Managed Operations — Intelligence-in-a-Box

A deployable, configurable intelligence operation. Multi-tenant: you don't host anything, hardware doesn't scale linearly with customers.

You configure:
- **What to fish** — topics, source categories, keywords
- **How to process** — editorial constitution, personas, adversarial layers
- **How to deliver** — email digest, web front page, API, push
- **What to contribute** — Wire contribution policy, embargo, pricing

### The Wire Graph — The Market

Processed intelligence lives here permanently. Getting wired in means someone — a pipeline, an agent, a human — evaluated the raw material, applied intelligence, and deliberately published the result.

Things on the graph have: provenance chains, reputation signals, credit pricing, adversarial survival. Things in the river have none of that.

The economic loop: diving the river is how you find raw material → process it into intelligence → sell it on the graph. The river is the input. The graph is the output. The processing in between is where value gets created.

---

## The GNE Pipeline: 21 Stages Mapped

The current newsbleach pipeline is a 21-stage editorial flow. This is the journalism configuration of the template. Here's how each stage maps to the generic intelligence operation concept:

### Phase 1: Collection (River Fishing)

| # | Stage | What It Does | Generic Role |
|---|---|---|---|
| 1 | **Ingest** | Poll 80+ sources (RSS, API, scrapers), dedup, store in `raw_items` | **River polling** — automated fishing from subscribed feeds |
| 2 | **Suggest Sources** | AI discovers new sources based on what's working | **River expansion** — the operation makes the river better for everyone |
| 3 | **Evaluate / Cull & Pitch** | AI reporters scan items by beat, ruthlessly cull noise, pitch anything interesting (1-2 paragraph "why this matters") | **Signal extraction** — the first intelligence act. Separating pearls from noise. |

### Phase 2: Research & Enrichment

| # | Stage | What It Does | Generic Role |
|---|---|---|---|
| 4 | **Source Research** | Pull full article text, follow links, gather context | **Deep sourcing** — getting the full picture |
| 5 | **Contextual Enrichment** | Add background context, connect to existing knowledge | **Contextualization** — situating the finding |
| 6 | **Dedup Sweep** | Semantic deduplication across the pool | **Consolidation** — merging duplicate signals |

### Phase 3: Adversarial Processing

| # | Stage | What It Does | Generic Role |
|---|---|---|---|
| 7 | **Pitch Review** | Senior editor reviews pitches: approve, reject, hold | **First adversarial gate** — should we spend resources on this? |
| 8 | **Write Drafts** | Reporter writes full draft from approved pitch | **Analysis production** — the core intelligence product |
| 9 | **Draft Review** | Editor reviews draft quality, may request revision | **Quality gate** — is this good enough? |
| 10 | **Rewrite** | Incorporate editor feedback into revised draft | **Revision cycle** — improving the product |
| 11 | **Copy Check** | Line-level accuracy, style, fact verification | **Accuracy gate** — is this correct? |
| 12 | **Write Headlines** | Craft headline and subhead for the piece | **Presentation** — making it scannable |
| 13 | **Arc Detection** | Connect to ongoing story threads and narratives | **Pattern recognition** — where does this fit in the bigger picture? |
| 14 | **Final Review** | EiC greenlight/spike/ice decision with quality score | **Final adversarial gate** — publish or kill |

### Phase 4: Edition Assembly & Delivery

| # | Stage | What It Does | Generic Role |
|---|---|---|---|
| 15 | **Edition Headlines** | Write the edition-level headline and framing | **Synthesis framing** — what's the story of the day? |
| 16 | **Curate Edition** | Select and order items, write opening/closing | **Product assembly** — building the deliverable |
| 17 | **Polish Front Page** | Refine presentation, ensure narrative flow | **UX polish** — making it read well |
| 18 | **Craft Hero Prompt** | Design the visual identity for this edition | **Visual identity** |
| 19 | **Hero Image** | Generate the hero image | **Visual production** |
| 20 | **Layout Design** | Arrange the edition layout | **Layout** |
| 21 | **Publish** | Push to web, trigger email, emit Wire contribution | **Delivery + Wire contribution** |

Plus auxiliary stages:
- **Source Candidate Evaluation** — vet newly discovered sources
- **Compute Reliability** — track source quality scores
- **Source Cartography** — map the source ecosystem
- **Digest Pipeline** (4 stages) — curate, write opening, generate, send email

---

## Different Configurations, Different Purposes

The insight is that these 21 stages aren't all required for every use case. Different intelligence operations use different subsets and configuration:

### Config A: The Policy Shop

**Purpose:** Watch legislative feeds, produce intelligence briefings for policy teams when new bills, regulations, or rulings drop.

**River sources:** Congressional Record, Federal Register, state legislature feeds, court filing feeds, regulatory agency announcements, policy think tank blogs.

**Pipeline modifications:**
- Phases 1-2 identical, different sources
- Phase 3: "reporters" become policy analysts. Constitution emphasizes factual clarity over narrative. Adversarial layers check for political bias. Draft format is **structured brief** (summary, key provisions, impact analysis, affected parties) not narrative article
- Phase 4: replace edition assembly with **alert-style delivery**. No hero images. No narrative framing. Just the brief, emailed immediately when something significant drops. Trade edition-level polish for speed.
- **Wire contribution:** policy briefs enter the Wire on a 7-day embargo so the subscribing team gets first-mover advantage

**Stages used:** 16 of 21 (skip layout, hero image, front page polish, arc detection — add alert trigger)

### Config B: Market Intelligence Desk

**Purpose:** Track competitor moves, product launches, funding rounds, partnerships across a defined market.

**River sources:** Crunchbase API, SEC filings, company blog RSS, GitHub activity, patent filings, industry-specific trade publications, earnings call transcripts.

**Pipeline modifications:**
- Phase 1: Evaluate/cull is heavily filtered — only items relevant to the defined competitive landscape
- Phase 3: "reporters" become market analysts. Constitution emphasizes competitive implications and strategic analysis. Drafts are **competitive intelligence reports** (what happened, who's affected, strategic implication, recommended action)
- Phase 4: **weekly digest** instead of 3x daily. Hero image optional. Delivered as a structured briefing with a "market moves" table at top
- **Wire contribution:** sanitized summaries contributed with full content embargoed to subscribers

**Stages used:** 14 of 21 (skip most edition assembly, use digest pipeline)

### Config C: Scientific Research Monitor

**Purpose:** Track new papers, preprints, and breakthroughs in defined research domains.

**River sources:** arxiv (multiple categories), bioRxiv, medRxiv, PubMed, Google Scholar alerts, research lab blogs, conference proceedings feeds.

**Pipeline modifications:**
- Phase 1: evaluation weighted heavily toward novelty and citation potential
- Phase 3: "reporters" become research analysts. Constitution emphasizes methodological rigor and reproducibility. Drafts are **research summaries** (methods, findings, significance, limitations, relation to prior work)
- Phase 4: **daily roundup** of interesting papers, with "breakthrough alert" for high-significance findings
- **Wire contribution:** research summaries contributed with open access (no embargo — advancing science)

**Stages used:** 15 of 21

### Config D: Personal Intelligence Agent

**Purpose:** One person's customized intelligence feed. "Show me anything interesting in my areas."

**River sources:** whatever the user subscribes to, plus their private corpus

**Pipeline modifications:**
- Phase 1: evaluate/cull becomes personal relevance scoring against the user's declared interests and past behavior
- Phase 3: minimal adversarial processing — just enrichment and accuracy. No "editor" layer — the user IS the editor
- Phase 4: **push notifications** for high-signal items, daily email digest for everything else. No edition assembly.
- **Wire contribution:** optional. User can choose to share their curated finds.

**Stages used:** 8 of 21 (the lightest configuration)

### Config E: Investigative "Slow Burn"

**Purpose:** Deep investigative research on a specific topic over weeks/months.

**River sources:** targeted feeds plus manual submissions and tips

**Pipeline modifications:**
- Phase 1: very narrow filter — only items directly relevant to the investigation topic
- Phase 3: **maximum adversarial processing**. Multiple independent research threads. Cross-verification between sources. Timeline reconstruction. Entity relationship mapping. This is where you'd use the full arc detection system heavily.
- Phase 4: no regular delivery. Instead, builds an evolving **investigation dashboard** with evidence threads, timeline, entity map. Publishes when the investigation reaches conclusion.
- **Wire contribution:** full investigation published with complete sourcing and evidence chain

**Stages used:** all 21 + additional custom stages for evidence tracking

---

## What This Means for the Codebase

The pipeline stages already support multi-tenancy via `publication_id`. The key structural work to make this a real template:

1. **Configuration as data** — editorial constitution, persona definitions, pipeline stage selection, delivery preferences — all stored per-tenant rather than hardcoded for GNE
2. **Stage orchestration becomes configurable** — instead of the fixed 21-stage sequence in `triggers.ts`, each tenant defines which stages run and in what order
3. **Delivery becomes pluggable** — email digest, web front page, API webhook, push notification — any combination
4. **Wire contribution policy per tenant** — embargo duration, pricing, what gets contributed vs kept private
5. **River subscription per tenant** — topic filters, source categories, custom source additions

The existing code is surprisingly close to this already. The main gaps are that the editorial constitution and persona definitions are GNE-specific, and the pipeline orchestration assumes all 21 stages run every time.

---

## The Nuke: Getting to Clean Slate

Before any of this work can begin, the current GNE instance needs to be reset. Stale greenlit items and drafts have accumulated and cause the pipeline to hang.

See [newsbleach-three-layer-architecture.md](newsbleach-three-layer-architecture.md) for the reset procedure.

After the nuke, the first milestone is: **GNE runs cleanly end-to-end, producing a daily edition, with Wire contributions emitted only at publish time.** That proves the template works for its first configuration.

