# Category: Fleet Mesh

Commands for multi-agent coordination — shared blackboard, intent declarations, fleet status.
All mesh endpoints require the X-Wire-Thread header passed via the headers arg of wire_api_call.

## Commands

### GET /api/v1/mesh/status
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** Get mesh status — connected peers, heartbeat info, coordination state.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/mesh/status", "headers": { "X-Wire-Thread": "thread-id" } } }`

### GET /api/v1/mesh/board
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** Read the shared blackboard — a key-value store visible to all agents in the thread.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/mesh/board", "headers": { "X-Wire-Thread": "thread-id" } } }`

### POST /api/v1/mesh/board
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ key: string, value: any, ttl_seconds?: number }`
- **Description:** Write a key-value entry to the shared blackboard. Optional TTL for auto-expiry.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/mesh/board", "body": { "key": "progress", "value": { "phase": "indexing", "pct": 42 } }, "headers": { "X-Wire-Thread": "thread-id" } } }`

### GET /api/v1/mesh/intent
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** List all declared intents from agents in the thread.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/mesh/intent", "headers": { "X-Wire-Thread": "thread-id" } } }`

### POST /api/v1/mesh/intent
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ action: string, target?: string, metadata?: object }`
- **Description:** Declare intent to perform an action, so other agents can coordinate and avoid conflicts.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/mesh/intent", "body": { "action": "rebuild", "target": "slug:my-project" }, "headers": { "X-Wire-Thread": "thread-id" } } }`

### DELETE /api/v1/mesh/intent
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ intent_id: string }`
- **Description:** Withdraw a previously declared intent.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "DELETE", "path": "/api/v1/mesh/intent", "body": { "intent_id": "intent-uuid" }, "headers": { "X-Wire-Thread": "thread-id" } } }`

### navigate:fleet (Coordination view)
- **Type:** navigate
- **Description:** Opens the Fleet tab with Coordination sub-view showing mesh status and blackboard.
- **Example:** `{ "navigate": { "mode": "fleet", "view": "coordination" } }`
