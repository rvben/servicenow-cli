use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use servicenow_cli::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use servicenow_cli::config::AuthType;

struct Pdi {
    instance: String,
    username: String,
    password: String,
}

impl Pdi {
    fn from_env() -> Self {
        Self {
            instance: required_env("SERVICENOW_E2E_INSTANCE"),
            username: required_env("SERVICENOW_E2E_USERNAME"),
            password: required_env("SERVICENOW_E2E_PASSWORD"),
        }
    }

    fn client(&self) -> ServiceNowClient {
        ServiceNowClient::new(
            &self.instance,
            Some(&self.username),
            &self.password,
            AuthType::Basic,
        )
        .expect("valid PDI configuration")
    }
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is required; copy .env.e2e.example to .env.e2e and fill it in")
    })
}

fn object(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn string_field<'a>(record: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    record
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Other(format!("record has no string field {field}: {record}")))
}

/// Exercises the real Table API lifecycle against an isolated incident and
/// removes the fixture even when an assertion in the verification phase fails.
#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_incident_crud_lifecycle() {
    let pdi = Pdi::from_env();
    let client = pdi.client();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let marker = format!("servicenow-cli-e2e-{unique}");

    let created = client
        .create_record(
            "incident",
            &object([
                ("short_description", Value::String(marker.clone())),
                (
                    "description",
                    Value::String("Temporary fixture created by servicenow-cli tests".into()),
                ),
                ("impact", Value::String("3".into())),
                ("urgency", Value::String("3".into())),
            ]),
        )
        .await
        .expect("create test incident");
    let sys_id = string_field(&created, "sys_id")
        .expect("created record has sys_id")
        .to_string();

    let verification: Result<(), ApiError> = async {
        let fetched = client
            .get_record("incident", &sys_id, None, DisplayValue::False)
            .await?;
        if string_field(&fetched, "short_description")? != marker {
            return Err(ApiError::Other(
                "created incident did not preserve short_description".into(),
            ));
        }

        let updated = client
            .update_record(
                "incident",
                &sys_id,
                &object([
                    (
                        "description",
                        Value::String("Updated by lifecycle test".into()),
                    ),
                    (
                        "work_notes",
                        Value::String("E2E update verification".into()),
                    ),
                ]),
            )
            .await?;
        if string_field(&updated, "description")? != "Updated by lifecycle test" {
            return Err(ApiError::Other(
                "updated incident did not preserve description".into(),
            ));
        }

        let listed = client
            .list_records(
                "incident",
                &ListOptions {
                    query: Some(format!("sys_id={sys_id}")),
                    fields: Some(vec![
                        "sys_id".into(),
                        "number".into(),
                        "short_description".into(),
                    ]),
                    limit: 1,
                    ..ListOptions::default()
                },
            )
            .await?;
        if listed.len() != 1 || string_field(&listed[0], "sys_id")? != sys_id {
            return Err(ApiError::Other(
                "sys_id query did not return exactly the test incident".into(),
            ));
        }

        let display = client
            .get_record(
                "incident",
                &sys_id,
                Some(&["sys_id".into(), "state".into(), "priority".into()]),
                DisplayValue::All,
            )
            .await?;
        if display.get("state").is_none() {
            return Err(ApiError::Other(
                "display-value response omitted the requested state field".into(),
            ));
        }
        Ok(())
    }
    .await;

    let cleanup = client.delete_record("incident", &sys_id).await;
    if let Err(error) = verification {
        cleanup.expect("cleanup after failed verification");
        panic!("PDI lifecycle verification failed: {error}");
    }
    cleanup.expect("delete test incident");

    let deleted = client
        .get_record("incident", &sys_id, None, DisplayValue::False)
        .await;
    assert!(matches!(deleted, Err(ApiError::NotFound(_))));
}

