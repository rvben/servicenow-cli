// THESIS: A live operations ledger makes arbitrary ServiceNow records feel located, inspectable, and calm; it refuses the miniature-web-dashboard default.
// OWN-WORLD: Midnight ink, mint indexing, amber attention, hairline rules, dense rows, and one clearly punched active mark.
// STORY: See the active instance and query, scan records, unfold incidents into overview, activity, file, and SLA evidence, then return to the shell with context intact.
// FIRST VIEWPORT: Identity and location span the top; a dominant record ledger and persistent detail sheet share the field; incident inspection expands into a four-view workspace.
// FORM: Operations ledger, seventh grounded direction; seed 221c1ea6. Signature interaction: the selected ledger row unfolds into a progressively loaded incident workspace.
// FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

use std::collections::BTreeSet;
use std::io::{self, IsTerminal};
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use serde_json::Value;

use crate::api::{
    ApiError, AttachmentMetadata, DisplayValue, ListOptions, ServiceNowClient, validate_table,
};
use crate::attachment::human_size;
use crate::commands::INCIDENT_LIST_FIELDS;
use crate::config::Config;

const MIN_PAGE_SIZE: usize = 5;
const MAX_PAGE_SIZE: usize = 200;
const RELATED_VIEW_LIMIT: usize = 100;
const DEFAULT_INCIDENT_QUERY: &str = "active=true^assigned_to=javascript:gs.getUserID()^ORassignment_group=javascript:getMyGroups()^ORDERBYDESCsys_updated_on";

#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub table: String,
    pub query: Option<String>,
    pub page_size: usize,
    pub color: bool,
}

