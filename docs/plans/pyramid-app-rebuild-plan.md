# Agent Wire Pyramid App — Rebuild Plan

**Status:** Draft 0.2 — 2026-04-22
**Decision gate:** Not yet committed. Phase 0 (vocabulary closure) is the go/no-go.
**Key change from 0.1:** The operation vocabulary compressed from ten to four primitives. The Node substrate compressed from seven APIs to one uniform surface with seven target values. Design system + pretext integrated as load-bearing UI substrate.

---

## Executive summary

Pull the pyramid tool out of Agent Wire Node and rebuild it from scratch as a peer app, using the current codebase as a live reference and spec. The Node becomes pure substrate (compute market, corpus sync, Wire gateway, identity, credits). The pyramid app is born **filesystem-native** (`.understanding/` per folder as canonical state) and **contribution-native** (everything-is-a-contribution as a schema invariant, not an audit item) from line 1.

Three architectural initiatives collapse into this one rebuild: (1) the self-describing filesystem vision, (2) the everything-is-a-contribution retrofit that has been running for three weeks, (3) the Node/app separation that makes Vibesmithy a peer instead of a downstream consumer. Each was going to be a major engagement on its own. Doing them together, starting fresh, with a settled spec, trades three expensive retrofits for one well-specified new thing.

The kernel is small: **four primitives** (think, compute, observe, emit), **three orthogonal axes** (target, trigger, iteration), and **recursion via chain invocation**. Every current operation — evidence loops, DADBEAR, question decomposition, folder ingestion, market bids, supersession, web edges — reduces to a composition of the four. Phase 0 is the gate that proves this: one focused session enumerating every current operation and showing its closure. If anything can't close, the kernel isn't ready and the rebuild doesn't start.

Visually and mechanically, the app is "legitimately alive" — Agent Wire's night-first design system (from Claude Design's Phase 4 handoff) on top of `pretext` (DOM-free text measurement). The aha moment — watching the pyramid grow a new sub-question in real time — requires sustained continuous visual progress with zero layout thrash. That's what pretext unlocks and what DOM-native rendering can't deliver.

Current Node stays authoritative throughout the rebuild. The swap only happens in Phase 5 when the new app matches the old on every content type for a sustained parity window.

---

## The thesis

Three weeks of ripping Rust → YAML/Markdown has converted most of the pyramid tool to declarative form. But hardcoded pieces keep surfacing. The evidence loop in Rust is the canonical diagnostic: the single most central pyramid operation still isn't expressible in the IR vocabulary. That tells you the kernel is incomplete, not that the retrofit is almost done.

Retrofit is asymptotic. You get to roughly 90% contribution-native and the last 10% gets harder, because each remaining hardcode is tangled with assumptions the earlier conversions left behind. You don't reach "everything is a contribution" from this direction — you reach "most of it is a contribution, with sedimentary layers from each era of the conversion."

A rebuild with the current codebase as spec cancels entire categories of work:
- No IR-vs-legacy feature flags to maintain
- No `ConfigContribution + sync_to_operational` shadow tables to synchronize
- No DADBEAR-replaces-stale-engine cohabitation
- No central SQLite → `.understanding/` migration pass (never was central)
- No Node/app decoupling phase separate from the app rebuild (do both)

The cost is the rebuild itself. The offset is three retrofits that don't happen.

---

## Architectural invariants (the bones)

These are load-bearing from line 1. If any is violated later, it's a bug.

1. **Everything is a contribution.** Nodes, evidence, edges, deltas, threads, annotations, chains, prompts, schemas, configs, vocabularies, FAQs. One write path, one read path, one event model. No shadow hot tables, no `sync_to_operational` caches as separate truth. (Wire Pillars 1–6)

2. **`.understanding/` per folder is canonical state.** SQLite exists only as a derived cache, trivially rebuildable from files. Move a folder, understanding moves. Git a folder, understanding gits. The filesystem IS the pyramid. (Self-describing FS spec)

3. **One IR, one executor.** Any number of chain definitions compile to one IR (`ExecutionPlan`). The executor runs IR. Every build, every pyramid operation, every DADBEAR tick, every cross-pyramid web edge is a chain execution. No parallel code paths. (Wire Pillar 18, Node Law 1)

4. **Four primitives, three axes.** Every chain step does exactly one of: `think` (invoke intelligence), `compute` (deterministic computation), `observe` (read state), `emit` (write state). Every step additionally carries a **target** (where), a **trigger** (when), and an **iteration mode** (how many). Nothing else is a primitive. (Compressed from the ten-operation canonical vocabulary.)

5. **Chains invoke chains. No special orchestrators.** If a workflow needs multiple steps, it's a chain. If a workflow invokes another workflow, it uses an action reference. There is no "coordinator module," "supervisor," or "dispatcher" that exists only in Rust. (Wire Pillar 17)

6. **Recursion IS the architecture.** Pyramids are recursive. DADBEAR is recursive. Question decomposition is recursive. Folder nodes nest recursively. `.understanding/` lives inside every folder recursively. Chains invoke chains recursively. The same primitive operates at every layer. Recursion is never a special case. (Node Law 2 + Wire Pillar 17)

