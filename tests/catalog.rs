use std::collections::BTreeMap;

use serde_json::{Value, json};
use servicenow_cli::api::{ApiError, ServiceNowClient};
use servicenow_cli::catalog::{self, CatalogItem, InputValue};
use servicenow_cli::config::AuthType;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AWS_ITEM: &str = "5a1efa5947520510f53d37d2846d439b";
const ZOOM_ITEM: &str = "54c9ead947dec110f53d37d2846d43ea";
const USER: &str = "62826bf03710200044e0bfc8bcbe5df1";

fn client(server: &MockServer) -> ServiceNowClient {
    ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic).unwrap()
}

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/catalog/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn item(name: &str) -> CatalogItem {
    CatalogItem::from_api(&fixture(name)["result"]).unwrap()
}

fn inputs(pairs: &[(&str, &str)]) -> BTreeMap<String, InputValue> {
    pairs
        .iter()
        .map(|(name, value)| {
            (
                (*name).to_string(),
                InputValue {
                    value: (*value).to_string(),
                    from_command_line: false,
                    list: false,
                },
            )
        })
        .collect()
}

#[tokio::test]
async fn search_sends_text_and_paging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_sc/servicecatalog/items"))
        .and(query_param("sysparm_text", "laptop"))
        .and(query_param("sysparm_limit", "20"))
        .and(query_param("sysparm_offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("items")))
        .expect(1)
        .mount(&server)
        .await;

    let items = client(&server)
        .search_catalog_items(Some("laptop"), 20, 0)
        .await
        .unwrap();
    assert_eq!(items.len(), 10);
}

#[tokio::test]
async fn invalid_item_is_reported_as_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": "Either Catalog Item is not valid or security constraints prevent access", "detail": ""},
            "status": "failure"
        })))
        .mount(&server)
        .await;

    let error = client(&server)
        .get_catalog_item(AWS_ITEM)
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::NotFound(_)), "{error:?}");
    assert!(error.to_string().contains("security constraints"));
}

#[tokio::test]
async fn order_posts_the_body_once_and_returns_the_request() {
    let server = MockServer::start().await;
    let body = json!({
        "sysparm_quantity": "1",
        "variables": {"division": "IT"}
    });
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .and(body_json(&body))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "sys_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "number": "REQ0010002",
                "request_number": "REQ0010002",
                "request_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "table": "sc_request"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let result = client(&server)
        .order_catalog_item(AWS_ITEM, &body, None)
        .await
        .unwrap();
    assert_eq!(result["request_number"], "REQ0010002");
}

#[tokio::test]
async fn server_errors_are_not_retried_and_warn_about_a_possible_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .order_catalog_item(
            AWS_ITEM,
            &json!({"sysparm_quantity": "1", "variables": {}}),
            None,
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("503"), "{message}");
    assert!(message.contains("may have been placed"), "{message}");
}

#[tokio::test]
async fn an_unreadable_order_response_warns_about_a_possible_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy</html>"))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .order_catalog_item(
            AWS_ITEM,
            &json!({"sysparm_quantity": "1", "variables": {}}),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("may have been placed"),
        "{error}"
    );
}

#[tokio::test]
async fn mandatory_rejection_is_surfaced_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": "Mandatory Variables are required", "detail": ""},
            "status": "failure"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .order_catalog_item(
            AWS_ITEM,
            &json!({"sysparm_quantity": "1", "variables": {}}),
            None,
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("Mandatory Variables are required"),
        "{message}"
    );
    assert!(!message.contains("may have been placed"), "{message}");
}

