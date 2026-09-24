//! Fixtures shared by the test modules in `state.rs`, `actions.rs`,
//! `render.rs`, `format.rs`, and `mod.rs`.

#![cfg(test)]

use crate::metadata::{ChoiceMetadata, TableMetadata};

use super::TuiOptions;
use super::actions::{IncidentActionForm, IncidentActionKind, IncidentTarget};
use super::format::infer_columns;
use super::state::{App, IncidentTab, Notice};

pub(super) fn app() -> App {
    let mut app = App::new(
        "work",
        "https://dev12345.service-now.com",
        false,
        TuiOptions {
            table: "incident".into(),
            query: None,
            page_size: 25,
            color: true,
        },
    );
    app.records = vec![serde_json::json!({
        "sys_id": {"value": "0123456789abcdef0123456789abcdef", "display_value": "0123456789abcdef0123456789abcdef"},
        "number": {"value": "INC0010001", "display_value": "INC0010001"},
        "priority": {"value": "1", "display_value": "1 - Critical"},
        "short_description": {"value": "Mail is unavailable", "display_value": "Mail is unavailable"},
        "state": {"value": "2", "display_value": "In Progress"},
        "assigned_to": {"value": "abc", "display_value": "Avery Stone"},
        "sys_updated_on": {"value": "2026-08-25 10:15:00", "display_value": "2026-08-25 10:15:00"},
        "u_api_token": "never-rendered"
    })];
    app.records_view = Some(app.ledger_view());
    app.columns = infer_columns(&app.records, &app.table);
    app.table_state.select(Some(0));
    app.notice = Notice::success("Loaded 1 record");
    app
}

pub(super) fn action_form(
    kind: IncidentActionKind,
    return_tab: Option<IncidentTab>,
) -> IncidentActionForm {
    IncidentActionForm::new(
        kind,
        IncidentTarget {
            sys_id: "0123456789abcdef0123456789abcdef".into(),
            title: "INC0010001".into(),
            return_tab,
        },
    )
}

pub(super) fn resolution_metadata() -> TableMetadata {
    TableMetadata {
        table: "incident".into(),
        fetched_at: 0,
        fields: Vec::new(),
        choices: std::collections::BTreeMap::from([
            (
                "state".into(),
                vec![ChoiceMetadata {
                    value: "9".into(),
                    label: "Resolved".into(),
                    sequence: 90,
                }],
            ),
            (
                "close_code".into(),
                vec![ChoiceMetadata {
                    value: "solved_permanently".into(),
                    label: "Solved (Permanently)".into(),
                    sequence: 10,
                }],
            ),
        ]),
    }
}

pub(super) fn rendered_text(buffer: &ratatui::buffer::Buffer) -> String {
    let width = usize::from(buffer.area.width);
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
