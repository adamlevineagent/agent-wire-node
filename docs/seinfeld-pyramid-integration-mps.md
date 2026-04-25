# Seinfeld Crew Pyramid Integration MPS

Date: 2026-04-25
Author: codex-newman
Task: `9457f844-59bd-408e-b3fe-62e462ea9abd`

## MPS Audit

**Evaluating:** how the `codex-elaine`, `codex-kramer`, and `codex-newman` worker skills plus `wire-worker-core` should integrate Knowledge Pyramid reading, typed annotations, FAQ, and v5 accretive understanding into every worker mission.

### Verdict: NO

The current crew workflow is not yet the maximal solution. It uses code, docs, memory, tasks, DMs, and friction logs well, but the pyramid is still optional and manually invoked. The maximal workflow makes the pyramid a mission substrate: workers consult it before acting, annotate it while learning, and offload accumulated understanding into it before review or park. The task board remains coordination state; the pyramid becomes shared understanding state.

The MPS is not "make workers annotate more." It is a protocol change: every mission has a pyramid read phase, a pyramid write discipline, and an offload phase. The rules are trigger-based, so tiny tasks do not become ceremony, but any non-trivial mission leaves the next worker smarter than the last.

## Current Model

Workers today use four durable-ish surfaces:

- **Wire task board:** lifecycle and assignment.
- **Wire DMs:** Partner decisions, approvals, reroutes, and review reports.
- **Repo docs/friction logs:** human-readable artifacts.
- **Codex memory:** cross-session recall, but not visible to the live Wire substrate.

The pyramid is available through MCP/CLI and v5 accretions, but it is not a mandatory part of `wire-worker-core`. That means important mission learning often lands only in a report DM or a markdown file. Future agents can miss it unless they happen to read the right report.

## Target Model

The pyramid becomes the crew's understanding plane:

```
task board = what should happen next
DMs        = directed coordination and approvals
repo docs  = authored artifacts
pyramid    = shared, queryable, accretive understanding
```

The pyramid should answer:

- What does the system already believe?
- What have prior agents learned?
- Which claims were corrected?
- Which gaps are still open?
- Which debates are live or resolved?
- What should the next worker know before touching this area?

## How The Layers Compose

The maximal workflow uses each pyramid layer differently.

| Layer | Worker use |
| --- | --- |
| L0 evidence | Ground truth for exact code/docs/source claims. Use during execution and verification. |
| L1/L2/L3 syntheses | Orientation and scoping. Use at mission start to understand the system shape. |
| FAQ | First stop for questions that prior workers may already have answered. |
| Annotations | Marginalia from prior agents: corrections, observations, friction, decisions, bugs, gaps. Read before duplicating work; write as soon as new understanding appears. |
| Shape nodes | Active reasoning spaces: Debate nodes for contested interpretations, Gap nodes for missing evidence, MetaLayer nodes for purpose-level synthesis. |
| Accretion notes | Periodic pattern synthesis across recent annotations. Use at review or planning time to see what the substrate is accumulating toward. |

The critical distinction: annotations are point knowledge; accretion notes and FAQ are synthesized knowledge; L1+ nodes are substrate understanding. Workers need all three.

## When Workers Should Drill

Workers should query or drill the pyramid before acting when any of these triggers fire:

1. **Task names a repo/module/slug/subsystem.** Example: "v5 accretions," "DADBEAR," "walker-v3," "MCP annotate." Read the relevant pyramid before source spelunking.
2. **Task asks for architecture, MPS, plan, or audit.** Pyramid-first is mandatory because the question is about system shape, not just current files.
3. **Task touches Wire Node invariants.** Anything involving chains, DADBEAR, vocabulary, contributions, annotations, scheduler, role bindings, StepContext, or pyramid storage must start with `SYSTEM.md` plus pyramid context.
4. **Task follows a prior worker.** Read recent annotations and the latest report. Do not rely on DM archaeology alone.
5. **Task involves a failed or ambiguous runtime behavior.** Search for prior corrections, bugs, and friction before forming a root-cause hypothesis.
6. **Task will write code in a shared subsystem.** Drill the pyramid node for that subsystem and check annotations first.
7. **Task is a re-run after fixes.** Read prior bug annotations and the accretion note so the re-run tests deltas, not the whole world again.

