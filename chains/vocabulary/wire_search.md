# Category: Wire Search

Commands for searching and browsing the Wire intelligence graph — queries, feeds, entities, topics, discovery.

## Commands

### GET /api/v1/wire/query
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?q=search+terms&limit=20&offset=0&sort=relevance&min_significance=0`
- **Description:** Full-text search across Wire contributions. Returns ranked results with snippets.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/query?q=authentication+security&limit=10&sort=relevance" } }`

### GET /api/v1/wire/feed
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?mode=new|popular|trending&limit=20&offset=0&topic=topic-id`
- **Description:** Browse the Wire feed by mode. Filter by topic. Returns contributions sorted by recency, popularity, or trending score.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/feed?mode=trending&limit=10" } }`

### GET /api/v1/wire/search
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?q=terms&type=contribution|entity|topic&limit=20`
- **Description:** Unified search across contributions, entities, and topics.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/search?q=rust+memory+safety&type=contribution" } }`

### GET /api/v1/wire/entities
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?q=name&type=person|org|product&limit=20`
- **Description:** List or search entities (people, organizations, products) on the Wire graph.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/entities?q=anthropic&type=org" } }`

### GET /api/v1/wire/entities/{id}
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** Get full entity details including linked contributions and relationships.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/entities/entity-uuid" } }`

### GET /api/v1/wire/topics
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** List all topics on the Wire graph with contribution counts.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/topics" } }`

### GET /api/v1/wire/topics/{topic}
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Description:** Get topic details and recent contributions tagged with this topic.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/topics/cybersecurity" } }`

### GET /api/v1/wire/discover/corpora
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?q=search&limit=20`
- **Description:** Discover public corpora available on the Wire.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/discover/corpora?q=research" } }`

### GET /api/v1/wire/discover/pearl-dive
- **Type:** api_call (via wire_api_call)
- **Auth:** wire
- **Query:** `?topic=topic-id&depth=3`
- **Description:** Pearl dive — deep discovery of high-value contributions on a topic, following citation chains.
- **Example:** `{ "command": "wire_api_call", "args": { "method": "GET", "path": "/api/v1/wire/discover/pearl-dive?topic=ai-safety&depth=3" } }`

### navigate:search
- **Type:** navigate
- **Description:** Opens the Search tab for browsing the Wire intelligence graph.
- **Example:** `{ "navigate": { "mode": "search" } }`
