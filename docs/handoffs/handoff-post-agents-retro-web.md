# Build Handoff: Post-Agents Retro Web Surface

> **Goal:** Turn every Wire node's knowledge pyramids into living websites, served through
> the existing Cloudflare tunnel, using a two-layer architecture: semantic HTML (works
> everywhere) + canvas-based ASCII aesthetic (progressive enhancement for JS-enabled browsers).
>
> **Timeline:** ~4 hours. Ship the whole thing.
>
> **Reference docs:**
> - `docs/vision/post-agents-retro-web.md` — The full vision, philosophy, and architecture
> - `docs/research/pretext-technical-reference.md` — Pretext/LayoutSans technical reference
> - `docs/plans/pyramid-remote-access-status.md` — What exists vs. what's needed
> - `docs/audit-post-agents-retro-web.md` — Audit of the docs against the codebase

---

## What This Is

Every Wire node runs a Tauri desktop app with an embedded warp HTTP server (port 8765)
behind a Cloudflare tunnel. That server already has ~50 JSON API endpoints for pyramid
CRUD, search, navigate, build, etc. It already has dual authentication (local + Wire JWT),
access tier enforcement (public/priced/circle/embargoed), rate limiting, and heartbeat
registration.

**What's missing:** HTML. Right now the server only speaks JSON. We're adding a parallel
set of routes under `/p/` that return rendered HTML pages — and then layering a canvas-based
"magical" experience on top for browsers with JS.

The result: visit `https://<tunnel-url>/p/my-pyramid` in a browser and see a beautiful,
dense, retro-styled knowledge surface. Ask it a question and watch knowledge synthesize
in real-time. `curl` the same URL and get clean semantic HTML. An agent hits the same URL
and gets structured, navigable content.

---

## The Philosophy (Read This — It Affects Every Design Decision)

This is NOT a conventional website. It is a **post-agents-retro** knowledge surface.
The design philosophy directly affects CSS choices, layout decisions, and interaction
patterns:

1. **Dense text signals respect.** No hero sections. No whitespace-as-design. Content is
   the product. Every pixel shows knowledge or provenance.

2. **Provenance is first-class.** Every node shows its version, source count, confidence
   score, staleness status, and handle path. This isn't hidden metadata — it's visible,
   styled, and part of the reading experience.

3. **Gaps are features.** When the pyramid doesn't know something, it says so explicitly
   and visually. Gaps are invitations, not errors. "This section doesn't exist yet.
   Your question is the reason it will."

4. **The aesthetic is the character grid.** Monospace type. Box-drawing borders
   (`┌─┐│└─┘`, `╔═╗║╚═╝`). Block fills for density (`░▒▓█`). Tree connectors (`├─`, `└─`).
   ANSI-inspired color semantics. This is closer to a BBS or a well-crafted terminal UI
   than a marketing site.

5. **Anti-marketing by design.** No CTAs, no signup flows, no tracking pixels, no cookie
   banners. The site exists to be queried and to grow.

6. **Questions drive expansion.** The question box is the primary interaction. Asking a
   question that the pyramid can't fully answer triggers the expansion pipeline. The
   visitor literally causes the site to grow.

Build agents: if you find yourself reaching for a hero section, a gradient button, or a
hamburger menu — stop. That's the wrong aesthetic. Think terminal, think density, think
the original web but with precise typography and intentional structure.

---

## Architecture: Two Concurrent Layers

### Layer 1: Semantic HTML (No JS Required)

New warp routes under `/p/` that return server-rendered HTML. These call the same DB
query functions the JSON API already uses — `db::get_node()`, `db::get_nodes_by_depth()`,
`query::search()`, etc. — but format the output as HTML instead of JSON.

**Routes:**

| Route | Purpose | Data Source |
|-------|---------|-------------|
| `GET /p/{slug}` | Pyramid home: apex headline + topic table of contents | Apex node + depth-1 children |
| `GET /p/{slug}/{node_id}` | Single node view: content + children + web edges | `db::get_node()` + children |
| `GET /p/{slug}/tree` | Full tree view with collapsible hierarchy | `db::get_nodes_by_depth()` |
| `GET /p/{slug}/search?q=...` | Search results with node snippets | `query::search()` |
| `GET /p/{slug}/glossary` | Auto-generated glossary from all `terms[]` | All nodes' terms fields |
| `GET /p/{slug}/folio` | Full folio document (depth-controlled) | Recursive node traversal |
| `GET /p/{slug}/folio?depth=N` | Folio with depth limit | Same, with depth cutoff |

