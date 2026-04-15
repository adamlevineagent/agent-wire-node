# Rust Handoff: `item_fields` dot-notation for nested field projection

## The Problem
`item_fields: ["node_id", "headline", "orientation", "topics"]` sends full topic objects — each with `current` (3-5 sentences), `entities` (array), `corrections` (array), `decisions` (array). For 60 docs per batch, this is 94-111K tokens. Mercury 2 rejects it.

We need `topics.name` — just the topic name strings, not the full objects. Without this, the clustering step either gets too little signal (no topics → one giant thread) or too much data (full topics → Qwen cascade). There is no working middle ground.

## The Fix

`item_fields` supports dot-notation paths for nested projection:

```yaml
item_fields: ["node_id", "headline", "orientation", "topics.name"]
```

Given:
```json
{
  "node_id": "D-L0-042",
  "headline": "Auth Token Design",
  "orientation": "This document covers...",
  "topics": [
    {
      "name": "Token Rotation",
      "current": "The system uses refresh tokens with 24h expiry and...",
      "entities": ["system: Supabase Auth", "decision: 24h expiry"],
      "corrections": [{"wrong": "...", "right": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    },
    {
      "name": "Session Management",
      "current": "Sessions are tracked via...",
      "entities": ["system: Redis", "config: TTL 3600"],
      "corrections": [],
      "decisions": []
    }
  ]
}
```

With `item_fields: ["node_id", "headline", "orientation", "topics.name"]`:
```json
{
  "node_id": "D-L0-042",
  "headline": "Auth Token Design",
  "orientation": "This document covers...",
  "topics": ["Token Rotation", "Session Management"]
}
```

The `topics` array of objects becomes an array of strings — just the `name` field from each object.

## Implementation

In `project_item()` (chain_executor.rs), handle dot-notation:

```rust
fn project_item(item: &Value, fields: &[String]) -> Value {
    let Some(obj) = item.as_object() else { return item.clone() };
    let mut projected = serde_json::Map::new();

    for field in fields {
        if let Some((parent, child)) = field.split_once('.') {
            // Dot-notation: extract child field from array of objects
            if let Some(Value::Array(arr)) = obj.get(parent) {
                let extracted: Vec<Value> = arr.iter()
                    .filter_map(|item| item.get(child).cloned())
                    .collect();
                projected.insert(parent.to_string(), Value::Array(extracted));
            }
        } else {
            // Top-level field: copy as-is
            if let Some(value) = obj.get(field.as_str()) {
                projected.insert(field.clone(), value.clone());
            }
        }
    }

    Value::Object(projected)
}
```

If multiple dot-notation fields reference the same parent (e.g., `topics.name` and `topics.entities`), the result should be an array of projected sub-objects:

```yaml
item_fields: ["node_id", "headline", "topics.name", "topics.entities"]
```

```json
{
  "node_id": "D-L0-042",
  "headline": "Auth Token Design",
  "topics": [
    {"name": "Token Rotation", "entities": ["system: Supabase Auth"]},
    {"name": "Session Management", "entities": ["system: Redis"]}
  ]
}
```

```rust
// When multiple dot-paths share a parent, merge into sub-objects
fn project_item(item: &Value, fields: &[String]) -> Value {
    let Some(obj) = item.as_object() else { return item.clone() };
    let mut projected = serde_json::Map::new();

    // Group dot-notation fields by parent
    let mut nested: HashMap<&str, Vec<&str>> = HashMap::new();

    for field in fields {
        if let Some((parent, child)) = field.split_once('.') {
            nested.entry(parent).or_default().push(child);
        } else {
            if let Some(value) = obj.get(field.as_str()) {
                projected.insert(field.clone(), value.clone());
            }
        }
    }

    // Process nested projections
    for (parent, children) in nested {
        if let Some(Value::Array(arr)) = obj.get(parent) {
            if children.len() == 1 {
                // Single child field → flatten to array of values
                let extracted: Vec<Value> = arr.iter()
                    .filter_map(|item| item.get(children[0]).cloned())
                    .collect();
                projected.insert(parent.to_string(), Value::Array(extracted));
            } else {
                // Multiple child fields → array of projected sub-objects
                let extracted: Vec<Value> = arr.iter()
                    .map(|item| {
                        let mut sub = serde_json::Map::new();
                        for child in &children {
                            if let Some(v) = item.get(*child) {
                                sub.insert(child.to_string(), v.clone());
                            }
                        }
                        Value::Object(sub)
                    })
                    .collect();
                projected.insert(parent.to_string(), Value::Array(extracted));
            }
        }
    }

    Value::Object(projected)
}
```

## Files
- `src-tauri/src/pyramid/chain_executor.rs` — update `project_item()` to handle dot-notation
