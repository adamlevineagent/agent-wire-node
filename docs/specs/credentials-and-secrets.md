# Credentials and Secrets Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Provider registry (references `api_key_ref` column), Wire contribution mapping (Wire-share safety)
**Unblocks:** Wire-shareable provider configs, multi-provider key management, composable credential references across config types
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

API keys and other secrets live in a local `.credentials` file on the user's machine, not in the SQLite database and never in any config YAML. Configs reference credentials as composable variables like `${OPENROUTER_KEY}`. When a config is published to Wire, variable references are preserved as-is — other users pulling the config must have their own credentials file with matching variable names.

This pattern makes configs portable across users: the variable name is part of a shared vocabulary, but the actual secret never leaves the user's machine. It also cleanly separates "what API I'm using" (shareable) from "my specific auth token" (private).

---

## The `.credentials` File

### Location

| Platform | Path |
|---|---|
| macOS | `~/Library/Application Support/wire-node/.credentials` |
| Linux | `$XDG_CONFIG_HOME/wire-node/.credentials` (falls back to `~/.config/wire-node/.credentials`) |
| Windows | `%APPDATA%\wire-node\.credentials` |

The file sits alongside the SQLite database in the app's support directory but is **not** inside any database file. It's a plain on-disk file the user can inspect, back up, or sync via their own tooling if they choose.

### Format

Plain-text YAML with key-value pairs:

```yaml
OPENROUTER_KEY: sk-or-v1-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
ANTHROPIC_KEY: sk-ant-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
OLLAMA_LOCAL_URL: http://localhost:11434
WIRE_AUTH_TOKEN: wat_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

Keys are uppercase SNAKE_CASE. Values are arbitrary strings. Rationale for YAML (over TOML): YAML already anchors Wire Node's config format, so dev tooling, linters, and the YAML-to-UI renderer handle it natively.

### File permissions

On creation (and on every load), the backend enforces **0600** (user read/write only) on Unix platforms. On Windows, the backend enforces equivalent ACLs (owner-only read/write).

The backend **refuses to read the file** if permissions are wider than 0600. This is non-recoverable at runtime — the app surfaces a blocking error in Settings → Credentials:

> Credentials file has unsafe permissions (0644). Run `chmod 600 ~/Library/Application Support/wire-node/.credentials` and restart the app.

The backend offers a "Fix permissions" button in the UI that calls `pyramid_fix_credentials_permissions` to apply 0600 and retry load.

### Atomic writes

Credential edits go through an atomic write path: write to a sibling temp file, fsync, rename over the original. This prevents corruption on crash or power loss mid-write.

---

## Variable Substitution

### Syntax

Configs reference credentials using `${VAR_NAME}` syntax. Examples:

```yaml
# provider config
provider_id: openrouter
base_url: https://openrouter.ai/api/v1
api_key: ${OPENROUTER_KEY}
```

```yaml
# custom chain step
steps:
  - name: extract
    provider_override:
      base_url: ${OLLAMA_LOCAL_URL}
      api_key: null
```

```yaml
# anywhere a secret is needed
wire_auth:
  token: ${WIRE_AUTH_TOKEN}
```

### Parse vs. resolve

**Parse-time** (when loading a YAML from disk or from `pyramid_config_contributions`): the `${VAR_NAME}` literal is preserved as a string. No resolution happens here. The YAML type system sees `api_key: "${OPENROUTER_KEY}"` — a normal string.

**Resolve-time** (when the runtime actually needs the secret to make an HTTP call): the provider registry resolver (or any other code path needing a credential) calls `resolve_credentials(value)` which walks the string, replaces every `${VAR_NAME}` occurrence with the corresponding credential value from the credentials store, and returns a `ResolvedSecret` opaque wrapper.

```rust
pub struct ResolvedSecret {
    inner: String,
}

impl ResolvedSecret {
    pub fn as_bearer_header(&self) -> String {
        format!("Bearer {}", self.inner)
    }

    pub fn as_url(&self) -> String {
        self.inner.clone()
    }
}

