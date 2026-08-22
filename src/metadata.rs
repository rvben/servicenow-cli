use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use crate::config::{cache_dir, validate_profile_name};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FieldMetadata {
    pub name: String,
    pub label: String,
    pub internal_type: String,
    pub reference: Option<String>,
    pub mandatory: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChoiceMetadata {
    pub value: String,
    pub label: String,
    pub sequence: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TableMetadata {
    pub table: String,
    pub fetched_at: u64,
    pub fields: Vec<FieldMetadata>,
    pub choices: BTreeMap<String, Vec<ChoiceMetadata>>,
}

impl TableMetadata {
    pub fn choice_label(&self, field: &str, value: &str) -> Option<&str> {
        self.choices
            .get(field)?
            .iter()
            .find(|choice| choice.value == value)
            .map(|choice| choice.label.as_str())
    }

    pub fn field(&self, name: &str) -> Option<&FieldMetadata> {
        self.fields.iter().find(|field| field.name == name)
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ReferenceKind {
    User,
    Group,
}

pub async fn sync_table(
    client: &ServiceNowClient,
    profile: &str,
    table: &str,
) -> Result<TableMetadata, ApiError> {
    crate::api::validate_table(table)?;
    validate_profile_name(profile)?;
    let dictionary = client
        .list_records(
            "sys_dictionary",
            &ListOptions {
                query: Some(format!("name={table}^elementISNOTEMPTY^active=true")),
                fields: Some(vec![
                    "element".into(),
                    "column_label".into(),
                    "internal_type".into(),
                    "reference".into(),
                    "mandatory".into(),
                    "read_only".into(),
                ]),
                all: true,
                ..ListOptions::default()
            },
        )
        .await?;
    let choices = client
        .list_records(
            "sys_choice",
            &ListOptions {
                query: Some(format!("name={table}^elementISNOTEMPTY^inactive=false")),
                fields: Some(vec![
                    "element".into(),
                    "value".into(),
                    "label".into(),
                    "sequence".into(),
                ]),
                all: true,
                ..ListOptions::default()
            },
        )
        .await?;

    let mut fields = dictionary
        .iter()
        .filter_map(parse_field)
        .collect::<Vec<_>>();
    fields.sort_by(|left, right| left.name.cmp(&right.name));
    let mut grouped_choices: BTreeMap<String, Vec<ChoiceMetadata>> = BTreeMap::new();
    for record in &choices {
        if let Some((field, choice)) = parse_choice(record) {
            grouped_choices.entry(field).or_default().push(choice);
        }
    }
    for values in grouped_choices.values_mut() {
        values.sort_by_key(|choice| choice.sequence);
    }
    let metadata = TableMetadata {
        table: table.into(),
        fetched_at: epoch_seconds(),
        fields,
        choices: grouped_choices,
    };
    save(profile, &metadata)?;
    Ok(metadata)
}

pub fn load(profile: &str, table: &str) -> Result<Option<TableMetadata>, ApiError> {
    crate::api::validate_table(table)?;
    validate_profile_name(profile)?;
    let path = cache_path(profile, table);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).map(Some).map_err(|error| {
            ApiError::Other(format!("failed to parse {}: {error}", path.display()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ApiError::Other(format!(
            "failed to read {}: {error}",
            path.display()
        ))),
    }
}

pub async fn resolve_reference(
    client: &ServiceNowClient,
    kind: ReferenceKind,
    value: &str,
) -> Result<Value, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.contains(['^', '\r', '\n']) {
        return Err(ApiError::InvalidInput(
            "reference value cannot be empty or contain encoded-query separators".into(),
        ));
    }
    if crate::api::validate_sys_id(value).is_ok() {
        let table = match kind {
            ReferenceKind::User => "sys_user",
            ReferenceKind::Group => "sys_user_group",
        };
        return client
            .get_record(table, value, None, DisplayValue::All)
            .await;
    }
    let (table, query, fields) = match kind {
        ReferenceKind::User if value == "@me" => (
            "sys_user",
            "sys_id=javascript:gs.getUserID()".into(),
            vec!["sys_id", "user_name", "name", "email"],
        ),
        ReferenceKind::User => (
            "sys_user",
            format!("user_name={value}^ORemail={value}^ORname={value}"),
            vec!["sys_id", "user_name", "name", "email"],
        ),
        ReferenceKind::Group => (
            "sys_user_group",
            format!("name={value}"),
            vec!["sys_id", "name", "description", "active"],
        ),
    };
    let records = client
        .list_records(
            table,
            &ListOptions {
                query: Some(query),
                fields: Some(fields.into_iter().map(str::to_string).collect()),
                limit: 6,
                display_value: DisplayValue::All,
                ..ListOptions::default()
            },
        )
        .await?;
    match records.as_slice() {
        [] => Err(ApiError::NotFound(format!(
            "{} reference '{value}'",
            match kind {
                ReferenceKind::User => "user",
                ReferenceKind::Group => "group",
            }
        ))),
        [record] => Ok(record.clone()),
        _ => Err(ApiError::Conflict(format!(
            "{} records matched '{value}'; use an exact user name, email, group name, or sys_id",
            records.len()
        ))),
    }
}

pub fn metadata_as_records(metadata: &TableMetadata) -> Vec<Value> {
    metadata
        .fields
        .iter()
        .map(|field| {
            json!({
                "field": field.name,
                "label": field.label,
                "type": field.internal_type,
                "reference": field.reference,
                "mandatory": field.mandatory,
                "read_only": field.read_only,
                "choices": metadata.choices.get(&field.name).map_or(0, Vec::len),
            })
        })
        .collect()
}

fn save(profile: &str, metadata: &TableMetadata) -> Result<(), ApiError> {
    let path = cache_path(profile, &metadata.table);
    let parent = path
        .parent()
        .ok_or_else(|| ApiError::Other("metadata cache path has no parent".into()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        ApiError::Other(format!("failed to create {}: {error}", parent.display()))
    })?;
    let content = serde_json::to_vec_pretty(metadata)
        .map_err(|error| ApiError::Other(format!("failed to encode metadata: {error}")))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| ApiError::Other(format!("failed to create metadata cache: {error}")))?;
    temp.write_all(&content)
        .map_err(|error| ApiError::Other(format!("failed to write metadata cache: {error}")))?;
    temp.persist(&path)
        .map_err(|error| ApiError::Other(format!("failed to replace metadata cache: {error}")))?;
    Ok(())
}

fn cache_path(profile: &str, table: &str) -> PathBuf {
    cache_dir()
        .join("metadata")
        .join(profile)
        .join(format!("{table}.json"))
}

fn parse_field(record: &Value) -> Option<FieldMetadata> {
    let name = text(record, "element")?;
    Some(FieldMetadata {
        label: text(record, "column_label").unwrap_or_else(|| name.clone()),
        internal_type: text(record, "internal_type").unwrap_or_else(|| "string".into()),
        reference: text(record, "reference").filter(|value| !value.is_empty()),
        mandatory: boolean(record, "mandatory"),
        read_only: boolean(record, "read_only"),
        name,
    })
}

fn parse_choice(record: &Value) -> Option<(String, ChoiceMetadata)> {
    let field = text(record, "element")?;
    Some((
        field,
        ChoiceMetadata {
            value: text(record, "value")?,
            label: text(record, "label")?,
            sequence: text(record, "sequence")
                .and_then(|value| value.parse().ok())
                .unwrap_or_default(),
        },
    ))
}

fn text(record: &Value, field: &str) -> Option<String> {
    let value = record.get(field)?;
    value
        .as_str()
        .or_else(|| value.get("value").and_then(Value::as_str))
        .or_else(|| value.get("display_value").and_then(Value::as_str))
        .map(str::to_string)
}

fn boolean(record: &Value, field: &str) -> bool {
    text(record, field).is_some_and(|value| matches!(value.as_str(), "true" | "1"))
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dictionary_fields_and_choice_labels() {
        let field = parse_field(&json!({
            "element": "state",
            "column_label": "State",
            "internal_type": {"value": "integer", "display_value": "Integer"},
            "mandatory": "true",
            "read_only": "false"
        }))
        .unwrap();
        assert_eq!(field.name, "state");
        assert_eq!(field.internal_type, "integer");
        assert!(field.mandatory);

        let mut metadata = TableMetadata {
            table: "incident".into(),
            fetched_at: 0,
            fields: vec![field],
            choices: BTreeMap::new(),
        };
        metadata.choices.insert(
            "state".into(),
            vec![ChoiceMetadata {
                value: "2".into(),
                label: "In Progress".into(),
                sequence: 20,
            }],
        );
        assert_eq!(metadata.choice_label("state", "2"), Some("In Progress"));
    }
}
