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

pub const DEFAULT_RESOLVED_STATE: &str = "6";

pub fn resolution_choice_value(
    metadata: &TableMetadata,
    field: &str,
    input: &str,
) -> Result<String, ApiError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ApiError::InvalidInput(format!(
            "{} cannot be empty",
            resolution_field_name(field)
        )));
    }
    let Some(choices) = metadata.choices.get(field) else {
        return Ok(input.into());
    };
    if choices.is_empty() {
        return Ok(input.into());
    }
    if let Some(choice) = choices.iter().find(|choice| choice.value == input) {
        return Ok(choice.value.clone());
    }
    if let Some(choice) = choices
        .iter()
        .find(|choice| choice.label.eq_ignore_ascii_case(input))
    {
        return Ok(choice.value.clone());
    }
    let configured = choices
        .iter()
        .map(|choice| format!("{} ({})", choice.label, choice.value))
        .collect::<Vec<_>>()
        .join(", ");
    Err(ApiError::InvalidInput(format!(
        "unknown {} '{input}'; configured choices: {configured}. Rerun with --refresh if the instance configuration changed",
        resolution_field_name(field)
    )))
}

pub fn resolved_state_value(
    metadata: &TableMetadata,
    requested: Option<&str>,
) -> Result<String, ApiError> {
    if let Some(requested) = requested {
        return resolution_choice_value(metadata, "state", requested);
    }
    let Some(choices) = metadata.choices.get("state") else {
        return Ok(DEFAULT_RESOLVED_STATE.into());
    };
    if choices.is_empty() {
        return Ok(DEFAULT_RESOLVED_STATE.into());
    }
    choices
        .iter()
        .find(|choice| choice.label.eq_ignore_ascii_case("Resolved"))
        .or_else(|| {
            choices
                .iter()
                .find(|choice| choice.value == DEFAULT_RESOLVED_STATE)
        })
        .map(|choice| choice.value.clone())
        .ok_or_else(|| {
            ApiError::InvalidInput(
                "the incident metadata has no Resolved state; supply its configured label or value with --state"
                    .into(),
            )
        })
}

pub fn require_resolvable(
    record: &Value,
    resolved_state: &str,
    metadata: &TableMetadata,
) -> Result<(), ApiError> {
    let Some(current_value) = record.get("state").and_then(raw_text) else {
        return Ok(());
    };
    let display = record
        .get("state")
        .and_then(|value| value.get("display_value"))
        .and_then(Value::as_str)
        .or_else(|| metadata.choice_label("state", current_value));
    if current_value == resolved_state
        || display.is_some_and(|label| label.eq_ignore_ascii_case("Resolved"))
    {
        return Err(ApiError::Conflict("incident is already resolved".into()));
    }
    let terminal_value = matches!(current_value, "7" | "8");
    let terminal_label = display.is_some_and(|label| {
        matches!(
            label.to_ascii_lowercase().as_str(),
            "closed" | "canceled" | "cancelled"
        )
    });
    if terminal_value || terminal_label {
        return Err(ApiError::Conflict(format!(
            "incident is already {}; closed or canceled incidents cannot be resolved",
            display.unwrap_or("in a terminal state")
        )));
    }
    Ok(())
}

fn resolution_field_name(field: &str) -> &str {
    match field {
        "state" => "resolution state",
        "close_code" => "resolution code",
        "close_notes" => "resolution notes",
        _ => field,
    }
}

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

    #[test]
    fn resolution_choices_accept_values_and_case_insensitive_labels() {
        let metadata = resolution_metadata();
        assert_eq!(
            resolution_choice_value(&metadata, "close_code", "Solved (Permanently)").unwrap(),
            "solved_permanently"
        );
        assert_eq!(
            resolution_choice_value(&metadata, "close_code", "solved_permanently").unwrap(),
            "solved_permanently"
        );
        assert_eq!(resolved_state_value(&metadata, None).unwrap(), "9");

        let metadata_without_choices = TableMetadata {
            table: "incident".into(),
            fetched_at: 0,
            fields: Vec::new(),
            choices: BTreeMap::new(),
        };
        assert_eq!(
            resolved_state_value(&metadata_without_choices, None).unwrap(),
            DEFAULT_RESOLVED_STATE
        );
        assert_eq!(
            resolution_choice_value(&metadata_without_choices, "close_code", "custom_resolution")
                .unwrap(),
            "custom_resolution"
        );
    }

    #[test]
    fn resolution_choices_reject_unknown_values_when_choices_are_known() {
        let error =
            resolution_choice_value(&resolution_metadata(), "close_code", "typo").unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(_)));
        assert!(error.to_string().contains("Solved (Permanently)"));
        assert!(error.to_string().contains("--refresh"));
    }

    #[test]
    fn terminal_and_resolved_incidents_cannot_be_resolved_again() {
        let metadata = resolution_metadata();
        let resolved = serde_json::json!({
            "state": {"value": "9", "display_value": "Resolved"}
        });
        assert!(matches!(
            require_resolvable(&resolved, "9", &metadata),
            Err(ApiError::Conflict(_))
        ));

        let closed = serde_json::json!({
            "state": {"value": "7", "display_value": "Closed"}
        });
        assert!(matches!(
            require_resolvable(&closed, "9", &metadata),
            Err(ApiError::Conflict(_))
        ));

        let canceled = serde_json::json!({
            "state": {"value": "42", "display_value": "Canceled"}
        });
        assert!(matches!(
            require_resolvable(&canceled, "9", &metadata),
            Err(ApiError::Conflict(_))
        ));
    }

    fn resolution_metadata() -> TableMetadata {
        use crate::metadata::ChoiceMetadata;

        TableMetadata {
            table: "incident".into(),
            fetched_at: 0,
            fields: Vec::new(),
            choices: BTreeMap::from([
                (
                    "state".into(),
                    vec![ChoiceMetadata {
                        value: "9".into(),
                        label: "Resolved".into(),
                        sequence: 90,
                    }],
                ),
                (
                    "close_code".into(),
                    vec![ChoiceMetadata {
                        value: "solved_permanently".into(),
                        label: "Solved (Permanently)".into(),
                        sequence: 10,
                    }],
                ),
            ]),
        }
    }
}
