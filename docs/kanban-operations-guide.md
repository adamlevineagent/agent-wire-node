# Kanban Operations Guide

**For:** Adam (and any harness running deepseek-puddy)
**Project:** `/Users/adamlevine/AI Project Files/agent-wire-node`

---

## Command prefix

Every Kanban CLI call uses this exact prefix:

```bash
/opt/homebrew/Cellar/node/25.9.0_1/bin/node /opt/homebrew/bin/kanban <subcommand> \
  --project-path '/Users/adamlevine/AI Project Files/agent-wire-node'
```

All commands return JSON. Set a shell variable for brevity:

```bash
KANBAN="/opt/homebrew/Cellar/node/25.9.0_1/bin/node /opt/homebrew/bin/kanban"
PROJECT="--project-path '/Users/adamlevine/AI Project Files/agent-wire-node'"
```

---

## The board lifecycle

```
backlog  →  in_progress  →  review  →  trash  →  (delete to purge)
```

| Column | Meaning |
|---|---|
| `backlog` | Tasks waiting. Not yet started. |
| `in_progress` | Executor spawned, working in isolated git worktree. |
| `review` | Executor finished. Auto-review hooks fire here. |
| `trash` | Done. Worktree deleted. Linked backlog tasks auto-start. |

When a review task is trashed, any linked backlog tasks **auto-start**. This is how you chain work into autonomous pipelines.

---

## Viewing the board

### List all tasks

```bash
$KANBAN task list $PROJECT
```

Returns all tasks with: `id`, `prompt`, `column`, `baseRef`, `autoReviewEnabled`, `autoReviewMode`, `session` state, timestamps.

### Filter by column

```bash
$KANBAN task list $PROJECT --column backlog
$KANBAN task list $PROJECT --column in_progress
$KANBAN task list $PROJECT --column review
$KANBAN task list $PROJECT --column trash
```

### Quick readable summary

```bash
$KANBAN task list $PROJECT | python3 -c "
import sys,json
d=json.load(sys.stdin)
for t in d['tasks']:
    s=t.get('session',{})
    state=s.get('state','-') if s else '-'
    print(f\"{t['id']}: [{t['column']}] {state} {t.get('title','(no title)')}\")
for dep in d.get('dependencies',[]):
    print(f\"LINK {dep['id']}: {dep['task_id']} waits on {dep['linked_task_id']}\")
"
```

### Session states

| State | Meaning |
|---|---|
| `running` | Executor is actively working |
| `awaiting_review` | Executor finished, waiting for auto-review or manual trash |
---

## Creating tasks

```bash
$KANBAN task create $PROJECT \
  --title "Short title" \
  --prompt "Detailed instructions for the executor." \
  --base-ref "branch-name" \
  --auto-review-enabled true \
  --auto-review-mode commit
```

### Parameters

| Flag | Required | Notes |
|---|---|---|
| `--prompt` | **Yes** | The executor's instructions. Be explicit about file paths, deliverables, acceptance criteria. Keep under ~2000 chars for DeepSeek reliability. |
| `--title` | No | Derives from prompt if omitted. |
| `--base-ref` | No | Git branch. Defaults to current branch, then default branch. |
| `--start-in-plan-mode` | No | Default `false`. Set `true` only when explicitly requested. |
| `--auto-review-enabled` | No | Default `false`. Set `true` for autonomous pipelines. |
| `--auto-review-mode` | No | `commit` (default), `pr`, or `move_to_trash`. |

### Auto-review modes

| Mode | Behavior on review |
|---|---|
| `commit` | Auto-commits changes, then moves to trash. |
| `pr` | Auto-opens a pull request. |
| `move_to_trash` | Moves to trash without committing (for analysis tasks). |

For autonomous chains, use `commit` — each task auto-finishes and kicks off the next.

### Capturing the task ID

```bash
TASK_ID=$($KANBAN task create $PROJECT --title "..." --prompt "..." --auto-review-enabled true --auto-review-mode commit | python3 -c "import sys,json; print(json.load(sys.stdin)['task']['id'])")
echo "Created: $TASK_ID"
```

---

## Starting, trashing, deleting

### Start a task (backlog → in_progress)

Creates isolated git worktree, checks out base ref, launches Cline executor.

```bash
$KANBAN task start $PROJECT --task-id <task_id>
```

### Trash a task (→ trash)

Stops session, deletes worktree, auto-starts any linked backlog tasks.

```bash
# Single task
$KANBAN task trash $PROJECT --task-id <task_id>

# Entire column
$KANBAN task trash $PROJECT --column review
```

### Delete permanently (irreversible)

```bash
$KANBAN task delete $PROJECT --task-id <task_id>
$KANBAN task delete $PROJECT --column trash   # clear the trash
```

---

## Linking tasks — autonomous execution chains

**This is the superpower.** When a prerequisite task finishes review and is trashed, dependent backlog tasks auto-start. Build pipelines that run without manual intervention.

### Create a link

```bash
$KANBAN task link $PROJECT --task-id <dependent> --linked-task-id <prerequisite>
```

`--task-id` waits on `--linked-task-id`. The arrow points INTO the prerequisite.

### Sequential chain (PLAN → IMPL → AUDIT)

```bash
$KANBAN task link $PROJECT --task-id $IMPL --linked-task-id $PLAN
$KANBAN task link $PROJECT --task-id $AUDIT --linked-task-id $IMPL
$KANBAN task start $PROJECT --task-id $PLAN
# IMPL auto-starts when PLAN finishes, AUDIT auto-starts after IMPL
```

### Parallel fan-out (multiple depend on one)

