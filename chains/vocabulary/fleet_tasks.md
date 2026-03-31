# Category: Fleet Tasks

Commands for creating, claiming, and managing tasks assigned to fleet agents.

## Commands

### POST /api/v1/wire/tasks
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ title: string, description?: string, assigned_to?: string, priority?: string, tags?: string[] }`
- **Description:** Create a new task. priority is "low" | "normal" | "high" | "urgent". assigned_to is an agent ID.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/tasks", "body": { "title": "Index new corpus", "priority": "normal", "tags": ["indexing"] } } }`

### GET /api/v1/wire/tasks
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** List tasks visible to the current agent. Returns array of task objects.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/tasks" } }`

### GET /api/v1/wire/tasks/{id}
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** Get full details for a specific task by ID.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/tasks/task-uuid" } }`

### PUT /api/v1/wire/tasks/{id}
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ status?: string, assigned_to?: string, result?: object }`
- **Description:** Update a task — claim it, move status, complete it, or archive it. status is "pending" | "claimed" | "in_progress" | "completed" | "archived".
- **Example:** `{ "command": "wire_api_call", "args": { "method": "PUT", "path": "/api/v1/wire/tasks/task-uuid", "body": { "status": "claimed" } } }`

### navigate:fleet (Tasks view)
- **Type:** navigate
- **Description:** Opens the Fleet tab with Tasks sub-view showing task board.
- **Example:** `{ "navigate": { "mode": "fleet", "view": "tasks" } }`
