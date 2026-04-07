# Post-Agents Retro: The Living Knowledge Surface

> A new category of web experience where pyramids ARE websites, questions ARE navigation,
> and the aesthetic — precise, dense, passively beautiful — is a philosophical statement
> about what the web should have been.

---

## The Concept

### Three Eras of the Web

**Web 1.0** was designed for **humans reading documents written by humans.** Pages were text, links, and forms. The author typed every word. The website was static — a snapshot of what the author knew when they published.

**Web 2.0** was designed for **humans interacting with applications built by humans.** SPAs, JavaScript frameworks, feeds, likes, comments. The dynamism was in the UI, not the content. The website became a marketing funnel wrapped around a database.

**Post-agent web** is designed for a world where **the content maintains itself, the readers include agents, and asking a question literally changes what the site contains.** Neither Web 1.0 nor Web 2.0 is built for this.

### What "Post-Agents Retro" Means

It's *retro* because the original web's values — semantic documents, universal addressing, machine-readability, hyperlinks — turn out to be exactly what the agent era needs. The SPA era was a 20-year detour optimized for human eyeballs, ad impressions, and engagement metrics.

But it's not *nostalgic*. It's retro the way brutalist architecture is retro — it strips away everything decorative and lets the structure be the aesthetic. The visual language is the character grid: monospace type, box-drawing connectors, block-fill density for confidence, ANSI-inspired color semantics. Every visual element is intentional. Every glyph position carries meaning.

### Core Principles

1. **The site is for two audiences simultaneously.** Agents and humans get the same content. The semantic HTML layer works with `curl`, screen readers, and RSS. The canvas layer renders the magical ASCII-aesthetic experience. Both serve the same knowledge.

2. **The site knows what it doesn't know.** No 404s. Gaps are features: "This page doesn't exist yet. Your question is the reason it will." The boundary of knowledge is visible, honest, and in motion.

3. **Navigation is question-driven.** No menu bars. You ask questions. The site shows you the path through the knowledge graph. If the path doesn't exist, it starts building it. Browsing IS questioning — every click deeper is a demand signal that shapes future expansion.

4. **Every sentence has a pedigree.** Not hidden metadata — visible, first-class provenance. Version, timestamp, source count, quality score, staleness status. The ability to prove every claim is the entire value proposition.

5. **The site grows from being read.** Questions from humans, agents, and DADBEAR all enter the same expansion pipeline. The audience shapes the knowledge by asking, not by writing. Demand drives coverage.

6. **The aesthetic is anti-marketing by design.** Dense text signals "I respect your intelligence." Visible provenance signals "I can prove every claim." Visible gaps signal "I'm honest about my limits." Precise typography signals "every visual detail is intentional." The site doesn't want your attention, your signup, or your credit card. It wants your curiosity.

---

## Architecture: Two Concurrent Layers

Not sequential tiers — concurrent rendering layers serving the same data.

### Layer 1: Semantic HTML (No JS Required)

Warp routes return server-rendered HTML using the same data functions the JSON API calls.

```
GET /p/{slug}              → Pyramid home: apex + topic TOC
GET /p/{slug}/{node_id}    → Single node: content + children + edges
GET /p/{slug}/tree         → Full tree view
GET /p/{slug}/search?q=... → Search results with node snippets
GET /p/{slug}/glossary     → Auto-generated from all terms[]
GET /p/{slug}/folio        → Full Folio document (depth-controlled)
GET /p/{slug}/folio?depth=2 → Folio with depth limit
```

This layer:
- Works with `curl`, `wget`, Lynx
- Is crawlable by search engines  
- Is accessible to screen readers
- Returns structured `<article>`, `<section>`, `<nav>` elements
- Includes provenance in `<footer>` elements with handle paths
- Serves agents that prefer HTML over JSON

**This IS the post-agents-retro philosophy in its purest form.** Documents with links and visible structure. If nothing else ships, this alone makes every Wire node a publisher.

### Layer 2: Pretext + LayoutSans Canvas (Progressive Enhancement)

When JavaScript is available, a canvas layer renders ON TOP of the semantic HTML. This is where the magic happens.

