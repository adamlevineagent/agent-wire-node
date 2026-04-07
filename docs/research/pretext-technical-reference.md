# Pretext Technical Reference for Pyramid Web Surface

> Research compiled 2026-04-06. Covers Pretext v1.x, LayoutSans v0.2, and the
> ASCII-aesthetic rendering stack for the post-agents-retro knowledge surface.

---

## Pretext: Core Facts

- **Package:** `@chenglou/pretext` — 15KB, zero dependencies, pure ESM TypeScript
- **Author:** Cheng Lou (React core team, ReasonML, Midjourney)
- **Released:** March 28, 2026 — 40.4k GitHub stars
- **License:** MIT
- **Install:** `npm install @chenglou/pretext` or `bun add @chenglou/pretext`

### How It Works

Pretext uses `canvas.measureText()` to access the browser's font shaping engine without
entering the DOM layout tree. Traditional DOM measurement (`getBoundingClientRect`,
`offsetHeight`) forces synchronous layout reflow — one of the most expensive browser
operations. Pretext never triggers reflow.

**Three-phase pipeline:**

1. `prepare(text, font)` — One-time: calls `canvas.measureText()`, caches character widths.
   Returns opaque handle.
2. `layout(prepared, maxWidth, lineHeight)` — Hot path: pure arithmetic greedy line-breaking.
   Returns `{ height, lineCount }`. Zero DOM access.
3. `layoutWithLines(prepared, maxWidth, lineHeight)` — Same but returns per-line data:
   `{ lines: [{ text, width, start, end }], height, lineCount }`

### Performance

| Scenario | DOM | Pretext | Speedup |
|----------|-----|---------|---------|
| 500 text blocks | ~47ms | ~0.09ms | ~500× |
| 1,000 text blocks | ~94ms | ~2ms | ~47× |
| 10,000 flex boxes | — | 4.82ms | 166× vs DOM |
| 100,000 variable-height | unusable | 46ms | ∞ |

### Key API: `layoutNextLine()`

`layoutNextLine(prepared, cursor, maxWidth)` computes one line at a time with a different
`maxWidth` per call. This enables text flowing around irregular shapes by computing
available horizontal width at each vertical position.

Community demos demonstrate: text flowing around an 80-segment dragon following the cursor,
physics-driven bouncing letters, animated orbs — all at 60fps with zero DOM reads per frame.

**For the pyramid surface:** ASCII art elements, box borders, and generated illustrations
become obstacles that text flows around, computed cheaply enough to update every frame.

### Caveats

- **Browser-only:** Requires `canvas.measureText()`. No server-side rendering without
  a canvas shim (`node-canvas`) or alternative (`cosmic-text` in Rust).
- **Whitespace modes:** Supports `normal` and `pre-wrap` only.
- **Font requirement:** `system-ui` unsafe on macOS — use named fonts.
- **Horizontal-only:** `prepare()` does horizontal work; `lineHeight` is layout-time input.

---

## LayoutSans v0.2: The Interaction Layer

Built directly on Pretext. Solves canvas searchability and accessibility.

### What It Provides

- **Canvas-based Ctrl+F** — Full-text search with highlighted matches and jump-to navigation
- **Text selection** — Click-drag, double-click word select, sub-glyph precision, shift-extend
- **Shadow semantic tree** — O(viewport) DOM nodes for VoiceOver, NVDA, JAWS compatibility
- **R-Tree spatial index** — Packed R-Tree for hit-testing 100k+ items at p95 < 0.5ms
- **Virtualization** — Only renders viewport-visible items, handles scroll efficiently

### Performance

- 100,000 variable-height items: 46ms render
- 10,000 flex boxes: 166× faster than DOM
- `queryPoint()` hit-testing: p95 < 0.5ms

### Why This Matters

LayoutSans eliminates the "canvas kills search/accessibility" objection. The pyramid web
surface can be fully canvas-rendered AND support Ctrl+F, screen readers, and text selection.

---

## Server-Side Rendering Options

Pretext cannot run server-side without a canvas shim. Options ranked:

| Approach | Viability | Tradeoffs |
|----------|-----------|-----------|
| **Semantic HTML from warp, Pretext as progressive enhancement** | ✅ Best | Two layers: HTML for curl/crawlers/agents, canvas for visual experience |
| `node-canvas` in Bun sidecar | Viable | Adds native binary dependency |
| `cosmic-text` in Rust | Viable for SSR | Full Rust font shaping — different API than Pretext |
| WASM compilation of Pretext | Not viable | Would need to reimplement `canvas.measureText()` in WASM |
| Pre-compute in browser, cache on server | Viable for static | Only works for content that changes infrequently |

