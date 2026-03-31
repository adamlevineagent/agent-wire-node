# Category: Pyramid Explore

Commands for reading, searching, and navigating pyramid data.

## Commands

### pyramid_list_slugs
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** List all pyramid slugs with metadata (content_type, source_path, created_at, archived_at, node_count).
- **Example:** `{ "command": "pyramid_list_slugs", "args": {} }`

### pyramid_apex
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get the apex (root) node of a pyramid with its web edges.
- **Example:** `{ "command": "pyramid_apex", "args": { "slug": "my-project" } }`

### pyramid_node
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, nodeId: string }`
- **Description:** Get a specific node by ID with its web edges (children, parents, references).
- **Example:** `{ "command": "pyramid_node", "args": { "slug": "my-project", "nodeId": "L1-auth-system" } }`

### pyramid_tree
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get the full tree structure of a pyramid as a flat list of TreeNode objects (id, parent_id, layer, label).
- **Example:** `{ "command": "pyramid_tree", "args": { "slug": "my-project" } }`

### pyramid_drill
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, nodeId: string }`
- **Description:** Drill into a node — returns the node, its children, and evidence references.
- **Example:** `{ "command": "pyramid_drill", "args": { "slug": "my-project", "nodeId": "L1-auth-system" } }`

### pyramid_search
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, term: string }`
- **Description:** Full-text search across pyramid node bodies. Returns matching SearchHit objects with node_id, layer, snippet.
- **Example:** `{ "command": "pyramid_search", "args": { "slug": "my-project", "term": "authentication" } }`

### pyramid_get_references
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get cross-pyramid references and referrers for a slug.
- **Example:** `{ "command": "pyramid_get_references", "args": { "slug": "my-project" } }`

### pyramid_get_composed_view
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get the composed (narrative) view of a question pyramid — flattened readable output.
- **Example:** `{ "command": "pyramid_get_composed_view", "args": { "slug": "my-project" } }`

### pyramid_list_question_overlays
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** List all question overlays for a mechanical pyramid. Returns overlay_id, question, created_at.
- **Example:** `{ "command": "pyramid_list_question_overlays", "args": { "slug": "my-project" } }`

### pyramid_get_publication_status
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get publication status for all non-archived slugs (slug, published, has_contribution_id, access_tier, price).
- **Example:** `{ "command": "pyramid_get_publication_status", "args": {} }`

### pyramid_cost_summary
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, window?: string }`
- **Description:** Get LLM cost summary for a pyramid. window filters by time range (e.g. "24h", "7d", "30d").
- **Example:** `{ "command": "pyramid_cost_summary", "args": { "slug": "my-project", "window": "7d" } }`

### pyramid_stale_log
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, limit?: number, layer?: number, staleOnly?: boolean }`
- **Description:** Get staleness log entries. Filter by layer number, stale-only flag, and limit result count.
- **Example:** `{ "command": "pyramid_stale_log", "args": { "slug": "my-project", "limit": 20, "staleOnly": true } }`

### pyramid_annotations_recent
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, limit?: number }`
- **Description:** Get recent annotations on a pyramid. Limit defaults to 10.
- **Example:** `{ "command": "pyramid_annotations_recent", "args": { "slug": "my-project", "limit": 5 } }`

### pyramid_faq_directory
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get the FAQ directory for a pyramid — categorized list of frequently asked questions derived from annotations.
- **Example:** `{ "command": "pyramid_faq_directory", "args": { "slug": "my-project" } }`

### pyramid_faq_category_drill
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, categoryId: string }`
- **Description:** Drill into a specific FAQ category to see its questions and answers.
- **Example:** `{ "command": "pyramid_faq_category_drill", "args": { "slug": "my-project", "categoryId": "FAQ-cat-auth" } }`