**Technology stack:**
- **Pretext** (`@chenglou/pretext`) — Text measurement and layout via `canvas.measureText()`. 300-600× faster than DOM measurement. Pure arithmetic line-breaking. Non-rectangular text flow via `layoutNextLine()` with variable widths.
- **LayoutSans** (v0.2, April 2026) — Canvas interaction layer built on Pretext. Provides Ctrl+F search with highlighted matches, click-drag text selection, shadow semantic tree for accessibility (VoiceOver, NVDA, JAWS), and R-Tree spatial indexing for 100k+ interactive elements at p95 < 0.5ms.
- **Canvas 2D** — Direct rendering of ASCII aesthetic: box-drawing characters, block fills, ANSI-inspired color, glow effects, streaming animations.

The canvas layer:
- Renders text flowing around generated ASCII art via `layoutNextLine()` with per-line variable widths
- Animates knowledge synthesis in real-time (streaming tokens → characters resolving)
- Provides canvas-native search (LayoutSans) styled as part of the ASCII aesthetic
- Maintains accessibility through LayoutSans' shadow semantic tree

**Visitors without JS get the full knowledge surface as clean HTML. Visitors with JS get the magical canvas experience. Same data, same warp routes, two rendering layers.**

---

## The Rendering System

### Data Flow

```
Rust/warp server (port 8765, behind Cloudflare tunnel)
  ├── HTTP routes: serve semantic HTML + pyramid JSON
  ├── WebSocket: stream synthesis tokens during expansion
  └── Static assets: serve Bun-bundled client JS

Bun client (TypeScript, progressive enhancement)
  ├── Pretext: text measurement engine (client-side only)
  ├── LayoutSans: interaction layer (selection, Ctrl+F, a11y)
  ├── Canvas renderer: ASCII aesthetic, streaming animation
  └── WebSocket client: receive expansion tokens, drive animation
```

### Typography as Semantic Encoding

In the post-agent world, visual presentation isn't decorative — it encodes meaning. Pretext's glyph-level precision enables this:

```
FULLY VERIFIED (sourced, fresh, high-confidence):
│  DADBEAR monitors source documents via content hashing.
│  Each pyramid node records the SHA-256 hash of the source
│  material it was synthesized from.

STALE (sourced, but source has changed since synthesis):
┊  The sweep interval defaults to 4 hours, configurable
┊  per pyramid via the staleness_config table.
┊  ⚠ stale since 2026-04-05T09:00:00Z — rebuild queued

INFERRED (synthesized from children, no direct source):
╎  Combining the hash-based detection with the transitive
╎  staleness propagation, DADBEAR ensures that changes to
╎  any leaf source propagate upward through the pyramid.

GAP (question-generated, not yet expanded):
░  Semantic drift detection — the ability to identify when
░  a source's meaning has changed even though its text
░  has not — is not currently implemented.
```

Border characters encode provenance: `│` solid = sourced and verified, `┊` dashed = stale, `╎` dotted = inferred, `░` shaded = gap. Human readers absorb this subconsciously. Agent readers parse it structurally. Both audiences served by the same encoding.

### ASCII Art via LLM Text Generation

Mercury-2 (`inception/mercury-2`) is a text LLM used throughout the codebase for synthesis, question compilation, and evidence answering. It generates ASCII art as **text output** — not image-diffusion-level generation, but reliable for structural and decorative character art:

- **Pyramid banners** — Thematic ASCII art generated from the apex headline. A pyramid about "Runtime Services" gets different art than one about "Cryptographic Protocols."
- **Topic dividers** — Contextual decorations per topic section, not repeated `═══════`.
- **Structural diagrams** — Architecture and flow diagrams rendered as ASCII box art, generated from the actual system structure described in the nodes.
- **Synthesis noise** — When content materializes, the "noise" characters that resolve into final text are thematically relevant, not random.

Box-drawing characters (`┌─┐│└─┘`, `╔═╗║╚═╝`), block elements (`░▒▓█`), and tree connectors (`├─`, `└─`) are high-reliability targets for LLM text generation. Scene illustrations use reference-guided prompting for consistency. For higher-fidelity ASCII art, fine-tuned models like SVE-ASCII exist but are not yet integrated.