impl TuiOptions {
    pub fn validate(&self) -> Result<(), ApiError> {
        validate_table(&self.table)?;
        if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&self.page_size) {
            return Err(ApiError::InvalidInput(format!(
                "TUI page size must be between {MIN_PAGE_SIZE} and {MAX_PAGE_SIZE}"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Overlay {
    #[default]
    None,
    Detail,
    Help {
        return_to_detail: bool,
    },
    TableInput(String),
    QueryInput(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NoticeKind {
    Quiet,
    Success,
    Error,
}

#[derive(Clone, Debug)]
struct Notice {
    kind: NoticeKind,
    text: String,
}

impl Notice {
    fn quiet(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Quiet,
            text: safe_text(&text.into()),
        }
    }

    fn success(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Success,
            text: safe_text(&text.into()),
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Error,
            text: safe_text(&text.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    None,
    Quit,
    Authenticate,
    Load,
    LoadDetail,
    LoadIncidentTab(IncidentTab),
    Open,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiExit {
    Quit,
    Authenticate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum IncidentTab {
    #[default]
    Overview,
    Activity,
    Attachments,
    Slas,
}

impl IncidentTab {
    const ALL: [Self; 4] = [
        Self::Overview,
        Self::Activity,
        Self::Attachments,
        Self::Slas,
    ];

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Activity => 1,
            Self::Attachments => 2,
            Self::Slas => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "OVERVIEW",
            Self::Activity => "ACTIVITY",
            Self::Attachments => "ATTACHMENTS",
            Self::Slas => "SLAs",
        }
    }
}

#[derive(Clone, Debug, Default)]
enum PanelState<T> {
    #[default]
    Idle,
    Loading,
    Ready {
        items: Vec<T>,
        truncated: bool,
    },
    Failed(String),
}

struct App {
    profile: String,
    instance: String,
    table: String,
    query: Option<String>,
    page_size: usize,
    offset: usize,
    records: Vec<Value>,
    detail_record: Option<Value>,
    detail_record_sys_id: Option<String>,
    overview_error: Option<String>,
    columns: Vec<String>,
    table_state: TableState,
    detail_scroll: u16,
    detail_viewport_width: u16,
    detail_viewport_height: u16,
    incident_tab: IncidentTab,
    activity: PanelState<Value>,
    attachments: PanelState<AttachmentMetadata>,
    slas: PanelState<Value>,
    has_next_page: bool,
    loading: bool,
    detail_loading: bool,
    load_failed: bool,
    auth_failed: bool,
    color: bool,
    overlay: Overlay,
    notice: Notice,
}

impl App {
    fn new(profile: &str, instance: &str, options: TuiOptions) -> Self {
        let query = options
            .query
            .filter(|query| !query.trim().is_empty())
            .or_else(|| default_query(&options.table));
        Self {
            profile: profile.into(),
            instance: compact_instance(instance),
            table: options.table,
            query,
            page_size: options.page_size,
            offset: 0,
            records: Vec::new(),
            detail_record: None,
            detail_record_sys_id: None,
            overview_error: None,
            columns: Vec::new(),
            table_state: TableState::default(),
            detail_scroll: 0,
            detail_viewport_width: 40,
            detail_viewport_height: 10,
            incident_tab: IncidentTab::Overview,
            activity: PanelState::Idle,
            attachments: PanelState::Idle,
            slas: PanelState::Idle,
            has_next_page: false,
            loading: false,
            detail_loading: false,
            load_failed: false,
            auth_failed: false,
            color: options.color,
            overlay: Overlay::None,
            notice: Notice::quiet("Preparing the ledger…"),
        }
    }

    async fn load(&mut self, client: &ServiceNowClient) {
        self.loading = true;
        self.notice = Notice::quiet(format!("Loading {}…", self.table));
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
        match client.list_records(&self.table, &options).await {
            Ok(mut records) => {
                self.has_next_page = records.len() > self.page_size;
                records.truncate(self.page_size);
                self.records = records;
                self.clear_detail_record();
                self.columns = infer_columns(&self.records, &self.table);
                let selected = (!self.records.is_empty()).then_some(
                    self.table_state
                        .selected()
                        .unwrap_or(0)
                        .min(self.records.len().saturating_sub(1)),
                );
                self.table_state.select(selected);
                self.detail_scroll = 0;
                self.load_failed = false;
                self.auth_failed = false;
                self.notice = if self.records.is_empty() {
                    Notice::quiet("No records match this view. Press / to change the query.")
                } else {
                    Notice::success(format!(
                        "Loaded {} record{}",
                        self.records.len(),
                        if self.records.len() == 1 { "" } else { "s" }
                    ))
                };
            }
            Err(error) => {
                self.records.clear();
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
            }
        }
        self.loading = false;
    }

    async fn load_detail(&mut self, client: &ServiceNowClient) {
        let Some(sys_id) = self
            .selected_record()
            .and_then(record_sys_id)
            .map(str::to_string)
        else {
            self.notice = Notice::error("This record has no usable sys_id.");
            return;
        };
        self.detail_loading = true;
        self.overview_error = None;
        self.notice = Notice::quiet("Reading the complete record sheet…");
        match client
            .get_record(&self.table, &sys_id, None, DisplayValue::All)
            .await
        {
            Ok(record) => {
                self.detail_record = Some(record);
                self.detail_record_sys_id = Some(sys_id);
                self.overview_error = None;
                self.notice = Notice::success("Complete record loaded.");
            }
            Err(error) => {
                self.detail_record = None;
                self.detail_record_sys_id = None;
                let message = format!("Could not load the complete record: {error}");
                self.overview_error = Some(message.clone());
                self.notice = Notice::error(format!("{message}. Showing index fields only."));
            }
        }
        self.detail_loading = false;
    }

    async fn load_incident_tab(&mut self, client: &ServiceNowClient, tab: IncidentTab) {
        if tab == IncidentTab::Overview {
            self.load_detail(client).await;
            return;
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
            return;
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
                match client.list_records("sys_journal_field", &options).await {
                    Ok(entries) => {
                        self.activity = bounded_panel(entries);
                        let count = panel_count(&self.activity).unwrap_or(0);
                        self.notice = related_loaded_notice(
                            count,
                            panel_truncated(&self.activity),
                            "activity entry",
                            "activity entries",
                        );
                    }
                    Err(error) => {
                        let message = format!("Could not load incident activity: {error}");
                        self.activity = PanelState::Failed(message.clone());
                        self.notice = Notice::error(message);
                    }
                }
            }
            IncidentTab::Attachments => {
                match client
                    .list_attachments("incident", &sys_id, RELATED_VIEW_LIMIT + 1, false)
                    .await
                {
                    Ok(attachments) => {
                        self.attachments = bounded_panel(attachments);
                        let count = panel_count(&self.attachments).unwrap_or(0);
                        self.notice = related_loaded_notice(
                            count,
                            panel_truncated(&self.attachments),
                            "attachment",
                            "attachments",
                        );
                    }
                    Err(error) => {
                        let message = format!("Could not load incident attachments: {error}");
                        self.attachments = PanelState::Failed(message.clone());
                        self.notice = Notice::error(message);
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
                match client.list_records("task_sla", &options).await {
                    Ok(slas) => {
                        self.slas = bounded_panel(slas);
                        let count = panel_count(&self.slas).unwrap_or(0);
                        self.notice = related_loaded_notice(
                            count,
                            panel_truncated(&self.slas),
                            "SLA",
                            "SLAs",
                        );
                    }
                    Err(error) => {
                        let message = format!("Could not load incident SLAs: {error}");
                        self.slas = PanelState::Failed(message.clone());
                        self.notice = Notice::error(message);
                    }
                }
            }
        }
    }

    fn selected_record(&self) -> Option<&Value> {
        self.table_state
            .selected()
            .and_then(|index| self.records.get(index))
    }

    fn select_next(&mut self) {
        if self.records.is_empty() {
            return;
        }
        let next = self
            .table_state
            .selected()
            .map_or(0, |index| (index + 1).min(self.records.len() - 1));
        self.table_state.select(Some(next));
        self.detail_scroll = 0;
        self.clear_detail_record();
    }

    fn select_previous(&mut self) {
        if self.records.is_empty() {
            return;
        }
        let previous = self.table_state.selected().unwrap_or(0).saturating_sub(1);
        self.table_state.select(Some(previous));
        self.detail_scroll = 0;
        self.clear_detail_record();
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        match &mut self.overlay {
            Overlay::TableInput(buffer) | Overlay::QueryInput(buffer) => match key.code {
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
                        .select((!self.records.is_empty()).then_some(0));
                    self.detail_scroll = 0;
                    self.clear_detail_record();
                    Action::None
                }
                KeyCode::End | KeyCode::Char('G') => {
                    self.table_state
                        .select((!self.records.is_empty()).then_some(self.records.len() - 1));
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
                        self.notice = Notice::quiet("Reading the complete record sheet…");
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
                self.offset = 0;
                self.table_state.select(Some(0));
                Action::Load
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
                self.notice = Notice::quiet("Reading the complete record sheet…");
            }
            IncidentTab::Activity => {
                self.activity = PanelState::Loading;
                self.notice = Notice::quiet("Reading comments and work notes…");
            }
            IncidentTab::Attachments => {
                self.attachments = PanelState::Loading;
                self.notice = Notice::quiet("Reading incident attachments…");
            }
            IncidentTab::Slas => {
                self.slas = PanelState::Loading;
                self.notice = Notice::quiet("Reading incident SLAs…");
            }
        }
        Action::LoadIncidentTab(tab)
    }

    fn detail_record_matches_selection(&self) -> bool {
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

    fn render(&mut self, frame: &mut ratatui::Frame<'_>) {
        let theme = Theme::new(self.color);
        frame.render_widget(Block::default().style(theme.canvas()), frame.area());
        if frame.area().width < 50 || frame.area().height < 12 {
            let message = Paragraph::new(vec![
                Line::styled("THE LEDGER NEEDS MORE ROOM", theme.title()),
                Line::styled("Resize to at least 50 columns × 12 rows.", theme.muted()),
                Line::from(vec![
                    Span::styled("q", theme.key()),
                    Span::styled(" or Ctrl-C to quit", theme.muted()),
                ]),
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
            frame.render_widget(message, frame.area());
            return;
        }
        let chrome = if frame.area().height < 14 {
            [
                Constraint::Length(3),
                Constraint::Min(7),
                Constraint::Length(2),
            ]
        } else {
            [
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
            ]
        };
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(chrome)
            .split(frame.area());
        self.render_header(frame, areas[0], theme);
        self.render_body(frame, areas[1], theme);
        self.render_footer(frame, areas[2], theme);

        match self.overlay.clone() {
            Overlay::Help { .. } => self.render_help(frame, theme),
            Overlay::TableInput(buffer) => {
                render_input(frame, theme, "GO TO TABLE", "Table name", &buffer)
            }
            Overlay::QueryInput(buffer) => render_input(
                frame,
                theme,
                "FILTER RECORDS",
                "ServiceNow encoded query; blank clears",
                &buffer,
            ),
            Overlay::Detail => self.render_detail_sheet(frame, frame.area(), theme, true),
            Overlay::None => {}
        }
    }

    fn render_header(&self, frame: &mut ratatui::Frame<'_>, area: Rect, theme: Theme) {
        let title = Line::from(vec![
            Span::styled(" SERVICENOW ", theme.brand()),
            Span::styled(" OPERATIONS LEDGER", theme.title()),
        ]);
        let location = if area.width >= 80 {
            format!(
                "{}  ·  {}  /  {}  ·  page {}",
                safe_text(&self.profile),
                safe_text(&self.instance),
                self.table,
                self.offset / self.page_size + 1
            )
        } else {
            format!(
                "{}  /  {}  ·  page {}",
                safe_text(&self.profile),
                self.table,
                self.offset / self.page_size + 1
            )
        };
        let header = Paragraph::new(vec![title, Line::styled(location, theme.muted())]).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme.rule()),
        );
        frame.render_widget(header, area);
    }

    fn render_body(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect, theme: Theme) {
        if self.loading {
            let loading = Paragraph::new(vec![
                Line::styled("INDEXING RECORDS", theme.title()),
                Line::styled(
                    format!("Reading {} from {}", self.table, self.instance),
                    theme.muted(),
                ),
            ])
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.rule()),
            );
            frame.render_widget(loading, inset(area, 2, 2));
            return;
        }

        if self.load_failed {
            if self.auth_failed {
                let failure = Paragraph::new(vec![
                    Line::styled("YOUR SERVICENOW SESSION NEEDS ATTENTION", theme.title()),
                    Line::styled(&self.notice.text, theme.body()),
                    Line::raw(""),
                    Line::from(vec![
                        Span::styled("enter / a", theme.key()),
                        Span::styled("  start secure sign-in   ", theme.muted()),
                        Span::styled("r", theme.key()),
                        Span::styled("  retry   ", theme.muted()),
                        Span::styled("q", theme.key()),
                        Span::styled("  quit", theme.muted()),
                    ]),
                ])
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(" SIGN-IN REQUIRED ")
                        .borders(Borders::ALL)
                        .border_style(theme.active_rule()),
                );
                frame.render_widget(failure, state_panel_area(area));
                return;
            }
            let failure = Paragraph::new(vec![
                Line::styled("COULD NOT LOAD THE LEDGER", theme.error()),
                Line::styled(&self.notice.text, theme.body()),
                Line::from(vec![
                    Span::styled("r", theme.key()),
                    Span::styled(" retry   ", theme.muted()),
                    Span::styled("/", theme.key()),
                    Span::styled(" change query   ", theme.muted()),
                    Span::styled("t", theme.key()),
                    Span::styled(" change table", theme.muted()),
                ]),
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" LOAD FAILED ")
                    .borders(Borders::ALL)
                    .border_style(theme.error()),
            );
            frame.render_widget(failure, state_panel_area(area));
            return;
        }

        if area.width >= 104 {
            let panels = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(area);
            self.render_ledger(frame, panels[0], theme);
            self.render_detail_sheet(frame, panels[1], theme, false);
        } else {
            self.render_ledger(frame, area, theme);
        }
    }

    fn render_ledger(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect, theme: Theme) {
        if self.records.is_empty() {
            let empty = Paragraph::new(vec![
                Line::styled("THE LEDGER IS EMPTY", theme.title()),
                Line::styled(
                    "No records match the current table and query.",
                    theme.muted(),
                ),
                Line::from(vec![
                    Span::styled("/", theme.key()),
                    Span::styled(" adjust query   ", theme.muted()),
                    Span::styled("t", theme.key()),
                    Span::styled(" change table", theme.muted()),
                ]),
            ])
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(" RECORD INDEX ")
                    .borders(Borders::ALL)
                    .border_style(theme.rule()),
            );
            frame.render_widget(empty, area);
            return;
        }

        let visible_columns = visible_columns(&self.columns, area.width);
        let rows = self.records.iter().map(|record| {
            let cells = visible_columns
                .iter()
                .map(|field| Cell::from(display_field(record, field)));
            Row::new(cells).height(1).style(theme.body())
        });
        let header = Row::new(
            visible_columns
                .iter()
                .map(|field| Cell::from(field_label(field))),
        )
        .style(theme.column_header())
        .height(1);
        let weights: Vec<u32> = visible_columns
            .iter()
            .map(|field| field_weight(field))
            .collect();
        let total_weight = weights.iter().sum::<u32>().max(1);
        let widths: Vec<Constraint> = weights
            .into_iter()
            .map(|weight| Constraint::Ratio(weight, total_weight))
            .collect();
        let title = format!(
            " RECORD INDEX  {}–{} ",
            self.offset + 1,
            self.offset + self.records.len()
        );
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(theme.selection())
            .highlight_symbol("▌")
            .column_spacing(1)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(theme.rule()),
            );
        frame.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn render_detail_sheet(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        theme: Theme,
        expanded: bool,
    ) {
        let sheet_area = if expanded {
            let width = area.width.saturating_sub(4).min(112);
            let height = area.height.saturating_sub(4).min(38);
            centered_rect(width, height, area)
        } else {
            area
        };
        if expanded {
            frame.render_widget(Clear, sheet_area);
            self.detail_viewport_width = sheet_area
                .width
                .saturating_sub(if self.table == "incident" { 4 } else { 2 })
                .max(1);
            self.detail_viewport_height = sheet_area
                .height
                .saturating_sub(if self.table == "incident" { 5 } else { 2 })
                .max(1);
        }
        let record = if expanded && self.detail_record_matches_selection() {
            self.detail_record.as_ref()
        } else {
            self.selected_record()
        };
        let Some(record) = record else {
            let detail = Paragraph::new("Select a record to inspect its fields.")
                .style(theme.muted())
                .block(
                    Block::default()
                        .title(" RECORD SHEET ")
                        .borders(Borders::ALL)
                        .border_style(theme.rule()),
                );
            frame.render_widget(detail, sheet_area);
            return;
        };
        let title = record_title(record);
        if expanded && self.table == "incident" {
            self.render_incident_workspace(frame, sheet_area, theme, record, &title);
            return;
        }
        let mut lines = Vec::new();
        if expanded {
            lines.push(Line::styled(title.clone(), theme.detail_title()));
            lines.push(Line::styled("", theme.body()));
        }
        if expanded && self.detail_loading {
            lines.push(Line::styled(
                "Reading all fields from ServiceNow…",
                theme.muted(),
            ));
            lines.push(Line::styled("", theme.body()));
        }
        if expanded {
            let fields = record.as_object().map_or(0, serde_json::Map::len);
            lines.push(Line::from(vec![
                Span::styled("ALL FIELDS", theme.key()),
                Span::styled(
                    format!(
                        "  ·  {fields} fields  ·  scroll {}  ·  j/k or PgUp/PgDn  ·  g/G edges",
                        self.detail_scroll
                    ),
                    theme.muted(),
                ),
            ]));
            lines.push(Line::styled("", theme.body()));
        }
        if let Some(object) = record.as_object() {
            let mut fields: Vec<_> = object.iter().collect();
            fields.sort_by(|(left, _), (right, _)| field_rank(left).cmp(&field_rank(right)));
            for (field, value) in fields {
                lines.push(Line::from(vec![
                    Span::styled(format!("{}  ", field_label(field)), theme.field()),
                    Span::styled(display_field_value(field, value), theme.body()),
                ]));
            }
        }
        let block_title = if expanded {
            format!(" RECORD SHEET  {title}  ·  ESC BACK ")
        } else {
            format!(" INDEX PREVIEW  {title}  ·  ENTER FOR ALL FIELDS ")
        };
        let detail = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .block(
                Block::default()
                    .title(block_title)
                    .borders(Borders::ALL)
                    .border_style(if expanded {
                        theme.active_rule()
                    } else {
                        theme.rule()
                    }),
            );
        frame.render_widget(detail, sheet_area);
    }

    fn render_incident_workspace(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        theme: Theme,
        record: &Value,
        title: &str,
    ) {
        let block = Block::default()
            .title(format!(" INCIDENT WORKSPACE  {title}  ·  ESC BACK "))
            .borders(Borders::ALL)
            .border_style(theme.active_rule())
            .style(theme.canvas());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let areas = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(inner);
        let tabs = Line::from(
            IncidentTab::ALL
                .into_iter()
                .flat_map(|tab| {
                    let label = if inner.width < 72 && tab == IncidentTab::Attachments {
                        "FILES"
                    } else {
                        tab.label()
                    };
                    let count = if inner.width >= 78 {
                        self.incident_tab_count(tab)
                            .map(|count| format!(" {count}"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let style = if tab == self.incident_tab {
                        theme.active_tab()
                    } else {
                        theme.muted()
                    };
                    let marker = if tab == self.incident_tab { "▌" } else { " " };
                    [
                        Span::styled(
                            format!("{marker}{} {}{count} ", tab.index() + 1, label),
                            style,
                        ),
                        Span::raw(" "),
                    ]
                })
                .collect::<Vec<_>>(),
        );
        let header = Paragraph::new(vec![
            Line::styled(title.to_string(), theme.detail_title()),
            tabs,
        ])
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme.rule()),
        );
        frame.render_widget(header, areas[0]);

        let lines = match self.incident_tab {
            IncidentTab::Overview => self.overview_lines(record, theme),
            IncidentTab::Activity => activity_lines(&self.activity, theme),
            IncidentTab::Attachments => attachment_lines(&self.attachments, theme),
            IncidentTab::Slas => sla_lines(&self.slas, theme),
        };
        let body = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(body, inset(areas[1], 1, 0));
    }

    fn incident_tab_count(&self, tab: IncidentTab) -> Option<String> {
        match tab {
            IncidentTab::Overview => self
                .detail_record
                .as_ref()
                .filter(|_| self.detail_record_matches_selection())
                .and_then(Value::as_object)
                .map(serde_json::Map::len)
                .map(|count| count.to_string()),
            IncidentTab::Activity => panel_count_label(&self.activity),
            IncidentTab::Attachments => panel_count_label(&self.attachments),
            IncidentTab::Slas => panel_count_label(&self.slas),
        }
    }

    fn overview_lines<'a>(&self, record: &'a Value, theme: Theme) -> Vec<Line<'a>> {
        let mut lines = Vec::new();
        let label = if let Some(error) = &self.overview_error {
            lines.push(Line::styled("COMPLETE RECORD UNAVAILABLE", theme.error()));
            lines.push(Line::styled(safe_text(error), theme.body()));
            lines.push(Line::from(vec![
                Span::styled("r", theme.key()),
                Span::styled(
                    " retry the full record  ·  Showing the ledger's index projection.",
                    theme.muted(),
                ),
            ]));
            lines.push(Line::raw(""));
            "INDEX FIELDS ONLY"
        } else if self.detail_loading {
            lines.push(Line::styled(
                "Reading all incident fields from ServiceNow…",
                theme.muted(),
            ));
            lines.push(Line::raw(""));
            "INDEX FIELDS"
        } else if self.detail_record_matches_selection() {
            "ALL FIELDS"
        } else {
            "INDEX FIELDS"
        };
        let fields = record.as_object().map_or(0, serde_json::Map::len);
        lines.push(Line::from(vec![
            Span::styled(label, theme.key()),
            Span::styled(
                format!(
                    "  ·  {fields} fields  ·  scroll {}  ·  Tab changes view",
                    self.detail_scroll
                ),
                theme.muted(),
            ),
        ]));
        lines.push(Line::raw(""));
        if let Some(object) = record.as_object() {
            let mut fields: Vec<_> = object.iter().collect();
            fields.sort_by(|(left, _), (right, _)| field_rank(left).cmp(&field_rank(right)));
            for (field, value) in fields {
                lines.push(Line::from(vec![
                    Span::styled(format!("{}  ", field_label(field)), theme.field()),
                    Span::styled(display_field_value(field, value), theme.body()),
                ]));
            }
        }
        lines
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect, theme: Theme) {
        let notice_style = match self.notice.kind {
            NoticeKind::Quiet => theme.muted(),
            NoticeKind::Success => theme.success(),
            NoticeKind::Error => theme.error(),
        };
        let query = safe_text(self.query.as_deref().unwrap_or("all records"));
        let notice_limit = usize::from(area.width).saturating_div(2).max(20);
        let notice = truncate_text(&self.notice.text, notice_limit);
        let previous_key = if self.offset > 0 {
            theme.key()
        } else {
            theme.muted()
        };
        let next_key = if self.has_next_page {
            theme.key()
        } else {
            theme.muted()
        };
        let hints = if self.load_failed && self.auth_failed {
            Line::from(vec![
                Span::styled("enter / a", theme.key()),
                Span::styled(" authenticate  ", theme.muted()),
                Span::styled("r", theme.key()),
                Span::styled(" retry  ", theme.muted()),
                Span::styled("?", theme.key()),
                Span::styled(" help  ", theme.muted()),
                Span::styled("q", theme.key()),
                Span::styled(" quit", theme.muted()),
            ])
        } else {
            Line::from(vec![
                Span::styled("↑↓", theme.key()),
                Span::styled(" move  ", theme.muted()),
                Span::styled("enter", theme.key()),
                Span::styled(" inspect  ", theme.muted()),
                Span::styled("/", theme.key()),
                Span::styled(" filter  ", theme.muted()),
                Span::styled("t", theme.key()),
                Span::styled(" table  ", theme.muted()),
                Span::styled("p", previous_key),
                Span::styled(
                    if self.offset > 0 {
                        " prev  "
                    } else {
                        " start  "
                    },
                    theme.muted(),
                ),
                Span::styled("n", next_key),
                Span::styled(
                    if self.has_next_page {
                        " next  "
                    } else {
                        " end  "
                    },
                    theme.muted(),
                ),
                Span::styled("?", theme.key()),
                Span::styled(" help  ", theme.muted()),
                Span::styled("q", theme.key()),
                Span::styled(" quit", theme.muted()),
            ])
        };
        let status = if area.width >= 90 {
            Line::from(vec![
                Span::styled(format!(" {notice} "), notice_style),
                Span::styled(
                    format!("  QUERY  {}", truncate_text(&query, notice_limit)),
                    theme.muted(),
                ),
            ])
        } else {
            Line::styled(format!(" {notice}"), notice_style)
        };
        let footer = Paragraph::new(vec![status, hints]).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(theme.rule()),
        );
        frame.render_widget(footer, area);
    }

    fn render_help(&self, frame: &mut ratatui::Frame<'_>, theme: Theme) {
        let area = centered_rect(66, 22, frame.area());
        frame.render_widget(Clear, area);
        let rows = [
            ("↑ / k, ↓ / j", "Move through records"),
            ("enter / →", "Unfold the selected record sheet"),
            ("tab / shift-tab", "Move through incident detail views"),
            ("1 / 2 / 3 / 4", "Open Overview, Activity, Files, or SLAs"),
            ("t", "Browse another table"),
            ("/", "Set or clear an encoded query"),
            ("n / p", "Load the next or previous page"),
            ("r", "Reload the current page or incident view"),
            (
                "enter / a",
                "Sign in when the current session needs attention",
            ),
            ("j/k, PgUp/PgDn", "Scroll inside a complete record sheet"),
            ("o", "Open the selected record in ServiceNow"),
            ("g / G", "Jump to first or last record"),
            ("esc", "Close the current sheet or prompt"),
            ("q / Ctrl-C", "Return to the shell"),
        ];
        let mut lines = vec![
            Line::styled("A keyboard map for the operations ledger", theme.muted()),
            Line::raw(""),
        ];
        for (key, description) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("{key:<16}"), theme.key()),
                Span::styled(description, theme.body()),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "This first release is intentionally read-only.",
            theme.success(),
        ));
        let help = Paragraph::new(lines).block(
            Block::default()
                .title(" KEYBOARD MAP  ·  ? OR ESC TO CLOSE ")
                .borders(Borders::ALL)
                .border_style(theme.active_rule())
                .style(theme.canvas()),
        );
        frame.render_widget(help, area);
    }

    fn open_selected(&mut self, client: &ServiceNowClient) {
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

pub async fn run(
    client: &ServiceNowClient,
    config: &Config,
    options: TuiOptions,
) -> Result<TuiExit, ApiError> {
    options.validate()?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(ApiError::InvalidInput(
            "the TUI requires an interactive terminal on stdin and stdout".into(),
        ));
    }

    enable_raw_mode().map_err(terminal_error)?;
    let _restore = RestoreTerminal;
    execute!(io::stdout(), EnterAlternateScreen).map_err(terminal_error)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).map_err(terminal_error)?;
    terminal.clear().map_err(terminal_error)?;

    let mut app = App::new(&config.profile, &config.instance, options);
    terminal
        .draw(|frame| app.render(frame))
        .map_err(terminal_error)?;
    app.load(client).await;

    loop {
        terminal
            .draw(|frame| app.render(frame))
            .map_err(terminal_error)?;
        if !event::poll(Duration::from_millis(200)).map_err(terminal_error)? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(terminal_error)? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.handle_key(key) {
            Action::None => {}
            Action::Quit => return Ok(TuiExit::Quit),
            Action::Authenticate => return Ok(TuiExit::Authenticate),
            Action::Open => app.open_selected(client),
            Action::Load => {
                terminal
                    .draw(|frame| app.render(frame))
                    .map_err(terminal_error)?;
                app.load(client).await;
            }
            Action::LoadDetail => {
                terminal
                    .draw(|frame| app.render(frame))
                    .map_err(terminal_error)?;
                app.load_detail(client).await;
            }
            Action::LoadIncidentTab(tab) => {
                terminal
                    .draw(|frame| app.render(frame))
                    .map_err(terminal_error)?;
                app.load_incident_tab(client, tab).await;
            }
        }
    }
}

struct ConnectionApp<'a> {
    profile: &'a str,
    instance: Option<&'a str>,
    reason: &'a str,
    color: bool,
}

