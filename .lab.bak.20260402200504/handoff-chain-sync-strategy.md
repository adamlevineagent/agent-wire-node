# Rust Handoff: Chain sync strategy — source tree is canonical

## Status
**IMPLEMENTED** — compiled, built into Wire Node v0.2.0.

**Supersedes:** `handoff-chain-auto-sync.md` (the "always overwrite" approach was too blunt — it overwrites dev work with stale embedded defaults)

## The Problem
`ensure_default_chains()` currently always overwrites runtime chain files with embedded defaults on startup. But the embedded defaults in the binary are stale v0.1.0 placeholders. This destroys the v5.1.0 chains we've been developing in the source tree.

## The Fix: Two-tier resolution

### Tier 1: Source tree present (dev mode)
If the source tree `chains/` directory exists, **always copy from source tree → data dir**. Source tree is canonical. No version checks, no hashing — just overwrite.

Detection: In debug mode, the app already resolves `chains_dir` to the source tree (`../chains`). In release mode, check if the source tree path exists alongside the binary. If it does, use it.

```rust
let source_chains = source_tree_path.join("chains");
if source_chains.exists() {
    // Dev mode: source tree wins, copy everything
    copy_dir_recursive(&source_chains, &data_dir_chains)?;
    return Ok(());
}
```

### Tier 2: No source tree (release/standalone)
Use embedded defaults, but **only write files that don't already exist** (the original behavior before the "always overwrite" change). This preserves the user's runtime chain files across app restarts.

```rust
// Release mode: bootstrap only, don't overwrite
if !path.exists() {
    std::fs::write(&path, content)?;
}
```

### Release upgrades
When shipping a new binary with updated embedded chains, bump `schema_version` in the embedded YAML. The loader checks:

```rust
if !path.exists() {
    // New file, write it
    std::fs::write(&path, content)?;
} else if embedded_schema_version > runtime_schema_version {
    // Embedded is newer, upgrade
    std::fs::write(&path, content)?;
}
```

`schema_version` is already a field in every chain YAML. It's an integer — bump it when the chain format changes or when embedded defaults should override user runtime files.

## Summary

| Scenario | Behavior |
|----------|----------|
| Dev mode (source tree exists) | Source tree → data dir, always overwrite |
| Release, first run | Embedded defaults → data dir |
| Release, subsequent runs | Keep existing runtime files |
| Release, new binary with higher schema_version | Embedded defaults upgrade runtime files |

## Files
- `src-tauri/src/pyramid/chain_loader.rs` — `ensure_default_chains()`
- `src-tauri/src/main.rs` — `chains_dir` resolution (already has debug vs release logic)

## Revert
The previous "always overwrite" change should be reverted back to `if !path.exists()` until this two-tier logic is in place.
