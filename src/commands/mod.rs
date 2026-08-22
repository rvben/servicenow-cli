use std::collections::BTreeSet;
use std::io::Read;

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};
use serde_json::{Map, Value};

use crate::api::ApiError;

pub const INCIDENT_LIST_FIELDS: &[&str] = &[
    "sys_id",
    "number",
    "short_description",
    "state",
    "priority",
    "assigned_to",
    "sys_updated_on",
];

pub const INCIDENT_HUMAN_FIELDS: &[&str] = &[
    "number",
    "priority",
    "short_description",
    "state",
    "assigned_to",
    "sys_updated_on",
];

pub fn parse_fields(fields: Option<&str>) -> Option<Vec<String>> {
    fields.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_string)
            .collect()
    })
}

pub fn build_body(data: Option<&str>, fields: &[String]) -> Result<Map<String, Value>, ApiError> {
    let mut body = match data {
        Some("-") => {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|error| ApiError::Other(format!("failed to read stdin: {error}")))?;
            parse_object(&input)?
        }
        Some(data) => parse_object(data)?,
        None => Map::new(),
    };
    for field in fields {
        let (name, raw_value) = field.split_once('=').ok_or_else(|| {
            ApiError::InvalidInput(format!("field must use name=value syntax, got '{field}'"))
        })?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(ApiError::InvalidInput(format!(
                "invalid field name '{name}'"
            )));
        }
        let value = serde_json::from_str(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_string()));
        body.insert(name.to_string(), value);
    }
    Ok(body)
}

fn parse_object(data: &str) -> Result<Map<String, Value>, ApiError> {
    serde_json::from_str::<Value>(data)
        .map_err(|error| ApiError::InvalidInput(format!("invalid JSON in --data: {error}")))?
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("--data must be a JSON object".into()))
}

pub fn record_sys_id(record: &Value) -> Result<&str, ApiError> {
    record
        .get("sys_id")
        .and_then(cell_value)
        .ok_or_else(|| ApiError::Other("ServiceNow response omitted sys_id".into()))
}

pub fn print_records(records: &[Value], requested_fields: Option<&[String]>, color: bool) {
    print_records_or(records, requested_fields, color, "No records found.");
}

pub fn print_records_or(
    records: &[Value],
    requested_fields: Option<&[String]>,
    color: bool,
    empty_message: &str,
) {
    if records.is_empty() {
        println!("{empty_message}");
        return;
    }
    let fields = requested_fields
        .filter(|fields| !fields.is_empty())
        .map(<[String]>::to_vec)
        .unwrap_or_else(|| infer_fields(records));
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            fields
                .iter()
                .map(|field| display_value(record.get(field).unwrap_or(&Value::Null)))
                .collect()
        })
        .collect();
    print_table(&fields, &rows, color);
}

pub fn print_record(record: &Value, color: bool) {
    let Some(object) = record.as_object() else {
        println!("{}", display_value(record));
        return;
    };
    let rows: Vec<Vec<String>> = object
        .iter()
        .map(|(field, value)| vec![field.clone(), display_value(value)])
        .collect();
    print_table(&["FIELD".into(), "VALUE".into()], &rows, color);
}

fn infer_fields(records: &[Value]) -> Vec<String> {
    let preferred = [
        "number",
        "name",
        "short_description",
        "state",
        "priority",
        "sys_updated_on",
        "sys_id",
    ];
    let available: BTreeSet<&str> = records
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|record| record.keys().map(String::as_str))
        .collect();
    let mut fields: Vec<String> = preferred
        .iter()
        .filter(|field| available.contains(**field))
        .take(6)
        .map(|field| (*field).to_string())
        .collect();
    if fields.is_empty() {
        fields.extend(available.into_iter().take(6).map(str::to_string));
    }
    fields
}

fn cell_value(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => Some(value),
        Value::Object(object) => object
            .get("display_value")
            .and_then(Value::as_str)
            .or_else(|| object.get("value").and_then(Value::as_str)),
        _ => None,
    }
}

fn display_value(value: &Value) -> String {
    if let Some(value) = cell_value(value) {
        if value.trim().is_empty() {
            "—".into()
        } else {
            value.to_string()
        }
    } else if value.is_null() {
        "—".into()
    } else if let Some(value) = value.as_bool() {
        if value { "Yes".into() } else { "No".into() }
    } else if value.is_number() {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "?".into())
    }
}

fn print_table(headers: &[String], rows: &[Vec<String>], color: bool) {
    let header = headers.iter().map(|value| {
        let cell = Cell::new(header_label(value)).add_attribute(Attribute::Bold);
        if color { cell.fg(Color::Cyan) } else { cell }
    });
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(header)
        .set_truncation_indicator("…");
    table.set_width(terminal_width());
    if !color {
        table.force_no_tty();
    }
    for row in rows {
        let cells = row.iter().enumerate().map(|(index, value)| {
            style_cell(
                headers.get(index).map(String::as_str).unwrap_or(""),
                value,
                color,
            )
        });
        table.add_row(cells);
    }
    println!("{table}");
}

fn header_label(field: &str) -> String {
    match field {
        "short_description" => "DESCRIPTION".into(),
        "assigned_to" => "ASSIGNEE".into(),
        "sys_updated_on" => "UPDATED".into(),
        "sys_created_on" => "CREATED".into(),
        "file_name" => "FILE".into(),
        "content_type" => "TYPE".into(),
        "sys_created_by" => "CREATED BY".into(),
        other => other.replace('_', " ").to_uppercase(),
    }
}

fn terminal_width() -> u16 {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .or_else(|| terminal_size::terminal_size().map(|(terminal_size::Width(width), _)| width))
        .unwrap_or(120)
        .clamp(72, 160)
}

fn style_cell(field: &str, value: &str, color: bool) -> Cell {
    let cell = Cell::new(value);
    if !color {
        return cell;
    }
    match field {
        "number" => cell.fg(Color::Cyan).add_attribute(Attribute::Bold),
        "priority" if value.starts_with('1') => cell.fg(Color::Red).add_attribute(Attribute::Bold),
        "priority" if value.starts_with('2') => cell.fg(Color::Yellow),
        "sys_id" | "sys_updated_on" | "sys_created_on" => cell.fg(Color::DarkGrey),
        _ => cell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_override_data_and_parse_json_scalars() {
        let body = build_body(
            Some(r#"{"active":false,"priority":"4"}"#),
            &["active=true".into(), "priority=1".into()],
        )
        .unwrap();
        assert_eq!(body["active"], true);
        assert_eq!(body["priority"], 1);
    }

    #[test]
    fn human_labels_and_values_are_readable() {
        assert_eq!(header_label("short_description"), "DESCRIPTION");
        assert_eq!(header_label("assignment_group"), "ASSIGNMENT GROUP");
        assert_eq!(display_value(&Value::Null), "—");
        assert_eq!(display_value(&Value::String(String::new())), "—");
        assert_eq!(display_value(&Value::Bool(true)), "Yes");
        assert_eq!(
            display_value(&serde_json::json!({
                "value": "2",
                "display_value": "In Progress"
            })),
            "In Progress"
        );
    }
}
