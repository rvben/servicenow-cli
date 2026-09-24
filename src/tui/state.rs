//! Application state: the `App` struct, the overlays and notices layered on
//! top of it, and the key handling and ServiceNow list/detail loading that
//! mutate it. `render.rs` reads this state to draw frames; `actions.rs`
//! reads and mutates it while preparing and executing incident writes.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use ratatui::widgets::TableState;
use serde_json::Value;

use crate::api::{
    ApiError, AttachmentMetadata, DisplayValue, ListOptions, ServiceNowClient, validate_table,
};
use crate::commands::INCIDENT_LIST_FIELDS;
use crate::metadata::TableMetadata;

use super::RELATED_VIEW_LIMIT;
use super::TuiOptions;
use super::actions::{
    IncidentActionForm, IncidentActionKind, IncidentActionMenu, IncidentTarget,
    PreparedIncidentAction,
};
use super::cancel::{RequestOutcome, run_cancellable, wait_for_cancel_key};
use super::format::{
    compact_instance, default_query, display_field_value, field_label, infer_columns,
    record_matches_search, record_sys_id, record_title, safe_text,
};
use super::render::{Theme, activity_lines, attachment_lines, scroll_extent, sla_lines};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) enum Overlay {
    #[default]
    None,
    Detail,
    Help {
        return_to_detail: bool,
    },
    TableInput(String),
    QueryInput(String),
    SearchInput(String),
    IncidentActions(IncidentActionMenu),
    IncidentActionForm(IncidentActionForm),
    IncidentActionReview(PreparedIncidentAction),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum NoticeKind {
    Quiet,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub(super) struct Notice {
    pub(super) kind: NoticeKind,
    pub(super) text: String,
}

impl Notice {
    pub(super) fn quiet(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Quiet,
            text: safe_text(&text.into()),
        }
    }

    pub(super) fn success(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Success,
            text: safe_text(&text.into()),
        }
    }

    pub(super) fn error(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Error,
            text: safe_text(&text.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Action {
    None,
    Quit,
    Authenticate,
    Load,
    LoadDetail,
    LoadIncidentTab(IncidentTab),
    Open,
    OpenIncidentActions { return_to_detail: bool },
    PrepareIncident(IncidentActionForm),
    ExecuteIncident(PreparedIncidentAction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiExit {
    Quit,
    Authenticate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum IncidentTab {
    #[default]
    Overview,
    Activity,
    Attachments,
    Slas,
}

impl IncidentTab {
    pub(super) const ALL: [Self; 4] = [
        Self::Overview,
        Self::Activity,
        Self::Attachments,
        Self::Slas,
    ];

    pub(super) fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub(super) fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub(super) fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Activity => 1,
            Self::Attachments => 2,
            Self::Slas => 3,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Overview => "OVERVIEW",
            Self::Activity => "ACTIVITY",
            Self::Attachments => "ATTACHMENTS",
            Self::Slas => "SLAs",
        }
    }
}

/// How a load request ended. A cancelled load is neither a success nor a
/// failure: the previous state stays on screen, so a caller must not describe
/// it as current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoadOutcome {
    Loaded,
    Failed,
    Cancelled,
}

/// The table, encoded query, and page a set of ledger rows was loaded for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LedgerView {
    table: String,
    query: Option<String>,
    offset: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) enum PanelState<T> {
    #[default]
    Idle,
    Loading,
    Ready {
        items: Vec<T>,
        truncated: bool,
    },
    Failed(String),
}

pub(super) struct App {
    pub(super) profile: String,
    pub(super) instance: String,
    pub(super) table: String,
    pub(super) query: Option<String>,
    pub(super) search: Option<String>,
    pub(super) page_size: usize,
    pub(super) offset: usize,
    pub(super) records: Vec<Value>,
    pub(super) records_view: Option<LedgerView>,
    pub(super) detail_record: Option<Value>,
    pub(super) detail_record_sys_id: Option<String>,
    pub(super) overview_error: Option<String>,
    pub(super) columns: Vec<String>,
    pub(super) table_state: TableState,
    pub(super) detail_scroll: u16,
    pub(super) detail_viewport_width: u16,
    pub(super) detail_viewport_height: u16,
    pub(super) action_review_scroll: u16,
    pub(super) action_review_max_scroll: u16,
    pub(super) action_review_viewport_height: u16,
    pub(super) incident_tab: IncidentTab,
    pub(super) activity: PanelState<Value>,
    pub(super) attachments: PanelState<AttachmentMetadata>,
    pub(super) slas: PanelState<Value>,
    pub(super) incident_metadata: Option<TableMetadata>,
    pub(super) has_next_page: bool,
    pub(super) loading: bool,
    pub(super) detail_loading: bool,
    pub(super) load_failed: bool,
    pub(super) auth_failed: bool,
    pub(super) read_only: bool,
    pub(super) color: bool,
    pub(super) overlay: Overlay,
    pub(super) notice: Notice,
}

impl App {
    pub(super) fn new(profile: &str, instance: &str, read_only: bool, options: TuiOptions) -> Self {
        let query = options
            .query
            .filter(|query| !query.trim().is_empty())
            .or_else(|| default_query(&options.table));
        Self {
            profile: profile.into(),
            instance: compact_instance(instance),
            table: options.table,
            query,
            search: None,
            page_size: options.page_size,
            offset: 0,
            records: Vec::new(),
            records_view: None,
            detail_record: None,
            detail_record_sys_id: None,
            overview_error: None,
            columns: Vec::new(),
            table_state: TableState::default(),
            detail_scroll: 0,
            detail_viewport_width: 40,
            detail_viewport_height: 10,
            action_review_scroll: 0,
            action_review_max_scroll: 0,
            action_review_viewport_height: 1,
            incident_tab: IncidentTab::Overview,
            activity: PanelState::Idle,
            attachments: PanelState::Idle,
            slas: PanelState::Idle,
            incident_metadata: None,
            has_next_page: false,
            loading: false,
            detail_loading: false,
            load_failed: false,
            auth_failed: false,
            read_only,
            color: options.color,
            overlay: Overlay::None,
            notice: Notice::quiet("Preparing the ledger…"),
        }
    }

    /// Marks the ledger as loading so the next frame shows the loading panel
    /// instead of rows fetched for a view the operator has just left.
    fn begin_load(&mut self) {
        self.loading = true;
        self.notice = Notice::quiet(format!("Loading {}… Esc cancels.", self.table));
    }

    pub(super) async fn load(&mut self, client: &ServiceNowClient) -> LoadOutcome {
        self.begin_load();
        let fields = (self.table == "incident").then(|| {
            INCIDENT_LIST_FIELDS
                .iter()
                .map(|field| (*field).into())
                .collect()
        });
        let options = ListOptions {
            query: self.query.clone(),
            fields,
            limit: self.page_size + 1,
            offset: self.offset,
            display_value: DisplayValue::All,
            ..ListOptions::default()
        };
        let outcome = match run_cancellable(
            client.list_records(&self.table, &options),
            wait_for_cancel_key(),
        )
        .await
        {
            RequestOutcome::Completed(Ok(mut records)) => {
                self.has_next_page = records.len() > self.page_size;
                records.truncate(self.page_size);
                self.records = records;
                self.records_view = Some(self.ledger_view());
                self.clear_detail_record();
                self.columns = infer_columns(&self.records, &self.table);
                let visible_records = self.visible_record_count();
                let selected = (visible_records > 0).then_some(
                    self.table_state
                        .selected()
                        .unwrap_or(0)
                        .min(visible_records.saturating_sub(1)),
                );
                self.table_state.select(selected);
                self.detail_scroll = 0;
                self.load_failed = false;
                self.auth_failed = false;
                self.notice = if self.records.is_empty() {
                    Notice::quiet("No records match this view. Press / to change the query.")
                } else if self.search.is_some() {
                    self.search_notice()
                } else {
                    Notice::success(format!(
                        "Loaded {} record{}",
                        self.records.len(),
                        if self.records.len() == 1 { "" } else { "s" }
                    ))
                };
                LoadOutcome::Loaded
            }
            RequestOutcome::Completed(Err(error)) => {
                self.records.clear();
                self.records_view = Some(self.ledger_view());
                self.columns.clear();
                self.table_state.select(None);
                self.has_next_page = false;
                self.load_failed = true;
                self.auth_failed = matches!(error, ApiError::Auth(_));
                self.notice = if self.auth_failed {
                    Notice::error(format!("{error}. Press Enter or a to sign in again."))
                } else {
                    Notice::error(format!("{error}. Press r to retry."))
                };
                LoadOutcome::Failed
            }
            RequestOutcome::Cancelled => {
                // A table, query, or page change selects the new view before
                // loading it, so the rows on screen would otherwise sit under
                // a view they were not loaded for.
                if self.records_view.as_ref() != Some(&self.ledger_view()) {
                    self.records.clear();
                    self.records_view = None;
                    self.columns.clear();
                    self.clear_detail_record();
                    self.table_state.select(None);
                    self.has_next_page = false;
                }
                self.notice = Notice::quiet("Cancelled. Press r to retry.");
                LoadOutcome::Cancelled
            }
        };
        self.loading = false;
        outcome
    }

    pub(super) async fn load_detail(&mut self, client: &ServiceNowClient) -> LoadOutcome {
        let Some(sys_id) = self
            .selected_record()
            .and_then(record_sys_id)
            .map(str::to_string)
        else {
            self.notice = Notice::error("This record has no usable sys_id.");
            return LoadOutcome::Failed;
        };
        self.detail_loading = true;
        self.overview_error = None;
        self.notice = Notice::quiet("Reading the complete record sheet… Esc cancels.");
        let outcome = match run_cancellable(
            client.get_record(&self.table, &sys_id, None, DisplayValue::All),
            wait_for_cancel_key(),
        )
        .await
        {
            RequestOutcome::Completed(Ok(record)) => {
                self.detail_record = Some(record);
                self.detail_record_sys_id = Some(sys_id);
                self.overview_error = None;
                self.notice = Notice::success("Complete record loaded.");
                LoadOutcome::Loaded
            }
            RequestOutcome::Completed(Err(error)) => {
                self.detail_record = None;
                self.detail_record_sys_id = None;
                let message = format!("Could not load the complete record: {error}");
                self.overview_error = Some(message.clone());
                self.notice = Notice::error(format!("{message}. Showing index fields only."));
                LoadOutcome::Failed
            }
            RequestOutcome::Cancelled => {
                self.detail_record = None;
                self.detail_record_sys_id = None;
                self.overview_error = None;
                self.notice = Notice::quiet("Cancelled. The complete record was not loaded.");
                LoadOutcome::Cancelled
            }
        };
        self.detail_loading = false;
        outcome
    }

    pub(super) async fn load_incident_tab(
        &mut self,
        client: &ServiceNowClient,
        tab: IncidentTab,
    ) -> LoadOutcome {
        if tab == IncidentTab::Overview {
            return self.load_detail(client).await;
        }
        let Some(sys_id) = self
            .selected_record()
            .and_then(record_sys_id)
            .map(str::to_string)
        else {
            let message = "This incident has no usable sys_id.".to_string();
            match tab {
                IncidentTab::Overview => {}
                IncidentTab::Activity => self.activity = PanelState::Failed(message.clone()),
                IncidentTab::Attachments => {
                    self.attachments = PanelState::Failed(message.clone());
                }
                IncidentTab::Slas => self.slas = PanelState::Failed(message.clone()),
            }
            self.notice = Notice::error(message);
            return LoadOutcome::Failed;
        };
        match tab {
            IncidentTab::Overview => unreachable!(),
            IncidentTab::Activity => {
                let options = ListOptions {
                    query: Some(format!(
                        "name=incident^element_id={sys_id}^elementINcomments,work_notes^ORDERBYDESCsys_created_on"
                    )),
                    fields: Some(
                        [
                            "element",
                            "value",
                            "sys_created_by",
                            "sys_created_on",
                            "sys_id",
                        ]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    ),
                    limit: RELATED_VIEW_LIMIT + 1,
                    display_value: DisplayValue::All,
                    ..ListOptions::default()
                };
                match run_cancellable(
                    client.list_records("sys_journal_field", &options),
                    wait_for_cancel_key(),
                )
                .await
                {
                    RequestOutcome::Completed(Ok(entries)) => {
                        self.activity = bounded_panel(entries);
                        let count = panel_count(&self.activity).unwrap_or(0);
                        self.notice = related_loaded_notice(
                            count,
                            panel_truncated(&self.activity),
                            "activity entry",
                            "activity entries",
                        );
                        LoadOutcome::Loaded
                    }
                    RequestOutcome::Completed(Err(error)) => {
                        let message = format!("Could not load incident activity: {error}");
                        self.activity = PanelState::Failed(message.clone());
                        self.notice = Notice::error(message);
                        LoadOutcome::Failed
                    }
                    RequestOutcome::Cancelled => {
                        self.activity = PanelState::Idle;
                        self.notice = Notice::quiet("Cancelled. Activity was not loaded.");
                        LoadOutcome::Cancelled
                    }
                }
            }
            IncidentTab::Attachments => {
                match run_cancellable(
                    client.list_attachments("incident", &sys_id, RELATED_VIEW_LIMIT + 1, false),
                    wait_for_cancel_key(),
                )
                .await
                {
                    RequestOutcome::Completed(Ok(attachments)) => {
                        self.attachments = bounded_panel(attachments);
                        let count = panel_count(&self.attachments).unwrap_or(0);
                        self.notice = related_loaded_notice(
                            count,
                            panel_truncated(&self.attachments),
                            "attachment",
                            "attachments",
                        );
                        LoadOutcome::Loaded
                    }
                    RequestOutcome::Completed(Err(error)) => {
                        let message = format!("Could not load incident attachments: {error}");
                        self.attachments = PanelState::Failed(message.clone());
                        self.notice = Notice::error(message);
                        LoadOutcome::Failed
                    }
                    RequestOutcome::Cancelled => {
                        self.attachments = PanelState::Idle;
                        self.notice = Notice::quiet("Cancelled. Attachments were not loaded.");
                        LoadOutcome::Cancelled
                    }
                }
            }
            IncidentTab::Slas => {
                let options = ListOptions {
                    query: Some(format!("task={sys_id}^ORDERBYDESCsys_created_on")),
                    fields: Some(
                        [
                            "sla",
                            "stage",
                            "has_breached",
                            "percentage",
                            "business_percentage",
                            "start_time",
                            "planned_end_time",
                            "end_time",
                            "duration",
                            "pause_duration",
                            "sys_id",
                        ]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    ),
                    limit: RELATED_VIEW_LIMIT + 1,
                    display_value: DisplayValue::All,
                    ..ListOptions::default()
                };
                match run_cancellable(
                    client.list_records("task_sla", &options),
                    wait_for_cancel_key(),
                )
                .await
                {
                    RequestOutcome::Completed(Ok(slas)) => {
                        self.slas = bounded_panel(slas);
                        let count = panel_count(&self.slas).unwrap_or(0);
                        self.notice = related_loaded_notice(
                            count,
                            panel_truncated(&self.slas),
                            "SLA",
                            "SLAs",
                        );
                        LoadOutcome::Loaded
                    }
                    RequestOutcome::Completed(Err(error)) => {
                        let message = format!("Could not load incident SLAs: {error}");
                        self.slas = PanelState::Failed(message.clone());
                        self.notice = Notice::error(message);
                        LoadOutcome::Failed
                    }
                    RequestOutcome::Cancelled => {
                        self.slas = PanelState::Idle;
                        self.notice = Notice::quiet("Cancelled. SLAs were not loaded.");
                        LoadOutcome::Cancelled
                    }
                }
            }
        }
    }

    pub(super) fn selected_record(&self) -> Option<&Value> {
        let visible_index = self.table_state.selected()?;
        let record_index = self.matching_record_indices().nth(visible_index)?;
        self.records.get(record_index)
    }

    pub(super) fn matching_record_indices(&self) -> impl Iterator<Item = usize> + '_ {
        let search = self.search.as_deref();
        self.records
            .iter()
            .enumerate()
            .filter_map(move |(index, record)| match search {
                Some(search) if !record_matches_search(record, search) => None,
                _ => Some(index),
            })
    }

    fn visible_record_count(&self) -> usize {
        self.matching_record_indices().count()
    }

    fn search_notice(&self) -> Notice {
        let matches = self.visible_record_count();
        let loaded = self.records.len();
        if matches == 0 {
            Notice::quiet(format!(
                "No loaded records contain '{}'. Press s to change or clear the search.",
                self.search.as_deref().unwrap_or_default()
            ))
        } else {
            Notice::success(format!(
                "{matches} of {loaded} loaded record{} match{}",
                if loaded == 1 { "" } else { "s" },
                if matches == 1 { "es" } else { "" }
            ))
        }
    }

    pub(super) fn apply_search(&mut self, value: &str) {
        let value = value.trim();
        self.search = (!value.is_empty()).then(|| value.to_string());
        self.table_state
            .select((self.visible_record_count() > 0).then_some(0));
        self.detail_scroll = 0;
        self.clear_detail_record();
        self.notice = if self.search.is_some() {
            self.search_notice()
        } else if self.records.is_empty() {
            Notice::quiet("No records match this view. Press / to change the query.")
        } else {
            Notice::success(format!(
                "Showing all {} loaded record{}",
                self.records.len(),
                if self.records.len() == 1 { "" } else { "s" }
            ))
        };
    }

    fn select_next(&mut self) {
        let visible_records = self.visible_record_count();
        if visible_records == 0 {
            return;
        }
        let next = self
            .table_state
            .selected()
            .map_or(0, |index| (index + 1).min(visible_records - 1));
        self.table_state.select(Some(next));
        self.detail_scroll = 0;
        self.clear_detail_record();
    }

    fn select_previous(&mut self) {
        if self.visible_record_count() == 0 {
            return;
        }
        let previous = self.table_state.selected().unwrap_or(0).saturating_sub(1);
        self.table_state.select(Some(previous));
        self.detail_scroll = 0;
        self.clear_detail_record();
    }

    pub(super) fn open_incident_actions(&mut self, return_to_detail: bool) {
        if self.table != "incident" {
            self.notice = Notice::quiet("Focused actions are available for incidents only.");
            return;
        }
        let Some(record) = self.selected_record() else {
            self.notice = Notice::error("Select an incident before opening actions.");
            return;
        };
        let Some(sys_id) = record_sys_id(record).map(str::to_string) else {
            self.notice = Notice::error("This incident has no usable sys_id.");
            return;
        };
        let target = IncidentTarget {
            sys_id,
            title: record_title(record),
            return_tab: return_to_detail.then_some(self.incident_tab),
        };
        self.overlay = Overlay::IncidentActions(IncidentActionMenu {
            target,
            selected: 0,
        });
        self.notice = if self.read_only {
            Notice::quiet(format!(
                "Profile '{}' is read-only; incident actions are unavailable.",
                self.profile
            ))
        } else {
            Notice::quiet("Choose a guarded incident action.")
        };
    }

    /// Applies a key press. A key that changes the table, query, or page
    /// updates the view immediately, so it also starts the load here: the
    /// frame drawn before the request runs must not pair the new view's
    /// header with the old view's rows.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Action {
        let action = self.apply_key(key);
        if action == Action::Load {
            self.begin_load();
        }
        action
    }

    fn apply_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        match &mut self.overlay {
            Overlay::TableInput(buffer)
            | Overlay::QueryInput(buffer)
            | Overlay::SearchInput(buffer) => match key.code {
                KeyCode::Esc => {
                    self.overlay = Overlay::None;
                    Action::None
                }
                KeyCode::Enter => self.commit_input(),
                KeyCode::Backspace => {
                    buffer.pop();
                    Action::None
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !character.is_control() =>
                {
                    buffer.push(character);
                    Action::None
                }
                _ => Action::None,
            },
            Overlay::IncidentActions(menu) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.overlay = if menu.target.return_tab.is_some() {
                        Overlay::Detail
                    } else {
                        Overlay::None
                    };
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    menu.selected = (menu.selected + 1) % IncidentActionKind::ALL.len();
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    menu.selected = (menu.selected + IncidentActionKind::ALL.len() - 1)
                        % IncidentActionKind::ALL.len();
                    Action::None
                }
                KeyCode::Char('1' | '2' | '3') => {
                    let selected = match key.code {
                        KeyCode::Char('1') => 0,
                        KeyCode::Char('2') => 1,
                        KeyCode::Char('3') => 2,
                        _ => unreachable!(),
                    };
                    menu.selected = selected;
                    if self.read_only {
                        self.notice = Notice::error(format!(
                            "Write operation blocked: profile '{}' is read-only.",
                            self.profile
                        ));
                        Action::None
                    } else {
                        self.notice = Notice::quiet(format!(
                            "Enter details for {}; nothing changes until review.",
                            IncidentActionKind::ALL[selected]
                                .label()
                                .to_ascii_lowercase()
                        ));
                        self.overlay = Overlay::IncidentActionForm(IncidentActionForm::new(
                            IncidentActionKind::ALL[selected],
                            menu.target.clone(),
                        ));
                        Action::None
                    }
                }
                KeyCode::Enter => {
                    if self.read_only {
                        self.notice = Notice::error(format!(
                            "Write operation blocked: profile '{}' is read-only.",
                            self.profile
                        ));
                        Action::None
                    } else {
                        self.notice = Notice::quiet(format!(
                            "Enter details for {}; nothing changes until review.",
                            IncidentActionKind::ALL[menu.selected]
                                .label()
                                .to_ascii_lowercase()
                        ));
                        self.overlay = Overlay::IncidentActionForm(IncidentActionForm::new(
                            IncidentActionKind::ALL[menu.selected],
                            menu.target.clone(),
                        ));
                        Action::None
                    }
                }
                _ => Action::None,
            },
            Overlay::IncidentActionForm(form) => match key.code {
                KeyCode::Esc => {
                    self.overlay = Overlay::IncidentActions(IncidentActionMenu {
                        target: form.target.clone(),
                        selected: form.kind.index(),
                    });
                    Action::None
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.focused = (form.focused + 1) % form.fields.len();
                    Action::None
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.focused = (form.focused + form.fields.len() - 1) % form.fields.len();
                    Action::None
                }
                KeyCode::Enter
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && form.focused_field_mut().multiline =>
                {
                    form.focused_field_mut().value.push('\n');
                    Action::None
                }
                KeyCode::Enter if form.focused + 1 < form.fields.len() => {
                    form.focused += 1;
                    Action::None
                }
                KeyCode::Enter => Action::PrepareIncident(form.clone()),
                KeyCode::Backspace => {
                    form.focused_field_mut().value.pop();
                    Action::None
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.focused_field_mut().value.clear();
                    Action::None
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !character.is_control() =>
                {
                    form.focused_field_mut().value.push(character);
                    Action::None
                }
                _ => Action::None,
            },
            Overlay::IncidentActionReview(prepared) => match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    self.action_review_scroll = 0;
                    self.overlay = Overlay::IncidentActionForm(prepared.form.clone());
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.action_review_scroll = self
                        .action_review_scroll
                        .saturating_add(1)
                        .min(self.action_review_max_scroll);
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.action_review_scroll = self.action_review_scroll.saturating_sub(1);
                    Action::None
                }
                KeyCode::PageDown => {
                    self.action_review_scroll = self
                        .action_review_scroll
                        .saturating_add(self.action_review_viewport_height)
                        .min(self.action_review_max_scroll);
                    Action::None
                }
                KeyCode::PageUp => {
                    self.action_review_scroll = self
                        .action_review_scroll
                        .saturating_sub(self.action_review_viewport_height);
                    Action::None
                }
                KeyCode::Char('q') => {
                    self.action_review_scroll = 0;
                    self.overlay = if prepared.form.target.return_tab.is_some() {
                        Overlay::Detail
                    } else {
                        Overlay::None
                    };
                    self.notice = Notice::quiet("Incident action cancelled; nothing was changed.");
                    Action::None
                }
                KeyCode::Enter => {
                    if self.read_only {
                        self.notice = Notice::error(format!(
                            "Write operation blocked: profile '{}' is read-only.",
                            self.profile
                        ));
                        Action::None
                    } else {
                        Action::ExecuteIncident(prepared.clone())
                    }
                }
                _ => Action::None,
            },
            Overlay::Help { return_to_detail } => match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.overlay = if *return_to_detail {
                        Overlay::Detail
                    } else {
                        Overlay::None
                    };
                    Action::None
                }
                _ => Action::None,
            },
            Overlay::Detail => match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    self.overlay = Overlay::None;
                    self.detail_scroll = 0;
                    Action::None
                }
                KeyCode::Tab if self.table == "incident" => {
                    self.select_incident_tab(self.incident_tab.next(), false)
                }
                KeyCode::BackTab if self.table == "incident" => {
                    self.select_incident_tab(self.incident_tab.previous(), false)
                }
                KeyCode::Char('1') if self.table == "incident" => {
                    self.select_incident_tab(IncidentTab::Overview, false)
                }
                KeyCode::Char('2') if self.table == "incident" => {
                    self.select_incident_tab(IncidentTab::Activity, false)
                }
                KeyCode::Char('3') if self.table == "incident" => {
                    self.select_incident_tab(IncidentTab::Attachments, false)
                }
                KeyCode::Char('4') if self.table == "incident" => {
                    self.select_incident_tab(IncidentTab::Slas, false)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.detail_scroll = self
                        .detail_scroll
                        .saturating_add(1)
                        .min(self.detail_max_scroll());
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                    Action::None
                }
                KeyCode::PageDown => {
                    self.detail_scroll = self
                        .detail_scroll
                        .saturating_add(8)
                        .min(self.detail_max_scroll());
                    Action::None
                }
                KeyCode::PageUp => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(8);
                    Action::None
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.detail_scroll = 0;
                    Action::None
                }
                KeyCode::End | KeyCode::Char('G') => {
                    self.detail_scroll = self.detail_max_scroll();
                    Action::None
                }
                KeyCode::Char('?') => {
                    self.overlay = Overlay::Help {
                        return_to_detail: true,
                    };
                    Action::None
                }
                KeyCode::Char('r') if self.table == "incident" => {
                    self.select_incident_tab(self.incident_tab, true)
                }
                KeyCode::Char('a') if self.table == "incident" => Action::OpenIncidentActions {
                    return_to_detail: true,
                },
                KeyCode::Char('o') => Action::Open,
                KeyCode::Char('q') => Action::Quit,
                _ => Action::None,
            },
            Overlay::None => match key.code {
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Enter | KeyCode::Char('a') if self.load_failed && self.auth_failed => {
                    Action::Authenticate
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.select_next();
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.select_previous();
                    Action::None
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.table_state
                        .select((self.visible_record_count() > 0).then_some(0));
                    self.detail_scroll = 0;
                    self.clear_detail_record();
                    Action::None
                }
                KeyCode::End | KeyCode::Char('G') => {
                    let visible_records = self.visible_record_count();
                    self.table_state
                        .select((visible_records > 0).then_some(visible_records.saturating_sub(1)));
                    self.detail_scroll = 0;
                    self.clear_detail_record();
                    Action::None
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')
                    if self.selected_record().is_some() =>
                {
                    self.overlay = Overlay::Detail;
                    if self.detail_record_matches_selection() {
                        Action::None
                    } else {
                        self.detail_loading = true;
                        self.notice =
                            Notice::quiet("Reading the complete record sheet… Esc cancels.");
                        Action::LoadDetail
                    }
                }
                KeyCode::Char('r') => Action::Load,
                KeyCode::Char('n') | KeyCode::PageDown if self.has_next_page => {
                    self.offset += self.page_size;
                    self.table_state.select(Some(0));
                    Action::Load
                }
                KeyCode::Char('p') | KeyCode::PageUp if self.offset > 0 => {
                    self.offset = self.offset.saturating_sub(self.page_size);
                    self.table_state.select(Some(0));
                    Action::Load
                }
                KeyCode::Char('t') => {
                    self.overlay = Overlay::TableInput(String::new());
                    Action::None
                }
                KeyCode::Char('/') => {
                    self.overlay = Overlay::QueryInput(self.query.clone().unwrap_or_default());
                    Action::None
                }
                KeyCode::Char('s') => {
                    self.overlay = Overlay::SearchInput(self.search.clone().unwrap_or_default());
                    Action::None
                }
                KeyCode::Char('a')
                    if self.table == "incident" && self.selected_record().is_some() =>
                {
                    Action::OpenIncidentActions {
                        return_to_detail: false,
                    }
                }
                KeyCode::Char('?') => {
                    self.overlay = Overlay::Help {
                        return_to_detail: false,
                    };
                    Action::None
                }
                KeyCode::Char('o') if self.selected_record().is_some() => Action::Open,
                _ => Action::None,
            },
        }
    }

    fn commit_input(&mut self) -> Action {
        match std::mem::take(&mut self.overlay) {
            Overlay::TableInput(value) => {
                let table = value.trim();
                if table.is_empty() {
                    self.notice = Notice::error("Enter a table name, such as incident or cmdb_ci.");
                    return Action::None;
                }
                if let Err(error) = validate_table(table) {
                    self.notice = Notice::error(error.to_string());
                    return Action::None;
                }
                self.table = table.into();
                self.query = default_query(&self.table);
                self.search = None;
                self.offset = 0;
                self.table_state.select(Some(0));
                Action::Load
            }
            Overlay::SearchInput(value) => {
                self.apply_search(&value);
                Action::None
            }
            Overlay::QueryInput(value) => {
                self.query = (!value.trim().is_empty()).then(|| value.trim().into());
                self.offset = 0;
                self.table_state.select(Some(0));
                Action::Load
            }
            overlay => {
                self.overlay = overlay;
                Action::None
            }
        }
    }

    pub(super) fn ledger_view(&self) -> LedgerView {
        LedgerView {
            table: self.table.clone(),
            query: self.query.clone(),
            offset: self.offset,
        }
    }

    fn clear_detail_record(&mut self) {
        self.detail_record = None;
        self.detail_record_sys_id = None;
        self.overview_error = None;
        self.incident_tab = IncidentTab::Overview;
        self.activity = PanelState::Idle;
        self.attachments = PanelState::Idle;
        self.slas = PanelState::Idle;
    }

    fn select_incident_tab(&mut self, tab: IncidentTab, force: bool) -> Action {
        self.incident_tab = tab;
        self.detail_scroll = 0;
        let needs_load = force
            || match tab {
                IncidentTab::Overview => !self.detail_record_matches_selection(),
                IncidentTab::Activity => !matches!(&self.activity, PanelState::Ready { .. }),
                IncidentTab::Attachments => !matches!(&self.attachments, PanelState::Ready { .. }),
                IncidentTab::Slas => !matches!(&self.slas, PanelState::Ready { .. }),
            };
        if !needs_load {
            return Action::None;
        }
        match tab {
            IncidentTab::Overview => {
                self.detail_loading = true;
                self.notice = Notice::quiet("Reading the complete record sheet… Esc cancels.");
            }
            IncidentTab::Activity => {
                self.activity = PanelState::Loading;
                self.notice = Notice::quiet("Reading comments and work notes… Esc cancels.");
            }
            IncidentTab::Attachments => {
                self.attachments = PanelState::Loading;
                self.notice = Notice::quiet("Reading incident attachments… Esc cancels.");
            }
            IncidentTab::Slas => {
                self.slas = PanelState::Loading;
                self.notice = Notice::quiet("Reading incident SLAs… Esc cancels.");
            }
        }
        Action::LoadIncidentTab(tab)
    }

    pub(super) fn detail_record_matches_selection(&self) -> bool {
        self.selected_record()
            .and_then(record_sys_id)
            .zip(self.detail_record_sys_id.as_deref())
            .is_some_and(|(selected, loaded)| selected == loaded)
    }

    fn detail_max_scroll(&self) -> u16 {
        let width = usize::from(self.detail_viewport_width.max(1));
        let height = usize::from(self.detail_viewport_height.max(1));
        if self.table == "incident" {
            let lines = match self.incident_tab {
                IncidentTab::Overview => self
                    .detail_record
                    .as_ref()
                    .filter(|_| self.detail_record_matches_selection())
                    .or_else(|| self.selected_record())
                    .map(|record| self.overview_lines(record, Theme::new(false)))
                    .unwrap_or_default(),
                IncidentTab::Activity => activity_lines(&self.activity, Theme::new(false)),
                IncidentTab::Attachments => attachment_lines(&self.attachments, Theme::new(false)),
                IncidentTab::Slas => sla_lines(&self.slas, Theme::new(false)),
            };
            return scroll_extent(&lines, width, height);
        }
        let record = if self.detail_record_matches_selection() {
            self.detail_record.as_ref()
        } else {
            self.selected_record()
        };
        let Some(object) = record.and_then(Value::as_object) else {
            return 0;
        };
        let mut lines = Vec::new();
        for (field, value) in object {
            lines.push(Line::raw(format!(
                "{}  {}",
                field_label(field),
                display_field_value(field, value)
            )));
        }
        scroll_extent(&lines, width, height)
    }

    pub(super) fn open_selected(&mut self, client: &ServiceNowClient) {
        let Some(sys_id) = self.selected_record().and_then(record_sys_id) else {
            self.notice = Notice::error("This record has no usable sys_id.");
            return;
        };
        let url = client.record_url(&self.table, sys_id);
        match open::that(&url) {
            Ok(()) => self.notice = Notice::success("Opened the selected record in ServiceNow."),
            Err(error) => {
                self.notice = Notice::error(format!("Could not open the browser: {error}"))
            }
        }
    }
}

