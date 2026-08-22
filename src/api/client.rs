use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::ApiError;
use crate::config::AuthType;

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum DisplayValue {
    #[default]
    False,
    True,
    All,
}

impl DisplayValue {
    fn as_api_value(self) -> &'static str {
        match self {
            Self::False => "false",
            Self::True => "true",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ListOptions {
    pub query: Option<String>,
    pub fields: Option<Vec<String>>,
    pub limit: usize,
    pub offset: usize,
    pub all: bool,
    pub display_value: DisplayValue,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            query: None,
            fields: None,
            limit: 50,
            offset: 0,
            all: false,
            display_value: DisplayValue::False,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Envelope<T> {
    result: T,
}

pub struct ServiceNowClient {
    http: reqwest::Client,
    site_url: String,
    table_url: String,
}

impl ServiceNowClient {
    pub fn new(
        instance: &str,
        username: Option<&str>,
        secret: &str,
        auth_type: AuthType,
    ) -> Result<Self, ApiError> {
        let site_url = normalize_instance(instance)?;
        let authorization = match auth_type {
            AuthType::Basic => {
                let username = username
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        ApiError::InvalidInput(
                            "Basic authentication requires a username. Set SERVICENOW_USERNAME."
                                .into(),
                        )
                    })?;
                let credentials = basic_auth(username, secret);
                format!("Basic {credentials}")
            }
            AuthType::Bearer | AuthType::OAuth => format!("Bearer {secret}"),
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&authorization)
                .map_err(|error| ApiError::Other(error.to_string()))?,
        );
        headers.insert("accept", HeaderValue::from_static("application/json"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let table_url = format!("{site_url}/api/now/table");

        Ok(Self {
            http,
            site_url,
            table_url,
        })
    }

    pub fn site_url(&self) -> &str {
        &self.site_url
    }

    pub fn record_url(&self, table: &str, sys_id: &str) -> String {
        format!("{}/{table}.do?sys_id={sys_id}", self.site_url)
    }

    pub async fn list_records(
        &self,
        table: &str,
        options: &ListOptions,
    ) -> Result<Vec<Value>, ApiError> {
        validate_table(table)?;
        if options.limit == 0 && !options.all {
            return Err(ApiError::InvalidInput(
                "--limit must be greater than zero".into(),
            ));
        }

        if !options.all {
            return self
                .list_page(table, options, options.limit, options.offset)
                .await;
        }

        const PAGE_SIZE: usize = 1000;
        let mut records = Vec::new();
        let mut offset = options.offset;
        loop {
            let page = self.list_page(table, options, PAGE_SIZE, offset).await?;
            let count = page.len();
            records.extend(page);
            if count < PAGE_SIZE {
                break;
            }
            offset += count;
        }
        Ok(records)
    }

    async fn list_page(
        &self,
        table: &str,
        options: &ListOptions,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, ApiError> {
        let url = format!("{}/{table}", self.table_url);
        let mut query = vec![
            ("sysparm_limit", limit.to_string()),
            ("sysparm_offset", offset.to_string()),
            (
                "sysparm_display_value",
                options.display_value.as_api_value().into(),
            ),
            ("sysparm_exclude_reference_link", "true".into()),
            ("sysparm_no_count", "true".into()),
        ];
        if let Some(encoded_query) = options.query.as_deref() {
            query.push(("sysparm_query", encoded_query.to_string()));
        }
        if let Some(fields) = options.fields.as_ref().filter(|fields| !fields.is_empty()) {
            query.push(("sysparm_fields", fields.join(",")));
        }

        let response = self.http.get(url).query(&query).send().await?;
        self.decode::<Envelope<Vec<Value>>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    pub async fn get_record(
        &self,
        table: &str,
        sys_id: &str,
        fields: Option<&[String]>,
        display_value: DisplayValue,
    ) -> Result<Value, ApiError> {
        validate_table(table)?;
        validate_sys_id(sys_id)?;
        let url = format!("{}/{table}/{sys_id}", self.table_url);
        let mut query = vec![
            (
                "sysparm_display_value",
                display_value.as_api_value().to_string(),
            ),
            ("sysparm_exclude_reference_link", "true".into()),
        ];
        if let Some(fields) = fields.filter(|fields| !fields.is_empty()) {
            query.push(("sysparm_fields", fields.join(",")));
        }
        let response = self.http.get(url).query(&query).send().await?;
        self.decode::<Envelope<Value>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    pub async fn find_one(
        &self,
        table: &str,
        field: &str,
        value: &str,
        fields: Option<Vec<String>>,
        display_value: DisplayValue,
    ) -> Result<Value, ApiError> {
        validate_field_name(field)?;
        validate_query_literal(value)?;
        let records = self
            .list_records(
                table,
                &ListOptions {
                    query: Some(format!("{field}={value}")),
                    fields,
                    limit: 2,
                    display_value,
                    ..ListOptions::default()
                },
            )
            .await?;
        match records.as_slice() {
            [] => Err(ApiError::NotFound(format!(
                "{table} record with {field}={value}"
            ))),
            [record] => Ok(record.clone()),
            _ => Err(ApiError::Conflict(format!(
                "more than one {table} record matched {field}={value}"
            ))),
        }
    }

    pub async fn create_record(
        &self,
        table: &str,
        body: &Map<String, Value>,
    ) -> Result<Value, ApiError> {
        validate_table(table)?;
        require_body(body)?;
        let response = self
            .http
            .post(format!("{}/{table}", self.table_url))
            .json(body)
            .send()
            .await?;
        self.decode::<Envelope<Value>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    pub async fn update_record(
        &self,
        table: &str,
        sys_id: &str,
        body: &Map<String, Value>,
    ) -> Result<Value, ApiError> {
        validate_table(table)?;
        validate_sys_id(sys_id)?;
        require_body(body)?;
        let response = self
            .http
            .patch(format!("{}/{table}/{sys_id}", self.table_url))
            .json(body)
            .send()
            .await?;
        self.decode::<Envelope<Value>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    pub async fn delete_record(&self, table: &str, sys_id: &str) -> Result<(), ApiError> {
        validate_table(table)?;
        validate_sys_id(sys_id)?;
        let response = self
            .http
            .delete(format!("{}/{table}/{sys_id}", self.table_url))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(map_response_error(response).await)
        }
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, ApiError> {
        if !response.status().is_success() {
            return Err(map_response_error(response).await);
        }
        response.json().await.map_err(ApiError::Http)
    }
}

fn require_body(body: &Map<String, Value>) -> Result<(), ApiError> {
    if body.is_empty() {
        Err(ApiError::InvalidInput(
            "at least one field must be supplied".into(),
        ))
    } else {
        Ok(())
    }
}

pub fn validate_table(table: &str) -> Result<(), ApiError> {
    if !table.is_empty()
        && table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "invalid table name '{table}'; use letters, digits, and underscores"
        )))
    }
}

pub fn validate_sys_id(sys_id: &str) -> Result<(), ApiError> {
    if sys_id.len() == 32 && sys_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "invalid sys_id '{sys_id}'; expected 32 hexadecimal characters"
        )))
    }
}

