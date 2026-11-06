use std::io::Write;
use std::path::Path;

use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::io::ReaderStream;

use super::ApiError;
use crate::config::AuthType;
use crate::cookies::BrowserCookies;

const ATTACHMENT_TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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

/// ServiceNow attachment metadata as returned by the Attachment API.
///
/// Only `sys_id` is guaranteed present; deserialization fails clearly if it
/// is missing. Every other field can legitimately be absent (most often an
/// ACL hiding it), which is distinct from being present and empty, so those
/// fields are `Option<String>` rather than defaulting to `""`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AttachmentMetadata {
    pub sys_id: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<String>,
    pub table_name: Option<String>,
    pub table_sys_id: Option<String>,
    pub download_link: Option<String>,
    pub sys_created_by: Option<String>,
    pub sys_created_on: Option<String>,
}

pub struct ServiceNowClient {
    http: reqwest::Client,
    auth_type: AuthType,
    browser_cookies: Option<BrowserCookies>,
    site_url: String,
    table_url: String,
    attachment_url: String,
    catalog_url: String,
}

impl ServiceNowClient {
    pub fn new(
        instance: &str,
        username: Option<&str>,
        secret: &str,
        auth_type: AuthType,
    ) -> Result<Self, ApiError> {
        Self::new_with_user_token(instance, username, secret, auth_type, None)
    }

