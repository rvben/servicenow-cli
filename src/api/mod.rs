mod client;

pub use client::{
    AttachmentMetadata, DisplayValue, ListOptions, ServiceNowClient, normalize_instance,
    validate_sys_id, validate_table,
};

use std::fmt;

#[derive(Debug)]
pub enum ApiError {
    InvalidInput(String),
    Auth(String),
    NotFound(String),
    Conflict(String),
    RateLimit,
    Api { status: u16, message: String },
    Http(reqwest::Error),
    Other(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "{message}"),
            Self::Auth(message) => write!(f, "authentication failed: {message}"),
            Self::NotFound(message) => write!(f, "not found: {message}"),
            Self::Conflict(message) => write!(f, "conflict: {message}"),
            Self::RateLimit => write!(f, "ServiceNow rate limit exceeded; wait and retry"),
            Self::Api { status, message } => {
                write!(f, "ServiceNow API error ({status}): {message}")
            }
            Self::Http(error) => write!(f, "HTTP error: {error}"),
            Self::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}