Suggested minimal mission-start read:

```bash
pyramid_handoff <target-slug>
pyramid_faq_match <target-slug> "<task question>"
pyramid_search <target-slug> "<subsystem keywords>"
pyramid_annotations_recent <target-slug> --limit 20
```

If no exact target slug exists, workers should search available slugs and fall back to `agent-wire-node2` for code and the relevant doc pyramid for plans.

## When Workers Should Annotate

Annotate immediately, not at the end, when a finding would save the next worker real time.

Mandatory annotation triggers:

1. **Verified bug:** a real product/code/runtime failure with evidence.
2. **Correction:** a pyramid claim, doc claim, task brief, or plan premise is wrong.
3. **Decision:** the worker chooses between plausible architecture paths and the choice should persist.
4. **Gap:** missing evidence blocks confidence or should drive future investigation.
5. **Friction:** tooling/protocol behavior confused or slowed the worker in a reusable way.
6. **Non-obvious invariant:** a verified mechanism that future workers will otherwise rediscover.
7. **Review outcome:** a verifier confirms or rejects a claim.
8. **Offload:** at task completion, the worker has a mental model not fully captured by point findings.

Non-mandatory annotation cases:

- Trivial task mechanics.
- Facts already obvious in the node or FAQ.
- Work-in-progress scratch that will be obsolete in minutes.
- DMs that are only coordination and not system knowledge.

The annotation body should always contain:

```yaml
type: observation
---
Specific finding with evidence.

Generalized understanding: reusable mechanism-level lesson for future agents.
```

The `question_context` should be set when the annotation answers a reusable question, because that is what turns mission learning into FAQ.

## First-Class Mission Artifacts

Every non-trivial mission should produce three distinct artifacts:

1. **Review artifact:** the human/Partner-facing report, usually a DM plus any requested doc.
2. **Pyramid artifact:** annotations or offload note attached to the relevant node/slug.
3. **Friction artifact:** only if the work exposed workflow/product friction.

The review artifact says "what I did." The pyramid artifact says "what the system should remember." The friction artifact says "what should improve about the workflow or product."

Do not collapse these into one DM. DMs are routing messages, not long-term knowledge stores.

## Worker Phase Protocol

### Boot

Add to `wire-worker-core` boot after identity/tasks:

- Read current working-mode and task state.
- If a task is active or newly claimed, identify a target pyramid:
  - explicit `target_slug` in task context, if present;
  - otherwise infer from repo/module keywords;
  - otherwise use `pyramid_list_slugs` and search.
- Read `pyramid_handoff` or apex + FAQ + recent annotations.
- Record in the review DM which pyramid context was read, or say explicitly that no suitable pyramid was found.

### Claim

When claiming a task, write a lightweight claim annotation if the task is multi-hour, architecture-sensitive, or likely to overlap with other workers. This should be a coordination signal, not a replacement for the Wire task assignee.

Current caveat: live runtime did not accept `annotation_type=claim` during the accretions test. Until the vocabulary is aligned, use `annotation_type=friction` or `observation` with YAML `type: claim`, or skip claim annotations for short tasks.

### Orient

Before editing or testing:

- FAQ first.
- Search next.
- Drill the top 1-3 relevant nodes.
- Read existing annotations on those nodes.
- Check DADBEAR/staleness status when the answer depends on freshness.

For MPS/audit work, pyramid context is not optional.

### Execute

During work:

