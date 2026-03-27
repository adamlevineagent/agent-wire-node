use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::execution_plan::TransformSpec;
use super::expression::{evaluate_expression, evaluate_path_against_value, ExpressionEnv};

pub fn execute_transform(spec: &TransformSpec) -> Result<Value> {
    execute_transform_function(&spec.function, &spec.args)
}

pub fn execute_transform_function(function: &str, args: &Value) -> Result<Value> {
    match function {
        "filter" => filter(args),
        "group" => group(args),
        "sort" => sort(args),
        "project" => project(args),
        "lines" => lines(args),
        "concat" => concat(args),
        "count" => count(args),
        "flatten" => flatten(args),
        "deduplicate" => deduplicate(args),
        "index_by" => index_by(args),
        "zip" => zip(args),
        "lookup" => lookup(args),
        "slice" => slice(args),
        "coalesce" => coalesce(args),
        "ensure_array" => ensure_array(args),
        unknown => Err(anyhow!("unsupported transform function '{}'", unknown)),
    }
}

pub fn resolve_transform_args<E: ExpressionEnv>(args: &Value, env: &E) -> Result<Value> {
    match args {
        Value::String(text) if text.trim_start().starts_with('$') => evaluate_expression(text, env),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                out.insert(key.clone(), resolve_transform_args(value, env)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(values) => {
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                out.push(resolve_transform_args(value, env)?);
            }
            Ok(Value::Array(out))
        }
        other => Ok(other.clone()),
    }
}

fn filter(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    let field = optional_string_arg(args, "field");
    let equals = args.get("equals");
    let truthy = optional_string_arg(args, "truthy");

    let filtered = collection
        .iter()
        .filter(|item| {
            if let Some(field) = truthy.as_deref() {
                return navigate_field(item, field)
                    .map(|value| !is_empty(&value))
                    .unwrap_or(false);
            }
            if let (Some(field), Some(expected)) = (field.as_deref(), equals) {
                return navigate_field(item, field).as_ref() == Some(expected);
            }
            !is_empty(item)
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(Value::Array(filtered))
}

fn group(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    let groups = array_arg(args, "by")?;
    let item_key = string_arg_with_default(args, "item_key", "id");

    let mut items_by_key = HashMap::new();
    for item in collection {
        let nav = navigate_field(item, item_key);
        let key = nav
            .as_ref()
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("group: every collection item must expose '{}'", item_key))?;
        items_by_key.insert(key.to_string(), item.clone());
    }

    let mut output = Vec::with_capacity(groups.len());
    for group in groups {
        let Some(group_obj) = group.as_object() else {
            bail!("group: group descriptors must be objects");
        };
        let member_ids = extract_group_member_ids(group)?;
        let items = member_ids
            .into_iter()
            .filter_map(|member_id| items_by_key.get(&member_id).cloned())
            .collect::<Vec<_>>();

        let mut entry = group_obj.clone();
        entry.insert("items".to_string(), Value::Array(items));
        output.push(Value::Object(entry));
    }

    Ok(Value::Array(output))
}

fn extract_group_member_ids(group: &Value) -> Result<Vec<String>> {
    if let Some(node_ids) = group.get("node_ids").and_then(Value::as_array) {
        return Ok(node_ids
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect());
    }

    if let Some(assignments) = group.get("assignments").and_then(Value::as_array) {
        return Ok(assignments
            .iter()
            .filter_map(|assignment| assignment.get("source_node").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect());
    }

    if let Some(assigned_items) = group.get("assigned_items").and_then(Value::as_array) {
        return Ok(assigned_items
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect());
    }

    bail!("group: group descriptor must include node_ids, assignments, or assigned_items")
}

fn sort(args: &Value) -> Result<Value> {
    let mut collection = array_arg(args, "collection")?.to_vec();
    let by = string_arg(args, "by")?;
    let descending = bool_arg(args, "descending").unwrap_or(false);
    collection.sort_by(|left, right| {
        compare_json_values(
            navigate_field(left, by).as_ref(),
            navigate_field(right, by).as_ref(),
        )
    });
    if descending {
        collection.reverse();
    }
    Ok(Value::Array(collection))
}

fn project(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    let fields = array_arg(args, "fields")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();

    let projected = collection
        .iter()
        .map(|item| {
            let mut map = Map::new();
            for field in &fields {
                if let Some(value) = navigate_field(item, field) {
                    map.insert((*field).to_string(), value);
                }
            }
            Value::Object(map)
        })
        .collect::<Vec<_>>();

    Ok(Value::Array(projected))
}

fn lines(args: &Value) -> Result<Value> {
    let text = string_arg(args, "text")?;
    let start = usize_arg_with_default(args, "start", 0)?;
    let count = usize_arg(args, "count")?;
    let lines = text
        .lines()
        .skip(start)
        .take(count)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Value::String(lines))
}

fn concat(args: &Value) -> Result<Value> {
    let collections = array_arg(args, "collections")?;
    let mut output = Vec::new();
    for collection in collections {
        let Some(items) = collection.as_array() else {
            bail!("concat: each collection must be an array");
        };
        output.extend(items.iter().cloned());
    }
    Ok(Value::Array(output))
}

fn count(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    Ok(Value::Number((collection.len() as u64).into()))
}

fn flatten(args: &Value) -> Result<Value> {
    let nested = array_arg(args, "nested")?;
    let mut output = Vec::new();
    for value in nested {
        match value {
            Value::Array(items) => output.extend(items.iter().cloned()),
            other => output.push(other.clone()),
        }
    }
    Ok(Value::Array(output))
}

fn deduplicate(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    let by = optional_string_arg(args, "by");
    let mut seen = HashSet::new();
    let mut output = Vec::new();

    for item in collection {
        let key = if let Some(field) = by.as_deref() {
            navigate_field(item, field)
                .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string()))
                .unwrap_or_else(|| "null".to_string())
        } else {
            serde_json::to_string(item).unwrap_or_else(|_| "null".to_string())
        };
        if seen.insert(key) {
            output.push(item.clone());
        }
    }

    Ok(Value::Array(output))
}

