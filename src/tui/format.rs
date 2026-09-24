//! Pure formatting and text helpers: turning a ServiceNow record field into
//! displayable text, redacting sensitive fields, choosing ledger columns, and
//! wrapping/truncating text for fixed-width panels. None of this touches
//! ratatui's `Frame` or `Rect`; `render.rs` calls into it to produce lines.

use std::collections::BTreeSet;

use ratatui::text::Line;
use serde_json::Value;

use super::DEFAULT_INCIDENT_QUERY;

pub(super) fn truthy_field(record: &Value, field: &str) -> bool {
    let Some(value) = record.get(field) else {
        return false;
    };
    let value = match value {
        Value::Object(object) => object.get("value").unwrap_or(value),
        _ => value,
    };
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        Value::Number(value) => value.as_u64().is_some_and(|value| value != 0),
        _ => false,
    }
}

pub(super) fn raw_field_string(record: &Value, field: &str) -> Option<String> {
    let value = record.get(field)?;
    let value = match value {
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("display_value"))
            .unwrap_or(value),
        _ => value,
    };
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

pub(super) fn percentage_field(record: &Value, field: &str) -> String {
    let value = display_field(record, field);
    if value == "—" || value.ends_with('%') {
        value
    } else {
        format!("{value}%")
    }
}

pub(super) fn compact_instance(instance: &str) -> String {
    instance
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .into()
}

pub(super) fn default_query(table: &str) -> Option<String> {
    (table == "incident").then(|| DEFAULT_INCIDENT_QUERY.into())
}

pub(super) fn infer_columns(records: &[Value], table: &str) -> Vec<String> {
    if table == "incident" {
        return [
            "number",
            "priority",
            "short_description",
            "state",
            "assigned_to",
            "sys_updated_on",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
    }
    let available: BTreeSet<&str> = records
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|record| record.keys().map(String::as_str))
        .collect();
    let preferred = [
        "number",
        "name",
        "short_description",
        "state",
        "status",
        "class",
        "sys_updated_on",
        "sys_id",
    ];
    let mut fields: Vec<String> = preferred
        .iter()
        .filter(|field| available.contains(**field))
        .map(|field| (*field).into())
        .take(6)
        .collect();
    for field in available {
        if fields.len() >= 6 {
            break;
        }
        if !fields.iter().any(|candidate| candidate == field) {
            fields.push(field.into());
        }
    }
    fields
}

pub(super) fn visible_columns(columns: &[String], width: u16) -> Vec<String> {
    let count = if width < 58 {
        2
    } else if width < 82 {
        3
    } else if width < 112 {
        4
    } else {
        6
    };
    columns.iter().take(count).cloned().collect()
}

pub(super) fn field_weight(field: &str) -> u32 {
    match field {
        "short_description" | "description" => 4,
        "sys_id" => 3,
        _ => 2,
    }
}

pub(super) fn field_rank(field: &str) -> (u8, &str) {
    let rank = match field {
        "number" => 0,
        "short_description" | "name" => 1,
        "priority" => 2,
        "state" | "status" => 3,
        "assigned_to" | "assignment_group" => 4,
        "description" => 5,
        "sys_updated_on" | "sys_created_on" => 8,
        "sys_id" => 10,
        _ => 7,
    };
    (rank, field)
}

pub(super) fn field_label(field: &str) -> String {
    safe_text(&match field {
        "short_description" => "DESCRIPTION".into(),
        "assigned_to" => "ASSIGNEE".into(),
        "sys_updated_on" => "UPDATED".into(),
        "sys_created_on" => "CREATED".into(),
        "sys_id" => "SYS ID".into(),
        other => other.replace('_', " ").to_uppercase(),
    })
}

pub(super) fn display_field(record: &Value, field: &str) -> String {
    record
        .get(field)
        .map(|value| display_field_value(field, value))
        .unwrap_or_else(|| "—".into())
}

pub(super) fn display_field_value(field: &str, value: &Value) -> String {
    if is_sensitive_field(field) {
        "[REDACTED]".into()
    } else {
        display_value(value)
    }
}