#[tokio::test]
async fn references_are_resolved_and_checked_against_their_table() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "user_name=abel.tuter^ORemail=abel.tuter^ORname=abel.tuter",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"sys_id": {"value": USER, "display_value": USER}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param("sysparm_query", format!("sys_id={USER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"sys_id": USER}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .expect(1)
        .mount(&server)
        .await;

    let zoom = item("zoom_webinar");
    let ok = catalog::prepare_order(
        &client(&server),
        &zoom,
        &inputs(&[
            ("topic", "Launch"),
            ("select_webinar_start_time", ""),
            ("add_co_hosts", "abel.tuter"),
        ]),
        &catalog::OrderOptions::default(),
    )
    .await;
    let error = ok.unwrap_err().to_string();
    assert!(!error.contains("add_co_hosts"), "{error}");

    let missing = catalog::prepare_order(
        &client(&server),
        &zoom,
        &inputs(&[("add_co_hosts", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        missing.contains("add_co_hosts: no sys_user record \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" is visible to you"),
        "{missing}"
    );
}

#[tokio::test]
async fn a_sys_id_from_the_wrong_table_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/cmn_department"))
        .and(query_param("sysparm_query", format!("sys_id={USER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .expect(1)
        .mount(&server)
        .await;

    let mut bulk = item("mobile_devices_bulk");
    bulk.variables
        .retain(|variable| variable.name != "mobile_devices_set");
    let error = catalog::prepare_order(
        &client(&server),
        &bulk,
        &inputs(&[
            ("department", USER),
            ("business_justification", "Field team"),
        ]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("department: no cmn_department record"),
        "{error}"
    );
}

#[tokio::test]
async fn a_qualifier_with_an_embedded_script_is_not_sent_as_a_query() {
    let server = MockServer::start().await;
    let department = "cccccccccccccccccccccccccccccccc";
    Mock::given(method("GET"))
        .and(path("/api/now/table/cmn_department"))
        .and(query_param("sysparm_query", format!("sys_id={department}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": department}]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut bulk = item("mobile_devices_bulk");
    bulk.variables
        .retain(|variable| variable.name != "mobile_devices_set");
    bulk.variables[0].reference_qualifier =
        Some("active=true^parent=JavaScript:current.variables.division".into());
    let plan = catalog::prepare_order(
        &client(&server),
        &bulk,
        &inputs(&[("department", department), ("business_justification", "x")]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(plan.request_body()["variables"]["department"], department);
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("script the CLI cannot evaluate")),
        "{:?}",
        plan.warnings
    );
}

#[tokio::test]
async fn a_plain_qualifier_is_applied_and_a_name_needs_a_sys_id() {
    let server = MockServer::start().await;
    let department = "cccccccccccccccccccccccccccccccc";
    Mock::given(method("GET"))
        .and(path("/api/now/table/cmn_department"))
        .and(query_param(
            "sysparm_query",
            format!("sys_id={department}^active=true"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .expect(1)
        .mount(&server)
        .await;

    let mut bulk = item("mobile_devices_bulk");
    bulk.variables
        .retain(|variable| variable.name != "mobile_devices_set");
    bulk.variables[0].reference_qualifier = Some("active=true".into());
    let qualified = catalog::prepare_order(
        &client(&server),
        &bulk,
        &inputs(&[("department", department), ("business_justification", "x")]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        qualified.contains("allowed by the variable's reference qualifier"),
        "{qualified}"
    );

    let named = catalog::prepare_order(
        &client(&server),
        &bulk,
        &inputs(&[("department", "Finance"), ("business_justification", "x")]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        named.contains("department: \"Finance\" is not a sys_id"),
        "{named}"
    );
}

#[tokio::test]
async fn date_variables_require_the_iso_date_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"date_format": "dd-MM-yyyy"}]
        })))
        .mount(&server)
        .await;

    let zoom = item("zoom_webinar");
    let error = catalog::prepare_order(
        &client(&server),
        &zoom,
        &inputs(&[
            ("topic", "Launch"),
            ("select_webinar_start_time", "2026-12-01 10:00:00"),
            ("select_webinar_end_time", "2026-12-01 11:00:00"),
            ("add_co_hosts", ""),
        ]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("your ServiceNow date format is dd-MM-yyyy"),
        "{error}"
    );
}

#[tokio::test]
async fn a_valid_order_plan_falls_back_to_the_instance_date_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"date_format": "", "time_format": ""}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_properties"))
        .and(query_param("sysparm_query", "name=glide.sys.time_format"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"value": "HH:mm:ss"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_properties"))
        .and(query_param("sysparm_query", "name=glide.sys.date_format"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"value": "yyyy-MM-dd"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param("sysparm_query", format!("sys_id={USER}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": USER}]})),
        )
        .mount(&server)
        .await;

    let zoom = item("zoom_webinar");
    let plan = catalog::prepare_order(
        &client(&server),
        &zoom,
        &inputs(&[
            ("topic", "Launch"),
            ("select_webinar_start_time", "2026-12-01 10:00:00"),
            ("select_webinar_end_time", "2026-12-01 11:00:00"),
            ("add_co_hosts", USER),
            ("set_password", "hunter2"),
        ]),
        &catalog::OrderOptions {
            quantity: 2,
            requested_for: Some(USER.into()),
            ..catalog::OrderOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(plan.item_sys_id, ZOOM_ITEM);
    assert_eq!(plan.request_body()["sysparm_quantity"], "2");
    assert_eq!(plan.request_body()["sysparm_requested_for"], USER);
    assert_eq!(plan.request_body()["variables"]["set_password"], "hunter2");
    assert_eq!(plan.plan_json()["variables"]["set_password"], "[REDACTED]");
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("time zone")),
        "{:?}",
        plan.warnings
    );
    assert!(
        plan.warnings
            .iter()
            .all(|warning| !warning.contains("format")),
        "{:?}",
        plan.warnings
    );
}

#[tokio::test]
async fn unreadable_date_formats_refuse_a_real_order_unless_allowed() {
    let server = MockServer::start().await;
    // A user record without the format fields: they are not readable.
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [{}]})))
        .mount(&server)
        .await;

    let zoom = item("zoom_webinar");
    let values = inputs(&[
        ("topic", "Launch"),
        ("select_webinar_start_time", "2026-12-01 10:00:00"),
        ("select_webinar_end_time", "2026-12-01 11:00:00"),
        ("add_co_hosts", ""),
    ]);
    let mut zoom_optional = zoom.clone();
    for variable in &mut zoom_optional.variables {
        variable.mandatory = false;
    }
    let with = |policy| catalog::OrderOptions {
        unverified_formats: policy,
        ..catalog::OrderOptions::default()
    };

    let refused = catalog::prepare_order(
        &client(&server),
        &zoom_optional,
        &values,
        &with(catalog::UnverifiedFormats::Refuse),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        refused.contains("could not read your ServiceNow date format")
            && refused.contains("could not read your ServiceNow time format")
            && refused.contains("--allow-unverified-formats"),
        "{refused}"
    );

    for (policy, note) in [
        (
            catalog::UnverifiedFormats::Allow,
            "(--allow-unverified-formats)",
        ),
        (
            catalog::UnverifiedFormats::Preview,
            "a real order needs --allow-unverified-formats",
        ),
    ] {
        let plan = catalog::prepare_order(&client(&server), &zoom_optional, &values, &with(policy))
            .await
            .unwrap();
        let format_warnings: Vec<&String> = plan
            .warnings
            .iter()
            .filter(|warning| warning.contains("could not read"))
            .collect();
        assert_eq!(format_warnings.len(), 2, "{:?}", plan.warnings);
        assert!(
            format_warnings.iter().all(|warning| warning.contains(note)),
            "{:?}",
            plan.warnings
        );
    }
}

#[tokio::test]
async fn date_times_require_a_24_hour_time_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"date_format": "yyyy-MM-dd", "time_format": "hh:mm:ss a"}]
        })))
        .mount(&server)
        .await;

    let zoom = item("zoom_webinar");
    let error = catalog::prepare_order(
        &client(&server),
        &zoom,
        &inputs(&[
            ("topic", "Launch"),
            ("select_webinar_start_time", "2026-12-01 10:00:00"),
            ("select_webinar_end_time", "2026-12-01 11:00:00"),
            ("add_co_hosts", ""),
        ]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("your ServiceNow time format is hh:mm:ss a"),
        "{error}"
    );
    assert!(!error.contains("date format"), "{error}");
}

