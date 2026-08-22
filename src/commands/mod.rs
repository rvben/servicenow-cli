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
    if records.is_empty() {
        println!("No records found.");
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
        value.to_string()
    } else if value.is_null() {
        "-".into()
    } else if let Some(value) = value.as_bool() {
        value.to_string()
    } else if value.is_number() {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "?".into())
    }
}

fn print_table(headers: &[String], rows: &[Vec<String>], color: bool) {
    let header = headers.iter().map(|value| {
        let cell = Cell::new(value).add_attribute(Attribute::Bold);
        if color { cell.fg(Color::Cyan) } else { cell }
    });
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(header)
        .set_truncation_indicator("…");
    if !color {
        table.force_no_tty();
    }
    for row in rows {
        table.add_row(row);
    }
    println!("{table}");
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
}
