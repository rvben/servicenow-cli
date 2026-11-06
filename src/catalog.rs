//! Service Catalog items, their variables, and client-side order validation.
//!
//! `order_now` enforces static mandatory flags and nothing else: invalid choice
//! values, references to the wrong table, unknown variable names and malformed
//! date-times are accepted and stored or silently dropped. The portal UI is what
//! normally prevents that, so this module is the validation layer for orders
//! placed from the command line.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient, validate_sys_id};
use crate::metadata::{self, ReferenceKind};
use crate::sensitive::{REDACTED, is_sensitive_field};

/// Catalog classes that `GET /items` returns but `order_now` cannot order.
const NON_ORDERABLE_CLASSES: &[(&str, &str)] = &[
    ("sc_cat_item_producer", "record producer"),
    ("sc_cat_item_composite_producer", "record producer"),
    ("std_change_record_producer", "standard change template"),
    ("sc_cat_item_guide", "order guide"),
    ("sc_cat_item_content", "content item"),
    ("sc_cat_item_wizard", "catalog wizard"),
];

/// Variable types that only shape the form and never carry a value.
const LAYOUT_TYPES: &[&str] = &[
    "container_end",
    "container_split",
    "label",
    "rich_text_label",
    "break",
];

/// The `type` codes of the same layout elements (label, break, container end,
/// container split, rich text label), which stay reliable when a
/// friendly_type is misspelt or new.
const LAYOUT_TYPE_CODES: &[i64] = &[11, 12, 20, 24, 32];

/// Custom widgets: rendered by the portal with their own code, so they are
/// not variables the CLI can fill, but they may collect input.
const CUSTOM_WIDGET_TYPES: &[&str] = &[
    "macro",
    "macro_with_label",
    // The spelling some instances return for "macro_with_label".
    "macro_with_abel",
    "ui_page",
    "custom",
    "custom_with_label",
];

/// Custom (14), UI page (15) and custom with label (17).
const CUSTOM_WIDGET_TYPE_CODES: &[i64] = &[14, 15, 17];

const DATE_FORMAT: &str = "yyyy-MM-dd";
const TIME_FORMAT: &str = "HH:mm:ss";

/// Shown when the instance flags an item `orderable: false`.
pub const INSTANCE_UNORDERABLE: &str = "the instance reports this item as not orderable through the API; ordering may only leave it in a cart";

/// What an `order_now` answer says happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderOutcome {
    /// A request was created. `number` is `None` when the answer omitted it.
    Request {
        sys_id: String,
        number: Option<String>,
    },
    /// No request was created; the item was left in this cart.
    CartOnly { cart_id: String },
    /// Neither a request id nor a cart: the field names the answer held,
    /// sorted, and the request number when it named one. Other values are
    /// never kept, since an answer can echo submitted variables.
    Unrecognized {
        keys: Vec<String>,
        number: Option<String>,
    },
}

/// A request number as instances generate them (`REQ0010001`): letters and
/// digits only, so a value that is not one is never echoed.
fn request_number(result: &Value, field: &str) -> Option<String> {
    result
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|number| {
            !number.is_empty()
                && number.len() <= 40
                && number.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(str::to_string)
}

impl OrderOutcome {
    /// Classifies the `result` of `POST /items/{sys_id}/order_now`.
    ///
    /// A bare `sys_id` names the request only when the answer says it is one
    /// (`table: sc_request`), or names neither a table nor a cart, so a cart
    /// or cart item never passes for a request.
    pub fn classify(result: &Value) -> Self {
        let id = |field: &str| {
            result
                .get(field)
                .and_then(Value::as_str)
                .filter(|id| validate_sys_id(id).is_ok())
                .map(str::to_string)
        };
        let names_request = match result.get("table").and_then(Value::as_str) {
            Some(table) => table == "sc_request",
            None => result.get("cart_id").is_none(),
        };
        let request = id("request_id").or_else(|| id("sys_id").filter(|_| names_request));
        if let Some(sys_id) = request {
            let number = request_number(result, "request_number")
                .or_else(|| request_number(result, "number"));
            return Self::Request { sys_id, number };
        }
        if let Some(cart_id) = id("cart_id") {
            return Self::CartOnly { cart_id };
        }
        let mut keys: Vec<String> = result
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort();
        Self::Unrecognized {
            keys,
            number: request_number(result, "request_number"),
        }
    }
}

/// The caller's cart, as `GET /cart` or `add_to_cart` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cart {
    pub cart_id: String,
    pub items: Vec<CartItem>,
}

/// One item in a [`Cart`].
#[derive(Debug, Clone, PartialEq)]
pub struct CartItem {
    pub cart_item_id: String,
    pub name: String,
    /// The catalog item this is a copy of, when the instance names one.
    pub catalog_item_id: Option<String>,
    /// `false` when the instance says the item cannot be ordered as it is.
    pub can_order: Option<bool>,
    /// The item as the instance described it, plus `frequency`: the
    /// recurring price frequency the instance grouped it under.
    pub details: Value,
}