Text flows AROUND these art elements via Pretext's `layoutNextLine()` with variable widths — like a magazine layout, but in character cells, at 60fps.

---

## The Question Experience

### Asking a Question

The visitor types: "How does DADBEAR detect that a source document has changed?"

**If the pyramid can answer:** The response renders immediately from existing nodes. Sources cited, confidence shown, handle paths linked. Related questions suggested.

**If the answer is partial:** The pyramid says what it knows, identifies the gap explicitly, and the gap enters the expansion queue. The visitor gets a bookmarkable handle path for the pending content.

**If the expansion pipeline runs live:** The visitor watches knowledge being synthesized in real-time. WebSocket streams synthesis tokens:

1. Server's LLM pipeline synthesizes answer tokens
2. Tokens arrive over WebSocket as `{ nodeId, token, done }` chunks  
3. Client's `requestAnimationFrame` loop drains the token buffer smoothly
4. Pretext re-lays-out the growing text (~0.1ms per call)
5. Canvas renders: characters resolve from noise to final text, lines reflow, ASCII borders grow to accommodate

> **Implementation note:** The internal plumbing for this largely exists:
> - `mpsc::channel<BuildProgress>` already streams 40+ event types through the build pipeline
> - Warp 0.3 (already in Cargo.toml) includes WebSocket support natively (`warp::ws()`)
> - OpenRouter supports SSE streaming (`"stream": true`), and `parse_openrouter_response_body`
>   already handles `data:` prefix stripping
> - Cloudflare tunnels proxy WebSocket upgrades natively
>
> The work is: (a) add a `warp::ws()` upgrade handler that forwards `BuildProgress` events
> (~30 lines), and (b) optionally add `"stream": true` to `call_model_unified` for
> token-level granularity. This is a day of plumbing, not a new system.

**The visitor literally watches knowledge being born.** Not a loading spinner — the product experience.

### Questions Are The Universal Primitive

A human question, an agent query, a DADBEAR staleness mutation, and a decomposed sub-question all enter the same expansion pipeline:

```
              ┌─────────────┐
              │  QUESTIONS   │ ← the universal primitive
              └──────┬───────┘
                     │
     ┌───────────────┼───────────────┐
     │               │               │
  Human on         Agent           DADBEAR
  the website      query           staleness
     │               │               │
     └───────────────┼───────────────┘
                     │
              ┌──────▼───────┐
              │ Gap Analysis  │ Can the pyramid answer this?
              └──────┬───────┘
                     │
              ┌──────▼───────┐
              │ Expansion     │ If not: queue for build
              │ Queue         │ Priority by demand signal
              └──────┬───────┘
                     │
              ┌──────▼───────┐
              │ Build Pipeline │ Synthesize new nodes
              └──────┬───────┘
                     │
              ┌──────▼───────┐
              │ Pyramid grows │ New nodes → website updates
              └──────────────┘
```

The source — human browser, remote agent, internal daemon — is metadata on the question, not a different kind of event.

### Expansion Economics (Already Designed)

Three absorption modes control who pays for question-driven expansion:

| Code Enum | Human Name | Who Pays | Who Owns | Revenue Flow |
|-----------|-----------|----------|----------|-------------|
| `open` | Owner-absorbs | Pyramid operator | Operator | Operator pays compute cost, owns the result |
| `absorb-all` | Questioner-pays | The asker (via Wire identity) | Questioner | Questioner owns the contribution; 35% citation royalty flows to pyramid operator at zero cost |
| `absorb-selective` | Action-chain | Decided per-question | Varies | Operator's agent evaluates the question and decides the mode |

For V1 (anonymous visitors without Wire identity), `open` mode is the default. V2 adds `absorb-all` (questioner-pays) once email/magic-link identity is wired into the web surface.

---

## The Network

### Discovery via Wire

Other nodes discover your pyramids via Wire graph queries for `pyramid_metadata` contributions, which contain:
- `tunnel_url`, `pyramid_slug`, `node_count`, `max_depth`
- `access_tier`, `access_price`, `absorption_mode`
- `topics`, `apex_headline`, `quality_score`

### Cross-Pyramid Links