7. **Local is local, Wire is Wire — peers.** Local contributions live in `.understanding/`. Wire-published contributions also exist on the Wire. Same schema, same identity. Sync is explicit and bidirectional. Neither is primary. (Wire Pillar 31)

8. **Never prescribe outputs to intelligence.** No hardcoded ranges, quotas, counts, thresholds that constrain LLM output. Every number is either operational config (a contribution) or absent. (Wire Pillar 37, Node Law 5)

9. **Loud deferrals, never silent.** Stubs raise exceptions. A not-yet-implemented code path fails visibly the first time the branch runs. No `NULL; -- TODO:` bombs.

---

## The kernel — four primitives, three axes, recursion

Operations reduce to **four**:

| Primitive | What it is | Why irreducible |
|-----------|-----------|-----------------|
| **`think`** | Invoke intelligence — an LLM, any model, any synthesis primitive | "Delegate to intelligence" is atomic; you can't compose it from simpler acts |
| **`compute`** | Deterministic computation — transforms, mechanical functions, AST, crypto, math | The opposite pole of intelligence; free, no derived_from, Pillar 20 |
| **`observe`** | Read state — filesystem, `.understanding/`, DB, Wire, event stream, another pyramid | Reading is atomic; splitting from write makes each step's effect unambiguous |
| **`emit`** | Write state — contribution, event, dispatch, message, supersession | Writing is atomic; `dispatch`/`wire.contribute`/`event.publish`/`task.post`/`game.start` are all `emit` with different targets |

The prior canonical vocabulary had ten operations (`llm`, `wire`, `task`, `game`, `transform`, `mechanical`, `time`, `observe`, `event`, `dispatch`). Six of those collapse into axes on the four:

- `dispatch` → `emit` with `target ≠ local`
- `time` → `trigger: schedule[cron]`
- `event` → `trigger: on-event[filter]`
- `wire` → target pattern (any `wire.*` call is `emit`/`observe` with `target: wire`)
- `task` → chain: `emit[target: task-board]` + `observe[on-event: completion]`
- `mechanical` → `compute` with `fn: registered-function`

### Three orthogonal axes

| Axis | Values | What it answers |
|------|--------|-----------------|
| **target** | `local`, `node`, `wire`, `peer[id]`, `fleet[policy]`, `market[policy]` | Where does this happen? |
| **trigger** | `now`, `event[filter]`, `schedule[cron]`, `condition[expr]` | When does it run? |
| **iteration** | `single`, `parallel[over, concurrency]`, `sequential[over, accumulate]` | How many times? |

`map`/`reduce`/`filter` are not primitives: map is `parallel`, reduce is `sequential + accumulate`, filter is a `when` gate or a `compute` transform. Iteration wraps the operation, not the other way around.

### Recursion

Any step may invoke a chain by ID. That invocation is itself an `emit` to the executor with a chain reference. Chain-invokes-chain is how recursion expresses itself: DADBEAR walks parent pointers by re-invoking itself on the parent; question decomposition re-invokes itself on each sub-question until `apex_ready`; folder trees walk themselves by invoking the build chain on each child folder.

### One uniform step shape

```yaml
step:
  do:      think | compute | observe | emit
  where:   local | node | wire | peer[id] | fleet[policy] | market[policy]
  when:    now | event[filter] | schedule[cron] | condition[expr]
  many:    single | parallel[over, concurrency] | sequential[over, accumulate]
  gate:    <predicate expression>
  input:   {...}
  output:  <name>
  # do-specific fields:
  prompt:  ...              # for think
  fn:      ...              # for compute (14 transforms + registered functions)
  select:  ...              # for observe
  payload: ...              # for emit
  invoke:  <chain-id>       # optional: this step IS a chain invocation
```

Every operation in the system — every contribution write, every LLM call, every file read, every market bid, every DADBEAR tick, every folder scan — uses this shape. No alternate forms. No special constructors. The uniform surface is the composability property.

### Provenance and economics stay load-bearing

The `think`/`compute` split isn't just mechanical. `think` costs money (LLM calls, payable through Node's compute dispatch and rotator arm); `compute` is free (Wire Pillar 20). `think` outputs need `derived_from` for attribution and royalty flow (Pillars 3, 7); `compute` outputs don't. Don't collapse to three primitives — the split is economically and provenantially load-bearing.

---

## Phase 0 — Vocabulary closure (the gate)

**Before any rebuild work happens.** One focused session.

Enumerate every operation the current pyramid system performs. For each, decide:

- **Primitive** — exactly one of `think`, `compute`, `observe`, `emit`
- **Axis values** — target, trigger, iteration
- **Composition** — if the operation is multi-step, show the chain
- **Named escape** — if an operation requires Rust, register a `mechanical` function with a contract
- **Gap** — can't close; kernel needs extension

**Forcing function:** the current codebase. Walk the recipe primitives and every hardcoded Rust path. Each must close.

### Known targets to close