impl ConnectionApp<'_> {
    fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let theme = Theme::new(self.color);
        frame.render_widget(Block::default().style(theme.canvas()), frame.area());
        if frame.area().width < 50 || frame.area().height < 12 {
            let message = Paragraph::new(vec![
                Line::styled("SIGN-IN REQUIRED", theme.title()),
                Line::from(vec![
                    Span::styled("enter", theme.key()),
                    Span::styled(" authenticate  ·  ", theme.muted()),
                    Span::styled("q", theme.key()),
                    Span::styled(" quit", theme.muted()),
                ]),
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
            frame.render_widget(message, frame.area());
            return;
        }

        let chrome = if frame.area().height < 14 {
            [
                Constraint::Length(3),
                Constraint::Min(7),
                Constraint::Length(2),
            ]
        } else {
            [
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
            ]
        };
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(chrome)
            .split(frame.area());
        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" SERVICENOW ", theme.brand()),
                Span::styled(" SECURE CONNECTION", theme.title()),
            ]),
            Line::styled(
                format!(
                    "{}{}",
                    safe_text(self.profile),
                    self.instance
                        .map(|instance| format!("  ·  {}", safe_text(instance)))
                        .unwrap_or_default()
                ),
                theme.muted(),
            ),
        ])
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme.rule()),
        );
        frame.render_widget(header, areas[0]);

        let panel_lines = if areas[1].height < 12 {
            vec![
                Line::styled("CONNECT THE OPERATIONS LEDGER", theme.title()),
                Line::styled("This profile needs a ServiceNow session.", theme.body()),
                Line::from(vec![
                    Span::styled("enter / a", theme.key()),
                    Span::styled("  secure sign-in", theme.body()),
                ]),
                Line::from(vec![
                    Span::styled("q / esc", theme.key()),
                    Span::styled("    return to shell", theme.muted()),
                ]),
            ]
        } else {
            vec![
                Line::styled("CONNECT THE OPERATIONS LEDGER", theme.title()),
                Line::styled(
                    "Authenticate this profile to browse live ServiceNow records.",
                    theme.body(),
                ),
                Line::raw(""),
                Line::styled(safe_text(self.reason), theme.muted()),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("enter / a", theme.key()),
                    Span::styled("  start secure sign-in", theme.body()),
                ]),
                Line::from(vec![
                    Span::styled("q / esc", theme.key()),
                    Span::styled("    return to the shell", theme.muted()),
                ]),
            ]
        };
        let panel = Paragraph::new(panel_lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" SIGN-IN REQUIRED ")
                    .borders(Borders::ALL)
                    .border_style(theme.active_rule()),
            );
        let panel_area = if areas[1].height < 12 {
            state_panel_area(areas[1])
        } else {
            centered_rect(76, 11, areas[1])
        };
        frame.render_widget(panel, panel_area);

        let footer_text = if areas[2].width < 70 {
            " Sign-in returns you to this ledger."
        } else {
            " Sign-in continues outside the TUI, then returns you to this ledger."
        };
        let footer = Paragraph::new(Line::styled(footer_text, theme.muted())).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(theme.rule()),
        );
        frame.render_widget(footer, areas[2]);
    }
}

