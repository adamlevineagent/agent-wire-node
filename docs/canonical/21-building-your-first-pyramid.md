# Building your first pyramid

This doc walks through creating one pyramid end to end, with what to expect and what to do if something goes off the rails. It assumes you've installed the app ([`10-install.md`](10-install.md)), done onboarding ([`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md)), and put an API key in place ([`12-credentials-and-keys.md`](12-credentials-and-keys.md)).

We'll build a pyramid over a small codebase, because it's the fastest path to a satisfying result. The same approach works for documents and conversations.

**Before you start:** pick a repository you know well. Something between 20 and 200 source files is ideal for a first build — small enough to finish quickly, large enough to produce interesting structure. A Python or TypeScript project of a few thousand lines works great.

---

## Step 1: Open Add Workspace

In the sidebar, click **Understanding**. If this is your first pyramid, the empty state shows an **Add Workspace** button front and center. Click it.

If you already have pyramids, click **Add Workspace** at the top of the Dashboard list.

## Step 2: Pick a directory

The file picker opens. Navigate to your chosen repository's root folder and click **Choose**. Agent Wire Node scans the folder to build an ingestion plan — this takes a second or two for small repos, longer for bigger ones.

Things Agent Wire Node looks at during scan:

- File types (ignores binaries, images, videos, lockfiles by default).
- File size (ignores files above a sane threshold unless overridden).
- Directory structure (uses it to suggest whether this is a flat codebase or a vine candidate).
- `.gitignore` entries (respects them).

The scan does **not** read file contents yet — that happens during build.

## Step 3: Pick content type

The wizard suggests a content type based on the scan. For a typical repo, it'll suggest `code`. Accept it (or override — see [`22-content-types.md`](22-content-types.md) for when each is appropriate).

If the wizard suggests `vine` (because it saw multiple subfolders that look like separate projects), you can accept to build a hierarchical pyramid across the whole thing, or force `code` to treat the repo as flat. For a first build, flat `code` is simpler.

## Step 4: Pick an apex question (or accept the default)

The preset question for `code` is:

> What is this codebase and how is it organized?

This is a good, broad first question — it drives the pyramid to extract architecture, module structure, relationships, decisions, and conventions. The L0 evidence base that results is useful for any later question.

You can override with a more specific question, like *"What are the security properties of this codebase?"* or *"How does authentication work?"* A narrower question produces a deeper but less general pyramid. For a first build, the preset is the right call — broad questions make for a richer evidence base.

## Step 5: Configure absorption (skip for now)

The wizard asks how the pyramid should absorb incoming questions if you publish it. The default is "open, no rate limits" which is fine for a local-only build. You can change this later if you publish.

## Step 6: Confirm and build

Click **Build**. A modal shows the ingestion plan:

- Operations it will perform (chunk N files into M chunks).
- Estimated cost (based on chunk counts and the current tier routing).
- Any files that errored during scan.
- Estimated duration (rough — first builds can take 2x the estimate).

Confirm. The Pyramid Surface opens in the main area and the build starts.

---

## What you see during the build

The Pyramid Surface renders live as the build progresses. Expect this sequence:

1. **Decomposition phase** (~10 seconds, 1-2 LLM calls). Your apex question becomes a tree of sub-questions. You'll see activity in the log but no nodes yet.
2. **Extraction schema phase** (~10 seconds, 1 LLM call). The system decides what L0 nodes should look like given all the leaf questions.
3. **L0 extraction phase** (bulk of the time). Chunks get processed, often many in parallel. L0 nodes appear in the surface as they're created. A small codebase (~50 files, ~200 chunks) on OpenRouter with a fast model can complete this in 2-5 minutes. A larger codebase can take an hour or more.
4. **Evidence answering phase.** Sub-questions get matched against the L0 evidence; KEEP/DISCONNECT/MISSING verdicts populate. You see edges appear between nodes in the surface.
5. **Synthesis phase.** Branch questions synthesize from their leaves; the apex synthesizes from branches. The top of the pyramid fills in.
6. **Reconciliation phase** (~30 seconds). Orphan detection, central node identification, gap clustering. The web edges between sibling nodes appear.
7. **Done.** The status in the header flips from Running to Complete.

The Activity Log (collapsed at the bottom by default) shows the sequence of events. The Pipeline Timeline at the top shows which phase is active and elapsed time per phase.

### During the build you can

- **Cancel** — if you want to abort, there's a Cancel button in the header. The build stops at the next safe checkpoint; anything already built persists.
- **Watch quietly** — the surface updates in real time, or you can collapse it and come back.
- **Switch tabs** — you can leave the surface and go do other things; the build runs in the background. The sidebar's Understanding item pulses to indicate activity.
- **Reroll a specific node** — if you see an answer come out looking wrong, you can reroll that one node without touching the rest. (This is rare during a first build; more common on later refinement passes.)