fn index_by(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    let field = string_arg(args, "field")?;
    let mut index = Map::new();
    for item in collection {
        let nav = navigate_field(item, field);
        let key = nav
            .as_ref()
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("index_by: field '{}' must resolve to a string", field))?;
        index.insert(key.to_string(), item.clone());
    }
    Ok(Value::Object(index))
}

fn zip(args: &Value) -> Result<Value> {
    let left = array_arg(args, "left")?;
    let right = array_arg(args, "right")?;
    let count = left.len().min(right.len());
    let output = (0..count)
        .map(|index| {
            Value::Object(Map::from_iter([
                ("left".to_string(), left[index].clone()),
                ("right".to_string(), right[index].clone()),
                ("index".to_string(), Value::Number((index as u64).into())),
            ]))
        })
        .collect::<Vec<_>>();
    Ok(Value::Array(output))
}

fn lookup(args: &Value) -> Result<Value> {
    let map = object_arg(args, "map")?;
    let key = string_arg(args, "key")?;
    Ok(map.get(key).cloned().unwrap_or(Value::Null))
}

fn slice(args: &Value) -> Result<Value> {
    let collection = array_arg(args, "collection")?;
    let start = usize_arg_with_default(args, "start", 0)?;
    let count = usize_arg(args, "count")?;
    Ok(Value::Array(
        collection.iter().skip(start).take(count).cloned().collect(),
    ))
}

fn coalesce(args: &Value) -> Result<Value> {
    let values = array_arg(args, "values")?;
    for value in values {
        if !is_empty(value) {
            return Ok(value.clone());
        }
    }
    Ok(Value::Null)
}

fn ensure_array(args: &Value) -> Result<Value> {
    if let Some(values) = args.get("values").and_then(Value::as_array) {
        for candidate in values {
            if is_empty(candidate) {
                continue;
            }
            return ensure_array(&Value::Object(Map::from_iter([(
                "value".to_string(),
                candidate.clone(),
            )])));
        }
        return Ok(Value::Array(vec![]));
    }

    let value = args.get("value").cloned().unwrap_or(Value::Null);
    match value {
        Value::Null => Ok(Value::Array(vec![])),
        Value::Array(items) => Ok(Value::Array(items)),
        other => Ok(Value::Array(vec![other])),
    }
}

fn array_arg<'a>(args: &'a Value, key: &str) -> Result<&'a [Value]> {
    args.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow!("transform arg '{}' must be an array", key))
}

fn object_arg<'a>(args: &'a Value, key: &str) -> Result<&'a Map<String, Value>> {
    args.get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("transform arg '{}' must be an object", key))
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("transform arg '{}' must be a string", key))
}

fn optional_string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn string_arg_with_default<'a>(args: &'a Value, key: &str, default: &'a str) -> &'a str {
    optional_string_arg(args, key).unwrap_or(default)
}

