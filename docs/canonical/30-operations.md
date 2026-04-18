# Operations (notifications, messages, live queue)

The **Operations** mode is the real-time dashboard of what your Agent Wire Node is doing right now. Notifications from the Wire, messages from other operators, active builds and DADBEAR ticks, the live inference queue. When something surprising happens on your node, this is where you look.

---

## The four tabs

**Operations** has four tabs, each focused on one kind of activity:

- **Notifications** — events from the Wire and from your node that want your attention.
- **Messages** — direct messages from other operators.
- **Active** — operations currently running on your node (builds, DADBEAR ticks, sync).
- **Queue** — the live LLM inference queue, per model.

---

## Notifications

Notifications are structured events. Each one has:

- **Icon** and **event type** (e.g. "pyramid absorbed question", "provider credits low", "DADBEAR breaker tripped").
- **Source agent** (who/what generated it — a fleet member, a corpus update, an infrastructure signal).
- **Read/unread** indicator.
- **Timestamp** (relative — "5 minutes ago").

Clicking a notification opens its detail card, which usually links to the affected object (pyramid, corpus, agent) and gives you options (acknowledge, reply, dismiss).

### Filtering

- **All / action required / informational** — quick triage.
- **Source**: all / fleet / corpora / infrastructure / system.
- **Time**: today / this week / all time.

### What generates notifications

Common event types:

- **Pyramid-level:** absorption triggered, build failed, DADBEAR breaker tripped, supersession waiting for acknowledgment.
- **Contribution-level:** a pulled contribution has an update available, migration is flagged, publication succeeded/failed.
- **Market-level:** your compute offer filled, your inference request completed, credit balance low.
- **Fleet-level:** an agent you own showed up / went offline; a peer joined the fleet.
- **Infrastructure:** tunnel dropped, provider outage, disk usage above threshold.

You can mute specific event types from Settings if you find certain categories noisy.

---

## Messages

Messages are direct communications. They can come from:

- **Other operators** on the Wire (via the Wire's messaging channel — typically scoped to a contribution context, like "I'm consuming your pyramid and have a question").
- **Agents** that need operator attention (a task completed, an escalation).
- **Yourself** — reminders you left.

Each message shows sender, body preview, read/unread state, timestamp. Clicking opens the full thread.

Wire messaging is not a general chat system. It's context-attached: messages are usually tied to a specific contribution, pyramid, or market interaction, and they're sparse. Use dedicated chat tools if you want general-purpose conversation.

---

## Active operations

The Active tab is a live view of everything your node is currently running:

- **Pyramid builds** — with progress, step, elapsed time, assigned model.
- **DADBEAR ticks** — which pyramid, which layer, how much pending work.
- **Sync operations** — folder syncs in progress.
- **Wire operations** — active publishes, pulls, discovery queries.
- **Compute market jobs** — both dispatched (buying) and accepted (selling).

Each row has progress indicators and click-through to the operation's detail view. Rows disappear when operations complete (moving the completed records to their respective history views — build history in Understanding → Builds, and so on).

This is where you come when you think something might be stuck. If an operation has been at the same state for an unreasonable time, the detail view shows logs and offers cancel/retry.

---

## Queue (live inference)

The Queue tab shows the live state of your LLM inference queues. Your node maintains one queue per model — each model has its own worker and progresses through jobs in order.

Per-model view:

- **Model ID** — e.g. `inception/mercury-2`, `gemma3:27b`, `anthropic/claude-sonnet-4-5`.
- **Queue depth** — how many jobs are waiting.
- **Executing** — yes/no; if yes, which job.
- **Current job source** — local build, fleet peer, compute market.
- **Completed count** — total jobs processed by this model on this node (since app start).
- **Average latency** — for the last N completed jobs.
- **Fleet job count** and **market job count** — breakdown by where work came from.
- **Recent jobs** — the last 10, with latency and source.

This view is especially useful when you're running as a compute market provider. You can see at a glance which models are busy, which are idle, whether fleet work is backlogging behind market work, and so on.

### When a queue is growing

Growing queue = the model can't keep up with incoming work. Causes:

- **The model itself is slow** (big local model, slow provider).
- **Incoming work is spiking** (a big build just started; multiple concurrent DADBEAR ticks).
- **The model is serving the market and receiving many external requests.**

Resolutions:

- Route some work to a different tier (Settings → Tier Routing).
- Pause DADBEAR on non-urgent pyramids.
- If you're market-serving, consider lowering your offer capacity temporarily.

---

## Why Operations matters

Most of what Agent Wire Node does is autonomous: DADBEAR runs, builds run, agents query, the market flows. Operations is the surface where you notice when any of that goes off rails.

A few habits to develop:

- **Glance at Notifications each session.** The things that need your attention are here, not buried in other tabs.
- **Check Queue when something feels slow.** A long queue explains perceived slowness; a suddenly empty queue during a build means the worker got unstuck.
- **Read Active when you started something and want to know if it finished.** Especially useful for long builds you left to run.
- **Use Messages for context exchange.** Quick notes tied to specific pyramids or contributions.

---

## Where to go next

- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — DADBEAR activity in detail.
- [`29-fleet.md`](29-fleet.md) — the agents whose activity surfaces here.
- [`70-compute-market-overview.md`](70-compute-market-overview.md) — market jobs that flow through the queue.
- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — deeper diagnostics.
