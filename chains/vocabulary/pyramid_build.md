# Category: Pyramid Build

Commands for creating pyramids, configuring build settings, and running builds.

## Commands

### pyramid_create_slug
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, contentType: string, sourcePath: string, referencedSlugs?: string[] }`
- **Description:** Create a new pyramid slug. contentType is "codebase" | "documents" | "question". sourcePath is the local directory to index.
- **Example:** `{ "command": "pyramid_create_slug", "args": { "slug": "my-project", "contentType": "codebase", "sourcePath": "/Users/me/project", "referencedSlugs": ["core-docs"] } }`

### pyramid_build
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Start a mechanical pyramid build for the given slug. Returns current BuildStatus.
- **Example:** `{ "command": "pyramid_build", "args": { "slug": "my-project" } }`

### pyramid_question_build
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, question: string, granularity?: number, maxDepth?: number, fromDepth?: number, characterization?: CharacterizationResult }`
- **Description:** Start a question pyramid build. granularity (default 3) controls sub-question fan-out. maxDepth (default 3) caps tree depth. fromDepth resumes from a specific depth. characterization seeds the build with a pre-run characterize result.
- **Example:** `{ "command": "pyramid_question_build", "args": { "slug": "my-project", "question": "How does auth work?", "granularity": 3, "maxDepth": 4 } }`

### pyramid_build_cancel
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Cancel a running pyramid build for the given slug.
- **Example:** `{ "command": "pyramid_build_cancel", "args": { "slug": "my-project" } }`

### pyramid_build_force_reset
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string }`
- **Description:** Force-reset a stuck build. Only works if the build has been running for >30 minutes.
- **Example:** `{ "command": "pyramid_build_force_reset", "args": { "slug": "my-project" } }`

### pyramid_set_config
- **Type:** command (Tauri invoke)
- **Args:** `{ apiKey?: string, authToken?: string, primaryModel?: string, fallbackModel1?: string, fallbackModel2?: string, useIrExecutor?: boolean }`
- **Description:** Set pyramid builder configuration. All fields optional — only provided fields are updated.
- **Example:** `{ "command": "pyramid_set_config", "args": { "primaryModel": "anthropic/claude-sonnet-4", "fallbackModel1": "google/gemini-2.0-flash-001" } }`

### pyramid_get_config
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get current pyramid builder config (api_key_set, auth_token_set, model names, use_ir_executor).
- **Example:** `{ "command": "pyramid_get_config", "args": {} }`

### pyramid_test_api_key
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Test the configured OpenRouter API key by making a small request. Returns model name on success.
- **Example:** `{ "command": "pyramid_test_api_key", "args": {} }`

### pyramid_set_access_tier
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, tier: string, price?: number, circles?: string }`
- **Description:** Set publication access tier for a pyramid. tier is "public" | "priced" | "circle" | "private". price is credit cost (required for "priced"). circles is comma-separated circle IDs (required for "circle").
- **Example:** `{ "command": "pyramid_set_access_tier", "args": { "slug": "my-project", "tier": "priced", "price": 50 } }`

### pyramid_set_absorption_mode
- **Type:** command (Tauri invoke)
- **Args:** `{ slug: string, mode: string, chainId?: string, rateLimit?: number, dailyCap?: number }`
- **Description:** Set how a pyramid absorbs external contributions. mode is "open" | "absorb-all" | "absorb-selective". chainId links to an action chain for selective mode. rateLimit (per hour) and dailyCap constrain absorption rate.
- **Example:** `{ "command": "pyramid_set_absorption_mode", "args": { "slug": "my-project", "mode": "absorb-all", "rateLimit": 10, "dailyCap": 100 } }`
