use servicenow_cli::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use servicenow_cli::config::AuthType;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> ServiceNowClient {
    ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic).unwrap()
}

#[tokio::test]
async fn list_records_sends_auth_and_table_api_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(header("authorization", "Basic YXBpLXVzZXI6c2VjcmV0"))
        .and(query_param(
            "sysparm_query",
            "active=true^ORDERBYDESCsys_updated_on",
        ))
        .and(query_param(
            "sysparm_fields",
            "sys_id,number,short_description",
        ))
        .and(query_param("sysparm_limit", "25"))
        .and(query_param("sysparm_offset", "10"))
        .and(query_param("sysparm_display_value", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "number": "INC0010001",
                "short_description": "VPN unavailable"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let records = client(&server)
        .list_records(
            "incident",
            &ListOptions {
                query: Some("active=true^ORDERBYDESCsys_updated_on".into()),
                fields: Some(vec![
                    "sys_id".into(),
                    "number".into(),
                    "short_description".into(),
                ]),
                limit: 25,
                offset: 10,
                all: false,
                display_value: DisplayValue::False,
            },
        )
        .await
        .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["number"], "INC0010001");
}

#[tokio::test]
async fn bearer_auth_and_display_values_are_supported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/now/table/incident/0123456789abcdef0123456789abcdef",
        ))
        .and(header("authorization", "Bearer access-token"))
        .and(query_param("sysparm_display_value", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": "0123456789abcdef0123456789abcdef",
                "assigned_to": {"value": "abc", "display_value": "Ada Lovelace"}
            }
        })))
        .mount(&server)
        .await;

    let client =
        ServiceNowClient::new(&server.uri(), None, "access-token", AuthType::Bearer).unwrap();
    let record = client
        .get_record(
            "incident",
            "0123456789abcdef0123456789abcdef",
            None,
            DisplayValue::All,
        )
        .await
        .unwrap();
    assert_eq!(record["assigned_to"]["display_value"], "Ada Lovelace");
}

#[tokio::test]
async fn create_update_and_delete_use_expected_methods() {
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";

    Mock::given(method("POST"))
        .and(path("/api/now/table/incident"))
        .and(body_partial_json(serde_json::json!({
            "short_description": "Email is down"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "result": {"sys_id": sys_id, "number": "INC0010002"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .and(body_partial_json(serde_json::json!({"state": "2"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"sys_id": sys_id, "state": "2"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let create = serde_json::from_value(serde_json::json!({
        "short_description": "Email is down"
    }))
    .unwrap();
    let created = client.create_record("incident", &create).await.unwrap();
    assert_eq!(created["number"], "INC0010002");

    let update = serde_json::from_value(serde_json::json!({"state": "2"})).unwrap();
    let updated = client
        .update_record("incident", sys_id, &update)
        .await
        .unwrap();
    assert_eq!(updated["state"], "2");
    client.delete_record("incident", sys_id).await.unwrap();
}

#[tokio::test]
async fn api_error_body_is_mapped_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/now/table/incident/0123456789abcdef0123456789abcdef",
        ))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": {"message": "User Not Authenticated", "detail": "Required ACL missing"}
        })))
        .mount(&server)
        .await;

    let error = client(&server)
        .get_record(
            "incident",
            "0123456789abcdef0123456789abcdef",
            None,
            DisplayValue::False,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::Auth(message) if message.contains("Required ACL missing")));
}

#[tokio::test]
async fn rejects_path_injection_before_sending_request() {
    let server = MockServer::start().await;
    let error = client(&server)
        .list_records("incident/../../sys_user", &ListOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::InvalidInput(_)));
}