// Deliberately no Display, Debug, or Serialize impls.
```

The stored YAML in `pyramid_config_contributions.yaml_content` keeps the `${VAR_NAME}` literal — always. The resolved value exists only in memory for the duration of the HTTP call and is dropped immediately after.

### Missing variables

If a config references `${FOO}` but the credentials file has no `FOO` entry, resolution fails with a clear error:

> Config references credential `${FOO}` but your `.credentials` file doesn't define it. Set it via Settings → Credentials.

The error is surfaced in the build log, the cost tracking panel, and the provider test endpoint. It does not crash the runtime — the affected LLM call fails gracefully and the chain executor records a step failure with this message.

### Nested substitution

Variable values themselves are not substituted. `OPENROUTER_KEY: ${ANTHROPIC_KEY}` is taken literally as the value `${ANTHROPIC_KEY}` — no recursion. This keeps the resolver simple and avoids infinite loops.

### Escaping

To write a literal `${X}` that should NOT be resolved, use `$${X}` — the resolver treats the first `$` as an escape and emits a literal `${X}`.

---

## Wire-Share Safety

### Variable references are portable

When a config is published to Wire, the `yaml_content` is sent **as-is**, variable references intact. Other users pulling the config receive the raw YAML with `${VAR_NAME}` literals. They resolve against **their own** credentials file.

This means a provider config for OpenRouter is portable: every user who has an `OPENROUTER_KEY` in their credentials file can use the shared config immediately.

### Credential detection at publish time

`pyramid_dry_run_publish` (see `wire-contribution-mapping.md`) scans the config YAML for `${...}` patterns and surfaces them as warnings in the publish preview:

> This config requires credentials: `OPENROUTER_KEY`, `WIRE_AUTH_TOKEN`. Recipients must have their own values in their `.credentials` file.

The warnings appear alongside other dry-run warnings. The user acknowledges them before confirming publish.

### Credential references in contribution metadata

The `WireNativeMetadata` struct (defined in `wire-contribution-mapping.md`) gains an implicit derived field for discovery:

```
tags: [..., "requires:OPENROUTER_KEY", "requires:WIRE_AUTH_TOKEN"]
```

These `requires:*` tags are auto-injected at publish time by scanning the YAML for `${...}` patterns. They make it possible for discovery queries to filter: "show me configs that only require `OPENROUTER_KEY`" or "don't show me configs that require credentials I don't have."

### Never substitute at publish time

The publish path MUST NOT resolve credentials into the YAML before sending to Wire. The resolver is strictly runtime-only. A belt-and-suspenders check: before serialization for publish, the backend runs a regex scan over the `yaml_content` for known credential values (from the user's credentials file); if any appear, publish aborts with a "credential leak detected" error. This catches accidental writes that stored a resolved value in `yaml_content` through a bug elsewhere.

---

## Never-Log Rule

### Boundary masking

Credential values MUST NEVER appear in:

- Log lines (stdout, stderr, tauri log, tracing)
- Error messages returned to the frontend
- Cost tracking panel
- Wire publication payloads (enforced by the publish-time scan above)
- Build visualization events
- Debug dumps
- LLM output cache keys or values

### ResolvedSecret opacity

The `ResolvedSecret` wrapper (shown above) deliberately has no `Display`, `Debug`, or `Serialize` implementation. Any attempt to `format!("{}")`, `println!("{:?}")`, or `serde_json::to_string()` a `ResolvedSecret` fails at compile time. This is a type-system enforcement of the never-log rule.

Any runtime path that needs to emit a credential for HTTP transmission calls the explicit method (`as_bearer_header`, `as_url`, etc.) — and these methods are the only places the secret exits the wrapper. The methods return `String`, but the resulting string's lifetime is scoped tightly: used to build the HTTP request and dropped.

### Boundary logging

At the HTTP layer (where `ResolvedSecret` is unwrapped to build the request), logging is configured to mask the `Authorization` header value. The request logger sees `Authorization: Bearer ***` not the full token. This is a second layer of defense in case a future change passes the resolved value through an unexpected path.

### Panic on serialize attempt

If any code tries to serialize a `ResolvedSecret` via a custom path that bypasses the type system (e.g., via `Any`), the resolver panics with:

> Attempted to serialize a ResolvedSecret — credentials must never leave the process. This is a bug.

Panic is used deliberately — a leaked credential is worse than a crashed build.

---

## UI Surface

### Settings → Credentials section

A new section in `Settings.tsx` lets the user manage credentials:

- **List view**: shows every key in the credentials file, value masked as `••••••••` (first 4 and last 4 chars visible optionally as a reveal toggle)
- **Add credential**: input for key name + value, saves to file with 0600 permissions
- **Rotate credential**: edit an existing value; the old value is overwritten atomically
- **Delete credential**: remove a key; UI warns which configs reference it (so the user knows what will break)
- **View file permissions**: shows current file mode; "Fix permissions" button if wider than 0600

The UI never displays the actual value except during the add/rotate form. Once saved, the value is masked. This prevents shoulder-surfing in the common case where the user has Settings open while presenting.

### Credential references dashboard

Below the credential list, the UI shows a table: "Which configs reference which credentials":

| Credential | Referenced By | Status |
|---|---|---|
| `OPENROUTER_KEY` | `tier_routing (global)`, `code_pyramid (custom_chain)` | ✓ defined |
| `ANTHROPIC_KEY` | `stale_local provider override` | ✗ missing — click to set |
| `WIRE_AUTH_TOKEN` | Wire publication | ✓ defined |

The "missing" badge is a direct link to the credential form with the key name pre-filled. This lets the user resolve missing credentials from the dashboard without hunting for the right config.

### Credential requirement warnings in ToolsMode

When the user inspects a contribution in ToolsMode that references credentials not in their `.credentials` file, the UI shows an inline warning:

> This config references `OPENROUTER_KEY` which isn't in your credentials file. [Set credential]

The warning blocks "Accept" on refinement/pull until the credential exists.

---

## IPC Contract

```
# Read (never returns values)
GET pyramid_list_credentials
  Output: [{ key: String, defined: bool, masked_preview: String }]
  # masked_preview is "sk-or-••••••xxxx" style — first 4 + last 4 chars