#[tokio::test]
async fn an_empty_date_time_does_not_need_a_time_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"date_format": "yyyy-MM-dd", "time_format": "hh:mm:ss a"}]
        })))
        .mount(&server)
        .await;

    let mut zoom = item("zoom_webinar");
    for variable in &mut zoom.variables {
        variable.mandatory = false;
        if variable.name == "select_webinar_start_time" {
            variable.kind = catalog::VariableKind::Date;
            variable.type_name = "date".into();
        }
    }
    let plan = catalog::prepare_order(
        &client(&server),
        &zoom,
        &inputs(&[
            ("topic", "Launch"),
            ("select_webinar_start_time", "2026-12-01"),
            ("select_webinar_end_time", ""),
        ]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(plan.variables["select_webinar_start_time"], "2026-12-01");
    assert!(
        plan.warnings
            .iter()
            .all(|warning| !warning.contains("time zone")),
        "{:?}",
        plan.warnings
    );
}

#[tokio::test]
async fn a_new_query_qualifier_still_restricts_the_chosen_record() {
    let server = MockServer::start().await;
    let department = "cccccccccccccccccccccccccccccccc";
    Mock::given(method("GET"))
        .and(path("/api/now/table/cmn_department"))
        .and(query_param(
            "sysparm_query",
            format!("sys_id={department}^active=true^NQsys_id={department}^nameSTARTSWITHA"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .expect(1)
        .mount(&server)
        .await;

    let mut bulk = item("mobile_devices_bulk");
    bulk.variables
        .retain(|variable| variable.name != "mobile_devices_set");
    bulk.variables[0].reference_qualifier = Some("active=true^NQnameSTARTSWITHA".into());
    let error = catalog::prepare_order(
        &client(&server),
        &bulk,
        &inputs(&[("department", department), ("business_justification", "x")]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("allowed by the variable's reference qualifier"),
        "{error}"
    );
}

#[tokio::test]
async fn a_qualified_lookup_must_return_the_chosen_record() {
    let server = MockServer::start().await;
    let department = "cccccccccccccccccccccccccccccccc";
    let other = "dddddddddddddddddddddddddddddddd";
    // An OR term that escaped the sys_id restriction would match other records.
    Mock::given(method("GET"))
        .and(path("/api/now/table/cmn_department"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": other}]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut bulk = item("mobile_devices_bulk");
    bulk.variables
        .retain(|variable| variable.name != "mobile_devices_set");
    bulk.variables[0].reference_qualifier = Some("active=true^ORnameSTARTSWITHA".into());
    let error = catalog::prepare_order(
        &client(&server),
        &bulk,
        &inputs(&[("department", department), ("business_justification", "x")]),
        &catalog::OrderOptions::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("allowed by the variable's reference qualifier"),
        "{error}"
    );
}