Web edges between pyramids become real hyperlinks between tunnel-served sites. When Adam's pyramid links to another operator's pyramid via web edges, those become clickable links that resolve across tunnel URLs.

If the remote node is offline, the link is offline. No p2p caching or fallback at this stage — the Wire handles coordination and discovery, not the web surface. Keep it simple.

### Access Tiers

| Tier | Web Surface Behavior |
|------|---------------------|
| `public` | Full access, no auth required |
| `circle-scoped` | Apex visible; deeper content requires Wire JWT with matching circle_id |
| `priced` | Apex visible as teaser; full content requires Wire credit payment |
| `embargoed` | Not served via tunnel at all |

---

## Performance Envelope

| Metric | Value | Source |
|--------|-------|--------|
| Pretext `layoutWithLines()` for 500-word node | < 0.5ms | Pretext benchmarks |
| 20 simultaneous nodes, full layout | 5-10ms | Within 16ms frame budget |
| LayoutSans `queryPoint()` hit-testing | p95 < 0.5ms | LayoutSans v0.2 docs |
| 100k variable-height items | 46ms | LayoutSans benchmarks |
| Re-layout on streaming token arrival | ~0.1ms | Pretext prepare() caching |
| Full resize reflow, 20 nodes | ~10ms | Pure arithmetic, no canvas calls |

Mobile: viewport-based virtualization via LayoutSans R-Tree. Optional 1× pixel density for low-end devices. Canvas capped at 4500×3500px to avoid Android compositing issues.

---

## Build Scope

### V1: The Living Surface (3-4 weeks)

| Component | Effort | Notes |
|-----------|--------|-------|
| Warp HTML routes (6-8 endpoints) | Days | Format existing data as HTML |
| CSS retro aesthetic | Days | Creative work — the feel |
| Pretext + LayoutSans client bundle | Days | Install, wire to data |
| Canvas renderer with ASCII aesthetic | 1-2 weeks | The creative/engineering core |
| WebSocket streaming from warp | Days | Well-understood pattern |
| Mercury-2 ASCII art generation | 1 week | Integration + prompt engineering |
| `layoutNextLine()` flowing around art | Days | Pretext handles natively |

### V2: Identity + Economics

- Email/magic-link visitor identity
- Questioner-pays absorption mode
- Action chain question assessment
- Returning-visitor diffs ("here's what grew since you last visited")
- Question quality scoring (productive questions → priority)
- Overlay lenses (question-driven re-weighting of the whole surface)

### What Already Exists (Not Build Items)

- Question → expansion pipeline: **fully wired and working locally**
- Cloudflare tunnel: **provisioned and connected**
- Dual auth (local + Wire JWT): **operational**
- Access tier enforcement: **operational**
- Rate limiting (per-operator, per-tunnel): **operational**
- Heartbeat with tunnel_url: **operational**
- Handle paths for universal addressing: **operational**
- DADBEAR staleness detection: **operational**
- Absorption modes: **designed and implemented**
- Discovery metadata on Wire graph: **publishing works**

---

## The Philosophy, Stated Plainly

The modern web treats visitors as **consumers** — of content, of products, of attention-optimized feeds. The entire stack is designed to extract value from the reader's presence.

A post-agents-retro knowledge surface treats visitors as **catalysts**. Your presence — specifically your questions — literally causes the site to grow. The site cannot grow without questions. It doesn't want your attention, your signup, your credit card. It wants your curiosity. That's the only input it needs to become more useful.

The aesthetic follows from the philosophy:
- Dense text → "I respect your time and intelligence"
- No decoration → "I'm not trying to sell you anything"
- Visible provenance → "I can prove every claim"
- Visible gaps → "I'm honest about my limits"
- Precise typography → "Every visual detail is intentional"
- Generated ASCII art → "Even the decoration was synthesized from the knowledge"
- Streaming synthesis → "You can watch knowledge being born"

It looks retro because the original web got it right. It feels like the future because no website has ever grown smarter from being visited.

---

*Document: docs/vision/post-agents-retro-web.md*
*Related: docs/plans/pyramid-folio-generator.md*
*Related: docs/handoffs/handoff-pyramid-folio-command.md*