| Current hardcode | Closure |
|---|---|
| `evidence_loop` (Rust recipe primitive) | `sequential` over layers × `parallel` over batches × `compute: group_by_token_budget` × `think: triage`. A chain. |
| `recursive_decompose` | Recursive chain: `think: decompose` → `parallel` over sub-questions → `invoke: self` with `apex_ready` `condition` |
| `process_gaps` | Chain triggered on `event: missing_verdict` → `think: identify_source` → `emit: demand_signal` |
| `cross_build_input` | `observe: prior_build_state` at chain start |
| `build_lifecycle` | A chain, not a primitive: `compute: validate` → `think: budget_preview` → `emit: dispatch` → `observe: results` → `emit: apply` |
| DADBEAR supervisor | A chain, firing on `event: filesystem_change OR event: wire_event` → `observe: hash` → `think: stale?` → `emit: new_version` → `invoke: self` on parent |
| Per-slug RwLock | Filesystem lockfile for local; Node-side lease primitive (`emit[target: node, op: lease]`) for cross-machine |
| Market quote/purchase/fill | `emit[target: market, op: quote]` + `observe[on-event: settlement]` |
| Per-model compute queue | `emit[where: node, mode: queued]` — queueing is a target policy, not a distinct step |
| Supersession | `emit` with `supersedes: prior_id` payload field |
| UFF / rotator arm | Substrate mechanics underneath `emit[target: wire]` — invisible at the primitive layer |

### Output of Phase 0

A single doc: `docs/architecture/operation-vocabulary-v1.md`. Lists every current operation, names its primitive, axis values, composition if any. If any operation can't close in a single session, the kernel isn't ready — **stop**, finish the IR, retry Phase 0 next session. If every operation closes, Phase 1 starts.

---

## Shape of the Node (substrate) after separation

The Node's external API collapses to one uniform surface: every external request is an **`observe` or `emit`** against a target. The prior "seven primitives" are **seven target values** on that one surface, not seven separate APIs.

### The target surface

| Target | What lives there | What pyramid-app + Vibesmithy + future peers do with it |
|---|---|---|
| `node:identity` | Wire identity, auth, tokens, routing | `observe` identity; `emit` sessions |
| `node:compute` | Compute market, fleet dispatch, market daemon, per-model queue | `emit` dispatch (with op + model + payload + policy); `observe` status; stream results |
| `node:corpus` | Shared source documents (`wire_source_documents`), backfill paths | `observe` by doc ID; `emit` sync with path-to-hash |
| `node:contributions` | Wire-published contributions only (local lives in `.understanding/`) | `emit` new contribution (with supersedes if updating); `observe` by filter |
| `node:events` | Network event stream (Wire contributions, market settlements, discovery) | `observe` with filter (streams) |
| `node:discovery` | Slug index, Wire-nodes topic index | `observe` catalogs; `emit` topic queries |
| `node:credits` | Credit balance, rotator-arm accounting, achievements | `observe` balance; `emit`s on compute auto-debit through `node:compute` |

The substrate doesn't "expose APIs." It exposes **state surfaces** that apps `observe` and `emit` against, using the same uniform step shape the apps use internally. That is what it means for Node-primitives-as-axis-values to hold.

### What moves out of Node

Everything pyramid-specific leaves:
- Local pyramid storage (nodes, evidence, edges, deltas, threads)
- Chain executor
- DADBEAR
- Delta/crystallization engine
- Evidence-weighted answering
- Partner conversation loop
- Folder ingestion
- Pyramid query layer
- `.understanding/` reader/writer
- Pyramid-specific MCP tools

### What the Node keeps (it IS these things)

- Wire identity, auth, tunnel, token lifecycle
- Compute market, ComputeQueue, Fleet Dispatch, Market Daemon, DispatchPolicy
- CreditTracker, rotator arm, cost model, prompt cache (as service)
- Wire publication plumbing, wire pull, wire import, wire discovery
- Tunnel, HTTP server, operator routes for substrate ops

Node becomes a **headless network gateway** — no Tauri desktop UI. Market/fleet/credit/wire features stay because those are the Node.

---

## Shape of pyramid-app

A filesystem-native local-first app that talks to a Node for compute and network.

### Storage — `.understanding/` per folder

Canonical state. Layout per the self-describing filesystem spec:

```
any-folder/
├── (regular files — code, docs, whatever)
└── .understanding/
    ├── folder.md                # filemap (the folder node itself)
    ├── nodes/
    │   ├── {node-id}/
    │   │   ├── v1.md
    │   │   ├── v2.md
    │   │   ├── current → v2.md  # symlink
    │   │   └── notes/
    │   │       └── v1-to-v2.md  # refinement note
    ├── edges/
    │   └── web-edges.jsonl
    ├── evidence/
    │   └── links.jsonl
    ├── configs/
    │   └── {policy-type}/       # per-folder config contributions
    ├── conversations/           # Claude Code + agent sessions co-located
    │   └── 2026-04-22-morning.jsonl
    ├── contributions/           # local contribution log (before wire publish)
    ├── cache/                   # content-addressable step cache (rebuildable)
    │   └── llm-outputs/
    └── .lock/                   # filesystem coordination
```