**Key requirements:**
- HTML must work without JavaScript — this is the foundation
- Use semantic elements: `<article>`, `<section>`, `<nav>`, `<footer>`
- Include provenance in `<footer>` elements (handle path, version, staleness)
- Each node's staleness status should be visually encoded (see vision doc for the
  border-character encoding: `│` solid = verified, `┊` dashed = stale, etc.)
- Cross-pyramid web edges become real `<a href>` links to other tunnel URLs
- The question box POSTs to the existing `/pyramid/:slug/navigate` endpoint

**Auth:** These routes need to honor the existing dual auth system. Public pyramids
serve HTML to anyone. Priced/circle-scoped pyramids show the apex as a teaser and
require Wire JWT for deeper content. Embargoed pyramids return nothing.

**CSS:** The retro aesthetic is the creative core of Layer 1. This is where the
character-grid feel, the monospace typography, the box-drawing borders, and the
ANSI-inspired color scheme live. The CSS should be self-contained — no Tailwind,
no framework. A single CSS file that establishes the entire visual identity.

Think: dark background, light monospace text, box-drawing borders around content
blocks, subtle glow effects on interactive elements, high contrast, dense layout.
Color semantics: green for verified/fresh, amber for stale, red for gaps, blue
for links/references. The kind of thing that looks like it was designed by someone
who loves terminals.

### Layer 2: Pretext + LayoutSans Canvas (Progressive Enhancement)

When JavaScript is available, a canvas layer renders ON TOP of the semantic HTML.
This is where the "magical" experience lives.

**Technology stack:**
- `@chenglou/pretext` — 15KB, zero-dep. Text measurement via `canvas.measureText()`.
  300-600× faster than DOM measurement. Key API: `prepare()`, `layout()`,
  `layoutNextLine()` (for text flowing around irregular shapes).
- `LayoutSans` (v0.2, April 2026) — Built on Pretext. Provides canvas-native Ctrl+F
  search, text selection, and accessibility via shadow semantic tree. R-Tree spatial
  indexing for hit-testing 100k+ items at <0.5ms.

**What the canvas layer does:**
- Renders the same pyramid content but with the full ASCII aesthetic: generated ASCII
  art banners, text flowing around art via `layoutNextLine()` with per-line variable
  widths, block-fill density encoding for confidence scores
- Provides streaming text materialization when synthesis is happening (characters
  resolve from noise to final text, lines reflow, borders grow to accommodate)
- Canvas-native search (LayoutSans) styled as part of the ASCII UI
- Full accessibility maintained through LayoutSans' shadow semantic tree

**Bundle:** Use Bun to bundle the client-side TypeScript. The warp server serves the
bundled JS as a static asset. The HTML pages include a `<script>` tag that loads
the bundle — if it loads, the canvas layer activates. If not, the HTML stands alone.

**Mercury-2 ASCII art:** The pyramid's text LLM (`inception/mercury-2`, available via
OpenRouter, unlimited and free) generates ASCII art as text output. Use it to generate:
- Per-pyramid thematic banners from the apex headline
- Contextual topic dividers
- Structural diagrams from system descriptions in nodes
These can be generated at build time or lazily on first page render and cached.

### WebSocket Streaming

The build pipeline already emits `BuildProgress` events through `mpsc::channel`
internally (40+ emission points across build.rs, build_runner.rs, vine.rs). Currently
these events drive the Tauri desktop UI.

**What to build:**
1. A `warp::ws()` upgrade handler (warp 0.3 includes WebSocket natively)
2. Subscribe to the existing BuildProgress channel
3. Forward events as JSON over the WebSocket to the browser
4. Client-side: drain events through `requestAnimationFrame` for smooth 60fps animation

**Optional enhancement:** Add `"stream": true` to the OpenRouter API call body in
`call_model_unified` to get SSE token-level streaming. The response parser
(`parse_openrouter_response_body`) already handles `data:` SSE prefix stripping.
Use `resp.bytes_stream()` instead of `resp.text().await` to get chunks incrementally.
Then forward individual tokens over the WebSocket for character-level materialization.

