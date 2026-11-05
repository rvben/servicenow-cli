use std::collections::BTreeSet;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use servicenow_cli::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use servicenow_cli::config::AuthType;
use tempfile::TempDir;

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

async fn configured_choice(
    client: &ServiceNowClient,
    field: &str,
    preferred_label: Option<&str>,
) -> Result<String, ApiError> {
    let choices = client
        .list_records(
            "sys_choice",
            &ListOptions {
                query: Some(format!(
                    "name=incident^element={field}^inactive=false^ORDERBYsequence"
                )),
                fields: Some(vec!["value".into(), "label".into()]),
                all: true,
                ..ListOptions::default()
            },
        )
        .await?;
    let choice = match preferred_label {
        Some(label) => choices
            .iter()
            .find(|choice| {
                string_field(choice, "label").is_ok_and(|value| value.eq_ignore_ascii_case(label))
            })
            .ok_or_else(|| {
                ApiError::NotFound(format!(
                    "configured incident.{field} choice labeled '{label}'"
                ))
            })?,
        None => choices
            .first()
            .ok_or_else(|| ApiError::NotFound(format!("configured incident.{field} choice")))?,
    };
    Ok(string_field(choice, "value")?.to_string())
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
    let flows_before = unreclaimable_flows(&client)
        .await
        .expect("read running flows");

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

    let cleanup = delete_fixture(&client, &mut Fixture::incident(&sys_id), &flows_before).await;
    conclude(verification, cleanup);

    let deleted = client
        .get_record("incident", &sys_id, None, DisplayValue::False)
        .await;
    assert!(matches!(deleted, Err(ApiError::NotFound(_))));
}

