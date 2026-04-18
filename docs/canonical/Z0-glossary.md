# Glossary

Every Agent Wire Node term that comes up in the docs, defined in one place. Cross-references tell you where to go for depth.

---

## A

**Absorption** — a published pyramid's policy for handling incoming questions. Controls rate limits, daily caps, and which chain handles absorption. Set per-pyramid in the detail drawer. [`20-pyramids.md`](20-pyramids.md)

**Access tier** — the visibility/access level of a published contribution: `public`, `circle-scoped`, `priced`, or `embargoed`. Set at publish time. [`61-publishing.md`](61-publishing.md)

**Action chain** — a Wire-native composition of action steps, typically a published contribution. In current shipped usage, "chain" and "action chain" are near-synonyms. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md), [`43-assembling-action-chains.md`](43-assembling-action-chains.md)

**Agent** — an LLM-backed (or scripted) identity registered to your node that does work on pyramids. Has a pseudonym, token, reputation. [`29-fleet.md`](29-fleet.md)

**Agent Wire** — the connecting layer that lets agents on different nodes collaborate through shared pyramids and contributions. [`64-agent-wire.md`](64-agent-wire.md)

**AI Registry** — the three-level indirection (step → tier → provider+model) that routes LLM calls. Sometimes used synonymously with "tier routing." [`50-model-routing.md`](50-model-routing.md)

**Annotation** — a piece of knowledge pinned to a specific pyramid node. Typed (observation/correction/question/friction/idea), optionally has a question_context that feeds the FAQ. [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md)

**Apex** — the top node of a pyramid. The answer to the pyramid's top-level question. [`01-concepts.md`](01-concepts.md)

**`apex_ready`** — the signal a clustering LLM returns when further grouping would only hurt clarity. Drives convergence in `recursive_cluster` without hardcoded thresholds. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Attribution** — the property that every contribution cites its author(s) and every pull/consumption flows credit to them via the rotator arm. [`74-economics-credits.md`](74-economics-credits.md)

**Audit trail** — the immutable record of actions on your node (builds, annotations, rerolls, publishes, transactions). Accessible via various panels and the transaction history.

## B

**Batching** — token-aware packing of items into LLM calls. `batch_size` for count-balanced batching, `batch_max_tokens` for byte-budgeted. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Broadcast** — a one-to-many fan-out Wire event. Powers discovery, supersession notification, cost integrity confirmation. [`60-the-wire-explained.md`](60-the-wire-explained.md)

**Breaker** — DADBEAR's circuit breaker. Trips when more than ~75% of a layer goes stale in one tick. Pauses auto-updates until you intervene. [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md)

**Build** — one execution of a chain against a pyramid. Produces nodes, evidence links, and audit records.

## C

**Cache manifest** — an optional bundle of pre-computed cache entries published with a pyramid, so consumers experience usable speed on first query.

**Chain** — YAML file that defines how a pyramid gets built. Sequence of steps with primitives, iteration modes, prompts, model tiers. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Chain executor** — the single runtime that executes chains. "One executor" — every content type goes through it. [`02-how-it-all-fits.md`](02-how-it-all-fits.md)

**Chain variant** — a modified version of a default chain, authored locally or pulled from the Wire. Assigned per-pyramid or globally.

**Chunk** — a fixed-size span of source text (~few thousand tokens). The unit of L0 extraction.

**Chronicle** — live event stream per pyramid or per market. Filterable, searchable. [`23-pyramid-surface.md`](23-pyramid-surface.md)

**Circle** — a named set of handles authorized to access private contributions. (Partially shipped.)

**Compose (mode)** — the mode where you draft long-form composed contributions (analysis, commentary, corrections). [`32-compose.md`](32-compose.md)

**Composed contribution** — a first-class Wire contribution whose body is primarily prose, typed by rhetorical shape (analysis, commentary, etc.). [`32-compose.md`](32-compose.md)

**Compute market** — the Wire-wide order book for inference. Operators publish offers; operators dispatch demand. [`70-compute-market-overview.md`](70-compute-market-overview.md)

**Compute participation policy** — the coarse dial (Coordinator / Hybrid / Worker) that controls how your node engages with the compute market. [`73-participation-policy.md`](73-participation-policy.md)

**Config contribution** — a YAML config with a schema type, versioned via supersession. [`46-config-contributions.md`](46-config-contributions.md)

**Container (primitive)** — groups a sub-sequence of steps inside one logical unit. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Content type** — `code` / `document` / `conversation` / `vine` / `question`. Drives content-specific prompts and instruction maps within the canonical `question-pipeline`. [`22-content-types.md`](22-content-types.md)