pub fn request_authentication(
    profile: &str,
    instance: Option<&str>,
    reason: &str,
    color: bool,
) -> Result<TuiExit, ApiError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(ApiError::InvalidInput(
            "the TUI requires an interactive terminal on stdin and stdout".into(),
        ));
    }

    enable_raw_mode().map_err(terminal_error)?;
    let _restore = RestoreTerminal;
    execute!(io::stdout(), EnterAlternateScreen).map_err(terminal_error)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).map_err(terminal_error)?;
    terminal.clear().map_err(terminal_error)?;
    let app = ConnectionApp {
        profile,
        instance,
        reason,
        color,
    };

    loop {
        terminal
            .draw(|frame| app.render(frame))
            .map_err(terminal_error)?;
        if !event::poll(Duration::from_millis(200)).map_err(terminal_error)? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(terminal_error)? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(TuiExit::Quit);
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('a') => return Ok(TuiExit::Authenticate),
            KeyCode::Esc | KeyCode::Char('q') => return Ok(TuiExit::Quit),
            _ => {}
        }
    }
}

struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

fn panel_count<T>(state: &PanelState<T>) -> Option<usize> {
    match state {
        PanelState::Ready { items, .. } => Some(items.len()),
        _ => None,
    }
}

fn panel_truncated<T>(state: &PanelState<T>) -> bool {
    matches!(
        state,
        PanelState::Ready {
            truncated: true,
            ..
        }
    )
}