/// Exercises the complete CLI resolution workflow against the PDI's real
/// choices and data policies, then removes the isolated incident fixture.
#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_incident_resolution_lifecycle() {
    let pdi = Pdi::from_env();
    let client = pdi.client();
    let current_user = client
        .list_records(
            "sys_user",
            &ListOptions {
                query: Some("sys_id=javascript:gs.getUserID()".into()),
                fields: Some(vec!["sys_id".into()]),
                limit: 1,
                ..ListOptions::default()
            },
        )
        .await
        .expect("resolve current PDI user")
        .into_iter()
        .next()
        .expect("PDI returned the current user");
    let user_id = string_field(&current_user, "sys_id")
        .expect("current user has sys_id")
        .to_string();
    let resolved_state = configured_choice(&client, "state", Some("Resolved"))
        .await
        .expect("find the PDI Resolved state");
    let resolution_code = configured_choice(&client, "close_code", None)
        .await
        .expect("find a PDI resolution code");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let marker = format!("servicenow-cli-resolution-e2e-{unique}");
    let resolution_notes = format!("Resolved by the isolated CLI lifecycle test {marker}");
    let flows_before = unreclaimable_flows(&client)
        .await
        .expect("read running flows");

    let created = client
        .create_record(
            "incident",
            &object([
                ("short_description", Value::String(marker.clone())),
                (
                    "description",
                    Value::String("Temporary resolution lifecycle fixture".into()),
                ),
                ("caller_id", Value::String(user_id.clone())),
                ("assigned_to", Value::String(user_id)),
                ("impact", Value::String("3".into())),
                ("urgency", Value::String("3".into())),
            ]),
        )
        .await
        .expect("create resolution test incident");
    let sys_id = string_field(&created, "sys_id")
        .expect("created incident has sys_id")
        .to_string();

    let verification: Result<(), ApiError> = async {
        let isolated_home = TempDir::new()
            .map_err(|error| ApiError::Other(format!("create isolated CLI home: {error}")))?;
        let output = Command::new(env!("CARGO_BIN_EXE_servicenow"))
            .env("SERVICENOW_INSTANCE", &pdi.instance)
            .env("SERVICENOW_USERNAME", &pdi.username)
            .env("SERVICENOW_PASSWORD", &pdi.password)
            .env("SERVICENOW_AUTH_TYPE", "basic")
            .env("SERVICENOW_CONFIG_DIR", isolated_home.path().join("config"))
            .env("SERVICENOW_CACHE_DIR", isolated_home.path().join("cache"))
            .env_remove("SERVICENOW_TOKEN")
            .env_remove("SERVICENOW_COOKIE")
            .env_remove("SERVICENOW_USER_TOKEN")
            .args([
                "--output",
                "json",
                "incidents",
                "resolve",
                &sys_id,
                "--code",
                &resolution_code,
                "--state",
                &resolved_state,
                "--notes",
                &resolution_notes,
            ])
            .output()
            .map_err(|error| ApiError::Other(format!("run resolve command: {error}")))?;
        if !output.status.success() {
            return Err(ApiError::Other(format!(
                "resolve command failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let resolved = client
            .get_record(
                "incident",
                &sys_id,
                Some(&[
                    "sys_id".into(),
                    "state".into(),
                    "close_code".into(),
                    "close_notes".into(),
                ]),
                DisplayValue::False,
            )
            .await?;
        for (field, expected) in [
            ("state", resolved_state.as_str()),
            ("close_code", resolution_code.as_str()),
            ("close_notes", resolution_notes.as_str()),
        ] {
            if string_field(&resolved, field)? != expected {
                return Err(ApiError::Other(format!(
                    "resolved incident did not preserve {field}"
                )));
            }
        }
        Ok(())
    }
    .await;

    let cleanup = delete_fixture(&client, &mut Fixture::incident(&sys_id), &flows_before).await;
    conclude(verification, cleanup);

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
    let flows_before = unreclaimable_flows(&client)
        .await
        .expect("read running flows");

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
            let cleanup =
                delete_fixture(&client, &mut Fixture::incident(&incident_id), &flows_before).await;
            conclude(Err(format!("upload test attachment: {error}")), cleanup);
            unreachable!("conclude panics on a failed verification");
        }
    };
    let attachment_id = uploaded.sys_id.clone();

    let verification: Result<(), ApiError> = async {
        if uploaded.file_name.as_deref() != Some(file_name.as_str())
            || uploaded.table_name.as_deref() != Some("incident")
            || uploaded.table_sys_id.as_deref() != Some(incident_id.as_str())
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
        if metadata.file_name.as_deref() != Some(file_name.as_str())
            || metadata.content_type.as_deref() != Some("text/plain")
        {
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

    // Deleted through the Attachment API first, which also removes the stored
    // content; the incident cleanup then deletes any row this left behind.
    let attachment_cleanup = client.delete_attachment(&attachment_id).await;
    let incident_cleanup =
        delete_fixture(&client, &mut Fixture::incident(&incident_id), &flows_before).await;
    let cleanup = match (attachment_cleanup, incident_cleanup) {
        (Ok(()), incident) => incident,
        (Err(attachment), Ok(())) => Err(attachment),
        (Err(attachment), Err(incident)) => {
            Err(ApiError::Other(format!("{attachment}; {incident}")))
        }
    };
    conclude(verification, cleanup);

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

/// Records the suite deletes, by table, in deletion order (children first).
type Inventory = Vec<(&'static str, Vec<String>)>;

/// Flow states the instance's own table cleanup reclaims, with every record
/// that hangs off the flow.
const FINISHED_FLOW_STATES: &str = "COMPLETE,CANCELLED,ERROR,PRESUMED_INTERRUPTED";

/// Tables whose records start the flows and workflows a test fixture triggers:
/// SLA flows on incidents, request and approval flows on orders, and their
/// subflows.
const FLOW_SOURCE_TABLES: &str =
    "incident,task_sla,sc_request,sc_req_item,sc_task,sysapproval_approver,sys_flow_context";

/// A record a test creates, together with everything the instance creates
/// for it, so cleanup removes the whole fixture.
///
/// Flow and workflow contexts are not deleted. The instance owns their
/// lifecycle: it purges finished contexts together with their logs, values
/// and bindings, while a Table API deletion removes only the context row and
/// orphans the rest, some of it (flow logs) beyond what any ACL lets the API
/// delete. Cleanup instead requires every flow the fixture started to have
/// finished, which [`unreclaimable_flows`] checks.
///
/// Catalog orders also leave a `catalog_channel_analytics` row. Deleting one
/// requires the `maint` role, which no PDI account has, so those rows stay.
enum Fixture {
    Incident {
        sys_id: String,
        /// SLAs attached to the incident, kept across cleanup passes since
        /// they can no longer be found from it once it is deleted.
        slas: BTreeSet<String>,
    },
    CatalogOrder {
        request_id: String,
        known: OrderParents,
    },
}

impl Fixture {
    fn incident(sys_id: &str) -> Self {
        Self::Incident {
            sys_id: sys_id.to_string(),
            slas: BTreeSet::new(),
        }
    }

    fn catalog_order(request_id: &str) -> Self {
        Self::CatalogOrder {
            request_id: request_id.to_string(),
            known: OrderParents::default(),
        }
    }

    /// The record the fixture hangs off, deleted even when no inventory
    /// could be read.
    fn root(&self) -> Inventory {
        match self {
            Self::Incident { sys_id, .. } => vec![("incident", vec![sys_id.clone()])],
            Self::CatalogOrder { request_id, .. } => {
                vec![("sc_request", vec![request_id.clone()])]
            }
        }
    }

    async fn inventory(&mut self, client: &ServiceNowClient) -> Result<Inventory, ApiError> {
        match self {
            Self::Incident { sys_id, slas } => incident_inventory(client, sys_id, slas).await,
            Self::CatalogOrder { request_id, known } => {
                catalog_order_inventory(client, request_id, known).await
            }
        }
    }
}

/// Ids of an order's records that others hang off, kept across cleanup
/// passes: once a requested item, task or approval is deleted it can no
/// longer be found from the request, but late notifications can still name it.
#[derive(Default)]
struct OrderParents {
    ritms: BTreeSet<String>,
    tasks: BTreeSet<String>,
    approvals: BTreeSet<String>,
    /// Variable values, which are found only through their links; once a
    /// link is deleted, a value that survived is found by its id.
    options: BTreeSet<String>,
}

fn id_list<'a>(ids: impl IntoIterator<Item = &'a String>) -> String {
    ids.into_iter().cloned().collect::<Vec<_>>().join(",")
}

/// The non-empty values of `field` across the records `query` matches.
async fn record_ids(
    client: &ServiceNowClient,
    table: &str,
    query: String,
    field: &str,
) -> Result<Vec<String>, ApiError> {
    let records = client
        .list_records(
            table,
            &ListOptions {
                query: Some(query),
                fields: Some(vec![field.to_string()]),
                limit: 500,
                display_value: DisplayValue::False,
                ..ListOptions::default()
            },
        )
        .await?;
    Ok(records
        .iter()
        .filter_map(|record| {
            let cell = record.get(field)?;
            cell.get("value")
                .or(Some(cell))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect())
}

/// Journal entries, attachments and notifications about any of `parents`.
async fn records_about(client: &ServiceNowClient, parents: &str) -> Result<Inventory, ApiError> {
    Ok(vec![
        (
            "sys_journal_field",
            record_ids(
                client,
                "sys_journal_field",
                format!("element_idIN{parents}"),
                "sys_id",
            )
            .await?,
        ),
        (
            "sys_attachment",
            record_ids(
                client,
                "sys_attachment",
                format!("table_sys_idIN{parents}"),
                "sys_id",
            )
            .await?,
        ),
        (
            "sys_email",
            record_ids(
                client,
                "sys_email",
                format!("instanceIN{parents}"),
                "sys_id",
            )
            .await?,
        ),
    ])
}

async fn incident_inventory(
    client: &ServiceNowClient,
    sys_id: &str,
    slas: &mut BTreeSet<String>,
) -> Result<Inventory, ApiError> {
    let current_slas = record_ids(client, "task_sla", format!("task={sys_id}"), "sys_id").await?;
    slas.extend(current_slas.iter().cloned());
    let parents = std::iter::once(sys_id.to_string())
        .chain(slas.iter().cloned())
        .collect::<Vec<_>>()
        .join(",");
    let mut inventory = records_about(client, &parents).await?;
    inventory.push(("task_sla", current_slas));
    inventory.push((
        "incident",
        record_ids(client, "incident", format!("sys_id={sys_id}"), "sys_id").await?,
    ));
    Ok(inventory)
}

async fn catalog_order_inventory(
    client: &ServiceNowClient,
    request_id: &str,
    known: &mut OrderParents,
) -> Result<Inventory, ApiError> {
    let request = record_ids(
        client,
        "sc_request",
        format!("sys_id={request_id}"),
        "sys_id",
    )
    .await?;
    let ritms = record_ids(
        client,
        "sc_req_item",
        format!("request={request_id}"),
        "sys_id",
    )
    .await?;
    known.ritms.extend(ritms.iter().cloned());
    let ritm_list = id_list(&known.ritms);
    let mtom = if known.ritms.is_empty() {
        Vec::new()
    } else {
        let query = format!("request_itemIN{ritm_list}");
        known.options.extend(
            record_ids(
                client,
                "sc_item_option_mtom",
                query.clone(),
                "sc_item_option",
            )
            .await?,
        );
        record_ids(client, "sc_item_option_mtom", query, "sys_id").await?
    };
    let options = if known.options.is_empty() {
        Vec::new()
    } else {
        record_ids(
            client,
            "sc_item_option",
            format!("sys_idIN{}", id_list(&known.options)),
            "sys_id",
        )
        .await?
    };
    let tasks = if known.ritms.is_empty() {
        Vec::new()
    } else {
        record_ids(
            client,
            "sc_task",
            format!("request_itemIN{ritm_list}"),
            "sys_id",
        )
        .await?
    };
    known.tasks.extend(tasks.iter().cloned());
    // Every record an approval, journal entry, attachment or email can hang
    // off: the request, its requested items, and their tasks.
    let parents = std::iter::once(request_id.to_string())
        .chain(known.ritms.iter().cloned())
        .chain(known.tasks.iter().cloned())
        .collect::<Vec<_>>()
        .join(",");
    let approvals = record_ids(
        client,
        "sysapproval_approver",
        format!("sysapprovalIN{parents}"),
        "sys_id",
    )
    .await?;
    known.approvals.extend(approvals.iter().cloned());
    // Notifications about an approval are keyed by the approval record.
    let notified = std::iter::once(parents)
        .chain(known.approvals.iter().cloned())
        .collect::<Vec<_>>()
        .join(",");
    let mut inventory = vec![("sysapproval_approver", approvals)];
    inventory.extend(records_about(client, &notified).await?);
    inventory.extend([
        ("sc_task", tasks),
        ("sc_item_option_mtom", mtom),
        ("sc_item_option", options),
        ("sc_req_item", ritms),
        ("sc_request", request),
    ]);
    Ok(inventory)
}

/// Flow and workflow contexts on fixture tables that the instance will never
/// purge: still running, or marked to skip the scheduled cleanup.
async fn unreclaimable_flows(client: &ServiceNowClient) -> Result<BTreeSet<String>, ApiError> {
    let flows = record_ids(
        client,
        "sys_flow_context",
        format!(
            "source_tableIN{FLOW_SOURCE_TABLES}^stateNOT IN{FINISHED_FLOW_STATES}^NQsource_tableIN{FLOW_SOURCE_TABLES}^skip_schedule_cleanup=true"
        ),
        "sys_id",
    )
    .await?;
    let workflows = record_ids(
        client,
        "wf_context",
        format!("tableIN{FLOW_SOURCE_TABLES}^state=executing"),
        "sys_id",
    )
    .await?;
    Ok(flows
        .into_iter()
        .map(|id| format!("sys_flow_context {id}"))
        .chain(workflows.into_iter().map(|id| format!("wf_context {id}")))
        .collect())
}

/// Deletes a fixture and requires that every flow it started has finished.
///
/// `flows_before` is [`unreclaimable_flows`] read before the fixture was
/// created, so flows that were already running on the instance are not
/// blamed on the test.
async fn delete_fixture(
    client: &ServiceNowClient,
    fixture: &mut Fixture,
    flows_before: &BTreeSet<String>,
) -> Result<(), ApiError> {
    delete_records(client, fixture).await?;
    // Deleting a record cancels its flows asynchronously.
    let mut left = BTreeSet::new();
    for _ in 0..6 {
        match unreclaimable_flows(client).await {
            Ok(current) => {
                left = current.difference(flows_before).cloned().collect();
                if left.is_empty() {
                    return Ok(());
                }
            }
            Err(error) => left = BTreeSet::from([format!("(flow read failed: {error})")]),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    Err(ApiError::Other(format!(
        "the fixture left flows the instance will not purge (still running or exempt from cleanup):\n{}",
        left.into_iter().collect::<Vec<_>>().join("\n")
    )))
}

async fn delete_records(client: &ServiceNowClient, fixture: &mut Fixture) -> Result<(), ApiError> {
    // The instance writes notifications from its event queue, so emails about
    // the fixture can appear after a deletion pass. Each pass is followed by
    // a pause before the leftover check, and records that arrived meanwhile
    // are deleted by the next pass.
    // A failed read never ends the cleanup: the first pass falls back to the
    // fixture's root record, and a later one keeps the previous inventory,
    // since giving up leaves the whole fixture behind.
    let mut read_error = None;
    let mut inventory = match read_inventory(client, fixture).await {
        Ok(inventory) => inventory,
        Err(error) => {
            read_error = Some(error.to_string());
            fixture.root()
        }
    };
    let mut failures = Vec::new();
    for _ in 0..4 {
        // A refused deletion stops neither the pass nor later passes, which
        // also catch late records and retry the refused ones; failures are
        // reported only for records still present at the end.
        failures.clear();
        for (table, ids) in &inventory {
            for id in ids {
                match client.delete_record(table, id).await {
                    Ok(_) | Err(ApiError::NotFound(_)) => {}
                    Err(error) => failures.push(format!("{table} {id}: {error}")),
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        match read_inventory(client, fixture).await {
            Ok(current) => {
                read_error = None;
                inventory = current;
                if inventory.iter().all(|(_, ids)| ids.is_empty()) {
                    return Ok(());
                }
            }
            Err(error) => read_error = Some(error.to_string()),
        }
    }
    let leftover: Vec<String> = inventory
        .into_iter()
        .filter(|(_, ids)| !ids.is_empty())
        .map(|(table, ids)| format!("{table}: {}", ids.join(" ")))
        .collect();
    let refused = if failures.is_empty() {
        String::new()
    } else {
        format!("\ndeletions refused:\n{}", failures.join("\n"))
    };
    let unread = read_error
        .map(|error| {
            format!("\nthe last inventory read failed, so this list may be incomplete: {error}")
        })
        .unwrap_or_default();
    Err(ApiError::Other(format!(
        "cleanup left records behind:\n{}{refused}{unread}",
        leftover.join("\n")
    )))
}

/// Reads the fixture's records, retrying transient failures.
async fn read_inventory(
    client: &ServiceNowClient,
    fixture: &mut Fixture,
) -> Result<Inventory, ApiError> {
    let mut attempt = 1;
    loop {
        match fixture.inventory(client).await {
            Ok(inventory) => return Ok(inventory),
            Err(error) if attempt == 3 => return Err(error),
            Err(_) => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
}

/// Fails the test with the verification error, the cleanup error, or both,
/// so a cleanup failure never hides why the verification failed.
fn conclude(verification: Result<(), impl std::fmt::Display>, cleanup: Result<(), ApiError>) {
    match (verification, cleanup) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) => panic!("verification failed: {error}"),
        (Ok(()), Err(cleanup_error)) => panic!("cleanup failed: {cleanup_error}"),
        (Err(error), Err(cleanup_error)) => {
            panic!("verification failed: {error}; cleanup also failed: {cleanup_error}")
        }
    }
}

/// The request an order created: the one it reported, else the one found by
/// its marker. Transient lookup failures are retried, since giving up leaves
/// the order behind; a final failure names the marker so it can be found.
async fn ordered_request(
    client: &ServiceNowClient,
    ordered: &Value,
    marker: &str,
    order: &std::process::Output,
) -> String {
    if let Some(id) = ordered["request"]["request_id"].as_str() {
        return id.to_string();
    }
    let mut last = String::from("no request carries the marker");
    for _ in 0..3 {
        match request_for_marker(client, marker).await {
            Ok(Some(id)) => return id,
            Ok(None) => last = "no request carries the marker".into(),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    panic!(
        "could not find the order carrying {marker} ({last}); if it was placed, delete it by hand. Order stderr: {}",
        String::from_utf8_lossy(&order.stderr)
    );
}

/// Finds the request an order created from the unique value it carried, for
/// when the order command did not report one.
async fn request_for_marker(
    client: &ServiceNowClient,
    marker: &str,
) -> Result<Option<String>, ApiError> {
    let first = |table: &'static str, query: String, field: &'static str| async move {
        let records = client
            .list_records(
                table,
                &ListOptions {
                    query: Some(query),
                    fields: Some(vec![field.to_string()]),
                    limit: 1,
                    display_value: DisplayValue::False,
                    ..ListOptions::default()
                },
            )
            .await?;
        Ok::<_, ApiError>(records.first().and_then(|record| {
            let cell = record.get(field)?;
            cell.get("value")
                .or(Some(cell))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }))
    };
    let Some(option) = first("sc_item_option", format!("value={marker}"), "sys_id").await? else {
        return Ok(None);
    };
    let Some(ritm) = first(
        "sc_item_option_mtom",
        format!("sc_item_option={option}"),
        "request_item",
    )
    .await?
    else {
        return Ok(None);
    };
    first("sc_req_item", format!("sys_id={ritm}"), "request").await
}

fn run_catalog_cli(pdi: &Pdi, home: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_servicenow"))
        .env("SERVICENOW_INSTANCE", &pdi.instance)
        .env("SERVICENOW_USERNAME", &pdi.username)
        .env("SERVICENOW_PASSWORD", &pdi.password)
        .env("SERVICENOW_AUTH_TYPE", "basic")
        .env("SERVICENOW_CONFIG_DIR", home.path().join("config"))
        .env("SERVICENOW_CACHE_DIR", home.path().join("cache"))
        .env_remove("SERVICENOW_PROFILE")
        .env_remove("SERVICENOW_READ_ONLY")
        .args(args)
        .args(["--output", "json"])
        .output()
        .expect("run servicenow")
}

/// Returns failures instead of panicking: a panic once an order exists would
/// skip its cleanup.
fn catalog_cli_json(pdi: &Pdi, home: &TempDir, args: &[&str]) -> Result<(Value, String), String> {
    let output = run_catalog_cli(pdi, home, args);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format!("{args:?} failed: {stderr}"));
    }
    let stdout = serde_json::from_slice::<Value>(&output.stdout)
        .map_err(|error| format!("{args:?} printed invalid JSON: {error}"))?;
    Ok((stdout, stderr))
}

#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_catalog_reorder_round_trips_date_times() {
    let pdi = Pdi::from_env();
    let client = pdi.client();
    let home = TempDir::new().unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let topic = format!("servicenow-cli-e2e-{unique}");
    let topic_var = format!("topic={topic}");
    let start = "2027-03-01 10:00:00";
    let end = "2027-03-01 11:30:00";
    let start_var = format!("select_webinar_start_time={start}");
    let flows_before = unreclaimable_flows(&client)
        .await
        .expect("read running flows");
    let end_var = format!("select_webinar_end_time={end}");
    let order = run_catalog_cli(
        &pdi,
        &home,
        &[
            "catalog",
            "order",
            "Request zoom webinar",
            "--var",
            &topic_var,
            "--var",
            &start_var,
            "--var",
            &end_var,
            "--var",
            "add_co_hosts=abel.tuter",
            "--yes",
        ],
    );
    let ordered = serde_json::from_slice::<Value>(&order.stdout).unwrap_or(Value::Null);
    let request_id = ordered_request(&client, &ordered, &topic, &order).await;

    let verification: Result<(), String> = async {
        let number = ordered["requestedItems"][0]["number"]
            .as_str()
            .ok_or_else(|| {
                format!(
                    "order reported no requested item: {}",
                    String::from_utf8_lossy(&order.stderr)
                )
            })?;
        let (variables, _) =
            catalog_cli_json(&pdi, &home, &["catalog", "requests", "vars", number])?;
        for (name, expected) in [
            ("select_webinar_start_time", start),
            ("select_webinar_end_time", end),
        ] {
            if variables[name] != expected {
                return Err(format!(
                    "{name} read back as {}, expected {expected}: {variables}",
                    variables[name]
                ));
            }
        }
        // The export is promised to be reusable as order input.
        let file = home.path().join("reorder.json");
        std::fs::write(&file, variables.to_string()).map_err(|error| error.to_string())?;
        let file = file.to_string_lossy().into_owned();
        let (plan, _) = catalog_cli_json(
            &pdi,
            &home,
            &[
                "catalog",
                "order",
                "Request zoom webinar",
                "--vars-file",
                &file,
                "--dry-run",
            ],
        )?;
        for (name, expected) in [
            ("select_webinar_start_time", start),
            ("select_webinar_end_time", end),
        ] {
            if plan["variables"][name] != expected {
                return Err(format!(
                    "reorder plan has {name} = {}: {plan}",
                    plan["variables"][name]
                ));
            }
        }
        Ok(())
    }
    .await;

    let cleanup = delete_fixture(
        &client,
        &mut Fixture::catalog_order(&request_id),
        &flows_before,
    )
    .await;
    conclude(verification, cleanup);
}

#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_catalog_order_lifecycle() {
    let pdi = Pdi::from_env();
    let client = pdi.client();
    let isolated_home = TempDir::new().unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let purpose = format!("servicenow-cli-e2e-{unique}");
    let run = |args: &[&str]| run_catalog_cli(&pdi, &isolated_home, args);
    let cli = |args: &[&str]| catalog_cli_json(&pdi, &isolated_home, args);
    let purpose_var = format!("describe_briefly_the_purpose_of_this_account={purpose}");
    let order_args = [
        "catalog",
        "order",
        "AWS account request",
        "--var",
        "division=IT",
        "--var",
        "environment=Dev",
        "--var",
        &purpose_var,
    ];

    let (plan, _) = cli(&[&order_args[..], &["--dry-run"]].concat()).unwrap();
    assert_eq!(plan["dryRun"], true);
    assert_eq!(plan["variables"]["division"], "IT");

    let flows_before = unreclaimable_flows(&client)
        .await
        .expect("read running flows");
    let order = run(&[&order_args[..], &["--yes"]].concat());
    let ordered = serde_json::from_slice::<Value>(&order.stdout).unwrap_or(Value::Null);
    let reported = ordered["request"]["request_id"]
        .as_str()
        .map(str::to_string);
    let request_id = ordered_request(&client, &ordered, &purpose, &order).await;

    let verification: Result<(), String> = async {
        if !order.status.success() || reported.is_none() {
            return Err(format!(
                "order failed or reported no request_id: {}",
                String::from_utf8_lossy(&order.stderr)
            ));
        }
        let ritm = ordered["requestedItems"]
            .as_array()
            .and_then(|items| items.first())
            .ok_or("order reported no requested items")?;
        let number = ritm["number"]
            .as_str()
            .ok_or("requested item has no number")?;

        // Proves the fallback cleanup path can find a real order.
        let found = request_for_marker(&client, &purpose)
            .await
            .map_err(|error| format!("marker lookup failed: {error}"))?;
        if found.as_deref() != Some(request_id.as_str()) {
            return Err(format!(
                "marker lookup found {found:?}, expected {request_id}"
            ));
        }

        let (variables, _) = cli(&["catalog", "requests", "vars", number])?;
        let expected = serde_json::json!({
            "division": "IT",
            "environment": "Dev",
            "describe_briefly_the_purpose_of_this_account": purpose,
        });
        if variables != expected {
            return Err(format!(
                "variables read back as {variables}, expected {expected}"
            ));
        }

        let (mine, _) = cli(&["catalog", "requests", "mine", "--open", "-n", "50"])?;
        let listed = mine["result"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["number"] == number));
        if !listed {
            return Err(format!(
                "{number} missing from `catalog requests mine`: {mine}"
            ));
        }
        Ok(())
    }
    .await;

    let cleanup = delete_fixture(
        &client,
        &mut Fixture::catalog_order(&request_id),
        &flows_before,
    )
    .await;
    conclude(verification, cleanup);
}
