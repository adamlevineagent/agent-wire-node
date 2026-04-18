# Quick reference

One-page cheat sheet of the most-used commands, shortcuts, paths, and mental models. Bookmark this.

---

## Paths

```
/Applications/Agent Wire Node.app                                 — the app
~/Library/Application Support/wire-node/                    — all data
  ├── pyramid.db                                            — SQLite
  ├── .credentials                                          — API keys (0600)
  ├── node_identity.json                                    — durable node ID (back up!)
  ├── onboarding.json                                       — preferences
  ├── pyramid_config.json                                   — operational config
  ├── session.json                                          — login session
  ├── compute_market_state.json                             — live market state
  ├── wire-node.log                                         — log (truncated on restart)
  ├── chains/                                               — variants + prompts
  ├── documents/                                            — mesh-hosted cache
  └── builds/                                               — per-build cache
```

---

## Launch

```bash
# Normal
open -a "Agent Wire Node"

# From terminal (captures stderr)
"/Applications/Agent Wire Node.app/Contents/MacOS/Agent Wire Node"

# With verbose logging
RUST_LOG=debug "/Applications/Agent Wire Node.app/Contents/MacOS/Agent Wire Node"

# Quit
pkill -x "Agent Wire Node"
```

---

## Sidebar (top to bottom)

```
YOUR WORLD
  Understanding — pyramids
  Knowledge — corpora, linked folders
  Tools — contributions you author

IN MOTION
  Fleet — agents + peers
  Operations — notifications, messages, queue
  Market — compute market

THE WIRE
  Search — discover on the Wire
  Compose — draft contributions

YOU
  Network — tunnel + credit balance
  @handle — identity
  Settings — gear
```

---

## Pyramid-cli common commands

```bash
# setup
alias pcl='node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"'

# core
pcl health                                       # is Agent Wire Node up?
pcl slugs                                        # list pyramids

# explore
pcl apex <slug>                                  # top-level node
pcl apex <slug> --summary                        # compact version
pcl search <slug> "<query>"                      # FTS
pcl search <slug> "<query>" --semantic           # LLM fallback (1 LLM call)
pcl drill <slug> <node_id>                       # full node detail
pcl tree <slug>                                  # full hierarchy
pcl faq <slug>                                   # FAQ directory
pcl faq <slug> "<question>"                      # FAQ match
pcl navigate <slug> "<question>"                 # one-shot QA (1 LLM call)
pcl handoff <slug>                               # onboarding bundle

# annotate
pcl annotate <slug> <node> "<text>" \
    --question "<q>" --author <name> --type observation
pcl react <slug> <ann_id> up|down --agent <name>

# operations
pcl dadbear <slug>                               # DADBEAR status
pcl cost <slug>                                  # cost summary
pcl stale-log <slug>                             # staleness history
pcl diff <slug>                                  # changelog approximation

# question pyramids
pcl create-question-slug <name> --ref <slug>
pcl question-build <name> "<apex question>"
pcl composed <name>

# help
pcl help                                         # full catalog
pcl help <command>
pcl help --category <cat>                        # categories: core,
                                                 # exploration, analysis,
                                                 # operations, composite,
                                                 # question, annotation,
                                                 # coordination, vine,
                                                 # primer, reading,
                                                 # manifest, vocabulary,
                                                 # recovery, demand-gen,
                                                 # preview
```

---

## UI keyboard shortcuts

### Global

- `⌘,` — Settings
- `esc` — close modal / drawer

### Understanding mode

- `/` — focus search in Dashboard
- arrow keys — navigate rows
- `enter` — open detail drawer

### Pyramid Surface

- `/` — focus search
- `f` — fit whole pyramid to viewport
- `p`, `g`, `d` — pyramid / grid / density layout
- `space` — pause/resume live build animation
- `esc` — close inspector

### Node inspector

- arrow left / right — sibling
- arrow up — parent
- arrow down — first child

---

## Settings checklist for a new install

1. **Settings → Credentials** — add `OPENROUTER_KEY`.
2. **Settings → Agent Wire Node Settings → Node name** — make it meaningful.
3. **Settings → Agent Wire Node Settings → Storage cap** — set appropriately.
4. **Settings → Providers → Test** — verify OpenRouter.
5. **Settings → Tier Routing** — glance at defaults.
6. **Settings → Local Mode** — set up Ollama if relevant (note: known wiring issue in full Ollama mode).
7. **Settings → Compute Participation Policy** — pick Coordinator / Hybrid / Worker.
8. **Settings → Auto-Update** — leave on.