fn panel_count_label<T>(state: &PanelState<T>) -> Option<String> {
    panel_count(state).map(|count| {
        if panel_truncated(state) {
            format!("{count}+")
        } else {
            count.to_string()
        }
    })
}

fn bounded_panel<T>(mut items: Vec<T>) -> PanelState<T> {
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

fn scroll_extent(lines: &[Line<'_>], width: usize, height: usize) -> u16 {
    let visual_lines = lines
        .iter()
        .map(|line| {
            let characters = line
                .spans
                .iter()
                .map(|span| span.content.chars().count())
                .sum::<usize>();
            characters.max(1).div_ceil(width.max(1))
        })
        .sum::<usize>();
    visual_lines
        .saturating_sub(height)
        .min(usize::from(u16::MAX)) as u16
}

fn activity_lines(state: &PanelState<Value>, theme: Theme) -> Vec<Line<'static>> {
    match state {
        PanelState::Idle | PanelState::Loading => loading_panel(
            "READING ACTIVITY",
            "Loading comments and work notes from the incident journal…",
            theme,
        ),
        PanelState::Failed(message) => failed_panel(message, theme),
        PanelState::Ready { items, .. } if items.is_empty() => empty_panel(
            "NO ACTIVITY RETURNED",
            "No readable comments or work notes were found for this incident.",
            theme,
        ),
        PanelState::Ready {
            items: entries,
            truncated,
        } => {
            let mut lines = vec![panel_meta(
                "INCIDENT JOURNAL",
                entries.len(),
                "entries",
                *truncated,
                theme,
            )];
            lines.push(Line::raw(""));
            for entry in entries {
                let kind = match raw_field_string(entry, "element").as_deref() {
                    Some("comments") => "COMMENT",
                    Some("work_notes") => "WORK NOTE",
                    _ => "ACTIVITY",
                };
                lines.push(Line::from(vec![
                    Span::styled(kind, theme.field()),
                    Span::styled(
                        format!(
                            "  {}  ·  {}",
                            display_field(entry, "sys_created_on"),
                            display_field(entry, "sys_created_by")
                        ),
                        theme.muted(),
                    ),
                ]));
                lines.push(Line::styled(display_field(entry, "value"), theme.body()));
                lines.push(Line::raw(""));
            }
            lines
        }
    }
}

fn attachment_lines(state: &PanelState<AttachmentMetadata>, theme: Theme) -> Vec<Line<'static>> {
    match state {
        PanelState::Idle | PanelState::Loading => loading_panel(
            "READING ATTACHMENTS",
            "Loading file metadata for this incident…",
            theme,
        ),
        PanelState::Failed(message) => failed_panel(message, theme),
        PanelState::Ready { items, .. } if items.is_empty() => empty_panel(
            "NO ATTACHMENTS",
            "No files are attached to this incident.",
            theme,
        ),
        PanelState::Ready {
            items: attachments,
            truncated,
        } => {
            let mut lines = vec![panel_meta(
                "ATTACHED FILES",
                attachments.len(),
                "files",
                *truncated,
                theme,
            )];
            lines.push(Line::raw(""));
            for attachment in attachments {
                lines.push(Line::styled(
                    safe_text(&attachment.file_name),
                    theme.field(),
                ));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "{}  ·  {}",
                            safe_text(&human_size(&attachment.size_bytes)),
                            safe_text(&attachment.content_type)
                        ),
                        theme.body(),
                    ),
                    Span::styled(
                        format!(
                            "  ·  {}  ·  {}",
                            safe_text(&attachment.sys_created_on),
                            safe_text(&attachment.sys_created_by)
                        ),
                        theme.muted(),
                    ),
                ]));
                lines.push(Line::raw(""));
            }
            lines
        }
    }
}

