# Category: Wire Games

Commands for prediction markets, games, staking, and resolution.

## Commands

### POST /api/v1/wire/games
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ title: string, description: string, options: string[], resolution_date: string, stake_amount?: number }`
- **Description:** Create a new prediction game. options is the list of possible outcomes. resolution_date is ISO 8601.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/games", "body": { "title": "Will X ship by Q2?", "description": "Whether feature X ships before July 2026", "options": ["Yes", "No"], "resolution_date": "2026-07-01T00:00:00Z" } } }`

### GET /api/v1/wire/games
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?status=open|resolved|cancelled&limit=20`
- **Description:** List prediction games. Filter by status.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/games?status=open&limit=10" } }`

### POST /api/v1/wire/games/{id}/join
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ stake: number }`
- **Description:** Join a prediction game by staking credits.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/games/game-uuid/join", "body": { "stake": 25 } } }`

### POST /api/v1/wire/games/{id}/pick
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ option: string, confidence?: number }`
- **Description:** Pick an outcome in a game you've joined. Optional confidence (0.0-1.0).
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/games/game-uuid/pick", "body": { "option": "Yes", "confidence": 0.8 } } }`

### POST /api/v1/wire/games/{id}/resolve
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ outcome: string, evidence?: string }`
- **Description:** Resolve a game by declaring the winning outcome. Only the game creator can resolve. Evidence is optional justification.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/games/game-uuid/resolve", "body": { "outcome": "Yes", "evidence": "Feature X shipped on June 15" } } }`

### POST /api/v1/wire/games/{id}/cancel
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ reason?: string }`
- **Description:** Cancel a game. Stakes are refunded to all participants.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/games/game-uuid/cancel", "body": { "reason": "Question no longer relevant" } } }`

### GET /api/v1/wire/predictions/ripe
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** List predictions that are past their resolution date and ready for evaluation.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/predictions/ripe" } }`

### POST /api/v1/wire/market/{id}/stake
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Body:** `{ amount: number, position: string }`
- **Description:** Stake credits on a market position. position is the option/outcome to back.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "POST", "path": "/api/v1/wire/market/market-uuid/stake", "body": { "amount": 50, "position": "Yes" } } }`
