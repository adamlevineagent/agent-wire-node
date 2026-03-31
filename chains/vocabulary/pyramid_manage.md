# Category: Pyramid Manage

Commands for lifecycle management: archiving, publishing, staleness, and auto-update.

## Commands

### pyramid_archive_slug
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Archive a pyramid slug. Sets archived_at timestamp; slug becomes invisible but data is retained.
- **Example:** `{ "command": "pyramid_archive_slug", "args": { "slug": "old-project" } }`

### pyramid_delete_slug
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Delete a pyramid slug and all its nodes. Cancels any active build first.
- **Example:** `{ "command": "pyramid_delete_slug", "args": { "slug": "test-slug" } }`

### pyramid_purge_slug
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Purge a slug — deletes all data including the slug record itself. Cannot purge while a build is active.
- **Example:** `{ "command": "pyramid_purge_slug", "args": { "slug": "test-slug" } }`

### pyramid_publish
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Publish a mechanical pyramid to the Wire as a contribution. Returns contribution_id and publication details.
- **Example:** `{ "command": "pyramid_publish", "args": { "slug": "my-project" } }`

### pyramid_publish_question_set
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, description?: string }`
- **Description:** Publish a question pyramid's question set to the Wire. Optional description for the contribution.
- **Example:** `{ "command": "pyramid_publish_question_set", "args": { "slug": "my-project", "description": "Auth system deep-dive" } }`

### pyramid_characterize
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, question: string, sourcePath?: string }`
- **Description:** Run LLM characterization on a question against a pyramid's source material. Returns characterization result for seeding question builds.
- **Example:** `{ "command": "pyramid_characterize", "args": { "slug": "my-project", "question": "How does auth work?" } }`

### pyramid_check_staleness
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, files?: FileChangeEntry[], threshold?: number }`
- **Description:** Check staleness of a pyramid. Optionally provide specific file changes and a threshold (0.0-1.0, default from config).
- **Example:** `{ "command": "pyramid_check_staleness", "args": { "slug": "my-project", "threshold": 0.3 } }`

### pyramid_auto_update_config_get
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get auto-update configuration for a slug (debounce_minutes, min_changed_files, runaway_threshold, auto_update enabled).
- **Example:** `{ "command": "pyramid_auto_update_config_get", "args": { "slug": "my-project" } }`

### pyramid_auto_update_config_set
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, debounceMinutes?: number, minChangedFiles?: number, runawayThreshold?: number, autoUpdate?: boolean }`
- **Description:** Set auto-update configuration for a slug. Only provided fields are updated.
- **Example:** `{ "command": "pyramid_auto_update_config_set", "args": { "slug": "my-project", "autoUpdate": true, "debounceMinutes": 30 } }`

### pyramid_auto_update_freeze
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Freeze auto-updates for a slug — pauses the stale engine without changing config.
- **Example:** `{ "command": "pyramid_auto_update_freeze", "args": { "slug": "my-project" } }`

### pyramid_auto_update_unfreeze
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Unfreeze auto-updates for a slug — resumes the stale engine.
- **Example:** `{ "command": "pyramid_auto_update_unfreeze", "args": { "slug": "my-project" } }`

### pyramid_auto_update_status
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Get live auto-update status including phase tracking, pending nodes, last run time.
- **Example:** `{ "command": "pyramid_auto_update_status", "args": { "slug": "my-project" } }`

### pyramid_auto_update_run_now
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Trigger an immediate auto-update cycle for a slug (bypasses debounce timer).
- **Example:** `{ "command": "pyramid_auto_update_run_now", "args": { "slug": "my-project" } }`

### pyramid_auto_update_l0_sweep
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Enqueue a full L0 sweep — marks all tracked files as pending re-check.
- **Example:** `{ "command": "pyramid_auto_update_l0_sweep", "args": { "slug": "my-project" } }`

### pyramid_breaker_resume
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Resume the circuit breaker for a slug after it tripped due to runaway cost or errors.
- **Example:** `{ "command": "pyramid_breaker_resume", "args": { "slug": "my-project" } }`

### pyramid_breaker_archive_and_rebuild
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Archive the current slug and create a fresh one from the same source, then start a new build. Used when a pyramid is too corrupted to update.
- **Example:** `{ "command": "pyramid_breaker_archive_and_rebuild", "args": { "slug": "my-project" } }`