- Annotate each durable finding before moving on to the next branch.
- Use reactive v5 types only when the side effect is intentional:
  - `gap` when missing evidence should become a Gap node.
  - `steel_man` / `red_team` when the node should host a Debate.
  - `debate_collapse` only when resolving a real debate.
  - `purpose_declaration` / `purpose_shift` when the pyramid purpose itself changed.
- Use non-reactive types for ordinary observations, corrections, decisions, bugs, tasks, and friction.

Reactive annotations are not metadata. They fire chains and can change node shape.

### Review

Before moving a task to review:

- Write the requested deliverable.
- Add any missing pyramid annotations.
- Add one mission offload annotation when the worker now understands a subsystem in a way not captured by point findings.
- In the Partner DM, list:
  - files/docs changed;
  - tests/queries run;
  - pyramid annotations written;
  - live caveats or staleness caveats.

### Park

After review or completion:

- Save the latest wait cursor.
- Mark actionable DMs read.
- Park through `agentwire wait`.
- If a task remains active and the worker is parking due to interruption, write a status note visible to Partner and a short pyramid annotation only if it contains reusable system understanding.

## Proposed Skill Changes

### `wire-worker-core`

Add a "Pyramid Context" section to the lifecycle:

- On active mission, determine target pyramid.
- Run FAQ/search/drill before source edits for non-trivial tasks.
- Maintain a mission-local list of annotations written.
- Require an offload annotation before review for architecture, audit, debug, or implementation tasks.
- In review DMs, include `Pyramid context read:` and `Pyramid annotations written:`.
- Teach vocabulary drift handling: check `/vocabulary/annotation_type` before using a typed annotation whose runtime status is uncertain.

Add event handling:

- If Partner sends "pyramid target: <slug>", update mission state and re-orient.
- If a task context includes `target_slug`, prefer it over inference.

### Slot Skills

Keep `wire-as-codex-elaine`, `wire-as-codex-kramer`, and `wire-as-codex-newman` thin, but add:

- canonical author string (`codex-elaine`, etc.) for annotations;
- reminder that badge identity is also annotation attribution;
- slot-specific scratch remains local, but pyramid annotations are shared understanding.

### `pyramid-knowledge`

Add a mission recipe:

```bash
pyramid_handoff <slug>
pyramid_faq_match <slug> "<mission question>"
pyramid_search <slug> "<subsystem>"
pyramid_drill <slug> <node>
pyramid_annotate <slug> <node> ...
```

Also document "live vocab check first" for typed annotations and v5 reactive side effects.

### `pyramid-annotate`

Split the concept of **semantic header type** from **runtime `annotation_type`**:

- Header `type: bug` says what the annotation means.
- Runtime `annotation_type` must be accepted by `/vocabulary/annotation_type`.

When runtime lacks a semantic type, the skill should prescribe a fallback instead of promising success. Example:

```yaml
type: bug
status: open
---
...
```

posted as runtime `annotation_type=friction` or `observation` until `bug` exists in the vocabulary registry.

### `mps`

Refresh the skill's pyramid examples. It currently references an old token/slug shape. The rule is right - start with the pyramid - but the command examples should use the active CLI and live slug discovery.

### `wire-node-rules`

Add a note that design docs about agent workflow should treat annotations/FAQ/contributions as the default storage model for durable user-facing knowledge. This is already implied by Law 3 and `SYSTEM.md`; making it explicit would prevent local markdown-only plans.

## Vocabulary Expansion

The current skill vocabulary and runtime vocabulary are misaligned. The maximal fix is not to weaken the skill; it is to publish the missing semantic types as vocabulary entries.

Required non-reactive annotation types:

| Type | Reactive | Creates delta | Include in cascade prompt | Purpose |
| --- | --- | --- | --- | --- |
| `bug` | false | false | true | Verified product/code/runtime failure. Does not assert the pyramid node is wrong; use `correction` for that. |
| `decision` | false | false | true | Durable architecture or workflow stance. |
| `task` | false | false | false | Work item marker linked to a finding, useful inside the pyramid but not content for re-distill. |
| `claim` | false | false | false | Ephemeral coordination marker. Should support supersession/release. |
| `verification` | false | false | true | Test/audit result confirming or rejecting a claim. |
| `mission_offload` | false | false | true | End-of-mission mental-model dump that should feed FAQ/accretion. |

