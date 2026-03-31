# Category: Knowledge Docs

Commands for managing corpora and documents — listing, creating, versioning, diffing, publishing.

## Commands

### list_my_corpora
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** List corpora owned by the current user. Returns array of CorpusInfo (id, slug, title, document_count).
- **Example:** `{ "command": "list_my_corpora", "args": {} }`

### list_public_corpora
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** List publicly visible corpora on the Wire.
- **Example:** `{ "command": "list_public_corpora", "args": {} }`

### create_corpus
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, title: string }`
- **Description:** Create a new corpus with the given slug and title. Created as private visibility, precursor material class.
- **Example:** `{ "command": "create_corpus", "args": { "slug": "research-notes", "title": "Research Notes" } }`

### fetch_document_versions
- **Type:** command (Tauri invoke)
- **Args:** `{ documentId: string }`
- **Description:** Get version history for a document. Returns list of versions with timestamps, sizes, and diff stats.
- **Example:** `{ "command": "fetch_document_versions", "args": { "documentId": "doc-uuid" } }`

### compute_diff
- **Type:** command (Tauri invoke)
- **Args:** `{ oldDocId: string, newDocId: string }`
- **Description:** Compute a word-level diff between two document versions. Returns array of DiffHunk objects.
- **Example:** `{ "command": "compute_diff", "args": { "oldDocId": "doc-v1-uuid", "newDocId": "doc-v2-uuid" } }`

### pin_version
- **Type:** command (Tauri invoke)
- **Args:** `{ documentId: string, folderPath: string }`
- **Description:** Pin (download and cache) a specific document version to the local .versions directory inside the linked folder. Respects storage quota.
- **Example:** `{ "command": "pin_version", "args": { "documentId": "doc-uuid", "folderPath": "/Users/me/docs" } }`

### update_document_status
- **Type:** command (Tauri invoke)
- **Args:** `{ documentId: string, status: string }`
- **Description:** Update the status of a document. status is "draft" | "published" | "retracted".
- **Example:** `{ "command": "update_document_status", "args": { "documentId": "doc-uuid", "status": "published" } }`

### bulk_publish
- **Type:** command (Tauri invoke)
- **Args:** `{ corpusSlug: string }`
- **Description:** Publish all draft documents in a corpus in bulk. Processes in batches of 200.
- **Example:** `{ "command": "bulk_publish", "args": { "corpusSlug": "my-corpus" } }`

### open_file
- **Type:** command (Tauri invoke)
- **Args:** `{ path: string }`
- **Description:** Open a local file in the system default application.
- **Example:** `{ "command": "open_file", "args": { "path": "/Users/me/docs/report.md" } }`
