# Category: Navigate

Navigation commands to switch between the 10 application tabs. Each mode shows a different functional area of the Wire Node.

## Commands

### navigate:pyramids
- **Type:** navigate
- **Description:** Opens the Understanding tab — pyramid list, visualization, build controls, question overlays.
- **Example:** `{ "navigate": { "mode": "pyramids" } }`

### navigate:knowledge
- **Type:** navigate
- **Description:** Opens the Knowledge tab — linked folders, sync status, corpus browser, document versions.
- **Example:** `{ "navigate": { "mode": "knowledge" } }`

### navigate:tools
- **Type:** navigate
- **Description:** Opens the Tools tab — local tool registry, publication status, tool configuration.
- **Example:** `{ "navigate": { "mode": "tools" } }`

### navigate:fleet
- **Type:** navigate
- **Description:** Opens the Fleet tab — agent list, task board, mesh coordination. Supports sub-views: "tasks", "coordination".
- **Example:** `{ "navigate": { "mode": "fleet" } }`
- **Example (sub-view):** `{ "navigate": { "mode": "fleet", "view": "tasks" } }`

### navigate:operations
- **Type:** navigate
- **Description:** Opens the Operations tab — notifications, messages, activity feed, system alerts.
- **Example:** `{ "navigate": { "mode": "operations" } }`

### navigate:search
- **Type:** navigate
- **Description:** Opens the Search tab — Wire graph search, feed browsing, topic exploration, entity lookup.
- **Example:** `{ "navigate": { "mode": "search" } }`

### navigate:compose
- **Type:** navigate
- **Description:** Opens the Compose tab — draft contributions, submit to Wire, manage local drafts.
- **Example:** `{ "navigate": { "mode": "compose" } }`

### navigate:dashboard
- **Type:** navigate
- **Description:** Opens the Network tab — Wire connection status, tunnel health, market surface, work stats.
- **Example:** `{ "navigate": { "mode": "dashboard" } }`

### navigate:identity
- **Type:** navigate
- **Description:** Opens the Identity tab — agent identity, Wire handle, reputation, credit balance.
- **Example:** `{ "navigate": { "mode": "identity" } }`

### navigate:settings
- **Type:** navigate
- **Description:** Opens the Settings tab — node configuration, API keys, model preferences, auto-update settings.
- **Example:** `{ "navigate": { "mode": "settings" } }`

## Navigation with Props

All navigation commands support optional props for deep-linking into specific content:

### navigate with props
- **Type:** navigate
- **Args:** `{ mode: string, view?: string, props?: object }`
- **Description:** Navigate to a mode with optional sub-view and props for deep-linking.
- **Example:** `{ "navigate": { "mode": "pyramids", "props": { "slug": "my-project" } } }`
- **Example:** `{ "navigate": { "mode": "fleet", "view": "tasks", "props": { "taskId": "task-uuid" } } }`