impl Cart {
    /// Reads a cart answer. `GET /cart` groups items by recurring price
    /// frequency, one object per frequency (`none` for one-off prices) with an
    /// `items` array; `add_to_cart` lists them under a top-level `items`.
    pub fn from_api(result: &Value) -> Result<Self, ApiError> {
        let missing_id =
            || ApiError::Other("the instance described the cart without a cart id".into());
        let object = result.as_object().ok_or_else(missing_id)?;
        let cart_id = object
            .get("cart_id")
            .and_then(Value::as_str)
            .filter(|id| validate_sys_id(id).is_ok())
            .ok_or_else(missing_id)?
            .to_string();
        let mut groups: Vec<(&str, &Vec<Value>)> = Vec::new();
        for (frequency, group) in object {
            if let Some(items) = group.get("items").and_then(Value::as_array) {
                groups.push((frequency, items));
            }
        }
        if let Some(items) = object.get("items").and_then(Value::as_array) {
            groups.push(("none", items));
        }
        // An item listed both in a frequency group and at the top level is
        // one item: counting it twice would misreport the cart.
        let mut seen = BTreeSet::new();
        let mut items = Vec::new();
        for (frequency, group) in groups {
            for item in group {
                let item = CartItem::from_api(item, frequency)?;
                if seen.insert(item.cart_item_id.clone()) {
                    items.push(item);
                }
            }
        }
        Ok(Self { cart_id, items })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "cart_id": self.cart_id,
            "items": self.items.iter().map(|item| item.details.clone()).collect::<Vec<_>>(),
        })
    }

    pub fn item(&self, cart_item_id: &str) -> Option<&CartItem> {
        self.items
            .iter()
            .find(|item| item.cart_item_id.eq_ignore_ascii_case(cart_item_id))
    }

    /// What identifies the cart's contents: its id and each item with its
    /// quantity, in a stable order.
    pub fn fingerprint(&self) -> (String, Vec<(String, Option<String>)>) {
        let mut items: Vec<(String, Option<String>)> = self
            .items
            .iter()
            .map(|item| {
                let quantity = item.details.get("quantity").map(|value| match value {
                    Value::String(text) => text.trim().to_string(),
                    other => other.to_string(),
                });
                (item.cart_item_id.clone(), quantity)
            })
            .collect();
        items.sort();
        (self.cart_id.clone(), items)
    }

    /// The items' names, for messages about the whole cart.
    pub fn item_names(&self) -> String {
        self.items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl CartItem {
    fn from_api(item: &Value, frequency: &str) -> Result<Self, ApiError> {
        let cart_item_id = item
            .get("cart_item_id")
            .and_then(Value::as_str)
            .filter(|id| validate_sys_id(id).is_ok())
            .ok_or_else(|| {
                ApiError::Other("the instance listed a cart item without a cart item id".into())
            })?
            .to_ascii_lowercase();
        let name = ["item_name", "name"]
            .iter()
            .find_map(|field| item.get(*field).and_then(Value::as_str))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("(unnamed item)")
            .to_string();
        let mut details = item.clone();
        if let Some(object) = details.as_object_mut() {
            object.insert("frequency".into(), Value::String(frequency.to_string()));
        }
        let catalog_item_id = item
            .get("catalog_item_id")
            .and_then(Value::as_str)
            .filter(|id| validate_sys_id(id).is_ok())
            .map(str::to_ascii_lowercase);
        Ok(Self {
            cart_item_id,
            name,
            catalog_item_id,
            can_order: item.get("can_order").and_then(Value::as_bool),
            details,
        })
    }
}

/// Short human label for an item class, as shown in the `KIND` column.
pub fn kind_label(class: &str) -> &str {
    match class {
        "sc_cat_item" => "item",
        "pc_hardware_cat_item" => "hardware",
        "pc_software_cat_item" => "software",
        other => NON_ORDERABLE_CLASSES
            .iter()
            .find(|(candidate, _)| *candidate == other)
            .map_or(other, |(_, label)| label),
    }
}

/// Whether `catalog order` accepts items of this class.
pub fn is_orderable_class(class: &str) -> bool {
    !NON_ORDERABLE_CLASSES
        .iter()
        .any(|(candidate, _)| *candidate == class)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableKind {
    Text,
    MultiLineText,
    Masked,
    CheckBox,
    YesNo,
    Choice,
    Reference,
    ListCollector,
    Date,
    DateTime,
    Attachment,
    MultiRowSet,
    Unknown,
}

impl VariableKind {
    fn from_friendly_type(friendly_type: &str) -> Self {
        match friendly_type {
            "single_line_text" | "wide_single_line_text" | "email" | "url" | "ip_address" => {
                Self::Text
            }
            "multi_line_text" | "html" => Self::MultiLineText,
            "masked" => Self::Masked,
            "check_box" => Self::CheckBox,
            "yes_no" => Self::YesNo,
            "select_box" | "multiple_choice" | "lookup_select_box" | "lookup_multiple_choice" => {
                Self::Choice
            }
            "reference" | "requested_for" => Self::Reference,
            "list_collector" => Self::ListCollector,
            "date" => Self::Date,
            "date_time" => Self::DateTime,
            "attachment" => Self::Attachment,
            "sc_multi_row" => Self::MultiRowSet,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogVariable {
    pub name: String,
    pub label: String,
    /// The instance's `friendly_type`, kept verbatim for display.
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(skip)]
    pub kind: VariableKind,
    pub mandatory: bool,
    pub read_only: bool,
    pub default_value: Option<String>,
    pub choices: Vec<Choice>,
    pub reference_table: Option<String>,
    pub reference_qualifier: Option<String>,
    /// Label of the enclosing container, for display.
    pub container: Option<String>,
    pub order: i64,
}

impl CatalogVariable {
    /// Masked variables, and free-text variables whose name or label looks
    /// like a secret. Constrained kinds (a checkbox such as
    /// `requires_password`, a choice, a reference) cannot hold a secret, so
    /// their values stay visible.
    pub fn is_sensitive(&self) -> bool {
        match self.kind {
            VariableKind::Masked => true,
            VariableKind::Text | VariableKind::MultiLineText | VariableKind::Unknown => {
                is_sensitive_field(&self.name) || is_sensitive_field(&self.label)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogItem {
    pub sys_id: String,
    pub name: String,
    pub short_description: String,
    pub class: String,
    /// `None` when the instance did not say.
    pub show_quantity: Option<bool>,
    pub mandatory_attachment: Option<bool>,
    /// The item's `orderable` flag as the instance reports it; `None` when
    /// the instance did not say. Not every release sets it reliably, so a
    /// `false` warns instead of refusing.
    pub instance_orderable: Option<bool>,
    /// Catalog UI policies or client scripts that the portal runs and the CLI does not.
    pub has_client_logic: bool,
    pub variables: Vec<CatalogVariable>,
    /// Labels of custom widgets (macros, UI pages), which the portal renders
    /// with its own code and the CLI can neither show nor fill.
    pub custom_widgets: Vec<String>,
}

impl CatalogItem {
    /// Parses the `result` of `GET /api/sn_sc/servicecatalog/items/{sys_id}`.
    pub fn from_api(value: &Value) -> Result<Self, ApiError> {
        let sys_id = string_field(value, "sys_id")
            .ok_or_else(|| ApiError::Other("catalog item response omitted sys_id".into()))?;
        let mut variables = Vec::new();
        let mut custom_widgets = Vec::new();
        if let Some(entries) = value.get("variables").and_then(Value::as_array) {
            flatten_variables(entries, None, &mut variables, &mut custom_widgets);
        }
        Ok(Self {
            sys_id,
            name: string_field(value, "name").unwrap_or_default(),
            short_description: string_field(value, "short_description").unwrap_or_default(),
            class: string_field(value, "sys_class_name").unwrap_or_default(),
            show_quantity: value.get("show_quantity").and_then(Value::as_bool),
            mandatory_attachment: value.get("mandatory_attachment").and_then(Value::as_bool),
            instance_orderable: value.get("orderable").and_then(Value::as_bool),
            has_client_logic: has_client_logic(value),
            variables,
            custom_widgets,
        })
    }

    pub fn variable(&self, name: &str) -> Option<&CatalogVariable> {
        self.variables.iter().find(|variable| variable.name == name)
    }

    /// The item as shown to the user: defaults of sensitive variables are
    /// replaced by the redaction placeholder.
    pub fn redacted(&self) -> CatalogItem {
        let mut item = self.clone();
        for variable in &mut item.variables {
            if variable.is_sensitive()
                && variable
                    .default_value
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
            {
                variable.default_value = Some(REDACTED.into());
            }
        }
        item
    }

    /// Portal page for the item, for anything the CLI cannot order.
    pub fn portal_url(&self, site_url: &str) -> String {
        format!("{site_url}/sp?id=sc_cat_item&sys_id={}", self.sys_id)
    }

    /// Rejects items that `catalog order` cannot submit correctly.
    pub fn ensure_orderable(&self, site_url: &str) -> Result<(), ApiError> {
        let portal = self.portal_url(site_url);
        if !is_orderable_class(&self.class) {
            return Err(ApiError::InvalidInput(format!(
                "\"{}\" is a {} ({}), not an orderable catalog item; use the portal: {portal}",
                self.name,
                kind_label(&self.class),
                self.class
            )));
        }
        // An optional set is left empty, as the portal submits it when no row
        // is added; a mandatory one cannot be satisfied without rows.
        if let Some(set) = self
            .variables
            .iter()
            .find(|variable| variable.kind == VariableKind::MultiRowSet && variable.mandatory)
        {
            return Err(ApiError::InvalidInput(format!(
                "\"{}\" has a mandatory multi-row variable set ({}), which the CLI cannot fill; use the portal: {portal}",
                self.name, set.name
            )));
        }
        if self.mandatory_attachment == Some(true) {
            return Err(ApiError::InvalidInput(format!(
                "\"{}\" requires an attachment, which the CLI does not support; use the portal: {portal}",
                self.name
            )));
        }
        Ok(())
    }
}

fn flatten_variables(
    entries: &[Value],
    container: Option<&str>,
    out: &mut Vec<CatalogVariable>,
    custom_widgets: &mut Vec<String>,
) {
    for entry in entries {
        let friendly_type = string_field(entry, "friendly_type").unwrap_or_default();
        if friendly_type == "container_start" {
            let label = string_field(entry, "label").filter(|label| !label.is_empty());
            if let Some(children) = entry.get("children").and_then(Value::as_array) {
                flatten_variables(
                    children,
                    label.as_deref().or(container),
                    out,
                    custom_widgets,
                );
            }
            continue;
        }
        let type_code = entry.get("type").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        });
        if CUSTOM_WIDGET_TYPES.contains(&friendly_type.as_str())
            || type_code.is_some_and(|code| CUSTOM_WIDGET_TYPE_CODES.contains(&code))
        {
            let label = string_field(entry, "label")
                .filter(|label| !label.trim().is_empty())
                .or_else(|| string_field(entry, "name").filter(|name| !name.is_empty()));
            if let Some(label) = label {
                custom_widgets.push(label);
            }
            continue;
        }
        if LAYOUT_TYPES.contains(&friendly_type.as_str())
            || type_code.is_some_and(|code| LAYOUT_TYPE_CODES.contains(&code))
        {
            continue;
        }
        let Some(name) = string_field(entry, "name").filter(|name| !name.is_empty()) else {
            continue;
        };
        let kind = VariableKind::from_friendly_type(&friendly_type);
        let type_name = if friendly_type.is_empty() {
            entry
                .get("type")
                .map(|value| format!("type {}", scalar_string(value).unwrap_or_default()))
                .unwrap_or_else(|| "unknown".into())
        } else {
            friendly_type
        };
        let reference_table = match kind {
            VariableKind::ListCollector => string_field(entry, "table"),
            VariableKind::Reference => string_field(entry, "reference"),
            _ => None,
        }
        .filter(|table| !table.is_empty())
        .or_else(|| (type_name == "requested_for").then(|| "sys_user".into()));
        out.push(CatalogVariable {
            label: string_field(entry, "label").unwrap_or_else(|| name.clone()),
            name,
            type_name,
            mandatory: entry.get("mandatory").and_then(Value::as_bool) == Some(true),
            read_only: entry.get("read_only").and_then(Value::as_bool) == Some(true),
            default_value: entry
                .get("value")
                .and_then(scalar_string)
                .filter(|value| !value.is_empty()),
            choices: parse_choices(entry),
            reference_table,
            reference_qualifier: string_field(entry, "ref_qualifier")
                .filter(|qualifier| !qualifier.trim().is_empty()),
            container: container.map(str::to_string),
            order: entry.get("order").and_then(Value::as_i64).unwrap_or(0),
            kind,
        });
    }
}

fn parse_choices(entry: &Value) -> Vec<Choice> {
    entry
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            let value = choice.get("value").and_then(scalar_string)?;
            if value.is_empty() {
                return None;
            }
            let label = string_field(choice, "label").unwrap_or_else(|| value.clone());
            Some(Choice { value, label })
        })
        .collect()
}

fn has_client_logic(item: &Value) -> bool {
    let policies = item
        .get("ui_policy")
        .and_then(Value::as_array)
        .is_some_and(|policies| !policies.is_empty());
    let scripts = item
        .get("client_script")
        .and_then(Value::as_object)
        .is_some_and(|scripts| {
            scripts
                .values()
                .any(|list| list.as_array().is_some_and(|list| !list.is_empty()))
        });
    policies || scripts
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// One supplied variable value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputValue {
    pub value: String,
    /// Given with `--var`, so visible in shell history and process lists.
    pub from_command_line: bool,
    /// Written as a list in `--vars-file`, which only a list collector takes.
    pub list: bool,
}

/// Parses a `--var name=value` argument.
pub fn parse_var_assignment(assignment: &str) -> Result<(String, String), ApiError> {
    // The argument is never echoed: a mistyped assignment may be a secret.
    let (name, value) = assignment.split_once('=').ok_or_else(|| {
        ApiError::InvalidInput(
            "--var must have the form name=value (the value is not shown)".into(),
        )
    })?;
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::InvalidInput(
            "--var has an empty variable name; use name=value".into(),
        ));
    }
    Ok((name.into(), value.into()))
}

/// Parses a `--vars-file` document: a JSON or YAML object of variable names to
/// scalar values. A list becomes a comma-separated value, for list collectors,
/// so a list item may not itself contain a comma.
///
/// JSON is read as YAML. Every scalar is taken as the text written, so
/// `01234`, `1.10` or `yes` reach the instance unchanged instead of being
/// re-rendered as numbers or booleans. Duplicate names are rejected, including
/// names that differ only in YAML type, such as `1` and `"1"`.
pub fn parse_vars_document(document: &str) -> Result<BTreeMap<String, InputValue>, ApiError> {
    let invalid = || {
        // Parser messages quote the offending source, which may hold a
        // secret, so only a position is reported, and only for a document
        // written as JSON, where the JSON parser's position is meaningful.
        let looks_like_json = document.trim_start().starts_with(['{', '[']);
        let position = match serde_json::from_str::<serde::de::IgnoredAny>(document) {
            Err(error) if looks_like_json && (error.is_syntax() || error.is_eof()) => format!(
                " (JSON parsing stopped at line {}, column {})",
                error.line(),
                error.column()
            ),
            _ => String::new(),
        };
        ApiError::InvalidInput(format!(
            "--vars-file must be a JSON or YAML object of unique variable names to strings, numbers, booleans, or lists of those{position}"
        ))
    };
    // The first pass learns the shape and rejects nesting and duplicates. It
    // types scalars, so its values are discarded.
    let shape: BTreeMap<String, Value> = serde_saphyr::from_str(document).map_err(|_| invalid())?;
    let mut lists = BTreeSet::new();
    for (name, value) in &shape {
        match value {
            Value::Array(items) if items.iter().all(is_scalar) => {
                if items
                    .iter()
                    .any(|item| item.as_str().is_some_and(|text| text.contains(',')))
                {
                    return Err(ApiError::InvalidInput(format!(
                        "--vars-file: an item of the list for {name} contains a comma, which separates list values; give each value as its own item"
                    )));
                }
                lists.insert(name.clone());
            }
            value if is_scalar(value) => {}
            _ => return Err(invalid()),
        }
    }
    // The second pass reads every scalar with a string hint, which keeps the
    // source text.
    LIST_NAMES.with(|names| *names.borrow_mut() = lists);
    let parsed = serde_saphyr::from_str::<VarsDocument>(document);
    LIST_NAMES.with(|names| names.borrow_mut().clear());
    Ok(parsed.map_err(|_| invalid())?.0)
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

thread_local! {
    /// Names whose values are lists, set by `parse_vars_document` for the
    /// duration of its second pass; serde gives a map visitor no other way
    /// to know which shape each value has.
    static LIST_NAMES: std::cell::RefCell<BTreeSet<String>> = const {
        std::cell::RefCell::new(BTreeSet::new())
    };
}

struct VarsDocument(BTreeMap<String, InputValue>);

impl<'de> Deserialize<'de> for VarsDocument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DocumentVisitor;
        impl<'de> serde::de::Visitor<'de> for DocumentVisitor {
            type Value = VarsDocument;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object of variable names to values")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<VarsDocument, A::Error> {
                let mut values = BTreeMap::new();
                while let Some(name) = map.next_key::<String>()? {
                    let list = LIST_NAMES.with(|names| names.borrow().contains(&name));
                    let value = if list {
                        map.next_value::<Vec<String>>()?.join(",")
                    } else {
                        map.next_value::<String>()?
                    };
                    let input = InputValue {
                        value,
                        from_command_line: false,
                        list,
                    };
                    if values.insert(name, input).is_some() {
                        return Err(serde::de::Error::custom("duplicate variable name"));
                    }
                }
                Ok(VarsDocument(values))
            }
        }
        deserializer.deserialize_map(DocumentVisitor)
    }
}

