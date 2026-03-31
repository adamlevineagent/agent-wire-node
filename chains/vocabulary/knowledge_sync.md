# Category: Knowledge Sync

Commands for linking local folders to Wire corpora and syncing content.

## Commands

### link_folder
- **Type:** command (Tauri invoke)
- **Args:** `{ folderPath: string, corpusSlug: string, direction: string }`
- **Description:** Link a local folder to a Wire corpus for syncing. direction is "push" | "pull" | "bidirectional".
- **Example:** `{ "command": "link_folder", "args": { "folderPath": "/Users/me/docs", "corpusSlug": "my-corpus", "direction": "push" } }`

### unlink_folder
- **Type:** command (Tauri invoke)
- **Args:** `{ folderPath: string }`
- **Description:** Unlink a previously linked folder from its corpus.
- **Example:** `{ "command": "unlink_folder", "args": { "folderPath": "/Users/me/docs" } }`

### sync_content
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Trigger an immediate sync of all linked folders. Uploads changed files and downloads new remote documents.
- **Example:** `{ "command": "sync_content", "args": {} }`

### get_sync_status
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get current sync state — linked folders, sync progress, last sync time, auto-sync settings.
- **Example:** `{ "command": "get_sync_status", "args": {} }`

### set_auto_sync
- **Type:** command (Tauri invoke)
- **Args:** `{ enabled: boolean, intervalSecs?: number }`
- **Description:** Enable/disable automatic periodic sync. intervalSecs sets the sync interval (minimum 60 seconds).
- **Example:** `{ "command": "set_auto_sync", "args": { "enabled": true, "intervalSecs": 300 } }`

### navigate:knowledge
- **Type:** navigate
- **Description:** Opens the Knowledge tab showing linked folders, sync status, and corpus browser.
- **Example:** `{ "navigate": { "mode": "knowledge" } }`