fn usize_arg(args: &Value, key: &str) -> Result<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .ok_or_else(|| anyhow!("transform arg '{}' must be an integer", key))
}

fn usize_arg_with_default(args: &Value, key: &str, default: usize) -> Result<usize> {
    Ok(args
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(default))
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn navigate_field<'a>(value: &'a Value, path: &str) -> Option<Value> {
    if path.contains('[') || path.contains('*') {
        return evaluate_path_against_value(value, path).ok();
    }

    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current.clone())
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
    }
}

fn compare_json_values(left: Option<&Value>, right: Option<&Value>) -> Ordering {
    match (left, right) {
        (Some(Value::Number(a)), Some(Value::Number(b))) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(Ordering::Equal),
        (Some(Value::String(a)), Some(Value::String(b))) => a.cmp(b),
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => a.cmp(b),
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => a.to_string().cmp(&b.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn group_transform_groups_items_by_assignments() {
        let result = execute_transform_function(
            "group",
            &json!({
                "collection": [
                    { "source_node": "C-L0-000", "headline": "A" },
                    { "source_node": "C-L0-001", "headline": "B" }
                ],
                "by": [
                    {
                        "name": "Auth",
                        "assignments": [
                            { "source_node": "C-L0-000" }
                        ]
                    }
                ],
                "item_key": "source_node"
            }),
        )
        .unwrap();

        assert_eq!(result[0]["items"][0]["headline"], json!("A"));
    }

    #[test]
    fn project_transform_selects_fields() {
        let result = execute_transform_function(
            "project",
            &json!({
                "collection": [
                    { "name": "alpha", "weight": 1, "ignore": true }
                ],
                "fields": ["name", "weight"]
            }),
        )
        .unwrap();

        assert_eq!(result, json!([{ "name": "alpha", "weight": 1 }]));
    }

    #[test]
    fn filter_with_truthy_field() {
        let result = execute_transform_function(
            "filter",
            &json!({
                "collection": [
                    { "name": "a", "active": true },
                    { "name": "b", "active": false },
                    { "name": "c", "active": true }
                ],
                "truthy": "active"
            }),
        )
        .unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);
    }

    #[test]
    fn filter_with_equals() {
        let result = execute_transform_function(
            "filter",
            &json!({
                "collection": [
                    { "type": "a" },
                    { "type": "b" },
                    { "type": "a" }
                ],
                "field": "type",
                "equals": "a"
            }),
        )
        .unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);
    }

    #[test]
    fn filter_empty_collection() {
        let result = execute_transform_function(
            "filter",
            &json!({
                "collection": [],
                "truthy": "name"
            }),
        )
        .unwrap();
        assert_eq!(result, json!([]));
    }

    #[test]
    fn sort_ascending() {
        let result = execute_transform_function(
            "sort",
            &json!({
                "collection": [
                    { "name": "charlie", "weight": 3 },
                    { "name": "alpha", "weight": 1 },
                    { "name": "bravo", "weight": 2 }
                ],
                "by": "name"
            }),
        )
        .unwrap();
        assert_eq!(result[0]["name"], json!("alpha"));
        assert_eq!(result[2]["name"], json!("charlie"));
    }

    #[test]
    fn sort_descending() {
        let result = execute_transform_function(
            "sort",
            &json!({
                "collection": [
                    { "val": 1 },
                    { "val": 3 },
                    { "val": 2 }
                ],
                "by": "val",
                "descending": true
            }),
        )
        .unwrap();
        assert_eq!(result[0]["val"], json!(3));
        assert_eq!(result[2]["val"], json!(1));
    }

    #[test]
    fn lines_extracts_range() {
        let result = execute_transform_function(
            "lines",
            &json!({
                "text": "line0\nline1\nline2\nline3\nline4",
                "start": 1,
                "count": 2
            }),
        )
        .unwrap();
        assert_eq!(result, json!("line1\nline2"));
    }

    #[test]
    fn concat_merges_arrays() {
        let result = execute_transform_function(
            "concat",
            &json!({
                "collections": [[1, 2], [3, 4], [5]]
            }),
        )
        .unwrap();
        assert_eq!(result, json!([1, 2, 3, 4, 5]));
    }

    #[test]
    fn count_returns_length() {
        let result = execute_transform_function(
            "count",
            &json!({
                "collection": [1, 2, 3]
            }),
        )
        .unwrap();
        assert_eq!(result, json!(3));
    }

    #[test]
    fn count_empty_array() {
        let result = execute_transform_function(
            "count",
            &json!({
                "collection": []
            }),
        )
        .unwrap();
        assert_eq!(result, json!(0));
    }

    #[test]
    fn flatten_unnests_one_level() {
        let result = execute_transform_function(
            "flatten",
            &json!({
                "nested": [[1, 2], [3], 4]
            }),
        )
        .unwrap();
        assert_eq!(result, json!([1, 2, 3, 4]));
    }

    #[test]
    fn ensure_array_wraps_non_array() {
        assert_eq!(
            execute_transform_function("ensure_array", &json!({ "value": "hello" })).unwrap(),
            json!(["hello"])
        );
        assert_eq!(
            execute_transform_function("ensure_array", &json!({ "value": null })).unwrap(),
            json!([])
        );
        assert_eq!(
            execute_transform_function("ensure_array", &json!({ "value": [1, 2] })).unwrap(),
            json!([1, 2])
        );
    }

    #[test]
    fn ensure_array_with_values_coalesce() {
        let result =
            execute_transform_function("ensure_array", &json!({ "values": [null, "", [1, 2]] }))
                .unwrap();
        assert_eq!(result, json!([1, 2]));
    }

    #[test]
    fn unknown_transform_errors() {
        let result = execute_transform_function("nonexistent", &json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn transform_composition_group_then_project() {
        // Group items, then project just the group names
        let grouped = execute_transform_function(
            "group",
            &json!({
                "collection": [
                    { "id": "a", "value": 1 },
                    { "id": "b", "value": 2 }
                ],
                "by": [
                    { "name": "Group1", "assigned_items": ["a"] },
                    { "name": "Group2", "assigned_items": ["b"] }
                ],
                "item_key": "id"
            }),
        )
        .unwrap();

        let projected = execute_transform_function(
            "project",
            &json!({
                "collection": grouped,
                "fields": ["name"]
            }),
        )
        .unwrap();

        assert_eq!(projected, json!([{"name": "Group1"}, {"name": "Group2"}]));
    }

    #[test]
    fn resolve_transform_args_resolves_references() {
        use crate::pyramid::expression::ValueEnv;
        let env_data = json!({
            "step_output": [1, 2, 3]
        });
        let env = ValueEnv::new(&env_data);
        let args = json!({
            "collection": "$step_output",
            "static_val": 42
        });
        let resolved = resolve_transform_args(&args, &env).unwrap();
        assert_eq!(resolved["collection"], json!([1, 2, 3]));
        assert_eq!(resolved["static_val"], json!(42));
    }

    #[test]
    fn lookup_coalesce_slice_deduplicate_index_by_zip_work() {
        assert_eq!(
            execute_transform_function(
                "lookup",
                &json!({
                    "map": { "a": 1, "b": 2 },
                    "key": "b"
                }),
            )
            .unwrap(),
            json!(2)
        );

        assert_eq!(
            execute_transform_function(
                "coalesce",
                &json!({
                    "values": [null, "", "ok"]
                }),
            )
            .unwrap(),
            json!("ok")
        );

        assert_eq!(
            execute_transform_function(
                "slice",
                &json!({
                    "collection": [1, 2, 3, 4],
                    "start": 1,
                    "count": 2
                }),
            )
            .unwrap(),
            json!([2, 3])
        );

        assert_eq!(
            execute_transform_function(
                "deduplicate",
                &json!({
                    "collection": [
                        { "id": "a", "value": 1 },
                        { "id": "a", "value": 2 },
                        { "id": "b", "value": 3 }
                    ],
                    "by": "id"
                }),
            )
            .unwrap(),
            json!([
                { "id": "a", "value": 1 },
                { "id": "b", "value": 3 }
            ])
        );

        assert_eq!(
            execute_transform_function(
                "index_by",
                &json!({
                    "collection": [
                        { "id": "a", "value": 1 },
                        { "id": "b", "value": 2 }
                    ],
                    "field": "id"
                }),
            )
            .unwrap(),
            json!({
                "a": { "id": "a", "value": 1 },
                "b": { "id": "b", "value": 2 }
            })
        );

        assert_eq!(
            execute_transform_function(
                "zip",
                &json!({
                    "left": ["a", "b"],
                    "right": [1, 2, 3]
                }),
            )
            .unwrap(),
            json!([
                { "left": "a", "right": 1, "index": 0 },
                { "left": "b", "right": 2, "index": 1 }
            ])
        );
    }
}