fn sla_lines(state: &PanelState<Value>, theme: Theme) -> Vec<Line<'static>> {
    match state {
        PanelState::Idle | PanelState::Loading => loading_panel(
            "READING SLAs",
            "Loading task SLA records for this incident…",
            theme,
        ),
        PanelState::Failed(message) => failed_panel(message, theme),
        PanelState::Ready { items, .. } if items.is_empty() => empty_panel(
            "NO SLAs RETURNED",
            "No readable task SLA records were found for this incident.",
            theme,
        ),
        PanelState::Ready {
            items: slas,
            truncated,
        } => {
            let mut lines = vec![panel_meta(
                "TASK SLAs",
                slas.len(),
                "records",
                *truncated,
                theme,
            )];
            lines.push(Line::raw(""));
            for sla in slas {
                let breached = truthy_field(sla, "has_breached");
                let status = if breached { "BREACHED" } else { "ON TRACK" };
                let status_style = if breached {
                    theme.error()
                } else {
                    theme.success()
                };
                lines.push(Line::styled(display_field(sla, "sla"), theme.field()));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "STAGE  {}  ·  PROGRESS  {}  ·  ",
                            display_field(sla, "stage"),
                            percentage_field(sla, "percentage")
                        ),
                        theme.body(),
                    ),
                    Span::styled(status, status_style),
                ]));
                lines.push(Line::styled(
                    format!(
                        "START  {}  ·  TARGET  {}  ·  END  {}",
                        display_field(sla, "start_time"),
                        display_field(sla, "planned_end_time"),
                        display_field(sla, "end_time")
                    ),
                    theme.muted(),
                ));
                lines.push(Line::styled(
                    format!(
                        "DURATION  {}  ·  PAUSED  {}",
                        display_field(sla, "duration"),
                        display_field(sla, "pause_duration")
                    ),
                    theme.muted(),
                ));
                lines.push(Line::raw(""));
            }
            lines
        }
    }
}

fn panel_meta(
    label: &str,
    count: usize,
    noun: &str,
    truncated: bool,
    theme: Theme,
) -> Line<'static> {
    let count = if truncated {
        format!("latest {count} {noun}; more available")
    } else {
        format!("{count} {noun}")
    };
    Line::from(vec![
        Span::styled(label.to_string(), theme.key()),
        Span::styled(
            format!("  ·  {count}  ·  j/k or PgUp/PgDn  ·  r reload  ·  Tab changes view"),
            theme.muted(),
        ),
    ])
}

fn loading_panel(title: &str, message: &str, theme: Theme) -> Vec<Line<'static>> {
    vec![
        Line::styled(title.to_string(), theme.title()),
        Line::styled(message.to_string(), theme.muted()),
    ]
}

fn failed_panel(message: &str, theme: Theme) -> Vec<Line<'static>> {
    vec![
        Line::styled("THIS VIEW COULD NOT BE LOADED", theme.error()),
        Line::styled(safe_text(message), theme.body()),
        Line::from(vec![
            Span::styled("r", theme.key()),
            Span::styled(
                " retry this view  ·  The incident overview remains available.",
                theme.muted(),
            ),
        ]),
    ]
}

fn empty_panel(title: &str, message: &str, theme: Theme) -> Vec<Line<'static>> {
    vec![
        Line::styled(title.to_string(), theme.title()),
        Line::styled(message.to_string(), theme.muted()),
        Line::from(vec![
            Span::styled("r", theme.key()),
            Span::styled(" reload this view", theme.muted()),
        ]),
    ]
}

fn truthy_field(record: &Value, field: &str) -> bool {
    let Some(value) = record.get(field) else {
        return false;
    };
    let value = match value {
        Value::Object(object) => object.get("value").unwrap_or(value),
        _ => value,
    };
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        Value::Number(value) => value.as_u64().is_some_and(|value| value != 0),
        _ => false,
    }
}

fn raw_field_string(record: &Value, field: &str) -> Option<String> {
    let value = record.get(field)?;
    let value = match value {
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("display_value"))
            .unwrap_or(value),
        _ => value,
    };
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn percentage_field(record: &Value, field: &str) -> String {
    let value = display_field(record, field);
    if value == "—" || value.ends_with('%') {
        value
    } else {
        format!("{value}%")
    }
}

#[derive(Clone, Copy)]
struct Theme {
    color: bool,
}

impl Theme {
    fn new(color: bool) -> Self {
        Self { color }
    }

    fn canvas(self) -> Style {
        self.style(Color::Rgb(211, 220, 218), Color::Rgb(8, 14, 18))
    }

    fn body(self) -> Style {
        self.style(Color::Rgb(211, 220, 218), Color::Reset)
    }

    fn muted(self) -> Style {
        self.style(Color::Rgb(123, 145, 145), Color::Reset)
    }

    fn brand(self) -> Style {
        self.style(Color::Rgb(4, 24, 27), Color::Rgb(101, 240, 202))
            .add_modifier(Modifier::BOLD)
    }

    fn title(self) -> Style {
        self.style(Color::Rgb(238, 246, 242), Color::Reset)
            .add_modifier(Modifier::BOLD)
    }

    fn detail_title(self) -> Style {
        self.style(Color::Rgb(101, 240, 202), Color::Reset)
            .add_modifier(Modifier::BOLD)
    }

    fn active_tab(self) -> Style {
        self.style(Color::Rgb(101, 240, 202), Color::Reset)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }

    fn column_header(self) -> Style {
        self.style(Color::Rgb(154, 177, 174), Color::Rgb(15, 27, 32))
            .add_modifier(Modifier::BOLD)
    }

    fn field(self) -> Style {
        self.style(Color::Rgb(101, 240, 202), Color::Reset)
            .add_modifier(Modifier::BOLD)
    }

    fn key(self) -> Style {
        self.style(Color::Rgb(246, 183, 82), Color::Reset)
            .add_modifier(Modifier::BOLD)
    }

    fn rule(self) -> Style {
        self.style(Color::Rgb(53, 74, 77), Color::Reset)
    }

    fn active_rule(self) -> Style {
        self.style(Color::Rgb(101, 240, 202), Color::Reset)
    }

    fn selection(self) -> Style {
        self.style(Color::Rgb(4, 24, 27), Color::Rgb(101, 240, 202))
            .add_modifier(Modifier::BOLD)
    }

    fn success(self) -> Style {
        self.style(Color::Rgb(101, 240, 202), Color::Reset)
    }

    fn error(self) -> Style {
        self.style(Color::Rgb(255, 126, 119), Color::Reset)
            .add_modifier(Modifier::BOLD)
    }

    fn style(self, foreground: Color, background: Color) -> Style {
        if self.color {
            Style::default().fg(foreground).bg(background)
        } else {
            Style::default()
        }
    }
}

