//! Guarded incident actions: the note/assign/resolve action types, the
//! `App` methods that prepare a reviewable write and then apply it, and the
//! cancellation/error handling around both steps. Nothing here is guessable
//! by a stray keypress; every write is reviewed before it is sent.

use std::fmt;

use serde_json::{Map, Value};

use crate::api::{ApiError, DisplayValue, ServiceNowClient};
use crate::config::Config;
use crate::incident;
use crate::metadata::{self, ReferenceKind};

use super::cancel::{RequestOutcome, cancellable_api, run_cancellable, wait_for_cancel_key};
use super::format::{nonempty, record_sys_id, record_title};
use super::state::{App, IncidentTab, LoadOutcome, Notice, NoticeKind, Overlay};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IncidentActionKind {
    Note,
    Assign,
    Resolve,
}

impl IncidentActionKind {
    pub(super) const ALL: [Self; 3] = [Self::Note, Self::Assign, Self::Resolve];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Note => "ADD WORK NOTE",
            Self::Assign => "ASSIGN",
            Self::Resolve => "RESOLVE",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Note => "Append operator context to the activity stream",
            Self::Assign => "Set an assignee, assignment group, or both",
            Self::Resolve => "Map the configured state and resolution code atomically",
        }
    }

    pub(super) fn index(self) -> usize {
        match self {
            Self::Note => 0,
            Self::Assign => 1,
            Self::Resolve => 2,
        }
    }

    pub(super) fn progress(self) -> &'static str {
        match self {
            Self::Note => "Preparing work note…",
            Self::Assign => "Resolving assignment references…",
            Self::Resolve => "Validating the incident lifecycle…",
        }
    }

    fn success(self, target: &str) -> String {
        match self {
            Self::Note => format!("Added a work note to {target}."),
            Self::Assign => format!("Assigned {target}."),
            Self::Resolve => format!("Resolved {target}."),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IncidentTarget {
    pub(super) sys_id: String,
    pub(super) title: String,
    pub(super) return_tab: Option<IncidentTab>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IncidentActionMenu {
    pub(super) target: IncidentTarget,
    pub(super) selected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActionField {
    pub(super) label: &'static str,
    pub(super) hint: &'static str,
    pub(super) value: String,
    pub(super) multiline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IncidentActionForm {
    pub(super) kind: IncidentActionKind,
    pub(super) target: IncidentTarget,
    pub(super) fields: Vec<ActionField>,
    pub(super) focused: usize,
}

impl IncidentActionForm {
    pub(super) fn new(kind: IncidentActionKind, target: IncidentTarget) -> Self {
        let fields = match kind {
            IncidentActionKind::Note => vec![ActionField {
                label: "WORK NOTE",
                hint: "Required; Shift-Enter inserts a line break",
                value: String::new(),
                multiline: true,
            }],
            IncidentActionKind::Assign => vec![
                ActionField {
                    label: "ASSIGNEE",
                    hint: "User name, email, display name, @me, or sys_id; optional",
                    value: String::new(),
                    multiline: false,
                },
                ActionField {
                    label: "ASSIGNMENT GROUP",
                    hint: "Exact group name or sys_id; optional",
                    value: String::new(),
                    multiline: false,
                },
            ],
            IncidentActionKind::Resolve => vec![
                ActionField {
                    label: "RESOLUTION CODE",
                    hint: "Configured label or raw value",
                    value: String::new(),
                    multiline: false,
                },
                ActionField {
                    label: "RESOLUTION NOTES",
                    hint: "Required; Shift-Enter inserts a line break",
                    value: String::new(),
                    multiline: true,
                },
            ],
        };
        Self {
            kind,
            target,
            fields,
            focused: 0,
        }
    }

    pub(super) fn focused_field_mut(&mut self) -> &mut ActionField {
        &mut self.fields[self.focused]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PreparedIncidentAction {
    pub(super) form: IncidentActionForm,
    pub(super) body: Map<String, Value>,
    pub(super) preview: Vec<(String, String)>,
}

/// Failure of a step that prepares an incident action: either ServiceNow
/// rejected a call, or the operator cancelled while one was in flight. Kept
/// distinct from `ApiError` because cancellation is not a ServiceNow error.
#[derive(Debug)]
pub(super) enum ActionPrepError {
    Api(ApiError),
    Cancelled,
}

impl fmt::Display for ActionPrepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(error) => write!(f, "{error}"),
            Self::Cancelled => write!(f, "Cancelled."),
        }
    }
}

impl From<ApiError> for ActionPrepError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl App {
    pub(super) async fn prepare_incident_action(
        &mut self,
        client: &ServiceNowClient,
        form: IncidentActionForm,
    ) {
        self.notice = Notice::quiet(format!("{} Esc cancels.", form.kind.progress()));
        let prepared = self
            .build_prepared_incident_action(client, form.clone())
            .await;
        match prepared {
            Ok(prepared) => {
                self.notice = Notice::quiet("Review the exact ServiceNow update before applying.");
                self.action_review_scroll = 0;
                self.overlay = Overlay::IncidentActionReview(prepared);
            }
            Err(ActionPrepError::Cancelled) => {
                self.notice = Notice::quiet("Cancelled.");
                self.overlay = Overlay::IncidentActionForm(form);
            }
            Err(error @ ActionPrepError::Api(_)) => {
                self.notice = Notice::error(error.to_string());
                self.overlay = Overlay::IncidentActionForm(form);
            }
        }
    }

    async fn build_prepared_incident_action(
        &mut self,
        client: &ServiceNowClient,
        form: IncidentActionForm,
    ) -> Result<PreparedIncidentAction, ActionPrepError> {
        let (body, preview) = match form.kind {
            IncidentActionKind::Note => {
                let note = form.fields[0].value.clone();
                let body = incident::work_note_body(note.clone())?;
                (body, vec![("WORK NOTE".into(), note)])
            }
            IncidentActionKind::Assign => {
                let assignee = nonempty(&form.fields[0].value);
                let group = nonempty(&form.fields[1].value);
                incident::require_assignment(assignee, group)?;
                let mut body = Map::new();
                let mut preview = Vec::new();
                if let Some(value) = assignee {
                    let record = cancellable_api(metadata::resolve_reference(
                        client,
                        ReferenceKind::User,
                        value,
                    ))
                    .await?;
                    let sys_id = record_sys_id(&record).ok_or_else(|| {
                        ActionPrepError::Api(ApiError::Other(
                            "resolved user has no usable sys_id".into(),
                        ))
                    })?;
                    body.insert("assigned_to".into(), Value::String(sys_id.into()));
                    preview.push(("ASSIGNEE".into(), record_title(&record)));
                }
                if let Some(value) = group {
                    let record = cancellable_api(metadata::resolve_reference(
                        client,
                        ReferenceKind::Group,
                        value,
                    ))
                    .await?;
                    let sys_id = record_sys_id(&record).ok_or_else(|| {
                        ActionPrepError::Api(ApiError::Other(
                            "resolved group has no usable sys_id".into(),
                        ))
                    })?;
                    body.insert("assignment_group".into(), Value::String(sys_id.into()));
                    preview.push(("ASSIGNMENT GROUP".into(), record_title(&record)));
                }
                (body, preview)
            }
            IncidentActionKind::Resolve => {
                let code = &form.fields[0].value;
                let notes = form.fields[1].value.clone();
                incident::require_resolution_input(code, &notes)?;
                let metadata = if let Some(metadata) = self.incident_metadata.clone() {
                    metadata
                } else if let Some(metadata) = metadata::load(&self.profile, "incident")? {
                    metadata
                } else {
                    cancellable_api(metadata::sync_table(client, &self.profile, "incident")).await?
                };
                self.incident_metadata = Some(metadata.clone());
                let body = incident::resolution_body(&metadata, code, notes.clone(), None)?;
                let resolved_state = body["state"]
                    .as_str()
                    .expect("resolution state is a string");
                let current = cancellable_api(client.get_record(
                    "incident",
                    &form.target.sys_id,
                    Some(&["sys_id".into(), "number".into(), "state".into()]),
                    DisplayValue::All,
                ))
                .await?;
                incident::require_resolvable(&current, resolved_state, &metadata)?;
                let resolution_code = body["close_code"]
                    .as_str()
                    .expect("resolution code is a string");
                let state_label = metadata
                    .choice_label("state", resolved_state)
                    .unwrap_or(resolved_state)
                    .to_string();
                let code_label = metadata
                    .choice_label("close_code", resolution_code)
                    .unwrap_or(resolution_code)
                    .to_string();
                (
                    body,
                    vec![
                        ("STATE".into(), state_label),
                        ("RESOLUTION CODE".into(), code_label),
                        ("RESOLUTION NOTES".into(), notes),
                    ],
                )
            }
        };
        Ok(PreparedIncidentAction {
            form,
            body,
            preview,
        })
    }

    pub(super) async fn execute_incident_action(
        &mut self,
        client: &ServiceNowClient,
        config: &Config,
        prepared: PreparedIncidentAction,
    ) {
        if let Err(error) = config.require_writable() {
            self.notice = Notice::error(error.to_string());
            self.overlay = Overlay::IncidentActionReview(prepared);
            return;
        }
        if prepared.form.kind == IncidentActionKind::Resolve {
            let recheck = async {
                let metadata = self.incident_metadata.as_ref().ok_or_else(|| {
                    ApiError::Other("resolution metadata is no longer available".into())
                })?;
                let resolved_state = prepared
                    .body
                    .get("state")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ApiError::Other("resolution state is missing".into()))?;
                let current = client
                    .get_record(
                        "incident",
                        &prepared.form.target.sys_id,
                        Some(&["sys_id".into(), "number".into(), "state".into()]),
                        DisplayValue::All,
                    )
                    .await?;
                incident::require_resolvable(&current, resolved_state, metadata)
            };
            match run_cancellable(recheck, wait_for_cancel_key()).await {
                RequestOutcome::Completed(Ok(())) => {}
                RequestOutcome::Completed(Err(error)) => {
                    self.notice = Notice::error(format!(
                        "Could not apply resolution after rechecking the incident: {error}"
                    ));
                    self.overlay = Overlay::IncidentActionReview(prepared);
                    return;
                }
                RequestOutcome::Cancelled => {
                    self.notice = Notice::quiet("Cancelled. The resolution was not applied.");
                    self.overlay = Overlay::IncidentActionReview(prepared);
                    return;
                }
            }
        }
        self.notice = Notice::quiet(format!(
            "Applying {}… Esc cancels.",
            prepared.form.kind.label()
        ));
        // A cancelled write may or may not have already reached ServiceNow, so
        // its outcome is unknown rather than a failure: fall through to refresh
        // the ledger instead of returning, and never claim success or failure
        // for this action below.
        let write_cancelled = match run_cancellable(
            client.update_record("incident", &prepared.form.target.sys_id, &prepared.body),
            wait_for_cancel_key(),
        )
        .await
        {
            RequestOutcome::Completed(Ok(_)) => false,
            RequestOutcome::Completed(Err(error)) => {
                self.notice = Notice::error(format!(
                    "Could not apply {}: {error}",
                    prepared.form.kind.label().to_ascii_lowercase()
                ));
                self.overlay = Overlay::IncidentActionReview(prepared);
                return;
            }
            RequestOutcome::Cancelled => true,
        };

        let success = prepared.form.kind.success(&prepared.form.target.title);
        let target_sys_id = prepared.form.target.sys_id.clone();
        let return_tab = prepared.form.target.return_tab;
        let unknown_outcome = format!(
            "Cancelled while applying {}. ServiceNow may or may not have received the update \
             before the connection was cancelled, so the outcome is unknown.",
            prepared.form.kind.label().to_ascii_lowercase()
        );
        self.overlay = Overlay::None;
        match self.load(client).await {
            LoadOutcome::Loaded => {}
            LoadOutcome::Failed => {
                self.notice = Notice::error(if write_cancelled {
                    format!("{unknown_outcome} The ledger refresh also failed; press r to retry.")
                } else {
                    format!("{success} The ledger refresh failed; press r to retry.")
                });
                return;
            }
            LoadOutcome::Cancelled => {
                self.notice = Notice::quiet(if write_cancelled {
                    format!(
                        "{unknown_outcome} The ledger refresh was also cancelled; press r to reload."
                    )
                } else {
                    format!("{success} The ledger refresh was cancelled; press r to reload.")
                });
                return;
            }
        }
        let unknown_outcome_current_ledger =
            format!("{unknown_outcome} The ledger below reflects the current state.");

        let selected = self.matching_record_indices().position(|record_index| {
            self.records
                .get(record_index)
                .and_then(record_sys_id)
                .is_some_and(|sys_id| sys_id == target_sys_id)
        });
        let Some(selected) = selected else {
            self.table_state.select(None);
            self.notice = if write_cancelled {
                Notice::quiet(unknown_outcome_current_ledger)
            } else {
                Notice::success(format!("{success} It no longer matches this ledger view."))
            };
            return;
        };
        self.table_state.select(Some(selected));

        if write_cancelled {
            self.notice = Notice::quiet(unknown_outcome_current_ledger);
            return;
        }

        if let Some(tab) = return_tab {
            self.overlay = Overlay::Detail;
            self.detail_loading = true;
            let mut reload = self.load_detail(client).await;
            let overview_error = self.overview_error.clone();
            if tab != IncidentTab::Overview && reload != LoadOutcome::Cancelled {
                self.incident_tab = tab;
                self.detail_scroll = 0;
                reload = self.load_incident_tab(client, tab).await;
            }
            // An error from load_detail must survive a later successful
            // load_incident_tab call, which otherwise overwrites self.notice
            // with its own (non-error) result.
            if let Some(overview_error) = overview_error {
                self.notice = Notice::error(format!(
                    "{success} {overview_error}. Showing index fields only."
                ));
                return;
            }
            if reload == LoadOutcome::Cancelled {
                self.notice = Notice::quiet(format!(
                    "{success} The incident reload was cancelled, so it may be out of date."
                ));
                return;
            }
            if self.notice.kind == NoticeKind::Error {
                self.notice = Notice::error(format!("{success} {}", self.notice.text));
                return;
            }
        }
        self.notice = Notice::success(success);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::AuthType;

    use super::super::cancel::arm_test_cancel;
    use super::super::state::Action;
    use super::super::test_support::{action_form, app, rendered_text, resolution_metadata};
    use super::*;

    #[test]
    fn incident_action_sheet_is_discoverable_and_read_only_is_explicit() {
        let backend = TestBackend::new(96, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.open_incident_actions(false);
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("INCIDENT ACTIONS"));
        assert!(text.contains("ADD WORK NOTE"));
        assert!(text.contains("ASSIGN"));
        assert!(text.contains("RESOLVE"));
        assert!(text.contains("review it before anything changes"));

        app.read_only = true;
        app.open_incident_actions(false);
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("READ ONLY"));
        assert!(text.contains("Actions are visible but unavailable"));
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
        assert!(matches!(app.overlay, Overlay::IncidentActions(_)));
        assert!(app.notice.text.contains("read-only"));
    }

    #[test]
    fn incident_action_form_advances_fields_and_preserves_text_for_review() {
        let mut app = app();
        app.open_incident_actions(true);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
            Action::None
        );
        let Overlay::IncidentActionForm(form) = &mut app.overlay else {
            panic!("expected resolution form");
        };
        form.fields[0].value = "Solved (Permanently)".into();
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
        let Overlay::IncidentActionForm(form) = &mut app.overlay else {
            panic!("expected resolution form");
        };
        assert_eq!(form.focused, 1);
        form.fields[1].value = "Corrected the mail gateway".into();
        let Action::PrepareIncident(form) =
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("expected action preparation");
        };
        assert_eq!(form.fields[0].value, "Solved (Permanently)");
        assert_eq!(form.fields[1].value, "Corrected the mail gateway");
        assert_eq!(form.target.return_tab, Some(IncidentTab::Overview));
    }

    #[tokio::test]
    async fn assignment_preparation_resolves_people_and_groups_before_review() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "sys_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "name": "Avery Stone",
                    "email": "avery@example.com"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_user_group"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "sys_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "name": "Network"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let mut app = app();
        let mut form = action_form(IncidentActionKind::Assign, None);
        form.fields[0].value = "avery@example.com".into();
        form.fields[1].value = "Network".into();

        let prepared = app
            .build_prepared_incident_action(&client, form)
            .await
            .unwrap();
        assert_eq!(
            prepared.body["assigned_to"],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            prepared.body["assignment_group"],
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(prepared.preview[0].1, "Avery Stone");
        assert_eq!(prepared.preview[1].1, "Network");
    }

    #[tokio::test]
    async fn resolution_preparation_maps_choices_and_rechecks_current_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {
                    "sys_id": "0123456789abcdef0123456789abcdef",
                    "number": "INC0010001",
                    "state": {"value": "2", "display_value": "In Progress"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let mut app = app();
        app.incident_metadata = Some(resolution_metadata());
        let mut form = action_form(IncidentActionKind::Resolve, Some(IncidentTab::Activity));
        form.fields[0].value = "Solved (Permanently)".into();
        form.fields[1].value = "Corrected the mail gateway".into();

        let prepared = app
            .build_prepared_incident_action(&client, form)
            .await
            .unwrap();
        assert_eq!(prepared.body["state"], "9");
        assert_eq!(prepared.body["close_code"], "solved_permanently");
        assert_eq!(prepared.body["close_notes"], "Corrected the mail gateway");
        assert_eq!(prepared.preview[0].1, "Resolved");
        assert_eq!(prepared.preview[1].1, "Solved (Permanently)");
    }

    #[tokio::test]
    async fn resolution_confirmation_rechecks_state_before_patching() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {
                    "sys_id": "0123456789abcdef0123456789abcdef",
                    "number": "INC0010001",
                    "state": {"value": "9", "display_value": "Resolved"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let metadata = resolution_metadata();
        let body = incident::resolution_body(
            &metadata,
            "Solved (Permanently)",
            "Corrected the mail gateway".into(),
            None,
        )
        .unwrap();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Resolve, None),
            body,
            preview: vec![("STATE".into(), "Resolved".into())],
        };
        let mut app = app();
        app.incident_metadata = Some(metadata);

        app.execute_incident_action(&client, &Config::for_test(&server.uri(), false), prepared)
            .await;

        assert!(matches!(app.overlay, Overlay::IncidentActionReview(_)));
        assert!(app.notice.text.contains("already resolved"));
    }

    #[tokio::test]
    async fn confirmed_incident_action_patches_once_and_refreshes_the_ledger() {
        let server = MockServer::start().await;
        let body = incident::work_note_body("Investigating the gateway".into()).unwrap();
        Mock::given(method("PATCH"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .and(body_json(serde_json::json!({
                "work_notes": "Investigating the gateway"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {"sys_id": "0123456789abcdef0123456789abcdef"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "sys_id": "0123456789abcdef0123456789abcdef",
                    "number": "INC0010001",
                    "state": {"value": "2", "display_value": "In Progress"}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let config = Config::for_test(&server.uri(), false);
        let mut app = app();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Note, None),
            body,
            preview: vec![("WORK NOTE".into(), "Investigating the gateway".into())],
        };

        app.execute_incident_action(&client, &config, prepared)
            .await;
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.notice.kind, NoticeKind::Success);
        assert_eq!(app.notice.text, "Added a work note to INC0010001.");
        assert_eq!(
            app.selected_record().map(record_title).as_deref(),
            Some("INC0010001")
        );
    }

    #[tokio::test]
    async fn failed_or_read_only_writes_keep_the_review_intact() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": {"message": "Update rejected", "detail": "Business rule failed"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let body = incident::work_note_body("Keep this text".into()).unwrap();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Note, None),
            body,
            preview: vec![("WORK NOTE".into(), "Keep this text".into())],
        };
        let mut app = app();
        app.execute_incident_action(&client, &Config::for_test(&server.uri(), false), prepared)
            .await;
        let Overlay::IncidentActionReview(prepared) = &app.overlay else {
            panic!("failed write should preserve review");
        };
        assert_eq!(prepared.preview[0].1, "Keep this text");
        assert!(app.notice.text.contains("Update rejected"));

        app.execute_incident_action(
            &client,
            &Config::for_test(&server.uri(), true),
            prepared.clone(),
        )
        .await;
        assert!(matches!(app.overlay, Overlay::IncidentActionReview(_)));
        assert!(app.notice.text.contains("read-only"));
    }

    #[tokio::test]
    async fn stale_selection_is_cleared_when_the_acted_on_incident_no_longer_matches() {
        let server = MockServer::start().await;
        let body = incident::work_note_body("Investigating the gateway".into()).unwrap();
        Mock::given(method("PATCH"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {"sys_id": "0123456789abcdef0123456789abcdef"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "sys_id": "fedcba9876543210fedcba9876543210",
                    "number": "INC0010002",
                    "state": {"value": "2", "display_value": "In Progress"}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let config = Config::for_test(&server.uri(), false);
        let mut app = app();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Note, None),
            body,
            preview: vec![("WORK NOTE".into(), "Investigating the gateway".into())],
        };

        app.execute_incident_action(&client, &config, prepared)
            .await;

        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.notice.kind, NoticeKind::Success);
        assert!(
            app.notice
                .text
                .contains("no longer matches this ledger view")
        );
        assert_eq!(
            app.table_state.selected(),
            None,
            "a record at the old index must not stay selected once it belongs to a different incident"
        );
    }

    #[tokio::test]
    async fn overview_error_survives_a_successful_incident_tab_reload() {
        let server = MockServer::start().await;
        let body = incident::work_note_body("Investigating the gateway".into()).unwrap();
        Mock::given(method("PATCH"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {"sys_id": "0123456789abcdef0123456789abcdef"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "sys_id": "0123456789abcdef0123456789abcdef",
                    "number": "INC0010001",
                    "state": {"value": "2", "display_value": "In Progress"}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": {"message": "Detail lookup failed"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_journal_field"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let config = Config::for_test(&server.uri(), false);
        let mut app = app();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Note, Some(IncidentTab::Activity)),
            body,
            preview: vec![("WORK NOTE".into(), "Investigating the gateway".into())],
        };

        app.execute_incident_action(&client, &config, prepared)
            .await;

        assert_eq!(
            app.notice.kind,
            NoticeKind::Error,
            "the overview load failure must not be replaced by the activity tab's success, got: {:?}",
            app.notice
        );
        assert!(app.notice.text.contains("Detail lookup failed"));
    }

    #[tokio::test]
    async fn cancelling_a_write_reports_an_unknown_outcome_and_refreshes_the_ledger() {
        let server = MockServer::start().await;
        let body = incident::work_note_body("Investigating the gateway".into()).unwrap();
        Mock::given(method("PATCH"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(serde_json::json!({
                        "result": {"sys_id": "0123456789abcdef0123456789abcdef"}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "sys_id": "0123456789abcdef0123456789abcdef",
                    "number": "INC0010001",
                    "state": {"value": "2", "display_value": "In Progress"}
                }]
            })))
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let config = Config::for_test(&server.uri(), false);
        let mut app = app();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Note, None),
            body,
            preview: vec![("WORK NOTE".into(), "Investigating the gateway".into())],
        };
        let _cancel = arm_test_cancel(Duration::from_millis(50));

        let started = std::time::Instant::now();
        app.execute_incident_action(&client, &config, prepared)
            .await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "cancellation should return well before the mocked 5s delay, took {elapsed:?}"
        );
        assert_eq!(app.notice.kind, NoticeKind::Quiet);
        assert!(
            app.notice.text.contains("unknown"),
            "a cancelled write must say the outcome is unknown, got: {:?}",
            app.notice.text
        );
        assert!(
            !app.notice.text.contains("Added a work note"),
            "a cancelled write must not claim success, got: {:?}",
            app.notice.text
        );
        assert!(
            !app.notice.text.to_lowercase().contains("failed"),
            "a cancelled write must not claim failure, got: {:?}",
            app.notice.text
        );
        assert!(
            app.notice.text.contains("reflects the current state"),
            "a completed refresh after a cancelled write vouches for the ledger, got: {:?}",
            app.notice.text
        );
    }

    async fn mount_note_patch(server: &MockServer, delay: Duration) {
        Mock::given(method("PATCH"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_json(
                serde_json::json!({
                    "result": {"sys_id": "0123456789abcdef0123456789abcdef"}
                }),
            ))
            .mount(server)
            .await;
    }

    async fn mount_ledger(server: &MockServer, delay: Duration) {
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_json(
                serde_json::json!({
                    "result": [{
                        "sys_id": "0123456789abcdef0123456789abcdef",
                        "number": "INC0010001",
                        "state": {"value": "6", "display_value": "Resolved"}
                    }]
                }),
            ))
            .mount(server)
            .await;
    }

    fn ledger_state(app: &App) -> &str {
        app.records[0]["state"]["display_value"]
            .as_str()
            .unwrap_or_default()
    }

    async fn run_note_action(server: &MockServer, return_tab: Option<IncidentTab>) -> App {
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let config = Config::for_test(&server.uri(), false);
        let mut app = app();
        let prepared = PreparedIncidentAction {
            form: action_form(IncidentActionKind::Note, return_tab),
            body: incident::work_note_body("Investigating the gateway".into()).unwrap(),
            preview: vec![("WORK NOTE".into(), "Investigating the gateway".into())],
        };
        let _cancel = arm_test_cancel(Duration::from_millis(50));
        app.execute_incident_action(&client, &config, prepared)
            .await;
        app
    }

    #[tokio::test]
    async fn cancelling_the_refresh_after_a_write_does_not_claim_a_current_ledger() {
        let server = MockServer::start().await;
        mount_note_patch(&server, Duration::ZERO).await;
        mount_ledger(&server, Duration::from_secs(5)).await;

        let app = run_note_action(&server, None).await;

        assert_eq!(
            ledger_state(&app),
            "In Progress",
            "the refresh was cancelled"
        );
        assert_ne!(
            app.notice.kind,
            NoticeKind::Success,
            "a bare success notice hides that the ledger is stale, got: {:?}",
            app.notice
        );
        assert!(
            app.notice.text.contains("Added a work note to INC0010001."),
            "the write itself completed, got: {:?}",
            app.notice.text
        );
        assert!(
            app.notice.text.contains("refresh was cancelled"),
            "the notice must say the ledger was not refreshed, got: {:?}",
            app.notice.text
        );
    }

    #[tokio::test]
    async fn cancelling_both_the_write_and_the_refresh_keeps_the_outcome_unknown() {
        let server = MockServer::start().await;
        mount_note_patch(&server, Duration::from_secs(5)).await;
        mount_ledger(&server, Duration::from_secs(5)).await;

        let app = run_note_action(&server, None).await;

        assert_eq!(
            ledger_state(&app),
            "In Progress",
            "the refresh was cancelled"
        );
        assert_eq!(app.notice.kind, NoticeKind::Quiet);
        assert!(
            app.notice.text.contains("unknown"),
            "a cancelled write must say the outcome is unknown, got: {:?}",
            app.notice.text
        );
        assert!(
            !app.notice.text.contains("reflects the current state"),
            "an unrefreshed ledger must not be described as current, got: {:?}",
            app.notice.text
        );
        assert!(
            app.notice.text.contains("refresh was also cancelled"),
            "the notice must say the ledger was not refreshed, got: {:?}",
            app.notice.text
        );
    }

    #[tokio::test]
    async fn a_failed_refresh_after_a_cancelled_write_is_reported_as_an_error() {
        let server = MockServer::start().await;
        mount_note_patch(&server, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": {"message": "Ledger lookup failed"}
            })))
            .mount(&server)
            .await;

        let app = run_note_action(&server, None).await;

        assert_eq!(
            app.notice.kind,
            NoticeKind::Error,
            "a refresh that failed outright is an error, got: {:?}",
            app.notice
        );
        assert!(
            app.notice.text.contains("unknown") && app.notice.text.contains("also failed"),
            "the notice must keep the unknown write outcome and the refresh failure, got: {:?}",
            app.notice.text
        );
    }

    #[tokio::test]
    async fn cancelling_the_incident_reload_after_a_write_skips_the_tab_and_says_so() {
        let server = MockServer::start().await;
        mount_note_patch(&server, Duration::ZERO).await;
        mount_ledger(&server, Duration::ZERO).await;
        Mock::given(method("GET"))
            .and(path(
                "/api/now/table/incident/0123456789abcdef0123456789abcdef",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(serde_json::json!({"result": {}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_journal_field"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": []
            })))
            .expect(0)
            .mount(&server)
            .await;

        let app = run_note_action(&server, Some(IncidentTab::Activity)).await;

        assert_eq!(
            ledger_state(&app),
            "Resolved",
            "the ledger refresh completed"
        );
        assert_ne!(
            app.notice.kind,
            NoticeKind::Success,
            "a bare success notice hides that the incident was not reloaded, got: {:?}",
            app.notice
        );
        assert!(
            app.notice.text.contains("Added a work note to INC0010001."),
            "the write itself completed, got: {:?}",
            app.notice.text
        );
        assert!(
            app.notice.text.contains("reload was cancelled"),
            "the notice must say the incident was not reloaded, got: {:?}",
            app.notice.text
        );
    }
}
