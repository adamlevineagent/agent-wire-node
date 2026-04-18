# Staleness and breakers (DADBEAR troubleshooting)

DADBEAR runs continuously; when it behaves unexpectedly, this is where to look.

---

## The breaker tripped

When more than ~75% of a layer's nodes go stale in a single DADBEAR tick, the breaker trips and auto-updates pause for that pyramid. You see this in Understanding → Oversight → the pyramid's card shows a red "breaker tripped" badge.

### What caused it

Almost always one of:

- You pulled in a big change set (git merge from main, bulk refactor, large doc import).
- You reshaped the source folder — mass renames, reorganizations.
- You changed a policy (tier routing, chain assignment) that invalidated many nodes.

The breaker exists because processing ~75% of a layer in one batch is expensive and produces poor results (each LLM sees a mostly-new pyramid context; consistency suffers).

### What to do

Three options, pick based on the cause:

**Resume** — if the wave of staleness is legitimate and you're ready for the cost + time to process it, click Resume in the pyramid's DADBEAR panel. DADBEAR chews through. Expect several minutes of heavy activity.

**Rebuild from scratch** — if the pyramid has diverged so much from source that incremental updates aren't sensible. Often the right move for major architecture changes. Rebuild is cheaper than a gigantic DADBEAR cascade.

**Freeze** — keep the pyramid as-is (known stale) until you decide. Useful if the change is temporary or if you're not sure what to do yet. Pausing DADBEAR costs nothing; all prior work stays accessible.

---

## DADBEAR isn't running when I expected it

### Check if it's enabled

Understanding → Oversight → the pyramid's card. If DADBEAR is paused or the pyramid is in "held" state, that's why.

Resume via the card's Resume button. If you paused globally (Oversight tab's top-level pause), resume that too.

### Check debounce

DADBEAR waits a debounce window (default 5 minutes for L0) after the last detected change before processing. If you edited files 2 minutes ago, DADBEAR is still waiting.

Force it immediately via the pyramid's DADBEAR panel → **Run now** button.

### Check the file watcher

DADBEAR relies on macOS's FSEvents to detect file changes. If the watcher isn't seeing changes:

- Maybe the source path you registered has moved or become unreachable.
- Maybe FSEvents is having issues on your Mac (rare).

Manually trigger a scan via the CLI: `curl -X POST http://localhost:8765/pyramid/<slug>/dadbear/scan` (bearer auth required).

### Check the stale log

Understanding → pyramid detail → DADBEAR panel → stale log. If it's empty despite recent source changes, the watcher didn't detect. If it has entries but they all say "fine," DADBEAR is running but deciding nothing is stale.

---

## DADBEAR says a node is fine but I can see it's actually stale

The LLM's staleness judgment was wrong for that specific node. Options:

- **Manually reroll the node.** From the node inspector → Reroll → add a note about what's changed.
- **Tune the staleness prompt.** If this happens repeatedly, the staleness check prompt isn't catching what it should. Edit `chains/prompts/shared/` or wherever the staleness prompt lives for your pyramid.
- **File a correction annotation.** A correction annotation triggers DADBEAR to re-evaluate the node with fresh eyes.

### The cost concern

Staleness checks cost per-node. If you route `stale_local` (or whatever tier your pyramid uses for staleness) to an expensive model, you pay a lot for each false-negative re-evaluation. Route staleness to a cheap tier — ideally local Ollama if you have the hardware.

---

## DADBEAR is running but the cost is alarming

Check the cost rollup scoped to this pyramid (Understanding → pyramid detail → DADBEAR panel → Cost Analysis).

Common causes:

- **Too-low debounce.** Every file save triggers a tick. Set debounce to something sensible (10-30 minutes for non-critical pyramids).
- **Too-low min_changed_files.** Any tick with even 1 changed file fires expensive evaluations. Raise to 3-5 for stable pyramids.
- **Staleness checks routed to an expensive model.** Fix tier routing.
- **Thrashing.** A pyramid whose source has many transient changes (build artifacts being written, tests running, caches updating) triggers evaluation after evaluation. Fix `.gitignore` for those files or add patterns to the file watcher's ignore list.

---

## A supersession propagated weirdly

### Symptoms

You expected a file-level change to update L0 and stop there; instead it cascaded up to apex. Or you expected a cascade and got nothing.

### Why

DADBEAR's propagation rules:

- **Staleness** attenuates — effects get smaller as they propagate upward. High-weight evidence changing is more likely to stale the parent answer than low-weight evidence.
- **Supersession** does not attenuate — a specific claim that's now false must be corrected everywhere it appears.

If you expected attenuation and got cascade: the change was evaluated as a supersession, not mere staleness. Check the stale log for the trigger node; the reason field tells you how the LLM classified it.

If you expected cascade and got nothing: the child change didn't meet the confidence threshold for triggering parent re-evaluation. The LLM said "the parent's answer is still fine given this child change."

### What to do

- If the LLM's classification is wrong, reroll or annotate-correction.
- If the thresholds are too tight or too loose, tune via the DADBEAR policy config.
- If you want to force propagation: supersede upward manually via the CLI.

---

## A pyramid won't leave "held" state

Held state happens when DADBEAR decides it can't proceed safely — usually a consistency issue where prior state doesn't match what's expected. You see this rarely.

### Recovery

1. Check the DADBEAR panel's status for the specific reason.
2. If a prior build was incomplete, retry or delete it.
3. If a corruption is reported, consider rebuilding the affected part of the pyramid from scratch.
4. If nothing else works, full pyramid reset (see [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md) → "When all else fails").

Held state shouldn't happen often. If it recurs, capture the log and file in the alpha channel.

---

## Annotations aren't triggering re-evaluation

Only certain annotation types trigger DADBEAR:

- **Correction** — always triggers re-evaluation.
- **Question** (with a `question_context` that doesn't resolve) — may trigger.
- **Observation**, **Idea**, **Friction** — do not trigger.

If you want a node re-evaluated based on new knowledge, use type `correction`.

---

## Leak detection alert fired

The evidence-triage system expects every synchronous cost reconciliation to have a matching asynchronous broadcast confirming it. If broadcasts don't arrive within the grace window, a leak detection alert fires.

Possible causes:

- **Broadcast webhook not configured** at the provider. See provider-specific setup (OpenRouter needs the webhook URL + shared secret).
- **Tunnel down** — broadcasts come in through your tunnel.
- **Grace window too short** — some providers' broadcasts are delayed. Extend the window in the evidence triage policy.
- **Actual leak** — real billing discrepancy. Rare but non-zero.

Understanding → Oversight → cost rollup shows the discrepancy amount. Investigate if it's significant.

---

## Where to go next

- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — DADBEAR in depth.
- [`A0-common-issues.md`](A0-common-issues.md) — grab bag.
- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — diagnostic surfaces.
- [`docs/PUNCHLIST.md`](../PUNCHLIST.md) — known issues.
