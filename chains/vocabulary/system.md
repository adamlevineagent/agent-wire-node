# Category: System

Commands for node configuration, authentication state, health, updates, and diagnostics.

## Commands

### get_config
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get the current Wire Node configuration including api_url, supabase_url, server_port, node_name, and runtime overlay values.
- **Example:** `{ "command": "get_config", "args": {} }`

### set_config
- **Type:** command (Tauri invoke)
- **Args:** `{ config: WireNodeConfig }`
- **Description:** Update Wire Node configuration. Currently a no-op placeholder (config is read-only at runtime).
- **Example:** `{ "command": "set_config", "args": { "config": {} } }`

### get_auth_state
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get the current authentication state — user_id, email, node_id, api_token presence, operator session status.
- **Example:** `{ "command": "get_auth_state", "args": {} }`

### logout
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Log out — clears auth state, removes session file, resets credits.
- **Example:** `{ "command": "logout", "args": {} }`

### get_health_status
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get node health — tunnel connectivity, API reachability, uptime, version.
- **Example:** `{ "command": "get_health_status", "args": {} }`

### check_for_update
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Check for application updates. Returns UpdateInfo with version, date, body, and whether an update is available.
- **Example:** `{ "command": "check_for_update", "args": {} }`

### install_update
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Download and install an available update. The app will restart after installation.
- **Example:** `{ "command": "install_update", "args": {} }`

### get_logs
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get the last 500 log lines from the Wire Node log file, newest first.
- **Example:** `{ "command": "get_logs", "args": {} }`

### get_node_name
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get the node's display name (derived from hostname or config).
- **Example:** `{ "command": "get_node_name", "args": {} }`

### is_onboarded
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Check if the node has completed onboarding (presence of onboarding marker file).
- **Example:** `{ "command": "is_onboarded", "args": {} }`

### get_credits
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get credit dashboard stats — balance, earned, spent, session totals.
- **Example:** `{ "command": "get_credits", "args": {} }`

### get_tunnel_status
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Get Cloudflare Tunnel status — connected, url, error state, last heartbeat.
- **Example:** `{ "command": "get_tunnel_status", "args": {} }`

### retry_tunnel
- **Type:** command (Tauri invoke)
- **Args:** `{}`
- **Description:** Retry establishing the Cloudflare Tunnel connection after a failure.
- **Example:** `{ "command": "retry_tunnel", "args": {} }`