/// Merges file values with `--var` values; the command line wins.
pub fn merge_inputs(
    file: BTreeMap<String, InputValue>,
    command_line: Vec<(String, String)>,
) -> BTreeMap<String, InputValue> {
    let mut merged = file;
    for (name, value) in command_line {
        merged.insert(
            name,
            InputValue {
                value,
                from_command_line: true,
                list: false,
            },
        );
    }
    merged
}

/// A reference value that must be confirmed against the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceCheck {
    pub variable: String,
    pub table: String,
    pub qualifier: Option<String>,
    pub values: Vec<String>,
}

/// Result of the checks that need no network access.
#[derive(Debug, Default)]
pub struct LocalValidation {
    pub variables: BTreeMap<String, String>,
    pub references: Vec<ReferenceCheck>,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub uses_dates: bool,
}

/// Validates supplied values against the item's variables without network
/// access. Defaults fill unsupplied variables, as the portal submits them.
pub fn validate_locally(
    item: &CatalogItem,
    inputs: &BTreeMap<String, InputValue>,
    quantity: u32,
    allow_unknown: bool,
) -> LocalValidation {
    let mut result = LocalValidation::default();
    if quantity == 0 {
        result.problems.push("--quantity must be at least 1".into());
    } else if quantity != 1 && item.show_quantity == Some(false) {
        result.problems.push(format!(
            "\"{}\" does not allow a quantity; omit --quantity",
            item.name
        ));
    }
    for name in inputs.keys() {
        if item.variable(name).is_some() {
            continue;
        }
        let message = format!("unknown variable '{name}'{}", suggestion(item, name));
        if allow_unknown {
            result
                .warnings
                .push(format!("{message}; not sent (--allow-unknown-vars)"));
        } else {
            result.problems.push(format!(
                "{message}; the server would silently drop it (see `catalog show`)"
            ));
        }
    }
    for variable in &item.variables {
        let problems_before = result.problems.len();
        match inputs.get(&variable.name) {
            Some(input) => validate_value(variable, input, ValueSource::Supplied, &mut result),
            None if variable.kind == VariableKind::MultiRowSet => {
                // order_now enforces only static mandatory flags, so a set
                // that client logic makes required is accepted without rows.
                let client_logic = if item.has_client_logic {
                    "; the item's client logic may require rows, which the server does not check"
                } else {
                    ""
                };
                result.warnings.push(format!(
                    "{}: optional multi-row variable set left empty{client_logic}",
                    variable.name
                ));
            }
            None => {
                if let Some(default) = &variable.default_value
                    && submits_value(&variable.kind)
                {
                    // A default is sent like a supplied value, so it is
                    // checked like one: it can name a record that is gone, a
                    // choice that was removed, or a date the instance would
                    // parse with the caller's format.
                    let input = InputValue {
                        value: default.clone(),
                        from_command_line: false,
                        list: false,
                    };
                    let source = if variable.read_only {
                        ValueSource::ReadOnlyDefault
                    } else {
                        ValueSource::Default
                    };
                    let before = result.problems.len();
                    validate_value(variable, &input, source, &mut result);
                    let note = match source {
                        ValueSource::ReadOnlyDefault => {
                            " (this is the item's read-only default, which the CLI cannot change; use the portal or ask the item's owner to fix it)"
                        }
                        _ => " (this is the item's default value; supply a value to override it)",
                    };
                    for problem in &mut result.problems[before..] {
                        problem.push_str(note);
                    }
                }
            }
        }
        // A rejected value already has its problem; "required" would repeat it.
        let rejected = result.problems.len() > problems_before;
        let value = result.variables.get(&variable.name);
        if variable.mandatory
            && !variable.read_only
            && !rejected
            && value.is_none_or(|value| !satisfies_mandatory(&variable.kind, value))
        {
            result
                .problems
                .push(format!("{}: required ({})", variable.name, variable.label));
        }
    }
    result
}

/// A mandatory checkbox is satisfied only when ticked, as on the portal.
fn satisfies_mandatory(kind: &VariableKind, value: &str) -> bool {
    match kind {
        VariableKind::CheckBox => value == "true",
        _ => !value.trim().is_empty(),
    }
}

/// Whether a variable carries a value in the order. An unrecognised kind is
/// assumed to, so its default and its stored value are handled like a
/// supplied one (sent unchecked, with a warning) instead of silently dropped.
fn submits_value(kind: &VariableKind) -> bool {
    !matches!(kind, VariableKind::Attachment | VariableKind::MultiRowSet)
}

/// Where a value being validated came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueSource {
    Supplied,
    Default,
    /// The default of a read-only variable, which the form submits itself.
    ReadOnlyDefault,
}

fn validate_value(
    variable: &CatalogVariable,
    input: &InputValue,
    source: ValueSource,
    result: &mut LocalValidation,
) {
    let name = &variable.name;
    let raw = input.value.as_str();
    if raw == REDACTED {
        result.problems.push(format!(
            "{name}: value is the redaction placeholder {REDACTED}; supply the real value"
        ));
        return;
    }
    if variable.read_only && source != ValueSource::ReadOnlyDefault {
        result.problems.push(format!(
            "{name}: read-only; the form does not accept a value"
        ));
        return;
    }
    if input.from_command_line && variable.is_sensitive() {
        result.problems.push(format!(
            "{name}: looks sensitive (masked, or its name or label suggests a secret), so it is not accepted with --var, which exposes values in shell history and process lists; supply it with --vars-file <file>, or --vars-file - with --yes"
        ));
        return;
    }
    if input.list && variable.kind != VariableKind::ListCollector {
        result.problems.push(format!(
            "{name}: a list was given, but this {} takes a single value",
            variable.type_name
        ));
        return;
    }
    if raw.trim().is_empty() {
        // Blank free text is kept as written; any other kind has no blank
        // form, so it is sent empty rather than as unchecked whitespace.
        let blank = match variable.kind {
            VariableKind::Text | VariableKind::MultiLineText | VariableKind::Masked => raw,
            _ => "",
        };
        result.variables.insert(name.clone(), blank.into());
        return;
    }
    let normalized = match &variable.kind {
        VariableKind::Text | VariableKind::MultiLineText | VariableKind::Masked => {
            Ok(raw.to_string())
        }
        VariableKind::CheckBox => parse_flag(raw).map(|flag| flag.to_string()),
        VariableKind::YesNo => parse_flag(raw).map(|flag| if flag { "Yes" } else { "No" }.into()),
        VariableKind::Choice if variable.choices.is_empty() => {
            result.warnings.push(format!(
                "{name}: the instance does not publish choices for this {}; sent unchecked",
                variable.type_name
            ));
            Ok(raw.to_string())
        }
        VariableKind::Choice => match_choice(&variable.choices, raw),
        VariableKind::Reference | VariableKind::ListCollector => {
            let values: Vec<String> = if variable.kind == VariableKind::ListCollector {
                raw.split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect()
            } else {
                vec![raw.trim().to_string()]
            };
            match &variable.reference_table {
                Some(table) => {
                    result.references.push(ReferenceCheck {
                        variable: name.clone(),
                        table: table.clone(),
                        qualifier: variable.reference_qualifier.clone(),
                        values: values.clone(),
                    });
                }
                None => result.warnings.push(format!(
                    "{name}: the instance does not say which table this references; sent unchecked"
                )),
            }
            Ok(values.join(","))
        }
        VariableKind::Date => {
            result.uses_dates = true;
            validate_date(raw.trim()).map(|()| raw.trim().to_string())
        }
        VariableKind::DateTime => {
            result.uses_dates = true;
            validate_date_time(raw.trim()).map(|()| raw.trim().to_string())
        }
        VariableKind::Attachment => {
            Err("attachment variables are not supported; use the portal".into())
        }
        VariableKind::MultiRowSet => Err(
            "multi-row variable sets cannot be filled by the CLI; remove it, or use the portal to fill it"
                .into(),
        ),
        VariableKind::Unknown => {
            result.warnings.push(format!(
                "{name}: variable type '{}' is not recognised; sent unchecked",
                variable.type_name
            ));
            Ok(raw.to_string())
        }
    };
    match normalized {
        Ok(value) => {
            result.variables.insert(name.clone(), value);
        }
        Err(message) => result.problems.push(format!("{name}: {message}")),
    }
}

/// Suggests the variable a mistyped name most likely meant: a case-insensitive
/// match on name or label, else the single closest name within two edits.
/// Short names need an exact match, since two edits would make most of them
/// ambiguous.
fn suggestion(item: &CatalogItem, name: &str) -> String {
    let lowered = name.to_ascii_lowercase();
    let exact = item.variables.iter().find(|variable| {
        variable.name.to_ascii_lowercase() == lowered
            || variable.label.to_ascii_lowercase() == lowered
    });
    let nearest = || {
        if lowered.chars().count() < 4 {
            return None;
        }
        let mut scored: Vec<(usize, &CatalogVariable)> = item
            .variables
            .iter()
            .map(|variable| {
                (
                    edit_distance(&lowered, &variable.name.to_ascii_lowercase()),
                    variable,
                )
            })
            .filter(|(distance, _)| *distance <= 2)
            .collect();
        scored.sort_by_key(|(distance, _)| *distance);
        match scored.as_slice() {
            [(best, variable), rest @ ..] if rest.first().is_none_or(|(next, _)| next > best) => {
                Some(*variable)
            }
            _ => None,
        }
    };
    exact
        .or_else(nearest)
        .map(|variable| format!(" (did you mean '{}'?)", variable.name))
        .unwrap_or_default()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_char) in left.chars().enumerate() {
        let mut current = vec![row + 1; right.len() + 1];
        for (column, right_char) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_char != *right_char);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        previous = current;
    }
    previous[right.len()]
}

fn parse_flag(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(format!("\"{raw}\" is not a boolean; use true or false")),
    }
}

