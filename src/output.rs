use std::collections::BTreeSet;
use std::io::IsTerminal;

use serde_json::Value;

use crate::api::ApiError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Text,
    Json,
    JsonLines,
    Yaml,
    Csv,
}

#[derive(Clone, Copy, Debug)]
pub struct OutputConfig {
    // Retained as a source-compatible shorthand for existing command code.
    pub json: bool,
    pub quiet: bool,
    pub format: OutputFormat,
    pub color: bool,
}

impl OutputConfig {
    pub fn new(
        output: &str,
        json_alias: bool,
        quiet: bool,
        no_color: bool,
    ) -> Result<Self, ApiError> {
        let format = match output {
            "auto" if json_alias || !std::io::stdout().is_terminal() => OutputFormat::Json,
            "auto" | "text" | "table" => OutputFormat::Text,
            "json" => OutputFormat::Json,
            "jsonl" | "ndjson" => OutputFormat::JsonLines,
            "yaml" | "yml" => OutputFormat::Yaml,
            "csv" => OutputFormat::Csv,
            other => {
                return Err(ApiError::InvalidInput(format!(
                    "invalid output format '{other}'; expected auto, table, text, json, jsonl, yaml, or csv"
                )));
            }
        };
        let color = format == OutputFormat::Text
            && std::io::stdout().is_terminal()
            && !no_color
            && std::env::var_os("NO_COLOR").is_none();
        Ok(Self {
            json: format != OutputFormat::Text,
            quiet,
            format,
            color,
        })
    }

    pub fn value(&self, value: &Value) {
        match self.format {
            OutputFormat::Text | OutputFormat::Json => println!(
                "{}",
                serde_json::to_string_pretty(value).expect("JSON serialization cannot fail")
            ),
            OutputFormat::JsonLines => print_json_lines(value),
            OutputFormat::Yaml => print!(
                "{}",
                serde_saphyr::to_string(value).expect("YAML serialization cannot fail")
            ),
            OutputFormat::Csv => print_csv(value),
        }
    }

    pub fn message(&self, message: &str) {
        if !self.quiet {
            eprintln!("{message}");
        }
    }

    pub fn success(&self, message: &str) {
        if self.quiet {
            return;
        }
        if self.color {
            eprintln!("\x1b[32m✓\x1b[0m {message}");
        } else {
            eprintln!("✓ {message}");
        }
    }

    pub fn heading(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[1;36m{value}\x1b[0m")
        } else {
            value.into()
        }
    }
}

fn print_json_lines(value: &Value) {
    if let Some(records) = value.get("result").and_then(Value::as_array) {
        for record in records {
            println!(
                "{}",
                serde_json::to_string(record).expect("JSON serialization cannot fail")
            );
        }
    } else {
        println!(
            "{}",
            serde_json::to_string(value).expect("JSON serialization cannot fail")
        );
    }
}

fn print_csv(value: &Value) {
    let owned;
    let records = if let Some(records) = value.get("result").and_then(Value::as_array) {
        records.as_slice()
    } else if let Some(record) = value.get("result") {
        owned = vec![record.clone()];
        owned.as_slice()
    } else {
        owned = vec![value.clone()];
        owned.as_slice()
    };
    let headers: Vec<String> = records
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|record| record.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut writer = csv::Writer::from_writer(std::io::stdout());
    writer
        .write_record(&headers)
        .expect("stdout should accept CSV header");
    for record in records {
        let row = headers
            .iter()
            .map(|header| record.get(header).map(csv_cell).unwrap_or_default());
        writer
            .write_record(row)
            .expect("stdout should accept CSV row");
    }
    writer.flush().expect("stdout should flush");
}

fn csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Object(value) => value
            .get("display_value")
            .and_then(Value::as_str)
            .or_else(|| value.get("value").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default()),
        Value::Array(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

pub mod exit_codes {
    pub const GENERAL: i32 = 1;
    pub const INPUT: i32 = 2;
    pub const AUTH: i32 = 3;
    pub const NOT_FOUND: i32 = 4;
    pub const API: i32 = 5;
    pub const RATE_LIMIT: i32 = 6;
    pub const CONFLICT: i32 = 7;
}

pub fn error_kind(error: &ApiError) -> &'static str {
    match error {
        ApiError::InvalidInput(_) => "invalid_input",
        ApiError::Auth(_) => "auth",
        ApiError::NotFound(_) => "not_found",
        ApiError::Conflict(_) => "conflict",
        ApiError::RateLimit => "rate_limit",
        ApiError::Api { .. } => "api_error",
        ApiError::Http(_) | ApiError::Other(_) => "unexpected_error",
    }
}

pub fn exit_code(error: &ApiError) -> i32 {
    match error {
        ApiError::InvalidInput(_) => exit_codes::INPUT,
        ApiError::Auth(_) => exit_codes::AUTH,
        ApiError::NotFound(_) => exit_codes::NOT_FOUND,
        ApiError::Conflict(_) => exit_codes::CONFLICT,
        ApiError::RateLimit => exit_codes::RATE_LIMIT,
        ApiError::Api { .. } => exit_codes::API,
        ApiError::Http(_) | ApiError::Other(_) => exit_codes::GENERAL,
    }
}

pub fn print_error(error: &ApiError, machine_readable: bool) {
    if machine_readable {
        eprintln!(
            "{}",
            serde_json::json!({
                "error": {
                    "kind": error_kind(error),
                    "message": error.to_string()
                }
            })
        );
    } else {
        eprintln!("error: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_aliases_are_stable() {
        assert_eq!(
            OutputConfig::new("table", false, false, true)
                .unwrap()
                .format,
            OutputFormat::Text
        );
        assert_eq!(
            OutputConfig::new("ndjson", false, false, true)
                .unwrap()
                .format,
            OutputFormat::JsonLines
        );
        assert!(OutputConfig::new("xml", false, false, true).is_err());
    }
}