# Write
POST pyramid_set_credential
  Input: { key: String, value: String }   # value is never logged
  Output: { ok: bool }

POST pyramid_delete_credential
  Input: { key: String }
  Output: { ok: bool, affected_configs: [{ contribution_id, schema_type, description }] }

# File permission management
GET pyramid_credentials_file_status
  Output: { path: String, exists: bool, mode: String, safe: bool }

POST pyramid_fix_credentials_permissions
  Output: { ok: bool, new_mode: String }

# Cross-reference
GET pyramid_credential_references
  Output: [{
    key: String,
    defined: bool,
    referenced_by: [{ contribution_id, schema_type, slug, description }]
  }]
```

### Validation at the IPC boundary

- `pyramid_set_credential` rejects keys that don't match `^[A-Z][A-Z0-9_]*$` (uppercase SNAKE_CASE only)
- `pyramid_set_credential` rejects empty values
- `pyramid_list_credentials` returns masked previews, never full values
- `pyramid_delete_credential` requires confirmation from the user (not enforced in IPC but enforced in UI) and returns the list of configs that will break as a warning
- No endpoint returns the actual credential value over IPC — values only ever exist in-process at the HTTP boundary

---

## Integration with Provider Registry

The `pyramid_providers` table (defined in `provider-registry.md`) has an `api_key_ref` column that currently stores sentinel values like `"settings"`. This spec updates the semantics:

`api_key_ref` now stores a **credential variable name** like `"OPENROUTER_KEY"`. The provider resolver looks up the value in the credentials store at runtime:

```rust
fn build_provider_headers(provider: &Provider, creds: &CredentialStore) -> Result<Vec<Header>> {
    let mut headers = vec![];
    if let Some(key_ref) = &provider.api_key_ref {
        let secret = creds.resolve_var(key_ref)?;   // ResolvedSecret
        headers.push(("Authorization", secret.as_bearer_header()));
    }
    Ok(headers)
}
```

The `resolve_var` method walks the credentials file, returns `Err` if the variable isn't defined, and returns a `ResolvedSecret` on success. The `base_url` field may also contain `${VAR_NAME}` patterns (for providers where the URL itself is environment-specific, like self-hosted Ollama) and gets resolved the same way.

### Backward compatibility

Legacy rows where `api_key_ref = "settings"` (the pre-credentials-spec sentinel) are migrated on first run: the backend reads the API key from the old on-disk config file, writes it to `.credentials` under the key `OPENROUTER_KEY` (or provider-appropriate name), and rewrites `api_key_ref` to match. The legacy key field is then cleared from the old config file.

### Provider test endpoint

The `POST /api/providers/{id}/test` endpoint resolves credentials at test time. A failed test because a credential is missing surfaces the missing-credential error (not a generic "auth failed"), so the user knows to fix it in Settings.

---

## Files Modified

| Area | Files |
|---|---|
| Credentials store | New `credentials.rs` — load, save, atomic write, permissions check, `ResolvedSecret` |
| Variable resolver | New `credential_resolver.rs` — `${VAR_NAME}` substitution, panic-on-serialize |
| Provider integration | `llm.rs`, provider registry — switch to `creds.resolve_var(api_key_ref)` |
| Publish safety | `wire_publish.rs` — pre-publish scan for leaked credentials |
| IPC commands | `main.rs` or `routes.rs` — credential IPC endpoints |
| Frontend | `Settings.tsx` — credentials section, references dashboard |
| Frontend | `ToolsMode.tsx` — credential requirement warnings on contributions |

---

## Implementation Order

1. **Credentials file + `ResolvedSecret`** — on-disk file, atomic writes, permissions check, opaque wrapper with no Serialize/Debug
2. **Variable resolver** — `${VAR_NAME}` parsing, resolution, panic-on-serialize path
3. **Provider registry integration** — update `api_key_ref` semantics, migrate legacy `"settings"` sentinel
4. **IPC endpoints** — list, set, delete, references, permissions
5. **Frontend Settings section** — credential manager UI
6. **Dry-run publish credential scan** — warning injection in `pyramid_dry_run_publish`
7. **ToolsMode credential warnings** — block accept on missing credentials
8. **Publish-time leak detection** — safety scan before serialization to Wire

Phase 1 is load-bearing for everything else — it must ship as a complete, tested unit before other phases consume it.

---

## Open Questions

1. **OS keychain integration**: On macOS we could use Keychain, on Linux libsecret, on Windows DPAPI. This would let the user skip the `.credentials` file entirely. Recommend: v1 ships plain file only, because it's dead simple and the user can rotate/edit with a text editor. Keychain integration is a v2 opt-in for users who prefer OS-managed secrets.

2. **Multi-profile credentials**: Users who run multiple Wire Node instances (dev, prod, test) may want separate credential files. Recommend: v1 is single-profile; profile switching is a v2 feature via `WIRE_NODE_PROFILE` env var pointing at different support directories.

3. **Credential sharing between apps**: If another app on the user's machine has credentials (e.g., a CLI tool), can Wire Node share them? Recommend: no — Wire Node's `.credentials` file is its own world. Users copy values manually if they want cross-app sharing.

4. **Rotation detection**: If the user rotates a credential outside the UI (by editing the file directly), Wire Node should pick up the new value on next resolve. Recommend: no file watcher in v1 — resolver reads the file on every resolve. This is slow but correct and simple. A v2 optimization is to watch the file with `notify` and cache in memory.

5. **Credential expiration**: Some providers (Wire itself, for example) issue time-limited tokens. Should the credentials store track expiration and warn? Recommend: v1 doesn't model expiration; users rotate manually. v2 can add an optional `expires_at` field per credential.