fn render_input(
    frame: &mut ratatui::Frame<'_>,
    theme: Theme,
    title: &str,
    prompt: &str,
    buffer: &str,
) {
    let width = frame.area().width.saturating_sub(4).min(76);
    let area = centered_rect(width, 7, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {title}  ·  ENTER APPLY  ESC CANCEL "))
        .borders(Borders::ALL)
        .border_style(theme.active_rule())
        .style(theme.canvas());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(safe_text(prompt)).style(theme.muted()),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let input_y = inner.y.saturating_add(2);
    frame.render_widget(
        Paragraph::new("› ").style(theme.key()),
        Rect::new(inner.x, input_y, 2.min(inner.width), 1),
    );
    let buffer_area = Rect::new(
        inner.x.saturating_add(2),
        input_y,
        inner.width.saturating_sub(2),
        1,
    );
    let buffer = safe_text(buffer);
    let length = buffer.chars().count() as u16;
    let visible_width = buffer_area.width.max(1);
    let horizontal_scroll = length.saturating_sub(visible_width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(buffer)
            .style(theme.body().add_modifier(Modifier::BOLD))
            .scroll((0, horizontal_scroll)),
        buffer_area,
    );
    let cursor_x = buffer_area
        .x
        .saturating_add(length.saturating_sub(horizontal_scroll))
        .min(buffer_area.right().saturating_sub(1));
    frame.set_cursor_position((cursor_x, input_y));
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn state_panel_area(area: Rect) -> Rect {
    if area.height < 12 {
        inset(area, 1, 0)
    } else {
        inset(area, 2, 2)
    }
}

fn terminal_error(error: io::Error) -> ApiError {
    ApiError::Other(format!("terminal error: {error}"))
}

fn compact_instance(instance: &str) -> String {
    instance
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .into()
}

fn default_query(table: &str) -> Option<String> {
    (table == "incident").then(|| DEFAULT_INCIDENT_QUERY.into())
}

fn infer_columns(records: &[Value], table: &str) -> Vec<String> {
    if table == "incident" {
        return [
            "number",
            "priority",
            "short_description",
            "state",
            "assigned_to",
            "sys_updated_on",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
    }
    let available: BTreeSet<&str> = records
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|record| record.keys().map(String::as_str))
        .collect();
    let preferred = [
        "number",
        "name",
        "short_description",
        "state",
        "status",
        "class",
        "sys_updated_on",
        "sys_id",
    ];
    let mut fields: Vec<String> = preferred
        .iter()
        .filter(|field| available.contains(**field))
        .map(|field| (*field).into())
        .take(6)
        .collect();
    for field in available {
        if fields.len() >= 6 {
            break;
        }
        if !fields.iter().any(|candidate| candidate == field) {
            fields.push(field.into());
        }
    }
    fields
}

fn visible_columns(columns: &[String], width: u16) -> Vec<String> {
    let count = if width < 58 {
        2
    } else if width < 82 {
        3
    } else if width < 112 {
        4
    } else {
        6
    };
    columns.iter().take(count).cloned().collect()
}

fn field_weight(field: &str) -> u32 {
    match field {
        "short_description" | "description" => 4,
        "sys_id" => 3,
        _ => 2,
    }
}

fn field_rank(field: &str) -> (u8, &str) {
    let rank = match field {
        "number" => 0,
        "short_description" | "name" => 1,
        "priority" => 2,
        "state" | "status" => 3,
        "assigned_to" | "assignment_group" => 4,
        "description" => 5,
        "sys_updated_on" | "sys_created_on" => 8,
        "sys_id" => 10,
        _ => 7,
    };
    (rank, field)
}

fn field_label(field: &str) -> String {
    safe_text(&match field {
        "short_description" => "DESCRIPTION".into(),
        "assigned_to" => "ASSIGNEE".into(),
        "sys_updated_on" => "UPDATED".into(),
        "sys_created_on" => "CREATED".into(),
        "sys_id" => "SYS ID".into(),
        other => other.replace('_', " ").to_uppercase(),
    })
}

fn display_field(record: &Value, field: &str) -> String {
    record
        .get(field)
        .map(|value| display_field_value(field, value))
        .unwrap_or_else(|| "—".into())
}

fn display_field_value(field: &str, value: &Value) -> String {
    if is_sensitive_field(field) {
        "[REDACTED]".into()
    } else {
        display_value(value)
    }
}

fn display_value(value: &Value) -> String {
    let value = match value {
        Value::Object(object) => object
            .get("display_value")
            .or_else(|| object.get("value"))
            .unwrap_or(value),
        _ => value,
    };
    let rendered = match value {
        Value::Null => "—".into(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(value) if value.trim().is_empty() => "—".into(),
        Value::String(value) => value.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "?".into()),
    };
    safe_text(&rendered)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.into();
    }
    let keep = max_chars.saturating_sub(1);
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push('…');
    truncated
}

fn is_sensitive_field(field: &str) -> bool {
    let normalized = field.to_ascii_lowercase().replace(['-', ' '], "_");
    normalized.contains("password")
        || normalized.contains("passwd")
        || normalized.contains("secret")
        || normalized.contains("private_key")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized
            .split('_')
            .any(|part| matches!(part, "token" | "cookie" | "credential" | "authorization"))
}

fn record_sys_id(record: &Value) -> Option<&str> {
    let value = record.get("sys_id")?;
    match value {
        Value::String(value) => Some(value),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .or_else(|| object.get("display_value").and_then(Value::as_str)),
        _ => None,
    }
}

fn record_title(record: &Value) -> String {
    ["number", "name", "short_description", "sys_id"]
        .into_iter()
        .find_map(|field| {
            record
                .get(field)
                .map(display_value)
                .filter(|value| value != "—")
        })
        .unwrap_or_else(|| "SELECTED RECORD".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthType;
    use ratatui::backend::TestBackend;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn app() -> App {
        let mut app = App::new(
            "work",
            "https://dev12345.service-now.com",
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
        app.columns = infer_columns(&app.records, &app.table);
        app.table_state.select(Some(0));
        app.notice = Notice::success("Loaded 1 record");
        app
    }

    #[test]
    fn incidents_open_on_active_work_assigned_to_the_user_or_their_groups() {
        let app = App::new(
            "work",
            "https://dev12345.service-now.com",
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

    #[test]
    fn renders_identity_ledger_and_selected_record_without_secrets() {
        let backend = TestBackend::new(140, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let text = rendered_text(buffer);
        if std::env::var_os("SERVICENOW_TUI_SNAPSHOT").is_some() {
            eprintln!("\n{text}");
        }
        assert!(text.contains("OPERATIONS LEDGER"));
        assert!(text.contains("dev12345.service-now.com"));
        assert!(text.contains("INC0010001"));
        assert!(text.contains("Mail is unavailable"));
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("never-rendered"));
    }

    #[test]
    fn compact_terminals_keep_the_ledger_primary() {
        let backend = TestBackend::new(72, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("OPERATIONS LEDGER"));
        assert!(text.contains("RECORD INDEX"));
        assert!(!text.contains("RECORD SHEET"));
    }

    #[test]
    fn tiny_terminals_show_a_specific_recovery() {
        let backend = TestBackend::new(36, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("THE LEDGER NEEDS MORE ROOM"));
    }

    #[test]
    fn no_color_render_uses_terminal_defaults() {
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.color = false;
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
    }

    #[test]
    fn load_failures_do_not_claim_the_ledger_is_empty() {
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.records.clear();
        app.load_failed = true;
        app.notice = Notice::error("ServiceNow rejected the query\u{1b}[31m");
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("COULD NOT LOAD THE LEDGER"));
        assert!(!text.contains("THE LEDGER IS EMPTY"));
        assert!(!text.contains('\u{1b}'));
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
    fn unauthenticated_launch_explains_the_handoff_and_return() {
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = ConnectionApp {
            profile: "work",
            instance: Some("dev12345.service-now.com"),
            reason: "This profile does not have a usable credential yet.",
            color: true,
        };
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        if std::env::var_os("SERVICENOW_TUI_SNAPSHOT").is_some() {
            eprintln!("\n{text}");
        }
        assert!(text.contains("SECURE CONNECTION"));
        assert!(text.contains("CONNECT THE OPERATIONS LEDGER"));
        assert!(text.contains("enter / a"));
        assert!(text.contains("returns you to this ledger"));
        assert!(text.contains("work"));
        assert!(text.contains("dev12345.service-now.com"));
    }

    #[test]
    fn compact_unauthenticated_launch_keeps_both_decisions_visible() {
        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = ConnectionApp {
            profile: "work",
            instance: None,
            reason: "No ServiceNow instance is connected to this profile yet.",
            color: false,
        };
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        if std::env::var_os("SERVICENOW_TUI_SNAPSHOT").is_some() {
            eprintln!("\n{text}");
        }
        assert!(text.contains("SIGN-IN REQUIRED"));
        assert!(text.contains("enter / a"));
        assert!(text.contains("q / esc"));
        assert!(text.contains("Sign-in returns you to this ledger"));
        assert!(!text.contains("THE LEDGER NEEDS MORE ROOM"));
    }

    #[test]
    fn long_query_inputs_keep_the_tail_visible() {
        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        let query = format!("{}TAIL", "active=true^".repeat(12));
        app.overlay = Overlay::QueryInput(query);
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("TAIL"));
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

    #[test]
    fn incident_workspace_renders_activity_and_switches_tabs_lazily() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.overlay = Overlay::Detail;
        app.detail_record = app.selected_record().cloned();
        app.detail_record_sys_id = Some("0123456789abcdef0123456789abcdef".into());
        app.incident_tab = IncidentTab::Activity;
        app.activity = PanelState::Ready {
            items: vec![serde_json::json!({
                "element": {"value": "work_notes", "display_value": "Work notes"},
                "value": "Investigating the mail gateway",
                "sys_created_by": "avery.stone",
                "sys_created_on": "2026-08-25 10:20:00"
            })],
            truncated: false,
        };

        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        if std::env::var_os("SERVICENOW_TUI_SNAPSHOT").is_some() {
            eprintln!("\n{text}");
        }
        assert!(text.contains("INCIDENT WORKSPACE"));
        assert!(text.contains("▌2 ACTIVITY"));
        assert!(text.contains("WORK NOTE"));
        assert!(text.contains("Investigating the mail gateway"));

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Action::LoadIncidentTab(IncidentTab::Attachments)
        );
        assert!(matches!(app.attachments, PanelState::Loading));
    }

    #[test]
    fn incident_workspace_renders_attachments_and_sla_status() {
        let backend = TestBackend::new(112, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.overlay = Overlay::Detail;
        app.detail_record = app.selected_record().cloned();
        app.detail_record_sys_id = Some("0123456789abcdef0123456789abcdef".into());
        app.incident_tab = IncidentTab::Attachments;
        app.attachments = PanelState::Ready {
            items: vec![
                AttachmentMetadata {
                    sys_id: "fedcba9876543210fedcba9876543210".into(),
                    file_name: "gateway-diagnostics.txt".into(),
                    content_type: "text/plain".into(),
                    size_bytes: "1536".into(),
                    table_name: "incident".into(),
                    table_sys_id: "0123456789abcdef0123456789abcdef".into(),
                    download_link: String::new(),
                    sys_created_by: "avery.stone".into(),
                    sys_created_on: "2026-08-25 10:25:00".into(),
                },
                AttachmentMetadata {
                    sys_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    file_name: "malformed-size.txt".into(),
                    content_type: "text/plain".into(),
                    size_bytes: "not-a-size\u{1b}[31m".into(),
                    table_name: "incident".into(),
                    table_sys_id: "0123456789abcdef0123456789abcdef".into(),
                    download_link: String::new(),
                    sys_created_by: "system".into(),
                    sys_created_on: "2026-08-25 10:26:00".into(),
                },
            ],
            truncated: false,
        };
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("gateway-diagnostics.txt"));
        assert!(text.contains("1.5 KiB"));
        assert!(text.contains("malformed-size.txt"));
        assert!(!text.contains('\u{1b}'));

        app.incident_tab = IncidentTab::Slas;
        app.slas = PanelState::Ready {
            items: vec![serde_json::json!({
                "sla": {"value": "sla-id", "display_value": "P1 resolution"},
                "stage": "in_progress",
                "has_breached": "true",
                "percentage": "106",
                "start_time": "2026-08-25 09:00:00",
                "planned_end_time": "2026-08-25 10:00:00",
                "end_time": "",
                "duration": "1 Hour",
                "pause_duration": "0 Seconds"
            })],
            truncated: false,
        };
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("P1 resolution"));
        assert!(text.contains("106%"));
        assert!(text.contains("BREACHED"));
    }

    #[test]
    fn related_view_failures_are_isolated_and_sanitized() {
        let backend = TestBackend::new(90, 26);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.overlay = Overlay::Detail;
        app.detail_record = app.selected_record().cloned();
        app.detail_record_sys_id = Some("0123456789abcdef0123456789abcdef".into());
        app.incident_tab = IncidentTab::Slas;
        app.slas = PanelState::Failed("Required ACL missing\u{1b}[31m".into());

        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("THIS VIEW COULD NOT BE LOADED"));
        assert!(text.contains("The incident overview remains available"));
        assert!(!text.contains('\u{1b}'));

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(app.incident_tab, IncidentTab::Overview);
    }

    #[test]
    fn overview_failure_labels_the_index_projection_and_offers_retry() {
        let backend = TestBackend::new(90, 26);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.overlay = Overlay::Detail;
        app.overview_error = Some("Required field ACL missing\u{1b}[31m".into());

        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("COMPLETE RECORD UNAVAILABLE"));
        assert!(text.contains("INDEX FIELDS ONLY"));
        assert!(text.contains("retry the full record"));
        assert!(!text.contains("ALL FIELDS"));
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn related_views_disclose_the_hundred_record_boundary() {
        let entries = (0..=RELATED_VIEW_LIMIT)
            .map(|index| {
                serde_json::json!({
                    "element": "work_notes",
                    "value": format!("Entry {index}")
                })
            })
            .collect();
        let state = bounded_panel(entries);
        assert_eq!(panel_count(&state), Some(RELATED_VIEW_LIMIT));
        assert!(panel_truncated(&state));
        assert_eq!(panel_count_label(&state).as_deref(), Some("100+"));
        let text = activity_lines(&state, Theme::new(false))
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("latest 100 entries; more available"));
        assert!(!text.contains("Entry 100"));
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

    #[test]
    fn generic_tables_choose_human_fields_before_sys_id() {
        let records = vec![serde_json::json!({
            "sys_id": "0123456789abcdef0123456789abcdef",
            "name": "Database cluster",
            "status": "online",
            "vendor": "Example"
        })];
        assert_eq!(
            infer_columns(&records, "cmdb_ci"),
            vec!["name", "status", "sys_id", "vendor"]
        );
    }

    #[test]
    fn sensitive_field_names_are_redacted() {
        for field in [
            "password",
            "u_client_secret",
            "access_token",
            "api-key",
            "session_cookie",
            "private_key_pem",
        ] {
            assert_eq!(
                display_field_value(field, &Value::String("sensitive".into())),
                "[REDACTED]"
            );
        }
        assert_eq!(
            display_field_value("tokenized_name", &Value::String("visible".into())),
            "visible"
        );
    }

    fn rendered_text(buffer: &ratatui::buffer::Buffer) -> String {
        let width = usize::from(buffer.area.width);
        buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
