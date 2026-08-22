use serde_json::Value;

use crate::api::{ApiError, DisplayValue, ServiceNowClient, validate_sys_id, validate_table};

pub async fn resolve(
    client: &ServiceNowClient,
    table: &str,
    identifier: &str,
    fields: Option<Vec<String>>,
    display_value: DisplayValue,
) -> Result<Value, ApiError> {
    validate_table(table)?;
    if let Some(sys_id) = sys_id_from_identifier(client.site_url(), table, identifier)? {
        client
            .get_record(table, &sys_id, fields.as_deref(), display_value)
            .await
    } else {
        client
            .find_one(table, "number", identifier, fields, display_value)
            .await
    }
}

pub fn attachment_sys_id(site_url: &str, identifier: &str) -> Result<String, ApiError> {
    if validate_sys_id(identifier).is_ok() {
        return Ok(identifier.to_ascii_lowercase());
    }
    let url = same_instance_url(site_url, identifier)?;
    let segments: Vec<&str> = url
        .path_segments()
        .map(|segments| segments.collect())
        .unwrap_or_default();
    let Some(index) = segments.iter().position(|segment| *segment == "attachment") else {
        return Err(ApiError::InvalidInput(
            "attachment must be a sys_id or same-instance Attachment API URL".into(),
        ));
    };
    let sys_id = segments.get(index + 1).copied().unwrap_or_default();
    validate_sys_id(sys_id)?;
    Ok(sys_id.to_ascii_lowercase())
}

fn sys_id_from_identifier(
    site_url: &str,
    expected_table: &str,
    identifier: &str,
) -> Result<Option<String>, ApiError> {
    if validate_sys_id(identifier).is_ok() {
        return Ok(Some(identifier.to_ascii_lowercase()));
    }
    if !(identifier.starts_with("https://") || identifier.starts_with("http://")) {
        return Ok(None);
    }
    let url = same_instance_url(site_url, identifier)?;
    let page = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or_default();
    let table = page.strip_suffix(".do").ok_or_else(|| {
        ApiError::InvalidInput("record URL must point to a ServiceNow form ending in .do".into())
    })?;
    if table != expected_table {
        return Err(ApiError::InvalidInput(format!(
            "record URL points to table '{table}', not '{expected_table}'"
        )));
    }
    let sys_id = url
        .query_pairs()
        .find_map(|(name, value)| (name == "sys_id").then(|| value.into_owned()))
        .ok_or_else(|| ApiError::InvalidInput("record URL has no sys_id query parameter".into()))?;
    validate_sys_id(&sys_id)?;
    Ok(Some(sys_id.to_ascii_lowercase()))
}

fn same_instance_url(site_url: &str, identifier: &str) -> Result<reqwest::Url, ApiError> {
    let expected = reqwest::Url::parse(site_url)
        .map_err(|error| ApiError::Other(format!("invalid configured instance URL: {error}")))?;
    let actual = reqwest::Url::parse(identifier)
        .map_err(|error| ApiError::InvalidInput(format!("invalid ServiceNow URL: {error}")))?;
    if expected.scheme() != actual.scheme()
        || expected.host_str() != actual.host_str()
        || expected.port_or_known_default() != actual.port_or_known_default()
    {
        return Err(ApiError::InvalidInput(format!(
            "URL belongs to a different instance; expected {}",
            expected.origin().ascii_serialization()
        )));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_urls_are_scoped_to_table_and_instance() {
        let site = "https://dev123.service-now.com";
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            sys_id_from_identifier(site, "incident", &format!("{site}/incident.do?sys_id={id}"))
                .unwrap(),
            Some(id.into())
        );
        assert!(
            sys_id_from_identifier(
                site,
                "change_request",
                &format!("{site}/incident.do?sys_id={id}")
            )
            .is_err()
        );
        assert!(
            sys_id_from_identifier(
                site,
                "incident",
                &format!("https://prod.service-now.com/incident.do?sys_id={id}")
            )
            .is_err()
        );
    }

    #[test]
    fn attachment_api_urls_are_accepted() {
        let site = "https://dev123.service-now.com";
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            attachment_sys_id(site, &format!("{site}/api/now/attachment/{id}/file")).unwrap(),
            id
        );
    }
}