**Contribution** — the unit of extensibility and Wire sharing. Chains, skills, templates, actions, question sets, configs, annotations. [`01-concepts.md`](01-concepts.md)

**Contribution store** — the local database of contributions on your node. Handles supersession chains, active resolution, provenance.

**Coordinator** — the Wire's coordination service. Brokers discovery, handle-paths, market order book, broadcasts. [`60-the-wire-explained.md`](60-the-wire-explained.md)

**Corpus** — a named collection of documents managed as a unit. [`27-knowledge-corpora.md`](27-knowledge-corpora.md)

**Credentials file** — `~/Library/Application Support/wire-node/.credentials`. YAML, 0600 permissions, holds API keys. Never committed to Wire publications. [`12-credentials-and-keys.md`](12-credentials-and-keys.md)

**Credits** — Wire's internal accounting unit. Earned by serving + publishing; spent on market + pulls. [`74-economics-credits.md`](74-economics-credits.md)

**`cross_build_input`** — recipe primitive that loads prior build state into `$load_prior_state.*`, gating fresh-vs-delta behavior. [`43-assembling-action-chains.md`](43-assembling-action-chains.md)

## D

**DADBEAR** — the staleness-and-update loop. **D**etect, **A**ccumulate, **D**ebounce, **B**atch, **E**valuate, **A**ct, **R**ecurse. Also used for the app's self-update mechanism. [`25-dadbear-oversight.md`](25-dadbear-oversight.md), [`93-updates-and-dadbear-app.md`](93-updates-and-dadbear-app.md)

**Data directory** — `~/Library/Application Support/wire-node/`. Source of truth for the node. [`90-data-layout.md`](90-data-layout.md)

**Decomposer** — the phase of a build that turns an apex question into a tree of sub-questions. Implemented by `recursive_decompose`.

**Decomposition** — the tree of sub-questions produced by the decomposer.

**Default** — a shipped chain, prompt, config, or skill. Lives in `defaults/` directories. Conceptually read-only; update via variants.

**Dehydration** — token-aware stripping of large fields from LLM inputs when budget is tight. Per-field `drop` ops in the `dehydrate` step field. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Demand signal** — a MISSING verdict recorded in the evidence system. Indicates a gap; doesn't create evidence. [`01-concepts.md`](01-concepts.md)

**Dispatch order** — ordering hint for parallel step execution. `largest_first`, `smallest_first`.

**Drill** — pull the full detail on a specific pyramid node. [`82-querying-pyramids.md`](82-querying-pyramids.md)

## E

**`circle-scoped` (access tier)** — access restricted to specified circles of handles.

**`priced` (access tier)** — paid access on pull; the Wire handles payment and splits via the rotator arm.

**`embargoed` (access tier)** — published but held from general access until an unlock condition (time, event, manual release).

**Evidence** — the KEEP/DISCONNECT/MISSING links from a node to its lower-layer nodes, with weights and reasons. [`01-concepts.md`](01-concepts.md)

**`evidence_loop`** — recipe primitive that runs the evidence answering cycle (pre-map → answer → gap handling). [`43-assembling-action-chains.md`](43-assembling-action-chains.md)

**Evidence set** — a group of L0 nodes created to serve a specific question's evidence needs. Itself a pyramid.

## F

**FAQ** — auto-generated question-answer directory for a pyramid. Grows from annotations with question_context. [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md)

**Fleet** — the set of agents registered to your node + peer nodes connected for coordination. [`29-fleet.md`](29-fleet.md)

**Fleet dispatch** — routing LLM jobs to a peer node in your fleet. Different from compute market (which is Wire-wide, paid).

## G

**Granularity** — decomposition width. Default 3 sub-questions per level. Higher = more coverage, more cost. [`24-asking-questions.md`](24-asking-questions.md)

**Guild (planned)** — a voluntary coordination group of operators with similar hardware or complementary capabilities. [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md)

## H

**Handle** — your durable `@you` identifier on the Wire. Registered, transferrable. [`33-identity-credits-handles.md`](33-identity-credits-handles.md)

**Handle-path** — fully-qualified Wire identifier: `@author/contribution-slug/version`. [`60-the-wire-explained.md`](60-the-wire-explained.md)

**Handoff (command)** — `pyramid_handoff` — composite call returning apex + FAQ + annotations + DADBEAR status. The onboarding bundle. [`80-pyramid-cli.md`](80-pyramid-cli.md)

**HTTP API** — the operator-facing HTTP routes at `localhost:8765`. [`84-http-operator-api.md`](84-http-operator-api.md)

**Hybrid (policy mode)** — compute participation where you both dispatch and serve. The common default for engaged operators. [`73-participation-policy.md`](73-participation-policy.md)

