# DADBEAR oversight

**DADBEAR** is the mechanism that keeps your pyramids current as source material changes. It runs continuously in the background: detecting changes, evaluating what needs re-answering, applying supersessions, and recursing upward. Understanding DADBEAR — what it's doing, when to let it run, and when to pause or intervene — is a meaningful part of operating Wire Node well.

The Oversight tab in Understanding is where you watch and control DADBEAR.

---

## What DADBEAR is doing when you aren't watching

DADBEAR's loop:

1. **Detect** — the file watcher notices that a source file has changed (hash changed), was deleted, was renamed, or was added.
2. **Accumulate** — the change becomes pending work for the layer it affects (usually L0 first).
3. **Debounce** — per-layer timers wait for a settling window so changes that arrive in bursts get batched.
4. **Batch** — pending work gets distributed into balanced groups.
5. **Evaluate** — for each batch, an LLM is asked: *given the old content and the new content, is this node still right?*
6. **Act** — confirmed stale nodes are superseded; tombstones are written for deletions; renames produce new node IDs with annotation carryforward.
7. **Recurse** — the supersession at layer N is itself a mutation at layer N+1, so the loop walks upward until nothing more is stale.

The same loop handles:

- File changes.
- File deletions and renames.
- New file discovery (scanning is DADBEAR's first tick with empty prior state).
- Belief contradictions (a new extraction contradicts an older one).
- Annotation triggers (certain annotations flag their target for re-evaluation).
- Policy changes (a new tier routing or a new prompt can make nodes re-eval-worthy).

One loop, one system. No separate scanner. No separate maintenance pipeline.

---

## The Oversight tab

**Understanding → Oversight** shows DADBEAR across all your pyramids.

### Per-pyramid cards

Each pyramid with DADBEAR enabled appears as a card:

- **Slug** and **display name**.
- **Status** — active (running), paused, breaker-tripped, held.
- **Pipeline counts** — input (pending work), processing (currently being evaluated), output (recent results).
- **Cost** — total spend on DADBEAR evaluations for this pyramid, with a window selector (all time / 7d / 30d).
- **Last check** — relative time.
- **Pause / resume** button.
- **Configure** button — opens the full DADBEAR panel for this pyramid.
- **View activity** button — opens the activity drawer with detailed logs.

You can filter the card grid: all pyramids / active only / paused only / breaker-tripped only.

### Provider Health banner

If any of your LLM providers is having trouble (repeated errors, 5xx responses, exhausted credits), a banner appears at the top. Click to expand details. DADBEAR will automatically back off when providers are unhealthy, but the banner lets you intervene proactively — switch tier routing to a different provider, rotate credentials, or pause DADBEAR entirely while the provider recovers.

### Orphan broadcasts panel

Sometimes the LLM cost-webhook arrives but Wire Node can't match it to any in-flight request. This is usually benign (a retry that succeeded after the original was abandoned). If orphans accumulate rapidly, it can indicate a bug or a misconfigured webhook secret. The panel shows recent orphans with sources and suggests actions (acknowledge, investigate).

### Cost rollup section

Aggregate cost across all pyramids for the selected window. Broken down by:

- **Source** — which subsystem (extraction, answering, stale-check, characterization).
- **Operation type** — which primitive (extract, synthesize, fuse, etc.).
- **Layer** — L0 / L1+ split.
- **Pyramid** — sorted by spend.

Plus a top-10 table of most recent high-cost calls. Useful for spotting spend anomalies.

---

## The per-pyramid DADBEAR panel

Clicking **Configure** on a pyramid card opens its full DADBEAR panel. Sections:

### Auto-Update Config

- **Enable toggle** — master on/off for auto-updates on this pyramid.
- **Debounce minutes** — how long to wait after the last change before processing (default: 5 minutes for L0, tapering for higher layers). Smaller = more responsive, more cost. Larger = more batched, less cost.
- **Min changed files** — don't trigger at all until at least this many files have changed. Default: 1.
- **Runaway threshold** — if more than this fraction of a layer's nodes go stale in a single batch, trip the breaker. Default: 0.75.
- **Save** — applies; new settings take effect on the next tick.

### Status

- **Frozen** — if manually paused.
- **Breaker tripped** — if the runaway threshold was hit; requires manual intervention.
- **Pending mutations by layer** — how many L0, L1, L2 etc. mutations are queued.
- **Last check time.**
- **Current phase and detail** — what DADBEAR is currently doing, if anything.
- **Timer fires at** — when the next tick will run.
- **Last result summary** — how many mutations processed, how many kept, how many superseded.

### Stale log

A table of recent staleness evaluations:

- **Layer** — which layer this node is in.
- **Node ID.**
- **Stale reason** — what the LLM said about whether the node is still valid.
- **Cost** — tokens + dollars for this specific evaluation.
- **Checked at** — timestamp.

Filterable by layer and by stale/keep status. Use this to answer "what has DADBEAR been doing lately on this pyramid?"

### Cost analysis

Per-pyramid cost with the same breakdowns as the global cost rollup: by source, by operation, by layer. Plus a recent-calls table.

### Contributions

How many annotations the pyramid has, unique authors, FAQ count, last contributor. DADBEAR triggers on annotations in specific cases, so this section ties back.

### Evidence density

- **Per-layer keep count** — for each layer, how many evidence links are KEEP'd. Indicator of evidence density.
- **Top nodes by inbound links** — the load-bearing nodes. If DADBEAR has been superseding these, that's where the most propagation happens.

### Run now

A button to force an immediate DADBEAR tick without waiting for the timer. Useful when you know you just changed a bunch of files and want to see the result right away.

---

## Pausing DADBEAR

Two scopes:

**Per-pyramid pause** — stops auto-updates on one pyramid, leaves others running. Click **Pause** on the pyramid's card or in its panel. The card shows a paused badge; mutations continue to accumulate but no evaluations run. Resume when ready.

**Global pause** — stops auto-updates on all pyramids. Useful during LLM provider outages, or when you want to batch-process a big change set manually. Available at the top of the Oversight tab.

Pausing does not lose data. Pending mutations persist. When you resume, the queue drains as usual.

---

## The breaker

If DADBEAR finds that more than 75% of a layer's nodes are stale in a single tick, the **breaker** trips. This is almost always a signal that something big has happened — typically:

- You just pulled in a huge change set (git merge from main, bulk refactor).
- You reshaped the source folder and many files got renamed.
- You changed a policy in a way that invalidated many nodes.

The breaker exists because blindly re-evaluating 75% of a layer at once is expensive and usually gives poor results (every LLM call sees a mostly-new pyramid context, and consistency suffers). Instead it pauses, and you choose:

1. **Resume** — if you're confident the wave of staleness is real and should be processed. DADBEAR chews through it.
2. **Rebuild from scratch** — sometimes the right move when the pyramid has diverged so much from source that incremental updates aren't the right approach.
3. **Freeze** — keep the pyramid as-is (known stale) until you decide what to do.

Breaker state is per-pyramid; one pyramid can have a tripped breaker without affecting others.

---

## When to trust DADBEAR's output

- **Small, normal change sets** (you edited a few files, DADBEAR re-evaluates, a few nodes update) — trust implicitly.
- **Medium change sets** (a refactor, a new module) — spot-check. Look at the stale log; drill a few superseded nodes in the surface to see that the supersession makes sense.
- **Large change sets** (breaker trips) — don't resume blindly. Understand what changed first. Sometimes rebuilding is the right move.
- **Contentious change sets** (you're reshaping the architecture in a way that contradicts prior understanding) — expect DADBEAR to struggle a bit. Consider writing a new apex question that reflects the new architecture and building a fresh pyramid, rather than trying to evolve the old one.

---

## DADBEAR and the cost model

DADBEAR is continuous cost. Even a pyramid nobody is actively using has DADBEAR ticks running over it as source files change. Tips to keep this in line:

- **Set debounce higher on stable pyramids.** A published archival pyramid doesn't need 5-minute debounce.
- **Use `stale_local` tier routing for staleness checks.** Staleness checks are exactly the kind of work that's cheap on a local Ollama model. You pay only for the heavy synthesis steps when they actually fire.
- **Review the cost rollup monthly.** Identify pyramids that cost more than they're worth and either archive them or dial down their DADBEAR.
- **Archive pyramids you aren't using.** Archived pyramids don't run DADBEAR.

---

## Common patterns

**"I made a bunch of changes; why isn't DADBEAR doing anything yet?"** — Debounce. By default it waits 5 minutes after the last change to avoid thrashing on bursty edits. Click **Run now** if you want to force it.

**"DADBEAR evaluated a node and kept it, but I can see the underlying file changed materially."** — The LLM thought the change didn't affect the node's answer. Drill the stale log to see the reasoning. If you disagree, reroll the node manually.

**"DADBEAR is spending more than I expected."** — Check cost analysis. Common culprits: too-low debounce, expensive tier routing for the stale-check step, a pyramid over a directory that has lots of noise changes (temp files, build artifacts). Dial the first two and fix `.gitignore` for the third.

**"The breaker tripped but I know the wave of staleness is legitimate."** — Resume. DADBEAR will chew through it. Budget some LLM cost; expect a few minutes of heavy activity.

---

## Where to go next

- [`43-auto-update-and-staleness.md`](43-auto-update-and-staleness.md) — tuning DADBEAR in detail.
- [`50-model-routing.md`](50-model-routing.md) — route stale-checks to cheaper tiers.
- [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md) — troubleshooting DADBEAR.
