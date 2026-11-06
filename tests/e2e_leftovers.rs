//! Finds records the e2e suite left pointing at records it deleted.
//!
//! The suite's own cleanup deletes what it knows how to find; this scan
//! checks that claim independently, against every reference the instance's
//! data dictionary declares to a table the suite deletes from. Run it after
//! the suite with `make test-e2e`, which sets `SERVICENOW_E2E_SINCE`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use servicenow_cli::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use servicenow_cli::config::AuthType;

/// Tables the suite deletes records from.
const DELETED_FROM: &[&str] = &[
    "incident",
    "task",
    "sc_request",
    "sc_req_item",
    "sc_task",
    "sysapproval_approver",
    "sc_item_option",
    "sc_item_option_mtom",
    "sc_cart",
    "sc_cart_item",
    "task_sla",
    "sys_attachment",
    "sys_email",
    "sys_journal_field",
];

/// Fields that name a record of any table, paired with the field naming that
/// table. The data dictionary types these as strings or document ids, so the
/// reference scan cannot find them.
const POLYMORPHIC: &[(&str, &str, &str)] = &[
    ("sys_email", "instance", "target_table"),
    ("sys_journal_field", "element_id", "name"),
    ("sys_attachment", "table_sys_id", "table_name"),
];

/// Tables whose rows the instance keeps for good: deleting one requires the
/// `maint` role, which no customer account holds. Catalog orders record their
/// channel in `catalog_channel_analytics`.
const RETAINED: &[&str] = &["catalog_channel_analytics"];

/// Table name prefix of Flow Designer's variable storage. These rows belong
/// to flow contexts, which the suite leaves for the instance to purge (see
/// `Fixture` in `tests/e2e.rs`), and the PDI user cannot read them.
const FLOW_VARIABLES: &str = "var__m_";

fn required_env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is required; run this scan through `make test-e2e`"))
}

fn client() -> ServiceNowClient {
    ServiceNowClient::new(
        &required_env("SERVICENOW_E2E_INSTANCE"),
        Some(&required_env("SERVICENOW_E2E_USERNAME")),
        &required_env("SERVICENOW_E2E_PASSWORD"),
        AuthType::Basic,
    )
    .expect("valid PDI configuration")
}

