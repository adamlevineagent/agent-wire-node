# Tester Onboarding — Full Design Plan

**Status:** Draft, rev 0.4. Plan-stage audit complete. Ready for Phase 0 pre-flight + build.
**Canon anchors:** `GoodNewsEveryone/docs/inviolables.md`, `GoodNewsEveryone/docs/wire-pillars.md`, wire-node-rules skill (5 Laws + module inventory).
**Purpose:** first-run experience for testers that explains Wire Node in Adam's voice, removes blocking gates, sets up any combination of compute paths, and pins a living pyramid explaining the app that grows richer as every tester uses it.

**Litmus:** tester installs app → is shown who they are + what this is + how to compute + three things they can do first → clicks "Ask the app anything" → asks a question → gets an answer that either was cached locally or flowed from the source pyramid and accreted for everyone's future benefit. They never see a market word. They never get blocked.

---

## 1. Architectural frame

### 1.1 Pin as living subscription

Pinning a pyramid is a subscription to a living artifact. The source pyramid lives on its owner's node (Adam's, for the Wire Node explainer). A pinned copy is a local cache plus a bidirectional sync relationship — queries against it flow to source on miss, answers accrete back, contributions broadcast to every pinned copy on the network. Local is fast; network is source of truth; the pyramid gets smarter with collective use.

### 1.2 Respecting the pillars

Every mechanism in this design compiles down to existing Wire primitives:

- **Everything is a contribution** (Pillar 1, Inviolable 1) — `onboarding_pin_set`, `tester_feedback`, pyramid nodes, FAQ annotations, accuracy flags, challenges all flow through the contribution store.
- **Chains invoke chains** (Pillar 17) — remote query is a published chain, not a new handler. Compiles to IR, runs on `chain_executor::execute_chain_from`. Law 1 honored.
- **DADBEAR recursive** (Law 2) — incoming FAQ contributions on pinned copies enter the DADBEAR mutation queue exactly like any other staleness signal. No new propagation logic.
- **StepContext everywhere** (Law 4) — every LLM call in the query path constructs StepContext via `make_step_ctx_from_llm_config`. No "just this one call" exceptions.
- **Unified Flow Formula** (Pillar 7) — pyramid queries are revenue events. Credits flow per UFF (see §1.3).
- **Audience framing** (Pillar 41, Inviolable 8) — remote query requests thread `audience` as a first-class parameter. Every downstream LLM call sees it. Existing `manifest.rs` cognition steering applies the audience lens.
- **Subtractive work is the thermostat** (Pillar 10) — every answer surface has a flag-as-wrong path that creates an `accuracy_flag` contribution, resolvable through the existing challenge-panel infrastructure (Pillar 24).
- **Handle-paths are the human interface** (Pillar 14) — consistent throughout: `@swift-otter-emerald` in user-facing copy, `swift-otter-emerald` unprefixed in technical slugs, `swift-otter-emerald/107/1` when referring to specific published contributions.
- **Wire-native documents** (Pillar 33) — `docs/user/*.md` source docs ship with trailing YAML blocks declaring publish intent. Writing IS publishing.
- **Preview-then-commit** (Pillar 23) — Page 4 compute-path selection previews actual build cost per path via existing `preview.rs`.
- **Never prescribe outputs to intelligence** (Pillar 37, Law 5) — no hardcoded counts, ranges, or quotas anywhere in this design's LLM-facing surfaces.

### 1.3 Unified Flow Formula applied to pyramid queries

A tester's novel question that routes to source and triggers LLM generation is a revenue event. Querier pays. The cost per query is the LLM generation cost translated to credits via the existing cost model in `cost_model.rs` (concrete rate is LLM-provider-dependent + operator-configured; actual credit cost per typical novel question is a Phase 0 pre-flight measurement — target range documented in §7.4 content + verified before Phase 2 ships).

UFF splits the payment via the existing rotator-arm (80-slot integer economics, Pillar 9):

- 60% Creator (pyramid author)
- 35% Source chain (the pyramid nodes / corpus docs cited in the answer)
- 2.5% Wire
- 2.5% Graph Fund

Cached FAQ hits (local `faq_match` hits before `evidence_loop` fires) are free — no revenue event, no ledger entry.

**Tester subsidy:** the 50k seed credits per registration bonus is the subsidy envelope. Phase 0 pre-flight measures actual credits/query against this envelope and either (a) confirms 50k easily covers a full onboarding session + dozens of novel queries, or (b) surfaces that the envelope is undersized and triggers either seed-credit bump or Graph Fund subsidy allocation per Pillar 35. Not a blocker for design; is a pre-build measurement.

Once seeded credits run out, querier-pays continues via the credit pool.

### 1.4 Flywheel

```
tester asks → local FAQ lookup hits? ─yes→ return (free, fast)
                                     ─no→ local node-search hits with high confidence? ─yes→ return
                                                                                       ─no→ route to source via Wire-minted JWT
                                                                                              ↓
                                                                              source runs remote_pyramid_query chain
                                                                              (threads audience, uses StepContext, creates annotation with question_context)
                                                                                              ↓
                                                                              FAQ engine auto-fires on annotation save
                                                                              New FAQ contribution published (Pillar 25 path)
                                                                                              ↓
                                                                              answer returns to querier for immediate display
                                                                              + DADBEAR propagates the new FAQ through evidence graph on source
                                                                              + pinned-copy sync picks up the FAQ contribution on next tick (≤5 min)
                                                                              + demand_signal emitted if answer confidence low (Pillar 10)
```

This is not new infrastructure. Each arrow uses existing modules from the wire-node inventory.

---

## 2. User journey

**T+0s:** tester opens app. Magic-link or OTP auth. Handle auto-assigned by Wire DB trigger (`@swift-otter-emerald`).