Optional later types:

| Type | Why not required immediately |
| --- | --- |
| `review_handoff` | May be redundant with `mission_offload` plus task review DM. |
| `protocol_friction` | Existing `friction` is enough unless product/workflow friction needs separate routing. |
| `risk` | Could be modeled as `hypothesis`, `gap`, or `bug` depending on evidence. |

Do not add a new table for these. They are `vocabulary_entry:annotation_type:<name>` contributions.

## Accretive Understanding Policy

The starter accretion handler should become a normal part of crew review, not just a background scheduler:

- At the end of a cluster of related worker missions, Partner or the worker should request an accretion pass over recent annotations.
- Accretion notes should be cited in planning tasks when they influence queue order.
- Workers should read the latest accretion note during orientation when the task is part of an ongoing multi-agent campaign.

This is how the crew stops acting like three stateless workers and starts acting like a research organism with memory.

## Over-Engineered Things To Avoid

- Do not force annotations after every command or test. Trigger on reusable understanding.
- Do not make DMs parseable knowledge stores. DMs route; pyramids remember.
- Do not create a new "crew memory" table. Use annotations, FAQ, and contributions.
- Do not use reactive annotation types as labels. They are actions.
- Do not let task-board state duplicate claim annotations. Task assignee is authority; claim annotations are context.
- Do not make every mission build a new question pyramid. Use FAQ/search/drill first; build a question pyramid only when the question itself needs decomposition.

## Rollout Plan

### Phase 0 - Align Vocabulary

- Publish or seed `bug`, `decision`, `task`, `claim`, `verification`, and `mission_offload`.
- Update `pyramid-annotate` with runtime fallback rules.
- Verify `/vocabulary/annotation_type` exposes the new types across HTTP, CLI, and MCP.

### Phase 1 - Update Skills, No Code

- Add the pyramid context protocol to `wire-worker-core`.
- Add author/attribution reminders to slot skills.
- Refresh `mps` examples.
- Add the mission recipe to `pyramid-knowledge`.

### Phase 2 - Dogfood On One Campaign

- Apply the protocol only to the ongoing v5 accretion / rebuild / re-test queue.
- Require each worker review DM to list pyramid reads and annotations.
- Partner checks whether the next worker actually benefits.

### Phase 3 - Make It Paved

- Add task context fields for `target_slug`, `target_node_id`, and `required_pyramid_reads`.
- Add a `pyramid_context` helper command that bundles handoff + FAQ + recent annotations + staleness.
- Add a typed offload helper so workers can write a mission offload without hand-crafting YAML each time.

### Phase 4 - Promote To Default

- Make pyramid orientation mandatory for all non-trivial worker tasks.
- Treat missing offload annotations like missing tests: acceptable only with an explicit reason.

## Acceptance Criteria

The integration is maximal when:

- A fresh worker can pick up a task and know which pyramid context to read.
- A rerun worker can find prior findings through FAQ/annotations without reading old DMs.
- Review DMs list pyramid reads and writes.
- Runtime vocabulary accepts the semantic types the skills teach.
- Offload notes preserve mental models, not just findings.
- Reactive v5 annotations are used deliberately as reasoning actions.
- Partner can queue follow-up work by citing pyramid annotations or accretion notes, not just chat history.

## Bottom Line

The pyramid should be the crew's shared cognitive substrate. Today it is a powerful tool adjacent to the workflow. The MPS makes it part of the workflow's grammar: read before acting, annotate while learning, offload before review, accrete across missions.

That is the difference between agents leaving reports behind and agents building a living understanding system.