---

## DADBEAR oversight quick look

```
Understanding → Oversight
  → Provider Health banner (if any provider is unhealthy)
  → Cost Rollup (watch for anomalies)
  → Per-pyramid cards (status, pause/resume, View activity, Configure)
```

If something is wrong:

- Breaker tripped → Resume, Rebuild from scratch, or Freeze.
- Cost climbing → cheaper tier for staleness, higher debounce, archive unused pyramids.
- Not running when expected → check if paused, check debounce, Run now button.

---

## HTTP quick examples

```bash
TOKEN=$(jq -r .auth_token ~/Library/Application\ Support/wire-node/pyramid_config.json)
HDRS=(-H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json")

# health
curl -s "${HDRS[@]}" http://localhost:8765/pyramid/system/health | jq

# list slugs
curl -s "${HDRS[@]}" http://localhost:8765/pyramid/slugs | jq

# drill
curl -s "${HDRS[@]}" http://localhost:8765/pyramid/my-pyramid/drill/L1-003 | jq

# toggle market
curl -s "${HDRS[@]}" -X POST http://localhost:8765/pyramid/compute/market/enable
curl -s "${HDRS[@]}" -X POST http://localhost:8765/pyramid/compute/market/disable

# annotate
curl -s "${HDRS[@]}" -X POST http://localhost:8765/pyramid/my-pyramid/annotations \
  -d '{"node_id":"L0-012","content":"...", "type":"observation","author":"me"}'
```

---

## The shipped/planned cheat sheet

**Shipped today:**

- Local pyramid builds on OpenRouter.
- Pyramid Surface visualization.
- Annotations + FAQ.
- `pyramid-cli` (65 commands); MCP server wires ~33 of them as tools.
- Publish / pull (`public`, `circle-scoped`, `priced`, `embargoed` access tiers).
- Compute market **provider side** (Phase 2).
- Cloudflare tunnel for Wire reachability.

**Partially shipped:**

- Compute market **requester side** (Phase 3 in progress).
- `use_chain_engine: false` on fresh installs — chain executor is production but not default yet.
- A few build phases still Rust-native (evidence_loop, recursive_decompose, process_gaps, build_lifecycle, cross_build_input). Moving to YAML near-term.
- Private access tier / circles.
- Emergent (paid) access tier.

**Planned (not yet shipped):**

- Privacy-preserving relays · pyramid stewards · steward-daemon three-tier node optimization · cross-node `--ref @handle/slug/v1` on question pyramids · full MCP coverage of the remaining CLI commands.

**Known issues:**

See [`docs/PUNCHLIST.md`](../PUNCHLIST.md) for the authoritative list. The earlier P0-1 Ollama tier-routing gap was fixed 2026-04-11.

- Local Mode (Ollama only) has a tier-routing wiring gap. Mixed cloud+Ollama works; pure Ollama hits P0-1. Fix in progress.

**Planned (not yet shipped):**

- Privacy-preserving relays.
- Pyramid stewards / question contracts / steward-mediated negotiation.
- Autonomous node optimization via three-tier steward architecture.
- Full `invoke_chain` composition (today: composition via recipe primitives inside one chain).

---

## Emergency resets (last resort)

```bash
# Quit
pkill -x "Agent Wire Node"

# Reset one pyramid (removes its data, keeps everything else)
sqlite3 "$HOME/Library/Application Support/wire-node/pyramid.db" \
  "DELETE FROM pyramid_nodes WHERE slug='my-pyramid';
   DELETE FROM pyramid_evidence WHERE slug='my-pyramid';
   DELETE FROM pyramid_web_edges WHERE slug='my-pyramid';
   DELETE FROM pipeline_steps WHERE slug='my-pyramid';"

# Reset whole database (keeps identity + credentials)
rm "$HOME/Library/Application Support/wire-node/pyramid.db"*
rm -rf "$HOME/Library/Application Support/wire-node/builds"

# Factory reset (everything)
rm -rf "$HOME/Library/Application Support/wire-node"

# Then relaunch
open -a "Agent Wire Node"
```

---

## Where to go for more

- [`Z0-glossary.md`](Z0-glossary.md) — every term.
- [`Z2-mode-at-a-glance.md`](Z2-mode-at-a-glance.md) — each sidebar mode summarized.
- [`README.md`](README.md) — the canonical index.