Derived SQLite cache rebuildable at any time from files. Never the source of truth.

### Execution — one IR executor

- Runs any valid `ExecutionPlan` (four primitives + axes + recursion)
- Every step local (`target: local`) or dispatched to Node (`target: node:compute`)
- `StepContext` content-addressed cache per step (sha256 of inputs × prompt × model)
- Resume on crash: build state is itself a contribution in `.understanding/contributions/`; resume reads and continues

### DADBEAR — a chain, not a supervisor

```
on event: filesystem_change OR event: wire_event
  → observe: hash of changed node
  → think: is this materially changed?
    → if no: emit: log, done
    → if yes: emit: new_version to .understanding/nodes/{id}/v{n+1}.md
             invoke: self on parent node
```

Holds/projections/breaker/timers are chain patterns composed from `observe` + `think` + `compute` + `emit` + `trigger: schedule`. If a specific pattern needs tight-loop performance and a registered `compute` function, fine — register it as a `mechanical` function with a documented contract.

### Event streams — two, one supervisor

- **Local:** filesystem watcher on the working directories → local event bus
- **Network:** `observe[target: node:events, trigger: on-event]` → Wire event bus

DADBEAR-as-chain subscribes to both. Same chain handles both.

### Network layer — uniform substrate talk

Everything the app does against the Node uses the same uniform step shape:
- Publish to Wire: `emit[target: node:contributions]` with local contribution payload
- Read from Wire: `observe[target: node:contributions]` or `[target: node:events]` for streams
- Compute dispatch: `emit[target: node:compute]` (local Ollama vs remote market is Node policy)
- Discovery: `observe[target: node:discovery]`

No hand-written API client per surface. One step shape, seven target values.

### UI — Tauri shell + React + pretext

- **Tauri shell** — keeps Rust bindings (identity, filesystem, system integration)
- **React + Vite** — component structure, doesn't fight Tauri
- **`@pyramid-app/render`** — thin library wrapping pretext for text measurement
- **Canvas/SVG** — dense surfaces (pyramid tree, spatial view, build viz)
- **DOM** — forms, dialogs, onboarding chrome
- **Design tokens** — from `colors_and_type.css`, imported once, extended nowhere

### Distribution

- Rust library + headless daemon + Tauri UI shell — one backend, multiple consumers
- Vibesmithy embeds the library for local pyramid reads
- Headless mode serves MCP + HTTP for agents

---

## Data model — what lives in `.understanding/`

One folder, one `.understanding/` directory, one self-contained unit of local state.

### File formats

| Type | Format | Why |
|------|--------|-----|
| `folder.md` (filemap) | YAML frontmatter + markdown body with checkbox list | Human-editable; scanner-owned vs user-owned field split |
| `nodes/{id}/v{n}.md` | YAML frontmatter + markdown body | Human-editable; diffable; git-friendly |
| `nodes/{id}/notes/v{n-1}-to-v{n}.md` | Plain markdown | Refinement narrative |
| `edges/web-edges.jsonl` | JSONL | High-volume, append-mostly, grep-able |
| `evidence/links.jsonl` | JSONL | Same |
| `configs/{type}/*.yaml` | YAML contributions | Per-folder policy (evidence, dadbear, etc.) |
| `conversations/{date}.jsonl` | JSONL | Standard chat transcripts, co-located |
| `contributions/*.json` | JSON | Local contribution log before wire publish |
| `cache/llm-outputs/*.bin` | Binary | Content-addressable, rebuildable |

### Scanner-owned vs user-owned fields (filemap)

Scanner writes: `path`, `size_bytes`, `mtime`, `sha256`, `detected_content_type`, `detected_inclusion`, `built_as_pyramid_node`, `last_build_at`, `last_build_error`. Scanner re-scans touch only these.

User writes: `user_included` (tri-state: null/true/false), `user_content_type`, `user_notes`. Scanner never touches these.

New file on disk: added with `user_included: null` (not curated). Does NOT auto-include.
Deleted file on disk: moves to `deleted:` tombstone list with last-known state.
Renamed file: treated as delete + add for v1. Sha256-based rename detection is v2.

### The five uncovered categories (closed enum)

1. `excluded_by_pattern` — matched an ignore rule
2. `excluded_by_size` — over size threshold
3. `excluded_by_type` — known binary/system
4. `unsupported_content_type` — no extractor yet (the "add new extractor" TODO list)
5. `failed_extraction` — tried, failed (persistent record instead of scrolled log line)

### Supersession as versioned directories

Every node has a directory. New version = new file. `current` symlink points to active. `notes/v{n-1}-to-v{n}.md` explains the transition. Walk the versions backward to understand how the node evolved. `git log` works. `git bisect` works. `diff v2.md v3.md` works.

### Schema evolution

Each file carries a `schema_version` in its frontmatter. App upgrade walks and migrates files. Standard file-format evolution story.

### Inheritance between folders

Parent folder's filemap may set `children_default: include | skip | unchecked` and `children_ignore_patterns: [...]`. Inheritance is always additive — a parent adds exclusions, never removes them. A child can override `children_default` locally.

### Cross-machine sync