pub(super) fn panel_count<T>(state: &PanelState<T>) -> Option<usize> {
    match state {
        PanelState::Ready { items, .. } => Some(items.len()),
        _ => None,
    }
}

pub(super) fn panel_truncated<T>(state: &PanelState<T>) -> bool {
    matches!(
        state,
        PanelState::Ready {
            truncated: true,
            ..
        }
    )
}

pub(super) fn panel_count_label<T>(state: &PanelState<T>) -> Option<String> {
    panel_count(state).map(|count| {
        if panel_truncated(state) {
            format!("{count}+")
        } else {
            count.to_string()
        }
    })
}

pub(super) fn bounded_panel<T>(mut items: Vec<T>) -> PanelState<T> {
    let truncated = items.len() > RELATED_VIEW_LIMIT;
    items.truncate(RELATED_VIEW_LIMIT);
    PanelState::Ready { items, truncated }
}

fn related_loaded_notice(count: usize, truncated: bool, singular: &str, plural: &str) -> Notice {
    if truncated {
        Notice::success(format!(
            "Loaded the latest {count} {plural}; more are available."
        ))
    } else {
        Notice::success(format!(
            "Loaded {count} {}.",
            if count == 1 { singular } else { plural }
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::AuthType;

    use super::super::DEFAULT_INCIDENT_QUERY;
    use super::super::cancel::arm_test_cancel;
    use super::super::test_support::{app, rendered_text};
    use super::*;

    async fn cancel_a_slow_load(app: &mut App) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(serde_json::json!({"result": []})),
            )
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let _cancel = arm_test_cancel(Duration::from_millis(50));
        let outcome = app.load(&client).await;
        assert_eq!(outcome, LoadOutcome::Cancelled);
    }

    #[tokio::test]
    async fn cancelling_a_refresh_of_the_same_view_keeps_its_rows() {
        let mut app = app();

        cancel_a_slow_load(&mut app).await;

        assert_eq!(app.records.len(), 1);
        assert_eq!(app.table_state.selected(), Some(0));
        assert!(app.selected_record().is_some());
    }

    #[tokio::test]
    async fn cancelling_a_load_for_a_new_view_drops_rows_from_the_old_one() {
        for change in ["table", "query", "page"] {
            let mut app = app();
            let action = match change {
                "table" => {
                    app.overlay = Overlay::TableInput("cmdb_ci".into());
                    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                }
                "query" => {
                    app.overlay = Overlay::QueryInput("active=false".into());
                    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                }
                _ => {
                    app.has_next_page = true;
                    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
                }
            };
            assert_eq!(action, Action::Load, "{change}");

            cancel_a_slow_load(&mut app).await;

            assert!(
                app.records.is_empty(),
                "rows loaded for the previous {change} must not be shown under the new one"
            );
            assert!(app.selected_record().is_none(), "{change}");
            assert!(!app.has_next_page, "{change}");
            assert!(app.notice.text.contains("Cancelled"), "{change}");
        }
    }

    #[test]
    fn the_frame_drawn_before_a_load_shows_the_loading_panel_not_the_old_rows() {
        for change in ["table", "query", "page", "refresh"] {
            let mut app = app();
            let action = match change {
                "table" => {
                    app.overlay = Overlay::TableInput("cmdb_ci".into());
                    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                }
                "query" => {
                    app.overlay = Overlay::QueryInput("active=false".into());
                    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                }
                "page" => {
                    app.has_next_page = true;
                    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
                }
                _ => app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            };
            assert_eq!(action, Action::Load, "{change}");

            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            let text = rendered_text(terminal.backend().buffer());

            assert!(text.contains("INDEXING RECORDS"), "{change}:\n{text}");
            assert!(text.contains("Esc cancels"), "{change}:\n{text}");
            assert!(!text.contains("INC0010001"), "{change}:\n{text}");
        }
    }

    #[test]
    fn keys_that_keep_the_view_do_not_start_a_load() {
        let mut app = app();
        let action = app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        assert_ne!(action, Action::Load);
        assert!(!app.loading);
        assert_eq!(app.notice.text, "Loaded 1 record");
    }

    #[tokio::test]
    async fn cancelling_a_read_returns_promptly_with_a_cancelled_notice() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(serde_json::json!({"result": []})),
            )
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let mut app = app();
        let _cancel = arm_test_cancel(Duration::from_millis(50));

        let started = std::time::Instant::now();
        app.load(&client).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "cancellation should return well before the mocked 5s delay, took {elapsed:?}"
        );
        assert_eq!(app.notice.kind, NoticeKind::Quiet);
        assert!(app.notice.text.contains("Cancelled"));
    }

    #[test]
    fn incidents_open_on_active_work_assigned_to_the_user_or_their_groups() {
        let app = App::new(
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

        assert_eq!(app.query.as_deref(), Some(DEFAULT_INCIDENT_QUERY));
    }

    #[test]
    fn explicit_and_non_incident_views_do_not_inherit_the_incident_default() {
        let explicit = App::new(
            "work",
            "https://dev12345.service-now.com",
            false,
            TuiOptions {
                table: "incident".into(),
                query: Some("priority=1^ORDERBYDESCnumber".into()),
                page_size: 25,
                color: true,
            },
        );
        let generic = App::new(
            "work",
            "https://dev12345.service-now.com",
            false,
            TuiOptions {
                table: "cmdb_ci".into(),
                query: None,
                page_size: 25,
                color: true,
            },
        );

        assert_eq!(
            explicit.query.as_deref(),
            Some("priority=1^ORDERBYDESCnumber")
        );
        assert_eq!(generic.query, None);
    }

    #[tokio::test]
    async fn authentication_failures_offer_sign_in_as_the_primary_recovery() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/incident"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "User is not authenticated", "detail": "Session expired"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            ServiceNowClient::new(&server.uri(), Some("admin"), "expired", AuthType::Basic)
                .unwrap();
        let mut app = app();
        app.load(&client).await;

        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("SIGN-IN REQUIRED"));
        assert!(text.contains("enter / a"));
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Authenticate
        );
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Action::Authenticate
        );
    }

    #[test]
    fn local_search_filters_selection_and_blank_input_restores_the_page() {
        let mut app = app();
        app.records.push(serde_json::json!({
            "sys_id": "fedcba9876543210fedcba9876543210",
            "number": "INC0010002",
            "short_description": "VPN unavailable",
            "assigned_to": {"value": "def", "display_value": "Grace Hopper"}
        }));

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            Action::None
        );
        assert!(matches!(app.overlay, Overlay::SearchInput(_)));
        if let Overlay::SearchInput(buffer) = &mut app.overlay {
            buffer.push_str("grace vpn");
        }
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(app.visible_record_count(), 1);
        assert_eq!(app.table_state.selected(), Some(0));
        assert_eq!(
            app.selected_record().map(record_title).as_deref(),
            Some("INC0010002")
        );

        app.overlay = Overlay::QueryInput("active=true".into());
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Load
        );
        assert_eq!(app.search.as_deref(), Some("grace vpn"));

        app.overlay = Overlay::SearchInput(String::new());
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(app.search, None);
        assert_eq!(app.visible_record_count(), 2);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn persisted_search_can_be_cleared_when_service_now_returns_no_records() {
        let mut app = app();
        app.apply_search("mail");
        app.records.clear();
        app.table_state.select(None);

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            Action::None
        );
        let Overlay::SearchInput(buffer) = &mut app.overlay else {
            panic!("expected search input overlay");
        };
        assert_eq!(buffer, "mail");
        buffer.clear();

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(app.search, None);
        assert_eq!(app.visible_record_count(), 0);
        assert!(app.notice.text.contains("No records match this view"));
    }

    #[test]
    fn keyboard_navigation_and_inputs_update_state() {
        let mut app = app();
        app.records.push(serde_json::json!({
            "sys_id": "fedcba9876543210fedcba9876543210",
            "number": "INC0010002"
        }));
        assert_eq!(app.table_state.selected(), Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.table_state.selected(), Some(1));
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(matches!(app.overlay, Overlay::QueryInput(_)));
        if let Overlay::QueryInput(buffer) = &mut app.overlay {
            buffer.clear();
            buffer.push_str("active=true");
        }
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Load
        );
        assert_eq!(app.query.as_deref(), Some("active=true"));
        assert_eq!(app.offset, 0);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::LoadDetail
        );
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
    }

    #[tokio::test]
    async fn related_incident_tabs_query_the_expected_service_now_resources() {
        let server = MockServer::start().await;
        let sys_id = "0123456789abcdef0123456789abcdef";
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_journal_field"))
            .and(query_param("sysparm_limit", "101"))
            .and(query_param(
                "sysparm_query",
                format!(
                    "name=incident^element_id={sys_id}^elementINcomments,work_notes^ORDERBYDESCsys_created_on"
                ),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{"element": "comments", "value": "Restored"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/attachment"))
            .and(query_param("sysparm_limit", "101"))
            .and(query_param(
                "sysparm_query",
                format!("table_name=incident^table_sys_id={sys_id}^ORDERBYDESCsys_created_on"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{"sys_id": "fedcba9876543210fedcba9876543210", "file_name": "trace.txt"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/task_sla"))
            .and(query_param("sysparm_limit", "101"))
            .and(query_param(
                "sysparm_query",
                format!("task={sys_id}^ORDERBYDESCsys_created_on"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{"sla": "P1 resolution", "has_breached": "false"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            ServiceNowClient::new(&server.uri(), Some("api-user"), "secret", AuthType::Basic)
                .unwrap();
        let mut app = app();
        app.load_incident_tab(&client, IncidentTab::Activity).await;
        app.load_incident_tab(&client, IncidentTab::Attachments)
            .await;
        app.load_incident_tab(&client, IncidentTab::Slas).await;

        assert_eq!(panel_count(&app.activity), Some(1));
        assert_eq!(panel_count(&app.attachments), Some(1));
        assert_eq!(panel_count(&app.slas), Some(1));
    }
}