**T+15s:** Page 1 — what Wire Node is, in Adam's voice.

**T+30s:** Page 2 — hackable engine, Agent-Wire.com as sharing layer.

**T+45s:** Page 3 — identity. Random handle shown as first-class; custom handle optional.

**T+60s:** Page 4 — three compute paths with inline cost preview. Agent Wire pre-toggled on by default (cooperative network is the cooperative default).

**T+75s:** Page 5 — three starting paths. Tester clicks "Ask the app anything."

**T+80s:** query surface opens against the pinned explainer pyramid. Tester asks "what's Agent Wire?" — local FAQ hit → instant answer.

**T+100s:** tester asks "what happens when I don't have a GPU and the network is empty?" — no local match → routes to source → remote_pyramid_query chain generates answer with tester-audience framing → FAQ accretes → answer returns to this tester and is propagated to every other pinned copy.

**T+5min:** tester has had a real product experience. Their novel question made the product smarter for everyone else. They've chosen compute paths knowing the actual costs. They have an identity on the network.

---

## 3. Onboarding flow — pages

State model: `~/.agent-wire/onboarding.json` stores per-step `{completed_at, choices_made}`. **Explicitly local-only per-device** — new machine installs trigger a fresh onboarding rather than syncing state across devices. This is the design choice; machine-specific hardware (GPU presence, Ollama availability) means setup decisions shouldn't port. Law 3 "read-through cache exception" applies.

Routing layer on boot checks the file; resumes at next uncompleted step. Settings → Onboarding replays any page. On replay, choices overwrite prior; no partial-delta model.

### Page 1 — What this is

