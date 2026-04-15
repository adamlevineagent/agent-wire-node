# Rust Handoff: Chain auto-sync on restart

## Status
**SUPERSEDED** by `handoff-chain-sync-strategy.md` — two-tier approach (source tree sync in dev, bootstrap-only in release). Implemented and built into Wire Node v0.2.0.

## Summary
`ensure_default_chains()` in `chain_loader.rs:175` only writes chain files `if !path.exists()`. It never updates existing files. This means every YAML/prompt change requires manual `rsync` from the source tree to the runtime data directory.

## The Problem
In release mode, chains are loaded from `~/Library/Application Support/wire-node/chains/`. The first run bootstraps them from embedded defaults. After that, they're never updated — even when the app ships a new version with updated chains.

We've had to manually rsync after every prompt or pipeline change this entire session.

## The Fix
Change `ensure_default_chains()` to always write the embedded defaults, overwriting existing files. The embedded defaults are the canonical source of truth — they ship with the binary.

In `chain_loader.rs`, change:
```rust
if !path.exists() {
    std::fs::write(&path, content)
```
To:
```rust
std::fs::write(&path, content)
```

(Remove the `if !path.exists()` guard for all embedded default files.)

This applies to both the chain YAML files and the prompt .md files that are embedded in the binary.

## Edge case
If users have manually edited their runtime chain files, this overwrites their changes. That's acceptable — the YAML/prompt files in the source tree are the canonical versions. User customization should happen there, not in the data directory.

## Files
- `src-tauri/src/pyramid/chain_loader.rs` — `ensure_default_chains()` function
