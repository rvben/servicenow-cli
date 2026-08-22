use std::io::IsTerminal;

use serde_json::Value;

use crate::api::ApiError;

#[derive(Clone, Copy, Debug)]
pub struct OutputConfig {
    pub json: bool,
    pub quiet: bool,
}

impl OutputConfig {
    pub fn new(output: &str, json_alias: bool, quiet: bool) -> Result<Self, ApiError> {
        let json = match output {
            "auto" => json_alias || !std::io::stdout().is_terminal(),
            "json" => true,
            "text" => false,
            other => {
                return Err(ApiError::InvalidInput(format!(
                    "invalid output format '{other}'; expected auto, text, or json"
                )));
            }
        };
        Ok(Self { json, quiet })
    }

    pub fn value(&self, value: &Value) {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("JSON serialization cannot fail")
        );
    }

    pub fn message(&self, message: &str) {
        if !self.quiet {
            eprintln!("{message}");
        }
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