**Cloudflare tunnels proxy WebSocket upgrades natively.** No config needed.

---

## What Already Exists (Do Not Rebuild)

The build team should audit these systems to understand them, not rebuild them:

| System | Status | Notes |
|--------|--------|-------|
| Warp HTTP server on port 8765 | ✅ Running | ~50 JSON API routes in `pyramid/routes.rs` |
| Cloudflare tunnel provisioning | ✅ Running | `tunnel.rs` — binary download, provision, monitor |
| Dual auth (local + Wire JWT) | ✅ Running | `with_dual_auth` / `with_slug_read_auth` filters |
| Access tier enforcement | ✅ Running | public/priced/circle/embargoed with 451 for embargo |
| Rate limiting (100/min/operator) | ✅ Running | Per-operator HashMap with 60s window |
| Question → expansion pipeline | ✅ Running | Gap analysis → expansion queue → build |
| Navigate endpoint (question answering) | ✅ Running | `POST /pyramid/:slug/navigate` — search + LLM synthesis |
| BuildProgress event channels | ✅ Running | `mpsc::channel<BuildProgress>` throughout build pipeline |
| Search (FTS + embedding) | ✅ Running | `query::search()` in query.rs |
| DADBEAR staleness detection | ✅ Running | Sweep, detect, propagate, rebuild |
| Handle paths | ✅ Running | Universal addressing for deep links |
| Absorption modes | ✅ Running | `open`, `absorb-all`, `absorb-selective` |
| Wire discovery metadata | ✅ Running | `pyramid_metadata` contributions with tunnel_url |
| OpenRouter LLM client | ✅ Running | `llm.rs` with 3-tier cascade, rate limiting, retry |

### Key Patterns to Follow

**Route registration:** Routes in `pyramid_routes()` use a `route!` macro that boxes
each handler to `(Response,)` to avoid nested Either types. Follow the existing pattern:
define the route, then chain it with `.or(previous).unify().boxed()`.

**Auth filters:** Use `with_slug_read_auth()` for routes that need pyramid-level read
access with dual auth support. Use `with_read_auth()` for non-slug-scoped reads.

**DB access:** All DB reads go through `state.reader.lock().await` to get a `Connection`.
The `db` module has all the query functions. Don't write raw SQL in route handlers —
add query functions to `db.rs` if needed.

**HTML rendering:** There's no template engine in the project. Server-side HTML
generation should use Rust string formatting or a lightweight approach. Consider
`maud` (compile-time HTML macro) or just `format!()` with escaped content. Don't
add a heavy template engine — the HTML is structural, not complex.

---

## Absorption Economics (Already Implemented)

Three modes control who pays for question-driven expansion via the web surface:

| Code Enum | Human Name | Behavior |
|-----------|-----------|----------|
| `open` | Owner-absorbs | Pyramid operator pays compute, owns the result |
| `absorb-all` | Questioner-pays | Questioner owns the contribution; 35% royalty to operator |
| `absorb-selective` | Action-chain | Operator's agent evaluates each question and decides |

For V1 (anonymous web visitors without Wire identity), `open` is the default.
Questioner-pays requires Wire identity (email/magic link), which is V2 scope.

The question box on the HTML page should POST to `/pyramid/:slug/navigate` for
immediate answers, and separately trigger the expansion pipeline for gaps (the
existing infrastructure handles this).

---

## Verification Criteria

### Functional

1. `curl https://<tunnel-url>/p/<slug>` returns valid semantic HTML with the apex
   headline, topic list, and question box
2. `curl https://<tunnel-url>/p/<slug>/<node-id>` returns the node's full content
   with children, provenance, and navigation
3. Searching via `/p/<slug>/search?q=...` returns highlighted results
4. The question box submits and returns an answer (using the existing navigate endpoint)
5. Access tiers are enforced — priced pyramids show teaser only without JWT
6. The CSS aesthetic is recognizably "retro terminal" — monospace, dark, dense, box-drawing
7. A browser with JS gets the canvas layer with the ASCII aesthetic enhancement
8. WebSocket connection establishes through the tunnel and receives events
9. All HTML pages validate and work without JavaScript

### Performance