    pub fn new_with_user_token(
        instance: &str,
        username: Option<&str>,
        secret: &str,
        auth_type: AuthType,
        browser_user_token: Option<&str>,
    ) -> Result<Self, ApiError> {
        let site_url = normalize_instance(instance)?;
        let browser_cookies = matches!(auth_type, AuthType::Browser)
            .then(|| BrowserCookies::new(&site_url, secret))
            .transpose()?;
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
                Some(format!("Basic {credentials}"))
            }
            AuthType::Browser => None,
            AuthType::Bearer | AuthType::OAuth => Some(format!("Bearer {secret}")),
        };

        let mut headers = HeaderMap::new();
        if let Some(authorization) = authorization {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&authorization)
                    .map_err(|error| ApiError::Other(error.to_string()))?,
            );
        } else {
            if let Some(user_token) = browser_user_token {
                let mut user_token = HeaderValue::from_str(user_token).map_err(|_| {
                    ApiError::InvalidInput("stored browser user token is invalid".into())
                })?;
                user_token.set_sensitive(true);
                headers.insert(HeaderName::from_static("x-usertoken"), user_token);
            }
        }
        headers.insert("accept", HeaderValue::from_static("application/json"));

        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30));
        if matches!(auth_type, AuthType::Browser) {
            builder = builder
                .redirect(reqwest::redirect::Policy::none())
                .cookie_provider(
                    browser_cookies
                        .as_ref()
                        .expect("browser cookie jar was initialized")
                        .provider(),
                );
        }
        let http = builder.build()?;
        let table_url = format!("{site_url}/api/now/table");
        let attachment_url = format!("{site_url}/api/now/attachment");
        let catalog_url = format!("{site_url}/api/sn_sc/servicecatalog");

        Ok(Self {
            http,
            auth_type,
            browser_cookies,
            site_url,
            table_url,
            attachment_url,
            catalog_url,
        })
    }

    pub fn site_url(&self) -> &str {
        &self.site_url
    }

    /// Return a cookie header only when ServiceNow rotated the browser session.
    ///
    /// Callers with a persistent credential store can save this value after a
    /// successful operation so the next CLI process resumes the current
    /// server-side session. Environment-provided credentials remain read-only.
    pub fn refreshed_browser_cookie(&self) -> Option<String> {
        self.browser_cookies
            .as_ref()
            .and_then(BrowserCookies::refreshed_header)
    }

    pub fn record_url(&self, table: &str, sys_id: &str) -> String {
        format!("{}/{table}.do?sys_id={sys_id}", self.site_url)
    }

    pub async fn list_attachments(
        &self,
        table: &str,
        table_sys_id: &str,
        limit: usize,
        all: bool,
    ) -> Result<Vec<AttachmentMetadata>, ApiError> {
        validate_table(table)?;
        validate_sys_id(table_sys_id)?;
        if limit == 0 && !all {
            return Err(ApiError::InvalidInput(
                "--limit must be greater than zero".into(),
            ));
        }

        const PAGE_SIZE: usize = 1000;
        let page_size = if all { PAGE_SIZE } else { limit };
        let mut offset = 0usize;
        let mut attachments = Vec::new();
        loop {
            let query = [
                ("sysparm_limit", page_size.to_string()),
                ("sysparm_offset", offset.to_string()),
                (
                    "sysparm_query",
                    format!(
                        "table_name={table}^table_sys_id={table_sys_id}^ORDERBYDESCsys_created_on"
                    ),
                ),
            ];
            let response = self
                .http
                .get(&self.attachment_url)
                .query(&query)
                .send()
                .await?;
            let page = self
                .decode::<Envelope<Vec<AttachmentMetadata>>>(response)
                .await?
                .result;
            let count = page.len();
            attachments.extend(page);
            if !all || count < page_size {
                break;
            }
            offset += count;
        }
        Ok(attachments)
    }

    pub async fn get_attachment(
        &self,
        attachment_sys_id: &str,
    ) -> Result<AttachmentMetadata, ApiError> {
        validate_sys_id(attachment_sys_id)?;
        let response = self
            .http
            .get(format!("{}/{attachment_sys_id}", self.attachment_url))
            .send()
            .await?;
        self.decode::<Envelope<AttachmentMetadata>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    pub async fn upload_attachment_file(
        &self,
        table: &str,
        table_sys_id: &str,
        file_name: &str,
        content_type: &str,
        path: &Path,
    ) -> Result<AttachmentMetadata, ApiError> {
        let file = tokio::fs::File::open(path).await.map_err(|error| {
            ApiError::Other(format!(
                "failed to open attachment {}: {error}",
                path.display()
            ))
        })?;
        let content_length = file
            .metadata()
            .await
            .map_err(|error| {
                ApiError::Other(format!(
                    "failed to inspect attachment {}: {error}",
                    path.display()
                ))
            })?
            .len();
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        self.upload_attachment_body(
            table,
            table_sys_id,
            file_name,
            content_type,
            Some(content_length),
            body,
        )
        .await
    }

    pub async fn upload_attachment_bytes(
        &self,
        table: &str,
        table_sys_id: &str,
        file_name: &str,
        content_type: &str,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<AttachmentMetadata, ApiError> {
        let bytes = bytes.into();
        let content_length = bytes.len() as u64;
        self.upload_attachment_body(
            table,
            table_sys_id,
            file_name,
            content_type,
            Some(content_length),
            bytes.into(),
        )
        .await
    }

    async fn upload_attachment_body(
        &self,
        table: &str,
        table_sys_id: &str,
        file_name: &str,
        content_type: &str,
        content_length: Option<u64>,
        body: reqwest::Body,
    ) -> Result<AttachmentMetadata, ApiError> {
        validate_table(table)?;
        validate_sys_id(table_sys_id)?;
        if file_name.trim().is_empty() || file_name.chars().any(char::is_control) {
            return Err(ApiError::InvalidInput(
                "attachment file name cannot be empty or contain control characters".into(),
            ));
        }
        if file_name.contains(['/', '\\']) {
            return Err(ApiError::InvalidInput(
                "attachment file name cannot contain path separators".into(),
            ));
        }
        let content_type = HeaderValue::from_str(content_type).map_err(|_| {
            ApiError::InvalidInput(format!("invalid content type '{content_type}'"))
        })?;
        let query = [
            ("table_name", table),
            ("table_sys_id", table_sys_id),
            ("file_name", file_name),
        ];
        let mut request = self
            .http
            .post(format!("{}/file", self.attachment_url))
            .query(&query)
            .header(CONTENT_TYPE, content_type)
            .timeout(ATTACHMENT_TRANSFER_TIMEOUT)
            .body(body);
        if let Some(content_length) = content_length {
            request = request.header(CONTENT_LENGTH, content_length);
        }
        let response = request.send().await?;
        self.decode::<Envelope<AttachmentMetadata>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    pub async fn download_attachment(
        &self,
        attachment_sys_id: &str,
        writer: &mut impl Write,
    ) -> Result<u64, ApiError> {
        validate_sys_id(attachment_sys_id)?;
        let mut response = self
            .http
            .get(format!("{}/{attachment_sys_id}/file", self.attachment_url))
            .header(ACCEPT, "*/*")
            .timeout(ATTACHMENT_TRANSFER_TIMEOUT)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(self.response_error(response).await);
        }
        let mut written = 0u64;
        while let Some(chunk) = response.chunk().await? {
            writer.write_all(&chunk).map_err(|error| {
                ApiError::Other(format!("failed to write downloaded attachment: {error}"))
            })?;
            written += chunk.len() as u64;
        }
        writer.flush().map_err(|error| {
            ApiError::Other(format!("failed to flush downloaded attachment: {error}"))
        })?;
        Ok(written)
    }

    pub async fn delete_attachment(&self, attachment_sys_id: &str) -> Result<(), ApiError> {
        validate_sys_id(attachment_sys_id)?;
        let response = self
            .http
            .delete(format!("{}/{attachment_sys_id}", self.attachment_url))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.response_error(response).await)
        }
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
            Err(self.response_error(response).await)
        }
    }

    /// Searches the Service Catalog. Results include producers, guides and
    /// content items alongside orderable items.
    pub async fn search_catalog_items(
        &self,
        text: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, ApiError> {
        if limit == 0 {
            return Err(ApiError::InvalidInput(
                "--limit must be greater than zero".into(),
            ));
        }
        let mut query = vec![
            ("sysparm_limit", limit.to_string()),
            ("sysparm_offset", offset.to_string()),
        ];
        if let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) {
            query.push(("sysparm_text", text.to_string()));
        }
        let response = self
            .http
            .get(format!("{}/items", self.catalog_url))
            .query(&query)
            .send()
            .await?;
        self.decode::<Envelope<Vec<Value>>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    /// Fetches a catalog item with its variables.
    pub async fn get_catalog_item(&self, sys_id: &str) -> Result<Value, ApiError> {
        validate_sys_id(sys_id)?;
        let response = self
            .http
            .get(format!("{}/items/{sys_id}", self.catalog_url))
            .send()
            .await?;
        match self.decode::<Envelope<Value>>(response).await {
            Ok(envelope) => Ok(envelope.result),
            Err(ApiError::Api {
                status: 400,
                message,
            }) => Err(ApiError::NotFound(format!(
                "catalog item {sys_id}: {message}"
            ))),
            Err(error) => Err(error),
        }
    }

    /// Submits `order_now` exactly once. A failure after the request may have
    /// reached the instance says so, because a retry could order twice.
    ///
    /// Pass `scrub` when the body carries sensitive values: an error response
    /// is then shown only as its decoded JSON message with `scrub` applied,
    /// and withheld when it is not JSON or the message still holds an escape,
    /// HTML entity or URL escape, because the instance can quote the request
    /// back in an encoded form.
    pub async fn order_catalog_item(
        &self,
        sys_id: &str,
        body: &Value,
        scrub: Option<&dyn Fn(&str) -> String>,
    ) -> Result<Value, ApiError> {
        validate_sys_id(sys_id)?;
        let request = self
            .http
            .post(format!("{}/items/{sys_id}/order_now", self.catalog_url))
            .json(body);
        self.send_once(request, scrub, "order", MAY_HAVE_ORDERED)
            .await
    }

    /// Adds an item to the caller's cart exactly once, with the same
    /// guarantees and `scrub` handling as [`Self::order_catalog_item`]. The
    /// instance ignores `sysparm_requested_for` here: whoever submits the cart
    /// is the requester.
    pub async fn add_catalog_item_to_cart(
        &self,
        sys_id: &str,
        body: &Value,
        scrub: Option<&dyn Fn(&str) -> String>,
    ) -> Result<Value, ApiError> {
        validate_sys_id(sys_id)?;
        let request = self
            .http
            .post(format!("{}/items/{sys_id}/add_to_cart", self.catalog_url))
            .json(body);
        self.send_once(request, scrub, "add-to-cart", MAY_HAVE_ADDED_TO_CART)
            .await
    }

    /// Reads the caller's cart, the one the portal shows. The instance creates
    /// an empty cart when the caller has none.
    pub async fn get_cart(&self) -> Result<Value, ApiError> {
        let response = self
            .http
            .get(format!("{}/cart", self.catalog_url))
            .send()
            .await?;
        self.decode::<Envelope<Value>>(response)
            .await
            .map(|envelope| envelope.result)
    }

    /// Orders everything in the caller's cart exactly once, with the same
    /// `scrub` handling as [`Self::order_catalog_item`]. This works whether or
    /// not the instance requires a separate checkout step.
    pub async fn submit_cart(
        &self,
        scrub: Option<&dyn Fn(&str) -> String>,
    ) -> Result<Value, ApiError> {
        let request = self
            .http
            .post(format!("{}/cart/submit_order", self.catalog_url));
        self.send_once(request, scrub, "cart submission", MAY_HAVE_ORDERED)
            .await
    }

    /// Removes one item from the caller's cart.
    pub async fn remove_cart_item(&self, cart_item_id: &str) -> Result<(), ApiError> {
        validate_sys_id(cart_item_id)?;
        let response = self
            .http
            .delete(format!("{}/cart/{cart_item_id}", self.catalog_url))
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        match self.response_error(response).await {
            // The instance answers an unknown or already removed item with a
            // plain 400.
            ApiError::Api {
                status: 400,
                message,
            } => Err(ApiError::NotFound(format!(
                "cart item {cart_item_id}: {message}"
            ))),
            error => Err(error),
        }
    }

    /// Removes every item from the cart `cart_id`.
    pub async fn empty_cart(&self, cart_id: &str) -> Result<(), ApiError> {
        validate_sys_id(cart_id)?;
        let response = self
            .http
            .delete(format!("{}/cart/{cart_id}/empty", self.catalog_url))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.response_error(response).await)
        }
    }

    /// Sends a request that must not be repeated, and decodes its result.
    /// Once the request may have reached the instance, a failure carries
    /// `uncertain`, which tells the caller what may have happened.
    async fn send_once(
        &self,
        request: reqwest::RequestBuilder,
        scrub: Option<&dyn Fn(&str) -> String>,
        what: &str,
        uncertain: &str,
    ) -> Result<Value, ApiError> {
        let response = request.send().await.map_err(|error| {
            if error.is_connect() {
                ApiError::Http(error)
            } else {
                ApiError::Other(format!(
                    "{what} request failed after it was sent ({error}); {uncertain}"
                ))
            }
        })?;
        let decoded = if response.status().is_success() {
            response
                .json::<Envelope<Value>>()
                .await
                .map_err(ApiError::Http)
        } else {
            Err(self.response_error_with(response, scrub).await)
        };
        match decoded {
            Ok(envelope) => Ok(envelope.result),
            Err(ApiError::Api { status, message }) if status >= 500 => Err(ApiError::Api {
                status,
                message: format!("{message}; {uncertain}"),
            }),
            Err(ApiError::Http(error)) => Err(ApiError::Other(format!(
                "could not read the {what} response ({error}); {uncertain}"
            ))),
            Err(error) => Err(error),
        }
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, ApiError> {
        if !response.status().is_success() {
            return Err(self.response_error(response).await);
        }
        response.json().await.map_err(ApiError::Http)
    }

    async fn response_error(&self, response: reqwest::Response) -> ApiError {
        self.response_error_with(response, None).await
    }

    async fn response_error_with(
        &self,
        response: reqwest::Response,
        scrub: Option<&dyn Fn(&str) -> String>,
    ) -> ApiError {
        let browser_session_rejected = matches!(self.auth_type, AuthType::Browser)
            && (response.status() == reqwest::StatusCode::UNAUTHORIZED
                || response.status().is_redirection());
        let error = map_response_error(response, scrub).await;
        if browser_session_rejected {
            ApiError::Auth(format!("browser session expired or was rejected: {error}"))
        } else if matches!(self.auth_type, AuthType::Browser)
            && let ApiError::Auth(message) = error
        {
            ApiError::Auth(format!(
                "browser session expired or was rejected: {message}"
            ))
        } else {
            error
        }
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

/// Remediation for any catalog order whose outcome the CLI cannot establish.
pub const MAY_HAVE_ORDERED: &str =
    "the order may have been placed; check `servicenow catalog requests mine` before retrying";

/// Remediation for an add-to-cart request whose outcome the CLI cannot
/// establish.
pub const MAY_HAVE_ADDED_TO_CART: &str = "the item may have been added to your cart, though nothing was ordered; check `servicenow catalog cart show` before retrying";

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

/// Characters that start a JSON or string escape (`\`), an HTML entity
/// (`&`) or a URL escape (`%`). A message still containing one after
/// scrubbing could hold a sensitive value in encoded form.
const ESCAPE_MARKERS: [char; 3] = ['\\', '&', '%'];

/// Turns an error response into an [`ApiError`]. With `scrub`, only a
/// decoded JSON message is shown, with `scrub` applied; anything that could
/// still hold an encoded secret (a non-JSON body, or a message containing an
/// escape, entity or URL escape) is withheld.
async fn map_response_error(
    response: reqwest::Response,
    scrub: Option<&dyn Fn(&str) -> String>,
) -> ApiError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let json_message = serde_json::from_str::<Value>(&body).ok().and_then(|value| {
        let error = value.get("error")?;
        let message = error.get("message")?.as_str()?;
        let detail = error.get("detail").and_then(Value::as_str).unwrap_or("");
        Some(if detail.is_empty() {
            message.to_string()
        } else {
            format!("{message}: {detail}")
        })
    });
    let trimmed = body.trim();
    let message = match (scrub, json_message) {
        (_, None) if trimmed.is_empty() => format!("HTTP {status}"),
        (None, Some(message)) => message,
        (None, None) => trimmed.chars().take(500).collect(),
        (Some(scrub), Some(message)) => {
            let scrubbed = scrub(&message);
            if scrubbed.contains(ESCAPE_MARKERS) {
                format!(
                    "HTTP {status} (error message withheld: it contains escaped or encoded text and the order contains sensitive values)"
                )
            } else {
                scrubbed
            }
        }
        (Some(_), None) => {
            format!("HTTP {status} (response body withheld: the order contains sensitive values)")
        }
    };
    match status {
        401 => ApiError::Auth(message),
        403 => ApiError::Permission(message),
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
