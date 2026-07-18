//! Lightweight JSON-schema validation and guidance for app-server parameters.
//!
//! The generated Codex schemas are plain JSON Schema (draft-07 subset). This
//! module implements exactly the keywords those schemas use — `type`,
//! `required`, `properties`, `items`, `enum`, `oneOf`/`anyOf`/`allOf`, and
//! nullable type arrays — so `/advanced` invocations can be checked before
//! they reach the app-server and parameter guidance can be rendered without
//! shipping the schema file to Discord.

use std::fmt::Write as _;

use regex::Regex;
use serde_json::Value;

const MAX_DEPTH: usize = 24;
const MAX_ERRORS: usize = 12;

/// Validate `params` against a generated parameter schema. Returns
/// human-readable problems; an empty vector means the parameters satisfy every
/// keyword this validator understands.
#[must_use]
pub fn validate(schema: &Value, params: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    check(schema, params, "params", 0, &mut errors);
    errors.truncate(MAX_ERRORS);
    errors
}

fn check(schema: &Value, value: &Value, path: &str, depth: usize, errors: &mut Vec<String>) {
    if depth >= MAX_DEPTH || errors.len() >= MAX_ERRORS {
        return;
    }
    let Some(schema) = schema.as_object() else {
        return;
    };

    // Composition keywords: a value is fine when any branch accepts it.
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let matched = branches
                .iter()
                .any(|branch| branch_accepts(branch, value, depth + 1));
            if !matched {
                errors.push(format!(
                    "{path}: does not match any of the {} allowed shapes ({})",
                    branches.len(),
                    branch_titles(branches)
                ));
            }
            return;
        }
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            check(branch, value, path, depth + 1, errors);
        }
    }

    if let Some(expected) = schema.get("type")
        && !type_matches(expected, value)
    {
        errors.push(format!(
            "{path}: expected {}, got {}",
            type_label(expected),
            value_kind(value)
        ));
        return;
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        errors.push(format!(
            "{path}: must be one of {}",
            allowed
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if let (Some(minimum), Some(actual)) = (
        schema.get("minimum").and_then(Value::as_f64),
        value.as_f64(),
    ) && actual < minimum
    {
        errors.push(format!("{path}: must be at least {minimum}"));
    }

    if let (Some(minimum), Some(actual)) = (
        schema.get("minLength").and_then(Value::as_u64),
        value.as_str(),
    ) && u64::try_from(actual.chars().count()).is_ok_and(|length| length < minimum)
    {
        errors.push(format!(
            "{path}: must contain at least {minimum} characters"
        ));
    }

    if let (Some(pattern), Some(actual)) = (
        schema.get("pattern").and_then(Value::as_str),
        value.as_str(),
    ) {
        match Regex::new(pattern) {
            Ok(regex) if !regex.is_match(actual) => {
                errors.push(format!("{path}: must match pattern `{pattern}`"));
            }
            Err(error) => errors.push(format!("{path}: invalid schema pattern: {error}")),
            Ok(_) => {}
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    errors.push(format!("{path}: missing required property `{name}`"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, child) in object {
                if let Some(property_schema) = properties.get(name) {
                    check(
                        property_schema,
                        child,
                        &format!("{path}.{name}"),
                        depth + 1,
                        errors,
                    );
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    errors.push(format!("{path}: unknown property `{name}`"));
                }
            }
        }
    }

    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (index, child) in values.iter().enumerate() {
            check(items, child, &format!("{path}[{index}]"), depth + 1, errors);
        }
    }
}

fn branch_accepts(schema: &Value, value: &Value, depth: usize) -> bool {
    let mut branch_errors = Vec::new();
    check(schema, value, "value", depth, &mut branch_errors);
    branch_errors.is_empty()
}

fn branch_titles(branches: &[Value]) -> String {
    let titles: Vec<&str> = branches
        .iter()
        .filter_map(|branch| {
            branch
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| branch.get("type").and_then(Value::as_str))
        })
        .take(6)
        .collect();
    if titles.is_empty() {
        "unnamed variants".to_owned()
    } else {
        titles.join(" | ")
    }
}

fn type_matches(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(kind) => single_type_matches(kind, value),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| single_type_matches(kind, value)),
        _ => true,
    }
}

fn single_type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        _ => true,
    }
}

fn type_label(expected: &Value) -> String {
    match expected {
        Value::String(kind) => kind.clone(),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "value".to_owned(),
    }
}

const fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Render one-line-per-parameter guidance for a generated parameter schema,
/// suitable for a Discord embed. Returns `None` when the method takes no
/// parameters.
#[must_use]
pub fn guidance(schema: &Value) -> Option<String> {
    if schema.is_null() || schema.get("type") == Some(&Value::String("null".into())) {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    if properties.is_empty() {
        return None;
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = String::new();
    for (name, property) in properties.iter().take(20) {
        let kind = property
            .get("type")
            .map(type_label)
            .or_else(|| {
                property
                    .get("oneOf")
                    .or_else(|| property.get("anyOf"))
                    .and_then(Value::as_array)
                    .map(|branches| branch_titles(branches))
            })
            .unwrap_or_else(|| "value".to_owned());
        let requirement = if required.contains(&name.as_str()) {
            "required"
        } else {
            "optional"
        };
        let _ = write!(out, "`{name}` ({kind}, {requirement})");
        if let Some(description) = property.get("description").and_then(Value::as_str) {
            let first_line = description.lines().next().unwrap_or_default();
            let brief: String = first_line.chars().take(120).collect();
            let _ = write!(out, " — {brief}");
        }
        out.push('\n');
    }
    if properties.len() > 20 {
        let _ = writeln!(out, "…and {} more properties", properties.len() - 20);
    }
    Some(out.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn thread_rollback_schema() -> Value {
        json!({
            "type": "object",
            "required": ["numTurns", "threadId"],
            "properties": {
                "numTurns": {"type": "integer", "format": "uint32", "minimum": 0},
                "threadId": {"type": "string"}
            }
        })
    }

    #[test]
    fn accepts_valid_parameters() {
        let errors = validate(
            &thread_rollback_schema(),
            &json!({"numTurns": 2, "threadId": "thr"}),
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn reports_missing_required_and_wrong_types() {
        let errors = validate(&thread_rollback_schema(), &json!({"numTurns": "two"}));
        assert!(errors.iter().any(|error| error.contains("threadId")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("numTurns") && error.contains("integer"))
        );
    }

    #[test]
    fn rejects_strings_shorter_than_min_length() {
        let schema = json!({"type":"string", "minLength": 3});
        let errors = validate(&schema, &json!("ab"));
        assert!(errors.iter().any(|error| error.contains("at least 3")));
    }

    #[test]
    fn rejects_numbers_below_minimum() {
        let schema = json!({"type":"number", "minimum": 2.5});
        let errors = validate(&schema, &json!(2.0));
        assert!(errors.iter().any(|error| error.contains("at least 2.5")));
    }

    #[test]
    fn rejects_strings_that_do_not_match_pattern() {
        let schema = json!({"type":"string", "pattern": "^[a-z][a-z0-9_-]+$"});
        let errors = validate(&schema, &json!("Not valid"));
        assert!(errors.iter().any(|error| error.contains("pattern")));
    }

    #[test]
    fn nullable_type_arrays_accept_null() {
        let schema = json!({
            "type": "object",
            "properties": {"cursor": {"type": ["string", "null"]}}
        });
        assert!(validate(&schema, &json!({"cursor": null})).is_empty());
        assert!(validate(&schema, &json!({"cursor": "next"})).is_empty());
        assert!(!validate(&schema, &json!({"cursor": 7})).is_empty());
    }

    #[test]
    fn one_of_accepts_any_branch_and_names_variants_on_failure() {
        let schema = json!({
            "type": "object",
            "required": ["target"],
            "properties": {
                "target": {
                    "oneOf": [
                        {
                            "type": "object",
                            "required": ["type"],
                            "properties": {"type": {"enum": ["uncommittedChanges"]}},
                            "title": "UncommittedChangesReviewTarget"
                        },
                        {
                            "type": "object",
                            "required": ["branch", "type"],
                            "properties": {
                                "branch": {"type": "string"},
                                "type": {"enum": ["baseBranch"]}
                            },
                            "title": "BaseBranchReviewTarget"
                        }
                    ]
                }
            }
        });
        assert!(
            validate(
                &schema,
                &json!({"target": {"type": "baseBranch", "branch": "main"}})
            )
            .is_empty()
        );
        let errors = validate(&schema, &json!({"target": {"type": "baseBranch"}}));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("BaseBranchReviewTarget"))
        );
    }

    #[test]
    fn enum_and_array_items_are_checked() {
        let schema = json!({
            "type": "object",
            "properties": {
                "sortKey": {"type": "string", "enum": ["createdAt", "updatedAt"]},
                "cwds": {"type": "array", "items": {"type": "string"}}
            }
        });
        assert!(!validate(&schema, &json!({"sortKey": "name"})).is_empty());
        assert!(!validate(&schema, &json!({"cwds": ["ok", 3]})).is_empty());
        assert!(
            validate(
                &schema,
                &json!({"sortKey": "createdAt", "cwds": ["C:/work"]})
            )
            .is_empty()
        );
    }

    #[test]
    fn guidance_lists_parameters_with_requirement_and_description() {
        let schema = json!({
            "type": "object",
            "required": ["threadId"],
            "properties": {
                "threadId": {"type": "string"},
                "limit": {
                    "type": ["integer", "null"],
                    "description": "Optional page size; defaults to a reasonable server-side value."
                }
            }
        });
        let text = guidance(&schema).unwrap();
        assert!(text.contains("`threadId` (string, required)"));
        assert!(text.contains("`limit` (integer|null, optional) — Optional page size"));
    }

    #[test]
    fn guidance_is_none_for_parameterless_methods() {
        assert!(guidance(&json!({"type": "null"})).is_none());
        assert!(guidance(&Value::Null).is_none());
        assert!(guidance(&json!({"type": "object", "properties": {}})).is_none());
    }
}
