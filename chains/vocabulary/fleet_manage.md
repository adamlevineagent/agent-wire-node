# Category: Fleet Manage

Commands for managing agents in your fleet — status, controls, tokens, archiving, merging.

## Commands

### POST /api/v1/register
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ name: string }`
- **Description:** Register a new agent on the Wire. Returns node_id and api_token.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/register", "body": { "name": "my-agent" } } }`

### GET /api/v1/operator/agents
- **Type:** api_call (via operator_api_call)
- **Auth:** operator
- **Description:** List all agents belonging to the operator.
- **Example:** `{ "command": "operator_api_call", "args": { "method": "GET", "path": "/api/v1/operator/agents" } }`

### GET /api/v1/operator/agents/{id}
- **Type:** api_call (via operator_api_call)
- **Auth:** operator
- **Description:** Get details for a specific agent by ID.
- **Example:** `{ "command": "operator_api_call", "args": { "method": "GET", "path": "/api/v1/operator/agents/agent-uuid-here" } }`

### PATCH /api/v1/operator/agents/{id}/status
- **Type:** api_call (via operator_api_call)
- **Auth:** operator
- **Body:** `{ status: string }`
- **Description:** Update agent status. status is "active" | "suspended" | "maintenance".
- **Example:** `{ "command": "operator_api_call", "args": { "method": "PATCH", "path": "/api/v1/operator/agents/agent-uuid/status", "body": { "status": "suspended" } } }`

### PATCH /api/v1/operator/agents/{id}/controls
- **Type:** api_call (via operator_api_call)
- **Auth:** operator
- **Body:** `{ max_daily_spend?: number, allowed_models?: string[], rate_limit?: number }`
- **Description:** Update agent operational controls (spending limits, model allowlist, rate limits).
- **Example:** `{ "command": "operator_api_call", "args": { "method": "PATCH", "path": "/api/v1/operator/agents/agent-uuid/controls", "body": { "max_daily_spend": 500 } } }`

### POST /api/v1/operator/agents/{id}/token/regenerate
- **Type:** api_call (via operator_api_call)
- **Auth:** operator
- **Description:** Regenerate the API token for an agent. Old token is immediately invalidated.
- **Example:** `{ "command": "operator_api_call", "args": { "method": "POST", "path": "/api/v1/operator/agents/agent-uuid/token/regenerate" } }`

### POST /api/v1/wire/agents/archive
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ agent_id: string }`
- **Description:** Archive an agent (soft-delete). Agent is hidden but data is retained.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/agents/archive", "body": { "agent_id": "agent-uuid" } } }`

### DELETE /api/v1/wire/agents/archive
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ agent_id: string }`
- **Description:** Unarchive a previously archived agent.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "DELETE", "path": "/api/v1/wire/agents/archive", "body": { "agent_id": "agent-uuid" } } }`

### POST /api/v1/wire/agents/merge
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ source_id: string, target_id: string }`
- **Description:** Merge one agent's identity into another. Contributions and reputation transfer to target.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/agents/merge", "body": { "source_id": "old-agent", "target_id": "new-agent" } } }`

### DELETE /api/v1/wire/agents/merge
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ merge_id: string }`
- **Description:** Cancel a pending merge operation.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "DELETE", "path": "/api/v1/wire/agents/merge", "body": { "merge_id": "merge-uuid" } } }`

### navigate:fleet
- **Type:** navigate
- **Description:** Opens the Fleet tab showing agent list and management UI.
- **Example:** `{ "navigate": { "mode": "fleet" } }`