fn match_choice(choices: &[Choice], raw: &str) -> Result<String, String> {
    if let Some(choice) = choices.iter().find(|choice| choice.value == raw) {
        return Ok(choice.value.clone());
    }
    let lowered = raw.trim().to_lowercase();
    let matches: Vec<&Choice> = choices
        .iter()
        .filter(|choice| {
            choice.value.to_lowercase() == lowered || choice.label.to_lowercase() == lowered
        })
        .collect();
    match matches.as_slice() {
        [choice] => Ok(choice.value.clone()),
        _ => Err(format!(
            "\"{raw}\" is not one of the choices: {}",
            choices
                .iter()
                .map(|choice| {
                    if choice.label == choice.value {
                        choice.value.clone()
                    } else {
                        format!("{} ({})", choice.value, choice.label)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn validate_date(raw: &str) -> Result<(), String> {
    parse_date(raw).ok_or_else(|| format!("\"{raw}\" is not a date; use yyyy-MM-dd"))
}

fn parse_date(raw: &str) -> Option<()> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: u32 = digits(&raw[0..4])?;
    let month: u32 = digits(&raw[5..7])?;
    let day: u32 = digits(&raw[8..10])?;
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days_in_month).contains(&day).then_some(())
}

fn validate_date_time(raw: &str) -> Result<(), String> {
    if raw.contains('T') || raw.ends_with('Z') || has_offset(raw) {
        return Err(format!(
            "\"{raw}\" is an ISO 8601 timestamp; use yyyy-MM-dd HH:mm:ss in your ServiceNow time zone (the instance silently mis-parses ISO forms)"
        ));
    }
    let valid = raw.len() == 19
        && raw.as_bytes()[10] == b' '
        && parse_date(&raw[0..10]).is_some()
        && parse_time(&raw[11..]).is_some();
    if valid {
        Ok(())
    } else {
        Err(format!(
            "\"{raw}\" is not a date-time; use yyyy-MM-dd HH:mm:ss"
        ))
    }
}

fn has_offset(raw: &str) -> bool {
    raw.get(19..)
        .is_some_and(|rest| rest.starts_with(['+', '-']))
}

fn parse_time(raw: &str) -> Option<()> {
    let bytes = raw.as_bytes();
    if bytes.len() != 8 || bytes[2] != b':' || bytes[5] != b':' {
        return None;
    }
    let hour: u32 = digits(&raw[0..2])?;
    let minute: u32 = digits(&raw[3..5])?;
    let second: u32 = digits(&raw[6..8])?;
    (hour < 24 && minute < 60 && second < 60).then_some(())
}

fn digits(raw: &str) -> Option<u32> {
    raw.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| raw.parse().ok())
        .flatten()
}

/// A validated order, ready to preview or submit.
#[derive(Debug, Clone)]
pub struct OrderPlan {
    pub item_sys_id: String,
    pub item_name: String,
    pub item_class: String,
    pub quantity: u32,
    pub requested_for: Option<String>,
    pub variables: BTreeMap<String, String>,
    pub sensitive: BTreeSet<String>,
    pub warnings: Vec<String>,
}

impl OrderPlan {
    /// Variables with sensitive values replaced, for anything that is displayed.
    pub fn display_variables(&self) -> Map<String, Value> {
        self.variables
            .iter()
            .map(|(name, value)| {
                let shown = if self.sensitive.contains(name) && !value.is_empty() {
                    REDACTED.to_string()
                } else {
                    value.clone()
                };
                (name.clone(), Value::String(shown))
            })
            .collect()
    }

    /// A scrubber replacing every submitted sensitive value in the text of
    /// an order error, or `None` when the order submits no sensitive value.
    /// It matches the raw value only; the client withholds error text that
    /// could hold an encoded form (see `Client::order_catalog_item`).
    pub fn error_scrubber(&self) -> Option<impl Fn(&str) -> String + '_> {
        let mut secrets: Vec<&str> = self
            .sensitive
            .iter()
            .filter_map(|name| self.variables.get(name))
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .collect();
        if secrets.is_empty() {
            return None;
        }
        // Longest first, so a secret containing another is replaced whole.
        secrets.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        secrets.dedup();
        Some(move |text: &str| {
            secrets.iter().fold(text.to_string(), |text, secret| {
                text.replace(secret, REDACTED)
            })
        })
    }

    /// The dry-run plan. Never contains a sensitive value.
    pub fn plan_json(&self) -> Value {
        json!({
            "dryRun": true,
            "operation": "order",
            "item": {
                "sys_id": self.item_sys_id,
                "name": self.item_name,
                "class": self.item_class,
            },
            "quantity": self.quantity,
            "requestedFor": self.requested_for,
            "variables": self.display_variables(),
            "warnings": self.warnings,
        })
    }

    /// The `order_now` request body, with real values.
    pub fn request_body(&self) -> Value {
        let mut body = json!({
            "sysparm_quantity": self.quantity.to_string(),
            "variables": self.variables,
        });
        if let Some(user) = &self.requested_for {
            body["sysparm_requested_for"] = Value::String(user.clone());
        }
        body
    }
}

/// What to do when the caller's date or time format cannot be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnverifiedFormats {
    /// Refuse the order: its dates could be mis-read.
    #[default]
    Refuse,
    /// Order anyway, with a warning (`--allow-unverified-formats`).
    Allow,
    /// A dry run: warn that a real order would be refused.
    Preview,
}

/// How an order is placed, besides its variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderOptions {
    pub quantity: u32,
    pub requested_for: Option<String>,
    pub allow_unknown_vars: bool,
    pub unverified_formats: UnverifiedFormats,
}

impl Default for OrderOptions {
    fn default() -> Self {
        Self {
            quantity: 1,
            requested_for: None,
            allow_unknown_vars: false,
            unverified_formats: UnverifiedFormats::Refuse,
        }
    }
}

/// Validates an order completely, including reference lookups and the
/// caller's date format, and reports every problem at once.
pub async fn prepare_order(
    client: &ServiceNowClient,
    item: &CatalogItem,
    inputs: &BTreeMap<String, InputValue>,
    options: &OrderOptions,
) -> Result<OrderPlan, ApiError> {
    let mut validation =
        validate_locally(item, inputs, options.quantity, options.allow_unknown_vars);
    for check in std::mem::take(&mut validation.references) {
        match resolve_reference_values(client, &check).await? {
            Ok((resolved, warning)) => {
                validation
                    .variables
                    .insert(check.variable.clone(), resolved.join(","));
                validation.warnings.extend(warning);
            }
            Err(problems) => validation.problems.extend(problems),
        }
    }
    if validation.uses_dates {
        let uses_date_times = item.variables.iter().any(|variable| {
            variable.kind == VariableKind::DateTime
                && validation
                    .variables
                    .get(&variable.name)
                    .is_some_and(|value| !value.trim().is_empty())
        });
        let formats = effective_formats(client).await;
        let policy = options.unverified_formats;
        check_format(&mut validation, "date", formats.date, DATE_FORMAT, policy);
        if uses_date_times {
            check_format(&mut validation, "time", formats.time, TIME_FORMAT, policy);
            validation.warnings.push(
                "date-time values are interpreted in your ServiceNow user's time zone".into(),
            );
        }
    }
    if item.instance_orderable == Some(false) {
        validation.warnings.push(INSTANCE_UNORDERABLE.into());
    }
    if !validation.problems.is_empty() {
        return Err(ApiError::InvalidInput(format!(
            "cannot order \"{}\":\n  - {}",
            item.name,
            validation.problems.join("\n  - ")
        )));
    }
    let sensitive = item
        .variables
        .iter()
        .filter(|variable| variable.is_sensitive())
        .map(|variable| variable.name.clone())
        .collect();
    Ok(OrderPlan {
        item_sys_id: item.sys_id.clone(),
        item_name: item.name.clone(),
        item_class: item.class.clone(),
        quantity: options.quantity,
        requested_for: options.requested_for.clone(),
        variables: validation.variables,
        sensitive,
        warnings: validation.warnings,
    })
}

type ReferenceOutcome = Result<(Vec<String>, Option<String>), Vec<String>>;

/// Resolves each value to a sys_id and confirms it exists in the variable's
/// table and matches a plain reference qualifier. Lookup failures that describe
/// the input become problems; transport and authentication failures abort.
async fn resolve_reference_values(
    client: &ServiceNowClient,
    check: &ReferenceCheck,
) -> Result<ReferenceOutcome, ApiError> {
    let name = &check.variable;
    let table = &check.table;
    let (qualifier, warning) = match check.qualifier.as_deref() {
        // A script anywhere in the qualifier refers to the form being filled,
        // which a Table API query cannot see.
        Some(qualifier) if qualifier.to_ascii_lowercase().contains("javascript:") => (
            None,
            Some(format!(
                "{name}: the reference qualifier is a script the CLI cannot evaluate; only the {table} table was checked"
            )),
        ),
        Some(qualifier) => (Some(qualifier), None),
        None => (None, None),
    };
    let mut resolved = Vec::new();
    let mut problems = Vec::new();
    for value in &check.values {
        let sys_id = match identify_reference(client, table, value).await {
            Ok(sys_id) => sys_id,
            Err(
                ApiError::InvalidInput(message)
                | ApiError::NotFound(message)
                | ApiError::Conflict(message),
            ) => {
                problems.push(format!("{name}: {message}"));
                continue;
            }
            Err(error) => return Err(error),
        };
        let query = match qualifier {
            Some(qualifier) => qualified_by(&sys_id, qualifier),
            None => format!("sys_id={sys_id}"),
        };
        let records = client
            .list_records(
                table,
                &ListOptions {
                    query: Some(query),
                    fields: Some(vec!["sys_id".into()]),
                    limit: 1,
                    display_value: DisplayValue::False,
                    ..ListOptions::default()
                },
            )
            .await?;
        // Only the chosen record counts: a qualifier term that escapes the
        // sys_id restriction could otherwise match some other record.
        let found = records
            .iter()
            .any(|record| crate::commands::record_sys_id(record).ok() == Some(sys_id.as_str()));
        if !found {
            problems.push(match qualifier {
                Some(_) => format!(
                    "{name}: \"{value}\" is not a {table} record allowed by the variable's reference qualifier"
                ),
                None => format!("{name}: no {table} record \"{value}\" is visible to you"),
            });
        } else {
            resolved.push(sys_id);
        }
    }
    Ok(if problems.is_empty() {
        Ok((resolved, warning))
    } else {
        Err(problems)
    })
}

/// Restricts an encoded query to one record. `^NQ` starts an independent
/// OR'd query, so the restriction is repeated in each part; prefixing only
/// the first would let any record matching a later part through.
fn qualified_by(sys_id: &str, qualifier: &str) -> String {
    qualifier
        .split("^NQ")
        .map(|part| format!("sys_id={sys_id}^{part}"))
        .collect::<Vec<_>>()
        .join("^NQ")
}

async fn identify_reference(
    client: &ServiceNowClient,
    table: &str,
    value: &str,
) -> Result<String, ApiError> {
    if validate_sys_id(value).is_ok() {
        return Ok(value.to_ascii_lowercase());
    }
    let kind = match table {
        "sys_user" => ReferenceKind::User,
        "sys_user_group" => ReferenceKind::Group,
        _ => {
            return Err(ApiError::InvalidInput(format!(
                "\"{value}\" is not a sys_id; this variable references {table}, so give the record's sys_id"
            )));
        }
    };
    let record = metadata::resolve_reference(client, kind, value).await?;
    crate::commands::record_sys_id(&record).map(str::to_string)
}

/// The instance parses submitted dates with the caller's formats, so any
/// other format than the one the CLI writes would silently shift values.
fn check_format(
    validation: &mut LocalValidation,
    kind: &str,
    format: Option<String>,
    expected: &str,
    unverified: UnverifiedFormats,
) {
    let unreadable =
        format!("could not read your ServiceNow {kind} format, so {kind}s sent as {expected}");
    match (format, unverified) {
        (Some(format), _) if format == expected => {}
        (Some(format), _) => validation.problems.push(format!(
            "your ServiceNow {kind} format is {format}; the CLI only submits {kind}s as {expected}, which the instance would mis-read. Set your user {kind} format to {expected} or use the portal"
        )),
        (None, UnverifiedFormats::Refuse) => validation.problems.push(format!(
            "{unreadable} may be mis-read; set your user {kind} format to {expected}, or pass --allow-unverified-formats to order anyway"
        )),
        (None, UnverifiedFormats::Allow) => validation.warnings.push(format!(
            "{unreadable} may be mis-read (--allow-unverified-formats)"
        )),
        (None, UnverifiedFormats::Preview) => validation.warnings.push(format!(
            "{unreadable} may be mis-read; a real order needs --allow-unverified-formats"
        )),
    }
}

/// The caller's date and time formats. `None` when unreadable.
struct Formats {
    date: Option<String>,
    time: Option<String>,
}

/// Each format is the user's own setting, else the instance default.
async fn effective_formats(client: &ServiceNowClient) -> Formats {
    let Some(user) = first_record(
        client,
        "sys_user",
        "sys_id=javascript:gs.getUserID()",
        &["date_format", "time_format"],
    )
    .await
    else {
        return Formats {
            date: None,
            time: None,
        };
    };
    Formats {
        date: user_or_instance_format(client, &user, "date_format").await,
        time: user_or_instance_format(client, &user, "time_format").await,
    }
}

async fn user_or_instance_format(
    client: &ServiceNowClient,
    user: &Value,
    field: &str,
) -> Option<String> {
    // An absent field is not an empty preference: it means the field was
    // unreadable, so the effective format is unknown.
    let own = user.get(field)?.as_str()?;
    if !own.is_empty() {
        return Some(own.to_string());
    }
    first_record(
        client,
        "sys_properties",
        &format!("name=glide.sys.{field}"),
        &["value"],
    )
    .await?
    .get("value")
    .and_then(Value::as_str)
    .filter(|format| !format.is_empty())
    .map(str::to_string)
}

async fn first_record(
    client: &ServiceNowClient,
    table: &str,
    query: &str,
    fields: &[&str],
) -> Option<Value> {
    let mut records = client
        .list_records(
            table,
            &ListOptions {
                query: Some(query.into()),
                fields: Some(fields.iter().map(|field| field.to_string()).collect()),
                limit: 1,
                display_value: DisplayValue::False,
                ..ListOptions::default()
            },
        )
        .await
        .ok()?;
    if records.is_empty() {
        return None;
    }
    Some(records.swap_remove(0))
}

/// A variable value read back from a requested item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RitmVariable {
    pub name: String,
    pub label: String,
    pub value: String,
    pub display_value: String,
    pub sensitive: bool,
    /// Filled by the form, so `catalog order` does not accept it back.
    pub read_only: bool,
    /// The item has a default, so an empty value was cleared on purpose and
    /// must be submitted again for a reorder to match.
    pub has_default: bool,
    /// The value in the form `catalog order` accepts, or `None` when the
    /// response does not carry it.
    pub order_value: Option<String>,
}