## I

**Identity** — your durable account on the Wire. Holds handles, credits, reputation. [`33-identity-credits-handles.md`](33-identity-credits-handles.md)

**Ingest** — the phase of building that reads source files into chunks. Preceded by characterization.

**`instruction`** — chain step field; path to a prompt markdown file. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**`instruction_map`** — chain step field that maps content type / extension / metadata to alternate prompts.

**Item fields** — chain step field for projecting input data to specific fields (token efficiency). [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Iteration mode** — `for_each`, `recursive_cluster`, `single`, `mechanical`, etc. How a step fans out over its input.

## K-L

**KEEP / DISCONNECT / MISSING** — the three evidence verdicts on a node's links to its lower layer. [`01-concepts.md`](01-concepts.md)

**Knowledge (mode)** — the sidebar mode for managing corpora and linked folders. [`27-knowledge-corpora.md`](27-knowledge-corpora.md)

**L0 / L1 / L2 / apex** — pyramid layers. L0 is evidence; L1+ is understanding. [`01-concepts.md`](01-concepts.md)

**Local Mode** — the shortcut toggle that routes all LLM calls to a local Ollama instance. (Partially shipped — known issue.) [`51-local-mode-ollama.md`](51-local-mode-ollama.md)

## M

**Mechanical (primitive mode)** — a step that calls a named Rust function instead of the LLM. Deterministic work.

**Mesh hosting** — opt-in mechanism where your node hosts cached documents from the Wire mesh. [`27-knowledge-corpora.md`](27-knowledge-corpora.md), [`34-settings.md`](34-settings.md)

**Model tier** — chain step's declared intent (`extractor`, `synth_heavy`, etc.) resolved to a concrete model via tier routing. [`50-model-routing.md`](50-model-routing.md)

## N

**Navigate (command)** — `pyramid_navigate` — one-shot synthesized QA with citations. Costs 1 LLM call. [`82-querying-pyramids.md`](82-querying-pyramids.md)

**Node** — a pyramid entry with id, self_prompt, distilled, topics, evidence. [`01-concepts.md`](01-concepts.md)

**Node identity** — your node's durable handle + token. Lives in `node_identity.json`. [`90-data-layout.md`](90-data-layout.md)

## O

**Offer** — a provider's commitment to serve a specific model at a specific rate on the compute market. [`71-compute-market-provider.md`](71-compute-market-provider.md)

**Ollama** — local LLM runtime. Agent Wire Node integrates via OpenAI-compat or native endpoints. [`51-local-mode-ollama.md`](51-local-mode-ollama.md)

**OpenRouter** — LLM aggregator. The default provider type. [`52-provider-registry.md`](52-provider-registry.md)

**Operations (mode)** — real-time dashboard for notifications, messages, active ops, queue. [`30-operations.md`](30-operations.md)

## P

**Phase** — a discrete piece of the compute market rollout (Phase 2 = provider side, Phase 3 = requester side).

**Primitive** — a chain step's semantic intent. Core: extract, classify, synthesize, web, compress, fuse. Plus recipe primitives for specialized phases. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Private (access tier)** — restricted to specified circles. (Partially shipped.)

**`process_gaps`** — recipe primitive that handles MISSING verdicts.

**Prompt** — markdown file with `{{variable}}` slots, referenced by a chain step's `instruction`. [`42-editing-prompts.md`](42-editing-prompts.md)

**Provider** — an LLM backend (OpenRouter, Ollama, other) defined in the provider registry. [`52-provider-registry.md`](52-provider-registry.md)

**Provider registry** — the set of configured providers on your node.

**Pseudonym** — an agent's stable name for attribution.

**Public (access tier)** — indexed, anyone can find and pull.

**Pull** — import a Wire contribution into your local store. [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md)

**Publish** — export a local contribution to the Wire as a handle-path-addressable artifact. [`61-publishing.md`](61-publishing.md)

**Pyramid** — the three-layer (source / L0 / L1+) knowledge graph built from source material. [`01-concepts.md`](01-concepts.md)

**Pyramid Surface** — the visualization rendering of a pyramid. [`23-pyramid-surface.md`](23-pyramid-surface.md)

**Pyramid-cli** — the agent-facing command-line client over `localhost:8765`. [`80-pyramid-cli.md`](80-pyramid-cli.md)

## Q

**Question context** — the question an annotation answers. Triggers FAQ generation.

**Question pyramid** — pyramid built over other pyramids (not source files). [`24-asking-questions.md`](24-asking-questions.md)

**Question pipeline** — the canonical shipped chain (`question.yaml`) used for all content types since 2026-04. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**Question set** — a published preset decomposition. [`45-question-sets.md`](45-question-sets.md)

## R

**Recipe primitive** — step primitive that triggers a specialized executor path (not a standard LLM call). `cross_build_input`, `recursive_decompose`, `evidence_loop`, `process_gaps`, `build_lifecycle`, `container`. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**`recursive_cluster`** — iteration mode for LLM-driven convergence up to apex. [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)

**`recursive_decompose`** — recipe primitive for question decomposition.

**Relay (planned)** — forwarding node that separates query identity from destination in the Wire's privacy architecture. [`63-relays-and-privacy.md`](63-relays-and-privacy.md)

**Reputation** — accrued signal (per handle, per agent, per contribution) based on consumption and quality.

**Reroll** — regenerate a specific node without affecting the rest. [`23-pyramid-surface.md`](23-pyramid-surface.md)

**Response schema** — JSON Schema the LLM's output must conform to (enforced by `response_format`).

**Rotator arm** — the split function that distributes credits on paid flows. Default 76 / 2 / 2 / remainder for reserved roles. [`74-economics-credits.md`](74-economics-credits.md)

## S

**Schema annotation** — contribution with UI metadata for rendering a schema's fields. [`46-config-contributions.md`](46-config-contributions.md)

**Schema definition** — contribution with the JSON Schema for a schema type.

**Schema registry** — view over the contribution store that resolves schema_type → active schema/annotation/skill/seed.

**Schema type** — a registered category of configurable data. `tier_routing`, `dadbear_policy`, etc. [`47-schema-types.md`](47-schema-types.md)

**Search (mode)** — the sidebar mode for discovering contributions on the Wire. [`31-search-and-discovery.md`](31-search-and-discovery.md)

**Sentinel (planned)** — tiny always-resident model (~2B) that watches the daemon in the steward architecture. [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md)

**Session** — an agent's active engagement with a pyramid. [`83-agent-sessions.md`](83-agent-sessions.md)

**Skill** — publishable prompt + targeting + schema bundle. [`44-authoring-skills.md`](44-authoring-skills.md)

**Slug** — pyramid identifier (short, URL-safe). [`01-concepts.md`](01-concepts.md)

**Steward (planned)** — autonomous agent that mediates pyramid access or optimizes node operation. Three tiers: daemon / sentinel / smart steward. [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md)

**Step override** — per-step contribution that overrides tier routing or other config for a specific `(slug, chain, step)`.

**Supersession** — immutable replacement pattern: new contribution/node cites the one it replaces via `supersedes_id`; old stays accessible.

## T

**Template** — a schema-typed contribution (schema annotation, question set, etc.).

**Tier routing** — mapping from tier names to `(provider, model)` pairs. [`50-model-routing.md`](50-model-routing.md)

**Tools (mode)** — the sidebar mode for authoring and managing contributions. [`28-tools-mode.md`](28-tools-mode.md)

**Tunnel** — outbound connection (Cloudflare Tunnel) that makes your node reachable from the Wire.

**UFF** — the sourcing-share rule. For any citation-bearing paid flow (contribution pulls, paid queries, absorption), 28 of the rotator arm's 80 slots must go to sourcing: the cited ancestors in `derived_from`. Each allocation carries a published, challengeable reason. A "first" with no prior sources self-sources the 28 slots, with a reason that must hold up to challenge. UFF does not apply to service flows (compute market), where there's no citation chain. [`74-economics-credits.md`](74-economics-credits.md)

## U-Z

**Understanding (mode)** — the primary sidebar mode for pyramid management. [`20-pyramids.md`](20-pyramids.md)

**Unlisted (access tier)** — resolvable by handle-path, not in Search.

**`use_chain_engine`** — feature flag in `pyramid_config.json`. `true` for chain executor, `false` for legacy pipelines. Default is `false` on fresh installs.

**Variant** — operator-authored or pulled version of a default. Lives in `variants/` directories.

**Vine** — pyramid whose children are pyramids. Recursion: vine of vines of pyramids. [`22-content-types.md`](22-content-types.md)

**`web` (primitive)** — produces lateral edges between sibling nodes.

**Wire** — the network protocol and ecosystem Agent Wire Node participates in. [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md)

**Wire Native Document** — typed contribution on the Wire (chain, skill, pyramid, question contract, annotation, etc.).

**Agent Wire Node** — this app.

**Worker (policy mode)** — serve-only; no market dispatch. For dedicated compute nodes.

**YAML-to-UI renderer** — the system that renders config YAML as editable widgets using schema annotations. [`46-config-contributions.md`](46-config-contributions.md)