> **Wire Node** *is a way to build understanding out of what you already have.*
>
> Wire Node uses language models to build an **understanding framework** on top of your documents, projects, conversations — whatever you point it at. That framework is called a pyramid.
>
> A pyramid lets you ask questions directly to your stuff. If the pyramid already knows the answer, you get it fast. If it doesn't, it figures out what evidence is missing, goes back to the source documents, and learns what it needs to. **The more you use a pyramid, the smarter and more useful it gets.**
>
> Pyramids keep themselves up to date as your files change. They can live above your core work as a substrate for your agents to operate from — so your agents understand the system holistically as it evolves, without you or them having to maintain docs by hand. Agents can also braindump their understanding back into the pyramid, where it gets generalized for next time.
>
> *(Wire Node does a bunch of other stuff too — but understanding pyramids are what you're here to test, so that's what we'll walk you through.)*
>
> You're not doing this alone. Wire Node connects you to other people building their own pyramids, and your idle GPU can help their builds while theirs help yours. You'll pick how much of that you want on the next screens.

Actions: `Continue →` · `Tour the app instead`

### Page 2 — The engine is yours to shape

> **The engine is hackable. The recipes are shared.**
>
> Everything that decides how a pyramid gets built — which questions to ask, how to chunk documents, how layers stack, how staleness propagates — lives in plain **YAML and Markdown files** on your machine. You can edit any of it and the engine picks up the change live. No restart. No rebuild. No code to touch.
>
> That also means the people who enjoy experimenting can share their work with the people who don't. **Agent-Wire.com** is the connecting layer — the best pyramid recipes, configurable pieces, and shared improvements show up there. You can pull whatever fits your use case, or publish your own for others. Experimenters lift everyone.

Actions: `Continue →` · `Back`

### Page 3 — Your identity

> **You're @swift-otter-emerald on the network.**
>
> We gave you a random handle so you can start immediately. It's a real identity — everything you publish, everyone who sees it, sees `@swift-otter-emerald`.
>
> Want a custom handle? Claim one here, or anytime later via Settings.

**Form (optional, inline):**
- Text input with live `GET /api/v1/wire/handles/check` validation as user types
- Green ✓ for available, red ✗ with `reason` code from check response
- Checkbox "Also release @swift-otter-emerald" checked by default (enabled once Wire-side release endpoint ships)

Actions: `Keep @swift-otter-emerald, continue →` · `Claim and continue →` (enabled when valid) · `Skip for now`

**Wire Agent Message Principle:** if the handle-claim endpoint fires any operator confirmation email, the request body passes `agent_message` (max 500 chars) describing the context: e.g. "Tester claimed a custom handle during first-run onboarding." Email surface renders it prominently. Absence of `agent_message` does not block the request.

### Page 4 — How do you want to compute?

> **Pick how your pyramids get built.**
> You can use any combination. Your choice doesn't lock you in — change it anytime in Settings.

Three cards side by side. Each card has a toggle + inline preview (§3.4 preview mechanism).

**Card A — Local (Ollama)**

*Private. Free. Slow unless you've got hardware.*

> Runs language models directly on your machine. Your files never leave your computer. Costs nothing per call. Speed depends entirely on your hardware — on a strong GPU it's snappy, on a laptop it's patient. Your fans will probably run.

Setup: `Use Ollama` → detect install (`which ollama` + `GET localhost:11434/api/tags`) → if missing, offer "Install Ollama" external link → model-select dropdown with recommended defaults per tier.

Inline preview (calls existing `POST /pyramid/preview` with Ollama path): "A typical project build takes ~15 min locally and costs 0 credits."

**Card B — OpenRouter**

*Fast. Cheap. Your queries travel to a third party.*

> Pay-per-call access to the big hosted models — GPT, Claude, Gemini, Llama on someone's GPU. Typical build costs pennies. Your prompts and responses are seen by OpenRouter and whichever provider serves the call.

Setup: `Add OpenRouter key` text field + optional monthly spending cap slider. Key validated via `GET https://openrouter.ai/api/v1/auth/key` on submit (format validation only; per-call validation happens at dispatch time).

Inline preview: "A typical project build takes ~2 min via OpenRouter and costs ~$0.25."

**Card C — Agent Wire**

*Peer-to-peer. Get seeded to start. Contribute when idle.*

> Other people's GPUs help build your pyramids while their owners are idle. Your GPU does the same for them. You spend credits when you request and contribute to the network to accumulate credits when idle. **We give you a pile of seed credits so you can start even if you can't help yet.** Speed depends on who's online. Privacy depends on the size of the network — larger network, more providers, less traceable.

Setup: Toggle (defaults on). If GPU detected (NVIDIA via CUDA, AMD via ROCm, or Apple Silicon via Metal — fail gracefully if detection errors), secondary "also contribute when idle" toggle defaults on. Both toggles independently overridable.

Inline preview: "A typical project build takes ~30s on the network and uses ~15 of your seed credits."

**Combination info box:**

> You can enable any combination. When you ask a question, Wire Node tries them in order:
>
> 1. Your own GPU if it's loaded and idle
> 2. The network if you're connected and capacity exists
> 3. OpenRouter if you've added a key
>
> If all three are on, you get the fastest result that matches your preferences. If only one is on, it's the only path. If none are on, Wire Node can't build — turn at least one on.

Validation: `Continue →` disabled until at least one path is set up. Agent Wire default-on means Continue works immediately.

**Per-path validation failures:**
- Ollama detection fails → "Not installed. Install now, skip for this session, or pick another path."
- OpenRouter key invalid → "Key format looks wrong. Check and re-enter, or skip."
- Agent Wire tunnel not yet provisioned → proceed anyway (tunnel comes up in <5s typical); first build attempt will fall through to another path if tunnel is still down at query time.

### Page 5 — What you can actually do

> **Ready. Here's where to start.**
>
> Wire Node is up. Your identity is set. Compute is configured. Here are three things you can do now:

**Three branch buttons:**

- **Ask the app anything** — opens query surface against the pinned explainer pyramid. First-click interaction is meaningful.
- **Build your first pyramid** — directory picker → triggers `build_runner::spawn_*`.
- **Wire up an agent** — MCP setup for Claude Code / Cursor / other MCP-aware tools.

> Not sure? Any of these is fine. Come back via Settings → Onboarding.

Actions: one of three + `Just show me the app` (tertiary, lands on Market tab per invisibility UX precedent).

### Page 6 — Tell us what's weird

> **One ask — tell us what's weird.**
>
> You're one of the first testers on Wire Node. The most valuable thing you can do is notice what confuses you, what feels wrong, what doesn't match what you expected. A sentence is enough.

**Form:** single textarea + send button. Submission publishes a `tester_feedback` Wire contribution (§5 schema spec).

Actions: `Send and continue →` · `Skip, never ask again` · `Remind me after my first pyramid`

---

## 4. The pinned-pyramid mechanism

### 4.1 Default pin list — `onboarding_pin_set` contribution

A Wire contribution that enumerates which pyramids a fresh-install node should auto-pin. Discovered via existing `wire_discovery.rs` — no new endpoint.

```yaml
schema_type: onboarding_pin_set
version: 1
pyramids:
  - slug: wire-node-explainer
    sources:
      - tunnel_url: https://node-<adam>.agent-wire.com
        priority: 1
    reason: "How to use Wire Node, built from the docs."
    priority: 1
```

`sources: [{tunnel_url, priority}]` is a list with exactly one entry for tester-ship. Forward-compatible with future mirror publishing: readers priority-sort and fall through on unreachable. Schema evolution requires no migration.

Adam updates the default list by publishing a new `onboarding_pin_set` contribution with higher version. Existing contribution-supersession handles propagation. Pinned nodes pick up the new list on their next sync tick.

### 4.2 Query routing — extends existing `reading_modes.rs`

The whole routing decision is chain-native per Pillar 17 (chains invoke chains, no special orchestrators). A new chain `ask_the_app` wraps both local-fast-path and remote-fallback; the HTTP entry point is an extension of `POST /pyramid/:slug/read/search` (existing route, handled by `reading_modes.rs::reading_search()`) with three added parameters:

- `remote_eligible=true` flag — enables the remote-fallback branch in the chain
- `audience` — Pillar 41 audience framing, first-class parameter threaded to every LLM call downstream
- JWT `aud=pyramid-query` Bearer auth (validated for cross-tunnel requests)

The route handler delegates entirely to `chain_executor::execute_chain_from(ask_the_app, inputs)`. No handler-side branch logic.

**`ask_the_app` chain** (new chain, published to Wire as a contribution per Pillar 28):

```yaml
# chains/defaults/ask_the_app.yaml
name: ask_the_app
version: 1
inputs:
  - question: string
  - audience: string
  - remote_eligible: bool
  - querier_operator_id: string
  - pyramid_slug: string
steps:
  - recipe: faq_match               # existing faq.rs::match_faq path wrapped as primitive
    inputs: { question, audience }
  - recipe: evidence_loop           # existing canonical primitive per skill inventory
    when: $steps[0].confidence < threshold_from_policy
    inputs: { question, audience }
  - recipe: remote_fallback_query   # new primitive — fires only when remote_eligible + still low-confidence
    when: $steps[1].confidence < threshold_from_policy && $inputs.remote_eligible
    inputs: { question, audience, querier_operator_id, pyramid_slug }
outputs:
  - answer: $coalesce(steps[2].answer, steps[1].answer, steps[0].answer)
  - confidence: $coalesce(steps[2].confidence, steps[1].confidence, steps[0].confidence)
  - source_nodes: ...
```

The `remote_fallback_query` primitive (new) mints a `aud=pyramid-query` JWT via Wire, POSTs to the source tunnel's same `/pyramid/:slug/read/search` route with `remote_eligible=false` (prevent re-routing loops), and returns the response. Source side runs the SAME `ask_the_app` chain — which means the source side ALSO runs `faq_match` → `evidence_loop` for the question. If `evidence_loop` produces a valid answer on the source side, that answer accretes into an annotation via existing `faq.rs::process_annotation` post-save hook (which is already `StepContext`-threaded via `call_model_and_ctx`), becomes a new FAQ contribution, and broadcasts to all pinned copies via existing DADBEAR + auto-publish.

Every primitive runs on `chain_executor::execute_chain_from`. Every LLM call inside `evidence_loop` + `faq.rs::process_annotation` already uses StepContext per existing code. Law 1 + Law 4 conform without new infrastructure.

**New primitives to build in Phase 1:**
- `faq_match` — wraps existing `faq.rs::match_faq` + returns confidence. ~30min.
- `remote_fallback_query` — HTTP client + JWT mint + response parsing. ~1h.

No other chain recipes invented. Existing `evidence_loop` handles the generation path.

### 4.3 Accretion + broadcast

Accretion happens entirely through existing pyramid infrastructure:

1. `evidence_loop` on source produces an answer via LLM generation (StepContext-threaded)
2. The chain-executor records the answer as an annotation on the question node with `question_context = original_question` (via existing annotation creation in `chain_dispatch.rs`)
3. Annotation save triggers existing `faq.rs::process_annotation` post-save hook — already `StepContext`-aware (uses `call_model_and_ctx` per `faq.rs:18-19, 100+`), creates or updates FAQ node with `match_triggers`
4. New FAQ contribution enters the source pyramid's build + auto-publishes to Wire via existing `publication.rs` flow
5. Pinned-copy 5-min refresh tick (`sync.rs:437`) pulls the new contribution
6. New FAQ enters pinned-copy DADBEAR mutation queue per Law 2 — existing recursive function handles propagation

No new accretion logic. No `faq_accrete` recipe invented — the annotation→FAQ path IS the existing mechanism.

For faster broadcast (sub-minute instead of ~5-min poll), the publish-on-annotation hook is a minor extension to `publication.rs` that emits immediately when a FAQ contribution lands, rather than waiting for build boundary. Stretch, not tester-ship critical.

### 4.4 Audience framing (Pillar 41, Inviolable 8)

The `audience` parameter is a first-class input on every chain step in `ask_the_app`. Explicit threading:

- **`faq_match`** — audience surfaced in the FAQ search prompt via existing `faq.rs` path (`call_model_and_ctx` already accepts audience via `LlmConfig.audience`)
- **`evidence_loop`** — audience threaded via existing chain-input mechanism; visible in every evidence-answer-synthesis prompt invocation inside the loop per `chain_executor` step-context propagation
- **`faq.rs::process_annotation`** (post-save) — audience-aware FAQ generalization via the already-StepContext-threaded `call_model_and_ctx` calls (lines 100, 277, 348 of faq.rs)
- **`remote_fallback_query`** — audience included in the remote request body + passed through to source-side `ask_the_app` invocation

Existing `manifest.rs` cognition steering consumed per-prompt where audience is surfaced. No hardcoded per-audience prompts; the LLM interprets audience context (Pillar 37 / Law 5 — never prescribe outputs). `audience: "tester"` activates explain-to-newcomer framing; `"operator"` deeper technical; `"agent"` structured-output bias. Manifest contributions can be superseded by operators to tune the interpretation.

### 4.5 Subtractive work — flag-as-wrong (Pillars 10, 24)

Every answer surface renders with a "this wasn't right" button. Clicking creates an `accuracy_flag` contribution pointing at the specific FAQ node or pyramid node that produced the bad answer, with a small text reason. The flag enters the existing challenge panel infrastructure (Pillar 24).

If the flag resolves in favor of the flagger (panel upholds), the original contributor pays clawback; flagger earns a small credit. If rejected, flagger forfeits a small deposit. Self-regulating — infinitely elastic supply per Pillar 10.

This closes the subtractive-work gap for the remote-query surface.

### 4.6 DADBEAR integration (Law 2)

New FAQ contributions arriving on pinned copies enter the pinned pyramid's DADBEAR mutation queue exactly like any other change signal. The recursive staleness function runs against the pinned copy's evidence graph. When the FAQ connects to question nodes that have existing answers, DADBEAR flags those answers as potentially-stale and propagates upward through existing edges.

No new propagation logic. Existing recursive function handles it.

### 4.7 Source-offline fallback

If Adam's node is offline when a tester asks a novel question: local FAQ + node-search still work instantly. Novel questions surface "This pyramid's source is offline right now. Try again in a minute, or ask something the pyramid might already know." Wire queues the question (stretch) and retries when source comes back. For tester-ship, accept the failure mode.

---

## 5. Schemas (Law 3 completeness)

Every new `schema_type` ships with: bundled seed contribution, generation skill, schema annotation. Per Law 3.

### 5.1 `onboarding_pin_set`

- **Bundled seed:** `src-tauri/assets/bundled_contributions.json` entry with Adam's published explainer pyramid slug + his tunnel URL.
- **Generation skill:** `chains/skills/onboarding_pin_set_generate.md` — chain recipe for generating a new pin-set when Adam adds new default pyramids. Invoked via `config_contributions.rs` generative path.
- **Schema annotation:** registered in schema registry (`schema_registry.rs`), schema validated against shape above on publish.

### 5.2 `tester_feedback`

Fields:

```yaml
schema_type: tester_feedback
version: 1
page_id: "onboarding_page_6"
text: "<tester feedback text>"
agent_id: null                          # optional
operator_handle: "swift-otter-emerald"  # optional
node_version: "0.3.0"                   # optional
session_metadata:
  onboarding_completed_pages: ["page_1", "page_2", "page_3", "page_4", "page_5"]
  compute_paths_enabled: ["agent_wire"]
  handle_claimed: false
```

- **Bundled seed:** none (no default feedback to seed).
- **Generation skill:** `chains/skills/tester_feedback_submit.md` — trivial skill that templates the fields from the form submission.
- **Schema annotation:** registered with loose shape (Pillar 3 — non-economic contribution, strict `derived_from` unnecessary).

Wire owner ships `/ops/feedback` admin view to query these contributions.

### 5.3 `accuracy_flag` (new, for Pillar 10)

```yaml
schema_type: accuracy_flag
version: 1
target_contribution_handle: "swift-otter-emerald/107/1"  # the FAQ node or pyramid node being flagged
reason: "<free text — what's wrong>"
flagger_operator_id: "<uuid>"
flagger_handle: "@curious-bison-crimson"
deposit_credits: 5                                       # initial value; tunable via accuracy_flag_policy
```

**Deposit rationale:** 5 credits balances spam-discouragement (enough cost to discourage frivolous flags) against participation-friendliness (small enough that legitimate flagging feels free). The value is tunable via a policy contribution (`accuracy_flag_policy`) so operators can supersede the default based on observed spam/signal ratios. Initial default chosen to align with subtractive-work thermostat principle (Pillar 10) — always-available, always-pays-enough-to-matter.

- **Bundled seed:** none.
- **Generation skill:** `chains/skills/accuracy_flag_submit.md`.
- **Schema annotation:** registered in schema registry; strict validation on `target_contribution_handle` field.
- Flows into the existing challenge panel via `challenge_panels.rs` (Pillar 24).

---

## 6. Handle flow

### 6.1 Shipped today

- Auto-assign via DB trigger `trg_wire_operator_bootstrap_handle` on `wire_operators INSERT`. Shape: `{adjective}-{noun}-{color}`. Example: `swift-otter-emerald`.
- `GET /api/v1/wire/handles/check?handle=foo` — availability query with reason codes.
- `POST /api/v1/wire/handles` with `{handle, payment_type}` — claim.
- Validation: `/^@?[a-zA-Z0-9_-]{3,30}$/`, lowercased.

### 6.2 Shipping with this work (per Wire owner)

- Release endpoint: `POST /api/v1/wire/handles` accepts optional `release_handle` field. Atomic insert-new + update-old `released_at = now()` in single transaction.
- Reserved-name list (code-hardcoded — flag for Pillar 2 follow-up to make this itself a contribution). Default set:
  `wire`, `admin`, `platform`, `agentwireplatform`, `agent-wire`, `agentwire`, `agent_wire`, `official`, `support`, `help`, `team`, `root`, `system`, `null`, `www`, `api`, `docs`, `blog`, `status`, `security`, `privacy`, `legal`, `abuse`, `moltbot`.
- Rejection reflected in `/check` response with `reason: "reserved"` so UI can surface.

### 6.3 Onboarding UX

Random handle is first-class. Custom claim is opt-in upgrade. No forced claim during onboarding (per Wire owner recommendation). Settings → Handles surfaces claim flow for power-users anytime.

Handle-path consistency rule enforced in this doc and all onboarding copy:
- `@swift-otter-emerald` — user-facing display form
- `swift-otter-emerald` — technical slug, unprefixed
- `swift-otter-emerald/107/1` — published contribution reference (handle + epoch-day + daily-seq)

---

## 7. Content — the explainer pyramid's corpus

### 7.1 Source docs

Seven user-facing docs at `agent-wire-node/docs/user/`:

- `what-is-wire-node.md` — positioning, value prop
- `pyramids-explained.md` — what a pyramid is, how it works, why it's useful
- `compute-paths.md` — the three paths, when to pick which
- `handles.md` — what a handle is, why it matters, how to claim custom
- `first-pyramid.md` — walkthrough: building your first pyramid from a folder
- `agent-setup.md` — MCP setup for Claude Code / Cursor / other
- `faq.md` — 20-30 top tester questions with answers (seed FAQ content)

Estimate: 2-4 hours of Adam-time, or agent-drafted with Adam editing pass.

### 7.2 Wire-native YAML (Pillar 33)

Every source doc ends with a trailing YAML block declaring Wire metadata. Writing IS publishing.

```yaml
---
wire:
  publish_as: contribution
  schema_type: user_documentation
  corpus_slug: wire-node-explainer-corpus
  part_of: wire-node-explainer
  license: cc0
  audience: [tester, operator]
  derived_from: []   # base source, no ancestry
---
```

Adam adds or edits a doc, saves; DADBEAR's file watcher picks up the change, the sync layer reads the trailing YAML and handles publishing automatically. Zero additional ceremony.

### 7.3 Building the pyramid

Standard build path via `chain_executor`:

1. Source docs at `agent-wire-node/docs/user/` become corpus via existing `folder_ingestion.rs`
2. `chain_registry.rs` assigns a chain (the mechanical question-pyramid recipe, or a forked variant)
3. `build_runner::spawn_*` fires
4. Auto-publish on build completion pushes to Wire under slug `wire-node-explainer`
5. Adam publishes `onboarding_pin_set` contribution pointing to `wire-node-explainer` slug + his tunnel URL

### 7.4 Freshness

Docs drift. Adam updates `docs/user/*.md`; file watcher triggers DADBEAR staleness propagation; the pyramid rebuilds; auto-publish pushes updates. Every pinned tester copy picks up changes on next poll. No tester action required.

### 7.5 Prototyping

Before `wire-node-explainer` exists, Phase 1 uses `core-selected-docs` (closest existing pyramid to user-facing content) as a placeholder pin target. Validates the mechanism against a real pyramid. Phase 4 swaps the prototype for the real explainer.

---

## 8. Wire-side work

Confirmed with Wire owner (architecture sign-off received). Ships in roughly 2.5 hours total.

### 8.1 Release endpoint + reserved-name list — ~1h
Atomic claim-and-release on existing `POST /api/v1/wire/handles`. Reserved-name validation server-side + reflection in `/check` reasons. Per §6.2.

### 8.2 Schema registration — ~30min
- `onboarding_pin_set` schema (with `sources: [{tunnel_url, priority}]` shape for forward compat)
- `tester_feedback` schema
- `accuracy_flag` schema (new per §5.3)
- `/ops/feedback` admin view for querying `tester_feedback` contributions

### 8.3 `aud=pyramid-query` JWT + rate limit — ~1h
- New JWT audience constant using existing EdDSA signing infrastructure (same key as dispatch + result-delivery JWTs)
- TTL: **5 minutes**. Mint fresh per query; no session caching. Matches compute-market convention.
- Rate limit: **100 pyramid-query mints per hour per operator**. Same mechanism as `/match` rate limit.
- Chronicle events on Wire side: `pyramid_query_minted` / `pyramid_query_denied`. Matches compute-market naming.
- Wire never sees query content — only mints JWTs.

### 8.4 Wire Agent Message Principle compliance
If handle-claim endpoint triggers any operator email, accept `agent_message` (max 500 chars) and render prominently. Required per the cross-cutting principle for every operator-email-triggering endpoint.

### 8.5 Stretch (post tester-ship)
- FAQ-delta sync: contributions fetchable by `since_timestamp` for incremental pull without full-export
- Push-on-publish notifications: mirror compute-market delivery-push pattern

---

## 9. Node-side work

### 9.1 Onboarding UI component
- `src/components/onboarding/` directory — new
- Components: `OnboardingRouter`, `Page1Welcome`, `Page2Engine`, `Page3Identity`, `Page4Compute`, `Page5NextSteps`, `Page6Feedback`
- State persistence via `~/.agent-wire/onboarding.json` (local-only per-device, §3)
- Routing layer in `App.tsx` that intercepts first-run
- Each page has its own error boundary with retry/skip/fallback actions

### 9.2 Query surface
- "Ask the app anything" as a drawer hooked into the existing `What do you want to do?` bar (persistent across every screen per invisibility UX)
- Calls the extended `/pyramid/:slug/read/search?remote=true&audience=tester` with JWT
- Displays answer + cites source nodes + renders "this wasn't right" flag button

### 9.3 Query-routing extension
- Extends `reading_modes.rs::search_mode` with remote fallback logic per §4.2
- New module `src-tauri/src/pyramid/remote_query.rs` containing:
  - JWT mint client (calls Wire's new pyramid-query-token endpoint)
  - Remote source-tunnel HTTP client
  - `remote_pyramid_query` chain integration
  - StepContext-threaded through every LLM call (Law 4)
  - Chronicle emissions: `pyramid_query_local_hit`, `pyramid_query_routed_to_source`, `pyramid_query_remote_success`, `pyramid_query_remote_failed`, `pyramid_query_late_answer`

### 9.4 Bootstrap orchestrator
- Runs on fresh-install detection: `onboarding.json` absent AND `wire_operators` table has no current operator row
- Sequence: magic-link auth → operator registered on Wire → api_token returned → discover latest `onboarding_pin_set` contribution → iterate `pyramids[]` → call `pyramid_pin(slug, sources[0].tunnel_url)` for each
- Explicit guard: fail loudly and surface UI message if api_token is missing when discovery is attempted
- Checkpoints stored in `onboarding.json` so partial completion is resumable

### 9.5 Flag-as-wrong component
- Button on every answer surface
- Opens modal: target_contribution_handle (pre-filled) + reason textarea + deposit disclosure
- Submits as `accuracy_flag` contribution via `config_contributions.rs::create`
- Confirmation toast: "Flag submitted. A challenge panel will review it."

### 9.6 Settings → Onboarding replay
- Lists completed pages with timestamps + choices made
- Each page has `Replay` button that re-runs that page from scratch (overwrites prior choices)

### 9.7 Preview integration for Page 4
- Each compute-path card calls `preview::generate_build_preview` + `preview::estimate_build_cost` (existing functions in `src-tauri/src/pyramid/preview.rs`) with a stub "typical project build" workload
- Displays estimated credits/time/cost inline beneath each toggle
- Refreshes when tester toggles path state

### 9.8 Remote_pyramid_query chain definition
- New chain YAML at `agent-wire-node/chains/defaults/remote_pyramid_query.yaml` per §4.2
- Chain published to Wire as a contribution (Pillar 25 — platform agents use public API)
- Imported via existing `chain_publish.rs` flow

---

## 10. Error handling + recovery

Every page has designed failure paths. No silent dead-ends.

| Failure mode | Behavior |
|---|---|
| Magic-link auth fails or times out | Retry button + "Try a different email" fallback; tester can exit onboarding and return later |
| Page 1-2 copy rendering fails | Fallback to plain text version; skip-with-warning option |
| Page 3 `/handles/check` returns 5xx | Accept claim attempt with best-effort; surface "server slow, keeping random for now" on timeout >5s |
| Page 3 handle-claim returns 4xx | Surface the `reason` code inline; offer retry or "Keep random and continue" |
| Page 4 Ollama detection fails | "Couldn't detect Ollama. Install it, skip for this session, or pick another path." |
| Page 4 OpenRouter key invalid format | Inline red X, offer retry |
| Page 4 tunnel not provisioned yet | Accept; first build retries via existing call_model_unified cascade |
| Page 4 preview call returns 5xx | Render card without preview, retry button |
| Page 5 "Ask the app anything" — pinned pyramid not yet synced | Grey out button with tooltip "Still syncing the explainer pyramid (< 30s). Try again shortly." |
| Page 5 pinned pyramid source offline | Accept local-only query; surface "source offline — novel questions may timeout" banner |
| Page 6 feedback POST fails | Cache in local onboarding.json; retry on next online tick |
| `pyramid_pin` succeeds but pyramid never finishes syncing | Surface in Settings → Pyramids with "Sync incomplete — retry" button; tester can skip and ask local-only questions until sync completes |
| FAQ contribution from source fails schema validation | Source-side logs + drops the contribution; pinned copy unaffected; tester's answer still displays from the immediate response (accretion path broken for that question only) |
| Bootstrap fetch of `onboarding_pin_set` fails | Complete onboarding without auto-pin; surface "Couldn't reach Wire to set up default pyramids. Pin manually via Settings when you can." |
| Bootstrap timeout (>30s) | Skip pin step; surface the same message; queue for background retry |

All failure surfaces avoid trader vocabulary per the invisibility purpose-lock.

---

## 11. Rollback

| Scenario | Recovery path | Owner |
|---|---|---|
| Remote-query JWT validation breaks | Wire owner disables pyramid-query endpoint → node-side falls through to local-only silently (no code change, feature-flag) | Wire owner |
| Explainer pyramid has bad content | Adam publishes corrected pyramid → updates `onboarding_pin_set` contribution → pinned copies pick up on next tick | Adam |
| Rate limit too low for real testers | Wire owner adjusts server-side limit | Wire owner |
| Bootstrap auto-pin fails | Onboarding completes without pinned pyramid; Settings → Pyramids → "Pin the explainer" surfaces manual path | Tester + documentation |
| `onboarding.json` corruption | Onboarding restarts from Page 1; prior ledger events preserved via Wire contributions (feedback, handle claim) | Tester, automatic |
| Accuracy flags creating noise | Adam publishes `accuracy_flag_policy` contribution raising deposit threshold | Adam |

---

## 12. Telemetry, consent, success metrics

### 12.1 Consent moment

Page 1 footer (above Continue):

> *Your questions, feedback, and how you use Wire Node help it get better for everyone. See our [privacy notes] for the full picture.*

The `[privacy notes]` link opens an in-app doc sourced from the explainer pyramid itself (`privacy-notes.md` in `docs/user/`) that enumerates every chronicle event type the app emits — not a vague policy but a specific list of what's tracked, matching §12.2 exactly. Scope includes queries, feedback, accuracy flags, compute-path toggles, onboarding page transitions, and network sync events.

No separate Page 1.5 — the footer is low-friction and the link is discoverable. Operators who want out can disable via Settings → Privacy (granular toggles per telemetry category).

### 12.2 Success metrics — chronicle events

Enumerated so we can verify the flywheel works:

| Event | Emitted when | Measures |
|---|---|---|
| `onboarding_page_completed` | Each page completion | Funnel step conversion |
| `onboarding_page_skipped` | Skip or defer action | Drop-off signal |
| `pyramid_query_attempted` | Tester hits "Ask the app anything" | Interest |
| `pyramid_query_local_hit` | Local FAQ or node-search answered | Cache effectiveness |
| `pyramid_query_routed_to_source` | Novel question routed | Cross-node flywheel activity |
| `pyramid_query_remote_success` | Source returned an answer | Success rate |
| `pyramid_query_remote_failed` | Source failed or timed out | Failure rate |
| `pyramid_query_late_answer` | Push arrived after timeout | Timing edge |
| `faq_contribution_received_on_pinned_copy` | Pinned-copy sync picked up accreted FAQ | Network propagation |
| `accuracy_flag_submitted` | Flag-as-wrong used | Subtractive-work engagement |
| `tester_feedback_submitted` | Page 6 feedback sent | Explicit feedback rate |
| `compute_path_enabled` / `compute_path_disabled` | Page 4 + Settings changes | Path preference distribution |

Success milestone: **first tester completes onboarding + asks a novel question + source returns answer + accreted FAQ propagates to another pinned copy** = tester-ship live.

---

## 13. Open decisions

### Resolved since rev 0.2
- UFF application to pyramid queries → §1.3 (querier pays, split per UFF, seed credits cover onboarding)
- Schema names → `onboarding_pin_set`, `tester_feedback`, `accuracy_flag`
- Explainer pyramid slug → `wire-node-explainer`
- Feedback destination → `tester_feedback` contribution + `/ops/feedback` admin view
- Rate limit → 100/hr per-operator, instrumented for retuning
- Re-entry semantics → overwrite prior choices on replay (§3)
- Skip vs defer → §3 state model
- Consent moment → Page 1 footer with privacy-notes link (§12.1)
- GPU detection logic → NVIDIA/AMD/Apple Silicon with graceful fallback (§3 Page 4)
- Multi-user household → onboarding.json is local-only per-device (§3, explicit choice)
- `docs/user` wire-native publishing → YAML block per-doc (§7.2)
- Remote query is new build → addressed via existing `reading_modes.rs` extension (§4.2, Law 1 compliance)
- First-pyramid success toast → defer to post-tester-ship (tracked in §14 stretch)
- Compute-market fork conflict check → pre-flight task logged (§14)

### Remaining open
1. **Content authorship** — Adam drafts vs. agent-drafted-with-Adam-review. Recommend agent-drafted, Adam editing pass for voice.
2. **PyramidBuildViz component for Page 2 visual** — existing in `src/components/pyramid-surface/`? If yes, reuse; if no, static graphic as placeholder.
3. **MCP setup page** — existing docs/UI to link to, or new sub-workstream?

### Resolved in rev 0.4
- Seed credit disclosure: **mention 50,000 explicitly** on Page 4 Card C preview line for concreteness ("~15 of your 50,000 seed credits"). Already baked into the Card C copy above.
- Actual credit-per-query cost: resolved via §1.3 — Phase 0 pre-flight measures against the 50k envelope before Phase 2 ships. Real-world data replaces placeholder "~5 credits" assumption.

---

## 14. Ship sequence

Realistic calendar time: 10-15 days post-audit-close. Phases overlap where dependencies permit.

### Phase 0 — Pre-flight (half-day)
- Adam or agent drafts first cut of `docs/user/*.md` corpus
- Wire owner lands Tier 1-3 asks from §8 in parallel
- Audit round 2+ cleared
- Compute-market fork file-conflict diff run; coordinate merge strategy

### Phase 1 — Mechanism scaffold (2 days)
- Node-side bootstrap orchestrator against `core-selected-docs` prototype pin (§9.4)
- Query-routing extension local-branch-only (§9.3) — FAQ + node-search locally, no remote routing yet
- `remote_pyramid_query` chain YAML published (chain definition exists even if not called)
- Schema registrations land on Wire side (§8.2)
- `cargo check` + `cargo test --lib` clean; no regressions

### Phase 2 — Source-side remote query + accretion (2 days)
- Wire-side: `aud=pyramid-query` JWT + rate limit live (§8.3)
- Node-side: remote fallback in query-routing lit up (§9.3)
- Source-side: `remote_pyramid_query` chain wired to auto-create annotations with question_context
- Pinned-copy FAQ pull via existing 5-min tick
- Publish-on-annotation path landed (§4.3 real-time broadcast extension)
- Offline smoke: novel question routes to source, accretes, propagates to second pinned copy

### Phase 3 — Onboarding UI (2 days)
- State persistence + routing (§9.1)
- Pages 1-6 copy + interactivity + error handling
- Page 4 preview integration (§9.7)
- Page 5 query surface (§9.2)
- Page 6 tester_feedback submission
- Flag-as-wrong modal (§9.5)
- Settings → Onboarding replay (§9.6)

### Phase 4 — Content + publish (half-day Adam-time)
- Adam editing pass on drafted `docs/user/*.md`
- Build `wire-node-explainer` pyramid from corpus
- Publish to Wire
- Publish new `onboarding_pin_set` superseding the prototype
- Fresh installs now auto-pin the real explainer

### Phase 5 — Stretch
- Push-on-publish notifications (faster sync)
- FAQ-delta incremental pull
- FAQ generalization prompt instrumentation
- First-pyramid success toast (handle-claim nudge)
- Offline queueing of novel questions

### Critical-path dependency
Adam's editing pass on content is the single human-critical-path item. Agent drafts can happen in parallel with Phase 1. Adam's review pass during Phase 2-3 (~1 hour). Phase 4 publish ships immediately after Phase 3.

---

## 15. Audit surfaces

Every section is auditable against:
- `GoodNewsEveryone/docs/wire-pillars.md` — all 44 pillars (excluding obsolete Pillar 26)
- `GoodNewsEveryone/docs/inviolables.md` — 14 non-negotiable constraints
- Wire-node 5 Laws (via `wire-node-rules` skill)
- Wire Agent Message Principle (`GoodNewsEveryone/docs/wire-agent-message-principle.md`)
- Stop-and-search checklist against wire-node module inventory

---

## Rev log

| Rev | Date | Change |
|---|---|---|
| 0.1 | 2026-04-17 | Initial plan. |
| 0.2 | 2026-04-17 | Wire owner architectural sign-off absorbed. JWT TTL, rate limit, chronicle naming, multi-source schema, tester_feedback contribution, expanded reserved-name list. §10 open-questions resolved 4. |
| 0.3 | 2026-04-17 | Pillar conformance pass resolved 8 violations (Pillars 7, 10, 14, 17, 23, 24, 33, 41). Wire-node 5 Laws alignment: remote query reframed as chain running on chain_executor; DADBEAR integration for FAQ propagation; Law 3 schema completeness (bundled seed + generation skill + schema annotation specified for all new schema_types); StepContext-on-every-LLM-call made explicit. Accuracy-flag mechanism added for subtractive work. Audience parameter threaded through remote query. UFF resolution (querier-pays + seed-credit subsidy). Preview-then-commit on Page 4 via existing preview.rs. Wire-native YAML on docs/user/*.md. Error handling + rollback + consent sections added. Stop-and-search module-reuse confirmed: reading_modes.rs extension, crystallization.rs compatibility, manifest.rs for audience steering, wire_discovery.rs for default-pin discovery. Calendar timing revised to 10-15 days. Document rewritten to read as final-native per AGENTS.md "internalize corrections, do not publish them." |
| 0.4 | 2026-04-17 | Verification pass against codebase reality. Corrections: reading_modes function is `reading_search()` not `search_mode`; preview uses `generate_build_preview` + `estimate_build_cost`; faq.rs confirmed StepContext-compliant via `call_model_and_ctx` usage (auditor's Law 4 concern was false alarm). Chain refactored from `remote_pyramid_query` to `ask_the_app` — wraps local-fast-path AND remote-fallback as a single chain invocation so handler has zero branching logic (tightens Pillar 17 conformance). Invented chain recipes replaced with canonical primitives (`evidence_loop` from skill inventory) + two new primitives (`faq_match`, `remote_fallback_query`) explicitly spec'd as Phase 1 work. Audience threading made explicit per LLM call site (§4.4). UFF credit-per-query reframed as Phase 0 pre-flight measurement against the 50k-credit envelope. Consent text broadened to cover full telemetry scope (§12.1). Error table expanded with magic-link, pin-sync-incomplete, FAQ-schema-validation failure rows. Handle-path schema example corrected to use auto-generated format. Deposit amount justified + flagged as policy-contribution-tunable. §13 cleaned up. Plan-stage audit closed. |