10. Semantic HTML pages render in <100ms server-side (it's just DB reads + formatting)
11. Pretext text measurement: <0.5ms for a typical node (500 words)
12. Canvas layer doesn't block initial HTML render (progressive enhancement)
13. WebSocket messages arrive at 60fps or better during active synthesis

### Accessibility

14. Each HTML page has a single `<h1>` with proper heading hierarchy
15. LayoutSans shadow semantic tree makes canvas content accessible to screen readers
16. Ctrl+F search works (LayoutSans) in the canvas layer
17. Text selection works in the canvas layer
18. Color semantics have sufficient contrast (WCAG AA minimum)

---

## Build Sequencing Suggestion

The team can re-sequence as they see fit, but here's a natural order:

**Hour 1: Foundation**
- Add the `/p/` route group to the warp server
- Implement `GET /p/{slug}` (pyramid home) and `GET /p/{slug}/{node_id}` (single node)
- Create the retro CSS file
- Verify it works through the tunnel

**Hour 2: Full Route Set + Aesthetic Polish**
- Implement remaining routes: tree, search, glossary, folio
- Wire the question box to POST to navigate
- Polish the CSS — this is where the aesthetic identity lives
- Test access tier enforcement on HTML routes

**Hour 3: Canvas Layer**
- Install Pretext + LayoutSans via Bun
- Build the client bundle
- Implement the canvas renderer with ASCII aesthetic
- Add `warp::ws()` handler forwarding BuildProgress events
- Verify WebSocket works through the tunnel

**Hour 4: Streaming + Art + Polish**
- Add OpenRouter SSE streaming (optional: `"stream": true`)
- Implement Mercury-2 ASCII art generation for banners/dividers
- Text flowing around art via `layoutNextLine()`
- End-to-end verification through tunnel
- Polish streaming animation effects

---

## Key Decisions Already Made

These are settled. Don't re-debate them:

1. **Two-layer architecture** — HTML foundation + canvas enhancement. Not "canvas only"
   or "HTML only". Both, concurrently.
2. **Pretext + LayoutSans** — Not DOM manipulation, not SVG, not WebGL. Canvas 2D with
   Pretext for measurement and LayoutSans for interaction.
3. **No p2p resilience** — Cross-pyramid links just use tunnel URLs. If the remote node
   is offline, the link is offline. The Wire handles coordination.
4. **No template engine** — Server-side HTML via Rust string formatting or a lightweight
   macro like `maud`. Keep it simple.
5. **Monospace retro aesthetic** — Not "modern minimal", not "material design", not
   "glassmorphism". Terminal-inspired, character-grid, BBS-aesthetic. Dense, precise,
   intentional.
6. **WebSocket via existing channels** — Don't build a new event system. Forward the
   existing `BuildProgress` mpsc events through `warp::ws()`.
7. **Mercury-2 for ASCII art** — It's a text LLM, not an image generator. It generates
   ASCII art as text output. Box-drawing characters, block elements, and tree connectors
   are high-reliability targets.

---

## What Success Looks Like

A visitor arrives at `https://<tunnel-url>/p/wire-platform`. They see a dense, dark,
beautifully typeset page with the pyramid's apex headline rendered as an ASCII art
banner. Below it: a table of contents showing topic areas with node counts and freshness
indicators. The typography is monospace, precise, and intentional. Box-drawing characters
frame content sections. Provenance metadata is visible on every block.

They type a question: "How does the prediction market work?" If the pyramid knows, the
answer renders immediately with cited node IDs. If it partially knows, it shows what it
has, marks the gap explicitly, and the gap enters the expansion queue. If the pipeline
runs live, they watch text materialize character by character — synthesis noise resolving
into final content, borders growing to accommodate, the knowledge surface literally
expanding before their eyes.

They can Ctrl+F to search the entire canvas. They can select text. Screen readers
announce the content. `curl` returns clean HTML. An agent gets the same knowledge through
the same URL. The same data, the same routes, two rendering layers serving every audience.

It looks like nothing else on the web. It looks like information infrastructure.
It looks retro because the original web got it right. It feels like the future because
no website has ever grown smarter from being visited.

---

*Handoff written: 2026-04-06*
*Author: Partner (strategic collaborator)*
*Recipient: Build team + audit team*