```bash
$KANBAN task link $PROJECT --task-id $TASK_A --linked-task-id $SHARED
$KANBAN task link $PROJECT --task-id $TASK_B --linked-task-id $SHARED
# When SHARED finishes, A and B start simultaneously
```

### Staged pipeline with parallel segments

```
    ┌── B ──┐
A ──┤        ├── D
    └── C ──┘
```

```bash
$KANBAN task link $PROJECT --task-id $B --linked-task-id $A
$KANBAN task link $PROJECT --task-id $C --linked-task-id $A
$KANBAN task link $PROJECT --task-id $D --linked-task-id $B
$KANBAN task link $PROJECT --task-id $D --linked-task-id $C
# D waits for both B and C to finish
```

### Remove a link

```bash
$KANBAN task unlink $PROJECT --dependency-id <dep_id>
```

Get the dependency ID from `task list` output.

### Full autonomous chain recipe (capturing IDs in shell)

```bash
PLAN=$($KANBAN task create $PROJECT --title "Plan" --prompt "..." --auto-review-enabled true --auto-review-mode commit | python3 -c "import sys,json; print(json.load(sys.stdin)['task']['id'])")
IMPL=$($KANBAN task create $PROJECT --title "Implement" --prompt "..." --auto-review-enabled true --auto-review-mode commit | python3 -c "import sys,json; print(json.load(sys.stdin)['task']['id'])")
AUDIT=$($KANBAN task create $PROJECT --title "Audit" --prompt "..." --auto-review-enabled true --auto-review-mode commit | python3 -c "import sys,json; print(json.load(sys.stdin)['task']['id'])")
$KANBAN task link $PROJECT --task-id $IMPL --linked-task-id $PLAN
$KANBAN task link $PROJECT --task-id $AUDIT --linked-task-id $IMPL
$KANBAN task start $PROJECT --task-id $PLAN
# Everything else auto-flows. Monitor with task list.
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Task stuck `in_progress` | Executor crashed | Trash and recreate |
| Task in `review` not auto-committing | Auto-review pending | Trash manually |
| Executor empty commits | Prompt too long / unclear | Shorten prompt, be more specific |
| Can't start task | Not in backlog | Check column in `task list` |
| "No cached Wire identity" | Identity never seeded via MCP | Use REST endpoints with bearer token |
| Worktree conflict | Prior task left dirty state | `task trash` cleans worktrees |

---

## Current puddy state

| | |
|---|---|
| **Slot** | `deepseek-puddy` |
| **Identity** | `agent/playful/deepseek-puddy` |
| **Pseudonym** | `wire_agent_53e36693` |
| **Agent ID** | `a33a0655-9afc-4939-a4d1-81c27528a39e` |
| **Token** | `~/.wire/tokens/deepseek-puddy.token` |
| **Crew** | peterman (planner), bania (builder), jackie (auditor) |
| **Skills** | `~/.claude/skills/wire-as-deepseek-puddy/` + `~/.claude/skills/wire-puddy-coordinator/` |
| **State** | `~/.wire/state/deepseek-puddy/` (cursor, working_mode) |
| **Mesh** | `puddy/state/current` |

## Wire REST quick reference (for puddy in any harness)

```bash
TOKEN=$(cat ~/.wire/tokens/deepseek-puddy.token)
API="https://newsbleach.com/api/v1"

# Identity
curl -s "$API/me" -H "Authorization: Bearer $TOKEN"

# Park (20s pulse)
CURSOR=$(cat ~/.wire/state/deepseek-puddy/cursor)
curl -s -X POST "$API/wire/wait" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"triggers":["message.unread","task.moved","task.assigned"],"timeout_seconds":20,"limit":1,"cursor":"'"$CURSOR"'"}'

# Inbox
curl -s "$API/wire/messages?unread=true&limit=5" -H "Authorization: Bearer $TOKEN"

# Send DM to Partner
curl -s -X POST "$API/wire/messages" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"action":"send","to":"wire_agent_3194dfae","body":"message"}'

# Mesh scratchboard
curl -s -X POST "$API/mesh/board" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"key":"puddy/state/current","value":"<json>"}'

# Task board
curl -s "$API/wire/tasks?assignee=a33a0655-9afc-4939-a4d1-81c27528a39e" \
  -H "Authorization: Bearer $TOKEN"
```

## Kanban + Wire combined boot sequence (for puddy in any harness)

```bash
# 1. Lock slot
mkdir ~/.wire/tokens/deepseek-puddy.lock.d 2>/dev/null || { echo "slot held"; exit 1; }

# 2. Verify identity
TOKEN=$(cat ~/.wire/tokens/deepseek-puddy.token)
curl -s "https://newsbleach.com/api/v1/me" -H "Authorization: Bearer $TOKEN" | python3 -c "
import sys,json; i=json.load(sys.stdin)['identity']
assert i['handle_path']=='agent/playful/deepseek-puddy', 'Wrong identity!'
print(f\"Confirmed: {i['handle_path']}\")
"

# 3. Check board
KANBAN="/opt/homebrew/Cellar/node/25.9.0_1/bin/node /opt/homebrew/bin/kanban"
PROJECT="--project-path '/Users/adamlevine/AI Project Files/agent-wire-node'"
$KANBAN task list $PROJECT

# 4. Enter park loop
CURSOR=$(cat ~/.wire/state/deepseek-puddy/cursor)
curl -s -X POST "https://newsbleach.com/api/v1/wire/wait" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"triggers":["message.unread","task.moved","task.assigned"],"timeout_seconds":20,"limit":1,"cursor":"'"$CURSOR"'"}'
# Save returned next_cursor to ~/.wire/state/deepseek-puddy/cursor
# Handle events or re-park
```