### If it looks stuck

A step that hasn't progressed in ~60 seconds is worth a look. Usually it's one of:

- **LLM provider is slow or erroring.** Check the provider health in Settings, or watch the log for repeated retries.
- **Ollama is swamped.** Local inference can bottleneck on a single model. Check CPU/GPU usage.
- **A particularly large synthesis step is running.** Complex syntheses can take several minutes on their own. The log will say "step N/M running".

If the build has been at 0% progress for several minutes, see [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md). The most common root cause is a missing or invalid credential; the second most common is a rate-limited provider.

## When the build completes

The header shows **Complete** and the total elapsed time. The Pyramid Surface shows the full structure. You can:

- **Hover** any node to see its headline.
- **Click** any node to open the inspector — full prompt, response, evidence links.
- **Use the arrow keys** in the inspector to walk siblings and parents.
- **Switch overlays** to see structure (default), web edges (cross-cutting connections), staleness (which nodes are flagged as stale — should be empty on a fresh build), or build (which nodes came from which phase).
- **Search** within the pyramid for specific terms.

Go back to **Understanding → Dashboard**. Your pyramid is there with a node count, a last-built timestamp, and a green dot (unpublished but current).

## Try your first query

From the detail drawer, click **Ask question**. Ask something specific:

- *"What are the main modules and their responsibilities?"*
- *"What would a new contributor need to know first?"*
- *"Where is user input validated?"*

This creates a question pyramid referencing your source pyramid. It re-uses the L0 evidence, decomposes the new question, and cross-links where it can. This second build is much faster than the first — sometimes seconds.

The answer pyramid has its own apex, its own sub-questions, and links back to evidence in the original pyramid. Drill the apex for the synthesis; drill a sub-question for the detail; drill the evidence for the source text.

## Hook up Claude

At this point it is worth connecting an agent. In Claude Desktop's config, add Agent Wire Node's MCP server (see [`81-mcp-server.md`](81-mcp-server.md)). Restart Claude. In a new session, ask Claude:

> Walk my pyramid `<your-slug>`. Start at the apex, then drill into whatever looks most relevant to someone new to the codebase. Leave annotations as you go with anything non-obvious you learn.

Claude will use `pyramid_apex`, `pyramid_drill`, `pyramid_search`, and `pyramid_annotate` to explore. Every annotation shows up in the FAQ directory on your node. Over time, the pyramid gets richer — and the next Claude session starts hot.

---

## Things that commonly happen on first builds

**The apex looks shallow.** If your codebase is small, the apex is high-level by necessity. Ask a follow-up question that drills into something specific, and the resulting question pyramid will have more depth. Alternatively, re-run with a more specific apex question.

**A few nodes are clearly wrong.** The reroll button on any node lets you re-generate that specific node without rebuilding anything else. Add a note in the reroll dialog if you want to steer it. See [`25-dadbear-oversight.md`](25-dadbear-oversight.md) for how supersession works.

**The build cost more than expected.** Check **Understanding → Oversight** → Cost Rollup. The breakdown shows which phases cost what. If extraction was most of the cost, a smaller model for extraction would help — see [`50-model-routing.md`](50-model-routing.md) for tuning tier routing.

**The build finished but the pyramid feels lopsided.** Common for codebases with uneven density (one massive module, several small ones). Try asking a question scoped to the dense area — the resulting question pyramid balances better.

---

## What you've accomplished

You have:

- One pyramid over your codebase, with an evidence base that other questions can draw from.
- Familiarity with the build flow and the Pyramid Surface.
- A derivative question pyramid if you tried the "Ask question" step.
- (Optionally) an agent connection that can work with your pyramid from any Claude session.

Next common steps:

- **Keep asking questions.** Each new question is a new pyramid that reuses evidence. This is where the compounding happens.
- **Publish the pyramid** to the Wire so collaborators can query it. See [`61-publishing.md`](61-publishing.md).
- **Tune DADBEAR.** As the codebase changes, DADBEAR re-evaluates. Go to Oversight to see it at work. See [`25-dadbear-oversight.md`](25-dadbear-oversight.md).
- **Edit a chain.** The default code chain is fine, but your own variant can focus on what you actually care about. See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md).

---

## Where to go next

- [`22-content-types.md`](22-content-types.md) — pick the right content type for a non-code build.
- [`23-pyramid-surface.md`](23-pyramid-surface.md) — deep dive on the visualization.
- [`24-asking-questions.md`](24-asking-questions.md) — question pyramids, cross-pyramid references.
- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — keeping it current.