**Recommended:** Semantic HTML from warp (Layer 1) + Pretext canvas (Layer 2, progressive enhancement).

---

## ASCII Art Generation via LLMs

### Quality by Category

| Category | LLM Reliability | Notes |
|----------|-----------------|-------|
| Box-drawing borders (`┌─┐│└─┘`) | **High** | Consistent, reliable |
| Tree characters (`├─`, `│`, `└─`) | **High** | Natural for hierarchy |
| Block fills (`░▒▓█`) | **High** | Good for density encoding |
| Decorative banners/logos | **High** | BBS art in training data |
| Scene illustrations | **Medium** | Needs reference examples |
| Complex diagrams | **Medium** | Better with seed sketch + refine |

### State of the Art

- **SVE-ASCII** (March 2026) — Fine-tuned model outperforming GPT and Claude on ASCII generation
- **ASCIIArt-7K + ASCIIArt-Bench** — Evaluation benchmarks for 5 quality dimensions
- Diffusion-based ASCII (experimental) — 32-channel probability vectors instead of pixels.
  Promising but no production-quality generator yet.

### For Mercury-2 Integration

Mercury-2 (unlimited, free diffusion LLM) can generate:
- Per-pyramid thematic banners from apex headlines
- Contextual topic dividers (not templated `═══════`)
- Structural diagrams from system descriptions
- Themed "synthesis noise" characters during streaming

---

## Streaming Text on Canvas

### Architecture

```
WebSocket (warp → client)
  → Token buffer (absorbs irregular chunk timing)
    → requestAnimationFrame drain loop (smooth 60fps)
      → Pretext re-layout per token (~0.1ms)
        → Canvas redraw (only dirty regions)
```

Key principle: drive animation from `requestAnimationFrame`, not network events.
Buffer absorbs irregular chunk arrivals; animation loop drains smoothly.

### Re-layout Cost

500-word node re-layout on token arrival: ~0.1-0.5ms (Pretext's `prepare()` caches
character widths; subsequent `layout()` calls are pure arithmetic).

20 simultaneous streaming nodes: 5-10ms total — well within 16ms frame budget.

### Demoscene-Inspired Effects

- **Falling characters reveal** — Characters resolve from random to final, column by column
- **Scan line fade-in** — Lines appear sequentially top to bottom
- **Phosphor glow pulse** — Brightness spike on answer arrival (CRT effect)
- **Wave-scroll entrance** — Text enters from edge and scrolls into position

All work with Pretext's per-line layout data: each `{ text, width, start, end }` line
is an individually animatable unit.

---

## Aesthetic Tradition Reference

### Key Influences

- **BBS ANSI art** — IBM CP437's 256-char set, box-drawing, block elements, shading gradients
- **Demoscene** — Scrollers, sine-wave text, character-cell plasma, full-screen ANSI animation
- **Charm.sh / Bubbletea** — "Terminal UIs from the future". Lipgloss styling, Elm architecture
- **cool-retro-term** — CRT aesthetic as a distinct visual language, not nostalgia
- **LazyVim / Neovim** — Terminal grid achieving visual richness competitive with graphical IDEs

### Design Principle

**Lean into the grid.** Misalignment and non-monospace mixing in a character-cell medium
looks broken. Embracing the grid — using it for rhythm, alignment, aesthetic structure —
transforms the constraint into an identity.

The constraint of the character grid gives the aesthetic a **language**. Dense knowledge
rendered in monospace, hierarchically indented with box-drawing connectors, animated like
a terminal receiving transmissions — that's not just retro, it's a visual argument that
the information was *synthesized*, not retrieved.

---

## Mobile Considerations

- Canvas elements > ~4500×3500px trigger Android compositing problems
- Mitigation: viewport-based virtualization via LayoutSans R-Tree
- Optional: 1× pixel density on mobile (softer text, smoother animation)
- Debounce resize events by 100ms, then reflow synchronously

---

## Community Resources

- **Pretext demos:** [chenglou.me/pretext](https://chenglou.me/pretext/)
- **Community demos:** [somnai-dreams.github.io/pretext-demos](https://somnai-dreams.github.io/pretext-demos/)
- **pretext.cool:** 17 community showcase demos
- **pretext.wiki:** Playground, font tester, snippets
- **LayoutSans:** Canvas text interaction engine built on Pretext

---

*Reference document for: docs/vision/post-agents-retro-web.md*
