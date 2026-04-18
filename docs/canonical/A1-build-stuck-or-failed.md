# Build stuck or failed

Builds can go wrong in a few patterns. This doc is a triage guide.

---

## Build is "running" but not progressing

### Check elapsed time and step count

**Understanding → Builds tab.** Each running build shows elapsed time and step count.

- **< 5 minutes in, making progress slowly:** probably fine. Large extraction phases can take a while.
- **< 5 minutes in, no visible progress:** the first step is setting up. Wait 60 more seconds.
- **> 10 minutes, no progress since last check:** suspect.

### Check the activity log

Click the running build. The activity log shows the current step and the last few events. If the most recent event is "LLM call dispatched" from several minutes ago, the LLM is either slow or not responding.

### Check the queue

**Operations → Queue.** Find the model the build is using. If the queue depth is growing and nothing is executing, the worker is stuck. If the queue is empty but the build is waiting, something else is wrong.

### Common causes

**Rate-limited provider.** Check the log for 429s. Retry backoff eventually works through it; if it's sustained, consider switching tier routing to a different provider.

**Provider outage.** OpenRouter or your upstream is down. **Settings → Providers → Test** to confirm. Switch to a fallback provider if you have one.

**Ollama hung.** Local Ollama occasionally deadlocks on specific prompts or resource contention. `ps aux | grep ollama` — if the process is alive but not responding, restart Ollama: `brew services restart ollama`.

**P0-1 wiring gap.** If you're in pure Ollama mode and the build errors on "no OpenRouter key," you've hit the known issue. See [`51-local-mode-ollama.md`](51-local-mode-ollama.md) for the workaround (mixed routing).

### Force reset

For builds stuck > 30 minutes: Understanding → Builds → your build → **Force reset** (button appears after the threshold). This clears step-level in-progress state while preserving completed work. You can retry the build after.

---

## Build failed immediately

"Failed" status within seconds of starting. Most common causes:

### Credential missing

Check the activity log or recent notifications. "Credential variable X not defined" tells you exactly what to add via Settings → Credentials.

### Provider misconfiguration

Check Settings → Providers → Test for the relevant provider. If the test fails, the provider's own error message is usually specific.

### Invalid chain assignment

If a pyramid is assigned to a chain that no longer exists (you deleted a variant, or a pulled chain was unpulled), the build errors on load. Reassign to a valid chain in the pyramid's detail drawer.

### Source path not accessible

The folder you linked the pyramid to was moved or deleted. The build errors on ingestion. Re-link the folder via the pyramid's detail drawer, or re-create with a new folder.

---

## Build failed partway through

The build ran for a while, then errored.

### Check which step failed

Activity log. The failed step's name + error message tells you the category.

### Common partway failures

**JSON parse errors.** The LLM returned malformed output. Steps have a built-in heal retry (temperature 0.1) that usually recovers. Repeated heal failures suggest the prompt is bad for this specific model — try a different model, or edit the prompt.

**Schema validation errors.** Step has `response_schema` set and the output doesn't conform. Same causes as JSON parse errors plus the possibility that the model doesn't support `response_format` for your request.

**Variable resolution errors.** `$foo.bar` or `{{foo}}` didn't resolve. Usually from editing a chain YAML without updating a prompt to match. Check the chain's step definitions and the prompt templates.

**Concurrency or rate-limit timeout.** Step took longer than the retry budget. The build can resume — restart it and the executor picks up from where it left off (see resume below).

### Retrying

From Understanding → Builds → your failed build → **Retry**. The executor resumes from the last checkpoint — completed work is preserved, only the failed step reruns. Most partway failures recover cleanly on retry.

---

## Build succeeds but output looks wrong

### Apex is too generic

Usually a too-broad apex question on a small pyramid. Ask a more specific follow-up question (creates a question pyramid with more depth in the area you care about).

### L0 nodes are too shallow

Extraction prompt isn't focused enough. Open a few L0 nodes in the inspector → Prompt tab to see what the LLM was asked. Then tune the prompt or author a variant with stronger negative constraints (see [`42-editing-prompts.md`](42-editing-prompts.md)).

### Some nodes are clearly wrong

Reroll them individually. From the node inspector → **Reroll** → add a note to steer. Rerolls are tracked in the audit trail; you can compare before/after.

### Evidence links don't make sense

The pre-map phase is flaky on some material. Try a different tier for `extractor` or add stronger entity-extraction to the L0 prompt. Poor evidence linking usually traces back to thin L0.

### Synthesis missed an obvious aspect

Decomposition didn't cover it. Either bump granularity and rebuild, or ask a follow-up question scoped to the missed area.

---

## The Pyramid Surface shows a half-finished pyramid

Build crashed and left partial state. Two options:

- **Retry the build.** Normally the right call. Resume picks up where it left off.
- **Reset the pyramid** (delete `pyramid_nodes` + `pyramid_evidence` + `pipeline_steps` for the slug) and rebuild from scratch. More destructive but sometimes the cleanest recovery. See **Starting Fresh** in [`chains/CHAIN-DEVELOPER-GUIDE.md`](../../chains/CHAIN-DEVELOPER-GUIDE.md).

---

## Concurrent build issues

If you've got multiple builds running on the same provider, they share the queue. A slow build can back up others behind it. The queue view (Operations → Queue) shows this.

To serialize: set `concurrency: 1` on the relevant steps, or reduce the number of concurrent pyramid builds.

---

## When all else fails

Full reset of the specific pyramid:

```bash
sqlite3 "$HOME/Library/Application Support/wire-node/pyramid.db" \
  "DELETE FROM pipeline_steps WHERE slug='my-pyramid';
   DELETE FROM pyramid_nodes WHERE slug='my-pyramid';
   DELETE FROM pyramid_evidence WHERE slug='my-pyramid';
   DELETE FROM pyramid_web_edges WHERE slug='my-pyramid';"
```

Then trigger a fresh build from the UI. This destroys all pyramid state for one slug while leaving the slug registration and its config intact.

**Note:** direct SQL on `pyramid.db` is a last resort. It bypasses Agent Wire Node's invariants. Back up first.

For a full factory reset, see [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md).

---

## Where to go next

- [`A0-common-issues.md`](A0-common-issues.md) — grab bag.
- [`A2-provider-and-network-errors.md`](A2-provider-and-network-errors.md) — provider / network.
- [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md) — DADBEAR issues.
- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — what to look at.
