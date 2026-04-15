# Hotfix: Add sub-chain primitives to VALID_PRIMITIVES

`chain_engine.rs:11` — `VALID_PRIMITIVES` array is missing the four sub-chain primitives. The validator rejects them before the executor can run.

Add to the list:
```rust
// Sub-chain flow control
"container",
"loop",
"gate",
"split",
```

One line change. The dispatch logic at chain_executor.rs:3627-3672 already handles all four — they just need to pass validation.
