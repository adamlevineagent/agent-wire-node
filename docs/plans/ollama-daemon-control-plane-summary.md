# Ollama Daemon Control Plane — What We Built

**Date:** 2026-04-13
**Companion to:** `ollama-daemon-control-plane.md` (the technical plan)
**Commits:** `858d098..e700e16` on `main`

---

## The Starting Point

You had a simple on/off toggle for local models (Ollama). Turn it on, everything routes to your local GPU. Turn it off, back to cloud. The model dropdown locked when enabled — to change models you had to toggle off and back on. There was no visibility into what was happening, no way to manage models, and builds were progressively slowing down because the stale-checking system was fighting with builds for the same single inference slot.

## What's Different Now

### 1. You can switch models without toggling off

The model dropdown stays live when local mode is on. Pick a different model from the list, it switches immediately — no disable/re-enable dance. The app checks that Ollama has the model before switching, and it won't let you switch while a build is running (to prevent half-and-half pyramids).

### 2. Models show as visual cards instead of a bare dropdown

Each model now shows as a card with its name, parameter size (e.g., "27B"), quantization level (e.g., "Q4_K_M"), disk size, and context window. The active model gets a cyan highlight and "Active" badge. Click any card to switch. Cards dim and become unclickable while an operation is in progress so you know something's happening.

### 3. You can pull new models from the app

There's a "Pull Model" section where you type a model name and click Pull. A progress bar shows the download in real-time (bytes downloaded, total size, current phase). You can cancel mid-download. There's a link to browse the Ollama library in your browser. When the pull completes, the model list refreshes automatically.

### 4. You can delete models from the app

Each model card has a small x button that appears on hover. Click it, get a confirmation dialog with a warning about in-progress builds, confirm, and the model is removed from Ollama. You can't delete the model that's currently active (the button doesn't appear on it).

### 5. You control the context window

A "Context Window" section shows the detected context limit for your current model and lets you override it. If you know your model supports more context than the default, type a number and click Apply. The override actually tells Ollama to allocate that context (not just Wire Node — this was a critical fix, previously the override would have been cosmetic). Reset button returns to auto-detected. Warning appears if you set it higher than what the model reports.

### 6. You control build concurrency

A "Concurrency" section lets you set how many parallel inference requests run during builds (1-12). Default is 1. Warning text explains most home users should leave it at 1. When you increase it, both the build pipeline AND the Ollama connection pool update together (this was a subtle but critical design point — without both changing, the parallelism would have been fake).

### 7. Builds no longer slow down progressively

This was the big invisible fix. Previously, the stale-checking system (which keeps your pyramids fresh) was competing with active builds for a single inference slot. With 800 nodes in a layer, 50+ build workers and 8+ stale-check workers all fought for one HTTP connection. Now: when local mode is enabled, stale checks automatically defer during active builds. They queue up silently and batch-process after the build finishes. The inference connection also routes through a proper per-provider pool instead of a global bottleneck. Testing confirmed it — 38 L0s in 10 minutes without slowdown, where before it bogged down at ~200/800.

### 8. Partial builds resume instead of skipping

Previously, if a build was interrupted partway through (say 400 out of 800 documents extracted) and you clicked Rebuild, it would skip the extraction entirely and jump ahead — leaving 400 documents unprocessed. Now it detects the gap and re-runs extraction. Already-extracted documents hit the cache (near-instant), only the missing ones cost real time.

### 9. You can see configuration history

A "Configuration History" section shows a timeline of every tier routing change — when it happened, why (the triggering note), who made it. You can roll back to any previous configuration with one click (after a confirmation). Rollback is blocked while local mode is enabled (you need to disable first) and the system validates that old configurations are still compatible before allowing rollback.

### 10. You can set optimization territory for the future steward

An "Optimization Territory" section lets you mark each configuration dimension (model selection, context, concurrency) as Locked, Experimental, or Bounded. This doesn't affect behavior today — it's metadata that the steward-daemon will read when it arrives. The idea: when the steward eventually manages your node autonomously, it'll only touch dimensions you've explicitly marked as fair game. You start conservative (everything locked), build trust, and gradually open surfaces.

### 11. Remote server warning

If you point the base URL at something that isn't localhost, an orange warning tells you all your prompts and build data will be sent to that server unencrypted. Not a block (you might have Ollama on another machine), but makes the risk visible.

### 12. The settings panel is organized into collapsible sections

All the new features live in accordion sections that collapse/expand. Models, Context Window, Concurrency, Pull Model, Configuration History, Optimization Territory — each is its own section. The panel isn't overwhelming because most sections start collapsed.

### 13. Accessibility improvements

Labels are properly linked to their inputs (screen reader friendly), the toggle has an aria-label, accordion sections have keyboard navigation (Enter/Space to open/close), model cards are keyboard accessible with role/aria attributes.

## What's Not Changed

- Cloud provider (OpenRouter) routing is untouched — all of this only applies when local mode is on
- The build pipeline itself (chain executor, prompts, clustering) is unchanged
- DADBEAR auto-update works the same
- The Wire network features (search, compose, sync) are unchanged

## Net Effect

The Ollama integration went from a dumb toggle to a full control surface. You can manage models, tune performance, see history, and prepare for the steward — all without leaving the app. And the invisible fix (stale deferral + provider pools) means local builds actually run at the speed they should instead of progressively choking.

---

## Commit Log

```
e700e16 feat: Phase 6 — experimental territory + accordion ResizeObserver
72e8348 feat: Phase 5 — config history timeline + rollback
4d73d3c feat: Phase 4 — pull models with streaming progress + delete from UI
cb0d51c feat: Phase 3 — context window + concurrency overrides
b671664 fix: model cards show disabled state during loading
12c2f96 feat: Phase 2 — model portfolio with rich detail cards
a882394 fix: provider pool routing rule, config_json merge, TOCTOU guard
f3b882e fix: partial L0 extraction resumes on rebuild instead of skipping
a573075 feat: Ollama daemon control plane Phase 0+1 — hot-swap, pool wiring, stale deferral
858d098 plan: Ollama daemon control plane — full spec with 4-auditor review
```

## Audit History

- **Plan audit (pre-implementation):** 4 blind auditors across 2 stages + 2 focused re-audits. 51 findings, all resolved before implementation.
- **Post-implementation audit (Phase 0+1):** 2-stage informed + discovery pair. Found provider pool routing rule was dead code (no routing rules), config_json overwrite, TOCTOU gap. All fixed.
- **Per-phase verification:** Each phase had a serial verifier + blind wanderer. Wanderers caught: stale accordion height (fixed with ResizeObserver), concurrency override not re-applied on re-enable, history list staleness, SQLite datetime parsing, proposed entries in history, model card loading state, bounds UI for categorical dimensions.