Git handles it well for opt-in users. rsync for others. Out of scope for v1 — filesystem-local is enough.

---

## UI substrate — Agent Wire design system + pretext

### Design system (canonical)

Lives at `questionpyramidsstandalone/project/`. Night-first (midnight blue + warm cream + amber), every token traceable to the Agent Wire app icon. Full spec at [questionpyramidsstandalone/project/README.md](../../../questionpyramidsstandalone/project/README.md).

**Copy unchanged into pyramid-app:**
- `colors_and_type.css` — all tokens (ground/ink/mark/jewels/glow/type/spacing/motion)
- Font stack: Fraunces (display + italic variable axes) / Newsreader (body) / JetBrains Mono (instrumentation). No substitutes.
- Layout grid: `--wrap: 1180px`, `--wrap-wide: 1380px`, 56/24px padding, 88–96px section padding
- Reading measures: 66ch/58ch/52ch/46ch — per-component, not global breakpoints
- **Zero corner radii.** Sharp corners everywhere except traffic lights, status dots, particles

**Voice rules (enforced in copy lints):**
- Second person always. Lowercase first word in small copy. One italic per sentence max.
- Mono for instrumentation only (folio, step counts, timestamps). Never body.
- No emoji. No exclamation marks. Contractions encouraged.
- Only glyphs: asterism `❋`, dice `⚄`, Fraunces italic arrow `→`, Fraunces italic checkmark `✓`, house `⌂`

**Brand primitives (four, each semantic):**
- **Icon** — the canonical app icon, never redrawn
- **Starburst** — fires *once per pyramid's lifetime* (first settle). Apex-small as top-of-pyramid marker otherwise
- **Wave** — cyan sine interference, background only, phase-shifts when peers are contributing. Never in working UI
- **Particles** — jewel-colored dots tied to node activity. Amethyst = new question, ruby = conflict, topaz = writing, citrine = settling, pearl = settled. **Never generic sparkle**

**Motion primitives (five):**
| Name | Duration | Used for |
|------|----------|----------|
| Pulse | 1.4s | Dot/status — live, architecting, now writing |
| Blink | 1.1s | Cursor bar |
| Ellipsis | 1.4s | `now writing···` suffix |
| Fade | 1.8s | In-flight node, uncommitted question |
| Shimmer | 3s | Crystallizing answer, pre-settle |

No spring/bounce. Hover ≤ 140ms. No scroll-jack, no parallax.

### The six-window onboarding (first ten minutes)

Lives at `questionpyramidsstandalone/project/index.html`. Canonical. Implement faithfully during Phase 2–3.

| Step | Window | Skippable | Load-bearing property |
|------|--------|-----------|----------------------|
| i | **Handle** — auto-assigned three-word (`@river-in-autumn`), dice reroll | no (~6s) | Every user reaches step vi with an identity |
| ii | **Folder** — drag/pick local dir, or skip (docs pyramid ships) | yes (~15s) | Shipped docs pyramid means step iv ALWAYS has something to answer |
| iii | **Compute** — multi-select: OpenRouter key / Ollama / co-op (starter credits on by default) | effectively (~30s) | Zero is fine to start; co-op carries |
| iv | **Aha** — ask the docs pyramid, watch the tree grow a new sub-question live | no (the point) | **This one screen carries the product in a screenshot** |
| v | **Agents** — paste MCP block, tick skills (3 pre-checked) | yes (~25s) | Loud skip; defaults ship "ask the pyramid" on |
| vi | **Welcome** — three cards (node/pyramids/compute), CTA lands on the docs pyramid where the step-iv sub-question just materialized | — | **Continuity:** the weird question they asked two minutes ago is still there on the pyramid |

The **"new, just created to hold this answer"** mark in step iv is the conversion moment. Every design choice in that window exists to make that moment land.

### Pretext — the "alive" substrate