fn validate_field_name(field: &str) -> Result<(), ApiError> {
    validate_table(field)
        .map_err(|_| ApiError::InvalidInput(format!("invalid field name '{field}'")))
}

fn validate_query_literal(value: &str) -> Result<(), ApiError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "invalid record identifier '{value}'"
        )))
    }
}

pub fn normalize_instance(instance: &str) -> Result<String, ApiError> {
    let instance = instance.trim().trim_end_matches('/');
    if instance.is_empty() {
        return Err(ApiError::InvalidInput(
            "ServiceNow instance cannot be empty".into(),
        ));
    }
    let url = if instance.starts_with("http://") || instance.starts_with("https://") {
        instance.to_string()
    } else if instance.contains('.') {
        format!("https://{instance}")
    } else {
        format!("https://{instance}.service-now.com")
    };
    let parsed = reqwest::Url::parse(&url).map_err(|error| {
        ApiError::InvalidInput(format!("invalid ServiceNow instance URL: {error}"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(ApiError::InvalidInput(
            "ServiceNow instance must be an HTTP(S) origin without credentials, a path, query, or fragment".into(),
        ));
    }
    Ok(url.trim_end_matches('/').to_string())
}

fn basic_auth(username: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{username}:{password}");
    let bytes = input.as_bytes();
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(b0 >> 2) as usize] as char);
        encoded.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

async fn map_response_error(response: reqwest::Response) -> ApiError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            let error = value.get("error")?;
            let message = error.get("message")?.as_str()?;
            let detail = error.get("detail").and_then(Value::as_str).unwrap_or("");
            Some(if detail.is_empty() {
                message.to_string()
            } else {
                format!("{message}: {detail}")
            })
        })
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                format!("HTTP {status}")
            } else {
                trimmed.chars().take(500).collect()
            }
        });
    match status {
        401 | 403 => ApiError::Auth(message),
        404 => ApiError::NotFound(message),
        409 => ApiError::Conflict(message),
        429 => ApiError::RateLimit,
        _ => ApiError::Api { status, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_short_instance_name() {
        assert_eq!(
            normalize_instance("dev12345").unwrap(),
            "https://dev12345.service-now.com"
        );
        assert!(normalize_instance("https://example.service-now.com/api/now").is_err());
        assert!(normalize_instance("ftp://example.service-now.com").is_err());
    }

    #[test]
    fn validates_table_and_sys_id() {
        assert!(validate_table("u_custom_table").is_ok());
        assert!(validate_table("incident/1").is_err());
        assert!(validate_sys_id("0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_sys_id("INC0010001").is_err());
    }
}