fn text(record: &Value, field: &str) -> Option<String> {
    let cell = record.get(field)?;
    cell.get("value")
        .or(Some(cell))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

async fn list(
    client: &ServiceNowClient,
    table: &str,
    query: String,
    fields: &[&str],
    limit: usize,
) -> Result<Vec<Value>, ApiError> {
    client
        .list_records(
            table,
            &ListOptions {
                query: Some(query),
                fields: Some(fields.iter().map(|field| field.to_string()).collect()),
                limit,
                display_value: DisplayValue::False,
                ..ListOptions::default()
            },
        )
        .await
}

/// The ids among `ids` that no longer exist in `table`.
async fn missing(
    client: &ServiceNowClient,
    table: &str,
    ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, ApiError> {
    let ids: Vec<&String> = ids.iter().collect();
    let mut found = BTreeSet::new();
    for chunk in ids.chunks(100) {
        let query = format!(
            "sys_idIN{}",
            chunk
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        for record in list(client, table, query, &["sys_id"], 200).await? {
            found.extend(text(&record, "sys_id"));
        }
    }
    Ok(ids
        .into_iter()
        .filter(|id| !found.contains(*id))
        .cloned()
        .collect())
}

/// Every `(table, field, referenced table)` reference to a table the suite
/// deletes from, except flow variables and [`RETAINED`] tables. Rotation shards (`sys_flow_log0001`)
/// are read through their base table.
async fn references(client: &ServiceNowClient) -> Vec<(String, String, String)> {
    let records = list(
        client,
        "sys_dictionary",
        format!(
            "internal_type=reference^referenceIN{}",
            DELETED_FROM.join(",")
        ),
        &["name", "element", "reference"],
        5000,
    )
    .await
    .expect("read reference fields from the data dictionary");
    let mut references: Vec<_> = records
        .iter()
        .filter_map(|record| {
            Some((
                text(record, "name")?,
                text(record, "element")?,
                text(record, "reference")?,
            ))
        })
        .filter(|(table, _, _)| {
            let digits = table.chars().rev().take_while(char::is_ascii_digit).count();
            (digits != 4 || table.len() == 4)
                && !table.starts_with(FLOW_VARIABLES)
                && !RETAINED.contains(&table.as_str())
        })
        .collect();
    references.sort();
    references.dedup();
    assert!(
        references.len() > DELETED_FROM.len(),
        "the data dictionary returned only {} reference fields; the scan would check nothing",
        references.len()
    );
    references
}

#[tokio::test]
#[ignore = "requires a ServiceNow Personal Developer Instance"]
async fn pdi_suite_leaves_no_records_pointing_at_deleted_ones() {
    let client = client();
    let since: u64 = required_env("SERVICENOW_E2E_SINCE")
        .parse()
        .expect("SERVICENOW_E2E_SINCE is a Unix timestamp");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_secs();
    // Resolved by the instance in its own time zone. Whole minutes round
    // down, so two extra minutes cover that and clock skew between this
    // machine and the instance.
    let window = format!(
        "sys_created_on>=javascript:gs.minutesAgoStart({})",
        now.saturating_sub(since) / 60 + 2
    );

    let mut orphans = Vec::new();
    let mut unreadable = Vec::new();
    let mut scanned = 0;
    for (table, field, target) in references(&client).await {
        let query = format!("{window}^{field}ISNOTEMPTY");
        let rows = match list(&client, &table, query, &["sys_id", &field], 1000).await {
            Ok(rows) => rows,
            Err(error) => {
                unreadable.push(format!("{table}.{field}: {error}"));
                continue;
            }
        };
        scanned += 1;
        let referenced: BTreeSet<String> =
            rows.iter().filter_map(|row| text(row, &field)).collect();
        if referenced.is_empty() {
            continue;
        }
        let gone = missing(&client, &target, &referenced)
            .await
            .unwrap_or_else(|error| {
                panic!("check {target} ids referenced by {table}.{field}: {error}")
            });
        for row in &rows {
            if text(row, &field).is_some_and(|id| gone.contains(&id)) {
                orphans.push(format!(
                    "{table} {} -> deleted {target} via {field}",
                    text(row, "sys_id").unwrap_or_default()
                ));
            }
        }
    }

    for (table, id_field, table_field) in POLYMORPHIC {
        let query = format!(
            "{window}^{table_field}IN{}^{id_field}ISNOTEMPTY",
            DELETED_FROM.join(",")
        );
        let rows = list(
            &client,
            table,
            query,
            &["sys_id", id_field, table_field],
            5000,
        )
        .await
        .unwrap_or_else(|error| panic!("read {table}: {error}"));
        scanned += 1;
        let mut by_table: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in &rows {
            if let (Some(target), Some(id)) = (text(row, table_field), text(row, id_field)) {
                by_table.entry(target).or_default().insert(id);
            }
        }
        for (target, ids) in &by_table {
            let gone = missing(&client, target, ids).await.unwrap_or_else(|error| {
                panic!("check {target} ids referenced by {table}: {error}")
            });
            for row in &rows {
                if text(row, table_field).as_deref() == Some(target.as_str())
                    && text(row, id_field).is_some_and(|id| gone.contains(&id))
                {
                    orphans.push(format!(
                        "{table} {} -> deleted {target} via {id_field}",
                        text(row, "sys_id").unwrap_or_default()
                    ));
                }
            }
        }
    }

    eprintln!("scanned {scanned} reference fields for records created since {since}");
    if !unreadable.is_empty() {
        eprintln!(
            "not checked, the PDI user cannot read them:\n{}",
            unreadable.join("\n")
        );
    }
    assert!(
        orphans.is_empty(),
        "{} records point at records the suite deleted:\n{}",
        orphans.len(),
        orphans.join("\n")
    );
}