/// Confirms that a credential failure has the public typed error contract.
#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_rejects_invalid_credentials_as_auth_error() {
    let pdi = Pdi::from_env();
    let bad_client = ServiceNowClient::new(
        &pdi.instance,
        Some(&pdi.username),
        "deliberately-invalid-password",
        AuthType::Basic,
    )
    .expect("valid PDI URL");
    let result = bad_client
        .list_records("incident", &ListOptions::default())
        .await;
    assert!(matches!(result, Err(ApiError::Auth(_))));
}

/// Exercises upload, list, metadata, binary download, and deletion against a
/// real attachment while guaranteeing cleanup of both attachment and incident.
#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_attachment_lifecycle() {
    let pdi = Pdi::from_env();
    let client = pdi.client();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let marker = format!("servicenow-cli-attachment-e2e-{unique}");
    let file_name = format!("{marker}.txt");
    let contents = format!("temporary attachment created by {marker}").into_bytes();

    let incident = client
        .create_record(
            "incident",
            &object([
                ("short_description", Value::String(marker.clone())),
                (
                    "description",
                    Value::String("Temporary attachment lifecycle fixture".into()),
                ),
                ("impact", Value::String("3".into())),
                ("urgency", Value::String("3".into())),
            ]),
        )
        .await
        .expect("create attachment test incident");
    let incident_id = string_field(&incident, "sys_id")
        .expect("created incident has sys_id")
        .to_string();

    let uploaded = match client
        .upload_attachment_bytes(
            "incident",
            &incident_id,
            &file_name,
            "text/plain",
            contents.clone(),
        )
        .await
    {
        Ok(uploaded) => uploaded,
        Err(error) => {
            client
                .delete_record("incident", &incident_id)
                .await
                .expect("cleanup incident after failed attachment upload");
            panic!("upload test attachment: {error}");
        }
    };
    let attachment_id = uploaded.sys_id.clone();

    let verification: Result<(), ApiError> = async {
        if uploaded.file_name != file_name
            || uploaded.table_name != "incident"
            || uploaded.table_sys_id != incident_id
        {
            return Err(ApiError::Other(format!(
                "unexpected uploaded attachment metadata: {uploaded:?}"
            )));
        }

        let listed = client
            .list_attachments("incident", &incident_id, 100, false)
            .await?;
        if !listed.iter().any(|item| item.sys_id == attachment_id) {
            return Err(ApiError::Other(
                "uploaded attachment was absent from record listing".into(),
            ));
        }

        let metadata = client.get_attachment(&attachment_id).await?;
        if metadata.file_name != file_name || metadata.content_type != "text/plain" {
            return Err(ApiError::Other(format!(
                "unexpected fetched attachment metadata: {metadata:?}"
            )));
        }

        let mut downloaded = Vec::new();
        let byte_count = client
            .download_attachment(&attachment_id, &mut downloaded)
            .await?;
        if byte_count != contents.len() as u64 || downloaded != contents {
            return Err(ApiError::Other(
                "downloaded attachment did not match uploaded bytes".into(),
            ));
        }
        Ok(())
    }
    .await;

    let attachment_cleanup = client.delete_attachment(&attachment_id).await;
    let incident_cleanup = client.delete_record("incident", &incident_id).await;
    if let Err(error) = verification {
        attachment_cleanup.expect("cleanup attachment after failed verification");
        incident_cleanup.expect("cleanup incident after failed verification");
        panic!("PDI attachment verification failed: {error}");
    }
    attachment_cleanup.expect("delete test attachment");
    incident_cleanup.expect("delete attachment test incident");

    let deleted = client.get_attachment(&attachment_id).await;
    assert!(matches!(deleted, Err(ApiError::NotFound(_))));
}

#[test]
fn fixture_body_has_stable_keys() {
    let body = object([
        ("short_description", Value::String("marker".into())),
        ("impact", Value::String("3".into())),
    ]);
    assert_eq!(body.len(), 2);
    assert!(body.contains_key("short_description"));
    assert!(body.contains_key("impact"));
}