pub(super) fn record_matches_search(record: &Value, search: &str) -> bool {
    let terms = search
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return true;
    }
    let Some(fields) = record.as_object() else {
        let haystack = display_value(record).to_lowercase();
        return terms.iter().all(|term| haystack.contains(term));
    };
    let haystack = fields
        .iter()
        .filter(|(field, _)| !is_sensitive_field(field))
        .map(|(field, value)| display_field_value(field, value))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    terms.iter().all(|term| haystack.contains(term))
}

pub(super) fn display_value(value: &Value) -> String {
    let value = match value {
        Value::Object(object) => object
            .get("display_value")
            .or_else(|| object.get("value"))
            .unwrap_or(value),
        _ => value,
    };
    let rendered = match value {
        Value::Null => "—".into(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(value) if value.trim().is_empty() => "—".into(),
        Value::String(value) => value.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "?".into()),
    };
    safe_text(&rendered)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

pub(super) fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub(super) fn wrap_text_exact(value: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    let mut wrapped = Vec::new();
    for logical_line in value.split('\n') {
        let logical_line = safe_text(logical_line);
        if logical_line.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let mut current = String::new();
        for character in logical_line.chars() {
            let mut candidate = current.clone();
            candidate.push(character);
            if !current.is_empty() && Line::raw(candidate.as_str()).width() > max_width {
                wrapped.push(current);
                current = String::new();
            }
            current.push(character);
        }
        wrapped.push(current);
    }
    wrapped
}

pub(super) fn tail_text(value: &str, max_chars: usize) -> String {
    let length = value.chars().count();
    if length <= max_chars {
        return value.into();
    }
    let keep = max_chars.saturating_sub(1);
    let mut tail = String::from("…");
    tail.extend(value.chars().skip(length.saturating_sub(keep)));
    tail
}

pub(super) fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.into();
    }
    let keep = max_chars.saturating_sub(1);
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push('…');
    truncated
}

pub(super) fn is_sensitive_field(field: &str) -> bool {
    let normalized = field.to_ascii_lowercase().replace(['-', ' '], "_");
    normalized.contains("password")
        || normalized.contains("passwd")
        || normalized.contains("secret")
        || normalized.contains("private_key")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized
            .split('_')
            .any(|part| matches!(part, "token" | "cookie" | "credential" | "authorization"))
}

pub(super) fn record_sys_id(record: &Value) -> Option<&str> {
    let value = record.get("sys_id")?;
    match value {
        Value::String(value) => Some(value),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .or_else(|| object.get("display_value").and_then(Value::as_str)),
        _ => None,
    }
}

pub(super) fn record_title(record: &Value) -> String {
    ["number", "name", "short_description", "sys_id"]
        .into_iter()
        .find_map(|field| {
            record
                .get(field)
                .map(display_value)
                .filter(|value| value != "—")
        })
        .unwrap_or_else(|| "SELECTED RECORD".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::app;

    #[test]
    fn local_search_matches_display_values_without_inspecting_secrets() {
        let app = app();
        let record = &app.records[0];

        assert!(record_matches_search(record, "mail"));
        assert!(record_matches_search(record, "AVERY progress"));
        assert!(record_matches_search(record, "critical"));
        assert!(!record_matches_search(record, "never-rendered"));

        let reference = serde_json::json!({
            "assigned_to": {
                "value": "raw-user-reference",
                "display_value": "Avery Stone"
            }
        });
        assert!(!record_matches_search(&reference, "raw-user-reference"));
    }

    #[test]
    fn generic_tables_choose_human_fields_before_sys_id() {
        let records = vec![serde_json::json!({
            "sys_id": "0123456789abcdef0123456789abcdef",
            "name": "Database cluster",
            "status": "online",
            "vendor": "Example"
        })];
        assert_eq!(
            infer_columns(&records, "cmdb_ci"),
            vec!["name", "status", "sys_id", "vendor"]
        );
    }

    #[test]
    fn sensitive_field_names_are_redacted() {
        for field in [
            "password",
            "u_client_secret",
            "access_token",
            "api-key",
            "session_cookie",
            "private_key_pem",
        ] {
            assert_eq!(
                display_field_value(field, &Value::String("sensitive".into())),
                "[REDACTED]"
            );
        }
        assert_eq!(
            display_field_value("tokenized_name", &Value::String("visible".into())),
            "visible"
        );
    }
}
