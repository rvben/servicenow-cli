use std::collections::BTreeMap;

use serde_json::{Map, Value};
use similar::TextDiff;

use crate::api::ApiError;
use crate::metadata::TableMetadata;

pub const EDITABLE_FIELDS: &[&str] = &[
    "short_description",
    "description",
    "state",
    "impact",
    "urgency",
    "category",
    "subcategory",
    "assigned_to",
    "assignment_group",
    "caller_id",
    "close_code",
    "close_notes",
];

pub fn edit_document(record: &Value, metadata: Option<&TableMetadata>) -> Result<String, ApiError> {
    let object = record
        .as_object()
        .ok_or_else(|| ApiError::Other("incident response is not an object".into()))?;
    let mut editable = BTreeMap::new();
    let mut annotations = Vec::new();
    for field in EDITABLE_FIELDS {
        let Some(value) = object.get(*field) else {
            continue;
        };
        if let Some(display) = value.get("display_value").and_then(Value::as_str) {
            if !display.is_empty() {
                annotations.push(format!("# {field}: {display}"));
            }
        } else if let Some(metadata) = metadata
            && let Some(raw) = raw_text(value)
            && let Some(label) = metadata.choice_label(field, raw)
        {
            annotations.push(format!("# {field}: {label}"));
        }
        editable.insert((*field).to_string(), raw_value(value));
    }
    let yaml = serde_saphyr::to_string(&editable)
        .map_err(|error| ApiError::Other(format!("failed to encode editable incident: {error}")))?;
    let mut document = String::from(
        "# Edit values below. Saving applies only changed fields.\n# Lines beginning with # are ignored.\n",
    );
    if !annotations.is_empty() {
        document.push_str(&annotations.join("\n"));
        document.push('\n');
    }
    document.push_str(&yaml);
    Ok(document)
}

pub fn parse_edit_document(document: &str) -> Result<Map<String, Value>, ApiError> {
    let parsed: BTreeMap<String, Value> = serde_saphyr::from_str(document)
        .map_err(|error| ApiError::InvalidInput(format!("invalid edited YAML: {error}")))?;
    let mut body = Map::new();
    for (field, mut value) in parsed {
        if !EDITABLE_FIELDS.contains(&field.as_str()) {
            return Err(ApiError::InvalidInput(format!(
                "field '{field}' is not editable in this workflow; use `incidents update --field` for custom fields"
            )));
        }
        if value.is_null() {
            value = Value::String(String::new());
        }
        body.insert(field, value);
    }
    Ok(body)
}

pub fn changed_fields(record: &Value, edited: Map<String, Value>) -> Map<String, Value> {
    edited
        .into_iter()
        .filter(|(field, value)| {
            record
                .get(field)
                .map(raw_value)
                .is_none_or(|original| original != *value)
        })
        .collect()
}

pub fn change_records(previous: &Value, current: &Value) -> Vec<Value> {
    let Some(current) = current.as_object() else {
        return Vec::new();
    };
    current
        .iter()
        .filter_map(|(field, value)| {
            let before = previous.get(field).map(raw_value).unwrap_or(Value::Null);
            let after = raw_value(value);
            (before != after).then(|| {
                serde_json::json!({
                    "field": field,
                    "before": before,
                    "after": after,
                })
            })
        })
        .collect()
}

pub fn unified_diff(original: &str, edited: &str) -> String {
    TextDiff::from_lines(original, edited)
        .unified_diff()
        .context_radius(2)
        .header("current", "edited")
        .to_string()
}

pub fn raw_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => object
            .get("value")
            .cloned()
            .or_else(|| object.get("display_value").cloned())
            .unwrap_or_else(|| value.clone()),
        _ => value.clone(),
    }
}

fn raw_text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("value").and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_only_patches_changed_allowed_fields() {
        let record = serde_json::json!({
            "short_description": "Before",
            "state": {"value": "2", "display_value": "In Progress"},
            "sys_id": "0123456789abcdef0123456789abcdef"
        });
        let document = edit_document(&record, None).unwrap();
        assert!(document.contains("# state: In Progress"));
        assert!(!document.contains("sys_id"));
        let edited = parse_edit_document("short_description: After\nstate: '2'\n").unwrap();
        let changes = changed_fields(&record, edited);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes["short_description"], "After");
    }

    #[test]
    fn editor_rejects_unknown_fields() {
        let error = parse_edit_document("sys_id: dangerous\n").unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(_)));
    }
}