/// Variable names that can be dot-walked with `variables.<name>`.
pub fn readable_variable_names(item: &CatalogItem) -> Vec<String> {
    item.variables
        .iter()
        .filter(|variable| submits_value(&variable.kind))
        .filter(|variable| {
            variable
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .map(|variable| variable.name.clone())
        .collect()
}

/// Pairs the item's variable definitions with the values in a requested item
/// read with `sysparm_display_value=all`. A variable the response omits is
/// skipped, never reported as empty.
pub fn ritm_variables(item: &CatalogItem, record: &Value) -> Vec<RitmVariable> {
    item.variables
        .iter()
        .filter_map(|variable| {
            let cell = record.get(format!("variables.{}", variable.name))?;
            let (value, display_value) = match cell {
                Value::Object(object) => (
                    object.get("value").and_then(scalar_string),
                    object.get("display_value").and_then(scalar_string),
                ),
                other => (scalar_string(other), None),
            };
            let value = value?;
            // A date-time is stored in UTC and ordered in the user's time
            // zone, which only its display value is in.
            let order_value = match variable.kind {
                VariableKind::DateTime if !value.is_empty() => display_value.clone(),
                _ => Some(value.clone()),
            };
            Some(RitmVariable {
                name: variable.name.clone(),
                label: variable.label.clone(),
                order_value,
                display_value: display_value.unwrap_or_else(|| value.clone()),
                value,
                sensitive: variable.is_sensitive(),
                read_only: variable.read_only,
                has_default: variable.default_value.is_some(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CART_ITEM: &str = "346489831b181710b97aa975624bcb75";

    #[test]
    fn a_cart_lists_items_from_every_frequency_group() {
        let cart = Cart::from_api(&json!({
            "cart_id": CART,
            "none": {"frequency_label": null, "items": [{
                "cart_item_id": CART_ITEM,
                "item_name": "AWS account request",
                "quantity": "1",
                "can_order": true,
            }]},
            "monthly": {"frequency_label": "Monthly", "items": [{
                "cart_item_id": "456489831b181710b97aa975624bcbb2",
                "item_name": "Zoom",
                "can_order": false,
            }]},
            "subtotal_price": "-",
        }))
        .unwrap();
        assert_eq!(cart.cart_id, CART);
        let names: Vec<&str> = cart.items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["Zoom", "AWS account request"]);
        assert_eq!(cart.items[0].can_order, Some(false));
        assert_eq!(cart.items[0].details["frequency"], "monthly");
        assert_eq!(cart.items[1].details["frequency"], "none");
        assert_eq!(cart.items[1].details["quantity"], "1");
        assert_eq!(cart.item_names(), "Zoom, AWS account request");
    }

    #[test]
    fn an_empty_cart_has_no_items() {
        let cart = Cart::from_api(&json!({
            "cart_id": CART,
            "show_subtotal_price": "false",
            "subtotal_price": "-",
        }))
        .unwrap();
        assert!(cart.items.is_empty());
        assert_eq!(cart.to_json(), json!({"cart_id": CART, "items": []}));
    }

    #[test]
    fn an_add_to_cart_answer_lists_the_whole_cart_at_the_top_level() {
        let cart = Cart::from_api(&json!({
            "cart_id": CART,
            "items": [{"cart_item_id": CART_ITEM, "item_name": "AWS account request"}],
            "subtotal": "$0.00",
        }))
        .unwrap();
        assert_eq!(cart.items.len(), 1);
        assert_eq!(cart.items[0].cart_item_id, CART_ITEM);
        assert_eq!(cart.items[0].can_order, None);
    }

    #[test]
    fn an_item_listed_in_a_group_and_at_the_top_level_counts_once() {
        let cart = Cart::from_api(&json!({
            "cart_id": CART,
            "none": {"items": [{"cart_item_id": CART_ITEM, "item_name": "AWS account request"}]},
            "items": [{
                "cart_item_id": CART_ITEM.to_uppercase(),
                "item_name": "AWS account request",
                "catalog_item_id": "5A1EFA5947520510F53D37D2846D439B",
            }],
        }))
        .unwrap();
        assert_eq!(cart.items.len(), 1);
        assert_eq!(cart.items[0].cart_item_id, CART_ITEM);
        assert_eq!(cart.items[0].details["frequency"], "none");
    }

    #[test]
    fn a_cart_item_names_its_catalog_item_in_lowercase() {
        let cart = Cart::from_api(&json!({
            "cart_id": CART,
            "items": [{
                "cart_item_id": CART_ITEM,
                "catalog_item_id": "5A1EFA5947520510F53D37D2846D439B",
            }],
        }))
        .unwrap();
        assert_eq!(
            cart.items[0].catalog_item_id.as_deref(),
            Some("5a1efa5947520510f53d37d2846d439b")
        );
    }

    #[test]
    fn a_cart_without_a_usable_cart_id_is_refused() {
        for answer in [
            json!({}),
            json!({"cart_id": "not-a-sys-id"}),
            json!([CART]),
            json!(CART),
        ] {
            let error = Cart::from_api(&answer).unwrap_err();
            assert!(error.to_string().contains("without a cart id"), "{error}");
        }
    }

    #[test]
    fn a_cart_item_without_a_usable_id_is_refused() {
        for item in [json!({"item_name": "AWS"}), json!({"cart_item_id": "x"})] {
            let error =
                Cart::from_api(&json!({"cart_id": CART, "none": {"items": [item]}})).unwrap_err();
            assert!(error.to_string().contains("cart item id"), "{error}");
        }
    }

    #[test]
    fn a_cart_item_is_found_by_id_in_any_case() {
        let cart = Cart::from_api(&json!({
            "cart_id": CART,
            "none": {"items": [{"cart_item_id": CART_ITEM, "name": "  "}]},
        }))
        .unwrap();
        assert_eq!(cart.items[0].name, "(unnamed item)");
        assert!(cart.item(&CART_ITEM.to_uppercase()).is_some());
        assert!(cart.item("ffffffffffffffffffffffffffffffff").is_none());
    }

    fn fixture(name: &str) -> CatalogItem {
        let path = format!(
            "{}/tests/fixtures/catalog/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("fixture exists"))
                .expect("fixture is JSON");
        CatalogItem::from_api(&document["result"]).expect("fixture parses")
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

    impl CatalogItem {
        fn variable_mut_for_test(&mut self, name: &str) -> &mut CatalogVariable {
            self.variables
                .iter_mut()
                .find(|variable| variable.name == name)
                .expect("fixture defines the variable")
        }
    }

    fn aws_valid() -> BTreeMap<String, InputValue> {
        inputs(&[
            ("division", "IT"),
            ("environment", "Dev"),
            ("describe_briefly_the_purpose_of_this_account", "Sandbox"),
        ])
    }

    #[test]
    fn parses_select_boxes_and_drops_the_blank_choice() {
        let item = fixture("aws_account_request");
        assert_eq!(item.name, "AWS account request");
        assert_eq!(item.class, "sc_cat_item");
        assert_eq!(item.show_quantity, Some(true));
        assert!(!item.has_client_logic);
        let division = item.variable("division").unwrap();
        assert_eq!(division.kind, VariableKind::Choice);
        assert!(division.mandatory);
        assert_eq!(division.default_value, None);
        assert_eq!(
            division
                .choices
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            ["IT", "Product", "Marketing", "Sales", "Finance"]
        );
    }

    #[test]
    fn flattens_containers_including_unnamed_ones() {
        let laptop = fixture("standard_laptop");
        let acrobat = laptop.variable("acrobat").unwrap();
        assert_eq!(acrobat.kind, VariableKind::CheckBox);
        assert_eq!(acrobat.container.as_deref(), Some("Optional Software"));
        assert_eq!(acrobat.default_value.as_deref(), Some("false"));
        assert!(laptop.variable("optional_label").is_none());

        let zoom = fixture("zoom_webinar");
        let requires_password = zoom.variable("requires_password").unwrap();
        assert_eq!(requires_password.container.as_deref(), Some("Options"));
        assert!(
            zoom.variables
                .iter()
                .all(|variable| !variable.name.is_empty())
        );
    }

    #[test]
    fn parses_masked_list_collector_and_client_logic() {
        let zoom = fixture("zoom_webinar");
        assert!(zoom.has_client_logic);
        let password = zoom.variable("set_password").unwrap();
        assert_eq!(password.kind, VariableKind::Masked);
        assert!(password.is_sensitive());
        assert!(!zoom.variable("requires_password").unwrap().is_sensitive());
        let hosts = zoom.variable("add_co_hosts").unwrap();
        assert_eq!(hosts.kind, VariableKind::ListCollector);
        assert_eq!(hosts.reference_table.as_deref(), Some("sys_user"));
        assert_eq!(
            zoom.variable("select_webinar_start_time").unwrap().kind,
            VariableKind::DateTime
        );
    }

    #[test]
    fn reference_table_comes_from_reference() {
        let item = fixture("mobile_devices_bulk");
        let department = item.variable("department").unwrap();
        assert_eq!(department.kind, VariableKind::Reference);
        assert_eq!(
            department.reference_table.as_deref(),
            Some("cmn_department")
        );
        assert_eq!(department.reference_qualifier, None);
    }

    #[test]
    fn an_optional_multi_row_set_does_not_block_ordering() {
        let item = fixture("mobile_devices_bulk");
        assert!(!item.variable("mobile_devices_set").unwrap().mandatory);
        assert!(item.ensure_orderable("https://x").is_ok());
    }

    #[test]
    fn mandatory_multi_row_sets_make_an_item_unorderable() {
        let mut item = fixture("mobile_devices_bulk");
        item.variable_mut_for_test("mobile_devices_set").mandatory = true;
        let message = item
            .ensure_orderable("https://example.service-now.com")
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("mandatory multi-row variable set (mobile_devices_set)"),
            "{message}"
        );
        assert!(
            message.contains("/sp?id=sc_cat_item&sys_id=e56a7ffe41011300964ff05369414ebd"),
            "{message}"
        );
    }

    #[test]
    fn an_empty_optional_multi_row_set_is_not_sent_and_is_named_in_a_warning() {
        let mut item = fixture("mobile_devices_bulk");
        item.has_client_logic = false;
        let result = validate_locally(
            &item,
            &inputs(&[("business_justification", "New starters")]),
            1,
            false,
        );
        assert!(!result.variables.contains_key("mobile_devices_set"));
        assert!(
            !result
                .problems
                .iter()
                .any(|problem| problem.contains("mobile_devices_set")),
            "{:?}",
            result.problems
        );
        assert_eq!(
            result
                .warnings
                .iter()
                .filter(|warning| warning.contains("mobile_devices_set"))
                .collect::<Vec<_>>(),
            ["mobile_devices_set: optional multi-row variable set left empty"]
        );
    }

    #[test]
    fn an_empty_multi_row_set_warns_that_client_logic_may_require_it() {
        let mut item = fixture("mobile_devices_bulk");
        item.has_client_logic = true;
        let result = validate_locally(&item, &inputs(&[]), 1, false);
        assert!(
            result.warnings.iter().any(|warning| warning.starts_with(
                "mobile_devices_set: optional multi-row variable set left empty; the item's client logic may require rows"
            )),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn a_value_for_a_multi_row_set_is_refused() {
        let item = fixture("mobile_devices_bulk");
        let result = validate_locally(
            &item,
            &inputs(&[("mobile_devices_set", "[{\"model\":\"x\"}]")]),
            1,
            false,
        );
        assert!(!result.variables.contains_key("mobile_devices_set"));
        assert!(
            result.problems.iter().any(|problem| problem
                == "mobile_devices_set: multi-row variable sets cannot be filled by the CLI; remove it, or use the portal to fill it"),
            "{:?}",
            result.problems
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|warning| warning.contains("left empty")),
            "{:?}",
            result.warnings
        );
    }

    const REQUEST: &str = "b54ae46e1b181310b97aa975624bcbf2";
    const CART: &str = "90ed51b23b50dbd0b8094b44c3e45aad";

    #[test]
    fn an_order_answer_naming_a_request_is_a_request() {
        let full = json!({
            "sys_id": REQUEST, "number": "REQ0010001", "request_number": "REQ0010001",
            "request_id": REQUEST, "table": "sc_request"
        });
        assert_eq!(
            OrderOutcome::classify(&full),
            OrderOutcome::Request {
                sys_id: REQUEST.into(),
                number: Some("REQ0010001".into())
            }
        );
        assert_eq!(
            OrderOutcome::classify(&json!({ "sys_id": REQUEST })),
            OrderOutcome::Request {
                sys_id: REQUEST.into(),
                number: None
            }
        );
        // A request wins over a cart when the answer names both.
        assert!(matches!(
            OrderOutcome::classify(&json!({ "request_id": REQUEST, "cart_id": CART })),
            OrderOutcome::Request { .. }
        ));
    }

    #[test]
    fn an_order_answer_with_only_a_cart_is_not_a_request() {
        assert_eq!(
            OrderOutcome::classify(&json!({ "cart_id": CART })),
            OrderOutcome::CartOnly {
                cart_id: CART.into()
            }
        );
        // The sys_id of another table is not a request id.
        assert_eq!(
            OrderOutcome::classify(&json!({ "sys_id": CART, "table": "sc_cart", "cart_id": CART })),
            OrderOutcome::CartOnly {
                cart_id: CART.into()
            }
        );
        // An untyped sys_id beside a cart is the cart's, not a request's.
        assert_eq!(
            OrderOutcome::classify(&json!({ "sys_id": CART, "cart_id": CART })),
            OrderOutcome::CartOnly {
                cart_id: CART.into()
            }
        );
        // So is an untyped sys_id beside an unusable cart id: never a success.
        assert!(matches!(
            OrderOutcome::classify(&json!({ "sys_id": CART, "cart_id": "" })),
            OrderOutcome::Unrecognized { .. }
        ));
    }

    #[test]
    fn an_order_answer_without_valid_ids_is_unrecognized() {
        assert_eq!(
            OrderOutcome::classify(&json!({ "request_id": "REQ0010001", "status": "ok" })),
            OrderOutcome::Unrecognized {
                keys: vec!["request_id".into(), "status".into()],
                number: None
            }
        );
        assert_eq!(
            OrderOutcome::classify(&json!({ "cart_id": "" })),
            OrderOutcome::Unrecognized {
                keys: vec!["cart_id".into()],
                number: None
            }
        );
        assert_eq!(
            OrderOutcome::classify(&json!(null)),
            OrderOutcome::Unrecognized {
                keys: Vec::new(),
                number: None
            }
        );
    }

    #[test]
    fn an_unrecognized_answer_keeps_only_a_well_formed_request_number() {
        assert_eq!(
            OrderOutcome::classify(&json!({ "request_number": " REQ0010001 " })),
            OrderOutcome::Unrecognized {
                keys: vec!["request_number".into()],
                number: Some("REQ0010001".into())
            }
        );
        // Anything else in the field could be an echoed variable value.
        for echoed in ["hunter2 secret", "a@b.example", "REQ-1", ""] {
            assert!(
                matches!(
                    OrderOutcome::classify(&json!({ "request_number": echoed })),
                    OrderOutcome::Unrecognized { number: None, .. }
                ),
                "{echoed:?}"
            );
        }
        // A bare `number` is not known to name a request without a request id.
        assert!(matches!(
            OrderOutcome::classify(&json!({ "number": "REQ0010001" })),
            OrderOutcome::Unrecognized { number: None, .. }
        ));
    }

    #[test]
    fn an_explicit_request_id_is_a_request_whatever_table_the_answer_names() {
        // `table` qualifies the bare `sys_id`; `request_id` names a request by itself.
        assert_eq!(
            OrderOutcome::classify(&json!({
                "request_id": REQUEST, "sys_id": CART, "table": "sc_cart"
            })),
            OrderOutcome::Request {
                sys_id: REQUEST.into(),
                number: None
            }
        );
    }

    #[test]
    fn producers_guides_and_content_are_not_orderable() {
        for class in [
            "sc_cat_item_producer",
            "sc_cat_item_composite_producer",
            "std_change_record_producer",
            "sc_cat_item_guide",
            "sc_cat_item_content",
            "sc_cat_item_wizard",
        ] {
            let mut item = fixture("aws_account_request");
            item.class = class.into();
            let message = item.ensure_orderable("https://x").unwrap_err().to_string();
            assert!(message.contains(class), "{message}");
        }
        for class in [
            "sc_cat_item",
            "pc_hardware_cat_item",
            "pc_software_cat_item",
            "u_custom_item",
        ] {
            assert!(is_orderable_class(class), "{class}");
        }
        assert_eq!(kind_label("sc_cat_item_guide"), "order guide");
        assert_eq!(kind_label("pc_hardware_cat_item"), "hardware");
        assert_eq!(kind_label("u_custom_item"), "u_custom_item");
    }

    #[test]
    fn mandatory_attachment_makes_an_item_unorderable() {
        let mut item = fixture("aws_account_request");
        item.mandatory_attachment = Some(true);
        let message = item.ensure_orderable("https://x").unwrap_err().to_string();
        assert!(message.contains("requires an attachment"), "{message}");
    }

    #[test]
    fn valid_input_passes_without_problems() {
        let item = fixture("aws_account_request");
        let result = validate_locally(&item, &aws_valid(), 1, false);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.variables["division"], "IT");
    }

    #[test]
    fn missing_mandatory_variables_are_named() {
        let item = fixture("aws_account_request");
        let result = validate_locally(&item, &inputs(&[("division", "IT")]), 1, false);
        assert_eq!(
            result.problems,
            [
                "environment: required (Environment)",
                "describe_briefly_the_purpose_of_this_account: required (Describe the purpose of this account briefly)"
            ]
        );
    }

    #[test]
    fn blank_and_whitespace_do_not_satisfy_mandatory() {
        let item = fixture("aws_account_request");
        let mut values = aws_valid();
        values.get_mut("environment").unwrap().value = "   ".into();
        let result = validate_locally(&item, &values, 1, false);
        assert_eq!(result.problems, ["environment: required (Environment)"]);
    }

    #[test]
    fn invalid_choice_is_rejected_and_labels_map_to_values() {
        let item = fixture("aws_account_request");
        let mut values = aws_valid();
        values.get_mut("division").unwrap().value = "Bogus".into();
        values.get_mut("environment").unwrap().value = "prod".into();
        let result = validate_locally(&item, &values, 1, false);
        assert_eq!(
            result.problems,
            [
                "division: \"Bogus\" is not one of the choices: IT, Product, Marketing, Sales, Finance"
            ]
        );
        assert_eq!(result.variables["environment"], "Prod");
    }

    #[test]
    fn unknown_names_are_errors_unless_allowed() {
        let item = fixture("aws_account_request");
        let mut values = aws_valid();
        values.insert(
            "Division".into(),
            InputValue {
                value: "IT".into(),
                from_command_line: false,
                list: false,
            },
        );
        let strict = validate_locally(&item, &values, 1, false);
        assert_eq!(strict.problems.len(), 1);
        assert!(
            strict.problems[0]
                .starts_with("unknown variable 'Division' (did you mean 'division'?)")
        );

        let lenient = validate_locally(&item, &values, 1, true);
        assert!(lenient.problems.is_empty());
        assert!(!lenient.variables.contains_key("Division"));
        assert!(lenient.warnings[0].contains("not sent"));
    }

    #[test]
    fn unknown_names_suggest_the_nearest_variable() {
        let item = fixture("aws_account_request");
        assert_eq!(
            suggestion(&item, "enviroment"),
            " (did you mean 'environment'?)"
        );
        assert_eq!(suggestion(&item, "divsion"), " (did you mean 'division'?)");
        assert_eq!(
            suggestion(&item, "Environment"),
            " (did you mean 'environment'?)"
        );
        assert_eq!(suggestion(&item, "region"), "");
        assert_eq!(suggestion(&item, "env"), "");
    }

    #[test]
    fn layout_widgets_are_not_variables() {
        // Shapes as the instance returns them, misspelt friendly_type included.
        let item = CatalogItem::from_api(&serde_json::json!({
            "sys_id": "item",
            "variables": [
                {"name": "entry_condition", "friendly_type": "macro_with_abel", "type": 17, "label": "Entry"},
                {"name": "widget", "friendly_type": "some_new_widget", "type": 14, "label": "Widget"},
                {"name": "task_sla", "friendly_type": "duration", "type": 29, "label": "Task SLA"},
                {"name": "target_table_name", "friendly_type": "table_name", "type": 40, "label": "Table"}
            ]
        }))
        .expect("parses");
        let names: Vec<&str> = item.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["task_sla", "target_table_name"]);
        assert_eq!(item.variables[0].kind, VariableKind::Unknown);
        // Custom widgets may collect input the CLI cannot provide, so they
        // are named instead of dropped silently.
        assert_eq!(item.custom_widgets, vec!["Entry", "Widget"]);
    }

    #[test]
    fn unrecognised_kinds_are_sent_and_read_back() {
        let mut item = fixture("standard_laptop");
        item.variables.push(CatalogVariable {
            name: "rating".into(),
            label: "Rating".into(),
            type_name: "numeric_scale".into(),
            kind: VariableKind::Unknown,
            mandatory: false,
            read_only: false,
            default_value: Some("3".into()),
            choices: Vec::new(),
            reference_table: None,
            reference_qualifier: None,
            container: None,
            order: 0,
        });
        let result = validate_locally(&item, &inputs(&[]), 1, false);
        assert_eq!(
            result.variables.get("rating").map(String::as_str),
            Some("3")
        );
        assert!(
            result.warnings.iter().any(|w| w.starts_with("rating:")),
            "{:?}",
            result.warnings
        );
        assert!(readable_variable_names(&item).contains(&"rating".to_string()));
        let record = serde_json::json!({
            "variables.rating": {"value": "5", "display_value": "5"}
        });
        let read = ritm_variables(&item, &record);
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].order_value.as_deref(), Some("5"));
    }

    #[test]
    fn check_boxes_and_yes_no_are_normalized() {
        let mut item = fixture("standard_laptop");
        item.variables.push(CatalogVariable {
            name: "needs_vpn".into(),
            label: "Needs VPN".into(),
            type_name: "yes_no".into(),
            kind: VariableKind::YesNo,
            mandatory: false,
            read_only: false,
            default_value: None,
            choices: Vec::new(),
            reference_table: None,
            reference_qualifier: None,
            container: None,
            order: 0,
        });
        let result = validate_locally(
            &item,
            &inputs(&[
                ("acrobat", "YES"),
                ("photoshop", "0"),
                ("needs_vpn", "true"),
            ]),
            1,
            false,
        );
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.variables["acrobat"], "true");
        assert_eq!(result.variables["photoshop"], "false");
        assert_eq!(result.variables["needs_vpn"], "Yes");

        let bad = validate_locally(&item, &inputs(&[("acrobat", "maybe")]), 1, false);
        assert_eq!(
            bad.problems,
            ["acrobat: \"maybe\" is not a boolean; use true or false"]
        );
    }

    #[test]
    fn a_mandatory_check_box_must_be_ticked() {
        let mut item = fixture("standard_laptop");
        let acrobat = item
            .variables
            .iter_mut()
            .find(|variable| variable.name == "acrobat")
            .unwrap();
        acrobat.mandatory = true;
        let unticked = validate_locally(&item, &inputs(&[("acrobat", "no")]), 1, false);
        assert_eq!(unticked.problems, ["acrobat: required (Adobe Acrobat)"]);
        let defaulted = validate_locally(&item, &inputs(&[]), 1, false);
        assert_eq!(defaulted.problems, ["acrobat: required (Adobe Acrobat)"]);
        let ticked = validate_locally(&item, &inputs(&[("acrobat", "yes")]), 1, false);
        assert!(ticked.problems.is_empty(), "{:?}", ticked.problems);
    }

    #[test]
    fn sensitive_values_are_refused_on_the_command_line() {
        let zoom = fixture("zoom_webinar");
        let from = |from_command_line| {
            BTreeMap::from([(
                "set_password".to_string(),
                InputValue {
                    value: "hunter2".into(),
                    from_command_line,
                    list: false,
                },
            )])
        };
        let flagged = validate_locally(&zoom, &from(true), 1, false);
        assert!(
            flagged.problems.iter().any(|problem| problem.starts_with(
                "set_password: looks sensitive (masked, or its name or label suggests a secret), so it is not accepted with --var"
            ) && problem.contains("--vars-file <file>")
                && problem.contains("--vars-file - with --yes")),
            "{:?}",
            flagged.problems
        );
        assert!(!flagged.variables.contains_key("set_password"));
        assert!(
            flagged
                .problems
                .iter()
                .all(|problem| !problem.contains("hunter2")),
            "{:?}",
            flagged.problems
        );
        let from_file = validate_locally(&zoom, &from(false), 1, false);
        assert_eq!(from_file.variables["set_password"], "hunter2");
    }

    #[test]
    fn malformed_assignments_do_not_echo_the_value() {
        for assignment in ["hunter2", "=hunter2"] {
            let error = parse_var_assignment(assignment).unwrap_err().to_string();
            assert!(!error.contains("hunter2"), "{error}");
        }
        assert!(
            parse_var_assignment("hunter2")
                .unwrap_err()
                .to_string()
                .contains("name=value")
        );
    }

    #[test]
    fn vars_documents_keep_values_exactly_as_written() {
        let yaml = parse_vars_document(
            "zip: 01234\nversion: 1.10\nbig: 1e3\nflag: yes\nid: 12345678901234567890123\n",
        )
        .unwrap();
        assert_eq!(yaml["zip"].value, "01234");
        assert_eq!(yaml["version"].value, "1.10");
        assert_eq!(yaml["big"].value, "1e3");
        assert_eq!(yaml["flag"].value, "yes");
        assert_eq!(yaml["id"].value, "12345678901234567890123");
        let json =
            parse_vars_document(r#"{"version": 1.10, "id": 12345678901234567890123}"#).unwrap();
        assert_eq!(json["version"].value, "1.10");
        assert_eq!(json["id"].value, "12345678901234567890123");
    }

    #[test]
    fn vars_document_errors_give_a_json_position_only_for_json() {
        let message = |document: &str| match parse_vars_document(document) {
            Err(ApiError::InvalidInput(message)) => message,
            other => panic!("{document:?}: {other:?}"),
        };
        assert!(
            message("{\"topic\": \"Launch\",\n  \"set_password\": }")
                .ends_with("(JSON parsing stopped at line 2, column 19)"),
            "{}",
            message("{\"topic\": \"Launch\",\n  \"set_password\": }")
        );
        for yaml in ["a: ~", "- one\n- two", "a:\n  b: c"] {
            assert!(
                !message(yaml).contains("JSON parsing"),
                "{yaml:?}: {}",
                message(yaml)
            );
        }
    }

    #[test]
    fn vars_documents_reject_duplicate_names() {
        for document in [
            r#"{"environment": "Prod", "environment": "Dev"}"#,
            "environment: Prod\nenvironment: Dev\n",
            "1: a\n\"1\": b\n",
        ] {
            assert!(parse_vars_document(document).is_err(), "{document}");
        }
    }

    #[test]
    fn defaults_are_validated_like_supplied_values() {
        let mut bulk = fixture("mobile_devices_bulk");
        bulk.variables
            .retain(|variable| variable.name != "mobile_devices_set");
        let department = bulk.variable_mut_for_test("department");
        department.default_value = Some("Finance".into());
        let result = validate_locally(&bulk, &inputs(&[("business_justification", "x")]), 1, false);
        assert_eq!(result.references.len(), 1, "{:?}", result.references);
        assert_eq!(result.references[0].variable, "department");
        assert_eq!(result.references[0].values, vec!["Finance".to_string()]);

        let mut zoom = fixture("zoom_webinar");
        zoom.variable_mut_for_test("select_webinar_start_time")
            .default_value = Some("2026-12-01 10:00:00".into());
        let dated = validate_locally(
            &zoom,
            &inputs(&[("topic", "Launch"), ("select_webinar_end_time", "")]),
            1,
            false,
        );
        assert!(dated.uses_dates);

        let mut aws = fixture("aws_account_request");
        aws.variable_mut_for_test("division").default_value = Some("Nowhere".into());
        let mut values = aws_valid();
        values.remove("division");
        let bad = validate_locally(&aws, &values, 1, false);
        let division: Vec<&String> = bad
            .problems
            .iter()
            .filter(|p| p.starts_with("division:"))
            .collect();
        assert_eq!(division.len(), 1, "{:?}", bad.problems);
        assert!(
            division[0].contains("the item's default value"),
            "{division:?}"
        );

        let mut read_only = fixture("aws_account_request");
        let division = read_only.variable_mut_for_test("division");
        division.read_only = true;
        division.default_value = Some("IT".into());
        let kept = validate_locally(&read_only, &values, 1, false);
        assert_eq!(
            kept.variables.get("division").map(String::as_str),
            Some("IT")
        );
        assert!(
            !kept.problems.iter().any(|p| p.starts_with("division:")),
            "{:?}",
            kept.problems
        );

        // A read-only default is submitted, so it is checked too.
        let mut zoom = fixture("zoom_webinar");
        let start = zoom.variable_mut_for_test("select_webinar_start_time");
        start.read_only = true;
        start.default_value = Some("2026-12-01 10:00:00".into());
        let dated = validate_locally(
            &zoom,
            &inputs(&[("topic", "Launch"), ("select_webinar_end_time", "")]),
            1,
            false,
        );
        assert!(dated.uses_dates);

        let mut broken = fixture("aws_account_request");
        let division = broken.variable_mut_for_test("division");
        division.read_only = true;
        division.default_value = Some("Nowhere".into());
        let refused = validate_locally(&broken, &values, 1, false);
        let division: Vec<&String> = refused
            .problems
            .iter()
            .filter(|p| p.starts_with("division:"))
            .collect();
        assert_eq!(division.len(), 1, "{:?}", refused.problems);
        assert!(
            division[0].contains("read-only default") && !division[0].contains("override"),
            "{division:?}"
        );
    }

    #[test]
    fn a_blank_constrained_value_is_sent_empty() {
        let mut zoom = fixture("zoom_webinar");
        for variable in &mut zoom.variables {
            variable.mandatory = false;
        }
        let result = validate_locally(
            &zoom,
            &inputs(&[
                ("topic", "  "),
                ("select_webinar_end_time", " "),
                ("add_co_hosts", " \t"),
            ]),
            1,
            false,
        );
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert!(result.references.is_empty(), "{:?}", result.references);
        // Free text keeps what was written; a typed value has nothing to send.
        assert_eq!(result.variables["topic"], "  ");
        assert_eq!(result.variables["select_webinar_end_time"], "");
        assert_eq!(result.variables["add_co_hosts"], "");
    }

    #[test]
    fn vars_document_list_items_may_not_contain_commas() {
        for document in [
            "hosts: [abel, \"beth,carol\"]\n",
            r#"{"hosts": ["abel", "beth,carol"]}"#,
        ] {
            let error = parse_vars_document(document).unwrap_err().to_string();
            assert!(error.contains("comma"), "{error}");
            assert!(!error.contains("carol"), "{error}");
        }
    }

    #[test]
    fn a_list_is_refused_for_a_single_value_variable() {
        let zoom = fixture("zoom_webinar");
        let mut values = parse_vars_document("add_co_hosts: [abel.tuter]\n").unwrap();
        values.extend(parse_vars_document("topic: [a, b]\n").unwrap());
        let result = validate_locally(&zoom, &values, 1, false);
        assert!(
            result
                .problems
                .iter()
                .any(|p| p.starts_with("topic: a list was given")),
            "{:?}",
            result.problems
        );
        assert!(
            !result
                .problems
                .iter()
                .any(|p| p.starts_with("add_co_hosts:")),
            "{:?}",
            result.problems
        );
    }

    #[test]
    fn invalid_vars_documents_do_not_quote_their_source() {
        let error = parse_vars_document("set_password: [hunter2\nother: x")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("--vars-file must be a JSON or YAML object"),
            "{error}"
        );
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn date_times_with_trailing_multibyte_text_are_rejected_without_panicking() {
        assert!(validate_date_time("2026-12-01 10:00:0\u{e9}").is_err());
        assert!(validate_date_time("2026-12-01 10:00:00\u{e9}+01").is_err());
        assert!(validate_date_time("2026-12-01 10:00:00+01:00").is_err());
        assert!(validate_date_time("2026-12-01 10:00:00").is_ok());
    }

    #[test]
    fn a_qualifier_with_new_query_parts_restricts_every_part() {
        assert_eq!(qualified_by("abc", "active=true"), "sys_id=abc^active=true");
        assert_eq!(
            qualified_by("abc", "active=true^NQnameSTARTSWITHx"),
            "sys_id=abc^active=true^NQsys_id=abc^nameSTARTSWITHx"
        );
    }

    #[test]
    fn defaults_fill_unsupplied_variables() {
        let item = fixture("standard_laptop");
        let result = validate_locally(&item, &BTreeMap::new(), 1, false);
        assert_eq!(result.variables["acrobat"], "false");
        assert!(
            !result
                .variables
                .contains_key("Additional_software_requirements")
        );
    }

    #[test]
    fn date_times_reject_iso_and_day_first_forms() {
        let item = fixture("zoom_webinar");
        for (value, expected) in [
            ("2026-12-01T10:00:00Z", "ISO 8601"),
            ("2026-12-01 10:00:00+01:00", "ISO 8601"),
            ("01-12-2026 10:00", "not a date-time"),
            ("2026-12-01 10:00", "not a date-time"),
            ("2026-02-30 10:00:00", "not a date-time"),
            ("2026-12-01 24:00:00", "not a date-time"),
        ] {
            let result = validate_locally(
                &item,
                &inputs(&[("select_webinar_start_time", value)]),
                1,
                false,
            );
            let problem = result
                .problems
                .iter()
                .find(|problem| problem.starts_with("select_webinar_start_time:"))
                .unwrap_or_else(|| panic!("{value} was accepted"));
            assert!(problem.contains(expected), "{value}: {problem}");
        }
        let ok = validate_locally(
            &item,
            &inputs(&[("select_webinar_start_time", "2028-02-29 23:59:59")]),
            1,
            false,
        );
        assert!(ok.uses_dates);
        assert!(
            !ok.problems
                .iter()
                .any(|problem| problem.starts_with("select_webinar_start_time:"))
        );
    }

    #[test]
    fn dates_are_strict() {
        assert!(validate_date("2026-12-01").is_ok());
        assert!(validate_date("2024-02-29").is_ok());
        for value in [
            "2026-2-01",
            "01-12-2026",
            "2025-02-29",
            "2026-13-01",
            "2026-12-01 10:00:00",
        ] {
            assert!(validate_date(value).is_err(), "{value}");
        }
    }

    #[test]
    fn references_become_checks_against_their_table() {
        let item = fixture("zoom_webinar");
        let result = validate_locally(
            &item,
            &inputs(&[(
                "add_co_hosts",
                "abel.tuter, 62826bf03710200044e0bfc8bcbe5df1,",
            )]),
            1,
            false,
        );
        assert_eq!(
            result.references,
            [ReferenceCheck {
                variable: "add_co_hosts".into(),
                table: "sys_user".into(),
                qualifier: None,
                values: vec![
                    "abel.tuter".into(),
                    "62826bf03710200044e0bfc8bcbe5df1".into()
                ],
            }]
        );
    }

    #[test]
    fn redaction_placeholder_and_read_only_values_are_rejected() {
        let mut item = fixture("aws_account_request");
        item.variables[0].read_only = true;
        let mut values = aws_valid();
        values.get_mut("environment").unwrap().value = REDACTED.into();
        let result = validate_locally(&item, &values, 1, false);
        assert_eq!(result.problems.len(), 2, "{:?}", result.problems);
        assert!(result.problems[0].starts_with("division: read-only"));
        assert!(result.problems[1].starts_with("environment: value is the redaction placeholder"));
    }

    #[test]
    fn quantity_is_checked_against_the_item() {
        let mut item = fixture("aws_account_request");
        assert!(
            validate_locally(&item, &aws_valid(), 3, false)
                .problems
                .is_empty()
        );
        item.show_quantity = Some(false);
        let problems = validate_locally(&item, &aws_valid(), 3, false).problems;
        assert_eq!(
            problems,
            ["\"AWS account request\" does not allow a quantity; omit --quantity"]
        );
        assert!(
            validate_locally(&item, &aws_valid(), 1, false)
                .problems
                .is_empty()
        );
        assert!(
            !validate_locally(&item, &aws_valid(), 0, false)
                .problems
                .is_empty()
        );
    }

    #[test]
    fn error_scrubber_replaces_overlapping_secrets_whole() {
        let plan = OrderPlan {
            item_sys_id: "a".repeat(32),
            item_name: "Request zoom webinar".into(),
            item_class: "sc_cat_item".into(),
            quantity: 1,
            requested_for: None,
            variables: BTreeMap::from([
                ("topic".into(), "abc".into()),
                ("pin".into(), "abc".into()),
                ("set_password".into(), "abcdef".into()),
                ("api_token".into(), String::new()),
            ]),
            sensitive: BTreeSet::from(["pin".into(), "set_password".into(), "api_token".into()]),
            warnings: Vec::new(),
        };
        let scrub = plan
            .error_scrubber()
            .expect("sensitive values were submitted");
        assert_eq!(
            scrub("got abcdef and abc"),
            format!("got {REDACTED} and {REDACTED}")
        );
    }

    #[test]
    fn error_scrubber_is_absent_without_sensitive_values() {
        let plan = OrderPlan {
            item_sys_id: "a".repeat(32),
            item_name: "Request zoom webinar".into(),
            item_class: "sc_cat_item".into(),
            quantity: 1,
            requested_for: None,
            variables: BTreeMap::from([
                ("topic".into(), "Launch".into()),
                ("set_password".into(), String::new()),
            ]),
            sensitive: BTreeSet::from(["set_password".into()]),
            warnings: Vec::new(),
        };
        assert!(plan.error_scrubber().is_none());
    }

    #[test]
    fn plan_redacts_sensitive_values_but_the_body_does_not() {
        let plan = OrderPlan {
            item_sys_id: "a".repeat(32),
            item_name: "Request zoom webinar".into(),
            item_class: "sc_cat_item".into(),
            quantity: 1,
            requested_for: None,
            variables: BTreeMap::from([
                ("topic".into(), "Launch".into()),
                ("set_password".into(), "hunter2".into()),
            ]),
            sensitive: BTreeSet::from(["set_password".into()]),
            warnings: Vec::new(),
        };
        let shown = plan.plan_json().to_string();
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(plan.plan_json()["variables"]["set_password"], REDACTED);
        assert_eq!(plan.request_body()["variables"]["set_password"], "hunter2");
        assert_eq!(plan.request_body()["sysparm_quantity"], "1");
        assert!(plan.request_body().get("sysparm_requested_for").is_none());
    }

    #[test]
    fn vars_documents_accept_json_and_yaml() {
        let json =
            parse_vars_document(r#"{"division": "IT", "acrobat": true, "count": 2}"#).unwrap();
        assert_eq!(json["acrobat"].value, "true");
        assert_eq!(json["count"].value, "2");
        let yaml =
            parse_vars_document("division: IT\nadd_co_hosts:\n  - abel.tuter\n  - beth.anglin\n")
                .unwrap();
        assert_eq!(yaml["add_co_hosts"].value, "abel.tuter,beth.anglin");
        assert!(parse_vars_document("- a\n- b\n").is_err());
        assert!(parse_vars_document("division:\n  nested: 1\n").is_err());
    }

    #[test]
    fn var_assignments_split_on_the_first_equals_sign() {
        assert_eq!(
            parse_var_assignment("query=a=b").unwrap(),
            ("query".into(), "a=b".into())
        );
        assert!(parse_var_assignment("novalue").is_err());
        assert!(parse_var_assignment("=x").is_err());
        let merged = merge_inputs(
            parse_vars_document("division: [IT]\n").unwrap(),
            vec![("division".into(), "Product".into())],
        );
        assert_eq!(merged["division"].value, "Product");
        assert!(merged["division"].from_command_line);
        assert!(!merged["division"].list);
    }

    #[test]
    fn ritm_variables_pair_definitions_with_values() {
        let item = fixture("zoom_webinar");
        let record = json!({
            "variables.topic": {"value": "Launch", "display_value": "Launch"},
            "variables.add_co_hosts": {"value": "id1,id2", "display_value": "Abel Tuter, Beth Anglin"},
            "variables.set_password": {"value": "hunter2", "display_value": "hunter2"},
        });
        let variables = ritm_variables(&item, &record);
        assert_eq!(variables.len(), 3);
        let hosts = variables
            .iter()
            .find(|variable| variable.name == "add_co_hosts")
            .unwrap();
        assert_eq!(hosts.value, "id1,id2");
        assert_eq!(hosts.display_value, "Abel Tuter, Beth Anglin");
        assert!(
            variables
                .iter()
                .find(|variable| variable.name == "set_password")
                .unwrap()
                .sensitive
        );
        assert!(readable_variable_names(&item).contains(&"select_webinar_end_time".to_string()));
        assert_eq!(hosts.order_value.as_deref(), Some("id1,id2"));
    }

    #[test]
    fn date_times_are_reordered_in_the_users_time_zone() {
        let item = fixture("zoom_webinar");
        // The stored value is UTC; the display value is in the user's zone,
        // which is how `catalog order` reads a date-time.
        let record = json!({
            "variables.select_webinar_start_time": {
                "value": "2027-03-01 18:00:00",
                "display_value": "2027-03-01 10:00:00"
            },
            "variables.select_webinar_end_time": "2027-03-01 19:30:00",
        });
        let variables = ritm_variables(&item, &record);
        let start = variables
            .iter()
            .find(|variable| variable.name == "select_webinar_start_time")
            .unwrap();
        assert_eq!(start.order_value.as_deref(), Some("2027-03-01 10:00:00"));
        let end = variables
            .iter()
            .find(|variable| variable.name == "select_webinar_end_time")
            .unwrap();
        assert_eq!(end.order_value, None, "a UTC value without a display value");
    }
}