[github.com/chenglou/pretext](https://github.com/chenglou/pretext) — pure JS multiline text measurement + layout without DOM reflow.

- **Prepare** once: normalize, segment, apply break rules, measure on canvas → opaque `PreparedText` handle
- **Layout** cheaply, repeatedly: pure arithmetic over cached widths → height/lineCount/lines at any container width

Ship as the text measurement substrate for every surface that renders node content. `@chenglou/pretext`. Wrap in a thin `@pyramid-app/render` library with:

```
measureNode(content, font, width) → LayoutResult
measureContribution(c, width) → LayoutResult       // cached by content hash
prepareRemote(c) → Promise<PreparedText>            // server/worker side
```

### Why pretext + this design = legitimately alive

The design asks for a surface where nodes arrive continuously, three visual states coexist per node, trees grow visibly, hundreds of nodes stay 60fps, and per-component reading measures recompute on resize. None of that holds on DOM-measure-every-frame.

1. **Continuous contribution streams don't thrash** — `prepare` off-thread, `layout` at current width is arithmetic. No `getBoundingClientRect`, no reflow
2. **Virtualization with exact positions** — thousands of nodes, a visible window of 30. Jump-to-node, minimap, breadcrumb-in-context all work
3. **Three coexisting states don't fight the browser** — pulse/blink/ellipsis/fade/shimmer are state flips on known geometry
4. **Canvas/SVG becomes viable where DOM can't keep up** — spatial views (Vibesmithy, pyramid minimap, cross-pyramid web) render to canvas with per-node layout data
5. **Server-side layout prediction** — remote contributions arrive with pre-computed pretext handle → instant positioning on receive
6. **Per-component reading measures are free** — same prepared handle, four layouts, pure arithmetic
7. **The aha-tree in step iv** — new sub-question arrives → prepare → layout → slot in → amber pulse → siblings shift via transform. The "growing" feeling requires sustained continuous visual progress with zero reflow cost. Pretext is the only way
8. **Handle reroll is instantaneous** — new three-word handle at handle-card width, zero layout flash even with word-length variance
9. **Compute-grid selection has no layout flash** — all three card heights prepared once; select is a class flip
10. **DADBEAR cascades show work visibly** — supersession tracing through N citing nodes is a wave of state transitions, not a layout thrash

**Net:** pretext isn't performance polish. It's the substrate without which the design's specific promises ("feels like watching bread rise") can't be cashed.

---

## Recursion — the unifying lens

Recursion is invariant #6 but deserves a dedicated section because it recurs (fittingly) in every other section.

| Where recursion lives | What recurses | Primitive |
|----------------------|---------------|-----------|
| Pyramid shape | Vines of vines of folders of files; any node can contain another pyramid | `invoke` |
| DADBEAR | Chain invokes itself on parent after rewriting a node; stops when nothing else changed | `invoke` + `condition: apex_ready` |
| Question decomposition | Apex question → sub-questions → further sub-questions; stops when chain returns `apex_ready` | `invoke` + `parallel` over children |
| Folder nodes | Folder's filemap references child folders, each with its own filemap | `observe[target: local]` walks |
| Chain invocation | Chains invoke chains by ID; no orchestrator sits above them | `invoke` |
| `.understanding/` | Every folder has its own `.understanding/`; children inherit but can override | Filesystem walks |
| Supersession | New version points to prior; walking backward is recursion through symlink history | Filesystem walks |
| Cross-pyramid web | A web edge is a contribution in one pyramid pointing to a node in another; the other pyramid has its own web edges | `observe[target: node:contributions]` with filters |
| Counter-pyramids | Contest a claim by opening a new pyramid underneath it; counter-pyramids can have counter-counter-pyramids | `emit` new pyramid contribution |

**Single mechanism throughout:** an `invoke` step that takes a chain ID, with termination expressed as a `condition` gate. The same mechanism handles DADBEAR's "nothing more changed," question decomposition's "apex ready," and pyramid-within-pyramid's nested build. No layer-specific recursion logic. No supervisor that knows about depth. The recursion stack IS the pyramid's parent pointers, or the folder tree, or the decomposition tree — whichever is relevant.

---

## Phases

Each phase produces a shippable artifact. Current Node runs authoritative throughout.

### Phase 0 — Vocabulary closure (the gate)
**One focused session.** Prove every current operation closes to the four primitives + axes + recursion. Output: `docs/architecture/operation-vocabulary-v1.md`. Go/no-go gate.

### Phase 1 — Node API contract
- Write the substrate surface spec as `docs/architecture/node-api-contract.md`
- Define the seven target values, the step shape, the observe/emit semantics
- Stand up a pass-through shim in current Node that exposes the surface against internal state
- Vibesmithy and future pyramid-app both talk through it
- **Deliverable:** current Node speaks the substrate surface; nothing else changes

### Phase 2 — Pyramid-app bones
- `.understanding/` reader/writer with schema
- Derived SQLite cache (pure index, rebuildable)
- IR executor (four primitives + axes)
- `StepContext` cache
- Filesystem watcher + local event bus
- DADBEAR-as-a-chain (one canonical chain file)
- Talks to Node through the substrate surface
- `@pyramid-app/render` shipped, design tokens imported, six Tauri window containers stubbed
- **Deliverable:** pyramid-app can build a trivial code pyramid end-to-end from a local folder, store results in `.understanding/`, render them through pretext

### Phase 3 — Content-type chains (parity bar)
Port existing chains from `chains/defaults/` and prompts from `chains/prompts/`, **unchanged**:
- Code pyramid
- Document pyramid
- Conversation pyramid
- Question pyramid
- Vine / vine-of-vines

Parallel UI workstream implements the docs-pyramid surface (Phase 4 design's step vi destination). Content live, not mocked.

**Parity bar:** pyramid-app produces comparable output to current Node on the same corpus, content-type by content-type. Diff and verify.
**Deliverable:** pyramid-app matches current Node on all five content types.

### Phase 4 — Folder-node filesystem + onboarding completion
- `.understanding/folder.md` as canonical folder node
- Scan writes filemap; user curates (editor or UI); build reads checklists
- Five uncovered categories + deleted tombstones from day one
- Inheritance via `children_default` / `children_ignore_patterns`
- Conversations co-located in `.understanding/conversations/`
- Supersession as `.understanding/nodes/{id}/v{n}.md` with `current` symlink
- Complete the six-window onboarding (handle/folder/compute/aha/agents/welcome) with real backend wiring
- **Deliverable:** move a folder anywhere, understanding moves with it. Git-commit `.understanding/`, understanding versioned with code. Onboarding ships.

### Phase 5 — Swap
- Minimum 2-week side-by-side operation with real corpora before swap
- Diff outputs; confirm chain outputs match
- Only swap when parity holds for a week with no regressions
- Pyramid-app becomes canonical
- Node strips pyramid code (executor, DADBEAR, `.understanding/` writer, content-type chains, partner loop, folder ingestion, evidence answering, delta engine, pyramid query, pyramid-specific MCP tools)
- Legacy tables retained as archive for rollback safety; dropped after N months of stable pyramid-app
- **Deliverable:** two small binaries where there was one large one. Vibesmithy has a proven path. Node is a headless substrate.

---

## Cannibalize / keep / rebuild split

### Keep (port unchanged)
- All prompts in `chains/prompts/**/*.md` — proven IP
- All chain YAMLs in `chains/defaults/*.yaml` — augmented where Phase 0 surfaces new primitives
- DB schema patterns — for derived cache only, not canonical
- MCP tool definitions — pyramid-specific tools move to pyramid-app; substrate tools stay in Node
- Provider registry, vocabulary, schema registry, extraction schema generator
- Reading modes (memoir, walk, thread, decisions, speaker, search)
- Reconciliation, triage, demand-signal logic — as chains
- Design tokens from `questionpyramidsstandalone/project/colors_and_type.css`
- Agent Wire app icon

### Keep in Node
- Wire identity, auth, tunnel, token lifecycle
- Compute market, ComputeQueue, Fleet Dispatch, Market Daemon, DispatchPolicy
- CreditTracker, rotator arm, cost model, prompt cache (as service)
- Wire publication plumbing, wire pull, wire import, wire discovery
- Tunnel, HTTP server, operator routes for substrate ops

### Rebuild (from vocabulary + spec)
- Executor (one, per Phase 0 vocabulary)
- Storage layer (filesystem-native `.understanding/`)
- DADBEAR (as a chain)
- Event bus (filesystem watcher + Wire subscription)
- Folder ingestion (scan → filemap → curate → build)
- Partner loop (as chains using the vocabulary)
- `@pyramid-app/render` — pretext wrapper + design token consumer
- Six-window onboarding UI

---

## Decisions

Each gets a plain-chat confirmation, no ceremony. Recommendations attached.

**D1. Is pyramid-app a separate binary?**
Rust library + headless daemon + optional Tauri UI shell. Vibesmithy embeds the library; headless serves MCP/HTTP; UI shell is one consumer among many. Same backend.

**D2. Do local contributions that get published to Wire exist twice?**
No. Same contribution schema, written locally first (to `.understanding/`), published to Wire on demand. Pillar 31. A contribution has a location; location is not its identity.

**D3. DADBEAR timers/batching/breaker — chain patterns or substrate?**
Chain patterns built on `observe` + `think` + `compute` + `emit` + `trigger: schedule`. Register `compute` functions only if a pattern genuinely needs tight-loop Rust.

**D4. Does the Node keep a Tauri desktop UI?**
No. Node becomes headless after Phase 5. The current Tauri app becomes pyramid-app's UI shell.

**D5. Parity bar timeline.**
Minimum 2 weeks side-by-side on real corpora before Phase 5 swap. Diff outputs. Confirm chain outputs match. Only swap when parity holds for a week with no regressions.

**D6. Where does the chain compiler live?**
Shared crate (`wire-chain-compiler`). Both pyramid-app and Node need it — Node for verifying published chains, pyramid-app for executing them.

**D7. Onboarding lives inside pyramid-app or Node shell?**
Inside pyramid-app. Node is headless; onboarding is a pyramid-app concern. First-run shows onboarding; subsequent launches skip to docs pyramid or last-viewed.

**D8. Docs pyramid — embedded or downloaded on first run?**
Embedded in the app bundle as pre-computed `.understanding/` content. Step ii skippable only works if docs pyramid is instantly available. Download breaks the ten-minute promise.

**D9. `@pyramid-app/render` — library or service?**
Library, in-process. Service adds IPC cost per measurement; pretext is fast in-thread. Server-side `prepareRemote` is the only cross-process case.

**D10. Three-word handle pool.**
Curated pool (~10k entries) shaped by nature/season + object/adjective + verb/place combos. Example: `@river-in-autumn`. Open question from the design doc — locked here unless Adam says otherwise.

---

## Execution principles

How we build, not what we build. Apply throughout.

- **One agent per focused task.** Don't combine audit surfaces. Minimizing agent count is false economy.
- **Serial verifier after implementation.** Second agent audits with fresh eyes, fixes in place. Then a wanderer with no punch list — just "does this actually work?"
- **Audit until clean.** Keep running audit cycles until auditors return no critical/major findings. Plan must be complete before building.
- **Fix all bugs when found.** No cleanup lists. No tech debt deferral. If broken, it goes in a workstream.
- **Frontend in every workstream.** Every backend feature gets a UI surface in the same phase. Adam tests by feel, not by curl.
- **No worktrees.** Isolate parallel agents by concerns/files. Same-doc edits go serial.
- **Always test dev end-to-end before committing.** Compiles ≠ works. Launch dev mode.
- **Describe functional impact, not technical detail, in deferrals.** "File renames lose annotations" beats "source_document_id deferred."
- **Live state in memory first.** For UI progress/current-step, the in-memory handle is canonical. Don't aggregate from DB without checking handles first.
- **Contributions all the way down.** If it feels like it isn't a contribution, it probably is.

---

## What NOT to do

- **No big-bang.** Current Node runs authoritative throughout. Swap only at Phase 5 on parity.
- **No new content types during rebuild.** Scope is parity. New content types land after swap.
- **No touching prompts during Phase 0–2.** They're proven. Port unchanged.
- **No premature optimization of the SQLite derived cache.** Start dumb; rebuildable.
- **No breaking the Wire contribution schema.** Pyramid-app's Wire-published contributions must be byte-compatible with current Node's. Interop comes free.
- **No silent deferrals.** Raise exceptions, not TODO nulls.
- **No emoji, no exclamation marks, no SaaS signup voice in UI copy.** See design voice rules.
- **No "let's merge think and compute."** The split is economically and provenantially load-bearing.
- **No hardcoded numbers constraining LLM output.** Pillar 37 / Node Law 5.

---

## What this buys you

- **Everything-is-a-contribution becomes a schema invariant, not an audit item.**
- **Three weeks of retrofit work collapses** into "port prompts + chain YAMLs, rebuild executor + storage + DADBEAR to spec." The prompts and chains are the expensive IP and carry forward intact.
- **Self-describing filesystem, contribution-native, wire-native, node-substrate-separated — all at once.** Three initiatives share one rebuild.
- **Vibesmithy has a tested path.** Same substrate surface, same `.understanding/` read.
- **The frankenstein dissolves.** No IR-vs-legacy feature flags. No `ConfigContribution + sync_to_operational` shadow tables. No DADBEAR-replaces-stale-engine cohabitation. No hardcoded recipe primitives.
- **Debugging becomes `cat` and `grep`.** Open `.understanding/folder.md`. Read `.understanding/nodes/X/current`. Diff `v2.md` vs `v3.md`. See `notes/v2-to-v3.md` for why.
- **The UI is legitimately alive, not polish-alive.** Pretext makes continuous visual progress mechanically cheap. The design's specific promises — bread rising, watching it happen, turning pages in a quiet book — are achievable, not aspirational.
- **Vocabulary is bounded and small.** Four primitives. Three axes. Recursion. Anyone reading a chain YAML can hold the whole surface in their head.

---

## Open questions

1. **Phase 0 timing.** When does vocabulary closure happen? Recommendation: next focused session.
2. **Rename detection in filemap.** V1 treats rename as delete + add. V2 could sha256-match. Decide before Phase 4.
3. **Cross-machine `.understanding/` sync.** Git for opt-in; rsync for others. Out of scope for v1.
4. **Counter-pyramid UI.** Design system mentions them; no Phase 4 artifact shows them. Either surface design them during Phase 3 or defer to post-swap.
5. **Persona LoRA integration.** Future hook: `think` outputs → compute market training jobs → new model contribution. Out of scope for v1 but the primitives support it.

---

## References

- [docs/vision/self-describing-filesystem.md](../vision/self-describing-filesystem.md) — target storage architecture
- [docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md](../handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md) — filemap pivot, decisions already made
- [questionpyramidsstandalone/project/README.md](../../../questionpyramidsstandalone/project/README.md) — Agent Wire design system
- [questionpyramidsstandalone/project/index.html](../../../questionpyramidsstandalone/project/index.html) — Phase 4 onboarding design (canonical)
- [questionpyramidsstandalone/project/colors_and_type.css](../../../questionpyramidsstandalone/project/colors_and_type.css) — design tokens
- [github.com/chenglou/pretext](https://github.com/chenglou/pretext) — rendering substrate
- [GoodNewsEveryone/docs/architecture/pyramid-chain-convergence.md](../../../GoodNewsEveryone/docs/architecture/pyramid-chain-convergence.md) — the March 2026 unification ADR
- [GoodNewsEveryone/docs/architecture/unified-chain-architecture.md](../../../GoodNewsEveryone/docs/architecture/unified-chain-architecture.md) — IR target architecture
- [GoodNewsEveryone/docs/architecture/action-primitives-canonical.md](../../../GoodNewsEveryone/docs/architecture/action-primitives-canonical.md) — prior ten-operation vocabulary (now compressed to four)
- [GoodNewsEveryone/docs/wire-pillars.md](../../../GoodNewsEveryone/docs/wire-pillars.md) — 44 pillars
- [docs/SYSTEM.md](../SYSTEM.md) — current Node system map
- `~/.claude/skills/wire-node-rules/SKILL.md` — the Five Laws
- `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_wire_canonical_vocabulary.md` — vocabulary summary
